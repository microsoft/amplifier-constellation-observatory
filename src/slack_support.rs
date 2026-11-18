//! One unchanged target Slack archive carried by a fresh Observatory desktop.
//! Existing installations and update support are never replaced by this module.
use crate::slack_package::{MAX_PACKAGE_BYTES, ReleaseDescriptor};
use amplifier_native_platform as platform;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

const DESCRIPTOR: &str = "descriptor.json";
const RECEIPT: &str = "retained.json";
const MAX_DESCRIPTOR: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Binding {
    schema: u8,
    source: String,
    version: String,
    target: String,
    archive_size: u64,
    archive_sha256: String,
    descriptor_sha256: String,
}
impl Binding {
    fn matches(&self, release: &ReleaseDescriptor) -> bool {
        self.schema == 1
            && self.source == release.source
            && self.version == release.version
            && self.target == release.target
            && self.archive_size == release.archive_size
            && self.archive_sha256 == release.archive_sha256
    }
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn ordinary(path: &Path, directory: bool) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        !metadata.file_type().is_symlink()
            && if directory {
                metadata.is_dir()
            } else {
                metadata.is_file()
            },
        "Slack support must use ordinary files and directories"
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            metadata.file_attributes() & 0x400 == 0,
            "Slack support reparse points are refused"
        );
    }
    Ok(())
}
fn present(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}
fn bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    ordinary(path, false)?;
    let file = File::open(path)?;
    ensure!(
        file.metadata()?.len() <= limit,
        "Excessive Slack support metadata"
    );
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "Slack support metadata changed size"
    );
    Ok(bytes)
}
fn binding(manifest: &Value) -> Result<Option<Binding>> {
    manifest
        .get("slack_bundle")
        .map(|value| {
            serde_json::from_value(value.clone()).context("Invalid packaged Slack binding")
        })
        .transpose()
}
fn checked_archive(path: &Path, release: &ReleaseDescriptor) -> Result<File> {
    ordinary(path, false)?;
    let mut file = File::open(path)?;
    ensure!(
        file.metadata()?.len() == release.archive_size
            && (1..=MAX_PACKAGE_BYTES).contains(&release.archive_size),
        "Slack support archive size changed"
    );
    let mut digest = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        ensure!(total <= release.archive_size, "Slack support archive grew");
        digest.update(&buffer[..count]);
    }
    ensure!(
        total == release.archive_size
            && format!("{:x}", digest.finalize()) == release.archive_sha256,
        "Slack support archive checksum changed"
    );
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(0))?;
    Ok(file)
}
fn inventory(
    root: &Path,
    expected: &Binding,
    retained: bool,
) -> Result<(ReleaseDescriptor, PathBuf)> {
    ordinary(root.parent().context("Missing Slack support parent")?, true)?;
    ordinary(root, true)?;
    let bytes = bounded(&root.join(DESCRIPTOR), MAX_DESCRIPTOR)?;
    ensure!(
        sha(&bytes) == expected.descriptor_sha256,
        "Slack support descriptor changed"
    );
    let release: ReleaseDescriptor = serde_json::from_slice(&bytes)?;
    release.validate_for_target(&expected.target)?;
    ensure!(
        expected.matches(&release),
        "Slack support descriptor binding changed"
    );
    let name = format!("{}.zip", release.target);
    let wanted: BTreeSet<_> = if retained {
        vec![DESCRIPTOR.to_owned(), name.clone(), RECEIPT.to_owned()]
    } else {
        vec![DESCRIPTOR.to_owned(), name.clone()]
    }
    .into_iter()
    .collect();
    let found = std::fs::read_dir(root)?
        .map(|entry| {
            entry?
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("Invalid Slack support filename"))
        })
        .collect::<Result<BTreeSet<String>>>()?;
    ensure!(
        found == wanted,
        "Incomplete or unexpected Slack support files"
    );
    if retained {
        let receipt: Binding = serde_json::from_slice(&bounded(&root.join(RECEIPT), 65536)?)?;
        ensure!(
            &receipt == expected,
            "Retained Slack support receipt changed"
        );
    }
    let path = root.join(name);
    checked_archive(&path, &release)?;
    Ok((release, path))
}

