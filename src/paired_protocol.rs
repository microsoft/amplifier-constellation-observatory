//! One fixed installer request, carried by the existing paired-computer channel.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::net::IpAddr;
use uuid::Uuid;

pub const RECIPE: &str = "paired-instance";
pub const INSTALL_ACCESS: &str = "install-agents";
pub fn package_lock() -> Value {
    serde_json::from_str(include_str!("../scripts/instance-bundle-lock.json")).unwrap()
}
pub fn target(os: &str, arch: &str) -> Result<String> {
    ensure!(
        ["aarch64", "x86_64"].contains(&arch),
        "Unsupported computer architecture"
    );
    let suffix = match os {
        "linux" => "unknown-linux-gnu",
        "macos" => "apple-darwin",
        "windows" => "pc-windows-msvc",
        _ => anyhow::bail!("Unsupported computer system"),
    };
    Ok(format!("{arch}-{suffix}"))
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct InstallRequest {
    pub schema: u8,
    pub id: Uuid,
    pub observatory_id: Uuid,
    pub host_id: Uuid,
    pub machine_id: Uuid,
    pub pairing_id: Uuid,
    pub name: String,
    pub target: String,
    pub package_sha256: String,
    pub bind_address: IpAddr,
}
impl InstallRequest {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == 1
                && !self.name.trim().is_empty()
                && self.name.len() <= 120
                && !self.name.chars().any(char::is_control),
            "Invalid agent installation request"
        );
        ensure!(
            !self.bind_address.is_unspecified() && !self.bind_address.is_multicast(),
            "Use this computer's connected network address"
        );
        let lock = package_lock();
        let expected = lock["targets"][&self.target]["sha256"]
            .as_str()
            .context("Unsupported package target")?;
        ensure!(
            self.package_sha256 == expected,
            "The requested agent package differs from this helper's supported package"
        );
        Ok(())
    }
    pub fn digest(&self) -> String {
        format!("{:x}", Sha256::digest(serde_json::to_vec(self).unwrap()))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Attachment {
    pub instance_id: Uuid,
    pub origin: String,
    pub ca_pem: String,
    pub credential: String,
    pub session_cwd: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct InstallResult {
    pub request: InstallRequest,
    pub state: String,
    pub message: String,
    pub attachment: Option<Attachment>,
}
impl InstallResult {
    pub fn validate(&self) -> Result<()> {
        self.request.validate()?;
        ensure!(
            ["completed", "failed", "unknown"].contains(&self.state.as_str())
                && self.message.len() <= 1000,
            "Invalid installation result"
        );
        ensure!(
            (self.state == "completed") == self.attachment.is_some(),
            "Incomplete installation receipt"
        );
        if let Some(a) = &self.attachment {
            let workspace = a.session_cwd.replace('\\', "/");
            ensure!(
                workspace.len() <= 8192
                    && !workspace.chars().any(char::is_control)
                    && !workspace.split('/').any(|s| s == "." || s == "..")
                    && workspace.contains(&format!("/agents/{}/", self.request.id)),
                "The installed workspace belongs to another installation"
            );
            let url = reqwest::Url::parse(&a.origin)?;
            ensure!(
                url.scheme() == "https"
                    && url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none()
                    && url.path() == "/"
                    && url.port().is_some(),
                "Invalid installed agent address"
            );
            let host = url
                .host_str()
                .unwrap()
                .trim_matches(['[', ']'])
                .parse::<IpAddr>()?;
            ensure!(
                host == self.request.bind_address,
                "The installed agent answered on another computer"
            );
            ensure!(
                (32..=4096).contains(&a.credential.len())
                    && !a.credential.chars().any(char::is_whitespace)
                    && !a.ca_pem.is_empty()
                    && a.ca_pem.len() <= 16384,
                "Invalid installed agent access"
            );
        }
        Ok(())
    }
}
