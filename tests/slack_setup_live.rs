//! Opt-in real Linux connector composition. Only package selection and the
//! eligible, inert agent are fixtures. Linux consumer support stays unchanged.
//! Any failed/unknown step retains this fresh home; no cleanup runs on unwind.
#![cfg(target_os = "linux")]

use agent_observatory::{
    config::Config,
    error::{ApiError, ApiResult},
    slack_local::{self, Action, ApiAction},
    slack_package::{LockedFile, ReleaseDescriptor},
    slack_setup::{self, AgentBinding, Backend, LockedTarget},
    state::AppState,
};
use amplifier_native_platform as platform;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::Mutex,
    time::Duration,
};
use uuid::Uuid;

type ResultFuture<'a, T> = Pin<Box<dyn Future<Output = ApiResult<T>> + Send + 'a>>;
const ACTOR: &str = "local-composition-fixture-owner";

fn digest(path: &Path) -> String {
    format!("{:x}", Sha256::digest(std::fs::read(path).unwrap()))
}

struct ActualOwner {
    root: PathBuf,
    package: PathBuf,
    release: LockedTarget,
    events: Mutex<Vec<Value>>,
}
impl ActualOwner {
    fn record(&self, event: Value) {
        let mut events = self.events.lock().unwrap();
        events.push(event);
        platform::private_write(
            &self.root.join("events.json"),
            &serde_json::to_vec_pretty(&*events).unwrap(),
        )
        .unwrap();
    }
    fn count(&self, action: &str) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e["event"] == "bridge-dispatch" && e["action"] == action)
            .count()
    }
    fn mutations(&self) -> usize {
        ["Setup", "Start", "PutConfig"]
            .iter()
            .map(|action| self.count(action))
            .sum()
    }
}
fn action_name(action: Action) -> &'static str {
    match action {
        Action::Setup => "Setup",
        Action::Start { .. } => "Start",
        Action::Status => "Status",
        Action::Api(ApiAction::GetConfig) => "GetConfig",
        Action::Api(ApiAction::PutConfig) => "PutConfig",
        Action::Api(ApiAction::GetStatus) => "GetStatus",
        Action::Api(ApiAction::GetSetup) => "GetSetup",
        _ => panic!("External authorization is outside this fixture"),
    }
}
impl Backend for ActualOwner {
    fn agent_ready<'a>(&'a self, _: &'a AppState, _: &'a AgentBinding) -> ResultFuture<'a, ()> {
        // Eligibility/actor/path checks remain production code. No engine is
        // invoked: this one readiness boundary is an explicitly inert agent.
        Box::pin(async { Ok(()) })
    }
    fn release(&self) -> ApiResult<Option<LockedTarget>> {
        Ok(Some(self.release.clone()))
    }
    fn home(&self, id: Uuid) -> ApiResult<PathBuf> {
        Ok(self.root.join("connectors").join(id.to_string()))
    }
    fn stage<'a>(&'a self, _: &'a Path, selected: &'a LockedTarget) -> ResultFuture<'a, PathBuf> {
        Box::pin(async move {
            assert_eq!(selected.release, self.release.release);
            let executable = self.package.join("acp-slack-adapter");
            assert_eq!(
                digest(&executable),
                selected.release.files["acp-slack-adapter"].sha256
            );
            self.record(json!({"event":"package-seam","source":selected.release.source,"binary_sha256":digest(&executable)}));
            Ok(executable)
        })
    }
    fn call<'a>(
        &'a self,
        exe: &'a Path,
        hash: &'a str,
        home: &'a Path,
        action: Action,
        body: Option<&'a Value>,
    ) -> ResultFuture<'a, Value> {
        Box::pin(async move {
            let name = action_name(action);
            self.record(json!({"event":"bridge-dispatch","action":name,"request_id":match action {Action::Start { request_id }=>Some(request_id),_=>None}}));
            let result = slack_local::call(exe, hash, home, action, body).await;
            match &result {
                Ok(value) => {
                    self.record(json!({"event":"bridge-return","action":name,"value":value}))
                }
                Err(error) => self.record(
                    json!({"event":"bridge-error","action":name,"error":error.to_string()}),
                ),
            }
            result.map_err(|error| ApiError::bad(error.to_string()))
        })
    }
}

