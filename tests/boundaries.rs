use agent_observatory::{
    acp, auth, config::Config, provider, provision, server, state::AppState, views,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tempfile::TempDir;
use tower::ServiceExt;

const PYTHON: &str = if cfg!(windows) { "python" } else { "python3" };

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
    let s = AppState::open(dir.path().into()).unwrap();
    let token = std::fs::read_to_string(dir.path().join("secrets/control-token")).unwrap();
    (dir, s, token)
}
async fn request(
    app: &Router,
    token: &str,
    method: &str,
    path: &str,
    b: Value,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .uri(path)
        .method(method)
        .header("content-type", "application/json");
    if !token.is_empty() {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    let r = app
        .clone()
        .oneshot(
            req.body(if b.is_null() {
                Body::empty()
            } else {
                Body::from(b.to_string())
            })
            .unwrap(),
        )
        .await
        .unwrap();
    let status = r.status();
    let bytes = to_bytes(r.into_body(), 8 * 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
fn fixture_config(dir: &TempDir) -> Value {
    let path = dir.path().join("fixture.py");
    std::fs::write(&path,r#"import sys,json
session='fixture-session'
def send(x): print(json.dumps(x),flush=True)
for line in sys.stdin:
 r=json.loads(line);m=r.get('method');i=r.get('id')
 if m=='initialize': result={'protocolVersion':1,'agentInfo':{'name':'independent-fixture'},'agentCapabilities':{'loadSession':True}}
 elif m in ('session/new','session/load'): result={'sessionId':session}
 elif m=='session/prompt':
  text=r['params']['prompt'][0]['text'];send({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':session,'update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'heard: '+text}}}});result={'stopReason':'end_turn'}
 else: result={}
 send({'jsonrpc':'2.0','id':i,'result':result})
"#).unwrap();
    json!({"command":PYTHON,"args":[path],"cwd":dir.path(),"transport":"stdio"})
}

#[tokio::test]
async fn experiment_transcripts_are_authenticated_revisioned_and_separate_from_the_index() {
    let (dir, state, token) = state();
    let app = server::router(state.clone());
    let (_, report) = request(
        &app,
        &token,
        "POST",
        "/api/v1/experiments",
        json!({"label":"Workshop", "status":"completed", "measurements":{"checked":9}}),
    )
    .await;
    let id = report["id"].as_str().unwrap();
    let path = format!("/api/v1/experiments/{id}/transcript");
    let (_, observer) = request(
        &app,
        &token,
        "POST",
        "/api/v1/tokens",
        json!({"name":"reviewer", "scopes":["observe"]}),
    )
    .await;
    let observer = observer["token"].as_str().unwrap();
    let body = json!({"expected_revision":report["revision"],"messages":[{"id":"one","speaker":"Alex","kind":"simulated_human","room":"product","text":"**Make the result visible.**"}]});
    assert_eq!(
        request(&app, "", "GET", &path, Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(&app, observer, "PUT", &path, body.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, &token, "PUT", &path, body.clone()).await.0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, &token, "PUT", &path, body).await.0,
        StatusCode::CONFLICT
    );
    let (_, index) = request(&app, observer, "GET", "/api/v1/experiments", Value::Null).await;
    assert_eq!(index["experiments"][0]["transcript_count"], 1);
    assert!(index["experiments"][0].get("transcript").is_none());
    assert_eq!(index["experiments"][0]["measurements"]["checked"], 9);
    let reopened = server::router(AppState::open(dir.path().into()).unwrap());
    let (status, transcript) = request(&reopened, observer, "GET", &path, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        transcript["messages"][0]["text"],
        "**Make the result visible.**"
    );
    assert_eq!(transcript["messages"][0]["kind"], "simulated_human");
    let view = views::create(&state).unwrap();
    let selected = views::commands(&state, view["id"].as_str().unwrap(), &json!({"expected_revision":view["revision"],"commands":[{"op":"set","field":"screen","value":"experiments"},{"op":"set","field":"experiment_id","value":id},{"op":"set","field":"experiment_measurements","value":true}]})).unwrap();
    assert_eq!(selected["experiment_id"], id);
    assert_eq!(selected["experiment_measurements"], true);
}

#[tokio::test]
async fn experiment_transcript_contract_refuses_private_fields_and_ambiguous_entries() {
    let (_dir, state, token) = state();
    let app = server::router(state);
    let (_, report) = request(
        &app,
        &token,
        "POST",
        "/api/v1/experiments",
        json!({"label":"Workshop"}),
    )
    .await;
    let path = format!(
        "/api/v1/experiments/{}/transcript",
        report["id"].as_str().unwrap()
    );
    let public = json!({"id":"one","speaker":"Builder","kind":"support_agent","room":"product","text":"Delivered"});
    for messages in [
        json!([{"private_draft":"Do not publish"}]),
        json!([public.clone(), public.clone()]),
        json!([{ "id":"private", "speaker":"Builder", "kind":"private_draft", "room":"product", "text":"Unpublished"}]),
        json!([{ "id":"large", "speaker":"Builder", "kind":"support_agent", "room":"product", "text":"x".repeat(128_001)}]),
    ] {
        assert_eq!(
            request(
                &app,
                &token,
                "PUT",
                &path,
                json!({"expected_revision":1,"messages":messages})
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        request(&app, &token, "GET", &path, Value::Null).await.1["revision"],
        1
    );
    let (_, saved) = request(
        &app,
        &token,
        "PUT",
        &path,
        json!({"expected_revision":1,"messages":[public]}),
    )
    .await;
    assert_eq!(saved["message_count"], 1);
    assert_eq!(
        request(
            &app,
            &token,
            "PUT",
            &path,
            json!({"expected_revision":saved["revision"],"messages":[]})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, &token, "GET", &path, Value::Null).await.1["messages"],
        json!([])
    );
}

#[tokio::test]
async fn live_progress_permission_decisions_and_cancellation_stay_with_the_requester() {
    let (dir, state, token) = state();
    let app = server::router(state.clone());
    let file = dir.path().join("permissions.py");
    std::fs::write(&file, include_str!("fixture_permissions.py")).unwrap();
    let (_, endpoint) = request(
        &app,
        &token,
        "POST",
        "/api/v1/endpoints",
        json!({"label":"Independent peer","acp":{"command":PYTHON,"args":[file],"cwd":dir.path()}}),
    )
    .await;
    let (_, session) = request(
        &app,
        &token,
        "POST",
        "/api/v1/sessions",
        json!({"endpoint_id":endpoint["id"]}),
    )
    .await;
    let path = format!("/api/v1/sessions/{}", session["id"].as_str().unwrap());
    let (_, other) = request(
        &app,
        &token,
        "POST",
        "/api/v1/tokens",
        json!({"name":"other","scopes":["interact"]}),
    )
    .await;
    let other = other["token"].as_str().unwrap();
    for cancel in [false, true] {
        assert_eq!(
            request(
                &app,
                &token,
                "POST",
                &format!("{path}/prompt"),
                json!({"text":"Perform action","request_id":uuid::Uuid::new_v4()})
            )
            .await
            .0,
            StatusCode::OK
        );
        let mut pending = Value::Null;
        for _ in 0..100 {
            pending = request(&app, &token, "GET", &path, Value::Null).await.1;
            if pending["permissions"]
                .as_array()
                .is_some_and(|p| !p.is_empty())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(pending["status"], "running");
        assert_eq!(pending["progress"]["text"], "Preparing.");
        assert_eq!(pending["progress"]["tools"][0]["title"], "Write a note");
        assert_eq!(pending["progress"]["plan"][0]["content"], "Prepare note");
        assert!(!pending.to_string().contains("PRIVATE THOUGHT"));
        assert!(!pending.to_string().contains("FOREIGN"));
        let id = pending["permissions"][0]["id"].as_str().unwrap();
        let route = format!("{path}/permissions/{id}");
        assert_eq!(
            request(&app, other, "GET", &path, Value::Null).await.1["permissions"],
            json!([])
        );
        assert_eq!(
            request(&app, other, "POST", &route, json!({"option_id":"once"}))
                .await
                .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            request(
                &app,
                &token,
                "POST",
                &route,
                json!({"option_id":"invented"})
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        if cancel {
            assert_eq!(
                request(&app, other, "POST", &format!("{path}/cancel"), json!({}))
                    .await
                    .0,
                StatusCode::FORBIDDEN
            );
            assert_eq!(
                request(&app, &token, "POST", &format!("{path}/cancel"), json!({}))
                    .await
                    .0,
                StatusCode::OK
            );
        } else {
            assert_eq!(
                request(&app, &token, "POST", &route, json!({"option_id":"once"}))
                    .await
                    .0,
                StatusCode::OK
            );
            assert_eq!(
                request(&app, &token, "POST", &route, json!({"option_id":"once"}))
                    .await
                    .0,
                StatusCode::CONFLICT
            );
        }
        for _ in 0..100 {
            let saved = request(&app, &token, "GET", &path, Value::Null).await.1;
            if saved["status"] != "running" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let saved = request(&app, &token, "GET", &path, Value::Null).await.1;
        assert_eq!(saved["status"], if cancel { "cancelled" } else { "idle" });
        assert_eq!(
            saved["progress"]["tools"][0]["status"],
            if cancel { "cancelled" } else { "completed" }
        );
        assert!(
            state
                .permissions
                .list(session["id"].as_str().unwrap(), "token:local-control")
                .is_empty()
        );
        if !cancel {
            assert_eq!(saved["messages"][1]["text"], "Preparing.\n\nDone.");
        }
        assert_eq!(
            request(&app, &token, "POST", &route, json!({"option_id":"once"}))
                .await
                .0,
            StatusCode::CONFLICT
        );
    }
    let audit = request(&app, &token, "GET", "/api/v1/activity", Value::Null)
        .await
        .1;
    assert!(!audit.to_string().contains("private-action"));
}

#[tokio::test]
async fn context_credentials_are_agent_scoped_read_only_and_revocable() {
    let (_dir, state, admin) = state();
    let app = server::router(state.clone());
    let a = uuid::Uuid::new_v4().to_string();
    let b = uuid::Uuid::new_v4().to_string();
    for (id, label) in [(&a, "Agent A"), (&b, "Agent B")] {
        state
            .save(
                "endpoints",
                json!({"id":id,"label":label,"acp":{"command":"fixture"}}),
                None,
            )
            .unwrap();
        state.save("sessions",json!({"id":format!("room-{id}"),"endpoint_id":id,"title":label,"messages":[{"role":"user","text":format!("private to {label}"),"at":1},{"role":"assistant","text":"**Answer**","at":2}]}),None).unwrap();
    }
    let (status, issued) = request(
        &app,
        &admin,
        "POST",
        "/api/v1/tokens",
        json!({"scopes":[format!("context:{a}")]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let token = issued["token"].as_str().unwrap();
    let path = format!("/agents/{a}/context/v1/query");
    let query = json!({"operation":"list","access":{"mode":"owner"}});
    let (status, result) = request(&app, token, "POST", &path, query.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    assert_eq!(result["items"][0]["label"], "Agent A");
    assert_eq!(
        request(
            &app,
            token,
            "POST",
            &format!("/agents/{b}/context/v1/query"),
            query.clone()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, token, "GET", "/api/v1/sessions", Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(request(&app,token,"POST",&path,json!({"operation":"read","access":{"mode":"owner"},"conversation_id":format!("room-{b}")})).await.0,StatusCode::FORBIDDEN);
    let (_,read)=request(&app,token,"POST",&path,json!({"operation":"read","access":{"mode":"owner"},"conversation_id":format!("room-{a}"),"limit":1})).await;
    assert_eq!(read["messages"][0]["text"], "**Answer**");
    assert_eq!(read["complete"], false);
    request(
        &app,
        &admin,
        "DELETE",
        &format!("/api/v1/tokens/{}", issued["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(
        request(&app, token, "POST", &path, query).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(
            &app,
            &admin,
            "POST",
            "/api/v1/tokens",
            json!({"scopes":["context:unknown"]})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn scopes_separate_observation_management_interaction_and_host_installation() {
    let (_d, s, t) = state();
    let app = server::router(s);
    assert_eq!(
        request(&app, "", "GET", "/api/v1/endpoints", Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (_, v) = request(
        &app,
        &t,
        "POST",
        "/api/v1/tokens",
        json!({"scopes":["observe"]}),
    )
    .await;
    let read = v["token"].as_str().unwrap();
    assert_eq!(
        request(&app, read, "GET", "/api/v1/endpoints", Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            read,
            "POST",
            "/api/v1/endpoints",
            json!({"label":"x","ui_url":"https://x.invalid"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, read, "GET", "/api/v1/sessions", Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, read, "GET", "/api/v1/jobs", Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (_, v) = request(
        &app,
        &t,
        "POST",
        "/api/v1/tokens",
        json!({"scopes":["manage"]}),
    )
    .await;
    let manage = v["token"].as_str().unwrap();
    assert_eq!(
        request(&app, manage, "POST", "/api/v1/hosts", json!({}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &app,
            manage,
            "POST",
            "/api/v1/tokens",
            json!({"scopes":["admin"]})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}
#[tokio::test]
async fn registration_is_generic_revisioned_and_does_not_imply_agent_management() {
    let (d, s, t) = state();
    let app = server::router(s.clone());
    let (status, e) = request(
        &app,
        &t,
        "POST",
        "/api/v1/endpoints",
        json!({"label":"Independent","kind":"agent","acp":fixture_config(&d)}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = e["id"].as_str().unwrap();
    let (_, o) = request(
        &app,
        &t,
        "POST",
        &format!("/api/v1/endpoints/{id}/inspect"),
        json!({}),
    )
    .await;
    assert_eq!(o["management"], "unsupported");
    assert_eq!(o["health"], "unknown");
    let (_, p) = request(
        &app,
        &t,
        "POST",
        &format!("/api/v1/endpoints/{id}/probe"),
        json!({}),
    )
    .await;
    assert_eq!(p["agentInfo"]["name"], "independent-fixture");
    let (_, observed) = request(
        &app,
        &t,
        "GET",
        &format!("/api/v1/endpoints/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(observed["acp_observation"]["health"], "reachable");
    assert_eq!(
        observed["acp_observation"]["capabilities"]["loadSession"],
        true
    );
    assert_eq!(observed["observation"]["management"], "unsupported");
    assert!(observed["management"].is_null());
    assert!(provider::operations(&observed).is_empty());
    let (status, _) = request(
        &app,
        &t,
        "PUT",
        &format!("/api/v1/endpoints/{id}"),
        json!({"label":"Changed","ui_url":"https://example.test","expected_revision":0}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(s.read("endpoints", id).is_ok());
}
#[tokio::test]
async fn acp_health_persists_failure_without_overwriting_management_or_trusting_edited_connections()
{
    let (d, s, t) = state();
    let app = server::router(s.clone());
    let (_, e) = request(
        &app,
        &t,
        "POST",
        "/api/v1/endpoints",
        json!({"label":"Independent", "kind":"agent", "acp":fixture_config(&d)}),
    )
    .await;
    let id = e["id"].as_str().unwrap();
    let probe = format!("/api/v1/endpoints/{id}/probe");
    let endpoint = format!("/api/v1/endpoints/{id}");
    // Separate provider data must survive both a successful and a failed ACP check.
    let management = s
        .save(
            "observations",
            json!({"id":id,"health":"reachable","status":{"private_provider_field":"preserved"}}),
            None,
        )
        .unwrap();
    let (_, observer) = request(
        &app,
        &t,
        "POST",
        "/api/v1/tokens",
        json!({"scopes":["observe"]}),
    )
    .await;
    assert_eq!(
        request(
            &app,
            observer["token"].as_str().unwrap(),
            "POST",
            &probe,
            json!({})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert!(s.read("acp_observations", id).is_err());
    assert_eq!(
        request(&app, &t, "POST", &probe, json!({})).await.0,
        StatusCode::OK
    );
    let (_, rows) = request(&app, &t, "GET", "/api/v1/endpoints", Value::Null).await;
    assert_eq!(
        rows["endpoints"][0]["acp_observation"]["health"],
        "reachable"
    );
    assert_eq!(rows["endpoints"][0]["observation"], management);
    let mut edit = e.clone();
    edit.as_object_mut()
        .unwrap()
        .retain(|k, _| ["label", "kind", "acp"].contains(&k.as_str()));
    edit["expected_revision"] = e["revision"].clone();
    edit["acp"]["command"] = json!(d.path().join("missing-agent"));
    assert_eq!(
        request(&app, &t, "PUT", &endpoint, edit).await.0,
        StatusCode::OK
    );
    let (_, edited) = request(&app, &t, "GET", &endpoint, Value::Null).await;
    assert!(edited["acp_observation"].is_null());
    assert_eq!(
        request(&app, &t, "POST", &probe, json!({})).await.0,
        StatusCode::BAD_REQUEST
    );
    let (_, failed) = request(&app, &t, "GET", &endpoint, Value::Null).await;
    assert_eq!(failed["acp_observation"]["health"], "unavailable");
    assert_eq!(
        failed["acp_observation"]["endpoint_revision"],
        edited["revision"]
    );
    assert!(
        failed["acp_observation"]["error"]
            .as_str()
            .unwrap()
            .contains("ACP connection failed")
    );
    assert_eq!(failed["observation"], management);
    assert!(failed["acp_observation"]["capabilities"].is_null());
    // Persisted observations remain useful after the web process exits.
    let reopened = AppState::open(d.path().to_owned()).unwrap();
    assert_eq!(
        reopened.read("acp_observations", id).unwrap(),
        failed["acp_observation"]
    );
}
#[tokio::test]
async fn credentials_are_write_only_and_control_token_cannot_be_referenced() {
    let (_d, s, t) = state();
    let app = server::router(s.clone());
    let secret = "fixture-not-a-real-credential";
    assert_eq!(
        request(
            &app,
            &t,
            "POST",
            "/api/v1/credentials",
            json!({"name":"remote","value":secret})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, v) = request(&app, &t, "GET", "/api/v1/credentials", Value::Null).await;
    assert!(!v.to_string().contains(secret));
    assert_eq!(s.secret("remote").unwrap(), secret);
    assert!(s.secret("control-token").is_err());
    assert!(s.secret("../config.json").is_err());
}
#[tokio::test]
async fn transcript_persists_across_client_exit_and_duplicate_prompt_is_not_replayed() {
    let (d, s, t) = state();
    let app = server::router(s.clone());
    let (_, e) = request(
        &app,
        &t,
        "POST",
        "/api/v1/endpoints",
        json!({"label":"Fixture","kind":"agent","acp":fixture_config(&d)}),
    )
    .await;
    let (_, v) = request(
        &app,
        &t,
        "POST",
        "/api/v1/sessions",
        json!({"endpoint_id":e["id"]}),
    )
    .await;
    let path = format!("/api/v1/sessions/{}/prompt", v["id"].as_str().unwrap());
    let b = json!({"text":"first thought","request_id":uuid::Uuid::new_v4()});
    assert_eq!(
        request(&app, &t, "POST", &path, b.clone()).await.0,
        StatusCode::OK
    );
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        if s.active_sessions.lock().unwrap().is_empty() {
            break;
        }
    }
    let v = s.read("sessions", v["id"].as_str().unwrap()).unwrap();
    assert_eq!(v["messages"][1]["text"], "heard: first thought");
    assert_eq!(v["status"], "idle");
    let (_, again) = request(&app, &t, "POST", &path, b).await;
    assert_eq!(again["messages"].as_array().unwrap().len(), 2);
    let (_, sent) = request(
        &app,
        &t,
        "POST",
        &path,
        json!({"text":"second thought","request_id":uuid::Uuid::new_v4()}),
    )
    .await;
    assert_eq!(sent["status"], "running");
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        if s.active_sessions.lock().unwrap().is_empty() {
            break;
        }
    }
    drop(app);
    drop(s);
    let s = AppState::open(d.path().into()).unwrap();
    let resumed = s.read("sessions", v["id"].as_str().unwrap()).unwrap();
    assert_eq!(resumed["messages"].as_array().unwrap().len(), 4);
    assert_eq!(resumed["acp_session_id"], "fixture-session");
}
#[test]
fn view_commands_are_revisioned_and_acknowledgments_do_not_flood_activity() {
    let (_d, s, _) = state();
    let v = views::create(&s).unwrap();
    let id = v["id"].as_str().unwrap();
    let command = json!({"expected_revision":1,"controller":"voice-agent","commands":[{"op":"set","field":"screen","value":"work"},{"op":"set","field":"filter","value":"urgent"}]});
    let v = views::commands(&s, id, &command).unwrap();
    assert_eq!(v["screen"], "work");
    assert!(views::commands(&s, id, &command).is_err());
    assert!(views::ack(&s, id, 99).is_err());
    views::ack(&s, id, 2).unwrap();
    assert_eq!(views::read(&s, id).unwrap()["rendered_revision"], 2);
    let count: i64 =
        s.db.lock()
            .unwrap()
            .query_row("SELECT count(*) FROM audit", [], |r| r.get(0))
            .unwrap();
    assert_eq!(count, 0);
    assert!(views::commands(&s,id,&json!({"expected_revision":2,"commands":[{"op":"set","field":"draft","value":{"password":"secret"}}]})).is_err());
    let pending = json!({"request_id":uuid::Uuid::new_v4(),"session":uuid::Uuid::new_v4(),"endpoint":uuid::Uuid::new_v4(),"text":"Exact message retry"});
    let set = |value: Value| json!({"expected_revision":2,"commands":[{"op":"set","field":"draft","value":{"pending_send":value}}]});
    let mut extra = pending.clone();
    extra["credential"] = json!("must not be accepted");
    assert!(views::commands(&s, id, &set(extra)).is_err());
    assert_eq!(
        views::commands(&s, id, &set(pending.clone())).unwrap()["draft"]["pending_send"],
        pending
    );
}
#[test]
fn selected_agent_and_conversation_cannot_disagree() {
    let (_d, s, _) = state();
    let alpha = uuid::Uuid::new_v4().to_string();
    let beta = uuid::Uuid::new_v4().to_string();
    let session = s
        .save(
            "sessions",
            json!({"id":uuid::Uuid::new_v4(),"endpoint_id":alpha}),
            Some(0),
        )
        .unwrap();
    let view = views::create(&s).unwrap();
    let id = view["id"].as_str().unwrap();
    let select = |revision, agent: &str| json!({"expected_revision":revision,"commands":[{"op":"set","field":"selected_id","value":agent},{"op":"set","field":"session_id","value":session["id"]}]});
    let error = views::commands(&s, id, &select(1, &beta)).unwrap_err();
    assert!(error.1.contains("different agent"));
    assert_eq!(views::read(&s, id).unwrap()["revision"], 1);
    views::commands(&s, id, &select(1, &alpha)).unwrap();
    let result = views::commands(&s, id, &json!({"expected_revision":2,"commands":[{"op":"set","field":"selected_id","value":beta}]})).unwrap();
    assert!(result["session_id"].is_null());
    assert_eq!(result["selected_id"], beta);
    assert!(views::commands(&s,id,&json!({"expected_revision":3,"commands":[{"op":"set","field":"session_id","value":uuid::Uuid::new_v4()}]})).is_err());
}
#[test]
fn host_plan_is_explicit_bounded_and_revision_pinned() {
    let (d, s, _) = state();
    let h=s.save("hosts",json!({"id":uuid::Uuid::new_v4(),"label":"Test","transport":"local","install_roots":[d.path()],"recipes":[{"id":"fixture","version":"1.2.3","argv":[PYTHON,"-c","print('complete')"]}]}),Some(0)).unwrap();
    let b = json!({"host_id":h["id"],"recipe_id":"fixture","name":"an agent","home":d.path().join("new"),"port":0});
    let p = provision::plan(&s, &b).unwrap();
    assert_eq!(p["version"], "1.2.3");
    assert_eq!(p["host_revision"], 1);
    assert!(p["plan_digest"].as_str().is_some());
    let mut bad = b.clone();
    bad["home"] = json!(d.path().join("../elsewhere"));
    assert!(provision::plan(&s, &bad).is_err());
    bad["home"] = json!(d.path());
    assert!(provision::plan(&s, &bad).is_err());
    assert!(provider::validate(&json!({"label":"x","management":{"base_url":"https://example.test","provider":"generic-json","credential_ref":"control-token"}})).is_err());
}
#[tokio::test]
async fn install_is_idempotent_preserves_logs_and_rejects_stale_plan() {
    let (d, s, _) = state();
    let h=s.save("hosts",json!({"id":uuid::Uuid::new_v4(),"label":"Test","transport":"local","install_roots":[d.path()],"recipes":[{"id":"fixture","version":"1","argv":[PYTHON,"-c","print('fixture installed')"]}]}),Some(0)).unwrap();
    let mut b = json!({"host_id":h["id"],"recipe_id":"fixture","name":"test","home":d.path().join("new"),"request_id":uuid::Uuid::new_v4()});
    let p = provision::plan(&s, &b).unwrap();
    b["plan_digest"] = p["plan_digest"].clone();
    let job = provision::start(s.clone(), &b, "test").unwrap();
    let again = provision::start(s.clone(), &b, "test").unwrap();
    assert_eq!(job["id"], again["id"]);
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        if s.read("jobs", job["id"].as_str().unwrap()).unwrap()["status"] == "completed" {
            break;
        }
    }
    assert_eq!(
        s.read("jobs", job["id"].as_str().unwrap()).unwrap()["status"],
        "completed"
    );
    assert!(
        std::fs::read_to_string(
            d.path()
                .join("jobs")
                .join(format!("{}.stdout", job["id"].as_str().unwrap()))
        )
        .unwrap()
        .contains("fixture installed")
    );
    let mut h = h;
    h["recipes"][0]["version"] = json!("2");
    s.save("hosts", h, Some(1)).unwrap();
    b["request_id"] = json!(uuid::Uuid::new_v4());
    assert!(provision::start(s.clone(), &b, "test").is_err());
}
#[tokio::test]
async fn provider_pins_remote_identity_before_any_mutation() {
    let (_d, s, _) = state();
    let e = json!({"label":"x","management":{"base_url":"https://127.0.0.1:1","provider":"agent-instance","credential_ref":"remote","access":"manage"}});
    let error = provider::call(&s, &e, "lifecycle", Some(&json!({"action":"stop"})))
        .await
        .unwrap_err();
    assert!(error.1.contains("pin"));
    let mut e = e;
    e["management"]["access"] = json!("observe");
    assert!(
        provider::call(&s, &e, "lifecycle", None)
            .await
            .unwrap_err()
            .1
            .contains("observation")
    );
}
#[test]
fn unsafe_urls_and_argument_shapes_are_rejected() {
    for url in [
        "http://host",
        "https://user:password@host",
        "https://host/path",
        "https://host?x=1",
    ] {
        assert!(provider::validate(&json!({"label":"x","ui_url":url})).is_err());
    }
    assert!(acp::validate(&json!({"command":"x","cwd":"relative"})).is_err());
    assert!(acp::validate(&json!({"command":"x","cwd":"/tmp","args":[1]})).is_err());
    assert!(provider::valid_path("work/123"));
    assert!(!provider::valid_path("../secrets"));
    assert!(!provider::valid_path("%2e%2e"));
}
#[tokio::test]
async fn browser_mutations_require_csrf_and_origin() {
    let (_d, s, _) = state();
    let cookie = "a-cookie";
    s.db.lock()
        .unwrap()
        .execute(
            "INSERT INTO browser_sessions VALUES(?1,'test','csrf',?2)",
            rusqlite::params![
                auth::digest(cookie),
                agent_observatory::state::now_ms() + 10000
            ],
        )
        .unwrap();
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        "cookie",
        format!("{}={cookie}", auth::cookie_name(&s))
            .parse()
            .unwrap(),
    );
    assert!(auth::check(&s, &headers, "manage", true).is_err());
    headers.insert("x-csrf-token", "csrf".parse().unwrap());
    headers.insert("host", "local.test".parse().unwrap());
    headers.insert("origin", "https://evil.test".parse().unwrap());
    assert!(auth::check(&s, &headers, "manage", true).is_err());
    headers.insert("origin", "https://local.test".parse().unwrap());
    assert!(auth::check(&s, &headers, "manage", true).is_ok());
}

#[tokio::test]
async fn endpoint_without_restore_keeps_its_live_context_until_explicit_disconnect() {
    let (d, s, _t) = state();
    let cfg = fixture_config(&d);
    let script = d.path().join("fixture.py");
    let code = std::fs::read_to_string(&script)
        .unwrap()
        .replace("'loadSession':True", "'loadSession':False")
        .replace(
            "session='fixture-session'",
            "session='fixture-session'\ncount=0",
        )
        .replace("text=r['params']", "count+=1; text=r['params']")
        .replace("'heard: '+text", "str(count)+': '+text");
    std::fs::write(script, code).unwrap();
    let a = acp::prompt(&s, "context", &cfg, None, "first", "test")
        .await
        .unwrap();
    assert_eq!(a["reply"], "1: first");
    assert_eq!(a["resumable"], false);
    let b = acp::prompt(
        &s,
        "context",
        &cfg,
        a["session_id"].as_str(),
        "second",
        "test",
    )
    .await
    .unwrap();
    assert_eq!(b["reply"], "2: second");
    s.acp_clients.lock().unwrap().remove("context");
    let error = acp::prompt(
        &s,
        "context",
        &cfg,
        a["session_id"].as_str(),
        "third",
        "test",
    )
    .await
    .unwrap_err();
    assert!(error.1.contains("cannot restore"));
    assert!(s.acp_clients.lock().unwrap().is_empty());
}

#[test]
fn provider_discovery_does_not_offer_missing_remote_operations() {
    let endpoint = json!({"management":{"provider":"agent-instance"}});
    let descriptor = json!({"operations":[{"path":"config","methods":["GET","PUT"]}]});
    let ops = provider::available_operations(&endpoint, &descriptor);
    assert!(ops.iter().any(|o| o.id == "config"));
    assert!(!ops.iter().any(|o| o.id == "work"));
    assert!(!ops.iter().any(|o| o.id == "memory"));
    let legacy = provider::available_operations(&endpoint, &json!({}));
    assert!(!legacy.iter().any(|o| {
        [
            "memory",
            "scheduler",
            "native-config",
            "execute-work",
            "updates",
            "request-update",
            "update-policy",
        ]
        .contains(&o.id.as_str())
    }));
    let extended = provider::available_operations(
        &endpoint,
        &json!({"operations":[
            {"path":"memory","methods":["GET"]},
            {"path":"scheduler","methods":["GET"]},
            {"path":"work/{id}/execution","methods":["GET","POST"]}
        ]}),
    );
    assert!(
        extended
            .iter()
            .any(|o| o.id == "memory" && o.access == "memory:read")
    );
    assert!(
        extended
            .iter()
            .any(|o| o.id == "execute-work" && o.access == "work:execute")
    );
    assert!(
        !extended
            .iter()
            .any(|o| o.id == "remember" || o.id == "native-config")
    );
    assert!(
        !provider::operations(&json!({"management":{"provider":"slack-adapter"}}))
            .iter()
            .any(|o| o.id == "validate-config")
    );
    let update_ops = provider::available_operations(
        &endpoint,
        &json!({"operations":[{"path":"updates","methods":["GET","POST"]},{"path":"updates/policy","methods":["PUT"]}]}),
    );
    assert!(
        update_ops
            .iter()
            .any(|o| o.id == "updates" && o.access == "observe")
    );
    assert!(
        update_ops
            .iter()
            .any(|o| o.id == "request-update" && o.access == "manage")
    );
    let generic = json!({"management":{"provider":"generic-json","operations":[{"id":"descriptor","method":"GET","path":"meta","access":"observe"}]}});
    let ops = provider::operations(&generic);
    assert_eq!(ops.iter().filter(|o| o.id == "descriptor").count(), 1);
    assert_eq!(
        ops.iter().find(|o| o.id == "descriptor").unwrap().path,
        "meta"
    );
}

#[test]
fn resource_operations_accept_identifiers_without_arbitrary_paths() {
    let endpoint = json!({"management":{"provider":"agent-instance"}});
    let operation = provider::operations(&endpoint)
        .into_iter()
        .find(|o| o.id == "execute-work")
        .unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let payload = json!({"type":"resume","budget":{"max_turns":4}});
    let (path, forwarded) = provider::operation_request(
        &operation,
        Some(&json!({"resource_id":id,"payload":payload})),
    )
    .unwrap();
    assert_eq!(path, format!("work/{id}/execution"));
    assert_eq!(forwarded, Some(payload));
    for body in [
        json!({"resource_id":"../../secrets"}),
        json!({"resource_id":"work/id"}),
        json!({"resource_id":id,"path":"secrets"}),
    ] {
        assert!(provider::operation_request(&operation, Some(&body)).is_err());
    }
}

#[tokio::test]
async fn memory_and_execution_grants_are_independent_of_observation_and_management() {
    let (_dir, s, admin) = state();
    let app = server::router(s);
    let (status, e) = request(&app, &admin, "POST", "/api/v1/endpoints", json!({"label":"Scoped peer","management":{"provider":"agent-instance","base_url":"https://127.0.0.1:1","credential_ref":"peer","access":"manage","expected_id":"peer"}})).await;
    assert_eq!(status, StatusCode::OK);
    for (scope, operation) in [
        ("observe", "memory"),
        ("work", "execute-work"),
        ("manage", "execute-work"),
        ("memory:read", "remember"),
    ] {
        let (status, token) = request(
            &app,
            &admin,
            "POST",
            "/api/v1/tokens",
            json!({"name":"scope check","scopes":[scope]}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let route = format!(
            "/api/v1/endpoints/{}/operations/{operation}",
            e["id"].as_str().unwrap()
        );
        assert_eq!(
            request(
                &app,
                token["token"].as_str().unwrap(),
                "POST",
                &route,
                json!({})
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
}

#[tokio::test]
async fn custom_work_scope_cannot_turn_an_observation_connection_into_a_writer() {
    let (_d, s, _) = state();
    let endpoint = json!({"label":"Observe only","management":{"provider":"generic-json","base_url":"https://fixture.invalid","credential_ref":"remote","access":"observe","expected_id":"fixture","operations":[{"id":"custom-work","method":"PUT","path":"work","access":"work"}]}});
    assert!(provider::validate(&endpoint).is_err());
    // Also reject any previously persisted bad mapping before making a network request.
    let error = provider::call(&s, &endpoint, "custom-work", Some(&json!({})))
        .await
        .unwrap_err();
    assert!(error.1.contains("observation"));
}

#[tokio::test]
async fn bare_agent_auth_failure_reaches_the_ui_without_raw_provider_error() {
    let (dir, state, _) = state();
    let config = fixture_config(&dir);
    std::fs::write(dir.path().join("fixture.py"), r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 result={'protocolVersion':1,'authMethods':[{'id':'login','name':'Host login','type':'terminal'}]} if r['method']=='initialize' else None
 print(json.dumps({'jsonrpc':'2.0','id':r['id'],**({'result':result} if result else {'error':{'code':-32000,'message':'DO NOT DISPLAY SECRET'}})}),flush=True)
"#).unwrap();
    let discovery = acp::probe(&state, &config).await.unwrap();
    assert_eq!(
        discovery["_meta"]["io.github.bkrabach.agent-observatory"]["authentication"]["methods"][0]
            ["id"],
        "login"
    );
    let error = acp::prompt(&state, "auth", &config, None, "hello", "test")
        .await
        .unwrap_err();
    assert!(error.1.contains("same service account"));
    assert!(!error.1.contains("SECRET"));
}

#[tokio::test]
async fn interactive_auth_and_forms_use_the_initiators_connection() {
    let (dir, state, token) = state();
    let app = server::router(state.clone());
    let fixture = dir.path().join("interactions.py");
    std::fs::write(&fixture, include_str!("fixture_interactions.py")).unwrap();
    let (_,e)=request(&app,&token,"POST","/api/v1/endpoints",json!({"label":"Question fixture","acp":{"command":PYTHON,"args":[fixture],"cwd":dir.path()}})).await;
    let (_, s) = request(
        &app,
        &token,
        "POST",
        "/api/v1/sessions",
        json!({"endpoint_id":e["id"]}),
    )
    .await;
    let path = format!("/api/v1/sessions/{}", s["id"].as_str().unwrap());
    let (_, other) = request(
        &app,
        &token,
        "POST",
        "/api/v1/tokens",
        json!({"name":"another actor","scopes":["interact"]}),
    )
    .await;
    request(
        &app,
        &token,
        "POST",
        &format!("{path}/prompt"),
        json!({"text":"Help","request_id":uuid::Uuid::new_v4()}),
    )
    .await;
    for (mode, content) in [
        ("form", json!({"method_id":"browser"})),
        ("url", json!({"ignored":"never forwarded"})),
        ("form", json!({"direction":"small"})),
    ] {
        let mut form = Value::Null;
        for _ in 0..200 {
            let (_, s) = request(&app, &token, "GET", &path, Value::Null).await;
            if s["elicitations"].as_array().is_some_and(|v| !v.is_empty()) {
                form = s["elicitations"][0].clone();
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(form["mode"], mode);
        assert_eq!(
            request(
                &app,
                other["token"].as_str().unwrap(),
                "GET",
                &path,
                Value::Null
            )
            .await
            .1["elicitations"],
            json!([])
        );
        let route = format!("{path}/elicitations/{}", form["id"].as_str().unwrap());
        assert_eq!(
            request(
                &app,
                other["token"].as_str().unwrap(),
                "POST",
                &route,
                json!({"action":"accept","content":content})
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            request(
                &app,
                &token,
                "POST",
                &route,
                json!({"action":"accept","content":content})
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    for _ in 0..200 {
        let (_, s) = request(&app, &token, "GET", &path, Value::Null).await;
        if s["status"] != "running" {
            assert!(s.to_string().contains("Authorized and answered"), "{s}");
            assert!(!s.to_string().contains("example.test"));
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("Interactive turn did not finish");
}

#[tokio::test]
async fn optional_session_controls_negotiate_and_keep_workspace_scope() {
    let (dir, state, _) = state();
    let file = dir.path().join("sessions.py");
    let log = dir.path().join("frames.jsonl");
    std::fs::write(&file,r#"import sys,json,os
for line in sys.stdin:
 r=json.loads(line);m=r.get('method');p=r.get('params',{});open(os.environ['LOG'],'a').write(json.dumps(r)+'\n')
 if m=='initialize': result={'protocolVersion':1,'agentCapabilities':{'sessionCapabilities':{} if os.environ.get('DISABLED') else {'list':{},'delete':{},'resume':{},'close':{}}}}
 elif m=='session/list': result={'sessions':[{'sessionId':'saved','title':'Saved work','cwd':p['cwd']},{'sessionId':'private-other','title':'Different folder','cwd':'/different'}],'nextCursor':'page-two' if 'cursor' not in p else None}
 elif m in ('session/close','session/delete'):
  print(json.dumps({'jsonrpc':'2.0','id':r['id'],'error':{'code':-32002,'message':'Session is unavailable'}}),flush=True);continue
 elif m=='session/new':result={'sessionId':'saved'}
 elif m=='session/resume':result={}
 elif m=='session/prompt':result={'stopReason':'end_turn'}
 else:raise RuntimeError(m)
 print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#).unwrap();
    let mut config = json!({"command":PYTHON,"args":[file],"cwd":dir.path(),"env":{"LOG":log}});
    let first = acp::catalog(&state, &config, None, None).await.unwrap();
    assert_eq!(first["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(first["nextCursor"], "page-two");
    assert_eq!(first["can_delete"], true);
    assert!(
        acp::catalog(&state, &config, Some("page-two"), None)
            .await
            .unwrap()["nextCursor"]
            .is_null()
    );
    assert_eq!(
        acp::catalog(&state, &config, None, Some("saved"))
            .await
            .unwrap()["removed_from_agent_history"],
        true
    );
    let turn = acp::prompt(&state, "room", &config, None, "hello", "test")
        .await
        .unwrap();
    assert_eq!(turn["resumable"], true);
    acp::disconnect(&state, "room").await.unwrap();
    let turn = acp::prompt(&state, "room", &config, Some("saved"), "again", "test")
        .await
        .unwrap();
    assert_eq!(turn["resumed"], true);
    let frames: Vec<Value> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(frames.iter().any(|f| f["method"] == "session/resume"));
    assert!(frames.iter().any(|f| f["method"] == "session/close"));
    assert!(!frames.iter().any(|f| f["method"] == "session/load"));
    config["env"]["DISABLED"] = json!("1");
    assert!(acp::catalog(&state, &config, None, None).await.is_err());
    assert!(
        acp::catalog(&state, &config, None, Some("saved"))
            .await
            .is_err()
    );
    let raw = std::fs::read_to_string(&log).unwrap();
    let tail: Vec<Value> = raw
        .lines()
        .rev()
        .take(2)
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(tail.iter().all(|f| f["method"] == "initialize"));
}
