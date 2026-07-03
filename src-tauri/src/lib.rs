//! Tauri command layer — thin shell over `sentient-installer-core`.
//! Phase 0: preflight checks. Phase 1: WSL2 provisioning + reboot-and-resume.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tauri::ipc::Channel;
use tauri::Manager;

use sentient_installer_core::checks::{self, Check};
use sentient_installer_core::distro;
use sentient_installer_core::progress::{Progress, ProgressFn};
use sentient_installer_core::wsl;

/// Run every preflight check and return the results for the UI. The checks shell
/// out to PowerShell/WSL (slow to spawn), so run them off the main thread to keep
/// the UI responsive.
#[tauri::command]
async fn preflight() -> Vec<Check> {
    tauri::async_runtime::spawn_blocking(checks::run_all)
        .await
        .unwrap_or_default()
}

#[derive(Serialize)]
pub struct WslResult {
    ready: bool,
    reboot_required: bool,
}

/// Install / enable WSL2, streaming progress to the webview. Returns whether WSL
/// is ready and whether a reboot is required to finish. Runs the (blocking, can
/// take minutes) work on a background thread so the UI stays responsive and the
/// progress channel delivers live — NOT on the main thread.
#[tauri::command]
async fn install_wsl(on_progress: Channel<Progress>) -> WslResult {
    let ch = on_progress;
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let sink: ProgressFn = Arc::new(move |p| {
            let _ = ch.send(p);
        });
        wsl::install(sink)
    })
    .await
    .expect("wsl install task panicked");
    WslResult {
        ready: outcome.ready,
        reboot_required: outcome.reboot_required,
    }
}

/// Is WSL functional right now? (used after a reboot to verify).
#[tauri::command]
async fn wsl_ready() -> bool {
    tauri::async_runtime::spawn_blocking(wsl::is_ready)
        .await
        .unwrap_or(false)
}

/// Import the `sentient` WSL distro and install Docker Engine, streaming
/// progress. Runs off the main thread.
#[tauri::command]
async fn setup_docker(app: tauri::AppHandle, on_progress: Channel<Progress>) -> Result<(), String> {
    let dir = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
    let ch = on_progress;
    tauri::async_runtime::spawn_blocking(move || {
        let sink: ProgressFn = Arc::new(move |p| {
            let _ = ch.send(p);
        });
        distro::setup(sink, &dir)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Is the distro up with Docker responding?
#[tauri::command]
async fn docker_ready() -> bool {
    tauri::async_runtime::spawn_blocking(distro::is_ready)
        .await
        .unwrap_or(false)
}

/// Deploy the SENTIENT stack, then register a logon task to start it on boot.
#[tauri::command]
async fn deploy_sentient(on_progress: Channel<Progress>) -> Result<(), String> {
    let ch = on_progress;
    let res = tauri::async_runtime::spawn_blocking(move || {
        let sink: ProgressFn = Arc::new(move |p| {
            let _ = ch.send(p);
        });
        distro::deploy(sink)
    })
    .await
    .map_err(|e| e.to_string())?;
    if res.is_ok() {
        let _ = arm_autostart();
    }
    res
}

/// Is the SENTIENT web server up?
#[tauri::command]
async fn sentient_running() -> bool {
    tauri::async_runtime::spawn_blocking(distro::is_running)
        .await
        .unwrap_or(false)
}

/// Open the SENTIENT web UI in the default browser.
#[tauri::command]
fn open_sentient() -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd")
            .args(["/c", "start", "", "http://localhost:8080"])
            .creation_flags(0x0800_0000)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Register a logon task that starts the distro + SENTIENT on boot (WSL2 distros
/// don't auto-start). Best-effort.
fn arm_autostart() -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("schtasks")
            .args([
                "/create",
                "/tn",
                "SENTIENT Autostart",
                "/tr",
                "wsl -d sentient -- docker compose -f /opt/sentient/docker-compose.yml up -d",
                "/sc",
                "onlogon",
                "/rl",
                "highest",
                "/f",
            ])
            .creation_flags(0x0800_0000)
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).into_owned())
        }
    }
    #[cfg(not(windows))]
    Ok(())
}

// ---- install-state persistence (survives reboots) ----------------------------

fn state_file(app: &tauri::AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_data_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("state.txt"))
}

/// The current step in the wizard, persisted so we resume after a reboot.
/// Defaults to "checks".
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

// ---- reboot & resume ---------------------------------------------------------

/// Register a one-shot entry so the installer relaunches after the next login,
/// to resume where it left off. No-op off Windows.
#[tauri::command]
fn arm_resume() -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let value = format!("\"{}\"", exe.display());
        let out = std::process::Command::new("reg")
            .args([
                "add",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\RunOnce",
                "/v",
                "SentientInstaller",
                "/t",
                "REG_SZ",
                "/d",
                &value,
                "/f",
            ])
            .creation_flags(0x0800_0000)
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).into_owned())
        }
    }
    #[cfg(not(windows))]
    Ok(())
}

/// Restart the machine (short delay). No-op off Windows.
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "linux")]
    for var in [
        "WEBKIT_DISABLE_DMABUF_RENDERER",
        "WEBKIT_DISABLE_COMPOSITING_MODE",
    ] {
        if std::env::var_os(var).is_none() {
            std::env::set_var(var, "1");
        }
    }

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            preflight,
            install_wsl,
            wsl_ready,
            setup_docker,
            docker_ready,
            deploy_sentient,
            sentient_running,
            open_sentient,
            get_state,
            set_state,
            arm_resume,
            reboot_now
        ])
        .run(tauri::generate_context!())
        .expect("error while running SENTIENT Platform Manager");
}