fn next(observed: &Value, action: &str) -> Value {
    json!({"action":action,"endpoint_revision":observed["endpoint_revision"],"operation_id":observed["operation"]["id"],"expected_revision":observed["revision"]})
}

async fn checked_control(owner: &ActualOwner, record: &Value, generation: &Value, action: &str) {
    assert!(matches!(action, "stop" | "retire"));
    let home = PathBuf::from(record["private"]["home"].as_str().unwrap());
    let exe = PathBuf::from(record["private"]["executable"].as_str().unwrap());
    assert_eq!(digest(&exe), record["private"]["sha256"]);
    let request = Uuid::new_v4();
    owner.record(json!({"event":"checked-control-intent","action":action,"request_id":request,"start_request_id":record["private"]["native_start_request_id"],"generation":generation}));
    let mut command = tokio::process::Command::new(&exe);
    command.env_clear();
    for name in [
        "HOME",
        "USER",
        "LOGNAME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "HOMEDRIVE",
        "HOMEPATH",
        "SYSTEMROOT",
        "WINDIR",
        "TEMP",
        "TMP",
        "TMPDIR",
        "LANG",
        "LC_ALL",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .args([
            "--home",
            home.to_str().unwrap(),
            &format!("native-{action}"),
            "--request-id",
            &request.to_string(),
        ])
        .current_dir(&home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Never repeat an uncertain control action or kill the managed application.
    let output = tokio::time::timeout(Duration::from_secs(70), command.output())
        .await
        .expect("Control timeout: exact fresh installation is retained")
        .expect("Control launch failed: exact fresh installation is retained");
    platform::private_create(
        &owner.root.join(format!("{request}.stdout")),
        &output.stdout,
    )
    .unwrap();
    platform::private_create(
        &owner.root.join(format!("{request}.stderr")),
        &output.stderr,
    )
    .unwrap();
    assert!(
        output.status.success(),
        "Control failed; preserve exact fresh installation"
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    owner.record(json!({"event":"checked-control-return","action":action,"value":value}));
    let phase = if action == "stop" {
        "inactive"
    } else {
        "retired"
    };
    assert_eq!(
        value["schema"],
        "com.microsoft.amplifier.slack-native-status/1"
    );
    assert_eq!(value["id"], record["connector"]["id"]);
    assert_eq!(value["home"], record["private"]["home"]);
    assert_eq!(value["executable"], record["private"]["executable"]);
    assert_eq!(value["service"], record["private"]["native_service"]);
    assert_eq!(value["state"], phase);
    assert_eq!(value["control_current"], true);
    assert_eq!(value["control"]["request_id"], request.to_string());
    assert_eq!(
        value["control"]["start_request_id"],
        record["private"]["native_start_request_id"]
    );
    assert_eq!(value["control"]["action"], action);
    assert_eq!(value["control"]["phase"], phase);
    assert_eq!(value["control"]["process_identity"], *generation);
    assert_eq!(value["control"]["process_absent"], true);
}

#[tokio::test]
#[ignore = "Requires exact owner-supplied self-contained connector and fresh evidence path; runs real owned Linux manager"]
async fn real_connector_prepare_bind_reopen_and_explicit_resume() {
    let root = PathBuf::from(
        std::env::var_os("OBSERVATORY_SLACK_LIVE_ROOT").expect("Set fresh absolute fixture root"),
    );
    let package = PathBuf::from(
        std::env::var_os("OBSERVATORY_SLACK_LIVE_PACKAGE").expect("Set exact corrected package"),
    );
    let expected = std::env::var("OBSERVATORY_SLACK_LIVE_BINARY_SHA256").unwrap();
    let source = std::env::var("OBSERVATORY_SLACK_LIVE_SOURCE").unwrap();
    let version = std::env::var("OBSERVATORY_SLACK_LIVE_VERSION").unwrap();
    assert!(root.is_absolute() && !root.exists());
    assert_eq!(source.len(), 40);
    assert_eq!(expected.len(), 64);
    let package = package.canonicalize().unwrap();
    let executable = package.join("acp-slack-adapter");
    assert_eq!(digest(&executable), expected);
    platform::private_create_dir(&root).unwrap();
    for child in ["observatory", "agents", "connectors"] {
        platform::private_create_dir(&root.join(child)).unwrap();
    }
    let obs_home = root.join("observatory");
    let config: Config = serde_json::from_value(json!({"id":Uuid::new_v4(),"owner":ACTOR,"bind":"127.0.0.1","port":0,"public_url":"","pam_service":"unused","tls_cert":"","tls_key":"","tls_ca":null})).unwrap();
    config.save(&obs_home).unwrap();
    let state = AppState::open(obs_home.clone()).unwrap();
    let endpoint_id = Uuid::new_v4().to_string();
    let host_id = Uuid::new_v4().to_string();
    let agent_home = root.join("agents").join(&endpoint_id);
    platform::private_create_dir(&agent_home).unwrap();
    platform::private_create_dir(&agent_home.join("workspace")).unwrap();
    platform::private_create(&agent_home.join("config.json"), br#"{"config":{"backend":{"working_directory":"${AGENT_HOME}/workspace","allowed_working_roots":["${AGENT_HOME}/workspace"]}}}"#).unwrap();
    platform::private_create(
        &agent_home.join("preserved-settings-and-session"),
        b"fixture settings and prior session remain unchanged",
    )
    .unwrap();
    let sentinel = digest(&agent_home.join("preserved-settings-and-session"));
    state.save("hosts", json!({"id":host_id,"native_local":true,"transport":"local","install_roots":[root.join("agents")]}), Some(0)).unwrap();
    state.save("jobs", json!({"id":endpoint_id,"actor":ACTOR,"status":"completed","endpoint_id":endpoint_id,"plan":{"host_id":host_id,"home":agent_home,"recipe_id":"bundled-instance"}}), Some(0)).unwrap();
    let endpoint = state.save("endpoints", json!({"id":endpoint_id,"label":"Isolated fixture agent","kind":"agent","host_id":host_id,"acp":{"transport":"stdio","command":agent_home.join("bin/agent-instance"),"args":["acp","--home",agent_home],"cwd":agent_home,"session_cwd":agent_home.join("workspace")},"management":{"provider":"agent-instance","access":"manage","expected_id":Uuid::new_v4()}}), Some(0)).unwrap();
    let owner = ActualOwner {
        root,
        package: package.clone(),
        events: Mutex::new(Vec::new()),
        release: LockedTarget {
            url: None,
            release: ReleaseDescriptor {
                schema: 1,
                source,
                version,
                target: "local-linux-fixture-package-selection-seam".into(),
                archive_size: 0,
                archive_sha256: "0".repeat(64),
                entry: "acp-slack-adapter".into(),
                native_bundle_sha256: digest(&package.join("native-bundle.json")),
                signed_derivative: None,
                files: BTreeMap::from([(
                    "acp-slack-adapter".into(),
                    LockedFile {
                        size: std::fs::metadata(&executable).unwrap().len(),
                        sha256: expected,
                        mode: 0o755,
                    },
                )]),
            },
        },
    };
    owner.record(json!({"event":"scope","package_selection":"explicit local fixture seam, not a qualified Linux consumer lock","agent_readiness":"inert eligible fixture, no provider or engine execution","connector":"actual executable, OS manager, bridge and API","cleanup_on_unknown":false}));
    let request = Uuid::new_v4();
    let body = json!({"action":"prepare","endpoint_revision":endpoint["revision"],"request_id":request,"expected_revision":0});
    let prepared = slack_setup::command_with(&state, &endpoint_id, ACTOR, body.clone(), &owner)
        .await
        .unwrap();
    owner.record(json!({"event":"prepared","value":prepared}));
    assert_eq!(
        prepared["operation"]["state"], "complete",
        "Preserve unknown prepare; do not retry"
    );
    assert_eq!(prepared["operation"]["uncertain"], false);
    assert_eq!(prepared["connector"]["binding_verified"], true);
    assert_eq!(prepared["capabilities"]["can_authorize"], true);
    assert_eq!(owner.count("Setup"), 1);
    assert_eq!(owner.count("Start"), 1);
    assert_eq!(owner.count("PutConfig"), 1);
    let before = owner.mutations();
    let duplicate = slack_setup::command_with(&state, &endpoint_id, ACTOR, body.clone(), &owner)
        .await
        .unwrap();
    assert_eq!(duplicate["operation"]["id"], prepared["operation"]["id"]);
    assert_eq!(
        owner.mutations(),
        before,
        "Same intent must not provision again"
    );
    assert!(
        slack_setup::command_with(&state, &endpoint_id, "different-owner", body, &owner)
            .await
            .is_err()
    );
    assert_eq!(owner.mutations(), before);
    let record = state.read("slack_setup", &endpoint_id).unwrap();
    let home = PathBuf::from(record["private"]["home"].as_str().unwrap());
    let config_before = std::fs::read(home.join("adapter.json")).unwrap();
    checked_control(
        &owner,
        &record,
        &record["private"]["native_process_identity"],
        "stop",
    )
    .await;
    drop(state);
    let reopened = AppState::open(obs_home).unwrap();
    let saved = slack_setup::get_with(&reopened, &endpoint_id, ACTOR, &owner)
        .await
        .unwrap();
    owner.record(json!({"event":"reopened-stopped","value":saved}));
    assert_eq!(saved["capabilities"]["can_resume"], true);
    assert_eq!(
        owner.mutations(),
        before,
        "Reopen/read cannot start a stopped connector"
    );
    let resumed = slack_setup::command_with(
        &reopened,
        &endpoint_id,
        ACTOR,
        next(&saved, "resume"),
        &owner,
    )
    .await
    .unwrap();
    owner.record(json!({"event":"resumed","value":resumed}));
    assert_eq!(resumed["operation"]["id"], prepared["operation"]["id"]);
    assert_eq!(resumed["operation"]["state"], "complete");
    assert_eq!(resumed["connector"]["binding_verified"], true);
    let new_record = reopened.read("slack_setup", &endpoint_id).unwrap();
    assert_ne!(
        new_record["private"]["native_start_request_id"],
        record["private"]["native_start_request_id"]
    );
    assert_ne!(
        new_record["private"]["native_process_identity"],
        record["private"]["native_process_identity"]
    );
    assert_eq!(
        new_record["private"]["native_service"],
        record["private"]["native_service"]
    );
    assert_eq!(
        std::fs::read(home.join("adapter.json")).unwrap(),
        config_before
    );
    assert_eq!(reopened.read("endpoints", &endpoint_id).unwrap(), endpoint);
    assert_eq!(
        digest(&agent_home.join("preserved-settings-and-session")),
        sentinel
    );
    assert_eq!(owner.count("Setup"), 1);
    assert_eq!(owner.count("Start"), 2);
    assert_eq!(owner.count("PutConfig"), 1);
    checked_control(
        &owner,
        &new_record,
        &new_record["private"]["native_process_identity"],
        "stop",
    )
    .await;
    checked_control(
        &owner,
        &new_record,
        &new_record["private"]["native_process_identity"],
        "retire",
    )
    .await;
    owner.record(json!({"event":"complete","cleanup":"checked fixed profile retired; fresh home and evidence retained","external_authorization":false,"model_calls":0,"same_intent_replayed":false}));
}
