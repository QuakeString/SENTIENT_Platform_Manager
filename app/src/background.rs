// Copyright © 2016-2026 The SENTIENT Authors

//! The Platform Manager's background part: one running copy per user that
//! keeps the tray (crate::tray) and the instance checks going while no
//! window is open.
//!
//! - One copy: the first one listens on 127.0.0.1 (a port and a token in
//!   the preferences folder, readable by this user only). A second start
//!   hands its request over (`show`, or `action <name>` from a taskbar
//!   action) and exits.
//! - Start at sign-in: an XDG autostart entry on Linux; on Windows a
//!   scheduled task at logon, unelevated: no UAC prompt at sign-in.
//! - Windows elevation (the same rule as Linux: only a user action asks):
//!   the tray runs unelevated. Opening the window or an action starts an
//!   elevated copy (`--takeover --request <request>`, one UAC prompt),
//!   which asks the running copy to hand over; that one confirms, removes
//!   its tray and quits, and the elevated copy becomes the running one.
//!   The lock records whether the running copy is elevated, so a later
//!   start forwards to it or takes over from it (see [`start_plan`]).
//! - Preferences (About): start at sign-in, show in the tray, closing the
//!   window quits. Without the tray there is no background part at all:
//!   closing the window quits and nothing starts at sign-in.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use futures::channel::mpsc::UnboundedSender;
use serde::{Deserialize, Serialize};

/// What the background part is asked to do (from the tray, a second start
/// or a taskbar action).
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Left click on the tray: open the window, or close it when it is open.
    Toggle,
    /// Open the window (a second start).
    Show,
    /// Open the window and do one thing (see [`ACTIONS`] and the tray's
    /// own `instance:<id>`, `notification:<id>`, `control:<verb>`).
    Action(String),
    /// Quit the Platform Manager (tray Exit, taskbar Exit).
    Exit,
    /// An elevated copy takes over: remove the tray and quit.
    Handover,
}

/// The taskbar actions (`--action <name>`): the Linux menu entry's Desktop
/// Actions and the Windows Jump List.
pub const ACTIONS: [(&str, &str); 6] = [
    ("open", "Open"),
    ("statistics", "Statistics"),
    ("notifications", "Notifications"),
    ("backup", "Back up this computer now"),
    ("check-updates", "Check for updates"),
    ("exit", "Exit"),
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Prefs {
    pub start_at_signin: bool,
    pub show_tray: bool,
    pub close_quits: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self { start_at_signin: true, show_tray: true, close_quits: false }
    }
}

fn prefs_path() -> PathBuf {
    crate::updates::prefs_dir().join("background.json")
}

impl Prefs {
    pub fn load() -> Self {
        std::fs::read_to_string(prefs_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let p = prefs_path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        std::fs::write(&p, serde_json::to_string_pretty(self).unwrap_or_default()).map_err(|e| format!("{}: {e}", p.display()))
    }

    /// The background part runs: the tray is on and closing does not quit.
    pub fn background(&self) -> bool {
        self.show_tray && !self.close_quits
    }
}

// ---------------------------------------------------------------------------
// One running copy
// ---------------------------------------------------------------------------

fn lock_path() -> PathBuf {
    crate::updates::prefs_dir().join("running.lock")
}

/// The request this start carries: `--request <text>` (a tray's handover),
/// `--action <name>`, else `show`.
pub fn request_of(args: &[String]) -> String {
    if let Some(r) = args.iter().position(|a| a == "--request").and_then(|i| args.get(i + 1)) {
        return r.clone();
    }
    match args.iter().position(|a| a == "--action").and_then(|i| args.get(i + 1)) {
        Some(a) => format!("action {a}"),
        None => "show".into(),
    }
}

/// The running copy as its lock says: port, token, and whether it runs
/// elevated. A lock from before the flag was an elevated copy.
pub fn parse_lock(text: &str) -> Option<(u16, String, bool)> {
    let mut parts = text.split_whitespace();
    let port = parts.next()?.parse::<u16>().ok()?;
    let token = parts.next()?.to_string();
    let elevated = parts.next().is_none_or(|f| f == "1");
    Some((port, token, elevated))
}

/// Whether a copy runs (its lock is there), and if so whether it is
/// elevated. A stale lock reads as running; [`forward`] then fails.
pub fn running() -> Option<bool> {
    parse_lock(&std::fs::read_to_string(lock_path()).ok()?).map(|(_, _, e)| e)
}

/// What a start does about the copy already running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartPlan {
    /// Hand the request to it and end.
    Forward,
    /// Ask it to hand over, then run as the one copy.
    TakeOver,
    /// Run as the one copy; `elevate`: ask for administrator rights first.
    Run { elevate: bool },
    /// Nothing to do (a sign-in start while a copy already runs).
    Exit,
}

