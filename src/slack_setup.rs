//! Durable client of one new, installation-owned Slack connector. No replay,
//! credential store, generic subprocess interface or existing-home adoption.
use crate::{
    config,
    error::{ApiError, ApiResult},
    slack_local::{self, Action, ApiAction},
    slack_package::{self, ReleaseDescriptor},
    state::{AppState, now_ms},
};
use amplifier_native_platform as platform;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

const KIND: &str = "slack_setup";
const APPLICATION: platform::Application = platform::Application {
    display_name: "Slack Connector",
    startup_description: "Constellation Slack Connector",
    executable_name: "acp-slack-adapter",
    startup_prefix: "acp-slack-adapter",
    data_directory: "acp-slack-adapter",
    background_command: "serve",
};
type FutureResult<'a, T> = Pin<Box<dyn Future<Output = ApiResult<T>> + Send + 'a>>;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LockedTarget {
    pub url: String,
    pub release: ReleaseDescriptor,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lock {
    schema: u8,
    targets: BTreeMap<String, LockedTarget>,
}

fn target() -> Option<String> {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        _ => return None,
    };
    let os = if cfg!(windows) {
        "pc-windows-msvc"
    } else if cfg!(target_os = "macos") {
        "apple-darwin"
    } else {
        return None;
    };
    Some(format!("{arch}-{os}"))
}
pub(crate) fn selected_release() -> ApiResult<Option<LockedTarget>> {
    let lock: Lock = serde_json::from_str(include_str!("../scripts/slack-bundle-lock.json"))
        .map_err(|_| ApiError::bad("Slack package qualification is unavailable"))?;
    if lock.schema != 1 {
        return Err(ApiError::bad("Slack package qualification is unavailable"));
    }
    let Some(target) = target() else {
        return Ok(None);
    };
    let Some(selected) = lock.targets.get(&target) else {
        return Ok(None);
    };
    selected
        .release
        .validate_for_target(&target)
        .map_err(|_| ApiError::bad("Slack package qualification does not match this computer"))?;
    validate_url(selected)?;
    Ok(Some(selected.clone()))
}
fn validate_url(selected: &LockedTarget) -> ApiResult<()> {
    let url = reqwest::Url::parse(&selected.url)
        .map_err(|_| ApiError::bad("Invalid locked Slack release address"))?;
    let prefix = format!(
        "/microsoft/amplifier-acp-slack/releases/download/v{}/",
        selected.release.version
    );
    let member = url.path().strip_prefix(&prefix).unwrap_or("");
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || member.is_empty()
        || !member
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(ApiError::bad(
            "Use the reviewed official Slack release address",
        ));
    }
    Ok(())
}

/// Finite seams for deterministic qualification. Production always uses the
/// fixed archive lock and slack_local's closed command set.
pub trait Backend: Sync {
    fn agent_ready<'a>(
        &'a self,
        state: &'a AppState,
        agent: &'a AgentBinding,
    ) -> FutureResult<'a, ()> {
        Box::pin(async move {
            let observed = crate::provider::inspect(state, &agent.endpoint).await?;
            if observed["instance_id"] != agent.instance_id
                || observed["status"]["setup_complete"] != true
                || observed["status"]["enabled"] != true
            {
                return Err(ApiError::bad(
                    "Finish this agent's setup and start it before connecting Slack",
                ));
            }
            Ok(())
        })
    }
    fn release(&self) -> ApiResult<Option<LockedTarget>>;
    fn home(&self, id: Uuid) -> ApiResult<PathBuf>;
    fn stage<'a>(&'a self, cache: &'a Path, release: &'a LockedTarget)
    -> FutureResult<'a, PathBuf>;
    fn call<'a>(
        &'a self,
        exe: &'a Path,
        hash: &'a str,
        home: &'a Path,
        action: Action,
        body: Option<&'a Value>,
    ) -> FutureResult<'a, Value>;
}
pub struct LocalBackend {
    home: PathBuf,
}
impl Backend for LocalBackend {
    fn release(&self) -> ApiResult<Option<LockedTarget>> {
        selected_release()
    }
    fn home(&self, id: Uuid) -> ApiResult<PathBuf> {
        Ok(platform::default_home(
            &APPLICATION,
            &format!("observatory-{id}"),
        )?)
    }
    fn stage<'a>(
        &'a self,
        cache: &'a Path,
        selected: &'a LockedTarget,
    ) -> FutureResult<'a, PathBuf> {
        Box::pin(async move {
            validate_url(selected)?;
            let home = self.home.clone();
            let expected = selected.release.clone();
            let retained = tokio::task::spawn_blocking(move || {
                crate::slack_support::installed_archive(&home, &expected)
            })
            .await
            .map_err(|_| ApiError::bad("Bundled Slack package verification was not confirmed"))??;
            if std::fs::symlink_metadata(cache).is_ok() {
                return Err(ApiError::bad(
                    "This package cache already exists; it was preserved",
                ));
            }
            let parent = cache
                .parent()
                .ok_or_else(|| ApiError::bad("Missing private package cache"))?;
            if parent.exists() {
                platform::validate_private_directory(parent)?;
            } else {
                platform::private_create_dir(parent)?;
            }
            platform::private_create_dir(cache)?;
            let archive = match retained {
                Some(archive) => archive,
                None => {
                    let archive = cache.join("download.zip");
                    download_archive(&archive, selected).await?;
                    archive
                }
            };
            let destination = cache.join("package");
            let release = selected.release.clone();
            tokio::task::spawn_blocking(move || {
                slack_package::stage_archive(&archive, &destination, &release).map(|p| p.executable)
            })
            .await
            .map_err(|_| ApiError::bad("Slack package staging was not confirmed"))?
            .map_err(Into::into)
        })
    }
    fn call<'a>(
        &'a self,
        exe: &'a Path,
        hash: &'a str,
        home: &'a Path,
        action: Action,
        body: Option<&'a Value>,
    ) -> FutureResult<'a, Value> {
        Box::pin(async move {
            slack_local::call(exe,hash,home,action,body).await.map_err(|_|ApiError(StatusCode::BAD_GATEWAY,"The connector did not confirm this step. Its installation and saved request were preserved.".into()))
        })
    }
}

