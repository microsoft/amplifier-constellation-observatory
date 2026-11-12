//! Native local setup uses installation credentials, never OS impersonation.
use agent_observatory::{
    auth,
    config::{AuthMode, Config, WebTransport},
    server,
    state::AppState,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tempfile::TempDir;
use tower::ServiceExt;

const PASSWORD: &str = "one installation password";
const LOCAL_ORIGIN: &str = "http://127.0.0.1:49152";
fn configuration(local: bool) -> Config {
    Config {
        id: uuid::Uuid::new_v4().to_string(),
        owner: "owner".into(),
        bind: "127.0.0.1".into(),
        port: 49152,
        public_url: if local {
            LOCAL_ORIGIN
        } else {
            "https://observatory.invalid"
        }
        .into(),
        auth_mode: AuthMode::LocalPassword,
        web_transport: if local {
            WebTransport::LocalHttp
        } else {
            WebTransport::Https
        },
        pam_service: String::new(),
        tls_cert: PathBuf::new(),
        tls_key: PathBuf::new(),
        tls_ca: None,
        embed_origins: vec![],
    }
}
fn fixture(local: bool) -> (TempDir, Arc<AppState>, Router) {
    let home = tempfile::tempdir().unwrap();
    auth::initialize_local_password(home.path(), PASSWORD).unwrap();
    configuration(local).save(home.path()).unwrap();
    let state = AppState::open(home.path().into()).unwrap();
    let app = server::router(state.clone());
    (home, state, app)
}
async fn request(
    app: &Router,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Value,
) -> (StatusCode, HeaderMap, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = app
        .clone()
        .oneshot(
            request
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
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), 65536).await.unwrap();
    (status, headers, serde_json::from_slice(&body).unwrap())
}
async fn login(app: &Router, password: &str, local: bool) -> (StatusCode, HeaderMap, Value) {
    let host = if local {
        "127.0.0.1:49152"
    } else {
        "observatory.invalid"
    };
    let origin = if local {
        LOCAL_ORIGIN
    } else {
        "https://observatory.invalid"
    };
    request(
        app,
        "POST",
        "/auth/login",
        &[("host", host), ("origin", origin)],
        json!({"username":"owner","password":password}),
    )
    .await
}