/// The rule: only a user action elevates. `running`: a copy runs, and
/// whether elevated. `admin`: this start is elevated. `takeover`: a tray
/// started this copy elevated to hand over to it. `background`: started at
/// sign-in. `window_needs_admin`: the window must run elevated (Windows).
pub fn start_plan(running: Option<bool>, admin: bool, takeover: bool, background: bool, window_needs_admin: bool) -> StartPlan {
    match running {
        Some(_) if takeover => StartPlan::TakeOver,
        Some(_) if background => StartPlan::Exit,
        // An elevated start finds an unelevated tray: it becomes the copy.
        Some(false) if admin && window_needs_admin => StartPlan::TakeOver,
        // Otherwise the running copy takes the request (an unelevated tray
        // elevates itself for it).
        Some(_) => StartPlan::Forward,
        None if background => StartPlan::Run { elevate: false },
        None => StartPlan::Run { elevate: window_needs_admin && !admin },
    }
}

/// Ask the running copy to hand over to this one, and wait (up to 10 s)
/// until it has let go. False when it did not answer.
pub fn take_over() -> bool {
    if !forward("takeover") {
        return false;
    }
    for _ in 0..50 {
        if !lock_path().exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

/// Hand `request` to the copy already running. True: it took it, and this
/// start ends here.
pub fn forward(request: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(lock_path()) else { return false };
    let Some((port, token, _)) = parse_lock(&text) else { return false };
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(400)) else { return false };
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    if writeln!(s, "{token} {request}").is_err() {
        return false;
    }
    let mut answer = String::new();
    BufReader::new(s).read_line(&mut answer).is_ok() && answer.trim() == "ok"
}

/// Listen for later starts; their requests go to `tx`. Returns false when
/// it could not listen (then every start is a copy of its own).
pub fn listen(tx: UnboundedSender<Command>) -> bool {
    let Ok(listener) = TcpListener::bind(("127.0.0.1", 0)) else { return false };
    let Ok(port) = listener.local_addr().map(|a| a.port()) else { return false };
    let token = format!("{:032x}", rand_u128());
    let path = lock_path();
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let elevated = if crate::elevate::is_admin() { 1 } else { 0 };
    if std::fs::write(&path, format!("{port} {token} {elevated}\n")).is_err() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    std::thread::Builder::new()
        .name("spm-single-instance".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut line = String::new();
                let mut reader = BufReader::new(&stream);
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let mut it = line.trim().splitn(2, ' ');
                if it.next() != Some(token.as_str()) {
                    continue;
                }
                let cmd = match it.next().unwrap_or("show") {
                    "show" => Command::Show,
                    "takeover" => Command::Handover,
                    r => match r.strip_prefix("action ") {
                        Some("exit") => Command::Exit,
                        Some("open") | None => Command::Show,
                        Some(a) => Command::Action(a.to_string()),
                    },
                };
                let _ = (&stream).write_all(b"ok\n");
                if tx.unbounded_send(cmd).is_err() {
                    break;
                }
            }
        })
        .is_ok()
}

/// Remove the lock when this copy ends (a stale one is harmless: the next
/// start cannot connect and becomes the running copy).
pub fn release() {
    let _ = std::fs::remove_file(lock_path());
}

fn rand_u128() -> u128 {
    use std::hash::{BuildHasher, Hasher};
    let mut v = 0u128;
    for _ in 0..2 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        h.write_u32(std::process::id());
        v = (v << 64) | h.finish() as u128;
    }
    v
}

// ---------------------------------------------------------------------------
// Start at sign-in
// ---------------------------------------------------------------------------

/// Start the background part at sign-in, or stop doing so.
pub fn set_autostart(on: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let r = imp::set_autostart(on, &exe);
    #[cfg(not(windows))]
    if on && r.is_ok() {
        crate::shortcuts::remember(crate::shortcuts::Owner::Manager, imp::entry().into_iter().collect());
    }
    AUTOSTART.store(if r.is_ok() { on } else { imp::present() }, std::sync::atomic::Ordering::Relaxed);
    r
}

