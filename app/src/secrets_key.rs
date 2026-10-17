// Copyright © 2016-2026 The SENTIENT Authors

//! SENTIENT's secrets key travelling with backups.
//!
//! SENTIENT encrypts stored secrets (mail, SMS and integration passwords, AI
//! keys…) with a master key kept in a key file (SENTIENT's
//! dao/src/secrets.rs): one base64 key per line, the current key first, every
//! key able to decrypt. A database restored on another installation is
//! unreadable without the key it was made with, so:
//!
//! - a backup saves the key file beside the archive (`<backup>.sentient-key`);
//! - a restore notes on the restored database which key file goes with it
//!   (the one beside the backup), without needing administrator rights;
//! - putting the restored data into service installs that key when the
//!   running one cannot open the data: ahead of the running key, which stays
//!   in the file so the data kept from before still opens.

use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use sentient_backup_core::db::ConnConfig;
use sentient_backup_core::telemetry_move as mv;
use sentient_installer_core::distro::docker_ops;
use sentient_installer_core::progress::{Progress, ProgressFn};

use crate::installation::{Engine, InstallationRecord};

/// The key file saved beside a backup: `<backup>.sentient-key`.
pub fn beside(backup: &Path) -> PathBuf {
    backup.with_extension("sentient-key")
}

const KEY_CHECK_SETTING: &str = "secretsKeyCheck";
const KEY_CHECK_NAME: &str = "secrets-key-check";
const KEY_CHECK_TEXT: &str = "sentient-secrets-key-check";
/// What a restored database's comment starts with; the key file follows.
const COMMENT_PREFIX: &str = "SENTIENT Platform Manager restore; secrets key file: ";

enum Location {
    /// SENTIENT_SECRETS_KEY: set outside SENTIENT, not a file.
    Environment,
    Container(String),
    /// A root-only file on Linux.
    Linux(String),
    File(PathBuf),
}

/// Where this installation's SENTIENT keeps its key, by SENTIENT's rule:
/// SECRETS_KEY_FILE, else /var/lib/sentient when that folder exists (Docker,
/// native Linux), else its working directory (native Windows: the install
/// folder).
fn location(record: &InstallationRecord) -> Location {
    let env = |name: &str| record.stack.tuning.extra_env.iter().find(|e| e.name == name).map(|e| e.value.clone());
    if env("SENTIENT_SECRETS_KEY").is_some() {
        return Location::Environment;
    }
    let custom = env("SECRETS_KEY_FILE");
    match record.engine {
        Engine::Docker => Location::Container(custom.unwrap_or_else(|| "/var/lib/sentient/secrets.key".into())),
        Engine::Native if cfg!(windows) => {
            Location::File(custom.map(PathBuf::from).unwrap_or_else(|| record.install_dir.join("secrets.key")))
        }
        Engine::Native => Location::Linux(custom.unwrap_or_else(|| {
            if Path::new("/var/lib/sentient").is_dir() {
                "/var/lib/sentient/secrets.key".into()
            } else {
                record.install_dir.join("secrets.key").display().to_string()
            }
        })),
    }
}

/// This installation's key file; `None` when SENTIENT has none (its key is
/// set in the environment, or it has not started yet).
pub fn read(record: &InstallationRecord) -> Result<Option<String>, String> {
    match location(record) {
        Location::Environment => Ok(None),
        Location::Container(path) => docker_ops::read_app_file(&record.stack, &path),
        Location::Linux(path) => sentient_installer_core::native_linux::read_private_file(&path),
        Location::File(path) => match std::fs::read_to_string(&path) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("read {}: {e}", path.display())),
        },
    }
}

fn write(record: &InstallationRecord, text: &str, sink: &ProgressFn) -> Result<(), String> {
    match location(record) {
        Location::Environment => Err("SENTIENT's secrets key is set in its environment (SENTIENT_SECRETS_KEY)".into()),
        Location::Container(path) => docker_ops::write_app_file(&record.stack, &path, text),
        Location::Linux(path) => sentient_installer_core::native_linux::write_private_file(sink, &path, text),
        Location::File(path) => {
            let tmp = path.with_extension("key.spm-new");
            std::fs::write(&tmp, text).map_err(|e| format!("write {}: {e}", tmp.display()))?;
            std::fs::rename(&tmp, &path).map_err(|e| format!("replace {}: {e}", path.display()))
        }
    }
}

