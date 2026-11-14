//! Disposable TLS providers exercise observation freshness and ambiguous replies.
//! No installed agent, model, or external service is involved.
use agent_observatory::{config::Config, provider, server, state::AppState};
use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::State,
    http::{HeaderMap, Method, Request, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use hyper::body::{Frame, SizeHint};
use serde_json::{Value, json};
use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tempfile::TempDir;
use tokio::sync::Notify;
use tower::ServiceExt;

const REMOTE_ID: &str = "management-fidelity-fixture";
const CREDENTIAL: &str = "fixture-management-credential";

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
    std::fs::write(dir.path().join("secrets/management-fixture"), CREDENTIAL).unwrap();
    let token = std::fs::read_to_string(dir.path().join("secrets/control-token")).unwrap();
    (dir, state, token)
}

#[derive(Clone, Copy)]
enum Reply {
    Json,
    Refuse(StatusCode),
    Malformed,
    Interrupted,
    Oversized,
}
struct Remote {
    reply: Reply,
    mutations: Vec<Value>,
    config: Value,
}
struct Control {
    remote: Mutex<Remote>,
    block_descriptor: AtomicBool,
    fail_descriptor: AtomicBool,
    entered: Notify,
    release: Notify,
}
struct Fixture {
    control: Arc<Control>,
    endpoint: Value,
    server: tokio::task::JoinHandle<()>,
    _dir: TempDir,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

// Let the TLS server send headers and a partial body before losing the stream.
struct InterruptedBody {
    sent: bool,
    delay: Pin<Box<tokio::time::Sleep>>,
}
impl hyper::body::Body for InterruptedBody {
    type Data = Bytes;
    type Error = std::io::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if !self.sent {
            self.sent = true;
            return Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"{")))));
        }
        if self.delay.as_mut().poll(cx).is_pending() {
            return Poll::Pending;
        }
        Poll::Ready(Some(Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "fixture reply interrupted",
        ))))
    }
    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(1000)
    }
}

