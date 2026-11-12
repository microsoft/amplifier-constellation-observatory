use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthMode {
    #[default]
    Pam,
    LocalPassword,
}
impl AuthMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pam => "pam",
            Self::LocalPassword => "local-password",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WebTransport {
    #[default]
    Https,
    LocalHttp,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub id: String,
    pub owner: String,
    pub bind: String,
    pub port: u16,
    pub public_url: String,
    #[serde(default)]
    pub auth_mode: AuthMode,
    #[serde(default)]
    pub web_transport: WebTransport,
    #[serde(default)]
    pub pam_service: String,
    pub tls_cert: PathBuf,
    pub tls_key: PathBuf,
    pub tls_ca: Option<PathBuf>,
    #[serde(default)]
    pub embed_origins: Vec<String>,
}
impl Config {
    pub fn load(home: &Path) -> Result<Self> {
        let c: Self = serde_json::from_slice(&std::fs::read(home.join("config.json"))?)?;
        c.validate()?;
        Ok(c)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            uuid::Uuid::parse_str(&self.id).is_ok(),
            "Invalid installation ID"
        );
        ensure!(
            !self.owner.is_empty() && self.owner.len() < 100,
            "Choose an owner account"
        );
        match self.web_transport {
            WebTransport::Https => ensure!(
                self.public_url.is_empty() || valid_origin(&self.public_url),
                "Use an HTTPS origin"
            ),
            WebTransport::LocalHttp => {
                ensure!(
                    self.bind == "127.0.0.1"
                        && self.port != 0
                        && (self.public_url == format!("http://127.0.0.1:{}", self.port)
                            || self.public_url == format!("http://localhost:{}", self.port))
                        && self.embed_origins.is_empty(),
                    "Local HTTP requires a loopback-only address and matching local origin"
                );
                ensure!(
                    self.auth_mode == AuthMode::LocalPassword,
                    "Local HTTP uses an Observatory password, not a system account"
                );
            }
        }
        ensure!(
            self.embed_origins.iter().all(|x| valid_origin(x)),
            "Use HTTPS embedding origins"
        );
        Ok(())
    }
    pub fn save(&self, home: &Path) -> Result<()> {
        self.validate()?;
        private_write(&home.join("config.json"), &serde_json::to_vec_pretty(self)?)
    }
}
pub fn valid_origin(value: &str) -> bool {
    reqwest::Url::parse(value).is_ok_and(|u| {
        u.scheme() == "https"
            && u.host_str().is_some()
            && u.username().is_empty()
            && u.password().is_none()
            && u.query().is_none()
            && u.fragment().is_none()
            && u.path() == "/"
    })
}
pub fn safe_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
}
pub fn private_dir(path: &Path) -> Result<()> {
    amplifier_native_platform::private_dir(path)
}
pub fn private_write(path: &Path, data: &[u8]) -> Result<()> {
    amplifier_native_platform::private_write(path, data)
}
pub fn private_create(path: &Path, data: &[u8]) -> Result<()> {
    amplifier_native_platform::private_create(path, data)
}
