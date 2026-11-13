//! Verified Instance bundle bytes shared by local and paired-computer setup.
use amplifier_native_platform as platform;
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};

pub(crate) fn ordinary(root: &Path, relative: &str) -> Result<PathBuf> {
    ensure!(
        !relative.is_empty() && !relative.contains(['\\', ':']),
        "Invalid package path"
    );
    let mut path = root.to_path_buf();
    for part in Path::new(relative).components() {
        let Component::Normal(part) = part else {
            anyhow::bail!("Invalid package path")
        };
        path.push(part);
        ensure!(!path.is_symlink(), "Package links are not supported");
    }
    ensure!(path.is_file(), "The agent package is incomplete");
    Ok(path)
}

pub fn inventory(root: &Path) -> Result<(Value, Vec<(String, PathBuf)>)> {
    let file = ordinary(root, "consumer-bundle.json")?;
    ensure!(
        file.metadata()?.len() <= 4 * 1024 * 1024,
        "Excessive package inventory"
    );
    let manifest: Value = serde_json::from_slice(&std::fs::read(file)?)?;
    ensure!(
        manifest["schema"] == 1 && manifest["kind"] == "instance-consumer-bundle",
        "Unsupported agent package"
    );
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    let suffix = if cfg!(windows) {
        "pc-windows-msvc"
    } else if cfg!(target_os = "macos") {
        "apple-darwin"
    } else {
        "unknown-linux-gnu"
    };
    ensure!(
        manifest["target"] == format!("{arch}-{suffix}") && manifest["native_runtime"].is_object(),
        "Use the complete agent package for this computer"
    );
    let expected = if cfg!(windows) {
        "agent-instance.exe"
    } else {
        "agent-instance"
    };
    ensure!(
        manifest["binary"] == expected,
        "The agent package is for another platform"
    );
    let files = manifest["files"]
        .as_object()
        .context("Missing package inventory")?;
    ensure!(
        files.len() <= 20_000 && files.contains_key(expected),
        "Invalid package inventory"
    );
    let mut total = 0u64;
    let mut paths = Vec::new();
    for (name, digest) in files {
        let path = ordinary(root, name)?;
        total += path.metadata()?.len();
        ensure!(
            total <= 1024 * 1024 * 1024,
            "Agent package exceeds its size limit"
        );
        let actual = format!("{:x}", Sha256::digest(std::fs::read(&path)?));
        ensure!(
            digest.as_str() == Some(actual.as_str()),
            "Agent package checksum mismatch: {name}"
        );
        paths.push((name.clone(), path));
    }
    Ok((manifest, paths))
}

pub fn copy_bundle(source: &Path, destination: &Path) -> Result<Value> {
    let (manifest, files) = inventory(source)?;
    ensure!(
        !destination.exists(),
        "An agent package is already installed"
    );
    platform::private_dir(destination)?;
    for (name, source) in files {
        let target = destination.join(name);
        platform::private_dir(target.parent().unwrap())?;
        std::fs::copy(&source, &target)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let executable = source.metadata()?.permissions().mode() & 0o111 != 0;
            std::fs::set_permissions(
                &target,
                std::fs::Permissions::from_mode(if executable { 0o700 } else { 0o600 }),
            )?;
        }
        #[cfg(windows)]
        platform::protect_file(&target)?;
    }
    platform::private_write(
        &destination.join("consumer-bundle.json"),
        &std::fs::read(source.join("consumer-bundle.json"))?,
    )?;
    inventory(destination)?;
    Ok(manifest)
}

/// Read the installation-owned backend selection, keeping the launcher working
/// directory separate from the directory sent in ACP session/new and load.
pub fn session_directory(home: &Path) -> Result<PathBuf> {
    let document: Value = serde_json::from_slice(&std::fs::read(home.join("config.json"))?)?;
    let backend = &document["config"]["backend"];
    let expand = |value: &str| -> Result<PathBuf> {
        let path = if let Some(relative) = value.strip_prefix("${AGENT_HOME}/") {
            home.join(relative)
        } else {
            PathBuf::from(value)
        };
        ensure!(
            path.is_absolute() && !value.contains('\0'),
            "Invalid installed workspace"
        );
        Ok(path.canonicalize()?)
    };
    let workspace = expand(
        backend["working_directory"]
            .as_str()
            .context("The installed agent has no working folder")?,
    )?;
    ensure!(
        workspace.is_dir() && workspace.starts_with(home.canonicalize()?),
        "The installed workspace is outside this installation"
    );
    let roots = backend["allowed_working_roots"]
        .as_array()
        .context("Missing installed working roots")?;
    ensure!(
        roots
            .iter()
            .filter_map(Value::as_str)
            .map(expand)
            .collect::<Result<Vec<_>>>()?
            .iter()
            .any(|root| workspace.starts_with(root)),
        "The installed workspace is outside its permitted working roots"
    );
    Ok(workspace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn installed_workspace_comes_from_backend_without_expanding_allowed_roots() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("agent");
        std::fs::create_dir_all(home.join("workspace")).unwrap();
        let mut config = json!({"config":{"backend":{"working_directory":"${AGENT_HOME}/workspace","allowed_working_roots":["${AGENT_HOME}/workspace"]}}});
        std::fs::write(home.join("config.json"), config.to_string()).unwrap();
        assert_eq!(
            session_directory(&home).unwrap(),
            home.join("workspace").canonicalize().unwrap()
        );
        config["config"]["backend"]["working_directory"] = json!(home);
        std::fs::write(home.join("config.json"), config.to_string()).unwrap();
        assert!(session_directory(&home).is_err());
        config["config"]["backend"]["working_directory"] = json!(dir.path());
        config["config"]["backend"]["allowed_working_roots"] = json!([dir.path()]);
        std::fs::write(home.join("config.json"), config.to_string()).unwrap();
        assert!(session_directory(&home).is_err());
    }
}
