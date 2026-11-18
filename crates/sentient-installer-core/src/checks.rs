//! Read-only preflight checks. Nothing here changes the system — it only reports
//! whether each prerequisite for installing SENTIENT is satisfied, will be set
//! up by the installer, or is a blocker the user must fix.
//!
//! Two sets, because the two deployment modes need different things:
//! [`run_all`] for the WSL2 + Docker mode, [`run_native`] for the native mode
//! (see [`crate::native`]), which needs neither but does need free ports and no
//! competing PostgreSQL.

use serde::Serialize;

use crate::diagnose::{Fix, FixAction};
use crate::units::{self, PgService};

#[cfg(windows)]
use crate::sys::{decode, output};

#[derive(Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Satisfied.
    Pass,
    /// Not satisfied, but the installer will set it up (WSL, distro, Docker…).
    Setup,
    /// A blocker the user must resolve externally (BIOS, Windows version…).
    Fail,
    /// Needs attention, but does not stop the install: a later choice can
    /// resolve it (a busy default port, when another one can be chosen).
    Warn,
    /// Couldn't determine (e.g. running the dev build off-Windows).
    Unknown,
}

#[derive(Serialize)]
pub struct Check {
    pub id: String,
    pub label: String,
    pub status: Status,
    pub detail: String,
    /// The pill's words when the status's own would be wrong ("Already
    /// installed" rather than "Will be installed").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// What the operator can do about it, as buttons (an action) or a
    /// command to copy. The System check adds "Check again" to every row
    /// that is not ready.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fixes: Vec<Fix>,
}

fn check(id: &str, label: &str, status: Status, detail: impl Into<String>) -> Check {
    Check { id: id.into(), label: label.into(), status, detail: detail.into(), state: None, fixes: Vec::new() }
}

impl Check {
    fn state(mut self, state: &str) -> Self {
        self.state = Some(state.into());
        self
    }

    fn fixes(mut self, fixes: Vec<Fix>) -> Self {
        self.fixes = fixes;
        self
    }
}

fn act(label: impl Into<String>, action: FixAction) -> Fix {
    Fix::Action { label: label.into(), action }
}

fn cmd(label: &str, command: impl Into<String>) -> Fix {
    Fix::Command { label: label.into(), command: command.into() }
}

/// Run every preflight check, in display order.
pub fn run_all() -> Vec<Check> {
    vec![
        windows_version(),
        administrator(),
        virtualization(),
        wsl_installed(),
        distro_present(),
        docker_ready(),
        disk_space(),
        internet(),
    ]
}

/// Preflight for the **native** mode. Deliberately a different list: WSL,
/// virtualization and Docker are irrelevant when nothing is containerised, and
/// asking about them would only confuse the operator.
///
/// `offline` selects whether reachability of the download hosts matters.
/// Preflight for a (host OS, deployment mode) pair. The wizard asks for the
/// host OS first and only offers modes that exist there; the Windows lists
/// are unchanged, the Linux ones never mention Windows.
///
/// `mode` is one of "windows-docker", "windows-native", "linux-docker",
/// "linux-native".
///
/// `ports`: the TCP ports the chosen configuration will bind, so the check
/// reflects what the operator actually picked; `None` means the defaults.
pub fn run_for(mode: &str, offline: bool, ports: Option<&[(u16, String)]>) -> Vec<Check> {
    let mut v = match mode {
        "windows-native" => run_native(offline),
        "linux-docker" => run_linux_docker(offline),
        "linux-native" => run_linux_native(offline),
        _ => run_all(),
    };
    if let Some(p) = ports {
        if let Some(slot) = v.iter_mut().find(|c| c.id == "ports") {
            *slot = attention(ports_free_of(p));
        }
    }
    share_fixes(&mut v);
    v
}

/// A busy port on the System check: needs attention, but a port can still
/// be chosen later, so it does not block.
fn attention(mut c: Check) -> Check {
    if c.status == Status::Fail {
        c.status = Status::Warn;
        c.label = "Ports".into();
    }
    c
}

/// Docker on a Linux host: the stack runs from SENTIENT's own compose files
/// with `docker compose` invoked directly — no WSL, no distro import.
pub fn run_linux_docker(offline: bool) -> Vec<Check> {
    let mut v = vec![docker_cli(), docker_daemon(), compose_plugin(), disk_space(), ports_free()];
    if !offline {
        v.push(internet());
    }
    v
}

/// Native Linux: systemd units for PostgreSQL (and ClickHouse when chosen)
/// plus the SENTIENT server, installed as root.
pub fn run_linux_native(offline: bool) -> Vec<Check> {
    let mut v = vec![systemd_present(), root_privileges(), package_manager(), disk_space(), ports_free()];
    v.push(existing_postgres_linux());
    if !offline {
        v.push(download_hosts_linux());
    }
    v
}

