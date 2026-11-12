use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

pub fn random_secret() -> String {
    let mut bytes = [0_u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn private_directory(path: &Path) -> Result<()> {
    ensure!(
        !path.is_symlink(),
        "Use an ordinary helper folder, not a symbolic link"
    );
    fs::create_dir_all(path)?;
    private_permissions(path, true)
}

#[cfg(unix)]
fn private_permissions(path: &Path, directory: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if directory { 0o700 } else { 0o600 }),
    )?;
    Ok(())
}

#[cfg(windows)]
pub fn current_user_sid() -> Result<String> {
    use std::ptr;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, LocalFree},
        Security::{
            Authorization::ConvertSidToStringSidW, GetTokenInformation, TOKEN_QUERY, TOKEN_USER,
            TokenUser,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };
    unsafe {
        let mut token = ptr::null_mut();
        ensure!(
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) != 0,
            "Cannot identify this Windows user"
        );
        let mut needed = 0;
        GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut needed);
        let mut buffer = vec![0_usize; (needed as usize).div_ceil(size_of::<usize>())];
        let ok = GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        );
        CloseHandle(token);
        ensure!(ok != 0, "Cannot read this Windows user's identity");
        let user = &*(buffer.as_ptr().cast::<TOKEN_USER>());
        let mut sid = ptr::null_mut();
        ensure!(
            ConvertSidToStringSidW(user.User.Sid, &mut sid) != 0,
            "Cannot describe this Windows user"
        );
        let mut len = 0;
        while *sid.add(len) != 0 {
            len += 1;
        }
        let result = String::from_utf16_lossy(std::slice::from_raw_parts(sid, len));
        LocalFree(sid.cast());
        Ok(result)
    }
}
#[cfg(windows)]
fn private_permissions(path: &Path, _directory: bool) -> Result<()> {
    use std::{os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, SetFileSecurityW,
        },
    };
    let sid = current_user_sid()?;
    unsafe {
        let sddl: Vec<u16> = format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;{sid})")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut descriptor = ptr::null_mut();
        ensure!(
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                ptr::null_mut()
            ) != 0,
            "Cannot create private storage policy"
        );
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let result = SetFileSecurityW(
            wide.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        );
        LocalFree(descriptor);
        ensure!(
            result != 0,
            "Cannot restrict helper storage to this Windows user"
        );
    }
    Ok(())
}

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
        let temporary = self.path(&format!("save-{}.tmp", uuid::Uuid::new_v4()))?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        private_permissions(&temporary, false)?;
        let result = (|| -> Result<()> {
            file.write_all(data)?;
            file.sync_all()?;
            drop(file);
            replace(&temporary, &target)?;
            #[cfg(unix)]
            File::open(&self.home)?.sync_all()?;
            Ok(())
        })();
        if temporary.exists() {
            let _ = fs::remove_file(temporary);
        }
        result
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
        self.write("credential", secret.as_bytes())?;
        Ok(secret)
    }
}
#[cfg(not(windows))]
fn replace(from: &Path, to: &Path) -> Result<()> {
    fs::rename(from, to)?;
    Ok(())
}
#[cfg(windows)]
fn replace(from: &Path, to: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    ensure!(
        unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } != 0,
        "Cannot commit helper state"
    );
    Ok(())
}