async fn respond(
    State(control): State<Arc<Control>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    bytes: Bytes,
) -> Response {
    if headers.get("authorization").and_then(|h| h.to_str().ok())
        != Some(&format!("Bearer {CREDENTIAL}"))
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if uri.path() == "/api/v1/descriptor" {
        if control.block_descriptor.swap(false, Ordering::SeqCst) {
            control.entered.notify_one();
            control.release.notified().await;
        }
        if control.fail_descriptor.load(Ordering::SeqCst) {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        return axum::Json(json!({"instance_id":REMOTE_ID,
            "operations":[{"path":"config","methods":["GET","PUT"]}]}))
        .into_response();
    }
    if uri.path() == "/api/v1/status" {
        return axum::Json(json!({"state":"ready"})).into_response();
    }
    if uri.path() != "/api/v1/config" {
        return StatusCode::NOT_FOUND.into_response();
    }
    let (reply, config) = {
        let mut remote = control.remote.lock().unwrap();
        if method == Method::PUT {
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            remote.mutations.push(value.clone());
            if !matches!(remote.reply, Reply::Refuse(_)) {
                remote.config = value;
            }
        }
        (remote.reply, remote.config.clone())
    };
    match reply {
        Reply::Json => axum::Json(config).into_response(),
        Reply::Refuse(code) => (code, "remote body must not be exposed").into_response(),
        Reply::Malformed => (StatusCode::OK, "committed but incomplete {").into_response(),
        Reply::Interrupted => Response::new(Body::new(InterruptedBody {
            sent: false,
            delay: Box::pin(tokio::time::sleep(Duration::from_millis(20))),
        })),
        Reply::Oversized => (StatusCode::OK, "x".repeat(8 * 1024 * 1024 + 1)).into_response(),
    }
}

async fn fixture() -> Fixture {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = TempDir::new().unwrap();
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let cert_pem = cert.pem();
    let cert_path = dir.path().join("ca.pem");
    std::fs::write(&cert_path, &cert_pem).unwrap();
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem(
        cert_pem.into_bytes(),
        signing_key.serialize_pem().into_bytes(),
    )
    .await
    .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let control = Arc::new(Control {
        remote: Mutex::new(Remote {
            reply: Reply::Json,
            mutations: vec![],
            config: json!({}),
        }),
        block_descriptor: AtomicBool::new(false),
        fail_descriptor: AtomicBool::new(false),
        entered: Notify::new(),
        release: Notify::new(),
    });
    let app = Router::new().fallback(respond).with_state(control.clone());
    let server = tokio::spawn(async move {
        axum_server::from_tcp_rustls(listener, tls)
            .unwrap()
            .serve(app.into_make_service())
            .await
            .unwrap();
    });
    Fixture {
        control,
        server,
        _dir: dir,
        endpoint: json!({"label":"Managed fixture", "kind":"agent",
        "management":{"provider":"agent-instance", "base_url":format!("https://{address}"),
        "credential_ref":"management-fixture", "ca_file":cert_path, "access":"manage", "expected_id":REMOTE_ID}}),
    }
}

async fn api(
    app: &Router,
    token: &str,
    method: Method,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(format!("/api/v1/{path}"))
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
async fn add(app: &Router, token: &str, fixture: &Fixture) -> Value {
    let (status, saved) = api(
        app,
        token,
        Method::POST,
        "endpoints",
        fixture.endpoint.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    saved
}
async fn edit(app: &Router, token: &str, fixture: &Fixture, saved: &Value) -> Value {
    let mut body = fixture.endpoint.clone();
    body["management"]["expected_id"] = json!("new-installation-identity");
    body["expected_revision"] = saved["revision"].clone();
    let (status, edited) = api(
        app,
        token,
        Method::PUT,
        &format!("endpoints/{}", saved["id"].as_str().unwrap()),
        body,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{edited}");
    edited
}
async fn assert_not_current(app: &Router, token: &str, id: &str) {
    let (_, item) = api(
        app,
        token,
        Method::GET,
        &format!("endpoints/{id}"),
        Value::Null,
    )
    .await;
    assert!(
        item["observation"].is_null(),
        "old management result appeared current: {item}"
    );
    let (_, index) = api(app, token, Method::GET, "endpoints", Value::Null).await;
    let row = index["endpoints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == id)
        .unwrap();
    assert!(row["observation"].is_null());
}

#[tokio::test]
async fn management_success_records_checked_revision_and_edit_invalidates_projection() {
    let (_dir, state, token) = state();
    let fixture = fixture().await;
    let app = server::router(state.clone());
    let saved = add(&app, &token, &fixture).await;
    let id = saved["id"].as_str().unwrap();
    let (_, observation) = api(
        &app,
        &token,
        Method::POST,
        &format!("endpoints/{id}/inspect"),
        json!({}),
    )
    .await;
    assert_eq!(observation["health"], "reachable");
    assert_eq!(observation["endpoint_revision"], saved["revision"]);
    edit(&app, &token, &fixture, &saved).await;
    assert_not_current(&app, &token, id).await;
    assert_eq!(
        state.read("observations", id).unwrap()["endpoint_revision"],
        saved["revision"]
    );
}

async fn delayed_inspect(failed: bool) {
    let (_dir, state, token) = state();
    let fixture = fixture().await;
    let app = server::router(state.clone());
    let saved = add(&app, &token, &fixture).await;
    let id = saved["id"].as_str().unwrap();
    fixture
        .control
        .block_descriptor
        .store(true, Ordering::SeqCst);
    fixture
        .control
        .fail_descriptor
        .store(failed, Ordering::SeqCst);
    let pending_app = app.clone();
    let pending_token = token.clone();
    let path = format!("endpoints/{id}/inspect");
    let pending = tokio::spawn(async move {
        api(&pending_app, &pending_token, Method::POST, &path, json!({})).await
    });
    tokio::time::timeout(Duration::from_secs(5), fixture.control.entered.notified())
        .await
        .unwrap();
    let edited = edit(&app, &token, &fixture, &saved).await;
    assert_ne!(edited["revision"], saved["revision"]);
    fixture.control.release.notify_one();
    let (_, observation) = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        observation["health"],
        if failed { "unavailable" } else { "reachable" }
    );
    assert_eq!(observation["endpoint_revision"], saved["revision"]);
    assert_not_current(&app, &token, id).await;
}
#[tokio::test]
async fn inflight_management_success_cannot_bind_to_a_later_endpoint_revision() {
    delayed_inspect(false).await;
}
#[tokio::test]
async fn inflight_management_failure_cannot_bind_to_a_later_endpoint_revision() {
    delayed_inspect(true).await;
}

#[tokio::test]
async fn legacy_management_observation_is_retained_but_not_presented_as_current() {
    let (_dir, state, token) = state();
    let fixture = fixture().await;
    let app = server::router(state.clone());
    let saved = add(&app, &token, &fixture).await;
    let id = saved["id"].as_str().unwrap();
    state
        .save(
            "observations",
            json!({"id":id,"health":"reachable","observed_at":123}),
            None,
        )
        .unwrap();
    assert_not_current(&app, &token, id).await;
    assert_eq!(
        state.read("observations", id).unwrap()["health"],
        "reachable"
    );
}

#[tokio::test]
async fn generic_refusal_denial_conflict_and_unsupported_outcomes_keep_their_class() {
    let (_dir, state, token) = state();
    let fixture = fixture().await;
    let app = server::router(state);
    let saved = add(&app, &token, &fixture).await;
    let path = format!(
        "endpoints/{}/operations/apply-config",
        saved["id"].as_str().unwrap()
    );
    for (index, (remote, expected)) in [
        (400, 400),
        (401, 403),
        (403, 403),
        (404, 501),
        (405, 501),
        (409, 409),
        (422, 422),
        (429, 429),
        (500, 502),
        (501, 501),
        (503, 502),
    ]
    .into_iter()
    .enumerate()
    {
        fixture.control.remote.lock().unwrap().reply =
            Reply::Refuse(StatusCode::from_u16(remote).unwrap());
        let (status, result) = api(&app, &token, Method::POST, &path, json!({"revision":1})).await;
        assert_eq!(status.as_u16(), expected, "remote {remote}: {result}");
        assert!(
            !result
                .to_string()
                .contains("remote body must not be exposed")
        );
        assert_eq!(
            fixture.control.remote.lock().unwrap().mutations.len(),
            index + 1
        );
    }
}

async fn uncertain_mutation(reply: Reply) {
    let (_dir, state, token) = state();
    let fixture = fixture().await;
    let app = server::router(state.clone());
    let saved = add(&app, &token, &fixture).await;
    fixture.control.remote.lock().unwrap().reply = reply;
    let intent = json!({"revision":1,"request_id":uuid::Uuid::new_v4().to_string()});
    let path = format!(
        "endpoints/{}/operations/apply-config",
        saved["id"].as_str().unwrap()
    );
    let (status, result) = api(&app, &token, Method::POST, &path, intent.clone()).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{result}");
    assert!(
        result["error"].as_str().unwrap().contains("unknown"),
        "{result}"
    );
    assert_eq!(
        fixture.control.remote.lock().unwrap().mutations,
        vec![intent.clone()]
    );
    fixture.control.remote.lock().unwrap().reply = Reply::Json;
    let observed = provider::call(&state, &saved, "config", None)
        .await
        .unwrap();
    assert_eq!(observed, intent);
    assert_eq!(fixture.control.remote.lock().unwrap().mutations.len(), 1);
}
#[tokio::test]
async fn malformed_reply_after_committed_mutation_remains_unknown_without_resend() {
    uncertain_mutation(Reply::Malformed).await;
}
#[tokio::test]
async fn interrupted_reply_after_committed_mutation_remains_unknown_without_resend() {
    uncertain_mutation(Reply::Interrupted).await;
}
#[tokio::test]
async fn oversized_reply_after_committed_mutation_remains_unknown_without_resend() {
    uncertain_mutation(Reply::Oversized).await;
}

#[tokio::test]
async fn malformed_read_is_a_provider_response_failure_not_a_caller_error() {
    let (_dir, state, _) = state();
    let fixture = fixture().await;
    fixture.control.remote.lock().unwrap().reply = Reply::Malformed;
    let error = provider::call(&state, &fixture.endpoint, "config", None)
        .await
        .unwrap_err();
    assert_eq!(error.0, StatusCode::BAD_GATEWAY);
    assert_eq!(fixture.control.remote.lock().unwrap().mutations.len(), 0);
}
