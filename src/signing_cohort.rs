//! Explicit private signed derivatives. Public locks remain origin evidence.
//! An empty compiled lock authorizes no derivative and never enables a fallback.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
};

const LOCK: &str = include_str!("../scripts/signed-instance-cohort-lock.json");
const RUNTIME: &str = "engine/native/agent-runtime";
const MANIFEST: &str = "consumer-bundle.json";
const MAX_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublicInstance {
    pub source: String,
    pub tree: String,
    pub version: String,
    pub archive_sha256: String,
    pub manifest_sha256: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublicRuntime {
    pub source: String,
    pub version: String,
    pub archive_sha256: String,
    pub binary_sha256: String,
    pub lock_sha256: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublicPython {
    pub archive_sha256: String,
    pub lock_sha256: String,
}
/// This descriptor is compiled into Observatory, never accepted from a browser
/// or an untrusted sidecar. The manifest digest closes every final member hash.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub schema: u32,
    pub cohort_id: String,
    pub target: String,
    pub instance: PublicInstance,
    pub runtime: PublicRuntime,
    pub python: PublicPython,
    pub manifest_sha256: String,
    pub instance_binary_sha256: String,
    pub runtime_binary_sha256: String,
    pub signing_receipt_sha256: String,
    pub signing_team_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lock {
    schema: u32,
    targets: BTreeMap<String, Descriptor>,
}

#[derive(Debug, PartialEq)]
pub(crate) struct RuntimeBinding {
    pub path: PathBuf,
    pub cohort_id: String,
    pub manifest_sha256: String,
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-+".contains(&b))
}
impl Descriptor {
    fn validate(&self, target: &str) -> Result<()> {
        ensure!(
            self.schema == 1
                && self.target == target
                && matches!(target, "aarch64-apple-darwin" | "x86_64-apple-darwin"),
            "Use the reviewed Mac package target"
        );
        ensure!(
            uuid::Uuid::parse_str(&self.cohort_id)?.to_string() == self.cohort_id,
            "Invalid signed package identity"
        );
        ensure!(
            [
                &self.instance.source,
                &self.instance.tree,
                &self.runtime.source
            ]
            .iter()
            .all(|s| hex(s, 40)),
            "Invalid signed package source"
        );
        ensure!(
            version(&self.instance.version) && version(&self.runtime.version),
            "Invalid signed package version"
        );
        ensure!(
            [
                &self.instance.archive_sha256,
                &self.instance.manifest_sha256,
                &self.runtime.archive_sha256,
                &self.runtime.binary_sha256,
                &self.runtime.lock_sha256,
                &self.python.archive_sha256,
                &self.python.lock_sha256,
                &self.manifest_sha256,
                &self.instance_binary_sha256,
                &self.runtime_binary_sha256,
                &self.signing_receipt_sha256
            ]
            .iter()
            .all(|s| hex(s, 64)),
            "Invalid signed package digest"
        );
        ensure!(
            self.signing_team_id.len() == 10
                && self
                    .signing_team_id
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()),
            "Invalid signing owner identity"
        );
        Ok(())
    }
}

fn target() -> String {
    format!(
        "{}-{}",
        std::env::consts::ARCH,
        if cfg!(target_os = "macos") {
            "apple-darwin"
        } else if cfg!(windows) {
            "pc-windows-msvc"
        } else {
            "unknown-linux-gnu"
        }
    )
}
fn digest(path: &Path, limit: u64) -> Result<(String, u64)> {
    let mut file = std::fs::File::open(path)?.take(limit + 1);
    let mut hash = Sha256::new();
    let mut count = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let size = file.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        count += size as u64;
        ensure!(count <= limit, "Signed package exceeds its size limit");
        hash.update(&buffer[..size]);
    }
    Ok((format!("{:x}", hash.finalize()), count))
}
fn regular(root: &Path, name: &str) -> Result<PathBuf> {
    ensure!(
        !name.is_empty()
            && name.len() <= 1024
            && name.split('/').count() <= 32
            && !name.contains(['\\', ':', '\0'])
            && name
                .split('/')
                .all(|p| !p.is_empty() && p != "." && p != ".."),
        "Invalid signed package path"
    );
    crate::instance_package::ordinary(root, name)
}
pub(crate) fn manifest(root: &Path) -> Result<(Value, String)> {
    let path = regular(root, MANIFEST)?;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 4 * 1024 * 1024,
        "Excessive signed package inventory"
    );
    Ok((
        serde_json::from_slice(&bytes)?,
        format!("{:x}", Sha256::digest(&bytes)),
    ))
}
fn listed(root: &Path, folder: &Path, names: &mut BTreeSet<String>) -> Result<()> {
    ensure!(
        folder.strip_prefix(root)?.components().count() <= 32,
        "Excessive signed package depth"
    );
    let metadata = std::fs::symlink_metadata(folder)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "Use an ordinary signed package directory"
    );
    let mut populated = false;
    for item in std::fs::read_dir(folder)? {
        populated = true;
        let path = item?.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "Signed package links are not supported"
        );
        if metadata.is_dir() {
            listed(root, &path, names)?;
        } else {
            ensure!(metadata.is_file(), "Use ordinary signed package files");
            let relative = path
                .strip_prefix(root)?
                .to_str()
                .context("Invalid signed package filename")?
                .replace('\\', "/");
            ensure!(
                names.insert(relative) && names.len() <= 20_001,
                "Excessive signed package inventory"
            );
        }
    }
    ensure!(populated, "Unlisted empty signed package directory");
    Ok(())
}

