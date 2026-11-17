//! Local TLS fixture, exercising the fleet API and real provider boundary.
use agent_observatory::{auth, config::Config, provider, server, state::AppState};
use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::State,
    http::{HeaderMap, Method, Request, StatusCode, Uri},
    response::IntoResponse,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tempfile::TempDir;
use tower::ServiceExt;

const ID: &str = "policy-fixture-instance";
const CREDENTIAL: &str = "fixture-remote-admin";
const BASE: &str = "/api/v1/approval-policy";
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
    let s = AppState::open(dir.path().into()).unwrap();
    std::fs::write(dir.path().join("secrets/fixture-policy"), CREDENTIAL).unwrap();
    let token = std::fs::read_to_string(dir.path().join("secrets/control-token")).unwrap();
    (dir, s, token)
}
struct Remote {
    descriptor: Value,
    policy: Value,
    puts: Vec<Value>,
    malformed_reply: bool,
    refusal: Option<StatusCode>,
    descriptor_reads: usize,
    pause_descriptor_at: Option<usize>,
    descriptor_started: Arc<tokio::sync::Notify>,
    descriptor_release: Arc<tokio::sync::Notify>,
}
struct Fixture {
    data: Arc<Mutex<Remote>>,
    endpoint: Value,
    server: tokio::task::JoinHandle<()>,
    _dir: TempDir,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
async fn respond(
    State(data): State<Arc<Mutex<Remote>>>,
    method: Method,
    uri: Uri,
    h: HeaderMap,
    bytes: Bytes,
) -> axum::response::Response {
    if h.get("authorization").and_then(|h| h.to_str().ok()) != Some(&format!("Bearer {CREDENTIAL}"))
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if uri.path() == "/api/v1/descriptor" {
        let (document, pause, started, release) = {
            let mut remote = data.lock().unwrap();
            remote.descriptor_reads += 1;
            (
                remote.descriptor.clone(),
                remote.pause_descriptor_at == Some(remote.descriptor_reads),
                remote.descriptor_started.clone(),
                remote.descriptor_release.clone(),
            )
        };
        if pause {
            started.notify_one();
            release.notified().await;
        }
        return axum::Json(document).into_response();
    }
    let mut remote = data.lock().unwrap();
    if uri.path() != BASE {
        return StatusCode::NOT_FOUND.into_response();
    }
    if let Some(status) = remote.refusal {
        return (status, axum::Json(json!({"private":"must not leak"}))).into_response();
    }
    if method == Method::PUT {
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        remote.puts.push(body.clone());
        if body["expected_revision"] != remote.policy["revision"] {
            return (StatusCode::CONFLICT, axum::Json(json!({"error":"changed"}))).into_response();
        }
        assert_eq!(body.as_object().unwrap().len(), 3);
        assert_eq!(
            body["delegated_clients"],
            remote.policy["delegated_clients"]
        );
        remote.policy["mode"] = body["mode"].clone();
        remote.policy["revision"] = json!(remote.policy["revision"].as_u64().unwrap() + 1);
        if remote.malformed_reply {
            return (StatusCode::OK, "truncated {").into_response();
        }
    }
    axum::Json(remote.policy.clone()).into_response()
}
async fn fixture() -> Fixture {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = TempDir::new().unwrap();
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let pem = cert.pem();
    let ca = dir.path().join("ca.pem");
    std::fs::write(&ca, &pem).unwrap();
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem(
        pem.into_bytes(),
        signing_key.serialize_pem().into_bytes(),
    )
    .await
    .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let data = Arc::new(Mutex::new(Remote {
        descriptor: json!({"instance_id":ID,"capabilities":["agent-instance.approval-policy/1"],
            "operations":[{"path":"approval-policy","methods":["GET","PUT"],"scope":"admin"}]}),
        policy: json!({"schema":"agent-instance.approval-policy/1","revision":3,"mode":"ask",
            "delegated_clients":["chosen-client"],"rules":[{"id":"remembered-rule","status":"active"}]}),
        puts: vec![],
        malformed_reply: false,
        refusal: None,
        descriptor_reads: 0,
        pause_descriptor_at: None,
        descriptor_started: Arc::new(tokio::sync::Notify::new()),
        descriptor_release: Arc::new(tokio::sync::Notify::new()),
    }));
    let app = Router::new().fallback(respond).with_state(data.clone());
    let server = tokio::spawn(async move {
        axum_server::from_tcp_rustls(listener, tls)
            .unwrap()
            .serve(app.into_make_service())
            .await
            .unwrap();
    });
    Fixture {
        data,
        server,
        _dir: dir,
        endpoint: json!({"label":"Fixture agent","kind":"agent",
        "management":{"provider":"agent-instance","base_url":format!("https://{address}"),
        "credential_ref":"fixture-policy","ca_file":ca,"access":"manage","expected_id":ID}}),
    }
}
fn save(s: &AppState, mut endpoint: Value, id: &str) -> Value {
    endpoint["id"] = json!(id);
    provider::validate(&endpoint).unwrap();
    s.save("endpoints", endpoint, Some(0)).unwrap()
}
async fn req(
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
                .body(if body.is_null() {
                    Body::empty()
                } else {
                    Body::from(body.to_string())
                })
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}
async fn preview(app: &Router, token: &str) -> Value {
    let (status, p) = req(app, token, "GET", BASE, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    p
}
fn command(preview: &Value, mode: &str) -> Value {
    json!({"request_id":uuid::Uuid::new_v4().to_string(),"mode":mode,"targets":preview["preview"]["targets"]})
}
async fn finish(app: &Router, token: &str, id: &str) -> Value {
    for _ in 0..200 {
        let (status, op) = req(
            app,
            token,
            "GET",
            &format!("{BASE}/operations/{id}"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        if op["status"] != "running" {
            return op;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("fixture operation did not finish");
}
#[test]
fn approval_provider_requires_named_capability_and_both_explicit_methods() {
    let endpoint = json!({"management":{"provider":"agent-instance"}});
    let mut descriptor = json!({"operations":[{"path":"approval-policy","methods":["GET","PUT"]}]});
    let has = |d: &Value, id: &str| {
        provider::available_operations(&endpoint, d)
            .iter()
            .any(|o| o.id == id)
    };
    assert!(!has(&descriptor, "approval-policy"));
    descriptor["capabilities"] = json!(["agent-instance.approval-policy/1"]);
    assert!(has(&descriptor, "approval-policy"));
    assert!(has(&descriptor, "apply-approval-policy"));
    descriptor["operations"][0]["methods"] = json!(["GET"]);
    assert!(!has(&descriptor, "apply-approval-policy"));
    assert!(!has(&json!({}), "approval-policy"));
}
#[tokio::test]
async fn whole_cohort_keeps_partial_agents_visible_and_preserves_remote_authority() {
    let (_dir, s, token) = state();
    let f = fixture().await;
    save(&s, f.endpoint.clone(), "a-primary");
    save(&s, f.endpoint.clone(), "b-alias");
    save(
        &s,
        json!({"label":"ACP only","kind":"agent","ui_url":"https://example.invalid"}),
        "c-unsupported",
    );
    let mut observer = f.endpoint.clone();
    observer["management"]["access"] = json!("observe");
    // An inaccessible alias stays visible instead of borrowing a healthy
    // connection's observed state or hiding its access failure.
    save(&s, observer, "d-read-only");
    let app = server::router(s.clone());
    let p = preview(&app, &token).await;
    assert_eq!(p["targets"].as_array().unwrap().len(), 4);
    assert_eq!(p["mode"], "unknown");
    assert_eq!(p["complete"], false);
    assert_eq!(p["targets"][1]["status"], "duplicate");
    assert_eq!(p["targets"][2]["status"], "unsupported");
    assert_eq!(p["targets"][3]["status"], "access-required");
    let cmd = command(&p, "automatic");
    let (status, _) = req(
        &app,
        &token,
        "POST",
        &format!("{BASE}/operations"),
        cmd.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let op = finish(&app, &token, cmd["request_id"].as_str().unwrap()).await;
    assert_eq!(op["status"], "partial");
    assert_eq!(op["targets"][0]["status"], "succeeded");
    assert_eq!(op["targets"][1]["status"], "duplicate");
    assert_eq!(op["targets"][2]["status"], "unsupported");
    assert_eq!(op["targets"][3]["status"], "access-required");
    assert!(op.get("request").is_none());
    {
        let remote = f.data.lock().unwrap();
        assert_eq!(remote.puts.len(), 1);
        assert_eq!(remote.policy["delegated_clients"], json!(["chosen-client"]));
        assert_eq!(
            remote.policy["rules"],
            json!([{"id":"remembered-rule","status":"active"}])
        );
    }
    let p = preview(&app, &token).await;
    assert_eq!(p["last_operation"]["id"], cmd["request_id"]);
}
#[tokio::test]
async fn lost_reply_is_unknown_and_replay_and_reconciliation_never_repeat_put() {
    let (_dir, s, token) = state();
    let f = fixture().await;
    save(&s, f.endpoint.clone(), "agent");
    f.data.lock().unwrap().malformed_reply = true;
    let app = server::router(s.clone());
    let cmd = command(&preview(&app, &token).await, "automatic");
    req(
        &app,
        &token,
        "POST",
        &format!("{BASE}/operations"),
        cmd.clone(),
    )
    .await;
    let id = cmd["request_id"].as_str().unwrap();
    let op = finish(&app, &token, id).await;
    assert_eq!(op["targets"][0]["status"], "unknown");
    assert_eq!(op["targets"][0]["observed"]["matches_requested_mode"], true);
    let (status, replayed) = req(
        &app,
        &token,
        "POST",
        &format!("{BASE}/operations"),
        cmd.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replayed["targets"][0]["status"], "unknown");
    finish(&app, &token, id).await;
    let mut changed = cmd;
    changed["mode"] = json!("ask");
    assert_eq!(
        req(&app, &token, "POST", &format!("{BASE}/operations"), changed)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(f.data.lock().unwrap().puts.len(), 1);
}
#[tokio::test]
async fn stale_policy_missing_cohort_and_changed_connection_do_not_write() {
    let (_dir, s, token) = state();
    let f = fixture().await;
    let endpoint = save(&s, f.endpoint.clone(), "agent");
    let app = server::router(s.clone());
    let p = preview(&app, &token).await;
    f.data.lock().unwrap().policy["revision"] = json!(4);
    let cmd = command(&p, "automatic");
    req(
        &app,
        &token,
        "POST",
        &format!("{BASE}/operations"),
        cmd.clone(),
    )
    .await;
    assert_eq!(
        finish(&app, &token, cmd["request_id"].as_str().unwrap()).await["targets"][0]["status"],
        "conflict"
    );
    s.save("endpoints", endpoint, Some(1)).unwrap();
    assert_eq!(
        req(
            &app,
            &token,
            "POST",
            &format!("{BASE}/operations"),
            command(&p, "automatic")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let mut p = preview(&app, &token).await;
    save(
        &s,
        json!({"label":"New agent","kind":"agent","ui_url":"https://example.invalid"}),
        "new-agent",
    );
    assert_eq!(
        req(
            &app,
            &token,
            "POST",
            &format!("{BASE}/operations"),
            command(&p, "automatic")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    p["preview"]["targets"] = json!([]);
    assert_eq!(
        req(
            &app,
            &token,
            "POST",
            &format!("{BASE}/operations"),
            command(&p, "automatic")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert!(f.data.lock().unwrap().puts.is_empty());
    f.server.abort();
    assert_eq!(
        preview(&app, &token).await["targets"][0]["status"],
        "offline"
    );
}
#[tokio::test]
async fn ask_and_unchanged_preserve_always_and_zero_delegates_do_not_block_automatic() {
    let (_dir, s, token) = state();
    let f = fixture().await;
    save(&s, f.endpoint.clone(), "agent");
    f.data.lock().unwrap().policy["delegated_clients"] = json!([]);
    let app = server::router(s.clone());
    for (mode, status) in [
        ("automatic", "succeeded"),
        ("automatic", "unchanged"),
        ("ask", "succeeded"),
    ] {
        let cmd = command(&preview(&app, &token).await, mode);
        assert_eq!(
            req(
                &app,
                &token,
                "POST",
                &format!("{BASE}/operations"),
                cmd.clone()
            )
            .await
            .0,
            StatusCode::OK
        );
        let op = finish(&app, &token, cmd["request_id"].as_str().unwrap()).await;
        assert_eq!(op["status"], "complete");
        assert_eq!(op["targets"][0]["status"], status);
    }
    let r = f.data.lock().unwrap();
    assert_eq!(r.puts.len(), 2);
    assert_eq!(r.policy["mode"], "ask");
    assert_eq!(r.policy["rules"][0]["status"], "active");
}
#[tokio::test]
async fn admin_required_and_remote_refusal_identity_and_capability_stay_distinct() {
    let (_dir, s, token) = state();
    let f = fixture().await;
    let mut endpoint = save(&s, f.endpoint.clone(), "agent");
    let app = server::router(s.clone());
    let manage = "fixture-manage-only";
    s.db.lock()
        .unwrap()
        .execute(
            "INSERT INTO tokens VALUES(?1,'limited','[\"manage\",\"observe\"]',?2)",
            rusqlite::params![auth::digest(manage), i64::MAX],
        )
        .unwrap();
    let mut generic = f.endpoint.clone();
    generic["management"]["provider"] = json!("generic-json");
    generic["management"]["operations"] = json!([
        {"id":"disguised-policy","path":"approval-policy","method":"PUT","access":"manage"},
        {"id":"disguised-revoke","path":"approval-policy/rules/one/revoke","method":"POST","access":"manage"}]);
    save(&s, generic, "generic-policy");
    for name in ["disguised-policy", "disguised-revoke"] {
        assert_eq!(
            req(
                &app,
                manage,
                "POST",
                &format!("/api/v1/endpoints/generic-policy/operations/{name}"),
                json!({})
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    s.remove("endpoints", "generic-policy", 1).unwrap();
    assert_eq!(
        req(&app, manage, "GET", BASE, Value::Null).await.0,
        StatusCode::FORBIDDEN
    );
    let p = preview(&app, &token).await;
    assert_eq!(
        req(
            &app,
            manage,
            "POST",
            &format!("{BASE}/operations"),
            command(&p, "automatic")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    f.data.lock().unwrap().refusal = Some(StatusCode::FORBIDDEN);
    let p = preview(&app, &token).await;
    assert_eq!(p["targets"][0]["status"], "access-required");
    assert!(!p.to_string().contains("must not leak"));
    f.data.lock().unwrap().refusal = None;
    f.data.lock().unwrap().descriptor["operations"][0]["methods"] = json!(["GET"]);
    assert_eq!(
        preview(&app, &token).await["targets"][0]["status"],
        "unsupported"
    );
    f.data.lock().unwrap().descriptor["operations"][0]["methods"] = json!(["GET", "PUT"]);
    endpoint["management"]["expected_id"] = json!("wrong-instance");
    s.save("endpoints", endpoint, Some(1)).unwrap();
    assert_eq!(
        preview(&app, &token).await["targets"][0]["status"],
        "conflict"
    );
    assert!(f.data.lock().unwrap().puts.is_empty());
}
#[test]
fn service_restart_recovers_intent_without_dispatching_and_keeps_history() {
    let (dir, s, _token) = state();
    s.save("approval_policy_operations",json!({"id":"interrupted","status":"running","mode":"automatic",
        "targets":[{"endpoint_id":"one","status":"sending"},{"endpoint_id":"two","status":"pending"},
            {"endpoint_id":"three","status":"succeeded"}]}),Some(0)).unwrap();
    drop(s);
    let restarted = AppState::open(dir.path().into()).unwrap();
    let op = restarted
        .read("approval_policy_operations", "interrupted")
        .unwrap();
    assert_eq!(op["status"], "partial");
    assert_eq!(op["targets"][0]["status"], "unknown");
    assert_eq!(op["targets"][1]["status"], "not-sent");
    assert_eq!(op["targets"][2]["status"], "succeeded");
}

#[tokio::test]
async fn connection_downgrade_during_final_identity_read_prevents_dispatch() {
    let (_dir, s, token) = state();
    let f = fixture().await;
    let mut endpoint = save(&s, f.endpoint.clone(), "agent");
    let app = server::router(s.clone());
    let cmd = command(&preview(&app, &token).await, "automatic");
    let (started, release) = {
        let mut remote = f.data.lock().unwrap();
        remote.pause_descriptor_at = Some(remote.descriptor_reads + 2);
        (
            remote.descriptor_started.clone(),
            remote.descriptor_release.clone(),
        )
    };
    assert_eq!(
        req(
            &app,
            &token,
            "POST",
            &format!("{BASE}/operations"),
            cmd.clone()
        )
        .await
        .0,
        StatusCode::OK
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    {
        let _registry = s.inventory_registry_lock.lock().unwrap();
        endpoint["management"]["access"] = json!("observe");
        s.save("endpoints", endpoint, Some(1)).unwrap();
    }
    release.notify_one();
    let op = finish(&app, &token, cmd["request_id"].as_str().unwrap()).await;
    assert_eq!(op["targets"][0]["status"], "conflict");
    assert!(f.data.lock().unwrap().puts.is_empty());
}
