//! One reviewed public Slack archive, staged into a fresh private cache only.
//!
//! The descriptor is release-owner input, never browser configuration. Slack's
//! native manifest does not embed its Git source: that association belongs to the
//! reviewed archive digest in this descriptor. No installed connector is read or
//! changed here. Partial stages are retained, never silently retried or removed.
use amplifier_native_platform as platform;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
};

pub const MAX_PACKAGE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_MEMBERS: usize = 20_000;
const MAX_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;
const RECEIPT: &str = "observatory-slack-package.json";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LockedFile {
    pub size: u64,
    pub sha256: String,
    /// Original archive permissions, exactly 0644 or 0755; cache files are private.
    pub mode: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseDescriptor {
    pub schema: u8,
    pub source: String,
    pub version: String,
    pub target: String,
    pub archive_size: u64,
    pub archive_sha256: String,
    pub entry: String,
    pub native_bundle_sha256: String,
    pub files: BTreeMap<String, LockedFile>,
}

#[derive(Debug)]
pub struct StagedPackage {
    pub root: PathBuf,
    pub executable: PathBuf,
    pub source: String,
    pub version: String,
    pub target: String,
    pub archive_sha256: String,
}

struct Layout {
    entry: &'static str,
    manifest: &'static str,
    python: &'static str,
    interpreter: &'static str,
    plist: Option<&'static str>,
}
fn layout(target: &str) -> Result<Layout> {
    match target {
        "x86_64-pc-windows-msvc" | "aarch64-pc-windows-msvc" => Ok(Layout {
            entry: "acp-slack-adapter.exe",
            manifest: "native-bundle.json",
            python: "runtime/python/",
            interpreter: "python.exe",
            plist: None,
        }),
        "x86_64-apple-darwin" | "aarch64-apple-darwin" => Ok(Layout {
            entry: "Slack Connector.app/Contents/MacOS/acp-slack-adapter",
            manifest: "Slack Connector.app/Contents/Resources/native-bundle.json",
            python: "Slack Connector.app/Contents/Resources/runtime/python/",
            interpreter: "bin/python3",
            plist: Some("Slack Connector.app/Contents/Info.plist"),
        }),
        _ => anyhow::bail!(
            "Slack setup currently supports only reviewed Windows and macOS ZIP packages"
        ),
    }
}

fn lowercase_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn release_version(value: &str) -> bool {
    value.len() <= 40
        && value.split('.').count() == 3
        && value.split('.').all(|v| {
            !v.is_empty()
                && v.bytes().all(|b| b.is_ascii_digit())
                && (v == "0" || !v.starts_with('0'))
        })
}

/// Current release names are ASCII. Refuse case, trailing-dot, device-name and
/// separator aliases consistently before touching a Windows or Mac filesystem.
fn member_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name.len() <= 1024 && name.is_ascii(),
        "Unsupported Slack package member name"
    );
    for part in name.split('/') {
        let base = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        let numbered_device = ["COM", "LPT"].iter().any(|prefix| {
            base.strip_prefix(prefix)
                .is_some_and(|v| v.len() == 1 && matches!(v.as_bytes()[0], b'1'..=b'9'))
        });
        ensure!(
            !part.is_empty()
                && !matches!(part, "." | "..")
                && !part.ends_with(['.', ' '])
                && !part
                    .bytes()
                    .any(|b| b < 32 || b == 127 || b"\\:<>\"|?*".contains(&b))
                && !matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                && !numbered_device,
            "Slack package contains an unsafe or aliased path"
        );
    }
    Ok(())
}

