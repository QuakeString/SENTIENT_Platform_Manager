// Copyright © 2016-2026 The SENTIENT Authors

//! Remote instances: SENTIENT on other computers, reached over SSH.
//!
//! The system's OpenSSH client does the connecting (ssh, sftp, ssh-keyscan:
//! part of Windows 10+ and every Linux): keys, agents, keep-alives. The
//! Platform Manager keeps its own `known_hosts`: the first connection shows
//! the server's key fingerprint, and after it is accepted a changed key is
//! refused. A key passphrase or sign-in password is kept in the system's
//! secret store (Windows: machine-sealed with DPAPI; Linux: the desktop
//! keyring through `secret-tool`, else only for this session) and reaches
//! ssh through SSH_ASKPASS, with this program as the helper (run with
//! SPM_ASKPASS_INSTANCE set, see main.rs).
//!
//! Work on the server is done by the Platform Manager itself, copied there:
//! a job is started detached from the connection (`systemd-run` on Linux),
//! writes its progress to a file, and is followed from here; a dropped
//! connection does not stop it, and reconnecting picks up where it is.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sentient_installer_core::progress::{Progress, ProgressFn};

pub mod reuse;

/// What the agent jobs mean (`--job …`, crate::headless), as one number:
/// raised whenever a job is added, removed or answers differently, so a
/// Platform Manager installed on a server is used as the agent only when it
/// speaks the same jobs (crate::remote::reuse). A build too old to report it
/// (no `--job agent-info`) counts as 0.
pub const PROTOCOL: u32 = 1;

/// The colours offered for a remote instance (this computer is SENTIENT teal).
pub const PALETTE: [&str; 6] = ["#6b4fa0", "#b4541a", "#4d7c2a", "#a23b5c", "#8a6d1f", "#c2410c"];
/// This computer's colour.
pub const LOCAL_COLOR: &str = "#187074";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    #[default]
    Linux,
    Windows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Auth {
    /// A key file (with or without a passphrase).
    #[default]
    Key,
    Password,
}

/// What the first connection found on the server.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Detected {
    pub os: Os,
    /// "Ubuntu 24.04 LTS", "Windows Server 2022".
    pub os_name: String,
    /// `/etc/os-release` ID: ubuntu, debian, rhel, … (empty on Windows).
    pub distro: String,
    /// x86_64, aarch64.
    pub arch: String,
    /// "Docker", "systemd services", "Windows services", "" (none found).
    pub install: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    pub id: String,
    pub name: String,
    pub color: String,
    #[serde(default)]
    pub os: Os,
    pub host: String,
    #[serde(default = "ssh_port")]
    pub port: u16,
    pub user: String,
    #[serde(default)]
    pub auth: Auth,
    #[serde(default)]
    pub key_file: Option<PathBuf>,
    /// The key file has a passphrase (kept in the secret store).
    #[serde(default)]
    pub key_has_passphrase: bool,
    /// The accepted host key, `SHA256:…`.
    #[serde(default)]
    pub host_key: Option<String>,
    #[serde(default)]
    pub detected: Option<Detected>,
}

fn ssh_port() -> u16 {
    22
}

impl Instance {
    pub fn address(&self) -> String {
        if self.port == 22 { format!("{}@{}", self.user, self.host) } else { format!("{}@{}:{}", self.user, self.host, self.port) }
    }
}

fn store_path() -> PathBuf {
    crate::updates::prefs_dir().join("instances.json")
}

pub fn known_hosts() -> PathBuf {
    crate::updates::prefs_dir().join("known_hosts")
}

pub fn load() -> Vec<Instance> {
    std::fs::read_to_string(store_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn save(list: &[Instance]) -> Result<(), String> {
    let path = store_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(list).map_err(|e| e.to_string())?).map_err(|e| format!("write {}: {e}", path.display()))
}

pub fn new_id() -> String {
    format!("{:x}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default())
}

/// Text on a background of `hex`: white on dark colours, dark on light ones.
pub fn text_on(hex: &str) -> u32 {
    let v = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0x187074);
    let (r, g, b) = (((v >> 16) & 0xff) as f64, ((v >> 8) & 0xff) as f64, (v & 0xff) as f64);
    let lin = |c: f64| {
        let c = c / 255.0;
        if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    let l = 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
    if l > 0.45 { 0x1f2328 } else { 0xffffff }
}

pub fn parse_hex(hex: &str) -> Option<u32> {
    let h = hex.trim().trim_start_matches('#');
    (h.len() == 6).then(|| u32::from_str_radix(h, 16).ok()).flatten()
}

// ---------------------------------------------------------------------------
// The secret (key passphrase or password)
// ---------------------------------------------------------------------------

static SESSION: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// Keep an instance's passphrase or password.
pub fn set_secret(id: &str, value: &str) {
    #[cfg(windows)]
    {
        let _ = crate::secrets::save_named(&format!("instance-{id}"), value);
    }
    #[cfg(not(windows))]
    {
        let stored = Command::new("secret-tool")
            .args(["store", "--label", "SENTIENT Platform Manager instance", "service", "sentient-platform-manager", "instance", id])
            .stdin(Stdio::piped())
            .spawn()
            .and_then(|mut c| {
                use std::io::Write as _;
                if let Some(mut s) = c.stdin.take() {
                    let _ = s.write_all(value.as_bytes());
                }
                c.wait()
            })
            .is_ok_and(|s| s.success());
        if !stored {
            // No keyring: only for this session.
            SESSION.lock().map(|mut g| g.get_or_insert_with(HashMap::new).insert(id.to_string(), value.to_string())).ok();
        }
    }
}

pub fn secret(id: &str) -> Option<String> {
    if let Some(v) = SESSION.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(id).cloned())) {
        return Some(v);
    }
    #[cfg(windows)]
    return crate::secrets::get_named(&format!("instance-{id}"));
    #[cfg(not(windows))]
    {
        let out = Command::new("secret-tool").args(["lookup", "service", "sentient-platform-manager", "instance", id]).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string()).filter(|s| !s.is_empty())
    }
}

pub fn forget_secret(id: &str) {
    forget_one(&sudo_key(id));
    forget_one(id);
    // The instance's SENTIENT sign-in (Licence) goes with it.
    crate::licence::forget_stored(id);
}

/// Forget one kept secret by its name.
pub fn forget_named(name: &str) {
    forget_one(name);
}

fn forget_one(id: &str) {
    SESSION.lock().map(|mut g| g.get_or_insert_with(HashMap::new).remove(id)).ok();
    #[cfg(windows)]
    crate::secrets::forget_named(&format!("instance-{id}"));
    #[cfg(not(windows))]
    {
        let _ = Command::new("secret-tool").args(["clear", "service", "sentient-platform-manager", "instance", id]).status();
    }
}

/// ssh asks for the passphrase or password of the
/// instance named in SPM_ASKPASS_INSTANCE. Prints it, or fails.
pub fn askpass() -> i32 {
    let Ok(id) = std::env::var("SPM_ASKPASS_INSTANCE") else { return 1 };
    // A one-off value the caller handed down (first connection, not yet saved).
    if let Ok(v) = std::env::var("SPM_ASKPASS_VALUE") {
        println!("{v}");
        return 0;
    }
    match secret(&id) {
        Some(v) => {
            println!("{v}");
            0
        }
        None => 1,
    }
}

// ---------------------------------------------------------------------------
// ssh
// ---------------------------------------------------------------------------

/// An ssh command for `inst` running `remote` there. `once` is a secret to
/// use instead of the stored one (before it is saved).
pub fn ssh(inst: &Instance, remote: &str, once: Option<&str>) -> Command {
    ssh_with(inst, &[], remote, once)
}

/// [`ssh`] with options before the host (`-s` for a subsystem).
fn ssh_with(inst: &Instance, before_host: &[&str], remote: &str, once: Option<&str>) -> Command {
    let mut c = Command::new("ssh");
    c.args(before_host);
    c.args(["-p", &inst.port.to_string()])
        .args(["-o", &format!("UserKnownHostsFile={}", known_hosts().display())])
        .args(["-o", "StrictHostKeyChecking=yes", "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=4", "-o", "ConnectTimeout=12"])
        .args(["-o", if inst.auth == Auth::Password { "PreferredAuthentications=password,keyboard-interactive" } else { "PreferredAuthentications=publickey" }]);
    if let (Auth::Key, Some(k)) = (inst.auth, &inst.key_file) {
        c.args(["-i", &k.display().to_string(), "-o", "IdentitiesOnly=yes"]);
    }
    c.arg(format!("{}@{}", inst.user, inst.host)).arg(remote);
    askpass_env(&mut c, inst, once);
    c.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c
}

fn askpass_env(c: &mut Command, inst: &Instance, once: Option<&str>) {
    // Tests have no `--askpass` entry: a stand-in helper.
    #[cfg(test)]
    let helper = std::env::var_os("SPM_TEST_ASKPASS").map(PathBuf::from).ok_or(());
    #[cfg(not(test))]
    let helper = std::env::current_exe();
    if let Ok(me) = helper {
        c.env("SSH_ASKPASS", me).env("SSH_ASKPASS_REQUIRE", "force").env("SPM_ASKPASS_INSTANCE", &inst.id);
        if std::env::var_os("DISPLAY").is_none() {
            c.env("DISPLAY", ":0");
        }
        // A value kept only in this process (no keyring) reaches the helper
        // the same way as a one-off one.
        let session = || SESSION.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(&inst.id).cloned()));
        if let Some(v) = once.map(str::to_string).or_else(session) {
            c.env("SPM_ASKPASS_VALUE", v);
        }
    }
}

/// Run a command on the server and return what it printed.
pub fn run(inst: &Instance, remote: &str, once: Option<&str>) -> Result<String, String> {
    let out = ssh(inst, remote, once).output().map_err(|e| format!("start ssh: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if out.status.success() {
        return Ok(stdout);
    }
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if once.is_none() && err.to_lowercase().contains("permission denied") && (inst.key_has_passphrase || inst.auth == Auth::Password) && secret(&inst.id).is_none() {
        return Err(FORGOTTEN.into());
    }
    Err(explain(&err).unwrap_or_else(|| if err.is_empty() { stdout.trim().to_string() } else { err }))
}

/// The passphrase or password was kept only until the Platform Manager closed.
pub const FORGOTTEN: &str = "The passphrase or password is not kept on this computer (no keyring). Enter it again with Edit.";

/// ssh's messages in plain words.
pub(crate) fn explain(err: &str) -> Option<String> {
    let e = err.to_lowercase();
    Some(if e.contains("remote host identification has changed") || e.contains("host key verification failed") {
        "The server's key is not the one accepted before. The connection was refused: if the server was reinstalled, remove and add the instance again; otherwise someone may be in between.".into()
    } else if e.contains("permission denied") {
        "The server refused the sign-in (user, key or password).".into()
    } else if e.contains("connection timed out") || e.contains("operation timed out") {
        "The server did not answer (address, port or firewall).".into()
    } else if e.contains("connection refused") {
        "The server refused the connection: no SSH service on that port.".into()
    } else if e.contains("could not resolve") || e.contains("name or service not known") {
        "That address is not known on this network.".into()
    } else {
        return None;
    })
}

/// The server's host keys and their fingerprint, before trusting it.
pub fn scan(host: &str, port: u16) -> Result<(String, String), String> {
    let out = Command::new("ssh-keyscan").args(["-p", &port.to_string(), "-T", "8", "-t", "ed25519,ecdsa,rsa", host]).output().map_err(|e| format!("ssh-keyscan: {e}"))?;
    let keys: String = String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()).map(|l| format!("{l}\n")).collect();
    if keys.is_empty() {
        return Err(format!("{host}:{port} did not answer as an SSH server."));
    }
    // The fingerprint of the first (strongest) key.
    let mut kg = Command::new("ssh-keygen").args(["-lf", "-"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().map_err(|e| format!("ssh-keygen: {e}"))?;
    if let Some(mut s) = kg.stdin.take() {
        use std::io::Write as _;
        let _ = s.write_all(keys.lines().next().unwrap_or("").as_bytes());
    }
    let fp = String::from_utf8_lossy(&kg.wait_with_output().map_err(|e| e.to_string())?.stdout)
        .split_whitespace()
        .find(|w| w.starts_with("SHA256:"))
        .unwrap_or("")
        .to_string();
    Ok((keys, fp))
}

/// Trust these host keys from now on.
pub fn trust(keys: &str) -> Result<(), String> {
    let path = known_hosts();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|e| e.to_string())?;
    f.write_all(keys.as_bytes()).map_err(|e| e.to_string())
}

/// Forget a server's keys (the instance is removed).
pub fn untrust(host: &str, port: u16) {
    let pattern = host_pattern(host, port);
    let _ = Command::new("ssh-keygen").args(["-R", &pattern, "-f", &known_hosts().display().to_string()]).output();
}

/// Find out the server's system, processor and SENTIENT install.
pub fn detect(inst: &Instance, once: Option<&str>) -> Result<Detected, String> {
    let unix = run(
        inst,
        "uname -sm; . /etc/os-release 2>/dev/null; echo \"$ID|$PRETTY_NAME\"; \
         if systemctl is-enabled sentient >/dev/null 2>&1; then echo systemd; \
         elif docker ps -a --format '{{.Names}}' 2>/dev/null | grep -qx sentient; then echo docker; else echo none; fi",
        once,
    );
    if let Ok(out) = unix {
        let mut lines = out.lines();
        let uname = lines.next().unwrap_or("");
        if uname.starts_with("Linux") {
            let arch = uname.split_whitespace().nth(1).unwrap_or("").to_string();
            let (id, pretty) = lines.next().unwrap_or("|").split_once('|').unwrap_or(("", ""));
            let install = match lines.next().unwrap_or("") {
                "systemd" => "systemd services",
                "docker" => "Docker",
                _ => "",
            };
            return Ok(Detected { os: Os::Linux, os_name: if pretty.is_empty() { "Linux".into() } else { pretty.into() }, distro: id.into(), arch, install: install.into() });
        }
    }
    // Windows (OpenSSH Server runs cmd).
    let out = run(inst, "powershell -NoProfile -Command \"(Get-CimInstance Win32_OperatingSystem).Caption; $env:PROCESSOR_ARCHITECTURE; if (Get-Service SentientServer -ErrorAction SilentlyContinue) {'services'} else {'none'}\"", once)?;
    let mut l = out.lines().map(str::trim);
    let name = l.next().unwrap_or("Windows").to_string();
    let arch = match l.next().unwrap_or("") {
        "AMD64" => "x86_64".to_string(),
        "ARM64" => "aarch64".to_string(),
        a => a.to_string(),
    };
    let install = if l.next() == Some("services") { "Windows services" } else { "" };
    Ok(Detected { os: Os::Windows, os_name: name, distro: String::new(), arch, install: install.into() })
}

// ---------------------------------------------------------------------------
// The Platform Manager on the server, and its jobs
// ---------------------------------------------------------------------------

/// Where the Platform Manager lives on a Linux server.
pub const REMOTE_SPM: &str = "/opt/sentient-platform-manager/sentient-platform-manager";
const REMOTE_JOBS: &str = "/var/lib/sentient-platform-manager/jobs";
/// For an account without sudo, the agent lives in its home instead and its
/// jobs run as that account (jobs that need root say so). Shell text: `$HOME`
/// is expanded on the server.
const USER_DIR: &str = "$HOME/.local/share/sentient-platform-manager";
const USER_SPM: &str = "$HOME/.local/share/sentient-platform-manager/sentient-platform-manager";
const USER_JOBS: &str = "$HOME/.local/share/sentient-platform-manager/jobs";
/// Printed (stderr) by the shell when it ran the per-user agent.
const USER_MARK: &str = "@@SPM_USER_AGENT";

/// What the account can do as administrator on the server.
#[derive(Debug, Clone, PartialEq)]
enum Admin {
    /// root, or sudo works (no password, or the one kept).
    Yes,
    /// sudo asks for a password and none is kept (or the kept one is wrong).
    NeedsPassword(String),
    /// No sudo for this account at all.
    No,
}

/// Whether sudo on the server wants a password nobody kept yet (one
/// connection; blocking).
pub fn sudo_needs_password(inst: &Instance) -> bool {
    matches!(probe_admin(inst), Admin::NeedsPassword(_))
}

/// The Files page's SFTP session (crate::sftp): `ssh -s sftp` with the
/// instance's sign-in, or, as administrator, the server's sftp-server
/// through sudo (its password, when one is kept, goes first on standard
/// input). Pipes set; spawn it and hand it to `Sftp::start`.
pub fn sftp_command(inst: &Instance, admin: bool) -> (Command, Option<String>) {
    let (mut c, pw) = if admin && inst.user != "root" {
        let pw = sudo_password(inst);
        let script = format!(
            "for p in /usr/lib/openssh/sftp-server /usr/libexec/openssh/sftp-server /usr/lib/ssh/sftp-server /usr/libexec/sftp-server /usr/lib/sftp-server; do if [ -x \"$p\" ]; then exec {}\"$p\"; fi; done; echo 'The server has no sftp-server program.' >&2; exit 127",
            sudo(inst, pw.is_some())
        );
        (ssh_with(inst, &[], &script, None), pw)
    } else {
        (ssh_with(inst, &["-s"], "sftp", None), None)
    };
    c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    (c, pw)
}

/// Delete a file or a whole folder tree on the server in one command
/// (Files, after its confirm): `rm -rf` on Linux, as administrator with
/// `admin`; Remove-Item on Windows. Refuses the root and empty paths.
pub fn remove_tree(inst: &Instance, path: &str, admin: bool) -> Result<(), String> {
    let p = path.trim();
    if p.is_empty() || p == "/" || p.trim_end_matches(['/', '\\']).is_empty() || (p.len() <= 3 && p.ends_with(":\\")) {
        return Err(format!("{path} cannot be deleted from here."));
    }
    if is_windows(inst) {
        return win::ps(inst, &format!("Remove-Item -LiteralPath '{}' -Recurse -Force -ErrorAction Stop", p.replace('\'', "''"))).map(|_| ());
    }
    let cmd = format!("rm -rf -- '{}'", p.replace('\'', "'\\''"));
    if admin && inst.user != "root" { sudo_run(inst, &cmd, None) } else { run(inst, &cmd, None) }.map(|_| ())
}

/// Find out with one connection whether sudo works for the account.
fn probe_admin(inst: &Instance) -> Admin {
    if inst.user == "root" {
        return Admin::Yes;
    }
    let pw = sudo_password(inst);
    let script = format!(
        "if [ \"$(id -u)\" = 0 ]; then echo ADMIN_OK; elif ! command -v sudo >/dev/null 2>&1; then echo NO_SUDO; else echo \"GROUPS $(id -nG)\"; {}true && echo ADMIN_OK; fi",
        sudo(inst, pw.is_some())
    );
    let mut c = ssh(inst, &script, None);
    c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let Ok(mut child) = c.spawn() else { return Admin::NeedsPassword("ssh did not start".into()) };
    if let Some(mut s) = child.stdin.take() {
        use std::io::Write as _;
        if let Some(pw) = &pw {
            let _ = writeln!(s, "{pw}");
        }
    }
    let Ok(out) = child.wait_with_output() else { return Admin::NeedsPassword("no answer".into()) };
    classify_admin(&String::from_utf8_lossy(&out.stdout), &String::from_utf8_lossy(&out.stderr), pw.is_some())
}

fn classify_admin(stdout: &str, stderr: &str, had_password: bool) -> Admin {
    if stdout.contains("ADMIN_OK") {
        return Admin::Yes;
    }
    if stdout.contains("NO_SUDO") || ["not in the sudoers", "may not run sudo", "is not allowed to execute"].iter().any(|w| stderr.contains(w)) {
        return Admin::No;
    }
    // Not in sudo, wheel or admin and sudo wants a password: most likely an
    // account without sudo; asked anyway when it is in one of them.
    let groups = stdout.lines().find_map(|l| l.strip_prefix("GROUPS ")).unwrap_or("");
    let admin_group = groups.split_whitespace().any(|g| ["sudo", "wheel", "admin"].contains(&g));
    if !had_password && !admin_group {
        return Admin::No;
    }
    Admin::NeedsPassword(stderr.trim().to_string())
}

/// Shell text that runs the agent with `args`: the system one as
/// administrator when it is there, else the per-user one as the account.
fn agent_cmd(inst: &Instance, args: &str, password: bool) -> String {
    let own = reuse::LINUX_OWN;
    let quoted = |path: &str| format!("{path} {args}").replace('\'', "'\\''");
    format!(
        "if [ -x {own} ]; then {s}sh -c '{}'; elif [ -x {REMOTE_SPM} ]; then {s}sh -c '{}'; elif [ -x \"{USER_SPM}\" ]; then echo {USER_MARK} >&2; \"{USER_SPM}\" {args}; else echo 'The server agent is not set up on this server.' >&2; exit 3; fi",
        quoted(own),
        quoted(REMOTE_SPM),
        s = sudo(inst, password),
    )
}

/// Shell text that sets `$A` to the system agent: this Platform Manager's
/// own beside an installed one, else the one at REMOTE_SPM.
fn pick_system_agent() -> String {
    format!("A={REMOTE_SPM}; [ -x {own} ] && A={own};", own = reuse::LINUX_OWN)
}

/// Which program does an instance's jobs.
#[derive(Debug, Clone, PartialEq)]
enum Agent {
    /// This Platform Manager's agent, set up earlier (or none yet: deploy).
    Own,
    /// A Platform Manager installed there, used as it is.
    Installed(String),
}

/// Look on the server for the agent: this Platform Manager's own, or an
/// installed Platform Manager that can do the jobs. Err: [`NOT_SET_UP`], an
/// installed one that cannot be used as it is ([`reuse::offer_message`]),
/// or why the server does not answer.
fn resolve(inst: &Instance) -> Result<Agent, String> {
    #[cfg(debug_assertions)]
    if let Ok(kind) = std::env::var("SPM_DEBUG_REMOTE_INSTALLED") {
        let ours = reuse::AgentInfo::this();
        let theirs = match kind.as_str() {
            "same" => ours.clone(),
            "newer" => reuse::AgentInfo { version: "0.9.0".into(), commit: "1234567".into(), protocol: PROTOCOL + 1, window: true },
            "old" => reuse::AgentInfo::default(),
            _ => reuse::AgentInfo { version: "0.8.0-rc11".into(), commit: "1234567".into(), protocol: PROTOCOL, window: true },
        };
        let path = if is_windows(inst) { r"C:\Program Files\SENTIENT Platform Manager\SENTIENT Platform Manager.exe".to_string() } else { REMOTE_SPM.to_string() };
        reuse::remember(&inst.id, Some(reuse::Installed { path: path.clone(), info: theirs.clone() }));
        return if reuse::usable(&inst.id, &theirs) { Ok(Agent::Installed(path)) } else { Err(reuse::offer_message(&theirs, &ours)) };
    }
    let probe = if is_windows(inst) {
        win::probe(inst)?
    } else {
        let out = run(
            inst,
            &format!(
                "if [ -x {own} ]; then echo @@OWN; elif [ -x {REMOTE_SPM} ]; then echo '@@INSTALLED {REMOTE_SPM}'; {REMOTE_SPM} --job agent-info 2>&1; \
                 elif [ -x \"{USER_SPM}\" ]; then echo @@OWN; else echo @@NONE; fi",
                own = reuse::LINUX_OWN
            ),
            None,
        )?;
        match reuse::parse_probe(&out) {
            // A bare agent at REMOTE_SPM (or one too old to say what it is)
            // is this Platform Manager's own, from an earlier Set up.
            reuse::Probe::Installed(i) if !i.info.window => reuse::Probe::Own,
            p => p,
        }
    };
    let agent = match probe {
        reuse::Probe::Own => {
            reuse::remember(&inst.id, None);
            Ok(Agent::Own)
        }
        reuse::Probe::None => {
            reuse::remember(&inst.id, None);
            Err(NOT_SET_UP.to_string())
        }
        reuse::Probe::Installed(i) => {
            reuse::remember(&inst.id, Some(i.clone()));
            if reuse::usable(&inst.id, &i.info) { Ok(Agent::Installed(i.path)) } else { Err(reuse::offer_message(&i.info, &reuse::AgentInfo::this())) }
        }
    };
    if let Ok(a) = &agent {
        win::set_exe(&inst.id, a);
    }
    agent
}

/// "Use it anyway": the installed Platform Manager found on the server does
/// the jobs from now on (for its version and build).
pub fn use_installed(inst: &Instance) -> Result<(), String> {
    let found = reuse::found(&inst.id).ok_or("No installed Platform Manager was found on this server. Check again.")?;
    reuse::accept(&inst.id, &found.info)
}

/// The account has no sudo: the next Set up puts the agent in its home.
pub fn choose_user_install(inst: &Instance) -> Result<(), String> {
    run(inst, &format!("mkdir -p \"{USER_DIR}\""), None).map(|_| ())
}

/// Put the agent in the account's home (no sudo there).
fn deploy_user(inst: &Instance, staging: &str, sink: &ProgressFn) -> Result<(), String> {
    sink(Progress::Log { line: format!("{} has no administrator rights on the server: the agent goes in its home folder, and jobs that need root will say so.", inst.user) });
    run(inst, &format!("mkdir -p \"{USER_JOBS}\" && install -m 0755 {staging} \"{USER_SPM}\""), None)
        .map_err(|e| failed("The agent could not be put in the account's home folder. The agent there is unchanged.", &e))?;
    Ok(())
}

/// `sudo` that reads its password from standard input when the instance
/// signs in with one (else the account needs passwordless sudo, or root).
fn sudo(inst: &Instance, password: bool) -> &'static str {
    if inst.user == "root" { "" } else if password { "sudo -S -p '' " } else { "sudo -n " }
}

