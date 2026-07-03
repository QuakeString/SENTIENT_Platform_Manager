//! Local persistence: SQLite (`sbr.db` in the app data dir) holds connection
//! profiles, backup/restore history, and settings. Passwords are NEVER stored in
//! SQLite — they live in the OS credential vault via `keyring` (Windows
//! Credential Manager / macOS Keychain / Linux Secret Service).

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

const KEYRING_SERVICE: &str = "com.quakestring.sentient-backup";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS connections (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,
  host TEXT NOT NULL,
  port INTEGER NOT NULL,
  dbname TEXT NOT NULL,
  username TEXT NOT NULL,
  has_password INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS backup_history (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts TEXT NOT NULL, host TEXT, dbname TEXT, output TEXT,
  archive_bytes INTEGER, sha256 TEXT, skipped TEXT, telemetry TEXT,
  status TEXT, message TEXT, duration_ms INTEGER
);
CREATE TABLE IF NOT EXISTS restore_history (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts TEXT NOT NULL, host TEXT, dbname TEXT, input TEXT,
  status TEXT, message TEXT, duration_ms INTEGER
);
CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT);
";

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn open(app: &AppHandle) -> Result<Connection, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let c = Connection::open(dir.join("sbr.db")).map_err(|e| e.to_string())?;
    c.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
    Ok(c)
}

