// Copyright © 2016-2026 The SENTIENT Authors

//! What went wrong in a failed step, in plain words, and the fix.
//!
//! Every failed job (Setup's install, and every Platform Manager job:
//! update, repair, migration, move data, restore, and the same jobs on a
//! server, where the agent runs this code and sends its answer back as one
//! `DIAG\t<json>` line) goes through here:
//!
//! 1. [`collect`] gathers the evidence: the error and the job's last output
//!    lines, its exit code, the failed services and their journal, free
//!    disk space, who holds the ports the error names, Docker's state, an
//!    older-major PostgreSQL data folder, and on Windows WSL's state and the
//!    service/event log. [`Evidence::from_text`] is the text-only form, for
//!    a server whose agent is too old to diagnose itself.
//! 2. [`diagnose`] matches the evidence against a table of known causes
//!    ([`RULES`]). Each gives a one-line cause, a one-line why, and its
//!    fixes: a one-click [`FixAction`] where the fix is safe and reversible
//!    (data is moved aside, never deleted), else the exact command or steps.
//! 3. Unknown causes still say something real: the last error lines the
//!    step printed, not the wrapper's exit code.
//!
//! The UI is the app's error panel (app/src/diagnosis.rs).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::old_pg::{self, Family, OldPgData};
use crate::progress::{Progress, ProgressFn};

/// The prefix of the progress line that carries a diagnosis from an agent.
pub const DIAG_PREFIX: &str = "DIAG\t";

/// A service that failed, with the end of its journal.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UnitLog {
    pub unit: String,
    pub journal: Vec<String>,
}

/// Free space where SENTIENT writes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Disk {
    pub mount: String,
    pub avail: u64,
    pub used_pct: u8,
}

/// Who listens on a port the error names.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PortUse {
    pub port: u16,
    pub owner: String,
    /// The next free port from it, for "Use port NNNN instead".
    pub free: Option<u16>,
}

/// Docker's state, when the step involved Docker.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DockerProbe {
    pub running: bool,
    pub permission_denied: bool,
    pub in_group: bool,
    pub text: String,
}

/// Everything known about a failure.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Evidence {
    /// The job's error message.
    pub error: String,
    /// The job's last output lines (stdout and stderr, in order).
    pub lines: Vec<String>,
    pub exit_code: Option<i32>,
    pub units: Vec<UnitLog>,
    pub disks: Vec<Disk>,
    pub ports: Vec<PortUse>,
    pub docker: Option<DockerProbe>,
    pub old_pg: Option<OldPgData>,
    /// The Platform Manager's download cache and its size.
    pub cache: Option<(PathBuf, u64)>,
    /// Windows: WSL's state, the services' state, recent service errors
    /// from the event log.
    pub windows: Vec<String>,
}

/// A fix that runs with one click.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "kebab-case")]
pub enum FixAction {
    /// Upgrade the older-major PostgreSQL data (it is kept).
    UpgradePgData { family: Family },
    /// Move it to `<data>.old-<date>` and start fresh.
    KeepPgDataAside { family: Family },
    /// Use another port: the page that asked applies it (Setup's field).
    UsePort { port: u16, instead: u16 },
    StartDocker,
    /// Empty the Platform Manager's download cache.
    ClearDownloadCache { dir: PathBuf, bytes: u64 },
    /// Ask for the administrator password again: the page runs the step
    /// again (Retry).
    AskAgain,
    /// Windows: start WSL.
    StartWsl,
    /// Stop a systemd service and keep it from starting at boot (a unit
    /// whose file is gone is only stopped).
    StopService { unit: String },
    /// Remove a PostgreSQL that is in the way: stop it, remove its package
    /// (when one is installed), clean its unit up, and move its data folder
    /// aside to `<dir>.old-<date>`. The data is never deleted.
    RemovePostgres { unit: String, package: Option<String>, data_dir: String },
    /// Stop a Docker container (it stays, and can be started again).
    StopContainer { name: String },
    /// Run the check again: the page that asked does it.
    Recheck,
}

impl FixAction {
    /// Applied by the page that shows the panel, not by [`apply`].
    pub fn is_page_action(&self) -> bool {
        matches!(self, FixAction::UsePort { .. } | FixAction::AskAgain | FixAction::Recheck)
    }

    /// The exact shell [`apply`] runs for it (as administrator, except
    /// where noted), for "Show command". None: nothing to show (a page
    /// action, or a fix done in-process).
    pub fn command(&self) -> Option<String> {
        match self {
            FixAction::StopService { unit } => Some(stop_service_script(unit)),
            FixAction::RemovePostgres { unit, package, data_dir } => {
                Some(remove_postgres_script(package_manager_here(), unit, package.as_deref(), data_dir))
            }
            FixAction::StopContainer { name } => Some(format!("docker stop {}", quote(name))),
            FixAction::StartDocker => Some(START_DOCKER.to_string()),
            #[cfg(unix)]
            FixAction::UpgradePgData { family } => Some(old_pg::fix_script(*family, old_pg::OldPgChoice::Upgrade)),
            #[cfg(unix)]
            FixAction::KeepPgDataAside { family } => Some(old_pg::fix_script(*family, old_pg::OldPgChoice::KeepAside)),
            FixAction::StartWsl => Some("wsl.exe --exec true".into()),
            _ => None,
        }
    }
}

/// A unit or container name as given: refused when it holds anything a
/// shell could read as more than a name.
#[cfg_attr(not(unix), allow(dead_code))]
fn safe_name(name: &str) -> Result<&str, String> {
    let ok = !name.is_empty() && !name.starts_with('-') && name.len() <= 255 && name.chars().all(|c| c.is_ascii_alphanumeric() || "@._:-+\\".contains(c));
    if ok { Ok(name) } else { Err(format!("“{name}” is not a service or container name.")) }
}

/// A value single-quoted for a POSIX shell.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

const START_DOCKER: &str = "systemctl enable --now docker.socket docker.service\nfor i in $(seq 1 30); do docker info >/dev/null 2>&1 && exit 0; sleep 1; done\necho '!! Docker did not answer within 30 seconds'; exit 1\n";

/// Stop `unit` and keep it from starting at boot. `disable` fails for a
/// unit whose file is gone (a removed package's service still running), so
/// that one is stopped only.
pub fn stop_service_script(unit: &str) -> String {
    let u = quote(unit);
    format!(
        "echo '>> Stopping {unit}'\n\
         systemctl disable --now {u} 2>/dev/null || systemctl stop {u}\n\
         systemctl reset-failed {u} 2>/dev/null || true\n"
    )
}

/// The package manager on this computer, by its program's name.
pub fn package_manager_here() -> Option<&'static str> {
    ["pacman", "apt-get", "dnf", "zypper"].into_iter().find(|b| {
        std::process::Command::new("sh").arg("-c").arg(format!("command -v {b}")).output().is_ok_and(|o| o.status.success())
    })
}

/// The command that removes `package` with package manager `pm` (its
/// program's name). Removes, never purges: a purge deletes configuration.
pub fn package_remove(pm: &str, package: &str) -> String {
    let p = quote(package);
    match pm {
        "pacman" => format!("pacman -R --noconfirm {p}"),
        "apt-get" => format!("DEBIAN_FRONTEND=noninteractive apt-get remove -y {p}"),
        "dnf" => format!("dnf remove -y {p}"),
        "zypper" => format!("zypper --non-interactive remove {p}"),
        other => format!("echo '!! No way to remove packages with {other}'; exit 1"),
    }
}

/// [`FixAction::RemovePostgres`]: stop the service, remove its package,
/// clean the unit up (a hand-written unit file under /etc is renamed
/// aside), and move the data folder to `<dir>.old-<date>`. Nothing is
/// deleted.
pub fn remove_postgres_script(pm: Option<&str>, unit: &str, package: Option<&str>, data_dir: &str) -> String {
    let u = quote(unit);
    let mut s = format!(
        "set -e\n\
         echo '>> Stopping {unit}'\n\
         systemctl disable --now {u} 2>/dev/null || systemctl stop {u} 2>/dev/null || true\n"
    );
    if let (Some(pkg), Some(pm)) = (package, pm) {
        s.push_str(&format!("echo '>> Removing the package {pkg}'\n{}\n", package_remove(pm, pkg)));
    }
    s.push_str(&format!(
        "echo '>> Cleaning up the service'\n\
         F=$(systemctl show -p FragmentPath --value {u} 2>/dev/null || true)\n\
         case \"$F\" in /etc/*) if [ -f \"$F\" ]; then mv \"$F\" \"$F.removed-$(date +%Y%m%d-%H%M%S)\"; fi ;; esac\n\
         systemctl daemon-reload\n\
         systemctl reset-failed {u} 2>/dev/null || true\n\
         DATA={d}\n\
         if [ -e \"$DATA\" ]; then\n\
         \x20 ASIDE=\"$DATA.old-$(date +%Y%m%d-%H%M%S)\"\n\
         \x20 mv \"$DATA\" \"$ASIDE\"\n\
         \x20 echo \">> The PostgreSQL data is kept in $ASIDE (nothing was deleted)\"\n\
         fi\n",
        d = quote(data_dir)
    ));
    s
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Fix {
    Action { label: String, action: FixAction },
    /// A command to copy and run.
    Command { label: String, command: String },
    Steps { text: String },
}

/// What went wrong, and the fix.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnosis {
    /// The cause's key ("pg-old-major", …, "unknown").
    pub id: String,
    /// One plain line: what went wrong.
    pub cause: String,
    /// One line: why it stops the step.
    pub why: String,
    pub fixes: Vec<Fix>,
    /// The real last error lines the step printed.
    pub last_lines: Vec<String>,
    /// Everything, for "Copy details".
    pub details: String,
}

