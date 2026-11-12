use agent_observatory::{auth, config::Config, places, server, state::AppState};
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
        owner: "test".into(),
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
    let endpoint = uuid::Uuid::new_v4().to_string();
    state
        .save("endpoints", json!({"id":endpoint,"label":"Maker"}), Some(0))
        .unwrap();
    (dir, state, endpoint)
}

fn place(endpoint: &str) -> Value {
    json!({
        "label":"Studio","service":"slack","kind":"workspace","external_id":"T1234",
        "rooms":[{"id":"C1234","label":"#design"}],
        "bindings":[{"endpoint_id":endpoint,"connection_id":"slack-design","room_id":"C1234"}]
    })
}

#[test]
fn one_venue_groups_multiple_agents_without_performing_operations() {
    let (_dir, state, endpoint) = state();
    let other = uuid::Uuid::new_v4().to_string();
    state
        .save("endpoints", json!({"id":other,"label":"Coach"}), Some(0))
        .unwrap();
    let before = state.list("endpoints").unwrap();
    let mut input = place(&endpoint);
    input["bindings"].as_array_mut().unwrap().push(json!({
        "endpoint_id":other,"connection_id":"slack-design","room_id":"C1234"
    }));
    input["expected_revision"] = json!(4);
    let mut body = places::body(&state, &input).unwrap();
    assert!(body.get("expected_revision").is_none());
    let lock = state.places_registry_lock.lock().unwrap();
    places::validate_bindings(&state, &body, None).unwrap();
    body["id"] = json!(uuid::Uuid::new_v4());
    let saved = state.save("places", body, Some(0)).unwrap();
    drop(lock);
    assert_eq!(saved["bindings"].as_array().unwrap().len(), 2);
    assert_eq!(state.list("endpoints").unwrap(), before);
    assert!(state.list("jobs").unwrap().is_empty());
    assert!(state.list("sessions").unwrap().is_empty());
    assert!(state.list("observations").unwrap().is_empty());
    assert_eq!(state.list("places").unwrap().len(), 1);
}

#[test]
fn empty_venue_records_no_presence_or_authority() {
    let (_dir, state, _) = state();
    let input =
        json!({"label":"Workshop","service":"minecraft","kind":"server","rooms":[],"bindings":[]});
    assert_eq!(places::body(&state, &input).unwrap(), input);
    places::validate_bindings(&state, &input, None).unwrap();
    assert!(state.list("places").unwrap().is_empty());
    assert!(input.get("connected").is_none());
    assert!(input.get("desired").is_none());
}

#[test]
fn place_body_rejects_unknown_authority_fields_and_invalid_source_identity() {
    let (_dir, state, endpoint) = state();
    let valid = place(&endpoint);
    let mut bad = vec![];
    for field in [
        "id",
        "revision",
        "url",
        "credential",
        "argv",
        "status",
        "desired",
    ] {
        let mut v = valid.clone();
        v[field] = json!("not allowed");
        bad.push(v);
    }
    for (field, value) in [
        ("service", json!("Slack")),
        ("service", json!("slack--team")),
        ("service", json!("https://slack.test")),
        ("kind", json!("channel")),
        ("label", json!("   ")),
        ("label", json!("control\nline")),
        ("external_id", json!("https://slack.test/workspace")),
    ] {
        let mut v = valid.clone();
        v[field] = value;
        bad.push(v);
    }
    let mut bad_room = valid.clone();
    bad_room["rooms"][0]["credential"] = json!("secret");
    bad.push(bad_room);
    let mut bad_binding = valid.clone();
    bad_binding["bindings"][0]["command"] = json!("do not execute");
    bad.push(bad_binding);
    for v in bad {
        assert!(places::body(&state, &v).is_err(), "accepted {v}");
    }
    assert!(state.list("places").unwrap().is_empty());
}

#[test]
fn bindings_need_existing_endpoints_and_declared_rooms() {
    let (_dir, state, endpoint) = state();
    let valid = place(&endpoint);
    let mut invalid = valid.clone();
    invalid["bindings"][0]["endpoint_id"] = json!(uuid::Uuid::new_v4());
    assert!(places::body(&state, &invalid).is_err());
    invalid = valid.clone();
    invalid["bindings"][0]["room_id"] = json!("C-not-declared");
    assert!(places::body(&state, &invalid).is_err());
    invalid = valid.clone();
    invalid["bindings"][0]["connection_id"] = json!("https://remote.test/command");
    assert!(places::body(&state, &invalid).is_err());
    invalid = valid.clone();
    invalid["bindings"]
        .as_array_mut()
        .unwrap()
        .push(valid["bindings"][0].clone());
    assert!(places::body(&state, &invalid).is_err());
    invalid = valid.clone();
    invalid["rooms"]
        .as_array_mut()
        .unwrap()
        .push(valid["rooms"][0].clone());
    assert!(places::body(&state, &invalid).is_err());
    let mut without_room = valid;
    without_room["bindings"][0]["room_id"] = Value::Null;
    assert!(places::body(&state, &without_room).is_ok());
}

