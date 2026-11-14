// Copyright © 2016-2026 The SENTIENT Authors

//! `--open-sentient [--kiosk]`: what the sign-in entry, the kiosk desktop
//! shortcut and "Open SENTIENT" run. The browser opens at once on a small
//! local page (kiosk::STARTING_PAGE) that waits for SENTIENT and switches to
//! it as soon as it answers: after a reboot SENTIENT's services start a
//! little after sign-in, and a browser pointed at SENTIENT straight away
//! shows an error page that a kiosk cannot leave. A kiosk gets a browser
//! profile of its own, so it always opens its own full-screen window even
//! when the same browser is already open. Runs as the signed-in user: no
//! window of its own, no elevation.

use std::path::PathBuf;

use sentient_installer_core::kiosk;

use crate::installation::InstallationRecord;

/// This user's folder for the starting page and the kiosk's browser profile.
fn user_dir() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("SENTIENT");
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
        .unwrap_or_else(std::env::temp_dir)
        .join("sentient-platform-manager");
    base
}

pub fn run(args: &[String]) -> i32 {
    let kiosk_mode = args.iter().any(|a| a == "--kiosk");
    let port = InstallationRecord::load().map(|r| r.stack.http_port).unwrap_or(8080);
    let dir = user_dir();
    // Without the page (a read-only profile) SENTIENT itself is opened.
    let url = kiosk::starting_page(&dir, port).unwrap_or_else(|_| format!("http://localhost:{port}"));
    let spawned = match kiosk::detect() {
        Some((browser, exe)) => std::process::Command::new(exe)
            .args(browser.open_args(&url, kiosk_mode, Some(&dir.join("kiosk-browser"))))
            .spawn()
            .is_ok(),
        None if cfg!(windows) => std::process::Command::new("cmd").args(["/c", "start", "", &url]).spawn().is_ok(),
        None => std::process::Command::new("xdg-open").arg(&url).spawn().is_ok(),
    };
    if spawned { 0 } else { 1 }
}