impl Diagnosis {
    /// The line an agent prints for the client.
    pub fn encode(&self) -> String {
        format!("{DIAG_PREFIX}{}", serde_json::to_string(self).unwrap_or_default())
    }

    pub fn decode(line: &str) -> Option<Diagnosis> {
        serde_json::from_str(line.strip_prefix(DIAG_PREFIX)?).ok()
    }

    pub fn is_known(&self) -> bool {
        self.id != "unknown"
    }
}

// ---------------------------------------------------------------------------
// Evidence
// ---------------------------------------------------------------------------

/// The exit code a wrapper error names: "pkexec exited with 2",
/// "exit status: 42", "exit=100", "exit code 1".
pub fn exit_code_of(text: &str) -> Option<i32> {
    for key in ["exited with ", "exit status: ", "exit status ", "exit code ", "exit=", "status="] {
        if let Some(i) = text.rfind(key) {
            let rest = &text[i + key.len()..];
            if let Some(n) = rest.split(|c: char| !c.is_ascii_digit() && c != '-').next().and_then(|n| n.parse().ok()) {
                return Some(n);
            }
        }
    }
    None
}

/// Port numbers the text names ("port 5432", ":5432", "5432 (PostgreSQL)").
pub fn ports_named(text: &str) -> Vec<u16> {
    let mut v: Vec<u16> = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'.') {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let n = &text[start..i];
            let before = text[..start].to_ascii_lowercase();
            let after = &text[i..];
            // ":5432", but not the minutes of "15:50".
            let colon = before.ends_with(':') && !before[..before.len() - 1].ends_with(|c: char| c.is_ascii_digit());
            let ported = before.ends_with("port ") || before.ends_with("port: ") || before.ends_with("port=") || colon
                || after.starts_with(" (");
            let dotted = after.starts_with('.') && after[1..].starts_with(|c: char| c.is_ascii_digit());
            if ported && !dotted && n.len() >= 2 && n.len() <= 5 {
                if let Ok(p) = n.parse::<u16>() {
                    if p >= 20 && !v.contains(&p) {
                        v.push(p);
                    }
                }
            }
        } else {
            i += 1;
        }
    }
    v
}

impl Evidence {
    /// Only what the job printed (no look at the machine): a server whose
    /// agent does not diagnose itself, or a test.
    pub fn from_text(error: &str, lines: &[String]) -> Evidence {
        let mut ev = Evidence { error: error.to_string(), lines: tail(lines, 200), ..Default::default() };
        ev.exit_code = exit_code_of(error);
        // The exit-42 line names both majors.
        ev.old_pg = old_major_line(&ev.text());
        ev
    }

    /// The error and every line, journals included, as one text.
    pub fn text(&self) -> String {
        let mut t = String::with_capacity(4096);
        t.push_str(&self.error);
        for l in &self.lines {
            t.push('\n');
            t.push_str(l);
        }
        for u in &self.units {
            for l in &u.journal {
                t.push('\n');
                t.push_str(l);
            }
        }
        if let Some(d) = &self.docker {
            t.push('\n');
            t.push_str(&d.text);
        }
        for w in &self.windows {
            t.push('\n');
            t.push_str(w);
        }
        t
    }
}

fn tail(lines: &[String], n: usize) -> Vec<String> {
    lines[lines.len().saturating_sub(n)..].to_vec()
}

/// "!! /var/lib/postgres/data holds a PostgreSQL 17 database; PostgreSQL 18 is installed."
fn old_major_line(text: &str) -> Option<OldPgData> {
    let line = text.lines().find(|l| l.contains("holds a PostgreSQL") && l.contains("is installed"))?;
    let dir = line.trim_start_matches("!! ").split(" holds a PostgreSQL").next()?.trim();
    let have = old_pg::major_of(line.split("holds a PostgreSQL").nth(1)?)?;
    let want = old_pg::major_of(line.split("; PostgreSQL").nth(1)?)?;
    let family = if dir.starts_with("/var/lib/pgsql") { Family::Rhel } else if dir.starts_with("/var/lib/postgresql") { Family::Debian } else { Family::Arch };
    Some(OldPgData { family, dir: PathBuf::from(dir), have: Some(have), want: Some(want) })
}

#[allow(dead_code)]
fn run(cmd: &str) -> Option<(bool, String)> {
    #[cfg(unix)]
    let o = crate::sys::command("sh").arg("-c").arg(cmd).output().ok()?;
    #[cfg(windows)]
    let o = crate::sys::command("powershell").args(["-NoProfile", "-NonInteractive", "-Command", cmd]).output().ok()?;
    let mut s = crate::sys::decode(&o.stdout);
    let e = crate::sys::decode(&o.stderr);
    if !e.trim().is_empty() {
        s.push_str(&e);
    }
    Some((o.status.success(), s.trim().to_string()))
}

/// Size of a folder, in bytes (best effort).
pub fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            match e.metadata() {
                Ok(m) if m.is_dir() => stack.push(e.path()),
                Ok(m) => total += m.len(),
                Err(_) => {}
            }
        }
    }
    total
}

/// Gather the evidence for a failed step on this machine. `cache_dir` is
/// the Platform Manager's download cache (offered when the disk is full).
pub fn collect(error: &str, lines: &[String], cache_dir: Option<&Path>) -> Evidence {
    let mut ev = Evidence::from_text(error, lines);

    #[cfg(unix)]
    {
        let lower = ev.text().to_ascii_lowercase();
        // Failed services that belong to SENTIENT, and any the output names.
        if let Some((_, out)) = run("systemctl list-units --failed --plain --no-legend --type=service 2>/dev/null") {
            for unit in out.lines().filter_map(|l| l.split_whitespace().next()) {
                let base = unit.trim_end_matches(".service");
                let ours = ["postgresql", "clickhouse", "sentient", "docker", "st-gateway", "spm-job"].iter().any(|p| base.starts_with(p));
                if ours || lower.contains(&base.to_ascii_lowercase()) {
                    let journal = run(&format!("journalctl -u {} -n 40 --no-pager -o cat 2>/dev/null", crate::exec::shq(unit)))
                        .map(|(_, j)| j.lines().map(str::to_string).collect())
                        .unwrap_or_default();
                    ev.units.push(UnitLog { unit: unit.to_string(), journal });
                }
                if ev.units.len() >= 4 {
                    break;
                }
            }
        }
        // Free space where things are written.
        let mut paths: Vec<String> = ["/", "/var/lib", "/opt", "/tmp"].iter().map(|s| s.to_string()).collect();
        if let Some(c) = cache_dir {
            paths.push(c.display().to_string());
        }
        let existing: Vec<String> = paths.into_iter().filter(|p| Path::new(p).exists()).map(|p| crate::exec::shq(&p)).collect();
        if let Some((_, out)) = run(&format!("df -Pk {} 2>/dev/null", existing.join(" "))) {
            ev.disks = parse_df(&out);
        }
        if lower.contains("docker") {
            let (ok, info) = run("docker info --format '{{.ServerVersion}}' 2>&1").unwrap_or((false, String::new()));
            let groups = run("id -nG").map(|(_, g)| g).unwrap_or_default();
            ev.docker = Some(DockerProbe {
                running: ok || info.to_ascii_lowercase().contains("permission denied"),
                permission_denied: info.to_ascii_lowercase().contains("permission denied"),
                in_group: groups.split_whitespace().any(|g| g == "docker") || run("id -u").is_some_and(|(_, u)| u == "0"),
                text: if ok { String::new() } else { info },
            });
        }
        if ev.old_pg.is_none() && (lower.contains("postgres") || lower.contains("exited with 42")) {
            ev.old_pg = old_pg::find().filter(|f| f.is_older());
        }
    }

    #[cfg(windows)]
    {
        ev.disks = windows_disks();
        if let Some((_, s)) = run("wsl.exe --status") {
            ev.windows.push(format!("wsl --status: {}", s.lines().take(6).collect::<Vec<_>>().join(" | ")));
        }
        if let Some((_, s)) = run("wsl.exe -l -v") {
            ev.windows.push(format!("wsl -l -v: {}", s.replace('\0', "").lines().take(6).collect::<Vec<_>>().join(" | ")));
        }
        if let Some((_, s)) = run("Get-Service -Name 'SENTIENT*','postgresql*','sentient*' -ErrorAction SilentlyContinue | ForEach-Object { $_.Name + ' ' + $_.Status }") {
            ev.windows.extend(s.lines().map(|l| format!("service: {l}")));
        }
        if let Some((_, s)) = run(
            "Get-WinEvent -FilterHashtable @{LogName='System'; ProviderName='Service Control Manager'; Level=2; StartTime=(Get-Date).AddHours(-2)} -MaxEvents 8 -ErrorAction SilentlyContinue | ForEach-Object { $_.TimeCreated.ToString('HH:mm:ss') + ' ' + ($_.Message -replace '\\s+',' ') }",
        ) {
            ev.windows.extend(s.lines().map(|l| format!("event: {l}")));
        }
    }

    // Who holds the ports the output (journals included) names.
    let text = ev.text();
    let lower = text.to_ascii_lowercase();
    let busy = lower.contains("already in use") || lower.contains("already allocated") || ev.exit_code == Some(43);
    if busy {
        for port in ports_named(&text).into_iter().take(4) {
            if let Some(owner) = crate::checks::port_owner(port) {
                ev.ports.push(PortUse { port, owner: owner.describe(), free: crate::checks::next_free_port(port + 1) });
            }
        }
    }
    if let Some(c) = cache_dir.filter(|c| c.exists()) {
        ev.cache = Some((c.to_path_buf(), dir_size(c)));
    }
    ev
}

