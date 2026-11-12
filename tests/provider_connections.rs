//! Disposable TLS management fixture: exercises real authenticated forwarding,
//! not an Instance/domain lifecycle or a claim that a connector is connected.
use agent_observatory::{config::Config, provider, server, state::AppState};
use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::State,
    http::{HeaderMap, Method, Request, StatusCode, Uri},
    response::IntoResponse,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tempfile::TempDir;
use tower::ServiceExt;

const REMOTE_ID: &str = "fixture-instance";
const CREDENTIAL: &str = "fixture-installation-admin-credential";

fn state() -> (TempDir, Arc<AppState>, String) {
    let dir = TempDir::new().unwrap();
    Config {
        id: uuid::Uuid::new_v4().to_string(),
        owner: "fixture".into(),
        bind: "127.0.0.1".into(),
        port: 0,
        public_url: String::new(),
        pam_service: "unused".into(),
        tls_cert: PathBuf::new(),
        tls_key: PathBuf::new(),
        tls_ca: None,
        embed_origins: vec![],
    }
    .save(dir.path())
    .unwrap();
    let state = AppState::open(dir.path().into()).unwrap();
    std::fs::write(dir.path().join("secrets/instance-fixture"), CREDENTIAL).unwrap();
    let token = std::fs::read_to_string(dir.path().join("secrets/control-token")).unwrap();
    (dir, state, token)
}

fn command(action: &str) -> Value {
    json!({"connection_id":"minecraft:workshop", "revision":"owner-revision",
        "action":action, "request_id":uuid::Uuid::new_v4().to_string()})
}

