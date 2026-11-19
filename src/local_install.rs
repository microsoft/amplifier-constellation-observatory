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
        // A private derivative is authorized by compiled cohort data, not by a
        // browser path or a self-asserted package manifest. Refuse before copy.
        crate::signing_cohort::runtime(source)?;
        let destination = state.home.join("support/instance");
        let manifest = copy_bundle(source, &destination)?;
        let runtime = crate::signing_cohort::runtime(&destination)?;
        recipes.push(recipe(final_home, &manifest, runtime.as_ref()));
    }
    state.save("hosts", json!({"id":id,"label":platform::hostname(),"transport":"local","native_local":true,"install_roots":[agent_root],"recipes":recipes}), Some(0))?;
    Ok(())
}

pub(crate) fn verified_recipe(bundle_root: &Path, final_home: &Path) -> Result<Value> {
    let (manifest, _) = inventory(bundle_root)?;
    let runtime = crate::signing_cohort::runtime(bundle_root)?;
    Ok(recipe(final_home, &manifest, runtime.as_ref()))
}

/// Recognize only the exact earlier owned recipe shape, without authorizing an
/// old derivative under this executable's new cohort lock. The caller retains
/// its existing installed-manifest and registry revision checks.
pub(crate) fn fixed_recipe_matches(home: &Path, manifest: &Value, saved: &Value) -> Result<bool> {
    let root = home.join("support/instance");
    let (actual, manifest_sha) = crate::signing_cohort::manifest(&root)?;
    ensure!(actual == *manifest, "The retained agent inventory changed");
    ensure!(
        manifest["binary"].as_str().is_some(),
        "Missing retained installer identity"
    );
    let runtime = if let Some(marker) = manifest.get("private_derivative") {
        let cohort = marker["cohort_id"]
            .as_str()
            .context("Missing previous signed cohort")?;
        ensure!(
            uuid::Uuid::parse_str(cohort)?.to_string() == cohort
                && marker == &json!({"schema":1,"kind":"signed_macos_instance","cohort_id":cohort})
                && manifest["native_runtime"]["kind"] == "private_derivative"
                && manifest["native_runtime"]["binary"] == "engine/native/agent-runtime"
                && manifest["native_runtime"]["binary_sha256"]
                    == manifest["files"]["engine/native/agent-runtime"],
            "Previous signed package identity differs"
        );
        Some(crate::signing_cohort::RuntimeBinding {
            path: root.join("engine/native/agent-runtime"),
            cohort_id: cohort.into(),
            manifest_sha256: manifest_sha,
        })
    } else {
        ensure!(
            manifest["native_runtime"]["kind"] != "private_derivative",
            "Previous signed marker is missing"
        );
        None
    };
    Ok(saved == &recipe(home, manifest, runtime.as_ref()))
}

fn recipe(
    home: &Path,
    manifest: &Value,
    runtime: Option<&crate::signing_cohort::RuntimeBinding>,
) -> Value {
    let support = home.join("support/instance");
    let binary = support.join(manifest["binary"].as_str().unwrap());
    let mut value = json!({"id":"bundled-instance","version":manifest["version"].as_str().unwrap_or("packaged"),"argv":[binary,"setup","install","--home","{home}","--name","{name}","--bind","127.0.0.1","--port","0","--no-browser"],"timeout_seconds":300});
    if let Some(runtime) = runtime {
        value["argv"].as_array_mut().unwrap().extend([
            json!("--runtime-binary"),
            json!(support.join("engine/native/agent-runtime")),
        ]);
        value["private_cohort"] =
            json!({"id":runtime.cohort_id,"manifest_sha256":runtime.manifest_sha256});
    }
    value
}

