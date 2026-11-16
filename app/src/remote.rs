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
pub const FORGOTTEN: &str = "The passphrase or password is not kept on this computer (no keyring). Enter it again with Edit.";

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
    let sys = format!("{REMOTE_SPM} {args}");
    format!(
        "if [ -x {REMOTE_SPM} ]; then {}sh -c '{}'; elif [ -x \"{USER_SPM}\" ]; then echo {USER_MARK} >&2; \"{USER_SPM}\" {args}; else echo 'The server agent is not set up on this server.' >&2; exit 3; fi",
        sudo(inst, password),
        sys.replace('\'', "'\\''")
    )
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
        files: vec![(compose.unwrap_or("/home/sunoil/sentient/docker-compose.yml").to_string(), true)],
        env_files: vec![("/home/sunoil/sentient/.env".into(), true)],
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
    if det.os == Os::Windows {
        return win::deploy(inst, sink);
    }
    if det.arch != "x86_64" {
        return Err(failed(&format!("No server agent for Linux on {} processors.", det.arch), ""));
    }
    let me = agent_file(false, sink)?;
    let want = sentient_installer_core::manifest::sha256_file(&me)?;
    let have = run(inst, &format!("if [ -x {REMOTE_SPM} ]; then sha256sum {REMOTE_SPM}; else sha256sum \"{USER_SPM}\"; fi 2>/dev/null | cut -d' ' -f1"), None).unwrap_or_default();
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
            if let Err(e) = sudo_run(inst, &format!("install -D -m 0755 {staging} {REMOTE_SPM} && mkdir -p {REMOTE_JOBS}"), None) {
                clean(inst);
                return Err(failed(&format!("The agent could not be put in place at {REMOTE_SPM} (as administrator). The agent there is unchanged."), &e));
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
    let there = if is_windows(inst) {
        win::ps(inst, &format!("Test-Path '{}'", win::EXE))?.trim() == "True"
    } else {
        run(inst, &format!("[ -x {REMOTE_SPM} ] || [ -x \"{USER_SPM}\" ] && echo yes || echo no"), None)?.trim() == "yes"
    };
    if there { Ok(()) } else { Err(NOT_SET_UP.into()) }
}

/// What [`status`] says before the Platform Manager is on the server.
/// The Platform Manager there has no SENTIENT added yet: nothing to report.
pub const NOT_ADDED: &str = "No SENTIENT is added on this server.";

pub const NOT_SET_UP: &str = "The server agent is not set up on this server. Set up installs it, to read the server's state and work on it.";

