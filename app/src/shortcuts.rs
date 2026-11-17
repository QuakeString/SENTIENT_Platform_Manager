// Copyright © 2016-2026 The SENTIENT Authors

//! Every file Setup and the Platform Manager create for shortcuts and for
//! starting at sign-in, kept in installation.json as a manifest, so that
//! uninstalling removes exactly those.
//!
//! The manifest holds absolute paths written by the account that made them.
//! That matters on native Linux: uninstalling runs as root (pkexec), whose
//! home is /root, so files under the operator's home were never found by
//! looking under `$HOME` (the kiosk icon stayed behind). Uninstalling also
//! removes the known default paths, for installs made before the manifest,
//! in the home of the account that asked (PKEXEC_UID / SUDO_UID) as well as
//! the current one, then refreshes the desktop's menu cache.
//!
//! Each file belongs to SENTIENT (the kiosk, the web shortcut, the sign-in
//! entry) or to the Platform Manager (its menu entry, icons, tray autostart).
//! Uninstalling SENTIENT removes SENTIENT's; removing the Platform Manager
//! removes its own.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Whose file it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Owner {
    Sentient,
    Manager,
}

/// One file made for a shortcut or for starting at sign-in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub path: PathBuf,
    pub owner: Owner,
}

/// What this process made so far (the install writes it into the record).
static NOTED: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

/// Note files this process made.
pub fn note(owner: Owner, paths: impl IntoIterator<Item = PathBuf>) {
    if let Ok(mut list) = NOTED.lock() {
        for path in paths {
            add(&mut list, Entry { path, owner });
        }
    }
}

/// The files noted so far.
pub fn noted() -> Vec<Entry> {
    NOTED.lock().map(|l| l.clone()).unwrap_or_default()
}

/// Add `e` to `list` once.
pub fn add(list: &mut Vec<Entry>, e: Entry) {
    if !list.iter().any(|x| x.path == e.path) {
        list.push(e);
    }
}

/// Note files made after the install (a kiosk shortcut renewed, the tray's
/// autostart) and add them to the record when this account may write it.
/// Never asks for a password: the default paths cover what is missed.
pub fn remember(owner: Owner, paths: Vec<PathBuf>) {
    note(owner, paths.clone());
    let Some(mut record) = crate::installation::InstallationRecord::load() else { return };
    let before = record.shortcuts.len();
    for path in paths {
        add(&mut record.shortcuts, Entry { path, owner });
    }
    if record.shortcuts.len() != before {
        if let Ok(json) = serde_json::to_string_pretty(&record) {
            let _ = std::fs::write(crate::installation::record_path(), json);
        }
    }
}

/// The manifest's files of `owner`.
pub fn of(list: &[Entry], owner: Owner) -> Vec<PathBuf> {
    list.iter().filter(|e| e.owner == owner).map(|e| e.path.clone()).collect()
}

// ---------------------------------------------------------------------------
// Known default paths (installs from before the manifest)
// ---------------------------------------------------------------------------

/// The Desktop folder of the account whose home is `home`, from its
/// `user-dirs.dirs` (readable as root, unlike `xdg-user-dir`'s answer).
#[cfg_attr(windows, allow(dead_code))]
pub fn desktop_dir(home: &Path, user_dirs: Option<&str>) -> PathBuf {
    let from_file = user_dirs.and_then(|t| {
        t.lines().find_map(|l| l.trim().strip_prefix("XDG_DESKTOP_DIR=")).map(|v| v.trim().trim_matches('"').replace("$HOME", &home.display().to_string()))
    });
    match from_file {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => home.join("Desktop"),
    }
}

/// The icon sizes the Platform Manager installs.
#[cfg_attr(windows, allow(dead_code))]
pub const ICON_SIZES: [&str; 10] = ["16x16", "22x22", "24x24", "32x32", "48x48", "64x64", "128x128", "256x256", "512x512", "scalable"];

