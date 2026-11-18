use super::super::*;
use amplifier_native_platform::{
    installation::{self, ActionOutcome, Service, ServiceAction, ServiceManager},
    process,
};
use rusqlite::{OpenFlags, types::ValueRef};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Read,
    net::{IpAddr, SocketAddr, TcpStream},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    thread,
};

#[path = "user_bus.rs"]
mod user_bus;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileIdentity {
    pub dev: u64,
    pub ino: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manager {
    pub service: Service,
    pub owner_uid: u32,
    pub definition_sha256: String,
    pub effective_definition_sha256: String,
    pub control_group: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EffectsReview {
    pub running_records_sha256s: Vec<String>,
    pub external_effects_accounted: bool,
    pub historical_unknowns_preserved: bool,
    pub no_replay: bool,
    pub evidence_sha256: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub schema: u32,
    pub request_id: String,
    pub installation_id: String,
    pub home: PathBuf,
    pub owner_uid: u32,
    pub candidate_sha256: String,
    pub old_binary_sha256: String,
    pub old_version: String,
    pub config_sha256: String,
    pub raw_state_sha256: String,
    pub service_lock: FileIdentity,
    pub manager: Manager,
    pub manager_identity: Value,
    pub old_processes: Vec<Value>,
    pub owner_effects: EffectsReview,
    pub copied_state_review_sha256: String,
}

fn hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn identity(meta: &std::fs::Metadata) -> FileIdentity {
    FileIdentity {
        dev: meta.dev(),
        ino: meta.ino(),
    }
}
fn private_file(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let meta = f.metadata()?;
    ensure!(
        meta.is_file()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.mode() & 0o077 == 0
            && meta.len() <= limit,
        "Use a bounded existing owner-only file"
    );
    let mut bytes = Vec::new();
    f.by_ref().take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit
            && identity(&meta) == identity(&std::fs::symlink_metadata(path)?),
        "Private input changed while reading"
    );
    Ok(bytes)
}
fn private_home(home: &Path) -> Result<()> {
    ensure!(
        home.is_absolute() && home.canonicalize()? == home,
        "Use the original absolute installation home"
    );
    for p in [home.to_path_buf(), home.join("secrets"), home.join("bin")] {
        let m = std::fs::symlink_metadata(p)?;
        ensure!(
            m.is_dir()
                && !m.file_type().is_symlink()
                && m.uid() == unsafe { libc::geteuid() }
                && m.mode() & 0o077 == 0,
            "Existing private installation policy is required; it is not repaired"
        );
    }
    configured_tls_is_bound(home)?;
    ensure!(
        private_file(&home.join("secrets/control-token"), 65536)?.len() >= 32,
        "Existing control credential is required"
    );
    Ok(())
}
fn configured_tls_is_bound(home: &Path) -> Result<()> {
    let c = config::Config::load(home)?;
    if c.web_transport == config::WebTransport::LocalHttp {
        return Ok(());
    }
    for path in [&c.tls_cert, &c.tls_key]
        .into_iter()
        .chain(c.tls_ca.as_ref())
    {
        retained_tls_path(home, path)?;
    }
    Ok(())
}
fn retained_tls_path(home: &Path, path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute()
            && path.starts_with(home.join("tls"))
            && path.canonicalize()? == path
            && std::fs::symlink_metadata(path)?.is_file(),
        "Managed adoption supports only regular configured TLS files inside the retained home/tls tree"
    );
    Ok(())
}
fn service_guard(home: &Path, expected: &FileIdentity) -> Result<File> {
    let p = home.join("service.lock");
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&p)?;
    let m = f.metadata()?;
    ensure!(
        m.is_file()
            && m.uid() == unsafe { libc::geteuid() }
            && m.mode() & 0o077 == 0
            && identity(&m) == *expected,
        "Existing service lock identity or policy changed"
    );
    f.try_lock_exclusive()
        .context("The original installation is not stopped")?;
    ensure!(
        identity(&std::fs::symlink_metadata(p)?) == *expected,
        "Service lock pathname changed"
    );
    Ok(f)
}
fn lock_identity(home: &Path) -> Result<FileIdentity> {
    Ok(identity(&std::fs::symlink_metadata(
        home.join("service.lock"),
    )?))
}
fn launch_guard(home: &Path) -> Result<File> {
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(home.join("desktop-update-start.lock"))?;
    let m = f.metadata()?;
    ensure!(
        m.is_file() && m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o077 == 0,
        "Managed startup lock policy changed"
    );
    f.try_lock_exclusive()
        .context("Managed startup/initialization is still in progress")?;
    Ok(f)
}

/// A logical read-only snapshot includes unknown schema and every stored value.
/// Only content hashes and selected record-key hashes leave this function.
fn raw_state(home: &Path) -> Result<(String, Vec<String>)> {
    let path = home.join("state.sqlite3");
    ordinary(&path)?;
    let mut db = rusqlite::Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let tx = db.transaction()?;
    let mut all = Vec::<Value>::new();
    let mut active = Vec::new();
    {
        let mut q =
            tx.prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name")?;
        let schema = q
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        all.push(serde_json::to_value(&schema)?);
        for (kind, name, _, _) in schema {
            if kind != "table" {
                continue;
            }
            let sql = format!("SELECT * FROM \"{}\"", name.replace('"', "\"\""));
            let mut q = tx.prepare(&sql)?;
            let n = q.column_count();
            let mut rows = q.query([])?;
            let mut values = Vec::new();
            while let Some(row) = rows.next()? {
                let mut cols = Vec::new();
                for i in 0..n {
                    cols.push(match row.get_ref(i)? {
                        ValueRef::Null => json!(["null"]),
                        ValueRef::Integer(x) => json!(["integer", x]),
                        ValueRef::Real(x) => json!(["real", x.to_bits()]),
                        ValueRef::Text(x) => json!(["text", x]),
                        ValueRef::Blob(x) => json!(["blob", x]),
                    });
                }
                values.push(serde_json::to_string(&cols)?);
                if name == "records" && n == 3 {
                    let kind: String = row.get(0)?;
                    let id: String = row.get(1)?;
                    if matches!(kind.as_str(), "sessions" | "jobs") {
                        let v: Value = serde_json::from_str(&row.get::<_, String>(2)?)?;
                        if matches!(
                            v["status"].as_str(),
                            Some("running" | "queued" | "disconnecting")
                        ) {
                            active.push(sha(format!("{kind}\0{id}").as_bytes()));
                        }
                    }
                }
            }
            values.sort();
            all.push(json!([name, values]));
        }
    }
    tx.rollback()?;
    active.sort();
    Ok((sha(&serde_json::to_vec(&all)?), active))
}
fn resource_digest(home: &Path, names: &[&str]) -> Result<String> {
    fn walk(root: &Path, p: &Path, rows: &mut Vec<Value>) -> Result<()> {
        let m = std::fs::symlink_metadata(p)?;
        ensure!(
            !m.file_type().is_symlink(),
            "Linked installation resources are unsupported"
        );
        if m.is_dir() {
            for item in std::fs::read_dir(p)? {
                walk(root, &item?.path(), rows)?;
            }
        } else {
            ensure!(m.is_file(), "Unsupported installation resource");
            rows.push(json!([p.strip_prefix(root)?, sha(&std::fs::read(p)?)]));
        }
        ensure!(
            rows.len() <= 100_000,
            "Installation resources exceed the observation bound"
        );
        Ok(())
    }
    let mut rows = Vec::new();
    for name in names {
        let p = home.join(name);
        if p.exists() {
            walk(home, &p, &mut rows)?;
        }
    }
    rows.sort_by_key(Value::to_string);
    Ok(sha(&serde_json::to_vec(&rows)?))
}
fn resources(home: &Path) -> Result<String> {
    resource_digest(
        home,
        &["config.json", "secrets", "tls", "agent-observatory.service"],
    )
}
fn support(home: &Path) -> Result<String> {
    resource_digest(home, &["support", "machine-helper-artifacts"])
}
fn quote(path: &Path) -> String {
    format!(
        "\"{}\"",
        path.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
    )
}
/// The unit the source installer writes. `Restart=on-failure` alone counts a
/// termination signal as a clean exit, so a service ended by another program
/// stayed down. `RestartForceExitStatus=SIGTERM` restarts it after that signal;
/// an exit with status 0 (the update's own admission stop) and a manager Stop
/// still leave the unit inactive, which every managed transition relies on.
fn unit_text(home: &Path) -> String {
    format!(
        "[Unit]\nDescription=Agent Observatory\nAfter=network-online.target\n\n[Service]\nType=simple\nExecStart={} --home {} serve\nRestart=on-failure\nRestartForceExitStatus=SIGTERM\nRestartSec=5\nUMask=0077\nKillMode=control-group\n\n[Install]\nWantedBy=default.target\n",
        quote(&platform::installed_binary(&platform::OBSERVATORY, home)),
        quote(home)
    )
}
/// The unit the source installer wrote through 0.12.2. Nothing here rewrites an
/// installed unit, so existing installations keep this exact text.
fn earlier_unit_text(home: &Path) -> String {
    format!(
        "[Unit]\nDescription=Agent Observatory\nAfter=network-online.target\n\n[Service]\nType=simple\nExecStart={} --home {} serve\nRestart=on-failure\nRestartSec=5\nUMask=0077\nKillMode=control-group\n\n[Install]\nWantedBy=default.target\n",
        quote(&platform::installed_binary(&platform::OBSERVATORY, home)),
        quote(home)
    )
}
fn manager_from(value: &Value) -> Result<Manager> {
    Ok(serde_json::from_value(value["manager"].clone())?)
}
fn manager_definition(home: &Path, m: &Manager, s: &Value) -> Result<()> {
    let c = config::Config::load(home)?;
    ensure!(
        m.owner_uid == unsafe { libc::geteuid() }
            && m.service.manager == ServiceManager::SystemdUser
            && m.service.name == format!("agent-observatory-{}.service", c.id),
        "Only the original current-user Observatory unit is supported"
    );
    let p = m
        .service
        .definition_path
        .as_ref()
        .context("Original regular unit file required")?;
    let md = std::fs::symlink_metadata(p)?;
    ensure!(
        p.is_absolute()
            && md.is_file()
            && !md.file_type().is_symlink()
            && md.uid() == m.owner_uid
            && md.mode() & 0o022 == 0,
        "Original unit owner/policy changed"
    );
    let bytes = std::fs::read(p)?;
    // Exactly the two texts the source installer has written, byte for byte.
    ensure!(
        bytes == unit_text(home).as_bytes() || bytes == earlier_unit_text(home).as_bytes(),
        "Only the unchanged original Observatory unit shape is supported"
    );
    ensure!(
        sha(&bytes) == m.definition_sha256
            && s["definition"]["sha256"] == m.definition_sha256
            && s["effective_definition"]["sha256"] == m.effective_definition_sha256,
        "Original unit definition changed"
    );
    // systemctl can omit an empty structured ExecStop array even with --all.
    // This representation is accepted only with the exact original unit above
    // and the explicit no-drop-in/effective-definition guards below.
    // Both accepted texts keep Restart=on-failure. The shared snapshot does not
    // report RestartForceExitStatus; the exact bytes and those same guards bind it.
    ensure!(
        s["definition_matches"] == true
            && s["masked"] == false
            && s["loaded"] == true
            && s["native"]["DropInPaths"] == ""
            && s["native"]
                .get("ExecStop")
                .is_none_or(|value| value.as_str() == Some(""))
            && s["native"]["KillMode"] == "control-group"
            && s["native"]["Type"] == "simple"
            && s["native"]["Restart"] == "on-failure",
        "Original unit effective behavior changed"
    );
    let cg = Path::new(&m.control_group);
    ensure!(
        m.control_group.starts_with(&format!(
            "/user.slice/user-{}.slice/user@{}.service/",
            m.owner_uid, m.owner_uid
        )) && cg.file_name().and_then(|x| x.to_str()) == Some(&m.service.name)
            && !cg
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
        "Use the exact original user-unit cgroup"
    );
    Ok(())
}
/// Supplement shared snapshots with fixed read-only fields they do not expose.
fn manager_extra(m: &Manager) -> Result<BTreeMap<String, String>> {
    let child = Command::new("/usr/bin/systemctl")
        .args([
            "--user",
            "--no-pager",
            "--no-ask-password",
            "show",
            "--property=ControlGroup,Job",
            "--",
            &m.service.name,
        ])
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let bytes = manager_output(child)?;
    let mut out = BTreeMap::new();
    for line in std::str::from_utf8(&bytes)?.lines() {
        let (k, v) = line
            .split_once('=')
            .context("Invalid manager observation")?;
        ensure!(
            out.insert(k.to_owned(), v.to_owned()).is_none(),
            "Duplicate manager observation"
        );
    }
    ensure!(
        out.len() == 2 && out.contains_key("ControlGroup") && out.contains_key("Job"),
        "Incomplete manager observation"
    );
    Ok(out)
}