async fn download_archive(archive: &Path, selected: &LockedTarget) -> ApiResult<()> {
    config::private_create(archive, &[])?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() > 5 {
                return attempt.error("Too many release redirects");
            }
            let url = attempt.url();
            if url.scheme() == "https"
                && url.username().is_empty()
                && url.password().is_none()
                && matches!(
                    url.host_str(),
                    Some(
                        "github.com"
                            | "release-assets.githubusercontent.com"
                            | "objects.githubusercontent.com"
                    )
                )
            {
                attempt.follow()
            } else {
                attempt.error("Unexpected release host")
            }
        }))
        .build()
        .map_err(|_| ApiError::bad("Could not prepare the reviewed package download"))?;
    let mut response = client
        .get(&selected.url)
        .send()
        .await
        .map_err(|_| ApiError::bad("The Slack package download was not completed"))?;
    if !response.status().is_success() {
        return Err(ApiError::bad("The reviewed Slack package was unavailable"));
    }
    let mut file = tokio::fs::OpenOptions::new()
        .append(true)
        .open(archive)
        .await?;
    let mut size = 0u64;
    let mut hash = Sha256::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ApiError::bad("The Slack package download was interrupted"))?
    {
        size = size
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| ApiError::bad("Slack package exceeds its reviewed size"))?;
        if size > selected.release.archive_size {
            return Err(ApiError::bad("Slack package exceeds its reviewed size"));
        }
        hash.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.sync_all().await?;
    drop(file);
    if size != selected.release.archive_size
        || format!("{:x}", hash.finalize()) != selected.release.archive_sha256
    {
        return Err(ApiError::bad(
            "Slack package bytes did not match the reviewed release",
        ));
    }

    Ok(())
}

