//! Helper-owned bootstrap discovery plus shared local platform support.
use amplifier_native_platform::{self as native, MACHINE_HELPER};
use anyhow::{Result, ensure};
pub use native::{arch, ensure_supported, hostname, open_browser, os, user_home};
use std::path::{Path, PathBuf};
pub fn default_home(id: &uuid::Uuid) -> Result<PathBuf> {
    native::default_home(&MACHINE_HELPER, &id.to_string())
}
pub fn installed_binary(home: &Path) -> PathBuf {
    native::installed_binary(&MACHINE_HELPER, home)
}
pub fn copy_self(home: &Path) -> Result<PathBuf> {
    native::copy_self(&MACHINE_HELPER, home)
}
pub fn systemd_unit(executable: &Path, home: &Path) -> Result<String> {
    native::systemd_unit(&MACHINE_HELPER, executable, home)
}
pub fn launch_agent(label: &str, executable: &Path, home: &Path) -> Result<String> {
    native::launch_agent(&MACHINE_HELPER, label, executable, home)
}
pub fn scheduled_task(executable: &Path, home: &Path) -> Result<String> {
    native::scheduled_task(&MACHINE_HELPER, executable, home)
}
pub fn install_startup(home: &Path, id: &uuid::Uuid) -> Result<()> {
    native::install_startup(&MACHINE_HELPER, home, id)
}
pub fn remove_startup(home: &Path, id: &uuid::Uuid) -> Result<()> {
    native::remove_startup(&MACHINE_HELPER, home, id)
}
pub fn task_matches(
    document: &str,
    executable: &Path,
    home: &Path,
    expected_sid: &str,
) -> Result<bool> {
    native::task_matches(&MACHINE_HELPER, document, executable, home, expected_sid)
}
pub fn show_launch_error(message: &str) {
    native::show_launch_error(&MACHINE_HELPER, message)
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

/// Selection only: no store, web listener, network or registration is opened.
#[derive(Debug, PartialEq, Eq)]
pub enum BootstrapChoice {
    Existing(PathBuf),
    Selected(PathBuf),
    Cancelled,
    Missing,
}

pub fn choose_initial_bootstrap(
    existing: Option<PathBuf>,
    interactive_unbound_mac: bool,
    choose: impl FnOnce() -> Result<Option<PathBuf>>,
) -> Result<BootstrapChoice> {
    if let Some(path) = existing {
        return Ok(BootstrapChoice::Existing(path));
    }
    if !interactive_unbound_mac {
        return Ok(BootstrapChoice::Missing);
    }
    Ok(match choose()? {
        Some(path) => BootstrapChoice::Selected(path),
        None => BootstrapChoice::Cancelled,
    })
}

pub fn read_selected_bootstrap(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;
    ensure!(path.is_absolute(), "Choose a local setup file");
    ensure!(
        !path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir)),
        "Choose the original setup file"
    );
    // Full ancestors retain a Windows drive/UNC root; a standalone Prefix
    // component (for example \\?\C:) is not an ordinary filesystem path.
    for observed in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
        let metadata = std::fs::symlink_metadata(observed)?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "Choose the original setup file, not a link"
        );
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            ensure!(
                metadata.file_attributes() & 0x400 == 0,
                "Choose the original setup file, not a link"
            );
        }
    }
    ensure!(
        std::fs::symlink_metadata(path)?.is_file(),
        "Choose an ordinary setup JSON file"
    );
    #[cfg(unix)]
    let file = open_selected_without_links(path)?;
    #[cfg(not(unix))]
    let file = {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x00200000); // FILE_FLAG_OPEN_REPARSE_POINT
        }
        options.open(path)?
    };
    let metadata = file.metadata()?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            metadata.file_attributes() & 0x400 == 0,
            "Choose the original setup file, not a link"
        );
    }
    ensure!(
        metadata.is_file() && metadata.len() <= 131072,
        "Choose an ordinary setup JSON file smaller than 128 KiB"
    );
    let mut bytes = Vec::new();
    file.take(131073).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 131072, "Setup file is too large");
    Ok(bytes)
}

#[cfg(unix)]
fn open_selected_without_links(path: &Path) -> Result<std::fs::File> {
    use std::{
        ffi::CString,
        os::fd::{AsRawFd, FromRawFd},
        os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    };
    let mut current = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")?;
    let mut names = path
        .components()
        .filter_map(|part| match part {
            std::path::Component::Normal(name) => Some(name),
            _ => None, // Absolute path and parent components were checked above.
        })
        .peekable();
    ensure!(names.peek().is_some(), "Choose an ordinary setup JSON file");
    while let Some(name) = names.next() {
        let name = CString::new(name.as_bytes())?;
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | if names.peek().is_some() {
                libc::O_DIRECTORY
            } else {
                libc::O_NONBLOCK
            };
        // Each component is opened relative to the retained parent descriptor;
        // replacing a pathname cannot redirect traversal through a symlink.
        let raw = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
        if raw < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // openat returned a newly owned descriptor and the prior parent closes
        // only after this assignment has acquired its child descriptor.
        current = unsafe { std::fs::File::from_raw_fd(raw) };
    }
    Ok(current)
}

#[cfg(target_os = "macos")]
pub fn choose_bootstrap_file() -> Result<Option<PathBuf>> {
    use std::{ffi::CStr, os::unix::ffi::OsStrExt};
    unsafe extern "C" {
        fn constellation_choose_pairing_file(buffer: *mut std::ffi::c_char, capacity: usize)
        -> i32;
    }
    let mut buffer = vec![0u8; 32768];
    // The shim opens only a native file panel on the main thread. It receives
    // no dynamic script, destination, credentials or filesystem write request.
    let result =
        unsafe { constellation_choose_pairing_file(buffer.as_mut_ptr().cast(), buffer.len()) };
    match result {
        0 => Ok(None),
        1 => {
            let value = CStr::from_bytes_until_nul(&buffer)?;
            let path = PathBuf::from(std::ffi::OsStr::from_bytes(value.to_bytes()));
            ensure!(path.is_absolute(), "Choose a local setup file");
            Ok(Some(path))
        }
        _ => anyhow::bail!(
            "The setup file picker could not open. Your computer has not been paired."
        ),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn choose_bootstrap_file() -> Result<Option<PathBuf>> {
    anyhow::bail!("The native setup file picker is only available on Mac")
}