/// An action offered on the PostgreSQL row is not offered again on the
/// ports row (the same server holding 5432): one place to act, and the
/// ports row points there.
pub fn share_fixes(v: &mut [Check]) {
    let Some(pg) = v.iter().find(|c| c.id == "existing-pg") else { return };
    let taken: Vec<FixAction> = pg.fixes.iter().filter_map(|f| if let Fix::Action { action, .. } = f { Some(action.clone()) } else { None }).collect();
    if taken.is_empty() {
        return;
    }
    if let Some(ports) = v.iter_mut().find(|c| c.id == "ports") {
        let before = ports.fixes.len();
        ports.fixes.retain(|f| !matches!(f, Fix::Action { action, .. } if taken.contains(action)));
        if ports.fixes.len() < before {
            let port = taken.iter().find_map(|a| if let FixAction::UsePort { port, .. } = a { Some(*port) } else { None }).unwrap_or(5432);
            ports.detail.push_str(&format!(" For port {port}, see Existing PostgreSQL."));
        }
    }
}

/// The Linux native engine fetches PostgreSQL from the distribution's own
/// mirrors (not probed — every distribution has its own) and ClickHouse from
/// packages.clickhouse.com.
fn download_hosts_linux() -> Check {
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;
    let ok = "packages.clickhouse.com:443"
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
        .map(|addr| TcpStream::connect_timeout(&addr, Duration::from_secs(5)).is_ok())
        .unwrap_or(false);
    if ok {
        check("downloads", "Download servers", Status::Pass, "packages.clickhouse.com is reachable (PostgreSQL comes from your distribution's mirrors).")
    } else {
        check("downloads", "Download servers", Status::Setup,
              "Can't reach packages.clickhouse.com. Fine for “PostgreSQL only”; the ClickHouse flavour needs it (or an offline bundle).")
    }
}

#[cfg(unix)]
fn sh(cmd: &str) -> Option<(bool, String)> {
    let out = crate::sys::host_command("sh").args(["-c", cmd]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string()
        + &String::from_utf8_lossy(&out.stderr);
    Some((out.status.success(), text.trim().to_string()))
}
#[cfg(not(unix))]
fn sh(_cmd: &str) -> Option<(bool, String)> {
    None
}

fn docker_cli() -> Check {
    match sh("docker --version") {
        Some((true, v)) => check("docker-cli", "Docker", Status::Pass, v.lines().next().unwrap_or("present").to_string()),
        _ => check("docker-cli", "Docker", Status::Setup,
            "Docker is not installed. Setup installs Docker Engine from your distribution's packages."),
    }
}

/// Reachability doubles as the permission check: a user outside the `docker`
/// group gets "permission denied" here. Neither case blocks — the Docker
/// setup step starts the daemon and grants this user access (through
/// `sg docker` until the next login), so both are "will set up" items.
fn docker_daemon() -> Check {
    match sh("docker info --format '{{.ServerVersion}}' 2>&1") {
        Some((true, v)) => check("docker-daemon", "Docker daemon", Status::Pass, format!("Engine {} is running and reachable.", v)),
        Some((false, e)) if e.contains("permission denied") => check("docker-daemon", "Docker daemon", Status::Setup,
            "Running, but this user isn't in the `docker` group yet. The installer will add you (asks for your password).")
            .state("Will be set up")
            .fixes(vec![cmd("Add yourself to the docker group now", "sudo usermod -aG docker \"$USER\"")]),
        Some((false, _)) if matches!(sh("command -v docker"), Some((true, _))) => check("docker-daemon", "Docker daemon", Status::Setup,
            "Docker is installed but not running. The installer will enable and start it.")
            .state("Will be started")
            .fixes(vec![act("Start Docker", FixAction::StartDocker)]),
        Some((false, _)) => check("docker-daemon", "Docker daemon", Status::Setup,
            "Not set up yet. The installer will install Docker from your distribution."),
        None => check("docker-daemon", "Docker daemon", Status::Unknown, "Could not query Docker."),
    }
}

fn compose_plugin() -> Check {
    match sh("docker compose version --short") {
        Some((true, v)) => check("compose", "Docker Compose", Status::Pass, format!("Compose {} (plugin).", v)),
        _ => check("compose", "Docker Compose", Status::Setup,
            "The `docker compose` plugin is missing. Setup installs it with Docker (docker-compose-plugin on Debian/Ubuntu, docker-compose on Arch)."),
    }
}

fn systemd_present() -> Check {
    match sh("systemctl --version") {
        Some((true, v)) => check("systemd", "systemd", Status::Pass, v.lines().next().unwrap_or("present").to_string()),
        _ => check("systemd", "systemd", Status::Fail, "The native mode installs systemd services and this system has no systemd."),
    }
}

/// The native install writes under /opt, /etc and /var/lib and registers
/// services, so it needs root. SPM itself runs as the desktop user; it asks
/// for elevation through pkexec at install time, which must exist.
fn root_privileges() -> Check {
    match sh("id -u") {
        Some((true, uid)) if uid.trim() == "0" => check("root", "Privileges", Status::Pass, "Running as root."),
        _ => match sh("command -v pkexec || command -v sudo") {
            Some((true, tool)) => check("root", "Privileges", Status::Pass,
                format!("Will ask for the administrator password via {} when installing.", tool.rsplit('/').next().unwrap_or("pkexec"))),
            _ => check("root", "Privileges", Status::Fail, "Neither pkexec nor sudo is available to elevate for the install."),
        },
    }
}

/// Which family of package manager provides PostgreSQL here. The engine
/// uses the distribution's PostgreSQL packages rather than shipping binaries
/// — there is no vendor binary tarball for Linux the way there is for
/// Windows, and the distro's build is what the rest of the system expects.
fn package_manager() -> Check {
    for (bin, label, _family) in [("apt-get", "apt (Debian/Ubuntu)", "apt"), ("pacman", "pacman (Arch)", "pacman"), ("dnf", "dnf (Fedora/RHEL)", "dnf"), ("zypper", "zypper (openSUSE)", "zypper")] {
        if matches!(sh(&format!("command -v {bin}")), Some((true, _))) {
            return check("pkg", "Package manager", Status::Pass, format!("{label}. PostgreSQL will come from here."));
        }
    }
    check("pkg", "Package manager", Status::Fail, "No supported package manager found (apt, pacman, dnf, zypper).")
}

fn existing_postgres_linux() -> Check {
    let found = units::classify_postgres(&units::postgres_units());
    let package = match &found {
        PgService::Installed(u) => units::package_owning(&u.fragment_path),
        _ => None,
    };
    existing_pg_check(&found, package, pg_data_dir(), next_free_port(5433))
}

/// Where this distribution's PostgreSQL keeps its data.
pub fn pg_data_dir() -> &'static str {
    crate::old_pg::Family::here().map(|f| f.data_dir()).unwrap_or("/var/lib/pgsql/data")
}