#[derive(Clone, Debug)]
pub struct AgentBinding {
    pub endpoint: Value,
    pub command: PathBuf,
    pub args: Value,
    pub cwd: PathBuf,
    pub instance_id: String,
}
/// A browser may select an endpoint; it may not supply any executable/path/env.
pub fn eligible_agent(state: &AppState, id: &str, actor: &str) -> ApiResult<AgentBinding> {
    let endpoint = state.read("endpoints", id)?;
    let job = state.read("jobs", id).map_err(|_| {
        ApiError::bad("Slack setup needs an agent created by this Observatory installation")
    })?;
    let host = state.read("hosts", endpoint["host_id"].as_str().unwrap_or(""))?;
    let home = PathBuf::from(job["plan"]["home"].as_str().unwrap_or(""));
    let expected_home = Path::new(host["install_roots"][0].as_str().unwrap_or("")).join(id);
    let command = home.join("bin").join(if cfg!(windows) {
        "agent-instance.exe"
    } else {
        "agent-instance"
    });
    let args = json!(["acp", "--home", home]);
    let cwd = PathBuf::from(endpoint["acp"]["session_cwd"].as_str().unwrap_or(""));
    let instance_id = endpoint["management"]["expected_id"].as_str().unwrap_or("");
    if Uuid::parse_str(id).is_err()
        || Uuid::parse_str(instance_id).is_err()
        || endpoint["kind"] != "agent"
        || endpoint["management"]["provider"] != "agent-instance"
        || endpoint["management"]["access"] != "manage"
        || job["status"] != "completed"
        || job["actor"] != actor
        || job["endpoint_id"] != id
        || job["plan"]["recipe_id"] != "bundled-instance"
        || job["plan"]["host_id"] != endpoint["host_id"]
        || host["native_local"] != true
        || host["transport"] != "local"
        || home != expected_home
        || !home.is_absolute()
        || endpoint["acp"]["transport"] != "stdio"
        || endpoint["acp"]["command"].as_str() != command.to_str()
        || endpoint["acp"]["args"] != args
        || endpoint["acp"]["cwd"].as_str() != home.to_str()
        || !cwd.is_absolute()
        || crate::instance_package::session_directory(&home)
            .ok()
            .as_ref()
            != Some(&cwd)
        || endpoint["acp"]
            .get("env")
            .is_some_and(|v| !v.is_null() && v != &json!({}))
    {
        return Err(ApiError::bad(
            "This endpoint is not the unchanged local agent created for this setup",
        ));
    }
    Ok(AgentBinding {
        instance_id: instance_id.to_owned(),
        endpoint,
        command,
        args,
        cwd,
    })
}
fn read_record(state: &AppState, id: &str) -> ApiResult<Option<Value>> {
    match state.read(KIND, id) {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.0 == StatusCode::NOT_FOUND => Ok(None),
        Err(e) => Err(e),
    }
}
fn ensure_owner(record: &Value, agent: &AgentBinding, actor: &str) -> ApiResult<()> {
    if record["actor"] != actor {
        return Err(ApiError::forbidden());
    }
    if record["endpoint_revision"] != agent.endpoint["revision"]
        || record["agent"]["instance_id"] != agent.instance_id
        || record["agent"]["command"].as_str() != agent.command.to_str()
        || record["agent"]["args"] != agent.args
        || record["agent"]["cwd"].as_str() != agent.cwd.to_str()
    {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "The selected agent changed. Its existing Slack setup was preserved.".into(),
        ));
    }
    Ok(())
}
fn save(state: &AppState, record: &Value) -> ApiResult<Value> {
    state.save(
        KIND,
        record.clone(),
        record["revision"].as_u64().or(Some(0)),
    )
}
fn current_agent(state: &AppState, record: &Value) -> ApiResult<AgentBinding> {
    let agent = eligible_agent(
        state,
        record["endpoint_id"].as_str().unwrap_or(""),
        record["actor"].as_str().unwrap_or(""),
    )?;
    ensure_owner(record, &agent, record["actor"].as_str().unwrap_or(""))?;
    Ok(agent)
}
fn phase(state: &AppState, record: &mut Value, name: &str) -> ApiResult<()> {
    current_agent(state, record)?;
    record["operation"]["phase"] = json!(name);
    record["operation"]["state"] = json!("running");
    record["operation"]["uncertain"] = json!(false);
    record["private"]["final_reply_received"] = json!(false);
    record["confirmation_refusal"] = Value::Null;
    *record = save(state, record)?;
    Ok(())
}
fn mark_unknown(state: &AppState, record: &mut Value) -> ApiResult<()> {
    if record["private"]["final_reply_received"] == true {
        record["operation"]["state"] = json!("complete");
        record["operation"]["uncertain"] = json!(false);
        observation_unavailable(record);
    } else {
        record["operation"]["state"] = json!("unknown");
        record["operation"]["uncertain"] = json!(true);
        record["message"] = json!(
            "This step was not confirmed. Its saved request and installation were preserved; it will not be repeated automatically."
        );
    }
    *record = save(state, record)?;
    Ok(())
}
fn observation_unavailable(record: &mut Value) {
    record["connector"]["binding_verified"] = json!(false);
    record["observation_error"] = json!(true);
    record["message"] = json!(
        "The current connector binding could not be verified. Setup actions are unavailable; the saved action outcome is unchanged."
    );
}
fn active_key(id: &str) -> String {
    format!("slack_setup:{id}")
}
struct Active<'a> {
    state: &'a AppState,
    key: String,
}
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.state.progress.lock().unwrap().remove(&self.key);
    }
}
fn activate<'a>(state: &'a AppState, id: &str) -> Active<'a> {
    let key = active_key(id);
    state
        .progress
        .lock()
        .unwrap()
        .insert(key.clone(), json!(true));
    Active { state, key }
}
fn bounded(value: &Value, key: &str, max: usize) -> Value {
    value[key]
        .as_str()
        .filter(|s| s.len() <= max && !s.chars().any(char::is_control))
        .map(|s| json!(s))
        .unwrap_or(Value::Null)
}
fn workspace(value: &Value) -> Value {
    let id = value["team_id"].as_str().unwrap_or("");
    if id.len() < 3
        || id.len() > 32
        || !id.starts_with(['T', 'E'])
        || !id
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    {
        return Value::Null;
    }
    json!({"team_id":id,"name":bounded(value,"name",120),"domain":bounded(value,"domain",120)})
}
fn public_setup(value: &Value) -> Value {
    json!({"authorization":bounded(value,"authorization",40),"authorization_pending":value["authorization_pending"]==true,"intended_workspace":workspace(&value["intended_workspace"]),"authorized_workspace":workspace(&value["authorized_workspace"]),"workspace_mismatch":value["workspace_mismatch"]==true,"next_action":bounded(value,"next_action",40)})
}
pub fn projection(record: Option<&Value>, endpoint: &Value, available: bool) -> Value {
    let mut out = json!({"schema":1,"endpoint_id":endpoint["id"],"endpoint_revision":endpoint["revision"],"revision":0,"operation":null,"connector":{},"setup":{},"capabilities":{"can_prepare":available,"can_claim":false,"can_bind":false,"can_resume":false,"can_authorize":false,"can_confirm":false,"can_connect":false,"can_discard":false},"message":if available{"Prepare a new private Slack connector for this agent."}else{"A reviewed Slack package and a configured, running agent are required. Existing installations are unchanged."}});
    let Some(r) = record else { return out };
    out["revision"] = r["revision"].clone();
    out["endpoint_revision"] = r["endpoint_revision"].clone();
    out["operation"] = json!({"id":r["operation"]["id"],"request_id":r["operation"]["request_id"],"phase":bounded(&r["operation"],"phase",80),"state":bounded(&r["operation"],"state",80),"uncertain":r["operation"]["uncertain"]==true});
    out["connector"] = json!({"id":bounded(&r["connector"],"id",200),"label":bounded(&r["connector"],"label",200),"endpoint_id":r["endpoint_id"],"binding_verified":r["connector"]["binding_verified"]==true,"account_ready":r["connector"]["account_ready"]==true,"connection_ready":r["connector"]["connection_ready"]==true,"connection_running":r["connector"]["connection_running"]==true,"setup_complete":r["connector"]["setup_complete"]==true,"native_inactive":r["private"]["native_inactive"]==true});
    out["setup"] = public_setup(&r["setup"]);
    out["message"] = bounded(r, "message", 1024);
    out["confirmation_refusal"] = bounded(r, "confirmation_refusal", 80);
    out["observation"] = json!({"verified":r["connector"]["binding_verified"]==true,"unavailable":r["observation_error"]==true,"observed_at":r["observed_at"]});
    out["capabilities"]["can_prepare"] = json!(false);
    out["capabilities"]["can_bind"] = json!(
        r["operation"]["state"] == "complete"
            && r["operation"]["uncertain"] != true
            && r["operation"]["phase"] == "bind"
            && r["private"]["lifecycle_verified"] == true
    );
    out["capabilities"]["can_resume"] = json!(
        r["operation"]["state"] == "complete"
            && r["operation"]["uncertain"] != true
            && r["private"]["native_inactive"] == true
    );
    let settled = r["operation"]["state"] == "complete"
        && r["operation"]["uncertain"] != true
        && r["connector"]["binding_verified"] == true;
    if settled && r["connector"]["setup_complete"] != true {
        let next = r["setup"]["next_action"].as_str().unwrap_or("");
        out["capabilities"]["can_authorize"] = json!(next == "authorize");
        out["capabilities"]["can_confirm"] =
            json!(next == "confirm" && r["setup"]["authorization_pending"] == true);
        out["capabilities"]["can_connect"] = json!(
            next == "connect"
                && !workspace(&r["setup"]["authorized_workspace"]).is_null()
                && r["setup"]["workspace_mismatch"] != true
        );
        out["capabilities"]["can_discard"] =
            json!(next == "connect" && r["connector"]["connection_ready"] != true);
    }
    out
}
fn command_ephemeral(value: &Value) -> Option<Value> {
    let command = value["command"].as_str()?;
    let ticket = command.strip_prefix("/slackauthticket ")?;
    let seconds = value["expires_at"].as_f64()?;
    let millis = (seconds * 1000.0) as i64;
    (ticket.len() <= 1024
        && !ticket.is_empty()
        && ticket
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_+/=.-".contains(&b))
        && seconds.is_finite()
        && millis > now_ms()
        && millis <= now_ms() + 660_000)
        .then(|| json!({"command":command,"expires_at":millis}))
}
fn recorded_paths(record: &Value) -> ApiResult<(PathBuf, PathBuf, String)> {
    let home = PathBuf::from(
        record["private"]["home"]
            .as_str()
            .ok_or_else(|| ApiError::bad("Missing retained connector home"))?,
    );
    let exe = PathBuf::from(
        record["private"]["executable"]
            .as_str()
            .ok_or_else(|| ApiError::bad("Missing retained connector executable"))?,
    );
    let hash = record["private"]["sha256"]
        .as_str()
        .ok_or_else(|| ApiError::bad("Missing retained connector identity"))?
        .to_owned();
    Ok((home, exe, hash))
}
fn verify_binding(record: &Value, config: &Value) -> bool {
    config["id"] == record["connector"]["id"]
        && config["acp"]["command"] == record["agent"]["command"]
        && config["acp"]["args"] == record["agent"]["args"]
        && config["acp"]["cwd"] == record["agent"]["cwd"]
        && config["acp"]["env"] == json!({})
        && config["acp"]["secret_env"] == json!({})
}
async fn observations<B: Backend>(backend: &B, record: &mut Value) -> ApiResult<Option<Value>> {
    let (home, exe, hash) = recorded_paths(record)?;
    let cfg = backend
        .call(&exe, &hash, &home, Action::Api(ApiAction::GetConfig), None)
        .await?;
    let status = backend
        .call(&exe, &hash, &home, Action::Api(ApiAction::GetStatus), None)
        .await?;
    if !verify_binding(record, &cfg) || status["id"] != record["connector"]["id"] {
        return Err(ApiError::bad(
            "The connector binding changed. Existing state was preserved",
        ));
    }
    let setup = backend
        .call(&exe, &hash, &home, Action::Api(ApiAction::GetSetup), None)
        .await?;
    record["connector"]["binding_verified"] = json!(true);
    record["private"]["binding_initialized"] = json!(true);
    record["connector"]["setup_complete"] = json!(
        cfg["setup_complete"] == true
            && status["setup_complete"] == true
            && setup["setup_complete"] == true
    );
    record["connector"]["connection_ready"] = json!(setup["connection_ready"] == true);
    record["connector"]["connection_running"] =
        json!(setup["connection_running"] == true && status["enabled"] == true);
    record["setup"] = public_setup(&setup);
    record["observed_at"] = json!(now_ms());
    record["observation_error"] = json!(false);
    Ok(command_ephemeral(&setup["authorization_command"]))
}