/// The server's state, from its own Platform Manager (`--job status-json`).
pub fn status(inst: &Instance) -> Result<RemoteStatus, String> {
    #[cfg(debug_assertions)]
    if let Some(secs) = std::env::var("SPM_DEBUG_REMOTE_SLOW").ok().and_then(|s| s.parse::<u64>().ok()) {
        std::thread::sleep(std::time::Duration::from_secs(secs));
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
    if run(inst, &format!("[ -x {REMOTE_SPM} ] || [ -x \"{USER_SPM}\" ] && echo yes || echo no"), None)?.trim() != "yes" {
        return Err(NOT_SET_UP.into());
    }
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
}

/// One service or container on the server (Status and Control).
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

/// Ask the server's Platform Manager for `job` (as administrator) and read
/// the one JSON line it prints.
fn read_json<T: serde::de::DeserializeOwned>(inst: &Instance, job: &str) -> Result<T, String> {
    let out = spm_job(inst, job)?;
    // "null": nothing to report yet, no SENTIENT added there. Not a failure.
    if out.lines().any(|l| l.trim() == "null") {
        return Err(NOT_ADDED.into());
    }
    let line = out.lines().rev().find(|l| { let t = l.trim_start(); t.starts_with('{') || t.starts_with('[') }).ok_or("The server's Platform Manager gave no answer. Try again; if it stays, Show details on the job, or run Set up again.")?;
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
        return Ok(t.to_string_lossy().split(' ').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect());
    }
    let out = spm_job(inst, "gateway-status")?;
    let line = out.lines().rev().find(|l| l.starts_with("installed=")).ok_or("The server's Platform Manager gave no gateway state.")?;
    Ok(line.split(' ').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect())
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
    let out = if is_windows(inst) { spm_job(inst, "monitor-json")? } else { run(inst, &format!("A={REMOTE_SPM}; [ -x $A ] || A=\"{USER_SPM}\"; \"$A\" --job monitor-json"), None)? };
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
    let system = run(inst, &format!("[ -x {REMOTE_SPM} ] && echo yes || echo no"), None)?.trim() == "yes";
    if system {
        sudo_run(
            inst,
            &format!(
                "mkdir -p {REMOTE_JOBS} && systemd-run --unit=spm-job-{id} --collect --property=StandardOutput=file:{REMOTE_JOBS}/{id}.log --property=StandardError=file:{REMOTE_JOBS}/{id}.err {REMOTE_SPM} --job --json {}",
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

/// A live statistics reading of the server (crate::stats).
pub fn stats_live(inst: &Instance) -> Result<crate::stats::Sample, String> {
    #[cfg(debug_assertions)]
    if let Some(d) = std::env::var_os("SPM_DEBUG_STATS") {
        return crate::stats::debug_live(std::path::Path::new(&d));
    }
    read_json(inst, "stats-live")
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
        win::ps(inst, &format!("& '{}' --job {job}", win::EXE))
    } else {
        agent_run(inst, &format!("--job {job}"))
    };
    // An agent that does not know the job answers with its usage text: that
    // text goes to the activity log, the pages say the agent is out of date.
    r.map_err(|e| {
        if e.contains("usage: --job") || e.contains("Unknown job") {
            crate::maintenance_ops::log_line(&format!("{}: the agent does not know \"{job}\" (out of date)", inst.name));
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

/// The agent's file name for a server's system.
pub fn agent_name(windows: bool) -> &'static str {
    if windows { "sentient-platform-manager-agent-windows-x86_64.exe" } else { "sentient-platform-manager-agent-linux-x86_64" }
}

/// The server agent for Linux (static, no desktop libraries) or Windows:
/// the one this package carries beside the program (`agents/`), else the
/// one published with this version, checked against its SHA-256.
fn agent_file(windows: bool, sink: &ProgressFn) -> Result<PathBuf, String> {
    let name = agent_name(windows);
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
    if let Some(bytes) = if windows { embedded::WINDOWS } else { embedded::LINUX } {
        let dir = crate::updates::prefs_dir().join("remote-builds");
        let file = dir.join(format!("{}-{name}", crate::updates::manager_version()));
        if std::fs::metadata(&file).map(|m| m.len() as usize) .ok() != Some(bytes.len()) {
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            std::fs::write(&file, bytes).map_err(|e| format!("write {}: {e}", file.display()))?;
        }
        return Ok(file);
    }
    released_agent(name, sink).map_err(|e| {
        failed(
            "This Platform Manager package does not carry the server agent, and none could be downloaded for its version.",
            &e,
        )
    })
}

/// The agents this build carries (build.rs).
mod embedded {
    include!(concat!(env!("OUT_DIR"), "/agents.rs"));
}

/// The agent published with this Platform Manager's version.
fn released_agent(name: &str, sink: &ProgressFn) -> Result<PathBuf, String> {
    use sentient_installer_core::releases;
    let version = crate::updates::manager_version().to_string();
    let dir = crate::updates::prefs_dir().join("remote-builds");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    sink(Progress::Step { name: format!("Getting the server agent of Platform Manager {version}") });
    let all = releases::fetch(releases::MANAGER_REPO)?;
    let r = all
        .iter()
        .find(|r| releases::is_manager(r) && releases::manager_version_of(r) == version)
        .ok_or_else(|| format!("Platform Manager {version} is not published"))?;
    let asset = r.assets.iter().find(|a| a.name == name).ok_or_else(|| format!("release {version} has no {name}"))?;
    let sha = crate::updates::published_sha256(r, asset).ok_or("no published checksum")?;
    let file = dir.join(format!("{version}-{name}"));
    let ok = |f: &Path| sentient_installer_core::manifest::sha256_file(f).is_ok_and(|h| h.eq_ignore_ascii_case(&sha));
    if !ok(&file) {
        let resp = sentient_installer_core::net::agent().get(&asset.browser_download_url).set("User-Agent", "SENTIENT-Platform-Manager").call().map_err(|e| format!("download {name}: {e}"))?;
        let mut out = std::fs::File::create(&file).map_err(|e| e.to_string())?;
        std::io::copy(&mut resp.into_reader(), &mut out).map_err(|e| format!("download {name}: {e}"))?;
        if !ok(&file) {
            let _ = std::fs::remove_file(&file);
            return Err(format!("{name} does not match its published checksum"));
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

    /// Run a PowerShell script on the server (encoded, so no quoting
    /// passes through cmd).
    pub fn ps(inst: &Instance, script: &str) -> Result<String, String> {
        use base64::Engine as _;
        let utf16: Vec<u8> = format!("$ProgressPreference='SilentlyContinue'; {script}").encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let enc = base64::engine::general_purpose::STANDARD.encode(utf16);
        run(inst, &format!("powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand {enc}"), None)
    }

    pub fn deploy(inst: &Instance, sink: &ProgressFn) -> Result<(), String> {
        let file = agent_file(true, sink)?;
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
        Ok(())
    }

    pub fn status(inst: &Instance) -> Result<RemoteStatus, String> {
        if ps(inst, &format!("Test-Path '{EXE}'"))?.trim() != "True" {
            return Err(NOT_SET_UP.into());
        }
        let out = ps(inst, &format!("& '{EXE}' --job status-json"))?;
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
        let cmdline = format!("/c \"\"{EXE}\" --job --json {} > \"{JOBS}\\{id}.log\" 2> \"{JOBS}\\{id}.err\"\"", quoted.join(" "));
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
        let me = agent_file(false, &sink).unwrap();
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
        let live = stats_live(&inst).unwrap();
        assert!(live.mem_total > 0 && live.boot > 0, "{live:?}");
        eprintln!("stats: {} readings, live cpu {:.1} %, {} cores", h.len(), live.cpu, live.cores);
        // Opening a server with an older agent on it (crate::manager::
        // remote_pages::refresh_agent: ready, then deploy) replaces it.
        sudo_run(&inst, &format!("sh -c 'printf old-agent > {REMOTE_SPM}'"), None).unwrap();
        assert!(ready(&inst).is_ok());
        deploy(&inst, &sink).unwrap();
        let there = run(&inst, &format!("sha256sum {REMOTE_SPM} | cut -d' ' -f1"), None).unwrap();
        let here = sentient_installer_core::manifest::sha256_file(&agent_file(false, &sink).unwrap()).unwrap();
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
    fn ssh_messages_read_plainly() {
        assert!(explain("@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @@@").unwrap().contains("not the one accepted"));
        assert!(explain("Permission denied (publickey).").unwrap().contains("refused the sign-in"));
        assert!(explain("something else").is_none());
    }
}
