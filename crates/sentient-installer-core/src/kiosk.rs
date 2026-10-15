//! Kiosk launcher: detect an installed browser (Chrome/Firefox), install
//! Chrome silently if none is found (Windows), and create Desktop + menu
//! shortcuts that open SENTIENT full-screen (`--kiosk`) in that browser. On
//! Linux the shortcuts are `.desktop` entries for whichever Chromium-family
//! browser or Firefox is on PATH. The
//! shortcut carries the SENTIENT icon (borrowed from the manager exe). Taskbar
//! pinning is intentionally not attempted — Windows 10/11 block it for
//! automation; the user can right-click → Pin to taskbar in one step.

use std::path::{Path, PathBuf};

use crate::progress::{Progress, ProgressFn};
#[cfg(windows)]
use crate::sys;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Browser {
    Chrome,
    Firefox,
}

impl Browser {
    pub fn label(self) -> &'static str {
        match self {
            Browser::Chrome => "Chrome",
            Browser::Firefox => "Firefox",
        }
    }
    /// Kiosk command-line arguments (the URL is appended by the caller).
    fn kiosk_args(self, port: u16) -> String {
        match self {
            Browser::Chrome => format!(
                "--kiosk --no-first-run --no-default-browser-check http://localhost:{port}"
            ),
            Browser::Firefox => format!("--kiosk http://localhost:{port}"),
        }
    }
}

/// Google's silent-install enterprise MSI.
#[cfg(windows)]
const CHROME_MSI: &str = "https://dl.google.com/dl/chrome/install/googlechromestandaloneenterprise64.msi";

/// The SENTIENT kiosk-shortcut icon, embedded so it's always available on disk.
#[cfg(windows)]
const KIOSK_ICO: &[u8] = include_bytes!("../assets/kiosk.ico");

#[cfg(windows)]
fn file(p: String) -> Option<PathBuf> {
    let pb = PathBuf::from(p);
    if pb.is_file() {
        Some(pb)
    } else {
        None
    }
}

#[cfg(windows)]
fn under(var: &str, rest: &str) -> Option<PathBuf> {
    let base = std::env::var_os(var)?;
    file(format!("{}\\{}", base.to_string_lossy(), rest))
}

/// The exe path recorded under `App Paths\<exe>` in the registry, if present.
#[cfg(windows)]
fn app_paths(exe: &str) -> Option<PathBuf> {
    let key = format!(
        r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{exe}"
    );
    let (ok, out, _) = sys::output("reg", &["query", &key, "/ve"])?;
    if !ok {
        return None;
    }
    for line in sys::decode(&out).lines() {
        if let Some(idx) = line.find("REG_SZ") {
            if let Some(p) = file(line[idx + 6..].trim().to_string()) {
                return Some(p);
            }
        }
    }
    None
}

/// Find an installed Chrome or Firefox. Prefers Chrome.
pub fn detect() -> Option<(Browser, PathBuf)> {
    #[cfg(windows)]
    {
        for rest in [
            r"Google\Chrome\Application\chrome.exe",
        ] {
            for var in ["PROGRAMFILES", "ProgramFiles(x86)", "LOCALAPPDATA"] {
                if let Some(p) = under(var, rest) {
                    return Some((Browser::Chrome, p));
                }
            }
        }
        if let Some(p) = app_paths("chrome.exe") {
            return Some((Browser::Chrome, p));
        }
        for var in ["PROGRAMFILES", "ProgramFiles(x86)"] {
            if let Some(p) = under(var, r"Mozilla Firefox\firefox.exe") {
                return Some((Browser::Firefox, p));
            }
        }
        if let Some(p) = app_paths("firefox.exe") {
            return Some((Browser::Firefox, p));
        }
        None
    }
    #[cfg(not(windows))]
    {
        for (bin, b) in [
            ("chromium", Browser::Chrome), ("chromium-browser", Browser::Chrome),
            ("google-chrome", Browser::Chrome), ("google-chrome-stable", Browser::Chrome),
            ("brave", Browser::Chrome), ("brave-browser", Browser::Chrome), ("microsoft-edge", Browser::Chrome),
            ("firefox", Browser::Firefox), ("firefox-esr", Browser::Firefox),
        ] {
            if let Some(p) = which(bin) {
                return Some((b, p));
            }
        }
        None
    }
}