#[tokio::test]
async fn local_installation_proof_is_not_available_on_https_deployments() {
    let (_home, _state, app) = fixture(false);
    let (status, _, _) = request(
        &app,
        "GET",
        &format!("/api/v1/local-proof?nonce={}", auth::random_token()),
        &[("host", "observatory.invalid")],
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[test]
fn old_configuration_defaults_to_pam_and_https_and_local_mode_cannot_expose_lan() {
    let mut old = serde_json::to_value(configuration(false)).unwrap();
    old.as_object_mut().unwrap().remove("auth_mode");
    old.as_object_mut().unwrap().remove("web_transport");
    old["pam_service"] = json!("original-service");
    let parsed: Config = serde_json::from_value(old).unwrap();
    assert_eq!(parsed.auth_mode, AuthMode::Pam);
    assert_eq!(parsed.web_transport, WebTransport::Https);
    assert_eq!(parsed.pam_service, "original-service");
    assert!(parsed.validate().is_ok());
    let good = configuration(true);
    assert!(good.validate().is_ok());
    for (field, value) in [
        ("bind", json!("0.0.0.0")),
        ("bind", json!("192.168.1.2")),
        ("public_url", json!("http://remote.invalid:49152")),
        ("public_url", json!("http://127.0.0.1:49153")),
        ("public_url", json!("http://127.0.0.1:49152/other")),
        ("auth_mode", json!("pam")),
        ("embed_origins", json!(["https://external.invalid"])),
    ] {
        let mut invalid = serde_json::to_value(&good).unwrap();
        invalid[field] = value;
        assert!(
            serde_json::from_value::<Config>(invalid)
                .unwrap()
                .validate()
                .is_err(),
            "{field}"
        );
    }
}

#[test]
fn local_password_is_salted_private_and_create_only() {
    let home = tempfile::tempdir().unwrap();
    assert!(auth::initialize_local_password(home.path(), "short").is_err());
    auth::initialize_local_password(home.path(), PASSWORD).unwrap();
    let path = home.path().join("secrets/local-password.json");
    let first = std::fs::read(&path).unwrap();
    let saved: Value = serde_json::from_slice(&first).unwrap();
    assert!(
        saved["password_hash"]
            .as_str()
            .unwrap()
            .starts_with("$argon2id$v=19$m=19456,t=2,p=1$")
    );
    assert!(!String::from_utf8_lossy(&first).contains(PASSWORD));
    assert!(auth::initialize_local_password(home.path(), "replacement password").is_err());
    assert_eq!(first, std::fs::read(&path).unwrap());
    let other = tempfile::tempdir().unwrap();
    auth::initialize_local_password(other.path(), PASSWORD).unwrap();
    assert_ne!(
        first,
        std::fs::read(other.path().join("secrets/local-password.json")).unwrap()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn native_login_preserves_cookie_csrf_scopes_and_logout() {
    let (_home, state, app) = fixture(true);
    assert_eq!(
        login(&app, "wrong password", true).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, headers, body) = login(&app, PASSWORD, true).await;
    assert_eq!(status, StatusCode::OK);
    let set_cookie = headers["set-cookie"].to_str().unwrap();
    assert!(set_cookie.contains("HttpOnly; SameSite=Strict"));
    assert!(!set_cookie.contains("Secure"));
    let cookie = set_cookie.split(';').next().unwrap();
    let csrf = body["csrf"].as_str().unwrap();
    let base = [
        ("host", "127.0.0.1:49152"),
        ("origin", LOCAL_ORIGIN),
        ("cookie", cookie),
    ];
    assert_eq!(
        request(&app, "GET", "/api/v1/endpoints", &base, Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "POST",
            "/api/v1/tokens",
            &base,
            json!({"name":"forbidden without csrf","scopes":["admin"]})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let mut full = base.to_vec();
    full.push(("x-csrf-token", csrf));
    let (_, _, descriptor) = request(&app, "GET", "/api/v1/descriptor", &full, Value::Null).await;
    assert_eq!(descriptor["capabilities"]["machine_pairing"], false);
    assert_eq!(
        request(
            &app,
            "POST",
            "/api/v1/machine-pairings",
            &full,
            json!({"request_id":uuid::Uuid::new_v4()})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (_, _, created) = request(
        &app,
        "POST",
        "/api/v1/tokens",
        &full,
        json!({"name":"read only","scopes":["observe"]}),
    )
    .await;
    let bearer = format!("Bearer {}", created["token"].as_str().unwrap());
    assert_eq!(
        request(
            &app,
            "POST",
            "/api/v1/tokens",
            &[("host", "127.0.0.1:49152"), ("authorization", &bearer)],
            json!({"name":"not allowed","scopes":["admin"]})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, "POST", "/auth/logout", &full, json!({}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "GET", "/api/v1/endpoints", &base, Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let audit = state
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT data FROM audit WHERE kind='auth.signed_in'",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap();
    assert!(audit.contains("local-password"));
    assert!(!audit.contains(PASSWORD));
}

#[tokio::test]
async fn loopback_origin_and_host_are_pinned_before_login_or_api_access() {
    let (_home, _state, app) = fixture(true);
    for headers in [
        vec![],
        vec![("host", "attacker.invalid:49152")],
        vec![("host", "127.0.0.1:49153")],
        vec![
            ("host", "127.0.0.1:49152"),
            ("origin", "https://attacker.invalid"),
        ],
        vec![
            ("host", "127.0.0.1:49152"),
            ("sec-fetch-site", "cross-site"),
        ],
        vec![("host", "127.0.0.1:49152"), ("sec-fetch-site", "same-site")],
    ] {
        assert_eq!(
            request(&app, "GET", "/auth/session", &headers, Value::Null)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(
                &app,
                "POST",
                "/auth/login",
                &headers,
                json!({"username":"owner","password":PASSWORD})
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        request(
            &app,
            "GET",
            "/auth/session",
            &[("host", "127.0.0.1:49152"), ("sec-fetch-site", "none")],
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn failed_local_logins_are_rate_limited_and_corrupt_hash_never_falls_back() {
    let (home, _state, app) = fixture(true);
    let hash = home.path().join("secrets/local-password.json");
    let mut value: Value = serde_json::from_slice(&std::fs::read(&hash).unwrap()).unwrap();
    value["password_hash"] = json!(
        value["password_hash"]
            .as_str()
            .unwrap()
            .replace("m=19456", "m=4294967295")
    );
    std::fs::write(&hash, value.to_string()).unwrap();
    for _ in 0..5 {
        assert_eq!(
            login(&app, PASSWORD, true).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        login(&app, PASSWORD, true).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn https_local_auth_retains_secure_cookie_and_old_origin_rules() {
    let (_home, _state, app) = fixture(false);
    let (status, headers, _) = login(&app, PASSWORD, false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Secure; HttpOnly; SameSite=Strict")
    );
    assert_eq!(
        request(
            &app,
            "POST",
            "/auth/login",
            &[
                ("host", "observatory.invalid"),
                ("origin", "https://foreign.invalid")
            ],
            json!({"username":"owner","password":PASSWORD})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn update_offer_can_navigate_to_public_root_but_not_call_local_api() {
    let (_home, _state, app) = fixture(true);
    for (path, mode, destination, site, expected) in [
        ("/", "navigate", "document", "same-site", StatusCode::OK),
        (
            "/",
            "navigate",
            "document",
            "cross-site",
            StatusCode::FORBIDDEN,
        ),
        ("/", "cors", "empty", "same-site", StatusCode::FORBIDDEN),
        (
            "/api/v1/status",
            "navigate",
            "document",
            "same-site",
            StatusCode::FORBIDDEN,
        ),
        (
            "/auth/status",
            "navigate",
            "document",
            "same-site",
            StatusCode::FORBIDDEN,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("host", "127.0.0.1:49152")
                    .header("sec-fetch-site", site)
                    .header("sec-fetch-mode", mode)
                    .header("sec-fetch-dest", destination)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{path}/{mode}/{site}");
    }
    let response = app
        .oneshot(
            Request::builder()
                .uri("/")
                .header("host", "127.0.0.1:49152")
                .header("origin", "http://127.0.0.1:12345")
                .header("sec-fetch-site", "same-site")
                .header("sec-fetch-mode", "navigate")
                .header("sec-fetch-dest", "document")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
