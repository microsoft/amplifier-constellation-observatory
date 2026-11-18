//! The real durable adapter with a deterministic owner bridge. No process,
//! download, Slack request, model, OS manager or installed service is invoked.
use agent_observatory::{
    config::Config,
    error::{ApiError, ApiResult},
    slack_local::{Action, ApiAction},
    slack_package::{LockedFile, ReleaseDescriptor},
    slack_setup::{self, AgentBinding, Backend, LockedTarget},
    state::AppState,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
};
use tempfile::TempDir;
use uuid::Uuid;
type ResultFuture<'a, T> = Pin<Box<dyn Future<Output = ApiResult<T>> + Send + 'a>>;

struct Fixture {
    _temp: TempDir,
    state: Arc<AppState>,
    endpoint: Value,
    owner: Owner,
}
struct Owner {
    root: PathBuf,
    state: Arc<AppState>,
    endpoint_id: String,
    connector_id: String,
    data: Mutex<OwnerData>,
    available: bool,
}
struct OwnerData {
    calls: Vec<String>,
    config: Value,
    setup: Value,
    fail_start: bool,
    native_inactive: bool,
    confirmation_refusal: Option<&'static str>,
    fail_connect: bool,
    ready: bool,
    status_ready: bool,
    fail_bind_reply: bool,
    change_endpoint_on_stage: bool,
    fail_authorize_observation: bool,
    fail_config_once: bool,
    change_endpoint_on_config_read: bool,
}
fn name(action: Action) -> &'static str {
    match action {
        Action::Setup => "setup",
        Action::Start { .. } => "start",
        Action::Status => "status",
        Action::Api(a) => match a {
            ApiAction::GetConfig => "get_config",
            ApiAction::PutConfig => "put_config",
            ApiAction::GetStatus => "get_status",
            ApiAction::GetSetup => "get_setup",
            ApiAction::Authorize => "authorize",
            ApiAction::Confirm => "confirm",
            ApiAction::Connect => "connect",
            ApiAction::Discard => "discard",
            _ => "unused",
        },
    }
}
fn lifecycle(saved: &Value, connector: &str, request: Value, state: &str) -> Value {
    json!({"schema":"com.microsoft.amplifier.slack-native-status/1","id":connector,"request":{"request_id":request,"phase":"ready"},"request_current":true,"home":saved["private"]["home"],"executable":saved["private"]["executable"],"state":state,"launch_observed":state=="ready","pid":1234,"process_identity":{"pid":1234,"birth":"fixture-generation"},"profile_matches":true,"autostart":false,"startup_change_supported":false,"service":{"name":"fixture-service"}})
}
fn release() -> LockedTarget {
    let entry = "acp-slack-adapter".to_owned();
    let mut files = BTreeMap::new();
    files.insert(
        entry.clone(),
        LockedFile {
            size: 4,
            sha256: "a".repeat(64),
            mode: 0o755,
        },
    );
    LockedTarget {
        url: "https://github.com/microsoft/amplifier-acp-slack/releases/download/v0.0.0/inert.zip"
            .into(),
        release: ReleaseDescriptor {
            schema: 1,
            source: "b".repeat(40),
            version: "0.0.0".into(),
            target: "fixture-only".into(),
            archive_size: 4,
            archive_sha256: "c".repeat(64),
            entry,
            native_bundle_sha256: "d".repeat(64),
            files,
        },
    }
}
impl Backend for Owner {
    fn agent_ready<'a>(&'a self, _: &'a AppState, _: &'a AgentBinding) -> ResultFuture<'a, ()> {
        Box::pin(async move {
            if self.data.lock().unwrap().ready {
                Ok(())
            } else {
                Err(ApiError::bad("Fixture agent is not enabled and configured"))
            }
        })
    }
    fn release(&self) -> ApiResult<Option<LockedTarget>> {
        Ok(self.available.then(release))
    }
    fn home(&self, id: Uuid) -> ApiResult<PathBuf> {
        Ok(self.root.join(format!("new-slack-{id}")))
    }
    fn stage<'a>(&'a self, cache: &'a Path, _: &'a LockedTarget) -> ResultFuture<'a, PathBuf> {
        Box::pin(async move {
            assert_eq!(
                self.state.read("slack_setup", &self.endpoint_id)?["operation"]["phase"],
                "package"
            );
            std::fs::create_dir_all(cache)?;
            let exe = cache.join("inert-package");
            std::fs::write(&exe, b"inert")?;
            if self.data.lock().unwrap().change_endpoint_on_stage {
                let mut endpoint = self.state.read("endpoints", &self.endpoint_id)?;
                let revision = endpoint["revision"].as_u64();
                endpoint["label"] = json!("Changed during package download");
                self.state.save("endpoints", endpoint, revision)?;
            }
            Ok(exe)
        })
    }
    fn call<'a>(
        &'a self,
        _exe: &'a Path,
        _hash: &'a str,
        home: &'a Path,
        action: Action,
        body: Option<&'a Value>,
    ) -> ResultFuture<'a, Value> {
        Box::pin(async move {
            let mut data = self.data.lock().unwrap();
            data.calls.push(name(action).into());
            let saved = self.state.read("slack_setup", &self.endpoint_id)?;
            match action {
                Action::Setup => {
                    assert_eq!(saved["operation"]["phase"], "native-setup");
                    assert!(!home.exists());
                    std::fs::create_dir_all(home.join("bin"))?;
                    let exe = home.join("bin").join(if cfg!(windows) {
                        "acp-slack-adapter.exe"
                    } else {
                        "acp-slack-adapter"
                    });
                    std::fs::write(&exe, b"inert")?;
                    Ok(
                        json!({"schema":"com.microsoft.amplifier.slack-native-setup/1","created":true,"serving":false,"id":self.connector_id,"version":"0.0.0","home":home.canonicalize()?,"executable":exe.canonicalize()?,"management":{"kind":"installation-local-cli","api_version":"acp-slack-adapter.management/1","relative_api_paths":true,"requires_running_connector":true}}),
                    )
                }
                Action::Start { request_id } => {
                    assert_eq!(saved["operation"]["phase"], "native-start");
                    data.native_inactive = false;
                    if data.fail_start {
                        return Err(ApiError::bad(
                            "injected unknown; never publish this private detail",
                        ));
                    }
                    Ok(lifecycle(
                        &saved,
                        &self.connector_id,
                        json!(request_id),
                        "ready",
                    ))
                }
                Action::Status => {
                    if !data.status_ready {
                        return Ok(json!({"ready":true,"state":"ready"}));
                    }
                    Ok(lifecycle(
                        &saved,
                        &self.connector_id,
                        saved["private"]["native_start_request_id"].clone(),
                        if data.native_inactive {
                            "inactive"
                        } else {
                            "ready"
                        },
                    ))
                }
                Action::Api(ApiAction::GetConfig) => {
                    if data.fail_config_once {
                        data.fail_config_once = false;
                        return Err(ApiError::bad("Read observation unavailable"));
                    }
                    if data.change_endpoint_on_config_read {
                        data.change_endpoint_on_config_read = false;
                        let mut endpoint = self.state.read("endpoints", &self.endpoint_id)?;
                        let revision = endpoint["revision"].as_u64();
                        endpoint["label"] = json!("Changed during connector observation");
                        self.state.save("endpoints", endpoint, revision)?;
                    }
                    Ok(data.config.clone())
                }
                Action::Api(ApiAction::PutConfig) => {
                    assert_eq!(saved["operation"]["phase"], "bind-agent");
                    let body = body.unwrap();
                    assert_eq!(body["expected_revision"], data.config["revision"]);
                    data.config = body["config"].clone();
                    data.config["revision"] = json!(2);
                    if data.fail_bind_reply {
                        return Err(ApiError::bad("Committed binding reply lost"));
                    }
                    Ok(data.config.clone())
                }
                Action::Api(ApiAction::GetStatus) => Ok(
                    json!({"id":self.connector_id,"setup_complete":data.config["setup_complete"],"enabled":data.config["enabled"],"private_unrelated":"never-store"}),
                ),
                Action::Api(ApiAction::GetSetup) => Ok(data.setup.clone()),
                Action::Api(ApiAction::Authorize) => {
                    assert_eq!(saved["operation"]["phase"], "authorize");
                    data.setup["authorization_command"] = json!({"command":"/slackauthticket fixture-private-ticket","expires_at":(agent_observatory::state::now_ms()+600000)as f64/1000.0});
                    data.setup["authorization_pending"] = json!(true);
                    data.setup["next_action"] = json!("confirm");
                    data.fail_config_once = data.fail_authorize_observation;
                    Ok(
                        json!({"command":"/slackauthticket fixture-private-ticket","expires_at":(agent_observatory::state::now_ms()+600000)as f64/1000.0,"private_unrelated":"do-not-persist"}),
                    )
                }
                Action::Api(ApiAction::Confirm) => {
                    assert_eq!(saved["operation"]["phase"], "confirm");
                    assert_eq!(body.unwrap()["challenge"], "ONLY-THIS-CODE");
                    if let Some(code) = data.confirmation_refusal {
                        if code != "invalid_confirmation_format" {
                            data.setup["authorization_pending"] = json!(false);
                            data.setup["next_action"] = json!("authorize");
                            data.setup
                                .as_object_mut()
                                .unwrap()
                                .remove("authorization_command");
                        }
                        return Ok(
                            json!({"confirmation_refusal":{"schema":1,"action":"confirm","code":code,"id":self.connector_id,"config_revision":data.config["revision"]}}),
                        );
                    }
                    data.setup
                        .as_object_mut()
                        .unwrap()
                        .remove("authorization_command");
                    data.setup["authorization_pending"] = json!(false);
                    data.setup["next_action"] = json!("connect");
                    data.setup["authorization"] = json!("managed");
                    data.setup["authorized_workspace"] = json!({"team_id":"TEXACT123","domain":"fixture-workspace","secret":"never-store"});
                    Ok(data.setup.clone())
                }
                Action::Api(ApiAction::Connect) => {
                    assert_eq!(saved["operation"]["phase"], "connect");
                    assert_eq!(body.unwrap()["team_id"], "TEXACT123");
                    data.config["setup_complete"] = json!(true);
                    data.config["enabled"] = json!(true);
                    data.setup["setup_complete"] = json!(true);
                    data.setup["connection_ready"] = json!(true);
                    data.setup["connection_running"] = json!(true);
                    data.setup["next_action"] = json!("done");
                    if data.fail_connect {
                        return Err(ApiError::bad("committed connect lost reply"));
                    }
                    Ok(data.setup.clone())
                }
                Action::Api(ApiAction::Discard) => {
                    assert_eq!(saved["operation"]["phase"], "discard");
                    data.setup["authorized_workspace"] = Value::Null;
                    data.setup["next_action"] = json!("authorize");
                    Ok(data.setup.clone())
                }
                _ => panic!("Unexpected fixture action"),
            }
        })
    }
}
fn fixture(available: bool) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("observatory");
    std::fs::create_dir(&home).unwrap();
    let config:Config=serde_json::from_value(json!({"id":Uuid::new_v4(),"owner":"fixture-owner","bind":"127.0.0.1","port":0,"public_url":"","pam_service":"unused","tls_cert":"","tls_key":"","tls_ca":null})).unwrap();
    config.save(&home).unwrap();
    let state = AppState::open(home).unwrap();
    let id = Uuid::new_v4().to_string();
    let host_id = Uuid::new_v4().to_string();
    let agents = temp.path().join("agents");
    let agent_home = agents.join(&id);
    let workspace = agent_home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(agent_home.join("config.json"),json!({"config":{"backend":{"working_directory":"${AGENT_HOME}/workspace","allowed_working_roots":["${AGENT_HOME}/workspace"]}}}).to_string()).unwrap();
    let workspace = workspace.canonicalize().unwrap();
    let instance_id = Uuid::new_v4().to_string();
    let connector_id = Uuid::new_v4().to_string();
    state
        .save(
            "hosts",
            json!({"id":host_id,"native_local":true,"transport":"local","install_roots":[agents]}),
            Some(0),
        )
        .unwrap();
    state.save("jobs",json!({"id":id,"actor":"fixture-owner","status":"completed","endpoint_id":id,"plan":{"host_id":host_id,"home":agent_home,"recipe_id":"bundled-instance"}}),Some(0)).unwrap();
    let endpoint=state.save("endpoints",json!({"id":id,"label":"Fixture agent","kind":"agent","host_id":host_id,"acp":{"transport":"stdio","command":agent_home.join("bin").join(if cfg!(windows){"agent-instance.exe"}else{"agent-instance"}),"args":["acp","--home",agent_home],"cwd":agent_home,"session_cwd":workspace},"management":{"provider":"agent-instance","access":"manage","expected_id":instance_id}}),Some(0)).unwrap();
    let initial = json!({"id":connector_id,"label":"New connector","revision":1,"enabled":false,"setup_complete":false,"acp":{"command":"","args":[],"env":{},"secret_env":{},"cwd":"","timeout_seconds":123,"client":{"approvals_required":true}},"slack":{"app_id":"","team_id":""},"participation":"mentions"});
    let owner = Owner {
        root: temp.path().to_path_buf(),
        state: state.clone(),
        endpoint_id: id,
        connector_id,
        data: Mutex::new(OwnerData {
            calls: vec![],
            config: initial,
            setup: json!({"authorization":"missing","authorization_pending":false,"next_action":"authorize","setup_complete":false,"connection_ready":false,"connection_running":false}),
            fail_start: false,
            native_inactive: false,
            confirmation_refusal: None,
            fail_connect: false,
            ready: true,
            status_ready: true,
            fail_bind_reply: false,
            change_endpoint_on_stage: false,
            fail_authorize_observation: false,
            fail_config_once: false,
            change_endpoint_on_config_read: false,
        }),
        available,
    };
    Fixture {
        _temp: temp,
        state,
        endpoint,
        owner,
    }
}
async fn read(f: &Fixture) -> Value {
    slack_setup::get_with(
        &f.state,
        f.endpoint["id"].as_str().unwrap(),
        "fixture-owner",
        &f.owner,
    )
    .await
    .unwrap()
}
async fn act(f: &Fixture, mut body: Value) -> Value {
    body["endpoint_revision"] = f.endpoint["revision"].clone();
    slack_setup::command_with(
        &f.state,
        f.endpoint["id"].as_str().unwrap(),
        "fixture-owner",
        body,
        &f.owner,
    )
    .await
    .unwrap()
}
async fn prepare(f: &Fixture) -> Value {
    act(
        f,
        json!({"action":"prepare","endpoint_revision":f.endpoint["revision"],"request_id":Uuid::new_v4(),"expected_revision":0}),
    )
    .await
}
fn next(observed: &Value, action: &str) -> Value {
    json!({"action":action,"endpoint_revision":observed["endpoint_revision"],"operation_id":observed["operation"]["id"],"expected_revision":observed["revision"]})
}
fn assert_refusal(f: &Fixture, value: &Value, body: &Value, code: &str) {
    assert_eq!(value["schema"], 1);
    assert_eq!(value["kind"], "slack_setup_refusal");
    assert_eq!(value["refused_before_intent"], true);
    assert_eq!(value["endpoint_id"], f.endpoint["id"]);
    assert_eq!(value["code"], code);
    for key in [
        "endpoint_revision",
        "action",
        "request_id",
        "operation_id",
        "expected_revision",
    ] {
        assert_eq!(value[key], body[key], "{key}");
    }
    assert_eq!(value.as_object().unwrap().len(), 11);
    assert!(value["message"].is_string());
    assert!(value.get("challenge").is_none());
}
fn mutations(f: &Fixture) -> Vec<String> {
    f.owner
        .data
        .lock()
        .unwrap()
        .calls
        .iter()
        .filter(|s| {
            matches!(
                s.as_str(),
                "setup" | "start" | "put_config" | "authorize" | "confirm" | "connect" | "discard"
            )
        })
        .cloned()
        .collect()
}

