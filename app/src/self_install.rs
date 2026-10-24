// Copyright © 2016-2026 The SENTIENT Authors

//! Installing the Platform Manager itself, the part of setup that is not
//! SENTIENT: put this program where it lives, add shortcuts, and write the
//! installation record. One API, one implementation per OS:
//!
//! - Windows: `C:\Program Files\SENTIENT Platform Manager`, Start menu and
//!   desktop shortcuts; the engine's Apps & features entry points here.
//! - Linux: `/opt/sentient-platform-manager`, a command in `/usr/local/bin`,
//!   and application-menu entries. Writes there run as root through polkit
//!   (one password prompt per step).

pub const PRODUCT: &str = "SENTIENT Platform Manager";

/// What setup creates besides the program itself.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Shortcuts {
    pub autostart: bool,
    pub desktop_sentient: bool,
    pub desktop_manager: bool,
    pub start_menu: bool,
    pub kiosk: bool,
    /// Open SENTIENT full screen when someone signs in (operator screens).
    #[serde(default)]
    pub open_at_signin: bool,
}

pub use imp::*;

#[cfg(windows)]
mod imp {
    use super::{PRODUCT, Shortcuts};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    const DESKTOP: &str = r"C:\Users\Public\Desktop";
    const START_MENU: &str = r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs";

    pub fn manager_dir() -> PathBuf {
        PathBuf::from(r"C:\Program Files").join(PRODUCT)
    }

    pub fn manager_exe() -> PathBuf {
        manager_dir().join(format!("{PRODUCT}.exe"))
    }

    fn quiet(program: &str) -> Command {
        use std::os::windows::process::CommandExt;
        let mut c = Command::new(program);
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        c
    }

    /// Copy the running program to `manager_exe()`. Skipped when it already
    /// runs from there (repair from an installed copy).
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
    fn shortcut(target: &Path, folder: &str, name: &str, args: &str) -> Result<(), String> {
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
    fn web_shortcut(folder: &str, name: &str, url: &str) -> Result<(), String> {
        let path = format!(r"{folder}\{name}.url");
        std::fs::write(&path, format!("[InternetShortcut]\r\nURL={url}\r\n"))
            .map_err(|e| format!("write {path}: {e}"))
    }

    pub fn create_shortcuts(manager: &Path, url: &str, s: &Shortcuts) -> Result<(), String> {
        if s.start_menu {
            shortcut(manager, START_MENU, PRODUCT, "--manager")?;
            web_shortcut(START_MENU, "SENTIENT", url)?;
        }
        if s.desktop_manager {
            shortcut(manager, DESKTOP, PRODUCT, "--manager")?;
        }
        if s.desktop_sentient {
            web_shortcut(DESKTOP, "SENTIENT", url)?;
        }
        Ok(())
    }

    /// The manager's Start menu entry (repair puts it back).
    pub fn restore_menu_entry(manager: &Path) {
        let _ = shortcut(manager, START_MENU, PRODUCT, "--manager");
    }

    /// Shortcuts of a Platform Manager installed without SENTIENT.
    pub fn create_manager_shortcuts(manager: &Path, s: &Shortcuts) -> Result<(), String> {
        if s.start_menu {
            shortcut(manager, START_MENU, PRODUCT, "--manager")?;
        }
        if s.desktop_manager {
            shortcut(manager, DESKTOP, PRODUCT, "--manager")?;
        }
        Ok(())
    }

    const MANAGER_ARP_KEY: &str = r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\SENTIENT Platform Manager";

    /// The Apps & features entry of a Platform Manager installed alone.
    /// SENTIENT's own entry replaces it once SENTIENT is installed.
    pub fn register_manager_entry(manager: &Path) {
        let exe = manager.display().to_string();
        for (name, kind, data) in [
            ("DisplayName", "REG_SZ", PRODUCT.to_string()),
            ("DisplayVersion", "REG_SZ", env!("CARGO_PKG_VERSION").to_string()),
            ("Publisher", "REG_SZ", "INVENIA SYSTEMS".to_string()),
            ("DisplayIcon", "REG_SZ", exe.clone()),
            ("InstallLocation", "REG_SZ", manager_dir().display().to_string()),
            ("UninstallString", "REG_SZ", format!("\"{exe}\" --uninstall")),
            ("NoModify", "REG_DWORD", "1".to_string()),
            ("NoRepair", "REG_DWORD", "1".to_string()),
        ] {
            let _ = quiet("reg.exe").args(["add", MANAGER_ARP_KEY, "/v", name, "/t", kind, "/d", &data, "/f"]).output();
        }
    }

    pub fn remove_manager_entry() {
        let _ = quiet("reg.exe").args(["delete", MANAGER_ARP_KEY, "/f"]).output();
    }

    pub fn remove_shortcuts() {
        for folder in [DESKTOP, START_MENU] {
            let _ = std::fs::remove_file(format!(r"{folder}\{PRODUCT}.lnk"));
            let _ = std::fs::remove_file(format!(r"{folder}\SENTIENT.url"));
        }
    }

    /// Delete the manager's folder once this process has exited (it may be
    /// running from there).
    pub fn remove_manager_later() {
        // PowerShell rather than cmd: Rust escapes embedded quotes in a way
        // cmd does not read, which left the folder behind. It waits for this
        // process (which may be running from that folder) to exit first.
        let script = format!(
            "Wait-Process -Id {pid} -ErrorAction SilentlyContinue; Start-Sleep -Seconds 1; \
             Remove-Item -LiteralPath '{dir}' -Recurse -Force -ErrorAction SilentlyContinue",
            pid = std::process::id(),
            dir = manager_dir().display().to_string().replace('\'', "''"),
        );
        let _ = quiet("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", &script]).spawn();
    }