fn config_digest(config: &Value) -> String {
    let mut value = config.clone();
    if let Some(object) = value.as_object_mut() {
        object.remove("revision");
    }
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&value).unwrap_or_default())
    )
}

/// Saved observations only: never invokes a connector or claims fresh liveness.
pub fn list_saved(state: &AppState, actor: &str) -> ApiResult<Vec<Value>> {
    let mut items = vec![];
    for record in state.list(KIND)? {
        if record["actor"] != actor {
            continue;
        }
        let Ok(agent) = eligible_agent(state, record["endpoint_id"].as_str().unwrap_or(""), actor)
        else {
            continue;
        };
        if ensure_owner(&record, &agent, actor).is_err() {
            continue;
        }
        let mut value = projection(Some(&record), &agent.endpoint, false);
        value["capabilities"] = json!({});
        value["saved_observation_at"] = record["observed_at"].clone();
        value["observation_fresh"] = json!(false);
        items.push(value);
    }
    Ok(items)
}
pub async fn get(state: &AppState, id: &str, actor: &str) -> ApiResult<Value> {
    get_with(
        state,
        id,
        actor,
        &LocalBackend {
            home: state.home.clone(),
        },
    )
    .await
}
pub async fn get_with<B: Backend>(
    state: &AppState,
    id: &str,
    actor: &str,
    backend: &B,
) -> ApiResult<Value> {
    let agent = eligible_agent(state, id, actor)?;
    let available =
        backend.release()?.is_some() && backend.agent_ready(state, &agent).await.is_ok();
    let mut record = {
        let _gate = state.provision_gate.lock().unwrap();
        let Some(mut record) = read_record(state, id)? else {
            return Ok(projection(None, &agent.endpoint, available));
        };
        ensure_owner(&record, &agent, actor)?;
        if state.progress.lock().unwrap().contains_key(&active_key(id)) {
            return Ok(projection(Some(&record), &agent.endpoint, false));
        }
        if record["operation"]["state"] == "running" {
            mark_unknown(state, &mut record)?;
        }
        record
    };
    let mut transient = None;
    if record["operation"]["uncertain"] == true
        && record["operation"]["phase"] == "native-start"
        && let Ok((home, exe, hash)) = recorded_paths(&record)
        && let Ok(value) = backend.call(&exe, &hash, &home, Action::Status, None).await
        && validate_lifecycle(&value, &record).is_ok()
    {
        record["private"]["lifecycle_verified"] = json!(true);
        record["operation"]["state"] = json!("complete");
        record["operation"]["uncertain"] = json!(false);
        record["operation"]["phase"] = json!(if record["private"]["binding_initialized"] == true {
            "resumed"
        } else {
            "bind"
        });
        retain_lifecycle(&mut record, &value);
        record["message"] = json!(
            "The exact saved startup request is proved ready. Finish connecting the agent explicitly; startup was not repeated."
        );
        record = save(state, &record)?;
    }
    if record["operation"]["uncertain"] == true
        && record["operation"]["phase"] == "bind-agent"
        && let Ok((home, exe, hash)) = recorded_paths(&record)
        && let Ok(cfg) = backend
            .call(&exe, &hash, &home, Action::Api(ApiAction::GetConfig), None)
            .await
        && verify_binding(&record, &cfg)
        && record["private"]["binding_config_sha256"].as_str() == Some(config_digest(&cfg).as_str())
        && observations(backend, &mut record).await.is_ok()
    {
        record["operation"]["state"] = json!("complete");
        record["operation"]["uncertain"] = json!(false);
        record["message"] = json!(
            "The exact requested agent binding is saved. The earlier reply remains unreceived; no write was repeated."
        );
        record = save(state, &record)?;
    }
    // A generic ready observation never acknowledges or repeats a lost action.
    // Only already bound installations can supply read-only refreshes.
    if record["private"]["binding_initialized"] == true {
        record["private"]["native_inactive"] = json!(false);
        let native = if let Ok((home, exe, hash)) = recorded_paths(&record) {
            backend
                .call(&exe, &hash, &home, Action::Status, None)
                .await
                .ok()
        } else {
            None
        };
        if native
            .as_ref()
            .is_some_and(|v| inactive_lifecycle(v, &record))
        {
            record["private"]["native_inactive"] = json!(true);
            record["connector"]["binding_verified"] = json!(false);
            record["connector"]["connection_running"] = json!(false);
            record["observation_error"] = json!(false);
            record["message"] = json!(
                "This saved connector is stopped. Start it explicitly to continue; its settings are preserved."
            );
        } else if native
            .as_ref()
            .is_some_and(|v| validate_lifecycle(v, &record).is_ok())
        {
            match observations(backend, &mut record).await {
                Ok(value) => transient = value,
                Err(_) => observation_unavailable(&mut record),
            }
        } else {
            observation_unavailable(&mut record);
        }
        record = save(state, &record)?;
    }
    let mut out = projection(Some(&record), &agent.endpoint, false);
    if let Some(value) = transient {
        out["authorization_command"] = value;
    }
    Ok(out)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    action: String,
    endpoint_revision: u64,
    expected_revision: u64,
    request_id: Option<Uuid>,
    operation_id: Option<Uuid>,
    challenge: Option<String>,
    team_id: Option<String>,
}
#[derive(Clone, Copy)]
enum Refusal {
    EndpointChanged,
    SetupChanged,
    InvalidInput,
    PackageUnavailable,
    ExistingHome,
    NotPrepared,
    StepUnavailable,
}
// This marker is emitted only at the explicit pre-intent branches below. It is
// never made by translating a generic error or a failed durable save. The UI
// must match every identity to its pending request before clearing that request.
fn refused(id: &str, command: &Command, reason: Refusal) -> Value {
    let (code, message) = match reason {
        Refusal::EndpointChanged => (
            "endpoint_changed",
            "The selected agent changed. Refresh it before continuing.",
        ),
        Refusal::SetupChanged => (
            "setup_changed",
            "Saved Slack setup changed. Read its current state before continuing.",
        ),
        Refusal::InvalidInput => (
            "invalid_input",
            "Use the offered setup step and its required fields.",
        ),
        Refusal::PackageUnavailable => (
            "package_unavailable",
            "A reviewed Slack package is not available for this computer.",
        ),
        Refusal::ExistingHome => (
            "existing_home",
            "The proposed new Slack home already exists or is invalid. It was preserved.",
        ),
        Refusal::NotPrepared => (
            "not_prepared",
            "Prepare this agent's Slack connector before continuing.",
        ),
        Refusal::StepUnavailable => (
            "step_unavailable",
            "This step is not available in the current saved setup.",
        ),
    };
    json!({"schema":1,"kind":"slack_setup_refusal","refused_before_intent":true,"endpoint_id":id,"endpoint_revision":command.endpoint_revision,"action":command.action,"request_id":command.request_id,"operation_id":command.operation_id,"expected_revision":command.expected_revision,"code":code,"message":message})
}
pub async fn command(state: &AppState, id: &str, actor: &str, body: Value) -> ApiResult<Value> {
    command_with(
        state,
        id,
        actor,
        body,
        &LocalBackend {
            home: state.home.clone(),
        },
    )
    .await
}
pub async fn command_with<B: Backend>(
    state: &AppState,
    id: &str,
    actor: &str,
    body: Value,
    backend: &B,
) -> ApiResult<Value> {
    let command: Command = serde_json::from_value(body)
        .map_err(|_| ApiError::bad("Use the offered Slack setup step and saved revision"))?;
    if ![
        "prepare",
        "bind",
        "resume",
        "authorize",
        "confirm",
        "connect",
        "discard",
    ]
    .contains(&command.action.as_str())
    {
        return Err(ApiError::bad("Choose an offered Slack setup step"));
    }
    let agent = eligible_agent(state, id, actor)?;
    if agent.endpoint["revision"].as_u64() != Some(command.endpoint_revision) {
        return Ok(refused(id, &command, Refusal::EndpointChanged));
    }
    if command.action == "prepare"
        && let Some(record) = read_record(state, id)?
    {
        ensure_owner(&record, &agent, actor)?;
        return Ok(projection(Some(&record), &agent.endpoint, false));
    }
    backend.agent_ready(state, &agent).await?;
    let current = eligible_agent(state, id, actor)?;
    if current.endpoint["revision"] != agent.endpoint["revision"] {
        return Ok(refused(id, &command, Refusal::EndpointChanged));
    }
    let (mut record, selected, active) = {
        let _gate = state.provision_gate.lock().unwrap();
        let existing = read_record(state, id)?;
        if let Some(record) = &existing {
            ensure_owner(record, &agent, actor)?;
        }
        if command.action == "prepare" {
            if let Some(record) = existing {
                return Ok(projection(Some(&record), &agent.endpoint, false));
            }
            if command.expected_revision != 0
                || command.operation_id.is_some()
                || command.challenge.is_some()
                || command.team_id.is_some()
            {
                return Ok(refused(id, &command, Refusal::InvalidInput));
            }
            let Some(request) = command.request_id else {
                return Ok(refused(id, &command, Refusal::InvalidInput));
            };
            let Some(selected) = backend.release()? else {
                return Ok(refused(id, &command, Refusal::PackageUnavailable));
            };
            let home = backend.home(request)?;
            if !home.is_absolute() || std::fs::symlink_metadata(&home).is_ok() {
                return Ok(refused(id, &command, Refusal::ExistingHome));
            }
            let record=state.save(KIND,json!({"id":id,"schema":1,"actor":actor,"endpoint_id":id,"endpoint_revision":agent.endpoint["revision"],"agent":{"instance_id":agent.instance_id,"command":agent.command,"args":agent.args,"cwd":agent.cwd},"operation":{"id":request,"request_id":request,"phase":"prepare","state":"running","uncertain":false},"connector":{"label":format!("{} in Slack",agent.endpoint["label"].as_str().unwrap_or("Agent")),"binding_verified":false},"private":{"home":home},"setup":{},"message":"Preparing a new private Slack connector."}),Some(0))?;
            (record, Some(selected), activate(state, id))
        } else {
            let Some(record) = existing else {
                return Ok(refused(id, &command, Refusal::NotPrepared));
            };
            if command.request_id.is_some()
                || record["revision"].as_u64() != Some(command.expected_revision)
                || command.operation_id.map(|v| json!(v)) != Some(record["operation"]["id"].clone())
            {
                return Ok(refused(id, &command, Refusal::SetupChanged));
            }
            if ![
                "authorize",
                "confirm",
                "connect",
                "discard",
                "bind",
                "resume",
            ]
            .contains(&command.action.as_str())
                || projection(Some(&record), &agent.endpoint, false)["capabilities"]
                    [format!("can_{}", command.action)]
                    != true
            {
                return Ok(refused(id, &command, Refusal::StepUnavailable));
            }
            if command.action == "confirm" {
                if !command.challenge.as_ref().is_some_and(|s| {
                    (1..=256).contains(&s.len()) && !s.chars().any(char::is_control)
                }) {
                    return Ok(refused(id, &command, Refusal::InvalidInput));
                }
            } else if command.challenge.is_some() {
                return Ok(refused(id, &command, Refusal::InvalidInput));
            }
            if command.action == "connect" {
                if command.team_id.as_deref()
                    != record["setup"]["authorized_workspace"]["team_id"].as_str()
                {
                    return Ok(refused(id, &command, Refusal::InvalidInput));
                }
            } else if command.team_id.is_some() {
                return Ok(refused(id, &command, Refusal::InvalidInput));
            }
            let mut record = record;
            phase(state, &mut record, &command.action)?;
            (record, None, activate(state, id))
        }
    };
    let result = if let Some(selected) = selected {
        prepare(state, backend, &mut record, &selected)
            .await
            .map(|_| None)
    } else if command.action == "resume" {
        resume(state, backend, &mut record).await
    } else if command.action == "bind" {
        bind_agent(state, backend, &mut record).await.map(|_| None)
    } else {
        async {
            let (home, exe, hash) = recorded_paths(&record)?;
            let action = match command.action.as_str() {
                "authorize" => ApiAction::Authorize,
                "confirm" => ApiAction::Confirm,
                "connect" => ApiAction::Connect,
                "discard" => ApiAction::Discard,
                _ => unreachable!(),
            };
            let mut body = json!({"action":command.action});
            if let Some(code) = &command.challenge {
                body["challenge"] = json!(code);
            }
            if let Some(team) = &command.team_id {
                body["team_id"] = json!(team);
            }
            let selected = current_agent(state, &record)?;
            backend.agent_ready(state, &selected).await?;
            let config = backend
                .call(&exe, &hash, &home, Action::Api(ApiAction::GetConfig), None)
                .await?;
            if !verify_binding(&record, &config) {
                record["connector"]["binding_verified"] = json!(false);
                return Err(ApiError::bad(
                    "The connector no longer has the selected agent binding",
                ));
            }
            current_agent(state, &record)?;
            let response = backend
                .call(&exe, &hash, &home, Action::Api(action), Some(&body))
                .await?;
            if !response["confirmation_refusal"].is_null() {
                let refused = &response["confirmation_refusal"];
                let code = refused["code"].as_str().unwrap_or("");
                if command.action != "confirm" || refused["schema"] != 1 || refused["action"] != "confirm"
                    || refused["id"] != config["id"] || refused["config_revision"] != config["revision"]
                    || !["invalid_confirmation_format", "authorization_expired", "invalid_challenge"].contains(&code) {
                    return Err(ApiError::bad("Unrecognized confirmation outcome"));
                }
                record["confirmation_refusal"] = json!(code);
                record["message"] = json!(match code {
                    "invalid_confirmation_format" => "The confirmation code format was not accepted. Review the code shown by Slack.",
                    "authorization_expired" => "This Slack approval expired. Start a new approval explicitly.",
                    _ => "Slack did not accept this confirmation code. Start a new approval explicitly.",
                });
            }
            record["private"]["final_reply_received"] = json!(true);
            record = save(state, &record)?;
            let mut transient = if command.action == "authorize" {
                command_ephemeral(&response)
            } else {
                None
            };
            drop(body);
            match observations(backend, &mut record).await {
                Ok(value) => { if transient.is_none() { transient = value; } },
                Err(_) => observation_unavailable(&mut record),
            }
            Ok(transient)
        }
        .await
    };
    let transient = match result {
        Ok(v) => {
            record["operation"]["state"] = json!("complete");
            record["operation"]["uncertain"] = json!(false);
            if record["observation_error"] != true && record["confirmation_refusal"].is_null() {
                record["message"] = json!("The connector's current setup is shown below.");
            }
            record = save(state, &record)?;
            v
        }
        Err(_) => {
            mark_unknown(state, &mut record)?;
            None
        }
    };
    drop(active);
    let mut out = projection(Some(&record), &agent.endpoint, false);
    if let Some(v) = transient {
        out["authorization_command"] = v;
    }
    Ok(out)
}