/// The System check's PostgreSQL row. Only an installed (loaded) unit is
/// an existing PostgreSQL; a unit whose package was removed is nothing,
/// unless its server still runs (then it holds the port and is said so).
/// `instead`: the free port "Use another port" gives SENTIENT.
pub fn existing_pg_check(found: &PgService, package: Option<String>, data_dir: &str, instead: Option<u16>) -> Check {
    let use_port = |fixes: &mut Vec<Fix>| {
        if let Some(p) = instead {
            fixes.push(act(format!("Use another port ({p})"), FixAction::UsePort { port: 5432, instead: p }));
        }
    };
    match found {
        PgService::None => check("existing-pg", "Existing PostgreSQL", Status::Pass, "No PostgreSQL service is installed."),
        PgService::Installed(u) => {
            let pkg = package.as_ref().map(|p| format!(" from the package {p}")).unwrap_or_default();
            let state = if u.running() && u.main_pid > 0 {
                format!("running (pid {})", u.main_pid)
            } else if u.running() {
                "running".to_string()
            } else {
                "stopped".to_string()
            };
            let mut fixes = vec![
                act("Stop and disable it", FixAction::StopService { unit: u.id.clone() }),
                act("Remove it", FixAction::RemovePostgres { unit: u.id.clone(), package, data_dir: data_dir.into() }),
            ];
            use_port(&mut fixes);
            check("existing-pg", "Existing PostgreSQL", Status::Warn, format!("PostgreSQL is already installed as {}{pkg}, {state}.", u.id))
                .state("Already installed")
                .fixes(fixes)
        }
        PgService::LeftoverRunning(u) => {
            let pid = if u.main_pid > 0 { format!(" (pid {})", u.main_pid) } else { String::new() };
            let mut fixes = vec![
                act("Stop it", FixAction::StopService { unit: u.id.clone() }),
                act("Remove it", FixAction::RemovePostgres { unit: u.id.clone(), package: None, data_dir: data_dir.into() }),
            ];
            use_port(&mut fixes);
            check(
                "existing-pg",
                "Existing PostgreSQL",
                Status::Warn,
                format!("{} is still running{pid} although its package was removed.", u.id),
            )
            .state("Still running")
            .fixes(fixes)
        }
    }
}

pub fn run_native(offline: bool) -> Vec<Check> {
    let mut v = vec![windows_version(), administrator(), disk_space(), ports_free()];
    v.push(existing_postgres());
    if !offline {
        v.push(download_hosts());
    }
    v
}

/// The default ports. A busy one is a warning here, not a blocker: the ports
/// are chosen later (Network ports, Database settings), and the install is
/// refused there while a chosen port is in use.
fn ports_free() -> Check {
    attention(ports_free_of(&[
        (8080, "HTTP".into()), (1883, "MQTT".into()), (5683, "CoAP".into()), (5432, "PostgreSQL".into()),
    ]))
}

/// Are these TCP ports free to bind? A busy port is a real blocker for the
/// install, but the operator can pick different ones on the Configure step,
/// and the Install step re-checks the ones actually chosen.
pub fn ports_free_of(wanted: &[(u16, String)]) -> Check {
    let busy: Vec<(u16, String, Option<PortOwner>, Option<u16>)> = wanted
        .iter()
        .filter(|(p, _)| port_busy(*p))
        .map(|(p, what)| (*p, what.clone(), port_owner(*p), next_free_port(p.saturating_add(1))))
        .collect();
    ports_check(wanted, &busy)
}

/// The ports row from what was found: each busy port with who holds it
/// and the next free one.
pub fn ports_check(wanted: &[(u16, String)], busy: &[(u16, String, Option<PortOwner>, Option<u16>)]) -> Check {
    let all: Vec<String> = wanted.iter().map(|(p, _)| p.to_string()).collect();
    if busy.is_empty() {
        return check("ports", "Required ports", Status::Pass, format!("{} are free.", all.join(", ")));
    }
    let several = busy.len() > 1;
    let mut lines = Vec::new();
    let mut fixes = Vec::new();
    for (port, what, owner, instead) in busy {
        lines.push(match owner {
            Some(o) => format!("Port {port} ({what}) is used by {}.", o.describe()),
            None => format!("Port {port} ({what}) is used by another program."),
        });
        fixes.extend(owner_fixes(*port, what, owner.as_ref(), *instead, several));
    }
    check("ports", "Required ports", Status::Fail, lines.join(" ")).state("In use").fixes(fixes)
}

