//! Tauri command layer for the SENTIENT Platform Manager — a thin shell over the
//! two engine crates. Installer side: preflight/WSL2/Docker/deploy + reboot-and-
//! resume. Backup side: inspect/backup/restore + connection store. Each engine
//! has its own `Progress` type, aliased here.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::Manager;

// installer engine
use sentient_installer_core::checks::{self, Check};
use sentient_installer_core::distro;
use sentient_installer_core::progress::{Progress as InstProgress, ProgressFn as InstProgressFn};
use sentient_installer_core::wsl;

// backup engine
use sentient_backup_core::backup::{self, BackupOptions, FileStoreSpec, Selection};
use sentient_backup_core::categories::catalog;
use sentient_backup_core::db::{build_report, CategoryReport, ConnConfig, DbInspector, ServerInfo};
use sentient_backup_core::files::{self, FileStoreStatus};
use sentient_backup_core::progress::{Progress as BkProgress, ProgressFn as BkProgressFn};
use sentient_backup_core::restore::{self, RestoreOptions};

mod store;

// ============================ INSTALLER SIDE =================================

#[tauri::command]
async fn preflight() -> Vec<Check> {
    tauri::async_runtime::spawn_blocking(checks::run_all).await.unwrap_or_default()
}

#[derive(Serialize)]
pub struct WslResult {
    ready: bool,
    reboot_required: bool,
}

#[tauri::command]
async fn install_wsl(on_progress: Channel<InstProgress>) -> WslResult {
    sentient_installer_core::cancel::reset();
    let ch = on_progress;
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let sink: InstProgressFn = Arc::new(move |p| {
            let _ = ch.send(p);
        });
        wsl::install(sink)
    })
    .await
    .expect("wsl install task panicked");
    WslResult { ready: outcome.ready, reboot_required: outcome.reboot_required }
}

/// Ask the running install step to stop: kills the in-flight child process and
/// sets a flag its loops check. The step's command then returns an error the
/// frontend treats as "cancelled".
#[tauri::command]
fn cancel_step() {
    sentient_installer_core::cancel::request();
}

/// Tear down a partial/cancelled deploy (containers, volumes, dangling images).
/// Leaves WSL + Docker in place so the install can be retried cleanly.
#[tauri::command]
async fn cleanup_install(on_progress: Channel<InstProgress>) -> Result<(), String> {
    sentient_installer_core::cancel::reset();
    let ch = on_progress;
    tauri::async_runtime::spawn_blocking(move || {
        let sink: InstProgressFn = Arc::new(move |p| {
            let _ = ch.send(p);
        });
        distro::cleanup(sink)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn wsl_ready() -> bool {
    tauri::async_runtime::spawn_blocking(wsl::is_ready).await.unwrap_or(false)
}

#[tauri::command]
async fn setup_docker(app: tauri::AppHandle, on_progress: Channel<InstProgress>) -> Result<(), String> {
    sentient_installer_core::cancel::reset();
    let dir = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
    let ch = on_progress;
    tauri::async_runtime::spawn_blocking(move || {
        let sink: InstProgressFn = Arc::new(move |p| {
            let _ = ch.send(p);
        });
        distro::setup(sink, &dir)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn docker_ready() -> bool {
    tauri::async_runtime::spawn_blocking(distro::is_ready).await.unwrap_or(false)
}

#[tauri::command]
async fn deploy_sentient(
    on_progress: Channel<InstProgress>,
    config: Option<distro::DeployConfig>,
) -> Result<(), String> {
    sentient_installer_core::cancel::reset();
    let cfg = config.unwrap_or_default();
    let ch = on_progress;
    let res = tauri::async_runtime::spawn_blocking(move || {
        let sink: InstProgressFn = Arc::new(move |p| {
            let _ = ch.send(p);
        });
        distro::deploy(sink, &cfg)
    })
    .await
    .map_err(|e| e.to_string())?;
    if res.is_ok() {
        let _ = arm_autostart();
    }
    res
}

#[tauri::command]
async fn sentient_running(port: Option<u16>) -> bool {
    let port = port.unwrap_or(8080);
    tauri::async_runtime::spawn_blocking(move || distro::is_running(port))
        .await
        .unwrap_or(false)
}

#[tauri::command]
fn open_sentient(port: Option<u16>) -> Result<(), String> {
    let port = port.unwrap_or(8080);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd")
            .args(["/c", "start", "", &format!("http://localhost:{port}")])
            .creation_flags(0x0800_0000)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(windows))]
    {
        let _ = port;
    }
    Ok(())
}

fn arm_autostart() -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("schtasks")
            .args([
                "/create", "/tn", "SENTIENT Autostart", "/tr",
                "wsl -d sentient -- docker compose -f /opt/sentient/docker-compose.yml up -d",
                "/sc", "onlogon", "/rl", "highest", "/f",
            ])
            .creation_flags(0x0800_0000)
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).into_owned()) }
    }
    #[cfg(not(windows))]
    Ok(())
}