async fn prepare<B: Backend>(
    state: &AppState,
    backend: &B,
    record: &mut Value,
    selected: &LockedTarget,
) -> ApiResult<()> {
    let request = Uuid::parse_str(record["operation"]["id"].as_str().unwrap())
        .map_err(|_| ApiError::bad("Invalid saved setup identity"))?;
    let home = PathBuf::from(record["private"]["home"].as_str().unwrap());
    phase(state, record, "package")?;
    let cache = state.home.join("slack-packages").join(request.to_string());
    let executable = backend.stage(&cache, selected).await?;
    let binary = selected
        .release
        .files
        .get(&selected.release.entry)
        .ok_or_else(|| ApiError::bad("Missing reviewed connector executable"))?;
    phase(state, record, "native-setup")?;
    let created = backend
        .call(&executable, &binary.sha256, &home, Action::Setup, None)
        .await?;
    let canonical = std::fs::canonicalize(&home)?;
    let installed = canonical.join("bin").join(if cfg!(windows) {
        "acp-slack-adapter.exe"
    } else {
        "acp-slack-adapter"
    });
    if created["schema"] != "com.microsoft.amplifier.slack-native-setup/1"
        || created["created"] != true
        || created["serving"] != false
        || created["home"].as_str() != canonical.to_str()
        || created["executable"].as_str() != installed.to_str()
        || created["version"] != selected.release.version
        || Uuid::parse_str(created["id"].as_str().unwrap_or("")).is_err()
        || created["management"]["kind"] != "installation-local-cli"
    {
        return Err(ApiError::bad(
            "The connector did not return the reviewed installation identity",
        ));
    }
    record["connector"]["id"] = created["id"].clone();
    record["private"] = json!({"home":canonical,"executable":installed,"sha256":binary.sha256,"package_source":selected.release.source,"package_archive_sha256":selected.release.archive_sha256,"native_start_request_id":request});
    *record = save(state, record)?;
    phase(state, record, "native-start")?;
    let started = backend
        .call(
            &installed,
            &binary.sha256,
            &canonical,
            Action::Start {
                request_id: request,
            },
            None,
        )
        .await?;
    validate_lifecycle(&started, record)?;
    retain_lifecycle(record, &started);
    *record = save(state, record)?;
    bind_agent(state, backend, record).await
}