fn keys(text: &str) -> Vec<[u8; 32]> {
    text.lines()
        .filter_map(|l| base64::engine::general_purpose::STANDARD.decode(l.trim()).ok())
        .filter_map(|b| <[u8; 32]>::try_from(b).ok())
        .collect()
}

/// Whether any key in `key_file` opens a database's key check (the value
/// SENTIENT sealed with its key at first start).
pub fn opens(key_file: &str, check: &str) -> bool {
    let Ok(sealed) = base64::engine::general_purpose::STANDARD.decode(check) else { return false };
    if sealed.len() < 12 + 16 {
        return false;
    }
    let (nonce, body) = sealed.split_at(12);
    // Associated data: the system (nil) tenant id, then the name.
    let mut aad = vec![0u8; 16];
    aad.extend_from_slice(KEY_CHECK_NAME.as_bytes());
    keys(key_file).iter().any(|key| {
        Aes256Gcm::new_from_slice(key)
            .ok()
            .and_then(|c| c.decrypt(Nonce::from_slice(nonce), Payload { msg: body, aad: &aad }).ok())
            .is_some_and(|plain| plain == KEY_CHECK_TEXT.as_bytes())
    })
}

/// `first`'s keys, then those of `then` it does not have.
fn merged(first: &str, then: &str) -> String {
    let mut lines: Vec<&str> = first.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    for l in then.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if !lines.contains(&l) {
            lines.push(l);
        }
    }
    lines.iter().map(|l| format!("{l}\n")).collect()
}

fn block_on<F: std::future::Future>(future: F) -> Result<F::Output, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map(|rt| rt.block_on(future))
        .map_err(|e| format!("start the runtime: {e}"))
}

/// The key check stored in a database; `None` if SENTIENT never stored one.
fn check_value(pg: &ConnConfig) -> Result<Option<String>, String> {
    block_on(async {
        let client = mv::connect(pg).await.map_err(|e| e.to_string())?;
        let row = client
            .query_opt("SELECT json_value FROM admin_settings WHERE key = $1 LIMIT 1", &[&KEY_CHECK_SETTING])
            .await
            .map_err(|e| format!("read the secrets key check of {}: {e}", pg.dbname))?;
        Ok(row
            .and_then(|r| r.get::<_, Option<String>>(0))
            .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
            .and_then(|v| v.get("check").and_then(|c| c.as_str()).map(str::to_string)))
    })?
}

/// After a backup: the key file beside it. Never fails the backup; says why
/// when the key could not be saved.
pub fn save_beside(record: &InstallationRecord, backup: &Path, sink: &ProgressFn) {
    let log = |line: String| sink(Progress::Log { line });
    match read(record) {
        Ok(Some(text)) => {
            let path = beside(backup);
            let written = std::fs::write(&path, text).map_err(|e| e.to_string());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            }
            match written {
                Ok(()) => log(format!(
                    "SENTIENT's secrets key: {}. It opens the stored passwords in this backup; keep it with the backup, and apart from it when the backup leaves this computer.",
                    path.display()
                )),
                Err(e) => log(format!("SENTIENT's secrets key was not saved ({e}).")),
            }
        }
        Ok(None) => log("SENTIENT's secrets key is not in a key file here (set in its environment, or SENTIENT has not started yet); keep a copy of it yourself.".into()),
        Err(e) => log(format!("SENTIENT's secrets key was not saved ({e}).")),
    }
}

/// After a restore into `target` from `backup`: note the key file that goes
/// with it, when there is one that opens it. `key` is a key file the operator
/// chose; otherwise the one beside the backup.
pub fn after_restore(live: &ConnConfig, target: &str, backup: &Path, key: Option<&Path>, sink: &ProgressFn) -> Result<(), String> {
    let restored = ConnConfig { dbname: target.to_string(), ..live.clone() };
    let Some(check) = check_value(&restored)? else { return Ok(()) };
    let key_path = key.map(Path::to_path_buf).unwrap_or_else(|| beside(backup));
    let key_text = std::fs::read_to_string(&key_path).ok();
    if !key_text.as_deref().is_some_and(|k| opens(k, &check)) {
        sink(Progress::Log {
            line: if key_text.is_some() {
                format!("{} does not open this backup's stored passwords; if they do not open after the switch, use the key file the backup was made with.", key_path.display())
            } else {
                format!("No secrets key beside the backup ({}). If the backup comes from another installation, its stored passwords (mail, SMS, integrations…) open only with its key file.", key_path.display())
            },
        });
        return Ok(());
    }
    let path = std::fs::canonicalize(&key_path).unwrap_or(key_path);
    let comment = format!("{COMMENT_PREFIX}{}", path.display()).replace('\'', "''");
    block_on(async {
        let client = mv::connect(&ConnConfig { dbname: "postgres".into(), ..live.clone() }).await.map_err(|e| e.to_string())?;
        client
            .batch_execute(&format!("COMMENT ON DATABASE \"{}\" IS '{comment}'", target.replace('"', "\"\"")))
            .await
            .map_err(|e| e.to_string())
    })??;
    sink(Progress::Log {
        line: format!("The backup's secrets key ({}) goes with it when it is put into service.", path.display()),
    });
    Ok(())
}

