// Copyright © 2016-2026 The SENTIENT Authors

//! Installation secrets, kept out of the installation record: the database
//! passwords (backup, restore, migration, update), the JWT signing secret
//! (repair re-registers the service with it, so a repair signs no one out).
//!
//! Windows keeps them machine-wide, not in one user's Credential Manager: any
//! administrator, and setup started from Apps & features under another
//! account, must find them. One file per secret in
//! `%ProgramData%\SENTIENT\secrets`, readable only by Administrators and
//! SYSTEM, encrypted with DPAPI in machine scope.

#[derive(Clone, Copy)]
pub enum Secret {
    /// The SENTIENT database user's password.
    Database,
    /// The PostgreSQL superuser's password.
    PostgresAdmin,
    /// The secret SENTIENT signs sign-in tokens with.
    Jwt,
    /// The ClickHouse database user's password (backup, restore).
    Clickhouse,
    /// The hidden ClickHouse account's password (Windows hybrid).
    ClickhouseAccount,
    /// An install interrupted by a restart, to continue after logon.
    PendingInstall,
}

impl Secret {
    #[cfg_attr(not(windows), allow(dead_code))]
    fn account(self) -> &'static str {
        match self {
            Secret::Database => "database",
            Secret::PostgresAdmin => "postgres-admin",
            Secret::Jwt => "jwt-secret",
            Secret::Clickhouse => "clickhouse",
            Secret::ClickhouseAccount => "clickhouse-account",
            Secret::PendingInstall => "pending-install",
        }
    }
}

#[cfg(windows)]
mod store {
    use std::path::PathBuf;

    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_LOCAL_MACHINE, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData,
        CryptUnprotectData,
    };

    const ENTROPY: &[u8] = b"SENTIENT Platform";

    pub fn dir() -> PathBuf {
        let base = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
        base.join("SENTIENT").join("secrets")
    }

    /// The folder, readable only by Administrators and SYSTEM.
    pub fn ensure_dir() -> Result<PathBuf, String> {
        let dir = dir();
        if !dir.exists() {
            std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        // Well-known SIDs: names differ by Windows language.
        let ok = std::process::Command::new("icacls.exe")
            .arg(&dir)
            .args(["/inheritance:r", "/grant:r", "*S-1-5-18:(OI)(CI)F", "*S-1-5-32-544:(OI)(CI)F"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ok {
            return Err(format!("restrict access to {}", dir.display()));
        }
        Ok(dir)
    }

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 }
    }

    /// Copy the output blob and free it.
    unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
        unsafe { LocalFree(out.pbData as _) };
        v
    }

    pub fn protect(plain: &[u8]) -> Result<Vec<u8>, String> {
        let input = blob(plain);
        let entropy = blob(ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_LOCAL_MACHINE | CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            return Err(format!("encrypt: {}", std::io::Error::last_os_error()));
        }
        Ok(unsafe { take(out) })
    }

    pub fn unprotect(sealed: &[u8]) -> Option<Vec<u8>> {
        let input = blob(sealed);
        let entropy = blob(ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        (ok != 0).then(|| unsafe { take(out) })
    }
}

#[cfg(windows)]
const ALL: [Secret; 6] = [
    Secret::Database,
    Secret::PostgresAdmin,
    Secret::Jwt,
    Secret::Clickhouse,
    Secret::ClickhouseAccount,
    Secret::PendingInstall,
];

#[cfg(windows)]
pub fn save(secret: Secret, value: &str) -> Result<(), String> {
    let dir = store::ensure_dir().map_err(|e| format!("save the {}: {e}", secret.account()))?;
    let sealed = store::protect(value.as_bytes()).map_err(|e| format!("save the {}: {e}", secret.account()))?;
    std::fs::write(dir.join(format!("{}.bin", secret.account())), sealed)
        .map_err(|e| format!("save the {}: {e}", secret.account()))
}

#[cfg(windows)]
pub fn get(secret: Secret) -> Option<String> {
    let sealed = std::fs::read(store::dir().join(format!("{}.bin", secret.account()))).ok()?;
    String::from_utf8(store::unprotect(&sealed)?).ok()
}

#[cfg(windows)]
pub fn forget(secret: Secret) {
    let _ = std::fs::remove_file(store::dir().join(format!("{}.bin", secret.account())));
}

#[cfg(not(windows))]
pub fn forget(_: Secret) {}

#[cfg(windows)]
pub fn forget_all() {
    for secret in ALL {
        forget(secret);
    }
    let _ = std::fs::remove_dir(store::dir());
}

// Linux installs are managed through systemd or Docker; the operator types
// passwords where they are needed.
#[cfg(not(windows))]
pub fn save(_: Secret, _: &str) -> Result<(), String> {
    Ok(())
}

#[cfg(not(windows))]
pub fn get(_: Secret) -> Option<String> {
    None
}

#[cfg(not(windows))]
pub fn forget_all() {}