/// Every early error cleans up only this read-only probe, with a bounded reap.
struct ManagerProbe {
    child: std::process::Child,
    reaped: bool,
}
impl Drop for ManagerProbe {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        let _ = self.child.kill();
        let limit = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) if std::time::Instant::now() < limit => {
                    thread::sleep(Duration::from_millis(10))
                }
                Ok(None) => break,
            }
        }
    }
}
fn manager_output(child: std::process::Child) -> Result<Vec<u8>> {
    manager_output_until(child, std::time::Instant::now() + Duration::from_secs(3))
}
fn manager_output_until(child: std::process::Child, limit: std::time::Instant) -> Result<Vec<u8>> {
    let mut probe = ManagerProbe {
        child,
        reaped: false,
    };
    let mut stdout = probe
        .child
        .stdout
        .take()
        .context("Manager output pipe is unavailable")?;
    let fd = stdout.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    ensure!(
        flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == 0,
        "Cannot bound manager output read"
    );
    let mut bytes = Vec::new();
    let mut ended = false;
    let mut status = None;
    loop {
        ensure!(
            std::time::Instant::now() < limit,
            "Manager observation deadline expired; no service action dispatched"
        );
        let mut chunk = [0u8; 4096];
        match stdout.read(&mut chunk) {
            Ok(0) => ended = true,
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.into()),
        }
        if status.is_none() {
            status = probe.child.try_wait()?;
            probe.reaped = status.is_some();
        }
        if ended && status.is_some() {
            break;
        }
        ensure!(
            std::time::Instant::now() < limit && bytes.len() <= 65536,
            "Bounded manager observation failed; no service action dispatched"
        );
        thread::sleep(Duration::from_millis(5));
    }
    ensure!(
        status.unwrap().success() && bytes.len() <= 65536,
        "Manager observation failed"
    );
    Ok(bytes)
}
fn cgroup_empty(m: &Manager) -> Result<()> {
    fn walk(path: &Path) -> Result<()> {
        ensure!(!path.is_symlink(), "Linked cgroup refused");
        ensure!(
            std::fs::read_to_string(path.join("cgroup.procs"))?
                .trim()
                .is_empty(),
            "Original service still has cgroup members"
        );
        let events = std::fs::read_to_string(path.join("cgroup.events"))?;
        ensure!(
            events.lines().any(|l| l == "populated 0"),
            "Original service cgroup remains populated"
        );
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                walk(&entry.path())?;
            }
        }
        Ok(())
    }
    let p = Path::new("/sys/fs/cgroup").join(m.control_group.trim_start_matches('/'));
    match std::fs::symlink_metadata(&p) {
        Ok(_) => walk(&p),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
fn listener_absent(home: &Path) -> Result<()> {
    let c = config::Config::load(home)?;
    let mut ip: IpAddr = c
        .bind
        .parse()
        .context("Adoption requires a fixed numeric bind address")?;
    if ip.is_unspecified() {
        ip = if ip.is_ipv4() {
            "127.0.0.1".parse()?
        } else {
            "::1".parse()?
        };
    }
    match TcpStream::connect_timeout(&SocketAddr::new(ip, c.port), Duration::from_secs(1)) {
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => Ok(()),
        _ => anyhow::bail!("Original listener absence is unproved"),
    }
}
fn old_generations_absent(list: &Value) -> Result<()> {
    for p in list
        .as_array()
        .context("Missing captured process generations")?
    {
        ensure!(
            !process::matches(p)?,
            "A captured process generation remains alive"
        );
    }
    Ok(())
}
fn stopped_proof(home: &Path, j: &Value) -> Result<Value> {
    let m = manager_from(j)?;
    let first = installation::snapshot(&m.service)?;
    manager_definition(home, &m, &first)?;
    ensure!(
        first["native"]["ActiveState"] == "inactive"
            && first["transitioning"] == false
            && first["processes"] == json!([]),
        "Owner maintenance must leave the original unit inactive, with no observed process"
    );
    let extra = manager_extra(&m)?;
    ensure!(
        extra["Job"].is_empty()
            && (extra["ControlGroup"].is_empty() || extra["ControlGroup"] == m.control_group),
        "Original unit has a pending job or different cgroup"
    );
    old_generations_absent(&j["old_processes"])?;
    cgroup_empty(&m)?;
    listener_absent(home)?;
    ensure!(
        installation::snapshot(&m.service)? == first,
        "Original unit changed during absence proof"
    );
    Ok(first)
}
fn manager_parent(identity: &Value) -> Result<()> {
    ensure!(
        process::matches(identity)?,
        "The reviewed user manager generation changed"
    );
    let pid = identity["pid"]
        .as_u64()
        .context("Missing user manager PID")?;
    let cmd = std::fs::read(format!("/proc/{pid}/cmdline"))?;
    ensure!(
        cmd.split(|b| *b == 0).any(|s| s == b"--user"),
        "Reviewed manager is not a user manager"
    );
    user_bus::prove(identity)?;
    Ok(())
}
fn running_proof(home: &Path, j: &Value, selected: &Value) -> Result<Value> {
    let m = manager_from(j)?;
    let s = installation::snapshot(&m.service)?;
    manager_definition(home, &m, &s)?;
    ensure!(
        s["native"]["ActiveState"] == "active" && s["transitioning"] == false,
        "Original unit is not stably running"
    );
    let p = s["processes"]
        .as_array()
        .context("Missing service processes")?
        .iter()
        .find(|p| p["role"] == "MainPID")
        .context("Missing original unit main process")?["identity"]
        .clone();
    let pid = p["pid"].as_u64().context("Missing process PID")?;
    ensure!(
        process::parent_identity(&p)?.as_ref() == Some(&j["manager_identity"]),
        "Original manager parent generation differs"
    );
    manager_parent(&j["manager_identity"])?;
    let exe = std::fs::read_link(format!("/proc/{pid}/exe"))?;
    ensure!(
        exe == platform::installed_binary(&platform::OBSERVATORY, home),
        "Original unit executable path changed"
    );
    ensure!(
        sha(&std::fs::read(format!("/proc/{pid}/exe"))?) == selected["binary_sha256"],
        "Original unit executable bytes differ"
    );
    let actual = std::fs::read(format!("/proc/{pid}/cmdline"))?;
    let expected = [
        exe.to_string_lossy().as_bytes(),
        b"--home",
        home.as_os_str().as_encoded_bytes(),
        b"serve",
    ]
    .iter()
    .flat_map(|v| v.iter().copied().chain([0]))
    .collect::<Vec<_>>();
    ensure!(actual == expected, "Original unit invocation changed");
    let cg = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))?;
    ensure!(
        cg.lines().any(|l| l == format!("0::{}", m.control_group)),
        "Process is outside the original service cgroup"
    );
    ensure!(
        process::matches(&p)? && installation::snapshot(&m.service)? == s,
        "Original unit generation changed during application proof"
    );
    Ok(json!({"process":p,"invocation_id":s["native"]["InvocationID"],"snapshot":s}))
}
fn claim_path(home: &Path, j: &Value, rollback: bool) -> Result<PathBuf> {
    Ok(
        operation(home, j["request_id"].as_str().context("Missing request")?)?.join(if rollback {
            "rollback-start-proof.json"
        } else {
            "candidate-start-proof.json"
        }),
    )
}
fn validate_journal(home: &Path, j: &Value) -> Result<()> {
    ensure!(
        super::journal(j),
        "Unsupported managed journal schema or kind"
    );
    ensure!(
        j["installation_id"] == config::Config::load(home)?.id,
        "Managed journal belongs to a different installation"
    );
    Ok(())
}
fn validate_baseline(home: &Path, baseline: &Value) -> Result<()> {
    ensure!(
        baseline["schema"] == 1 && baseline["id"] == config::Config::load(home)?.id,
        "Managed baseline schema or installation identity differs"
    );
    Ok(())
}
fn fixed_inputs(home: &Path, j: &Value) -> Result<()> {
    configured_tls_is_bound(home)?;
    ensure!(
        sha(&std::fs::read(home.join("config.json"))?) == j["config_sha256"],
        "Original configuration changed"
    );
    ensure!(
        resources(home)? == j["resources_sha256"],
        "Retained installation resources changed"
    );
    ensure!(
        serde_json::to_value(lock_identity(home)?)? == j["service_lock"],
        "Original service lock was replaced"
    );
    if j["kind"] == "managed_adoption" {
        ensure!(
            support(home)? == j["previous_support_sha256"],
            "Original retained support changed"
        );
    }
    Ok(())
}
fn create_or_equal(path: &Path, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if path.exists() {
        ensure!(
            private_file(path, 2 * 1024 * 1024)? == bytes,
            "Existing publication differs; retained without repair"
        );
        return Ok(());
    }
    platform::private_create(path, &bytes)
}