#[cfg(windows)]
fn windows_disks() -> Vec<Disk> {
    use sysinfo::Disks;
    Disks::new_with_refreshed_list()
        .list()
        .iter()
        .map(|d| {
            let total = d.total_space().max(1);
            Disk {
                mount: d.mount_point().display().to_string(),
                avail: d.available_space(),
                used_pct: (100 - d.available_space() * 100 / total) as u8,
            }
        })
        .collect()
}

/// `df -Pk` rows, one per mount.
pub fn parse_df(out: &str) -> Vec<Disk> {
    let mut v: Vec<Disk> = Vec::new();
    for l in out.lines().skip(1) {
        let w: Vec<&str> = l.split_whitespace().collect();
        if w.len() < 6 {
            continue;
        }
        let (Ok(avail), Ok(pct)) = (w[3].parse::<u64>(), w[4].trim_end_matches('%').parse::<u8>()) else { continue };
        let mount = w[5..].join(" ");
        if !v.iter().any(|d| d.mount == mount) {
            v.push(Disk { mount, avail: avail * 1024, used_pct: pct });
        }
    }
    v
}

// ---------------------------------------------------------------------------
// Matching
// ---------------------------------------------------------------------------

/// One known cause: its key and the matcher.
pub struct Rule {
    pub id: &'static str,
    pub matches: fn(&Evidence, &str) -> Option<Diagnosis>,
}

/// The known causes, most specific first.
pub const RULES: &[Rule] = &[
    Rule { id: "ssh-host-key", matches: ssh_host_key },
    Rule { id: "ssh-network", matches: ssh_network },
    Rule { id: "auth-cancelled", matches: auth_cancelled },
    Rule { id: "not-admin", matches: not_admin },
    Rule { id: "win-replace-file", matches: win_replace_file },
    Rule { id: "pg-old-major", matches: pg_old_major },
    Rule { id: "port-in-use", matches: port_in_use },
    Rule { id: "disk-full", matches: disk_full },
    Rule { id: "data-permission", matches: data_permission },
    Rule { id: "pkg-lock", matches: pkg_lock },
    Rule { id: "pkg-network", matches: pkg_network },
    Rule { id: "docker-group", matches: docker_group },
    Rule { id: "docker-down", matches: docker_down },
    Rule { id: "clickhouse-memory", matches: clickhouse_memory },
    Rule { id: "clickhouse-limits", matches: clickhouse_limits },
    Rule { id: "wsl-restart", matches: wsl_restart },
    Rule { id: "wsl-missing", matches: wsl_missing },
    Rule { id: "wsl-stopped", matches: wsl_stopped },
    Rule { id: "service-failed", matches: service_failed },
];

/// Match the evidence against [`RULES`]; the first that fits, else the
/// real last error lines.
pub fn diagnose(ev: &Evidence) -> Diagnosis {
    let text = ev.text();
    let mut d = RULES.iter().find_map(|r| (r.matches)(ev, &text)).unwrap_or_else(|| unknown(ev));
    d.last_lines = last_error_lines(ev);
    d.details = details(ev, &d);
    d
}

/// Collect and diagnose in one call (what jobs use on failure).
pub fn diagnose_failure(error: &str, lines: &[String], cache_dir: Option<&Path>) -> Diagnosis {
    diagnose(&collect(error, lines, cache_dir))
}

fn diag(id: &str, cause: impl Into<String>, why: impl Into<String>, fixes: Vec<Fix>) -> Option<Diagnosis> {
    Some(Diagnosis { id: id.into(), cause: cause.into(), why: why.into(), fixes, last_lines: Vec::new(), details: String::new() })
}

fn cmd(label: &str, command: impl Into<String>) -> Fix {
    Fix::Command { label: label.into(), command: command.into() }
}

fn steps(text: impl Into<String>) -> Fix {
    Fix::Steps { text: text.into() }
}

fn action(label: impl Into<String>, action: FixAction) -> Fix {
    Fix::Action { label: label.into(), action }
}

fn has(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| text.contains(n))
}

fn has_ci(lower: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| lower.contains(&n.to_ascii_lowercase()))
}

fn ssh_host_key(_: &Evidence, t: &str) -> Option<Diagnosis> {
    if !has(t, &["REMOTE HOST IDENTIFICATION HAS CHANGED", "Host key verification failed"]) {
        return None;
    }
    // "Offending ED25519 key in /home/u/.ssh/known_hosts:3"
    let offending = t.lines().find_map(|l| l.split("Offending ").nth(1)).and_then(|r| r.split(" key in ").nth(1)).map(str::trim);
    let host = t.lines().find_map(|l| l.strip_prefix("Host key for ")).and_then(|r| r.split(" has changed").next()).map(str::trim);
    let changed = t.contains("HAS CHANGED");
    let remove = match (host, offending) {
        (Some(h), _) => format!("ssh-keygen -R '{h}'"),
        (None, Some(o)) => format!("ssh-keygen -R <server>   # the entry is {o}"),
        _ => "ssh-keygen -R '[<server>]:<port>'".into(),
    };
    diag(
        "ssh-host-key",
        if changed { "The server's SSH host key has changed since the last connection." } else { "The server's SSH host key could not be verified." },
        "SSH refuses to connect: a reinstalled server has a new key, but so would someone intercepting the connection.",
        vec![
            steps("If the server was reinstalled or its SSH keys were regenerated: in the Platform Manager, remove the instance and add it again; it shows the new key's fingerprint to confirm (the Platform Manager keeps its own list of trusted keys). If not, ask the server's administrator before you do."),
            cmd("For your own ssh, remove the old key", remove),
        ],
    )
}

/// The connection to a server over SSH: no answer at all, or one that
/// drops part-way (a lossy network or VPN tunnel).
fn ssh_network(_: &Evidence, t: &str) -> Option<Diagnosis> {
    let dropped = has(t, &["client_loop:", "closed by remote host", "Timeout, server", "kex_exchange_identification", "The connection to the server keeps dropping"]);
    let silent = has(t, &["ssh: connect to host", "The server did not answer (address, port or firewall)", "did not answer as an SSH server"])
        && has_ci(&t.to_ascii_lowercase(), &["timed out", "did not answer", "no route to host", "network is unreachable"]);
    if !dropped && !silent {
        return None;
    }
    // "ssh: connect to host 10.10.0.5 port 22: Connection timed out",
    // "Connection to 10.10.0.5 closed by remote host."
    let host = t
        .lines()
        .find_map(|l| {
            let r = l.split("connect to host ").nth(1).or_else(|| l.split("Connection to ").nth(1)).or_else(|| l.split("Timeout, server ").nth(1))?;
            let h = r.split_whitespace().next()?.trim_end_matches([':', '.']);
            (!h.is_empty() && h != "the").then(|| h.to_string())
        });
    let name = host.clone().unwrap_or_else(|| "the server".into());
    let mut fixes = vec![steps(
        "Check the network between this computer and the server: a VPN or WireGuard tunnel that is down or losing packets, Wi-Fi, or a firewall. When the line is steady again, Retry.",
    )];
    if let Some(h) = &host {
        fixes.push(cmd("See how many packets get through (more than a few percent lost is too many)", format!("ping -c 20 {h}")));
    }
    if dropped {
        diag(
            "ssh-network",
            format!("The connection to {name} keeps dropping."),
            "SSH connected, then the line went quiet or was cut part-way through: the network is losing packets or the connection is unstable. The server itself may be fine.",
            fixes,
        )
    } else {
        diag(
            "ssh-network",
            format!("{} does not answer on the network.", if host.is_some() { name } else { "The server".into() }),
            "Nothing came back from its SSH port: the network or VPN is down, the address or port is wrong, or a firewall is in the way.",
            fixes,
        )
    }
}

