//! Read-only preflight checks. Nothing here changes the system — it only reports
//! whether each prerequisite for installing SENTIENT is satisfied, will be set
//! up by the installer, or is a blocker the user must fix.
//!
//! Two sets, because the two deployment modes need different things:
//! [`run_all`] for the WSL2 + Docker mode, [`run_native`] for the native mode
//! (see [`crate::native`]), which needs neither but does need free ports and no
//! competing PostgreSQL.

use serde::Serialize;

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
}

fn check(id: &str, label: &str, status: Status, detail: impl Into<String>) -> Check {
    Check { id: id.into(), label: label.into(), status, detail: detail.into() }
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
            *slot = ports_free_of(p);
        }
    }
    v
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
              "Can't reach packages.clickhouse.com — fine for “PostgreSQL only”; the ClickHouse flavour needs it (or an offline bundle).")
    }
}

#[cfg(unix)]
fn sh(cmd: &str) -> Option<(bool, String)> {
    let out = std::process::Command::new("sh").args(["-c", cmd]).output().ok()?;
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
            "Docker is not installed. Install Docker Engine from your distribution or docs.docker.com, then re-check."),
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
            "Running, but this user isn't in the `docker` group yet — the installer will add you (asks for your password)."),
        Some((false, _)) if matches!(sh("command -v docker"), Some((true, _))) => check("docker-daemon", "Docker daemon", Status::Setup,
            "Docker is installed but not running — the installer will enable and start it."),
        Some((false, _)) => check("docker-daemon", "Docker daemon", Status::Setup,
            "Not set up yet — the installer will install Docker from your distribution."),
        None => check("docker-daemon", "Docker daemon", Status::Unknown, "Could not query Docker."),
    }
}

fn compose_plugin() -> Check {
    match sh("docker compose version --short") {
        Some((true, v)) => check("compose", "Docker Compose", Status::Pass, format!("Compose {} (plugin).", v)),
        _ => check("compose", "Docker Compose", Status::Setup,
            "The `docker compose` plugin is missing. Install docker-compose-plugin (Debian/Ubuntu) or docker-compose (Arch), then re-check."),
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
            return check("pkg", "Package manager", Status::Pass, format!("{label} — PostgreSQL will come from here."));
        }
    }
    check("pkg", "Package manager", Status::Fail, "No supported package manager found (apt, pacman, dnf, zypper).")
}

fn existing_postgres_linux() -> Check {
    match sh("systemctl list-units --type=service --all --no-legend 'postgresql*' 2>/dev/null | head -3") {
        Some((true, out)) if !out.is_empty() => check("existing-pg", "Existing PostgreSQL", Status::Setup,
            format!("A PostgreSQL service already exists ({}). The installer will use a dedicated cluster on the chosen port and leave it alone.", out.split_whitespace().next().unwrap_or("postgresql"))),
        _ => check("existing-pg", "Existing PostgreSQL", Status::Pass, "No PostgreSQL service is installed yet."),
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
    let mut c = ports_free_of(&[
        (8080, "HTTP".into()), (1883, "MQTT".into()), (5683, "CoAP".into()), (5432, "PostgreSQL".into()),
    ]);
    if c.status == Status::Fail {
        c.status = Status::Warn;
        c.label = "Default ports".into();
    }
    c
}

/// Are these TCP ports free to bind? A busy port is a real blocker for the
/// install, but the operator can pick different ones on the Configure step,
/// and the Install step re-checks the ones actually chosen.
pub fn ports_free_of(wanted: &[(u16, String)]) -> Check {
    let busy = busy_ports(wanted);
    let all: Vec<String> = wanted.iter().map(|(p, _)| p.to_string()).collect();
    if busy.is_empty() {
        check("ports", "Required ports", Status::Pass, format!("{} are free.", all.join(", ")))
    } else {
        check(
            "ports",
            "Required ports",
            Status::Fail,
            format!(
                "already in use: {} — stop whatever is listening, or choose other ports later in setup (PostgreSQL under Database settings, the rest under Network ports).",
                busy.join(", ")
            ),
        )
    }
}

/// The ports of `wanted` something already listens on, as "8080 (HTTP)".
pub fn busy_ports(wanted: &[(u16, String)]) -> Vec<String> {
    use std::net::TcpListener;
    wanted
        .iter()
        .filter(|(p, _)| TcpListener::bind(("127.0.0.1", *p)).is_err())
        .map(|(p, what)| format!("{p} ({what})"))
        .collect()
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
                  "Not elevated — the installer will request administrator rights when needed.")
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
                "Disabled — enable virtualization (VT-x / AMD-V, SVM) in your BIOS/UEFI. Required for WSL2."),
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
            _ => check("wsl", "WSL2", Status::Setup, "Not installed — the installer will install and update WSL2."),
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
            check("distro", "SENTIENT WSL distro", Status::Setup, "Not present — the installer will create it.")
        }
    }
    #[cfg(not(windows))]
    check("distro", "SENTIENT WSL distro", Status::Unknown, "Windows only.")
}

fn docker_ready() -> Check {
    #[cfg(windows)]
    {
        match output("wsl.exe", &["-d", "sentient", "--", "docker", "version"]) {
            Some((true, _, _)) => check("docker", "Docker Engine", Status::Pass, "Docker is running in the distro."),
            _ => check("docker", "Docker Engine", Status::Setup, "Not set up yet — the installer will install Docker Engine."),
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
                check("disk", "Disk space", Status::Fail,
                      format!("{gb:.1} GB free — 10 GB+ recommended for the images and data."))
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