#[tokio::test]
async fn disabled_or_unconfigured_agent_refuses_before_intent_or_effect() {
    let f = fixture(true);
    f.owner.data.lock().unwrap().ready = false;
    assert!(
        slack_setup::command_with(
            &f.state,
            f.endpoint["id"].as_str().unwrap(),
            "fixture-owner",
            json!({"action":"prepare","endpoint_revision":f.endpoint["revision"],"request_id":Uuid::new_v4(),"expected_revision":0}),
            &f.owner
        )
        .await
        .is_err()
    );
    assert!(f.state.list("slack_setup").unwrap().is_empty());
    assert!(mutations(&f).is_empty());
}

#[tokio::test]
async fn first_prepare_cannot_silently_bind_a_newer_endpoint_revision() {
    let f = fixture(true);
    let mut endpoint = f.endpoint.clone();
    endpoint["label"] = json!("New selected endpoint revision");
    f.state
        .save(
            "endpoints",
            endpoint,
            Some(f.endpoint["revision"].as_u64().unwrap()),
        )
        .unwrap();
    let result=slack_setup::command_with(&f.state,f.endpoint["id"].as_str().unwrap(),"fixture-owner",json!({"action":"prepare","endpoint_revision":f.endpoint["revision"],"expected_revision":0,"request_id":Uuid::new_v4()}),&f.owner).await;
    assert_eq!(result.unwrap()["code"], "endpoint_changed");
    assert!(mutations(&f).is_empty());
    assert!(f.state.list("slack_setup").unwrap().is_empty());
}

