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
    let mut c = Command::new("ssh");
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
pub const FORGOTTEN: &str = "The passphrase or password is not kept on this computer (no keyring here). Enter it again with Edit.";

/// ssh's messages in plain words.
fn explain(err: &str) -> Option<String> {
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
    let pattern = if port == 22 { host.to_string() } else { format!("[{host}]:{port}") };
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
pub fn deploy(inst: &Instance, sink: &ProgressFn) -> Result<(), String> {
    let det = inst.detected.clone().ok_or("Connect to the server first.")?;
    if det.os != Os::Linux {
        return Err("Remote Windows servers come in the next step; Linux servers work now.".into());
    }
    let local_arch = std::env::consts::ARCH;
    if !cfg!(target_os = "linux") || det.arch != local_arch {
        return Err(format!("This Platform Manager is built for {} {local_arch}; the server is Linux {}. A matching build is downloaded in the next step.", std::env::consts::OS, det.arch));
    }
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let want = sentient_installer_core::manifest::sha256_file(&me)?;
    let have = run(inst, &format!("sha256sum {REMOTE_SPM} 2>/dev/null | cut -d' ' -f1"), None).unwrap_or_default();
    if have.trim() == want {
        sink(Progress::Log { line: "The Platform Manager on the server is up to date.".into() });
        return Ok(());
    }
    sink(Progress::Step { name: "Copying the Platform Manager to the server".into() });
    let staging = format!("/tmp/spm-upload-{}", &want[..12]);
    sftp_put(inst, &me, &staging)?;
    let got = run(inst, &format!("sha256sum {staging} | cut -d' ' -f1"), None)?;
    if got.trim() != want {
        return Err("The copy on the server does not match (checksum); nothing was replaced. Try again.".into());
    }
    let pw = secret(&inst.id);
    sudo_run(inst, &format!("install -D -m 0755 {staging} {REMOTE_SPM} && rm -f {staging} && mkdir -p {REMOTE_JOBS}"), pw.as_deref())?;
    Ok(())
}

/// Copy a file with sftp, resuming a part already there.
fn sftp_put(inst: &Instance, local: &Path, remote: &str) -> Result<(), String> {
    let mut c = Command::new("sftp");
    c.args(["-P", &inst.port.to_string(), "-b", "-"])
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
        let _ = writeln!(s, "reput \"{}\" \"{remote}\"", local.display());
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("The copy to the server failed: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
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
    if err.contains("a password is required") || err.contains("sudo:") {
        return Err(if password.is_some() {
            format!("sudo refused the administrator password for {} on the server. Enter it again with Edit.", inst.user)
        } else {
            format!("{} needs administrator rights on the server: enter its sudo password with Edit, sign in as root, or allow it sudo without a password.", inst.user)
        });
    }
    Err(explain(&err).unwrap_or(err))
}

/// What [`status`] says before the Platform Manager is on the server.
pub const NOT_SET_UP: &str = "The Platform Manager is not on this server yet. Set it up to see the server's state and work on it.";

/// The server's state, from its own Platform Manager (`--job status-json`).
pub fn status(inst: &Instance) -> Result<RemoteStatus, String> {
    // Development aid: canned answers by instance id (screens).
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("SPM_DEBUG_REMOTE_STATUS") {
        let all: HashMap<String, RemoteStatus> = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        if let Some(s) = all.get(&inst.id) {
            return Ok(s.clone());
        }
    }
    if inst.detected.as_ref().is_some_and(|d| d.os == Os::Windows) {
        return Err("Remote Windows servers come in the next step; Linux servers work now.".into());
    }
    if run(inst, &format!("test -x {REMOTE_SPM} && echo yes || echo no"), None)?.trim() != "yes" {
        return Err(NOT_SET_UP.into());
    }
    let pw = secret(&inst.id);
    let out = sudo_run(inst, &format!("{REMOTE_SPM} --job status-json"), pw.as_deref())?;
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
}

/// The server's processor, memory and disk now (the top bar while it is
/// open). No administrator rights needed.
pub fn monitor(inst: &Instance) -> Result<crate::sysmon::Monitor, String> {
    // Development aid: a canned reading (screens).
    #[cfg(debug_assertions)]
    if let Some(t) = std::env::var_os("SPM_DEBUG_REMOTE_MONITOR") {
        return serde_json::from_str(&t.to_string_lossy()).map_err(|e| e.to_string());
    }
    let out = run(inst, &format!("{REMOTE_SPM} --job monitor-json"), None)?;
    let line = out.lines().rev().find(|l| l.trim_start().starts_with('{')).ok_or("no reading")?;
    serde_json::from_str(line).map_err(|e| e.to_string())
}

/// The id of the job running on the server, if one is.
pub fn running_job(inst: &Instance) -> Option<String> {
    let pw = secret(&inst.id);
    let out = sudo_run(inst, "systemctl list-units --type=service --state=running --no-legend --plain 'spm-job-*' | head -1", pw.as_deref()).ok()?;
    out.split_whitespace().next()?.strip_prefix("spm-job-")?.strip_suffix(".service").map(str::to_string)
}

/// Start `--job <args>` on the server, detached from this connection.
/// Returns the job's id; its progress goes to a file [`follow`] reads.
pub fn start_job(inst: &Instance, args: &[&str]) -> Result<String, String> {
    let id = format!("{}", chrono::Utc::now().format("%Y%m%d%H%M%S"));
    let pw = secret(&inst.id);
    // One job at a time on a server.
    let busy = sudo_run(inst, "systemctl list-units --type=service --state=running --no-legend 'spm-job-*' | head -1", pw.as_deref()).unwrap_or_default();
    if !busy.trim().is_empty() {
        return Err("Another job is still running on this server. Wait until it ends.".into());
    }
    let quoted: Vec<String> = args.iter().map(|a| format!("'{}'", a.replace('\'', ""))).collect();
    sudo_run(
        inst,
        &format!(
            "mkdir -p {REMOTE_JOBS} && systemd-run --unit=spm-job-{id} --collect --property=StandardOutput=file:{REMOTE_JOBS}/{id}.log --property=StandardError=file:{REMOTE_JOBS}/{id}.err {REMOTE_SPM} --job --json {}",
            quoted.join(" ")
        ),
        pw.as_deref(),
    )?;
    Ok(id)
}

/// Follow a job's progress until it ends, reconnecting when the connection
/// drops (the job keeps running there). Blocking.
pub fn follow(inst: &Instance, id: &str, sink: &ProgressFn) -> Result<(), String> {
    let mut seen = 0usize;
    let mut drops = 0u32;
    loop {
        let mut child = ssh(inst, &format!("tail -n +{} -F {REMOTE_JOBS}/{id}.log 2>/dev/null & TP=$!; while systemctl is-active --quiet spm-job-{id} 2>/dev/null; do sleep 2; done; sleep 2; kill $TP 2>/dev/null; echo '@@END'", seen + 1), None)
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
    let pw = secret(&inst.id);
    sudo_run(inst, &format!("{REMOTE_SPM} --job logs {lines}"), pw.as_deref())
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
        // A changed key afterwards is refused.
        std::fs::write(known_hosts(), keys.replace("AAAA", "AAAB")).unwrap();
        assert!(run(&inst, "true", Some(v[4])).unwrap_err().contains("not the one accepted"));
        untrust(v[0], port);
    }

    #[test]
    fn ssh_messages_read_plainly() {
        assert!(explain("@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @@@").unwrap().contains("not the one accepted"));
        assert!(explain("Permission denied (publickey).").unwrap().contains("refused the sign-in"));
        assert!(explain("something else").is_none());
    }
}