/// Called immediately before a fresh owned installer is dispatched. A private
/// signed package must still match the compiled lock, all retained member bytes
/// and the exact saved fixed recipe. Existing public recipes remain unchanged.
pub(crate) fn validate_dispatch(state: &AppState, host: &Value, plan: &Value) -> Result<()> {
    if host["native_local"] != true || plan["recipe_id"] != "bundled-instance" {
        return Ok(());
    }
    let root = state.home.join("support/instance");
    let runtime = crate::signing_cohort::runtime(&root)?;
    let saved = host["recipes"]
        .as_array()
        .context("Missing local recipe")?
        .iter()
        .find(|r| r["id"] == "bundled-instance")
        .context("Missing local recipe")?;
    let Some(runtime) = runtime else {
        ensure!(
            saved.get("private_cohort").is_none()
                && !plan["argv"]
                    .as_array()
                    .context("Missing installer arguments")?
                    .iter()
                    .any(|v| v == "--runtime-binary"),
            "Private package binding is missing"
        );
        return Ok(());
    };
    let (manifest, manifest_sha) = crate::signing_cohort::manifest(&root)?;
    ensure!(
        manifest_sha == runtime.manifest_sha256,
        "The signed inventory changed before dispatch"
    );
    let expected = recipe(&state.home, &manifest, Some(&runtime));
    ensure!(
        saved == &expected
            && host["transport"] == "local"
            && plan["transport"] == "local"
            && host["id"] == plan["host_id"]
            && plan["version"] == expected["version"]
            && plan["timeout_seconds"] == 300
            && plan["cwd"].is_null(),
        "The signed installer recipe changed"
    );
    let mut argv = Vec::new();
    for value in expected["argv"].as_array().unwrap() {
        argv.push(
            value
                .as_str()
                .unwrap()
                .replace(
                    "{name}",
                    plan["name"].as_str().context("Missing installation name")?,
                )
                .replace(
                    "{home}",
                    plan["home"].as_str().context("Missing installation home")?,
                )
                .replace(
                    "{port}",
                    &plan["port"]
                        .as_u64()
                        .context("Missing installation port")?
                        .to_string(),
                )
                .replace("{version}", expected["version"].as_str().unwrap()),
        );
    }
    ensure!(
        plan["argv"] == json!(argv),
        "The signed installer arguments changed"
    );
    Ok(())
}