#[tokio::test]
async fn endpoint_change_during_package_staging_refuses_before_native_setup() {
    let f = fixture(true);
    f.owner.data.lock().unwrap().change_endpoint_on_stage = true;
    let result = prepare(&f).await;
    assert_eq!(result["operation"]["state"], "unknown");
    assert!(mutations(&f).is_empty());
}

#[tokio::test]
async fn endpoint_change_during_connector_read_does_not_dispatch_authorization() {
    let f = fixture(true);
    let prepared = prepare(&f).await;
    let before = mutations(&f);
    f.owner.data.lock().unwrap().change_endpoint_on_config_read = true;
    let result = act(&f, next(&prepared, "authorize")).await;
    assert_eq!(result["operation"]["state"], "unknown");
    assert_eq!(mutations(&f), before);
}

#[tokio::test]
async fn exact_lost_start_read_only_recovery_offers_explicit_bind_without_redispatch() {
    let f = fixture(true);
    f.owner.data.lock().unwrap().fail_start = true;
    let lost = prepare(&f).await;
    assert_eq!(lost["operation"]["state"], "unknown");
    let before = mutations(&f);
    let recovered = read(&f).await;
    assert_eq!(recovered["operation"]["state"], "complete");
    assert_eq!(recovered["operation"]["phase"], "bind");
    assert_eq!(recovered["capabilities"]["can_bind"], true);
    assert_eq!(mutations(&f), before);
    let bound = act(&f, next(&recovered, "bind")).await;
    assert_eq!(bound["capabilities"]["can_authorize"], true);
    assert_eq!(mutations(&f), vec!["setup", "start", "put_config"]);
}

