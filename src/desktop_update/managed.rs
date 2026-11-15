//! Existing Linux user-unit adoption. The owner first stops and reviews effects;
//! this module neither manufactures that review nor changes the unit definition.
#[cfg(not(target_os = "linux"))]
use anyhow::Result;
use serde_json::Value;
#[cfg(not(target_os = "linux"))]
use std::path::Path;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{adopt, apply, finish, guard, prepare_update};

pub fn journal(value: &Value) -> bool {
    value["schema"] == 1
        && matches!(
            value["kind"].as_str(),
            Some("managed_adoption" | "managed_update")
        )
}
pub fn known_kind(value: &Value) -> bool {
    value.get("kind").is_none() || journal(value)
}

#[cfg(not(target_os = "linux"))]
pub async fn adopt(_: &Path, _: Option<&Path>, _: Option<&str>, _: bool, _: bool) -> Result<Value> {
    anyhow::bail!("Existing-service adoption currently requires a Linux user systemd installation")
}
#[cfg(not(target_os = "linux"))]
pub async fn apply(_: &Path, _: &mut Value) -> Result<()> {
    anyhow::bail!("Managed application updates are unsupported on this platform")
}
#[cfg(not(target_os = "linux"))]
pub fn guard(_: &Path, _: Option<&Value>) -> Result<std::fs::File> {
    anyhow::bail!("Managed application startup is unsupported on this platform")
}
#[cfg(not(target_os = "linux"))]
pub fn finish(_: &crate::state::AppState, _: &Value) -> Result<()> {
    anyhow::bail!("Managed application settlement is unsupported on this platform")
}
#[cfg(not(target_os = "linux"))]
pub fn prepare_update(_: &Path, _: &mut Value) -> Result<()> {
    anyhow::bail!("Managed application updates are unsupported on this platform")
}