/// The password `sudo` asks for: the sign-in password, or, for a key
/// sign-in, the administrator password kept for the instance (if any).
fn sudo_password(inst: &Instance) -> Option<String> {
    if inst.user == "root" {
        return None;
    }
    match inst.auth {
        Auth::Password => secret(&inst.id),
        Auth::Key => secret(&sudo_key(&inst.id)),
    }
}

/// Where an instance's sudo password is kept.
pub fn sudo_key(id: &str) -> String {
    format!("{id}-sudo")
}

/// Put this Platform Manager's build for the server's system on the
/// server (Linux): this very program when the systems match, checked by its
/// SHA-256 after copying; the copy is resumed after a dropped connection.
#[cfg(debug_assertions)]
static DEBUG_AGENT_UPDATED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(debug_assertions)]
fn debug_plan(compose: Option<&str>) -> sentient_installer_core::distro::adopt::TakeOverPlan {
    sentient_installer_core::distro::adopt::TakeOverPlan {
        project: "sentient".into(),
        files: vec![(compose.unwrap_or("/home/northwind/sentient/docker-compose.yml").to_string(), true)],
        env_files: vec![("/home/northwind/sentient/.env".into(), true)],
        containers: vec!["sentient".into(), "sentient-postgres".into()],
        image: "quakestring/sentient:4.4.0.1".into(),
        volumes: vec!["sentient_data".into(), "sentient_pg".into()],
        ports: vec!["8080 web".into(), "1883 MQTT".into(), "8883 MQTTS".into(), "5683 CoAP".into(), "5684 CoAPS".into()],
        extra_env: Vec::new(),
        loopback_port: Some(15432),
        problems: Vec::new(),
        new_file: "/var/lib/sentient-platform-manager/stack/docker-compose.yml".into(),
        ..Default::default()
    }
}

pub fn deploy(inst: &Instance, sink: &ProgressFn) -> Result<(), String> {
    #[cfg(debug_assertions)]
    match std::env::var("SPM_DEBUG_REMOTE_TAKEOVER").as_deref() {
        Ok("old") => {
            std::thread::sleep(std::time::Duration::from_secs(2));
            DEBUG_AGENT_UPDATED.store(true, std::sync::atomic::Ordering::Relaxed);
            return Ok(());
        }
        Ok("sudo") if secret(&sudo_key(&inst.id)).is_none() => {
            return Err(failed(&format!("{} needs administrator rights on the server: sudo asks for a password.", inst.user), "sudo: a password is required"));
        }
        Ok("sudo") => {
            DEBUG_AGENT_UPDATED.store(true, std::sync::atomic::Ordering::Relaxed);
            return Ok(());
        }
        _ => {}
    }
    let det = inst.detected.clone().ok_or("Connect to the server first.")?;
    // An installed Platform Manager that does the jobs: nothing to copy.
    // One that cannot is left as it is, and this version's agent goes
    // beside it (crate::remote::reuse).
    let beside = match resolve(inst) {
        Ok(Agent::Installed(path)) => {
            sink(Progress::Log { line: format!("The Platform Manager installed on the server does the work ({path}).") });
            if det.os != Os::Windows {
                ensure_sampler(inst, sink, false);
            }
            return Ok(());
        }
        Err(e) if is_not_admin(&e) => return Err(e),
        Ok(Agent::Own) | Err(_) => reuse::found(&inst.id).is_some(),
    };
    if det.os == Os::Windows {
        return win::deploy(inst, sink);
    }
    let arch = match det.arch.as_str() {
        "x86_64" | "amd64" => Arch::X86_64,
        "aarch64" | "arm64" => Arch::Aarch64,
        other => return Err(failed(&format!("There is no server agent for Linux on {other} processors. The agent runs on x86_64 and ARM64 (aarch64) servers."), "")),
    };
    let me = agent_file(System::Linux(arch), sink)?;
    // Where it goes as administrator: REMOTE_SPM, or beside an installed
    // Platform Manager there (never over it).
    let target = if beside || run(inst, &format!("[ -x {} ] && echo yes || echo no", reuse::LINUX_OWN), None).is_ok_and(|o| o.trim() == "yes") { reuse::LINUX_OWN } else { REMOTE_SPM };
    let want = sentient_installer_core::manifest::sha256_file(&me)?;
    let have = run(inst, &format!("if [ -x {target} ]; then sha256sum {target}; else sha256sum \"{USER_SPM}\"; fi 2>/dev/null | cut -d' ' -f1"), None).unwrap_or_default();
    if have.trim() == want {
        sink(Progress::Log { line: "The Platform Manager on the server is up to date.".into() });
        ensure_sampler(inst, sink, false);
        return Ok(());
    }
    sink(Progress::Step { name: "Copying the Platform Manager to the server".into() });
    // Staged in the account's home, not /tmp: /tmp is often mounted
    // noexec, and the copy must run once before it replaces anything.
    let home = home(inst).unwrap_or_else(|_| "/tmp".into());
    let staging = format!("{}/.spm-upload-{}", home.trim_end_matches('/'), &want[..12]);
    let clean = |inst: &Instance| {
        let _ = run(inst, &format!("rm -f {staging}"), None);
    };
    // A staging file from an earlier attempt: the same file is used as it is;
    // anything else (a partial or another build) goes, and the copy starts
    // fresh. Resuming is only for a connection dropped during this copy.
    let staged = run(inst, &format!("sha256sum {staging} 2>/dev/null | cut -d' ' -f1"), None).unwrap_or_default();
    if staged.trim() != want {
        clean(inst);
        sftp_copy(inst, &me, &staging).map_err(|e| failed("The agent could not be copied to the server. Nothing on the server was changed.", &e))?;
        let got = run(inst, &format!("sha256sum {staging} | cut -d' ' -f1"), None)?;
        if got.trim() != want {
            clean(inst);
            return Err("The copy on the server does not match (checksum); nothing was replaced. Try again.".into());
        }
    }
    // It must start on the server before it replaces anything.
    if let Err(e) = run(inst, &format!("chmod +x {staging} && {staging} --job monitor-json"), None) {
        clean(inst);
        return Err(failed(&format!("The Platform Manager agent does not start on {}. Nothing on the server was changed.", if det.os_name.is_empty() { inst.name.as_str() } else { det.os_name.as_str() }), &e));
    }
    // Where it goes: the system place when sudo works; the account's home
    // when it has no sudo, or chose that earlier (a per-user agent and no
    // system one there); sudo's password asked for otherwise.
    let user_chosen = run(inst, &format!("[ ! -e {REMOTE_SPM} ] && [ -d \"{USER_DIR}\" ] && echo yes || echo no"), None).is_ok_and(|o| o.trim() == "yes");
    match probe_admin(inst) {
        Admin::Yes => {
            if let Err(e) = sudo_run(inst, &format!("install -D -m 0755 {staging} {target} && mkdir -p {REMOTE_JOBS}"), None) {
                clean(inst);
                return Err(failed(&format!("The agent could not be put in place at {target} (as administrator). The agent there is unchanged."), &e));
            }
            if target == reuse::LINUX_OWN {
                sink(Progress::Log { line: format!("This version's agent is set up beside the Platform Manager installed on the server, at {target}; the installed one is not changed.") });
            }
        }
        Admin::No => {
            let r = deploy_user(inst, &staging, sink);
            clean(inst);
            r?;
        }
        Admin::NeedsPassword(_) if user_chosen => {
            let r = deploy_user(inst, &staging, sink);
            clean(inst);
            r?;
        }
        Admin::NeedsPassword(why) => {
            clean(inst);
            let had = sudo_password(inst).is_some();
            return Err(failed(
                &if had {
                    format!("sudo refused the administrator password for {} on the server. Enter it again.", inst.user)
                } else {
                    format!("{} needs administrator rights on the server: sudo asks for a password.", inst.user)
                },
                &why,
            ));
        }
    }
    clean(inst);
    ensure_sampler(inst, sink, true);
    Ok(())
}

/// Copy a file fresh; only a connection dropped during this copy resumes it.
fn sftp_copy(inst: &Instance, local: &Path, remote: &str) -> Result<(), String> {
    match sftp_batch(inst, &format!("put \"{}\" \"{remote}\"", local.display())) {
        Ok(()) => Ok(()),
        Err(e) if ["Connection closed", "Connection reset", "Broken pipe", "Connection lost", "timed out"].iter().any(|w| e.contains(w)) => {
            sftp_batch(inst, &format!("reput \"{}\" \"{remote}\"", local.display()))
        }
        Err(e) => Err(e),
    }
}

/// The statistics sampler on the server (crate::stats): installed with the
/// agent, or when an up-to-date agent has none yet. Never fails the deploy.
fn ensure_sampler(inst: &Instance, sink: &ProgressFn, always: bool) {
    if !always && run(inst, "[ -e /etc/systemd/system/sentient-pm-stats.timer ] || [ -e \"$HOME/.config/systemd/user/sentient-pm-stats.timer\" ] && echo yes || echo no", None).is_ok_and(|o| o.trim() == "yes") {
        return;
    }
    match spm_job(inst, "stats-install") {
        Ok(out) => sink(Progress::Log { line: out.trim().to_string() }),
        Err(e) => sink(Progress::Log { line: format!("The statistics sampler was not installed: {e}") }),
    }
}

/// Copy a file with sftp, resuming a part already there (a copy cut off
/// earlier), else from the start.
fn sftp_put(inst: &Instance, local: &Path, remote: &str) -> Result<(), String> {
    match sftp_batch(inst, &format!("reput \"{}\" \"{remote}\"", local.display())) {
        Ok(()) => Ok(()),
        // Nothing there yet to resume.
        Err(e) if e.contains("No such file") || e.contains("stat remote") => sftp_batch(inst, &format!("put \"{}\" \"{remote}\"", local.display())),
        Err(e) => Err(e),
    }
}

fn sftp_batch(inst: &Instance, command: &str) -> Result<(), String> {
    let mut c = Command::new("sftp");
    // BatchMode=no before -b: batch mode would otherwise refuse the passphrase or password prompt.
    c.args(["-o", "BatchMode=no", "-P", &inst.port.to_string(), "-b", "-"])
        .args(["-o", &format!("UserKnownHostsFile={}", known_hosts().display()), "-o", "StrictHostKeyChecking=yes", "-o", "ServerAliveInterval=15"]);
    if let (Auth::Key, Some(k)) = (inst.auth, &inst.key_file) {
        c.args(["-i", &k.display().to_string()]);
    }
    c.arg(format!("{}@{}", inst.user, inst.host));
    askpass_env(&mut c, inst, None);
    c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("start sftp: {e}"))?;
    if let Some(mut s) = child.stdin.take() {
        use std::io::Write as _;
        let _ = writeln!(s, "{command}");
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("The copy to the server failed: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Run the agent with `args` (crate::remote::agent_cmd): as administrator
/// from the system place, else as the account from its home. A per-user
/// agent refused for want of root says so plainly.
fn agent_run(inst: &Instance, args: &str) -> Result<String, String> {
    let pw = sudo_password(inst);
    let mut c = ssh(inst, &agent_cmd(inst, args, pw.is_some()), None);
    if pw.is_some() {
        c.stdin(Stdio::piped());
    }
    c.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("start ssh: {e}"))?;
    if let (Some(pw), Some(mut s)) = (&pw, child.stdin.take()) {
        use std::io::Write as _;
        let _ = writeln!(s, "{pw}");
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let user = stderr.contains(USER_MARK);
    let err = stderr.replace(USER_MARK, "").trim().to_string();
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).to_string());
    }
    if user {
        let lower = err.to_lowercase();
        if lower.contains("permission denied") || lower.contains("operation not permitted") || lower.contains("must be root") || lower.contains("as root") {
            return Err(failed(&format!("This takes root on the server, and {} has no sudo there. Sign in as an account with sudo, or as root, to do it.", inst.user), &err));
        }
        return Err(explain(&err).unwrap_or(err));
    }
    Err(sudo_error(inst, pw.is_some(), &err))
}