/// What can be done about a port `owner` holds: stop the container or the
/// service (a PostgreSQL can also be removed), or give SENTIENT another
/// port. `tag`: name the port in each label (several are busy).
pub fn owner_fixes(port: u16, what: &str, owner: Option<&PortOwner>, instead: Option<u16>, tag: bool) -> Vec<Fix> {
    let label = |l: &str| if tag { format!("{l} (port {port})") } else { l.to_string() };
    let mut v = Vec::new();
    match owner {
        Some(PortOwner { container: Some(name), .. }) => v.push(act(label("Stop the container"), FixAction::StopContainer { name: name.clone() })),
        Some(o @ PortOwner { unit: Some(unit), .. }) if !unit.starts_with("user@") => {
            v.push(act(label(if o.removed { "Stop it" } else { "Stop and disable it" }), FixAction::StopService { unit: unit.clone() }));
            if unit.starts_with("postgresql") {
                v.push(act(
                    label("Remove it"),
                    FixAction::RemovePostgres { unit: unit.clone(), package: o.package.clone(), data_dir: o.data_dir.clone().unwrap_or_else(|| pg_data_dir().into()) },
                ));
            }
        }
        Some(PortOwner { pid: Some(pid), .. }) => v.push(cmd(&label("Stop the program"), format!("sudo kill {pid}"))),
        _ => {}
    }
    if let Some(p) = instead {
        let l = if tag { format!("Use {p} for {what}") } else { format!("Use another port ({p})") };
        v.push(act(l, FixAction::UsePort { port, instead: p }));
    }
    if owner.is_none() || owner.is_some_and(|o| o.pid.is_none() && o.container.is_none() && o.unit.is_none()) {
        #[cfg(windows)]
        v.push(cmd(&label("See what uses it"), format!("Get-NetTCPConnection -LocalPort {port} -State Listen | Select-Object OwningProcess")));
        #[cfg(not(windows))]
        v.push(cmd(&label("See what uses it"), format!("sudo ss -ltnp 'sport = :{port}'")));
    }
    v
}

/// The ports of `wanted` something already listens on, as "8080 (HTTP)".
pub fn busy_ports(wanted: &[(u16, String)]) -> Vec<String> {
    wanted.iter().filter(|(p, _)| port_busy(*p)).map(|(p, what)| format!("{p} ({what})")).collect()
}

/// Whether something listens on `port` (TCP, this computer).
pub fn port_busy(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_err()
}

/// The first free port from `start` up (at most 100 tried).
pub fn next_free_port(start: u16) -> Option<u16> {
    (start..start.saturating_add(100)).find(|p| *p > 0 && !port_busy(*p))
}

/// The program listening on a port: its process name and where it runs
/// from, as far as this computer tells, and on Linux the systemd service or
/// Docker container it belongs to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PortOwner {
    pub process: String,
    pub path: Option<String>,
    pub pid: Option<u32>,
    /// The systemd service it runs in ("postgresql.service").
    pub unit: Option<String>,
    /// The Docker container that publishes the port.
    pub container: Option<String>,
    /// Its package was removed (the unit file or the program is gone) and
    /// it still runs.
    pub removed: bool,
    /// The package its unit came from, when it is installed.
    pub package: Option<String>,
    /// A PostgreSQL's data folder, for "Remove it".
    pub data_dir: Option<String>,
}

impl PortOwner {
    /// Whether it belongs to a SENTIENT install (ours or an earlier one):
    /// its program sits under SENTIENT's or its PostgreSQL's folders, or it
    /// is ClickHouse's WSL machine.
    pub fn is_sentient(&self, our_dirs: &[std::path::PathBuf]) -> bool {
        let path = self.path.as_deref().unwrap_or("").to_lowercase().replace('/', "\\");
        our_dirs.iter().any(|d| {
            let d = d.display().to_string().to_lowercase().replace('/', "\\");
            !d.is_empty() && path.starts_with(&d)
        })
    }

    /// "postgres.exe in C:\Program Files\PostgreSQL\16\bin", "the
    /// Docker container sentient-postgres", "postgres (pid 812), the
    /// postgresql.service service", or, for a server whose package was
    /// removed, "postgres (pid 812, postgresql.service), still running
    /// although its package was removed".
    pub fn describe(&self) -> String {
        if let Some(c) = &self.container {
            return format!("the Docker container {c}");
        }
        let pid = self.pid.map(|p| format!("pid {p}"));
        if self.removed {
            let ids: Vec<String> = pid.into_iter().chain(self.unit.clone()).collect();
            let ids = if ids.is_empty() { String::new() } else { format!(" ({})", ids.join(", ")) };
            return format!("{}{ids}, still running although its package was removed", self.process);
        }
        let named = match pid {
            Some(p) => format!("{} ({p})", self.process),
            None => self.process.clone(),
        };
        if let Some(u) = &self.unit {
            return format!("{named}, the {u} service");
        }
        match self.path.as_deref().and_then(|p| p.rfind(['\\', '/']).map(|i| &p[..i])) {
            Some(dir) if !dir.is_empty() => format!("{} in {dir}", named),
            _ => named,
        }
    }
}

