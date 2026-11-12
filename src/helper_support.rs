//! Retained computer setup downloads from the checked desktop package.
use crate::config;
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

const PLATFORMS: [&str; 6] = [
    "windows-x86_64",
    "windows-aarch64",
    "macos-x86_64",
    "macos-aarch64",
    "linux-x86_64",
    "linux-aarch64",
];

fn digest(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}
fn ordinary(path: &Path, limit: u64) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= limit,
        "Computer setup must use bounded ordinary files"
    );
    Ok(())
}
pub(crate) fn inventory(root: &Path, complete: bool) -> Result<(String, Vec<PathBuf>)> {
    ensure!(
        root.is_dir() && !root.is_symlink(),
        "Computer setup folder is unavailable"
    );
    let file = root.join("manifest.json");
    ordinary(&file, 65536)?;
    let bytes = std::fs::read(&file)?;
    let manifest: Value = serde_json::from_slice(&bytes)?;
    let artifacts = manifest["artifacts"]
        .as_array()
        .context("Invalid computer setup inventory")?;
    ensure!(
        manifest["schema"] == 1 && (5..=6).contains(&artifacts.len()),
        "Incomplete computer setup inventory"
    );
    let mut platforms = HashSet::new();
    let mut names = HashSet::new();
    let mut files = vec![file];
    for artifact in artifacts {
        let platform = artifact["platform"]
            .as_str()
            .context("Missing setup platform")?;
        let name = artifact["filename"]
            .as_str()
            .context("Missing setup file")?;
        ensure!(
            PLATFORMS.contains(&platform)
                && platforms.insert(platform)
                && names.insert(name)
                && name.ends_with(".zip")
                && !name.contains(['/', '\\', ':'])
                && name.len() < 200,
            "Invalid computer setup platform or file"
        );
        let path = root.join(name);
        ordinary(&path, 80 * 1024 * 1024)?;
        ensure!(
            artifact["sha256"].as_str() == Some(digest(&std::fs::read(&path)?).as_str()),
            "Computer setup bytes changed"
        );
        files.push(path);
    }
    ensure!(
        platforms.len() == 6 || (!complete && !platforms.contains("windows-aarch64")),
        "A complete desktop download includes all six computer setup platforms"
    );
    Ok((digest(&bytes), files))
}
pub(crate) fn copy(from: &Path, to: &Path) -> Result<String> {
    let (digest, files) = inventory(from, true)?;
    ensure!(!to.exists(), "Existing computer setup files were preserved");
    config::private_dir(to)?;
    for path in files {
        config::private_write(&to.join(path.file_name().unwrap()), &std::fs::read(path)?)?;
    }
    ensure!(
        inventory(to, true)?.0 == digest,
        "Copied computer setup inventory changed"
    );
    Ok(digest)
}
pub(crate) fn installed(home: &Path) -> Result<Option<String>> {
    let root = home.join("machine-helper-artifacts");
    match std::fs::symlink_metadata(&root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
        Ok(_) => Ok(Some(inventory(&root, false)?.0)),
    }
}
/// Capture a validated legacy support tree once; an explicit existing field must match.
pub(crate) fn capture_baseline(home: &Path, metadata: &mut Value) -> Result<()> {
    let current = serde_json::to_value(installed(home)?)?;
    if metadata.get("helpers_manifest_sha256").is_none() {
        metadata["helpers_manifest_sha256"] = current;
    } else {
        ensure!(
            metadata["helpers_manifest_sha256"] == current,
            "Installed computer setup changed; it was preserved"
        );
    }
    Ok(())
}
pub(crate) fn matches(home: &Path, metadata: &Value) -> Result<()> {
    if metadata.get("helpers_manifest_sha256").is_some() {
        ensure!(
            serde_json::to_value(installed(home)?)? == metadata["helpers_manifest_sha256"],
            "The selected computer setup inventory changed"
        );
    }
    Ok(())
}

pub(crate) fn retain(home: &Path, executable: &Path, manifest: &Value) -> Result<Option<String>> {
    let existing = installed(home)?;
    let Some(expected) = manifest["helpers_manifest_sha256"].as_str() else {
        return Ok(existing);
    };
    let root = executable
        .parent()
        .context("Missing application folder")?
        .join("support/machine-helpers");
    ensure!(
        inventory(&root, true)?.0 == expected,
        "Packaged computer setup inventory changed"
    );
    if let Some(current) = existing {
        ensure!(
            current == expected,
            "Existing computer setup files were preserved; use the app update flow"
        );
        return Ok(Some(current));
    }
    Ok(Some(copy(&root, &home.join("machine-helper-artifacts"))?))
}

