// Copyright © 2016-2026 The SENTIENT Authors
//
//! File and folder choosers. Every "Choose…" in the manager and Setup comes
//! here, so all of them open the desktop's own dialog in the folder of the
//! current value.
//!
//! Linux: on KDE Plasma, kdialog (KDE's own chooser) when it is installed:
//! the portal answers with whatever backend the session is set up for, which
//! on a Plasma without xdg-desktop-portal-kde is the GTK one. Elsewhere the
//! FileChooser portal (through GPUI), and zenity when no portal answers.
//! Windows and macOS: the system dialog through GPUI.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;
use futures::channel::oneshot;
use gpui_kit::{App, PathPromptOptions};

/// What a chooser asks for.
pub struct Ask<'a> {
    pub title: &'a str,
    /// Open in this folder (or the folder of this file).
    pub start: Option<&'a Path>,
    pub directory: bool,
}

/// Choose one existing file or folder. Same answer shape as GPUI's
/// `prompt_for_paths`, so callers keep their code.
pub fn open(cx: &App, ask: Ask, options: PathPromptOptions) -> oneshot::Receiver<Result<Option<Vec<PathBuf>>>> {
    if let Some(mut cmd) = native_open(&ask) {
        let (tx, rx) = oneshot::channel();
        cx.background_executor()
            .spawn(async move {
                let got = cmd.output();
                let _ = tx.send(match got {
                    // 0: chosen; 1: cancelled; anything else: the tool failed.
                    Ok(o) if o.status.success() => Ok(first_line(&o.stdout).map(|p| vec![p])),
                    Ok(o) if o.status.code() == Some(1) => Ok(None),
                    Ok(o) => Err(anyhow::anyhow!("The file chooser failed: {}", String::from_utf8_lossy(&o.stderr).trim())),
                    Err(e) => Err(anyhow::anyhow!("The file chooser could not start: {e}")),
                });
            })
            .detach();
        return rx;
    }
    cx.prompt_for_paths(options)
}

/// Choose where to save a new file.
pub fn save(cx: &App, title: &str, directory: &Path, name: Option<&str>) -> oneshot::Receiver<Result<Option<PathBuf>>> {
    let start = directory.join(name.unwrap_or(""));
    if let Some(mut cmd) = native_save(title, &start) {
        let (tx, rx) = oneshot::channel();
        cx.background_executor()
            .spawn(async move {
                let got = cmd.output();
                let _ = tx.send(match got {
                    Ok(o) if o.status.success() => Ok(first_line(&o.stdout)),
                    Ok(o) if o.status.code() == Some(1) => Ok(None),
                    Ok(o) => Err(anyhow::anyhow!("The file chooser failed: {}", String::from_utf8_lossy(&o.stderr).trim())),
                    Err(e) => Err(anyhow::anyhow!("The file chooser could not start: {e}")),
                });
            })
            .detach();
        return rx;
    }
    cx.prompt_for_new_path(directory, name)
}

/// Where a chooser for `value` opens: its folder when it names a file or
/// folder that exists, else `fallback`.
pub fn start_for(value: &str, fallback: Option<PathBuf>) -> Option<PathBuf> {
    let p = PathBuf::from(value.trim());
    if !value.trim().is_empty() {
        if p.is_dir() {
            return Some(p);
        }
        if let Some(parent) = p.parent().filter(|d| d.is_dir()) {
            return Some(parent.to_path_buf());
        }
    }
    fallback.filter(|d| d.is_dir())
}

fn first_line(out: &[u8]) -> Option<PathBuf> {
    String::from_utf8_lossy(out).lines().next().map(str::trim).filter(|l| !l.is_empty()).map(PathBuf::from)
}

/// Which native tool to use here, if any.
#[derive(Debug, PartialEq, Eq)]
enum Tool {
    Kdialog,
    Zenity,
}

fn tool() -> Option<Tool> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_uppercase();
    pick_tool(&desktop, has("kdialog"), has("zenity"), portal_present())
}

/// KDE with kdialog: kdialog. Otherwise the portal; zenity only without one.
fn pick_tool(desktop: &str, kdialog: bool, zenity: bool, portal: bool) -> Option<Tool> {
    if desktop.split(':').any(|d| d == "KDE") && kdialog {
        return Some(Tool::Kdialog);
    }
    if !portal && zenity {
        return Some(Tool::Zenity);
    }
    None
}

fn has(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| std::env::split_paths(&paths).any(|d| d.join(program).is_file()))
}

/// Is a desktop portal on the session bus? (Asked once.)
fn portal_present() -> bool {
    static PRESENT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *PRESENT.get_or_init(|| {
        Command::new("busctl")
            .args(["--user", "--no-pager", "status", "org.freedesktop.portal.Desktop"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(true) // no busctl: assume a portal and let GPUI try
    })
}

fn native_open(ask: &Ask) -> Option<Command> {
    let start = ask.start.map(|p| p.display().to_string()).unwrap_or_default();
    match tool()? {
        Tool::Kdialog => {
            let mut c = Command::new("kdialog");
            c.arg("--title").arg(ask.title);
            if ask.directory {
                c.arg("--getexistingdirectory").arg(if start.is_empty() { home() } else { start });
            } else {
                c.arg("--getopenfilename").arg(if start.is_empty() { home() } else { format!("{start}/") });
            }
            Some(c)
        }
        Tool::Zenity => {
            let mut c = Command::new("zenity");
            c.args(["--file-selection", "--title", ask.title]);
            if ask.directory {
                c.arg("--directory");
            }
            if !start.is_empty() {
                c.arg(format!("--filename={start}/"));
            }
            Some(c)
        }
    }
}

fn native_save(title: &str, start: &Path) -> Option<Command> {
    match tool()? {
        Tool::Kdialog => {
            let mut c = Command::new("kdialog");
            c.arg("--title").arg(title).arg("--getsavefilename").arg(start);
            Some(c)
        }
        Tool::Zenity => {
            let mut c = Command::new("zenity");
            c.args(["--file-selection", "--save", "--confirm-overwrite", "--title", title]).arg(format!("--filename={}", start.display()));
            Some(c)
        }
    }
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kde_uses_kdialog_and_others_the_portal() {
        assert_eq!(pick_tool("KDE", true, true, true), Some(Tool::Kdialog));
        assert_eq!(pick_tool("ubuntu:GNOME", true, true, true), None);
        assert_eq!(pick_tool("KDE", false, true, true), None);
        assert_eq!(pick_tool("XFCE", false, true, false), Some(Tool::Zenity));
        assert_eq!(pick_tool("XFCE", false, false, false), None);
    }
}
