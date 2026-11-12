use crate::storage::private_directory;
use anyhow::{Context, Result, bail, ensure};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

pub fn os() -> &'static str {
    std::env::consts::OS
}
pub fn arch() -> &'static str {
    std::env::consts::ARCH
}
pub fn user_home() -> Result<PathBuf> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let home = env::var_os(key)
        .map(PathBuf::from)
        .context("Cannot find this user's home folder")?;
    ensure!(home.is_absolute(), "User home must be absolute");
    Ok(home)
}
pub fn default_home(observatory_id: &uuid::Uuid) -> Result<PathBuf> {
    let root = if cfg!(windows) {
        env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .context("Cannot find local application data")?
    } else if cfg!(target_os = "macos") {
        user_home()?.join("Library/Application Support")
    } else {
        env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or(user_home()?.join(".local/share"))
    };
    Ok(root
        .join("constellation-machine-helper")
        .join(observatory_id.to_string()))
}
pub fn hostname() -> String {
    let key = if cfg!(windows) {
        "COMPUTERNAME"
    } else {
        "HOSTNAME"
    };
    env::var(key)
        .ok()
        .or_else(|| {
            Command::new("hostname")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        })
        .filter(|s| !s.is_empty() && s.len() <= 160 && !s.chars().any(char::is_control))
        .unwrap_or_else(|| "My computer".into())
}
pub fn bootstrap_path(executable: &Path) -> Option<PathBuf> {
    let mut candidates = vec![executable.parent()?.join("observatory-pairing.json")];
    for ancestor in executable.ancestors() {
        if ancestor.extension().is_some_and(|e| e == "app") {
            candidates.push(ancestor.join("Contents/Resources/observatory-pairing.json"));
            if let Some(parent) = ancestor.parent() {
                candidates.push(parent.join("observatory-pairing.json"));
            }
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}
pub fn open_browser(url: &str) -> Result<()> {
    ensure!(
        url.starts_with("http://127.0.0.1:"),
        "Setup must open on this computer"
    );
    #[cfg(windows)]
    let status = Command::new("rundll32.exe")
        .args(["url.dll,FileProtocolHandler", url])
        .status()?;
    #[cfg(target_os = "macos")]
    let status = Command::new("open").arg(url).status()?;
    #[cfg(all(unix, not(target_os = "macos")))]
    let status = Command::new("xdg-open").arg(url).status()?;
    ensure!(
        status.success(),
        "Could not open a browser; open the printed local setup address"
    );
    Ok(())
}
pub fn installed_binary(home: &Path) -> PathBuf {
    home.join("bin").join(if cfg!(windows) {
        "constellation-machine-helper.exe"
    } else {
        "constellation-machine-helper"
    })
}
pub fn copy_self(home: &Path) -> Result<PathBuf> {
    let target = installed_binary(home);
    private_directory(target.parent().unwrap())?;
    let current = env::current_exe()?.canonicalize()?;
    if target.exists() {
        ensure!(
            !target.is_symlink(),
            "The installed helper cannot be a symbolic link"
        );
        if current == target.canonicalize()? {
            return Ok(target);
        }
        ensure!(
            fs::read(&target)? == fs::read(&current)?,
            "Another helper version is already installed; it was preserved"
        );
        return Ok(target);
    }
    fs::copy(current, &target)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o700))?;
    }
    Ok(target)
}
fn safe_path(path: &Path) -> Result<&str> {
    let s = path
        .to_str()
        .context("This path cannot be represented in startup settings")?;
    ensure!(
        !s.chars().any(char::is_control),
        "Startup paths cannot contain control characters"
    );
    Ok(s)
}
pub fn systemd_unit(executable: &Path, home: &Path) -> Result<String> {
    fn quote(s: &str) -> String {
        format!(
            "\"{}\"",
            s.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('%', "%%")
                .replace('$', "$$")
        )
    }
    Ok(format!(
        "[Unit]\nDescription=Constellation computer connection\nAfter=network-online.target\n[Service]\nExecStart={} --home {} run\nRestart=on-failure\nRestartSec=5\nUMask=0077\n[Install]\nWantedBy=default.target\n",
        quote(safe_path(executable)?),
        quote(safe_path(home)?)
    ))
}
fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
pub fn launch_agent(label: &str, executable: &Path, home: &Path) -> Result<String> {
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array><string>{}</string><string>--home</string><string>{}</string><string>run</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict><key>ThrottleInterval</key><integer>5</integer></dict></plist>\n",
        xml(label),
        xml(safe_path(executable)?),
        xml(safe_path(home)?)
    ))
}
pub fn scheduled_task(executable: &Path, home: &Path) -> Result<String> {
    // Arguments follow the Windows CommandLineToArgvW quoting rule.
    let path = safe_path(home)?;
    ensure!(
        !path.contains('"'),
        "Windows startup path cannot contain quotes"
    );
    let escaped = path.trim_end_matches('\\');
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?><Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\"><Triggers><LogonTrigger><Enabled>true</Enabled></LogonTrigger></Triggers><Principals><Principal id=\"Author\"><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals><Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><ExecutionTimeLimit>PT0S</ExecutionTimeLimit><RestartOnFailure><Interval>PT1M</Interval><Count>3</Count></RestartOnFailure></Settings><Actions Context=\"Author\"><Exec><Command>{}</Command><Arguments>--home &quot;{}&quot; run</Arguments></Exec></Actions></Task>\n",
        xml(safe_path(executable)?),
        xml(escaped)
    ))
}
pub fn install_startup(home: &Path, id: &uuid::Uuid) -> Result<()> {
    let executable = copy_self(home)?;
    let label = format!("constellation-machine-{id}");
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let units = user_home()?.join(".config/systemd/user");
        fs::create_dir_all(&units)?;
        let path = units.join(format!("{label}.service"));
        let expected = systemd_unit(&executable, home)?;
        if path.exists() {
            ensure!(
                fs::read_to_string(&path)? == expected,
                "Existing startup settings differ; they were preserved"
            );
        } else {
            fs::write(&path, expected)?;
        }
        ensure!(
            Command::new("systemctl")
                .args(["--user", "daemon-reload"])
                .status()?
                .success(),
            "User service manager is unavailable"
        );
        ensure!(
            Command::new("systemctl")
                .args(["--user", "enable", &format!("{label}.service")])
                .status()?
                .success(),
            "Could not enable startup for this user"
        );
    }
    #[cfg(target_os = "macos")]
    {
        let directory = user_home()?.join("Library/LaunchAgents");
        fs::create_dir_all(&directory)?;
        let path = directory.join(format!("{label}.plist"));
        let expected = launch_agent(&label, &executable, home)?;
        if path.exists() {
            ensure!(
                fs::read_to_string(&path)? == expected,
                "Existing startup settings differ; they were preserved"
            );
        } else {
            fs::write(&path, expected)?;
        }
        // LaunchAgent is loaded at the next user sign-in; current process already runs.
    }
    #[cfg(windows)]
    {
        if let Some(existing) = query_windows_task(&label)? {
            ensure!(
                task_matches(
                    &existing,
                    &executable,
                    home,
                    &crate::storage::current_user_sid()?
                )?,
                "Existing startup task belongs to another command; it was preserved"
            );
            return Ok(());
        }
        let xml = scheduled_task(&executable, home)?.replace(
            "<LogonType>",
            &format!(
                "<UserId>{}</UserId><LogonType>",
                crate::storage::current_user_sid()?
            ),
        );
        let path = home.join("startup.xml");
        let bytes: Vec<u8> = std::iter::once(0xfeff)
            .chain(xml.encode_utf16())
            .flat_map(u16::to_le_bytes)
            .collect();
        fs::write(&path, bytes)?;
        let status = Command::new("schtasks.exe")
            .arg("/Create")
            .arg("/TN")
            .arg(&label)
            .arg("/XML")
            .arg(path)
            .status()?;
        ensure!(
            status.success(),
            "Could not create sign-in startup; an existing task was not replaced"
        );
    }
    Ok(())
}
pub fn remove_startup(home: &Path, id: &uuid::Uuid) -> Result<()> {
    let label = format!("constellation-machine-{id}");
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let path = user_home()?
            .join(".config/systemd/user")
            .join(format!("{label}.service"));
        if path.exists() {
            ensure!(
                fs::read_to_string(&path)? == systemd_unit(&installed_binary(home), home)?,
                "Startup settings changed; remove them in your system settings"
            );
            let status = Command::new("systemctl")
                .args(["--user", "disable", &format!("{label}.service")])
                .status()?;
            ensure!(status.success(), "Could not disable startup");
            fs::remove_file(path)?;
        }
    }
    #[cfg(target_os = "macos")]
    {
        let path = user_home()?
            .join("Library/LaunchAgents")
            .join(format!("{label}.plist"));
        if path.exists() {
            ensure!(
                fs::read_to_string(&path)? == launch_agent(&label, &installed_binary(home), home)?,
                "Startup settings changed; remove them in your system settings"
            );
            fs::remove_file(path)?;
        }
    }
    #[cfg(windows)]
    {
        let Some(existing) = query_windows_task(&label)? else {
            return Ok(());
        };
        ensure!(
            task_matches(
                &existing,
                &installed_binary(home),
                home,
                &crate::storage::current_user_sid()?
            )?,
            "Startup task belongs to another command; it was preserved"
        );
        let status = Command::new("schtasks.exe")
            .args(["/Delete", "/TN", &label, "/F"])
            .status()?;
        ensure!(status.success(), "Could not remove sign-in startup");
    }
    Ok(())
}
pub fn ensure_supported() -> Result<()> {
    if !["windows", "macos", "linux"].contains(&os()) || !["aarch64", "x86_64"].contains(&arch()) {
        bail!("This computer platform is not supported")
    };
    Ok(())
}