/// Who listens on `port`, when the system says.
#[cfg(windows)]
pub fn port_owner(port: u16) -> Option<PortOwner> {
    let out = ps(&format!(
        "$c = Get-NetTCPConnection -LocalPort {port} -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1; \
         if ($c) {{ $p = Get-Process -Id $c.OwningProcess -ErrorAction SilentlyContinue; \
         if ($p) {{ $p.ProcessName + '.exe|' + $p.Path }} else {{ 'process ' + $c.OwningProcess + '|' }} }}"
    ));
    parse_owner(&out)
}

#[cfg(unix)]
pub fn port_owner(port: u16) -> Option<PortOwner> {
    // A Docker container: its docker-proxy runs as root, so ss would not
    // name it to this user anyway.
    if matches!(sh("command -v docker"), Some((true, _))) {
        if let Some((true, ps)) = sh("docker ps --format '{{.Names}}|{{.Ports}}' 2>/dev/null") {
            if let Some(name) = units::container_on(&ps, port) {
                return Some(PortOwner { process: "docker-proxy".into(), container: Some(name), ..Default::default() });
            }
        }
    }
    // `ss -ltnpH 'sport = :5432'`: users:(("postgres",pid=812,fd=6)), when
    // this user may see the process.
    let ss = sh(&format!("ss -ltnpH 'sport = :{port}' 2>/dev/null | head -1")).map(|(_, o)| o).unwrap_or_default();
    if let Some((name, pid)) = units::parse_ss(&ss) {
        let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok().map(|p| p.display().to_string());
        let unit = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok().and_then(|t| units::unit_of_cgroup(&t));
        let info = unit.as_ref().and_then(|u| units::show(std::slice::from_ref(u)).into_iter().next());
        let mut o = owner_of(name, Some(pid), exe, unit, info.as_ref());
        if o.unit.as_deref().is_some_and(|u| u.starts_with("postgresql")) {
            o.data_dir = Some(pg_data_dir().into());
        }
        return Some(o);
    }
    // PostgreSQL of another user (ss shows no process then): its socket
    // names the port, and systemd the service that runs it.
    let socket = ["/run/postgresql", "/var/run/postgresql", "/tmp"].iter().any(|d| std::path::Path::new(&format!("{d}/.s.PGSQL.{port}")).exists());
    if socket {
        let found = units::classify_postgres(&units::postgres_units());
        let u = match found {
            PgService::Installed(u) if u.running() => Some(u),
            PgService::LeftoverRunning(u) => Some(u),
            _ => None,
        };
        let mut o = match &u {
            Some(u) => owner_of("postgres".into(), (u.main_pid > 0).then_some(u.main_pid), None, Some(u.id.clone()), Some(u)),
            None => PortOwner { process: "a PostgreSQL server".into(), ..Default::default() },
        };
        o.data_dir = Some(pg_data_dir().into());
        return Some(o);
    }
    None
}

/// A listening process with what systemd says of its service. Removed: the
/// unit file is gone (not-found) or the program file was deleted under it.
pub fn owner_of(process: String, pid: Option<u32>, exe: Option<String>, unit: Option<String>, info: Option<&units::UnitInfo>) -> PortOwner {
    let deleted = exe.as_deref().is_some_and(|e| e.ends_with(" (deleted)"));
    let not_found = info.is_some_and(|i| i.load_state == "not-found");
    let package = info.filter(|i| i.loaded()).and_then(|i| units::package_owning(&i.fragment_path));
    PortOwner {
        process,
        path: exe.map(|e| e.trim_end_matches(" (deleted)").to_string()),
        pid,
        unit,
        container: None,
        removed: deleted || not_found,
        package,
        data_dir: None,
    }
}

#[cfg(not(any(windows, unix)))]
pub fn port_owner(_: u16) -> Option<PortOwner> {
    None
}

#[cfg_attr(not(windows), allow(dead_code))]
fn parse_owner(out: &str) -> Option<PortOwner> {
    let line = out.lines().map(str::trim).find(|l| !l.is_empty())?;
    let (process, path) = line.split_once('|').unwrap_or((line, ""));
    Some(PortOwner { process: process.trim().to_string(), path: (!path.trim().is_empty()).then(|| path.trim().to_string()), ..Default::default() })
}

#[cfg(test)]
mod port_owner_tests {
    use super::*;

    #[test]
    fn an_owner_is_read_and_told_apart_from_sentient() {
        let o = parse_owner("postgres.exe|C:\\Program Files\\PostgreSQL\\16\\bin\\postgres.exe\r\n").unwrap();
        assert_eq!(o.process, "postgres.exe");
        assert_eq!(o.describe(), "postgres.exe in C:\\Program Files\\PostgreSQL\\16\\bin");
        assert!(!o.is_sentient(&[std::path::PathBuf::from("C:\\PostgreSQL\\18")]));
        assert!(o.is_sentient(&[std::path::PathBuf::from("C:\\Program Files\\PostgreSQL\\16")]));
        assert!(parse_owner("  ").is_none());
    }