/// Where Linux keeps `owner`'s files by default, for an account whose home
/// is `home` and whose Desktop is `desktop`, plus the system-wide ones.
/// `packaged`: the Platform Manager came from a package, which owns its
/// menu entry and icons (they are the package manager's to remove).
#[cfg_attr(windows, allow(dead_code))]
pub fn linux_defaults(owner: Owner, home: &Path, desktop: &Path, packaged: bool) -> Vec<PathBuf> {
    let apps = home.join(".local/share/applications");
    let mut v = Vec::new();
    match owner {
        Owner::Sentient => {
            v.push(apps.join("sentient-kiosk.desktop"));
            v.push(desktop.join("sentient-kiosk.desktop"));
            v.push(apps.join("sentient.desktop"));
            v.push(desktop.join("sentient.desktop"));
            v.push(home.join(".config/autostart/sentient-open.desktop"));
            v.push(PathBuf::from("/usr/share/applications/sentient.desktop"));
            v.push(PathBuf::from("/usr/share/applications/sentient-kiosk.desktop"));
            v.push(PathBuf::from("/etc/xdg/autostart/sentient-open.desktop"));
        }
        Owner::Manager => {
            v.push(apps.join("sentient-platform-manager.desktop"));
            v.push(desktop.join("sentient-platform-manager.desktop"));
            v.push(home.join(".config/autostart/sentient-platform-manager.desktop"));
            for size in ICON_SIZES {
                let ext = if size == "scalable" { "svg" } else { "png" };
                v.push(home.join(format!(".local/share/icons/hicolor/{size}/apps/sentient-platform-manager.{ext}")));
            }
            if !packaged {
                v.push(PathBuf::from("/usr/share/applications/sentient-platform-manager.desktop"));
                for size in ICON_SIZES {
                    let ext = if size == "scalable" { "svg" } else { "png" };
                    v.push(PathBuf::from(format!("/usr/share/icons/hicolor/{size}/apps/sentient-platform-manager.{ext}")));
                }
            }
        }
    }
    v
}

/// The homes to clean: this process's, and the account that started it
/// through pkexec or sudo (root's own home is /root).
#[cfg(unix)]
fn homes() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    if let Some(h) = std::env::var_os("HOME") {
        v.push(PathBuf::from(h));
    }
    for var in ["PKEXEC_UID", "SUDO_UID"] {
        if let Some(home) = std::env::var(var).ok().and_then(|uid| home_of(&uid)) {
            if !v.contains(&home) {
                v.push(home);
            }
        }
    }
    v
}

/// The account (name, home) of a uid, from the passwd database.
#[cfg(unix)]
fn account_of(uid: &str) -> Option<(String, PathBuf)> {
    let out = std::process::Command::new("getent").args(["passwd", uid]).output().ok()?;
    let line = String::from_utf8_lossy(&out.stdout).lines().next()?.to_string();
    let f: Vec<&str> = line.split(':').collect();
    (f.len() >= 6 && !f[5].is_empty()).then(|| (f[0].to_string(), PathBuf::from(f[5])))
}

#[cfg(unix)]
fn home_of(uid: &str) -> Option<PathBuf> {
    account_of(uid).map(|(_, h)| h)
}

/// A path under a home is the account's; the rest needs root.
#[cfg_attr(windows, allow(dead_code))]
pub fn is_system(path: &Path, homes: &[PathBuf]) -> bool {
    !homes.iter().any(|h| path.starts_with(h))
}

// ---------------------------------------------------------------------------
// Removal
// ---------------------------------------------------------------------------