/// Windows PowerShell 5.1: a file moved onto one that exists (its
/// Move-Item -Force does not replace files), or one in use.
fn win_replace_file(_: &Evidence, t: &str) -> Option<Diagnosis> {
    let exists = has(t, &["Cannot create a file when that file already exists"]);
    let in_use = has(t, &["being used by another process"]);
    if !exists && !in_use {
        return None;
    }
    let agent = t.contains("sentient-platform-manager.exe") || t.contains("The agent could not be put in place");
    diag(
        "win-replace-file",
        if agent { "The old agent on the Windows server could not be replaced." } else { "A file on the Windows server could not be replaced." },
        if in_use {
            "The file is in use by a program running on the server."
        } else {
            "Windows PowerShell 5.1 does not overwrite a file that is already there; older Platform Manager versions relied on that."
        },
        vec![
            steps("Retry: this version renames the old file aside first and puts it back if anything fails. If it still fails, close any Platform Manager window open on the server, then Retry."),
            cmd(
                "Or remove the old agent on the server (in an administrator PowerShell there), then Set up",
                "Remove-Item -Force 'C:\\ProgramData\\SENTIENT\\Platform Manager\\remote\\sentient-platform-manager.exe'",
            ),
        ],
    )
}

fn auth_cancelled(ev: &Evidence, t: &str) -> Option<Diagnosis> {
    let no_agent = has(t, &["No authentication agent found", "Error creating textual authentication agent"]);
    let refused = has(t, &["The password prompt was closed.", "Administrator authorisation was refused or cancelled", "Request dismissed", "Not authorized", "Authorization failed", "authentication failed"])
        || (ev.error.contains("pkexec exited with 126") || ev.error.contains("pkexec exited with 127"));
    let sudo = has(t, &["sudo: a password is required", "sudo: a terminal is required", "sudo: no tty present"]);
    // pkexec exited with 2 and the script printed nothing: the dialog was
    // closed before it ran.
    let silent = ev.error.contains("pkexec exited with 2")
        && ev.lines.iter().all(|l| l.trim().is_empty() || l.starts_with("Waiting for the administrator password"));
    if no_agent {
        return diag(
            "auth-no-agent",
            "No password prompt could open for the administrator rights this step needs.",
            "pkexec needs a desktop authentication agent, and none is running in this session.",
            vec![
                steps("Sign in to a desktop session (its agent shows the prompt), or run this from a terminal with sudo."),
                action("Ask again", FixAction::AskAgain),
            ],
        );
    }
    if sudo {
        return diag(
            "auth-sudo",
            "sudo needs a password, and there is nowhere to type it.",
            "The step runs as root through sudo without a terminal, so sudo cannot ask.",
            vec![steps("Run it from a terminal, or allow it without a password for this user (sudo visudo)."), action("Ask again", FixAction::AskAgain)],
        );
    }
    if refused || silent {
        return diag(
            "auth-cancelled",
            "The administrator password prompt was cancelled or refused.",
            "This step needs administrator rights, so it stopped before changing anything.",
            vec![action("Ask for the password again", FixAction::AskAgain)],
        );
    }
    None
}

/// The account the Platform Manager signs in with on a server is not an
/// administrator there (crate::remote's check, or the agent's own refusal).
fn not_admin(_: &Evidence, t: &str) -> Option<Diagnosis> {
    if !has(t, &["is not an administrator. The Platform Manager needs an administrator account there", "Run this from an elevated prompt"]) {
        return None;
    }
    diag(
        "not-admin",
        "The account the Platform Manager uses on this server is not an administrator.",
        "Its work there (services, databases, files) needs administrator rights, so the server refused it.",
        vec![steps("On the server, add that account to the Administrators group (Windows) or give it sudo rights (Linux); or edit the instance to sign in with an administrator account. Then Refresh.")],
    )
}

fn pg_old_major(ev: &Evidence, t: &str) -> Option<Diagnosis> {
    let said = has(t, &["An old version of the database format was found", "database files are incompatible with server", "holds a PostgreSQL", "a database from another PostgreSQL version"])
        || ev.exit_code == Some(42);
    // The upgrade itself is not possible here (exit 44): keep aside only.
    let blocked = has(t, &["cannot be upgraded here", "could not be upgraded on this computer"]) || ev.exit_code == Some(44);
    if !said && !blocked {
        return None;
    }
    let found = ev.old_pg.clone();
    let hinted = if has(t, &["/var/lib/postgres/", "wiki.archlinux.org", "postgresql-old-upgrade"]) {
        Some(Family::Arch)
    } else if t.contains("/var/lib/pgsql") {
        Some(Family::Rhel)
    } else if t.contains("/var/lib/postgresql/") {
        Some(Family::Debian)
    } else {
        None
    };
    let family = found.as_ref().map(|f| f.family).or(hinted).or_else(Family::here).unwrap_or(Family::Arch);
    let dir = found.as_ref().map(|f| f.dir.display().to_string()).unwrap_or_else(|| family.data_dir().to_string());
    let (have, want) = (found.as_ref().and_then(|f| f.have), found.as_ref().and_then(|f| f.want));
    let cause = match (have, want) {
        (Some(h), Some(w)) => format!("The PostgreSQL data folder is from PostgreSQL {h}; PostgreSQL {w} is installed."),
        _ => "The PostgreSQL data folder is from an older PostgreSQL version.".into(),
    };
    let to = want.map(|w| format!(" to PostgreSQL {w}")).unwrap_or_default();
    if blocked {
        let line = t.lines().find(|l| l.contains("cannot be upgraded here") || l.contains("provides PostgreSQL")).map(|l| l.trim_start_matches("!! ").to_string());
        return diag(
            "pg-old-major",
            format!("The PostgreSQL {} data cannot be upgraded on this computer.", have.map(|h| h.to_string()).unwrap_or_default()).replace("PostgreSQL  data", "PostgreSQL data"),
            line.unwrap_or_else(|| "The tools for its version are not available here.".into()),
            vec![
                action("Keep it aside and start fresh", FixAction::KeepPgDataAside { family }),
                steps("Or upgrade it one major version at a time by hand (pg_upgrade), then Retry."),
            ],
        );
    }
    diag(
        "pg-old-major",
        cause,
        format!("PostgreSQL only opens data made by its own major version, so it stops at start ({dir})."),
        vec![
            action(format!("Upgrade it{to}"), FixAction::UpgradePgData { family }),
            action("Keep it aside and start fresh", FixAction::KeepPgDataAside { family }),
        ],
    )
}

fn port_in_use(ev: &Evidence, t: &str) -> Option<Diagnosis> {
    let lower = t.to_ascii_lowercase();
    if !(has_ci(&lower, &["address already in use", "port is already allocated", "already in use", "because another program uses it"]) || ev.exit_code == Some(43)) {
        return None;
    }
    let named = ports_named(t);
    let port = ev.ports.first().map(|p| p.port).or_else(|| named.first().copied());
    let owner = ev.ports.first().map(|p| p.owner.clone());
    let cause = match (port, &owner) {
        (Some(p), Some(o)) => format!("Port {p} is already used by {o}."),
        (Some(p), None) => format!("Port {p} is already used by another program."),
        _ => "A port this step needs is already used by another program.".into(),
    };
    let mut fixes = Vec::new();
    if let Some(p) = port {
        let instead = ev.ports.first().and_then(|u| u.free).unwrap_or(p.saturating_add(1));
        fixes.push(action(format!("Use port {instead} instead"), FixAction::UsePort { port: p, instead }));
        #[cfg(windows)]
        fixes.push(cmd("See what uses it", format!("Get-NetTCPConnection -LocalPort {p} -State Listen | Select-Object OwningProcess")));
        #[cfg(not(windows))]
        fixes.push(cmd("See what uses it", format!("sudo ss -ltnp 'sport = :{p}'")));
    }
    fixes.push(steps("Or stop the program that uses it, then Retry."));
    diag("port-in-use", cause, "Two programs cannot listen on the same port, so the service stopped at start.", fixes)
}

fn disk_full(ev: &Evidence, t: &str) -> Option<Diagnosis> {
    let said = has(t, &["No space left on device", "ENOSPC", "not enough free disk space", "There is not enough space on the disk", "Disk quota exceeded"]);
    let low = ev.disks.iter().filter(|d| d.avail < 200 * 1024 * 1024 || d.used_pct >= 99).min_by_key(|d| d.avail);
    if !said && low.is_none() {
        return None;
    }
    let cause = match low {
        Some(d) => format!("The disk holding {} is full ({} free).", d.mount, human(d.avail)),
        None => "A disk this step writes to is full.".into(),
    };
    let mut fixes = Vec::new();
    if let Some((dir, bytes)) = ev.cache.clone().filter(|(_, b)| *b > 0) {
        fixes.push(action(format!("Free {}: clear the download cache", human(bytes)), FixAction::ClearDownloadCache { dir, bytes }));
    }
    #[cfg(windows)]
    fixes.push(steps("Free space on that drive (Settings > System > Storage), then Retry."));
    #[cfg(not(windows))]
    {
        let m = low.map(|d| d.mount.clone()).unwrap_or_else(|| "/".into());
        fixes.push(cmd("See what uses the space", format!("sudo du -xh --max-depth=2 {m} 2>/dev/null | sort -h | tail -20")));
    }
    diag("disk-full", cause, "Nothing more can be written there, so the step stopped.", fixes)
}