#[cfg(not(windows))]
fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(bin)).find(|p| p.is_file())
}

/// Where a `.desktop` launcher goes for the current user: the applications
/// menu, and the Desktop folder when there is one.
#[cfg(not(windows))]
fn desktop_entry_paths() -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let data_home = std::env::var("XDG_DATA_HOME").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| format!("{home}/.local/share"));
    let mut v = vec![PathBuf::from(format!("{data_home}/applications/sentient-kiosk.desktop"))];
    let desktop = std::process::Command::new("xdg-user-dir").arg("DESKTOP").output().ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{home}/Desktop"));
    if Path::new(&desktop).is_dir() {
        v.push(PathBuf::from(format!("{desktop}/sentient-kiosk.desktop")));
    }
    v
}

/// A browser we can drive in kiosk mode — the existing one, or Chrome installed
/// on demand (the app runs elevated, so the MSI installs silently).
#[cfg(windows)]
fn ensure_browser(sink: &ProgressFn, work_dir: &Path) -> Result<(Browser, PathBuf), String> {
    if let Some(found) = detect() {
        sink(Progress::Log { line: format!("Using installed {}.", found.0.label()) });
        return Ok(found);
    }
    sink(Progress::Step { name: "Installing Google Chrome for the kiosk launcher".into() });
    std::fs::create_dir_all(work_dir).ok();
    let msi = work_dir.join("chrome_enterprise.msi");
    crate::distro::download(CHROME_MSI, &msi, sink)?;
    sink(Progress::Log { line: "Running the Chrome installer…".into() });
    let res = sys::output("msiexec", &["/i", &msi.to_string_lossy(), "/qn", "/norestart"]);
    let _ = std::fs::remove_file(&msi);
    // msiexec may return 3010 (success, reboot pending) which isn't success();
    // re-detect rather than trust the exit code.
    detect().ok_or_else(|| {
        let detail = res
            .map(|(_, _, e)| sys::decode(&e).trim().to_string())
            .unwrap_or_default();
        if detail.is_empty() {
            "Chrome could not be installed automatically.".into()
        } else {
            format!("Chrome could not be installed automatically: {detail}")
        }
    })
}

#[cfg(windows)]
fn ps_quote(s: &str) -> String {
    // single-quoted PowerShell string; escape embedded single quotes
    format!("'{}'", s.replace('\'', "''"))
}