/// Restore only the captured helper-support tree, including an interrupted directory move.
pub(crate) fn restore(
    home: &Path,
    operation: &Path,
    previous_digest: Option<&str>,
    candidate_digest: &str,
) -> Result<()> {
    let current = home.join("machine-helper-artifacts");
    let previous = operation.join("previous-helpers");
    let observed = installed(home)?;
    if previous.exists() {
        ensure!(
            Some(inventory(&previous, false)?.0.as_str()) == previous_digest,
            "Previous computer setup inventory changed"
        );
        if current.exists() {
            ensure!(
                observed.as_deref() == Some(candidate_digest),
                "Changed computer setup files were preserved during recovery"
            );
            std::fs::rename(&current, operation.join("failed-helpers"))?;
        }
        std::fs::rename(previous, &current)?;
    } else if observed.as_deref() != previous_digest {
        ensure!(
            previous_digest.is_none() && observed.as_deref() == Some(candidate_digest),
            "Computer setup recovery is incomplete; retained files were preserved"
        );
        std::fs::rename(&current, operation.join("failed-helpers"))?;
    }
    ensure!(
        installed(home)?.as_deref() == previous_digest,
        "Previous computer setup inventory changed"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture(root: &Path) {
        std::fs::create_dir(root).unwrap();
        let mut rows = Vec::new();
        for platform in PLATFORMS {
            let bytes = format!("inert archive for {platform}").into_bytes();
            let name = format!("{platform}.zip");
            std::fs::write(root.join(&name), &bytes).unwrap();
            rows.push(json!({"platform":platform,"filename":name,"sha256":digest(&bytes)}));
        }
        std::fs::write(
            root.join("manifest.json"),
            json!({"schema":1,"artifacts":rows}).to_string(),
        )
        .unwrap();
    }
    #[test]
    fn helper_inventory_refuses_partial_changed_and_unsafe_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("source");
        fixture(&root);
        assert_eq!(inventory(&root, true).unwrap().1.len(), 7);
        std::fs::write(root.join("windows-aarch64.zip"), b"changed").unwrap();
        assert!(inventory(&root, true).is_err());
        std::fs::remove_file(root.join("windows-aarch64.zip")).unwrap();
        assert!(inventory(&root, true).is_err());
    }
    #[test]
    fn rollback_restores_interrupted_move_and_retains_prior_extra_files() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let current = home.join("machine-helper-artifacts");
        fixture(&current);
        std::fs::write(current.join("owner-note"), b"retained").unwrap();
        let expected = installed(home).unwrap().unwrap();
        let operation = home.join("operation");
        std::fs::create_dir(&operation).unwrap();
        std::fs::rename(&current, operation.join("previous-helpers")).unwrap();
        restore(home, &operation, Some(&expected), "candidate-not-yet-moved").unwrap();
        assert_eq!(installed(home).unwrap(), Some(expected));
        assert_eq!(
            std::fs::read(current.join("owner-note")).unwrap(),
            b"retained"
        );
    }
    #[test]
    fn rollback_of_first_helper_support_preserves_failed_tree_and_never_touches_instance() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let current = home.join("machine-helper-artifacts");
        fixture(&current);
        let expected = installed(home).unwrap().unwrap();
        std::fs::create_dir_all(home.join("support/instance")).unwrap();
        std::fs::write(home.join("support/instance/keep"), b"instance").unwrap();
        let operation = home.join("operation");
        std::fs::create_dir(&operation).unwrap();
        restore(home, &operation, None, &expected).unwrap();
        assert_eq!(installed(home).unwrap(), None);
        assert_eq!(
            inventory(&operation.join("failed-helpers"), true)
                .unwrap()
                .0,
            expected
        );
        assert_eq!(
            std::fs::read(home.join("support/instance/keep")).unwrap(),
            b"instance"
        );
        restore(home, &operation, None, &expected).unwrap();
    }
    #[test]
    fn helper_retention_is_exact_and_never_overwrites_existing_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("source");
        fixture(&root);
        let target = temp.path().join("retained");
        let first = copy(&root, &target).unwrap();
        assert_eq!(inventory(&target, true).unwrap().0, first);
        std::fs::write(target.join("owner-note"), b"keep").unwrap();
        assert!(copy(&root, &target).is_err());
        assert_eq!(std::fs::read(target.join("owner-note")).unwrap(), b"keep");
    }
    #[test]
    fn legacy_manual_inventory_is_captured_once_without_rewriting_its_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("machine-helper-artifacts");
        fixture(&root);
        // The first published helper bundle had five platforms, without Windows ARM.
        let manifest_path = root.join("manifest.json");
        let mut manifest: Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest["artifacts"]
            .as_array_mut()
            .unwrap()
            .retain(|row| row["platform"] != "windows-aarch64");
        std::fs::write(&manifest_path, manifest.to_string()).unwrap();
        std::fs::remove_file(root.join("windows-aarch64.zip")).unwrap();
        std::fs::write(root.join("owner-note"), b"manual setup retained").unwrap();
        let before = std::fs::read(&manifest_path).unwrap();
        let mut metadata = json!({"version":"0.11.0","id":"unchanged"});
        capture_baseline(temp.path(), &mut metadata).unwrap();
        assert_eq!(metadata["helpers_manifest_sha256"], digest(&before));
        assert_eq!(metadata["version"], "0.11.0");
        assert_eq!(metadata["id"], "unchanged");
        let captured = metadata.clone();
        capture_baseline(temp.path(), &mut metadata).unwrap();
        assert_eq!(metadata, captured);
        assert_eq!(std::fs::read(&manifest_path).unwrap(), before);
        assert_eq!(
            std::fs::read(root.join("owner-note")).unwrap(),
            b"manual setup retained"
        );
        assert!(inventory(&root, true).is_err());
    }
    #[test]
    fn explicit_baseline_drift_and_explicit_absence_are_not_recaptured() {
        let temp = tempfile::tempdir().unwrap();
        let mut absent = json!({"version":"0.11.0"});
        capture_baseline(temp.path(), &mut absent).unwrap();
        assert!(absent["helpers_manifest_sha256"].is_null());
        let root = temp.path().join("machine-helper-artifacts");
        fixture(&root);
        let before = std::fs::read(root.join("manifest.json")).unwrap();
        for mut metadata in [
            absent,
            json!({"helpers_manifest_sha256":"different-recorded-inventory"}),
        ] {
            let captured = metadata.clone();
            assert!(capture_baseline(temp.path(), &mut metadata).is_err());
            assert_eq!(metadata, captured);
            assert!(matches(temp.path(), &metadata).is_err());
        }
        assert_eq!(std::fs::read(root.join("manifest.json")).unwrap(), before);
    }
    #[test]
    fn staged_corruption_and_changed_selected_inventory_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let staged = temp.path().join("candidate-helpers");
        fixture(&staged);
        let captured = inventory(&staged, true).unwrap().0;
        let archive = staged.join("linux-x86_64.zip");
        let original = std::fs::read(&archive).unwrap();
        std::fs::write(&archive, b"corrupted after staging").unwrap();
        assert!(inventory(&staged, true).is_err());
        std::fs::write(&archive, original).unwrap();
        assert_eq!(inventory(&staged, true).unwrap().0, captured);

        let selected = temp.path().join("machine-helper-artifacts");
        std::fs::rename(&staged, &selected).unwrap();
        let metadata = json!({"helpers_manifest_sha256":captured});
        matches(temp.path(), &metadata).unwrap();
        let manifest_path = selected.join("manifest.json");
        let original = std::fs::read(&manifest_path).unwrap();
        let mut changed: Value = serde_json::from_slice(&original).unwrap();
        changed["artifacts"][0]["version"] = json!("different valid inventory");
        std::fs::write(&manifest_path, changed.to_string()).unwrap();
        assert!(inventory(&selected, true).is_ok());
        assert!(matches(temp.path(), &metadata).is_err());
        std::fs::write(&manifest_path, original).unwrap();
        std::fs::remove_file(selected.join("macos-aarch64.zip")).unwrap();
        assert!(matches(temp.path(), &metadata).is_err());
    }
}