/// Remove `owner`'s shortcut and sign-in files: those in `manifest`, then
/// the known defaults; then refresh the menus. Logs each file removed.
pub fn remove(owner: Owner, manifest: &[Entry], sink: &sentient_installer_core::progress::ProgressFn) {
    #[cfg(unix)]
    {
        let homes = homes();
        let packaged = crate::self_install::installed_by_package_manager();
        let mut paths = of(manifest, owner);
        for home in &homes {
            let user_dirs = std::fs::read_to_string(home.join(".config/user-dirs.dirs")).ok();
            let desktop = desktop_dir(home, user_dirs.as_deref());
            for p in linux_defaults(owner, home, &desktop, packaged) {
                if !paths.contains(&p) {
                    paths.push(p);
                }
            }
        }
        let (system, user): (Vec<PathBuf>, Vec<PathBuf>) = paths.into_iter().filter(|p| p.symlink_metadata().is_ok()).partition(|p| is_system(p, &homes));
        for p in user {
            if std::fs::remove_file(&p).is_ok() {
                sink(sentient_installer_core::progress::Progress::Log { line: format!("removed: {}", p.display()) });
            }
        }
        if !system.is_empty() {
            let list: Vec<String> = system.iter().map(|p| sentient_installer_core::privileged::shell_quote(&p.display().to_string())).collect();
            if sentient_installer_core::privileged::run(&sentient_installer_core::progress::noop(), &format!("rm -f {}", list.join(" "))).is_ok() {
                for p in &system {
                    sink(sentient_installer_core::progress::Progress::Log { line: format!("removed: {}", p.display()) });
                }
            }
        }
        refresh_menus(&homes);
    }
    #[cfg(windows)]
    {
        // The manifest's files first; the defaults are the shortcut folders
        // self_install and kiosk clean by name.
        for p in of(manifest, owner) {
            if std::fs::remove_file(&p).is_ok() {
                sink(sentient_installer_core::progress::Progress::Log { line: format!("removed: {}", p.display()) });
            }
        }
    }
}

/// The commands that rebuild the desktop's menu, those that exist: the
/// desktop-file database for each applications folder, KDE's sycoca, and
/// xdg-utils' menu refresh.
#[cfg(unix)]
pub fn menu_refresh_commands(homes: &[PathBuf], have: impl Fn(&str) -> bool) -> Vec<Vec<String>> {
    let mut v = Vec::new();
    if have("update-desktop-database") {
        v.push(vec!["update-desktop-database".into(), "-q".into(), "/usr/share/applications".into()]);
        for h in homes {
            let apps = h.join(".local/share/applications");
            v.push(vec!["update-desktop-database".into(), "-q".into(), apps.display().to_string()]);
        }
    }
    for k in ["kbuildsycoca6", "kbuildsycoca5"] {
        if have(k) {
            v.push(vec![k.into(), "--noincremental".into()]);
            break;
        }
    }
    if have("xdg-desktop-menu") {
        v.push(vec!["xdg-desktop-menu".into(), "forceupdate".into()]);
    }
    if have("gtk-update-icon-cache") {
        v.push(vec!["gtk-update-icon-cache".into(), "-q".into(), "-t".into(), "/usr/share/icons/hicolor".into()]);
    }
    v
}