    fn unit(id: &str, load: &str, active: &str, pid: u32) -> units::UnitInfo {
        units::UnitInfo { id: id.into(), load_state: load.into(), active_state: active.into(), fragment_path: String::new(), main_pid: pid }
    }

    fn actions(c: &Check) -> Vec<(String, FixAction)> {
        c.fixes.iter().filter_map(|f| if let Fix::Action { label, action } = f { Some((label.clone(), action.clone())) } else { None }).collect()
    }

    #[test]
    fn a_removed_postgres_unit_is_not_an_existing_postgres() {
        // `pacman -R postgresql`: systemd still lists the unit as not-found.
        let names = units::parse_unit_names("● postgresql.service not-found inactive dead postgresql.service\n");
        assert_eq!(names, vec!["postgresql.service"]);
        let found = units::classify_postgres(&[unit("postgresql.service", "not-found", "inactive", 0)]);
        let c = existing_pg_check(&found, None, "/var/lib/postgres/data", Some(5433));
        assert!(c.status == Status::Pass, "{}", c.detail);
        assert!(!c.detail.contains('●'));
        assert!(c.fixes.is_empty());
    }

    #[test]
    fn an_installed_postgres_is_named_with_its_actions() {
        let found = units::classify_postgres(&[unit("postgresql.service", "loaded", "active", 812)]);
        let c = existing_pg_check(&found, Some("postgresql".into()), "/var/lib/postgres/data", Some(5433));
        assert!(c.status == Status::Warn);
        assert_eq!(c.state.as_deref(), Some("Already installed"));
        assert_eq!(c.detail, "PostgreSQL is already installed as postgresql.service from the package postgresql, running (pid 812).");
        let a = actions(&c);
        assert_eq!(a.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>(), ["Stop and disable it", "Remove it", "Use another port (5433)"]);
        assert_eq!(a[0].1, FixAction::StopService { unit: "postgresql.service".into() });
        assert_eq!(
            a[1].1,
            FixAction::RemovePostgres { unit: "postgresql.service".into(), package: Some("postgresql".into()), data_dir: "/var/lib/postgres/data".into() }
        );
        assert_eq!(a[2].1, FixAction::UsePort { port: 5432, instead: 5433 });
    }

    #[test]
    fn a_still_running_server_of_a_removed_package_is_said_so() {
        let found = units::classify_postgres(&[unit("postgresql.service", "not-found", "active", 812)]);
        let c = existing_pg_check(&found, None, "/var/lib/postgres/data", Some(5433));
        assert_eq!(c.state.as_deref(), Some("Still running"));
        assert_eq!(c.detail, "postgresql.service is still running (pid 812) although its package was removed.");
        assert_eq!(actions(&c)[0], ("Stop it".into(), FixAction::StopService { unit: "postgresql.service".into() }));
    }

    #[test]
    fn port_owners_are_named() {
        // A systemd service.
        let info = unit("postgresql.service", "loaded", "active", 812);
        let o = owner_of("postgres".into(), Some(812), Some("/usr/bin/postgres".into()), Some("postgresql.service".into()), Some(&info));
        assert_eq!(o.describe(), "postgres (pid 812), the postgresql.service service");
        assert!(!o.removed);
        // A Docker container.
        let o = PortOwner { process: "docker-proxy".into(), container: Some("sentient-postgres".into()), ..Default::default() };
        assert_eq!(o.describe(), "the Docker container sentient-postgres");
        // The postmaster of a removed package: its unit not-found, or its program deleted.
        let gone = unit("postgresql.service", "not-found", "active", 812);
        let o = owner_of("postgres".into(), Some(812), None, Some("postgresql.service".into()), Some(&gone));
        assert_eq!(o.describe(), "postgres (pid 812, postgresql.service), still running although its package was removed");
        let o = owner_of("postgres".into(), Some(812), Some("/usr/bin/postgres (deleted)".into()), None, None);
        assert!(o.removed);
        assert_eq!(o.path.as_deref(), Some("/usr/bin/postgres"));
        assert_eq!(o.describe(), "postgres (pid 812), still running although its package was removed");
    }

