use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::{self, File, OpenOptions},
    path::PathBuf,
};

pub fn random_secret() -> String {
    let mut bytes = [0_u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(windows)]
pub use amplifier_native_platform::current_user_sid;
pub use amplifier_native_platform::private_dir as private_directory;

pub struct Store {
    pub home: PathBuf,
}
impl Store {
    pub fn open(home: PathBuf) -> Result<Self> {
        ensure!(
            home.is_absolute(),
            "The helper folder must be an absolute path"
        );
        private_directory(&home)?;
        Ok(Self { home })
    }
    fn path(&self, name: &str) -> Result<PathBuf> {
        ensure!(
            !name.is_empty()
                && name.len() <= 80
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                && name != "."
                && name != "..",
            "Invalid private file name"
        );
        let path = self.home.join(name);
        ensure!(
            !path.is_symlink(),
            "Private helper files cannot be symbolic links"
        );
        Ok(path)
    }
    pub fn read<T: DeserializeOwned>(&self, name: &str) -> Result<Option<T>> {
        let path = self.path(name)?;
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(path)?;
        ensure!(bytes.len() <= 1024 * 1024, "Helper state is too large");
        Ok(Some(serde_json::from_slice(&bytes).context(
            "Saved helper state needs review; it was not replaced",
        )?))
    }
    pub fn save<T: Serialize>(&self, name: &str, value: &T) -> Result<()> {
        let data = serde_json::to_vec_pretty(value)?;
        ensure!(data.len() <= 1024 * 1024, "Helper state is too large");
        self.write(name, &data)
    }
    pub fn write(&self, name: &str, data: &[u8]) -> Result<()> {
        let target = self.path(name)?;
        amplifier_native_platform::private_write(&target, data)
    }
    pub fn lock(&self) -> Result<File> {
        let path = self.path("helper.lock")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        fs2::FileExt::try_lock_exclusive(&file).context("The machine helper is already running")?;
        Ok(file)
    }
    pub fn secret(&self) -> Result<String> {
        let path = self.path("credential")?;
        if path.exists() {
            let secret = fs::read_to_string(path)?;
            ensure!(
                secret.len() == 64
                    && secret
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
                "Saved helper credential needs review; it was not replaced"
            );
            return Ok(secret);
        }
        let mut bytes = [0_u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        let secret = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        amplifier_native_platform::private_create(&path, secret.as_bytes())?;
        Ok(secret)
    }
}