fn sudo_error(inst: &Instance, password: bool, err: &str) -> String {
    if err.contains("a password is required") || err.contains("sudo:") {
        return if password {
            format!("sudo refused the administrator password for {} on the server. Enter it again with Edit.", inst.user)
        } else {
            format!("{} needs administrator rights on the server: enter its sudo password with Edit, sign in as root, or allow it sudo without a password.", inst.user)
        };
    }
    explain(err).unwrap_or(err.to_string())
}

fn sudo_run(inst: &Instance, cmd: &str, _login: Option<&str>) -> Result<String, String> {
    let pw = sudo_password(inst);
    let password = pw.as_deref();
    let full = format!("{}sh -c '{}'", sudo(inst, password.is_some()), cmd.replace('\'', "'\\''"));
    let mut c = ssh(inst, &full, None);
    if password.is_some() {
        c.stdin(Stdio::piped());
    }
    c.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("start ssh: {e}"))?;
    if let (Some(pw), Some(mut s)) = (password, child.stdin.take()) {
        use std::io::Write as _;
        let _ = writeln!(s, "{pw}");
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).to_string());
    }
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(sudo_error(inst, password.is_some(), &err))
}

/// Whether the server answers and has the Platform Manager (Err:
/// [`NOT_SET_UP`], or why it cannot be reached).
pub fn ready(inst: &Instance) -> Result<(), String> {
    #[cfg(debug_assertions)]
    if std::env::var_os("SPM_DEBUG_REMOTE_STATUS").is_some() {
        return status(inst).map(|_| ());
    }
    resolve(inst).map(|_| ())
}

/// What [`status`] says before the Platform Manager is on the server.
/// The Platform Manager there has no SENTIENT added yet: nothing to report.
pub const NOT_ADDED: &str = "No SENTIENT is added on this server.";

pub const NOT_SET_UP: &str = "The server agent is not set up on this server. Set up installs it, to read the server's state and work on it.";

/// Whether an error means the agent needs setting up (or choosing): none on
/// the server, or an installed Platform Manager not used as it is yet.
pub fn needs_setup(e: &str) -> bool {
    e == NOT_SET_UP || reuse::is_offer(e)
}

/// What the agent prints on Windows when the account is not an
/// administrator (crate::headless::run).
const NOT_ELEVATED: &str = "Run this from an elevated prompt";

/// Whether a job's answer is the agent's refusal for want of rights.
fn not_elevated(out: &str) -> bool {
    out.contains(NOT_ELEVATED)
}

/// A Windows account that is not an administrator: every job refuses, in
/// words of its own; this says it plainly, whichever way it came back.
fn rights_answer(inst: &Instance, r: Result<String, String>) -> Result<String, String> {
    match r {
        Ok(out) | Err(out) if is_windows(inst) && not_elevated(&out) => Err(not_admin(inst)),
        r => r,
    }
}

/// The Windows account the instance signs in with is not an administrator.
pub fn not_admin(inst: &Instance) -> String {
    not_admin_message(&inst.user, &inst.host)
}

pub fn not_admin_message(user: &str, host: &str) -> String {
    format!(
        "The account {user} on {host} is not an administrator. The Platform Manager needs an administrator account there: add it to the Administrators group, or use an administrator account for this instance."
    )
}

/// Whether an error is [`not_admin`].
pub fn is_not_admin(e: &str) -> bool {
    e.contains(" is not an administrator. The Platform Manager needs an administrator account there")
}

/// The server's state, from its own Platform Manager (`--job status-json`).
pub fn status(inst: &Instance) -> Result<RemoteStatus, String> {
    #[cfg(debug_assertions)]
    if let Some(secs) = std::env::var("SPM_DEBUG_REMOTE_SLOW").ok().and_then(|s| s.parse::<u64>().ok()) {
        std::thread::sleep(std::time::Duration::from_secs(secs));
    }
    // Screens: an installed Platform Manager found there (crate::remote::resolve).
    #[cfg(debug_assertions)]
    if std::env::var_os("SPM_DEBUG_REMOTE_INSTALLED").is_some() {
        resolve(inst)?;
    }
    if is_windows(inst) {
        #[cfg(debug_assertions)]
        if std::env::var_os("SPM_DEBUG_REMOTE_STATUS").is_none() {
            return win::status(inst);
        }
        #[cfg(not(debug_assertions))]
        return win::status(inst);
    }
    // Development aid: canned answers by instance id (screens).
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("SPM_DEBUG_REMOTE_STATUS") {
        let all: HashMap<String, RemoteStatus> = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        if let Some(s) = all.get(&inst.id) {
            return Ok(s.clone());
        }
    }
    resolve(inst)?;
    let out = spm_job(inst, "status-json")?;
    let line = out.lines().rev().find(|l| l.trim_start().starts_with('{')).ok_or("The server's Platform Manager gave no status.")?;
    let mut st: RemoteStatus = serde_json::from_str(line).map_err(|e| format!("unexpected status from the server: {e}"))?;
    st.running_job = running_job(inst);
    Ok(st)
}

/// What a card shows.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct RemoteStatus {
    pub installed: bool,
    pub serving: bool,
    pub sentient_version: String,
    pub database: String,
    pub database_bytes: Option<u64>,
    pub disk_free: Option<u64>,
    pub last_backup: Option<String>,
    pub uptime_secs: Option<u64>,
    pub manager_version: String,
    /// A job running there now (started earlier, maybe from a closed window).
    #[serde(default)]
    pub running_job: Option<String>,
    /// SENTIENT's web port there (Open SENTIENT).
    #[serde(default)]
    pub http_port: Option<u16>,
    /// Compose files the stack was started with that are gone since.
    #[serde(default)]
    pub vanished_files: Vec<String>,
    /// It runs from its own compose file (adopted): settings and the
    /// database type need the takeover first.
    #[serde(default)]
    pub external_compose: bool,
    /// The licence's one-word state, read-only from SENTIENT's database
    /// (crate::licence::read_tile); None: not read (an older agent too).
    #[serde(default)]
    pub licence: Option<crate::licence::TileLicence>,
}

/// One service or container on the server (Status).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RemoteService {
    pub name: String,
    pub running: bool,
    pub starting: bool,
    pub detail: String,
}

/// The server's backups and their settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteBackups {
    pub folder: PathBuf,
    pub daily_at: Option<String>,
    pub keep: u32,
    pub list: Vec<crate::backups::Entry>,
    /// What an automatic backup includes (an older agent sends none).
    #[serde(default)]
    pub preset: crate::backups::Preset,
    /// How much telemetry history a backup takes (an older agent sends none).
    #[serde(default)]
    pub history: crate::backups::History,
    /// Free space where the folder is (an older agent sends none).
    #[serde(default)]
    pub free: Option<u64>,
}

/// No JSON line came back: say what did (the last lines it printed), or
/// that it printed nothing at all, instead of only "no answer".
fn no_answer(out: &str) -> String {
    let lines: Vec<&str> = out.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if lines.is_empty() {
        failed(
            "The server's Platform Manager printed nothing for this request.",
            "It may have stopped at once (a missing file or a blocked program), or it is a windowed build that was not waited for. Run Set up again; if it stays, run it there by hand with --job status-json and look at what it says.",
        )
    } else {
        let tail = lines[lines.len().saturating_sub(6)..].join("\n");
        failed("The server's Platform Manager answered, but not with the data asked for. What it printed:", &tail)
    }
}

/// Ask the server's Platform Manager for `job` (as administrator) and read
/// the one JSON line it prints.
fn read_json<T: serde::de::DeserializeOwned>(inst: &Instance, job: &str) -> Result<T, String> {
    let out = spm_job(inst, job)?;
    // "null": nothing to report yet, no SENTIENT added there. Not a failure.
    if out.lines().any(|l| l.trim() == "null") {
        return Err(NOT_ADDED.into());
    }
    let line = out.lines().rev().find(|l| { let t = l.trim_start(); t.starts_with('{') || t.starts_with('[') }).ok_or_else(|| no_answer(&out))?;
    serde_json::from_str(line).map_err(|e| format!("unexpected answer from the server: {e}"))
}

pub fn services(inst: &Instance) -> Result<Vec<RemoteService>, String> {
    #[cfg(debug_assertions)]
    if let Some(secs) = std::env::var("SPM_DEBUG_REMOTE_SLOW").ok().and_then(|s| s.parse::<u64>().ok()) {
        std::thread::sleep(std::time::Duration::from_secs(secs));
    }
    #[cfg(debug_assertions)]
    if let Some(t) = std::env::var_os("SPM_DEBUG_REMOTE_SERVICES") {
        return serde_json::from_str(&t.to_string_lossy()).map_err(|e| e.to_string());
    }
    read_json(inst, "services-json")
}

pub fn dbstatus(inst: &Instance) -> Result<crate::dbstatus::DbStatus, String> {
    #[cfg(debug_assertions)]
    if let Some(secs) = std::env::var("SPM_DEBUG_REMOTE_SLOW").ok().and_then(|s| s.parse::<u64>().ok()) {
        std::thread::sleep(std::time::Duration::from_secs(secs));
    }
    #[cfg(debug_assertions)]
    if let Some(p) = std::env::var_os("SPM_DEBUG_DBSTATUS") {
        return std::fs::read_to_string(p).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()));
    }
    read_json(inst, "dbstatus-json")
}

/// The server's settings by catalog name (crate::settings::current there).
pub fn settings(inst: &Instance) -> Result<HashMap<String, String>, String> {
    #[cfg(debug_assertions)]
    if let Some(t) = std::env::var_os("SPM_DEBUG_REMOTE_SETTINGS") {
        return serde_json::from_str(&t.to_string_lossy()).map_err(|e| e.to_string());
    }
    read_json(inst, "settings-json")
}

/// Write `text` to `path` on the server (readable by the owner only).
pub fn put_text(inst: &Instance, path: &str, text: &str) -> Result<(), String> {
    if is_windows(inst) {
        use base64::Engine as _;
        let b = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
        return win::ps(inst, &format!("[IO.File]::WriteAllText('{}', [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{b}')))", path.replace('\'', "''"))).map(|_| ());
    }
    let mut c = ssh(inst, &format!("umask 077 && cat > '{}'", path.replace('\'', "")), None);
    c.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("start ssh: {e}"))?;
    if let Some(mut s) = child.stdin.take() {
        use std::io::Write as _;
        s.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() { Ok(()) } else { Err(explain(&String::from_utf8_lossy(&out.stderr)).unwrap_or_else(|| String::from_utf8_lossy(&out.stderr).trim().to_string())) }
}

/// The gateway on the server (`gateway-status` there): key → value.
pub fn gateway(inst: &Instance) -> Result<HashMap<String, String>, String> {
    #[cfg(debug_assertions)]
    if let Some(t) = std::env::var_os("SPM_DEBUG_REMOTE_GATEWAY") {
        return Ok(t.to_string_lossy().split([' ', '+']).filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect());
    }
    let out = spm_job(inst, "gateway-status")?;
    let line = out.lines().rev().find(|l| l.starts_with("installed=")).ok_or("The server's Platform Manager gave no gateway state.")?;
    // Values with spaces come %20-escaped (headless's gateway-status).
    Ok(line.split(' ').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), v.replace("%20", " ").replace("%25", "%"))).collect())
}

/// The account's home folder on the server.
pub fn home(inst: &Instance) -> Result<String, String> {
    if is_windows(inst) {
        return Ok(win::ps(inst, "$env:USERPROFILE")?.trim().to_string());
    }
    Ok(run(inst, "printf %s \"$HOME\"", None)?.trim().to_string())
}

pub fn backups(inst: &Instance) -> Result<RemoteBackups, String> {
    // Development aid: canned backups (screens).
    #[cfg(debug_assertions)]
    if let Some(t) = std::env::var_os("SPM_DEBUG_REMOTE_BACKUPS") {
        return serde_json::from_str(&t.to_string_lossy()).map_err(|e| e.to_string());
    }
    read_json(inst, "backups-json")
}

/// A path or name for a job's command line: one argument, quoted for the
/// server's shell (a quote in it is refused, never stripped).
fn job_arg(a: &str) -> Result<String, String> {
    if a.contains(['\'', '"', '\n']) {
        return Err(format!("{a} has a quote in it; pick a folder without one."));
    }
    Ok(format!("'{a}'"))
}

/// Where each store keeps its data on the server, its disks and the old
/// copies kept (crate::movedata there).
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn movedata_overview(inst: &Instance) -> Result<sentient_installer_core::distro::move_data::Overview, String> {
    read_json(inst, "movedata-json")
}

/// A folder on the server checked for a store.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn movedata_check(inst: &Instance, store: sentient_installer_core::distro::move_data::Store, path: &str) -> Result<sentient_installer_core::distro::move_data::Check, String> {
    read_json(inst, &format!("movedata-check {} {}", store.as_str(), job_arg(path)?))
}

/// How long the moves would take, measured on the server's disks.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn movedata_estimate(inst: &Instance, moves: &[sentient_installer_core::distro::move_data::Move]) -> Result<sentient_installer_core::distro::move_data::Estimate, String> {
    read_json(inst, &format!("movedata-estimate {}", job_arg(&crate::movedata::moves_arg(moves))?))
}

/// Ask the move running on the server to stop and put everything back.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn movedata_cancel(inst: &Instance) -> Result<(), String> {
    spm_job(inst, "move-data-cancel").map(|_| ())
}

/// The backup folder chooser on the server (crate::backups::dir_list there).
pub fn dir_list(inst: &Instance, path: &str) -> Result<crate::backups::DirList, String> {
    #[cfg(debug_assertions)]
    if let Some(secs) = std::env::var("SPM_DEBUG_REMOTE_SLOW").ok().and_then(|s| s.parse::<u64>().ok()) {
        std::thread::sleep(std::time::Duration::from_secs(secs));
    }
    #[cfg(debug_assertions)]
    if let Some(l) = debug_dirs(path) {
        return l;
    }
    read_json(inst, &format!("dir-list {}", job_arg(path)?))
}

/// Screens: canned folders, a JSON object of path → DirList
/// (SPM_DEBUG_REMOTE_DIRS names the file); a path missing there fails.
#[cfg(debug_assertions)]
fn debug_dirs(path: &str) -> Option<Result<crate::backups::DirList, String>> {
    let file = std::env::var_os("SPM_DEBUG_REMOTE_DIRS")?;
    let all: std::collections::HashMap<String, crate::backups::DirList> = serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
    Some(all.get(path).cloned().ok_or_else(|| format!("{path} cannot be opened: No such file or directory")))
}

/// Make the folder `name` in `path` on the server; the new folder, listed.
pub fn dir_mkdir(inst: &Instance, path: &str, name: &str) -> Result<crate::backups::DirList, String> {
    read_json(inst, &format!("dir-mkdir {} {}", job_arg(path)?, job_arg(name)?))
}

/// Make the folder writable for the backup job on the server (as root).
pub fn dir_make_writable(inst: &Instance, path: &str) -> Result<crate::backups::DirList, String> {
    read_json(inst, &format!("dir-make-writable {}", job_arg(path)?))
}

/// The server's processor, memory and disk now (the top bar while it is
/// open). No administrator rights needed.
pub fn monitor(inst: &Instance) -> Result<crate::sysmon::Monitor, String> {
    // Development aid: a canned reading (screens).
    #[cfg(debug_assertions)]
    if let Some(t) = std::env::var_os("SPM_DEBUG_REMOTE_MONITOR") {
        return serde_json::from_str(&t.to_string_lossy()).map_err(|e| e.to_string());
    }
    let out = if is_windows(inst) { spm_job(inst, "monitor-json")? } else { run(inst, &format!("{} [ -x $A ] || A=\"{USER_SPM}\"; \"$A\" --job monitor-json", pick_system_agent()), None)? };
    let line = out.lines().rev().find(|l| l.trim_start().starts_with('{')).ok_or("no reading")?;
    serde_json::from_str(line).map_err(|e| e.to_string())
}

/// Shell text that prints the job running on the server, if one is: a
/// systemd unit (`spm-job-<id>.service`) or a per-user one (`user-job-<id>`).
fn running_script() -> String {
    format!("systemctl list-units --type=service --state=running --no-legend --plain 'spm-job-*' 2>/dev/null | head -1; for f in \"{USER_JOBS}\"/*.pid; do [ -e \"$f\" ] && kill -0 \"$(cat \"$f\")\" 2>/dev/null && basename \"$f\" .pid | sed 's/^/user-job-/'; done 2>/dev/null | head -1")
}

/// The id of the job running on the server, if one is.
pub fn running_job(inst: &Instance) -> Option<String> {
    if is_windows(inst) {
        return win::running_job(inst);
    }
    let out = run(inst, &running_script(), None).ok()?;
    let w = out.split_whitespace().next()?;
    w.strip_prefix("spm-job-").and_then(|w| w.strip_suffix(".service")).or_else(|| w.strip_prefix("user-job-")).map(str::to_string)
}