/// The key file noted on a restored database by [`after_restore`].
fn noted_key(live: &ConnConfig, db: &str) -> Result<Option<PathBuf>, String> {
    block_on(async {
        let client = mv::connect(&ConnConfig { dbname: "postgres".into(), ..live.clone() }).await.map_err(|e| e.to_string())?;
        let row = client
            .query_opt("SELECT shobj_description(oid, 'pg_database') FROM pg_database WHERE datname = $1", &[&db])
            .await
            .map_err(|e| e.to_string())?;
        Ok(row
            .and_then(|r| r.get::<_, Option<String>>(0))
            .and_then(|c| c.strip_prefix(COMMENT_PREFIX).map(PathBuf::from)))
    })?
}

/// What the switch to restored data does with the key.
pub enum KeyChange {
    /// The running key opens the restored data (or there is nothing to open).
    None,
    /// The restored data's key goes in ahead of the running one; `previous`
    /// is the key file to put back if the switch is undone.
    Install { previous: String, new: String },
    /// No key file at hand opens the restored data.
    Missing,
}

/// Before the switch (SENTIENT still on its data): decide what to do with
/// the key for `restored`, a database not yet in service.
pub fn plan_switch(record: &InstallationRecord, live: &ConnConfig, restored: &str) -> Result<KeyChange, String> {
    let restored_conn = ConnConfig { dbname: restored.to_string(), ..live.clone() };
    let Some(check) = check_value(&restored_conn)? else { return Ok(KeyChange::None) };
    let Some(current) = read(record)? else { return Ok(KeyChange::None) };
    if opens(&current, &check) {
        return Ok(KeyChange::None);
    }
    let noted = noted_key(live, restored)?.and_then(|p| std::fs::read_to_string(p).ok());
    match noted.filter(|k| opens(k, &check)) {
        Some(key) => Ok(KeyChange::Install { new: merged(&key, &current), previous: current }),
        None => Ok(KeyChange::Missing),
    }
}

/// Write the key file for the switch (or to undo it).
pub fn install(record: &InstallationRecord, text: &str, sink: &ProgressFn) -> Result<(), String> {
    write(record, text, sink)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key check sealed by SENTIENT (dao/src/secrets.rs key_check_value)
    /// with the key below.
    fn sealed(key: &[u8; 32]) -> String {
        let nonce = [7u8; 12];
        let mut aad = vec![0u8; 16];
        aad.extend_from_slice(KEY_CHECK_NAME.as_bytes());
        let body = Aes256Gcm::new_from_slice(key)
            .unwrap()
            .encrypt(Nonce::from_slice(&nonce), Payload { msg: KEY_CHECK_TEXT.as_bytes(), aad: &aad })
            .unwrap();
        let mut out = nonce.to_vec();
        out.extend(body);
        base64::engine::general_purpose::STANDARD.encode(out)
    }

    #[test]
    fn any_key_in_the_file_opens_the_check() {
        let (a, b) = ([1u8; 32], [2u8; 32]);
        let enc = |k: &[u8; 32]| base64::engine::general_purpose::STANDARD.encode(k);
        let check = sealed(&b);
        assert!(!opens(&format!("{}\n", enc(&a)), &check));
        assert!(opens(&format!("{}\n{}\n", enc(&a), enc(&b)), &check));
        assert!(!opens("not base64\n", &check));
    }

    #[test]
    fn merged_keeps_order_without_duplicates() {
        assert_eq!(merged("k2\nk1\n", "k1\nk0\n"), "k2\nk1\nk0\n");
    }
}
