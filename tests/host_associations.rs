use agent_observatory::{config::Config, provision, server, state::AppState};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tempfile::TempDir;
use tower::ServiceExt;

fn state() -> (TempDir, Arc<AppState>, String) {
    let dir = TempDir::new().unwrap();
    Config {
        id: uuid::Uuid::new_v4().to_string(),
        owner: "fixture".into(),
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
    (dir, state, token)
}
async fn request(
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
                .uri(path)
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
fn host() -> Value {
    json!({"label":"Spark", "transport":"ssh", "address":"spark-fixture", "install_roots":[], "recipes":[]})
}
fn endpoint() -> Value {
    json!({"label":"Spark", "kind":"agent", "ui_url":"https://fixture.invalid"})
}
async fn saved_host(app: &Router, token: &str) -> Value {
    let (status, value) = request(app, token, "POST", "/api/v1/hosts", host()).await;
    assert_eq!(status, StatusCode::OK);
    value
}

#[tokio::test]
async fn inventory_only_computer_does_not_grant_an_installer_or_claim_observed_readiness() {
    let (_dir, state, token) = state();
    let app = server::router(state.clone());
    let saved = saved_host(&app, &token).await;
    assert_eq!(saved["recipes"], json!([]));
    assert_eq!(saved["install_roots"], json!([]));
    assert!(saved.get("connected").is_none());
    assert!(saved.get("ready").is_none());
    let plan = provision::plan(
        &state,
        &json!({"host_id":saved["id"],"recipe_id":"uninstalled","name":"Agent","home":"/tmp/uncreated","port":0}),
    );
    assert!(plan.unwrap_err().1.contains("recipe"));
    assert!(state.list("jobs").unwrap().is_empty());
    assert!(state.list("endpoints").unwrap().is_empty());
    let mut enabled = host();
    enabled["recipes"] = json!([{"id":"example","version":"1","argv":["false"]}]);
    assert!(
        provision::validate_host(&enabled)
            .unwrap_err()
            .1
            .contains("folder")
    );
    enabled["install_roots"] = json!(["/tmp/disposable-observatory-test-root"]);
    assert!(provision::validate_host(&enabled).is_ok());
}

#[tokio::test]
async fn missing_machine_ids_are_refused_before_create_or_edit_and_names_do_not_bind() {
    let (_dir, state, token) = state();
    let app = server::router(state.clone());
    let h = saved_host(&app, &token).await;
    let mut body = endpoint();
    body["host_id"] = json!(uuid::Uuid::new_v4().to_string());
    assert_eq!(
        request(&app, &token, "POST", "/api/v1/endpoints", body.clone())
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert!(state.list("endpoints").unwrap().is_empty());
    let (status, saved) = request(&app, &token, "POST", "/api/v1/endpoints", endpoint()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["label"], h["label"]);
    assert!(saved["host_id"].is_null());
    body["expected_revision"] = saved["revision"].clone();
    let path = format!("/api/v1/endpoints/{}", saved["id"].as_str().unwrap());
    assert_eq!(
        request(&app, &token, "PUT", &path, body).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        state
            .read("endpoints", saved["id"].as_str().unwrap())
            .unwrap(),
        saved
    );
}

#[tokio::test]
async fn omission_preserves_residence_null_detaches_and_host_removal_never_deletes_agents() {
    let (_dir, state, token) = state();
    let app = server::router(state.clone());
    let h = saved_host(&app, &token).await;
    let mut body = endpoint();
    body["host_id"] = h["id"].clone();
    let (status, saved) = request(&app, &token, "POST", "/api/v1/endpoints", body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["host_id"], h["id"]);
    let endpoint_path = format!("/api/v1/endpoints/{}", saved["id"].as_str().unwrap());
    let host_path = format!("/api/v1/hosts/{}", h["id"].as_str().unwrap());
    let mut rename = endpoint();
    rename["label"] = json!("Renamed agent");
    rename["expected_revision"] = saved["revision"].clone();
    let (status, renamed) = request(&app, &token, "PUT", &endpoint_path, rename.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(renamed["host_id"], h["id"]);
    assert_eq!(
        request(
            &app,
            &token,
            "DELETE",
            &host_path,
            json!({"expected_revision":h["revision"]})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    rename["host_id"] = Value::Null;
    rename["expected_revision"] = renamed["revision"].clone();
    let (status, detached) = request(&app, &token, "PUT", &endpoint_path, rename).await;
    assert_eq!(status, StatusCode::OK);
    assert!(detached["host_id"].is_null());
    assert_eq!(
        request(
            &app,
            &token,
            "DELETE",
            &host_path,
            json!({"expected_revision":h["revision"]})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(state.list("hosts").unwrap().is_empty());
    assert_eq!(
        state
            .read("endpoints", saved["id"].as_str().unwrap())
            .unwrap(),
        detached
    );
    assert!(state.list("jobs").unwrap().is_empty());
    assert!(state.list("sessions").unwrap().is_empty());
}

#[tokio::test]
async fn changing_residence_is_revision_checked_inventory_only() {
    let (_dir, state, token) = state();
    let app = server::router(state.clone());
    let first = saved_host(&app, &token).await;
    let second = saved_host(&app, &token).await;
    let mut body = endpoint();
    body["host_id"] = first["id"].clone();
    let (_, saved) = request(&app, &token, "POST", "/api/v1/endpoints", body.clone()).await;
    let path = format!("/api/v1/endpoints/{}", saved["id"].as_str().unwrap());
    body["host_id"] = second["id"].clone();
    body["expected_revision"] = saved["revision"].clone();
    let (status, moved) = request(&app, &token, "PUT", &path, body.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(moved["host_id"], second["id"]);
    assert_eq!(moved["id"], saved["id"]);
    body["host_id"] = first["id"].clone();
    assert_eq!(
        request(&app, &token, "PUT", &path, body).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        state
            .read("endpoints", saved["id"].as_str().unwrap())
            .unwrap(),
        moved
    );
    assert!(state.list("jobs").unwrap().is_empty());
    assert!(state.list("sessions").unwrap().is_empty());
}

#[tokio::test]
async fn association_and_provision_grants_remain_separate() {
    let (_dir, state, admin) = state();
    let app = server::router(state.clone());
    let h = saved_host(&app, &admin).await;
    let mut body = endpoint();
    body["host_id"] = h["id"].clone();
    for (scope, can_assign, can_add_host) in [
        ("observe", false, false),
        ("manage", true, false),
        ("provision", false, true),
    ] {
        let (_, token) = request(
            &app,
            &admin,
            "POST",
            "/api/v1/tokens",
            json!({"name":scope,"scopes":[scope]}),
        )
        .await;
        let token = token["token"].as_str().unwrap();
        assert_eq!(
            request(&app, token, "POST", "/api/v1/endpoints", body.clone())
                .await
                .0,
            if can_assign {
                StatusCode::OK
            } else {
                StatusCode::FORBIDDEN
            }
        );
        assert_eq!(
            request(&app, token, "POST", "/api/v1/hosts", host())
                .await
                .0,
            if can_add_host {
                StatusCode::OK
            } else {
                StatusCode::FORBIDDEN
            }
        );
    }
    assert!(state.list("jobs").unwrap().is_empty());
}

#[tokio::test]
async fn invalid_association_and_unknown_control_fields_cannot_change_inventory() {
    let (_dir, state, token) = state();
    let app = server::router(state.clone());
    for value in [
        json!("Spark"),
        json!("ssh://spark"),
        json!({"id":"nested"}),
        json!(12),
    ] {
        let mut body = endpoint();
        body["host_id"] = value;
        assert_eq!(
            request(&app, &token, "POST", "/api/v1/endpoints", body)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    let mut body = endpoint();
    body["deploy"] = json!(true);
    assert_eq!(
        request(&app, &token, "POST", "/api/v1/endpoints", body)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let mut machine = host();
    machine["connected"] = json!(true);
    assert_eq!(
        request(&app, &token, "POST", "/api/v1/hosts", machine)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert!(state.list("hosts").unwrap().is_empty());
    assert!(state.list("endpoints").unwrap().is_empty());
}

#[tokio::test]
async fn new_connection_credential_does_not_replace_an_existing_grant() {
    let (_dir, state, token) = state();
    let app = server::router(state.clone());
    let path = "/api/v1/credentials";
    let first = json!({"name":"existing", "value":"first-private-value", "create_only":true});
    assert_eq!(
        request(&app, &token, "POST", path, first).await.0,
        StatusCode::OK
    );
    let refused = request(
        &app,
        &token,
        "POST",
        path,
        json!({"name":"existing", "value":"replacement", "create_only":true}),
    )
    .await;
    assert_eq!(refused.0, StatusCode::CONFLICT);
    assert_eq!(
        std::fs::read_to_string(state.home.join("secrets/existing")).unwrap(),
        "first-private-value"
    );
    let list = request(&app, &token, "GET", path, json!({})).await;
    assert_eq!(list.1, json!({"names":["existing"]}));
    assert!(!refused.1.to_string().contains("private-value"));
}