fn data_permission(_: &Evidence, t: &str) -> Option<Diagnosis> {
    let line = t.lines().find(|l| {
        (l.contains("Permission denied") || l.contains("permission denied")) && has(l, &["/var/lib/postgres", "/var/lib/pgsql", "/var/lib/postgresql", "/var/lib/clickhouse", "/var/lib/sentient", "data directory", "/opt/sentient"])
    });
    let invalid = t.lines().find(|l| l.contains("has invalid permissions") || l.contains("has wrong ownership"));
    let line = line.or(invalid)?;
    // The folder the line names.
    let path = line.split(['"', '\'', ' ']).find(|w| w.starts_with("/var/lib/") || w.starts_with("/opt/")).unwrap_or("").trim_end_matches(':').to_string();
    let (dir, owner) = if path.contains("clickhouse") {
        ("/var/lib/clickhouse".to_string(), "clickhouse:clickhouse")
    } else if path.contains("sentient") {
        (path.split("/sentient").next().map(|p| format!("{p}/sentient")).unwrap_or(path.clone()), "sentient:sentient")
    } else {
        let base = ["/var/lib/postgres/data", "/var/lib/pgsql/data"].into_iter().find(|b| path.starts_with(b)).map(str::to_string).unwrap_or_else(|| path.clone());
        (base, "postgres:postgres")
    };
    let dir = if dir.is_empty() { "<data folder>".to_string() } else { dir };
    let mode = if owner.starts_with("postgres") { format!(" && sudo chmod 700 {dir}") } else { String::new() };
    diag(
        "data-permission",
        format!("The service cannot read its data folder ({dir}): permission denied."),
        format!("The folder or a file in it is not owned by {}, the account the service runs as.", owner.split(':').next().unwrap_or(owner)),
        vec![cmd("Give it back to its owner", format!("sudo chown -R {owner} {dir}{mode}")), steps("Then Retry.")],
    )
}

fn pkg_lock(_: &Evidence, t: &str) -> Option<Diagnosis> {
    let pacman = has(t, &["unable to lock database", "could not lock database"]);
    let apt = has(t, &["Could not get lock", "Unable to acquire the dpkg frontend lock", "Waiting for cache lock", "dpkg was interrupted"]);
    let dnf = has(t, &["Waiting for process with pid", "Another app is currently holding the yum lock", "Failed to obtain the transaction lock", "System management is locked"]);
    if !(pacman || apt || dnf) {
        return None;
    }
    // "It is held by process 7 (perl)" → " (process 7, perl)"
    let holder = t.lines().find_map(|l| l.split("held by process ").nth(1)).map(|r| {
        let r = r.trim().trim_end_matches(['.', ')']);
        match r.split_once(" (") {
            Some((pid, name)) => format!(" (process {pid}, {name})"),
            None => format!(" (process {r})"),
        }
    });
    let mut fixes = vec![steps("Wait for the other install or update to finish (or close the software updater), then Retry.")];
    if pacman {
        fixes.push(cmd("Only if no pacman is running: remove the stale lock", "pgrep -x pacman || sudo rm /var/lib/pacman/db.lck"));
    }
    if t.contains("dpkg was interrupted") {
        fixes.push(cmd("Finish the interrupted install", "sudo dpkg --configure -a"));
    } else if apt {
        fixes.push(cmd("See what holds it", "sudo fuser -v /var/lib/dpkg/lock-frontend /var/lib/apt/lists/lock"));
    }
    diag(
        "pkg-lock",
        format!("Another package manager is running{}, so the package database is locked.", holder.unwrap_or_default()),
        "Only one program can install packages at a time.",
        fixes,
    )
}

fn pkg_network(_: &Evidence, t: &str) -> Option<Diagnosis> {
    let lines = [
        "failed retrieving file", "failed to synchronize all databases", "Could not resolve", "Temporary failure resolving", "Failed to fetch",
        "Cannot download repomd.xml", "Curl error", "Failed to download metadata", "Network is unreachable", "Could not connect to", "Connection timed out",
        "Cannot find a valid baseurl", "Unable to locate package", "target not found",
    ];
    let line = t.lines().find(|l| has(l, &lines))?;
    let host = line.split("Could not resolve host: ").nth(1).or_else(|| line.split("Could not resolve '").nth(1)).map(|h| h.trim_end_matches(['\'', '.']).trim().to_string());
    let (refresh, label) = if t.contains("pacman") || t.contains("core.db") || t.contains("target not found") {
        ("sudo pacman -Syy", "Refresh the package lists")
    } else if t.contains("repomd") || t.contains("dnf") || t.contains("baseurl") {
        ("sudo dnf makecache --refresh", "Refresh the package lists")
    } else {
        ("sudo apt-get update", "Refresh the package lists")
    };
    let missing = has(line, &["Unable to locate package", "target not found"]);
    diag(
        "pkg-network",
        match (&host, missing) {
            (_, true) => "The package lists do not have a package this step needs.".to_string(),
            (Some(h), _) => format!("The package mirror could not be reached ({h})."),
            _ => "The package mirror could not be reached.".to_string(),
        },
        if missing { "The lists are out of date or the mirror is missing a repository." } else { "Packages are downloaded from your distribution's mirrors, and the download failed." },
        vec![steps("Check the internet connection and proxy, or choose another mirror, then Retry."), cmd(label, refresh)],
    )
}

fn docker_group(ev: &Evidence, t: &str) -> Option<Diagnosis> {
    let lower = t.to_ascii_lowercase();
    let denied = lower.contains("permission denied while trying to connect to the docker") || ev.docker.as_ref().is_some_and(|d| d.permission_denied && !d.in_group);
    if !denied {
        return None;
    }
    diag(
        "docker-group",
        "Your account is not allowed to use Docker.",
        "Docker's socket is open to root and the docker group only, and this account is in neither.",
        vec![cmd("Add yourself to the docker group", "sudo usermod -aG docker \"$USER\""), steps("Then sign out and in again (or restart), and Retry.")],
    )
}

fn docker_down(ev: &Evidence, t: &str) -> Option<Diagnosis> {
    let lower = t.to_ascii_lowercase();
    let said = has_ci(&lower, &["Cannot connect to the Docker daemon", "failed to connect to the docker API", "Is the docker daemon running", "docker.sock: connect: no such file", "error during connect"]);
    let probed = ev.docker.as_ref().is_some_and(|d| !d.running);
    if !(said || probed) {
        return None;
    }
    #[cfg(windows)]
    let fixes = vec![steps("Start Docker Desktop (or the Docker service), wait until it says it is running, then Retry.")];
    #[cfg(not(windows))]
    let fixes = vec![action("Start Docker", FixAction::StartDocker), cmd("Or from a terminal", "sudo systemctl enable --now docker")];
    diag("docker-down", "Docker is not running.", "SENTIENT's containers need the Docker service, and nothing answers on its socket.", fixes)
}

fn is_clickhouse(ev: &Evidence, t: &str) -> bool {
    ev.units.iter().any(|u| u.unit.starts_with("clickhouse")) || t.to_ascii_lowercase().contains("clickhouse")
}

fn clickhouse_memory(ev: &Evidence, t: &str) -> Option<Diagnosis> {
    if !is_clickhouse(ev, t) {
        return None;
    }
    if !has(t, &["oom-kill", "OOM killer", "Memory limit (total) exceeded", "MEMORY_LIMIT_EXCEEDED", "Cannot allocate memory", "std::bad_alloc"]) {
        return None;
    }
    diag(
        "clickhouse-memory",
        "ClickHouse ran out of memory and was stopped.",
        "The system (or ClickHouse's own limit) ended it when it asked for more memory than there is.",
        vec![
            steps("Close other memory-hungry programs or add memory. A smaller memory setting for ClickHouse (Settings > ClickHouse) also helps."),
            cmd("See its memory messages", "journalctl -u clickhouse-server -n 100 --no-pager | grep -iE 'memory|oom'"),
        ],
    )
}

fn clickhouse_limits(ev: &Evidence, t: &str) -> Option<Diagnosis> {
    if !is_clickhouse(ev, t) {
        return None;
    }
    if !has(t, &["Too many open files", "errno: 24", "Cannot set max size of core file", "max user processes", "Resource temporarily unavailable"]) {
        return None;
    }
    diag(
        "clickhouse-limits",
        "ClickHouse hit the system's limit on open files or processes.",
        "ClickHouse keeps many files open at once and needs higher limits than the default.",
        vec![
            cmd(
                "Raise its limits",
                "sudo mkdir -p /etc/systemd/system/clickhouse-server.service.d && printf '[Service]\\nLimitNOFILE=500000\\nLimitNPROC=65535\\n' | sudo tee /etc/systemd/system/clickhouse-server.service.d/limits.conf && sudo systemctl daemon-reload",
            ),
            steps("Then Retry."),
        ],
    )
}

/// WSL turned on in this run: Windows finishes it only after a restart.
fn wsl_restart(_: &Evidence, t: &str) -> Option<Diagnosis> {
    if !has(t, &["A restart is required to finish enabling it", "restart is required to finish", "The requested operation requires a reboot", "ERROR_SUCCESS_REBOOT_REQUIRED"]) {
        return None;
    }
    diag(
        "wsl-restart",
        "Windows needs a restart to finish turning on WSL.",
        "ClickHouse runs in Docker inside WSL2 on Windows; WSL was turned on just now and becomes active after a restart.",
        vec![steps("Restart Windows. Setup carries on by itself after you sign in again (the install continues where it stopped); if it does not, open SENTIENT Setup and Retry.")],
    )
}