fn runtime_for(root: &Path, target: &str, lock: &Lock) -> Result<Option<RuntimeBinding>> {
    ensure!(lock.schema == 1, "Invalid compiled signed package lock");
    let (manifest, manifest_sha) = manifest(root)?;
    let marked = manifest.get("private_derivative").is_some()
        || manifest["native_runtime"]["kind"] == "private_derivative";
    let selected = lock.targets.get(target);
    if !marked && selected.is_none() {
        return Ok(None);
    }
    let descriptor = selected.context("This private package has no reviewed signed cohort")?;
    descriptor.validate(target)?;
    ensure!(
        marked && manifest_sha == descriptor.manifest_sha256,
        "The expected signed package is missing or changed"
    );
    ensure!(
        manifest["schema"] == 1
            && manifest["kind"] == "instance-consumer-bundle"
            && manifest["target"] == target
            && manifest["source"] == descriptor.instance.source
            && manifest["version"] == descriptor.instance.version
            && manifest["binary"] == "agent-instance",
        "Signed Instance identity differs"
    );
    ensure!(
        manifest["private_derivative"]
            == json!({"schema":1,"kind":"signed_macos_instance","cohort_id":descriptor.cohort_id}),
        "Signed package origin is not explicit"
    );
    let runtime = &manifest["native_runtime"];
    let platform = if target == "aarch64-apple-darwin" {
        "macos-aarch64"
    } else {
        "macos-x86_64"
    };
    ensure!(
        runtime["kind"] == "private_derivative"
            && runtime["binary"] == RUNTIME
            && runtime["version"] == descriptor.runtime.version
            && runtime["binary_sha256"] == descriptor.runtime_binary_sha256
            && runtime["platform"] == platform
            && runtime.get("archive").is_none()
            && runtime.get("archive_sha256").is_none()
            && runtime["public_origin"]
                == json!({"source":descriptor.runtime.source,"archive_sha256":descriptor.runtime.archive_sha256,"binary_sha256":descriptor.runtime.binary_sha256}),
        "Signed Runtime identity differs"
    );
    ensure!(
        manifest["python_archive_sha256"] == descriptor.python.archive_sha256,
        "Private Python origin differs"
    );
    let files = manifest["files"]
        .as_object()
        .context("Missing signed package inventory")?;
    ensure!(
        !files.is_empty() && files.len() <= 20_000 && !files.contains_key(MANIFEST),
        "Invalid signed package inventory"
    );
    for (name, expected) in [
        ("agent-instance", &descriptor.instance_binary_sha256),
        (RUNTIME, &descriptor.runtime_binary_sha256),
        (
            "setup/native-runtime-lock.json",
            &descriptor.runtime.lock_sha256,
        ),
        (
            "setup/private-python-lock.json",
            &descriptor.python.lock_sha256,
        ),
    ] {
        ensure!(
            files.get(name).and_then(Value::as_str) == Some(expected),
            "Signed package required member differs"
        );
    }
    let mut total = 0u64;
    for (name, expected) in files {
        ensure!(
            expected.as_str().is_some_and(|s| hex(s, 64)),
            "Invalid signed member digest"
        );
        let path = regular(root, name)?;
        let (actual, size) = digest(&path, MAX_BYTES - total)?;
        total += size;
        ensure!(expected == &actual, "Signed package member changed");
    }
    let mut actual = BTreeSet::new();
    listed(root, root, &mut actual)?;
    let mut expected: BTreeSet<_> = files.keys().cloned().collect();
    expected.insert(MANIFEST.into());
    ensure!(actual == expected, "Signed package inventory is not closed");
    Ok(Some(RuntimeBinding {
        path: regular(root, RUNTIME)?,
        cohort_id: descriptor.cohort_id.clone(),
        manifest_sha256: manifest_sha,
    }))
}

