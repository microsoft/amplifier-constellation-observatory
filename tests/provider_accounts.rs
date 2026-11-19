//! Fixed account setup mappings and disposable TLS admission fixtures.
//! No installed agent, external account, model or provider is contacted.
use agent_observatory::provider;
use serde_json::{Value, json};

fn endpoint() -> Value {
    json!({"management":{"provider":"agent-instance","access":"manage","expected_id":"fixture"}})
}
fn advertised() -> Value {
    json!({"capabilities":["agent-instance.native-account/1"],"operations":[
        {"path":"native-account","methods":["GET","DELETE"]},
        {"path":"native-account/login","methods":["GET","POST","DELETE"]},
        {"path":"native-account/models","methods":["POST"]},
        {"path":"native-config/credential","methods":["POST"]},
        {"path":"backend/account","methods":["GET"]},
        {"path":"backend/account/login","methods":["GET","POST","DELETE"]}
    ]})
}
#[test]
fn account_routes_require_exact_advertisement_and_administration() {
    let names = [
        "native-account",
        "native-login",
        "start-native-login",
        "cancel-native-login",
        "native-models",
        "native-credential",
        "backend-account",
        "backend-login",
        "start-backend-login",
        "cancel-backend-login",
    ];
    let all = provider::available_operations(&endpoint(), &advertised());
    for name in names {
        assert_eq!(all.iter().find(|o| o.id == name).unwrap().access, "admin");
        assert!(
            !provider::available_operations(&endpoint(), &json!({}))
                .iter()
                .any(|o| o.id == name)
        );
    }
    let mut d = advertised();
    d["capabilities"] = json!([]);
    assert!(
        !provider::available_operations(&endpoint(), &d)
            .iter()
            .any(|o| o.id == "start-native-login")
    );
    let mut d = advertised();
    d["operations"][1]["methods"] = json!(["GET"]);
    assert!(
        !provider::available_operations(&endpoint(), &d)
            .iter()
            .any(|o| o.id == "start-native-login")
    );
    assert!(
        !all.iter()
            .any(|o| o.path == "native-account" && o.method == "DELETE"),
        "Onboarding must not revoke an existing account"
    );
}
#[test]
fn account_requests_cannot_choose_commands_urls_credentials_files_or_callback_ports() {
    let operations = provider::operations(&endpoint());
    let start = operations
        .iter()
        .find(|o| o.id == "start-native-login")
        .unwrap();
    let body = json!({"expected_backend_sha256":"a".repeat(64),"callback_port":0});
    assert_eq!(
        provider::operation_request(start, Some(&body)).unwrap().0,
        "native-account/login"
    );
    for key in [
        "command",
        "authorization_url",
        "credentials_file",
        "callback_host",
    ] {
        let mut bad = body.clone();
        bad[key] = json!("unexpected");
        assert!(provider::operation_request(start, Some(&bad)).is_err());
    }
    let mut bad = body.clone();
    bad["callback_port"] = json!(1455);
    assert!(provider::operation_request(start, Some(&bad)).is_err());
    let mut bad = body;
    bad["expected_backend_sha256"] = json!("stale");
    assert!(provider::operation_request(start, Some(&bad)).is_err());
    let cancel = operations
        .iter()
        .find(|o| o.id == "cancel-native-login")
        .unwrap();
    assert!(
        provider::operation_request(cancel, Some(&json!({"job_id":uuid::Uuid::new_v4()}))).is_ok()
    );
    assert!(provider::operation_request(cancel, Some(&json!({}))).is_err());
}
#[test]
fn credential_is_bounded_write_only_and_status_accepts_no_private_fields() {
    let operations = provider::operations(&endpoint());
    let key = operations
        .iter()
        .find(|o| o.id == "native-credential")
        .unwrap();
    assert_eq!(key.method, "POST");
    assert!(
        provider::operation_request(
            key,
            Some(&json!({"expected_backend_sha256":"b".repeat(64),"value":"fixture-value"}))
        )
        .is_ok()
    );
    for value in [
        json!(""),
        json!(" "),
        json!("x".repeat(65537)),
        json!({"hidden":"value"}),
    ] {
        assert!(
            provider::operation_request(
                key,
                Some(&json!({"expected_backend_sha256":"b".repeat(64),"value":value}))
            )
            .is_err()
        );
    }
    let status = operations
        .iter()
        .find(|o| o.id == "native-account")
        .unwrap();
    assert!(
        provider::operation_request(status, Some(&json!({})))
            .unwrap()
            .1
            .is_none()
    );
    assert!(
        provider::operation_request(status, Some(&json!({"value":"never-send-on-read"}))).is_err()
    );
}

mod dispatch_admission {
    use super::*;
    use agent_observatory::{config::Config, state::AppState};
    use axum::{
        Router,
        body::Bytes,
        extract::State,
        http::{Method, StatusCode, Uri},
        response::{IntoResponse, Response},
    };
    use std::{
        path::PathBuf,
        sync::{Arc, Mutex},
        time::Duration,
    };
    use tempfile::TempDir;
    use tokio::sync::Notify;