fn wsl_missing(_: &Evidence, t: &str) -> Option<Diagnosis> {
    if !has(t, &["Windows Subsystem for Linux is not installed", "WSL is not installed", "Windows Subsystem for Linux has no installed distributions", "wsl.exe' is not recognized", "0x8007019e", "WSL_E_WSL_OPTIONAL_COMPONENT_REQUIRED"]) {
        return None;
    }
    diag(
        "wsl-missing",
        "WSL (the Windows Subsystem for Linux) is not installed.",
        "ClickHouse runs in Docker inside WSL2 on Windows.",
        vec![cmd("Install it (as administrator), then restart Windows", "wsl --install --no-distribution"), steps("After the restart, Retry.")],
    )
}

fn wsl_stopped(_: &Evidence, t: &str) -> Option<Diagnosis> {
    let virt = has(t, &["0x80370102", "HCS_E_HYPERV_NOT_INSTALLED", "Virtual Machine Platform", "virtualization is not enabled"]);
    let stopped = has(t, &["WslRegisterDistribution failed", "The service cannot be started", "There is no distribution with the supplied name", "Wsl/Service", "WSL_E_", "The remote procedure call failed"]);
    if virt {
        return diag(
            "wsl-virtualization",
            "WSL2 cannot start: virtualization is off.",
            "WSL2 runs a small virtual machine, which needs virtualization in the firmware and Virtual Machine Platform in Windows.",
            vec![
                cmd("Turn on Virtual Machine Platform (as administrator)", "dism.exe /online /enable-feature /featurename:VirtualMachinePlatform /all /norestart"),
                steps("Enable virtualization (Intel VT-x / AMD SVM) in the BIOS if it is off, restart, then Retry."),
            ],
        );
    }
    if !stopped {
        return None;
    }
    diag(
        "wsl-stopped",
        "WSL is not running or its distribution is missing.",
        "ClickHouse's Docker runs inside WSL, which did not answer.",
        vec![action("Start WSL", FixAction::StartWsl), cmd("Or restart it (as administrator)", "wsl --shutdown; wsl --status")],
    )
}

/// The most telling line of a journal: the last error-like one.
fn telling(lines: &[String]) -> Option<String> {
    let skip = |l: &str| l.contains("Failed with result") || l.starts_with("Failed to start") || l.contains("Control process exited") || l.contains("Main process exited") || l.starts_with("Starting ");
    lines.iter().rev().find(|l| is_error_line(l) && !skip(l)).or_else(|| lines.iter().rev().find(|l| !skip(l) && !l.trim().is_empty())).map(|l| l.trim().to_string())
}

fn service_failed(ev: &Evidence, t: &str) -> Option<Diagnosis> {
    let unit = ev.units.first();
    let said = t.lines().find_map(|l| {
        l.strip_prefix("Job for ").and_then(|r| r.split(" failed").next()).map(str::to_string).or_else(|| l.contains("Failed to start").then(|| l.to_string()))
    });
    let script = matches!(ev.exit_code, Some(40) | Some(41)) || has(t, &["PostgreSQL stopped while starting", "did not answer within 4 minutes"]);
    if unit.is_none() && said.is_none() && !script {
        return None;
    }
    let name = unit.map(|u| u.unit.clone()).or_else(|| said.clone().filter(|s| s.ends_with(".service"))).unwrap_or_else(|| "A service".into());
    let pretty = name.trim_end_matches(".service").to_string();
    let why = unit.and_then(|u| telling(&u.journal)).or_else(|| telling(&ev.lines)).unwrap_or_else(|| "Its log says why (below).".into());
    let journal = if pretty == "A service" { "journalctl -xe --no-pager | tail -60".to_string() } else { format!("journalctl -u {pretty} -n 100 --no-pager") };
    diag(
        "service-failed",
        format!("{pretty} did not start."),
        why,
        vec![steps("Fix what its log reports, then Retry."), cmd("Its full log", journal)],
    )
}

fn unknown(ev: &Evidence) -> Diagnosis {
    let last = last_error_lines(ev);
    // The last error-like line the step printed; when it printed none (only
    // progress, "… is up to date"), the job's own error says what stopped it,
    // unless that is only a wrapper's "exited with N".
    let printed_error = last.iter().any(|l| is_error_line(l));
    let own = ev.error.lines().map(str::trim).find(|l| !l.is_empty()).filter(|_| ev.exit_code.is_none()).map(str::to_string);
    let cause = match own {
        Some(e) if !printed_error => e,
        own => last.last().cloned().filter(|l| !l.is_empty()).or(own).unwrap_or_else(|| "The step failed.".to_string()),
    };
    Diagnosis {
        id: "unknown".into(),
        cause: format!("The step stopped: {}", cause.trim_start_matches("!! ").trim_start_matches("E: ").trim_start_matches("error: ")),
        why: "This is not a cause the Platform Manager knows yet. The lines below are what the step printed last.".into(),
        fixes: Vec::new(),
        last_lines: last,
        details: String::new(),
    }
}

fn is_error_line(l: &str) -> bool {
    let lower = l.to_ascii_lowercase();
    l.starts_with("!! ")
        || l.starts_with("E: ")
        || has_ci(&lower, &["error", "fatal", "failed", "denied", "cannot", "could not", "unable to", "no such", "not found", "refused", "panic", "exception", "timed out", "already in use", "old version", "incompatible", "invalid", "killed", "too many", "no space"])
}

/// The real last error lines: what the step printed (journals included),
/// not the wrapper's "exited with N".
pub fn last_error_lines(ev: &Evidence) -> Vec<String> {
    let ours = |l: &str| l.starts_with("Waiting for the administrator password") || l.starts_with(">> ") || l.trim().is_empty();
    let mut all: Vec<String> = ev.lines.iter().filter(|l| !ours(l)).cloned().collect();
    for u in &ev.units {
        all.extend(u.journal.iter().cloned());
    }
    let errs: Vec<String> = all.iter().filter(|l| is_error_line(l)).cloned().collect();
    let pick = if errs.is_empty() { all } else { errs };
    let mut v = tail(&pick, 8);
    // Each line once, at its last place.
    let mut seen = std::collections::HashSet::new();
    v.reverse();
    v.retain(|l| seen.insert(l.clone()));
    v.reverse();
    if v.is_empty() {
        v.push(ev.error.lines().next().unwrap_or_default().to_string());
    }
    v
}

/// "1.2 GB", "450 MB", "12 KB"
pub fn human(bytes: u64) -> String {
    crate::size::label(bytes)
}

/// Everything, for "Copy details".
fn details(ev: &Evidence, d: &Diagnosis) -> String {
    let mut s = format!("Cause: {}\nWhy: {}\n", d.cause, d.why);
    for f in &d.fixes {
        match f {
            Fix::Action { label, .. } => s.push_str(&format!("Fix: {label}\n")),
            Fix::Command { label, command } => s.push_str(&format!("Fix: {label}: {command}\n")),
            Fix::Steps { text } => s.push_str(&format!("Fix: {text}\n")),
        }
    }
    s.push_str(&format!("\nError: {}\n", ev.error));
    if let Some(c) = ev.exit_code {
        s.push_str(&format!("Exit code: {c}\n"));
    }
    for u in &ev.units {
        let j = if u.journal.is_empty() { "(its journal needs administrator rights to read)".to_string() } else { u.journal.join("\n") };
        s.push_str(&format!("\nFailed: {} (journal):\n{j}\n", u.unit));
    }
    if !ev.disks.is_empty() {
        s.push_str("\nFree space:\n");
        for disk in &ev.disks {
            s.push_str(&format!("  {} {} free ({}% used)\n", disk.mount, human(disk.avail), disk.used_pct));
        }
    }
    for p in &ev.ports {
        s.push_str(&format!("Port {}: {}\n", p.port, p.owner));
    }
    if let Some(dk) = &ev.docker {
        s.push_str(&format!("Docker: running={} permission_denied={} in_group={} {}\n", dk.running, dk.permission_denied, dk.in_group, dk.text));
    }
    if let Some(o) = &ev.old_pg {
        s.push_str(&format!("PostgreSQL data: {}\n", o.describe()));
    }
    for w in &ev.windows {
        s.push_str(&format!("{w}\n"));
    }
    s.push_str("\nLast output lines:\n");
    s.push_str(&ev.lines.join("\n"));
    s
}

// ---------------------------------------------------------------------------
// One-click fixes
// ---------------------------------------------------------------------------

fn log(sink: &ProgressFn, line: impl Into<String>) {
    sink(Progress::Log { line: line.into() });
}

