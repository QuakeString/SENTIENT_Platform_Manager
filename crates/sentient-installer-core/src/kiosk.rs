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
    /// Arguments that open SENTIENT on `port`, full screen (`kiosk`) or in
    /// an ordinary window. Restore-session prompts are kept off: after a
    /// power cut the operator screen should simply show SENTIENT.
    pub fn launch_args(self, port: u16, kiosk: bool) -> Vec<String> {
        let url = format!("http://localhost:{port}");
        let mut a: Vec<String> = match self {
            Browser::Chrome => [
                "--no-first-run",
                "--no-default-browser-check",
                "--noerrdialogs",
                "--disable-session-crashed-bubble",
                "--hide-crash-restore-bubble",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            Browser::Firefox => Vec::new(),
        };
        a.push(if kiosk { "--kiosk".into() } else { "--new-window".into() });
        a.push(url);
        a
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

// ---------------------------------------------------------------------------
// Opening SENTIENT when someone signs in (operator screens)
// ---------------------------------------------------------------------------

/// The sign-in entry for every user: the all-users Startup folder on
/// Windows, the XDG autostart folder on Linux. It starts `launcher
/// --open-sentient [--kiosk]`, which waits for SENTIENT before opening the
/// browser (at boot the services are usually still starting).
#[cfg(not(windows))]
const XDG_AUTOSTART: &str = "/etc/xdg/autostart/sentient-open.desktop";
#[cfg(windows)]
const STARTUP_LNK: &str = "SENTIENT (operator screen).lnk";

#[cfg(windows)]
fn startup_lnk() -> Option<PathBuf> {
    let base = std::env::var_os("ProgramData")?;
    Some(PathBuf::from(base).join(r"Microsoft\Windows\Start Menu\Programs\StartUp").join(STARTUP_LNK))
}

/// Whether SENTIENT opens when someone signs in: `Some(true)` full screen,
/// `Some(false)` in a window, `None` not at all.
pub fn open_at_signin() -> Option<bool> {
    #[cfg(windows)]
    {
        let lnk = startup_lnk().filter(|p| p.exists())?;
        let script = format!("(New-Object -ComObject WScript.Shell).CreateShortcut({}).Arguments", ps_quote(&lnk.to_string_lossy()));
        let args = sys::output("powershell", &["-NoProfile", "-NonInteractive", "-Command", &script])
            .map(|(_, o, _)| sys::decode(&o))
            .unwrap_or_default();
        Some(args.contains("--kiosk"))
    }
    #[cfg(not(windows))]
    {
        std::fs::read_to_string(XDG_AUTOSTART).ok().map(|t| t.contains("--kiosk"))
    }
}

/// Make SENTIENT open when someone signs in (`Some(kiosk)`), or stop it
/// (`None`). `launcher` is the installed Platform Manager. Needs
/// administrator rights (a folder shared by every user).
pub fn set_open_at_signin(sink: &ProgressFn, launcher: &Path, mode: Option<bool>) -> Result<(), String> {
    let args = match mode {
        Some(true) => "--open-sentient --kiosk",
        Some(false) => "--open-sentient",
        None => "",
    };
    #[cfg(windows)]
    {
        let lnk = startup_lnk().ok_or("The all-users Startup folder could not be found.")?;
        if mode.is_none() {
            let _ = std::fs::remove_file(&lnk);
            sink(Progress::Done { message: "SENTIENT no longer opens when someone signs in.".into() });
            return Ok(());
        }
        let script = format!(
            "$ErrorActionPreference = 'Stop'\n\
             $s = (New-Object -ComObject WScript.Shell).CreateShortcut({lnk})\n\
             $s.TargetPath = {exe}\n$s.Arguments = {args}\n$s.WorkingDirectory = {dir}\n\
             $s.Description = 'Open SENTIENT when someone signs in'\n$s.Save()\n",
            lnk = ps_quote(&lnk.to_string_lossy()),
            exe = ps_quote(&launcher.to_string_lossy()),
            args = ps_quote(args),
            dir = ps_quote(&launcher.parent().unwrap_or(Path::new("C:\\")).to_string_lossy()),
        );
        match sys::output("powershell", &["-NoProfile", "-NonInteractive", "-Command", &script]) {
            Some((true, _, _)) => {}
            Some((false, _, e)) => return Err(format!("Could not create the sign-in shortcut: {}", sys::decode(&e).trim())),
            None => return Err("Could not run PowerShell to create the sign-in shortcut.".into()),
        }
    }
    #[cfg(not(windows))]
    {
        let script = match mode {
            None => format!("rm -f {XDG_AUTOSTART}\n"),
            Some(_) => format!(
                "mkdir -p /etc/xdg/autostart\ncat > {XDG_AUTOSTART} <<'SENTIENTEOF'\n[Desktop Entry]\nType=Application\nName=SENTIENT\n\
                 Comment=Open SENTIENT when someone signs in\nExec={} {args}\nIcon=sentient-platform-manager\nTerminal=false\n\
                 X-GNOME-Autostart-enabled=true\nSENTIENTEOF\nchmod 644 {XDG_AUTOSTART}\n",
                launcher.display()
            ),
        };
        crate::exec::run_privileged(sink, &script)?;
        if mode.is_none() {
            sink(Progress::Done { message: "SENTIENT no longer opens when someone signs in.".into() });
            return Ok(());
        }
    }
    sink(Progress::Done {
        message: format!(
            "SENTIENT opens {} when someone signs in, as soon as it answers.",
            if mode == Some(true) { "full screen" } else { "in a browser window" }
        ),
    });
    Ok(())
}
