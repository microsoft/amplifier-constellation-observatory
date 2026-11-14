//! Optional Instance source receipts attached to the existing Talk input.
//! Observation never acknowledges input, prompts, creates work or settles effects.
use crate::{
    acp, auth,
    error::{ApiError, ApiResult},
    intake,
    state::{AppState, now_ms},
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use serde_json::{Value, json};
use std::sync::Arc;

pub const META: &str = "com.microsoft.amplifier.source-delivery";
pub const METHOD: &str = "_amplifier/source_receipts";

// Existing transcript sharing is unchanged. A private receipt observation is
// visible only to the actor who submitted and inspected that particular input.
pub fn project(mut record: Value, actor: &str) -> Value {
    if let Some(messages) = record["messages"].as_array_mut() {
        for message in messages {
            if message["actor"] != actor
                && let Some(fields) = message.as_object_mut()
            {
                fields.remove("source_observation");
            }
        }
    }
    record
}

pub fn supported(init: &Value) -> bool {
    let c =
        &init["_meta"]["io.github.bkrabach.agent-instance"]["inputContinuity"]["sourceDelivery"];
    c["version"] == 1
        && c["metadata"] == META
        && c["receiptsMethod"] == METHOD
        && c["ordinaryPrompts"] == true
        && c["promptRetry"] == "inspectOnly"
        && c["workReferences"] == true
}

pub fn prepare(
    state: &AppState,
    room: &str,
    input: &str,
    sid: &str,
    init: &Value,
) -> Result<Option<Value>> {
    if !supported(init) {
        return Ok(None);
    }
    let record = state.read("sessions", room)?;
    let (index, message) = record["messages"]
        .as_array()
        .context("Invalid saved input")?
        .iter()
        .enumerate()
        .find(|(_, m)| m["role"] == "user" && m["request_id"] == input)
        .context("Missing saved input")?;
    let source = json!({"id":format!("{room}:{index}"),"revision":"1","occurredAt":message["at"]});
    ensure!(
        source["occurredAt"].as_i64().is_some_and(|n| n > 0),
        "Invalid saved source time"
    );
    // The original message is immutable; delivery changes do not revise its text.
    let binding = json!({"version":1,"source":source,"session_id":sid});
    ensure!(
        message
            .get("source_delivery")
            .is_none_or(|old| old == &binding),
        "Source binding changed"
    );
    intake::mutate(state, room, |record| {
        let message = record["messages"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|m| m["role"] == "user" && m["request_id"] == input)
            .unwrap();
        message["source_delivery"] = binding;
    })?;
    Ok(Some(
        json!({"version":1,"inputId":input,"sources":[source]}),
    ))
}

pub fn checked_receipt(response: Value, source: &Value, input: &str) -> Result<Value> {
    ensure!(response["version"] == 1, "Invalid source receipt version");
    ensure!(
        response["coverage"]["delivery"] == "recent_session_records"
            && response["coverage"]["work"] == "retained_session_records"
            && response["coverage"]["missing"] == "unknown",
        "Invalid source receipt coverage"
    );
    let receipts = response["receipts"]
        .as_array()
        .context("Invalid source receipt list")?;
    ensure!(receipts.len() <= 1, "Invalid source receipt count");
    let Some(r) = receipts.first() else {
        return Ok(Value::Null);
    };
    ensure!(
        r["sourceId"] == source["id"]
            && r["revision"] == source["revision"]
            && r["occurredAt"] == source["occurredAt"]
            && (r["inputId"].is_null() || r["inputId"] == input),
        "Invalid source receipt correlation"
    );
    ensure!(
        matches!(
            r["state"].as_str(),
            Some(
                "received"
                    | "submitted"
                    | "remoteQueued"
                    | "incorporated"
                    | "notAccepted"
                    | "promptRequired"
                    | "unknown"
            )
        ) && r["incorporated"].is_boolean()
            && r["completion"] == "not_established"
            && (r["observedAt"].is_null() || r["observedAt"].as_i64().is_some_and(|n| n > 0))
            && matches!(
                r["workCoverage"].as_str(),
                Some("partial" | "retained_session_records")
            ),
        "Invalid source receipt evidence"
    );
    let work = r["work"]
        .as_array()
        .context("Invalid source receipt work")?;
    ensure!(work.len() <= 64, "Invalid source receipt work count");
    let mut refs = Vec::new();
    for w in work {
        ensure!(
            w["id"]
                .as_str()
                .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok())
                && w["ownerAgentId"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty() && s.len() <= 512)
                && w["revision"].as_u64().is_some_and(|n| n > 0)
                && w["inputId"] == input
                && matches!(
                    w["state"].as_str(),
                    Some(
                        "proposed"
                            | "accepted"
                            | "in_progress"
                            | "blocked"
                            | "review"
                            | "completed"
                            | "cancelled"
                    )
                ),
            "Invalid source receipt work reference"
        );
        refs.push(json!({"id":w["id"],"ownerAgentId":w["ownerAgentId"],"revision":w["revision"],"state":w["state"],"inputId":w["inputId"]}));
    }
    // Keep only fixed evidence fields, never arbitrary peer-supplied content.
    Ok(
        json!({"sourceId":r["sourceId"],"revision":r["revision"],"occurredAt":r["occurredAt"],"inputId":r["inputId"],
        "state":r["state"],"incorporated":r["incorporated"],"observedAt":r["observedAt"],"completion":"not_established","work":refs,"workCoverage":r["workCoverage"]}),
    )
}

