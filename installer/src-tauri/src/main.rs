// Copyright (c) 2016-2026 The SENTIENT Authors
//
// SENTIENT Platform Manager installer.
//
// Why this exists rather than an NSIS or Inno package: those draw their branding
// with a Win32 `StretchBlt` in COLORONCOLOR mode, which nearest-neighbour-scales
// the bitmaps by the display's DPI factor. At 125% scaling that duplicates every
// fourth column of pixels, and no source resolution survives it — verified with
// a 1px test pattern, which came back with runs of 2 and no intermediate greys.
// A WebView renders SVG through the real DPI pipeline, so the artwork is sharp
// at any scaling. The UI is HTML; the work below is plain Rust.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};

use serde::Serialize;

/// The Platform Manager binary, baked into this installer at build time so the
/// whole thing ships as one file with nothing to unpack alongside it.
#[cfg(windows)]
const PAYLOAD: &[u8] = include_bytes!("../payload/SENTIENT Platform Manager.exe");
#[cfg(not(windows))]
const PAYLOAD: &[u8] = b"";

const PRODUCT: &str = "SENTIENT Platform Manager";
const PUBLISHER: &str = "INVENIA SYSTEMS";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const ARP_KEY: &str =
    r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\SENTIENT Platform Manager";

#[derive(Serialize)]
struct Env {
    /// "install" (nothing here yet), "maintenance" (already installed, offer
    /// repair/update/uninstall) or "uninstall" (launched from Apps & features).
    mode: String,
    default_dir: String,
    install_dir: Option<String>,
    installed_version: Option<String>,
    /// Set when the installed copy is a different version from this one.
    is_update: bool,
    payload_mb: f64,
    version: String,
}

/// Apps & features calls us back with this flag.
fn launched_to_uninstall() -> bool {
    std::env::args().any(|a| a.eq_ignore_ascii_case("--uninstall"))
}