#[test]
fn bindings_are_unique_across_places_but_can_be_kept_when_editing() {
    let (_dir, state, endpoint) = state();
    let candidate = places::body(&state, &place(&endpoint)).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let mut saved = candidate.clone();
    saved["id"] = json!(id);
    let lock = state.places_registry_lock.lock().unwrap();
    places::validate_bindings(&state, &candidate, None).unwrap();
    state.save("places", saved, Some(0)).unwrap();
    let duplicate = places::validate_bindings(&state, &candidate, None).unwrap_err();
    assert_eq!(duplicate.0, axum::http::StatusCode::CONFLICT);
    places::validate_bindings(&state, &candidate, Some(&id)).unwrap();
    let mut other = candidate;
    other["bindings"][0]["connection_id"] = json!("slack-another-channel");
    places::validate_bindings(&state, &other, None).unwrap();
    drop(lock);
}

#[test]
fn room_and_binding_limits_bound_display_data() {
    let (_dir, state, endpoint) = state();
    let mut candidate = place(&endpoint);
    candidate["rooms"] = json!(
        (0..500)
            .map(|n| json!({"id":format!("C{n}"),"label":format!("Room {n}")}))
            .collect::<Vec<_>>()
    );
    candidate["bindings"] = json!((0..1000).map(|n| json!({"endpoint_id":endpoint,"connection_id":format!("connection-{n}"),"room_id":"C0"})).collect::<Vec<_>>());
    assert!(places::body(&state, &candidate).is_ok());
    let mut overflow = candidate.clone();
    overflow["rooms"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"overflow","label":"Extra"}));
    assert!(places::body(&state, &overflow).is_err());
    candidate["bindings"]
        .as_array_mut()
        .unwrap()
        .push(json!({"endpoint_id":endpoint,"connection_id":"overflow"}));
    assert!(places::body(&state, &candidate).is_err());
}

async fn request(
    app: &Router,
    token: &str,
    method: &str,
    path: &str,
    value: Value,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {token}"))
                .body(if value.is_null() {
                    Body::empty()
                } else {
                    Body::from(value.to_string())
                })
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn place_routes_enforce_scopes_revisions_and_binding_ownership() {
    let (dir, state, endpoint) = state();
    let admin = std::fs::read_to_string(dir.path().join("secrets/control-token")).unwrap();
    let observer = auth::random_token();
    state
        .db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO tokens VALUES(?1,'observer','[\"observe\"]',?2)",
            rusqlite::params![
                auth::digest(&observer),
                agent_observatory::state::now_ms() + 60_000
            ],
        )
        .unwrap();
    let app = server::router(state.clone());
    let input = place(&endpoint);
    assert_eq!(
        request(&app, "invalid", "GET", "/api/v1/places", Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(&app, &observer, "POST", "/api/v1/places", input.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, saved) = request(&app, &admin, "POST", "/api/v1/places", input.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let (status, list) = request(&app, &observer, "GET", "/api/v1/places", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["places"].as_array().unwrap().len(), 1);
    assert_eq!(
        request(&app, &admin, "POST", "/api/v1/places", input.clone())
            .await
            .0,
        StatusCode::CONFLICT
    );
    let path = format!("/api/v1/places/{}", saved["id"].as_str().unwrap());
    let mut edit = input;
    edit["label"] = json!("Studio renamed");
    edit["expected_revision"] = json!(0);
    assert_eq!(
        request(&app, &admin, "PUT", &path, edit.clone()).await.0,
        StatusCode::CONFLICT
    );
    edit["expected_revision"] = saved["revision"].clone();
    let (status, edited) = request(&app, &admin, "PUT", &path, edit).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(edited["label"], "Studio renamed");
    let remove = json!({"expected_revision":edited["revision"]});
    assert_eq!(
        request(&app, &observer, "DELETE", &path, remove.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, &admin, "DELETE", &path, remove).await.0,
        StatusCode::OK
    );
    assert!(state.list("places").unwrap().is_empty());
    assert_eq!(state.list("endpoints").unwrap().len(), 1);
    assert!(state.list("jobs").unwrap().is_empty());
    assert!(state.list("sessions").unwrap().is_empty());
}
