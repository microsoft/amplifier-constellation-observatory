//! Real TLS against a selected interface; no external service or agent call.
use agent_observatory::{
    auth,
    config::{AuthMode, Config, WebTransport},
    machine_network, server,
    state::AppState,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tower::ServiceExt;

fn fixture() -> (tempfile::TempDir, Arc<AppState>, Router, String) {
    let home = tempfile::tempdir().unwrap();
    Config {
        id: uuid::Uuid::new_v4().to_string(),
        owner: "fixture".into(),
        bind: "127.0.0.1".into(),
        port: 32123,
        public_url: "http://127.0.0.1:32123".into(),
        auth_mode: AuthMode::LocalPassword,
        web_transport: WebTransport::LocalHttp,
        pam_service: String::new(),
        tls_cert: PathBuf::new(),
        tls_key: PathBuf::new(),
        tls_ca: None,
        embed_origins: vec![],
    }
    .save(home.path())
    .unwrap();
    let state = AppState::open(home.path().into()).unwrap();
    let token = std::fs::read_to_string(home.path().join("secrets/control-token")).unwrap();
    let app = server::router(state.clone());
    (home, state, app, token)
}
async fn call(
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
                .header("host", "127.0.0.1:32123")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
async fn network(app: &Router, token: &str) -> Value {
    call(app, token, "GET", "/api/v1/machine-network", Value::Null)
        .await
        .1
}
#[tokio::test]
async fn network_enable_requires_explicit_owned_address_and_preserves_local_config() {
    let (home, _state, app, token) = fixture();
    let original = std::fs::read(home.path().join("config.json")).unwrap();
    assert_eq!(network(&app, &token).await["available"], false);
    for address in ["0.0.0.0", "127.0.0.1", "203.0.113.244"] {
        let (status,_)=call(&app,&token,"POST","/api/v1/machine-network",json!({"request_id":uuid::Uuid::new_v4(),"expected_revision":0,"enabled":true,"address":address})).await;
        assert!(status.is_client_error());
        assert!(!home.path().join("machine-network.json").exists());
    }
    assert_eq!(
        std::fs::read(home.path().join("config.json")).unwrap(),
        original
    );
    assert_eq!(
        call(&app, &token, "GET", "/api/v1/hosts", Value::Null)
            .await
            .0,
        StatusCode::OK
    );
}
#[tokio::test]
async fn actual_helper_tls_is_scoped_idempotent_and_retains_grants_when_disabled() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (home, state, app, token) = fixture();
    let before = std::fs::read(home.path().join("config.json")).unwrap();
    let discovery = network(&app, &token).await;
    let address = discovery["addresses"]
        .as_array()
        .unwrap()
        .first()
        .expect("Native test host must have a network interface")["address"]
        .clone();
    let enable = json!({"request_id":uuid::Uuid::new_v4(),"expected_revision":0,"enabled":true,"address":address});
    let (code, ready) = call(
        &app,
        &token,
        "POST",
        "/api/v1/machine-network",
        enable.clone(),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{ready}");
    assert_eq!(ready["available"], true);
    let ca = std::fs::read(home.path().join("machine-network-tls/ca.pem")).unwrap();
    let endpoint = ready["endpoint"].as_str().unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .add_root_certificate(reqwest::Certificate::from_pem(&ca).unwrap())
        .build()
        .unwrap();
    for path in [
        "/",
        "/auth/login",
        "/api/v1/status",
        "/api/v1/endpoints",
        "/api/v1/machine-network",
    ] {
        let response = client
            .get(format!("{endpoint}{path}"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
    let (_, created) = call(
        &app,
        &token,
        "POST",
        "/api/v1/machine-pairings",
        json!({"request_id":uuid::Uuid::new_v4()}),
    )
    .await;
    let path = format!(
        "{endpoint}/api/v1/machine-pairings/{}/redeem",
        created["pairing"]["id"].as_str().unwrap()
    );
    let claim = json!({"schema":1,"observatory_id":state.config.id,"code":created["pairing"]["code"],"request_id":uuid::Uuid::new_v4(),"machine_id":uuid::Uuid::new_v4(),"machine_credential":auth::random_token(),"name":"Disposable computer","os":"linux","arch":"aarch64","helper_version":"0.2.0"});
    assert_eq!(
        client
            .post(&path)
            .header("origin", "https://unrelated.invalid")
            .json(&claim)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let response = client.post(&path).json(&claim).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let receipt: Value = response.json().await.unwrap();
    assert_eq!(receipt["access"], json!(["observe"]));
    let host = receipt["host_id"].as_str().unwrap();
    let heartbeat = json!({"schema":1,"machine_id":claim["machine_id"],"name":"Disposable computer","os":"linux","arch":"aarch64","helper_version":"0.2.0"});
    assert_eq!(
        client
            .post(format!("{endpoint}/api/v1/machines/{host}/heartbeat"))
            .bearer_auth(claim["machine_credential"].as_str().unwrap())
            .json(&heartbeat)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    // Admin control is intentionally absent from this helper-only address.
    assert_eq!(
        client
            .post(format!("{endpoint}/api/v1/machines/{host}/revoke"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    // Strict TLS does not trust an unapproved installation certificate.
    assert!(
        reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap()
            .get(format!("{endpoint}/"))
            .send()
            .await
            .is_err()
    );
    // Reuse the normal app's update hold on helper mutations as well.
    *state.update_hold.lock().unwrap() = Some(
        state
            .operation_gate
            .clone()
            .try_acquire_many_owned(4)
            .unwrap(),
    );
    for suffix in ["heartbeat", "revoke"] {
        assert_eq!(
            client
                .post(format!("{endpoint}/api/v1/machines/{host}/{suffix}"))
                .bearer_auth(claim["machine_credential"].as_str().unwrap())
                .json(&heartbeat)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
    assert_eq!(
        client
            .post(&path)
            .json(&claim)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        call(
            &app,
            &token,
            "POST",
            &format!("/api/v1/machines/{host}/revoke"),
            Value::Null
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    state.update_hold.lock().unwrap().take();
    // A committed request with a lost reply keeps its exact endpoint, code and CA.
    let (_, same) = call(
        &app,
        &token,
        "POST",
        "/api/v1/machine-network",
        enable.clone(),
    )
    .await;
    assert_eq!(same["revision"], ready["revision"]);
    assert_eq!(same["endpoint"], ready["endpoint"]);
    assert_eq!(
        std::fs::read(home.path().join("machine-network-tls/ca.pem")).unwrap(),
        ca
    );
    let (_, second) = call(
        &app,
        &token,
        "POST",
        "/api/v1/machine-pairings",
        json!({"request_id":uuid::Uuid::new_v4()}),
    )
    .await;
    let mut other = claim.clone();
    other["code"] = second["pairing"]["code"].clone();
    other["machine_id"] = json!(uuid::Uuid::new_v4());
    other["request_id"] = json!(uuid::Uuid::new_v4());
    other["machine_credential"] = json!(auth::random_token());
    let second: Value = client
        .post(format!(
            "{endpoint}/api/v1/machine-pairings/{}/redeem",
            second["pairing"]["id"].as_str().unwrap()
        ))
        .json(&other)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let second_host = second["host_id"].as_str().unwrap();
    assert_eq!(
        client
            .post(format!("{endpoint}/api/v1/machines/{host}/revoke"))
            .bearer_auth(other["machine_credential"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .post(format!("{endpoint}/api/v1/machines/{second_host}/revoke"))
            .bearer_auth(other["machine_credential"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let (_,off)=call(&app,&token,"POST","/api/v1/machine-network",json!({"request_id":uuid::Uuid::new_v4(),"expected_revision":1,"enabled":false,"address":address})).await;
    assert_eq!(off["available"], false);
    assert_eq!(off["enabled"], false);
    assert_eq!(
        call(&app, &token, "POST", "/api/v1/machine-network", enable)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let grants: i64 = state
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM machine_grants WHERE host_id=?1 AND revoked_at IS NULL",
            [host],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(grants, 1);
    assert_eq!(
        call(
            &app,
            &token,
            "POST",
            &format!("/api/v1/machines/{host}/revoke"),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        std::fs::read(home.path().join("config.json")).unwrap(),
        before
    );
    drop(client);
    state.shutdown.send(true).unwrap();
}
#[tokio::test]
async fn unavailable_saved_network_keeps_local_observatory_and_saved_endpoint() {
    let (home, state, app, token) = fixture();
    let saved = json!({"schema":1,"revision":1,"request_id":uuid::Uuid::new_v4(),"enabled":true,"address":"203.0.113.244","port":32124});
    std::fs::write(home.path().join("machine-network.json"), saved.to_string()).unwrap();
    machine_network::restore(&state).await;
    let observed = network(&app, &token).await;
    assert_eq!(observed["available"], false);
    assert_eq!(observed["enabled"], true);
    assert!(observed["error"].is_string());
    assert_eq!(observed["endpoint"], "https://203.0.113.244:32124");
    assert_eq!(
        call(&app, &token, "GET", "/api/v1/hosts", Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    let current: Value =
        serde_json::from_slice(&std::fs::read(home.path().join("machine-network.json")).unwrap())
            .unwrap();
    assert_eq!(current, saved);
}

#[tokio::test]
async fn occupied_saved_port_stays_unavailable_until_a_restart_can_reuse_it() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (home, state, app, token) = fixture();
    let discovery = network(&app, &token).await;
    let address: std::net::IpAddr = discovery["addresses"][0]["address"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let reserved = std::net::TcpListener::bind(std::net::SocketAddr::new(address, 0)).unwrap();
    let port = reserved.local_addr().unwrap().port();
    let saved = json!({"schema":1,"revision":1,"request_id":uuid::Uuid::new_v4(),"enabled":true,"address":address,"port":port});
    std::fs::write(home.path().join("machine-network.json"), saved.to_string()).unwrap();
    machine_network::restore(&state).await;
    let unavailable = network(&app, &token).await;
    assert_eq!(unavailable["available"], false);
    assert_eq!(unavailable["enabled"], true);
    assert!(
        unavailable["error"]
            .as_str()
            .unwrap()
            .contains("unavailable")
    );
    assert_eq!(
        call(&app, &token, "GET", "/api/v1/hosts", Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    drop(reserved);
    let restarted = AppState::open(home.path().into()).unwrap();
    machine_network::restore(&restarted).await;
    let ready = network(&server::router(restarted.clone()), &token).await;
    assert_eq!(ready["available"], true);
    assert_eq!(ready["endpoint"], unavailable["endpoint"]);
    assert_eq!(
        std::fs::read_to_string(home.path().join("machine-network.json")).unwrap(),
        saved.to_string()
    );
    restarted.shutdown.send(true).unwrap();
}
