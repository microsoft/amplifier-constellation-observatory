//! A complete desktop package carries one fixed, verified Instance installer.
//! Its installations remain separate from Observatory's own application folder.
use crate::{config, provider, state::AppState};
use amplifier_native_platform as platform;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
#[cfg(test)]
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const INSTANCE: platform::Application = platform::Application {
    display_name: "Constellation agent",
    startup_description: "Constellation agent",
    executable_name: "agent-instance",
    startup_prefix: "agent-instance",
    data_directory: "AgentInstance",
    background_command: "serve",
};

#[cfg(test)]
use crate::instance_package::ordinary;
pub(crate) use crate::instance_package::{copy_bundle, inventory};

/// Called only during explicit new desktop setup. Existing registries are never
/// rewritten by opening an application or replacing a downloaded executable.
pub fn prepare(
    state: &AppState,
    final_home: &Path,
    bundle: Option<&Path>,
    agent_root: &Path,
) -> Result<()> {
    ensure!(agent_root.is_absolute(), "Agent storage must be absolute");
    let id = uuid::Uuid::new_v4().to_string();
    let mut recipes = Vec::new();
    if let Some(source) = bundle {
        let destination = state.home.join("support/instance");
        let manifest = copy_bundle(source, &destination)?;
        let binary = final_home
            .join("support/instance")
            .join(manifest["binary"].as_str().unwrap());
        recipes.push(json!({"id":"bundled-instance","version":manifest["version"].as_str().unwrap_or("packaged"),"argv":[binary,"setup","install","--home","{home}","--name","{name}","--bind","127.0.0.1","--port","0","--no-browser"],"timeout_seconds":300}));
    }
    state.save("hosts", json!({"id":id,"label":platform::hostname(),"transport":"local","native_local":true,"install_roots":[agent_root],"recipes":recipes}), Some(0))?;
    Ok(())
}

pub fn bundled_path() -> Result<Option<PathBuf>> {
    let root = std::env::current_exe()?
        .parent()
        .context("Missing application folder")?
        .join("support/instance");
    Ok(root.exists().then_some(root))
}

pub fn default_agent_root() -> Result<PathBuf> {
    Ok(platform::default_home(&INSTANCE, "placeholder")?
        .parent()
        .unwrap()
        .to_path_buf())
}

pub fn folder(host: &Value, body: &Value) -> Result<PathBuf> {
    let id = uuid::Uuid::parse_str(
        body["request_id"]
            .as_str()
            .context("Start a new installation request")?,
    )?;
    let root = host["install_roots"][0]
        .as_str()
        .context("Missing agent storage")?;
    ensure!(
        body["home"].is_null() && body["port"].as_u64().unwrap_or(0) == 0,
        "This installer chooses its own folder and port"
    );
    Ok(Path::new(root).join(id.to_string()))
}