async fn bind_agent<B: Backend>(
    state: &AppState,
    backend: &B,
    record: &mut Value,
) -> ApiResult<()> {
    let (canonical, installed, hash) = recorded_paths(record)?;
    let status = backend
        .call(&installed, &hash, &canonical, Action::Status, None)
        .await?;
    validate_lifecycle(&status, record)?;
    let agent = current_agent(state, record)?;
    let mut cfg = backend
        .call(
            &installed,
            &hash,
            &canonical,
            Action::Api(ApiAction::GetConfig),
            None,
        )
        .await?;
    if cfg["id"] != record["connector"]["id"]
        || cfg["enabled"] != false
        || cfg["setup_complete"] != false
        || cfg["acp"]["command"] != ""
        || !cfg["slack"]["app_id"].as_str().is_some_and(str::is_empty)
        || !cfg["slack"]["team_id"].as_str().is_some_and(str::is_empty)
    {
        return Err(ApiError::bad(
            "The new connector's initial configuration changed; it was preserved",
        ));
    }
    let revision = cfg["revision"]
        .as_u64()
        .ok_or_else(|| ApiError::bad("Missing connector configuration revision"))?;
    cfg["label"] = record["connector"]["label"].clone();
    cfg["acp"]["command"] = json!(agent.command);
    cfg["acp"]["args"] = agent.args.clone();
    cfg["acp"]["cwd"] = json!(agent.cwd);
    if cfg["acp"]["env"] != json!({}) || cfg["acp"]["secret_env"] != json!({}) {
        return Err(ApiError::bad(
            "The new connector contains unexpected credential mappings",
        ));
    }
    record["private"]["binding_config_sha256"] = json!(config_digest(&cfg));
    backend.agent_ready(state, &agent).await?;
    phase(state, record, "bind-agent")?;
    backend
        .call(
            &installed,
            &hash,
            &canonical,
            Action::Api(ApiAction::PutConfig),
            Some(&json!({"expected_revision":revision,"config":cfg})),
        )
        .await?;
    record["private"]["final_reply_received"] = json!(true);
    record["private"]["binding_initialized"] = json!(true);
    *record = save(state, record)?;
    observations(backend, record).await.map(|_| ())
}