// ---- manage the deployed stack (M3: Status + Update) -------------------------

#[tauri::command]
async fn stack_status() -> distro::StackStatus {
    tauri::async_runtime::spawn_blocking(distro::status)
        .await
        .unwrap_or(distro::StackStatus { installed: false, running: false, containers: Vec::new() })
}

#[tauri::command]
async fn stack_control(action: String, on_progress: Channel<InstProgress>) -> Result<(), String> {
    sentient_installer_core::cancel::reset();
    let ch = on_progress;
    tauri::async_runtime::spawn_blocking(move || {
        let sink: InstProgressFn = Arc::new(move |p| {
            let _ = ch.send(p);
        });
        distro::control(sink, &action)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn stack_logs(tail: Option<u32>) -> String {
    let tail = tail.unwrap_or(200);
    tauri::async_runtime::spawn_blocking(move || distro::logs(tail))
        .await
        .unwrap_or_default()
}

#[tauri::command]
async fn update_stack(on_progress: Channel<InstProgress>) -> Result<(), String> {
    sentient_installer_core::cancel::reset();
    let ch = on_progress;
    tauri::async_runtime::spawn_blocking(move || {
        let sink: InstProgressFn = Arc::new(move |p| {
            let _ = ch.send(p);
        });
        distro::update(sink)
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---- install-state persistence (survives reboots) ----------------------------

fn state_file(app: &tauri::AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_data_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("state.txt"))
}

#[tauri::command]
fn get_state(app: tauri::AppHandle) -> String {
    state_file(&app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "checks".into())
}

#[tauri::command]
fn set_state(app: tauri::AppHandle, step: String) -> Result<(), String> {
    let p = state_file(&app).ok_or("no data dir")?;
    std::fs::write(p, step).map_err(|e| e.to_string())
}

#[tauri::command]
fn arm_resume() -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let value = format!("\"{}\"", exe.display());
        let out = std::process::Command::new("reg")
            .args([
                "add", r"HKCU\Software\Microsoft\Windows\CurrentVersion\RunOnce",
                "/v", "SentientManager", "/t", "REG_SZ", "/d", &value, "/f",
            ])
            .creation_flags(0x0800_0000)
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).into_owned()) }
    }
    #[cfg(not(windows))]
    Ok(())
}

#[tauri::command]
fn reboot_now() -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("shutdown")
            .args(["/r", "/t", "5", "/c", "Restarting to finish WSL2 setup for SENTIENT"])
            .creation_flags(0x0800_0000)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ============================== BACKUP SIDE ==================================

#[derive(Serialize)]
pub struct InspectResult {
    server: ServerInfo,
    categories: Vec<CategoryReport>,
    total_bytes: i64,
    total_rows: i64,
    table_count: usize,
}

fn conn(host: String, port: u16, dbname: String, user: String, password: String) -> ConnConfig {
    ConnConfig { host, port, dbname, user, password }
}

fn bk_sink(ch: Channel<BkProgress>) -> BkProgressFn {
    Arc::new(move |p| {
        let _ = ch.send(p);
    })
}