    /// Start with Windows (delayed automatic) or by hand.
    pub fn set_autostart(services: &[&str], automatic: bool) {
        let mode = if automatic { "delayed-auto" } else { "demand" };
        for service in services {
            let _ = quiet("sc.exe").args(["config", service, "start=", mode]).output();
        }
    }

    pub fn write_record(path: &Path, json: &str) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        std::fs::write(path, json).map_err(|e| format!("write {}: {e}", path.display()))
    }

    pub fn remove_record(path: &Path) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Shortcuts;
    use sentient_installer_core::privileged::{run, shell_quote as q};
    use sentient_installer_core::progress::noop;
    use std::path::{Path, PathBuf};

    const DIR: &str = "/opt/sentient-platform-manager";
    const BIN: &str = "/usr/local/bin/sentient-platform-manager";
    const APPS: &str = "/usr/share/applications";
    const ICON: &str = "/usr/share/icons/hicolor/256x256/apps/sentient-platform-manager.png";
    const ICON_PNG: &[u8] = include_bytes!("../assets/icon-256.png");

    pub fn manager_dir() -> PathBuf {
        PathBuf::from(DIR)
    }

    pub fn manager_exe() -> PathBuf {
        manager_dir().join("sentient-platform-manager")
    }

    /// Install this program, its command and its icon as root.
    pub fn install_manager() -> Result<PathBuf, String> {
        let me = std::env::current_exe().map_err(|e| format!("locate this program: {e}"))?;
        let target = manager_exe();
        if me == target {
            return Ok(target);
        }
        let icon = std::env::temp_dir().join("sentient-platform-manager.png");
        std::fs::write(&icon, ICON_PNG).map_err(|e| format!("write icon: {e}"))?;
        let script = format!(
            "set -e\n\
             install -Dm755 {me} {target}\n\
             ln -sf {target} {BIN}\n\
             install -Dm644 {icon} {ICON}\n",
            me = q(&me.display().to_string()),
            target = q(&target.display().to_string()),
            icon = q(&icon.display().to_string()),
        );
        run(&noop(), &script)?;
        let _ = std::fs::remove_file(&icon);
        Ok(target)
    }

    fn desktop_entry(name: &str, exec: &str, comment: &str) -> String {
        format!(
            "[Desktop Entry]\nType=Application\nName={name}\nComment={comment}\nExec={exec}\n\
             Icon=sentient-platform-manager\nTerminal=false\nCategories=System;Monitor;\n"
        )
    }

    pub fn create_shortcuts(manager: &Path, url: &str, s: &Shortcuts) -> Result<(), String> {
        if !s.start_menu && !s.desktop_sentient {
            return Ok(());
        }
        let mut script = String::from("set -e\n");
        if s.start_menu {
            script.push_str(&format!(
                "cat > {APPS}/sentient-platform-manager.desktop <<'EOF'\n{}EOF\n",
                desktop_entry(super::PRODUCT, &format!("{} --manager", manager.display()), "Run, update and back up SENTIENT")
            ));
        }
        if s.desktop_sentient || s.start_menu {
            script.push_str(&format!(
                "cat > {APPS}/sentient.desktop <<'EOF'\n{}EOF\n",
                desktop_entry("SENTIENT", &format!("xdg-open {url}"), "Open SENTIENT in the browser")
            ));
        }
        script.push_str("command -v update-desktop-database >/dev/null && update-desktop-database -q || true\n");
        run(&noop(), &script)
    }

    pub fn restore_menu_entry(manager: &Path) {
        let s = Shortcuts {
            autostart: true,
            desktop_sentient: false,
            desktop_manager: false,
            start_menu: true,
            kiosk: false,
            open_at_signin: false,
        };
        let _ = create_shortcuts(manager, "http://localhost:8080", &s);
    }

    /// The menu entry of a Platform Manager installed without SENTIENT.
    pub fn create_manager_shortcuts(manager: &Path, s: &Shortcuts) -> Result<(), String> {
        if !s.start_menu {
            return Ok(());
        }
        run(&noop(), &format!(
            "set -e\ncat > {APPS}/sentient-platform-manager.desktop <<'EOF'\n{}EOF\n\
             command -v update-desktop-database >/dev/null && update-desktop-database -q || true\n",
            desktop_entry(super::PRODUCT, &format!("{} --manager", manager.display()), "Run, update and back up SENTIENT")
        ))
    }

    /// Linux has no Apps & features list.
    pub fn register_manager_entry(_manager: &Path) {}

    pub fn remove_manager_entry() {}

    /// The pacman package that owns the program, when it came from one
    /// (`pacman -U sentient-platform-manager-*.pkg.tar.zst`). Its files are
    /// pacman's to remove, or the package database goes stale.
    fn pacman_package() -> Option<String> {
        let out = std::process::Command::new("pacman").arg("-Qqo").arg(manager_exe()).output().ok()?;
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (out.status.success() && !name.is_empty()).then_some(name)
    }

    pub fn remove_shortcuts() {
        // The package ships the menu entry and icon itself.
        let files = if pacman_package().is_some() {
            format!("{APPS}/sentient.desktop {BIN}")
        } else {
            format!("{APPS}/sentient-platform-manager.desktop {APPS}/sentient.desktop {ICON} {BIN}")
        };
        let _ = run(&noop(), &format!("rm -f {files}"));
    }

    pub fn remove_manager_later() {
        // A running Linux binary can be deleted; its inode lives on until exit.
        let script = match pacman_package() {
            Some(pkg) => format!("pacman -R --noconfirm {}", q(&pkg)),
            None => format!("rm -rf {DIR}"),
        };
        let _ = run(&noop(), &script);
    }

    /// systemd units and Docker restart policies already start with the
    /// machine; nothing to change.
    pub fn set_autostart(_services: &[&str], _automatic: bool) {}

    pub fn write_record(path: &Path, json: &str) -> Result<(), String> {
        // No password prompt when this user may write it anyway.
        if path.parent().is_some_and(|d| d.is_dir()) && std::fs::write(path, json).is_ok() {
            return Ok(());
        }
        let tmp = std::env::temp_dir().join("sentient-installation.json");
        std::fs::write(&tmp, json).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        let result = run(
            &noop(),
            &format!("install -Dm644 {} {}", q(&tmp.display().to_string()), q(&path.display().to_string())),
        );
        let _ = std::fs::remove_file(&tmp);
        result
    }

    pub fn remove_record(path: &Path) {
        let _ = run(&noop(), &format!("rm -f {}", q(&path.display().to_string())));
    }
}

/// Open SENTIENT when someone signs in (`Some(kiosk)`), or stop it. On
/// Windows SENTIENT's services are made to start with Windows as well:
/// otherwise the screen waits after a reboot for a SENTIENT nobody starts.
pub fn set_open_at_signin(sink: &sentient_installer_core::progress::ProgressFn, mode: Option<bool>) -> Result<(), String> {
    sentient_installer_core::kiosk::set_open_at_signin(sink, &manager_exe(), mode)?;
    #[cfg(windows)]
    if mode.is_some() {
        use sentient_installer_core::native::{APP_SERVICE, PG_SERVICE};
        set_autostart(&[APP_SERVICE, PG_SERVICE], true);
        sink(sentient_installer_core::progress::Progress::Log { line: "SENTIENT's services start with Windows.".into() });
    }
    Ok(())
}