impl ReleaseDescriptor {
    pub fn validate_for_target(&self, expected_target: &str) -> Result<()> {
        let paths = layout(&self.target)?;
        ensure!(
            self.schema == 1
                && self.target == expected_target
                && lowercase_hex(&self.source, 40)
                && release_version(&self.version)
                && lowercase_hex(&self.archive_sha256, 64)
                && lowercase_hex(&self.native_bundle_sha256, 64)
                && (1..=MAX_PACKAGE_BYTES).contains(&self.archive_size)
                && self.entry == paths.entry,
            "The reviewed Slack release does not match this computer or archive contract"
        );
        ensure!(
            (5..=MAX_MEMBERS).contains(&self.files.len()),
            "Invalid Slack package inventory size"
        );
        let mut total = 0u64;
        let mut names = BTreeSet::new();
        let mut path_spellings = BTreeMap::new();
        for (name, file) in &self.files {
            member_name(name)?;
            let mut prefix = String::new();
            for part in name.split('/') {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(part);
                if let Some(previous) =
                    path_spellings.insert(prefix.to_ascii_lowercase(), prefix.clone())
                {
                    ensure!(
                        previous == prefix,
                        "Slack package contains directory aliases"
                    );
                }
            }
            ensure!(
                names.insert(name.to_ascii_lowercase())
                    && lowercase_hex(&file.sha256, 64)
                    && matches!(file.mode, 0o644 | 0o755),
                "Invalid or aliased Slack package inventory"
            );
            ensure!(
                name == paths.entry
                    || name == paths.manifest
                    || matches!(name.as_str(), "START-HERE.txt" | "LICENSE")
                    || paths.plist == Some(name.as_str())
                    || name
                        .strip_prefix(paths.python)
                        .is_some_and(|tail| !tail.is_empty()),
                "Unexpected Slack package member"
            );
            total = total
                .checked_add(file.size)
                .context("Package size overflow")?;
            ensure!(
                total <= MAX_PACKAGE_BYTES,
                "Slack package exceeds its expanded size limit"
            );
        }
        for name in &names {
            let mut at = name.as_str();
            while let Some((parent, _)) = at.rsplit_once('/') {
                ensure!(
                    !names.contains(parent),
                    "Package file conflicts with a directory"
                );
                at = parent;
            }
        }
        for name in [paths.entry, paths.manifest, "START-HERE.txt", "LICENSE"] {
            ensure!(
                self.files.contains_key(name),
                "Incomplete Slack package inventory"
            );
        }
        if let Some(plist) = paths.plist {
            ensure!(
                self.files.contains_key(plist),
                "Missing Slack application metadata"
            );
        }
        let interpreter = format!("{}{}", paths.python, paths.interpreter);
        ensure!(
            self.files.contains_key(&interpreter),
            "Missing private Python interpreter"
        );
        ensure!(
            self.files[paths.entry].size > 0
                && self.files[paths.entry].mode == 0o755
                && self.files[paths.manifest].size <= MAX_MANIFEST_BYTES
                && self.files[paths.manifest].sha256 == self.native_bundle_sha256,
            "Invalid executable or native bundle binding"
        );
        Ok(())
    }

    /// A fixed official release URL, for the internal download step. This does
    /// not make a network request or accept a browser-provided destination URL.
    pub fn download_url(&self) -> Result<String> {
        self.validate_for_target(&self.target)?;
        Ok(format!(
            "https://github.com/microsoft/amplifier-acp-slack/releases/download/v{}/acp-slack-adapter-{}-{}.zip",
            self.version, self.version, self.target
        ))
    }
}

fn current_target() -> Result<String> {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        _ => anyhow::bail!("Unsupported computer architecture"),
    };
    let system = if cfg!(windows) {
        "pc-windows-msvc"
    } else if cfg!(target_os = "macos") {
        "apple-darwin"
    } else {
        anyhow::bail!(
            "Slack desktop setup is currently qualified for Windows and macOS packages only"
        )
    };
    Ok(format!("{arch}-{system}"))
}

fn file_digest(file: &mut File, expected_size: u64) -> Result<String> {
    ensure!(
        file.metadata()?.len() == expected_size,
        "Slack archive size changed"
    );
    file.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        ensure!(
            total <= expected_size,
            "Slack archive exceeded its reviewed size"
        );
        digest.update(&buffer[..count]);
    }
    ensure!(total == expected_size, "Incomplete Slack archive");
    file.seek(SeekFrom::Start(0))?;
    Ok(format!("{:x}", digest.finalize()))
}

fn ordinary_file(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "Package input must be an ordinary file"
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            metadata.file_attributes() & 0x400 == 0,
            "Package reparse points are not supported"
        );
    }
    Ok(())
}

/// Ingest downloaded bytes only after the caller selected a reviewed descriptor.
/// The destination must be absent, directly inside existing private owned cache
/// storage. Existing stages, connector homes and their permissions are untouched.
pub fn stage_archive(
    archive: &Path,
    destination: &Path,
    release: &ReleaseDescriptor,
) -> Result<StagedPackage> {
    stage_for_target(archive, destination, release, &current_target()?)
}

