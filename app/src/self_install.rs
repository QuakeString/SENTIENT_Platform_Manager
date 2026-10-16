// Copyright © 2016-2026 The SENTIENT Authors

//! Installing the Platform Manager itself, the part of setup that is not
//! SENTIENT: copy this program into Program Files, add shortcuts, and leave
//! the uninstall entry pointing at it. Ported from the former Tauri setup
//! program (`installer/src-tauri/src/main.rs`).

use std::path::{Path, PathBuf};
use std::process::Command;

pub const PRODUCT: &str = "SENTIENT Platform Manager";

/// Where the manager lives once installed. The engine's Apps & features
/// entry points its Uninstall button here.
pub fn manager_dir() -> PathBuf {
    PathBuf::from(r"C:\Program Files").join(PRODUCT)
}

pub fn manager_exe() -> PathBuf {
    manager_dir().join(format!("{PRODUCT}.exe"))
}

const DESKTOP: &str = r"C:\Users\Public\Desktop";
const START_MENU: &str = r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs";

fn quiet(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

/// Copy the running program to `manager_exe()`. Skipped when it already runs
/// from there (repair from an installed copy).
pub fn install_manager() -> Result<PathBuf, String> {
    let me = std::env::current_exe().map_err(|e| format!("locate this program: {e}"))?;
    let target = manager_exe();
    if me == target {
        return Ok(target);
    }
    std::fs::create_dir_all(manager_dir())
        .map_err(|e| format!("create {}: {e}", manager_dir().display()))?;
    // A running copy cannot be overwritten.
    let _ = quiet("taskkill").args(["/f", "/im", &format!("{PRODUCT}.exe")]).output();
    std::thread::sleep(std::time::Duration::from_millis(500));
    std::fs::copy(&me, &target).map_err(|e| format!("copy to {}: {e}", target.display()))?;
    Ok(target)
}

/// A `.lnk` to `target` named `name` in `folder`.
pub fn shortcut(target: &Path, folder: &str, name: &str, args: &str) -> Result<(), String> {
    let lnk = format!(r"{folder}\{name}.lnk");
    let esc = |p: &str| p.replace('\'', "''");
    let script = format!(
        "$w = New-Object -ComObject WScript.Shell; \
         $s = $w.CreateShortcut('{}'); \
         $s.TargetPath = '{}'; \
         $s.Arguments = '{}'; \
         $s.WorkingDirectory = '{}'; \
         $s.Save()",
        esc(&lnk),
        esc(&target.display().to_string()),
        esc(args),
        esc(&target.parent().unwrap_or(Path::new("")).display().to_string()),
    );
    let ok = quiet("powershell.exe")
        .args(["-NoProfile", "-Command", &script])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok { Ok(()) } else { Err(format!("Could not create the {name} shortcut")) }
}

/// An internet shortcut (`.url`) that opens SENTIENT in the default browser.
pub fn web_shortcut(folder: &str, name: &str, url: &str) -> Result<(), String> {
    let path = format!(r"{folder}\{name}.url");
    std::fs::write(&path, format!("[InternetShortcut]\r\nURL={url}\r\n"))
        .map_err(|e| format!("write {path}: {e}"))
}

pub fn desktop() -> &'static str {
    DESKTOP
}

pub fn start_menu() -> &'static str {
    START_MENU
}

/// `auto` (start with Windows) or `demand` (manual) for a service.
pub fn set_service_start(service: &str, automatic: bool) {
    let mode = if automatic { "delayed-auto" } else { "demand" };
    let _ = quiet("sc.exe").args(["config", service, "start=", mode]).output();
}
