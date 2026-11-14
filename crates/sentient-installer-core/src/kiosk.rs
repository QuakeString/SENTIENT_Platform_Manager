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
    /// Arguments that open `url`, full screen (`kiosk`) or in an ordinary
    /// window. Restore-session prompts are kept off: after a power cut the
    /// operator screen should simply show SENTIENT.
    ///
    /// A kiosk gets a browser profile of its own (`profile`): a browser that
    /// is already running with the user's profile would otherwise take the
    /// address into one of its existing windows instead of opening a full
    /// screen one.
    pub fn open_args(self, url: &str, kiosk: bool, profile: Option<&Path>) -> Vec<String> {
        let mut a: Vec<String> = Vec::new();
        match self {
            Browser::Chrome => {
                if kiosk {
                    a.push("--kiosk".into());
                    if let Some(dir) = profile {
                        a.push(format!("--user-data-dir={}", dir.display()));
                    }
                } else {
                    a.push("--new-window".into());
                }
                a.extend(
                    [
                        "--no-first-run",
                        "--no-default-browser-check",
                        "--noerrdialogs",
                        "--disable-session-crashed-bubble",
                        "--hide-crash-restore-bubble",
                    ]
                    .iter()
                    .map(|s| s.to_string()),
                );
            }
            Browser::Firefox => {
                if kiosk {
                    a.push("--kiosk".into());
                    if let Some(dir) = profile {
                        a.extend(["--new-instance".into(), "--profile".into(), dir.display().to_string()]);
                    }
                } else {
                    a.push("--new-window".into());
                }
            }
        }
        a.push(url.into());
        a
    }
}

/// The page shown while SENTIENT starts: it waits for SENTIENT and then
/// opens it (assets/starting.html).
pub const STARTING_PAGE: &str = include_str!("../assets/starting.html");

/// Write the starting page into `dir` and give its address for SENTIENT on
/// `port`: `file:///C:/Users/op/AppData/Local/SENTIENT/starting.html?port=8080`.
pub fn starting_page(dir: &Path, port: u16) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let page = dir.join("starting.html");
    std::fs::write(&page, STARTING_PAGE).map_err(|e| format!("write {}: {e}", page.display()))?;
    Ok(format!("{}?port={port}", file_url(&page)))
}

