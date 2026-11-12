//! Pairing is a narrow, durable computer-presence grant, independent of agents.
use agent_observatory::{auth, config::Config, server, state::AppState};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tempfile::TempDir;
use tower::ServiceExt;

fn fixture() -> (TempDir, Arc<AppState>, String) {
    let dir = TempDir::new().unwrap();
    Config {
        id: uuid::Uuid::new_v4().to_string(),
        owner: "fixture".into(),
        bind: "127.0.0.1".into(),
        port: 0,
        public_url: "https://observatory.fixture.invalid".into(),
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
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if !token.is_empty() {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(
            req.body(if body.is_null() {
                Body::empty()
            } else {
                Body::from(body.to_string())
            })
            .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| panic!("{status}: {}", String::from_utf8_lossy(&bytes))),
    )
}

async fn scoped(app: &Router, admin: &str, scope: &str) -> String {
    let (status, reply) = request(
        app,
        admin,
        "POST",
        "/api/v1/tokens",
        json!({"scopes":[scope]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    reply["token"].as_str().unwrap().to_owned()
}

async fn create(app: &Router, token: &str) -> Value {
    let (status, reply) = request(
        app,
        token,
        "POST",
        "/api/v1/machine-pairings",
        json!({"request_id":uuid::Uuid::new_v4().to_string()}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    reply
}

fn claim(state: &AppState, created: &Value) -> Value {
    json!({"schema":1,"observatory_id":state.config.id,"code":created["pairing"]["code"],
        "request_id":uuid::Uuid::new_v4().to_string(),"machine_id":uuid::Uuid::new_v4().to_string(),
        "machine_credential":auth::random_token(),"name":"Spark","os":"linux","arch":"aarch64","helper_version":"0.1.0"})
}

fn path(created: &Value, action: &str) -> String {
    format!(
        "/api/v1/machine-pairings/{}/{}",
        created["pairing"]["id"].as_str().unwrap(),
        action
    )
}

async fn accept(app: &Router, created: &Value, claim: &Value) -> Value {
    let (status, receipt) = request(app, "", "POST", &path(created, "redeem"), claim.clone()).await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    receipt
}

fn heartbeat(claim: &Value) -> Value {
    let mut body = claim.clone();
    for field in ["observatory_id", "request_id", "machine_credential", "code"] {
        body.as_object_mut().unwrap().remove(field);
    }
    body
}

fn expire(state: &AppState, created: &Value) {
    state
        .db
        .lock()
        .unwrap()
        .execute(
            "UPDATE machine_pairings SET value=json_set(value,'$.expires_at',0) WHERE id=?1",
            [created["pairing"]["id"].as_str().unwrap()],
        )
        .unwrap();
}

#[tokio::test]
async fn pairing_requires_provision_and_pending_codes_are_actor_private() {
    let (_dir, state, admin) = fixture();
    let app = server::router(state);
    let observer = scoped(&app, &admin, "observe").await;
    let installer = scoped(&app, &admin, "provision").await;
    for (token, expected) in [
        ("", StatusCode::UNAUTHORIZED),
        (observer.as_str(), StatusCode::FORBIDDEN),
    ] {
        assert_eq!(
            request(
                &app,
                token,
                "POST",
                "/api/v1/machine-pairings",
                json!({"request_id":uuid::Uuid::new_v4().to_string()})
            )
            .await
            .0,
            expected
        );
    }
    let created = create(&app, &installer).await;
    assert_eq!(created["downloads"], json!([])); // No fixture artifact means no invented setup link.
    let other = scoped(&app, &admin, "provision").await;
    let get = format!(
        "/api/v1/machine-pairings/{}",
        created["pairing"]["id"].as_str().unwrap()
    );
    assert!(
        request(&app, &other, "GET", &get, Value::Null)
            .await
            .0
            .is_client_error()
    );
    let (_, list) = request(&app, &other, "GET", "/api/v1/machine-pairings", Value::Null).await;
    assert_eq!(list["pairings"], json!([]));
    assert!(
        request(&app, &other, "POST", &path(&created, "cancel"), json!({}))
            .await
            .0
            .is_client_error()
    );
}

#[tokio::test]
async fn create_retry_preserves_code_expiry_and_identity() {
    let (_dir, state, admin) = fixture();
    let app = server::router(state);
    let body = json!({"request_id":uuid::Uuid::new_v4().to_string()});
    let (status, first) = request(
        &app,
        &admin,
        "POST",
        "/api/v1/machine-pairings",
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, second) = request(&app, &admin, "POST", "/api/v1/machine-pairings", body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first, second);
}

#[tokio::test]
async fn old_expired_create_retry_never_mints_a_fresh_code_or_expands_its_deadline() {
    let (_dir, state, admin) = fixture();
    let app = server::router(state.clone());
    let created = create(&app, &admin).await;
    expire(&state, &created); // Older than one day as well as outside the ten-minute validity.
    let body = json!({"request_id":created["pairing"]["id"]});
    let (status, retry) = request(&app, &admin, "POST", "/api/v1/machine-pairings", body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(retry["pairing"]["id"], created["pairing"]["id"]);
    assert_eq!(retry["pairing"]["expires_at"], 0);
    assert_eq!(retry["pairing"]["state"], "expired");
    assert!(retry["pairing"]["code"].is_null());
    assert!(state.list("hosts").unwrap().is_empty());
}

#[tokio::test]
async fn accepted_lost_reply_survives_server_restart_and_code_expiry_without_duplicate_host() {
    let (dir, state, admin) = fixture();
    let app = server::router(state.clone());
    let created = create(&app, &admin).await;
    let claim = claim(&state, &created);
    let receipt = accept(&app, &created, &claim).await;
    expire(&state, &created);
    drop(app);
    drop(state);
    let reopened = AppState::open(dir.path().into()).unwrap();
    let app = server::router(reopened.clone());
    assert_eq!(accept(&app, &created, &claim).await, receipt);
    assert_eq!(reopened.list("hosts").unwrap().len(), 1);
    assert!(reopened.list("endpoints").unwrap().is_empty());
    assert!(reopened.list("jobs").unwrap().is_empty());
    assert_eq!(receipt["access"], json!(["observe"]));
    let raw: String = reopened
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT credential_hash FROM machine_grants WHERE host_id=?1",
            [receipt["host_id"].as_str().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    assert_ne!(raw, claim["machine_credential"].as_str().unwrap());
    assert_eq!(
        raw,
        auth::digest(claim["machine_credential"].as_str().unwrap())
    );
}

#[tokio::test]
async fn conflicting_replay_and_existing_machine_credential_replacement_are_rejected() {
    let (_dir, state, admin) = fixture();
    let app = server::router(state.clone());
    let created = create(&app, &admin).await;
    let original = claim(&state, &created);
    accept(&app, &created, &original).await;
    for field in ["machine_credential", "machine_id", "request_id"] {
        let mut changed = original.clone();
        changed[field] = if field == "machine_credential" {
            json!(auth::random_token())
        } else {
            json!(uuid::Uuid::new_v4().to_string())
        };
        assert_eq!(
            request(&app, "", "POST", &path(&created, "redeem"), changed)
                .await
                .0,
            StatusCode::CONFLICT,
            "{field}"
        );
    }
    let second = create(&app, &admin).await;
    let mut replacement = claim(&state, &second);
    replacement["machine_id"] = original["machine_id"].clone();
    assert_eq!(
        request(&app, "", "POST", &path(&second, "redeem"), replacement)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(state.list("hosts").unwrap().len(), 1);
}

#[tokio::test]
async fn wrong_observatory_and_malformed_identity_cannot_claim_or_create_hosts() {
    let (_dir, state, admin) = fixture();
    let app = server::router(state.clone());
    let created = create(&app, &admin).await;
    let original = claim(&state, &created);
    for (field, value) in [
        ("observatory_id", json!(uuid::Uuid::new_v4().to_string())),
        ("machine_id", json!("not-uuid")),
        ("machine_credential", json!("short")),
        ("os", json!("invented")),
        ("arch", json!("invented")),
    ] {
        let mut bad = original.clone();
        bad[field] = value;
        assert!(
            request(&app, "", "POST", &path(&created, "redeem"), bad)
                .await
                .0
                .is_client_error(),
            "{field}"
        );
        assert!(state.list("hosts").unwrap().is_empty());
    }
}

#[tokio::test]
async fn expired_cancelled_and_locked_codes_do_not_create_grants() {
    let (_dir, state, admin) = fixture();
    let app = server::router(state.clone());
    let expired = create(&app, &admin).await;
    let expired_claim = claim(&state, &expired);
    expire(&state, &expired);
    assert_eq!(
        request(&app, "", "POST", &path(&expired, "redeem"), expired_claim)
            .await
            .0,
        StatusCode::GONE
    );
    let cancelled = create(&app, &admin).await;
    let cancelled_claim = claim(&state, &cancelled);
    assert_eq!(
        request(&app, &admin, "POST", &path(&cancelled, "cancel"), json!({}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "",
            "POST",
            &path(&cancelled, "redeem"),
            cancelled_claim
        )
        .await
        .0,
        StatusCode::GONE
    );
    let locked = create(&app, &admin).await;
    let correct = claim(&state, &locked);
    for _ in 0..5 {
        let mut wrong = correct.clone();
        wrong["code"] = if correct["code"] == "BBBB-BBBB" {
            json!("CCCC-CCCC")
        } else {
            json!("BBBB-BBBB")
        };
        assert!(
            request(&app, "", "POST", &path(&locked, "redeem"), wrong)
                .await
                .0
                .is_client_error()
        );
    }
    assert_eq!(
        request(&app, "", "POST", &path(&locked, "redeem"), correct)
            .await
            .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert!(state.list("hosts").unwrap().is_empty());
}

#[tokio::test]
async fn helper_credential_only_controls_its_own_presence_and_not_admin_or_another_machine() {
    let (_dir, state, admin) = fixture();
    let app = server::router(state.clone());
    let first = create(&app, &admin).await;
    let first_claim = claim(&state, &first);
    let first_receipt = accept(&app, &first, &first_claim).await;
    let second = create(&app, &admin).await;
    let second_claim = claim(&state, &second);
    let second_receipt = accept(&app, &second, &second_claim).await;
    let token = first_claim["machine_credential"].as_str().unwrap();
    for endpoint in ["hosts", "endpoints", "machine-pairings", "credentials"] {
        assert!(
            request(
                &app,
                token,
                "GET",
                &format!("/api/v1/{endpoint}"),
                Value::Null
            )
            .await
            .0
            .is_client_error(),
            "{endpoint}"
        );
    }
    let other = format!(
        "/api/v1/machines/{}/heartbeat",
        second_receipt["host_id"].as_str().unwrap()
    );
    assert!(
        request(&app, token, "POST", &other, heartbeat(&second_claim))
            .await
            .0
            .is_client_error()
    );
    let own = format!(
        "/api/v1/machines/{}/heartbeat",
        first_receipt["host_id"].as_str().unwrap()
    );
    assert_eq!(
        request(&app, token, "POST", &own, heartbeat(&first_claim))
            .await
            .0,
        StatusCode::OK
    );
    let other_revoke = format!(
        "/api/v1/machines/{}/revoke",
        second_receipt["host_id"].as_str().unwrap()
    );
    assert!(
        request(&app, token, "POST", &other_revoke, json!({}))
            .await
            .0
            .is_client_error()
    );
}

#[tokio::test]
async fn heartbeat_is_fresh_observation_without_revision_churn_or_agent_discovery() {
    let (_dir, state, admin) = fixture();
    let app = server::router(state.clone());
    let created = create(&app, &admin).await;
    let claim = claim(&state, &created);
    let receipt = accept(&app, &created, &claim).await;
    let host_id = receipt["host_id"].as_str().unwrap();
    let before = state.read("hosts", host_id).unwrap();
    let mut body = heartbeat(&claim);
    body["name"] = json!("A changed reported hostname");
    let (status, _) = request(
        &app,
        claim["machine_credential"].as_str().unwrap(),
        "POST",
        &format!("/api/v1/machines/{host_id}/heartbeat"),
        body,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state.read("hosts", host_id).unwrap(), before);
    let (_, live) = request(&app, &admin, "GET", "/api/v1/hosts", Value::Null).await;
    let host = live["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["id"] == host_id)
        .unwrap();
    assert_eq!(host["machine"]["state"], "online");
    assert_eq!(
        host["machine"]["reported_name"],
        "A changed reported hostname"
    );
    assert_eq!(host["recipes"], json!([]));
    state.db.lock().unwrap().execute("UPDATE machine_observations SET value=json_set(value,'$.last_seen_at',0) WHERE host_id=?1", [host_id]).unwrap();
    let (_, stale) = request(&app, &admin, "GET", "/api/v1/hosts", Value::Null).await;
    assert_eq!(stale["hosts"][0]["machine"]["state"], "offline");
    assert!(state.list("endpoints").unwrap().is_empty());
    assert!(state.list("jobs").unwrap().is_empty());
}

#[tokio::test]
async fn revocation_survives_old_claim_retry_and_fresh_pairing_preserves_host_identity() {
    let (_dir, state, admin) = fixture();
    let app = server::router(state.clone());
    let created = create(&app, &admin).await;
    let original = claim(&state, &created);
    let receipt = accept(&app, &created, &original).await;
    let host_id = receipt["host_id"].as_str().unwrap();
    let (status, endpoint) = request(&app, &admin, "POST", "/api/v1/endpoints", json!({"label":"Independent agent","kind":"agent","host_id":host_id,"ui_url":"https://agent.fixture.invalid"})).await;
    assert_eq!(status, StatusCode::OK);
    let revoke = format!("/api/v1/machines/{host_id}/revoke");
    assert_eq!(
        request(&app, &admin, "POST", &revoke, json!({})).await.0,
        StatusCode::OK
    );
    let audit: String = state
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT data FROM audit WHERE kind='machine.revoked' AND resource_id=?1",
            [host_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!audit.contains(original["machine_credential"].as_str().unwrap()));
    assert!(!audit.contains(original["code"].as_str().unwrap()));
    assert_eq!(
        request(
            &app,
            "",
            "POST",
            &path(&created, "redeem"),
            original.clone()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &app,
            original["machine_credential"].as_str().unwrap(),
            "POST",
            &format!("/api/v1/machines/{host_id}/heartbeat"),
            heartbeat(&original)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        state
            .read("endpoints", endpoint["id"].as_str().unwrap())
            .unwrap(),
        endpoint
    );
    let fresh = create(&app, &admin).await;
    let mut fresh_claim = claim(&state, &fresh);
    fresh_claim["machine_id"] = original["machine_id"].clone();
    // The helper keeps its credential across explicit re-pair; generation, not
    // credential rotation, must make the old accepted request ineligible.
    fresh_claim["machine_credential"] = original["machine_credential"].clone();
    assert_eq!(accept(&app, &fresh, &fresh_claim).await["host_id"], host_id);
    assert_eq!(state.list("hosts").unwrap().len(), 1);
    assert!(
        request(&app, "", "POST", &path(&created, "redeem"), original)
            .await
            .0
            .is_client_error()
    );
}

#[tokio::test]
async fn paired_hosts_cannot_be_converted_into_remote_commands_or_deleted_while_active() {
    let (_dir, state, admin) = fixture();
    let app = server::router(state.clone());
    let created = create(&app, &admin).await;
    let receipt = accept(&app, &created, &claim(&state, &created)).await;
    let host_id = receipt["host_id"].as_str().unwrap();
    let saved = state.read("hosts", host_id).unwrap();
    let endpoint = format!("/api/v1/hosts/{host_id}");
    assert!(
        request(
            &app,
            &admin,
            "DELETE",
            &endpoint,
            json!({"expected_revision":saved["revision"]})
        )
        .await
        .0
        .is_client_error()
    );
    let body = json!({"expected_revision":saved["revision"],"label":"Same machine","transport":"ssh","address":"untrusted-host","install_roots":["/tmp"],"recipes":[{"id":"new","version":"1","argv":["false"]}]});
    assert!(
        request(&app, &admin, "PUT", &endpoint, body)
            .await
            .0
            .is_client_error()
    );
    assert_eq!(state.read("hosts", host_id).unwrap(), saved);
    let (status, _) = request(&app, &admin, "POST", "/api/v1/install/plan", json!({"host_id":host_id,"recipe_id":"missing","name":"Never installed","home":"/tmp/never-installed","port":0})).await;
    assert!(status.is_client_error());
    assert!(state.list("jobs").unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_claimants_create_exactly_one_host_and_only_the_winner_can_heartbeat() {
    let (_dir, state, admin) = fixture();
    let app = server::router(state.clone());
    let created = create(&app, &admin).await;
    let one = claim(&state, &created);
    let two = claim(&state, &created);
    let route = path(&created, "redeem");
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let jobs = [one.clone(), two.clone()].map(|body| {
        let app = app.clone();
        let route = route.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            request(&app, "", "POST", &route, body).await
        })
    });
    let [first, second] = jobs;
    let (first, second) = (first.await.unwrap(), second.await.unwrap());
    let (winner, loser, receipt) = if first.0 == StatusCode::OK {
        assert_eq!(second.0, StatusCode::CONFLICT);
        (&one, &two, first.1)
    } else {
        assert_eq!(first.0, StatusCode::CONFLICT);
        assert_eq!(second.0, StatusCode::OK);
        (&two, &one, second.1)
    };
    assert_eq!(state.list("hosts").unwrap().len(), 1);
    let route = format!(
        "/api/v1/machines/{}/heartbeat",
        receipt["host_id"].as_str().unwrap()
    );
    assert_eq!(
        request(
            &app,
            winner["machine_credential"].as_str().unwrap(),
            "POST",
            &route,
            heartbeat(winner)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(
        request(
            &app,
            loser["machine_credential"].as_str().unwrap(),
            "POST",
            &route,
            heartbeat(loser)
        )
        .await
        .0
        .is_client_error()
    );
}

#[tokio::test]
async fn browser_pairing_mutation_requires_same_origin_and_session_csrf() {
    let (_dir, state, _admin) = fixture();
    let app = server::router(state.clone());
    let browser = auth::random_token();
    let csrf = auth::random_token();
    state
        .db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO browser_sessions(hash,owner,csrf,expires_at) VALUES(?1,'fixture',?2,?3)",
            rusqlite::params![
                auth::digest(&browser),
                csrf,
                agent_observatory::state::now_ms() + 60_000
            ],
        )
        .unwrap();
    for (origin, supplied_csrf, expected) in [
        (
            "https://foreign.invalid",
            Some(csrf.as_str()),
            StatusCode::FORBIDDEN,
        ),
        (
            "https://observatory.fixture.invalid",
            None,
            StatusCode::FORBIDDEN,
        ),
        (
            "https://observatory.fixture.invalid",
            Some(csrf.as_str()),
            StatusCode::OK,
        ),
    ] {
        let mut req = Request::builder()
            .method("POST")
            .uri("/api/v1/machine-pairings")
            .header("content-type", "application/json")
            .header("host", "observatory.fixture.invalid")
            .header("origin", origin)
            .header("cookie", format!("{}={browser}", auth::cookie_name(&state)));
        if let Some(value) = supplied_csrf {
            req = req.header("x-csrf-token", value);
        }
        let reply = app
            .clone()
            .oneshot(
                req.body(Body::from(
                    json!({"request_id":uuid::Uuid::new_v4().to_string()}).to_string(),
                ))
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(reply.status(), expected);
    }
}

#[tokio::test]
async fn setup_download_binds_public_observatory_metadata_without_code_or_credentials() {
    use sha2::{Digest, Sha256};
    use std::io::{Cursor, Read, Write};
    let (_dir, state, admin) = fixture();
    let artifacts = state.home.join("machine-helper-artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    archive
        .start_file(
            "Constellation Computer",
            // This is a Mac-produced release archive even when the server
            // preparing its pairing download is running on Windows.
            zip::write::SimpleFileOptions::default()
                .system(zip::System::Unix)
                .unix_permissions(0o755),
        )
        .unwrap();
    archive
        .write_all(b"inert fixture executable; never launched")
        .unwrap();
    let data = archive.finish().unwrap().into_inner();
    std::fs::write(artifacts.join("linux.zip"), &data).unwrap();
    let platforms = [
        "windows-x86_64",
        "windows-aarch64",
        "macos-x86_64",
        "macos-aarch64",
        "linux-x86_64",
        "linux-aarch64",
    ];
    let manifest = json!({"schema":1,"artifacts":platforms.map(|platform|json!({"platform":platform,"label":"Fixture","architecture":"fixture","filename":"linux.zip","sha256":format!("{:x}",Sha256::digest(&data)),"version":"0.1.0"}))});
    std::fs::write(artifacts.join("manifest.json"), manifest.to_string()).unwrap();
    let app = server::router(state.clone());
    let created = create(&app, &admin).await;
    assert_eq!(created["downloads"].as_array().unwrap().len(), 6);
    let url = created["downloads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["platform"] == "windows-aarch64")
        .unwrap()["url"]
        .as_str()
        .unwrap();
    assert_eq!(
        request(&app, "", "GET", url, Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    let reply = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(url)
                .header("authorization", format!("Bearer {admin}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::OK);
    assert_eq!(reply.headers()["content-type"], "application/zip");
    let bytes = to_bytes(reply.into_body(), 1024 * 1024).await.unwrap();
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut raw = String::new();
    zip.by_name("observatory-pairing.json")
        .unwrap()
        .read_to_string(&mut raw)
        .unwrap();
    let bootstrap: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(bootstrap["observatory_id"], state.config.id);
    assert_eq!(bootstrap["observatory_url"], state.config.public_url);
    assert_eq!(bootstrap["pairing_id"], created["pairing"]["id"]);
    assert_eq!(bootstrap["expires_at"], created["pairing"]["expires_at"]);
    assert!(!raw.contains(created["pairing"]["code"].as_str().unwrap()));
    assert!(!raw.contains(admin.trim()));
    assert!(bootstrap.get("machine_credential").is_none());
    // Post-advertisement tampering cannot deliver unverified executable bytes.
    std::fs::write(artifacts.join("linux.zip"), b"corrupted package").unwrap();
    assert!(
        request(&app, &admin, "GET", url, Value::Null)
            .await
            .0
            .is_client_error()
    );
}

#[tokio::test]
async fn mac_setup_carries_public_bootstrap_inside_app_for_translocation_safe_lookup() {
    use sha2::{Digest, Sha256};
    use std::io::{Cursor, Read, Write};
    let (_dir, state, admin) = fixture();
    let artifacts = state.home.join("machine-helper-artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let binary_path = "Constellation Computer.app/Contents/MacOS/constellation-machine-helper";
    let executable = b"inert macOS fixture; never launched";
    archive
        .start_file(
            binary_path,
            zip::write::SimpleFileOptions::default()
                .system(zip::System::Unix)
                .unix_permissions(0o755),
        )
        .unwrap();
    archive.write_all(executable).unwrap();
    archive
        .start_file(
            "Constellation Computer.app/Contents/Info.plist",
            zip::write::SimpleFileOptions::default()
                .system(zip::System::Unix)
                .unix_permissions(0o644),
        )
        .unwrap();
    archive.write_all(b"<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>CFBundleExecutable</key><string>constellation-machine-helper</string></dict></plist>").unwrap();
    let original = archive.finish().unwrap().into_inner();
    std::fs::write(artifacts.join("mac.zip"), &original).unwrap();
    let manifest = json!({"schema":1,"artifacts":[{"platform":"macos-aarch64","label":"Mac","architecture":"Apple silicon","filename":"mac.zip","sha256":format!("{:x}",Sha256::digest(&original)),"version":"0.1.0"}]});
    std::fs::write(artifacts.join("manifest.json"), manifest.to_string()).unwrap();
    let app = server::router(state.clone());
    let created = create(&app, &admin).await;
    let url = created["downloads"][0]["url"].as_str().unwrap();
    let reply = app
        .oneshot(
            Request::builder()
                .uri(url)
                .header("authorization", format!("Bearer {admin}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::OK);
    let bytes = to_bytes(reply.into_body(), 1024 * 1024).await.unwrap();
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut raw = String::new();
    zip.by_name("Constellation Computer.app/Contents/Resources/observatory-pairing.json")
        .unwrap()
        .read_to_string(&mut raw)
        .unwrap();
    let bootstrap: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(bootstrap["observatory_id"], state.config.id);
    assert_eq!(bootstrap["observatory_url"], state.config.public_url);
    assert_eq!(bootstrap["pairing_id"], created["pairing"]["id"]);
    assert!(!raw.contains(created["pairing"]["code"].as_str().unwrap()));
    assert!(!raw.contains(admin.trim()));
    assert!(bootstrap.get("machine_credential").is_none());
    let mut retained = Vec::new();
    let mut binary = zip.by_name(binary_path).unwrap();
    assert_eq!(binary.unix_mode().unwrap() & 0o777, 0o755);
    binary.read_to_end(&mut retained).unwrap();
    assert_eq!(retained, executable);
    assert_eq!(std::fs::read(artifacts.join("mac.zip")).unwrap(), original);
}