    #[test]
    fn busy_ports_name_their_owner_and_offer_actions() {
        let wanted = [(8080, "HTTP".to_string()), (5432, "PostgreSQL".to_string())];
        let docker = PortOwner { process: "docker-proxy".into(), container: Some("sentient-postgres".into()), ..Default::default() };
        let c = ports_check(&wanted, &[(5432, "PostgreSQL".into(), Some(docker), Some(5433))]);
        assert_eq!(c.detail, "Port 5432 (PostgreSQL) is used by the Docker container sentient-postgres.");
        assert_eq!(c.state.as_deref(), Some("In use"));
        assert_eq!(
            actions(&c),
            vec![
                ("Stop the container".into(), FixAction::StopContainer { name: "sentient-postgres".into() }),
                ("Use another port (5433)".into(), FixAction::UsePort { port: 5432, instead: 5433 }),
            ]
        );
        // A removed package's postmaster: stop it, remove it, or another port.
        let gone = unit("postgresql.service", "not-found", "active", 812);
        let mut o = owner_of("postgres".into(), Some(812), None, Some("postgresql.service".into()), Some(&gone));
        o.data_dir = Some("/var/lib/postgres/data".into());
        let c = ports_check(&wanted, &[(5432, "PostgreSQL".into(), Some(o), Some(5433))]);
        assert_eq!(c.detail, "Port 5432 (PostgreSQL) is used by postgres (pid 812, postgresql.service), still running although its package was removed.");
        assert_eq!(actions(&c).iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>(), ["Stop it", "Remove it", "Use another port (5433)"]);
        // Nobody known: another port, and the command that shows who.
        let c = ports_check(&wanted, &[(8080, "HTTP".into(), None, Some(8081)), (5432, "PostgreSQL".into(), None, None)]);
        assert_eq!(actions(&c), vec![("Use 8081 for HTTP".into(), FixAction::UsePort { port: 8080, instead: 8081 })]);
        // The command that shows who holds it: ss here, Get-NetTCPConnection on Windows.
        let shows = if cfg!(windows) { "-LocalPort 5432" } else { ":5432" };
        assert!(c.fixes.iter().any(|f| matches!(f, Fix::Command { command, .. } if command.contains(shows))));
        // Its server is the PostgreSQL row's too: acted on there only.
        let pg = existing_pg_check(&PgService::LeftoverRunning(gone.clone()), None, "/var/lib/postgres/data", Some(5433));
        let mut o = owner_of("postgres".into(), Some(812), None, Some("postgresql.service".into()), Some(&gone));
        o.data_dir = Some("/var/lib/postgres/data".into());
        let mut v = vec![pg, ports_check(&wanted, &[(5432, "PostgreSQL".into(), Some(o), Some(5433))])];
        share_fixes(&mut v);
        assert!(actions(&v[1]).is_empty(), "{:?}", actions(&v[1]));
        assert!(v[1].detail.ends_with("For port 5432, see Existing PostgreSQL."), "{}", v[1].detail);
        assert_eq!(actions(&v[0]).len(), 3);
        // Free: ready, nothing to do.
        let c = ports_check(&wanted, &[]);
        assert!(c.status == Status::Pass && c.fixes.is_empty());
    }
}

/// A PostgreSQL that we did not install is a genuine hazard: the installer would
/// register a second service against the same port, and only one can hold the
/// data directory. Report it rather than failing later in a confusing way.
fn existing_postgres() -> Check {
    #[cfg(windows)]
    {
        let svc = ps(
            "(Get-Service -Name 'postgres*','*postgresql*' -ErrorAction SilentlyContinue |              Where-Object { $_.Name -ne 'sentient-postgresql' } | Select-Object -Expand Name) -join ', '",
        );
        if svc.trim().is_empty() {
            return check(
                "postgres-conflict",
                "Existing PostgreSQL",
                Status::Pass,
                "No other PostgreSQL service found.",
            );
        }
        return check(
            "postgres-conflict",
            "Existing PostgreSQL",
            Status::Fail,
            format!(
                "another PostgreSQL service is installed ({}). Remove it, or point SENTIENT at it instead of installing its own.",
                svc.trim()
            ),
        );
    }
    #[cfg(not(windows))]
    check("postgres-conflict", "Existing PostgreSQL", Status::Unknown, "Windows only.")
}

/// Online installs pull PostgreSQL from EDB and TimescaleDB from GitHub. Check
/// both, since a network that reaches one may be blocked from the other.
fn download_hosts() -> Check {
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;
    let reachable = |host: &str| -> bool {
        host.to_socket_addrs()
            .ok()
            .and_then(|mut a| a.next())
            .map(|addr| TcpStream::connect_timeout(&addr, Duration::from_secs(5)).is_ok())
            .unwrap_or(false)
    };
    let edb = reachable("get.enterprisedb.com:443");
    let gh = reachable("github.com:443");
    match (edb, gh) {
        (true, true) => check("downloads", "Download servers", Status::Pass, "EDB and GitHub are reachable."),
        (false, true) => check("downloads", "Download servers", Status::Fail,
            "Can't reach get.enterprisedb.com (PostgreSQL). Use an offline bundle instead."),
        (true, false) => check("downloads", "Download servers", Status::Fail,
            "Can't reach github.com (TimescaleDB). Use an offline bundle instead."),
        (false, false) => check("downloads", "Download servers", Status::Fail,
            "No access to the download servers. Use an offline bundle instead."),
    }
}

// ---- command helper ----------------------------------------------------------

#[cfg(windows)]
fn ps(script: &str) -> String {
    output("powershell", &["-NoProfile", "-NonInteractive", "-Command", script])
        .map(|(_, out, _)| decode(&out).trim().to_string())
        .unwrap_or_default()
}

// ---- Windows checks ----------------------------------------------------------

fn windows_version() -> Check {
    #[cfg(windows)]
    {
        let build: u32 = ps("[System.Environment]::OSVersion.Version.Build").parse().unwrap_or(0);
        if build == 0 {
            check("windows", "Windows version", Status::Unknown, "Could not read the Windows build.")
        } else if build >= 19041 {
            check("windows", "Windows version", Status::Pass, format!("Build {build} supports WSL2."))
        } else {
            check("windows", "Windows version", Status::Fail,
                  format!("Build {build}; WSL2 needs Windows 10 2004 (19041) or newer."))
        }
    }
    #[cfg(not(windows))]
    check("windows", "Windows version", Status::Unknown, "This installer targets Windows.")
}