#[tokio::test]
async fn exact_committed_binding_read_reconciliation_never_repeats_config_write() {
    let f = fixture(true);
    f.owner.data.lock().unwrap().fail_bind_reply = true;
    let lost = prepare(&f).await;
    assert_eq!(lost["operation"]["phase"], "bind-agent");
    assert_eq!(lost["operation"]["uncertain"], true);
    let before = mutations(&f);
    let recovered = read(&f).await;
    assert_eq!(recovered["operation"]["uncertain"], false);
    assert_eq!(recovered["capabilities"]["can_authorize"], true);
    assert_eq!(mutations(&f), before);
}

#[tokio::test]
async fn current_binding_failure_clears_actions_but_preserves_action_history() {
    let f = fixture(true);
    let before = prepare(&f).await;
    let command = f.owner.data.lock().unwrap().config["acp"]["command"].clone();
    f.owner.data.lock().unwrap().config["acp"]["command"] = json!("/different-agent");
    let invalid = read(&f).await;
    assert_eq!(invalid["connector"]["binding_verified"], false);
    assert_eq!(invalid["capabilities"]["can_authorize"], false);
    assert_eq!(invalid["operation"], before["operation"]);
    f.owner.data.lock().unwrap().config["acp"]["command"] = command;
    let restored = read(&f).await;
    assert_eq!(restored["connector"]["binding_verified"], true);
    assert_eq!(restored["capabilities"]["can_authorize"], true);
    assert_eq!(restored["operation"], before["operation"]);
}