/// Consume the connector-owned, bracketed generation proof. A generic API
/// status or a later start cannot acknowledge this saved request.
fn lifecycle_binding(value: &Value, record: &Value) -> bool {
    value["schema"] == "com.microsoft.amplifier.slack-native-status/1"
        && value["id"] == record["connector"]["id"]
        && value["home"] == record["private"]["home"]
        && value["executable"] == record["private"]["executable"]
        && value["request"]["request_id"] == record["private"]["native_start_request_id"]
        && record["private"]["native_start_request_id"]
            .as_str()
            .is_some_and(|s| Uuid::parse_str(s).is_ok())
        && value["request_current"] == true
        && value["profile_matches"] == true
        && value["autostart"] == false
        && value["startup_change_supported"] == false
        && !value["service"].is_null()
        && (record["private"]["native_service"].is_null()
            || value["service"] == record["private"]["native_service"])
}
pub fn validate_lifecycle(value: &Value, record: &Value) -> ApiResult<()> {
    if !lifecycle_binding(value, record)
        || value["state"] != "ready"
        || value["launch_observed"] != true
        || value["process_identity"]
            .as_object()
            .is_none_or(|o| o.len() < 2)
        || value["process_identity"]["pid"]
            .as_u64()
            .is_none_or(|p| p == 0)
        || value["pid"] != value["process_identity"]["pid"]
    {
        return Err(ApiError::bad(
            "The exact connector start is not proved ready. Its request and installation were preserved",
        ));
    }
    Ok(())
}
fn inactive_lifecycle(value: &Value, record: &Value) -> bool {
    lifecycle_binding(value, record)
        && value["state"] == "inactive"
        && (value["request"]["phase"] == "ready"
            || value["control"]["phase"] == "inactive"
                && value["control"]["start_request_id"] == value["request"]["request_id"])
}
fn retain_lifecycle(record: &mut Value, value: &Value) {
    record["private"]["lifecycle_verified"] = json!(true);
    record["private"]["native_inactive"] = json!(false);
    record["private"]["native_service"] = value["service"].clone();
    record["private"]["native_process_identity"] = value["process_identity"].clone();
}
async fn resume<B: Backend>(
    state: &AppState,
    backend: &B,
    record: &mut Value,
) -> ApiResult<Option<Value>> {
    let (home, exe, hash) = recorded_paths(record)?;
    let prior = backend
        .call(&exe, &hash, &home, Action::Status, None)
        .await?;
    if !inactive_lifecycle(&prior, record) {
        return Err(ApiError::bad("The saved connector is not proved inactive"));
    }
    let request_id = Uuid::new_v4();
    record["private"]["native_start_request_id"] = json!(request_id);
    record["private"]["native_inactive"] = json!(false);
    phase(state, record, "native-start")?;
    let value = backend
        .call(&exe, &hash, &home, Action::Start { request_id }, None)
        .await?;
    validate_lifecycle(&value, record)?;
    retain_lifecycle(record, &value);
    record["private"]["final_reply_received"] = json!(true);
    *record = save(state, record)?;
    observations(backend, record).await
}
