//! Read-only identity proof for the current Linux user manager. A protected
//! session-bus connection's credentials remain readable when /proc/PID/exe is not.
use super::{FileIdentity, identity, manager_output_until};
use amplifier_native_platform::process;
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    ffi::OsStr,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Debug, PartialEq, Eq)]
struct RouteIdentity {
    runtime: FileIdentity,
    socket: FileIdentity,
}
fn route_identity(runtime: &Path, uid: u32) -> Result<RouteIdentity> {
    let directory = std::fs::symlink_metadata(runtime)?;
    ensure!(
        runtime.is_absolute()
            && runtime.canonicalize()? == runtime
            && directory.is_dir()
            && !directory.file_type().is_symlink()
            && directory.uid() == uid
            && directory.mode() & 0o077 == 0,
        "The canonical private user runtime directory is unavailable"
    );
    let socket = std::fs::symlink_metadata(runtime.join("bus"))?;
    // Socket mode can legitimately be broader inside the private runtime directory.
    ensure!(
        socket.file_type().is_socket() && socket.uid() == uid,
        "The canonical current-user bus socket is unavailable"
    );
    Ok(RouteIdentity {
        runtime: identity(&directory),
        socket: identity(&socket),
    })
}
fn route_environment(
    runtime: &Path,
    address: &str,
    xdg: Option<&OsStr>,
    dbus: Option<&OsStr>,
) -> Result<()> {
    ensure!(
        xdg.is_none_or(|v| v == runtime.as_os_str())
            && dbus.is_none_or(|v| v == OsStr::new(address)),
        "Conflicting user-manager bus environment is unsupported"
    );
    Ok(())
}
fn unique_name(value: &str) -> bool {
    let Some(rest) = value.strip_prefix(':') else {
        return false;
    };
    value.len() <= 255
        && rest.contains('.')
        && rest.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
        })
}
fn scalar_line(bytes: &[u8]) -> Result<&str> {
    ensure!(bytes.len() <= 1024, "Excessive user-bus response");
    let text = std::str::from_utf8(bytes)?;
    let text = text.strip_suffix('\n').unwrap_or(text);
    ensure!(
        !text.contains(['\r', '\n']),
        "Multiple user-bus response lines"
    );
    Ok(text)
}
fn string_scalar(bytes: &[u8]) -> Result<&str> {
    scalar_line(bytes)?
        .strip_prefix("s \"")
        .and_then(|s| s.strip_suffix('"'))
        .context("Expected one typed user-bus string")
}
fn name_scalar(bytes: &[u8]) -> Result<String> {
    let value = string_scalar(bytes)?;
    ensure!(unique_name(value), "Invalid unique user-bus owner");
    Ok(value.to_owned())
}
fn id_scalar(bytes: &[u8]) -> Result<String> {
    let value = string_scalar(bytes)?;
    ensure!(
        value.len() == 32
            && value
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        "Invalid user-bus identity"
    );
    Ok(value.to_owned())
}
fn unsigned_scalar(bytes: &[u8]) -> Result<u32> {
    let digits = scalar_line(bytes)?
        .strip_prefix("u ")
        .context("Expected one typed user-bus unsigned integer")?;
    ensure!(
        !digits.is_empty() && digits.len() <= 10 && digits.bytes().all(|b| b.is_ascii_digit()),
        "Invalid user-bus integer"
    );
    Ok(digits.parse()?)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Query<'a> {
    Id,
    Owner,
    Pid(&'a str),
    Uid(&'a str),
}
fn command(address: &str, query: Query<'_>) -> Result<Command> {
    let mut command = Command::new("/usr/bin/busctl");
    command
        .args([
            "--user",
            "--no-pager",
            "--timeout=3",
            "--auto-start=no",
            "--allow-interactive-authorization=no",
        ])
        .arg(format!("--address={address}"))
        .args([
            "call",
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        ]);
    match query {
        Query::Id => {
            command.arg("GetId");
        }
        Query::Owner => {
            command.args(["GetNameOwner", "s", "org.freedesktop.systemd1"]);
        }
        Query::Pid(name) | Query::Uid(name) => {
            ensure!(unique_name(name), "Invalid credential-query owner");
            command.args([
                if matches!(query, Query::Pid(_)) {
                    "GetConnectionUnixProcessID"
                } else {
                    "GetConnectionUnixUser"
                },
                "s",
                name,
            ]);
        }
    }
    command
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    Ok(command)
}
fn remaining(deadline: Instant) -> Result<()> {
    ensure!(
        Instant::now() < deadline,
        "User-manager identity observation deadline expired"
    );
    Ok(())
}
fn query(address: &str, what: Query<'_>, deadline: Instant) -> Result<Vec<u8>> {
    remaining(deadline)?; // Do not spawn even a read-only probe after budget exhaustion.
    let bytes = manager_output_until(command(address, what)?.spawn()?, deadline)?;
    remaining(deadline)?;
    Ok(bytes)
}
fn observe(
    mut query: impl FnMut(Query<'_>) -> Result<Vec<u8>>,
    mut generation: impl FnMut() -> Result<bool>,
    expected_pid: u32,
    expected_uid: u32,
) -> Result<()> {
    ensure!(
        generation()?,
        "Reviewed user-manager generation changed before bus observation"
    );
    let before_id = id_scalar(&query(Query::Id)?)?;
    let before_owner = name_scalar(&query(Query::Owner)?)?;
    let pid = unsigned_scalar(&query(Query::Pid(&before_owner))?)?;
    let uid = unsigned_scalar(&query(Query::Uid(&before_owner))?)?;
    let after_owner = name_scalar(&query(Query::Owner)?)?;
    let after_id = id_scalar(&query(Query::Id)?)?;
    ensure!(
        before_id == after_id && before_owner == after_owner,
        "User-manager bus or unique owner changed during observation"
    );
    ensure!(
        pid != 0 && pid == expected_pid && uid == expected_uid,
        "The current user-bus manager owner differs from the reviewed generation"
    );
    ensure!(
        generation()?,
        "Reviewed user-manager generation changed after bus observation"
    );
    Ok(())
}
pub(super) fn prove(expected: &Value) -> Result<()> {
    let uid = unsafe { libc::geteuid() };
    let pid = u32::try_from(
        expected["pid"]
            .as_u64()
            .context("Missing reviewed manager PID")?,
    )?;
    let runtime = PathBuf::from(format!("/run/user/{uid}"));
    let address = format!("unix:path={}/bus", runtime.display());
    route_environment(
        &runtime,
        &address,
        std::env::var_os("XDG_RUNTIME_DIR").as_deref(),
        std::env::var_os("DBUS_SESSION_BUS_ADDRESS").as_deref(),
    )?;
    let before = route_identity(&runtime, uid)?;
    let deadline = Instant::now() + Duration::from_secs(3);
    observe(
        |what| query(&address, what, deadline),
        || process::matches(expected),
        pid,
        uid,
    )?;
    ensure!(
        route_identity(&runtime, uid)? == before,
        "User runtime or bus socket was replaced during identity observation"
    );
    ensure!(
        process::matches(expected)?,
        "Reviewed user-manager generation changed after route observation"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        collections::VecDeque,
        os::unix::{fs::PermissionsExt, net::UnixListener},
    };

    fn responses() -> Vec<Vec<u8>> {
        [
            "s \"0123456789abcdef0123456789abcdef\"\n",
            "s \":1.4\"\n",
            "u 4245\n",
            "u 1000\n",
            "s \":1.4\"\n",
            "s \"0123456789abcdef0123456789abcdef\"\n",
        ]
        .iter()
        .map(|s| s.as_bytes().to_vec())
        .collect()
    }
    #[test]
    fn typed_scalars_refuse_malformed_excess_and_wrong_types() {
        assert_eq!(name_scalar(b"s \":1.4\"\n").unwrap(), ":1.4");
        for bytes in [
            b"s \"org.freedesktop.systemd1\"\n".as_slice(),
            b"s \":1\"\n",
            b"s \":1..4\"\n",
            b"s \":1.4\\x\"\n",
            b"s \":1.4\" \"extra\"\n",
            b"s \":1.4\"\n\n",
            b"u 4245\n",
        ] {
            assert!(name_scalar(bytes).is_err());
        }
        assert_eq!(unsigned_scalar(b"u 4294967295\n").unwrap(), u32::MAX);
        for bytes in [
            b"u 4294967296\n".as_slice(),
            b"u -1\n",
            b"u +1\n",
            b"u 1 2\n",
            b"u 1\nu 2\n",
            b"s \"1\"\n",
            b"u \n",
            b"u 1\r\n",
        ] {
            assert!(unsigned_scalar(bytes).is_err());
        }
        assert!(id_scalar(b"s \"0123456789abcdef0123456789abcdef\"\n").is_ok());
        assert!(id_scalar(b"s \"0123456789abcdef0123456789abcdeF\"\n").is_err());
        assert!(scalar_line(&vec![b'x'; 1025]).is_err());
    }
    #[test]
    fn credentials_are_queried_only_for_the_bracketed_unique_owner() {
        let mut replies: VecDeque<_> = responses().into();
        let mut seen = Vec::new();
        let generations = Cell::new(0);
        observe(
            |q| {
                seen.push(format!("{q:?}"));
                Ok(replies.pop_front().unwrap())
            },
            || {
                generations.set(generations.get() + 1);
                Ok(true)
            },
            4245,
            1000,
        )
        .unwrap();
        assert_eq!(
            seen,
            [
                "Id",
                "Owner",
                "Pid(\":1.4\")",
                "Uid(\":1.4\")",
                "Owner",
                "Id"
            ]
        );
        assert!(replies.is_empty());
        assert_eq!(generations.get(), 2);
    }
    #[test]
    fn changed_bus_owner_pid_uid_or_generation_refuses_without_fallback() {
        for (index, value) in [
            (5, "s \"fedcba9876543210fedcba9876543210\"\n"),
            (4, "s \":1.5\"\n"),
            (2, "u 4246\n"),
            (2, "u 0\n"),
            (3, "u 1001\n"),
        ] {
            let mut r = responses();
            r[index] = value.as_bytes().to_vec();
            let mut replies: VecDeque<_> = r.into();
            assert!(
                observe(
                    |_| Ok(replies.pop_front().unwrap()),
                    || Ok(true),
                    4245,
                    1000
                )
                .is_err()
            );
            assert!(replies.is_empty());
        }
        for fail_at in [1, 2] {
            let calls = Cell::new(0);
            let generations = Cell::new(0);
            let mut replies: VecDeque<_> = responses().into();
            assert!(
                observe(
                    |_| {
                        calls.set(calls.get() + 1);
                        Ok(replies.pop_front().unwrap())
                    },
                    || {
                        generations.set(generations.get() + 1);
                        Ok(generations.get() != fail_at)
                    },
                    4245,
                    1000
                )
                .is_err()
            );
            assert_eq!(calls.get(), if fail_at == 1 { 0 } else { 6 });
        }
        let calls = Cell::new(0);
        assert!(
            observe(
                |_| {
                    calls.set(calls.get() + 1);
                    Err(anyhow::anyhow!("unavailable"))
                },
                || Ok(true),
                4245,
                1000
            )
            .is_err()
        );
        assert_eq!(calls.get(), 1);
    }
    #[test]
    fn conflicting_ambient_routes_are_refused_without_changing_environment() {
        let r = Path::new("/run/user/1000");
        let a = "unix:path=/run/user/1000/bus";
        assert!(route_environment(r, a, None, None).is_ok());
        assert!(route_environment(r, a, Some(r.as_os_str()), Some(OsStr::new(a))).is_ok());
        for (xdg, bus) in [
            (Some(OsStr::new("/tmp/other")), None),
            (None, Some(OsStr::new("tcp:host=example.invalid"))),
            (None, Some(OsStr::new("unix:path=/run/user/1001/bus"))),
            (Some(OsStr::new("")), None),
        ] {
            assert!(route_environment(r, a, xdg, bus).is_err());
        }
    }
    #[test]
    fn private_runtime_and_socket_identity_are_required_without_inventing_socket_mode() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().canonicalize().unwrap();
        let uid = unsafe { libc::geteuid() };
        std::fs::set_permissions(&r, std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket = UnixListener::bind(r.join("bus")).unwrap();
        std::fs::set_permissions(r.join("bus"), std::fs::Permissions::from_mode(0o666)).unwrap();
        let before = route_identity(&r, uid).unwrap();
        assert!(route_identity(&r, uid.wrapping_add(1)).is_err());
        std::fs::set_permissions(&r, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(route_identity(&r, uid).is_err());
        std::fs::set_permissions(&r, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(route_identity(&r, uid).unwrap(), before);
        std::fs::remove_file(r.join("bus")).unwrap();
        let replacement = UnixListener::bind(r.join("bus")).unwrap();
        assert_ne!(route_identity(&r, uid).unwrap(), before);
        drop(replacement);
        drop(socket);
        std::fs::remove_file(r.join("bus")).unwrap();
        std::fs::write(r.join("bus"), b"not a socket").unwrap();
        assert!(route_identity(&r, uid).is_err());
    }
    #[test]
    fn command_is_fixed_readonly_and_expired_deadline_does_not_spawn() {
        let a = "unix:path=/run/user/1000/bus";
        let c = command(a, Query::Pid(":1.4")).unwrap();
        assert_eq!(c.get_program(), OsStr::new("/usr/bin/busctl"));
        let args: Vec<_> = c.get_args().map(|s| s.to_str().unwrap()).collect();
        assert_eq!(
            args,
            [
                "--user",
                "--no-pager",
                "--timeout=3",
                "--auto-start=no",
                "--allow-interactive-authorization=no",
                "--address=unix:path=/run/user/1000/bus",
                "call",
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "GetConnectionUnixProcessID",
                "s",
                ":1.4"
            ]
        );
        assert!(command(a, Query::Uid("--system")).is_err());
        // This error is before Command::spawn; no real bus is queried by this suite.
        assert!(
            query(a, Query::Id, Instant::now() - Duration::from_secs(1))
                .unwrap_err()
                .to_string()
                .contains("deadline expired")
        );
    }
    #[test]
    fn exhausted_collection_deadline_reaps_its_owned_probe() {
        let child = Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let p = process::identity(child.id()).unwrap().unwrap();
        assert!(manager_output_until(child, Instant::now() - Duration::from_secs(1)).is_err());
        assert!(!process::matches(&p).unwrap());
    }
}
