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
    if let Some((cmd, kind)) = native_open(&ask) {
        let (tx, rx) = oneshot::channel();
        cx.background_executor()
            .spawn(async move {
                let _ = tx.send(run(cmd, kind).map(|p| p.map(|p| vec![p])));
            })
            .detach();
        return rx;
    }
    cx.prompt_for_paths(options)
}

/// Choose where to save a new file.
pub fn save(cx: &App, title: &str, directory: &Path, name: Option<&str>) -> oneshot::Receiver<Result<Option<PathBuf>>> {
    if let Some((cmd, kind)) = native_save(title, directory, name) {
        let (tx, rx) = oneshot::channel();
        cx.background_executor()
            .spawn(async move {
                let _ = tx.send(run(cmd, kind));
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

/// Run a chooser tool: Ok(None) is a cancel.
fn run(mut cmd: Command, kind: Tool) -> Result<Option<PathBuf>> {
    let o = cmd.output().map_err(|e| anyhow::anyhow!(nf!("The file chooser could not start: {e}")))?;
    match kind {
        Tool::KdeService if o.status.success() => Ok(kde_answer(&String::from_utf8_lossy(&o.stdout))),
        // kdialog and zenity: 0 chosen, 1 cancelled.
        _ if o.status.success() => Ok(first_line(&o.stdout)),
        _ if o.status.code() == Some(1) && kind != Tool::KdeService => Ok(None),
        _ => Err(anyhow::anyhow!(nf!("The file chooser failed: {}", String::from_utf8_lossy(&o.stderr).trim()))),
    }
}

fn first_line(out: &[u8]) -> Option<PathBuf> {
    String::from_utf8_lossy(out).lines().next().map(str::trim).filter(|l| !l.is_empty()).map(PathBuf::from)
}

/// Which native tool to use here, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Kdialog,
    /// KDE's own file dialog service, asked directly on the session bus, for
    /// a Plasma whose desktop portal answers with the GTK dialog.
    KdeService,
    Zenity,
}

fn tool() -> Option<Tool> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_uppercase();
    pick_tool(&desktop, has("kdialog"), kde_service(), has("zenity"), portal_present())
}

/// KDE: kdialog, else KDE's dialog service. Otherwise the portal; zenity only
/// without one.
fn pick_tool(desktop: &str, kdialog: bool, kde_service: bool, zenity: bool, portal: bool) -> Option<Tool> {
    let kde = desktop.split(':').any(|d| d == "KDE");
    if kde && kdialog {
        return Some(Tool::Kdialog);
    }
    if kde && kde_service {
        return Some(Tool::KdeService);
    }
    if !portal && zenity {
        return Some(Tool::Zenity);
    }
    None
}

/// Is KDE's file dialog service installed (xdg-desktop-portal-kde)?
fn kde_service() -> bool {
    has("busctl")
        && ["/usr/share/dbus-1/services", "/usr/local/share/dbus-1/services"]
            .iter()
            .any(|d| Path::new(d).join("org.freedesktop.impl.portal.desktop.kde.service").is_file())
}

/// KDE's FileChooser, asked directly: (handle, app id, parent window, title,
/// options).
fn kde_service_command(method: &str, title: &str, directory: bool, start: Option<&Path>, name: Option<&str>) -> Command {
    let mut c = sentient_installer_core::host::host_command("busctl");
    c.args(["--user", "--timeout=86400", "call", "org.freedesktop.impl.portal.desktop.kde", "/org/freedesktop/portal/desktop", "org.freedesktop.impl.portal.FileChooser", method, "osssa{sv}"]);
    c.arg(format!("/org/freedesktop/portal/desktop/request/spm/r{}", std::process::id())).arg("").arg("").arg(title);
    // a{sv}: the number of entries, then key, type and value of each.
    let mut entries: Vec<Vec<String>> = vec![vec!["modal".into(), "b".into(), "true".into()]];
    if directory {
        entries.push(vec!["directory".into(), "b".into(), "true".into()]);
    }
    if let Some(d) = start.filter(|d| d.is_dir()) {
        // A byte string ending in NUL: ay, its length, its bytes.
        let bytes: Vec<u8> = d.display().to_string().into_bytes().into_iter().chain([0]).collect();
        let mut e = vec!["current_folder".to_string(), "ay".into(), bytes.len().to_string()];
        e.extend(bytes.iter().map(u8::to_string));
        entries.push(e);
    }
    if let Some(n) = name {
        entries.push(vec!["current_name".into(), "s".into(), n.into()]);
    }
    c.arg(entries.len().to_string());
    for e in entries {
        c.args(e);
    }
    c
}

/// The path in a FileChooser answer, `ua{sv} 0 1 "uris" as 1 "file:///…"`;
/// a response other than 0 is a cancel.
fn kde_answer(out: &str) -> Option<PathBuf> {
    if out.split_whitespace().nth(1)? != "0" {
        return None;
    }
    let start = out.find("\"file://")? + 1;
    let rest = &out[start..];
    let uri = &rest[..rest.find('"')?];
    Some(PathBuf::from(percent_decode(uri.trim_start_matches("file://"))))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn has(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| std::env::split_paths(&paths).any(|d| d.join(program).is_file()))
}

/// Is a desktop portal on the session bus? (Asked once.)
fn portal_present() -> bool {
    static PRESENT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *PRESENT.get_or_init(|| {
        sentient_installer_core::host::host_command("busctl")
            .args(["--user", "--no-pager", "status", "org.freedesktop.portal.Desktop"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(true) // no busctl: assume a portal and let GPUI try
    })
}

fn native_open(ask: &Ask) -> Option<(Command, Tool)> {
    let start = ask.start.map(|p| p.display().to_string()).unwrap_or_default();
    let tool = tool()?;
    let cmd = match tool {
        Tool::Kdialog => {
            let mut c = sentient_installer_core::host::host_command("kdialog");
            c.arg("--title").arg(ask.title);
            if ask.directory {
                c.arg("--getexistingdirectory").arg(if start.is_empty() { home() } else { start });
            } else {
                c.arg("--getopenfilename").arg(if start.is_empty() { home() } else { format!("{start}/") });
            }
            c
        }
        Tool::KdeService => kde_service_command("OpenFile", ask.title, ask.directory, ask.start, None),
        Tool::Zenity => {
            let mut c = sentient_installer_core::host::host_command("zenity");
            c.args(["--file-selection", "--title", ask.title]);
            if ask.directory {
                c.arg("--directory");
            }
            if !start.is_empty() {
                c.arg(format!("--filename={start}/"));
            }
            c
        }
    };
    Some((cmd, tool))
}

fn native_save(title: &str, directory: &Path, name: Option<&str>) -> Option<(Command, Tool)> {
    let start = directory.join(name.unwrap_or(""));
    let tool = tool()?;
    let cmd = match tool {
        Tool::Kdialog => {
            let mut c = sentient_installer_core::host::host_command("kdialog");
            c.arg("--title").arg(title).arg("--getsavefilename").arg(&start);
            c
        }
        Tool::KdeService => kde_service_command("SaveFile", title, false, Some(directory), name),
        Tool::Zenity => {
            let mut c = sentient_installer_core::host::host_command("zenity");
            c.args(["--file-selection", "--save", "--confirm-overwrite", "--title", title]).arg(format!("--filename={}", start.display()));
            c
        }
    };
    Some((cmd, tool))
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kde_uses_kdialog_and_others_the_portal() {
        assert_eq!(pick_tool("KDE", true, true, true, true), Some(Tool::Kdialog));
        assert_eq!(pick_tool("KDE", false, true, true, true), Some(Tool::KdeService));
        assert_eq!(pick_tool("ubuntu:GNOME", true, true, true, true), None);
        assert_eq!(pick_tool("KDE", false, false, true, true), None);
        assert_eq!(pick_tool("XFCE", false, false, true, false), Some(Tool::Zenity));
        assert_eq!(pick_tool("XFCE", false, false, false, false), None);
    }

    #[test]
    fn reads_the_kde_answer() {
        assert_eq!(kde_answer("ua{sv} 0 1 \"uris\" as 1 \"file:///home/ops/.ssh/id%20rsa\""), Some(PathBuf::from("/home/ops/.ssh/id rsa")));
        assert_eq!(kde_answer("ua{sv} 1 0"), None);
    }
}