/// Package integrity only. Future update candidates may have a different lock;
/// they do not gain setup authority from this structural/hash verification.
pub(crate) fn validate_package(executable_root: &Path, manifest: &Value) -> Result<()> {
    let root = executable_root.join("support/slack");
    match binding(manifest)? {
        Some(expected) => {
            ensure!(
                manifest["target"] == expected.target,
                "Slack support belongs to another package target"
            );
            inventory(&root, &expected, false)?;
        }
        None => ensure!(!present(&root)?, "Unrecorded Slack support was preserved"),
    }
    Ok(())
}
fn compiled_release(expected: &ReleaseDescriptor) -> Result<()> {
    let selected = crate::slack_setup::selected_release()?
        .context("This application has no reviewed Slack setup package")?;
    ensure!(
        &selected.release == expected,
        "Slack support differs from this application's reviewed lock"
    );
    Ok(())
}
fn retain_with_release(
    home: &Path,
    executable_root: &Path,
    manifest: &Value,
    release: &ReleaseDescriptor,
) -> Result<()> {
    let expected = binding(manifest)?.context("Missing packaged Slack binding")?;
    ensure!(
        manifest["target"] == expected.target,
        "Slack support belongs to another package target"
    );
    let source = executable_root.join("support/slack");
    let (packaged, archive) = inventory(&source, &expected, false)?;
    ensure!(
        &packaged == release,
        "Slack support differs from the selected reviewed descriptor"
    );
    platform::validate_private_directory(home)?;
    let support = home.join("support");
    if present(&support)? {
        platform::validate_private_directory(&support)?;
    } else {
        platform::private_create_dir(&support)?;
    }
    let destination = support.join("slack");
    ensure!(
        !present(&destination)?,
        "Existing Slack support was preserved"
    );
    platform::private_create_dir(&destination)?;
    let mut input = checked_archive(&archive, release)?;
    let target = destination.join(format!("{}.zip", release.target));
    platform::private_create(&target, b"")?;
    let mut output = OpenOptions::new().write(true).open(&target)?;
    let mut total = 0u64;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        ensure!(
            total <= release.archive_size,
            "Slack archive changed while retaining it"
        );
        output.write_all(&buffer[..count])?;
        digest.update(&buffer[..count]);
    }
    ensure!(
        total == release.archive_size
            && format!("{:x}", digest.finalize()) == release.archive_sha256,
        "Copied Slack archive checksum changed"
    );
    output.sync_all()?;
    let descriptor = bounded(&source.join(DESCRIPTOR), MAX_DESCRIPTOR)?;
    ensure!(
        sha(&descriptor) == expected.descriptor_sha256,
        "Slack descriptor changed while retaining it"
    );
    platform::private_create(&destination.join(DESCRIPTOR), &descriptor)?;
    checked_archive(&target, release)?;
    platform::private_create(&destination.join(RECEIPT), &serde_json::to_vec(&expected)?)?;
    inventory(&destination, &expected, true)?;
    Ok(())
}
/// Only the new private Observatory home receives support bytes. Never adopts,
/// overwrites, repairs or removes an existing Slack support directory.
pub(crate) fn retain(home: &Path, executable: &Path, manifest: &Value) -> Result<()> {
    let root = executable.parent().context("Missing application folder")?;
    let Some(expected) = binding(manifest)? else {
        return Ok(());
    };
    let (release, _) = inventory(&root.join("support/slack"), &expected, false)?;
    compiled_release(&release)?;
    retain_with_release(home, root, manifest, &release)
}
fn installed_with_release(home: &Path, release: &ReleaseDescriptor) -> Result<Option<PathBuf>> {
    let root = home.join("support/slack");
    let install = home.join("app-install.json");
    let manifest = if present(&install)? {
        let record: Value = serde_json::from_slice(&bounded(&install, 1024 * 1024)?)?;
        ensure!(
            record["schema"] == 1,
            "Invalid retained application receipt"
        );
        record["package"].clone()
    } else {
        Value::Null
    };
    let Some(expected) = binding(&manifest)? else {
        ensure!(
            !present(&root)?,
            "Unrecorded Slack support was preserved; no fallback is allowed"
        );
        return Ok(None);
    };
    ensure!(
        manifest["target"] == expected.target,
        "Installed Slack support target changed"
    );
    platform::validate_private_directory(home)?;
    platform::validate_private_directory(&home.join("support"))?;
    platform::validate_private_directory(&root)?;
    let (retained, archive) = inventory(&root, &expected, true)?;
    ensure!(
        &retained == release,
        "Retained Slack support differs from the selected reviewed descriptor"
    );
    Ok(Some(archive))
}
/// None means a genuine thin installation, not corrupt/missing expected support.
/// The caller uses the existing fixed official download only in that None case.
pub fn installed_archive(home: &Path, release: &ReleaseDescriptor) -> Result<Option<PathBuf>> {
    compiled_release(release)?;
    installed_with_release(home, release)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slack_package::LockedFile;
    use serde_json::json;
    use std::collections::BTreeMap;

    struct Fixture {
        _temp: tempfile::TempDir,
        package: PathBuf,
        home: PathBuf,
        archive: PathBuf,
        release: ReleaseDescriptor,
        manifest: Value,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let home = root.join("home");
            platform::private_create_dir(&home).unwrap();
            let package = root.join("package");
            let support = package.join("support/slack");
            std::fs::create_dir_all(&support).unwrap();
            let target = "x86_64-pc-windows-msvc";
            // Transport/receipt fixture only: bytes are never extracted or run.
            let data = b"unchanged reviewed owner archive fixture";
            let archive = support.join(format!("{target}.zip"));
            std::fs::write(&archive, data).unwrap();
            let files: BTreeMap<_, _> = [
                "acp-slack-adapter.exe",
                "native-bundle.json",
                "runtime/python/python.exe",
                "START-HERE.txt",
                "LICENSE",
            ]
            .into_iter()
            .map(|name| {
                (
                    name.to_owned(),
                    LockedFile {
                        size: 1,
                        sha256: "a".repeat(64),
                        mode: if name.ends_with(".exe") { 0o755 } else { 0o644 },
                    },
                )
            })
            .collect();
            let release = ReleaseDescriptor {
                schema: 1,
                source: "b".repeat(40),
                version: "0.0.0".into(),
                target: target.into(),
                archive_size: data.len() as u64,
                archive_sha256: sha(data),
                entry: "acp-slack-adapter.exe".into(),
                native_bundle_sha256: "a".repeat(64),
                files,
            };
            let bytes = serde_json::to_vec_pretty(&release).unwrap();
            std::fs::write(support.join(DESCRIPTOR), &bytes).unwrap();
            let binding = Binding {
                schema: 1,
                source: release.source.clone(),
                version: release.version.clone(),
                target: release.target.clone(),
                archive_size: release.archive_size,
                archive_sha256: release.archive_sha256.clone(),
                descriptor_sha256: sha(&bytes),
            };
            let manifest = json!({"target":target,"slack_bundle":binding});
            Self {
                _temp: temp,
                package,
                home,
                archive,
                release,
                manifest,
            }
        }
        fn record(&self) {
            platform::private_create(
                &self.home.join("app-install.json"),
                &serde_json::to_vec(&json!({"schema":1,"package":self.manifest})).unwrap(),
            )
            .unwrap();
        }
        fn retain(&self) {
            retain_with_release(&self.home, &self.package, &self.manifest, &self.release).unwrap();
            self.record();
        }
    }
    #[test]
    fn exact_archive_and_descriptor_are_retained_privately_without_adopting_existing_support() {
        let f = Fixture::new();
        validate_package(&f.package, &f.manifest).unwrap();
        f.retain();
        let path = installed_with_release(&f.home, &f.release)
            .unwrap()
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes, std::fs::read(&f.archive).unwrap());
        platform::validate_private_directory(path.parent().unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o077,
                0
            );
        }
        assert!(retain_with_release(&f.home, &f.package, &f.manifest, &f.release).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
    #[test]
    fn thin_absence_is_distinct_from_missing_expected_or_unrecorded_support() {
        let thin = Fixture::new();
        assert!(
            installed_with_release(&thin.home, &thin.release)
                .unwrap()
                .is_none()
        );
        thin.record();
        assert!(installed_with_release(&thin.home, &thin.release).is_err());
        let extra = Fixture::new();
        std::fs::create_dir_all(extra.home.join("support/slack")).unwrap();
        assert!(installed_with_release(&extra.home, &extra.release).is_err());
        assert!(validate_package(&extra.package, &json!({})).is_err());
    }
    #[test]
    fn every_present_corruption_refuses_instead_of_falling_back() {
        for member in [
            "descriptor.json",
            "retained.json",
            "x86_64-pc-windows-msvc.zip",
        ] {
            let f = Fixture::new();
            f.retain();
            let path = f.home.join("support/slack").join(member);
            std::fs::write(&path, b"changed").unwrap();
            assert!(
                installed_with_release(&f.home, &f.release).is_err(),
                "{member}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), b"changed");
        }
        let f = Fixture::new();
        f.retain();
        std::fs::remove_file(f.home.join("support/slack/retained.json")).unwrap();
        assert!(installed_with_release(&f.home, &f.release).is_err());
        let f = Fixture::new();
        f.retain();
        std::fs::write(f.home.join("support/slack/unreviewed"), b"kept").unwrap();
        assert!(installed_with_release(&f.home, &f.release).is_err());
    }
    #[test]
    fn sidecar_cannot_authorize_another_release_or_repair_a_changed_archive() {
        let f = Fixture::new();
        let mut other = f.release.clone();
        other.source = "c".repeat(40);
        assert!(retain_with_release(&f.home, &f.package, &f.manifest, &other).is_err());
        assert!(!f.home.join("support/slack").exists());
        std::fs::write(&f.archive, b"changed archive").unwrap();
        assert!(validate_package(&f.package, &f.manifest).is_err());
        assert!(retain_with_release(&f.home, &f.package, &f.manifest, &f.release).is_err());
        assert!(!f.home.join("support/slack").exists());
    }
    #[test]
    fn archive_binding_must_match_the_outer_and_retained_application_target() {
        let f = Fixture::new();
        let mut changed = f.manifest.clone();
        changed["target"] = json!("aarch64-pc-windows-msvc");
        assert!(validate_package(&f.package, &changed).is_err());
        assert!(retain_with_release(&f.home, &f.package, &changed, &f.release).is_err());
        assert!(!f.home.join("support/slack").exists());
        f.retain();
        std::fs::write(
            f.home.join("app-install.json"),
            serde_json::to_vec(&json!({"schema":1,"package":changed})).unwrap(),
        )
        .unwrap();
        assert!(installed_with_release(&f.home, &f.release).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn linked_archive_is_refused_even_when_its_target_has_matching_bytes() {
        let f = Fixture::new();
        let other = f.package.join("other.zip");
        std::fs::rename(&f.archive, &other).unwrap();
        std::os::unix::fs::symlink(&other, &f.archive).unwrap();
        assert!(validate_package(&f.package, &f.manifest).is_err());
        assert!(!f.home.join("support/slack").exists());
    }
}
