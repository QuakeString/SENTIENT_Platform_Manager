// Copyright © 2016-2026 The SENTIENT Authors

//! The Platform Manager installed without SENTIENT: install SENTIENT from a
//! package, or remove the Platform Manager again.

use std::path::{Path, PathBuf};

use gpui_kit::component::WindowExt as _;
use gpui_kit::*;

/// SENTIENT Setup in `dir`, an unpacked SENTIENT package.
fn setup_in(dir: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) { "SENTIENT-Setup.exe" } else { "SENTIENT-Setup" };
    Some(dir.join(name)).filter(|p| p.is_file())
}

/// Ask for an unpacked SENTIENT package and start its Setup.
pub fn install_sentient(window: &mut Window, cx: &mut App) {
    let handle = window.window_handle();
    let rx = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some("Choose the SENTIENT package folder".into()),
    });
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(paths))) = rx.await else { return };
        let Some(dir) = paths.into_iter().next() else { return };
        let message = match setup_in(&dir) {
            Some(setup) => match std::process::Command::new(&setup).arg("--install").current_dir(&dir).spawn() {
                Ok(_) => return,
                Err(e) => format!("Could not start {}: {e}", setup.display()),
            },
            None => format!(
                "{} is not a SENTIENT package: choose the unpacked folder that holds SENTIENT-Setup and payload.",
                dir.display()
            ),
        };
        let _ = cx.update_window(handle, |_, window, cx| window.push_notification(message, cx));
    })
    .detach();
}

/// Confirm, then remove the Platform Manager and close it.
pub fn remove_manager(window: &mut Window, cx: &mut App) {
    window.open_alert_dialog(cx, |dialog, _, _| {
        dialog
            .title("Remove the Platform Manager?")
            .description("Its shortcuts and the program are removed from this computer. SENTIENT is not installed, so nothing else changes.")
            .confirm()
            .ok_text("Remove")
            .on_ok(|_, _, cx| {
                cx.spawn(async move |cx| {
                    let sink = sentient_installer_core::progress::noop();
                    let _ = cx.background_spawn(async move { crate::maintenance::run_remove_manager(sink) }).await;
                    let _ = cx.update(|cx| cx.quit());
                })
                .detach();
                true
            })
    });
}