/// Start `--job <args>` on the server, detached from this connection.
/// Returns the job's id; its progress goes to a file [`follow`] reads.
pub fn start_job(inst: &Instance, args: &[&str]) -> Result<String, String> {
    let id = format!("{}", chrono::Utc::now().format("%Y%m%d%H%M%S"));
    if is_windows(inst) {
        return win::start_job(inst, &id, args).map(|_| id);
    }
    // One job at a time on a server.
    let busy = run(inst, &running_script(), None).unwrap_or_default();
    if !busy.trim().is_empty() {
        return Err("Another job is still running on this server. Wait until it ends.".into());
    }
    let quoted: Vec<String> = args.iter().map(|a| format!("'{}'", a.replace('\'', ""))).collect();
    let system = run(inst, &format!("{} [ -x $A ] && echo \"$A\" || echo no", pick_system_agent()), None)?.trim().to_string();
    if system != "no" {
        sudo_run(
            inst,
            &format!(
                "mkdir -p {REMOTE_JOBS} && systemd-run --unit=spm-job-{id} --collect --property=StandardOutput=file:{REMOTE_JOBS}/{id}.log --property=StandardError=file:{REMOTE_JOBS}/{id}.err {system} --job --json {}",
                quoted.join(" ")
            ),
            None,
        )?;
    } else {
        // The per-user agent: detached from the connection with setsid; its
        // process id marks it running.
        run(
            inst,
            &format!(
                "mkdir -p \"{USER_JOBS}\" && cd \"{USER_JOBS}\" && (setsid nohup \"{USER_SPM}\" --job --json {} > {id}.log 2> {id}.err < /dev/null & echo $! > {id}.pid)",
                quoted.join(" ")
            ),
            None,
        )?;
    }
    Ok(id)
}

/// Follow a job's progress until it ends, reconnecting when the connection
/// drops (the job keeps running there). Blocking.
pub fn follow(inst: &Instance, id: &str, sink: &ProgressFn) -> Result<(), String> {
    if is_windows(inst) {
        return win::follow(inst, id, sink);
    }
    let mut seen = 0usize;
    let mut drops = 0u32;
    loop {
        let mut child = ssh(inst, &format!("L={REMOTE_JOBS}/{id}.log; [ -e $L ] || L=\"{USER_JOBS}/{id}.log\"; tail -n +{} -F \"$L\" 2>/dev/null & TP=$!; while systemctl is-active --quiet spm-job-{id} 2>/dev/null || kill -0 \"$(cat \"{USER_JOBS}/{id}.pid\" 2>/dev/null)\" 2>/dev/null; do sleep 2; done; sleep 2; kill $TP 2>/dev/null; echo '@@END'", seen + 1), None)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("start ssh: {e}"))?;
        let stdout = child.stdout.take().expect("piped");
        let mut ended = false;
        let mut failure: Option<String> = None;
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line == "@@END" {
                ended = true;
                break;
            }
            seen += 1;
            match serde_json::from_str::<Progress>(&line) {
                Ok(Progress::Error { message }) => failure = Some(message),
                Ok(p) => sink(p),
                Err(_) => sink(Progress::Log { line }),
            }
        }
        let _ = child.wait();
        if ended {
            return match failure {
                Some(e) => Err(e),
                None => Ok(()),
            };
        }
        drops += 1;
        sink(Progress::Log { line: format!("The connection to {} was lost; the job keeps running there. Reconnecting…", inst.name) });
        std::thread::sleep(std::time::Duration::from_secs((2 * drops as u64).min(30)));
    }
}

/// The last lines of SENTIENT's log on the server.
pub fn logs(inst: &Instance, lines: u32) -> Result<String, String> {
    spm_job(inst, &format!("logs {lines}"))
}

/// One log source on the server: the last `lines`, or since midnight with
/// `today` (crate::logsource).
pub fn logs_from(inst: &Instance, source: crate::logsource::Source, lines: u32, today: bool) -> Result<String, String> {
    #[cfg(debug_assertions)]
    if let Some(t) = std::env::var_os("SPM_DEBUG_REMOTE_LOGS") {
        return std::fs::read_to_string(std::path::Path::new(&t).join(format!("{}.log", source.key()))).map_err(|_| format!("Could not read the {} log.", source.label()));
    }
    spm_job(inst, &format!("logs {lines} --source {}{}", source.key(), if today { " --today" } else { "" }))
}

/// ClickHouse parts set aside on the server.
pub fn ch_detached(inst: &Instance) -> Result<Vec<crate::dbstatus::DetachedPart>, String> {
    read_json(inst, "ch-detached")
}

/// Restore (attach) or delete parts set aside: the agent's lines, one per part.
pub fn ch_parts(inst: &Instance, attach: bool, parts: &[(String, String)]) -> Result<String, String> {
    let args: Vec<String> = parts.iter().flat_map(|(t, p)| [t.clone(), p.clone()]).collect();
    spm_job(inst, &format!("{} {}", if attach { "ch-attach" } else { "ch-drop" }, args.join(" ")))
}

/// One part of a live reading of the server (crate::stats::Part). An agent
/// from before the parts answers the whole reading.
pub fn stats_live_part(inst: &Instance, part: crate::stats::Part) -> Result<crate::stats::Sample, String> {
    #[cfg(debug_assertions)]
    if let Some(d) = std::env::var_os("SPM_DEBUG_STATS") {
        return crate::stats::debug_part(std::path::Path::new(&d), part);
    }
    read_json(inst, &format!("stats-live {}", part.arg()))
}

/// The server's statistics history of the last `secs` seconds.
pub fn stats_history(inst: &Instance, secs: i64) -> Result<Vec<crate::stats::Sample>, String> {
    #[cfg(debug_assertions)]
    if let Some(d) = std::env::var_os("SPM_DEBUG_STATS") {
        return Ok(crate::stats::debug_history(std::path::Path::new(&d), secs));
    }
    read_json(inst, &format!("stats-history {secs}"))
}

/// The log sources the server has.
pub fn log_sources(inst: &Instance) -> Result<Vec<crate::logsource::Source>, String> {
    #[cfg(debug_assertions)]
    if std::env::var_os("SPM_DEBUG_REMOTE_LOGS").is_some() {
        return Ok(vec![crate::logsource::Source::Sentient, crate::logsource::Source::Database, crate::logsource::Source::ClickHouse]);
    }
    read_json(inst, "logs 0 --sources")
}

/// Is there a file at `path` on the server (a certificate or key for
/// Platform settings)? Looks only; nothing is read.
pub fn file_exists(inst: &Instance, path: &str) -> Result<bool, String> {
    if is_windows(inst) {
        return Ok(win::ps(inst, &format!("Test-Path -PathType Leaf '{}'", path.replace('\'', "''")))?.trim() == "True");
    }
    let quoted = format!("'{}'", path.replace('\'', "'\\''"));
    Ok(run(inst, &format!("test -f {quoted} && echo yes || echo no"), None)?.trim() == "yes")
}

/// The server runs Windows.
pub fn is_windows(inst: &Instance) -> bool {
    inst.os == Os::Windows || inst.detected.as_ref().is_some_and(|d| d.os == Os::Windows)
}

/// Run `--job <job>` with the server's Platform Manager as administrator
/// and return what it printed.
fn spm_job(inst: &Instance, job: &str) -> Result<String, String> {
    let r = if is_windows(inst) {
        // Piped, so PowerShell waits for it and reads its output even when
        // it is the windowed build (an installed Platform Manager); its
        // errors folded into the output.
        win::exe(inst).and_then(|exe| win::ps(inst, &format!("& '{}' --job {job} 2>&1 | ForEach-Object {{ \"$_\" }}", exe.replace('\'', "''"))))
    } else {
        agent_run(inst, &format!("--job {job}"))
    };
    let r = rights_answer(inst, r);
    // An agent that does not know the job answers with its usage text: that
    // text goes to the activity log, the pages say the agent is out of date.
    r.map_err(|e| {
        if e.contains("usage: --job") || e.contains("Unknown job") {
            crate::maintenance_ops::log_line(&format!("{}: the agent does not know \"{job}\" (out of date)", inst.name));
            // An installed Platform Manager in use: Update agent sets up
            // this version's own beside it.
            if reuse::found(&inst.id).is_some() {
                reuse::refuse(&inst.id);
            }
            AGENT_OLD.to_string()
        } else {
            e
        }
    })
}

/// What taking the server's SENTIENT over would do (crate::manager::takeover),
/// with a compose file picked on the server.
pub fn takeover_plan(inst: &Instance, compose: Option<&str>) -> Result<sentient_installer_core::distro::adopt::TakeOverPlan, String> {
    // Screens and click tests: a canned plan; "old" answers as an old agent
    // until the agent was updated (deploy, below), "sudo" as well but the
    // update needs sudo's password.
    #[cfg(debug_assertions)]
    if let Ok(kind) = std::env::var("SPM_DEBUG_REMOTE_TAKEOVER") {
        if (kind == "old" || kind == "sudo") && !DEBUG_AGENT_UPDATED.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(AGENT_OLD.into());
        }
        return Ok(debug_plan(compose));
    }
    let job = match compose {
        Some(c) => format!("takeover-plan --compose '{}'", c.replace('\'', "")),
        None => "takeover-plan".into(),
    };
    let out = spm_job(inst, &job)?;
    if out.lines().any(|l| l.trim() == "null") {
        return Err(NOT_ADDED.into());
    }
    let line = out.lines().rev().find(|l| l.trim_start().starts_with('{')).ok_or("The server's Platform Manager gave no plan.")?;
    serde_json::from_str(line).map_err(|e| format!("unexpected answer from the server: {e}"))
}

/// The plan, updating an agent too old to answer it first (the same update
/// as the Status page's Update agent), then asking again.
pub fn takeover_plan_updating(inst: &Instance, compose: Option<&str>) -> Result<sentient_installer_core::distro::adopt::TakeOverPlan, String> {
    let sink: ProgressFn = std::sync::Arc::new(|_| {});
    after_agent_update(|| takeover_plan(inst, compose), || deploy(inst, &sink))
}

/// Ask; when the agent is too old for it, update the agent and ask once more.
pub(crate) fn after_agent_update<T>(mut ask: impl FnMut() -> Result<T, String>, update: impl FnOnce() -> Result<(), String>) -> Result<T, String> {
    match ask() {
        Err(e) if e == AGENT_OLD => {
            update().map_err(|e| {
                let (head, details) = split_failure(&e);
                failed(&format!("The server agent is out of date and could not be updated first: {head}"), details.as_deref().unwrap_or(""))
            })?;
            ask()
        }
        r => r,
    }
}

/// A password for the server's agent, in a file only the account reads
/// (never on a command line); the agent removes it when it read it.
pub fn put_secret_file(inst: &Instance, text: &str) -> Result<String, String> {
    let file = format!("{}/.spm-db-pw-{}", home(inst)?.trim_end_matches('/'), chrono::Utc::now().timestamp_millis());
    put_text(inst, &file, text)?;
    Ok(file)
}

/// Whether the server's PostgreSQL takes `password` (the takeover dialog).
pub fn check_takeover_password(inst: &Instance, ch: bool, password: &str) -> Result<(), String> {
    let file = put_secret_file(inst, password)?;
    let r = spm_job(inst, &format!("takeover-check-password{} --db-password-file '{}'", if ch { " --clickhouse" } else { "" }, file.replace('\'', ""))).map(|_| ());
    remove_file(inst, &file);
    r
}

/// One call to SENTIENT's licence functions on the server, made there by
/// its agent (crate::licence::agent_job). The request (it may hold the
/// sign-in token or password) goes in a file only the account reads; the
/// agent removes it once read, and so does this, in case it never ran.
pub fn licence_api(inst: &Instance, req: &crate::licence::Request) -> Result<crate::licence::Reply, String> {
    let text = serde_json::to_string(req).map_err(|e| e.to_string())?;
    let file = format!("{}/.spm-licence-{}", home(inst)?.trim_end_matches('/'), chrono::Utc::now().timestamp_millis());
    put_text(inst, &file, &text)?;
    let out = spm_job(inst, &format!("licence-api --request-file '{}'", file.replace('\'', "")));
    remove_file(inst, &file);
    let out = out?;
    let line = out.lines().rev().find(|l| l.trim_start().starts_with('{')).ok_or_else(|| no_answer(&out))?;
    serde_json::from_str(line).map_err(|e| format!("unexpected answer from the server: {e}"))
}

/// Whether a migration could start on the server now (crate::migration::
/// preflight). An older agent checks this itself when the migration starts.
pub fn migrate_preflight(inst: &Instance) -> Result<(), String> {
    match spm_job(inst, "migrate-preflight") {
        Ok(_) => Ok(()),
        Err(e) if e == AGENT_OLD => Ok(()),
        Err(e) => Err(e),
    }
}

/// The agent on the server answered a job it does not know: it is older
/// than this Platform Manager. Pages show one notice with Update agent.
pub const AGENT_OLD: &str = "The agent on this server is out of date.";

/// Delete a file on the server (the settings file after applying).
pub fn remove_file(inst: &Instance, path: &str) {
    let _ = if is_windows(inst) {
        win::ps(inst, &format!("Remove-Item -Force -ErrorAction SilentlyContinue '{}'", path.replace('\'', "''")))
    } else {
        run(inst, &format!("rm -f '{}'", path.replace('\'', "")), None)
    };
}

/// An error in two parts: a plain headline, and the technical details the
/// screen shows behind "Show details" (crate::manager::instances).
/// A job's failure as a plain headline and its technical details: split at
/// DETAILS, or, for a raw command error, a plain headline with the raw text
/// as the details.
pub fn split_failure(m: &str) -> (String, Option<String>) {
    if let Some((h, d)) = m.split_once(DETAILS) {
        return (h.to_string(), Some(d.to_string()));
    }
    let raw = m.contains("docker ") || m.contains("exit status") || m.contains("exited") || m.lines().count() > 2 || m.len() > 200;
    if !raw {
        return (m.to_string(), None);
    }
    let headline = if m.contains("no such file") || m.contains("not found") && m.contains(".yml") {
        "A compose file SENTIENT's stack needs is missing."
    } else if m.contains("permission denied") || m.contains("Permission denied") {
        "The server refused the action: not enough rights."
    } else {
        "The action did not finish on the server."
    };
    (headline.to_string(), Some(m.to_string()))
}

pub fn failed(headline: &str, details: &str) -> String {
    if details.trim().is_empty() { headline.to_string() } else { format!("{headline}{DETAILS}{}", details.trim()) }
}

/// Where [`failed`] splits the headline from the details.
pub const DETAILS: &str = "\n\nDetails: ";

/// A server's processor, for its Linux agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
}

/// The system a server agent is built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum System {
    Linux(Arch),
    Windows,
}

impl System {
    /// How the messages name it.
    pub fn label(self) -> &'static str {
        match self {
            System::Linux(Arch::X86_64) => "Linux (x86_64)",
            System::Linux(Arch::Aarch64) => "Linux (ARM64)",
            System::Windows => "Windows",
        }
    }
}

/// The agent's file name for a server's system (as the release publishes it).
pub fn agent_name(system: System) -> &'static str {
    match system {
        System::Linux(Arch::X86_64) => "sentient-platform-manager-agent-linux-x86_64",
        System::Linux(Arch::Aarch64) => "sentient-platform-manager-agent-linux-aarch64",
        System::Windows => "sentient-platform-manager-agent-windows-x86_64.exe",
    }
}

/// Why no server agent could be had for a system: this package does not
/// carry it, and the download (the fallback, for published releases) did
/// not give one either. Its text names the cause and what to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoAgent {
    /// This version is not a published release (a test or local build).
    NotPublished { system: System, version: String },
    /// The release is published without this system's agent.
    NotInRelease { system: System, version: String },
    /// The download failed (no connection, the release site refused…).
    Download { system: System, version: String, error: String },
    /// What was downloaded does not match its published checksum.
    Checksum { system: System, version: String },
}

impl std::fmt::Display for NoAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let fix = |s: &System| format!("Install the Platform Manager on that computer, or use a Platform Manager package that includes the {} agent.", s.label());
        match self {
            NoAgent::NotPublished { system, version } => write!(
                f,
                "This package has no {} agent, and this build ({version}) is not a published release, so none can be downloaded. {}",
                system.label(),
                fix(system)
            ),
            NoAgent::NotInRelease { system, version } => write!(f, "This package has no {} agent, and the published release {version} has none either. {}", system.label(), fix(system)),
            NoAgent::Download { system, version, .. } => write!(
                f,
                "This package has no {} agent, and the one published with {version} could not be downloaded. Check this computer's internet connection and try again. {}",
                system.label(),
                fix(system)
            ),
            NoAgent::Checksum { system, version } => write!(
                f,
                "This package has no {} agent, and the one downloaded for {version} does not match its published checksum, so it was not used. Try again. {}",
                system.label(),
                fix(system)
            ),
        }
    }
}

impl NoAgent {
    /// The message with the technical details behind "Show details".
    pub fn message(&self) -> String {
        let details = match self {
            NoAgent::Download { error, .. } => error.clone(),
            NoAgent::NotPublished { system, version } | NoAgent::NotInRelease { system, version } | NoAgent::Checksum { system, version } => {
                format!("{} for Platform Manager {version}", agent_name(*system))
            }
        };
        failed(&self.to_string(), &details)
    }
}

/// The agent this build carries for `system` (build.rs).
fn embedded_agent(system: System) -> Option<&'static [u8]> {
    match system {
        System::Linux(Arch::X86_64) => embedded::LINUX,
        System::Linux(Arch::Aarch64) => embedded::LINUX_AARCH64,
        System::Windows => embedded::WINDOWS,
    }
}

/// The server agent for a system (Linux static, with no desktop
/// libraries; Windows): the one beside the program (`agents/`), else the one
/// this program carries, else, for a published release, the one published
/// with this version, checked against its SHA-256.
fn agent_file(system: System, sink: &ProgressFn) -> Result<PathBuf, String> {
    let name = agent_name(system);
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = std::env::var_os("SPM_AGENT_DIR") {
        dirs.push(PathBuf::from(d));
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())) {
        dirs.push(dir.join("agents"));
        dirs.push(dir.join("..").join("lib").join("sentient-platform-manager").join("agents"));
    }
    if let Some(found) = dirs.into_iter().map(|d| d.join(name)).find(|p| p.is_file()) {
        return Ok(found);
    }
    // Carried inside this program: written out once per version.
    if let Some(bytes) = embedded_agent(system) {
        return write_out(name, bytes);
    }
    #[cfg(debug_assertions)]
    if std::env::var_os("SPM_DEBUG_NO_DOWNLOAD").is_some() {
        return Err(NoAgent::NotPublished { system, version: crate::updates::manager_version().to_string() }.message());
    }
    released_agent(system, sink).map_err(|e| e.message())
}

/// The agents this program carries, each written out (`--job agents`).
pub fn carried_agents() -> Vec<(System, Result<Option<PathBuf>, String>)> {
    [System::Linux(Arch::X86_64), System::Linux(Arch::Aarch64), System::Windows]
        .into_iter()
        .map(|s| (s, embedded_agent(s).map(|b| write_out(agent_name(s), b)).transpose()))
        .collect()
}