/// Plain serve is permitted only for the current original manager generation.
/// A create-only claim admits at most one generation for each saved launch intent.
pub fn guard(home: &Path, j: Option<&Value>) -> Result<File> {
    let hold = launch_guard(home)?;
    // The caller's first read is outside this lock. Revalidate every branch,
    // including terminal and baseline-only starts, against recovery revocation.
    let current = optional_journal(home)?;
    if let Some(expected) = j {
        ensure!(
            current.as_ref() == Some(expected),
            "Managed launch was revoked before its initialization hold"
        );
        validate_journal(home, expected)?;
    } else if let Some(current) = current.as_ref() {
        ensure!(
            !super::journal(current)
                && super::known_kind(current)
                && (complete(current)
                    || matches!(current["phase"].as_str(), Some("prepared" | "deferred"))),
            "A new managed transition appeared before initialization"
        );
    }
    let owned;
    let j = if let Some(j) = j {
        j
    } else {
        owned = read(&home.join("app-install.json"))?;
        &owned
    };
    if !super::journal(j) {
        // Normal restarts of an enrolled manager installation bind the current
        // manager parent anew; there is no transition to recover or authority to replay.
        let baseline = read(&home.join("app-install.json"))?;
        validate_baseline(home, &baseline)?;
        let self_id = process::identity(std::process::id())?.context("Current process vanished")?;
        let parent =
            process::parent_identity(&self_id)?.context("Missing current manager parent")?;
        let evidence = json!({"manager":baseline["manager"],"manager_identity":parent});
        let proof = running_proof(home, &evidence, &baseline)?;
        ensure!(
            proof["process"] == self_id,
            "Only the original manager may launch this installation"
        );
        return Ok(hold);
    }
    let phase = j["phase"].as_str().context("Missing managed phase")?;
    if matches!(phase, "applied" | "rolled_back") {
        let selected = if phase == "applied" {
            &j["candidate"]
        } else {
            &j["previous"]
        };
        let mut evidence = j.clone();
        let me = process::identity(std::process::id())?.context("Current process vanished")?;
        evidence["manager_identity"] =
            process::parent_identity(&me)?.context("Missing manager parent")?;
        ensure!(
            running_proof(home, &evidence, selected)?["process"] == me,
            "Only the original manager may launch verified bytes"
        );
        return Ok(hold);
    }
    fixed_inputs(home, j)?;
    if j["kind"] == "managed_update"
        && matches!(phase, "prepared" | "deferred")
        && j.get("admission_stop_intent").is_none()
    {
        ensure!(
            running_proof(home, j, &j["previous"])?["process"]["pid"] == std::process::id(),
            "Only the original manager may continue before admission closure"
        );
        return Ok(hold);
    }
    ensure!(
        matches!(
            phase,
            "adoption_starting" | "starting" | "rollback_starting"
        ),
        "Managed transition needs owner reconciliation before startup"
    );
    let rollback = phase == "rollback_starting";
    let selected = if rollback {
        &j["previous"]
    } else {
        &j["candidate"]
    };
    ensure!(
        selected["version"] == env!("CARGO_PKG_VERSION")
            && j[if rollback {
                "rollback_start_intent"
            } else {
                "start_intent"
            }]
            .is_object(),
        "Managed launch intent differs from this build"
    );
    let proof = running_proof(home, j, selected)?;
    ensure!(
        proof["process"]["pid"] == std::process::id(),
        "Manual process cannot consume a managed startup intent"
    );
    let current = journal(home)?;
    ensure!(current == *j, "Managed launch was revoked during proof");
    create_or_equal(
        &claim_path(home, j, rollback)?,
        &json!({"request_id":j["request_id"],"binary_sha256":selected["binary_sha256"],"process":proof["process"],"invocation_id":proof["invocation_id"]}),
    )?;
    Ok(hold)
}