static AUTOSTART: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the system really starts the manager at sign-in (what About's
/// switch shows): the entry on Linux; on Windows what the last change left.
pub fn autostart_on() -> bool {
    if cfg!(windows) { AUTOSTART.load(std::sync::atomic::Ordering::Relaxed) } else { imp::present() }
}

#[cfg(not(windows))]
mod imp {
    use std::path::{Path, PathBuf};

    pub(super) fn entry() -> Option<PathBuf> {
        let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(config.join("autostart").join("sentient-platform-manager.desktop"))
    }

    pub fn present() -> bool {
        entry().and_then(|p| std::fs::read_to_string(p).ok()).is_some_and(|t| t.contains("--background"))
    }

    pub fn set_autostart(on: bool, exe: &Path) -> Result<(), String> {
        let path = entry().ok_or("no home folder")?;
        if !on {
            let _ = std::fs::remove_file(&path);
            return Ok(());
        }
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        // The installed command when there is one (stays valid across updates).
        let cmd = if Path::new("/usr/bin/sentient-platform-manager").exists() {
            "/usr/bin/sentient-platform-manager".to_string()
        } else if Path::new("/usr/local/bin/sentient-platform-manager").exists() {
            "/usr/local/bin/sentient-platform-manager".to_string()
        } else {
            exe.display().to_string()
        };
        let text = format!(
            "[Desktop Entry]\nType=Application\nName=SENTIENT Platform Manager\nComment=Tray and checks of the SENTIENT instances\nExec={cmd} --manager --background\nIcon=sentient-platform-manager\nTerminal=false\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n"
        );
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

#[cfg(windows)]
mod imp {
    use std::os::windows::process::CommandExt;
    use std::path::Path;

    const TASK: &str = "SENTIENT Platform Manager (background)";
    const NO_WINDOW: u32 = 0x0800_0000;

    pub fn present() -> bool {
        std::process::Command::new("schtasks.exe").args(["/Query", "/TN", TASK]).creation_flags(NO_WINDOW).output().is_ok_and(|o| o.status.success())
    }

    pub fn set_autostart(on: bool, exe: &Path) -> Result<(), String> {
        let out = if on {
            let tr = format!("\"{}\" --manager --background", exe.display());
            std::process::Command::new("schtasks.exe")
                // Unelevated: the tray asks for administrator rights only
                // when the user opens the window or starts an action.
                .args(["/Create", "/TN", TASK, "/SC", "ONLOGON", "/RL", "LIMITED", "/TR", &tr, "/F"])
                .creation_flags(NO_WINDOW)
                .output()
        } else {
            std::process::Command::new("schtasks.exe").args(["/Delete", "/TN", TASK, "/F"]).creation_flags(NO_WINDOW).output()
        }
        .map_err(|e| e.to_string())?;
        if out.status.success() || !on {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lock_says_whether_the_running_copy_is_elevated() {
        assert_eq!(parse_lock("41234 abc 0\n"), Some((41234, "abc".into(), false)));
        assert_eq!(parse_lock("41234 abc 1\n"), Some((41234, "abc".into(), true)));
        // A lock from before the flag: that copy was elevated.
        assert_eq!(parse_lock("41234 abc\n"), Some((41234, "abc".into(), true)));
        assert_eq!(parse_lock("garbage"), None);
    }

    #[test]
    fn only_a_user_action_elevates_on_windows() {
        use StartPlan::*;
        // Sign-in start, nothing running: an unelevated tray, no UAC.
        assert_eq!(start_plan(None, false, false, true, true), Run { elevate: false });
        // A sign-in start while a copy runs: nothing to do.
        assert_eq!(start_plan(Some(false), false, false, true, true), Exit);
        // The menu entry with nothing running: the window asks for UAC.
        assert_eq!(start_plan(None, false, false, false, true), Run { elevate: true });
        assert_eq!(start_plan(None, true, false, false, true), Run { elevate: false });
        // The menu entry while the unelevated tray runs: the tray takes the
        // request (and elevates itself for it).
        assert_eq!(start_plan(Some(false), false, false, false, true), Forward);
        // The tray's elevated copy, or any elevated start: takes over.
        assert_eq!(start_plan(Some(false), true, true, false, true), TakeOver);
        assert_eq!(start_plan(Some(false), true, false, false, true), TakeOver);
        // An elevated copy runs: everything goes to it.
        assert_eq!(start_plan(Some(true), false, false, false, true), Forward);
        assert_eq!(start_plan(Some(true), true, false, false, true), Forward);
        // The tray went away before its elevated copy started.
        assert_eq!(start_plan(None, true, true, false, true), Run { elevate: false });
    }

    #[test]
    fn linux_never_elevates_the_window() {
        use StartPlan::*;
        assert_eq!(start_plan(None, true, false, false, false), Run { elevate: false });
        assert_eq!(start_plan(Some(true), true, false, false, false), Forward);
    }

    #[test]
    fn a_handover_carries_the_request() {
        let args: Vec<String> = ["spm", "--manager", "--takeover", "--request", "action control:stop"].iter().map(|s| s.to_string()).collect();
        assert_eq!(request_of(&args), "action control:stop");
    }

    #[test]
    fn requests_from_the_command_line() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(request_of(&a(&["spm"])), "show");
        assert_eq!(request_of(&a(&["spm", "--action", "statistics"])), "action statistics");
    }

    #[test]
    fn prefs_default_to_a_tray_that_keeps_running() {
        let p = Prefs::default();
        assert!(p.start_at_signin && p.show_tray && !p.close_quits && p.background());
        let p: Prefs = serde_json::from_str("{\"show_tray\":false}").unwrap();
        assert!(!p.background() && p.start_at_signin);
    }
}