#[derive(Clone, Debug)]
struct Seen {
    method: Method,
    path: String,
    body: Value,
}
struct Remote {
    descriptor: Value,
    catalog: Value,
    requests: BTreeMap<String, Value>,
    calls: Vec<Seen>,
    refusal: Option<StatusCode>,
    malformed_reply: bool,
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
    headers: HeaderMap,
    bytes: Bytes,
) -> axum::response::Response {
    if headers.get("authorization").and_then(|h| h.to_str().ok())
        != Some(&format!("Bearer {CREDENTIAL}"))
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    let mut remote = data.lock().unwrap();
    remote.calls.push(Seen {
        method: method.clone(),
        path: uri.path().to_string(),
        body: body.clone(),
    });
    let (status, result) = match (method.as_str(), uri.path()) {
        ("GET", "/api/v1/descriptor") => (StatusCode::OK, remote.descriptor.clone()),
        ("GET", "/api/v1/service-connections") => (StatusCode::OK, remote.catalog.clone()),
        ("POST", "/api/v1/service-connections") => {
            if let Some(status) = remote.refusal {
                (status, json!({"error":"fixture remote refusal"}))
            } else if body["revision"] != "owner-revision"
                || (body["action"] == "add" && body["values"]["server"] != "workshop")
            {
                (
                    StatusCode::BAD_REQUEST,
                    json!({"error":"fixture template refused"}),
                )
            } else {
                let request_id = body["request_id"].as_str().unwrap().to_string();
                if let Some(old) = remote.requests.get(&request_id) {
                    if old["command"] == body {
                        (StatusCode::OK, old.clone())
                    } else {
                        (StatusCode::CONFLICT, json!({"error":"request_id_reused"}))
                    }
                } else {
                    let operation = json!({"command":body, "created_at":123,
                        "finished_at":null, "result":null});
                    remote.requests.insert(request_id, operation.clone());
                    remote.catalog["request"] = operation.clone();
                    (StatusCode::OK, operation)
                }
            }
        }
        _ => (StatusCode::NOT_FOUND, json!({"error":"no fixture route"})),
    };
    if method == Method::POST && remote.malformed_reply {
        return (StatusCode::OK, "truncated acknowledgement {").into_response();
    }
    (status, axum::Json(result)).into_response()
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
    let data = Arc::new(Mutex::new(Remote {
        descriptor: json!({"instance_id":REMOTE_ID,
            "capabilities":["constellation.service-connections/1"],
            "operations":[{"path":"service-connections", "methods":["GET","POST"], "scope":"admin"}]}),
        catalog: json!({"schema":1,"services":[{"id":"minecraft","label":"Minecraft","enrolled":true,
            "add":{"revision":"owner-revision","fields":[
                {"name":"server","label":"Server","kind":"choice","required":true,"choices":[{"value":"workshop","label":"Workshop"}]}]}}],
            "connections":[{"id":"minecraft:workshop","service":"minecraft","label":"Workshop",
                "revision":"owner-revision","enrolled":true,"state":"unknown","desired_state":"connected",
                "context_active":false,"settled":false,"context_ready":false}],
            "observed_at":123,"helper_connected":false,"request":null,"last_operation":null}),
        requests: BTreeMap::new(),
        calls: vec![],
        refusal: None,
        malformed_reply: false,
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
        endpoint: json!({"label":"Fixture agent", "kind":"agent",
        "management":{"provider":"agent-instance","base_url":format!("https://{address}"),
            "credential_ref":"instance-fixture","ca_file":cert_path,"access":"manage","expected_id":REMOTE_ID}}),
        server,
        _dir: dir,
    }
}

#[test]
fn saved_catalog_requires_named_capability_and_each_exact_method() {
    let endpoint = json!({"management":{"provider":"agent-instance"}});
    let mut descriptor =
        json!({"operations":[{"path":"service-connections","methods":["GET","POST"]}]});
    let has = |d: &Value, id: &str| {
        provider::available_operations(&endpoint, d)
            .iter()
            .any(|o| o.id == id)
    };
    assert!(!has(&descriptor, "service-connections"));
    descriptor["capabilities"] = json!(["constellation.service-connections/1"]);
    assert!(has(&descriptor, "service-connections"));
    assert!(has(&descriptor, "service-connection-action"));
    descriptor["operations"][0]["methods"] = json!(["GET"]);
    assert!(!has(&descriptor, "service-connection-action"));
    assert!(!has(&json!({}), "service-connections"));
    assert!(
        !provider::operations(&endpoint)
            .iter()
            .any(|o| o.path.ends_with("/report"))
    );
}

#[test]
fn connection_identifiers_are_data_and_commands_cannot_select_host_paths() {
    let op = provider::operations(&json!({"management":{"provider":"agent-instance"}}))
        .into_iter()
        .find(|o| o.id == "service-connection-action")
        .unwrap();
    for action in ["connect", "disconnect"] {
        let body = command(action);
        assert_eq!(
            provider::operation_request(&op, Some(&body)).unwrap(),
            ("service-connections".into(), Some(body))
        );
    }
    let original = command("connect");
    for (key, value) in [
        ("connection_id", json!("../../report")),
        ("connection_id", json!("https://elsewhere")),
        ("request_id", json!("123")),
        ("request_id", json!("ABCDEFAB-1234-4567-8910-ABCDEFABCDEF")),
        ("revision", json!("")),
        ("action", json!("report")),
        ("path", json!("context/providers")),
        ("argv", json!(["sh", "-c", "anything"])),
        ("values", json!({"extra":"invalid"})),
    ] {
        let mut bad = original.clone();
        bad[key] = value;
        assert!(
            provider::operation_request(&op, Some(&bad)).is_err(),
            "accepted {bad}"
        );
    }
}

#[test]
fn typed_add_accepts_only_bounded_primitive_setup_values() {
    let op = provider::operations(&json!({"management":{"provider":"agent-instance"}}))
        .into_iter()
        .find(|o| o.id == "service-connection-action")
        .unwrap();
    let good = json!({"service_id":"minecraft","revision":"owner-revision","action":"add",
        "request_id":uuid::Uuid::new_v4().to_string(),"values":{"label":"Workshop","server":"workshop","count":1,"enabled":true}});
    assert!(provider::operation_request(&op, Some(&good)).is_ok());
    for values in [
        json!({"password":"secret"}),
        json!({"value":{"command":"anything"}}),
        json!({"items":["a"]}),
        json!({"n":1.1}),
        json!({"text":"x".repeat(1001)}),
        json!({"null":null}),
        json!({"bad/path":"x"}),
    ] {
        let mut body = good.clone();
        body["values"] = values;
        assert!(provider::operation_request(&op, Some(&body)).is_err());
    }
}

#[test]
fn optional_machine_association_is_an_identifier_not_an_address() {
    let mut endpoint = json!({"label":"Agent", "ui_url":"https://example.test"});
    for host in [Value::Null, json!(uuid::Uuid::new_v4().to_string())] {
        endpoint["host_id"] = host;
        assert!(provider::validate(&endpoint).is_ok());
    }
    for host in [
        json!("spark"),
        json!("https://machine"),
        json!(1),
        json!({"id":"x"}),
    ] {
        endpoint["host_id"] = host;
        assert!(provider::validate(&endpoint).is_err());
    }
}

#[tokio::test]
async fn real_tls_catalog_keeps_offline_unknown_and_desired_state_distinct() {
    let (_dir, state, _) = state();
    let f = fixture().await;
    let result = provider::call(&state, &f.endpoint, "service-connections", Some(&json!({})))
        .await
        .unwrap();
    let remote = f.data.lock().unwrap();
    assert_eq!(result, remote.catalog);
    assert_eq!(result["connections"][0]["state"], "unknown");
    assert_eq!(result["connections"][0]["desired_state"], "connected");
    assert_eq!(result["helper_connected"], false);
    assert_eq!(remote.calls.len(), 2);
    assert_eq!(remote.calls[0].path, "/api/v1/descriptor");
    assert_eq!(remote.calls[1].path, "/api/v1/service-connections");
    assert_eq!(remote.calls[1].body, Value::Null);
}

#[tokio::test]
async fn real_tls_action_is_queued_once_and_same_request_reconciliation_keeps_identity() {
    let (_dir, state, _) = state();
    let f = fixture().await;
    let body = command("disconnect");
    let result = provider::call(
        &state,
        &f.endpoint,
        "service-connection-action",
        Some(&body),
    )
    .await
    .unwrap();
    assert_eq!(result["command"], body);
    assert!(result["finished_at"].is_null());
    assert!(result["result"].is_null());
    assert!(result.get("state").is_none());
    let same = provider::call(
        &state,
        &f.endpoint,
        "service-connection-action",
        Some(&body),
    )
    .await
    .unwrap();
    assert_eq!(result, same);
    let mut changed = body.clone();
    changed["action"] = json!("connect");
    let error = provider::call(
        &state,
        &f.endpoint,
        "service-connection-action",
        Some(&changed),
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, StatusCode::CONFLICT);
    let remote = f.data.lock().unwrap();
    assert_eq!(remote.requests.len(), 1);
    assert_eq!(remote.catalog["connections"][0]["state"], "unknown");
    assert_eq!(
        remote
            .calls
            .iter()
            .filter(|c| c.method == Method::POST)
            .count(),
        3
    );
}

#[tokio::test]
async fn identity_and_capability_are_checked_before_catalog_or_command() {
    let (_dir, state, _) = state();
    let f = fixture().await;
    let mut unpinned = f.endpoint.clone();
    unpinned["management"]
        .as_object_mut()
        .unwrap()
        .remove("expected_id");
    assert!(
        provider::call(&state, &unpinned, "service-connections", None)
            .await
            .unwrap_err()
            .1
            .contains("pin")
    );
    assert!(f.data.lock().unwrap().calls.is_empty());
    f.data.lock().unwrap().descriptor["instance_id"] = json!("different-instance");
    assert!(
        provider::call(
            &state,
            &f.endpoint,
            "service-connection-action",
            Some(&command("connect"))
        )
        .await
        .unwrap_err()
        .1
        .contains("identity changed")
    );
    {
        let mut r = f.data.lock().unwrap();
        r.descriptor["instance_id"] = json!(REMOTE_ID);
        r.descriptor["capabilities"] = json!([]);
    }
    assert_eq!(
        provider::call(&state, &f.endpoint, "service-connections", None)
            .await
            .unwrap_err()
            .0,
        StatusCode::NOT_IMPLEMENTED
    );
    assert!(
        f.data
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|c| c.path == "/api/v1/descriptor")
    );
}

#[tokio::test]
async fn actual_template_refusal_and_uncertain_server_failure_never_retry() {
    let (_dir, state, _) = state();
    let f = fixture().await;
    let mut body = json!({"service_id":"minecraft","revision":"owner-revision","action":"add",
        "request_id":uuid::Uuid::new_v4().to_string(),"values":{"server":"not-enrolled"}});
    assert_eq!(
        provider::call(
            &state,
            &f.endpoint,
            "service-connection-action",
            Some(&body)
        )
        .await
        .unwrap_err()
        .0,
        StatusCode::BAD_REQUEST
    );
    body["values"]["server"] = json!("workshop");
    assert_eq!(
        provider::call(
            &state,
            &f.endpoint,
            "service-connection-action",
            Some(&body)
        )
        .await
        .unwrap()["command"],
        body
    );
    f.data.lock().unwrap().refusal = Some(StatusCode::INTERNAL_SERVER_ERROR);
    let error = provider::call(
        &state,
        &f.endpoint,
        "service-connection-action",
        Some(&command("connect")),
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, StatusCode::BAD_GATEWAY);
    assert!(error.1.contains("could not confirm"));
    assert_eq!(
        f.data
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|c| c.method == Method::POST)
            .count(),
        3
    );
}