/// `C:\Users\a b\x.html` → `file:///C:/Users/a%20b/x.html`; `/home/a/x.html`
/// → `file:///home/a/x.html`.
fn file_url(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");
    if !s.starts_with('/') {
        out.push('/');
    }
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' | b':' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
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

/// Ensure a browser, then create Desktop + Start Menu kiosk shortcuts, with
/// the embedded SENTIENT icon. They run `launcher --open-sentient --kiosk`
/// (the Platform Manager), which opens the browser full screen on the
/// starting page: SENTIENT appears as soon as it answers, never an error page.
pub fn create_shortcut(sink: &ProgressFn, launcher: &Path, work_dir: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        let (browser, _) = ensure_browser(sink, work_dir)?;
        let exe = launcher.to_path_buf();
        let args = KIOSK_LAUNCH_ARGS.to_string();

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
$targets = @([Environment]::GetFolderPath('CommonDesktopDirectory'), [Environment]::GetFolderPath('CommonPrograms'))
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
        let (browser, _) = detect().ok_or_else(|| {
            "No supported browser found (Chromium, Chrome, Brave, Edge or Firefox). Install one and add the kiosk shortcut again.".to_string()
        })?;
        sink(Progress::Step { name: "Creating the SENTIENT desktop launcher".into() });
        let entry = format!(
            "[Desktop Entry]\nType=Application\nName=SENTIENT\nComment=Launch SENTIENT (kiosk mode)\n\
             Exec={} {}\nIcon=sentient-platform-manager\nTerminal=false\nCategories=Network;\nStartupNotify=true\n",
            launcher.display(), KIOSK_LAUNCH_ARGS
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

/// What the kiosk shortcut and the sign-in entry pass to the Platform
/// Manager (`--open-sentient`, see the app's open_client).
pub const KIOSK_LAUNCH_ARGS: &str = "--open-sentient --kiosk";

/// Whether the kiosk shortcut `create_shortcut` makes is on this computer:
/// tells an install from before the kiosk choice was recorded.
pub fn shortcut_exists() -> bool {
    #[cfg(windows)]
    {
        let mut paths = Vec::new();
        if let Some(p) = std::env::var_os("PUBLIC") {
            paths.push(PathBuf::from(p).join("Desktop").join("SENTIENT.lnk"));
        }
        if let Some(p) = std::env::var_os("ProgramData") {
            paths.push(PathBuf::from(p).join(r"Microsoft\Windows\Start Menu\Programs").join("SENTIENT.lnk"));
        }
        if let Some(p) = std::env::var_os("USERPROFILE") {
            paths.push(PathBuf::from(p).join("Desktop").join("SENTIENT.lnk"));
        }
        if let Some(p) = std::env::var_os("APPDATA") {
            paths.push(PathBuf::from(p).join(r"Microsoft\Windows\Start Menu\Programs").join("SENTIENT.lnk"));
        }
        paths.iter().any(|p| p.exists())
    }
    #[cfg(not(windows))]
    {
        desktop_entry_paths().iter().any(|p| p.exists())
    }
}

/// Remove what opening SENTIENT left in each account (used by uninstall):
/// the starting page and the kiosk's own browser profile (the app's
/// open_client writes them under the account's local app data).
pub fn remove_user_data(sink: &ProgressFn) {
    #[cfg(windows)]
    {
        let script = r#"foreach ($u in Get-ChildItem -Directory -Path (Split-Path $env:PUBLIC) -ErrorAction SilentlyContinue) {
  $d = Join-Path $u.FullName 'AppData\Local\SENTIENT'
  if (-not (Test-Path -LiteralPath $d)) { continue }
  foreach ($item in @('kiosk-browser', 'starting.html')) {
    $p = Join-Path $d $item
    if (Test-Path -LiteralPath $p) { Remove-Item -LiteralPath $p -Recurse -Force -ErrorAction SilentlyContinue; Write-Output ("removed: " + $p) }
  }
  if (-not (Get-ChildItem -LiteralPath $d -Force -ErrorAction SilentlyContinue)) { Remove-Item -LiteralPath $d -Force -ErrorAction SilentlyContinue }
}"#;
        if let Some((_, o, _)) = sys::output("powershell", &["-NoProfile", "-NonInteractive", "-Command", script]) {
            for line in sys::decode(&o).lines().map(str::trim).filter(|l| !l.is_empty()) {
                sink(Progress::Log { line: line.into() });
            }
        }
    }
    #[cfg(not(windows))]
    {
        let home = std::env::var("HOME").unwrap_or_default();
        let data_home = std::env::var("XDG_DATA_HOME").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| format!("{home}/.local/share"));
        let dir = PathBuf::from(data_home).join("sentient-platform-manager");
        for item in ["kiosk-browser", "starting.html"] {
            let p = dir.join(item);
            let removed = if p.is_dir() { std::fs::remove_dir_all(&p).is_ok() } else { std::fs::remove_file(&p).is_ok() };
            if removed {
                sink(Progress::Log { line: format!("removed: {}", p.display()) });
            }
        }
        let _ = std::fs::remove_dir(&dir);
    }
}

/// Delete the Desktop + Start Menu SENTIENT shortcuts (used by uninstall).
pub fn remove_shortcuts(sink: &ProgressFn, work_dir: &Path) {
    #[cfg(windows)]
    {
        // The shared folders (where they are made now), and every account's
        // own Desktop and Start menu: earlier managers put them there, for the
        // account Setup ran as (an administrator's, when elevated from
        // another). Only a shortcut this manager made is removed: SENTIENT.lnk
        // with its kiosk description.
        let script = r#"$ws = New-Object -ComObject WScript.Shell
$dirs = @([Environment]::GetFolderPath('CommonDesktopDirectory'), [Environment]::GetFolderPath('CommonPrograms'))
foreach ($u in Get-ChildItem -Directory -Path (Split-Path $env:PUBLIC) -ErrorAction SilentlyContinue) {
  $dirs += (Join-Path $u.FullName 'Desktop')
  $dirs += (Join-Path $u.FullName 'OneDrive\Desktop')
  $dirs += (Join-Path $u.FullName 'AppData\Roaming\Microsoft\Windows\Start Menu\Programs')
}
foreach ($dir in $dirs) {
  $lnk = Join-Path $dir 'SENTIENT.lnk'
  if (-not (Test-Path -LiteralPath $lnk)) { continue }
  if ($ws.CreateShortcut($lnk).Description -ne 'Launch SENTIENT (kiosk mode)') { continue }
  Remove-Item -LiteralPath $lnk -Force -ErrorAction SilentlyContinue
  Write-Output ("removed: " + $lnk)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kiosk_gets_its_own_full_screen_browser_instance() {
        let profile = Path::new("/home/op/.local/share/sentient/kiosk-browser");
        let chrome = Browser::Chrome.open_args("file:///x/starting.html?port=8080", true, Some(profile));
        assert_eq!(chrome[0], "--kiosk");
        assert!(chrome.contains(&"--user-data-dir=/home/op/.local/share/sentient/kiosk-browser".to_string()));
        assert_eq!(chrome.last().unwrap(), "file:///x/starting.html?port=8080");
        let firefox = Browser::Firefox.open_args("u", true, Some(profile));
        assert_eq!(firefox, ["--kiosk", "--new-instance", "--profile", "/home/op/.local/share/sentient/kiosk-browser", "u"]);
        // A browser window (not kiosk) uses the operator's own profile.
        let window = Browser::Chrome.open_args("u", false, Some(profile));
        assert_eq!(window[0], "--new-window");
        assert!(!window.iter().any(|a| a.starts_with("--user-data-dir")));
    }

    #[cfg(not(windows))]
    #[test]
    fn uninstall_removes_the_starting_page_and_kiosk_profile() {
        let base = std::env::temp_dir().join(format!("spm-userdata-{}", std::process::id()));
        let dir = base.join("sentient-platform-manager");
        std::fs::create_dir_all(dir.join("kiosk-browser").join("Default")).unwrap();
        std::fs::write(dir.join("starting.html"), "x").unwrap();
        std::env::set_var("XDG_DATA_HOME", &base);
        let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = lines.clone();
        let sink: ProgressFn = std::sync::Arc::new(move |p| {
            if let Progress::Log { line } = p {
                seen.lock().unwrap().push(line);
            }
        });
        remove_user_data(&sink);
        std::env::remove_var("XDG_DATA_HOME");
        assert!(!dir.exists());
        assert_eq!(lines.lock().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_starting_page_is_a_file_url_with_the_port() {
        assert_eq!(file_url(Path::new("/home/op/a b/starting.html")), "file:///home/op/a%20b/starting.html");
        assert_eq!(
            file_url(Path::new(r"C:\Users\op\AppData\Local\SENTIENT\starting.html")),
            "file:///C:/Users/op/AppData/Local/SENTIENT/starting.html"
        );
        let dir = std::env::temp_dir().join(format!("spm-starting-{}", std::process::id()));
        let url = starting_page(&dir, 9090).unwrap();
        assert!(url.ends_with("/starting.html?port=9090"));
        assert!(std::fs::read_to_string(dir.join("starting.html")).unwrap().contains("SENTIENT is starting"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