    struct Remote {
        entered: Notify,
        release: Notify,
        mutations: Mutex<Vec<(String, Value)>>,
    }
    struct Fixture {
        state: Arc<AppState>,
        endpoint: Value,
        remote: Arc<Remote>,
        server: tokio::task::JoinHandle<()>,
        _dir: TempDir,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.server.abort();
        }
    }
    async fn respond(
        State(remote): State<Arc<Remote>>,
        method: Method,
        uri: Uri,
        bytes: Bytes,
    ) -> Response {
        if method == Method::GET && uri.path() == "/api/v1/descriptor" {
            remote.entered.notify_one();
            remote.release.notified().await;
            let mut descriptor = advertised();
            descriptor["instance_id"] = json!("fixture");
            descriptor["operations"].as_array_mut().unwrap().extend([
                json!({"path":"native-config","methods":["PUT"]}),
                json!({"path":"config","methods":["PUT"]}),
                json!({"path":"lifecycle","methods":["POST"]}),
            ]);
            return axum::Json(descriptor).into_response();
        }
        remote
            .mutations
            .lock()
            .unwrap()
            .push((uri.path().into(), serde_json::from_slice(&bytes).unwrap()));
        (StatusCode::OK, axum::Json(json!({"accepted":true}))).into_response()
    }
    async fn fixture() -> Fixture {
        let _ = rustls::crypto::ring::default_provider().install_default();
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
        std::fs::write(
            dir.path().join("secrets/setup-fixture"),
            "disposable-fixture-credential",
        )
        .unwrap();
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let cert_pem = cert.pem();
        let ca = dir.path().join("ca.pem");
        std::fs::write(&ca, &cert_pem).unwrap();
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem(
            cert_pem.into_bytes(),
            signing_key.serialize_pem().into_bytes(),
        )
        .await
        .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let remote = Arc::new(Remote {
            entered: Notify::new(),
            release: Notify::new(),
            mutations: Mutex::new(vec![]),
        });
        let app = Router::new().fallback(respond).with_state(remote.clone());
        let server = tokio::spawn(async move {
            axum_server::from_tcp_rustls(listener, tls)
                .unwrap()
                .serve(app.into_make_service())
                .await
                .unwrap();
        });
        let endpoint = state.save("endpoints", json!({"id":uuid::Uuid::new_v4(),"label":"Disposable setup fixture","kind":"agent",
            "management":{"provider":"agent-instance","access":"manage","expected_id":"fixture","base_url":format!("https://{address}"),
                "ca_file":ca,"credential_ref":"setup-fixture"}}),Some(0)).unwrap();
        Fixture {
            state,
            endpoint,
            remote,
            server,
            _dir: dir,
        }
    }
    fn commands() -> Vec<(&'static str, Value)> {
        vec![
            (
                "native-credential",
                json!({"expected_backend_sha256":"b".repeat(64),"value":"must-not-reach-stale-endpoint"}),
            ),
            (
                "start-native-login",
                json!({"expected_backend_sha256":"b".repeat(64),"callback_port":0}),
            ),
            (
                "cancel-native-login",
                json!({"job_id":uuid::Uuid::new_v4()}),
            ),
            ("start-backend-login", json!({})),
            ("cancel-backend-login", json!({})),
            (
                "apply-native-config",
                json!({"config":{"provider":"fixture"}}),
            ),
            ("apply-config", json!({"config":{"setup_complete":true}})),
            ("lifecycle", json!({"action":"start"})),
        ]
    }
    async fn exercise(change: Option<bool>) {
        for (operation, body) in commands() {
            let f = fixture().await;
            let s = f.state.clone();
            let e = f.endpoint.clone();
            let task =
                tokio::spawn(async move { provider::call(&s, &e, operation, Some(&body)).await });
            tokio::time::timeout(Duration::from_secs(5), f.remote.entered.notified())
                .await
                .unwrap();
            if let Some(delete) = change {
                let _registry = f.state.inventory_registry_lock.lock().unwrap();
                if delete {
                    f.state
                        .remove("endpoints", f.endpoint["id"].as_str().unwrap(), 1)
                        .unwrap();
                } else {
                    let mut changed = f.endpoint.clone();
                    changed["management"]["access"] = json!("observe");
                    f.state.save("endpoints", changed, Some(1)).unwrap();
                }
            }
            f.remote.release.notify_one();
            let result = tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .unwrap()
                .unwrap();
            if change.is_some() {
                assert!(
                    result.is_err(),
                    "{operation} dispatched after saved connection changed"
                );
                assert_eq!(result.unwrap_err().0, StatusCode::CONFLICT, "{operation}");
                assert!(
                    f.remote.mutations.lock().unwrap().is_empty(),
                    "{operation} sent a mutation or secret to the old connection"
                );
            } else {
                assert_eq!(result.unwrap()["accepted"], true, "{operation}");
                assert_eq!(
                    f.remote.mutations.lock().unwrap().len(),
                    1,
                    "{operation} must dispatch once"
                );
            }
        }
    }
    #[tokio::test]
    async fn account_and_config_writes_refuse_edit_during_identity_discovery() {
        exercise(Some(false)).await;
    }
    #[tokio::test]
    async fn account_and_config_writes_refuse_deletion_during_identity_discovery() {
        exercise(Some(true)).await;
    }
    #[tokio::test]
    async fn account_and_config_writes_dispatch_once_when_binding_is_unchanged() {
        exercise(None).await;
    }
}
