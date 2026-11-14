use agent_observatory::{config::Config, server, state::AppState};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tempfile::TempDir;
use tower::ServiceExt;

async fn api(
    app: &Router,
    token: &str,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(format!("/api/v1{path}"))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(json!({"body_not_json":true})),
    )
}
async fn setup(mode: &str) -> (TempDir, Arc<AppState>, Router, String, String, String) {
    let dir = TempDir::new().unwrap();
    Config {
        id: uuid::Uuid::new_v4().to_string(),
        owner: "owner".into(),
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
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixture_source_work.py");
    let (status,endpoint)=api(&app,&token,"POST","/endpoints",json!({"label":"Source fixture","acp":{"command":if cfg!(windows){"python"}else{"python3"},"args":[fixture,mode,dir.path().join("frames.jsonl")],"cwd":dir.path()}})).await;
    assert_eq!(status, StatusCode::OK, "{endpoint}");
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
        endpoint["id"].as_str().unwrap().into(),
    )
}
async fn submit(app: &Router, token: &str, sid: &str, id: &str) -> Value {
    let (status, value) = api(
        app,
        token,
        "POST",
        &format!("/sessions/{sid}/prompt"),
        json!({"text":"One original source input","request_id":id}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    for _ in 0..300 {
        let (_, value) = api(app, token, "GET", &format!("/sessions/{sid}"), Value::Null).await;
        if value["status"] != "running" {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("Fixture did not settle")
}
fn frames(dir: &TempDir) -> Vec<Value> {
    std::fs::read_to_string(dir.path().join("frames.jsonl"))
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}

async fn wait_for_file(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("Fixture did not reach {}", path.display()));
}

fn method_count(log: &[Value], method: &str) -> usize {
    log.iter().filter(|m| m["method"] == method).count()
}

fn inspection_task(
    app: &Router,
    token: &str,
    sid: &str,
    input: &str,
) -> tokio::task::JoinHandle<(StatusCode, Value)> {
    let app = app.clone();
    let token = token.to_owned();
    let path = format!("/sessions/{sid}/inputs/{input}/inspect");
    tokio::spawn(async move { api(&app, &token, "POST", &path, json!({})).await })
}

fn no_inspection_effects(log: &[Value]) {
    for method in [
        "authenticate",
        "session/cancel",
        "_session/acknowledge_inputs",
        "_session/steering",
    ] {
        assert_eq!(method_count(log, method), 0, "Unexpected {method}");
    }
}

#[tokio::test]
async fn lost_prompt_inspection_keeps_one_source_and_existing_work_without_replay() {
    let (dir, state, app, token, sid, _) = setup("lost").await;
    let input = uuid::Uuid::new_v4().to_string();
    let before = submit(&app, &token, &sid, &input).await;
    assert_eq!(before["messages"][0]["delivery"], "unknown");
    let original = frames(&dir)
        .into_iter()
        .find(|m| m["method"] == "session/prompt")
        .unwrap();
    let metadata = &original["params"]["_meta"]["com.microsoft.amplifier.source-delivery"];
    assert_eq!(metadata["inputId"], input);
    assert_eq!(
        metadata["sources"][0],
        json!({"id":format!("{sid}:0"),"revision":"1","occurredAt":before["messages"][0]["at"]})
    );
    let (status, observation) = api(
        &app,
        &token,
        "POST",
        &format!("/sessions/{sid}/inputs/{input}/inspect"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{observation}");
    assert_eq!(observation["receipt"]["work"].as_array().unwrap().len(), 2);
    assert_eq!(observation["receipt"]["work"][0]["state"], "accepted");
    assert_eq!(observation["receipt"]["work"][1]["state"], "proposed");
    assert_eq!(observation["receipt"]["completion"], "not_established");
    let (_, after) = api(
        &app,
        &token,
        "GET",
        &format!("/sessions/{sid}"),
        Value::Null,
    )
    .await;
    assert_eq!(after["messages"][0]["delivery"], "unknown");
    assert!(after["messages"][0].get("acknowledged").is_none());
    assert_eq!(after["messages"][0]["source_observation"], observation);
    api(
        &app,
        &token,
        "POST",
        &format!("/sessions/{sid}/prompt"),
        json!({"text":"One original source input","request_id":input}),
    )
    .await;
    let log = frames(&dir);
    assert_eq!(
        log.iter()
            .filter(|m| m["method"] == "session/prompt")
            .count(),
        1
    );
    assert_eq!(
        log.iter().filter(|m| m["method"] == "session/new").count(),
        1
    );
    assert_eq!(
        log.iter().filter(|m| m["method"] == "session/load").count(),
        1
    );
    assert_eq!(
        log.iter()
            .filter(|m| m["method"] == "_amplifier/source_receipts")
            .count(),
        1
    );
    assert!(
        !log.iter()
            .any(|m| m["method"] == "_session/acknowledge_inputs")
    );
    state.acp_clients.lock().unwrap().clear();
}

#[tokio::test]
async fn unsupported_agent_keeps_plain_acp_and_inspection_does_not_prompt() {
    let (dir, state, app, token, sid, _) = setup("unsupported").await;
    let input = uuid::Uuid::new_v4().to_string();
    submit(&app, &token, &sid, &input).await;
    let log = frames(&dir);
    let prompt = log
        .iter()
        .find(|m| m["method"] == "session/prompt")
        .unwrap();
    assert!(prompt["params"].get("_meta").is_none());
    let (status, _) = api(
        &app,
        &token,
        "POST",
        &format!("/sessions/{sid}/inputs/{input}/inspect"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(frames(&dir).len(), log.len());
    state.acp_clients.lock().unwrap().clear();
}

#[tokio::test]
async fn inspection_refuses_other_actor_changed_endpoint_and_mismatched_receipt() {
    let (dir, state, app, token, sid, endpoint) = setup("malformed").await;
    let input = uuid::Uuid::new_v4().to_string();
    submit(&app, &token, &sid, &input).await;
    let (_, other) = api(
        &app,
        &token,
        "POST",
        "/tokens",
        json!({"scopes":["interact"]}),
    )
    .await;
    let path = format!("/sessions/{sid}/inputs/{input}/inspect");
    let count = frames(&dir).len();
    assert_eq!(
        api(
            &app,
            other["token"].as_str().unwrap(),
            "POST",
            &path,
            json!({})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(frames(&dir).len(), count);
    assert_eq!(
        api(&app, &token, "POST", &path, json!({})).await.0,
        StatusCode::BAD_GATEWAY
    );
    let (_, saved) = api(
        &app,
        &token,
        "GET",
        &format!("/sessions/{sid}"),
        Value::Null,
    )
    .await;
    assert!(saved["messages"][0].get("source_observation").is_none());
    assert_eq!(saved["messages"][0]["delivery"], "unknown");
    let mut e = state.read("endpoints", &endpoint).unwrap();
    let revision = e["revision"].as_u64().unwrap();
    e["label"] = json!("Changed");
    state.save("endpoints", e, Some(revision)).unwrap();
    let count = frames(&dir).len();
    assert_eq!(
        api(&app, &token, "POST", &path, json!({})).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(frames(&dir).len(), count);
    state.acp_clients.lock().unwrap().clear();
}

#[tokio::test]
async fn inspection_refuses_new_input_before_persistence_without_stranding_it() {
    let (dir, state, app, token, sid, _) = setup("inspect-barrier").await;
    let original = uuid::Uuid::new_v4().to_string();
    let before = submit(&app, &token, &sid, &original).await;
    assert_eq!(before["status"], "idle");
    let task = inspection_task(&app, &token, &sid, &original);
    wait_for_file(&dir.path().join("inspect-entered")).await;
    assert!(state.inspection_sessions.lock().unwrap().contains(&sid));

    let duplicate = api(
        &app,
        &token,
        "POST",
        &format!("/sessions/{sid}/prompt"),
        json!({"text":"One original source input","request_id":original}),
    )
    .await;
    assert_eq!(duplicate.0, StatusCode::OK);
    assert_eq!(duplicate.1["messages"], before["messages"]);
    let refused = uuid::Uuid::new_v4().to_string();
    let result = api(
        &app,
        &token,
        "POST",
        &format!("/sessions/{sid}/prompt"),
        json!({"text":"Not accepted during inspection","request_id":refused}),
    )
    .await;
    assert_eq!(result.0, StatusCode::CONFLICT, "{}", result.1);
    assert_eq!(
        state.read("sessions", &sid).unwrap()["messages"],
        before["messages"]
    );
    assert_eq!(method_count(&frames(&dir), "session/prompt"), 1);

    std::fs::write(dir.path().join("inspect-release"), "release").unwrap();
    let (status, observation) = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StatusCode::OK, "{observation}");
    assert!(!state.inspection_sessions.lock().unwrap().contains(&sid));
    assert!(!state.active_sessions.lock().unwrap().contains(&sid));
    let later = uuid::Uuid::new_v4().to_string();
    let after = submit(&app, &token, &sid, &later).await;
    assert_eq!(after["status"], "idle");
    assert!(
        after["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["request_id"] != refused)
    );
    assert_eq!(method_count(&frames(&dir), "session/prompt"), 2);
    no_inspection_effects(&frames(&dir));
    state.acp_clients.lock().unwrap().clear();
}

#[tokio::test]
async fn cancelling_inspection_releases_ownership_without_acknowledging_input() {
    let (dir, state, app, token, sid, _) = setup("inspect-barrier").await;
    let original = uuid::Uuid::new_v4().to_string();
    let before = submit(&app, &token, &sid, &original).await;
    let task = inspection_task(&app, &token, &sid, &original);
    wait_for_file(&dir.path().join("inspect-entered")).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(!state.inspection_sessions.lock().unwrap().contains(&sid));
    assert!(!state.active_sessions.lock().unwrap().contains(&sid));
    assert!(!state.acp_clients.lock().unwrap().contains_key(&sid));
    let saved = state.read("sessions", &sid).unwrap();
    assert_eq!(saved["messages"], before["messages"]);
    assert!(saved["messages"][0].get("source_observation").is_none());
    let later = uuid::Uuid::new_v4().to_string();
    assert_eq!(submit(&app, &token, &sid, &later).await["status"], "idle");
    let log = frames(&dir);
    assert_eq!(method_count(&log, "session/prompt"), 2);
    assert_eq!(method_count(&log, "_amplifier/source_receipts"), 1);
    no_inspection_effects(&log);
    state.acp_clients.lock().unwrap().clear();
}

#[tokio::test]
async fn restored_inspection_advertises_no_interactive_authority_and_refuses_callbacks() {
    let (dir, state, app, token, sid, _) = setup("restore-interactions").await;
    let original = uuid::Uuid::new_v4().to_string();
    let before = submit(&app, &token, &sid, &original).await;
    assert_eq!(before["messages"][0]["delivery"], "unknown");
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        api(
            &app,
            &token,
            "POST",
            &format!("/sessions/{sid}/inputs/{original}/inspect"),
            json!({}),
        ),
    )
    .await
    .unwrap();
    assert_eq!(result.0, StatusCode::OK, "{}", result.1);
    assert!(dir.path().join("initialize-callbacks-refused").exists());
    assert!(dir.path().join("restore-callbacks-refused").exists());
    let log = frames(&dir);
    let initializations: Vec<_> = log.iter().filter(|m| m["method"] == "initialize").collect();
    assert_eq!(initializations.len(), 2);
    assert!(initializations[0]["params"]["clientCapabilities"]["elicitation"].is_object());
    assert_eq!(
        initializations[1]["params"]["clientCapabilities"],
        json!({})
    );
    assert_eq!(method_count(&log, "session/prompt"), 1);
    assert_eq!(method_count(&log, "session/load"), 1);
    assert_eq!(method_count(&log, "session/new"), 1);
    assert_eq!(method_count(&log, "_amplifier/source_receipts"), 1);
    no_inspection_effects(&log);
    let (_, after) = api(
        &app,
        &token,
        "GET",
        &format!("/sessions/{sid}"),
        Value::Null,
    )
    .await;
    assert_eq!(after["messages"][0]["delivery"], "unknown");
    assert!(after["messages"][0].get("acknowledged").is_none());
    assert_eq!(after["permissions"], json!([]));
    assert_eq!(after["elicitations"], json!([]));
    assert!(!state.acp_clients.lock().unwrap().contains_key(&sid));
}

#[tokio::test]
async fn authentication_required_during_inspection_restore_does_not_authenticate_or_retry() {
    let (dir, state, app, token, sid, _) = setup("restore-auth").await;
    let original = uuid::Uuid::new_v4().to_string();
    let before = submit(&app, &token, &sid, &original).await;
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        api(
            &app,
            &token,
            "POST",
            &format!("/sessions/{sid}/inputs/{original}/inspect"),
            json!({}),
        ),
    )
    .await
    .unwrap();
    assert_eq!(result.0, StatusCode::BAD_GATEWAY, "{}", result.1);
    let log = frames(&dir);
    assert_eq!(method_count(&log, "initialize"), 2);
    assert_eq!(method_count(&log, "session/load"), 1);
    assert_eq!(method_count(&log, "session/prompt"), 1);
    assert_eq!(method_count(&log, "_amplifier/source_receipts"), 0);
    no_inspection_effects(&log);
    let (_, after) = api(
        &app,
        &token,
        "GET",
        &format!("/sessions/{sid}"),
        Value::Null,
    )
    .await;
    assert_eq!(after["messages"], before["messages"]);
    assert_eq!(after["permissions"], json!([]));
    assert_eq!(after["elicitations"], json!([]));
    assert!(!state.active_sessions.lock().unwrap().contains(&sid));
    assert!(!state.inspection_sessions.lock().unwrap().contains(&sid));
    assert!(!state.acp_clients.lock().unwrap().contains_key(&sid));
}

#[tokio::test]
async fn cached_work_observation_is_hidden_from_other_actors_on_session_list_and_intake() {
    let (dir, state, app, token, sid, _) = setup("lost").await;
    let original = uuid::Uuid::new_v4().to_string();
    submit(&app, &token, &sid, &original).await;
    let (status, observation) = api(
        &app,
        &token,
        "POST",
        &format!("/sessions/{sid}/inputs/{original}/inspect"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{observation}");
    let (_, other) = api(
        &app,
        &token,
        "POST",
        "/tokens",
        json!({"scopes":["interact"]}),
    )
    .await;
    let other_token = other["token"].as_str().unwrap();
    let (_, owner_view) = api(
        &app,
        &token,
        "GET",
        &format!("/sessions/{sid}"),
        Value::Null,
    )
    .await;
    assert_eq!(owner_view["messages"][0]["source_observation"], observation);
    let (_, other_view) = api(
        &app,
        other_token,
        "GET",
        &format!("/sessions/{sid}"),
        Value::Null,
    )
    .await;
    assert_eq!(other_view["messages"][0]["request_id"], original);
    assert!(
        other_view["messages"][0]
            .get("source_observation")
            .is_none()
    );
    let (_, listing) = api(&app, other_token, "GET", "/sessions", Value::Null).await;
    let listed = listing["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == sid)
        .unwrap();
    assert!(listed["messages"][0].get("source_observation").is_none());
    let result = api(&app, other_token, "POST", &format!("/sessions/{sid}/prompt"),
        json!({"text":"Separate actor input waits for explicit recovery","request_id":uuid::Uuid::new_v4()})).await;
    assert_eq!(result.0, StatusCode::OK, "{}", result.1);
    assert!(result.1["messages"][0].get("source_observation").is_none());
    assert_eq!(
        state.read("sessions", &sid).unwrap()["messages"][0]["source_observation"],
        observation
    );
    assert_eq!(method_count(&frames(&dir), "session/prompt"), 1);
    no_inspection_effects(&frames(&dir));
    state.acp_clients.lock().unwrap().clear();
}

#[tokio::test]
async fn retained_work_with_null_delivery_and_absent_receipts_remain_unknown() {
    for mode in ["retained", "empty"] {
        let (dir, state, app, token, sid, _) = setup(mode).await;
        let original = uuid::Uuid::new_v4().to_string();
        let before = submit(&app, &token, &sid, &original).await;
        assert_eq!(before["messages"][0]["delivery"], "unknown");
        let result = api(
            &app,
            &token,
            "POST",
            &format!("/sessions/{sid}/inputs/{original}/inspect"),
            json!({}),
        )
        .await;
        assert_eq!(result.0, StatusCode::OK, "{}", result.1);
        if mode == "retained" {
            let receipt = &result.1["receipt"];
            assert_eq!(receipt.get("inputId"), Some(&Value::Null));
            assert_eq!(receipt.get("observedAt"), Some(&Value::Null));
            assert_eq!(receipt["state"], "unknown");
            assert_eq!(receipt["incorporated"], false);
            assert_eq!(receipt["completion"], "not_established");
            assert_eq!(receipt["work"].as_array().unwrap().len(), 2);
            assert!(
                receipt["work"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|w| w["inputId"] == original)
            );
        } else {
            assert_eq!(result.1.get("receipt"), Some(&Value::Null));
            assert_eq!(result.1["coverage"]["missing"], "unknown");
        }
        let saved = state.read("sessions", &sid).unwrap();
        assert_eq!(saved["messages"][0]["delivery"], "unknown");
        assert!(saved["messages"][0].get("acknowledged").is_none());
        let log = frames(&dir);
        assert_eq!(method_count(&log, "session/prompt"), 1);
        assert_eq!(method_count(&log, "_amplifier/source_receipts"), 1);
        no_inspection_effects(&log);
        state.acp_clients.lock().unwrap().clear();
    }
}

#[tokio::test]
async fn session_history_change_during_inspection_refuses_stale_observation() {
    let (dir, state, app, token, sid, _) = setup("inspect-barrier").await;
    let original = uuid::Uuid::new_v4().to_string();
    submit(&app, &token, &sid, &original).await;
    let task = inspection_task(&app, &token, &sid, &original);
    wait_for_file(&dir.path().join("inspect-entered")).await;
    let mut record = state.read("sessions", &sid).unwrap();
    let revision = record["revision"].as_u64().unwrap();
    record["agent_history_removed"] = json!(true);
    state.save("sessions", record, Some(revision)).unwrap();
    std::fs::write(dir.path().join("inspect-release"), "release").unwrap();
    let (status, value) = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{value}");
    let saved = state.read("sessions", &sid).unwrap();
    assert_eq!(saved["agent_history_removed"], true);
    assert!(saved["messages"][0].get("source_observation").is_none());
    assert!(!state.active_sessions.lock().unwrap().contains(&sid));
    assert!(!state.inspection_sessions.lock().unwrap().contains(&sid));
    let log = frames(&dir);
    assert_eq!(method_count(&log, "session/prompt"), 1);
    assert_eq!(method_count(&log, "_amplifier/source_receipts"), 1);
    no_inspection_effects(&log);
    state.acp_clients.lock().unwrap().clear();
}
