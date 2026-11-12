//! One local, installation-owned replacement. No scheduler or agent operation.
use crate::{
    auth, config, desktop,
    error::{ApiError, ApiResult},
    state::AppState,
};
use amplifier_native_platform as platform;
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::{Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::any,
};
use fs2::FileExt;
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    time::Duration,
};

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}
fn save(path: &Path, value: &Value) -> Result<()> {
    config::private_write(path, &serde_json::to_vec(value)?)
}
fn binary_name() -> &'static str {
    if cfg!(windows) {
        "agent-observatory.exe"
    } else {
        "agent-observatory"
    }
}
fn candidate_binary(folder: &Path) -> PathBuf {
    folder.join(if cfg!(windows) {
        "candidate.exe"
    } else {
        "candidate"
    })
}
fn target() -> String {
    format!(
        "{}-{}",
        std::env::consts::ARCH,
        if cfg!(windows) {
            "pc-windows-msvc"
        } else if cfg!(target_os = "macos") {
            "apple-darwin"
        } else {
            "unknown-linux-gnu"
        }
    )
}
fn ordinary(path: &Path) -> Result<()> {
    ensure!(
        path.is_file() && !path.is_symlink(),
        "Use ordinary application files"
    );
    Ok(())
}
fn copy_binary(from: &Path, to: &Path) -> Result<()> {
    ordinary(from)?;
    config::private_dir(to.parent().context("Missing application folder")?)?;
    config::private_write(to, &std::fs::read(from)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
fn lock(home: &Path, name: &str) -> Result<File> {
    let path = home.join(name);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    platform::protect_file(&path)?;
    file.try_lock_exclusive()
        .context("Another local update is already working")?;
    Ok(file)
}
fn worker_active(home: &Path) -> bool {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(home.join("desktop-update.lock"))
        .map(|file| file.try_lock_exclusive().is_err())
        .unwrap_or(true)
}
fn journal(home: &Path) -> Result<Value> {
    read(&home.join("desktop-update.json"))
}
fn write_journal(home: &Path, value: &Value) -> Result<()> {
    save(&home.join("desktop-update.json"), value)
}
fn operation(home: &Path, id: &str) -> Result<PathBuf> {
    uuid::Uuid::parse_str(id)?;
    Ok(home.join("app-updates").join(id))
}
fn phase(home: &Path, value: &mut Value, next: &str) -> Result<()> {
    value["phase"] = json!(next);
    write_journal(home, value)
}
fn complete(value: &Value) -> bool {
    matches!(value["phase"].as_str(), Some("applied" | "rolled_back"))
}

pub fn package(executable: &Path) -> Result<Value> {
    ordinary(executable)?;
    let root = executable.parent().context("Missing application folder")?;
    let path = root.join("observatory-package.json");
    ordinary(&path)?;
    ensure!(
        path.metadata()?.len() < 65536,
        "Invalid application inventory"
    );
    let manifest = read(&path)?;
    ensure!(
        manifest["schema"] == 1
            && manifest["component"] == "agent-observatory"
            && manifest["target"] == target()
            && manifest["binary"] == binary_name(),
        "Use the matching complete Observatory package"
    );
    ensure!(
        manifest["binary_sha256"] == sha(&std::fs::read(executable)?),
        "Observatory package bytes changed"
    );
    if let Some(expected) = manifest["instance_manifest_sha256"].as_str() {
        let folder = root.join("support/instance");
        ensure!(
            sha(&std::fs::read(folder.join("consumer-bundle.json"))?) == expected,
            "Bundled agent inventory changed"
        );
        crate::local_install::inventory(&folder)?;
    }
    Ok(manifest)
}

/// Retained independently of the optional sign-in startup choice.
pub fn retain(state: &AppState) -> Result<()> {
    let current = std::env::current_exe()?;
    let destination = platform::installed_binary(&platform::OBSERVATORY, &state.home);
    if !destination.exists() {
        copy_binary(&current, &destination)?;
    }
    ensure!(
        sha(&std::fs::read(&destination)?) == sha(&std::fs::read(&current)?),
        "Existing Observatory executable was preserved"
    );
    let manifest = if current
        .parent()
        .unwrap()
        .join("observatory-package.json")
        .exists()
    {
        Some(package(&current)?)
    } else {
        None
    }; // Source/dev launches do not supply update packages.
    let host = state
        .list("hosts")?
        .into_iter()
        .find(|h| h["native_local"] == true);
    let recipe = host
        .as_ref()
        .and_then(|h| h["recipes"].as_array())
        .and_then(|rs| rs.iter().find(|r| r["id"] == "bundled-instance"))
        .cloned();
    let instance_manifest = state.home.join("support/instance/consumer-bundle.json");
    let instance_digest = if instance_manifest.exists() {
        Some(sha(&std::fs::read(instance_manifest)?))
    } else {
        None
    };
    save(
        &state.home.join("app-install.json"),
        &json!({"schema":1,"id":state.config.id,"version":env!("CARGO_PKG_VERSION"),"binary_sha256":sha(&std::fs::read(&current)?),"package":manifest,"instance_manifest_sha256":instance_digest,"host_id":host.as_ref().map(|h|h["id"].clone()),"recipe":recipe}),
    )
}

pub fn pending(home: &Path) -> bool {
    journal(home).is_ok_and(|j| !complete(&j))
}
pub fn direct_start_allowed(home: &Path) -> Result<()> {
    if let Ok(j) = journal(home) {
        ensure!(
            complete(&j) || matches!(j["phase"].as_str(), Some("prepared" | "deferred")),
            "An application update needs recovery. Open Observatory to continue the same operation."
        );
    }
    Ok(())
}
fn version_numbers(value: &Value) -> Result<Vec<u64>> {
    let parts = value
        .as_str()
        .context("Missing application version")?
        .split('.')
        .map(str::parse)
        .collect::<std::result::Result<Vec<u64>, _>>()?;
    ensure!(parts.len() == 3, "Unsupported application version");
    Ok(parts)
}
pub fn pending_verification(home: &Path) -> bool {
    journal(home)
        .is_ok_and(|j| matches!(j["phase"].as_str(), Some("starting" | "rollback_starting")))
}
pub fn busy(state: &AppState) -> bool {
    state.operation_gate.is_closed() || state.update_hold.lock().unwrap().is_some()
}

/// Only an installation-bound, staged local transaction can request shutdown.
pub fn stop(state: &AppState, body: &Value) -> ApiResult<Value> {
    let _talk = state.talk_gate.lock().unwrap();
    let _provision = state.provision_gate.lock().unwrap();
    let j =
        journal(&state.home).map_err(|_| ApiError::bad("No local application update is staged"))?;
    if body["request_id"] != j["request_id"]
        || j["installation_id"] != state.config.id
        || body["version"] != env!("CARGO_PKG_VERSION")
        || complete(&j)
    {
        return Err(ApiError::bad("The local update identity changed"));
    }
    let digest = desktop::binary_digest().map_err(ApiError::from)?;
    if j["previous"]["binary_sha256"] != digest && j["candidate"]["binary_sha256"] != digest {
        return Err(ApiError::bad("This is a different application build"));
    }
    if !state.active_sessions.lock().unwrap().is_empty()
        || state
            .list("jobs")?
            .iter()
            .any(|j| matches!(j["status"].as_str(), Some("queued" | "running")))
    {
        return Err(ApiError::bad(
            "Observatory is still working. Let the current conversation or installation finish, then try again.",
        ));
    }
    let _permit = if state.update_hold.lock().unwrap().is_some() {
        None
    } else {
        Some(
            state
                .operation_gate
                .clone()
                .try_acquire_many_owned(4)
                .map_err(|_| {
                    ApiError::bad(
                        "Observatory is still completing an operation. Try again shortly.",
                    )
                })?,
        )
    };
    state.operation_gate.close();
    state.shutdown.send_replace(true);
    Ok(json!({"accepted":true,"request_id":j["request_id"],"id":state.config.id}))
}
pub fn finish(state: &AppState, body: &Value) -> ApiResult<Value> {
    let j = journal(&state.home).map_err(|_| ApiError::bad("No local update receipt"))?;
    if body["request_id"] != j["request_id"]
        || j["installation_id"] != state.config.id
        || !complete(&j)
    {
        return Err(ApiError::bad("Update has not been verified"));
    }
    let selected = if j["phase"] == "applied" {
        &j["candidate"]
    } else {
        &j["previous"]
    };
    if selected["version"] != env!("CARGO_PKG_VERSION")
        || selected["binary_sha256"] != desktop::binary_digest().map_err(ApiError::from)?
    {
        return Err(ApiError::bad(
            "The verified application differs from this process",
        ));
    }
    state.update_hold.lock().unwrap().take();
    Ok(json!({"ready":true,"id":state.config.id}))
}

fn recipe_matches(home: &Path, installed: &Value) -> Result<()> {
    if installed["recipe"].is_null() {
        return Ok(());
    }
    let db = rusqlite::Connection::open(home.join("state.sqlite3"))?;
    let raw: Option<String> = db
        .query_row(
            "SELECT value FROM records WHERE kind='hosts' AND id=?1",
            [installed["host_id"]
                .as_str()
                .context("Missing owned computer")?],
            |r| r.get(0),
        )
        .optional()?;
    let host: Value = serde_json::from_str(raw.as_deref().context("Owned computer changed")?)?;
    ensure!(
        host["native_local"] == true
            && host["recipes"]
                .as_array()
                .is_some_and(|rs| rs.iter().any(|r| r == &installed["recipe"])),
        "The bundled setup recipe was changed; existing settings were preserved"
    );
    Ok(())
}
fn set_recipe(home: &Path, old: &Value, new: &Value) -> Result<()> {
    if old["recipe"].is_null() {
        return Ok(());
    }
    let mut db = rusqlite::Connection::open(home.join("state.sqlite3"))?;
    let tx = db.transaction()?;
    let raw: String = tx.query_row(
        "SELECT value FROM records WHERE kind='hosts' AND id=?1",
        [old["host_id"].as_str().context("Missing computer")?],
        |r| r.get(0),
    )?;
    let mut host: Value = serde_json::from_str(&raw)?;
    let recipes = host["recipes"].as_array_mut().context("Missing recipes")?;
    let recipe = recipes
        .iter_mut()
        .find(|r| **r == old["recipe"])
        .context("The owned setup recipe changed")?;
    *recipe = new["recipe"].clone();
    host["revision"] = json!(host["revision"].as_u64().unwrap_or(0) + 1);
    host["updated_at"] = json!(crate::state::now_ms());
    tx.execute(
        "UPDATE records SET value=?1 WHERE kind='hosts' AND id=?2",
        rusqlite::params![host.to_string(), old["host_id"].as_str().unwrap()],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn stage(home: &Path, executable: &Path) -> Result<Value> {
    let _lock = lock(home, "desktop-update.lock")?;
    let package = package(executable)?;
    ensure!(
        package["version"] == env!("CARGO_PKG_VERSION"),
        "The package version differs from this application"
    );
    let c = config::Config::load(home)?;
    let installed = read(&home.join("app-install.json")).context(
        "This older installation has no retained update baseline. Its files were preserved.",
    )?;
    ensure!(installed["id"] == c.id, "Installation identity changed");
    ensure!(
        version_numbers(&package["version"])? >= version_numbers(&installed["version"])?,
        "An older download cannot replace this Observatory. Its files were preserved."
    );
    let old = platform::installed_binary(&platform::OBSERVATORY, home);
    ordinary(&old)?;
    ensure!(
        installed["binary_sha256"] == sha(&std::fs::read(&old)?),
        "Installed application bytes changed; they were preserved"
    );
    if let Ok(j) = journal(home)
        && !complete(&j)
    {
        ensure!(
            j["candidate"]["binary_sha256"] == package["binary_sha256"],
            "Another update is pending; finish it before opening a different download"
        );
        return Ok(j);
    }
    let id = uuid::Uuid::new_v4().to_string();
    let folder = operation(home, &id)?;
    config::private_dir(&folder)?;
    copy_binary(executable, &candidate_binary(&folder))?;
    copy_binary(&old, &folder.join("previous"))?;
    let mut candidate = installed.clone();
    candidate["version"] = package["version"].clone();
    candidate["binary_sha256"] = package["binary_sha256"].clone();
    candidate["package"] = package.clone();
    let replace_support = package["instance_manifest_sha256"].is_string()
        && installed["recipe"].is_object()
        && package["instance_manifest_sha256"] != installed["instance_manifest_sha256"];
    if replace_support {
        let root = executable.parent().unwrap().join("support/instance");
        crate::local_install::copy_bundle(&root, &folder.join("candidate-instance"))?;
        let old_manifest = home.join("support/instance/consumer-bundle.json");
        ensure!(
            installed["instance_manifest_sha256"] == sha(&std::fs::read(&old_manifest)?),
            "Installed agent setup was changed; it was preserved"
        );
        crate::local_install::inventory(&home.join("support/instance"))?;
        candidate["instance_manifest_sha256"] = package["instance_manifest_sha256"].clone();
        candidate["recipe"]["version"] =
            read(&root.join("consumer-bundle.json"))?["version"].clone();
    }
    let j = json!({"schema":1,"request_id":id,"installation_id":c.id,"phase":"prepared","previous":installed,"candidate":candidate,"replace_support":replace_support});
    write_journal(home, &j)?;
    Ok(j)
}

async fn stopped(home: &Path, j: &Value) -> Result<File> {
    if let Ok(status) = desktop::local_api(home, "GET", "status", None).await {
        ensure!(
            status["id"] == j["installation_id"],
            "A different Observatory answered"
        );
        let _ = desktop::local_api(
            home,
            "POST",
            "native-update/stop",
            Some(json!({"request_id":j["request_id"],"version":status["version"]})),
        )
        .await;
    }
    for _ in 0..100 {
        if let Ok(l) = lock(home, "service.lock") {
            return Ok(l);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    anyhow::bail!(
        "Observatory is still working, or this older version cannot close safely. Nothing was replaced; try again after it finishes."
    )
}
pub fn validate_start(home: &Path, id: &str, nonce: &str) -> Result<()> {
    let j = journal(home)?;
    ensure!(
        j["request_id"] == id
            && j["launch_nonce"] == nonce
            && matches!(j["phase"].as_str(), Some("starting" | "rollback_starting")),
        "This update startup is no longer authorized"
    );
    let selected = if j["phase"] == "starting" {
        &j["candidate"]
    } else {
        &j["previous"]
    };
    ensure!(
        selected["version"] == env!("CARGO_PKG_VERSION")
            && selected["binary_sha256"] == desktop::binary_digest()?,
        "Update startup bytes changed"
    );
    Ok(())
}
fn start(home: &Path, j: &mut Value) -> Result<std::process::Child> {
    let nonce = auth::random_token();
    j["launch_nonce"] = json!(nonce);
    j["child_pid"] = Value::Null;
    write_journal(home, j)?;
    let mut cmd = Command::new(platform::installed_binary(&platform::OBSERVATORY, home));
    cmd.arg("--home")
        .arg(home)
        .arg("serve-update")
        .arg(j["request_id"].as_str().unwrap())
        .arg(nonce)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x00000200 | 0x00000008);
    }
    let mut child = cmd.spawn()?;
    j["child_pid"] = json!(child.id());
    if let Err(error) = write_journal(home, j) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    Ok(child)
}
fn stop_child(child: &mut std::process::Child) -> Result<()> {
    if child.try_wait()?.is_none() {
        child.kill()?;
    }
    child.wait()?;
    Ok(())
}
async fn healthy(
    home: &Path,
    j: &Value,
    metadata: &Value,
    mut child: Option<&mut std::process::Child>,
) -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(45);
    while std::time::Instant::now() < deadline {
        if let Some(child) = child.as_deref_mut() {
            ensure!(
                child.try_wait()?.is_none(),
                "The replacement exited before confirming startup"
            );
        }
        if let Ok(s) = desktop::local_api(home, "GET", "status", None).await
            && s["id"] == j["installation_id"]
            && s["version"] == metadata["version"]
            && s["binary_sha256"] == metadata["binary_sha256"]
            && s["pid"] == j["child_pid"]
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    anyhow::bail!(
        "The spawned replacement did not confirm this Observatory's identity, version and binary"
    )
}

async fn settle(home: &Path, j: &Value) -> Result<()> {
    desktop::local_api(
        home,
        "POST",
        "native-update/finish",
        Some(json!({"request_id":j["request_id"]})),
    )
    .await?;
    Ok(())
}
async fn rollback(home: &Path, j: &mut Value, folder: &Path) -> Result<()> {
    // Invalidate a candidate that was spawned but has not yet acquired the
    // service lock. It must check this nonce after acquiring that lock.
    phase(home, j, "recovering")?;
    let service = stopped(home, j)
        .await
        .context("The replacement still owns this installation; recovery was preserved")?;
    let current = platform::installed_binary(&platform::OBSERVATORY, home);
    let digest = sha(&std::fs::read(&current)?);
    ensure!(
        j["candidate"]["binary_sha256"] == digest || j["previous"]["binary_sha256"] == digest,
        "Application bytes changed during recovery; they were preserved"
    );
    copy_binary(&folder.join("previous"), &current)?;
    if j["replace_support"] == true {
        let current = home.join("support/instance");
        let previous = folder.join("previous-instance");
        if previous.exists() {
            if current.exists() {
                ensure!(
                    sha(&std::fs::read(current.join("consumer-bundle.json"))?)
                        == j["candidate"]["instance_manifest_sha256"],
                    "Changed agent setup was preserved during recovery"
                );
                crate::local_install::inventory(&current)?;
                let failed = folder.join("failed-instance");
                ensure!(
                    !failed.exists(),
                    "Retained failed setup already exists; inspect it before recovery"
                );
                std::fs::rename(&current, failed)?;
            }
            std::fs::rename(previous, &current)?;
        }
        ensure!(
            sha(&std::fs::read(current.join("consumer-bundle.json"))?)
                == j["previous"]["instance_manifest_sha256"],
            "Previous setup inventory changed"
        );
        if recipe_matches(home, &j["previous"]).is_err() {
            set_recipe(home, &j["candidate"], &j["previous"])?;
        }
    }
    save(&home.join("app-install.json"), &j["previous"])?;
    phase(home, j, "rollback_starting")?;
    drop(service);
    let mut child = start(home, j)?;
    if let Err(error) = healthy(home, j, &j["previous"], Some(&mut child)).await {
        stop_child(&mut child)?;
        return Err(error);
    }
    phase(home, j, "rolled_back")?;
    settle(home, j).await
}

pub async fn apply(home: &Path, id: &str) -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let _update = loop {
        match lock(home, "desktop-update.lock") {
            Ok(guard) => break guard,
            Err(error) => {
                if std::time::Instant::now() >= deadline {
                    return Err(error);
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    };
    let mut j = journal(home)?;
    ensure!(
        j["schema"] == 1 && j["installation_id"] == config::Config::load(home)?.id,
        "Update installation identity changed"
    );
    ensure!(j["request_id"] == id, "Update request changed");
    let folder = operation(home, id)?;
    if complete(&j) {
        if settle(home, &j).await.is_ok() {
            return Ok(());
        }
        // A verified application may have closed after the acknowledgement was
        // lost. Restart those same installed bytes, without switching or replay.
        let service = lock(home, "service.lock")?;
        let applied = j["phase"] == "applied";
        let selected = if applied {
            j["candidate"].clone()
        } else {
            j["previous"].clone()
        };
        ensure!(
            sha(&std::fs::read(platform::installed_binary(
                &platform::OBSERVATORY,
                home
            ))?) == selected["binary_sha256"],
            "Verified application bytes changed; they were preserved"
        );
        phase(
            home,
            &mut j,
            if applied {
                "starting"
            } else {
                "rollback_starting"
            },
        )?;
        drop(service);
        let mut child = start(home, &mut j)?;
        if let Err(error) = healthy(home, &j, &selected, Some(&mut child)).await {
            stop_child(&mut child)?;
            return Err(error);
        }
        phase(
            home,
            &mut j,
            if applied { "applied" } else { "rolled_back" },
        )?;
        return settle(home, &j).await;
    }
    ensure!(
        sha(&std::fs::read(candidate_binary(&folder))?) == j["candidate"]["binary_sha256"],
        "Staged application changed"
    );
    ensure!(
        sha(&std::fs::read(folder.join("previous"))?) == j["previous"]["binary_sha256"],
        "Retained previous application changed"
    );
    if !matches!(j["phase"].as_str(), Some("prepared" | "deferred")) {
        // A lost response after successful startup only settles the original operation.
        if j["phase"] == "starting" && healthy(home, &j, &j["candidate"], None).await.is_ok() {
            phase(home, &mut j, "applied")?;
            return settle(home, &j).await;
        }
        if j["phase"] == "rollback_starting"
            && healthy(home, &j, &j["previous"], None).await.is_ok()
        {
            phase(home, &mut j, "rolled_back")?;
            return settle(home, &j).await;
        }
        return rollback(home, &mut j, &folder).await;
    }
    let service = match stopped(home, &j).await {
        Ok(service) => service,
        Err(error) => {
            j["error"] = json!(error.to_string());
            phase(home, &mut j, "deferred")?;
            return Err(error);
        }
    };
    if let Err(error) = recipe_matches(home, &j["previous"]) {
        drop(service);
        phase(home, &mut j, "rollback_starting")?;
        let mut child = start(home, &mut j)?;
        if let Err(error) = healthy(home, &j, &j["previous"], Some(&mut child)).await {
            stop_child(&mut child)?;
            return Err(error);
        }
        j["error"] = json!(error.to_string());
        phase(home, &mut j, "rolled_back")?;
        settle(home, &j).await?;
        return Err(error);
    }
    phase(home, &mut j, "switching")?;
    let switched: Result<()> = (|| {
        if j["replace_support"] == true {
            let current = home.join("support/instance");
            ensure!(
                sha(&std::fs::read(current.join("consumer-bundle.json"))?)
                    == j["previous"]["instance_manifest_sha256"],
                "Installed agent setup changed"
            );
            crate::local_install::inventory(&current)?;
            std::fs::rename(&current, folder.join("previous-instance"))?;
            std::fs::rename(folder.join("candidate-instance"), &current)?;
            set_recipe(home, &j["previous"], &j["candidate"])?;
        }
        copy_binary(
            &candidate_binary(&folder),
            &platform::installed_binary(&platform::OBSERVATORY, home),
        )?;
        save(&home.join("app-install.json"), &j["candidate"])?;
        Ok(())
    })();
    if switched.is_ok() {
        phase(home, &mut j, "starting")?;
    }
    drop(service);
    let result = match switched {
        Ok(()) => match start(home, &mut j) {
            Ok(mut child) => match healthy(home, &j, &j["candidate"], Some(&mut child)).await {
                Ok(()) => Ok(()),
                Err(error) => {
                    stop_child(&mut child)?;
                    Err(error)
                }
            },
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        j["error"] = json!(error.to_string());
        write_journal(home, &j)?;
        rollback(home, &mut j, &folder).await?;
        return Err(error);
    }
    phase(home, &mut j, "applied")?;
    // A lost acknowledgement here never triggers rollback of verified success.
    settle(home, &j).await
}

struct Offer {
    home: PathBuf,
    origin: String,
    token: String,
    id: String,
    expires_at: i64,
    shutdown: tokio::sync::watch::Sender<bool>,
}
pub async fn offer(home: PathBuf, no_browser: bool) -> Result<()> {
    let j = if let Ok(j) = journal(&home)
        && !complete(&j)
    {
        let digest = desktop::binary_digest()?;
        ensure!(
            j["installation_id"] == config::Config::load(&home)?.id
                && (j["candidate"]["binary_sha256"] == digest
                    || j["previous"]["binary_sha256"] == digest),
            "Finish the pending update using its original download or installed application"
        );
        j
    } else {
        stage(&home, &std::env::current_exe()?)?
    };
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let origin = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
    let token = auth::random_token();
    let url = format!("{origin}/#{token}");
    if no_browser {
        println!(
            "{}",
            json!({"state":"update_available","url":url,"version":j["candidate"]["version"]})
        );
    } else {
        platform::open_browser(&url)?;
    }
    let state = Arc::new(Offer {
        home,
        origin,
        token,
        id: j["request_id"].as_str().unwrap().into(),
        expires_at: crate::state::now_ms() + 30 * 60 * 1000,
        shutdown: tokio::sync::watch::channel(false).0,
    });
    let mut shutdown = state.shutdown.subscribe();
    axum::serve(
        listener,
        Router::new().fallback(any(offer_request)).with_state(state),
    ).with_graceful_shutdown(async move {tokio::select!{_=shutdown.changed()=>{},_=tokio::time::sleep(Duration::from_secs(1800))=>{}}})
    .await?;
    Ok(())
}
async fn offer_request(state: State<Arc<Offer>>, request: Request) -> Response {
    let mut response = offer_request_inner(state, request).await;
    let h = response.headers_mut();
    h.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    h.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    h.insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
    h.insert(header::CONTENT_SECURITY_POLICY, "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'".parse().unwrap());
    response
}
async fn offer_request_inner(State(s): State<Arc<Offer>>, request: Request) -> Response {
    let headers = request.headers();
    if headers.get(header::HOST).and_then(|h| h.to_str().ok()) != s.origin.strip_prefix("http://")
        || headers
            .get(header::ORIGIN)
            .is_some_and(|h| h.to_str().ok() != Some(s.origin.as_str()))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    if headers
        .get("sec-fetch-site")
        .is_some_and(|h| !matches!(h.to_str().ok(), Some("same-origin" | "none")))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let path = request.uri().path();
    if path == "/" && request.method() == "GET" {
        return axum::response::Html(include_str!("../ui/desktop-update.html")).into_response();
    }
    if path == "/update.js" && request.method() == "GET" {
        return (
            [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
            include_str!("../ui/desktop-update.js"),
        )
            .into_response();
    }
    if path == "/setup.css" && request.method() == "GET" {
        return (
            [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
            include_str!("../ui/desktop-setup.css"),
        )
            .into_response();
    }
    if s.expires_at <= crate::state::now_ms()
        || headers
            .get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            != Some(format!("Bearer {}", s.token).as_str())
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let result = match (request.method().as_str(), path) {
        ("GET", "/status") => match journal(&s.home) {
            Ok(j) if j["request_id"] == s.id => {
                let ready = if complete(&j) {
                    settle(&s.home, &j).await.is_ok()
                } else {
                    false
                };
                config::Config::load(&s.home).map(|c|json!({"phase":if complete(&j)&&!ready {json!("finishing")}else{j["phase"].clone()},"version":if j["phase"]=="rolled_back" {j["previous"]["version"].clone()} else {j["candidate"]["version"].clone()},"error":j["error"],"working":worker_active(&s.home),"url":c.public_url}))
            }
            Ok(_) => Err(anyhow::anyhow!("A newer update was opened")),
            Err(e) => Err(e),
        },
        ("POST", "/close") => {
            s.shutdown.send_replace(true);
            Ok(json!({"closed":true}))
        }
        ("POST", "/apply") => launch_worker(&s.home, &s.id).map(|_| json!({"started":true})),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::CONFLICT, Json(json!({"error":e.to_string()}))).into_response(),
    }
}
fn launch_worker(home: &Path, id: &str) -> Result<()> {
    let _lock = lock(home, "desktop-update.lock")?;
    let j = journal(home)?;
    ensure!(j["request_id"] == id, "Update request changed");
    let executable = candidate_binary(&operation(home, id)?);
    ensure!(
        sha(&std::fs::read(&executable)?) == j["candidate"]["binary_sha256"],
        "Staged application changed"
    );
    Command::new(executable)
        .arg("--home")
        .arg(home)
        .arg("apply-update")
        .arg(id)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}