pub fn bundled_path() -> Result<Option<PathBuf>> {
    let root =
        crate::package_layout::resource_root(&std::env::current_exe()?)?.join("support/instance");
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
    fn fixture_target() -> String {
        format!(
            "{}-{}",
            if cfg!(target_arch = "aarch64") {
                "aarch64"
            } else {
                "x86_64"
            },
            if cfg!(windows) {
                "pc-windows-msvc"
            } else if cfg!(target_os = "macos") {
                "apple-darwin"
            } else {
                "unknown-linux-gnu"
            }
        )
    }
    fn signed_target_selected() -> bool {
        let lock: Value =
            serde_json::from_str(include_str!("../scripts/signed-instance-cohort-lock.json"))
                .unwrap();
        lock["targets"].get(fixture_target()).is_some()
    }
    fn fixture_bundle(root: &Path) -> String {
        std::fs::create_dir_all(root).unwrap();
        let binary = if cfg!(windows) {
            "agent-instance.exe"
        } else {
            "agent-instance"
        };
        let bytes = b"inert fixture, never executed";
        std::fs::write(root.join(binary), bytes).unwrap();
        std::fs::write(root.join("consumer-bundle.json"), json!({"schema":1,"kind":"instance-consumer-bundle","version":"fixture","binary":binary,"target":fixture_target(),"native_runtime":{},"files":{binary:format!("{:x}",Sha256::digest(bytes))}}).to_string()).unwrap();
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
        // On selected Mac targets the marker refusal is earlier than copying;
        // also prove the changed-byte inventory failure independently.
        assert!(inventory(&source).is_err());
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
    fn copied_inventory_survives_download_removal_and_recipe_uses_final_home() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("download");
        fixture_bundle(&source);
        let staging = temp.path().join("staging");
        let final_home = temp.path().join("installed");
        // Inventory retention and fixed recipe construction are independent of
        // compiled signed-cohort admission, which is tested separately below.
        let retained = staging.join("support/instance");
        let manifest = copy_bundle(&source, &retained).unwrap();
        let saved_recipe = recipe(&final_home, &manifest, None);
        std::fs::remove_dir_all(source).unwrap();
        assert_eq!(inventory(&retained).unwrap().0, manifest);
        let executable = PathBuf::from(saved_recipe["argv"][0].as_str().unwrap());
        assert!(executable.starts_with(&final_home));
        assert!(!executable.starts_with(&staging));
    }
    #[test]
    fn fresh_public_prepare_follows_compiled_target_selection() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("download");
        fixture_bundle(&source);
        let before = std::fs::read(source.join("consumer-bundle.json")).unwrap();
        let state = state(&temp.path().join("observatory"));
        let result = prepare(
            &state,
            &state.home,
            Some(&source),
            &temp.path().join("agents"),
        );
        if signed_target_selected() {
            assert_eq!(
                result.unwrap_err().to_string(),
                "The expected signed package is missing or changed"
            );
            assert!(state.list("hosts").unwrap().is_empty());
            assert!(!state.home.join("support/instance").exists());
        } else {
            result.unwrap();
            let hosts = state.list("hosts").unwrap();
            assert_eq!(hosts.len(), 1);
            assert_eq!(hosts[0]["native_local"], true);
            let (manifest, _) = inventory(&state.home.join("support/instance")).unwrap();
            assert_eq!(hosts[0]["recipes"][0], recipe(&state.home, &manifest, None));
        }
        assert_eq!(
            std::fs::read(source.join("consumer-bundle.json")).unwrap(),
            before
        );
    }
    #[test]
    fn retained_public_recipe_is_recognized_but_dispatch_obeys_current_selection() {
        let temp = tempfile::tempdir().unwrap();
        let state = state(&temp.path().join("observatory"));
        let retained = state.home.join("support/instance");
        let binary = fixture_bundle(&retained);
        let before = std::fs::read(retained.join("consumer-bundle.json")).unwrap();
        let manifest: Value = serde_json::from_slice(&before).unwrap();
        let expected = json!({"id":"bundled-instance","version":"fixture","argv":[state.home.join("support/instance").join(binary),"setup","install","--home","{home}","--name","{name}","--bind","127.0.0.1","--port","0","--no-browser"],"timeout_seconds":300});
        assert_eq!(recipe(&state.home, &manifest, None), expected);
        assert!(fixed_recipe_matches(&state.home, &manifest, &expected).unwrap());
        // A retained historical recipe is not a fresh setup under today's lock.
        let host = state.save("hosts", json!({"id":uuid::Uuid::new_v4(),"label":"Retained fixture","transport":"local","native_local":true,"install_roots":[temp.path().join("agents")],"recipes":[expected.clone()]}), Some(0)).unwrap();
        let plan = crate::provision::plan(&state, &json!({"host_id":host["id"],"recipe_id":"bundled-instance","request_id":uuid::Uuid::new_v4(),"name":"Fresh agent"})).unwrap();
        if signed_target_selected() {
            assert_eq!(
                verified_recipe(&retained, &state.home)
                    .unwrap_err()
                    .to_string(),
                "The expected signed package is missing or changed"
            );
            assert_eq!(
                validate_dispatch(&state, &host, &plan)
                    .unwrap_err()
                    .to_string(),
                "The expected signed package is missing or changed"
            );
        } else {
            assert_eq!(verified_recipe(&retained, &state.home).unwrap(), expected);
            validate_dispatch(&state, &host, &plan).unwrap();
        }
        assert_eq!(
            state.read("hosts", host["id"].as_str().unwrap()).unwrap(),
            host
        );
        assert_eq!(
            std::fs::read(retained.join("consumer-bundle.json")).unwrap(),
            before
        );
        let mut changed = plan.clone();
        changed["argv"]
            .as_array_mut()
            .unwrap()
            .extend([json!("--runtime-binary"), json!("/unreviewed")]);
        assert!(validate_dispatch(&state, &host, &changed).is_err());
        let mut marked = host;
        marked["recipes"][0]["private_cohort"] = json!({"id":"removed lock"});
        assert!(validate_dispatch(&state, &marked, &plan).is_err());
    }
    #[test]
    fn private_recipe_uses_only_final_retained_runtime_path() {
        let temp = tempfile::tempdir().unwrap();
        let final_home = temp.path().join("final");
        let manifest = json!({"binary":"agent-instance","version":"fixture"});
        let bound = crate::signing_cohort::RuntimeBinding {
            path: temp.path().join("source/engine/native/agent-runtime"),
            cohort_id: uuid::Uuid::new_v4().to_string(),
            manifest_sha256: "a".repeat(64),
        };
        let value = recipe(&final_home, &manifest, Some(&bound));
        let argv = value["argv"].as_array().unwrap();
        assert_eq!(argv[argv.len() - 2], "--runtime-binary");
        assert_eq!(
            Path::new(argv.last().unwrap().as_str().unwrap()),
            final_home
                .join("support")
                .join("instance")
                .join("engine")
                .join("native")
                .join("agent-runtime")
        );
        assert_eq!(
            value["private_cohort"],
            json!({"id":bound.cohort_id,"manifest_sha256":bound.manifest_sha256})
        );
        assert!(!value.to_string().contains("source/engine"));
    }
    #[test]
    fn old_fixed_recipe_shape_is_not_new_cohort_authority_or_custom_migration() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let root = home.join("support/instance");
        fixture_bundle(&root);
        let file = root.join("consumer-bundle.json");
        let mut manifest: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        let public = recipe(home, &manifest, None);
        assert!(fixed_recipe_matches(home, &manifest, &public).unwrap());
        let mut custom = public;
        custom["argv"]
            .as_array_mut()
            .unwrap()
            .push(json!("--no-service"));
        assert!(!fixed_recipe_matches(home, &manifest, &custom).unwrap());
        let id = uuid::Uuid::new_v4().to_string();
        manifest["private_derivative"] =
            json!({"schema":1,"kind":"signed_macos_instance","cohort_id":id});
        manifest["native_runtime"] = json!({"kind":"private_derivative","binary":"engine/native/agent-runtime","binary_sha256":"a".repeat(64)});
        manifest["files"]["engine/native/agent-runtime"] = json!("a".repeat(64));
        let raw = serde_json::to_vec(&manifest).unwrap();
        std::fs::write(file, &raw).unwrap();
        let binding = crate::signing_cohort::RuntimeBinding {
            path: root.join("engine/native/agent-runtime"),
            cohort_id: id,
            manifest_sha256: format!("{:x}", Sha256::digest(&raw)),
        };
        let old = recipe(home, &manifest, Some(&binding));
        assert!(fixed_recipe_matches(home, &manifest, &old).unwrap());
        // Old shape recognition cannot authorize setup under the current lock.
        assert!(crate::signing_cohort::runtime(&root).is_err());
        let mut wrong = old;
        wrong["private_cohort"]["manifest_sha256"] = json!("b".repeat(64));
        assert!(!fixed_recipe_matches(home, &manifest, &wrong).unwrap());
    }
    #[test]
    fn unreviewed_derivative_refuses_before_copy_or_host_registration() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("download");
        fixture_bundle(&source);
        let file = source.join("consumer-bundle.json");
        let mut manifest: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        manifest["private_derivative"] =
            json!({"schema":1,"kind":"signed_macos_instance","cohort_id":uuid::Uuid::new_v4()});
        std::fs::write(&file, serde_json::to_vec(&manifest).unwrap()).unwrap();
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
        assert!(!state.home.join("support/instance").exists());
        assert!(state.list("hosts").unwrap().is_empty());
        assert_eq!(
            serde_json::from_slice::<Value>(&std::fs::read(&file).unwrap()).unwrap(),
            manifest
        );
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