fn program_files() -> PathBuf {
    std::env::var("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(r"C:\Program Files"))
}

fn default_dir() -> PathBuf {
    program_files().join(PRODUCT)
}

/// Run a console tool without flashing a window — this is a GUI process.
fn quiet(program: &str) -> std::process::Command {
    #[allow(unused_mut)]
    let mut c = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

fn reg_read(value: &str) -> Option<String> {
    let out = quiet("reg.exe").args(["query", ARP_KEY, "/v", value]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find(|l| l.trim_start().starts_with(value))
        .and_then(|l| l.split_whitespace().last())
        .map(|s| s.to_string())
}

#[tauri::command]
fn environment() -> Env {
    let installed = reg_read("DisplayName").is_some();
    let installed_version = reg_read("DisplayVersion");
    let install_dir = reg_read("InstallLocation");
    let mode = if launched_to_uninstall() {
        "uninstall"
    } else if installed {
        "maintenance"
    } else {
        "install"
    };
    Env {
        mode: mode.into(),
        default_dir: install_dir
            .clone()
            .unwrap_or_else(|| default_dir().display().to_string()),
        install_dir,
        is_update: installed_version.as_deref().map(|v| v != VERSION).unwrap_or(false),
        installed_version,
        payload_mb: (PAYLOAD.len() as f64 / 1_048_576.0 * 10.0).round() / 10.0,
        version: VERSION.to_string(),
    }
}

#[tauri::command]
fn pick_folder(app: tauri::AppHandle, current: String) -> Option<String> {
    let _ = app;
    // Kept deliberately simple: PowerShell's folder browser avoids pulling the
    // dialog plugin (and its own dependency tree) into a single-purpose binary.
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; \
         $d = New-Object System.Windows.Forms.FolderBrowserDialog; \
         $d.SelectedPath = '{}'; \
         if ($d.ShowDialog() -eq 'OK') {{ $d.SelectedPath }}",
        current.replace('\'', "''")
    );
    let out = quiet("powershell.exe")
        .args(["-NoProfile", "-STA", "-Command", &script])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

#[derive(Serialize, Clone)]
struct Step {
    message: String,
    percent: u8,
}

fn emit(win: &tauri::WebviewWindow, message: &str, percent: u8) {
    use tauri::Emitter;
    let _ = win.emit("progress", Step { message: message.into(), percent });
}

#[tauri::command]
async fn install(
    window: tauri::WebviewWindow,
    dir: String,
    desktop_shortcut: bool,
    start_menu: bool,
) -> Result<String, String> {
    let dir = PathBuf::from(&dir);
    let exe = dir.join(format!("{PRODUCT}.exe"));

    emit(&window, "Preparing…", 5);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;

    // A running copy can't be overwritten; stop it rather than failing late.
    emit(&window, "Closing any running instance…", 12);
    let _ = quiet("taskkill").args(["/f", "/im", "sentient-manager-app.exe"]).output();
    let _ = quiet("taskkill").args(["/f", "/im", &format!("{PRODUCT}.exe")]).output();
    std::thread::sleep(std::time::Duration::from_millis(600));

    emit(&window, "Copying program files…", 30);
    std::fs::write(&exe, PAYLOAD).map_err(|e| format!("Could not write {}: {e}", exe.display()))?;

    // An offline bundle ships a `payload` folder beside this installer holding
    // PostgreSQL, TimescaleDB and the SENTIENT archive. Those are far too large
    // to embed in the executable, so they travel alongside and get copied in
    // where the Platform Manager looks for them.
    if let Some(src) = payload_beside_installer() {
        emit(&window, "Copying the offline payload (this is the large part)…", 42);
        copy_tree(&src, &dir.join("payload"))
            .map_err(|e| format!("Could not copy the offline payload: {e}"))?;
    }

    emit(&window, "Creating shortcuts…", 60);
    if desktop_shortcut {
        make_shortcut(&exe, r"C:\Users\Public\Desktop")?;
    }
    if start_menu {
        make_shortcut(&exe, r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs")?;
    }

    emit(&window, "Registering with Windows…", 80);
    register_uninstall(&dir, &exe)?;

    emit(&window, "Finishing…", 95);
    // Leave a copy of this installer behind as the uninstaller. Apps & features
    // then calls a real GUI with `--uninstall` instead of a .cmd that flashes a
    // console window at the user.
    if let Ok(me) = std::env::current_exe() {
        let target = dir.join("uninstall.exe");
        if me != target {
            std::fs::copy(&me, &target)
                .map_err(|e| format!("Could not write the uninstaller: {e}"))?;
        }
    }

    emit(&window, "Installed", 100);
    Ok(exe.display().to_string())
}

/// The offline payload folder shipped next to this installer, if present.
fn payload_beside_installer() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.join("payload");
    if dir.is_dir() && std::fs::read_dir(&dir).ok()?.next().is_some() {
        Some(dir)
    } else {
        None
    }
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("create {}: {e}", to.display()))?;
    for entry in std::fs::read_dir(from).map_err(|e| format!("read {}: {e}", from.display()))? {
        let entry = entry.map_err(|e| format!("entry: {e}"))?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_tree(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst).map_err(|e| format!("copy {}: {e}", src.display()))?;
        }
    }
    Ok(())
}

fn make_shortcut(target: &Path, folder: &str) -> Result<(), String> {
    let lnk = format!(r"{folder}\{PRODUCT}.lnk");
    let script = format!(
        "$w = New-Object -ComObject WScript.Shell; \
         $s = $w.CreateShortcut('{lnk}'); \
         $s.TargetPath = '{}'; \
         $s.WorkingDirectory = '{}'; \
         $s.Description = 'Install, run, update and back up the SENTIENT platform'; \
         $s.Save()",
        target.display(),
        target.parent().unwrap_or(Path::new("")).display()
    );
    let st = quiet("powershell.exe")
        .args(["-NoProfile", "-Command", &script])
        .status()
        .map_err(|e| format!("shortcut: {e}"))?;
    if st.success() { Ok(()) } else { Err(format!("Could not create the shortcut in {folder}")) }
}

fn register_uninstall(dir: &Path, exe: &Path) -> Result<(), String> {
    let size_kb = (PAYLOAD.len() / 1024).to_string();
    let uninstaller = dir.join("uninstall.exe");
    let values: Vec<(&str, &str, String)> = vec![
        ("DisplayName", "REG_SZ", PRODUCT.into()),
        ("DisplayVersion", "REG_SZ", VERSION.into()),
        ("Publisher", "REG_SZ", PUBLISHER.into()),
        ("InstallLocation", "REG_SZ", dir.display().to_string()),
        ("DisplayIcon", "REG_SZ", exe.display().to_string()),
        ("UninstallString", "REG_SZ", format!("\"{}\" --uninstall", uninstaller.display())),
        ("EstimatedSize", "REG_DWORD", size_kb),
        ("NoModify", "REG_DWORD", "1".into()),
        ("NoRepair", "REG_DWORD", "1".into()),
    ];
    for (name, kind, data) in values {
        let ok = quiet("reg.exe")
            .args(["add", ARP_KEY, "/v", name, "/t", kind, "/d", &data, "/f"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return Err(format!("Could not write the {name} registry value"));
        }
    }
    Ok(())
}

/// Remove the product: shortcuts, registry entry, then the program directory.
///
/// The uninstaller is running *from* that directory, so it cannot delete its own
/// file while executing. It schedules the last step instead — a detached `cmd`
/// that waits for this process to exit, then removes the folder.
#[tauri::command]
async fn uninstall(window: tauri::WebviewWindow, dir: String) -> Result<(), String> {
    let dir = PathBuf::from(&dir);

    emit(&window, "Closing the application…", 15);
    let _ = quiet("taskkill").args(["/f", "/im", &format!("{PRODUCT}.exe")]).output();
    std::thread::sleep(std::time::Duration::from_millis(600));

    emit(&window, "Removing shortcuts…", 40);
    for folder in [
        r"C:\Users\Public\Desktop",
        r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs",
    ] {
        let _ = std::fs::remove_file(format!(r"{folder}\{PRODUCT}.lnk"));
    }

    emit(&window, "Removing the registry entry…", 65);
    let _ = quiet("reg.exe").args(["delete", ARP_KEY, "/f"]).status();

    emit(&window, "Removing program files…", 85);
    // includes the offline payload, which is the bulk of the footprint
    // Delete everything except the running uninstaller, then hand the folder
    // itself to a detached cleanup that waits for us to exit.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.file_name().and_then(|n| n.to_str()) == Some("uninstall.exe") {
                continue;
            }
            let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
        }
    }
    let _ = quiet("cmd.exe")
        .args([
            "/c",
            &format!(
                "ping -n 4 127.0.0.1 >nul & rmdir /s /q \"{}\"",
                dir.display()
            ),
        ])
        .spawn();

    emit(&window, "Removed", 100);
    Ok(())
}