fn administrator() -> Check {
    #[cfg(windows)]
    {
        let admin = ps("([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltinRole]::Administrator)");
        if admin.eq_ignore_ascii_case("true") {
            check("admin", "Administrator rights", Status::Pass, "Running elevated.")
        } else {
            check("admin", "Administrator rights", Status::Setup,
                  "Not elevated. The installer will request administrator rights when needed.")
        }
    }
    #[cfg(not(windows))]
    check("admin", "Administrator rights", Status::Unknown, "Windows only.")
}

fn virtualization() -> Check {
    #[cfg(windows)]
    {
        let v = ps("$hv=(Get-CimInstance Win32_ComputerSystem).HypervisorPresent; $vf=(Get-CimInstance Win32_Processor | Select-Object -First 1).VirtualizationFirmwareEnabled; if($hv -or $vf){'enabled'}else{'disabled'}");
        match v.as_str() {
            "enabled" => check("virt", "CPU virtualization", Status::Pass, "Enabled in firmware."),
            "disabled" => check("virt", "CPU virtualization", Status::Fail,
                "Disabled. Enable virtualization (VT-x / AMD-V, SVM) in your BIOS/UEFI. Required for WSL2."),
            _ => check("virt", "CPU virtualization", Status::Unknown, "Could not determine virtualization state."),
        }
    }
    #[cfg(not(windows))]
    check("virt", "CPU virtualization", Status::Unknown, "Windows only.")
}

fn wsl_installed() -> Check {
    #[cfg(windows)]
    {
        match output("wsl.exe", &["--status"]) {
            Some((true, out, _)) => {
                let s = decode(&out);
                let v2 = s.contains('2');
                check("wsl", "WSL2", Status::Pass,
                      if v2 { "Installed.".into() } else { "Installed (will ensure default version 2).".to_string() })
            }
            _ => check("wsl", "WSL2", Status::Setup, "Not installed. The installer will install and update WSL2."),
        }
    }
    #[cfg(not(windows))]
    check("wsl", "WSL2", Status::Unknown, "Windows only.")
}

fn distro_present() -> Check {
    #[cfg(windows)]
    {
        let present = output("wsl.exe", &["-l", "-q"])
            .map(|(_, out, _)| decode(&out).lines().any(|l| l.trim().eq_ignore_ascii_case("sentient")))
            .unwrap_or(false);
        if present {
            check("distro", "SENTIENT WSL distro", Status::Pass, "The 'sentient' distro exists.")
        } else {
            check("distro", "SENTIENT WSL distro", Status::Setup, "Not present. The installer will create it.")
        }
    }
    #[cfg(not(windows))]
    check("distro", "SENTIENT WSL distro", Status::Unknown, "Windows only.")
}

fn docker_ready() -> Check {
    #[cfg(windows)]
    {
        let args = ["-d", "sentient", "--", "docker", "version"];
        if crate::distro::left_out(&args) {
            return check("docker", "Docker Engine", Status::Unknown, "Not asked: the distro is stopped.");
        }
        match output("wsl.exe", &args) {
            Some((true, _, _)) => check("docker", "Docker Engine", Status::Pass, "Docker is running in the distro."),
            _ => check("docker", "Docker Engine", Status::Setup, "Not set up yet. The installer will install Docker Engine."),
        }
    }
    #[cfg(not(windows))]
    check("docker", "Docker Engine", Status::Unknown, "Windows only.")
}

// ---- cross-platform checks ---------------------------------------------------

fn disk_space() -> Check {
    use sysinfo::Disks;
    let disks = Disks::new_with_refreshed_list();
    let want_win = cfg!(windows);
    let avail = disks
        .list()
        .iter()
        .filter(|d| {
            let m = d.mount_point().to_string_lossy().to_string();
            if want_win { m.starts_with("C:") } else { m == "/" }
        })
        .map(|d| d.available_space())
        .max()
        .or_else(|| disks.list().iter().map(|d| d.available_space()).max());
    match avail {
        Some(bytes) => {
            let gb = bytes as f64 / 1e9;
            if gb >= 10.0 {
                check("disk", "Disk space", Status::Pass, format!("{gb:.0} GB free."))
            } else {
                #[cfg(windows)]
                let see = cmd("See what takes the room", "Get-PSDrive C");
                #[cfg(not(windows))]
                let see = cmd("See what takes the room", "sudo du -xh --max-depth=1 / 2>/dev/null | sort -h | tail -n 12");
                check("disk", "Disk space", Status::Fail,
                      format!("{gb:.1} GB free. 10 GB+ recommended for the images and data."))
                    .fixes(vec![see])
            }
        }
        None => check("disk", "Disk space", Status::Unknown, "Could not read free disk space."),
    }
}

fn internet() -> Check {
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;
    let ok = "registry-1.docker.io:443"
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
        .map(|addr| TcpStream::connect_timeout(&addr, Duration::from_secs(5)).is_ok())
        .unwrap_or(false);
    if ok {
        check("internet", "Internet access", Status::Pass, "Docker registry is reachable.")
    } else {
        check("internet", "Internet access", Status::Fail, "Can't reach the Docker registry (registry-1.docker.io:443).")
    }
}