// ---- Keyring (password vault) ------------------------------------------------
fn entry(id: i64) -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYRING_SERVICE, &format!("conn:{id}")).map_err(|e| e.to_string())
}
fn keyring_set(id: i64, pw: &str) -> Result<(), String> {
    entry(id)?.set_password(pw).map_err(|e| e.to_string())
}
fn keyring_get(id: i64) -> Result<Option<String>, String> {
    match entry(id)?.get_password() {
        Ok(p) => Ok(Some(p)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}
fn keyring_delete(id: i64) {
    if let Ok(e) = entry(id) {
        let _ = e.delete_credential();
    }
}

// ---- Connection profiles -----------------------------------------------------
#[derive(Serialize, Deserialize)]
pub struct ConnProfile {
    #[serde(default)]
    pub id: Option<i64>,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub dbname: String,
    pub username: String,
    #[serde(default)]
    pub has_password: bool,
    /// Only present on save input; never returned by `list_connections`.
    #[serde(default)]
    pub password: Option<String>,
}

#[tauri::command]
pub fn list_connections(app: AppHandle) -> Result<Vec<ConnProfile>, String> {
    let c = open(&app)?;
    let mut stmt = c
        .prepare("SELECT id,name,host,port,dbname,username,has_password FROM connections ORDER BY name")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(ConnProfile {
                id: Some(r.get(0)?),
                name: r.get(1)?,
                host: r.get(2)?,
                port: r.get::<_, i64>(3)? as u16,
                dbname: r.get(4)?,
                username: r.get(5)?,
                has_password: r.get::<_, i64>(6)? != 0,
                password: None,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct SaveResult {
    id: i64,
    /// Whether the password made it into the OS keychain. False when a password
    /// was given but the platform has no keychain available (e.g. a Linux box
    /// with no running Secret Service) — the profile is still saved.
    password_saved: bool,
}

#[tauri::command]
pub fn save_connection(app: AppHandle, profile: ConnProfile) -> Result<SaveResult, String> {
    let c = open(&app)?;
    let id = match profile.id {
        Some(id) => {
            c.execute(
                "UPDATE connections SET name=?1,host=?2,port=?3,dbname=?4,username=?5 WHERE id=?6",
                params![profile.name, profile.host, profile.port as i64, profile.dbname, profile.username, id],
            ).map_err(|e| e.to_string())?;
            id
        }
        None => {
            c.execute(
                "INSERT INTO connections (name,host,port,dbname,username,has_password,created_at) VALUES (?1,?2,?3,?4,?5,0,?6)",
                params![profile.name, profile.host, profile.port as i64, profile.dbname, profile.username, now()],
            ).map_err(|e| e.to_string())?;
            c.last_insert_rowid()
        }
    };
    // Try the OS keychain; degrade gracefully if unavailable so the profile
    // still saves. has_password reflects whether the password is really stored.
    let mut password_saved = false;
    if let Some(pw) = profile.password.filter(|p| !p.is_empty()) {
        password_saved = keyring_set(id, &pw).is_ok();
    }
    c.execute(
        "UPDATE connections SET has_password=?1 WHERE id=?2",
        params![password_saved as i64, id],
    ).map_err(|e| e.to_string())?;
    Ok(SaveResult { id, password_saved })
}

#[tauri::command]
pub fn delete_connection(app: AppHandle, id: i64) -> Result<(), String> {
    let c = open(&app)?;
    c.execute("DELETE FROM connections WHERE id=?1", params![id]).map_err(|e| e.to_string())?;
    keyring_delete(id);
    Ok(())
}

#[tauri::command]
pub fn get_connection_password(_app: AppHandle, id: i64) -> Result<Option<String>, String> {
    keyring_get(id)
}

// ---- History -----------------------------------------------------------------
#[derive(Serialize)]
pub struct BackupRow {
    id: i64, ts: String, host: String, dbname: String, output: String,
    archive_bytes: i64, telemetry: String, skipped: String, status: String, message: String,
}

#[derive(Serialize)]
pub struct RestoreRow {
    id: i64, ts: String, host: String, dbname: String, input: String, status: String, message: String,
}

#[allow(clippy::too_many_arguments)]
pub fn record_backup(app: &AppHandle, host: &str, dbname: &str, output: &str, bytes: i64,
                     sha: &str, skipped: &str, telemetry: &str, status: &str, message: &str, dur_ms: i64) {
    if let Ok(c) = open(app) {
        let _ = c.execute(
            "INSERT INTO backup_history (ts,host,dbname,output,archive_bytes,sha256,skipped,telemetry,status,message,duration_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![now(), host, dbname, output, bytes, sha, skipped, telemetry, status, message, dur_ms],
        );
    }
}

pub fn record_restore(app: &AppHandle, host: &str, dbname: &str, input: &str, status: &str, message: &str, dur_ms: i64) {
    if let Ok(c) = open(app) {
        let _ = c.execute(
            "INSERT INTO restore_history (ts,host,dbname,input,status,message,duration_ms) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![now(), host, dbname, input, status, message, dur_ms],
        );
    }
}

#[tauri::command]
pub fn list_backup_history(app: AppHandle) -> Result<Vec<BackupRow>, String> {
    let c = open(&app)?;
    let mut stmt = c.prepare(
        "SELECT id,ts,host,dbname,output,archive_bytes,telemetry,skipped,status,message
         FROM backup_history ORDER BY id DESC LIMIT 200").map_err(|e| e.to_string())?;
    let rows = stmt.query_map([], |r| Ok(BackupRow {
        id: r.get(0)?, ts: r.get(1)?, host: r.get(2).unwrap_or_default(), dbname: r.get(3).unwrap_or_default(),
        output: r.get(4).unwrap_or_default(), archive_bytes: r.get(5).unwrap_or(0),
        telemetry: r.get(6).unwrap_or_default(), skipped: r.get(7).unwrap_or_default(),
        status: r.get(8).unwrap_or_default(), message: r.get(9).unwrap_or_default(),
    })).map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_restore_history(app: AppHandle) -> Result<Vec<RestoreRow>, String> {
    let c = open(&app)?;
    let mut stmt = c.prepare(
        "SELECT id,ts,host,dbname,input,status,message FROM restore_history ORDER BY id DESC LIMIT 200")
        .map_err(|e| e.to_string())?;
    let rows = stmt.query_map([], |r| Ok(RestoreRow {
        id: r.get(0)?, ts: r.get(1)?, host: r.get(2).unwrap_or_default(), dbname: r.get(3).unwrap_or_default(),
        input: r.get(4).unwrap_or_default(), status: r.get(5).unwrap_or_default(), message: r.get(6).unwrap_or_default(),
    })).map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn clear_history(app: AppHandle) -> Result<(), String> {
    let c = open(&app)?;
    c.execute_batch("DELETE FROM backup_history; DELETE FROM restore_history;").map_err(|e| e.to_string())
}

// ---- Settings ----------------------------------------------------------------
#[tauri::command]
pub fn setting_get(app: AppHandle, key: String) -> Result<Option<String>, String> {
    let c = open(&app)?;
    c.query_row("SELECT value FROM settings WHERE key=?1", params![key], |r| r.get::<_, String>(0))
        .map(Some)
        .or_else(|e| if e == rusqlite::Error::QueryReturnedNoRows { Ok(None) } else { Err(e.to_string()) })
}

#[tauri::command]
pub fn setting_set(app: AppHandle, key: String, value: String) -> Result<(), String> {
    let c = open(&app)?;
    c.execute(
        "INSERT INTO settings (key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    ).map_err(|e| e.to_string())?;
    Ok(())
}