/// Write a carried agent out (crate::updates::prefs_dir/remote-builds), once
/// per version and build.
fn write_out(name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let dir = crate::updates::prefs_dir().join("remote-builds");
    let file = dir.join(format!("{}-{}-{name}", crate::updates::manager_version(), crate::updates::build_commit()));
    if std::fs::metadata(&file).map(|m| m.len() as usize).ok() != Some(bytes.len()) {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(&file, bytes).map_err(|e| format!("write {}: {e}", file.display()))?;
    }
    Ok(file)
}

/// The agents this build carries (build.rs).
mod embedded {
    include!(concat!(env!("OUT_DIR"), "/agents.rs"));
}

/// The agent published with this Platform Manager's version.
fn released_agent(system: System, sink: &ProgressFn) -> Result<PathBuf, NoAgent> {
    use sentient_installer_core::releases;
    let name = agent_name(system);
    let version = crate::updates::manager_version().to_string();
    let download = |error: String| NoAgent::Download { system, version: version.clone(), error };
    let dir = crate::updates::prefs_dir().join("remote-builds");
    std::fs::create_dir_all(&dir).map_err(|e| download(e.to_string()))?;
    sink(Progress::Step { name: format!("Getting the server agent of Platform Manager {version}") });
    let all = releases::fetch(releases::MANAGER_REPO).map_err(download)?;
    let r = all
        .iter()
        .find(|r| releases::is_manager(r) && releases::manager_version_of(r) == version)
        .ok_or_else(|| NoAgent::NotPublished { system, version: version.clone() })?;
    let asset = r.assets.iter().find(|a| a.name == name).ok_or_else(|| NoAgent::NotInRelease { system, version: version.clone() })?;
    let sha = crate::updates::published_sha256(r, asset).ok_or_else(|| NoAgent::Checksum { system, version: version.clone() })?;
    let file = dir.join(format!("{version}-{name}"));
    let ok = |f: &Path| sentient_installer_core::manifest::sha256_file(f).is_ok_and(|h| h.eq_ignore_ascii_case(&sha));
    if !ok(&file) {
        let resp = sentient_installer_core::net::agent()
            .get(&asset.browser_download_url)
            .set("User-Agent", "SENTIENT-Platform-Manager")
            .call()
            .map_err(|e| download(format!("download {name}: {e}")))?;
        let mut out = std::fs::File::create(&file).map_err(|e| download(e.to_string()))?;
        std::io::copy(&mut resp.into_reader(), &mut out).map_err(|e| download(format!("download {name}: {e}")))?;
        if !ok(&file) {
            let _ = std::fs::remove_file(&file);
            return Err(NoAgent::Checksum { system, version });
        }
    }
    Ok(file)
}


/// Windows servers: OpenSSH Server, commands in PowerShell, the Platform
/// Manager under ProgramData, jobs as scheduled tasks running as SYSTEM
/// (they outlive the connection), progress in a file polled from here.
mod win {
    use super::*;

    pub const DIR: &str = r"C:\ProgramData\SENTIENT\Platform Manager\remote";
    pub const EXE: &str = r"C:\ProgramData\SENTIENT\Platform Manager\remote\sentient-platform-manager.exe";
    const JOBS: &str = r"C:\ProgramData\SENTIENT\Platform Manager\remote\jobs";

    /// The program each Windows instance's jobs run with, as last resolved
    /// (crate::remote::resolve): this Platform Manager's own copy at EXE,
    /// or the installed one.
    static AGENT_EXE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

    pub fn set_exe(id: &str, agent: &Agent) {
        let exe = match agent {
            Agent::Own => EXE.to_string(),
            Agent::Installed(path) => path.clone(),
        };
        AGENT_EXE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(id.to_string(), exe);
    }