/// Reconcile a successful fixed installer result; never repeat installation.
/// Only the newly created local home supplies a credential, never browser input.
pub async fn attach(state: &AppState, job: &Value) -> Result<Value> {
    let plan = &job["plan"];
    let host = state.read(
        "hosts",
        plan["host_id"].as_str().context("Missing computer")?,
    )?;
    ensure!(
        host["native_local"] == true && plan["recipe_id"] == "bundled-instance",
        "Not a bundled installation"
    );
    let id = job["id"].as_str().context("Missing installation request")?;
    let home = PathBuf::from(
        plan["home"]
            .as_str()
            .context("Missing installation folder")?,
    );
    let expected_home = Path::new(
        host["install_roots"][0]
            .as_str()
            .context("Missing agent storage")?,
    )
    .join(id);
    ensure!(
        home == expected_home && !home.is_symlink(),
        "Installation folder changed"
    );
    let output: Value = serde_json::from_slice(&std::fs::read(
        state.home.join("jobs").join(format!("{id}.stdout")),
    )?)?;
    ensure!(
        output["home"].as_str() == home.to_str() && output["running_verified"] == true,
        "The agent has not confirmed startup"
    );
    let instance_id = output["instance_id"]
        .as_str()
        .context("Missing installed identity")?;
    uuid::Uuid::parse_str(instance_id)?;
    let endpoints: Value = serde_json::from_slice(&std::fs::read(home.join("endpoints.json"))?)?;
    let origin = endpoints["base_url"]
        .as_str()
        .context("Missing installed address")?;
    let url = reqwest::Url::parse(origin)?;
    ensure!(
        url.scheme() == "https"
            && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
            && url.path() == "/"
            && url.username().is_empty()
            && url.password().is_none(),
        "The local installer returned a different host"
    );
    let ca = PathBuf::from(
        endpoints["trust_certificate_file"]
            .as_str()
            .context("Missing installed trust certificate")?,
    );
    ensure!(
        ca.starts_with(&home) && !ca.is_symlink(),
        "The installed trust certificate changed"
    );
    let credential = std::fs::read_to_string(home.join("secrets/control-token"))?;
    ensure!(
        credential.trim().len() >= 32 && credential.len() <= 4096,
        "Invalid installed credential"
    );
    let reference = format!("installed-{id}");
    let secret = state.home.join("secrets").join(&reference);
    if secret.exists() {
        ensure!(
            std::fs::read_to_string(&secret)?.trim() == credential.trim(),
            "Saved installation credential changed"
        );
    } else {
        config::private_create(&secret, credential.trim().as_bytes())?;
    }
    let instance_binary = home.join("bin").join(if cfg!(windows) {
        "agent-instance.exe"
    } else {
        "agent-instance"
    });
    ensure!(
        instance_binary.is_file() && !instance_binary.is_symlink(),
        "Missing installed agent executable"
    );
    let workspace = crate::instance_package::session_directory(&home)?;
    let endpoint = json!({"id":id,"label":plan["name"],"kind":"agent","host_id":host["id"],"ui_url":origin,"acp":{"transport":"stdio","command":instance_binary,"args":["acp","--home",home],"cwd":home,"session_cwd":workspace},"management":{"provider":"agent-instance","base_url":origin,"expected_id":instance_id,"ca_file":ca,"credential_ref":reference,"access":"manage"}});
    provider::validate(&endpoint)?;
    let mut observed = provider::inspect(state, &endpoint).await?;
    ensure!(
        observed["instance_id"] == instance_id,
        "A different agent answered"
    );
    let _inventory = state.inventory_registry_lock.lock().unwrap();
    let current_host = state.read("hosts", host["id"].as_str().unwrap())?;
    ensure!(
        current_host["revision"] == host["revision"],
        "Computer settings changed; check them before connecting"
    );
    let endpoint = if let Ok(existing) = state.read("endpoints", id) {
        ensure!(
            existing["management"] == endpoint["management"],
            "This installation request already has another connection"
        );
        existing
    } else {
        state.save("endpoints", endpoint, Some(0))?
    };
    observed["endpoint_revision"] = endpoint["revision"].clone();
    state.save("observations", observed, None)?;
    state.audit(
        "installation.connected",
        Some(id),
        json!({"instance_id":instance_id,"host_id":host["id"]}),
    )?;
    Ok(endpoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture_bundle(root: &Path) -> String {
        std::fs::create_dir_all(root).unwrap();
        let binary = if cfg!(windows) {
            "agent-instance.exe"
        } else {
            "agent-instance"
        };
        let bytes = b"inert fixture, never executed";
        std::fs::write(root.join(binary), bytes).unwrap();
        std::fs::write(root.join("consumer-bundle.json"), json!({"schema":1,"kind":"instance-consumer-bundle","version":"fixture","binary":binary,"target":format!("{}-{}",if cfg!(target_arch="aarch64"){"aarch64"}else{"x86_64"},if cfg!(windows){"pc-windows-msvc"}else if cfg!(target_os="macos"){"apple-darwin"}else{"unknown-linux-gnu"}),"native_runtime":{},"files":{binary:format!("{:x}",Sha256::digest(bytes))}}).to_string()).unwrap();
        binary.into()
    }
    fn state(root: &Path) -> std::sync::Arc<AppState> {
        let c: config::Config = serde_json::from_value(json!({"id":uuid::Uuid::new_v4(),"owner":"fixture","bind":"127.0.0.1","port":0,"public_url":"","pam_service":"unused","tls_cert":"","tls_key":"","tls_ca":null})).unwrap();
        c.save(root).unwrap();
        AppState::open(root.into()).unwrap()
    }
    #[test]
    fn copy_preserves_exact_inventory_bytes_used_by_package_identity() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("download");
        fixture_bundle(&source);
        let path = source.join("consumer-bundle.json");
        let value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let bytes = format!("{}\n", serde_json::to_string_pretty(&value).unwrap()).into_bytes();
        std::fs::write(&path, &bytes).unwrap();
        let destination = temp.path().join("retained");
        copy_bundle(&source, &destination).unwrap();
        assert_eq!(
            std::fs::read(destination.join("consumer-bundle.json")).unwrap(),
            bytes
        );
    }
    #[test]
    fn changed_package_is_rejected_before_enabling_local_installation() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("download");
        let binary = fixture_bundle(&source);
        std::fs::write(source.join(binary), b"changed bytes").unwrap();
        let state = state(&temp.path().join("observatory"));
        assert!(
            prepare(
                &state,
                &state.home,
                Some(&source),
                &temp.path().join("agents")
            )
            .is_err()
        );
        assert!(state.list("hosts").unwrap().is_empty());
        assert!(!state.home.join("support/instance").exists());
    }
    #[test]
    fn copied_package_survives_download_removal_and_uses_final_home() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("download");
        fixture_bundle(&source);
        let state = state(&temp.path().join("staging"));
        let final_home = temp.path().join("installed");
        prepare(
            &state,
            &final_home,
            Some(&source),
            &temp.path().join("agents"),
        )
        .unwrap();
        std::fs::remove_dir_all(source).unwrap();
        inventory(&state.home.join("support/instance")).unwrap();
        let hosts = state.list("hosts").unwrap();
        assert_eq!(hosts.len(), 1);
        let executable = PathBuf::from(hosts[0]["recipes"][0]["argv"][0].as_str().unwrap());
        assert!(executable.starts_with(&final_home));
        assert!(!executable.starts_with(&state.home));
        assert_eq!(hosts[0]["native_local"], true);
    }
    #[test]
    fn limited_download_shows_local_computer_without_inventing_installer() {
        let temp = tempfile::tempdir().unwrap();
        let state = state(&temp.path().join("observatory"));
        prepare(&state, &state.home, None, &temp.path().join("agents")).unwrap();
        let hosts = state.list("hosts").unwrap();
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0]["recipes"], json!([]));
        assert!(!temp.path().join("agents").exists());
    }
    #[test]
    fn local_plan_has_stable_private_folder_and_refuses_browser_path() {
        let temp = tempfile::tempdir().unwrap();
        let host = json!({"install_roots":[temp.path()]});
        let id = uuid::Uuid::new_v4().to_string();
        let body = json!({"request_id":id});
        assert_eq!(folder(&host, &body).unwrap(), temp.path().join(&id));
        assert_eq!(folder(&host, &body).unwrap(), folder(&host, &body).unwrap());
        assert!(folder(&host, &json!({"request_id":id,"home":"elsewhere"})).is_err());
        assert!(folder(&host, &json!({"request_id":id,"port":1234})).is_err());
        assert!(folder(&host, &json!({"request_id":"../elsewhere"})).is_err());
    }
    #[test]
    fn traversal_and_links_are_not_package_members() {
        let temp = tempfile::tempdir().unwrap();
        fixture_bundle(temp.path());
        for path in [
            "../outside",
            "/absolute",
            "a/../other",
            "C:/elsewhere",
            "a\\b",
            "",
        ] {
            assert!(ordinary(temp.path(), path).is_err(), "{path}");
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                temp.path().join("agent-instance"),
                temp.path().join("alias"),
            )
            .unwrap();
            assert!(ordinary(temp.path(), "alias").is_err());
        }
    }
}