#[tauri::command]
fn launch(path: String) -> Result<(), String> {
    // Start it as the logged-in user, not elevated-as-installer, so the app runs
    // with the desktop's own token.
    quiet("cmd.exe")
        .args(["/c", "start", "", &path])
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn quit(app: tauri::AppHandle) {
    app.exit(0);
}

/// Windows 11 rounds standard window frames, but the WebView host window does
/// not pick it up on its own — ask DWM directly.
#[cfg(windows)]
fn round_corners(window: &tauri::WebviewWindow) {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE};
    const DWMWCP_ROUND: u32 = 2;
    if let Ok(handle) = window.hwnd() {
        let hwnd = handle.0 as HWND;
        let pref = DWMWCP_ROUND;
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE as u32,
                &pref as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<u32>() as u32,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Pre-flight: WebView2
// ---------------------------------------------------------------------------
//
// This installer draws its UI with WebView2, so it has to satisfy that
// dependency before opening a window — a Tauri window that fails to create
// gives the user nothing to act on, just a process that doesn't appear.
//
// The Rust process itself runs fine without the runtime; only webview creation
// needs it. So everything below happens before `tauri::Builder`, and speaks to
// the user through a plain Win32 message box, which needs nothing at all.

/// WebView2 Evergreen supports Windows 10 1607 (build 14393) and later.
/// Below that the runtime cannot be installed, and no amount of downloading
/// helps — the honest answer is that the OS is too old.
#[cfg(windows)]
const MIN_WINDOWS_BUILD: u32 = 14393;

#[cfg(windows)]
fn windows_build() -> u32 {
    reg_value(
        r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion",
        "CurrentBuildNumber",
    )
    .and_then(|s| s.trim().parse().ok())
    .unwrap_or(0)
}

/// Read one registry value via reg.exe, so we need no registry crate.
#[cfg(windows)]
fn reg_value(key: &str, value: &str) -> Option<String> {
    let out = quiet("reg.exe").args(["query", key, "/v", value]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find(|l| l.trim_start().starts_with(value))
        .and_then(|l| l.split_whitespace().last())
        .map(|s| s.to_string())
}

/// The Evergreen runtime registers its version under this GUID, per-machine or
/// per-user. Either satisfies us.
#[cfg(windows)]
fn webview2_version() -> Option<String> {
    const GUID: &str = "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
    let roots = [
        format!(r"HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{GUID}"),
        format!(r"HKLM\SOFTWARE\Microsoft\EdgeUpdate\Clients\{GUID}"),
        format!(r"HKCU\SOFTWARE\Microsoft\EdgeUpdate\Clients\{GUID}"),
    ];
    roots
        .iter()
        .filter_map(|k| reg_value(k, "pv"))
        .find(|v| !v.is_empty() && v != "0.0.0.0")
}

#[cfg(windows)]
fn message_box(text: &str, caption: &str, flags: u32) -> i32 {
    use windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW;
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            wide(text).as_ptr(),
            wide(caption).as_ptr(),
            flags,
        )
    }
}

/// Fetch and run the Evergreen bootstrapper. Small (~2 MB) but it needs the
/// network; an air-gapped machine has to be handled by telling the truth rather
/// than retrying.
#[cfg(windows)]
fn install_webview2() -> Result<(), String> {
    const URL: &str = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";
    let dest = std::env::temp_dir().join("MicrosoftEdgeWebview2Setup.exe");

    let ok = quiet("curl.exe")
        .args(["-L", "-f", "-s", "--retry", "2", "-o", &dest.display().to_string(), URL])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok || !dest.exists() {
        return Err(
            "Could not download the WebView2 runtime.\n\n             This computer may be offline or behind a proxy. You can install it              manually from https://go.microsoft.com/fwlink/p/?LinkId=2124703 on              another machine, then run this setup again."
                .into(),
        );
    }

    let status = quiet(&dest.display().to_string())
        .args(["/silent", "/install"])
        .status()
        .map_err(|e| format!("Could not run the WebView2 installer: {e}"))?;
    if !status.success() {
        return Err("The WebView2 runtime installer did not complete successfully.".into());
    }
    if webview2_version().is_none() {
        return Err("The WebView2 runtime still isn't registered after installation.".into());
    }
    Ok(())
}

/// Returns false when we should stop rather than open a window.
#[cfg(windows)]
fn preflight() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IDYES, MB_ICONERROR, MB_ICONQUESTION, MB_OK, MB_YESNO,
    };

    if webview2_version().is_some() {
        return true;
    }

    let build = windows_build();
    if build != 0 && build < MIN_WINDOWS_BUILD {
        message_box(
            &format!(
                "SENTIENT Platform Manager needs the Microsoft WebView2 runtime,                  which requires Windows 10 version 1607 (build {MIN_WINDOWS_BUILD}) or newer.\n\n                 This computer reports build {build}.\n\n                 Please update Windows, then run this setup again."
            ),
            "Windows version not supported",
            MB_OK | MB_ICONERROR,
        );
        return false;
    }

    let answer = message_box(
        "SENTIENT Platform Manager needs the Microsoft WebView2 runtime,          which isn't installed on this computer.\n\n         Setup can download and install it now (about 2 MB, requires an          internet connection).\n\nInstall it now?",
        "WebView2 runtime required",
        MB_YESNO | MB_ICONQUESTION,
    );
    if answer != IDYES {
        return false;
    }

    match install_webview2() {
        Ok(()) => true,
        Err(e) => {
            message_box(&e, "Could not install WebView2", MB_OK | MB_ICONERROR);
            false
        }
    }
}

fn main() {
    // Satisfy (or explain) the WebView2 dependency before any window exists.
    #[cfg(windows)]
    if !preflight() {
        return;
    }

    tauri::Builder::default()
        .setup(|app| {
            #[cfg(windows)]
            {
                use tauri::Manager;
                if let Some(w) = app.get_webview_window("main") {
                    round_corners(&w);
                }
            }
            let _ = app;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![environment, pick_folder, install, uninstall, launch, quit])
        .run(tauri::generate_context!())
        .expect("error while running the SENTIENT installer");
}
