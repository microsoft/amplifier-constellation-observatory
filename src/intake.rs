//! Durable conversation intake. A worker owns one session; input never cancels it.
use crate::{
    acp,
    error::{ApiError, ApiResult},
    state::{AppState, now_ms},
};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::sync::Arc;

pub const META: &str = "io.github.bkrabach.agent-runtime";
pub fn signal(state: &AppState, room: &str) -> Arc<tokio::sync::Notify> {
    state
        .talk_inputs
        .lock()
        .unwrap()
        .entry(room.into())
        .or_default()
        .clone()
}

fn waiting(m: &Value) -> bool {
    matches!(m["delivery"].as_str(), Some("received" | "promptRequired"))
}
fn unsettled(m: &Value) -> bool {
    matches!(
        m["delivery"].as_str(),
        Some("submitted" | "received_remote" | "remoteQueued")
    )
}
fn uncertain(s: &Value) -> bool {
    s["messages"].as_array().is_some_and(|ms| {
        ms.iter()
            .any(|m| m["delivery"] == "unknown" && m["acknowledged"] != true)
    })
}
fn endpoint(state: &AppState, session: &Value) -> ApiResult<Value> {
    let e = state.read("endpoints", session["endpoint_id"].as_str().unwrap_or(""))?;
    if e["revision"] != session["endpoint_revision"] {
        return Err(ApiError::bad(
            "The endpoint configuration changed. Start a new conversation to use it.",
        ));
    }
    if session["agent_history_removed"] == true {
        return Err(ApiError::bad(
            "This agent session was removed. Start a new conversation.",
        ));
    }
    Ok(e)
}
pub fn submit(
    state: Arc<AppState>,
    room: &str,
    principal: &crate::auth::Principal,
    body: &Value,
) -> ApiResult<Value> {
    let actor = principal.actor.as_str();
    let text = body["text"].as_str().unwrap_or("").trim();
    let id = body["request_id"].as_str().unwrap_or("");
    if text.is_empty() || text.len() > 100000 || uuid::Uuid::parse_str(id).is_err() {
        return Err(ApiError::bad(
            "A message and unique request ID are required",
        ));
    }
    let _gate = state.talk_gate.lock().unwrap();
    if crate::desktop_update::busy(&state) {
        return Err(ApiError::busy());
    }
    let mut session = state.read("sessions", room)?;
    if let Some(old) = session["messages"].as_array().and_then(|ms| {
        ms.iter()
            .find(|m| m["role"] == "user" && m["request_id"] == id)
    }) {
        if old["text"] != text || old.get("actor").is_some_and(|v| v != actor) {
            return Err(ApiError::bad(
                "This request ID belongs to a different message",
            ));
        }
        return Ok(session);
    }
    let e = endpoint(&state, &session)?;
    if session["status"] == "disconnecting" {
        return Err(ApiError::busy());
    }
    let ms = session["messages"]
        .as_array()
        .ok_or_else(|| ApiError::bad("Invalid conversation"))?;
    let had_waiting = ms.iter().any(waiting);
    let pending: Vec<_> = ms.iter().filter(|m| waiting(m) || unsettled(m)).collect();
    if pending.len() >= 32
        || pending
            .iter()
            .map(|m| m["text"].as_str().unwrap_or("").len())
            .sum::<usize>()
            + text.len()
            > 1024 * 1024
    {
        return Err(ApiError::bad(
            "The message queue is full. Wait for the agent before sending more.",
        ));
    }
    let sequence = session["next_input_sequence"].as_u64().unwrap_or(0) + 1;
    session["next_input_sequence"] = json!(sequence);
    session["messages"].as_array_mut().unwrap().push(json!({"role":"user","text":text,"request_id":id,"actor":actor,"sequence":sequence,"delivery":"received","at":now_ms()}));
    let mut active = state.active_sessions.lock().unwrap();
    let start = !(active.contains(room)
        || uncertain(&session)
        || (had_waiting
            && matches!(
                session["status"].as_str(),
                Some("failed" | "cancelled" | "interrupted" | "needs_review")
            )));
    let permit = if start {
        Some(
            state
                .operation_gate
                .clone()
                .try_acquire_owned()
                .map_err(|_| ApiError::busy())?,
        )
    } else {
        None
    };
    if start {
        session["status"] = json!("running");
    }
    state.db.lock().unwrap().execute(
        "INSERT OR REPLACE INTO input_authority(session_id,input_id,actor,browser_hash) VALUES(?1,?2,?3,?4)",
        params![room, id, actor, principal.session_hash],
    )?;
    let saved = state.save("sessions", session.clone(), session["revision"].as_u64())?;
    if let Some(signal) = state.talk_inputs.lock().unwrap().get(room) {
        signal.notify_one();
    }
    if let Some(permit) = permit {
        active.insert(room.into());
        spawn(state.clone(), room.into(), e, permit);
    }
    Ok(saved)
}
pub fn resume(state: Arc<AppState>, room: &str, actor: &str, ack: bool) -> ApiResult<Value> {
    let _gate = state.talk_gate.lock().unwrap();
    if crate::desktop_update::busy(&state) {
        return Err(ApiError::busy());
    }
    let mut session = state.read("sessions", room)?;
    let e = endpoint(&state, &session)?;
    let mut active = state.active_sessions.lock().unwrap();
    if active.contains(room) {
        return Err(ApiError::busy());
    }
    if uncertain(&session) && !ack {
        return Err(ApiError::bad(
            "Inspect the agent's effects, then explicitly continue without resending uncertain inputs.",
        ));
    }
    if ack {
        session["recovery_requested"] = json!(true);
        session["recovery_actor"] = json!(actor);
    }
    let permit = state
        .operation_gate
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::busy())?;
    session["status"] = json!("running");
    session.as_object_mut().unwrap().remove("error");
    let saved = state.save("sessions", session.clone(), session["revision"].as_u64())?;
    active.insert(room.into());
    spawn(state.clone(), room.into(), e, permit);
    Ok(saved)
}
// Credential references stay in a private table, never in transcript/API records.
pub fn authorized(state: &AppState, room: &str, input: &str) -> bool {
    let db = state.db.lock().unwrap();
    let binding: Option<(String, Option<String>)> = db
        .query_row(
            "SELECT actor,browser_hash FROM input_authority WHERE session_id=?1 AND input_id=?2",
            params![room, input],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .ok()
        .flatten();
    let Some((actor, browser)) = binding else {
        return false;
    };
    if let Some(hash) = browser {
        return db
            .query_row(
                "SELECT 1 FROM browser_sessions WHERE hash=?1 AND owner=?2 AND expires_at>?3",
                params![hash, actor, now_ms()],
                |_| Ok(()),
            )
            .optional()
            .ok()
            .flatten()
            .is_some();
    }
    let Some(id) = actor.strip_prefix("token:") else {
        return false;
    };
    let scopes: Option<String> = db
        .query_row(
            "SELECT scopes FROM tokens WHERE id=?1 AND expires_at>?2",
            params![id, now_ms()],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten();
    scopes
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .is_some_and(|ss| ss.iter().any(|s| s == "admin" || s == "interact"))
}
fn retire_unauthorized(state: &AppState, room: &str, session: &mut Value) -> bool {
    let mut changed = false;
    for input in session["messages"].as_array_mut().unwrap() {
        if waiting(input) && !authorized(state, room, input["request_id"].as_str().unwrap_or("")) {
            input["delivery"] = json!("notAccepted");
            input["delivery_reason"] = json!("authorization_changed");
            changed = true;
        }
    }
    changed
}

fn spawn(
    state: Arc<AppState>,
    room: String,
    endpoint_record: Value,
    permit: tokio::sync::OwnedSemaphorePermit,
) {
    tokio::spawn(async move {
        let _permit = permit;
        if let Ok(s) = state.read("sessions", &room)
            && s["recovery_requested"] == true
        {
            let result = acp::recover_inputs(
                &state,
                &room,
                &endpoint_record["acp"],
                s["acp_session_id"].as_str(),
                s["recovery_actor"].as_str().unwrap_or(""),
            )
            .await;
            if let Err(error) = result {
                let _ = mutate(&state, &room, |s| {
                    s["status"] = json!("needs_review");
                    s["error"] = json!(error.1);
                });
                state.active_sessions.lock().unwrap().remove(&room);
                state.talk_inputs.lock().unwrap().remove(&room);
                return;
            }
        }
        loop {
            let next = {
                let _gate = state.talk_gate.lock().unwrap();
                let Ok(mut s) = state.read("sessions", &room) else {
                    state.active_sessions.lock().unwrap().remove(&room);
                    state.talk_inputs.lock().unwrap().remove(&room);
                    return;
                };
                if endpoint(&state, &s).is_err() {
                    s["status"] = json!("failed");
                    s["error"] = json!("The connection changed. Queued messages were not sent.");
                    for m in s["messages"].as_array_mut().unwrap() {
                        if waiting(m) {
                            m["delivery"] = json!("notAccepted");
                        }
                    }
                    let _ = state.save("sessions", s.clone(), s["revision"].as_u64());
                    state.active_sessions.lock().unwrap().remove(&room);
                    state.talk_inputs.lock().unwrap().remove(&room);
                    return;
                }
                if retire_unauthorized(&state, &room, &mut s)
                    && let Ok(saved) = state.save("sessions", s.clone(), s["revision"].as_u64())
                {
                    s = saved;
                }
                let next = s["messages"]
                    .as_array()
                    .and_then(|ms| ms.iter().find(|m| waiting(m)))
                    .cloned();
                if next.is_none() {
                    s["status"] = json!(if uncertain(&s) {
                        "needs_review"
                    } else {
                        "idle"
                    });
                    let _ = state.save("sessions", s.clone(), s["revision"].as_u64());
                    state.active_sessions.lock().unwrap().remove(&room);
                    state.talk_inputs.lock().unwrap().remove(&room);
                }
                next.map(|m| (s, m))
            };
            let Some((session, input)) = next else {
                return;
            };
            let actor = input["actor"].as_str().unwrap_or("");
            let id = input["request_id"].as_str().unwrap();
            let (tx, rx) = tokio::sync::watch::channel(false);
            state
                .cancellations
                .lock()
                .unwrap()
                .insert(room.clone(), (actor.into(), tx));
            state.progress.lock().unwrap().insert(
                room.clone(),
                json!({"text":"","tools":[],"plan":[],"revision":0}),
            );
            let response = acp::prompt_input(
                &state,
                &room,
                &endpoint_record["acp"],
                session["acp_session_id"].as_str(),
                input["text"].as_str().unwrap(),
                actor,
                Some(id),
            )
            .await;
            let stopped = *rx.borrow();
            if response.is_err() {
                state.acp_clients.lock().unwrap().remove(&room);
            }
            let _gate = state.talk_gate.lock().unwrap();
            state.cancellations.lock().unwrap().remove(&room);
            let Ok(mut current) = state.read("sessions", &room) else {
                state.active_sessions.lock().unwrap().remove(&room);
                state.talk_inputs.lock().unwrap().remove(&room);
                return;
            };
            if let Some(mut p) = state.progress.lock().unwrap().remove(&room) {
                if (response.is_err() || stopped)
                    && let Some(ts) = p["tools"].as_array_mut()
                {
                    for t in ts {
                        if matches!(t["status"].as_str(), Some("pending" | "in_progress")) {
                            t["status"] = json!(if stopped { "cancelled" } else { "interrupted" });
                        }
                    }
                }
                current["progress"] = p;
            }
            let mut halt = stopped || response.is_err();
            match response {
                Ok(r) if !stopped => {
                    current["acp_session_id"] = r["session_id"].clone();
                    current["resumable"] = r["resumable"].clone();
                    if let Some(m) = current["messages"]
                        .as_array_mut()
                        .unwrap()
                        .iter_mut()
                        .find(|m| m["request_id"] == id && m["role"] == "user")
                    {
                        m["delivery"] = json!("completed");
                    }
                    current["messages"].as_array_mut().unwrap().push(json!({"role":"assistant","text":r["reply"],"media":r["media"],"stop_reason":r["stop_reason"],"request_id":id,"at":now_ms()}));
                }
                result => {
                    current["status"] = json!(if stopped { "cancelled" } else { "failed" });
                    current["error"] = json!(if stopped {
                        "You stopped this turn. Work already performed is not undone.".to_owned()
                    } else {
                        result.err().map(|e| e.1).unwrap_or_default()
                    });
                }
            }
            for m in current["messages"].as_array_mut().unwrap() {
                if stopped && m["request_id"] == id && m["role"] == "user" {
                    m["delivery"] = json!("cancelled");
                }
                if !halt && m["delivery"] == "received_remote" {
                    m["delivery"] = json!("delivered");
                }
                if unsettled(m) {
                    m["delivery"] = json!("unknown");
                    halt = true;
                }
                if stopped && waiting(m) {
                    m["delivery"] = json!("cancelled");
                }
            }
            if uncertain(&current) {
                current["status"] = json!("needs_review");
                halt = true;
            }
            if !halt {
                current["status"] = json!("running");
                current.as_object_mut().unwrap().remove("error");
            }
            let _ = state.save("sessions", current.clone(), current["revision"].as_u64());
            let _ = state.audit(
                "acp.turn_finished",
                Some(&room),
                json!({"actor":actor,"endpoint_id":endpoint_record["id"],"status":current["status"]}),
            );
            if halt {
                state.active_sessions.lock().unwrap().remove(&room);
                state.talk_inputs.lock().unwrap().remove(&room);
                return;
            }
        }
    });
}
pub fn mutate(state: &AppState, room: &str, f: impl FnOnce(&mut Value)) -> ApiResult<()> {
    let _gate = state.talk_gate.lock().unwrap();
    let mut s = state.read("sessions", room)?;
    f(&mut s);
    state.save("sessions", s.clone(), s["revision"].as_u64())?;
    Ok(())
}
pub fn mark(state: &AppState, room: &str, id: &str, delivery: &str) -> ApiResult<()> {
    mutate(state, room, |s| {
        if let Some(m) = s["messages"].as_array_mut().and_then(|ms| {
            ms.iter_mut()
                .find(|m| m["role"] == "user" && m["request_id"] == id)
        }) {
            m["delivery"] = json!(delivery);
        }
    })
}
pub fn claim_update(state: &AppState, room: &str, root: &str) -> ApiResult<Option<Value>> {
    let _gate = state.talk_gate.lock().unwrap();
    let mut s = state.read("sessions", room)?;
    endpoint(state, &s)?;
    if retire_unauthorized(state, room, &mut s) {
        s = state.save("sessions", s.clone(), s["revision"].as_u64())?;
    }
    let m = s["messages"].as_array_mut().and_then(|ms| {
        ms.iter_mut()
            .find(|m| m["request_id"] != root && m["delivery"] == "received")
    });
    let Some(m) = m else {
        return Ok(None);
    };
    m["delivery"] = json!("submitted");
    m["target_turn"] = json!(root);
    let claimed = m.clone();
    state.save("sessions", s.clone(), s["revision"].as_u64())?;
    Ok(Some(claimed))
}
pub fn receipt(state: &AppState, room: &str, root: &str, r: &Value) -> ApiResult<()> {
    let Some(id) = r["inputId"].as_str() else {
        return Ok(());
    };
    let Some(status) = r["state"].as_str().filter(|v| {
        [
            "received",
            "submitted",
            "remoteQueued",
            "incorporated",
            "notAccepted",
            "unknown",
        ]
        .contains(v)
    }) else {
        return Ok(());
    };
    mutate(state, room, |s| {
        if let Some(m) = s["messages"].as_array_mut().and_then(|ms| {
            ms.iter_mut()
                .find(|m| m["role"] == "user" && m["request_id"] == id && m["target_turn"] == root)
        }) {
            // Late acceptance cannot downgrade an incorporation or terminal failure.
            let terminal = matches!(
                m["delivery"].as_str(),
                Some("incorporated" | "notAccepted" | "unknown")
            );
            // A provider queue is stronger evidence than a delayed transport
            // receipt. Only incorporation or a terminal outcome resolves it.
            if m["delivery"] == "remoteQueued" && matches!(status, "received" | "submitted") {
                return;
            }
            if !terminal || matches!(status, "unknown" | "incorporated" | "notAccepted") {
                m["delivery"] = json!(if status == "received" {
                    "received_remote"
                } else {
                    status
                });
            }
        }
    })
}
