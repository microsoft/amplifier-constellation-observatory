use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub id: String,
    pub owner: String,
    pub bind: String,
    pub port: u16,
    pub public_url: String,
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
        ensure!(
            self.public_url.is_empty() || valid_origin(&self.public_url),
            "Use an HTTPS origin"
        );
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
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
pub fn private_write(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let temporary = path.with_file_name(format!(".write-{}", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(data)?;
    file.sync_all()?;
    std::fs::rename(temporary, path)?;
    Ok(())
}