/// Refresh the menus. As root, the per-account caches (KDE's sycoca) are
/// rebuilt as the account that asked, which is whose menu shows.
#[cfg(unix)]
fn refresh_menus(homes: &[PathBuf]) {
    let have = |b: &str| std::process::Command::new("sh").args(["-c", &format!("command -v {b}")]).output().is_ok_and(|o| o.status.success());
    let asker = ["PKEXEC_UID", "SUDO_UID"].iter().find_map(|v| std::env::var(v).ok()).and_then(|uid| account_of(&uid));
    for cmd in menu_refresh_commands(homes, have) {
        let per_user = cmd[0].starts_with("kbuildsycoca") || cmd[0] == "xdg-desktop-menu";
        let mut c = match (&asker, per_user) {
            (Some((name, home)), true) if crate::rootjob::is_root() => {
                let mut c = std::process::Command::new("runuser");
                c.args(["-u", name, "--", "env", &format!("HOME={}", home.display())]).args(&cmd);
                c
            }
            _ => {
                let mut c = std::process::Command::new(&cmd[0]);
                c.args(&cmd[1..]);
                c
            }
        };
        let _ = c.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_round_trips_in_the_record() {
        let mut record = crate::installation::tests::sample();
        add(&mut record.shortcuts, Entry { path: "/home/op/.local/share/applications/sentient-kiosk.desktop".into(), owner: Owner::Sentient });
        add(&mut record.shortcuts, Entry { path: "/home/op/.config/autostart/sentient-platform-manager.desktop".into(), owner: Owner::Manager });
        // Noted twice, kept once.
        add(&mut record.shortcuts, Entry { path: "/home/op/.local/share/applications/sentient-kiosk.desktop".into(), owner: Owner::Sentient });
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains(r#""owner":"sentient""#));
        let back: crate::installation::InstallationRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.shortcuts.len(), 2);
        assert_eq!(of(&back.shortcuts, Owner::Sentient), vec![PathBuf::from("/home/op/.local/share/applications/sentient-kiosk.desktop")]);
        assert_eq!(of(&back.shortcuts, Owner::Manager), vec![PathBuf::from("/home/op/.config/autostart/sentient-platform-manager.desktop")]);
        // A record from before the manifest reads with none.
        let old = crate::installation::tests::sample();
        let text = serde_json::to_string(&old).unwrap();
        assert!(!text.contains("shortcuts"));
        assert!(serde_json::from_str::<crate::installation::InstallationRecord>(&text).unwrap().shortcuts.is_empty());
    }

    #[test]
    fn desktop_folder_comes_from_user_dirs() {
        let home = Path::new("/home/op");
        assert_eq!(desktop_dir(home, Some("XDG_DESKTOP_DIR=\"$HOME/Schreibtisch\"\n")), PathBuf::from("/home/op/Schreibtisch"));
        assert_eq!(desktop_dir(home, None), PathBuf::from("/home/op/Desktop"));
    }

    #[test]
    fn defaults_split_by_owner() {
        let home = Path::new("/home/op");
        let s = linux_defaults(Owner::Sentient, home, &home.join("Desktop"), false);
        let m = linux_defaults(Owner::Manager, home, &home.join("Desktop"), false);
        let has = |v: &[PathBuf], p: &str| v.iter().any(|x| x == Path::new(p));
        assert!(has(&s, "/home/op/.local/share/applications/sentient-kiosk.desktop"));
        assert!(has(&s, "/home/op/Desktop/sentient-kiosk.desktop"));
        assert!(has(&s, "/etc/xdg/autostart/sentient-open.desktop"));
        assert!(!s.iter().any(|p| p.to_string_lossy().contains("platform-manager")));
        assert!(has(&m, "/home/op/.config/autostart/sentient-platform-manager.desktop"));
        assert!(has(&m, "/usr/share/applications/sentient-platform-manager.desktop"));
        assert!(has(&m, "/usr/share/icons/hicolor/48x48/apps/sentient-platform-manager.png"));
        assert!(has(&m, "/usr/share/icons/hicolor/scalable/apps/sentient-platform-manager.svg"));
        assert!(!m.iter().any(|p| p.to_string_lossy().contains("kiosk")));
        // A package owns its menu entry and icons.
        let pkg = linux_defaults(Owner::Manager, home, &home.join("Desktop"), true);
        assert!(!pkg.iter().any(|p| p.starts_with("/usr")));
        assert!(has(&pkg, "/home/op/.config/autostart/sentient-platform-manager.desktop"));
    }

    #[test]
    fn home_files_need_no_root() {
        let homes = vec![PathBuf::from("/home/op")];
        assert!(!is_system(Path::new("/home/op/Desktop/sentient-kiosk.desktop"), &homes));
        assert!(is_system(Path::new("/usr/share/applications/sentient.desktop"), &homes));
    }

    #[cfg(unix)]
    #[test]
    fn menus_refresh_with_whatever_exists() {
        let homes = vec![PathBuf::from("/home/op")];
        let kde = menu_refresh_commands(&homes, |b| b != "kbuildsycoca5");
        let firsts: Vec<&str> = kde.iter().map(|c| c[0].as_str()).collect();
        assert_eq!(firsts, ["update-desktop-database", "update-desktop-database", "kbuildsycoca6", "xdg-desktop-menu", "gtk-update-icon-cache"]);
        assert!(kde[1].contains(&"/home/op/.local/share/applications".to_string()));
        let bare = menu_refresh_commands(&homes, |_| false);
        assert!(bare.is_empty());
    }
}
