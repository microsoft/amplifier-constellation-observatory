use agent_observatory::{config::Config, state::AppState, views};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tempfile::TempDir;

fn state() -> (TempDir, Arc<AppState>) {
    let dir = TempDir::new().unwrap();
    Config {
        id: uuid::Uuid::new_v4().to_string(),
        owner: "test".into(),
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
    let s = AppState::open(dir.path().into()).unwrap();
    (dir, s)
}

fn set_world(revision: u64, world: Value) -> Value {
    json!({"expected_revision":revision,"commands":[
        {"op":"set","field":"screen","value":"world"},
        {"op":"set","field":"world","value":world}
    ]})
}

#[test]
fn world_layout_survives_restart_without_changing_installations() {
    let (dir, s) = state();
    let agent = uuid::Uuid::new_v4().to_string();
    let host = uuid::Uuid::new_v4().to_string();
    let endpoint = s
        .save("endpoints", json!({"id":agent,"label":"Maker"}), Some(0))
        .unwrap();
    let machine = s
        .save("hosts", json!({"id":host,"label":"Computer"}), Some(0))
        .unwrap();
    let v = views::create(&s).unwrap();
    let id = v["id"].as_str().unwrap().to_owned();
    assert_eq!(v["world"]["camera"]["scale"], 1);
    let world = json!({
        "camera":{"x":-45.5,"y":120,"scale":0.75},
        "positions":{
            format!("machine:{host}"):{"x":20,"y":60},
            format!("agent:{agent}"):{"x":30,"y":80},
            "place:instance-1.slack-studio":{"x":400,"y":10}
        },
        "machine_id":host,"place_id":"instance-1.slack-studio",
        "room_id":"C1234","navigation":"room"
    });
    let mut command = set_world(1, world.clone());
    command["commands"]
        .as_array_mut()
        .unwrap()
        .push(json!({"op":"set","field":"selected_id","value":agent}));
    views::commands(&s, &id, &command).unwrap();
    assert_eq!(s.read("endpoints", &agent).unwrap(), endpoint);
    assert_eq!(s.read("hosts", &host).unwrap(), machine);
    assert!(s.list("jobs").unwrap().is_empty());
    assert!(s.list("sessions").unwrap().is_empty());
    let activity: i64 =
        s.db.lock()
            .unwrap()
            .query_row("SELECT count(*) FROM audit", [], |r| r.get(0))
            .unwrap();
    assert_eq!(activity, 0);
    drop(s);
    let reopened = AppState::open(dir.path().into()).unwrap();
    let actual = views::read(&reopened, &id).unwrap();
    assert_eq!(actual["screen"], "world");
    assert_eq!(actual["selected_id"], agent);
    assert_eq!(actual["world"], world);
}

#[test]
fn invalid_world_update_cannot_overwrite_current_layout_or_navigation() {
    let (_dir, s) = state();
    let v = views::create(&s).unwrap();
    let id = v["id"].as_str().unwrap();
    let current = views::commands(
        &s,
        id,
        &set_world(
            1,
            json!({
                "camera":{"x":0,"y":0,"scale":1.25},"navigation":"world"
            }),
        ),
    )
    .unwrap();
    let bad = [
        json!({"camera":{"x":0,"y":0,"scale":0.049}}),
        json!({"camera":{"x":0,"y":0,"scale":2.51}}),
        json!({"camera":{"x":1e300,"y":0,"scale":1}}),
        json!({"camera":{"x":"NaN","y":0,"scale":1}}),
        json!({"camera":{"x":0,"y":0,"scale":1,"command":"do not execute"}}),
        json!({"positions":{"machine:not-a-uuid":{"x":0,"y":0}}}),
        json!({"positions":{"place:https://example.test":{"x":0,"y":0}}}),
        json!({"positions":{"place:workspace":{"x":0,"y":0,"credential":"secret"}}}),
        json!({"positions":{"place:workspace":{"x":0,"y":-1000001}}}),
        json!({"machine_id":"arbitrary host command"}),
        json!({"place_id":"../../private-file"}),
        json!({"room_id":"room\ncommand"}),
        json!({"navigation":"install"}),
        json!({"selected_id":uuid::Uuid::new_v4()}),
        json!({"password":"secret"}),
        json!({"command":"do not execute"}),
    ];
    for world in bad {
        assert!(
            views::commands(&s, id, &set_world(2, world.clone())).is_err(),
            "accepted {world}"
        );
        assert_eq!(views::read(&s, id).unwrap(), current);
    }
    let stale = views::commands(&s, id, &set_world(1, json!({}))).unwrap_err();
    assert_eq!(stale.0, axum::http::StatusCode::CONFLICT);
    assert_eq!(views::read(&s, id).unwrap(), current);
}

#[test]
fn world_positions_allow_the_bound_and_refuse_overflow() {
    let (_dir, s) = state();
    let v = views::create(&s).unwrap();
    let id = v["id"].as_str().unwrap();
    let mut positions = serde_json::Map::new();
    for i in 0..500 {
        positions.insert(format!("place:p{i}"), json!({"x":i,"y":-i}));
    }
    let current = views::commands(
        &s,
        id,
        &set_world(
            1,
            json!({
                "positions":positions,"camera":{"x":-1000000,"y":1000000,"scale":0.05}
            }),
        ),
    )
    .unwrap();
    let mut overflow = current["world"].clone();
    overflow["positions"]["place:one-too-many"] = json!({"x":0,"y":0});
    assert!(views::commands(&s, id, &set_world(2, overflow)).is_err());
    assert_eq!(views::read(&s, id).unwrap(), current);
    assert!(
        views::commands(
            &s,
            id,
            &set_world(
                2,
                json!({
                    "camera":{"x":0,"y":0,"scale":2.5},
                    "machine_id":null,"place_id":null,"room_id":null,"navigation":"world"
                })
            )
        )
        .is_ok()
    );
}

#[test]
fn world_navigation_reuses_the_existing_agent_session_boundary() {
    let (_dir, s) = state();
    let alpha = uuid::Uuid::new_v4().to_string();
    let beta = uuid::Uuid::new_v4().to_string();
    let session = s
        .save(
            "sessions",
            json!({
                "id":uuid::Uuid::new_v4(),"endpoint_id":alpha
            }),
            Some(0),
        )
        .unwrap();
    let v = views::create(&s).unwrap();
    let id = v["id"].as_str().unwrap();
    views::commands(
        &s,
        id,
        &json!({"expected_revision":1,"commands":[
            {"op":"set","field":"selected_id","value":alpha},
            {"op":"set","field":"session_id","value":session["id"]}
        ]}),
    )
    .unwrap();
    let mut move_to_beta = set_world(2, json!({"navigation":"agent"}));
    move_to_beta["commands"]
        .as_array_mut()
        .unwrap()
        .push(json!({"op":"set","field":"selected_id","value":beta}));
    let current = views::commands(&s, id, &move_to_beta).unwrap();
    assert!(current["session_id"].is_null());
    assert_eq!(current["selected_id"], beta);
    assert_eq!(
        s.read("sessions", session["id"].as_str().unwrap()).unwrap(),
        session
    );
}

#[test]
fn older_view_without_world_can_opt_in_without_migration() {
    let (_dir, s) = state();
    let mut v = views::create(&s).unwrap();
    let id = v["id"].as_str().unwrap().to_owned();
    v.as_object_mut().unwrap().remove("world");
    s.db.lock()
        .unwrap()
        .execute(
            "UPDATE views SET value=?2 WHERE id=?1",
            [&id, &v.to_string()],
        )
        .unwrap();
    assert!(views::read(&s, &id).unwrap().get("world").is_none());
    let updated = views::commands(&s, &id, &set_world(1, json!({"positions":{}}))).unwrap();
    assert_eq!(updated["world"], json!({"positions":{}}));
    assert_eq!(updated["screen"], "world");
}