pub(crate) fn runtime(root: &Path) -> Result<Option<RuntimeBinding>> {
    runtime_for(root, &target(), &serde_json::from_str(LOCK)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    const TARGET: &str = "aarch64-apple-darwin";
    fn sha(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }
    struct Fixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
        manifest: Value,
        lock: Lock,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("retained");
            std::fs::create_dir(&root).unwrap();
            let members = [
                ("agent-instance", "signed Instance"),
                (RUNTIME, "signed Runtime"),
                (
                    "setup/native-runtime-lock.json",
                    "original public Runtime lock",
                ),
                ("setup/private-python-lock.json", "original Python lock"),
                ("setup/native-install.py", "unchanged owner setup"),
                (
                    "setup/consumer-bundle.py",
                    "unchanged owner bundle validation",
                ),
                ("support/python/bin/python3.13", "signed private Python"),
                ("engine/native/notices/LICENSE", "original Runtime notice"),
            ];
            let mut files = BTreeMap::new();
            for (name, bytes) in members {
                let path = root.join(name);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, bytes).unwrap();
                files.insert(name, sha(bytes.as_bytes()));
            }
            let descriptor = Descriptor {
                schema: 1,
                cohort_id: uuid::Uuid::new_v4().to_string(),
                target: TARGET.into(),
                instance: PublicInstance {
                    source: "a".repeat(40),
                    tree: "b".repeat(40),
                    version: "1.2.3".into(),
                    archive_sha256: "c".repeat(64),
                    manifest_sha256: "d".repeat(64),
                },
                runtime: PublicRuntime {
                    source: "e".repeat(40),
                    version: "2.3.4".into(),
                    archive_sha256: "f".repeat(64),
                    binary_sha256: "0".repeat(64),
                    lock_sha256: files["setup/native-runtime-lock.json"].clone(),
                },
                python: PublicPython {
                    archive_sha256: "1".repeat(64),
                    lock_sha256: files["setup/private-python-lock.json"].clone(),
                },
                manifest_sha256: "2".repeat(64),
                instance_binary_sha256: files["agent-instance"].clone(),
                runtime_binary_sha256: files[RUNTIME].clone(),
                signing_receipt_sha256: "3".repeat(64),
                signing_team_id: "AB12CD34EF".into(),
            };
            let manifest = json!({"schema":1,"kind":"instance-consumer-bundle","target":TARGET,"version":descriptor.instance.version,"source":descriptor.instance.source,"binary":"agent-instance",
                "private_derivative":{"schema":1,"kind":"signed_macos_instance","cohort_id":descriptor.cohort_id},
                "native_runtime":{"kind":"private_derivative","binary":RUNTIME,"version":descriptor.runtime.version,"platform":"macos-aarch64","binary_sha256":descriptor.runtime_binary_sha256,
                    "public_origin":{"source":descriptor.runtime.source,"archive_sha256":descriptor.runtime.archive_sha256,"binary_sha256":descriptor.runtime.binary_sha256}},
                "python_directory":"python","python_executable":"bin/python3.13","python_archive_sha256":descriptor.python.archive_sha256,"files":files});
            let mut fixture = Self {
                _temp: temp,
                root,
                manifest,
                lock: Lock {
                    schema: 1,
                    targets: BTreeMap::from([(TARGET.into(), descriptor)]),
                },
            };
            fixture.freeze();
            fixture
        }
        fn freeze(&mut self) {
            let bytes = serde_json::to_vec(&self.manifest).unwrap();
            std::fs::write(self.root.join(MANIFEST), &bytes).unwrap();
            self.lock.targets.get_mut(TARGET).unwrap().manifest_sha256 = sha(&bytes);
        }
        fn verify(&self) -> Result<Option<RuntimeBinding>> {
            runtime_for(&self.root, TARGET, &self.lock)
        }
    }
    #[test]
    fn compiled_descriptor_binds_closed_retained_bytes_and_original_locks() {
        let f = Fixture::new();
        let bound = f.verify().unwrap().unwrap();
        assert_eq!(bound.path, f.root.join(RUNTIME));
        assert_eq!(bound.cohort_id, f.lock.targets[TARGET].cohort_id);
        assert_eq!(
            std::fs::read(f.root.join("setup/native-runtime-lock.json")).unwrap(),
            b"original public Runtime lock"
        );
        assert_eq!(
            bound.manifest_sha256,
            f.lock.targets[TARGET].manifest_sha256
        );
    }
    #[test]
    fn empty_lock_wrong_target_and_missing_private_marker_never_fall_back() {
        let mut f = Fixture::new();
        let empty: Lock = serde_json::from_str(LOCK).unwrap();
        assert!(empty.targets.is_empty());
        assert!(runtime_for(&f.root, TARGET, &empty).is_err());
        assert!(runtime_for(&f.root, "x86_64-apple-darwin", &f.lock).is_err());
        assert!(runtime_for(&f.root, "aarch64-unknown-linux-gnu", &f.lock).is_err());
        f.manifest
            .as_object_mut()
            .unwrap()
            .remove("private_derivative");
        f.manifest["native_runtime"]
            .as_object_mut()
            .unwrap()
            .remove("kind");
        f.freeze();
        assert!(f.verify().is_err());
        assert!(runtime_for(&f.root, TARGET, &empty).unwrap().is_none());
    }
    #[test]
    fn changed_member_manifest_or_unlisted_entry_refuses_without_repair() {
        for name in [
            RUNTIME,
            "agent-instance",
            "support/python/bin/python3.13",
            "setup/native-runtime-lock.json",
        ] {
            let f = Fixture::new();
            std::fs::write(f.root.join(name), b"changed").unwrap();
            assert!(f.verify().is_err());
            assert_eq!(std::fs::read(f.root.join(name)).unwrap(), b"changed");
        }
        let f = Fixture::new();
        std::fs::write(f.root.join(MANIFEST), b"{}").unwrap();
        assert!(f.verify().is_err());
        let f = Fixture::new();
        std::fs::write(f.root.join("unlisted"), b"preserve").unwrap();
        assert!(f.verify().is_err());
        assert_eq!(std::fs::read(f.root.join("unlisted")).unwrap(), b"preserve");
        let f = Fixture::new();
        std::fs::create_dir(f.root.join("unlisted-empty")).unwrap();
        assert!(f.verify().is_err());
    }
    #[test]
    fn signed_origin_and_fixed_runtime_cannot_be_rebound_by_manifest_alone() {
        for field in ["binary", "binary_sha256", "version", "platform", "archive"] {
            let mut f = Fixture::new();
            f.manifest["native_runtime"][field] = json!("other");
            f.freeze();
            assert!(f.verify().is_err(), "{field}");
        }
        let mut f = Fixture::new();
        f.manifest["native_runtime"]["public_origin"]["archive_sha256"] = json!("4".repeat(64));
        f.freeze();
        assert!(f.verify().is_err());
        let mut f = Fixture::new();
        f.manifest["python_archive_sha256"] = json!("5".repeat(64));
        f.freeze();
        assert!(f.verify().is_err());
        let mut f = Fixture::new();
        f.manifest["files"]["alias/../agent-instance"] =
            json!(f.lock.targets[TARGET].instance_binary_sha256);
        f.freeze();
        assert!(f.verify().is_err());
    }
    #[test]
    fn malformed_or_nonmac_compiled_descriptor_refuses() {
        let mut f = Fixture::new();
        f.lock.targets.get_mut(TARGET).unwrap().signing_team_id = "unreviewed".into();
        assert!(f.verify().is_err());
        let mut f = Fixture::new();
        f.lock.targets.get_mut(TARGET).unwrap().instance.source = "main".into();
        assert!(f.verify().is_err());
        let f = Fixture::new();
        let mut descriptor = serde_json::to_value(&f.lock.targets[TARGET]).unwrap();
        descriptor["download_url"] = json!("https://example.invalid");
        assert!(serde_json::from_value::<Descriptor>(descriptor).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn signed_member_or_directory_link_is_never_adopted() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new();
        let original = f.root.join(RUNTIME);
        let outside = f._temp.path().join("outside");
        std::fs::rename(&original, &outside).unwrap();
        symlink(&outside, &original).unwrap();
        assert!(f.verify().is_err());
        let f = Fixture::new();
        symlink(f.root.join("setup"), f.root.join("other")).unwrap();
        assert!(f.verify().is_err());
    }
}
