//! Fixed distribution resources. Installed homes keep their existing layout.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

const MANIFEST: &str = "observatory-package.json";

pub(crate) fn resource_root(executable: &Path) -> Result<PathBuf> {
    resource_root_for(executable, cfg!(target_os = "macos"))
}

fn manifest(path: &Path) -> Result<Value> {
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "Use an ordinary application inventory"
    );
    ensure!(metadata.len() < 65536, "Invalid application inventory");
    let mut bytes = Vec::new();
    File::open(path)?.take(65536).read_to_end(&mut bytes)?;
    ensure!(bytes.len() < 65536, "Invalid application inventory");
    Ok(serde_json::from_slice(&bytes)?)
}

fn resource_root_for(executable: &Path, macos: bool) -> Result<PathBuf> {
    let flat = executable.parent().context("Missing application folder")?;
    if !macos {
        return Ok(flat.to_owned());
    }
    let contents = flat.parent().filter(|p| {
        flat.file_name().is_some_and(|n| n == "MacOS")
            && p.file_name().is_some_and(|n| n == "Contents")
            && p.parent()
                .is_some_and(|app| app.extension().is_some_and(|e| e == "app"))
    });
    let Some(contents) = contents else {
        return Ok(flat.to_owned());
    };
    let resources = contents.join("Resources");
    match std::fs::symlink_metadata(&resources) {
        Ok(metadata) => {
            ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "Use ordinary Mac application resources"
            );
            // A present new inventory is authoritative. Legacy apps may have
            // unrelated Resources files without moving their flat inventory.
            match std::fs::symlink_metadata(resources.join(MANIFEST)) {
                Ok(_) => {
                    let selected = manifest(&resources.join(MANIFEST))?;
                    ensure!(
                        selected["schema"] == 1
                            && selected["component"] == "agent-observatory"
                            && selected["mac_bundle_layout"] == 1,
                        "Use the complete Mac application resources"
                    );
                    return Ok(resources);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    // A new launcher-backed app cannot silently use a legacy sidecar after its
    // Resources inventory disappears. No unrelated legacy Resources is read.
    match std::fs::symlink_metadata(flat.join("Observatory")) {
        Ok(_) => anyhow::bail!("The Mac application resources are missing"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    let legacy = flat.join(MANIFEST);
    match std::fs::symlink_metadata(&legacy) {
        Ok(_) => ensure!(
            manifest(&legacy)?.get("mac_bundle_layout").is_none(),
            "The Mac application resources are missing"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    Ok(flat.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let exe = temp
            .path()
            .join("Observatory.app/Contents/MacOS/agent-observatory");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"fixed service").unwrap();
        (temp, exe)
    }
    fn save(root: &Path, value: Value) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join(MANIFEST), serde_json::to_vec(&value).unwrap()).unwrap();
    }
    #[test]
    fn mac_resources_are_fixed_and_legacy_flat_is_explicit() {
        let (_temp, exe) = fixture();
        let flat = exe.parent().unwrap();
        save(flat, json!({"schema":1,"component":"agent-observatory"}));
        assert_eq!(resource_root_for(&exe, true).unwrap(), flat);
        let resources = flat.parent().unwrap().join("Resources");
        std::fs::create_dir(&resources).unwrap();
        std::fs::write(resources.join("legacy-icon"), b"preserve").unwrap();
        assert_eq!(resource_root_for(&exe, true).unwrap(), flat);
        save(
            &resources,
            json!({"schema":1,"component":"agent-observatory","mac_bundle_layout":1}),
        );
        assert_eq!(resource_root_for(&exe, true).unwrap(), resources);
        assert_eq!(resource_root_for(&exe, false).unwrap(), flat);
    }
    #[test]
    fn missing_malformed_and_wrong_new_resources_never_fall_back() {
        let (_temp, exe) = fixture();
        let flat = exe.parent().unwrap();
        save(flat, json!({"schema":1,"component":"agent-observatory"}));
        let resources = flat.parent().unwrap().join("Resources");
        std::fs::create_dir(&resources).unwrap();
        std::fs::write(flat.join("Observatory"), b"new GUI launcher").unwrap();
        assert!(resource_root_for(&exe, true).is_err());
        std::fs::remove_file(flat.join("Observatory")).unwrap();
        for bytes in [
            b"{".as_slice(),
            br#"{"schema":1,"component":"agent-observatory","mac_bundle_layout":2}"#,
            br#"{"schema":1,"component":"other","mac_bundle_layout":1}"#,
        ] {
            std::fs::write(resources.join(MANIFEST), bytes).unwrap();
            assert!(resource_root_for(&exe, true).is_err());
        }
        std::fs::remove_dir_all(&resources).unwrap();
        save(
            flat,
            json!({"schema":1,"component":"agent-observatory","mac_bundle_layout":1}),
        );
        assert!(resource_root_for(&exe, true).is_err());
    }
    #[test]
    fn installed_home_and_nonmac_layout_stay_flat() {
        let temp = tempfile::tempdir().unwrap();
        let exe = temp.path().join("bin/agent-observatory");
        assert_eq!(
            resource_root_for(&exe, true).unwrap(),
            exe.parent().unwrap()
        );
        assert_eq!(
            resource_root_for(&exe, false).unwrap(),
            exe.parent().unwrap()
        );
    }
    #[cfg(unix)]
    #[test]
    fn resource_or_manifest_symlink_is_refused() {
        use std::os::unix::fs::symlink;
        let (temp, exe) = fixture();
        let resources = exe.parent().unwrap().parent().unwrap().join("Resources");
        let other = temp.path().join("other");
        save(
            &other,
            json!({"schema":1,"component":"agent-observatory","mac_bundle_layout":1}),
        );
        symlink(&other, &resources).unwrap();
        assert!(resource_root_for(&exe, true).is_err());
        std::fs::remove_file(&resources).unwrap();
        std::fs::create_dir(&resources).unwrap();
        symlink(other.join(MANIFEST), resources.join(MANIFEST)).unwrap();
        assert!(resource_root_for(&exe, true).is_err());
    }
}
