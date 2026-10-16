// Copyright © 2016-2026 The SENTIENT Authors

//! Installation secrets, kept in the operating system's credential store
//! (Windows Credential Manager) rather than in the installation record: the
//! database password (backup, restore), and the PostgreSQL admin password and
//! JWT signing secret (repair re-registers the service with them, so a repair
//! neither needs the operator to remember them nor signs everyone out).

#[derive(Clone, Copy)]
pub enum Secret {
    /// The SENTIENT database user's password.
    Database,
    /// The PostgreSQL superuser's password.
    PostgresAdmin,
    /// The secret SENTIENT signs sign-in tokens with.
    Jwt,
}

impl Secret {
    #[cfg_attr(not(windows), allow(dead_code))]
    fn account(self) -> &'static str {
        match self {
            Secret::Database => "database",
            Secret::PostgresAdmin => "postgres-admin",
            Secret::Jwt => "jwt-secret",
        }
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
const SERVICE: &str = "SENTIENT Platform";

#[cfg(windows)]
pub fn save(secret: Secret, value: &str) -> Result<(), String> {
    keyring::Entry::new(SERVICE, secret.account())
        .and_then(|e| e.set_password(value))
        .map_err(|e| format!("save the {} in Credential Manager: {e}", secret.account()))
}

#[cfg(windows)]
pub fn get(secret: Secret) -> Option<String> {
    keyring::Entry::new(SERVICE, secret.account()).and_then(|e| e.get_password()).ok()
}

#[cfg(windows)]
pub fn forget_all() {
    for secret in [Secret::Database, Secret::PostgresAdmin, Secret::Jwt] {
        if let Ok(e) = keyring::Entry::new(SERVICE, secret.account()) {
            let _ = e.delete_credential();
        }
    }
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