#[tokio::test]
async fn committed_but_unreadable_ack_is_reconciled_by_catalog_without_resubmitting() {
    let (_dir, state, _) = state();
    let f = fixture().await;
    f.data.lock().unwrap().malformed_reply = true;
    let body = command("connect");
    let error = provider::call(
        &state,
        &f.endpoint,
        "service-connection-action",
        Some(&body),
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, StatusCode::BAD_GATEWAY);
    assert!(error.1.contains("response was incomplete"));
    let catalog = provider::call(&state, &f.endpoint, "service-connections", None)
        .await
        .unwrap();
    assert_eq!(catalog["request"]["command"], body);
    assert!(catalog["request"]["finished_at"].is_null());
    assert_eq!(catalog["connections"][0]["state"], "unknown");
    let remote = f.data.lock().unwrap();
    assert_eq!(remote.requests.len(), 1);
    assert_eq!(
        remote
            .calls
            .iter()
            .filter(|c| c.method == Method::POST)
            .count(),
        1
    );
}

async fn api(app: &Router, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
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

#[tokio::test]
async fn local_observer_can_read_catalog_but_neither_local_nor_endpoint_scope_can_mutate() {
    let (_dir, state, admin) = state();
    let f = fixture().await;
    let app = server::router(state.clone());
    let (status, endpoint) = api(&app, &admin, "/api/v1/endpoints", f.endpoint.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let (_, token) = api(
        &app,
        &admin,
        "/api/v1/tokens",
        json!({"name":"observer","scopes":["observe"]}),
    )
    .await;
    let token = token["token"].as_str().unwrap();
    let path = format!(
        "/api/v1/endpoints/{}/operations/",
        endpoint["id"].as_str().unwrap()
    );
    assert_eq!(
        api(
            &app,
            token,
            &format!("{path}service-connections"),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        api(
            &app,
            token,
            &format!("{path}service-connection-action"),
            command("connect")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let mut observation_only = f.endpoint.clone();
    observation_only["management"]["access"] = json!("observe");
    assert!(
        provider::call(
            &state,
            &observation_only,
            "service-connection-action",
            Some(&command("connect"))
        )
        .await
        .unwrap_err()
        .1
        .contains("observation")
    );
    assert!(
        f.data
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|c| c.method == Method::GET)
    );
}