    /// The program to run jobs with (resolved once, then remembered).
    pub fn exe(inst: &Instance) -> Result<String, String> {
        let known = AGENT_EXE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&inst.id).cloned());
        match known {
            Some(exe) => Ok(exe),
            None => resolve(inst).map(|a| match a {
                Agent::Own => EXE.to_string(),
                Agent::Installed(path) => path,
            }),
        }
    }

    /// Look for this Platform Manager's own agent, else an installed
    /// Platform Manager, and ask that one what it is (one connection).
    pub fn probe(inst: &Instance) -> Result<reuse::Probe, String> {
        let out = ps(
            inst,
            &format!(
                "if (-not ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole('Administrators')) {{ '@@NOTADMIN'; exit 0 }}; \
                 if (Test-Path -PathType Leaf '{EXE}') {{ '@@OWN' }} else {{ {}; \
                 if ($i) {{ '@@INSTALLED ' + $i; & $i --job agent-info 2>&1 | ForEach-Object {{ \"$_\" }} }} else {{ '@@NONE' }} }}; exit 0",
                reuse::windows_find_script()
            ),
        )?;
        if out.lines().any(|l| l.trim() == "@@NOTADMIN") || not_elevated(&out) {
            return Err(not_admin(inst));
        }
        Ok(reuse::parse_probe(&out))
    }

    /// Run a PowerShell script on the server (encoded, so no quoting
    /// passes through cmd).
    pub fn ps(inst: &Instance, script: &str) -> Result<String, String> {
        use base64::Engine as _;
        let utf16: Vec<u8> = format!("$ProgressPreference='SilentlyContinue'; {script}").encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let enc = base64::engine::general_purpose::STANDARD.encode(utf16);
        run(inst, &format!("powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand {enc}"), None)
    }

    pub fn deploy(inst: &Instance, sink: &ProgressFn) -> Result<(), String> {
        let file = agent_file(System::Windows, sink)?;
        let want = sentient_installer_core::manifest::sha256_file(&file)?;
        let have = ps(inst, &format!("if (Test-Path '{EXE}') {{ (Get-FileHash -Algorithm SHA256 '{EXE}').Hash }}")).unwrap_or_default();
        if have.trim().eq_ignore_ascii_case(&want) {
            sink(Progress::Log { line: "The Platform Manager on the server is up to date.".into() });
            return Ok(());
        }
        sink(Progress::Step { name: "Copying the Platform Manager to the server".into() });
        ps(inst, &format!("New-Item -ItemType Directory -Force '{JOBS}' | Out-Null"))?;
        let upload = format!("{DIR}\\upload.exe");
        let sftp_path = format!("/{}", upload.replace('\\', "/"));
        // A leftover upload is used when it is this build, else replaced.
        let staged = ps(inst, &format!("if (Test-Path '{upload}') {{ (Get-FileHash -Algorithm SHA256 '{upload}').Hash }}")).unwrap_or_default();
        if !staged.trim().eq_ignore_ascii_case(&want) {
            let _ = ps(inst, &format!("Remove-Item -Force -ErrorAction SilentlyContinue '{upload}'"));
            super::sftp_copy(inst, &file, &sftp_path).map_err(|e| super::failed("The agent could not be copied to the server. Nothing on the server was changed.", &e))?;
        }
        let got = ps(inst, &format!("(Get-FileHash -Algorithm SHA256 '{upload}').Hash"))?;
        if !got.trim().eq_ignore_ascii_case(&want) {
            let _ = ps(inst, &format!("Remove-Item -Force -ErrorAction SilentlyContinue '{upload}'"));
            return Err("The copy on the server does not match (checksum); nothing was replaced. Try again.".into());
        }
        ps(inst, &format!("Move-Item -Force '{upload}' '{EXE}'")).map_err(|e| super::failed("The agent could not be put in place. The agent there is unchanged.", &e))?;
        set_exe(&inst.id, &Agent::Own);
        if reuse::found(&inst.id).is_some() {
            sink(Progress::Log { line: format!("This version's agent is set up beside the Platform Manager installed on the server, at {EXE}; the installed one is not changed.") });
        }
        Ok(())
    }

    pub fn status(inst: &Instance) -> Result<RemoteStatus, String> {
        let exe = match resolve(inst)? {
            Agent::Own => EXE.to_string(),
            Agent::Installed(path) => path,
        };
        let out = ps(inst, &format!("& '{}' --job status-json 2>&1 | ForEach-Object {{ \"$_\" }}", exe.replace('\'', "''")))?;
        if not_elevated(&out) {
            return Err(not_admin(inst));
        }
        let line = out.lines().rev().find(|l| l.trim_start().starts_with('{')).ok_or("The server's Platform Manager gave no status.")?;
        let mut st: RemoteStatus = serde_json::from_str(line).map_err(|e| format!("unexpected status from the server: {e}"))?;
        st.running_job = running_job(inst);
        Ok(st)
    }

    pub fn running_job(inst: &Instance) -> Option<String> {
        let out = ps(inst, "Get-ScheduledTask -TaskName 'spm-job-*' -ErrorAction SilentlyContinue | Where-Object State -eq 'Running' | Select-Object -First 1 -ExpandProperty TaskName").ok()?;
        out.trim().strip_prefix("spm-job-").map(str::to_string)
    }

    pub fn start_job(inst: &Instance, id: &str, args: &[&str]) -> Result<(), String> {
        let quoted: Vec<String> = args.iter().map(|a| format!("\"{}\"", a.replace(['"', '\''], ""))).collect();
        let exe = exe(inst)?;
        let cmdline = format!("/c \"\"{exe}\" --job --json {} > \"{JOBS}\\{id}.log\" 2> \"{JOBS}\\{id}.err\"\"", quoted.join(" "));
        let script = format!(
            "New-Item -ItemType Directory -Force '{JOBS}' | Out-Null; \
             if (Get-ScheduledTask -TaskName 'spm-job-*' -ErrorAction SilentlyContinue | Where-Object State -eq 'Running') {{ 'BUSY'; exit }}; \
             $a = New-ScheduledTaskAction -Execute 'cmd.exe' -Argument '{}'; \
             $p = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest; \
             Register-ScheduledTask -TaskName 'spm-job-{id}' -Action $a -Principal $p -Force | Out-Null; \
             Start-ScheduledTask -TaskName 'spm-job-{id}'; 'STARTED'",
            cmdline.replace('\'', "''")
        );
        let out = ps(inst, &script)?;
        if out.contains("BUSY") {
            return Err("Another job is still running on this server. Wait until it ends.".into());
        }
        if !out.contains("STARTED") {
            return Err(format!("The job did not start on the server: {}", out.trim()));
        }
        Ok(())
    }

    /// Poll the job's progress file until its task stops; a dropped
    /// connection only delays the next poll.
    pub fn follow(inst: &Instance, id: &str, sink: &ProgressFn) -> Result<(), String> {
        let mut seen = 0usize;
        let mut failure: Option<String> = None;
        let mut drops = 0u32;
        let read = |skip: usize| {
            ps(
                inst,
                &format!(
                    "$f='{JOBS}\\{id}.log'; if (Test-Path $f) {{ Get-Content -Encoding UTF8 $f | Select-Object -Skip {skip} }}; \
                     $t = Get-ScheduledTask -TaskName 'spm-job-{id}' -ErrorAction SilentlyContinue; \
                     if ($t -and $t.State -eq 'Running') {{ '@@RUNNING' }} else {{ '@@END' }}"
                ),
            )
        };
        loop {
            match read(seen) {
                Ok(out) => {
                    drops = 0;
                    let mut ended = false;
                    for line in out.lines() {
                        match line.trim_end() {
                            "@@RUNNING" => {}
                            "@@END" => ended = true,
                            l => {
                                seen += 1;
                                match serde_json::from_str::<Progress>(l) {
                                    Ok(Progress::Error { message }) => failure = Some(message),
                                    Ok(p) => sink(p),
                                    Err(_) => sink(Progress::Log { line: l.to_string() }),
                                }
                            }
                        }
                    }
                    if ended {
                        // What it wrote between the read and the end.
                        if let Ok(rest) = read(seen) {
                            for l in rest.lines().filter(|l| !l.starts_with("@@")) {
                                match serde_json::from_str::<Progress>(l) {
                                    Ok(Progress::Error { message }) => failure = Some(message),
                                    Ok(p) => sink(p),
                                    Err(_) => sink(Progress::Log { line: l.to_string() }),
                                }
                            }
                        }
                        let _ = ps(inst, &format!("Unregister-ScheduledTask -TaskName 'spm-job-{id}' -Confirm:$false -ErrorAction SilentlyContinue"));
                        return match failure {
                            Some(e) => Err(e),
                            None => Ok(()),
                        };
                    }
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
                Err(_) => {
                    drops += 1;
                    sink(Progress::Log { line: format!("The connection to {} was lost; the job keeps running there. Reconnecting…", inst.name) });
                    std::thread::sleep(std::time::Duration::from_secs((2 * drops as u64).min(30)));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_stays_readable_on_any_colour() {
        assert_eq!(text_on("#187074"), 0xffffff);
        assert_eq!(text_on("#6b4fa0"), 0xffffff);
        assert_eq!(text_on("#f5e663"), 0x1f2328);
        assert_eq!(parse_hex("#a1B2c3"), Some(0xa1b2c3));
        assert_eq!(parse_hex("red"), None);
    }

    /// Against a scratch sshd: SPM_TEST_SSH="host port user keyfile passphrase",
    /// SPM_TEST_ASKPASS a script printing $SPM_ASKPASS_VALUE, XDG_CONFIG_HOME a scratch folder.
    #[test]
    #[ignore]
    fn connects_checks_the_host_key_and_detects_the_system() {
        let spec = std::env::var("SPM_TEST_SSH").expect("SPM_TEST_SSH");
        let v: Vec<&str> = spec.split_whitespace().collect();
        let port: u16 = v[1].parse().unwrap();
        let _ = std::fs::remove_file(known_hosts());
        let mut inst = Instance {
            id: "t1".into(),
            name: "lab".into(),
            color: PALETTE[0].into(),
            os: Os::Linux,
            host: v[0].into(),
            port,
            user: v[2].into(),
            auth: Auth::Key,
            key_file: Some(v[3].into()),
            key_has_passphrase: true,
            host_key: None,
            detected: None,
        };
        // Not trusted yet: refused.
        let e = run(&inst, "true", Some(v[4])).unwrap_err();
        assert!(e.contains("not the one accepted") || e.contains("Host key"), "{e}");
        let (keys, fp) = scan(v[0], port).unwrap();
        assert!(fp.starts_with("SHA256:"), "{fp}");
        trust(&keys).unwrap();
        inst.host_key = Some(fp);
        assert_eq!(run(&inst, "echo hi", Some(v[4])).unwrap().trim(), "hi");
        // Wrong passphrase: refused plainly.
        assert!(run(&inst, "true", Some("wrong")).unwrap_err().contains("refused the sign-in"));
        let d = detect(&inst, Some(v[4])).unwrap();
        assert_eq!(d.os, Os::Linux);
        assert!(!d.arch.is_empty() && !d.os_name.is_empty(), "{d:?}");
        eprintln!("{d:?}");
        // Copying: a first copy, then a copy cut off halfway and resumed.
        set_secret(&inst.id, v[4]);
        let dir = std::env::temp_dir().join(format!("spm-sftp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("big.bin");
        let data: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&src, &data).unwrap();
        let dst = dir.join("copy.bin");
        let _ = std::fs::remove_file(&dst);
        sftp_put(&inst, &src, &dst.display().to_string()).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), data);
        std::fs::write(&dst, &data[..1_000_000]).unwrap();
        sftp_put(&inst, &src, &dst.display().to_string()).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), data);
        let _ = std::fs::remove_dir_all(&dir);
        // A changed key afterwards is refused.
        std::fs::write(known_hosts(), keys.replace("AAAA", "AAAB")).unwrap();
        assert!(run(&inst, "true", Some(v[4])).unwrap_err().contains("not the one accepted"));
        untrust(v[0], port);
    }

    /// End to end against a bare server: SPM_E2E="port", SPM_E2E_KEY,
    /// SPM_E2E_SUDO, SPM_AGENT_DIR (the static agent), XDG_CONFIG_HOME scratch.
    #[test]
    #[ignore]
    fn sets_up_a_bare_server_and_runs_a_job() {
        let port: u16 = std::env::var("SPM_E2E").expect("SPM_E2E").parse().unwrap();
        let _ = std::fs::remove_file(known_hosts());
        let mut inst = Instance {
            id: format!("e2e{port}"),
            name: format!("e2e {port}"),
            color: PALETTE[0].into(),
            os: Os::Linux,
            host: "127.0.0.1".into(),
            port,
            user: "ops".into(),
            auth: Auth::Key,
            key_file: Some(std::env::var("SPM_E2E_KEY").unwrap().into()),
            key_has_passphrase: false,
            host_key: None,
            detected: None,
        };
        let (keys, fp) = scan("127.0.0.1", port).unwrap();
        trust(&keys).unwrap();
        inst.host_key = Some(fp);
        let d = detect(&inst, None).unwrap();
        eprintln!("detected: {d:?}");
        inst.detected = Some(d);
        assert_eq!(ready(&inst).unwrap_err(), NOT_SET_UP);
        let sink: ProgressFn = std::sync::Arc::new(|p| eprintln!("  {p:?}"));
        // sudo asks for a password: without one kept, Set up asks for it
        // (crate::manager::remote_pages::ask_sudo_password) and changes nothing;
        // a wrong one is refused the same way.
        forget_one(&sudo_key(&inst.id));
        let e = deploy(&inst, &sink).unwrap_err();
        assert!(e.contains("needs administrator rights"), "{e}");
        set_secret(&sudo_key(&inst.id), "wrong-password");
        let e = deploy(&inst, &sink).unwrap_err();
        assert!(e.contains("sudo refused"), "{e}");
        assert_eq!(ready(&inst).unwrap_err(), NOT_SET_UP);
        set_secret(&sudo_key(&inst.id), &std::env::var("SPM_E2E_SUDO").unwrap());
        deploy(&inst, &sink).unwrap();
        ready(&inst).unwrap();
        let st = status(&inst).unwrap();
        eprintln!("status: {st:?}");
        assert!(!st.installed);
        let m = monitor(&inst).unwrap();
        assert!(m.mem_total > 0);
        // Deploying again finds it up to date.
        deploy(&inst, &sink).unwrap();
        // A staging file left by an earlier attempt never blocks a retry:
        // the complete one is used, a partial one is replaced.
        let me = agent_file(System::Linux(Arch::X86_64), &sink).unwrap();
        let want = sentient_installer_core::manifest::sha256_file(&me).unwrap();
        let home = home(&inst).unwrap();
        let staging = format!("{}/.spm-upload-{}", home.trim_end_matches('/'), &want[..12]);
        for partial in [false, true] {
            sftp_copy(&inst, &me, &staging).unwrap();
            if partial {
                run(&inst, &format!("truncate -s 100000 {staging}"), None).unwrap();
            }
            // An older agent in place, so the deploy has to copy.
            sudo_run(&inst, &format!("sh -c 'printf old > {REMOTE_SPM}'"), None).unwrap();
            deploy(&inst, &sink).unwrap_or_else(|e| panic!("retry with a {} staging file: {e}", if partial { "partial" } else { "complete" }));
            let there = run(&inst, &format!("sha256sum {REMOTE_SPM} | cut -d' ' -f1"), None).unwrap();
            assert_eq!(there.trim(), want);
            assert!(run(&inst, &format!("test -e {staging} && echo left || echo gone"), None).unwrap().contains("gone"), "the staging file is removed");
        }
        // Statistics: the sampler came with the agent; readings land in the
        // history and read back; a live reading answers.
        assert_eq!(run(&inst, "systemctl is-enabled sentient-pm-stats.timer", None).unwrap().trim(), "enabled");
        spm_job(&inst, "stats-sample").unwrap();
        spm_job(&inst, "stats-sample").unwrap();
        let h = stats_history(&inst, 3600).unwrap();
        assert!(h.len() >= 2, "{} readings", h.len());
        assert!(h.iter().all(|s| s.mem_total > 0 && s.cores > 0), "{h:?}");
        let live = stats_live_part(&inst, crate::stats::Part::System).unwrap();
        assert!(live.mem_total > 0 && live.boot > 0, "{live:?}");
        assert!(live.services.is_empty() && live.db.is_none(), "the system part alone: {live:?}");
        eprintln!("stats: {} readings, live cpu {:.1} %, {} cores", h.len(), live.cpu, live.cores);
        // Opening a server with an older agent on it (crate::manager::
        // remote_pages::refresh_agent: ready, then deploy) replaces it.
        sudo_run(&inst, &format!("sh -c 'printf old-agent > {REMOTE_SPM}'"), None).unwrap();
        assert!(ready(&inst).is_ok());
        deploy(&inst, &sink).unwrap();
        let there = run(&inst, &format!("sha256sum {REMOTE_SPM} | cut -d' ' -f1"), None).unwrap();
        let here = sentient_installer_core::manifest::sha256_file(&agent_file(System::Linux(Arch::X86_64), &sink).unwrap()).unwrap();
        assert_eq!(there.trim(), here, "the older agent was replaced");
        // A detached job, followed to its end.
        let id = start_job(&inst, &["find-existing"]).unwrap();
        follow(&inst, &id, &sink).unwrap();
        assert!(running_job(&inst).is_none());
        let e = logs(&inst, 10).unwrap_err();
        eprintln!("logs without SENTIENT: {e}");
        // Nothing added: a calm "not added", not a failure.
        assert_eq!(services(&inst).unwrap_err(), NOT_ADDED);
        assert_eq!(backups(&inst).unwrap_err(), NOT_ADDED);
        assert!(!status(&inst).unwrap().installed);
        // After Set up: no SENTIENT there, so nothing is added and it says so.
        let id = start_job(&inst, &["add-if-one"]).unwrap();
        let said = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let s2 = said.clone();
        let collect: ProgressFn = std::sync::Arc::new(move |p| {
            if let Progress::Done { message } = p {
                s2.lock().unwrap().push(message);
            }
        });
        follow(&inst, &id, &collect).unwrap();
        let said = said.lock().unwrap().join(" ");
        eprintln!("add-if-one: {said}");
        assert!(said.contains("No SENTIENT found"), "{said}");
        backup_jobs_on_the_server(&inst, &sink);
        let _ = std::fs::remove_file(known_hosts());
    }

    /// The plan the agent prints (headless takeover-plan) reads back here
    /// (crate::remote::takeover_plan), files with their found flags.
    #[test]
    fn takeover_plan_reads_back() {
        use sentient_installer_core::distro::adopt::TakeOverPlan;
        let p = TakeOverPlan { project: "p".into(), files: vec![("/a/compose.yml".into(), false)], env_files: vec![("/a/.env".into(), true)], loopback_port: Some(15432), ..Default::default() };
        let out = format!("some log line\n{}\n", serde_json::to_string(&p).unwrap());
        let line = out.lines().rev().find(|l| l.trim_start().starts_with('{')).unwrap();
        let back: TakeOverPlan = serde_json::from_str(line).unwrap();
        assert_eq!(back.files, p.files);
        assert_eq!(back.env_files, p.env_files);
        assert_eq!(back.loopback_port, Some(15432));
    }

    /// The takeover button on a server: a current agent answers at once; an
    /// old one is updated first and then answers; a failed update says so
    /// (with sudo's message, which the dialog turns into the password prompt).
    #[test]
    fn an_old_agent_is_updated_before_the_plan() {
        use std::cell::Cell;
        // Current agent: asked once, never updated.
        let current = Cell::new(0);
        assert_eq!(after_agent_update(|| { current.set(current.get() + 1); Ok::<_, String>("plan") }, || panic!("no update for a current agent")), Ok("plan"));
        assert_eq!(current.get(), 1);
        // Old agent: updated, then asked again.
        let asked = Cell::new(0);
        let updated = Cell::new(false);
        let ask = || {
            asked.set(asked.get() + 1);
            if updated.get() { Ok("plan") } else { Err(AGENT_OLD.to_string()) }
        };
        assert_eq!(after_agent_update(ask, || { updated.set(true); Ok(()) }), Ok("plan"));
        assert_eq!(asked.get(), 2);
        // The update fails: the reason, nothing asked again.
        let e = after_agent_update(|| Err::<&str, _>(AGENT_OLD.to_string()), || Err("ops needs administrator rights on the server: sudo asks for a password.".into())).unwrap_err();
        assert!(e.contains("could not be updated first") && e.contains("needs administrator rights"), "{e}");
        #[cfg(feature = "gui")]
        assert!(crate::manager::remote_pages::needs_sudo_password(&e));
        // Another failure passes through untouched.
        assert_eq!(after_agent_update(|| Err::<&str, _>("ssh: timeout".into()), || panic!()), Err("ssh: timeout".to_string()));
    }

    #[test]
    fn tells_what_sudo_allows() {
        assert_eq!(classify_admin("GROUPS ops sudo\nADMIN_OK\n", "", true), Admin::Yes);
        assert_eq!(classify_admin("NO_SUDO\n", "", false), Admin::No);
        assert_eq!(classify_admin("GROUPS solo\n", "sudo: a password is required", false), Admin::No);
        assert!(matches!(classify_admin("GROUPS ops sudo\n", "sudo: a password is required", false), Admin::NeedsPassword(_)));
        assert!(matches!(classify_admin("GROUPS solo\n", "Sorry, try again.", true), Admin::NeedsPassword(_)));
        assert_eq!(classify_admin("GROUPS solo\n", "solo is not in the sudoers file.", true), Admin::No);
    }

    /// An account without sudo (SPM_E2E_NOSUDO="user", same key): the agent
    /// goes in its home, its jobs run as it, a job that needs root says so.
    #[test]
    #[ignore]
    fn sets_up_for_an_account_without_sudo() {
        let port: u16 = std::env::var("SPM_E2E").expect("SPM_E2E").parse().unwrap();
        let user = std::env::var("SPM_E2E_NOSUDO").unwrap_or_else(|_| "solo".into());
        let _ = std::fs::remove_file(known_hosts());
        let mut inst = Instance {
            id: format!("e2e-nosudo{port}"),
            name: format!("e2e no sudo {port}"),
            color: PALETTE[0].into(),
            os: Os::Linux,
            host: "127.0.0.1".into(),
            port,
            user,
            auth: Auth::Key,
            key_file: Some(std::env::var("SPM_E2E_KEY").unwrap().into()),
            key_has_passphrase: false,
            host_key: None,
            detected: None,
        };
        let (keys, fp) = scan("127.0.0.1", port).unwrap();
        trust(&keys).unwrap();
        inst.host_key = Some(fp);
        inst.detected = Some(detect(&inst, None).unwrap());
        forget_one(&sudo_key(&inst.id));
        let _ = run(&inst, &format!("rm -rf \"{USER_DIR}\""), None);
        assert_eq!(probe_admin(&inst), Admin::No);
        assert_eq!(ready(&inst).unwrap_err(), NOT_SET_UP);
        let sink: ProgressFn = std::sync::Arc::new(|p| eprintln!("  {p:?}"));
        deploy(&inst, &sink).unwrap();
        ready(&inst).unwrap();
        assert!(run(&inst, &format!("test -x \"{USER_SPM}\" && echo yes"), None).unwrap().contains("yes"));
        let st = status(&inst).unwrap();
        assert!(!st.installed);
        assert!(monitor(&inst).unwrap().mem_total > 0);
        // Up to date the second time.
        deploy(&inst, &sink).unwrap();
        // A detached job as the account, followed to its end.
        let id = start_job(&inst, &["find-existing"]).unwrap();
        follow(&inst, &id, &sink).unwrap();
        assert!(running_job(&inst).is_none());
        // A long one shows as running while it runs.
        let id = run(&inst, &format!("mkdir -p \"{USER_JOBS}\" && cd \"{USER_JOBS}\" && (setsid nohup sleep 6 >/dev/null 2>&1 < /dev/null & echo $! > e2esleep.pid) && echo e2esleep"), None).unwrap();
        assert_eq!(running_job(&inst).as_deref(), Some(id.trim()));
        assert!(start_job(&inst, &["find-existing"]).unwrap_err().contains("Another job"));
        std::thread::sleep(std::time::Duration::from_secs(7));
        assert!(running_job(&inst).is_none());
        // A job refused for want of root (Docker's socket, a root-owned
        // folder) says so: a stand-in agent that fails like one.
        run(&inst, &format!("cp \"{USER_SPM}\" \"{USER_SPM}.real\" && printf '#!/bin/sh\\necho \"Got permission denied while trying to connect to the Docker daemon socket\" >&2\\nexit 1\\n' > \"{USER_SPM}\""), None).unwrap();
        let e = spm_job(&inst, "services-json").unwrap_err();
        eprintln!("root-only job as {}: {e}", inst.user);
        assert!(e.contains("takes root"), "{e}");
        run(&inst, &format!("mv \"{USER_SPM}.real\" \"{USER_SPM}\""), None).unwrap();
        ready(&inst).unwrap();
        let _ = run(&inst, &format!("rm -rf \"{USER_DIR}\""), None);
        let _ = std::fs::remove_file(known_hosts());
    }

    /// A backup file the way sentient-backup-core writes one: a tar with a
    /// manifest listing each part's SHA-256 (`recorded` wrong: damaged).
    fn backup_file(path: &Path, damaged: bool) {
        let data = b"dump";
        let tmp = path.with_extension("part");
        std::fs::write(&tmp, data).unwrap();
        let sha = sentient_installer_core::manifest::sha256_file(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);
        let manifest = serde_json::json!({
            "format_version": 2, "tool_version": "e2e", "created_at": "2026-11-15T00:00:00Z",
            "source": {"database": "sentient", "postgres_version": "18", "timescaledb_version": null},
            "components": [], "telemetry": {"mode": "none"},
            "files": [{"path": "db/dump.pgc.zst", "bytes": data.len(), "sha256": if damaged { "0".repeat(64) } else { sha }}],
            "file_stores": [], "encryption": {"scheme": "none"}
        });
        let mut tar = tar::Builder::new(std::fs::File::create(path).unwrap());
        let mj = serde_json::to_vec(&manifest).unwrap();
        for (n, bytes) in [("manifest.json", mj.as_slice()), ("db/dump.pgc.zst", data.as_slice())] {
            let mut h = tar::Header::new_gnu();
            h.set_size(bytes.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            tar.append_data(&mut h, n, bytes).unwrap();
        }
        tar.finish().unwrap();
    }

    /// The Backup page's jobs against a server with an added SENTIENT (an
    /// installation record; no running stack): the automatic backup's
    /// schedule, Verify, Delete, and Restore reaching the restore.
    fn backup_jobs_on_the_server(inst: &Instance, sink: &ProgressFn) {
        let job = |args: &[&str]| -> Result<(), String> {
            let id = start_job(inst, args)?;
            follow(inst, &id, sink)
        };
        let record = crate::installation::InstallationRecord {
            sentient_version: "4.4.0.1".into(),
            manager_version: crate::updates::manager_version().into(),
            engine: crate::installation::Engine::Docker,
            install_dir: "/opt/sentient".into(),
            pg_dir: "/opt/sentient".into(),
            state_dir: "/opt/sentient".into(),
            stack: Default::default(),
            installed_at: "2026-11-15T00:00:00Z".into(),
            kiosk: None,
            changes_done: Vec::new(),
        };
        let dir = std::env::temp_dir().join(format!("spm-e2e-bk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("installation.json"), serde_json::to_string(&record).unwrap()).unwrap();
        backup_file(&dir.join("good.sentient-backup"), false);
        backup_file(&dir.join("bad.sentient-backup"), true);
        for f in ["installation.json", "good.sentient-backup", "bad.sentient-backup"] {
            sftp_put(inst, &dir.join(f), &format!("/tmp/{f}")).unwrap();
        }
        sudo_run(
            inst,
            "sh -c 'mkdir -p /var/lib/sentient-platform-manager /var/backups/sentient && mv /tmp/installation.json /var/lib/sentient-platform-manager/ && mv /tmp/good.sentient-backup /tmp/bad.sentient-backup /var/backups/sentient/'",
            None,
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        // A log that cannot be read is an error for the page, never a line.
        let e = logs_from(inst, crate::logsource::Source::Sentient, 10, false).unwrap_err();
        assert!(e.contains("Could not read"), "{e}");
        assert!(log_sources(inst).unwrap().contains(&crate::logsource::Source::Database));
        let b = backups(inst).unwrap();
        assert_eq!(b.list.len(), 2, "{:?}", b.list.iter().map(|e| &e.name).collect::<Vec<_>>());
        assert!(b.daily_at.is_none());

        // Schedule: saved, and the timer registered and enabled.
        job(&["backup-auto", "02:30", "14", "configuration"]).unwrap();
        let b = backups(inst).unwrap();
        assert_eq!(b.daily_at.as_deref(), Some("02:30"));
        assert_eq!(b.keep, 14);
        assert_eq!(b.preset, crate::backups::Preset::Configuration);
        let timer = sudo_run(inst, "cat /etc/systemd/system/sentient-backup.timer", None).unwrap();
        assert!(timer.contains("OnCalendar=*-*-* 02:30:00"), "{timer}");
        assert_eq!(sudo_run(inst, "systemctl is-enabled sentient-backup.timer", None).unwrap().trim(), "enabled");
        assert!(job(&["backup-auto", "25:99", "14", "configuration"]).is_err());
        // Off: the timer goes.
        job(&["backup-auto", "off", "7", "everything"]).unwrap();
        assert!(backups(inst).unwrap().daily_at.is_none());
        assert!(sudo_run(inst, "test -e /etc/systemd/system/sentient-backup.timer && echo there || echo gone", None).unwrap().contains("gone"));

        // Verify: the good file passes and is remembered; the damaged one fails.
        job(&["backup-verify", "/var/backups/sentient/good.sentient-backup"]).unwrap();
        let e = job(&["backup-verify", "/var/backups/sentient/bad.sentient-backup"]).unwrap_err();
        assert!(e.contains("damaged"), "{e}");
        let b = backups(inst).unwrap();
        let state = |n: &str| b.list.iter().find(|e| e.name == n).and_then(|e| e.check.as_ref()).map(|c| c.ok);
        assert_eq!(state("good"), Some(true));
        assert_eq!(state("bad"), Some(false));
        // Only files in the backup folder.
        let e = job(&["backup-verify", "/etc/passwd"]).unwrap_err();
        assert!(e.contains("is not a backup"), "{e}");

        // Delete: the damaged one goes.
        job(&["backup-delete", "/var/backups/sentient/bad.sentient-backup"]).unwrap();
        assert_eq!(backups(inst).unwrap().list.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["good"]);
        assert!(job(&["backup-delete", "/etc/passwd"]).is_err());

        // Restore: the job reaches the restore and stops plainly where the
        // server has no running stack to restore into.
        let e = job(&["replace", "/var/backups/sentient/good.sentient-backup"]).unwrap_err();
        eprintln!("restore without a running stack: {e}");
        assert!(!e.is_empty());
        sudo_run(inst, "rm -f /var/lib/sentient-platform-manager/installation.json", None).unwrap();
    }

    #[test]
    fn a_windows_account_without_administrator_rights_is_named() {
        let mut inst = Instance { id: "w".into(), name: "w".into(), color: PALETTE[0].into(), os: Os::Windows, host: "plant-pc".into(), port: 22, user: "operator".into(), auth: Auth::Password, key_file: None, key_has_passphrase: false, host_key: None, detected: None };
        let want = "The account operator on plant-pc is not an administrator. The Platform Manager needs an administrator account there: add it to the Administrators group, or use an administrator account for this instance.";
        // The agent's refusal, on stdout (piped 2>&1) or as the error.
        let piped = "sentient-platform-manager.exe : Run this from an elevated prompt (Run as administrator).".to_string();
        assert_eq!(rights_answer(&inst, Ok(piped.clone())).unwrap_err(), want);
        assert_eq!(rights_answer(&inst, Err(piped.clone())).unwrap_err(), want);
        assert!(is_not_admin(want) && !needs_setup(want));
        // Other answers pass through.
        assert_eq!(rights_answer(&inst, Ok("{\"installed\":true}".into())).unwrap(), "{\"installed\":true}");
        assert_eq!(rights_answer(&inst, Err("timed out".into())).unwrap_err(), "timed out");
        // Linux has its own sudo messages.
        inst.os = Os::Linux;
        assert_eq!(rights_answer(&inst, Ok(piped.clone())).unwrap(), piped);
    }

    #[test]
    fn a_missing_agent_says_why_and_what_to_do() {
        let e = NoAgent::NotPublished { system: System::Windows, version: "0.8.0-rc12".into() }.message();
        let (head, details) = split_failure(&e);
        assert_eq!(
            head,
            "This package has no Windows agent, and this build (0.8.0-rc12) is not a published release, so none can be downloaded. Install the Platform Manager on that computer, or use a Platform Manager package that includes the Windows agent."
        );
        assert_eq!(details.as_deref(), Some("sentient-platform-manager-agent-windows-x86_64.exe for Platform Manager 0.8.0-rc12"));
        let e = NoAgent::Download { system: System::Linux(Arch::Aarch64), version: "0.8.0".into(), error: "download: timed out".into() }.message();
        assert!(e.starts_with("This package has no Linux (ARM64) agent, and the one published with 0.8.0 could not be downloaded."), "{e}");
        assert!(e.ends_with("timed out"));
    }

    /// The agent command runs this Platform Manager's own agent beside an
    /// installed one first, then the one at REMOTE_SPM, then the per-user one.
    #[test]
    fn the_own_agent_beside_an_installed_one_comes_first() {
        let inst = Instance { id: "t".into(), name: "t".into(), color: PALETTE[0].into(), os: Os::Linux, host: "h".into(), port: 22, user: "ops".into(), auth: Auth::Key, key_file: None, key_has_passphrase: false, host_key: None, detected: None };
        let c = agent_cmd(&inst, "--job status-json", false);
        let own = c.find(reuse::LINUX_OWN).unwrap();
        let sys = c.find(&format!("[ -x {REMOTE_SPM} ]")).unwrap();
        let user = c.find("$HOME/.local/share").unwrap();
        assert!(own < sys && sys < user, "{c}");
        assert!(c.contains(&format!("sudo -n sh -c '{} --job status-json'", reuse::LINUX_OWN)), "{c}");
        // Run by a shell, the choice picks the right program. The script is
        // the Linux server's (POSIX sh, exec bits, / paths): run it only on a
        // unix host; the command-line checks above run everywhere.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = std::env::temp_dir().join(format!("spm-pick-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            let pick = pick_system_agent().replace(reuse::LINUX_OWN, &dir.join("own").display().to_string()).replace(REMOTE_SPM, &dir.join("sys").display().to_string());
            let out = |s: &str| String::from_utf8(Command::new("sh").arg("-c").arg(format!("{s} echo $A")).output().unwrap().stdout).unwrap();
            assert!(out(&pick).trim().ends_with("/sys"));
            std::fs::write(dir.join("own"), "").unwrap();
            std::fs::set_permissions(dir.join("own"), std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(out(&pick).trim().ends_with("/own"));
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// An installed Platform Manager on the test bed is used as the agent,
    /// offered when its version differs, and never replaced: SPM_E2E="port",
    /// SPM_E2E_KEY, SPM_AGENT_DIR (this branch's static agent),
    /// XDG_CONFIG_HOME scratch. The account needs sudo without a password.
    /// The installed one is a stand-in at REMOTE_SPM that answers
    /// agent-info as a full Platform Manager and runs this branch's agent
    /// for everything else; what was there is put back at the end.
    #[test]
    #[ignore]
    fn reuses_an_installed_manager_on_the_test_bed() {
        let port: u16 = std::env::var("SPM_E2E").expect("SPM_E2E").parse().unwrap();
        let mut inst = Instance {
            id: format!("e2e-reuse-{port}"),
            name: "e2e reuse".into(),
            color: PALETTE[0].into(),
            os: Os::Linux,
            host: "127.0.0.1".into(),
            port,
            user: "ops".into(),
            auth: Auth::Key,
            key_file: Some(std::env::var("SPM_E2E_KEY").unwrap().into()),
            key_has_passphrase: false,
            host_key: None,
            detected: None,
        };
        let (keys, fp) = scan("127.0.0.1", port).unwrap();
        trust(&keys).unwrap();
        inst.host_key = Some(fp);
        inst.detected = Some(detect(&inst, None).unwrap());
        let sink: ProgressFn = std::sync::Arc::new(|p| eprintln!("  {p:?}"));
        let logs = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
        let logged: ProgressFn = {
            let logs = logs.clone();
            std::sync::Arc::new(move |p| {
                if let Progress::Log { line } = &p {
                    logs.lock().unwrap().push(line.clone());
                }
                eprintln!("  {p:?}");
            })
        };
        let dir = "/opt/sentient-platform-manager";
        let sh = |c: &str| sudo_run(&inst, c, None).unwrap();
        sh(&format!("cp -a {REMOTE_SPM} {REMOTE_SPM}.r-bak && rm -rf /var/lib/sentient-platform-manager/agent"));
        // This branch's agent, and the stand-in in front of it.
        let me = agent_file(System::Linux(Arch::X86_64), &sink).unwrap();
        let home = home(&inst).unwrap();
        let staged = format!("{}/.spm-r-real", home.trim_end_matches('/'));
        sftp_copy(&inst, &me, &staged).unwrap();
        sh(&format!("install -m 0755 {staged} {dir}/spm-real && rm -f {staged}"));
        sh(&format!("printf '#!/bin/sh\\nif [ \"$1 $2\" = \"--job agent-info\" ]; then cat {dir}/fake-info.json; exit 0; fi\\nexec {dir}/spm-real \"$@\"\\n' > {REMOTE_SPM} && chmod 755 {REMOTE_SPM}"));
        let fake = |info: &reuse::AgentInfo| sh(&format!("printf '%s\\n' '{}' > {dir}/fake-info.json", serde_json::to_string(info).unwrap()));
        let stand_in = run(&inst, &format!("sha256sum {REMOTE_SPM} | cut -d' ' -f1"), None).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let ours = reuse::AgentInfo::this();
            // The same version: used as it is, nothing copied.
            fake(&reuse::AgentInfo { window: true, ..ours.clone() });
            ready(&inst).unwrap();
            let st = status(&inst).unwrap();
            eprintln!("status (installed, same): {st:?}");
            deploy(&inst, &logged).unwrap();
            assert!(logs.lock().unwrap().iter().any(|l| l.contains("installed on the server does the work")));
            assert_eq!(run(&inst, &format!("[ -e {} ] && echo yes || echo no", reuse::LINUX_OWN), None).unwrap().trim(), "no");
            // Another version, same protocol: offered, used once chosen.
            fake(&reuse::AgentInfo { version: "0.0.1-rc1".into(), window: true, ..ours.clone() });
            let e = ready(&inst).unwrap_err();
            assert!(reuse::is_offer(&e) && e.contains("0.0.1-rc1, older than this one") && needs_setup(&e), "{e}");
            assert_eq!(reuse::fit(&reuse::found(&inst.id).unwrap().info, &ours), reuse::Fit::Compatible);
            use_installed(&inst).unwrap();
            ready(&inst).unwrap();
            status(&inst).unwrap();
            // Another protocol: Set up puts this version's agent beside it.
            fake(&reuse::AgentInfo { version: "9.0.0".into(), protocol: PROTOCOL + 1, window: true, ..ours.clone() });
            let e = ready(&inst).unwrap_err();
            assert!(reuse::is_offer(&e) && e.contains("newer than this one") && e.contains("beside it"), "{e}");
            deploy(&inst, &logged).unwrap();
            assert!(logs.lock().unwrap().iter().any(|l| l.contains("set up beside the Platform Manager installed")));
            ready(&inst).unwrap();
            // Jobs run with the own agent now (its agent-info is the real one),
            // and the installed one is unchanged.
            let info = reuse::parse_info(&spm_job(&inst, "agent-info").unwrap());
            assert_eq!(info, reuse::AgentInfo { window: false, ..ours.clone() });
            assert!(!status(&inst).unwrap().manager_version.is_empty());
            let id = start_job(&inst, &["kept"]).unwrap();
            follow(&inst, &id, &sink).unwrap();
            assert_eq!(run(&inst, &format!("sha256sum {REMOTE_SPM} | cut -d' ' -f1"), None).unwrap(), stand_in);
            // Deploying again finds the own agent up to date.
            logs.lock().unwrap().clear();
            deploy(&inst, &logged).unwrap();
            assert!(logs.lock().unwrap().iter().any(|l| l.contains("up to date")), "{:?}", logs.lock().unwrap());
        }));
        sh(&format!("mv -f {REMOTE_SPM}.r-bak {REMOTE_SPM} && rm -f {dir}/spm-real {dir}/fake-info.json && rm -rf /var/lib/sentient-platform-manager/agent"));
        reuse::forget(&inst.id);
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    #[test]
    fn ssh_messages_read_plainly() {
        assert!(explain("@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @@@").unwrap().contains("not the one accepted"));
        assert!(explain("Permission denied (publickey).").unwrap().contains("refused the sign-in"));
        assert!(explain("something else").is_none());
    }
}

// ---------------------------------------------------------------------------
// Restore (crate::restore_flow): jobs, the upload from this computer, a
// backup copied from another instance, the history
// ---------------------------------------------------------------------------

/// A file only the account reads on the server, for a job to take (a
/// restore request, a passphrase): the job removes it once read.
pub fn put_job_file(inst: &Instance, kind: &str, text: &str) -> Result<String, String> {
    let sep = if is_windows(inst) { "\\" } else { "/" };
    let file = format!("{}{sep}.spm-restore-{kind}-{}", home(inst)?.trim_end_matches(['/', '\\']), chrono::Utc::now().timestamp_millis());
    put_text(inst, &file, text)?;
    Ok(file)
}

/// The arguments of a restore job: `restore-inspect <file>` or
/// `restore-precheck|restore-run <request>`, the passphrase in a file.
pub fn restore_args(inst: &Instance, job: &str, req: &crate::restore_flow::Request, passphrase: Option<&str>) -> Result<Vec<String>, String> {
    let mut args = vec![job.to_string()];
    if job == "restore-inspect" {
        args.push(req.file.display().to_string());
    } else {
        args.push(put_job_file(inst, "request", &serde_json::to_string(req).map_err(|e| e.to_string())?)?);
    }
    if let Some(p) = passphrase.filter(|p| !p.is_empty()) {
        args.push("--passphrase-file".into());
        args.push(put_job_file(inst, "pass", p)?);
    }
    Ok(args)
}

/// Run a job on the server and follow it to its end (the agent brought up
/// to date first), as the pages' long jobs do. Blocking.
pub fn run_followed(inst: &Instance, args: &[String], sink: &ProgressFn) -> Result<(), String> {
    let updated = deploy(inst, sink);
    if let Err(e) = &updated {
        sink(Progress::Log { line: format!("The agent could not be updated: {}", split_failure(e).0) });
    }
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match (start_job(inst, &a).and_then(|id| follow(inst, &id, sink)), updated) {
        (Err(e), Err(u)) if e == AGENT_OLD || e.contains("usage: --job") => Err(failed("The agent on this server could not be updated, and the one there is too old for this.", &split_failure(&u).1.unwrap_or(u.clone()))),
        (r, _) => r,
    }
}

/// The restores on the server, newest first.
pub fn restore_history(inst: &Instance) -> Result<Vec<crate::restore_flow::Entry>, String> {
    #[cfg(debug_assertions)]
    if let Some(p) = std::env::var_os("SPM_DEBUG_RESTORE_HISTORY") {
        return std::fs::read_to_string(p).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()));
    }
    let out = spm_job(inst, "restore-history-json")?;
    let line = out.lines().rev().find(|l| l.trim_start().starts_with('[')).ok_or_else(|| no_answer(&out))?;
    serde_json::from_str(line).map_err(|e| format!("unexpected answer from the server: {e}"))
}

/// One restore's log on the server.
pub fn restore_log(inst: &Instance, id: &str) -> Result<String, String> {
    #[cfg(debug_assertions)]
    if let Some(t) = std::env::var_os("SPM_DEBUG_RESTORE_LOG") {
        return Ok(t.to_string_lossy().replace("\\n", "\n"));
    }
    spm_job(inst, &format!("restore-log {}", job_arg(id)?))
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The size of a file on the server (0 when it is not there).
pub fn remote_size(inst: &Instance, path: &str) -> u64 {
    let out = if is_windows(inst) {
        win::ps(inst, &format!("if (Test-Path -LiteralPath '{0}') {{ (Get-Item -LiteralPath '{0}').Length }} else {{ 0 }}", path.replace('\'', "''")))
    } else {
        run(inst, &format!("stat -c %s {} 2>/dev/null || echo 0", shell_quote(path)), None)
    };
    out.ok().and_then(|o| o.trim().parse().ok()).unwrap_or(0)
}

/// Whether the account itself can write in `folder` on the server (the
/// upload runs as the account, not as administrator).
pub fn can_write(inst: &Instance, folder: &str) -> Result<bool, String> {
    #[cfg(debug_assertions)]
    if let Ok(v) = std::env::var("SPM_DEBUG_REMOTE_WRITABLE") {
        return Ok(v != "no");
    }
    if is_windows(inst) {
        let p = format!("{}\\.spm-write-test", folder.trim_end_matches('\\'));
        return Ok(win::ps(inst, &format!("try {{ New-Item -Force -ItemType File -Path '{0}' | Out-Null; Remove-Item -Force '{0}'; 'yes' }} catch {{ 'no' }}", p.replace('\'', "''")))?.trim() == "yes");
    }
    Ok(run(inst, &format!("mkdir -p {0} 2>/dev/null; [ -w {0} ] && echo yes || echo no", shell_quote(folder)), None)?.trim() == "yes")
}

fn join_remote(inst: &Instance, folder: &str, name: &str) -> String {
    if is_windows(inst) { format!("{}\\{name}", folder.trim_end_matches('\\')) } else { format!("{}/{name}", folder.trim_end_matches('/')) }
}

/// The sftp path of a server path (Windows: `/C:/…`).
fn sftp_path(inst: &Instance, path: &str) -> String {
    if is_windows(inst) { format!("/{}", path.replace('\\', "/")) } else { path.to_string() }
}

fn rename_remote(inst: &Instance, from: &str, to: &str) -> Result<(), String> {
    if is_windows(inst) {
        win::ps(inst, &format!("Move-Item -Force -LiteralPath '{}' '{}'", from.replace('\'', "''"), to.replace('\'', "''"))).map(|_| ())
    } else {
        run(inst, &format!("mv -f {} {}", shell_quote(from), shell_quote(to)), None).map(|_| ())
    }
}

/// Watch a file on the server grow while `work` runs: `progress(sent)`
/// every two seconds.
fn with_progress<T>(inst: &Instance, path: &str, progress: &(dyn Fn(u64) + Sync), work: impl FnOnce() -> T) -> T {
    let done = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|s| {
        s.spawn(|| {
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                progress(remote_size(inst, path));
                for _ in 0..20 {
                    if done.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        });
        let r = work();
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        r
    })
}

/// Upload a backup from this computer into `folder` on the server, with its
/// `.sha256` and its parts (R5). A broken upload goes on from where it
/// stopped: the part already there (`<name>.part`) is kept and continued.
/// Returns the file's path on the server. `progress(sent, total)`.
pub fn upload_backup(inst: &Instance, local: &Path, folder: &str, progress: &(dyn Fn(u64, u64) + Sync)) -> Result<String, String> {
    let (_, files) = sentient_backup_core::parts::resolve(local).map_err(|e| e.to_string())?;
    let total: u64 = files.iter().filter_map(|f| std::fs::metadata(f).ok()).map(|m| m.len()).sum();
    let mut before = 0u64;
    for f in &files {
        let name = f.file_name().ok_or("no file name")?.to_string_lossy().to_string();
        let dest = join_remote(inst, folder, &name);
        let part = format!("{dest}.part");
        let len = std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
        let base = before;
        let r = with_progress(inst, &part, &|sent| progress(base + sent.min(len), total), || sftp_put(inst, f, &sftp_path(inst, &part)));
        if let Err(e) = r {
            let sent = remote_size(inst, &part);
            return Err(format!("{}\n{}", UPLOAD_BROKE, serde_json::json!({"sent": base + sent, "total": total, "why": e})));
        }
        if remote_size(inst, &part) != len {
            return Err(format!("{}\n{}", UPLOAD_BROKE, serde_json::json!({"sent": base + remote_size(inst, &part), "total": total, "why": "The size on the server does not match."})));
        }
        rename_remote(inst, &part, &dest)?;
        before += len;
        progress(before, total);
    }
    let sidecar = sentient_backup_core::parts::sidecar_path(local);
    if sidecar.is_file() {
        let name = sidecar.file_name().unwrap_or_default().to_string_lossy().to_string();
        sftp_put(inst, &sidecar, &sftp_path(inst, &join_remote(inst, folder, &name)))?;
    }
    let key = crate::secrets_key::beside(local);
    if key.is_file() {
        let name = key.file_name().unwrap_or_default().to_string_lossy().to_string();
        let _ = sftp_put(inst, &key, &sftp_path(inst, &join_remote(inst, folder, &name)));
    }
    Ok(join_remote(inst, folder, &local.file_name().unwrap_or_default().to_string_lossy()))
}

/// The first line of an upload's error that went on part of the way; the
/// second is JSON {sent, total, why}.
pub const UPLOAD_BROKE: &str = "UPLOAD-BROKE";

/// How a backup travels from another instance (R6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyWay {
    /// The source sends it straight to the target over SSH.
    Direct,
    /// Down to this computer's pipe and up again, never stored here.
    ThroughHere,
}

/// Whether `src` reaches `dst`'s SSH port (server to server copies).
pub fn reaches(src: &Instance, dst: &Instance) -> bool {
    #[cfg(debug_assertions)]
    if let Ok(v) = std::env::var("SPM_DEBUG_REMOTE_REACHES") {
        return v == "yes";
    }
    if is_windows(src) {
        return win::ps(src, &format!("(Test-NetConnection -ComputerName '{}' -Port {} -WarningAction SilentlyContinue).TcpTestSucceeded", dst.host.replace('\'', ""), dst.port)).is_ok_and(|o| o.trim() == "True");
    }
    run(src, &format!("timeout 6 bash -c '</dev/tcp/{}/{}' 2>/dev/null && echo yes || echo no", dst.host.replace(['\'', ' '], ""), dst.port), None).is_ok_and(|o| o.trim() == "yes")
}

/// Copy a backup (and its `.sha256` and key file) from `src` into `folder`
/// on `dst`. Returns its path on `dst`.
pub fn copy_between(src: &Instance, dst: &Instance, path: &str, folder: &str, way: CopyWay, progress: &(dyn Fn(u64, u64) + Sync)) -> Result<String, String> {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path).to_string();
    let dest = join_remote(dst, folder, &name);
    let part = format!("{dest}.part");
    let total = remote_size(src, path);
    let windows = is_windows(src) || is_windows(dst);
    let r = if windows {
        // Through a file here: sftp down, then up (Windows has no cat to pipe).
        let dir = std::env::temp_dir().join(format!("spm-copy-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let local = dir.join(&name);
        let got = sftp_batch(src, &format!("get \"{}\" \"{}\"", sftp_path(src, path), local.display()));
        let r = got.and_then(|_| with_progress(dst, &part, &|s| progress(s, total), || sftp_put(dst, &local, &sftp_path(dst, &part))));
        let _ = std::fs::remove_dir_all(&dir);
        r
    } else {
        match way {
            CopyWay::ThroughHere => with_progress(dst, &part, &|s| progress(s, total), || pipe_copy(src, dst, path, &part)),
            CopyWay::Direct => with_progress(dst, &part, &|s| progress(s, total), || direct_copy(src, dst, path, &part)),
        }
    };
    r?;
    if remote_size(dst, &part) != total {
        return Err(format!("The copy on {} is {} bytes, the backup on {} {total}. Try again.", dst.name, remote_size(dst, &part), src.name));
    }
    rename_remote(dst, &part, &dest)?;
    // The checksum file and the secrets key go along (small).
    for extra in [format!("{path}.sha256"), path.trim_end_matches(".sentient-backup").to_string() + ".key"] {
        if remote_size(src, &extra) > 0 {
            let n = extra.rsplit(['/', '\\']).next().unwrap_or(&extra).to_string();
            let text = if is_windows(src) { win::ps(src, &format!("Get-Content -Raw -LiteralPath '{}'", extra.replace('\'', "''"))) } else { run(src, &format!("cat {}", shell_quote(&extra)), None) };
            if let Ok(t) = text {
                let _ = put_text(dst, &join_remote(dst, folder, &n), &t);
            }
        }
    }
    Ok(dest)
}

/// `ssh src cat` into `ssh dst cat >>`, resuming a part already there.
fn pipe_copy(src: &Instance, dst: &Instance, path: &str, part: &str) -> Result<(), String> {
    let have = remote_size(dst, part);
    let mut reader = ssh(src, &format!("tail -c +{} {}", have + 1, shell_quote(path)), None).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("start ssh: {e}"))?;
    let out = reader.stdout.take().ok_or("no output")?;
    let mut writer = ssh(dst, &format!("cat >> {}", shell_quote(part)), None).stdin(Stdio::from(out)).stderr(Stdio::piped()).spawn().map_err(|e| format!("start ssh: {e}"))?;
    let w = writer.wait().map_err(|e| e.to_string())?;
    let r = reader.wait_with_output().map_err(|e| e.to_string())?;
    if !r.status.success() {
        return Err(format!("Reading the backup on {} failed: {}", src.name, String::from_utf8_lossy(&r.stderr).trim()));
    }
    if !w.success() {
        return Err(format!("Writing the copy on {} failed.", dst.name));
    }
    Ok(())
}

/// The source sends the file itself: a key made for this one copy is
/// allowed on the target (restricted) and given to the source, then both
/// are removed.
/// The host-key pattern known_hosts uses for `host` on `port`.
fn host_pattern(host: &str, port: u16) -> String {
    if port == 22 { host.to_string() } else { format!("[{host}]:{port}") }
}

/// The keys this Platform Manager pinned for `pattern`, from the output of
/// `ssh-keygen -F <pattern>`, as plain `<pattern> <type> <key>` lines (a
/// hashed host name is written out again; markers like @revoked are left).
fn pinned_lines(found: &str, pattern: &str) -> String {
    found
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('@'))
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.len() >= 3).then(|| format!("{pattern} {} {}\n", f[1], f[2]))
        })
        .collect()
}

