//! Fixed Instance setup, owned by this computer. A saved start is never replayed.
use crate::{
    client::Bootstrap,
    instance_package,
    paired_protocol::{Attachment, InstallRequest, InstallResult, package_lock},
    storage::Store,
};
use amplifier_native_platform as platform;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    io::Read,
    net::{IpAddr, UdpSocket},
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncWriteExt;

fn record_name(id: uuid::Uuid) -> String {
    format!("install-{id}.json")
}
pub fn pending(store: &Store) -> Result<Option<InstallResult>> {
    let Some(request) = store.read::<InstallRequest>("pending-installation.json")? else {
        return Ok(None);
    };
    let record: Value = store
        .read(&record_name(request.id))?
        .context("Missing saved installation intent")?;
    if record["delivered"] == true {
        return Ok(None);
    }
    if let Some(result) = record.get("result") {
        return Ok(Some(serde_json::from_value(result.clone())?));
    }
    Ok(Some(InstallResult { request, state: "unknown".into(),
        message: "Setup was interrupted. Its original folder and request are preserved; the installer has not been repeated.".into(), attachment: None }))
}
pub fn acknowledged(store: &Store, result: &InstallResult) -> Result<()> {
    let name = record_name(result.request.id);
    let mut record: Value = store.read(&name)?.context("Missing installation record")?;
    ensure!(
        record["request"] == json!(result.request),
        "Installation acknowledgement changed identity"
    );
    record["result"] = json!(result);
    record["delivered"] = json!(true);
    store.save(&name, &record)
}
pub async fn install(
    store: &Store,
    bootstrap: &Bootstrap,
    request: &InstallRequest,
) -> Result<InstallResult> {
    request.validate()?;
    ensure!(
        request.observatory_id == bootstrap.observatory_id
            && request.target
                == crate::paired_protocol::target(crate::platform::os(), crate::platform::arch())?,
        "This installation belongs to another computer or Observatory"
    );
    let name = record_name(request.id);
    if let Some(record) = store.read::<Value>(&name)? {
        ensure!(
            record["request"] == json!(request),
            "This request was already used for a different installation"
        );
        if let Some(result) = record.get("result") {
            return Ok(serde_json::from_value(result.clone())?);
        }
        return Ok(InstallResult { request: request.clone(), state:"unknown".into(),
            message:"This installation has a saved start without a confirmed result. It has not been repeated.".into(), attachment:None });
    }
    // This is the dispatch fence. Every future delivery reads this intent.
    store.save(
        &name,
        &json!({"request":request,"state":"started","delivered":false}),
    )?;
    store.save("pending-installation.json", request)?;
    let outcome = install_once(store, bootstrap, request).await;
    let result = match outcome {
        Ok(attachment) => InstallResult {
            request: request.clone(),
            state: "completed".into(),
            message: "Agent installed. Connecting its verified identity to Observatory.".into(),
            attachment: Some(attachment),
        },
        Err(error) => InstallResult {
            request: request.clone(),
            state: "unknown".into(),
            message: format!(
                "Setup needs attention; its original request and files are preserved: {error}"
            )
            .chars()
            .scan(0usize, |bytes, c| {
                *bytes += c.len_utf8();
                (*bytes <= 1000).then_some(c)
            })
            .collect(),
            attachment: None,
        },
    };
    store.save(
        &name,
        &json!({"request":request,"result":result,"delivered":false}),
    )?;
    Ok(result)
}
async fn route_address(bootstrap: &Bootstrap) -> Result<IpAddr> {
    let url = bootstrap.validate()?;
    let destination = tokio::net::lookup_host((
        url.host_str().unwrap(),
        url.port_or_known_default().unwrap(),
    ))
    .await?
    .next()
    .context("Observatory's network address is unavailable")?;
    let socket = UdpSocket::bind(if destination.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    })?;
    // UDP connect chooses the existing route without sending a packet.
    socket.connect(destination)?;
    Ok(socket.local_addr()?.ip())
}
async fn install_once(
    store: &Store,
    bootstrap: &Bootstrap,
    request: &InstallRequest,
) -> Result<Attachment> {
    ensure!(
        route_address(bootstrap).await? == request.bind_address,
        "This computer and Observatory need a shared LAN or private-network route for agent access"
    );
    let package = prepare_package(store, request).await?;
    let agents = store.home.join("agents");
    platform::private_dir(&agents)?;
    let home = agents.join(request.id.to_string());
    ensure!(
        !home.try_exists()?,
        "The requested agent folder already exists; it has not been changed"
    );
    let binary = package.join(if cfg!(windows) {
        "agent-instance.exe"
    } else {
        "agent-instance"
    });
    let stdout_path = store.home.join(format!("install-{}.stdout", request.id));
    let stderr_path = store.home.join(format!("install-{}.stderr", request.id));
    platform::private_create(&stdout_path, b"")?;
    platform::private_create(&stderr_path, b"")?;
    let stdout = std::fs::OpenOptions::new().write(true).open(&stdout_path)?;
    let stderr = std::fs::OpenOptions::new().write(true).open(&stderr_path)?;
    let mut command = tokio::process::Command::new(&binary);
    command
        .args(["setup", "install", "--home"])
        .arg(&home)
        .args([
            "--name",
            &request.name,
            "--bind",
            &request.bind_address.to_string(),
            "--port",
            "0",
            "--no-browser",
        ])
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(not(windows))]
    let mut child = command
        .spawn()
        .context("The packaged agent installer could not start")?;
    #[cfg(windows)]
    let (mut child, _owned_processes) =
        crate::process_tree::ProcessJob::spawn(&mut command).await?;
    #[cfg(unix)]
    let _owned_processes =
        InstallerGroup(child.id().context("Installer exited during launch")? as i32);
    let status = match tokio::time::timeout(Duration::from_secs(300), child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            let _ = child.kill().await;
            anyhow::bail!("The installer did not finish within its setup budget")
        }
    };
    ensure!(
        status.success(),
        "The packaged agent installer did not confirm success"
    );
    let output: Value = read_json(&stdout_path, 65536)?;
    ensure!(
        output["home"].as_str() == home.to_str() && output["running_verified"] == true,
        "The installed agent did not confirm startup"
    );
    let endpoints: Value = read_json(&home.join("endpoints.json"), 65536)?;
    let certificate = PathBuf::from(
        endpoints["trust_certificate_file"]
            .as_str()
            .context("Missing agent trust certificate")?,
    );
    ensure!(
        certificate.starts_with(&home) && !certificate.is_symlink(),
        "Agent trust path changed"
    );
    let ca_pem = read_text(&certificate, 16384)?;
    let credential = read_text(&home.join("secrets/control-token"), 4096)?
        .trim()
        .to_string();
    let attachment = Attachment {
        session_cwd: instance_package::session_directory(&home)?
            .to_str()
            .context("Unsupported installed workspace")?
            .into(),
        instance_id: uuid::Uuid::parse_str(
            output["instance_id"]
                .as_str()
                .context("Missing installed agent identity")?,
        )?,
        origin: endpoints["base_url"]
            .as_str()
            .context("Missing installed agent address")?
            .into(),
        ca_pem,
        credential,
    };
    InstallResult {
        request: request.clone(),
        state: "completed".into(),
        message: String::new(),
        attachment: Some(attachment.clone()),
    }
    .validate()?;
    Ok(attachment)
}
#[cfg(unix)]
struct InstallerGroup(i32);
#[cfg(unix)]
impl Drop for InstallerGroup {
    fn drop(&mut self) {
        // Setup's foreground descendants are ours. The Instance uses its own
        // OS manager registration for the persistent service.
        unsafe {
            libc::kill(-self.0, libc::SIGKILL);
        }
    }
}
fn read_text(path: &Path, limit: u64) -> Result<String> {
    ensure!(!path.is_symlink(), "Setup receipt cannot be a link");
    let file = std::fs::File::open(path)?;
    ensure!(
        file.metadata()?.is_file() && file.metadata()?.len() <= limit,
        "Setup receipt exceeds its limit"
    );
    let mut text = String::new();
    file.take(limit + 1).read_to_string(&mut text)?;
    ensure!(
        text.len() as u64 <= limit,
        "Setup receipt exceeds its limit"
    );
    Ok(text)
}
fn read_json(path: &Path, limit: u64) -> Result<Value> {
    Ok(serde_json::from_str(&read_text(path, limit)?)?)
}

