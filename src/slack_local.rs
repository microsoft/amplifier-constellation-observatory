//! Bounded installation-owned Slack CLI calls. No arbitrary command or API path.
//! Dispatch uncertainty stays distinct from validation refusal. This bridge never
//! stops an existing service or cleans an installation after an uncertain result.
use amplifier_native_platform as platform;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fmt,
    io::Read,
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command};
use uuid::Uuid;

const MAX_JSON: usize = 1024 * 1024;
const MAX_EXECUTABLE: u64 = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub enum Action {
    Setup,
    Start { request_id: Uuid },
    Status,
    Api(ApiAction),
}
#[derive(Clone, Copy, Debug)]
pub enum ApiAction {
    GetConfig,
    PutConfig,
    GetSetup,
    Authorize,
    Confirm,
    Connect,
    Discard,
    GetStatus,
    GetConversations,
    GetActivity,
}
impl ApiAction {
    fn route(self) -> (&'static str, &'static str, Option<&'static str>) {
        match self {
            Self::GetConfig => ("GET", "config", None),
            Self::PutConfig => ("PUT", "config", None),
            Self::GetSetup => ("GET", "setup/slack", None),
            Self::Authorize => ("POST", "setup/slack", Some("authorize")),
            Self::Confirm => ("POST", "setup/slack", Some("confirm")),
            Self::Connect => ("POST", "setup/slack", Some("connect")),
            Self::Discard => ("POST", "setup/slack", Some("discard")),
            Self::GetStatus => ("GET", "status", None),
            Self::GetConversations => ("GET", "conversations", None),
            Self::GetActivity => ("GET", "activity", None),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    InvalidExecutable,
    InvalidHome,
    ExistingSetup,
    InvalidBody,
    BodyPreparation,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unknown {
    Launch,
    Timeout,
    Output,
    Exit,
    Response,
    Reap,
    BodyCleanup,
    Supervision,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallError {
    Refused(Refusal),
    Unknown(Unknown),
}
impl fmt::Display for CallError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(reason) => {
                write!(out, "Slack request refused before dispatch ({reason:?})")
            }
            Self::Unknown(reason) => write!(
                out,
                "Slack request outcome is unconfirmed ({reason:?}); do not repeat it"
            ),
        }
    }
}
impl std::error::Error for CallError {}

fn absolute_plain(path: &Path) -> bool {
    path.is_absolute()
        && path.components().all(|part| {
            matches!(
                part,
                Component::Prefix(_) | Component::RootDir | Component::Normal(_)
            )
        })
}
fn ordinary(path: &Path) -> std::io::Result<()> {
    let mut prefix = PathBuf::new();
    for part in path.components() {
        prefix.push(part);
        let metadata = std::fs::symlink_metadata(&prefix)?;
        if metadata.file_type().is_symlink() {
            return Err(std::io::Error::other("Symbolic path"));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(std::io::Error::other("Reparse path"));
            }
        }
    }
    if !std::fs::metadata(path)?.is_file() {
        return Err(std::io::Error::other("Not an ordinary executable"));
    }
    Ok(())
}
fn validate_executable(executable: &Path, expected: &str) -> Result<(), CallError> {
    let refused = || CallError::Refused(Refusal::InvalidExecutable);
    if !absolute_plain(executable)
        || expected.len() != 64
        || !expected
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(refused());
    }
    ordinary(executable).map_err(|_| refused())?;
    let mut file = std::fs::File::open(executable).map_err(|_| refused())?;
    let length = file.metadata().map_err(|_| refused())?.len();
    if !(1..=MAX_EXECUTABLE).contains(&length) {
        return Err(refused());
    }
    let mut digest = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let size = file.read(&mut buffer).map_err(|_| refused())?;
        if size == 0 {
            break;
        }
        total += size as u64;
        if total > length {
            return Err(refused());
        }
        digest.update(&buffer[..size]);
    }
    if total != length || format!("{:x}", digest.finalize()) != expected {
        return Err(refused());
    }
    Ok(())
}
fn validate_home(home: &Path, action: Action) -> Result<(), CallError> {
    if !absolute_plain(home) {
        return Err(CallError::Refused(Refusal::InvalidHome));
    }
    if matches!(action, Action::Setup) {
        match std::fs::symlink_metadata(home) {
            Ok(_) => return Err(CallError::Refused(Refusal::ExistingSetup)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err(CallError::Refused(Refusal::InvalidHome)),
        }
        let parent = home
            .parent()
            .ok_or(CallError::Refused(Refusal::InvalidHome))?;
        platform::validate_private_directory(parent)
            .map_err(|_| CallError::Refused(Refusal::InvalidHome))
    } else {
        platform::validate_private_directory(home)
            .map_err(|_| CallError::Refused(Refusal::InvalidHome))
    }
}

fn body_bytes(action: Action, body: Option<&Value>) -> Result<Option<Vec<u8>>, CallError> {
    let refused = CallError::Refused(Refusal::InvalidBody);
    let Action::Api(api) = action else {
        return if body.is_none() {
            Ok(None)
        } else {
            Err(refused)
        };
    };
    let (method, _, selected) = api.route();
    if method == "GET" {
        return if body.is_none() {
            Ok(None)
        } else {
            Err(refused)
        };
    }
    let mut value = match body {
        Some(Value::Object(value)) => value.clone(),
        None if selected.is_some() => serde_json::Map::new(),
        _ => return Err(refused),
    };
    if let Some(selected) = selected {
        if value
            .get("action")
            .is_some_and(|v| v.as_str() != Some(selected))
        {
            return Err(refused);
        }
        value.insert("action".into(), Value::String(selected.into()));
    }
    let bytes = serde_json::to_vec(&value).map_err(|_| refused)?;
    if bytes.len() > MAX_JSON {
        return Err(refused);
    }
    Ok(Some(bytes))
}
fn command(executable: &Path, home: &Path, action: Action, body_file: Option<&Path>) -> Command {
    let mut command = Command::new(executable);
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
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("SYSTEMROOT") {
        command.env("PATH", PathBuf::from(root).join("System32"));
    }
    #[cfg(unix)]
    command.env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
    command.arg("--home").arg(home);
    match action {
        Action::Setup => {
            command.arg("native-setup");
        }
        Action::Start { request_id } => {
            command.args(["native-start", "--request-id", &request_id.to_string()]);
        }
        Action::Status => {
            command.arg("native-status");
        }
        Action::Api(api) => {
            let (method, path, _) = api.route();
            command.args(["api", method, path]);
            if let Some(path) = body_file {
                command.arg("--body-file").arg(path);
            }
        }
    }
    command
        .current_dir(if matches!(action, Action::Setup) {
            home.parent().unwrap()
        } else {
            home
        })
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    command
}
fn budget(action: Action) -> Duration {
    Duration::from_secs(match action {
        Action::Setup => 300,
        // Owner action budget is60s; fixed10s transport/exit margin does not
        // extend that product budget or permit redispatch.
        Action::Start { .. } => 70,
        // These fixed owner operations have an existing180s helper budget.
        // The facade allows10s for its response; this outer bridge allows
        // another10s for transport/exit, without retrying the owner action.
        Action::Api(
            ApiAction::Authorize | ApiAction::Confirm | ApiAction::Connect | ApiAction::Discard,
        ) => 200,
        Action::Status | Action::Api(_) => 100,
    })
}

#[cfg(unix)]
struct ProcessGroup(Option<u32>);
#[cfg(unix)]
impl ProcessGroup {
    fn disarm(&mut self) {
        self.0 = None;
    }
}
#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // SAFETY: this group was created for this owned, still-unreaped leader.
        // wait() disarms synchronously before the numeric ID can be reused.
        if let Some(pid) = self.0 {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
}

/// A dropped caller does not abort the owned cleanup task. The original deadline
/// still applies; no second command is sent. Runtime shutdown drops the process
/// guard, while an unreaped child's request file remains private and retained.
pub async fn call(
    executable: &Path,
    expected_sha256: &str,
    home: &Path,
    action: Action,
    body: Option<&Value>,
) -> Result<Value, CallError> {
    validate_executable(executable, expected_sha256)?;
    validate_home(home, action)?;
    let bytes = body_bytes(action, body)?;
    let executable = executable.to_owned();
    let home = home.to_owned();
    tokio::spawn(async move { run(&executable, &home, action, bytes, budget(action)).await })
        .await
        .map_err(|_| CallError::Unknown(Unknown::Supervision))?
}

async fn run(
    executable: &Path,
    home: &Path,
    action: Action,
    body: Option<Vec<u8>>,
    deadline: Duration,
) -> Result<Value, CallError> {
    let body_file = if let Some(bytes) = body {
        let secrets = home.join("secrets");
        platform::validate_private_directory(&secrets)
            .map_err(|_| CallError::Refused(Refusal::BodyPreparation))?;
        let path = secrets.join(format!("observatory-request-{}.json", Uuid::new_v4()));
        platform::private_create(&path, &bytes)
            .map_err(|_| CallError::Refused(Refusal::BodyPreparation))?;
        Some(path)
    } else {
        None
    };
    let mut command = command(executable, home, action, body_file.as_deref());
    #[cfg(windows)]
    let spawned = crate::process_tree::ProcessJob::spawn(&mut command).await;
    #[cfg(not(windows))]
    let spawned = command.spawn();
    // A failed process-creation boundary is kept unknown. In particular, the
    // Windows job launcher may have created a suspended child before refusal.
    #[cfg(windows)]
    let (mut child, owned) = spawned.map_err(|_| CallError::Unknown(Unknown::Launch))?;
    #[cfg(not(windows))]
    let mut child = spawned.map_err(|_| CallError::Unknown(Unknown::Launch))?;
    #[cfg(unix)]
    let mut owned = ProcessGroup(Some(child.id().ok_or(CallError::Unknown(Unknown::Launch))?));
    let mut stdout = child
        .stdout
        .take()
        .ok_or(CallError::Unknown(Unknown::Output))?;
    let observe = async {
        let read = async {
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 8192];
            loop {
                let count = stdout
                    .read(&mut buffer)
                    .await
                    .map_err(|_| Unknown::Output)?;
                if count == 0 {
                    break;
                }
                if bytes.len() + count > MAX_JSON {
                    return Err(Unknown::Output);
                }
                bytes.extend_from_slice(&buffer[..count]);
            }
            Ok::<_, Unknown>(bytes)
        };
        let wait = async {
            let result = child.wait().await;
            // No await between wait returning and disarming. An error also
            // loses proof that the numeric group is safe to signal.
            #[cfg(unix)]
            owned.disarm();
            result.map_err(|_| Unknown::Reap)
        };
        tokio::try_join!(read, wait)
    };
    let result = tokio::time::timeout(deadline, observe).await;
    drop(owned);
    let result = match result {
        Ok(Ok((bytes, status))) => {
            if !status.success() {
                Err(CallError::Unknown(Unknown::Exit))
            } else {
                serde_json::from_slice::<Value>(&bytes)
                    .map_err(|_| CallError::Unknown(Unknown::Response))
                    .and_then(|value| {
                        if value.is_object() {
                            Ok(value)
                        } else {
                            Err(CallError::Unknown(Unknown::Response))
                        }
                    })
            }
        }
        other => {
            let _ = child.start_kill();
            if !matches!(
                tokio::time::timeout(Duration::from_secs(5), child.wait()).await,
                Ok(Ok(_))
            ) {
                return Err(CallError::Unknown(Unknown::Reap));
            }
            Err(CallError::Unknown(match other {
                Err(_) => Unknown::Timeout,
                Ok(Err(reason)) => reason,
                _ => unreachable!(),
            }))
        }
    };
    // This branch is reached only after child.wait confirmed exit. No broad
    // directory cleanup, installation cleanup or operation replay is performed.
    if let Some(path) = body_file {
        std::fs::remove_file(path).map_err(|_| CallError::Unknown(Unknown::BodyCleanup))?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fixed_owner_setup_transport_fits_existing_helper_budget() {
        for api in [
            ApiAction::Authorize,
            ApiAction::Confirm,
            ApiAction::Connect,
            ApiAction::Discard,
        ] {
            assert_eq!(budget(Action::Api(api)), Duration::from_secs(200));
        }
        for api in [
            ApiAction::GetConfig,
            ApiAction::PutConfig,
            ApiAction::GetSetup,
            ApiAction::GetStatus,
            ApiAction::GetConversations,
            ApiAction::GetActivity,
        ] {
            assert_eq!(budget(Action::Api(api)), Duration::from_secs(100));
        }
        assert_eq!(
            budget(Action::Start {
                request_id: Uuid::nil()
            }),
            Duration::from_secs(70)
        );
        assert_eq!(budget(Action::Setup), Duration::from_secs(300));
    }

    #[test]
    fn fixed_api_routes_and_action_discriminators_cannot_be_overridden() {
        for (api, method, path) in [
            (ApiAction::GetConfig, "GET", "config"),
            (ApiAction::PutConfig, "PUT", "config"),
            (ApiAction::GetSetup, "GET", "setup/slack"),
            (ApiAction::GetStatus, "GET", "status"),
            (ApiAction::GetConversations, "GET", "conversations"),
            (ApiAction::GetActivity, "GET", "activity"),
        ] {
            assert_eq!((api.route().0, api.route().1), (method, path));
        }
        for (api, expected) in [
            (ApiAction::Authorize, "authorize"),
            (ApiAction::Confirm, "confirm"),
            (ApiAction::Connect, "connect"),
            (ApiAction::Discard, "discard"),
        ] {
            let bytes = body_bytes(Action::Api(api), None).unwrap().unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&bytes).unwrap()["action"],
                expected
            );
            assert_eq!(
                body_bytes(Action::Api(api), Some(&json!({"action":"other"}))).unwrap_err(),
                CallError::Refused(Refusal::InvalidBody)
            );
        }
        assert!(body_bytes(Action::Status, Some(&json!({}))).is_err());
        assert!(body_bytes(Action::Api(ApiAction::GetConfig), Some(&json!({}))).is_err());
        assert!(body_bytes(Action::Api(ApiAction::PutConfig), None).is_err());
        assert!(
            body_bytes(
                Action::Api(ApiAction::PutConfig),
                Some(&json!({"oversize":"x".repeat(MAX_JSON)}))
            )
            .is_err()
        );
    }

    #[test]
    fn error_projection_never_carries_process_output_or_body() {
        for error in [
            CallError::Refused(Refusal::InvalidBody),
            CallError::Unknown(Unknown::Exit),
            CallError::Unknown(Unknown::Timeout),
        ] {
            let text = error.to_string();
            assert!(!text.contains("xoxb-private"));
            if matches!(error, CallError::Unknown(_)) {
                assert!(text.contains("do not repeat"));
            }
        }
    }

    #[cfg(unix)]
    mod unix {
        use super::*;
        use std::os::unix::fs::PermissionsExt;
        struct Fixture {
            _temp: tempfile::TempDir,
            executable: PathBuf,
            digest: String,
            home: PathBuf,
            marker: PathBuf,
        }
        impl Fixture {
            fn new(mode: &str) -> Self {
                let temp = tempfile::tempdir().unwrap();
                // macOS temporary paths may use /var's system alias. The
                // application receives the canonical private fixture path.
                let root = temp.path().canonicalize().unwrap();
                platform::private_dir(&root).unwrap();
                let home = root.join("connector");
                platform::private_create_dir(&home).unwrap();
                platform::private_create_dir(&home.join("secrets")).unwrap();
                let executable = root.join("fixed-fixture");
                let marker = root.join("invocations");
                let script = format!(
                    r##"#!/usr/bin/env python3
import json,os,pathlib,subprocess,sys,time
marker=pathlib.Path({marker})
with marker.open('a') as out: out.write('one\n')
mode={mode}
body=None
if '--body-file' in sys.argv:
    filename=pathlib.Path(sys.argv[sys.argv.index('--body-file')+1])
    body=json.loads(filename.read_text())
    assert filename.stat().st_mode & 0o077 == 0
if mode=='timeout': time.sleep(60)
if mode=='held':
    marker.with_suffix('.ready').write_text('ready')
    deadline=time.monotonic()+10
    while not marker.with_suffix('.release').exists():
        if time.monotonic()>=deadline: sys.exit(4)
        time.sleep(.005)
if mode=='exit': print('xoxb-private-value',file=sys.stderr); sys.exit(3)
if mode=='bad': print('xoxb-private-invalid-json'); sys.exit(0)
if mode=='oversize': print('x'*(1024*1024+1)); time.sleep(60)
if mode=='exited-parent':
    code="import pathlib,time; time.sleep(.2); pathlib.Path("+repr(str(marker)+'.survived')+").write_text('survived')"
    subprocess.Popen([sys.executable,'-c',code],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
print(json.dumps({{'ok':True,'arguments':sys.argv[1:],'body_action':None if body is None else body.get('action'),'body_received':body is not None,'path':os.environ.get('PATH'),'ambient_secrets':bool(set(os.environ)&{{'OPENAI_API_KEY','ANTHROPIC_API_KEY','GITHUB_TOKEN','HTTP_PROXY','HTTPS_PROXY','PYTHONPATH'}})}}),flush=True)
"##,
                    marker = serde_json::to_string(marker.to_str().unwrap()).unwrap(),
                    mode = serde_json::to_string(mode).unwrap()
                );
                std::fs::write(&executable, script.as_bytes()).unwrap();
                std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
                    .unwrap();
                let digest = format!("{:x}", Sha256::digest(script.as_bytes()));
                Self {
                    _temp: temp,
                    executable,
                    digest,
                    home,
                    marker,
                }
            }
            fn body_count(&self) -> usize {
                std::fs::read_dir(self.home.join("secrets"))
                    .unwrap()
                    .count()
            }
            fn invocations(&self) -> usize {
                std::fs::read_to_string(&self.marker)
                    .unwrap()
                    .lines()
                    .count()
            }
        }

        #[tokio::test]
        async fn protected_body_is_never_an_argument_and_is_removed_after_success() {
            let fixture = Fixture::new("success");
            let result = call(
                &fixture.executable,
                &fixture.digest,
                &fixture.home,
                Action::Api(ApiAction::Confirm),
                Some(&json!({"challenge":"xoxb-private-value"})),
            )
            .await
            .unwrap();
            assert_eq!(result["body_action"], "confirm");
            assert_eq!(result["body_received"], true);
            assert_eq!(result["path"], "/usr/bin:/bin:/usr/sbin:/sbin");
            assert_eq!(result["ambient_secrets"], false);
            assert!(
                !result["arguments"]
                    .to_string()
                    .contains("xoxb-private-value")
            );
            assert_eq!(fixture.body_count(), 0);
            assert_eq!(fixture.invocations(), 1);
        }

        #[tokio::test]
        async fn wrong_digest_changed_image_and_existing_setup_refuse_before_dispatch() {
            let fixture = Fixture::new("success");
            assert_eq!(
                call(
                    &fixture.executable,
                    &"0".repeat(64),
                    &fixture.home,
                    Action::Status,
                    None
                )
                .await
                .unwrap_err(),
                CallError::Refused(Refusal::InvalidExecutable)
            );
            assert_eq!(
                call(
                    &fixture.executable,
                    &fixture.digest,
                    &fixture.home,
                    Action::Setup,
                    None
                )
                .await
                .unwrap_err(),
                CallError::Refused(Refusal::ExistingSetup)
            );
            std::fs::write(&fixture.executable, b"changed executable").unwrap();
            assert_eq!(
                call(
                    &fixture.executable,
                    &fixture.digest,
                    &fixture.home,
                    Action::Status,
                    None
                )
                .await
                .unwrap_err(),
                CallError::Refused(Refusal::InvalidExecutable)
            );
            assert!(!fixture.marker.exists());
            assert_eq!(fixture.body_count(), 0);
        }

        #[tokio::test]
        async fn nonzero_malformed_and_oversize_results_remain_unknown_without_replay() {
            for (mode, reason) in [
                ("exit", Unknown::Exit),
                ("bad", Unknown::Response),
                ("oversize", Unknown::Output),
            ] {
                let fixture = Fixture::new(mode);
                let error = call(
                    &fixture.executable,
                    &fixture.digest,
                    &fixture.home,
                    Action::Api(ApiAction::Authorize),
                    None,
                )
                .await
                .unwrap_err();
                assert_eq!(error, CallError::Unknown(reason));
                assert!(!error.to_string().contains("xoxb-private"));
                assert_eq!(fixture.invocations(), 1);
                assert_eq!(fixture.body_count(), 0);
            }
        }

        #[tokio::test]
        async fn timeout_reaps_owned_child_then_removes_only_its_body() {
            let fixture = Fixture::new("timeout");
            std::fs::write(
                fixture.home.join("secrets/retained"),
                b"existing credential",
            )
            .unwrap();
            let bytes = body_bytes(Action::Api(ApiAction::Authorize), None).unwrap();
            let error = run(
                &fixture.executable,
                &fixture.home,
                Action::Api(ApiAction::Authorize),
                bytes,
                Duration::from_millis(250),
            )
            .await
            .unwrap_err();
            assert_eq!(error, CallError::Unknown(Unknown::Timeout));
            assert_eq!(fixture.invocations(), 1);
            assert_eq!(fixture.body_count(), 1);
            assert_eq!(
                std::fs::read(fixture.home.join("secrets/retained")).unwrap(),
                b"existing credential"
            );
        }

        #[tokio::test]
        async fn cancelled_observer_does_not_abandon_owned_child_cleanup_or_repeat_call() {
            let fixture = Fixture::new("held");
            let executable = fixture.executable.clone();
            let digest = fixture.digest.clone();
            let home = fixture.home.clone();
            let observer = tokio::spawn(async move {
                call(
                    &executable,
                    &digest,
                    &home,
                    Action::Api(ApiAction::Authorize),
                    None,
                )
                .await
            });
            // This is interpreter/fixture readiness, not the product deadline
            // or cleanup allowance. Keep the child held after it has read its
            // protected request so cancellation cannot race a short sleep.
            let ready = tokio::time::timeout(Duration::from_secs(30), async {
                while !fixture.marker.with_extension("ready").exists() {
                    assert!(!observer.is_finished(), "Fixture returned before readiness");
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await;
            if ready.is_err() {
                std::fs::write(fixture.marker.with_extension("release"), b"release").unwrap();
            }
            ready.expect("Fixture did not reach its controlled readiness checkpoint");
            assert_eq!(fixture.body_count(), 1);
            observer.abort();
            assert!(observer.await.unwrap_err().is_cancelled());
            assert_eq!(fixture.body_count(), 1);
            std::fs::write(fixture.marker.with_extension("release"), b"release").unwrap();
            tokio::time::timeout(Duration::from_secs(3), async {
                while fixture.body_count() != 0 {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(fixture.invocations(), 1);
        }

        #[tokio::test]
        async fn fixed_setup_start_and_status_arguments_do_not_accept_dynamic_commands() {
            let fixture = Fixture::new("success");
            let id = Uuid::new_v4();
            let result = call(
                &fixture.executable,
                &fixture.digest,
                &fixture.home,
                Action::Start { request_id: id },
                None,
            )
            .await
            .unwrap();
            assert_eq!(
                result["arguments"],
                json!([
                    "--home",
                    fixture.home,
                    "native-start",
                    "--request-id",
                    id.to_string()
                ])
            );
            let result = call(
                &fixture.executable,
                &fixture.digest,
                &fixture.home,
                Action::Status,
                None,
            )
            .await
            .unwrap();
            assert_eq!(
                result["arguments"],
                json!(["--home", fixture.home, "native-status"])
            );
            let home = fixture.home.parent().unwrap().join("fresh");
            let result = call(
                &fixture.executable,
                &fixture.digest,
                &home,
                Action::Setup,
                None,
            )
            .await
            .unwrap();
            assert_eq!(result["arguments"], json!(["--home", home, "native-setup"]));
            assert_eq!(fixture.invocations(), 3);
        }

        #[tokio::test]
        async fn reaped_leader_disarms_numeric_group_before_returning() {
            let fixture = Fixture::new("exited-parent");
            let result = call(
                &fixture.executable,
                &fixture.digest,
                &fixture.home,
                Action::Status,
                None,
            )
            .await
            .unwrap();
            assert_eq!(result["ok"], true);
            // The child deliberately leaves one finite fixture descendant. It
            // proves the guard does not signal a numeric group after reaping;
            // production native-start service lifetime still belongs to its OS manager.
            let survived = fixture.marker.with_extension("survived");
            tokio::time::timeout(Duration::from_secs(3), async {
                while !survived.exists() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(std::fs::read_to_string(survived).unwrap(), "survived");
            assert_eq!(fixture.invocations(), 1);
        }
    }
}