/// `dst`'s host keys as this Platform Manager pinned them.
fn pinned_host_keys(dst: &Instance) -> Result<String, String> {
    let pattern = host_pattern(&dst.host, dst.port);
    let out = Command::new("ssh-keygen").args(["-F", &pattern, "-f"]).arg(known_hosts()).output().map_err(|e| format!("ssh-keygen: {e}"))?;
    let lines = pinned_lines(&String::from_utf8_lossy(&out.stdout), &pattern);
    if lines.is_empty() {
        return Err(format!("This computer has no pinned host key for {0}, so the other server could not check whom it sends the backup to. Connect to {0} from here once, or copy through this computer instead.", dst.name));
    }
    Ok(lines)
}

/// The scp the source server runs: only `dst`'s pinned keys in a known_hosts
/// file of their own (`known` on the source), checked strictly.
fn direct_copy_command(dst: &Instance, key: &str, known: &str, path: &str, part: &str) -> String {
    format!(
        "scp -q -i {} -P {} -o StrictHostKeyChecking=yes -o UserKnownHostsFile={} -o GlobalKnownHostsFile=/dev/null -o BatchMode=yes {} {}",
        shell_quote(key),
        dst.port,
        shell_quote(known),
        shell_quote(path),
        shell_quote(&format!("{}@{}:{}", dst.user, dst.host, part))
    )
}