async fn prepare_package(store: &Store, request: &InstallRequest) -> Result<PathBuf> {
    let root = store.home.join("packages");
    platform::private_dir(&root)?;
    let lock = package_lock();
    let destination = root.join(format!(
        "instance-{}-{}",
        lock["version"].as_str().unwrap(),
        request.target
    ));
    if destination.exists() {
        let (manifest, _) = instance_package::inventory(&destination)?;
        ensure!(
            manifest["source"] == lock["source"] && manifest["version"] == lock["version"],
            "The retained agent package has another identity"
        );
        return Ok(destination);
    }
    let archive = root.join(format!("{}.zip", request.id));
    platform::private_create(&archive, b"")?;
    let url = format!(
        "https://github.com/microsoft/amplifier-constellation-instance/releases/download/v{}/agent-instance-consumer-{}.zip",
        lock["version"].as_str().unwrap(),
        request.target
    );
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(5))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        .build()?;
    let mut response = client.get(url).send().await?.error_for_status()?;
    ensure!(
        response.url().scheme() == "https",
        "Package download requires HTTPS"
    );
    let mut output = tokio::fs::OpenOptions::new()
        .write(true)
        .open(&archive)
        .await?;
    let mut digest = Sha256::new();
    let mut total = 0usize;
    while let Some(chunk) = response.chunk().await? {
        total += chunk.len();
        ensure!(
            total <= 1024 * 1024 * 1024,
            "Agent package exceeds its download limit"
        );
        digest.update(&chunk);
        output.write_all(&chunk).await?;
    }
    output.flush().await?;
    drop(output);
    ensure!(
        format!("{:x}", digest.finalize()) == request.package_sha256,
        "Published agent package checksum mismatch"
    );
    let stage = root.join(format!("stage-{}", request.id));
    unpack(&archive, &stage)?;
    let (manifest, _) = instance_package::inventory(&stage)?;
    ensure!(
        manifest["source"] == lock["source"] && manifest["version"] == lock["version"],
        "Published agent package identity mismatch"
    );
    ensure!(
        !destination.exists(),
        "Agent package appeared during setup; it was preserved"
    );
    std::fs::rename(stage, &destination)?;
    std::fs::remove_file(archive)?;
    Ok(destination)
}
fn unpack(archive: &Path, destination: &Path) -> Result<()> {
    ensure!(!destination.exists(), "Package staging already exists");
    platform::private_dir(destination)?;
    let mut source = zip::ZipArchive::new(std::fs::File::open(archive)?)?;
    ensure!(source.len() <= 20_000, "Excessive package inventory");
    let mut total = 0u64;
    let mut seen = HashSet::new();
    for i in 0..source.len() {
        let mut entry = source.by_index(i)?;
        let name = entry.name()?.into_owned();
        let key = if cfg!(target_os = "linux") {
            name.clone()
        } else {
            name.to_lowercase()
        };
        let mode = entry.unix_mode().unwrap_or(0o644);
        ensure!(
            !entry.is_dir()
                && !name.is_empty()
                && !name.contains(['\\', ':'])
                && Path::new(&name)
                    .components()
                    .all(|p| matches!(p, Component::Normal(_)))
                && seen.insert(key)
                && (mode & 0o170000 == 0 || mode & 0o170000 == 0o100000),
            "Invalid agent package entry"
        );
        total = total
            .checked_add(entry.size())
            .context("Excessive package size")?;
        ensure!(
            total <= 1024 * 1024 * 1024,
            "Agent package exceeds its size limit"
        );
        let target = destination.join(name);
        platform::private_dir(target.parent().unwrap())?;
        platform::private_create(&target, b"")?;
        let mut output = std::fs::OpenOptions::new().write(true).open(&target)?;
        let copied = std::io::copy(&mut entry, &mut output)?;
        ensure!(copied == entry.size(), "Incomplete package entry");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                &target,
                std::fs::Permissions::from_mode(if mode & 0o111 != 0 { 0o700 } else { 0o600 }),
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> InstallRequest {
        let target =
            crate::paired_protocol::target(crate::platform::os(), crate::platform::arch()).unwrap();
        InstallRequest {
            schema: 1,
            id: uuid::Uuid::new_v4(),
            observatory_id: uuid::Uuid::new_v4(),
            host_id: uuid::Uuid::new_v4(),
            machine_id: uuid::Uuid::new_v4(),
            pairing_id: uuid::Uuid::new_v4(),
            name: "Agent".into(),
            package_sha256: package_lock()["targets"][&target]["sha256"]
                .as_str()
                .unwrap()
                .into(),
            target,
            bind_address: "127.0.0.1".parse().unwrap(),
        }
    }
    fn bootstrap(r: &InstallRequest) -> Bootstrap {
        Bootstrap {
            schema: 1,
            observatory_url: "https://127.0.0.1:1".into(),
            observatory_id: r.observatory_id,
            observatory_name: "Fixture".into(),
            pairing_id: r.pairing_id,
            expires_at: u64::MAX,
            ca_pem: None,
            install_agents: true,
        }
    }
    #[tokio::test]
    async fn interrupted_dispatch_and_lost_result_keep_one_intent_without_network_or_rerun() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("helper")).unwrap();
        let request = request();
        store
            .save(
                &record_name(request.id),
                &json!({"request":request,"state":"started","delivered":false}),
            )
            .unwrap();
        store.save("pending-installation.json", &request).unwrap();
        let first = install(&store, &bootstrap(&request), &request)
            .await
            .unwrap();
        assert_eq!(first.state, "unknown");
        assert!(first.attachment.is_none());
        assert!(!store.home.join("packages").exists());
        acknowledged(&store, &first).unwrap();
        assert!(pending(&store).unwrap().is_none());
        let second = install(&store, &bootstrap(&request), &request)
            .await
            .unwrap();
        assert_eq!(first, second);
        let mut changed = request.clone();
        changed.name = "Another agent".into();
        assert!(
            install(&store, &bootstrap(&changed), &changed)
                .await
                .is_err()
        );
        assert!(!store.home.join("agents").exists());
    }
    #[test]
    fn completed_receipt_survives_lost_ack_and_reopen_as_same_result() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("helper");
        let store = Store::open(home.clone()).unwrap();
        let request = request();
        let result = InstallResult {
            request: request.clone(),
            state: "completed".into(),
            message: "Ready".into(),
            attachment: Some(Attachment {
                instance_id: uuid::Uuid::new_v4(),
                origin: "https://127.0.0.1:12345/".into(),
                ca_pem: "fixture".into(),
                credential: "a".repeat(64),
                session_cwd: format!("/fixture/agents/{}/workspace", request.id),
            }),
        };
        store
            .save(
                &record_name(request.id),
                &json!({"request":request,"result":result,"delivered":false}),
            )
            .unwrap();
        store.save("pending-installation.json", &request).unwrap();
        drop(store);
        let reopened = Store::open(home).unwrap();
        assert_eq!(pending(&reopened).unwrap(), Some(result.clone()));
        acknowledged(&reopened, &result).unwrap();
        assert!(pending(&reopened).unwrap().is_none());
    }
}
