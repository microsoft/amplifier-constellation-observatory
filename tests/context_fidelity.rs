use agent_observatory::{config::Config, server, state::AppState};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tempfile::TempDir;
use tower::ServiceExt;

fn state() -> (TempDir, Arc<AppState>, Router, String) {
    let dir = TempDir::new().unwrap();
    Config {
        id: uuid::Uuid::new_v4().to_string(),
        owner: "browser-owner".into(),
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
    (dir, state, app, token)
}

async fn request(
    app: &Router,
    token: &str,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(if body.is_null() {
            Body::empty()
        } else {
            Body::from(body.to_string())
        })
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn seed(state: &AppState, messages: Value) -> (Value, Value) {
    let endpoint = state
        .save(
            "endpoints",
            json!({
                "id":uuid::Uuid::new_v4(),
                "label":"Original display name",
                "acp":{"command":"must-not-run"}
            }),
            Some(0),
        )
        .unwrap();
    let session = state
        .save(
            "sessions",
            json!({
                "id":uuid::Uuid::new_v4(),
                "endpoint_id":endpoint["id"],
                "endpoint_revision":endpoint["revision"],
                "title":"Saved source conversation",
                "status":"needs_review",
                "messages":messages
            }),
            Some(0),
        )
        .unwrap();
    (endpoint, session)
}

async fn token(app: &Router, admin: &str, scope: String) -> Value {
    let (status, value) = request(
        app,
        admin,
        "POST",
        "/api/v1/tokens",
        json!({"scopes":[scope]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}

async fn context(app: &Router, token: &str, endpoint: &Value, body: Value) -> Value {
    let path = format!(
        "/agents/{}/context/v1/query",
        endpoint["id"].as_str().unwrap()
    );
    let (status, value) = request(app, token, "POST", &path, body).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}

#[tokio::test]
async fn context_preserves_recorded_authors_and_submission_outcomes() {
    let (_dir, state, app, admin) = state();
    let original_request = uuid::Uuid::new_v4().to_string();
    // An existing unknown input holds admission. The two API submissions below
    // exercise real principal attribution without starting an ACP process.
    let (endpoint, session) = seed(
        &state,
        json!([{
            "role":"user", "text":"Original outcome is unknown", "actor":"browser-owner",
            "request_id":original_request, "sequence":0, "delivery":"unknown",
            "acknowledged":false, "at":1,
            "private_grant":"must-not-project", "browser_hash":"must-not-project"
        }]),
    );
    let sid = session["id"].as_str().unwrap();
    let first = token(&app, &admin, "interact".into()).await;
    let second = token(&app, &admin, "interact".into()).await;
    let mut expected = vec![("browser-owner".to_owned(), original_request, "unknown")];
    for (principal, text) in [
        (&first, "First token contribution"),
        (&second, "Second token contribution"),
    ] {
        let request_id = uuid::Uuid::new_v4().to_string();
        let (status, value) = request(
            &app,
            principal["token"].as_str().unwrap(),
            "POST",
            &format!("/api/v1/sessions/{sid}/prompt"),
            json!({"text":text,"request_id":request_id}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        expected.push((
            format!("token:{}", principal["id"].as_str().unwrap()),
            request_id,
            "received",
        ));
    }
    assert!(state.active_sessions.lock().unwrap().is_empty());
    let saved = state.read("sessions", sid).unwrap();
    let reader = token(
        &app,
        &admin,
        format!("context:{}", endpoint["id"].as_str().unwrap()),
    )
    .await;
    let result = context(
        &app,
        reader["token"].as_str().unwrap(),
        &endpoint,
        json!({"operation":"read","access":{"mode":"owner"},"conversation_id":sid}),
    )
    .await;
    assert_eq!(result["provider_id"], format!("fleet:{}", state.config.id));
    assert_eq!(result["endpoint_id"], endpoint["id"]);
    assert_eq!(result["conversation_id"], sid);
    assert_eq!(result["revision"], saved["revision"]);
    assert_eq!(result["complete"], true);
    assert_eq!(result["messages"].as_array().unwrap().len(), expected.len());
    for (index, (actor, request_id, delivery)) in expected.iter().enumerate() {
        let message = &result["messages"][index];
        assert_eq!(message["id"], format!("{sid}:{index}"));
        assert_eq!(message["actor"], *actor);
        assert_eq!(message["request_id"], *request_id);
        assert_eq!(message["sequence"], index as u64);
        assert_eq!(message["delivery"], *delivery);
        assert!(message.get("private_grant").is_none());
        assert!(message.get("browser_hash").is_none());
        assert!(message.get("work_id").is_none());
    }
    assert_eq!(result["messages"][0]["acknowledged"], false);
    assert_eq!(state.read("sessions", sid).unwrap(), saved);
}

#[tokio::test]
async fn context_keeps_legacy_authorship_unknown_and_endpoint_identity_stable() {
    let (_dir, state, app, admin) = state();
    let (endpoint, session) = seed(
        &state,
        json!([
            {"role":"user","text":"Legacy contribution","at":1},
            {"role":"assistant","text":"Legacy response","at":2}
        ]),
    );
    let sid = session["id"].as_str().unwrap();
    let reader = token(
        &app,
        &admin,
        format!("context:{}", endpoint["id"].as_str().unwrap()),
    )
    .await;
    let query = json!({"operation":"read","access":{"mode":"owner"},"conversation_id":sid});
    let before = context(
        &app,
        reader["token"].as_str().unwrap(),
        &endpoint,
        query.clone(),
    )
    .await;
    assert!(before["messages"][0]["actor"].is_null());
    assert!(before["messages"][1]["actor"].is_null());
    assert!(before["messages"][0].get("actor_label").is_none());
    assert_eq!(
        before["messages"][1]["actor_label"],
        "Original display name"
    );
    assert!(before["messages"][0].get("request_id").is_none());
    assert!(before["messages"][0].get("delivery").is_none());
    let mut renamed = endpoint.clone();
    renamed["label"] = json!("Renamed display name");
    state
        .save("endpoints", renamed, endpoint["revision"].as_u64())
        .unwrap();
    let after = context(&app, reader["token"].as_str().unwrap(), &endpoint, query).await;
    assert_eq!(after["endpoint_id"], endpoint["id"]);
    assert_eq!(after["provider_id"], before["provider_id"]);
    assert_eq!(after["conversation_id"], before["conversation_id"]);
    for index in 0..2 {
        assert_eq!(
            after["messages"][index]["id"],
            before["messages"][index]["id"]
        );
        assert!(after["messages"][index]["actor"].is_null());
    }
    assert_eq!(after["messages"][1]["actor_label"], "Renamed display name");
    let list = context(
        &app,
        reader["token"].as_str().unwrap(),
        &endpoint,
        json!({"operation":"list","access":{"mode":"owner"}}),
    )
    .await;
    assert_eq!(list["items"][0]["endpoint_id"], endpoint["id"]);
    assert_eq!(list["items"][0]["revision"], session["revision"]);
}

#[tokio::test]
async fn context_projection_keeps_existing_access_and_text_bounds() {
    let (_dir, state, app, admin) = state();
    let (endpoint, session) = seed(
        &state,
        json!([{"role":"user","actor":"token:recorded","text":"é".repeat(30001),"at":1}]),
    );
    let (other_endpoint, other_session) = seed(&state, json!([]));
    let reader = token(
        &app,
        &admin,
        format!("context:{}", endpoint["id"].as_str().unwrap()),
    )
    .await;
    let reader_token = reader["token"].as_str().unwrap();
    let path = format!(
        "/agents/{}/context/v1/query",
        endpoint["id"].as_str().unwrap()
    );
    let query =
        json!({"operation":"read","access":{"mode":"owner"},"conversation_id":session["id"]});
    let result = context(&app, reader_token, &endpoint, query.clone()).await;
    assert_eq!(
        result["messages"][0]["text"].as_str().unwrap().len(),
        60_000
    );
    assert_eq!(result["messages"][0]["truncated"], true);
    assert_eq!(result["complete"], false);
    assert_eq!(result["messages"][0]["actor"], "token:recorded");
    assert_eq!(request(&app, reader_token, "POST", &path,
        json!({"operation":"read","access":{"mode":"owner"},"conversation_id":other_session["id"]})).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        request(
            &app,
            reader_token,
            "POST",
            &format!(
                "/agents/{}/context/v1/query",
                other_endpoint["id"].as_str().unwrap()
            ),
            json!({"operation":"list","access":{"mode":"owner"}})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let revoked = request(
        &app,
        &admin,
        "DELETE",
        &format!("/api/v1/tokens/{}", reader["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(revoked.0, StatusCode::OK);
    assert_eq!(
        request(&app, reader_token, "POST", &path, query).await.0,
        StatusCode::UNAUTHORIZED
    );
}