fn direct_copy(src: &Instance, dst: &Instance, path: &str, part: &str) -> Result<(), String> {
    // Before anything is set up: the source checks dst's key against the one
    // pinned here, or there is no copy.
    let known_keys = pinned_host_keys(dst)?;
    let tag = format!("spm-restore-copy-{}", chrono::Utc::now().timestamp_millis());
    let dir = std::env::temp_dir().join(&tag);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let key = dir.join("key");
    let made = Command::new("ssh-keygen").args(["-q", "-t", "ed25519", "-N", "", "-C", &tag, "-f"]).arg(&key).output().map_err(|e| format!("ssh-keygen: {e}"))?;
    if !made.status.success() {
        let _ = std::fs::remove_dir_all(&dir);
        return Err("A key for the copy could not be made on this computer.".into());
    }
    let public = std::fs::read_to_string(key.with_extension("pub")).map_err(|e| e.to_string())?;
    let private = std::fs::read_to_string(&key).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&dir);
    let cleanup = || {
        let _ = run(dst, &format!("sed -i '/{tag}/d' ~/.ssh/authorized_keys"), None);
        let _ = run(src, &format!("rm -f ~/.{tag} ~/.{tag}.known_hosts"), None);
    };
    let r = (|| -> Result<(), String> {
        run(dst, &format!("mkdir -p ~/.ssh && chmod 700 ~/.ssh && echo {} >> ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys", shell_quote(&format!("restrict {}", public.trim()))), None)?;
        let base = home(src)?.trim_end_matches('/').to_string();
        let (key_at, known_at) = (format!("{base}/.{tag}"), format!("{base}/.{tag}.known_hosts"));
        put_text(src, &key_at, &private)?;
        put_text(src, &known_at, &known_keys)?;
        run(src, &direct_copy_command(dst, &key_at, &known_at, path, part), None)
            .map(|_| ())
            .map_err(|e| format!("{} could not send it to {} ({e}). Copy through this computer instead.", src.name, dst.name))
    })();
    cleanup();
    r
}

#[cfg(test)]
mod direct_copy_tests {
    use super::*;

    fn dst(port: u16) -> Instance {
        serde_json::from_value(serde_json::json!({"id": "b", "name": "Plant B", "color": "#6b4fa0", "host": "plantb.local", "port": port, "user": "ops"})).unwrap()
    }

    #[test]
    fn the_source_checks_the_pinned_key_strictly() {
        let c = direct_copy_command(&dst(2242), "/home/ops/.k", "/home/ops/.k.known_hosts", "/var/backups/a b.sentient-backup", "/var/backups/a b.part");
        assert!(c.contains("-o StrictHostKeyChecking=yes"), "{c}");
        assert!(c.contains("-o UserKnownHostsFile='/home/ops/.k.known_hosts'"), "{c}");
        assert!(c.contains("-o GlobalKnownHostsFile=/dev/null"), "{c}");
        assert!(!c.contains("accept-new") && !c.contains("UserKnownHostsFile=/dev/null"), "{c}");
        assert!(c.contains("-P 2242"));
        assert!(c.ends_with("'/var/backups/a b.sentient-backup' 'ops@plantb.local:/var/backups/a b.part'"), "{c}");
    }

    #[test]
    fn pinned_keys_are_written_for_the_host_as_the_source_names_it() {
        let found = "# Host [plantb.local]:2242 found: line 3\n|1|abc=|def= ssh-ed25519 AAAAC3Nz\n@revoked * ssh-rsa AAAAB3\n[plantb.local]:2242 ecdsa-sha2-nistp256 AAAAE2 comment\n";
        assert_eq!(pinned_lines(found, "[plantb.local]:2242"), "[plantb.local]:2242 ssh-ed25519 AAAAC3Nz\n[plantb.local]:2242 ecdsa-sha2-nistp256 AAAAE2\n");
        assert_eq!(pinned_lines("", "x"), "");
        assert_eq!(host_pattern("plantb.local", 22), "plantb.local");
        assert_eq!(host_pattern("10.0.0.5", 2242), "[10.0.0.5]:2242");
    }
}

/// Ask the restore running on the server to stop before its switch.
pub fn restore_cancel(inst: &Instance) -> Result<(), String> {
    spm_job(inst, "restore-cancel").map(|_| ())
}

/// The backups in another folder on the server.
pub fn backups_in(inst: &Instance, folder: &str) -> Result<Vec<crate::backups::Entry>, String> {
    #[cfg(debug_assertions)]
    if let Some(t) = std::env::var_os("SPM_DEBUG_REMOTE_BACKUPS") {
        return serde_json::from_str::<RemoteBackups>(&t.to_string_lossy()).map(|b| b.list).map_err(|e| e.to_string());
    }
    let out = spm_job(inst, &format!("backup-list {}", job_arg(folder)?))?;
    let line = out.lines().rev().find(|l| l.trim_start().starts_with('[')).ok_or_else(|| no_answer(&out))?;
    serde_json::from_str(line).map_err(|e| format!("unexpected answer from the server: {e}"))
}

/// Copy a backup from a server into `folder` on this computer (another
/// instance's backup restored here), with its `.sha256`.
pub fn download_backup(src: &Instance, path: &str, folder: &Path, progress: &(dyn Fn(u64, u64) + Sync)) -> Result<PathBuf, String> {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path).to_string();
    let total = remote_size(src, path);
    std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let dest = folder.join(&name);
    let part = folder.join(format!("{name}.part"));
    let done = std::sync::atomic::AtomicBool::new(false);
    let r = std::thread::scope(|s| {
        s.spawn(|| {
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                progress(std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0), total);
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        });
        let r = sftp_batch(src, &format!("reget \"{}\" \"{}\"", sftp_path(src, path), part.display())).or_else(|_| sftp_batch(src, &format!("get \"{}\" \"{}\"", sftp_path(src, path), part.display())));
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        r
    });
    r?;
    if std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0) != total {
        return Err("The copy here does not have the backup's size. Try again.".into());
    }
    std::fs::rename(&part, &dest).map_err(|e| e.to_string())?;
    let _ = sftp_batch(src, &format!("get \"{}.sha256\" \"{}.sha256\"", sftp_path(src, path), dest.display()));
    Ok(dest)
}

/// The restore jobs against the shared test bed (read-only there: a backup
/// is made into /tmp, inspected, pre-checked, uploaded and copied; nothing
/// is restored). SPM_E2E_RESTORE=<instance id>, XDG_CONFIG_HOME with the
/// instances, SPM_AGENT_DIR with the static agent.
#[cfg(test)]
mod restore_live {
    use super::*;

    #[test]
    #[ignore]
    fn restore_jobs_on_the_test_bed() {
        let id = std::env::var("SPM_E2E_RESTORE").expect("SPM_E2E_RESTORE");
        let inst = load().into_iter().find(|i| i.id == id).expect("instance");
        let lines: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
        let l2 = lines.clone();
        let sink: ProgressFn = std::sync::Arc::new(move |p| {
            let line = match p {
                Progress::Step { name } => format!(">> {name}"),
                Progress::Log { line } => line,
                Progress::Done { message } => format!("done: {message}"),
                Progress::Error { message } => format!("error: {message}"),
                _ => return,
            };
            l2.lock().unwrap().push(line);
        });
        let take = || std::mem::take(&mut *lines.lock().unwrap());
        deploy(&inst, &sink).expect("deploy");
        run(&inst, "rm -rf /tmp/spme && mkdir -p /tmp/spme/up /tmp/spme/copy && chmod 777 /tmp/spme /tmp/spme/up /tmp/spme/copy", None).unwrap();
        // A small, well-formed backup (SPM_E2E_BACKUP, made by the test's
        // script): the test bed's SENTIENT is a stand-in with no schema.
        let fake = PathBuf::from(std::env::var("SPM_E2E_BACKUP").expect("SPM_E2E_BACKUP"));
        let file = upload_backup(&inst, &fake, "/tmp/spme", &|_, _| {}).expect("first upload");
        let file = file.as_str();
        // Inspect: sound, the parts listed and checked.
        let req = crate::restore_flow::Request { file: file.into(), ..Default::default() };
        let args = restore_args(&inst, "restore-inspect", &req, None).unwrap();
        run_followed(&inst, &args, &sink).expect("inspect");
        let got = take();
        let i: crate::restore_flow::Inspection = crate::restore_flow::read_data(&got, "inspect").expect("inspection");
        assert!(i.sound(), "{:?}", i.parts);
        assert!(got.iter().filter(|l| l.starts_with("PART\t")).count() > i.parts.len());
        eprintln!("inspect: {} parts, {} bytes, here {:?}", i.parts.len(), i.file_bytes, i.here);
        // Not a backup.
        run(&inst, "echo nope > /tmp/spme/x.sentient-backup", None).unwrap();
        let bad = crate::restore_flow::Request { file: "/tmp/spme/x.sentient-backup".into(), ..Default::default() };
        let args = restore_args(&inst, "restore-inspect", &bad, None).unwrap();
        assert!(run_followed(&inst, &args, &sink).is_err());
        let u: crate::restore_flow::Unreadable = crate::restore_flow::read_data(&take(), "unreadable").expect("unreadable");
        assert!(matches!(u, crate::restore_flow::Unreadable::NotABackup(_)), "{u:?}");
        // Pre-check: configuration only with Keep (orphans), and everything.
        for scope in [sentient_backup_core::restore_scope::Scope::Configuration, sentient_backup_core::restore_scope::Scope::Everything] {
            let req = crate::restore_flow::Request { file: file.into(), scope: scope.clone(), keep_telemetry: true, safety_backup: true, ..Default::default() };
            let args = restore_args(&inst, "restore-precheck", &req, None).unwrap();
            if let Err(e) = run_followed(&inst, &args, &sink) {
                panic!("precheck: {e}\n{}", take().join("\n"));
            }
            let p: crate::restore_flow::Precheck = crate::restore_flow::read_data(&take(), "precheck").expect("precheck answer");
            eprintln!("precheck {scope:?}: disks {:?}\n  estimate {:?} upgrade {:?} settings {} same {} orphans {:?} live {:?}", p.disks, p.estimate, p.upgrade, p.settings.len(), p.same, p.orphans, p.live);
            assert!(!p.disks.is_empty() && p.disks.iter().all(|d| d.free.is_some()));
        }
        // History answers (an empty list is fine).
        let h = restore_history(&inst).expect("history");
        eprintln!("history: {} entries", h.len());
        // Upload from here, with resume: a partial part first.
        let dir = std::env::temp_dir().join(format!("spme-up-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let local = dir.join("up.sentient-backup");
        let data: Vec<u8> = (0..30_000_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&local, &data).unwrap();
        put_text(&inst, "/tmp/spme/up/up.sentient-backup.part", "").unwrap();
        run(&inst, "head -c 10000000 /dev/zero > /tmp/spme/up/up.sentient-backup.part.tmp; true", None).unwrap();
        assert!(can_write(&inst, "/tmp/spme/up").unwrap());
        assert!(!can_write(&inst, "/root/spme").unwrap_or(false));
        let seen = std::sync::Mutex::new(Vec::new());
        let path = upload_backup(&inst, &local, "/tmp/spme/up", &|s, t| seen.lock().unwrap().push((s, t))).expect("upload");
        assert_eq!(remote_size(&inst, &path), data.len() as u64);
        let sum = run(&inst, &format!("sha256sum {path}"), None).unwrap();
        let mine = sentient_backup_core::util::sha256_file(&local).unwrap().0;
        assert!(sum.starts_with(&mine), "{sum} vs {mine}");
        let readings = seen.lock().unwrap().clone();
        eprintln!("upload: {} progress readings, last {:?}", readings.len(), readings.last());
        // Resume: cut the file on the server to a part and send again.
        run(&inst, &format!("head -c 12000000 {path} > {path}.part && rm {path}"), None).unwrap();
        let path = upload_backup(&inst, &local, "/tmp/spme/up", &|_, _| {}).expect("resumed upload");
        assert!(run(&inst, &format!("sha256sum {path}"), None).unwrap().starts_with(&mine));
        // Another instance: the same server as the source. Through this
        // computer works; server to server needs the source to reach the
        // target's address, which inside the test bed (a port mapped on
        // this computer) it does not: the check says so and the copy fails
        // with the plain advice, leaving no key behind.
        run(&inst, "mkdir -p /tmp/spme/copy /tmp/spme/copy2", None).unwrap();
        let p = copy_between(&inst, &inst, file, "/tmp/spme/copy", CopyWay::ThroughHere, &|_, _| {}).expect("copy through here");
        assert_eq!(remote_size(&inst, &p), remote_size(&inst, file));
        eprintln!("copy through this computer: {p}");
        let reach = reaches(&inst, &inst);
        eprintln!("reaches itself at {}:{}: {reach}", inst.host, inst.port);
        match copy_between(&inst, &inst, file, "/tmp/spme/copy2", CopyWay::Direct, &|_, _| {}) {
            Ok(p) => assert!(reach && remote_size(&inst, &p) == remote_size(&inst, file)),
            Err(e) => {
                assert!(!reach && e.contains("Copy through this computer instead"), "{e}");
                assert!(!run(&inst, "grep -c spm-restore-copy ~/.ssh/authorized_keys || true", None).unwrap().trim().starts_with('1'));
                eprintln!("direct copy refused as expected: {e}");
            }
        }
        // Down to this computer.
        let got = download_backup(&inst, file, &dir, &|_, _| {}).expect("download");
        assert_eq!(std::fs::metadata(&got).unwrap().len(), remote_size(&inst, file));
        let _ = run(&inst, "rm -rf /tmp/spme", None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