/// Ensure a browser, then create Desktop + Start Menu kiosk shortcuts pointing at
/// `http://localhost:<port>`, using the embedded SENTIENT icon.
pub fn create_shortcut(sink: &ProgressFn, port: u16, work_dir: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        let (browser, exe) = ensure_browser(sink, work_dir)?;
        let args = browser.kiosk_args(port);

        // Drop the icon next to the app data so the shortcut always has it.
        std::fs::create_dir_all(work_dir).ok();
        let ico = work_dir.join("sentient-kiosk.ico");
        std::fs::write(&ico, KIOSK_ICO).map_err(|e| e.to_string())?;
        let icon_location = ico.to_string_lossy().to_string();

        sink(Progress::Step { name: "Creating the SENTIENT desktop shortcut".into() });
        std::fs::create_dir_all(work_dir).ok();
        let script = format!(
            r#"$ErrorActionPreference = 'Stop'
$ws = New-Object -ComObject WScript.Shell
$targets = @([Environment]::GetFolderPath('Desktop'), [Environment]::GetFolderPath('Programs'))
foreach ($dir in $targets) {{
  if (-not (Test-Path $dir)) {{ continue }}
  $lnk = Join-Path $dir 'SENTIENT.lnk'
  $s = $ws.CreateShortcut($lnk)
  $s.TargetPath = {exe}
  $s.Arguments = {args}
  $s.IconLocation = {icon}
  $s.Description = 'Launch SENTIENT (kiosk mode)'
  $s.WindowStyle = 3
  $s.Save()
  Write-Output ("shortcut: " + $lnk)
}}
"#,
            exe = ps_quote(&exe.to_string_lossy()),
            args = ps_quote(&args),
            icon = ps_quote(&icon_location),
        );
        let ps1 = work_dir.join("mk_kiosk_shortcut.ps1");
        std::fs::write(&ps1, &script).map_err(|e| e.to_string())?;

        let out = sys::output(
            "powershell",
            &["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File",
              &ps1.to_string_lossy()],
        );
        let _ = std::fs::remove_file(&ps1);
        match out {
            Some((true, o, _)) => {
                for line in sys::decode(&o).lines() {
                    let l = line.trim();
                    if !l.is_empty() {
                        sink(Progress::Log { line: l.into() });
                    }
                }
                sink(Progress::Done {
                    message: format!("Desktop shortcut created — opens SENTIENT in {}.", browser.label()),
                });
                Ok(())
            }
            Some((false, _, e)) => Err(format!("Could not create the shortcut: {}", sys::decode(&e).trim())),
            None => Err("Could not run PowerShell to create the shortcut.".into()),
        }
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = work_dir;
        let (browser, exe) = detect().ok_or_else(|| {
            "No supported browser found (Chromium, Chrome, Brave, Edge or Firefox). Install one and add the kiosk shortcut again.".to_string()
        })?;
        sink(Progress::Step { name: "Creating the SENTIENT desktop launcher".into() });
        let entry = format!(
            "[Desktop Entry]\nType=Application\nName=SENTIENT\nComment=Launch SENTIENT (kiosk mode)\n\
             Exec={} {}\nIcon=sentient-platform-manager\nTerminal=false\nCategories=Network;\nStartupNotify=true\n",
            exe.display(), browser.kiosk_args(port)
        );
        for path in desktop_entry_paths() {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).ok();
            }
            std::fs::write(&path, &entry).map_err(|e| format!("write {}: {e}", path.display()))?;
            // Desktop files on the Desktop folder need the exec bit for most
            // environments to offer "Launch" rather than "Untrusted".
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
            sink(Progress::Log { line: format!("launcher: {}", path.display()) });
        }
        sink(Progress::Done { message: format!("Desktop launcher created — opens SENTIENT in {}.", browser.label()) });
        Ok(())
    }
}

/// Delete the Desktop + Start Menu SENTIENT shortcuts (used by uninstall).
pub fn remove_shortcuts(sink: &ProgressFn, work_dir: &Path) {
    #[cfg(windows)]
    {
        let script = r#"foreach ($dir in @([Environment]::GetFolderPath('Desktop'), [Environment]::GetFolderPath('Programs'))) {
  $lnk = Join-Path $dir 'SENTIENT.lnk'
  if (Test-Path $lnk) { Remove-Item $lnk -Force -ErrorAction SilentlyContinue; Write-Output ("removed: " + $lnk) }
}"#;
        std::fs::create_dir_all(work_dir).ok();
        let ps1 = work_dir.join("rm_kiosk_shortcut.ps1");
        if std::fs::write(&ps1, script).is_ok() {
            if let Some((_, o, _)) = sys::output(
                "powershell",
                &["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File",
                  &ps1.to_string_lossy()],
            ) {
                for line in sys::decode(&o).lines() {
                    let l = line.trim();
                    if !l.is_empty() {
                        sink(Progress::Log { line: l.into() });
                    }
                }
            }
            let _ = std::fs::remove_file(&ps1);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = work_dir;
        for path in desktop_entry_paths() {
            if path.exists() && std::fs::remove_file(&path).is_ok() {
                sink(Progress::Log { line: format!("removed: {}", path.display()) });
            }
        }
    }
}