fn stage_for_target(
    archive: &Path,
    destination: &Path,
    release: &ReleaseDescriptor,
    target: &str,
) -> Result<StagedPackage> {
    release.validate_for_target(target)?;
    ensure!(
        archive.is_absolute()
            && destination.is_absolute()
            && destination.components().all(|v| matches!(
                v,
                Component::Prefix(_) | Component::RootDir | Component::Normal(_)
            )),
        "Use an absolute fresh package cache path"
    );
    let parent = destination
        .parent()
        .context("Missing private package cache")?;
    platform::validate_private_directory(parent)?;
    ensure!(
        std::fs::symlink_metadata(destination)
            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
        "Package cache entry already exists or cannot be inspected"
    );
    ordinary_file(archive)?;
    let mut file = File::open(archive)?;
    ensure!(
        file_digest(&mut file, release.archive_size)? == release.archive_sha256,
        "Slack archive checksum mismatch"
    );
    let mut zip = zip::ZipArchive::new(file)?;
    ensure!(
        zip.len() == release.files.len(),
        "Slack archive member count differs from reviewed inventory"
    );
    let mut seen = BTreeSet::new();
    // Reject all archive metadata before creating any output.
    for i in 0..zip.len() {
        let member = zip.by_index(i)?;
        let name = member.name()?.into_owned();
        member_name(&name)?;
        let locked = release
            .files
            .get(&name)
            .context("Unreviewed Slack archive entry")?;
        let mode = member.unix_mode().context("Missing package member mode")?;
        ensure!(
            seen.insert(name)
                && !member.is_dir()
                && mode & 0o170000 == 0o100000
                && mode & 0o7777 == locked.mode
                && member.size() == locked.size,
            "Slack archive contains a link, duplicate, mode or size mismatch"
        );
    }
    platform::private_create_dir(destination)
        .context("Create fresh private package cache entry")?;
    let mut directories = BTreeSet::new();
    for i in 0..zip.len() {
        let mut member = zip.by_index(i)?;
        let name = member.name()?.into_owned();
        let locked = &release.files[&name];
        let parts: Vec<_> = name.split('/').collect();
        let mut directory = destination.to_path_buf();
        for part in &parts[..parts.len() - 1] {
            directory.push(part);
            if directories.insert(directory.clone()) {
                platform::private_create_dir(&directory)?;
            }
        }
        let target = destination.join(&name);
        platform::private_create(&target, b"")?;
        let mut output = OpenOptions::new().write(true).open(&target)?;
        let mut digest = Sha256::new();
        let mut remaining = locked.size;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = member.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            ensure!(
                count as u64 <= remaining,
                "Slack member exceeds its reviewed size"
            );
            output.write_all(&buffer[..count])?;
            digest.update(&buffer[..count]);
            remaining -= count as u64;
        }
        ensure!(
            remaining == 0 && format!("{:x}", digest.finalize()) == locked.sha256,
            "Slack package member checksum mismatch: {name}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            output.set_permissions(std::fs::Permissions::from_mode(if locked.mode == 0o755 {
                0o700
            } else {
                0o600
            }))?;
        }
        output.sync_all()?;
    }
    let mut source = zip.into_inner();
    ensure!(
        file_digest(&mut source, release.archive_size)? == release.archive_sha256,
        "Slack archive changed during staging"
    );
    validate_native_manifest(destination, release)?;
    platform::private_create(
        &destination.join(RECEIPT),
        &serde_json::to_vec_pretty(release)?,
    )?;
    Ok(StagedPackage {
        root: destination.to_path_buf(),
        executable: destination.join(&release.entry),
        source: release.source.clone(),
        version: release.version.clone(),
        target: release.target.clone(),
        archive_sha256: release.archive_sha256.clone(),
    })
}