#[tokio::test]
async fn acknowledged_action_and_failed_read_are_distinct_without_losing_transient_command() {
    let f = fixture(true);
    let prepared = prepare(&f).await;
    f.owner.data.lock().unwrap().fail_authorize_observation = true;
    let result = act(&f, next(&prepared, "authorize")).await;
    assert_eq!(result["operation"]["state"], "complete");
    assert_eq!(result["operation"]["uncertain"], false);
    assert_eq!(result["observation"]["unavailable"], true);
    assert_eq!(result["capabilities"]["can_authorize"], false);
    assert_eq!(result["connector"]["binding_verified"], false);
    assert_eq!(
        result["authorization_command"]["command"],
        "/slackauthticket fixture-private-ticket"
    );
    let before = mutations(&f);
    let recovered = read(&f).await;
    assert_eq!(recovered["operation"], result["operation"]);
    assert_eq!(recovered["capabilities"]["can_confirm"], true);
    assert_eq!(mutations(&f), before);
}

#[tokio::test]
async fn saved_projection_list_has_no_bridge_calls_paths_or_fresh_authority() {
    let f = fixture(true);
    prepare(&f).await;
    let calls = f.owner.data.lock().unwrap().calls.clone();
    let saved = slack_setup::list_saved(&f.state, "fixture-owner").unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0]["observation_fresh"], false);
    assert_eq!(saved[0]["capabilities"], json!({}));
    assert_eq!(f.owner.data.lock().unwrap().calls, calls);
    assert!(
        !saved[0]
            .to_string()
            .contains(f.owner.root.to_str().unwrap())
    );
    assert!(
        slack_setup::list_saved(&f.state, "other-owner")
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn missing_package_never_advertises_or_dispatches_prepare() {
    let f = fixture(false);
    assert_eq!(read(&f).await["capabilities"]["can_prepare"], false);
    let body = json!({"action":"prepare","endpoint_revision":f.endpoint["revision"],"request_id":Uuid::new_v4(),"expected_revision":0});
    let refusal = slack_setup::command_with(
        &f.state,
        f.endpoint["id"].as_str().unwrap(),
        "fixture-owner",
        body.clone(),
        &f.owner,
    )
    .await
    .unwrap();
    assert_refusal(&f, &refusal, &body, "package_unavailable");
    assert!(f.owner.data.lock().unwrap().calls.is_empty());
    assert!(f.state.list("slack_setup").unwrap().is_empty());
}

#[tokio::test]
async fn existing_fresh_home_refusal_is_correlated_without_adoption_or_cleanup() {
    let f = fixture(true);
    let request = Uuid::new_v4();
    let home = f.owner.home(request).unwrap();
    std::fs::create_dir(&home).unwrap();
    std::fs::write(home.join("retained"), b"unchanged").unwrap();
    let body = json!({"action":"prepare","endpoint_revision":f.endpoint["revision"],"request_id":request,"expected_revision":0});
    let refusal = act(&f, body.clone()).await;
    assert_refusal(&f, &refusal, &body, "existing_home");
    assert_eq!(std::fs::read(home.join("retained")).unwrap(), b"unchanged");
    assert!(f.state.list("slack_setup").unwrap().is_empty());
    assert!(mutations(&f).is_empty());
}

#[tokio::test]
async fn durable_save_failure_is_never_translated_into_safe_refusal() {
    let f = fixture(true);
    f.state.db.lock().unwrap().execute_batch("CREATE TRIGGER reject_setup BEFORE INSERT ON records WHEN NEW.kind='slack_setup' BEGIN SELECT RAISE(ABORT,'controlled fixture store failure'); END;").unwrap();
    let result=slack_setup::command_with(&f.state,f.endpoint["id"].as_str().unwrap(),"fixture-owner",json!({"action":"prepare","endpoint_revision":f.endpoint["revision"],"request_id":Uuid::new_v4(),"expected_revision":0}),&f.owner).await;
    assert!(result.is_err());
    assert!(mutations(&f).is_empty());
    assert!(f.state.list("slack_setup").unwrap().is_empty());
}

#[tokio::test]
async fn unreadable_saved_state_never_receives_a_refusal_marker() {
    let f = fixture(true);
    f.state
        .db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO records VALUES(?1,?2,?3)",
            rusqlite::params![
                "slack_setup",
                f.endpoint["id"].as_str().unwrap(),
                "not json"
            ],
        )
        .unwrap();
    let result=slack_setup::command_with(&f.state,f.endpoint["id"].as_str().unwrap(),"fixture-owner",json!({"action":"prepare","endpoint_revision":f.endpoint["revision"],"request_id":Uuid::new_v4(),"expected_revision":0}),&f.owner).await;
    assert!(result.is_err());
    assert!(mutations(&f).is_empty());
}
#[tokio::test]
async fn prepare_records_every_intent_binds_known_agent_and_duplicate_never_restarts() {
    let f = fixture(true);
    let observed = prepare(&f).await;
    assert_eq!(observed["capabilities"]["can_authorize"], true);
    assert_eq!(observed["connector"]["binding_verified"], true);
    assert_eq!(observed["connector"]["setup_complete"], false);
    let before = f.owner.data.lock().unwrap().calls.clone();
    let duplicate = act(
        &f,
        json!({"action":"prepare","endpoint_revision":f.endpoint["revision"],"request_id":Uuid::new_v4(),"expected_revision":0}),
    )
    .await;
    assert_eq!(duplicate["operation"]["id"], observed["operation"]["id"]);
    assert_eq!(f.owner.data.lock().unwrap().calls, before);
    let cfg = f.owner.data.lock().unwrap().config.clone();
    assert_eq!(cfg["acp"]["args"], f.endpoint["acp"]["args"]);
    assert_eq!(cfg["acp"]["cwd"], f.endpoint["acp"]["session_cwd"]);
    assert_eq!(cfg["acp"]["timeout_seconds"], 123);
    assert_eq!(cfg["acp"]["client"]["approvals_required"], true);
    assert_eq!(cfg["participation"], "mentions");
    assert_eq!(cfg["enabled"], false);
}
#[tokio::test]
async fn authorization_is_transient_workspace_confirmed_and_secrets_never_persist() {
    let f = fixture(true);
    let p = prepare(&f).await;
    let a = act(&f, next(&p, "authorize")).await;
    assert!(
        a["authorization_command"]["expires_at"].as_i64().unwrap()
            > agent_observatory::state::now_ms()
    );
    assert_eq!(a["capabilities"]["can_confirm"], true);
    let stored = f
        .state
        .read("slack_setup", f.endpoint["id"].as_str().unwrap())
        .unwrap()
        .to_string();
    assert!(!stored.contains("fixture-private-ticket"));
    assert!(!stored.contains("never-store"));
    let mut body = next(&a, "confirm");
    body["challenge"] = json!("ONLY-THIS-CODE");
    let confirmed = act(&f, body).await;
    assert_eq!(
        confirmed["setup"]["authorized_workspace"]["team_id"],
        "TEXACT123"
    );
    let mut wrong = next(&confirmed, "connect");
    wrong["team_id"] = json!("TWRONG123");
    let refusal = slack_setup::command_with(
        &f.state,
        f.endpoint["id"].as_str().unwrap(),
        "fixture-owner",
        wrong.clone(),
        &f.owner,
    )
    .await
    .unwrap();
    assert_refusal(&f, &refusal, &wrong, "invalid_input");
    let mut correct = next(&confirmed, "connect");
    correct["team_id"] = json!("TEXACT123");
    let done = act(&f, correct).await;
    assert_eq!(done["connector"]["setup_complete"], true);
    assert_eq!(done["connector"]["endpoint_id"], f.endpoint["id"]);
    assert_eq!(done["connector"]["binding_verified"], true);
    let persisted = f
        .state
        .read("slack_setup", f.endpoint["id"].as_str().unwrap())
        .unwrap()
        .to_string();
    for secret in [
        "fixture-private-ticket",
        "ONLY-THIS-CODE",
        "never-store",
        "do-not-persist",
    ] {
        assert!(!persisted.contains(secret));
        assert!(!done.to_string().contains(secret));
    }
}
#[tokio::test]
async fn uncertain_start_and_restart_never_replay_or_adopt_an_existing_home() {
    let f = fixture(true);
    f.owner.data.lock().unwrap().fail_start = true;
    f.owner.data.lock().unwrap().status_ready = false;
    let result = prepare(&f).await;
    assert_eq!(result["operation"]["state"], "unknown");
    assert!(result.get("refused_before_intent").is_none());
    let saved = f
        .state
        .read("slack_setup", f.endpoint["id"].as_str().unwrap())
        .unwrap();
    assert!(Path::new(saved["private"]["home"].as_str().unwrap()).exists());
    let before = mutations(&f);
    assert_eq!(read(&f).await["operation"]["state"], "unknown");
    act(
        &f,
        json!({"action":"prepare","endpoint_revision":f.endpoint["revision"],"request_id":Uuid::new_v4(),"expected_revision":0}),
    )
    .await;
    assert_eq!(mutations(&f), before);
    let mut pending = saved;
    pending["operation"]["state"] = json!("running");
    f.state
        .save("slack_setup", pending.clone(), pending["revision"].as_u64())
        .unwrap();
    assert_eq!(read(&f).await["operation"]["state"], "unknown");
    assert_eq!(mutations(&f), before);
}
#[tokio::test]
async fn later_generic_ready_does_not_settle_a_lost_connect_or_repeat_it() {
    let f = fixture(true);
    let p = prepare(&f).await;
    let a = act(&f, next(&p, "authorize")).await;
    let mut c = next(&a, "confirm");
    c["challenge"] = json!("ONLY-THIS-CODE");
    let c = act(&f, c).await;
    f.owner.data.lock().unwrap().fail_connect = true;
    let mut b = next(&c, "connect");
    b["team_id"] = json!("TEXACT123");
    let lost = act(&f, b).await;
    assert_eq!(lost["operation"]["uncertain"], true);
    let later = read(&f).await;
    assert_eq!(later["connector"]["setup_complete"], true);
    assert_eq!(later["operation"]["uncertain"], true);
    assert_eq!(later["capabilities"]["can_connect"], false);
    assert_eq!(
        f.owner
            .data
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|s| s.as_str() == "connect")
            .count(),
        1
    );
}
#[tokio::test]
async fn actor_revision_and_unrelated_endpoint_are_not_mutation_authority() {
    let f = fixture(true);
    let p = prepare(&f).await;
    let before = f.owner.data.lock().unwrap().calls.len();
    assert!(
        slack_setup::get_with(
            &f.state,
            f.endpoint["id"].as_str().unwrap(),
            "other-owner",
            &f.owner
        )
        .await
        .is_err()
    );
    let mut bad = next(&p, "authorize");
    bad["expected_revision"] = json!(0);
    let refusal = slack_setup::command_with(
        &f.state,
        f.endpoint["id"].as_str().unwrap(),
        "fixture-owner",
        bad.clone(),
        &f.owner,
    )
    .await
    .unwrap();
    assert_refusal(&f, &refusal, &bad, "setup_changed");
    let mut endpoint = f.endpoint.clone();
    endpoint["label"] = json!("Concurrent owner change");
    f.state
        .save(
            "endpoints",
            endpoint,
            Some(f.endpoint["revision"].as_u64().unwrap()),
        )
        .unwrap();
    assert!(
        slack_setup::get_with(
            &f.state,
            f.endpoint["id"].as_str().unwrap(),
            "fixture-owner",
            &f.owner
        )
        .await
        .is_err()
    );
    assert_eq!(f.owner.data.lock().unwrap().calls.len(), before);
}
#[test]
fn generic_readiness_is_not_the_exact_native_start_receipt() {
    let request = Uuid::new_v4();
    let record = json!({"connector":{"id":"one"},"private":{"home":"/private/home","executable":"/private/home/bin/slack","native_start_request_id":request},"operation":{"id":Uuid::new_v4()}});
    assert!(
        slack_setup::validate_lifecycle(&json!({"ready":true,"state":"ready"}), &record).is_err()
    );
    let exact = lifecycle(&record, "one", json!(request), "ready");
    assert!(slack_setup::validate_lifecycle(&exact, &record).is_ok());
    for field in ["request_current", "launch_observed", "profile_matches"] {
        let mut changed = exact.clone();
        changed[field] = json!(false);
        assert!(
            slack_setup::validate_lifecycle(&changed, &record).is_err(),
            "{field}"
        );
    }
    let mut changed = exact.clone();
    changed["request"]["request_id"] = json!(Uuid::new_v4());
    assert!(slack_setup::validate_lifecycle(&changed, &record).is_err());
    let mut changed = exact.clone();
    changed["process_identity"] = json!({"pid":1234});
    assert!(slack_setup::validate_lifecycle(&changed, &record).is_err());
    let mut changed = exact.clone();
    changed["pid"] = json!(4321);
    assert!(slack_setup::validate_lifecycle(&changed, &record).is_err());
    let mut bound = record.clone();
    bound["private"]["native_service"] = json!({"name":"another"});
    assert!(slack_setup::validate_lifecycle(&exact, &bound).is_err());
}
#[tokio::test]
async fn reopening_authorization_returns_current_transient_command_without_reauthorizing() {
    let f = fixture(true);
    let p = prepare(&f).await;
    let a = act(&f, next(&p, "authorize")).await;
    let before = mutations(&f);
    let reopened = read(&f).await;
    assert_eq!(
        reopened["authorization_command"],
        a["authorization_command"]
    );
    assert!(
        reopened["authorization_command"]["expires_at"]
            .as_i64()
            .unwrap()
            > agent_observatory::state::now_ms()
    );
    assert_eq!(mutations(&f), before);
    let saved = f
        .state
        .read("slack_setup", f.endpoint["id"].as_str().unwrap())
        .unwrap()
        .to_string();
    assert!(!saved.contains("fixture-private-ticket"));
}
#[tokio::test]
async fn exact_known_confirmation_refusal_is_settled_without_auto_authorization() {
    for code in [
        "invalid_confirmation_format",
        "authorization_expired",
        "invalid_challenge",
    ] {
        let f = fixture(true);
        let p = prepare(&f).await;
        let a = act(&f, next(&p, "authorize")).await;
        f.owner.data.lock().unwrap().confirmation_refusal = Some(code);
        let mut body = next(&a, "confirm");
        body["challenge"] = json!("ONLY-THIS-CODE");
        let reply = act(&f, body).await;
        assert_eq!(reply["operation"]["state"], "complete");
        assert_eq!(reply["operation"]["uncertain"], false);
        assert_eq!(reply["confirmation_refusal"], code);
        assert_eq!(
            reply["capabilities"][if code == "invalid_confirmation_format" {
                "can_confirm"
            } else {
                "can_authorize"
            }],
            true
        );
        assert_eq!(
            mutations(&f).iter().filter(|s| *s == "authorize").count(),
            1
        );
        assert_eq!(mutations(&f).iter().filter(|s| *s == "confirm").count(), 1);
    }
}
#[tokio::test]
async fn stopped_saved_connector_resumes_only_on_explicit_request_and_keeps_configuration() {
    let f = fixture(true);
    let prepared = prepare(&f).await;
    let old = f
        .state
        .read("slack_setup", f.endpoint["id"].as_str().unwrap())
        .unwrap();
    let config = f.owner.data.lock().unwrap().config.clone();
    let before = mutations(&f);
    f.owner.data.lock().unwrap().native_inactive = true;
    let stopped = read(&f).await;
    assert_eq!(stopped["capabilities"]["can_resume"], true);
    assert_eq!(mutations(&f), before);
    let resumed = act(&f, next(&stopped, "resume")).await;
    assert_eq!(resumed["operation"]["id"], prepared["operation"]["id"]);
    assert_eq!(resumed["operation"]["state"], "complete");
    let new = f
        .state
        .read("slack_setup", f.endpoint["id"].as_str().unwrap())
        .unwrap();
    assert_ne!(
        old["private"]["native_start_request_id"],
        new["private"]["native_start_request_id"]
    );
    assert_eq!(f.owner.data.lock().unwrap().config, config);
    assert_eq!(mutations(&f).iter().filter(|s| *s == "setup").count(), 1);
    assert_eq!(mutations(&f).iter().filter(|s| *s == "start").count(), 2);
    assert_eq!(
        mutations(&f).iter().filter(|s| *s == "put_config").count(),
        1
    );
}
#[tokio::test]
async fn lost_resume_reply_reconciles_same_saved_start_without_second_dispatch() {
    let f = fixture(true);
    prepare(&f).await;
    f.owner.data.lock().unwrap().native_inactive = true;
    let stopped = read(&f).await;
    f.owner.data.lock().unwrap().fail_start = true;
    let lost = act(&f, next(&stopped, "resume")).await;
    assert_eq!(lost["operation"]["uncertain"], true);
    let later = read(&f).await;
    assert_eq!(later["operation"]["uncertain"], false);
    assert_ne!(later["operation"]["phase"], "bind");
    assert_eq!(later["capabilities"]["can_bind"], false);
    assert_eq!(mutations(&f).iter().filter(|s| *s == "start").count(), 2);
    assert_eq!(
        mutations(&f).iter().filter(|s| *s == "put_config").count(),
        1
    );
}