/// Check the fixed startup action before touching an existing Windows task.
pub fn task_matches(
    document: &str,
    executable: &Path,
    home: &Path,
    expected_sid: &str,
) -> Result<bool> {
    fn element<'a>(text: &'a str, name: &str) -> Option<&'a str> {
        let start = format!("<{name}>");
        let end = format!("</{name}>");
        if text.matches(&start).count() != 1 || text.matches(&end).count() != 1 {
            return None;
        }
        let value = text.split_once(&start)?.1.split_once(&end)?.0;
        if value.contains('<') {
            return None;
        }
        Some(value)
    }
    fn decode(value: &str) -> String {
        value
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&")
    }
    let command = element(document, "Command").map(decode);
    let args = element(document, "Arguments").map(decode);
    // Task Scheduler omits the optional RunLevel when it has the default
    // least-privilege value. Do not confuse an invalid/duplicate element with
    // that omission: only no mention or one exact value is accepted.
    let least_privilege =
        match document.matches("<RunLevel").count() + document.matches("</RunLevel").count() {
            0 => true,
            2 => element(document, "RunLevel") == Some("LeastPrivilege"),
            _ => false,
        };
    Ok(document.matches("<Exec>").count() == 1
        && document.matches("<Principal ").count() == 1
        && !document.contains("<Principal>")
        && element(document, "UserId").map(decode).as_deref() == Some(expected_sid)
        && element(document, "LogonType") == Some("InteractiveToken")
        && least_privilege
        && !document.contains("<ComHandler>")
        && command.as_deref() == Some(safe_path(executable)?)
        && args.as_deref()
            == Some(&format!(
                "--home \"{}\" run",
                safe_path(home)?.trim_end_matches('\\')
            )))
}
#[cfg(windows)]
fn query_windows_task(label: &str) -> Result<Option<String>> {
    let output = Command::new("schtasks.exe")
        .args(["/Query", "/TN", label, "/XML"])
        .output()?;
    if !output.status.success() {
        return Ok(None);
    }
    let bytes = output.stdout;
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[b'<', 0]) {
        let start = if bytes.starts_with(&[0xff, 0xfe]) {
            2
        } else {
            0
        };
        ensure!((bytes.len() - start) % 2 == 0, "Invalid task XML encoding");
        let wide: Vec<u16> = bytes[start..]
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        Ok(Some(String::from_utf16(&wide)?))
    } else {
        Ok(Some(String::from_utf8(bytes)?))
    }
}

/// Foreground launch failures must remain visible when no terminal was opened.
/// Background mode logs instead, avoiding repeated system dialogs at sign-in.
pub fn show_launch_error(message: &str) {
    let message = message.chars().take(3000).collect::<String>();
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
        let text: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
        let title: Vec<u16> = "Constellation Computer"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
    #[cfg(target_os = "macos")]
    {
        // Fixed AppleScript; the error is data in argv, never source code.
        let _ = Command::new("osascript")
            .args([
                "-e",
                "on run argv",
                "-e",
                "display alert \"Constellation Computer\" message (item 1 of argv) as critical",
                "-e",
                "end run",
                "--",
                &message,
            ])
            .status();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // Desktop environments normally show the browser; use an installed
        // native dialog tool only for failures before that page can open.
        let _ = Command::new("zenity")
            .args([
                "--error",
                "--title=Constellation Computer",
                "--text",
                &message,
            ])
            .status();
    }
}
