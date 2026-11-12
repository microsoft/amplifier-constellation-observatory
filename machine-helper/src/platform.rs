//! Helper-owned bootstrap discovery plus shared local platform support.
use amplifier_native_platform::{self as native, MACHINE_HELPER};
use anyhow::Result;
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
