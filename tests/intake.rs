use agent_observatory::{config::Config, server, state::AppState};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tempfile::TempDir;
use tower::ServiceExt;

const PYTHON: &str = if cfg!(windows) { "python" } else { "python3" };

async fn api(
    app: &Router,
    token: &str,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(format!("/api/v1{path}"))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {token}"))
                .body(if body.is_null() {
                    Body::empty()
                } else {
                    Body::from(body.to_string())
                })
                .unwrap(),
        )
        .await
        .unwrap();
    (
        r.status(),
        serde_json::from_slice(&to_bytes(r.into_body(), 8 * 1024 * 1024).await.unwrap()).unwrap(),
    )
}
async fn setup(mode: &str) -> (TempDir, Arc<AppState>, Router, String, String) {
    let dir = TempDir::new().unwrap();
    Config {
        id: uuid::Uuid::new_v4().to_string(),
        owner: "test".into(),
        bind: "127.0.0.1".into(),
        port: 0,
        public_url: String::new(),
        auth_mode: Default::default(),
        web_transport: Default::default(),
        pam_service: "unused".into(),
        tls_cert: PathBuf::new(),
        tls_key: PathBuf::new(),
        tls_ca: None,
        embed_origins: vec![],
    }
    .save(dir.path())
    .unwrap();
    let state = AppState::open(dir.path().into()).unwrap();
    let token = std::fs::read_to_string(dir.path().join("secrets/control-token")).unwrap();
    let app = server::router(state.clone());
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixture_steering.py");
    let (_,endpoint)=api(&app,&token,"POST","/endpoints",json!({"label":"Fixture","acp":{"command":PYTHON,"args":[fixture,mode,dir.path().join("journal")],"cwd":dir.path()}})).await;
    let (_, session) = api(
        &app,
        &token,
        "POST",
        "/sessions",
        json!({"endpoint_id":endpoint["id"]}),
    )
    .await;
    (
        dir,
        state,
        app,
        token,
        session["id"].as_str().unwrap().into(),
    )
}
async fn send(app: &Router, token: &str, sid: &str, text: &str) -> Value {
    let (status, value) = api(
        app,
        token,
        "POST",
        &format!("/sessions/{sid}/prompt"),
        json!({"text":text,"request_id":uuid::Uuid::new_v4()}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}
async fn wait(app: &Router, token: &str, sid: &str, predicate: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..300 {
        let (_, s) = api(app, token, "GET", &format!("/sessions/{sid}"), Value::Null).await;
        if predicate(&s) {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "timed out: {}",
        api(app, token, "GET", &format!("/sessions/{sid}"), Value::Null)
            .await
            .1
    );
}
fn journal(dir: &TempDir) -> Vec<Value> {
    std::fs::read_to_string(dir.path().join("journal"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}
fn user<'a>(s: &'a Value, text: &str) -> &'a Value {
    s["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "user" && m["text"] == text)
        .unwrap()
}

#[tokio::test]
async fn remote_queue_survives_delayed_receipts_and_ack_without_false_completion() {
    for mode in ["remote-queued", "remote-ack-only"] {
        let (dir, _state, app, token, sid) = setup(mode).await;
        send(&app, &token, &sid, "approval").await;
        let pending = wait(&app, &token, &sid, |s| {
            s["permissions"].as_array().is_some_and(|p| !p.is_empty())
        })
        .await;
        send(&app, &token, &sid, "unincorporated").await;
        wait(&app, &token, &sid, |_| {
            journal(&dir).iter().any(|e| e["kind"] == "queued-chain")
        })
        .await;
        // Let the complete delayed receipt sequence reach the durable ledger.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let during = api(
            &app,
            &token,
            "GET",
            &format!("/sessions/{sid}"),
            Value::Null,
        )
        .await
        .1;
        assert_eq!(
            user(&during, "unincorporated")["delivery"],
            "remoteQueued",
            "{mode}: {during}"
        );
        let permission = pending["permissions"][0]["id"].as_str().unwrap();
        assert_eq!(
            api(
                &app,
                &token,
                "POST",
                &format!("/sessions/{sid}/permissions/{permission}"),
                json!({"option_id":"once"})
            )
            .await
            .0,
            StatusCode::OK
        );
        let finished = wait(&app, &token, &sid, |s| s["status"] == "needs_review").await;
        assert_eq!(user(&finished, "unincorporated")["delivery"], "unknown");
        let logs = journal(&dir);
        assert_eq!(logs.iter().filter(|e| e["kind"] == "prompt").count(), 1);
        assert_eq!(logs.iter().filter(|e| e["kind"] == "steer").count(), 1);
        assert_eq!(logs.iter().filter(|e| e["kind"] == "effect").count(), 1);
        assert_eq!(logs.iter().filter(|e| e["kind"] == "cancel").count(), 0);
    }
}

#[tokio::test]
async fn repeated_updates_share_one_turn_effect_and_keep_approval_live() {
    let (dir, _state, app, t, sid) = setup("native").await;
    send(&app, &t, &sid, "approval").await;
    let before = wait(&app, &t, &sid, |s| {
        s["permissions"].as_array().is_some_and(|p| !p.is_empty())
    })
    .await;
    let permission = before["permissions"][0]["id"].as_str().unwrap();
    send(&app, &t, &sid, "first update").await;
    let body = json!({"text":"second update","request_id":uuid::Uuid::new_v4()});
    let path = format!("/sessions/{sid}/prompt");
    let (a, b) = tokio::join!(
        api(&app, &t, "POST", &path, body.clone()),
        api(&app, &t, "POST", &path, body.clone())
    );
    assert_eq!(a.0, StatusCode::OK);
    assert_eq!(b.0, StatusCode::OK);
    let during = wait(&app, &t, &sid, |s| {
        s["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["text"] == "second update" && m["delivery"] == "incorporated")
    })
    .await;
    assert_eq!(during["permissions"][0]["id"], permission);
    assert_eq!(during["messages"].as_array().unwrap().len(), 3);
    assert_eq!(
        api(
            &app,
            &t,
            "POST",
            &format!("/sessions/{sid}/permissions/{permission}"),
            json!({"option_id":"once"})
        )
        .await
        .0,
        StatusCode::OK
    );
    let done = wait(&app, &t, &sid, |s| s["status"] == "idle").await;
    assert_eq!(user(&done, "first update")["delivery"], "incorporated");
    assert!(!done.to_string().contains("PRIVATE OTHER ROOM"));
    let logs = journal(&dir);
    assert_eq!(logs.iter().filter(|e| e["kind"] == "prompt").count(), 1);
    assert_eq!(logs.iter().filter(|e| e["kind"] == "effect").count(), 1);
    assert_eq!(logs.iter().filter(|e| e["kind"] == "cancel").count(), 0);
    assert_eq!(
        logs.iter()
            .filter(|e| e["kind"] == "steer")
            .map(|e| e["text"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["first update", "second update"]
    );
    let (_, again) = api(&app, &t, "POST", &path, body).await;
    assert_eq!(again["messages"].as_array().unwrap().len(), 4);
}

#[tokio::test]
async fn mismatched_or_error_ack_remains_unknown_without_losing_approval_or_replay() {
    for mode in ["wrong-ack-id", "wrong-ack-id-type", "error-ack"] {
        let (dir, _state, app, token, sid) = setup(mode).await;
        send(&app, &token, &sid, "approval").await;
        let before = wait(&app, &token, &sid, |s| {
            s["permissions"].as_array().is_some_and(|p| !p.is_empty())
        })
        .await;
        let permission = before["permissions"][0]["id"].as_str().unwrap();
        send(&app, &token, &sid, "uncertain update").await;
        wait(&app, &token, &sid, |_| {
            journal(&dir).iter().any(|e| e["kind"] == "invalid-ack")
        })
        .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let during = api(
            &app,
            &token,
            "GET",
            &format!("/sessions/{sid}"),
            Value::Null,
        )
        .await
        .1;
        assert_eq!(
            user(&during, "uncertain update")["delivery"],
            "unknown",
            "{mode}: {during}"
        );
        assert_eq!(during["permissions"][0]["id"], permission);
        send(&app, &token, &sid, "keep queued").await;
        assert_eq!(
            api(
                &app,
                &token,
                "POST",
                &format!("/sessions/{sid}/permissions/{permission}"),
                json!({"option_id":"once"})
            )
            .await
            .0,
            StatusCode::OK
        );
        let done = wait(&app, &token, &sid, |s| s["status"] == "needs_review").await;
        assert_eq!(user(&done, "uncertain update")["delivery"], "unknown");
        assert_eq!(user(&done, "keep queued")["delivery"], "received");
        let logs = journal(&dir);
        assert_eq!(logs.iter().filter(|e| e["kind"] == "prompt").count(), 1);
        assert_eq!(logs.iter().filter(|e| e["kind"] == "steer").count(), 1);
        assert_eq!(logs.iter().filter(|e| e["kind"] == "cancel").count(), 0);
    }
}

#[tokio::test]
async fn invalid_rpc_envelope_times_out_as_unknown_without_replaying_or_dropping_approval() {
    let (dir, _state, app, token, sid) = setup("conflicting-envelope").await;
    send(&app, &token, &sid, "approval").await;
    let before = wait(&app, &token, &sid, |s| {
        s["permissions"].as_array().is_some_and(|p| !p.is_empty())
    })
    .await;
    let permission = before["permissions"][0]["id"].as_str().unwrap();
    send(&app, &token, &sid, "uncertain update").await;
    wait(&app, &token, &sid, |_| {
        journal(&dir).iter().any(|e| e["kind"] == "invalid-ack")
    })
    .await;
    // The official SDK discards a response containing both result and error.
    // It is not an acknowledgment. Our existing 30-second acknowledgment
    // deadline must retain uncertainty without cancelling the active prompt.
    tokio::time::sleep(Duration::from_secs(31)).await;
    let during = api(
        &app,
        &token,
        "GET",
        &format!("/sessions/{sid}"),
        Value::Null,
    )
    .await
    .1;
    assert_eq!(user(&during, "uncertain update")["delivery"], "unknown");
    assert_eq!(during["permissions"][0]["id"], permission);
    assert_eq!(
        api(
            &app,
            &token,
            "POST",
            &format!("/sessions/{sid}/permissions/{permission}"),
            json!({"option_id":"once"})
        )
        .await
        .0,
        StatusCode::OK
    );
    let done = wait(&app, &token, &sid, |s| s["status"] == "needs_review").await;
    assert_eq!(user(&done, "uncertain update")["delivery"], "unknown");
    let logs = journal(&dir);
    assert_eq!(logs.iter().filter(|e| e["kind"] == "prompt").count(), 1);
    assert_eq!(logs.iter().filter(|e| e["kind"] == "steer").count(), 1);
    assert_eq!(logs.iter().filter(|e| e["kind"] == "effect").count(), 1);
    assert_eq!(logs.iter().filter(|e| e["kind"] == "cancel").count(), 0);
}
#[tokio::test]
async fn generic_acp_queues_without_cancelling_or_overlapping_prompts() {
    let (dir, _state, app, t, sid) = setup("generic").await;
    send(&app, &t, &sid, "root").await;
    send(&app, &t, &sid, "one").await;
    send(&app, &t, &sid, "two").await;
    let s = wait(&app, &t, &sid, |s| s["status"] == "idle").await;
    assert_eq!(user(&s, "two")["delivery"], "completed");
    let logs = journal(&dir);
    assert_eq!(
        logs.iter()
            .filter(|e| e["kind"] == "prompt")
            .map(|e| e["text"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["root", "one", "two"]
    );
    assert_eq!(logs.iter().filter(|e| e["kind"] == "effect").count(), 3);
    assert!(
        !logs
            .iter()
            .any(|e| e["kind"] == "cancel" || e["kind"] == "steer")
    );
}
#[tokio::test]
async fn finalization_race_definite_idle_ack_uses_one_normal_fallback() {
    let (dir, _state, app, t, sid) = setup("idle").await;
    send(&app, &t, &sid, "root").await;
    send(&app, &t, &sid, "late").await;
    let s = wait(&app, &t, &sid, |s| s["status"] == "idle").await;
    assert_eq!(user(&s, "late")["delivery"], "completed");
    let logs = journal(&dir);
    assert_eq!(logs.iter().filter(|e| e["kind"] == "prompt").count(), 2);
    assert_eq!(logs.iter().filter(|e| e["kind"] == "steer").count(), 1);
    assert_eq!(logs.iter().filter(|e| e["kind"] == "effect").count(), 2);
}
#[tokio::test]
async fn transport_loss_retains_unknown_and_never_blindly_replays() {
    let (dir, _state, app, t, sid) = setup("drop").await;
    send(&app, &t, &sid, "root").await;
    send(&app, &t, &sid, "uncertain").await;
    let s = wait(&app, &t, &sid, |s| s["status"] == "needs_review").await;
    assert_eq!(user(&s, "uncertain")["delivery"], "unknown");
    assert_eq!(s["acp_session_id"], "saved-fixture");
    send(&app, &t, &sid, "later").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        journal(&dir)
            .iter()
            .filter(|e| e["kind"] == "prompt")
            .count(),
        1
    );
    let path = format!("/sessions/{sid}/inputs/resume");
    assert_eq!(
        api(&app, &t, "POST", &path, json!({})).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        api(&app, &t, "POST", &path, json!({"acknowledge_unknown":true}))
            .await
            .0,
        StatusCode::OK
    );
    let s = wait(&app, &t, &sid, |s| s["status"] == "idle").await;
    assert_eq!(user(&s, "uncertain")["delivery"], "unknown");
    assert_eq!(user(&s, "uncertain")["acknowledged"], true);
    let logs = journal(&dir);
    assert_eq!(
        logs.iter()
            .filter(|e| e["kind"] == "prompt")
            .map(|e| e["text"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["root", "later"]
    );
    assert!(logs.iter().any(|e| e["kind"] == "session/load"));
}
#[tokio::test]
async fn explicit_stop_cancels_queue_while_partial_frames_survive_input_polling() {
    let (dir, _state, app, t, sid) = setup("native").await;
    send(&app, &t, &sid, "partial").await;
    tokio::time::sleep(Duration::from_millis(220)).await;
    send(&app, &t, &sid, "more").await;
    let s = wait(&app, &t, &sid, |s| s["status"] == "idle").await;
    assert_eq!(user(&s, "more")["delivery"], "incorporated");
    assert_eq!(
        journal(&dir)
            .iter()
            .filter(|e| e["kind"] == "effect")
            .count(),
        1
    );
    send(&app, &t, &sid, "approval").await;
    wait(&app, &t, &sid, |s| {
        !s["permissions"].as_array().unwrap().is_empty()
    })
    .await;
    assert_eq!(
        api(
            &app,
            &t,
            "POST",
            &format!("/sessions/{sid}/cancel"),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    let stopped = wait(&app, &t, &sid, |s| s["status"] == "cancelled").await;
    assert!(stopped["permissions"].as_array().unwrap().is_empty());
    assert_eq!(
        journal(&dir)
            .iter()
            .filter(|e| e["kind"] == "effect")
            .count(),
        1
    );
}

#[tokio::test]
async fn bounded_queue_rejects_unaccepted_overflow_and_stop_does_not_run_it() {
    let (dir, _state, app, t, sid) = setup("generic").await;
    send(&app, &t, &sid, "approval").await;
    wait(&app, &t, &sid, |s| {
        !s["permissions"].as_array().unwrap().is_empty()
    })
    .await;
    for n in 0..31 {
        send(&app, &t, &sid, &format!("queued {n}")).await;
    }
    let overflow = json!({"text":"overflow","request_id":uuid::Uuid::new_v4()});
    assert_eq!(
        api(
            &app,
            &t,
            "POST",
            &format!("/sessions/{sid}/prompt"),
            overflow
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    api(
        &app,
        &t,
        "POST",
        &format!("/sessions/{sid}/cancel"),
        json!({}),
    )
    .await;
    let s = wait(&app, &t, &sid, |s| s["status"] == "cancelled").await;
    assert_eq!(s["messages"].as_array().unwrap().len(), 32);
    assert!(
        s["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["delivery"] == "cancelled")
    );
    assert!(!journal(&dir).iter().any(|e| e["kind"] == "effect"));
}

#[tokio::test]
async fn failed_and_malformed_steering_are_visible_without_next_prompt_replay() {
    for mode in ["failed", "malformed"] {
        let (dir, _state, app, t, sid) = setup(mode).await;
        send(&app, &t, &sid, "root").await;
        send(&app, &t, &sid, "update").await;
        let s = wait(&app, &t, &sid, |s| s["status"] == "needs_review").await;
        assert_eq!(user(&s, "update")["delivery"], "unknown");
        assert_eq!(
            journal(&dir)
                .iter()
                .filter(|e| e["kind"] == "prompt")
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn explicit_recovery_uses_advertised_receipts_with_same_saved_session() {
    let (dir, _state, app, t, sid) = setup("recover").await;
    send(&app, &t, &sid, "root").await;
    send(&app, &t, &sid, "uncertain").await;
    wait(&app, &t, &sid, |s| s["status"] == "needs_review").await;
    send(&app, &t, &sid, "later").await;
    assert_eq!(
        api(
            &app,
            &t,
            "POST",
            &format!("/sessions/{sid}/inputs/resume"),
            json!({"acknowledge_unknown":true})
        )
        .await
        .0,
        StatusCode::OK
    );
    wait(&app, &t, &sid, |s| s["status"] == "idle").await;
    let logs = journal(&dir);
    let ack = logs
        .iter()
        .position(|e| e["kind"] == "acknowledge")
        .unwrap();
    let prompt = logs
        .iter()
        .position(|e| e["kind"] == "prompt" && e["text"] == "later")
        .unwrap();
    assert!(ack < prompt);
    assert_eq!(
        logs.iter().filter(|e| e["kind"] == "session/new").count(),
        1
    );
    assert_eq!(
        logs.iter().filter(|e| e["kind"] == "session/load").count(),
        1
    );
}

#[tokio::test]
async fn changed_endpoint_stops_the_queued_prefix_before_any_new_effect() {
    let (dir, state, app, t, sid) = setup("generic").await;
    send(&app, &t, &sid, "approval").await;
    let s = wait(&app, &t, &sid, |s| {
        !s["permissions"].as_array().unwrap().is_empty()
    })
    .await;
    send(&app, &t, &sid, "queued").await;
    let mut endpoint = state
        .read("endpoints", s["endpoint_id"].as_str().unwrap())
        .unwrap();
    endpoint["label"] = json!("changed");
    state
        .save("endpoints", endpoint.clone(), endpoint["revision"].as_u64())
        .unwrap();
    api(
        &app,
        &t,
        "POST",
        &format!(
            "/sessions/{sid}/permissions/{}",
            s["permissions"][0]["id"].as_str().unwrap()
        ),
        json!({"option_id":"once"}),
    )
    .await;
    let s = wait(&app, &t, &sid, |s| s["status"] == "failed").await;
    assert_eq!(user(&s, "queued")["delivery"], "notAccepted");
    assert_eq!(
        journal(&dir)
            .iter()
            .filter(|e| e["kind"] == "effect")
            .count(),
        1
    );
}

#[tokio::test]
async fn service_restart_preserves_order_and_marks_submitted_input_unknown() {
    let (dir, state, _app, _t, sid) = setup("native").await;
    let mut s = state.read("sessions", &sid).unwrap();
    s["status"] = json!("running");
    s["messages"] = json!([
        {"role":"user","text":"sent","request_id":"a","delivery":"submitted","sequence":1},
        {"role":"user","text":"waiting","request_id":"b","delivery":"received","sequence":2}]);
    state
        .save("sessions", s.clone(), s["revision"].as_u64())
        .unwrap();
    drop(state);
    let restored = AppState::open(dir.path().into())
        .unwrap()
        .read("sessions", &sid)
        .unwrap();
    assert_eq!(restored["status"], "interrupted");
    assert_eq!(user(&restored, "sent")["delivery"], "unknown");
    assert_eq!(user(&restored, "waiting")["delivery"], "received");
    assert_eq!(user(&restored, "waiting")["sequence"], 2);
}

#[tokio::test]
async fn unknown_only_recovery_acknowledges_without_a_new_prompt() {
    let (dir, _state, app, token, sid) = setup("recover").await;
    send(&app, &token, &sid, "root").await;
    send(&app, &token, &sid, "uncertain").await;
    wait(&app, &token, &sid, |s| s["status"] == "needs_review").await;
    assert_eq!(
        api(
            &app,
            &token,
            "POST",
            &format!("/sessions/{sid}/inputs/resume"),
            json!({"acknowledge_unknown":true})
        )
        .await
        .0,
        StatusCode::OK
    );
    let settled = wait(&app, &token, &sid, |s| s["status"] == "idle").await;
    assert_eq!(user(&settled, "uncertain")["delivery"], "unknown");
    assert_eq!(user(&settled, "uncertain")["acknowledged"], true);
    assert_eq!(
        journal(&dir)
            .iter()
            .filter(|e| e["kind"] == "prompt")
            .count(),
        1
    );
    assert_eq!(
        journal(&dir)
            .iter()
            .filter(|e| e["kind"] == "acknowledge")
            .count(),
        1
    );
    assert_eq!(
        journal(&dir)
            .iter()
            .filter(|e| e["kind"] == "effect")
            .count(),
        0
    );
}

#[tokio::test]
async fn revoked_or_expired_queued_authority_does_not_start_work() {
    for expired in [false, true] {
        let (dir, state, app, token, sid) = setup("generic").await;
        let (_, credential) = api(
            &app,
            &token,
            "POST",
            "/tokens",
            json!({"scopes":["interact"]}),
        )
        .await;
        send(&app, &token, &sid, "approval").await;
        let pending = wait(&app, &token, &sid, |s| {
            !s["permissions"].as_array().unwrap().is_empty()
        })
        .await;
        send(
            &app,
            credential["token"].as_str().unwrap(),
            &sid,
            "queued with access",
        )
        .await;
        if expired {
            state
                .db
                .lock()
                .unwrap()
                .execute(
                    "UPDATE tokens SET expires_at=0 WHERE id=?1",
                    [credential["id"].as_str().unwrap()],
                )
                .unwrap();
        } else {
            assert_eq!(
                api(
                    &app,
                    &token,
                    "DELETE",
                    &format!("/tokens/{}", credential["id"].as_str().unwrap()),
                    Value::Null
                )
                .await
                .0,
                StatusCode::OK
            );
        }
        api(
            &app,
            &token,
            "POST",
            &format!(
                "/sessions/{sid}/permissions/{}",
                pending["permissions"][0]["id"].as_str().unwrap()
            ),
            json!({"option_id":"once"}),
        )
        .await;
        let done = wait(&app, &token, &sid, |s| s["status"] == "idle").await;
        assert_eq!(user(&done, "queued with access")["delivery"], "notAccepted");
        assert_eq!(
            user(&done, "queued with access")["delivery_reason"],
            "authorization_changed"
        );
        assert_eq!(
            journal(&dir)
                .iter()
                .filter(|e| e["kind"] == "prompt")
                .count(),
            1
        );
        assert!(!done.to_string().contains("browser_hash"));
    }
}

#[tokio::test]
async fn unconfirmed_remote_recovery_keeps_unknown_and_does_not_start_queue() {
    let (dir, _state, app, token, sid) = setup("bad-recovery").await;
    send(&app, &token, &sid, "root").await;
    send(&app, &token, &sid, "uncertain").await;
    wait(&app, &token, &sid, |s| s["status"] == "needs_review").await;
    send(&app, &token, &sid, "next").await;
    assert_eq!(
        api(
            &app,
            &token,
            "POST",
            &format!("/sessions/{sid}/inputs/resume"),
            json!({"acknowledge_unknown":true})
        )
        .await
        .0,
        StatusCode::OK
    );
    let held = wait(&app, &token, &sid, |s| s["status"] == "needs_review").await;
    assert_ne!(user(&held, "uncertain")["acknowledged"], true);
    assert_eq!(user(&held, "next")["delivery"], "received");
    assert_eq!(
        journal(&dir)
            .iter()
            .filter(|e| e["kind"] == "prompt")
            .count(),
        1
    );
}