/// Run a one-click fix on this machine. Returns what was done. The page
/// actions ([`FixAction::is_page_action`]) are the page's to apply.
pub fn apply(action: &FixAction, sink: &ProgressFn) -> Result<String, String> {
    match action {
        #[cfg(unix)]
        FixAction::UpgradePgData { family } => {
            sink(Progress::Step { name: "Upgrading the PostgreSQL data".into() });
            crate::exec::run_privileged(sink, &old_pg::fix_script(*family, old_pg::OldPgChoice::Upgrade))?;
            Ok("The PostgreSQL data is upgraded; the old data is kept. Press Retry to continue.".into())
        }
        #[cfg(unix)]
        FixAction::KeepPgDataAside { family } => {
            sink(Progress::Step { name: "Keeping the old PostgreSQL data aside".into() });
            crate::exec::run_privileged(sink, &old_pg::fix_script(*family, old_pg::OldPgChoice::KeepAside))?;
            Ok("The old PostgreSQL data is kept aside (nothing was deleted). Press Retry to start fresh.".into())
        }
        #[cfg(unix)]
        FixAction::StartDocker => {
            sink(Progress::Step { name: "Starting Docker".into() });
            crate::exec::run_privileged(sink, START_DOCKER)?;
            Ok("Docker is running. Press Retry.".into())
        }
        #[cfg(unix)]
        FixAction::StopService { unit } => {
            sink(Progress::Step { name: format!("Stopping {unit}") });
            crate::exec::run_privileged(sink, &stop_service_script(safe_name(unit)?))?;
            Ok(format!("{unit} is stopped and will not start at boot."))
        }
        #[cfg(unix)]
        FixAction::RemovePostgres { unit, package, data_dir } => {
            sink(Progress::Step { name: "Removing PostgreSQL".into() });
            if let Some(p) = package {
                safe_name(p)?;
            }
            if !data_dir.starts_with('/') || data_dir.contains("..") || data_dir.trim_end_matches('/').matches('/').count() < 2 {
                return Err(format!("{data_dir} is not a PostgreSQL data folder."));
            }
            crate::exec::run_privileged(sink, &remove_postgres_script(package_manager_here(), safe_name(unit)?, package.as_deref(), data_dir))?;
            Ok(format!("PostgreSQL is removed. Its data was moved to {data_dir}.old-<date> (nothing was deleted)."))
        }
        #[cfg(unix)]
        FixAction::StopContainer { name } => {
            sink(Progress::Step { name: format!("Stopping the container {name}") });
            let name = safe_name(name)?;
            // As this user first (the docker group); as administrator when
            // the socket is closed to it.
            let own = std::process::Command::new("docker").args(["stop", name]).output();
            match own {
                Ok(o) if o.status.success() => {}
                _ => crate::exec::run_privileged(sink, &format!("docker stop {}", quote(name)))?,
            }
            Ok(format!("The container {name} is stopped (not removed: Docker can start it again)."))
        }
        #[cfg(windows)]
        FixAction::StartWsl => {
            sink(Progress::Step { name: "Starting WSL".into() });
            let o = crate::sys::output("wsl.exe", &["--exec", "true"]).ok_or("could not run wsl.exe")?;
            if o.0 { Ok("WSL is running. Press Retry.".into()) } else { Err(crate::sys::decode(&o.2).trim().to_string()) }
        }
        FixAction::ClearDownloadCache { dir, .. } => {
            sink(Progress::Step { name: "Clearing the download cache".into() });
            let before = dir_size(dir);
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    let r = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
                    if let Err(err) = r {
                        log(sink, format!("could not remove {}: {err}", p.display()));
                    }
                }
            }
            Ok(format!("{} freed. Press Retry.", human(before.saturating_sub(dir_size(dir)))))
        }
        other => Err(format!("{other:?} is applied by the page that shows it, or not on this system.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<String> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/diagnose").join(name);
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())).lines().map(str::to_string).collect()
    }

    fn of(error: &str, file: &str) -> Diagnosis {
        diagnose(&Evidence::from_text(error, &fixture(file)))
    }

    fn actions(d: &Diagnosis) -> Vec<FixAction> {
        d.fixes.iter().filter_map(|f| if let Fix::Action { action, .. } = f { Some(action.clone()) } else { None }).collect()
    }

    #[test]
    fn system_check_actions_map_to_their_commands() {
        let stop = FixAction::StopService { unit: "postgresql.service".into() }.command().unwrap();
        assert!(stop.contains("systemctl disable --now 'postgresql.service' 2>/dev/null || systemctl stop 'postgresql.service'"), "{stop}");
        assert_eq!(FixAction::StopContainer { name: "sentient-postgres".into() }.command().as_deref(), Some("docker stop 'sentient-postgres'"));
        let rm = remove_postgres_script(Some("pacman"), "postgresql.service", Some("postgresql"), "/var/lib/postgres/data");
        assert!(rm.contains("pacman -R --noconfirm 'postgresql'"), "{rm}");
        assert!(rm.contains("DATA='/var/lib/postgres/data'"));
        assert!(rm.contains("ASIDE=\"$DATA.old-$(date +%Y%m%d-%H%M%S)\""));
        assert!(rm.contains("mv \"$DATA\" \"$ASIDE\""));
        assert!(rm.contains("systemctl daemon-reload"));
        // The data is moved, never deleted; a removed package needs no package step.
        assert!(!rm.contains("rm -") && !rm.contains("\nrm ") && !rm.contains(" rm "), "{rm}");
        let left = remove_postgres_script(Some("pacman"), "postgresql.service", None, "/var/lib/postgres/data");
        assert!(!left.contains("pacman"));
        assert!(package_remove("apt-get", "postgresql-common").starts_with("DEBIAN_FRONTEND=noninteractive apt-get remove -y"));
        assert!(!package_remove("apt-get", "x").contains("purge"));
        // Page actions run nothing of their own.
        assert_eq!(FixAction::UsePort { port: 5432, instead: 5433 }.command(), None);
        assert_eq!(FixAction::Recheck.command(), None);
        assert!(FixAction::Recheck.is_page_action());
        // A name is a name.
        assert!(safe_name("postgresql@16-main.service").is_ok());
        assert!(safe_name("a;rm -rf /").is_err());
        assert!(safe_name("--force").is_err());
    }

    #[test]
    fn exit_codes_and_ports_are_read() {
        assert_eq!(exit_code_of("pkexec exited with 2"), Some(2));
        assert_eq!(exit_code_of("bash: exit status: 42"), Some(42));
        assert_eq!(exit_code_of("no code"), None);
        assert_eq!(ports_named("Is another postmaster already running on port 5432? and :8080 and 18.7"), vec![5432, 8080]);
        assert_eq!(ports_named("already in use: 8080 (HTTP), 5432 (PostgreSQL)"), vec![8080, 5432]);
    }

    #[test]
    fn the_owners_case_old_major_from_the_journal() {
        // What he saw: the wrapper error, the psql socket error, and the
        // journal of the failed unit.
        let mut ev = Evidence::from_text(
            "pkexec exited with 2",
            &["psql: error: connection to server on socket \"/run/postgresql/.s.PGSQL.5432\" failed: No such file or directory".into()],
        );
        ev.units.push(UnitLog { unit: "postgresql.service".into(), journal: fixture("arch-old-major-journal.txt") });
        let d = diagnose(&ev);
        assert_eq!(d.id, "pg-old-major", "{d:?}");
        assert!(actions(&d).contains(&FixAction::UpgradePgData { family: Family::Arch }));
        assert!(actions(&d).contains(&FixAction::KeepPgDataAside { family: Family::Arch }));
        assert!(d.last_lines.iter().any(|l| l.contains("An old version of the database format")), "{:?}", d.last_lines);
    }

    #[test]
    fn old_major_from_the_exit_42_line_names_both_versions() {
        let d = diagnose(&Evidence::from_text(
            "bash exited with 42",
            &["!! /var/lib/postgres/data holds a PostgreSQL 17 database; PostgreSQL 18 is installed.".into()],
        ));
        assert_eq!(d.id, "pg-old-major");
        assert_eq!(d.cause, "The PostgreSQL data folder is from PostgreSQL 17; PostgreSQL 18 is installed.");
        assert!(d.fixes.iter().any(|f| matches!(f, Fix::Action { label, .. } if label == "Upgrade it to PostgreSQL 18")));
    }

    #[test]
    fn an_upgrade_that_cannot_run_offers_keep_aside_only() {
        let d = diagnose(&Evidence::from_text(
            "bash exited with 44",
            &["!! The PostgreSQL 15 data cannot be upgraded here: postgresql-old-upgrade provides PostgreSQL 17 only.".into()],
        ));
        assert_eq!(d.id, "pg-old-major");
        assert_eq!(actions(&d), vec![FixAction::KeepPgDataAside { family: Family::Arch }]);
    }

    #[test]
    fn port_in_use_from_the_journal() {
        let mut ev = Evidence::from_text("pkexec exited with 43", &[]);
        ev.units.push(UnitLog { unit: "postgresql.service".into(), journal: fixture("arch-port-in-use.txt") });
        ev.ports.push(PortUse { port: 5432, owner: "postgres (pid 953)".into(), free: Some(5433) });
        let d = diagnose(&ev);
        assert_eq!(d.id, "port-in-use");
        assert_eq!(d.cause, "Port 5432 is already used by postgres (pid 953).");
        assert!(actions(&d).contains(&FixAction::UsePort { port: 5432, instead: 5433 }));
        // Text only, no owner: still the port.
        let d = of("x exited with 1", "arch-port-in-use.txt");
        assert_eq!((d.id.as_str(), d.cause.as_str()), ("port-in-use", "Port 5432 is already used by another program."));
    }

    #[test]
    fn disk_full_offers_the_cache() {
        let mut ev = Evidence::from_text("dd exited with 1", &fixture("linux-disk-full.txt"));
        let lines = fixture("linux-disk-full.txt");
        let df = lines.iter().skip_while(|l| *l != "===DF").skip(1).cloned().collect::<Vec<_>>().join("\n");
        ev.disks = parse_df(&df);
        assert_eq!(ev.disks, vec![Disk { mount: "/mnt/tiny".into(), avail: 0, used_pct: 100 }]);
        ev.cache = Some((PathBuf::from("/home/u/.cache/sentient-platform-manager/downloads"), 1_400_000_000));
        let d = diagnose(&ev);
        assert_eq!(d.id, "disk-full");
        assert_eq!(d.cause, "The disk holding /mnt/tiny is full (0 B free).");
        assert!(matches!(&actions(&d)[0], FixAction::ClearDownloadCache { bytes: 1_400_000_000, .. }));
    }

    #[test]
    fn permission_denied_on_the_data_folder() {
        let d = of("x exited with 1", "arch-permission-denied.txt");
        assert_eq!(d.id, "data-permission");
        assert!(d.cause.contains("/var/lib/postgres/data"), "{}", d.cause);
        assert!(matches!(&d.fixes[0], Fix::Command { command, .. } if command == "sudo chown -R postgres:postgres /var/lib/postgres/data && sudo chmod 700 /var/lib/postgres/data"));
    }

    #[test]
    fn auth_cancelled_three_ways() {
        let d = diagnose(&Evidence::from_text("Administrator authorisation was refused or cancelled.", &[]));
        assert_eq!(d.id, "auth-cancelled");
        assert_eq!(actions(&d), vec![FixAction::AskAgain]);
        // exit 2 and nothing printed but our own hint.
        let d = diagnose(&Evidence::from_text("pkexec exited with 2", &["Waiting for the administrator password: if nothing moves, look for the password dialog (it may be behind this window).".into()]));
        assert_eq!(d.id, "auth-cancelled");
        // exit 2 with real output is not a cancel.
        let d = diagnose(&Evidence::from_text("pkexec exited with 2", &["something else broke".into()]));
        assert_ne!(d.id, "auth-cancelled");
        let d = of("pkexec exited with 127", "pkexec-no-agent.txt");
        assert_eq!(d.id, "auth-no-agent");
    }

    #[test]
    fn package_manager_lock_and_mirror() {
        assert_eq!(of("x exited with 1", "arch-pacman-lock.txt").id, "pkg-lock");
        let d = of("x exited with 100", "debian-apt-lock.txt");
        assert_eq!(d.id, "pkg-lock");
        assert!(d.cause.contains("(process 7, perl)"), "{}", d.cause);
        let d = of("x exited with 1", "arch-pacman-mirror.txt");
        assert_eq!(d.id, "pkg-network");
        assert_eq!(d.cause, "The package mirror could not be reached (mirror.nonexistent.invalid).");
        assert!(d.fixes.iter().any(|f| matches!(f, Fix::Command { command, .. } if command == "sudo pacman -Syy")));
        let d = of("x exited with 100", "debian-apt-mirror.txt");
        assert_eq!(d.id, "pkg-network");
        assert!(d.fixes.iter().any(|f| matches!(f, Fix::Command { command, .. } if command == "sudo apt-get update")));
    }

    #[test]
    fn docker_down_and_group() {
        let d = of("docker exited with 1", "docker-not-running.txt");
        assert_eq!(d.id, "docker-down");
        #[cfg(unix)]
        assert_eq!(actions(&d), vec![FixAction::StartDocker]);
        assert_eq!(of("docker exited with 1", "docker-not-in-group.txt").id, "docker-group");
    }

    #[test]
    fn clickhouse_memory_and_limits() {
        let mut ev = Evidence::from_text("x exited with 41", &[]);
        ev.units.push(UnitLog { unit: "clickhouse-server.service".into(), journal: fixture("clickhouse-oom.txt") });
        assert_eq!(diagnose(&ev).id, "clickhouse-memory");
        assert_eq!(of("x exited with 1", "clickhouse-open-files.txt").id, "clickhouse-limits");
    }

    #[test]
    fn wsl_needing_a_restart_is_named() {
        let ev = Evidence::from_text("WSL isn't active yet. A restart is required to finish enabling it.", &[]);
        assert_eq!(diagnose(&ev).id, "wsl-restart");
    }

    #[test]
    fn wsl_missing_and_stopped() {
        assert_eq!(of("wsl exited with 1", "wsl-not-installed.txt").id, "wsl-missing");
        assert_eq!(of("wsl exited with 1", "wsl-virtualization.txt").id, "wsl-virtualization");
    }

    #[test]
    fn not_admin_is_named() {
        let ev = Evidence::from_text("The account spmtest on 192.168.122.161 is not an administrator. The Platform Manager needs an administrator account there: add it to the Administrators group, or use an administrator account for this instance.", &[]);
        assert_eq!(diagnose(&ev).id, "not-admin");
        let ev = Evidence::from_text("job failed", &["Run this from an elevated prompt (Run as administrator).".to_string()]);
        assert_eq!(diagnose(&ev).id, "not-admin");
    }

    #[test]
    fn closed_prompt_from_the_helper_is_cancelled() {
        let ev = Evidence::from_text("The password prompt was closed.", &[]);
        assert_eq!(diagnose(&ev).id, "auth-cancelled");
    }

    #[test]
    fn ssh_host_key_changed() {
        let d = of("ssh exited with 255", "ssh-host-key-changed.txt");
        assert_eq!(d.id, "ssh-host-key");
        assert!(d.fixes.iter().any(|f| matches!(f, Fix::Command { command, .. } if command == "ssh-keygen -R '127.0.0.1'")));
    }

    #[test]
    fn an_old_agent_that_would_not_move_is_named() {
        let ev = Evidence::from_text("The agent could not be put in place. The agent there is unchanged.\n\nDetails: Move-Item : Cannot create a file when that file already exists.", &[]);
        let d = diagnose(&ev);
        assert_eq!(d.id, "win-replace-file");
        assert_eq!(d.cause, "The old agent on the Windows server could not be replaced.");
    }

    #[test]
    fn a_dropping_connection_is_named() {
        let ev = Evidence::from_text("ssh exited with 255", &["client_loop: send disconnect: Broken pipe".into(), "Connection to 10.10.0.5 closed.".into()]);
        let d = diagnose(&ev);
        assert_eq!(d.id, "ssh-network");
        assert_eq!(d.cause, "The connection to 10.10.0.5 keeps dropping.");
        assert!(d.fixes.iter().any(|f| matches!(f, Fix::Command { command, .. } if command == "ping -c 20 10.10.0.5")));
    }

    #[test]
    fn a_silent_server_is_named() {
        let ev = Evidence::from_text("ssh: connect to host 10.10.0.7 port 22: Connection timed out", &[]);
        let d = diagnose(&ev);
        assert_eq!(d.id, "ssh-network");
        assert_eq!(d.cause, "10.10.0.7 does not answer on the network.");
    }

    #[test]
    fn a_failed_service_says_its_telling_line() {
        let mut ev = Evidence::from_text("x exited with 41", &[]);
        ev.units.push(UnitLog {
            unit: "sentient.service".into(),
            journal: vec!["Starting SENTIENT...".into(), "thread 'main' panicked at src/main.rs:12: bad config value".into(), "sentient.service: Main process exited, code=exited, status=101/n/a".into(), "sentient.service: Failed with result 'exit-code'.".into()],
        });
        let d = diagnose(&ev);
        assert_eq!(d.id, "service-failed");
        assert_eq!(d.cause, "sentient did not start.");
        assert_eq!(d.why, "thread 'main' panicked at src/main.rs:12: bad config value");
    }

    #[test]
    fn unknown_shows_the_real_last_error_not_the_wrapper() {
        let d = diagnose(&Evidence::from_text("pkexec exited with 3", &["copying files".into(), "tar: sentient.tar: Cannot open: Input/output error".into(), "done?".into()]));
        assert_eq!(d.id, "unknown");
        assert_eq!(d.cause, "The step stopped: tar: sentient.tar: Cannot open: Input/output error");
        assert!(d.details.contains("pkexec exited with 3"));
    }

    #[test]
    fn unknown_names_the_jobs_error_when_the_step_printed_none() {
        let d = diagnose(&Evidence::from_text(
            "System has not been booted with systemd as init system (PID 1). Can't operate.",
            &[">> Connect to the server".into(), "The Platform Manager on the server is up to date.".into()],
        ));
        assert_eq!(d.id, "unknown");
        assert_eq!(d.cause, "The step stopped: System has not been booted with systemd as init system (PID 1). Can't operate.");
    }

    #[test]
    fn a_diagnosis_crosses_as_one_line() {
        let d = of("x exited with 1", "arch-pacman-lock.txt");
        let line = d.encode();
        assert!(line.starts_with(DIAG_PREFIX) && !line.contains('\n'));
        assert_eq!(Diagnosis::decode(&line), Some(d));
    }
}