fn validate_native_manifest(root: &Path, release: &ReleaseDescriptor) -> Result<()> {
    let paths = layout(&release.target)?;
    let bytes = std::fs::read(root.join(paths.manifest))?;
    let manifest: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        manifest["schema"] == 1
            && manifest["component"] == "acp-slack-adapter"
            && manifest["target"] == release.target
            && manifest["version"] == release.version
            && manifest["python"]["target"] == release.target,
        "Slack native bundle identity differs from the reviewed release"
    );
    let python = manifest["python_files"]
        .as_object()
        .context("Missing private Python inventory")?;
    ensure!(
        !python.is_empty() && python.len() < MAX_MEMBERS,
        "Invalid private Python inventory"
    );
    // PBS metadata names its versioned interpreter on macOS; the application
    // also requires the ordinary-file bin/python3 alias retained in the bundle.
    let interpreter = manifest["python"]["executable"]
        .as_str()
        .context("Missing private Python executable metadata")?;
    member_name(interpreter)?;
    ensure!(
        python.contains_key(interpreter),
        "Declared private Python executable is missing"
    );
    let expected: BTreeMap<_, _> = release
        .files
        .iter()
        .filter_map(|(name, file)| {
            name.strip_prefix(paths.python)
                .map(|relative| (relative, file.sha256.as_str()))
        })
        .collect();
    ensure!(
        python.len() == expected.len(),
        "Private Python inventory is incomplete"
    );
    for (name, hash) in python {
        ensure!(
            expected.get(name.as_str()).copied() == hash.as_str(),
            "Private Python inventory differs from reviewed files"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use zip::{ZipWriter, write::SimpleFileOptions};

    const WINDOWS: &str = "x86_64-pc-windows-msvc";
    const MAC: &str = "aarch64-apple-darwin";
    struct Fixture {
        _temp: tempfile::TempDir,
        archive: PathBuf,
        destination: PathBuf,
        release: ReleaseDescriptor,
        files: BTreeMap<String, (Vec<u8>, u32)>,
    }
    fn sha(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }
    fn write_zip(path: &Path, files: &BTreeMap<String, (Vec<u8>, u32)>) {
        let mut writer = ZipWriter::new(File::create(path).unwrap());
        for (name, (bytes, mode)) in files {
            writer
                .start_file(name, SimpleFileOptions::default().unix_permissions(*mode))
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
    }
    impl Fixture {
        fn new(target: &str) -> Self {
            let temp = tempfile::tempdir().unwrap();
            platform::private_dir(temp.path()).unwrap();
            let paths = layout(target).unwrap();
            let python_bytes = b"inert private Python fixture; never executed".to_vec();
            let mut manifest = json!({"schema":1,"component":"acp-slack-adapter","version":"0.0.0","target":target,
                "python":{"target":target,"executable":paths.interpreter},
                "python_files":{paths.interpreter:sha(&python_bytes),"lib/fixture.py":sha(b"private module")}});
            let mut files = BTreeMap::from([
                (
                    paths.entry.into(),
                    (
                        b"inert native application fixture; never executed".to_vec(),
                        0o755,
                    ),
                ),
                (
                    paths.manifest.into(),
                    (serde_json::to_vec(&manifest).unwrap(), 0o644),
                ),
                (
                    format!("{}{}", paths.python, paths.interpreter),
                    (python_bytes, 0o755),
                ),
                (
                    format!("{}lib/fixture.py", paths.python),
                    (b"private module".to_vec(), 0o644),
                ),
                (
                    "START-HERE.txt".into(),
                    (b"fixture instructions".to_vec(), 0o644),
                ),
                ("LICENSE".into(), (b"fixture license".to_vec(), 0o644)),
            ]);
            if let Some(plist) = paths.plist {
                manifest["python"]["executable"] = json!("bin/python3.13");
                manifest["python_files"]["bin/python3.13"] =
                    json!(sha(b"versioned Python fixture"));
                files.insert(
                    format!("{}bin/python3.13", paths.python),
                    (b"versioned Python fixture".to_vec(), 0o755),
                );
                files.get_mut(paths.manifest).unwrap().0 = serde_json::to_vec(&manifest).unwrap();
                files.insert(
                    plist.into(),
                    (b"inert reviewed app metadata".to_vec(), 0o644),
                );
            }
            let mut fixture = Self {
                archive: temp.path().join("source.zip"),
                destination: temp.path().join("fresh-cache"),
                _temp: temp,
                files,
                release: ReleaseDescriptor {
                    schema: 1,
                    source: "a".repeat(40),
                    version: "0.0.0".into(),
                    target: target.into(),
                    archive_size: 0,
                    archive_sha256: String::new(),
                    entry: paths.entry.into(),
                    native_bundle_sha256: String::new(),
                    files: BTreeMap::new(),
                },
            };
            fixture.reseal();
            fixture
        }
        fn reseal_archive(&mut self) {
            write_zip(&self.archive, &self.files);
            let bytes = std::fs::read(&self.archive).unwrap();
            self.release.archive_size = bytes.len() as u64;
            self.release.archive_sha256 = sha(&bytes);
        }
        fn reseal(&mut self) {
            self.reseal_archive();
            self.release.files = self
                .files
                .iter()
                .map(|(name, (bytes, mode))| {
                    (
                        name.clone(),
                        LockedFile {
                            size: bytes.len() as u64,
                            sha256: sha(bytes),
                            mode: *mode,
                        },
                    )
                })
                .collect();
            self.release.native_bundle_sha256 = self.release.files
                [layout(&self.release.target).unwrap().manifest]
                .sha256
                .clone();
        }
        fn stage(&self) -> Result<StagedPackage> {
            stage_for_target(
                &self.archive,
                &self.destination,
                &self.release,
                &self.release.target,
            )
        }
    }

    #[test]
    fn windows_and_mac_complete_bundles_keep_every_byte_and_private_modes() {
        for target in [WINDOWS, MAC] {
            let fixture = Fixture::new(target);
            let result = fixture.stage().unwrap();
            assert_eq!(
                result.executable,
                fixture.destination.join(&fixture.release.entry)
            );
            assert_eq!(result.source, fixture.release.source);
            for (name, (bytes, mode)) in &fixture.files {
                let file = fixture.destination.join(name);
                assert_eq!(&std::fs::read(&file).unwrap(), bytes);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    assert_eq!(
                        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                        if *mode == 0o755 { 0o700 } else { 0o600 }
                    );
                }
                #[cfg(not(unix))]
                let _ = mode;
            }
            let receipt: ReleaseDescriptor =
                serde_json::from_slice(&std::fs::read(fixture.destination.join(RECEIPT)).unwrap())
                    .unwrap();
            assert_eq!(receipt, fixture.release);
            assert!(
                fixture.stage().is_err(),
                "Existing cache must never be silently reused"
            );
        }
    }

    #[test]
    fn exact_official_url_requires_complete_reviewed_descriptor() {
        let mut fixture = Fixture::new(WINDOWS);
        assert_eq!(
            fixture.release.download_url().unwrap(),
            "https://github.com/microsoft/amplifier-acp-slack/releases/download/v0.0.0/acp-slack-adapter-0.0.0-x86_64-pc-windows-msvc.zip"
        );
        for source in ["main", "../other", "ABCDEF"] {
            fixture.release.source = source.into();
            assert!(fixture.release.download_url().is_err());
        }
    }

    #[test]
    fn unsupported_tar_wrong_target_and_invalid_release_refuse_before_stage() {
        let mut fixture = Fixture::new(WINDOWS);
        assert!(
            stage_for_target(
                &fixture.archive,
                &fixture.destination,
                &fixture.release,
                MAC
            )
            .is_err()
        );
        fixture.release.target = "aarch64-unknown-linux-gnu".into();
        assert!(fixture.stage().is_err());
        assert!(fixture.release.download_url().is_err());
        fixture.release.target = WINDOWS.into();
        for version in ["../1.2.3", "01.2.3", "1.2", "1.2.3/other"] {
            fixture.release.version = version.into();
            assert!(fixture.stage().is_err());
        }
        assert!(!fixture.destination.exists());
    }

    #[test]
    fn archive_digest_size_and_unreviewed_extra_entry_refuse_before_stage() {
        let mut fixture = Fixture::new(WINDOWS);
        fixture.release.archive_sha256 = "0".repeat(64);
        assert!(fixture.stage().is_err());
        fixture.reseal();
        fixture.release.archive_size += 1;
        assert!(fixture.stage().is_err());
        fixture.reseal();
        fixture
            .files
            .insert("unexpected.txt".into(), (b"unreviewed".to_vec(), 0o644));
        fixture.reseal_archive();
        assert!(fixture.stage().is_err());
        assert!(!fixture.destination.exists());
    }

    #[test]
    fn aliases_traversal_reserved_names_and_file_directory_conflicts_are_refused() {
        for name in [
            "../escape",
            "runtime/python/../escape",
            "/absolute",
            "runtime/python/a\\b",
            "runtime/python/AUX.py",
            "runtime/python/a.",
            "runtime/python/a ",
            "runtime/python/a:b",
            "runtime/python//x",
            "runtime/python/café.py",
            "Runtime/python/another.py",
            "runtime/python/lib",
        ] {
            let mut fixture = Fixture::new(WINDOWS);
            fixture
                .files
                .insert(name.into(), (b"invalid".to_vec(), 0o644));
            fixture.reseal();
            assert!(fixture.stage().is_err(), "{name}");
            assert!(!fixture.destination.exists());
        }
    }

    #[test]
    fn archive_symlink_and_mode_changes_are_rejected_before_stage() {
        let mut fixture = Fixture::new(WINDOWS);
        let mut writer = ZipWriter::new(File::create(&fixture.archive).unwrap());
        for (name, (bytes, mode)) in &fixture.files {
            if name == "LICENSE" {
                writer
                    .add_symlink(name, "outside", SimpleFileOptions::default())
                    .unwrap();
            } else {
                writer
                    .start_file(name, SimpleFileOptions::default().unix_permissions(*mode))
                    .unwrap();
                writer.write_all(bytes).unwrap();
            }
        }
        writer.finish().unwrap();
        let bytes = std::fs::read(&fixture.archive).unwrap();
        fixture.release.archive_size = bytes.len() as u64;
        fixture.release.archive_sha256 = sha(&bytes);
        assert!(fixture.stage().is_err());
        assert!(!fixture.destination.exists());
        fixture.reseal();
        fixture.release.files.get_mut("LICENSE").unwrap().mode = 0o755;
        assert!(fixture.stage().is_err());
        assert!(!fixture.destination.exists());
    }

    #[test]
    fn content_mismatch_retains_partial_stage_without_completion_receipt() {
        let mut fixture = Fixture::new(WINDOWS);
        fixture.release.files.get_mut("LICENSE").unwrap().sha256 = "0".repeat(64);
        assert!(fixture.stage().is_err());
        assert!(fixture.destination.exists());
        assert!(!fixture.destination.join(RECEIPT).exists());
        assert_eq!(
            std::fs::read(fixture.destination.join("LICENSE")).unwrap(),
            b"fixture license"
        );
        assert!(fixture.stage().is_err());
    }

    #[test]
    fn manifest_target_and_python_inventory_must_match_reviewed_payload() {
        for change in ["target", "python_target", "inventory", "interpreter"] {
            let mut fixture = Fixture::new(WINDOWS);
            let path = layout(WINDOWS).unwrap().manifest;
            let mut value: Value = serde_json::from_slice(&fixture.files[path].0).unwrap();
            match change {
                "target" => value["target"] = json!(MAC),
                "python_target" => value["python"]["target"] = json!(MAC),
                "inventory" => {
                    value["python_files"]
                        .as_object_mut()
                        .unwrap()
                        .remove("lib/fixture.py");
                }
                _ => value["python"]["executable"] = json!("another.exe"),
            }
            fixture.files.get_mut(path).unwrap().0 = serde_json::to_vec(&value).unwrap();
            fixture.reseal();
            assert!(fixture.stage().is_err(), "{change}");
            assert!(!fixture.destination.join(RECEIPT).exists());
        }
    }

    #[test]
    fn size_limits_are_checked_without_allocating_large_payloads() {
        let mut fixture = Fixture::new(WINDOWS);
        fixture.release.archive_size = MAX_PACKAGE_BYTES + 1;
        assert!(fixture.stage().is_err());
        fixture.reseal();
        fixture.release.files.get_mut("LICENSE").unwrap().size = MAX_PACKAGE_BYTES;
        assert!(fixture.stage().is_err());
        assert!(!fixture.destination.exists());
    }

    #[test]
    fn existing_cache_is_preserved_even_if_empty() {
        let fixture = Fixture::new(WINDOWS);
        platform::private_create_dir(&fixture.destination).unwrap();
        std::fs::write(fixture.destination.join("retained"), b"existing state").unwrap();
        assert!(fixture.stage().is_err());
        assert_eq!(
            std::fs::read(fixture.destination.join("retained")).unwrap(),
            b"existing state"
        );
        assert!(!fixture.destination.join(RECEIPT).exists());
    }

    #[cfg(unix)]
    #[test]
    fn public_parent_and_archive_symlink_are_refused_without_repair() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let fixture = Fixture::new(WINDOWS);
        let parent = fixture.destination.parent().unwrap();
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(fixture.stage().is_err());
        assert_eq!(
            std::fs::metadata(parent).unwrap().permissions().mode() & 0o777,
            0o755
        );
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        let link = parent.join("linked.zip");
        symlink(&fixture.archive, &link).unwrap();
        assert!(stage_for_target(&link, &fixture.destination, &fixture.release, WINDOWS).is_err());
        assert!(!fixture.destination.exists());
    }
}