fn retained_baseline(home: &Path, package: &Value, manager: &Manager) -> Result<Value> {
    let c = config::Config::load(home)?;
    let db = rusqlite::Connection::open_with_flags(
        home.join("state.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let mut q = db.prepare("SELECT value FROM records WHERE kind='hosts'")?;
    let hosts = q
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut selected = None;
    for raw in hosts {
        let host: Value = serde_json::from_str(&raw)?;
        if host["native_local"] == true {
            ensure!(
                selected.is_none(),
                "Multiple owned local hosts require review"
            );
            selected = Some(host);
        }
    }
    let recipe = selected
        .as_ref()
        .and_then(|h| h["recipes"].as_array())
        .and_then(|v| v.iter().find(|r| r["id"] == "bundled-instance"))
        .cloned();
    let instance = home.join("support/instance/consumer-bundle.json");
    let instance_digest = if instance.exists() {
        crate::local_install::inventory(instance.parent().unwrap())?;
        Some(sha(&std::fs::read(instance)?))
    } else {
        None
    };
    Ok(
        json!({"schema":1,"id":c.id,"version":package["version"],"binary_sha256":package["binary_sha256"],"package":package,"instance_manifest_sha256":instance_digest,"helpers_manifest_sha256":crate::helper_support::installed(home)?,"host_id":selected.as_ref().map(|h|&h["id"]),"recipe":recipe,"manager":manager}),
    )
}
fn validate_review(home: &Path, r: &Review, package: &Value) -> Result<()> {
    private_home(home)?;
    ensure!(
        r.schema == 1
            && r.home == home
            && r.owner_uid == unsafe { libc::geteuid() }
            && r.installation_id == config::Config::load(home)?.id,
        "Review installation identity differs"
    );
    uuid::Uuid::parse_str(&r.request_id)?;
    for h in [
        &r.candidate_sha256,
        &r.old_binary_sha256,
        &r.config_sha256,
        &r.raw_state_sha256,
        &r.owner_effects.evidence_sha256,
        &r.copied_state_review_sha256,
        &r.manager.definition_sha256,
        &r.manager.effective_definition_sha256,
    ] {
        ensure!(hex(h), "Review requires exact SHA256 bindings");
    }
    ensure!(
        package["binary_sha256"] == r.candidate_sha256
            && package["version"] == env!("CARGO_PKG_VERSION"),
        "Review must bind this exact complete candidate package"
    );
    ensure!(
        version_numbers(&package["version"])? >= version_numbers(&json!(r.old_version))?,
        "Adoption cannot downgrade the original application"
    );
    ensure!(
        (1..=4096).contains(&r.old_processes.len()),
        "Bind the captured original process generations"
    );
    ensure!(
        r.owner_effects.external_effects_accounted
            && r.owner_effects.historical_unknowns_preserved
            && r.owner_effects.no_replay,
        "An explicit owner effects review preserving historical unknowns is required"
    );
    let (digest, active) = raw_state(home)?;
    ensure!(
        digest == r.raw_state_sha256 && active == r.owner_effects.running_records_sha256s,
        "Raw durable state or accounted running records changed"
    );
    ensure!(
        sha(&std::fs::read(home.join("config.json"))?) == r.config_sha256,
        "Reviewed configuration changed"
    );
    ensure!(
        sha(&private_file(
            &platform::installed_binary(&platform::OBSERVATORY, home),
            128 * 1024 * 1024
        )?) == r.old_binary_sha256,
        "Reviewed original executable changed"
    );
    manager_parent(&r.manager_identity)?;
    Ok(())
}

pub async fn adopt(
    home: &Path,
    review: Option<&Path>,
    id: Option<&str>,
    recover: bool,
    inspect: bool,
) -> Result<Value> {
    private_home(home)?;
    if inspect {
        let Some(j) = optional_journal(home)? else {
            ensure!(id.is_none(), "No saved managed request exists");
            let (digest, running) = raw_state(home)?;
            let c = config::Config::load(home)?;
            return Ok(json!({
                "schema":1,"inspection_only":true,"not_a_stop_or_effects_proof":true,
                "installation_id":c.id,"home":home,"owner_uid":unsafe{libc::geteuid()},
                "config_sha256":sha(&std::fs::read(home.join("config.json"))?),
                "raw_state_sha256":digest,"running_records_sha256s":running,
                "service_lock":lock_identity(home)?,"resources_sha256":resources(home)?,
                "support_sha256":support(home)?,
                "old_binary_sha256":sha(&private_file(&platform::installed_binary(&platform::OBSERVATORY,home),128*1024*1024)?),
                "app_install_present":home.join("app-install.json").exists()
            }));
        };
        ensure!(
            super::journal(&j) && id.is_none_or(|id| j["request_id"] == id),
            "Choose the existing managed request"
        );
        return Ok(j);
    }
    let _update = lock(home, "desktop-update.lock")?;
    let mut j = if let Some(path) = review {
        let bytes = private_file(path, 256 * 1024)?;
        let r: Review = serde_json::from_slice(&bytes)?;
        if let Some(j) = optional_journal(home)? {
            if j["request_id"] == r.request_id {
                ensure!(
                    j["review_sha256"] == sha(&bytes) && super::journal(&j),
                    "Existing request review differs"
                );
                j
            } else {
                ensure!(complete(&j), "Another transition is unresolved");
                prepare(home, &r, &bytes)?
            }
        } else {
            prepare(home, &r, &bytes)?
        }
    } else {
        let j = journal(home)?;
        ensure!(
            id.is_some_and(|id| j["request_id"] == id) && super::journal(&j),
            "Supply the exact saved managed request"
        );
        j
    };
    if recover {
        recover_managed(home, &mut j).await?;
    } else {
        apply(home, &mut j).await?;
    }
    journal(home)
}
fn prepare(home: &Path, r: &Review, bytes: &[u8]) -> Result<Value> {
    ensure!(
        !home.join("app-install.json").exists(),
        "This installation already has an update baseline; use its ordinary update path"
    );
    let current = std::env::current_exe()?;
    let package = package(&current)?;
    validate_review(home, r, &package)?;
    let _service = service_guard(home, &r.service_lock)?;
    let mut j = json!({"schema":1,"kind":"managed_adoption","request_id":r.request_id,"installation_id":r.installation_id,"phase":"adoption_prepared","manager":r.manager,"manager_identity":r.manager_identity,"old_processes":r.old_processes,"config_sha256":r.config_sha256,"raw_state_sha256":r.raw_state_sha256,"service_lock":r.service_lock,"review_sha256":sha(bytes),"owner_effects_review_sha256":r.owner_effects.evidence_sha256,"copied_state_review_sha256":r.copied_state_review_sha256});
    stopped_proof(home, &j)?;
    validate_review(home, r, &package)?;
    let folder = operation(home, &r.request_id)?;
    ensure!(
        !folder.exists(),
        "Existing request artifacts are preserved; reconcile them explicitly"
    );
    config::private_dir(&folder)?;
    copy_binary(&current, &candidate_binary(&folder))?;
    copy_binary(
        &platform::installed_binary(&platform::OBSERVATORY, home),
        &folder.join("previous"),
    )?;
    ensure!(
        sha(&std::fs::read(folder.join("previous"))?) == r.old_binary_sha256,
        "Original backup differs"
    );
    j["previous"] = json!({"kind":"observed_legacy","id":r.installation_id,"version":r.old_version,"binary_sha256":r.old_binary_sha256,"package":null,"app_install_present":false});
    j["candidate"] = retained_baseline(home, &package, &r.manager)?;
    j["candidate"]["adoption"] = json!({"kind":"observed_legacy","request_id":r.request_id,"previous_binary_sha256":r.old_binary_sha256,"owner_review_sha256":sha(bytes)});
    j["resources_sha256"] = json!(resources(home)?);
    j["previous_support_sha256"] = json!(support(home)?);
    write_journal(home, &j)?;
    Ok(j)
}
pub fn prepare_update(home: &Path, j: &mut Value) -> Result<()> {
    ensure!(
        j["schema"] == 1 && j["installation_id"] == config::Config::load(home)?.id,
        "Managed update preparation identity differs"
    );
    validate_baseline(home, &j["previous"])?;
    configured_tls_is_bound(home)?;
    let m: Manager = serde_json::from_value(j["previous"]["manager"].clone())?;
    let s = installation::snapshot(&m.service)?;
    manager_definition(home, &m, &s)?;
    let process = s["processes"]
        .as_array()
        .context("Missing manager processes")?
        .iter()
        .find(|p| p["role"] == "MainPID")
        .context("The enrolled original unit is not running")?["identity"]
        .clone();
    let parent = process::parent_identity(&process)?.context("Missing original manager")?;
    manager_parent(&parent)?;
    j["kind"] = json!("managed_update");
    j["manager"] = serde_json::to_value(&m)?;
    j["manager_identity"] = parent;
    j["old_processes"] = json!([process]);
    j["service_lock"] = serde_json::to_value(lock_identity(home)?)?;
    j["config_sha256"] = json!(sha(&std::fs::read(home.join("config.json"))?));
    j["resources_sha256"] = json!(resources(home)?);
    j["previous_support_sha256"] = json!(support(home)?);
    Ok(())
}

fn dispatch_start(home: &Path, j: &mut Value, rollback: bool) -> Result<()> {
    let key = if rollback {
        "rollback_start_intent"
    } else {
        "start_intent"
    };
    ensure!(
        j.get(key).is_none(),
        "Start was already dispatched or became unknown; reconcile this operation without replay"
    );
    let snapshot = stopped_proof(home, j)?;
    let m = manager_from(j)?;
    manager_parent(&j["manager_identity"])?;
    j[key] = json!({"snapshot_sha256":sha(&serde_json::to_vec(&snapshot)?),"dispatch_count":1});
    phase(
        home,
        j,
        if rollback {
            "rollback_starting"
        } else if j["kind"] == "managed_adoption" {
            "adoption_starting"
        } else {
            "starting"
        },
    )?;
    let result =
        installation::request_state_checked(&m.service, ServiceAction::Start, &snapshot, |_| {
            fixed_inputs(home, j)?;
            cgroup_empty(&m)?;
            listener_absent(home)?;
            Ok(None)
        });
    match result {
        Ok(outcome) => {
            j[if rollback {
                "rollback_start_outcome"
            } else {
                "start_outcome"
            }] = serde_json::to_value(&outcome)?;
            write_journal(home, j)?;
            ensure!(
                outcome == ActionOutcome::Accepted,
                "Manager Start was not confirmed; reconcile the saved intent without replay"
            );
            Ok(())
        }
        Err(e) => {
            j["error"] = json!(e.to_string());
            write_journal(home, j)?;
            Err(e.context("Manager Start outcome is unknown; no redispatch"))
        }
    }
}
async fn verify_running(home: &Path, j: &Value, rollback: bool) -> Result<Value> {
    let selected = if rollback {
        &j["previous"]
    } else {
        &j["candidate"]
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(45);
    loop {
        if let Ok(proof) = running_proof(home, j, selected)
            && let Ok(status) = desktop::local_api(home, "GET", "status", None).await
        {
            let legacy = rollback && j["kind"] == "managed_adoption";
            if status["id"] == j["installation_id"]
                && status["version"] == selected["version"]
                && (legacy
                    || (status["binary_sha256"] == selected["binary_sha256"]
                        && status["pid"] == proof["process"]["pid"]))
            {
                if !legacy {
                    let claim = read(&claim_path(home, j, rollback)?)?;
                    ensure!(
                        claim["process"] == proof["process"]
                            && claim["invocation_id"] == proof["invocation_id"],
                        "Original manager generation differs from the admitted startup"
                    );
                }
                fixed_inputs(home, j)?;
                if j["kind"] == "managed_update" {
                    crate::helper_support::matches(home, selected)?;
                    let expected = if rollback {
                        &j["previous_support_sha256"]
                    } else {
                        &j["candidate_support_sha256"]
                    };
                    ensure!(
                        support(home)? == *expected,
                        "Selected support changed during verification"
                    );
                }
                return Ok(proof);
            }
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("Original-unit application verification remains unknown");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
pub fn finish(state: &AppState, j: &Value) -> Result<()> {
    validate_journal(&state.home, j)?;
    ensure!(
        j["phase"] == "applied" || (j["kind"] == "managed_update" && j["phase"] == "rolled_back"),
        "Managed settlement has no verified application"
    );
    let selected = if j["phase"] == "applied" {
        &j["candidate"]
    } else {
        &j["previous"]
    };
    let proof = running_proof(&state.home, j, selected)?;
    ensure!(
        proof["process"]["pid"] == std::process::id() && proof["process"] == j["verified_process"],
        "Only the verified manager generation may finish"
    );
    ensure!(
        read(&state.home.join("app-install.json"))? == *selected,
        "Verified baseline publication changed"
    );
    Ok(())
}

async fn stop_owned_candidate(home: &Path, j: &mut Value) -> Result<()> {
    // Revoke every candidate guard before any stop, including late startup.
    let launch = launch_guard(home)?;
    phase(home, j, "adoption_recovering")?;
    drop(launch);
    if stopped_proof(home, j).is_ok() {
        return Ok(());
    }
    ensure!(
        j.get("recovery_stop_intent").is_none(),
        "Saved Stop requires fresh owner maintenance/absence reconciliation, not replay"
    );
    let proof = running_proof(home, j, &j["candidate"])?;
    let m = manager_from(j)?;
    let expected = proof["snapshot"].clone();
    j["recovery_stop_intent"] = json!({"process":proof["process"],"invocation_id":proof["invocation_id"],"dispatch_count":1});
    write_journal(home, j)?;
    let outcome =
        installation::request_state_checked(&m.service, ServiceAction::Stop, &expected, |_| {
            ensure!(
                process::matches(&proof["process"])?,
                "Candidate generation changed"
            );
            fixed_inputs(home, j)?;
            Ok(None)
        })?;
    j["recovery_stop_outcome"] = serde_json::to_value(&outcome)?;
    write_journal(home, j)?;
    ensure!(
        outcome == ActionOutcome::Accepted,
        "Checked Stop refused or unknown; use owner maintenance, never reset-failed/raw stop"
    );
    let until = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if stopped_proof(home, j).is_ok() {
            return Ok(());
        }
        ensure!(
            std::time::Instant::now() < until,
            "Original unit absence remains unknown; owner maintenance required"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
async fn recover_managed(home: &Path, j: &mut Value) -> Result<()> {
    validate_journal(home, j)?;
    if j["phase"] == "rolled_back" {
        return if j["kind"] == "managed_update" {
            settle(home, j).await
        } else {
            Ok(())
        };
    }
    if j["phase"] == "rollback_starting" {
        let proof = verify_running(home, j, true).await?;
        j["verified_process"] = proof["process"].clone();
        phase(home, j, "rolled_back")?;
        if j["kind"] == "managed_update" {
            settle(home, j).await?;
        }
        return Ok(());
    }
    ensure!(
        j["phase"] != "applied",
        "Verified success is not automatically rolled back"
    );
    if published_candidate(home, j)? {
        // Publication follows successful candidate verification. A lost terminal
        // write only settles those same bytes/generation; it cannot restore legacy.
        return enroll(home, j).await;
    }
    stop_owned_candidate(home, j).await?;
    let expected: FileIdentity = serde_json::from_value(j["service_lock"].clone())?;
    let service = service_guard(home, &expected)?;
    stopped_proof(home, j)?;
    fixed_inputs(home, j)?;
    let folder = operation(home, j["request_id"].as_str().context("Missing request")?)?;
    let old = platform::installed_binary(&platform::OBSERVATORY, home);
    let current = sha(&std::fs::read(&old)?);
    ensure!(
        j["candidate"]["binary_sha256"] == current || j["previous"]["binary_sha256"] == current,
        "Changed executable was preserved during recovery"
    );
    ensure!(
        sha(&std::fs::read(folder.join("previous"))?) == j["previous"]["binary_sha256"],
        "Original backup changed"
    );
    copy_binary(&folder.join("previous"), &old)?;
    if j["kind"] == "managed_update" {
        restore_support(home, j, &folder)?;
        ensure!(
            support(home)? == j["previous_support_sha256"],
            "Original support was not fully restored"
        );
        save(&home.join("app-install.json"), &j["previous"])?;
    }
    j["resources_sha256"] = json!(resources(home)?);
    drop(service);
    dispatch_start(home, j, true)?;
    let proof = verify_running(home, j, true).await?;
    j["verified_process"] = proof["process"].clone();
    phase(home, j, "rolled_back")?;
    if j["kind"] == "managed_update" {
        settle(home, j).await?;
    }
    Ok(())
}

/// Baseline publication follows verification in both managed paths. Once it is
/// published, recovery is forward settlement, never implicit rollback of success.
fn published_candidate(home: &Path, j: &Value) -> Result<bool> {
    let path = home.join("app-install.json");
    if !path.exists() {
        ensure!(
            j["kind"] == "managed_adoption",
            "Enrolled baseline disappeared"
        );
        return Ok(false);
    }
    let current = read(&path)?;
    if current == j["candidate"] {
        return Ok(true);
    }
    ensure!(
        j["kind"] == "managed_update" && current == j["previous"],
        "Conflicting baseline preserved before any recovery mutation"
    );
    Ok(false)
}

fn admission_stop_can_dispatch(j: &Value) -> bool {
    j.get("admission_stop_intent").is_none()
        || j["admission_stop_intent"]["outcome"] == "busy_before_shutdown"
}
fn reserve_admission_stop(home: &Path, j: &mut Value, proof: &Value) -> Result<bool> {
    if !admission_stop_can_dispatch(j) {
        return Ok(false);
    }
    let mut attempt = 1;
    if let Some(previous) = j.get("admission_stop_intent").cloned() {
        ensure!(
            previous["proof"] == *proof,
            "Busy refusal belongs to a different previous manager generation; no new stop dispatched"
        );
        attempt = previous["attempt"]
            .as_u64()
            .context("Invalid previous admission attempt")?
            .checked_add(1)
            .context("Admission attempt overflow")?;
        if j.get("admission_stop_history").is_none() {
            j["admission_stop_history"] = json!([]);
        }
        j["admission_stop_history"]
            .as_array_mut()
            .context("Invalid admission history")?
            .push(previous);
    }
    j["admission_stop_intent"] =
        json!({"process":proof["process"],"proof":proof,"attempt":attempt,"dispatch_count":1});
    write_journal(home, j)?;
    Ok(true)
}
fn correlated_busy(status: u16, body: &Value, j: &Value, proof: &Value) -> bool {
    status == 409
        && body["accepted"] == false
        && body["code"] == "update_busy_before_shutdown"
        && body["request_id"] == j["request_id"]
        && body["id"] == j["installation_id"]
        && body["version"] == j["previous"]["version"]
        && body["binary_sha256"] == j["previous"]["binary_sha256"]
        && body["pid"] == proof["process"]["pid"]
}
fn record_admission_outcome(
    home: &Path,
    j: &mut Value,
    proof: &Value,
    result: &Result<Value>,
) -> Result<bool> {
    let outcome = match result {
        Ok(v)
            if v["accepted"] == true
                && v["request_id"] == j["request_id"]
                && v["id"] == j["installation_id"] =>
        {
            "accepted"
        }
        Err(e) => match e.downcast_ref::<desktop::LocalApiResponseError>() {
            Some(e) if correlated_busy(e.status, &e.body, j, proof) => "busy_before_shutdown",
            Some(e) if (400..500).contains(&e.status) => "refused",
            _ => "unknown",
        },
        _ => "unknown",
    };
    j["admission_stop_intent"]["outcome"] = json!(outcome);
    write_journal(home, j)?;
    Ok(outcome == "busy_before_shutdown")
}

fn switch_support(home: &Path, j: &Value, folder: &Path) -> Result<()> {
    if j["replace_support"] == true {
        let current = home.join("support/instance");
        recipe_matches(home, &j["previous"])?;
        ensure!(
            sha(&std::fs::read(current.join("consumer-bundle.json"))?)
                == j["previous"]["instance_manifest_sha256"],
            "Original agent support changed"
        );
        crate::local_install::inventory(&current)?;
        std::fs::rename(&current, folder.join("previous-instance"))?;
        std::fs::rename(folder.join("candidate-instance"), &current)?;
        set_recipe(home, &j["previous"], &j["candidate"])?;
    }
    if j["replace_helpers"] == true {
        ensure!(
            crate::helper_support::inventory(&folder.join("candidate-helpers"), true)?.0
                == j["candidate"]["helpers_manifest_sha256"],
            "Candidate helpers changed"
        );
        ensure!(
            serde_json::to_value(crate::helper_support::installed(home)?)?
                == j["previous"]["helpers_manifest_sha256"],
            "Original helpers changed"
        );
        let current = home.join("machine-helper-artifacts");
        if current.exists() {
            std::fs::rename(&current, folder.join("previous-helpers"))?;
        }
        std::fs::rename(folder.join("candidate-helpers"), current)?;
    }
    Ok(())
}
fn restore_support(home: &Path, j: &Value, folder: &Path) -> Result<()> {
    if j["replace_support"] == true {
        let current = home.join("support/instance");
        let old = folder.join("previous-instance");
        if old.exists() {
            if current.exists() {
                ensure!(
                    sha(&std::fs::read(current.join("consumer-bundle.json"))?)
                        == j["candidate"]["instance_manifest_sha256"],
                    "Changed support retained"
                );
                crate::local_install::inventory(&current)?;
                std::fs::rename(&current, folder.join("failed-instance"))?;
            }
            std::fs::rename(old, &current)?;
        }
        if recipe_matches(home, &j["previous"]).is_err() {
            set_recipe(home, &j["candidate"], &j["previous"])?;
        }
    }
    if j["replace_helpers"] == true {
        crate::helper_support::restore(
            home,
            folder,
            j["previous"]["helpers_manifest_sha256"].as_str(),
            j["candidate"]["helpers_manifest_sha256"]
                .as_str()
                .context("Missing candidate helper digest")?,
        )?;
    }
    Ok(())
}

pub async fn apply(home: &Path, j: &mut Value) -> Result<()> {
    validate_journal(home, j)?;
    if j["phase"] == "applied" {
        return settle(home, j).await;
    }
    if j["phase"] == "rolled_back" {
        return if j["kind"] == "managed_update" {
            settle(home, j).await
        } else {
            Ok(())
        };
    }
    if j["phase"] == "adoption_recovering" || j["phase"] == "rollback_starting" {
        return recover_managed(home, j).await;
    }
    if matches!(
        j["phase"].as_str(),
        Some("adoption_starting" | "starting" | "adoption_verified" | "adoption_enrolling")
    ) {
        return enroll(home, j).await;
    }
    ensure!(
        matches!(
            j["phase"].as_str(),
            Some("adoption_prepared" | "prepared" | "deferred")
        ),
        "Interrupted replacement requires explicit same-request recovery"
    );
    let expected: FileIdentity = serde_json::from_value(j["service_lock"].clone())?;
    if j["kind"] == "managed_update" && admission_stop_can_dispatch(j) {
        fixed_inputs(home, j)?;
        ensure!(
            read(&home.join("app-install.json"))? == j["previous"]
                && support(home)? == j["previous_support_sha256"],
            "Previous package or support changed before admission"
        );
        let proof = running_proof(home, j, &j["previous"])?;
        // Existing authenticated admission closure shuts down the owned server.
        // Its original systemd unit remains the only subsequent launcher.
        if reserve_admission_stop(home, j, &proof)? {
            let result = desktop::local_api(
                home,
                "POST",
                "native-update/stop",
                Some(json!({"request_id":j["request_id"],"version":j["previous"]["version"]})),
            )
            .await;
            if record_admission_outcome(home, j, &proof, &result)? {
                anyhow::bail!(
                    "Original application refused shutdown while busy; current work was preserved. A later explicit apply may retry only this exact unchanged generation"
                );
            }
        }
    }
    // An unobserved response can already have stopped this exact operation.
    // The fresh stopped proof below reconciles it without a second POST.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let service = loop {
        if let Ok(guard) = service_guard(home, &expected)
            && stopped_proof(home, j).is_ok()
        {
            break guard;
        }
        ensure!(
            std::time::Instant::now() < deadline,
            "Original unit is not proven inactive; no replacement occurred"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    fixed_inputs(home, j)?;
    if j["kind"] == "managed_adoption" {
        ensure!(
            raw_state(home)?.0 == j["raw_state_sha256"],
            "Reviewed raw state changed before replacement"
        );
    }
    let folder = operation(home, j["request_id"].as_str().context("Missing request")?)?;
    ensure!(
        sha(&std::fs::read(candidate_binary(&folder))?) == j["candidate"]["binary_sha256"]
            && sha(&std::fs::read(folder.join("previous"))?) == j["previous"]["binary_sha256"],
        "Staged or previous executable changed"
    );
    ensure!(
        sha(&std::fs::read(platform::installed_binary(
            &platform::OBSERVATORY,
            home
        ))?) == j["previous"]["binary_sha256"],
        "Installed executable changed"
    );
    phase(home, j, "adoption_switching")?;
    switch_support(home, j, &folder)?;
    copy_binary(
        &candidate_binary(&folder),
        &platform::installed_binary(&platform::OBSERVATORY, home),
    )?;
    j["candidate_support_sha256"] = json!(support(home)?);
    write_journal(home, j)?;
    drop(service);
    dispatch_start(home, j, false)?;
    enroll(home, j).await
}
async fn enroll(home: &Path, j: &mut Value) -> Result<()> {
    let proof = verify_running(home, j, false).await?;
    j["verified_process"] = proof["process"].clone();
    j["verified_invocation_id"] = proof["invocation_id"].clone();
    phase(home, j, "adoption_verified")?;
    phase(home, j, "adoption_enrolling")?;
    if j["kind"] == "managed_adoption" {
        create_or_equal(&home.join("app-install.json"), &j["candidate"])?;
    } else {
        let existing = read(&home.join("app-install.json"))?;
        ensure!(
            existing == j["previous"] || existing == j["candidate"],
            "Installed baseline changed"
        );
        save(&home.join("app-install.json"), &j["candidate"])?;
    }
    phase(home, j, "applied")?;
    settle(home, j).await
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture_config(home: &Path) -> config::Config {
        let c = config::Config {
            id: uuid::Uuid::new_v4().to_string(),
            owner: "fixture".into(),
            bind: "127.0.0.1".into(),
            port: 1,
            public_url: "http://127.0.0.1:1".into(),
            auth_mode: config::AuthMode::LocalPassword,
            web_transport: config::WebTransport::LocalHttp,
            pam_service: String::new(),
            tls_cert: PathBuf::new(),
            tls_key: PathBuf::new(),
            tls_ca: None,
            embed_origins: vec![],
        };
        c.save(home).unwrap();
        c
    }
    /// Every unit text the source installer has written, current first.
    const INSTALLER_UNIT_TEXTS: [fn(&Path) -> String; 2] = [unit_text, earlier_unit_text];
    fn original_unit_fixture(home: &Path, text: fn(&Path) -> String) -> (Manager, Value) {
        unit_fixture(home, &text(home))
    }
    /// A unit file with these bytes, and a manager binding and snapshot that
    /// agree with it, so only the accepted-text check can tell texts apart.
    fn unit_fixture(home: &Path, bytes: &str) -> (Manager, Value) {
        let c = fixture_config(home);
        let name = format!("agent-observatory-{}.service", c.id);
        let path = home.join(&name);
        std::fs::write(&path, bytes).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let effective = format!("# {}\n{bytes}", path.display());
        let owner_uid = unsafe { libc::geteuid() };
        let manager = Manager {
            service: Service {
                manager: ServiceManager::SystemdUser,
                name: name.clone(),
                definition_path: Some(path.clone()),
            },
            owner_uid,
            definition_sha256: sha(bytes.as_bytes()),
            effective_definition_sha256: sha(effective.as_bytes()),
            control_group: format!(
                "/user.slice/user-{owner_uid}.slice/user@{owner_uid}.service/app.slice/{name}"
            ),
        };
        // The retained real systemd snapshot omitted ExecStop despite requesting
        // it with --all. Keep that structured-array shape, with disposable paths.
        let snapshot = json!({
            "definition":{"path":path,"sha256":manager.definition_sha256},
            "effective_definition":{"text":effective,"sha256":manager.effective_definition_sha256},
            "definition_matches":true,"loaded":true,"masked":false,
            "native":{"DropInPaths":"","NeedDaemonReload":"no","KillMode":"control-group","Type":"simple","Restart":"on-failure"}
        });
        (manager, snapshot)
    }
    fn no_adoption_writes(home: &Path) {
        for name in [
            "desktop-update.json",
            "app-install.json",
            "app-updates",
            "state.sqlite3",
        ] {
            assert!(!home.join(name).exists(), "Unexpected write: {name}");
        }
    }
    #[test]
    fn original_unit_accepts_omitted_or_empty_execstop() {
        for text in INSTALLER_UNIT_TEXTS {
            let temp = tempfile::tempdir().unwrap();
            let (manager, mut snapshot) = original_unit_fixture(temp.path(), text);
            assert!(snapshot["native"].get("ExecStop").is_none());
            manager_definition(temp.path(), &manager, &snapshot).unwrap();
            snapshot["native"]["ExecStop"] = json!("");
            manager_definition(temp.path(), &manager, &snapshot).unwrap();
            no_adoption_writes(temp.path());
        }
    }
    #[test]
    fn original_unit_refuses_nonempty_or_malformed_execstop() {
        for text in INSTALLER_UNIT_TEXTS {
            let temp = tempfile::tempdir().unwrap();
            let (manager, mut snapshot) = original_unit_fixture(temp.path(), text);
            for value in [
                json!("{ path=/bin/true ; argv[]=/bin/true }"),
                json!(" "),
                Value::Null,
                json!(false),
                json!(0),
                json!([]),
                json!({}),
            ] {
                snapshot["native"]["ExecStop"] = value;
                assert!(manager_definition(temp.path(), &manager, &snapshot).is_err());
            }
            no_adoption_writes(temp.path());
        }
    }
    #[test]
    fn original_unit_omitted_execstop_does_not_relax_peer_guards() {
        for text in INSTALLER_UNIT_TEXTS {
            let temp = tempfile::tempdir().unwrap();
            let (manager, snapshot) = original_unit_fixture(temp.path(), text);
            for (key, value) in [
                ("DropInPaths", json!("/tmp/override.conf")),
                ("DropInPaths", Value::Null),
                ("KillMode", json!("process")),
                ("Type", json!("forking")),
                ("Restart", json!("no")),
                ("Restart", json!("always")),
            ] {
                let mut changed = snapshot.clone();
                changed["native"][key] = value;
                assert!(manager_definition(temp.path(), &manager, &changed).is_err());
            }
            for key in ["DropInPaths", "KillMode", "Type", "Restart"] {
                let mut changed = snapshot.clone();
                changed["native"].as_object_mut().unwrap().remove(key);
                assert!(manager_definition(temp.path(), &manager, &changed).is_err());
            }
            for (key, value) in [
                ("definition_matches", false),
                ("loaded", false),
                ("masked", true),
            ] {
                let mut changed = snapshot.clone();
                changed[key] = json!(value);
                assert!(manager_definition(temp.path(), &manager, &changed).is_err());
            }
            no_adoption_writes(temp.path());
        }
    }
    #[test]
    fn original_unit_omitted_execstop_still_requires_exact_definition() {
        for text in INSTALLER_UNIT_TEXTS {
            let temp = tempfile::tempdir().unwrap();
            let (manager, snapshot) = original_unit_fixture(temp.path(), text);
            for key in ["definition", "effective_definition"] {
                let mut changed = snapshot.clone();
                changed[key]["sha256"] = json!("0".repeat(64));
                assert!(manager_definition(temp.path(), &manager, &changed).is_err());
            }
            let path = manager.service.definition_path.as_ref().unwrap();
            let changed =
                text(temp.path()).replace("Type=simple\n", "Type=simple\nExecStop=/bin/true\n");
            std::fs::write(path, &changed).unwrap();
            let mut rebound = manager.clone();
            rebound.definition_sha256 = sha(changed.as_bytes());
            let effective = format!("# {}\n{changed}", path.display());
            rebound.effective_definition_sha256 = sha(effective.as_bytes());
            let mut rebound_snapshot = snapshot;
            rebound_snapshot["definition"]["sha256"] = json!(rebound.definition_sha256);
            rebound_snapshot["effective_definition"] =
                json!({"text":effective,"sha256":rebound.effective_definition_sha256});
            assert!(manager_definition(temp.path(), &rebound, &rebound_snapshot).is_err());
            no_adoption_writes(temp.path());
        }
    }
    #[test]
    fn earlier_installer_unit_text_is_frozen_one_line_from_the_current_text() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        // Installations made through 0.12.2 carry exactly this text. It is written
        // out here so that a later edit to either function cannot move it.
        let written_through_0_12_2 = format!(
            "[Unit]\nDescription=Agent Observatory\nAfter=network-online.target\n\n[Service]\nType=simple\nExecStart={} --home {} serve\nRestart=on-failure\nRestartSec=5\nUMask=0077\nKillMode=control-group\n\n[Install]\nWantedBy=default.target\n",
            quote(&home.join("bin/agent-observatory")),
            quote(home)
        );
        assert_eq!(earlier_unit_text(home), written_through_0_12_2);
        assert_eq!(
            unit_text(home),
            written_through_0_12_2.replace(
                "Restart=on-failure\n",
                "Restart=on-failure\nRestartForceExitStatus=SIGTERM\n"
            )
        );
        assert_ne!(unit_text(home), earlier_unit_text(home));
    }
    #[test]
    fn any_other_unit_text_is_refused_even_with_matching_hashes() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let current = unit_text(home);
        let earlier = earlier_unit_text(home);
        // The fixture binds whatever bytes it is given, so both accepted texts pass.
        for accepted in [&current, &earlier] {
            let (manager, snapshot) = unit_fixture(home, accepted);
            manager_definition(home, &manager, &snapshot).unwrap();
        }
        let forced = "RestartForceExitStatus=SIGTERM\n";
        let changed = [
            // A different forced-restart rule, or the same rule anywhere else.
            current.replace(forced, "RestartForceExitStatus=SIGTERM SIGKILL\n"),
            current.replace(forced, "RestartForceExitStatus=SIGTERM 143\n"),
            current.replace(forced, "RestartForceExitStatus=143\n"),
            current.replace(forced, "RestartForceExitStatus=TERM\n"),
            current.replace(forced, "RestartForceExitStatus=0\n"),
            current.replace(forced, "RestartForceExitStatus=\n"),
            current.replace(forced, &forced.repeat(2)),
            earlier.replace("RestartSec=5\n", &format!("RestartSec=5\n{forced}")),
            earlier.replace("Type=simple\n", &format!("Type=simple\n{forced}")),
            format!("{earlier}{forced}"),
            // Another restart rule, with or without the accepted line.
            current.replace("Restart=on-failure\n", "Restart=always\n"),
            earlier.replace("Restart=on-failure\n", "Restart=always\n"),
            current.replace("Restart=on-failure\n", "Restart=on-abnormal\n"),
            earlier.replace(
                "Restart=on-failure\n",
                "Restart=on-failure\nRestartPreventExitStatus=SIGTERM\n",
            ),
            earlier.replace(
                "Restart=on-failure\n",
                "Restart=on-failure\nSuccessExitStatus=SIGTERM\n",
            ),
            current.replace("RestartSec=5\n", "RestartSec=1\n"),
            current.replace("RestartSec=5\n", ""),
            // Anything else, however small.
            current.replace("KillMode=control-group\n", "KillMode=process\n"),
            current.replace(" serve\n", " serve --verbose\n"),
            current.replace("UMask=0077\n", "UMask=0077\nEnvironment=A=B\n"),
            current.replace(forced, "RestartForceExitStatus=SIGTERM \n"),
            current.replace(forced, "RestartForceExitStatus=SIGTERM\r\n"),
            current.replace(forced, "# RestartForceExitStatus=SIGTERM\n"),
            format!("{current}\n"),
            format!("{earlier}\n"),
            current.trim_end().to_owned(),
            earlier.trim_end().to_owned(),
            String::new(),
        ];
        for text in &changed {
            assert!(text != &current && text != &earlier, "{text:?}");
            let (manager, snapshot) = unit_fixture(home, text);
            let error = manager_definition(home, &manager, &snapshot).unwrap_err();
            assert!(
                error.to_string().contains("unchanged original"),
                "{text:?}: {error}"
            );
        }
        no_adoption_writes(home);
    }
    /// The installer and this module must agree byte for byte, or a fresh source
    /// installation could not be adopted later.
    #[test]
    fn source_installer_writes_the_current_unit_text() {
        let script = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/install.py"));
        let lines = script
            .lines()
            .find_map(|line| {
                line.trim_start()
                    .strip_prefix("unit_text='\\n'.join([")?
                    .strip_suffix("])")
            })
            .expect("scripts/install.py no longer joins one list into unit_text");
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let rendered = lines
            .split(',')
            .map(|line| {
                let line = line
                    .strip_prefix("f'")
                    .or_else(|| line.strip_prefix('\''))
                    .and_then(|line| line.strip_suffix('\''))
                    .expect("Unexpected installer unit line");
                if line == "ExecStart={quote(binary)} --home {quote(home)} serve" {
                    return format!(
                        "ExecStart={} --home {} serve",
                        quote(&platform::installed_binary(&platform::OBSERVATORY, home)),
                        quote(home)
                    );
                }
                assert!(
                    !line.contains(['{', '}', '\\', '\'']),
                    "Unexpected installer unit line: {line}"
                );
                line.to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(rendered, unit_text(home));
        assert!(rendered.contains("\nRestart=on-failure\nRestartForceExitStatus=SIGTERM\n"));
    }
    #[test]
    fn owner_review_rejects_unrecognized_fields_and_invalid_hashes() {
        assert!(!hex("A".repeat(64).as_str()));
        assert!(hex(&"a".repeat(64)));
        assert!(serde_json::from_value::<EffectsReview>(json!({"running_records_sha256s":[],"external_effects_accounted":true,"historical_unknowns_preserved":true,"no_replay":true,"evidence_sha256":"a".repeat(64),"idle":true})).is_err());
    }
    #[test]
    fn raw_snapshot_keeps_unknowns_and_binds_all_rows() {
        let temp = tempfile::tempdir().unwrap();
        let db = rusqlite::Connection::open(temp.path().join("state.sqlite3")).unwrap();
        db.execute_batch(
            "CREATE TABLE records(kind TEXT,id TEXT,value TEXT); CREATE TABLE extra(value BLOB);",
        )
        .unwrap();
        db.execute("INSERT INTO records VALUES('sessions','kept',?1)",[json!({"status":"interrupted","messages":[{"delivery":"unknown","request_id":"historical","acknowledged":false}]}).to_string()]).unwrap();
        let (before, active) = raw_state(temp.path()).unwrap();
        assert!(active.is_empty());
        db.execute("INSERT INTO extra VALUES(X'0102')", []).unwrap();
        assert_ne!(raw_state(temp.path()).unwrap().0, before);
        let text: String = db
            .query_row("SELECT value FROM records", [], |r| r.get(0))
            .unwrap();
        assert!(text.contains("unknown"));
        assert!(text.contains("false"));
    }
    #[test]
    fn service_lock_is_same_inode_and_never_repaired() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("service.lock");
        std::fs::write(&path, b"").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let id = lock_identity(temp.path()).unwrap();
        let held = service_guard(temp.path(), &id).unwrap();
        assert!(service_guard(temp.path(), &id).is_err());
        drop(held);
        std::fs::rename(&path, temp.path().join("original")).unwrap();
        std::fs::write(&path, b"").unwrap();
        assert!(service_guard(temp.path(), &id).is_err());
    }
    #[test]
    fn create_only_publication_reconciles_exact_bytes_without_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("receipt");
        let a = json!({"schema":1,"value":"verified"});
        create_or_equal(&path, &a).unwrap();
        let before = identity(&std::fs::metadata(&path).unwrap());
        create_or_equal(&path, &a).unwrap();
        assert_eq!(before, identity(&std::fs::metadata(&path).unwrap()));
        assert!(create_or_equal(&path, &json!({"schema":1,"value":"different"})).is_err());
        assert_eq!(read(&path).unwrap(), a);
    }

    #[tokio::test]
    async fn launch_hold_prevents_revocation_during_initialization() {
        let temp = tempfile::tempdir().unwrap();
        let mut j = json!({"schema":1,"kind":"managed_adoption","phase":"adoption_starting"});
        write_journal(temp.path(), &j).unwrap();
        let bytes = std::fs::read(temp.path().join("desktop-update.json")).unwrap();
        let guard = launch_guard(temp.path()).unwrap();
        // Failure is before any manager observation/action or journal revocation.
        assert!(stop_owned_candidate(temp.path(), &mut j).await.is_err());
        assert_eq!(
            std::fs::read(temp.path().join("desktop-update.json")).unwrap(),
            bytes
        );
        assert_eq!(j["phase"], "adoption_starting");
        drop(guard);
        assert!(launch_guard(temp.path()).is_ok());
    }

    #[tokio::test]
    async fn conflicting_enrollment_refuses_before_executable_or_journal_change() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        std::fs::create_dir(home.join("bin")).unwrap();
        let binary = platform::installed_binary(&platform::OBSERVATORY, home);
        std::fs::write(&binary, b"verified candidate remains").unwrap();
        let c = fixture_config(home);
        let mut j = json!({"schema":1,"installation_id":c.id,"kind":"managed_adoption","phase":"adoption_enrolling","candidate":{"binary_sha256":"candidate"}});
        write_journal(home, &j).unwrap();
        save(
            &home.join("app-install.json"),
            &json!({"binary_sha256":"foreign"}),
        )
        .unwrap();
        let before = std::fs::read(home.join("desktop-update.json")).unwrap();
        assert!(recover_managed(home, &mut j).await.is_err());
        assert_eq!(
            std::fs::read(binary).unwrap(),
            b"verified candidate remains"
        );
        assert_eq!(
            std::fs::read(home.join("desktop-update.json")).unwrap(),
            before
        );
        assert_eq!(j["phase"], "adoption_enrolling");
    }

    #[test]
    fn immutable_resources_are_separate_from_owned_support_switch() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        std::fs::write(home.join("config.json"), b"same config").unwrap();
        std::fs::create_dir(home.join("support")).unwrap();
        std::fs::write(home.join("support/file"), b"previous").unwrap();
        let fixed = resources(home).unwrap();
        let old = support(home).unwrap();
        std::fs::write(home.join("support/file"), b"candidate").unwrap();
        assert_eq!(resources(home).unwrap(), fixed);
        assert_ne!(support(home).unwrap(), old);
        std::fs::write(home.join("config.json"), b"changed config").unwrap();
        assert_ne!(resources(home).unwrap(), fixed);
    }

    #[test]
    fn partial_helper_move_restores_exact_previous_tree_and_owner_note() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let folder = home.join("operation");
        std::fs::create_dir(&folder).unwrap();
        let previous = folder.join("previous-helpers");
        std::fs::create_dir(&previous).unwrap();
        let mut artifacts = Vec::new();
        for platform in [
            "linux-x86_64",
            "linux-aarch64",
            "macos-x86_64",
            "macos-aarch64",
            "windows-x86_64",
            "windows-aarch64",
        ] {
            let name = format!("{platform}.zip");
            let bytes = platform.as_bytes();
            std::fs::write(previous.join(&name), bytes).unwrap();
            artifacts.push(json!({"platform":platform,"filename":name,"sha256":sha(bytes)}));
        }
        std::fs::write(
            previous.join("manifest.json"),
            json!({"schema":1,"artifacts":artifacts}).to_string(),
        )
        .unwrap();
        std::fs::write(previous.join("owner-note"), b"preserve extra file").unwrap();
        let old = crate::helper_support::inventory(&previous, false)
            .unwrap()
            .0;
        let j = json!({"replace_helpers":true,"previous":{"helpers_manifest_sha256":old},"candidate":{"helpers_manifest_sha256":"a".repeat(64)}});
        // Crash after moving the old tree, before moving the candidate tree.
        restore_support(home, &j, &folder).unwrap();
        assert_eq!(crate::helper_support::installed(home).unwrap(), Some(old));
        assert_eq!(
            std::fs::read(home.join("machine-helper-artifacts/owner-note")).unwrap(),
            b"preserve extra file"
        );
        assert!(!previous.exists());
    }

    #[test]
    fn unknown_journal_kind_is_held_and_refused_before_start() {
        let temp = tempfile::tempdir().unwrap();
        for phase in ["prepared", "applied"] {
            write_journal(temp.path(), &json!({"kind":"future-unknown","phase":phase})).unwrap();
            assert!(pending_verification(temp.path()));
            assert!(direct_start_allowed(temp.path()).is_err());
        }
    }

    #[test]
    fn published_candidate_in_either_managed_path_selects_only_forward_settlement() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        for kind in ["managed_adoption", "managed_update"] {
            let j = json!({"kind":kind,"phase":"adoption_enrolling","previous":{"binary_sha256":"old"},"candidate":{"binary_sha256":"new"}});
            save(&home.join("app-install.json"), &j["candidate"]).unwrap();
            let bytes = std::fs::read(home.join("app-install.json")).unwrap();
            assert!(published_candidate(home, &j).unwrap());
            assert_eq!(std::fs::read(home.join("app-install.json")).unwrap(), bytes);
            save(&home.join("app-install.json"), &j["previous"]).unwrap();
            if kind == "managed_update" {
                assert!(!published_candidate(home, &j).unwrap());
            } else {
                assert!(published_candidate(home, &j).is_err());
            }
        }
    }

    #[test]
    fn lost_admission_reply_retains_one_intent_without_second_dispatch() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let mut j = json!({"kind":"managed_update","phase":"prepared"});
        let proof = json!({"process":{"pid":123,"start_ticks":"4"}});
        assert!(reserve_admission_stop(home, &mut j, &proof).unwrap());
        // Reload after a crash/lost reply, before observing whether Stop settled.
        let mut reloaded = journal(home).unwrap();
        assert!(
            !reserve_admission_stop(home, &mut reloaded, &json!({"process":"replacement"}))
                .unwrap()
        );
        assert_eq!(reloaded, j);
        assert_eq!(reloaded["admission_stop_intent"]["dispatch_count"], 1);
        assert!(reloaded.get("start_intent").is_none());
    }

    #[test]
    fn unknown_start_intent_is_not_redispatched() {
        let temp = tempfile::tempdir().unwrap();
        let mut j = json!({"kind":"managed_adoption","phase":"adoption_starting","start_intent":{"dispatch_count":1}});
        let err = dispatch_start(temp.path(), &mut j, false).unwrap_err();
        assert!(err.to_string().contains("already dispatched"));
        assert_eq!(j["start_intent"]["dispatch_count"], 1);
    }
    #[test]
    fn manager_probe_setup_error_reaps_only_owned_child() {
        let child = Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let p = process::identity(child.id()).unwrap().unwrap();
        // Missing pipe fails before fcntl/read; the same guard covers all later errors.
        assert!(manager_output(child).is_err());
        assert!(!process::matches(&p).unwrap());
    }

    #[test]
    fn definite_busy_keeps_history_and_only_same_generation_can_explicitly_retry() {
        let temp = tempfile::tempdir().unwrap();
        let h = temp.path();
        let mut j = json!({"kind":"managed_update","phase":"prepared","request_id":"same-request","installation_id":"same-home","previous":{"version":"0.12.1","binary_sha256":"previous"}});
        let proof =
            json!({"process":{"pid":123,"start_ticks":"4"},"snapshot":{"invocation":"same"}});
        assert!(reserve_admission_stop(h, &mut j, &proof).unwrap());
        let body = json!({"accepted":false,"code":"update_busy_before_shutdown","request_id":"same-request","id":"same-home","version":"0.12.1","binary_sha256":"previous","pid":123,"error":"Busy"});
        let response = desktop::local_api_response(axum::http::StatusCode::CONFLICT, body.clone());
        assert!(record_admission_outcome(h, &mut j, &proof, &response).unwrap());
        let first = j["admission_stop_intent"].clone();
        let bytes = std::fs::read(h.join("desktop-update.json")).unwrap();
        assert!(
            reserve_admission_stop(
                h,
                &mut j,
                &json!({"process":{"pid":123,"start_ticks":"replacement"}})
            )
            .is_err()
        );
        assert_eq!(std::fs::read(h.join("desktop-update.json")).unwrap(), bytes);
        assert!(reserve_admission_stop(h, &mut j, &proof).unwrap());
        assert_eq!(j["admission_stop_history"], json!([first]));
        assert_eq!(j["admission_stop_intent"]["attempt"], 2);
        assert!(!reserve_admission_stop(h, &mut j, &proof).unwrap());
        for key in [
            "request_id",
            "id",
            "version",
            "binary_sha256",
            "pid",
            "code",
        ] {
            let mut altered = body.clone();
            altered[key] = json!("changed");
            assert!(!correlated_busy(409, &altered, &j, &proof));
        }
        assert!(!correlated_busy(500, &body, &j, &proof));
        assert!(
            !record_admission_outcome(h, &mut j, &proof, &Err(anyhow::anyhow!("lost reply")))
                .unwrap()
        );
        assert_eq!(j["admission_stop_intent"]["outcome"], "unknown");
        assert!(!reserve_admission_stop(h, &mut j, &proof).unwrap());
    }

    #[test]
    fn busy_stop_response_is_correlated_before_any_admission_or_shutdown_mutation() {
        use crate::config::{AuthMode, Config, WebTransport};
        let temp = tempfile::tempdir().unwrap();
        let h = temp.path();
        Config {
            id: uuid::Uuid::new_v4().to_string(),
            owner: "fixture".into(),
            bind: "127.0.0.1".into(),
            port: 1,
            public_url: "http://127.0.0.1:1".into(),
            auth_mode: AuthMode::LocalPassword,
            web_transport: WebTransport::LocalHttp,
            pam_service: String::new(),
            tls_cert: PathBuf::new(),
            tls_key: PathBuf::new(),
            tls_ca: None,
            embed_origins: vec![],
        }
        .save(h)
        .unwrap();
        let state = crate::state::AppState::open(h.into()).unwrap();
        let j = json!({"schema":1,"kind":"managed_update","phase":"prepared","request_id":uuid::Uuid::new_v4().to_string(),"installation_id":state.config.id,"previous":{"version":env!("CARGO_PKG_VERSION"),"binary_sha256":desktop::binary_digest().unwrap()}});
        write_journal(h, &j).unwrap();
        let original = std::fs::read(h.join("desktop-update.json")).unwrap();
        let body = json!({"request_id":j["request_id"],"version":env!("CARGO_PKG_VERSION")});
        state
            .active_sessions
            .lock()
            .unwrap()
            .insert("existing".into());
        let refused = stop_response(&state, &body).unwrap();
        assert!(correlated_busy(
            409,
            &refused,
            &j,
            &json!({"process":{"pid":std::process::id()}})
        ));
        state.active_sessions.lock().unwrap().clear();
        let held = state.operation_gate.clone().try_acquire_owned().unwrap();
        assert_eq!(stop_response(&state, &body).unwrap(), refused);
        assert!(!state.operation_gate.is_closed());
        assert!(!*state.shutdown.borrow());
        assert_eq!(
            std::fs::read(h.join("desktop-update.json")).unwrap(),
            original
        );
        drop(held);
        assert_eq!(stop_response(&state, &body).unwrap()["accepted"], true);
        assert!(*state.shutdown.borrow());
        assert!(state.operation_gate.is_closed());
    }
    #[tokio::test]
    async fn managed_rollback_terminal_reconciles_finish_without_restarting_or_rewriting() {
        let temp = tempfile::tempdir().unwrap();
        let h = temp.path();
        // Terminal publication can precede delivery of finish. An unavailable
        // endpoint must remain an error on both re-entry paths, never false success.
        let c = fixture_config(h);
        let mut j = json!({"schema":1,"installation_id":c.id,"kind":"managed_update","phase":"rolled_back","request_id":uuid::Uuid::new_v4().to_string(),"rollback_start_intent":{"dispatch_count":1},"recovery_stop_intent":{"dispatch_count":1},"verified_process":{"pid":123,"start_ticks":"4"}});
        write_journal(h, &j).unwrap();
        let before = std::fs::read(h.join("desktop-update.json")).unwrap();
        assert!(apply(h, &mut j).await.is_err());
        assert!(recover_managed(h, &mut j).await.is_err());
        assert_eq!(
            std::fs::read(h.join("desktop-update.json")).unwrap(),
            before
        );
        assert_eq!(j["rollback_start_intent"]["dispatch_count"], 1);
        assert_eq!(j["recovery_stop_intent"]["dispatch_count"], 1);
        j["kind"] = json!("managed_adoption");
        // Legacy plain serve has no update hold/finish API, so its verified rollback is terminal.
        assert!(recover_managed(h, &mut j).await.is_ok());
    }
    #[test]
    fn startup_rechecks_terminal_prepared_and_absent_snapshots_under_launch_hold() {
        let temp = tempfile::tempdir().unwrap();
        let h = temp.path();
        for old_phase in ["applied", "rolled_back", "prepared", "deferred"] {
            let stale = json!({"kind":"managed_update","phase":old_phase});
            let revoked = json!({"kind":"managed_update","phase":"adoption_recovering"});
            write_journal(h, &revoked).unwrap();
            let before = std::fs::read(h.join("desktop-update.json")).unwrap();
            let error = guard(h, Some(&stale)).unwrap_err();
            assert!(error.to_string().contains("revoked before"));
            assert_eq!(
                std::fs::read(h.join("desktop-update.json")).unwrap(),
                before
            );
        }
        // A baseline-only call cannot ignore a transition created after its first read.
        let error = guard(h, None).unwrap_err();
        assert!(
            error.to_string().contains("new managed transition"),
            "{error:#}"
        );
    }
    #[test]
    fn configured_tls_is_confined_to_the_hashed_retained_tree_without_repair() {
        let temp = tempfile::tempdir().unwrap();
        let h = temp.path().canonicalize().unwrap();
        std::fs::create_dir(h.join("tls")).unwrap();
        let inside = h.join("tls/cert.pem");
        let outside = h.join("external.pem");
        std::fs::write(&inside, b"retained").unwrap();
        std::fs::write(&outside, b"external").unwrap();
        assert!(retained_tls_path(&h, &inside).is_ok());
        assert!(retained_tls_path(&h, &outside).is_err());
        std::os::unix::fs::symlink(&outside, h.join("tls/linked.pem")).unwrap();
        assert!(retained_tls_path(&h, &h.join("tls/linked.pem")).is_err());
        assert_eq!(std::fs::read(outside).unwrap(), b"external");
        assert_eq!(std::fs::read(inside).unwrap(), b"retained");
    }
    #[tokio::test]
    async fn future_schema_and_wrong_installation_refuse_before_journal_binary_or_state_action() {
        let temp = tempfile::tempdir().unwrap();
        let h = temp.path();
        let c = fixture_config(h);
        std::fs::create_dir(h.join("bin")).unwrap();
        let bin = platform::installed_binary(&platform::OBSERVATORY, h);
        std::fs::write(&bin, b"unchanged executable").unwrap();
        for (schema, id) in [(2, c.id.clone()), (1, "wrong-installation".into())] {
            for phase in ["prepared", "applied", "rolled_back"] {
                let mut j = json!({"schema":schema,"installation_id":id,"kind":"managed_update","phase":phase});
                write_journal(h, &j).unwrap();
                let before = std::fs::read(h.join("desktop-update.json")).unwrap();
                assert!(apply(h, &mut j).await.is_err());
                assert!(recover_managed(h, &mut j).await.is_err());
                assert!(guard(h, Some(&j)).is_err());
                assert_eq!(
                    std::fs::read(h.join("desktop-update.json")).unwrap(),
                    before
                );
                assert_eq!(std::fs::read(&bin).unwrap(), b"unchanged executable");
                assert!(!h.join("state.sqlite3").exists());
                if schema == 2 {
                    assert!(pending_verification(h));
                }
            }
            std::fs::remove_file(h.join("desktop-update.json")).unwrap();
            save(
                &h.join("app-install.json"),
                &json!({"schema":schema,"id":id,"manager":{}}),
            )
            .unwrap();
            assert!(
                guard(h, None)
                    .unwrap_err()
                    .to_string()
                    .contains("baseline schema or installation")
            );
        }
    }
}