#[tauri::command]
async fn inspect(host: String, port: u16, dbname: String, user: String, password: String) -> Result<InspectResult, String> {
    let db = DbInspector::connect(&conn(host, port, dbname, user, password)).await.map_err(|e| e.to_string())?;
    let server = db.server_info().await.map_err(|e| e.to_string())?;
    let tables = db.tables_with_true_sizes().await.map_err(|e| e.to_string())?;
    let categories = build_report(&tables);
    let total_bytes = categories.iter().map(|c| c.bytes).sum();
    let total_rows = categories.iter().map(|c| c.rows).sum();
    Ok(InspectResult { server, categories, total_bytes, total_rows, table_count: tables.len() })
}

#[derive(Deserialize)]
pub struct FileStoreArg {
    id: String,
    category_id: String,
    path: String,
}

#[derive(Serialize)]
pub struct BackupResult {
    output: String,
    archive_bytes: u64,
    dump_sha256: String,
    file_stores: usize,
}

#[tauri::command]
fn file_store_status() -> Vec<FileStoreStatus> {
    files::statuses()
}

#[tauri::command]
fn is_encrypted(path: String) -> Result<bool, String> {
    restore::is_encrypted(std::path::Path::new(&path)).map_err(|e| e.to_string())
}

#[tauri::command]
async fn backup(
    app: tauri::AppHandle,
    host: String, port: u16, dbname: String, user: String, password: String,
    output: String, skip: Vec<String>, telemetry_days: Option<u32>,
    file_stores: Vec<FileStoreArg>, passphrase: Option<String>,
    on_progress: Channel<BkProgress>,
) -> Result<BackupResult, String> {
    let (host_c, dbname_c, output_c) = (host.clone(), dbname.clone(), output.clone());
    let telemetry_label = if skip.iter().any(|s| s == "telemetry_historical") {
        "excluded".to_string()
    } else {
        match telemetry_days { Some(n) => format!("last {n}d"), None => "all".to_string() }
    };
    let skipped_label = skip.join(",");
    let mut selection = Selection::skipping(&skip);
    selection.telemetry_days = telemetry_days;
    let specs = file_stores
        .into_iter()
        .filter(|f| selection.is_included(&f.category_id))
        .map(|f| FileStoreSpec { id: f.id, category_id: f.category_id, path: PathBuf::from(f.path) })
        .collect();
    let opts = BackupOptions {
        output: PathBuf::from(output),
        selection,
        file_stores: specs,
        zstd_level: 10,
        passphrase: passphrase.filter(|p| !p.is_empty()),
    };
    let start = std::time::Instant::now();
    let result = backup::run(&conn(host, port, dbname, user, password), &opts, bk_sink(on_progress)).await;
    let dur = start.elapsed().as_millis() as i64;
    match result {
        Ok(s) => {
            let out = s.output.display().to_string();
            store::record_backup(&app, &host_c, &dbname_c, &out, s.archive_bytes as i64,
                &s.dump_sha256, &skipped_label, &telemetry_label, "success", "", dur);
            Ok(BackupResult { output: out, archive_bytes: s.archive_bytes, dump_sha256: s.dump_sha256, file_stores: s.file_stores })
        }
        Err(e) => {
            let msg = e.to_string();
            store::record_backup(&app, &host_c, &dbname_c, &output_c, 0, "",
                &skipped_label, &telemetry_label, "failed", &msg, dur);
            Err(msg)
        }
    }
}

#[derive(Serialize)]
pub struct RestoreResult {
    database: String,
}