struct InspectionGuard {
    state: Arc<AppState>,
    room: String,
    completed: bool,
}
impl Drop for InspectionGuard {
    fn drop(&mut self) {
        let _gate = self.state.talk_gate.lock().unwrap();
        if !self.completed {
            self.state.acp_clients.lock().unwrap().remove(&self.room);
        }
        self.state
            .active_sessions
            .lock()
            .unwrap()
            .remove(&self.room);
        self.state
            .inspection_sessions
            .lock()
            .unwrap()
            .remove(&self.room);
    }
}

pub async fn inspect(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((room, input)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let actor = auth::check(&state, &headers, "interact", true)?.actor;
    let (endpoint, sid, source, _permit, mut guard) = {
        let _gate = state.talk_gate.lock().unwrap();
        if crate::desktop_update::busy(&state) {
            return Err(ApiError::busy());
        }
        let record = state.read("sessions", &room)?;
        let message = record["messages"]
            .as_array()
            .and_then(|ms| {
                ms.iter()
                    .find(|m| m["role"] == "user" && m["request_id"] == input)
            })
            .ok_or_else(ApiError::missing)?;
        if message["actor"] != actor {
            return Err(ApiError::forbidden());
        }
        let endpoint = state.read("endpoints", record["endpoint_id"].as_str().unwrap_or(""))?;
        if endpoint["revision"] != record["endpoint_revision"]
            || record["agent_history_removed"] == true
        {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "The saved endpoint/session changed; source inspection cannot switch identities"
                    .into(),
            ));
        }
        let binding = &message["source_delivery"];
        if binding["version"] != 1 {
            return Err(ApiError(
                StatusCode::NOT_IMPLEMENTED,
                "This input has no negotiated ordinary source/work binding".into(),
            ));
        }
        let sid = record["acp_session_id"]
            .as_str()
            .ok_or_else(|| ApiError::bad("No saved agent session"))?
            .to_owned();
        if binding["session_id"] != sid {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "The original source session changed".into(),
            ));
        }
        let permit = state
            .operation_gate
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::busy())?;
        if !state.active_sessions.lock().unwrap().insert(room.clone()) {
            return Err(ApiError::busy());
        }
        state
            .inspection_sessions
            .lock()
            .unwrap()
            .insert(room.clone());
        let guard = InspectionGuard {
            state: state.clone(),
            room: room.clone(),
            completed: false,
        };
        (endpoint, sid, binding["source"].clone(), permit, guard)
    };
    let response =
        acp::inspect_source(&state, &room, &endpoint["acp"], &sid, &actor, &input).await?;
    let observation = json!({"request_id":input,"session_id":room,"acp_session_id":sid,"source":source,
        "observed_at":now_ms(),"receipt":response["receipt"],"coverage":{"delivery":"recent_session_records","work":"retained_session_records","missing":"unknown"},
        "transport_outcome":"unchanged","work_authority":"remote_owner"});
    {
        let _gate = state.talk_gate.lock().unwrap();
        // The caller may have lost access while the remote read was pending.
        if auth::check(&state, &headers, "interact", true)?.actor != actor {
            return Err(ApiError::forbidden());
        }
        if state.read("endpoints", endpoint["id"].as_str().unwrap())?["revision"]
            != endpoint["revision"]
        {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "The endpoint changed during source inspection".into(),
            ));
        }
        let mut record = state.read("sessions", &room)?;
        if record["acp_session_id"] != sid
            || record["endpoint_id"] != endpoint["id"]
            || record["endpoint_revision"] != endpoint["revision"]
            || record["agent_history_removed"] == true
        {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "The saved session changed during source inspection".into(),
            ));
        }
        let message = record["messages"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|m| m["role"] == "user" && m["request_id"] == input)
            .ok_or_else(ApiError::missing)?;
        if message["actor"] != actor
            || message["source_delivery"]["session_id"] != sid
            || message["source_delivery"]["source"] != source
        {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "The original source binding changed during inspection".into(),
            ));
        }
        message["source_observation"] = observation.clone();
        state.save("sessions", record.clone(), record["revision"].as_u64())?;
    }
    guard.completed = true;
    Ok(Json(observation))
}