#[tauri::command]
async fn restore(
    app: tauri::AppHandle,
    host: String, port: u16, dbname: String, user: String, password: String,
    input: String, allow_nonempty: bool, file_store_paths: Vec<(String, String)>,
    passphrase: Option<String>, on_progress: Channel<BkProgress>,
) -> Result<RestoreResult, String> {
    let (host_c, dbname_c, input_c) = (host.clone(), dbname.clone(), input.clone());
    let opts = RestoreOptions {
        input: PathBuf::from(input),
        allow_nonempty,
        file_store_paths: file_store_paths.into_iter().map(|(id, p)| (id, PathBuf::from(p))).collect(),
        passphrase: passphrase.filter(|p| !p.is_empty()),
    };
    let start = std::time::Instant::now();
    let result = restore::run(&conn(host, port, dbname, user, password), &opts, bk_sink(on_progress)).await;
    let dur = start.elapsed().as_millis() as i64;
    match result {
        Ok(s) => {
            store::record_restore(&app, &host_c, &dbname_c, &input_c, "success", "", dur);
            Ok(RestoreResult { database: s.database })
        }
        Err(e) => {
            let msg = e.to_string();
            store::record_restore(&app, &host_c, &dbname_c, &input_c, "failed", &msg, dur);
            Err(msg)
        }
    }
}

#[tauri::command]
async fn create_database(host: String, port: u16, dbname: String, user: String, password: String, name: String) -> Result<(), String> {
    sentient_backup_core::db::create_database(&conn(host, port, dbname, user, password), &name).await.map_err(|e| e.to_string())
}

#[tauri::command]
fn default_categories() -> serde_json::Value {
    serde_json::to_value(catalog()).unwrap_or(serde_json::Value::Null)
}

#[tauri::command]
async fn pick_save_path(app: tauri::AppHandle, default_name: String) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file()
        .add_filter("SENTIENT backup", &["sentient-backup"])
        .set_file_name(default_name)
        .save_file(move |p| { let _ = tx.send(p); });
    rx.await.ok().flatten().and_then(|fp| fp.into_path().ok()).map(|p| p.to_string_lossy().into_owned())
}

#[tauri::command]
async fn pick_open_path(app: tauri::AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file()
        .add_filter("SENTIENT backup", &["sentient-backup"])
        .pick_file(move |p| { let _ = tx.send(p); });
    rx.await.ok().flatten().and_then(|fp| fp.into_path().ok()).map(|p| p.to_string_lossy().into_owned())
}

// =============================================================================

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "linux")]
    for var in ["WEBKIT_DISABLE_DMABUF_RENDERER", "WEBKIT_DISABLE_COMPOSITING_MODE"] {
        if std::env::var_os(var).is_none() {
            std::env::set_var(var, "1");
        }
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // Point the backup engine at bundled pg client tools if present.
            let (dump_name, restore_name) = if cfg!(windows) {
                ("pg_dump.exe", "pg_restore.exe")
            } else {
                ("pg_dump", "pg_restore")
            };
            let mut dirs: Vec<PathBuf> = Vec::new();
            if let Ok(res) = app.path().resource_dir() {
                dirs.push(res.join("pgtools").join("bin"));
            }
            if let Ok(exe) = std::env::current_exe() {
                if let Some(d) = exe.parent() {
                    dirs.push(d.join("pgtools").join("bin"));
                }
            }
            for bin in dirs {
                let dump = bin.join(dump_name);
                let restore_p = bin.join(restore_name);
                if dump.exists() && restore_p.exists() {
                    std::env::set_var("SBR_PG_DUMP", &dump);
                    std::env::set_var("SBR_PG_RESTORE", &restore_p);
                    break;
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // installer
            preflight, install_wsl, wsl_ready, setup_docker, docker_ready,
            deploy_sentient, sentient_running, open_sentient,
            cancel_step, cleanup_install,
            stack_status, stack_control, stack_logs, update_stack,
            get_state, set_state, arm_resume, reboot_now,
            // backup
            inspect, backup, restore, create_database, default_categories,
            file_store_status, is_encrypted, pick_save_path, pick_open_path,
            store::list_connections, store::save_connection, store::delete_connection,
            store::get_connection_password, store::list_backup_history,
            store::list_restore_history, store::clear_history,
            store::setting_get, store::setting_set
        ])
        .run(tauri::generate_context!())
        .expect("error while running SENTIENT Platform Manager");
}
