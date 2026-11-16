// Copyright © 2016-2026 The SENTIENT Authors

//! Uninstall, part by part: SENTIENT, its PostgreSQL, its ClickHouse, and
//! (Windows) the WSL distribution and WSL itself, each removed for real
//! (service stopped and disabled, leftover processes ended, the package or
//! the binaries Setup put there removed, the data folder too unless it is
//! kept), then a check that nothing is left.
//!
//! The scripts are built by plain functions (tested here); the app decides
//! which parts go (app `uninstall`). Root work runs as one script per part
//! through [`crate::exec::run_privileged`], so a desktop session is asked
//! for the password once (crate::elevation).
//!
//! The check after it ([`verify`]) runs `ss`/`systemctl`/`docker`/`ps`
//! (`netstat`/`tasklist`/`sc`/`wsl`/`dism` on Windows) through an injected
//! runner, so its parsing is tested without any of them.

use std::path::{Path, PathBuf};

#[cfg(unix)]
use crate::exec::shq;
#[cfg(not(unix))]
fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The package manager family of a Linux machine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pm {
    Pacman,
    Apt,
    Dnf,
    Zypper,
}

impl Pm {
    /// This machine's (Linux), from what is on PATH.
    #[cfg(unix)]
    pub fn detect() -> Option<Pm> {
        use crate::exec::have;
        if have("pacman") {
            Some(Pm::Pacman)
        } else if have("apt-get") {
            Some(Pm::Apt)
        } else if have("dnf") {
            Some(Pm::Dnf)
        } else if have("zypper") {
            Some(Pm::Zypper)
        } else {
            None
        }
    }
    #[cfg(not(unix))]
    pub fn detect() -> Option<Pm> {
        None
    }

    /// The PostgreSQL server packages Setup installs (crate::native_linux::setup).
    pub fn postgres_packages(self) -> &'static [&'static str] {
        match self {
            Pm::Pacman => &["postgresql", "postgresql-old-upgrade"],
            Pm::Apt => &["postgresql", "postgresql-common", "postgresql-client-common"],
            Pm::Dnf | Pm::Zypper => &["postgresql-server", "postgresql"],
        }
    }

    /// Where that PostgreSQL keeps its clusters (and on Debian its
    /// configuration and logs).
    pub fn postgres_data_dirs(self) -> &'static [&'static str] {
        match self {
            Pm::Pacman => &["/var/lib/postgres"],
            Pm::Apt => &["/var/lib/postgresql", "/etc/postgresql", "/var/log/postgresql"],
            Pm::Dnf | Pm::Zypper => &["/var/lib/pgsql"],
        }
    }

    /// The command that says whether `pkg` is installed (exit 0 = yes).
    pub fn query(self, pkg: &str) -> (String, Vec<String>) {
        match self {
            Pm::Pacman => ("pacman".into(), vec!["-Q".into(), pkg.into()]),
            Pm::Apt => ("dpkg-query".into(), vec!["-W".into(), "-f=${Status}".into(), pkg.into()]),
            Pm::Dnf | Pm::Zypper => ("rpm".into(), vec!["-q".into(), pkg.into()]),
        }
    }

    /// The command line that removes `pkgs`, for a person to type.
    pub fn remove_command(self, pkgs: &[&str], purge: bool) -> String {
        let list = pkgs.join(" ");
        match self {
            Pm::Pacman => format!("sudo pacman -Rns {list}"),
            Pm::Apt if purge => format!("sudo apt-get purge --auto-remove {list}"),
            Pm::Apt => format!("sudo apt-get remove --auto-remove {list}"),
            Pm::Dnf => format!("sudo dnf remove {list}"),
            Pm::Zypper => format!("sudo zypper rm {list}"),
        }
    }
}

/// What uninstalling a database means here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DbRemoval {
    /// Setup installed it: stop and disable it, end its processes, remove
    /// the package or binaries and the service; `keep_data` keeps the data
    /// folder.
    Remove { keep_data: bool },
    /// It was there before SENTIENT: SENTIENT stops using it. Without
    /// `keep_data` SENTIENT's own database and user in it are dropped; the
    /// service itself is left as it is.
    StopUsing { keep_data: bool },
}

impl DbRemoval {
    pub fn keeps_data(self) -> bool {
        match self {
            DbRemoval::Remove { keep_data } | DbRemoval::StopUsing { keep_data } => keep_data,
        }
    }
}

// ---------------------------------------------------------------------------
// Linux native: the scripts (run as root)
// ---------------------------------------------------------------------------

/// Root script that removes the distribution's PostgreSQL (`Remove`) or
/// only SENTIENT's database in it (`StopUsing`). `port` is where it listens;
/// `db`/`user` SENTIENT's database and user.
pub fn linux_postgres_script(pm: Pm, how: DbRemoval, port: u16, db: &str, user: &str, timescale: bool) -> String {
    let drop_ours = format!(
        "runuser -u postgres -- psql -p {port} -qAt -c {} 2>/dev/null || true\n\
         runuser -u postgres -- psql -p {port} -qAt -c {} 2>/dev/null || true\n",
        shq(&format!("DROP DATABASE IF EXISTS \"{db}\" WITH (FORCE);")),
        shq(&format!("DROP USER IF EXISTS \"{user}\";")),
    );
    let keep_data = match how {
        DbRemoval::StopUsing { keep_data: true } => {
            return "echo '>> PostgreSQL was here before SENTIENT: it is left as it is, with SENTIENT'\"'\"'s database in it.'\n".into();
        }
        DbRemoval::StopUsing { keep_data: false } => {
            return format!("echo '>> PostgreSQL was here before SENTIENT: only SENTIENT'\"'\"'s database and user are removed from it.'\n{drop_ours}");
        }
        DbRemoval::Remove { keep_data } => keep_data,
    };
    let mut pkgs = pm.postgres_packages().join(" ");
    if timescale {
        pkgs.push_str(match pm {
            Pm::Pacman => " timescaledb",
            Pm::Apt => " timescaledb-2-postgresql-16 timescaledb-2-postgresql-17 timescaledb-2-postgresql-18 timescaledb-tools",
            Pm::Dnf | Pm::Zypper => " timescaledb",
        });
    }
    let remove = match pm {
        Pm::Pacman => "[ -n \"$PKGS\" ] && pacman -Rns --noconfirm $PKGS\n".to_string(),
        Pm::Apt if keep_data => "export DEBIAN_FRONTEND=noninteractive\n[ -n \"$PKGS\" ] && apt-get remove -y -qq --auto-remove $PKGS 'postgresql-[0-9]*'\n".into(),
        Pm::Apt => "export DEBIAN_FRONTEND=noninteractive\n[ -n \"$PKGS\" ] && apt-get purge -y -qq --auto-remove $PKGS 'postgresql-[0-9]*' 'postgresql-client-[0-9]*'\n".into(),
        Pm::Dnf => "[ -n \"$PKGS\" ] && dnf remove -y $PKGS\n".into(),
        Pm::Zypper => "[ -n \"$PKGS\" ] && zypper --non-interactive rm $PKGS\n".into(),
    };
    let installed = match pm {
        Pm::Pacman => format!("PKGS=$(pacman -Qq {pkgs} 2>/dev/null | tr '\\n' ' ')\n"),
        Pm::Apt => format!("PKGS=$(for p in {pkgs}; do dpkg-query -W -f='${{Status}}' \"$p\" 2>/dev/null | grep -q 'ok installed' && printf '%s ' \"$p\"; done)\n"),
        Pm::Dnf | Pm::Zypper => format!("PKGS=$(for p in {pkgs}; do rpm -q \"$p\" >/dev/null 2>&1 && printf '%s ' \"$p\"; done)\n"),
    };
    let mut s = String::from("set -u\n");
    // Where its data is, asked while it still runs (a cluster moved out of
    // the default folder goes too).
    s.push_str(&format!("DATA=$(runuser -u postgres -- psql -p {port} -XAtqc 'SHOW data_directory' 2>/dev/null || true)\n"));
    s.push_str(
        "echo '>> Stopping PostgreSQL and turning it off at boot'\n\
         systemctl disable --now postgresql.service >/dev/null 2>&1 || true\n\
         for u in $(systemctl list-units --plain --no-legend --all 'postgresql@*' 2>/dev/null | awk '{print $1}'); do systemctl disable --now \"$u\" >/dev/null 2>&1 || true; done\n\
         for i in $(seq 1 30); do pgrep -u postgres >/dev/null 2>&1 || break; sleep 1; done\n\
         if pgrep -u postgres >/dev/null 2>&1; then echo '>> Ending the PostgreSQL processes still running'; pkill -9 -u postgres 2>/dev/null || true; sleep 1; fi\n",
    );
    s.push_str("echo '>> Removing the PostgreSQL packages'\n");
    s.push_str(&installed);
    s.push_str(&remove);
    s.push_str(
        "rm -rf /etc/systemd/system/postgresql.service.d /etc/systemd/system/postgresql.service\n\
         systemctl daemon-reload 2>/dev/null || true\n\
         systemctl reset-failed 'postgresql*' >/dev/null 2>&1 || true\n",
    );
    if keep_data {
        s.push_str("echo '>> PostgreSQL'\"'\"'s data folder is kept'\n");
    } else {
        s.push_str("echo '>> Removing PostgreSQL'\"'\"'s data folder'\n");
        for d in pm.postgres_data_dirs() {
            s.push_str(&format!("rm -rf {}\n", shq(d)));
        }
        // A cluster outside them: only a real PostgreSQL data folder.
        s.push_str("case \"$DATA\" in /*) [ -f \"$DATA/PG_VERSION\" ] && rm -rf \"$DATA\";; esac\n");
    }
    s.push_str("true\n");
    s
}

/// The links `clickhouse install` puts beside the binary.
pub const CLICKHOUSE_LINKS: &[&str] = &[
    "clickhouse-server",
    "clickhouse-client",
    "clickhouse-local",
    "clickhouse-benchmark",
    "clickhouse-compressor",
    "clickhouse-format",
    "clickhouse-extract-from-config",
    "clickhouse-obfuscator",
    "clickhouse-git-import",
    "clickhouse-keeper",
    "clickhouse-keeper-converter",
    "clickhouse-keeper-client",
    "clickhouse-disks",
    "clickhouse-static-files-disk-uploader",
    "clickhouse-su",
    "ch",
    "chl",
    "chc",
];

/// Root script that removes the ClickHouse Setup installed (the vendor's
/// static binary and the unit Setup wrote: crate::native_linux) or only
/// SENTIENT's database in it.
pub fn linux_clickhouse_script(how: DbRemoval, native_port: u16, db: &str, user: &str) -> String {
    let drop_ours = format!(
        "clickhouse-client --port {native_port} -n -q {} 2>/dev/null || true\n",
        shq(&format!("DROP DATABASE IF EXISTS {db}; DROP USER IF EXISTS {user};"))
    );
    let keep_data = match how {
        DbRemoval::StopUsing { keep_data: true } => {
            return "echo '>> ClickHouse was here before SENTIENT: it is left as it is, with SENTIENT'\"'\"'s database in it.'\n".into();
        }
        DbRemoval::StopUsing { keep_data: false } => {
            return format!("echo '>> ClickHouse was here before SENTIENT: only SENTIENT'\"'\"'s database and user are removed from it.'\n{drop_ours}");
        }
        DbRemoval::Remove { keep_data } => keep_data,
    };
    let mut s = String::from("set -u\n");
    s.push_str(
        "echo '>> Stopping ClickHouse and turning it off at boot'\n\
         systemctl disable --now clickhouse-server.service >/dev/null 2>&1 || true\n\
         for i in $(seq 1 60); do pgrep -u clickhouse >/dev/null 2>&1 || pgrep -x clickhouse-serv >/dev/null 2>&1 || break; sleep 1; done\n\
         if pgrep -u clickhouse >/dev/null 2>&1 || pgrep -x clickhouse-serv >/dev/null 2>&1; then echo '>> Ending the ClickHouse processes still running'; pkill -9 -u clickhouse 2>/dev/null || true; pkill -9 -x clickhouse-serv 2>/dev/null || true; sleep 1; fi\n\
         echo '>> Removing the ClickHouse program and its service'\n\
         rm -f /etc/systemd/system/clickhouse-server.service\n\
         rm -rf /etc/systemd/system/clickhouse-server.service.d\n",
    );
    for l in CLICKHOUSE_LINKS {
        s.push_str(&format!("[ -L /usr/bin/{l} ] && rm -f /usr/bin/{l}\n"));
    }
    s.push_str(
        "rm -f /usr/bin/clickhouse\n\
         rm -rf /etc/clickhouse-server /etc/clickhouse-client /etc/clickhouse-keeper /var/log/clickhouse-server /run/clickhouse-server /etc/security/limits.d/clickhouse.conf\n\
         rm -rf /var/lib/sentient/downloads/clickhouse\n\
         systemctl daemon-reload 2>/dev/null || true\n\
         systemctl reset-failed clickhouse-server.service >/dev/null 2>&1 || true\n",
    );
    if keep_data {
        s.push_str("echo '>> ClickHouse'\"'\"'s data folder is kept'\n");
    } else {
        s.push_str(
            "echo '>> Removing ClickHouse'\"'\"'s data folder'\n\
             rm -rf /var/lib/clickhouse\n\
             userdel clickhouse >/dev/null 2>&1 || true\n\
             groupdel clickhouse >/dev/null 2>&1 || true\n",
        );
    }
    s.push_str("true\n");
    s
}

/// Run a Linux removal script as root (one password for the job).
#[cfg(unix)]
pub fn run_root(sink: &crate::progress::ProgressFn, script: &str) -> Result<(), String> {
    crate::exec::run_privileged(sink, script)
}

// ---------------------------------------------------------------------------
// Docker (Linux): containers, images and volumes, part by part
// ---------------------------------------------------------------------------

/// What goes of a Docker stack.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DockerParts {
    pub sentient: bool,
    pub postgres: bool,
    pub clickhouse: bool,
    pub keep_pg_data: bool,
    pub keep_ch_data: bool,
    /// SENTIENT's own volume (file stores, backups).
    pub keep_files: bool,
    /// The stack has a ClickHouse container at all.
    pub has_clickhouse: bool,
}

/// The shell script that removes them: each container with its image, the
/// volumes not kept, and the compose file once nothing of the stack is left.
pub fn docker_script(names: &crate::distro::Names, parts: &DockerParts, compose_files: &[String]) -> String {
    let mut s = String::new();
    let mut one = |container: &str, volumes: &[&str]| {
        s.push_str(&format!(
            "IMG=$(docker inspect -f '{{{{.Config.Image}}}}' {c} 2>/dev/null || true)\n\
             docker rm -f {c} >/dev/null 2>&1 || true\n\
             [ -n \"$IMG\" ] && {{ docker rmi \"$IMG\" >/dev/null 2>&1 && echo \">> Removed the image $IMG\" || echo \">> The image $IMG is still used by another container: kept\"; }}\n",
            c = shq(container)
        ));
        for v in volumes {
            s.push_str(&format!("docker volume rm {} >/dev/null 2>&1 || true\n", shq(v)));
        }
    };
    if parts.sentient {
        let data: Vec<&str> = if parts.keep_files { vec![] } else { vec![names.vol_data.as_str()] };
        one(&names.app, &data);
    }
    if parts.postgres {
        let v: Vec<&str> = if parts.keep_pg_data { vec![] } else { vec![names.vol_pg.as_str()] };
        one(&names.pg, &v);
    }
    if parts.clickhouse {
        let v: Vec<&str> = if parts.keep_ch_data { vec![] } else { vec![names.vol_ch.as_str()] };
        one(&names.ch, &v);
    }
    if parts.sentient && parts.postgres && (parts.clickhouse || !parts.has_clickhouse) {
        for f in compose_files {
            s.push_str(&format!("rm -f {}\n", shq(f)));
        }
    }
    s
}

// ---------------------------------------------------------------------------
// The check afterwards
// ---------------------------------------------------------------------------

/// What must be gone after the uninstall.
#[derive(Clone, Debug, Default)]
pub struct Expect {
    /// (what it is, process names): none of them may run.
    pub processes: Vec<(String, Vec<String>)>,
    /// (what it is, port): nothing may listen there.
    pub ports: Vec<(String, u16)>,
    /// systemd units (Linux) or Windows services: none may be registered.
    pub units: Vec<String>,
    /// Linux packages: none may be installed.
    pub packages: Vec<String>,
    pub pm: Option<Pm>,
    /// Docker containers and images: none may be left.
    pub containers: Vec<String>,
    pub images: Vec<String>,
    pub volumes: Vec<String>,
    /// Files and folders: none may exist.
    pub paths: Vec<PathBuf>,
    /// WSL distributions (of this account): none may be registered.
    pub wsl_distros: Vec<String>,
    /// The WSL Windows feature must be off (or off after the restart).
    pub wsl_feature_off: bool,
    pub windows: bool,
}

/// One line of the result.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Check {
    pub what: String,
    pub ok: bool,
    /// What is left, when something is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<String>,
    /// The exact command that removes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

/// The prefix of a check line in a job's output (the dialog reads them).
pub const CHECK_PREFIX: &str = "UNINSTALL-CHECK\t";

impl Check {
    pub fn encode(&self) -> String {
        format!("{CHECK_PREFIX}{}", serde_json::to_string(self).unwrap_or_default())
    }
    pub fn decode(line: &str) -> Option<Check> {
        serde_json::from_str(line.strip_prefix(CHECK_PREFIX)?).ok()
    }
}

/// How the check runs a program: (program, args) → (exit 0, stdout).
pub type Runner<'a> = &'a dyn Fn(&str, &[&str]) -> Option<(bool, String)>;
pub type Exists<'a> = &'a dyn Fn(&Path) -> bool;

/// The real runner and file check.
pub fn system_runner(program: &str, args: &[&str]) -> Option<(bool, String)> {
    crate::sys::output(program, args).map(|(ok, out, _)| (ok, crate::sys::decode(&out)))
}

/// Ports something listens on, from `ss -Htln` (Linux) or `netstat -ano -p tcp` (Windows).
pub fn listening_ports(text: &str, windows: bool) -> Vec<u16> {
    let mut ports: Vec<u16> = text
        .lines()
        .filter_map(|l| {
            let cols: Vec<&str> = l.split_whitespace().collect();
            let local = if windows {
                // TCP  127.0.0.1:5432  0.0.0.0:0  LISTENING  1234
                if cols.len() < 4 || !cols[3].eq_ignore_ascii_case("LISTENING") {
                    return None;
                }
                cols[1]
            } else {
                // LISTEN 0 244 127.0.0.1:5432 0.0.0.0:*
                if cols.first() != Some(&"LISTEN") || cols.len() < 4 {
                    return None;
                }
                cols[3]
            };
            local.rsplit(':').next()?.parse().ok()
        })
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// Process names running, from `ps -eo comm=` (Linux) or `tasklist /fo csv /nh` (Windows).
pub fn process_names(text: &str, windows: bool) -> Vec<String> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() {
                return None;
            }
            if windows {
                // "postgres.exe","1234","Services","0","12,345 K"
                let name = l.split(',').next()?.trim_matches('"');
                Some(name.trim_end_matches(".exe").trim_end_matches(".EXE").to_lowercase())
            } else {
                Some(l.to_string())
            }
        })
        .collect()
}

/// Unit files listed by `systemctl list-unit-files --no-legend --plain <units>`.
pub fn listed_units(text: &str) -> Vec<String> {
    text.lines().filter_map(|l| l.split_whitespace().next()).filter(|u| u.contains('.')).map(str::to_string).collect()
}

/// Names from `docker ps -a --format {{.Names}}`, `docker images --format
/// {{.Repository}}:{{.Tag}}` or `docker volume ls --format {{.Name}}`.
pub fn docker_names(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect()
}

/// Distributions from `wsl.exe -l -q` (already decoded from UTF-16).
pub fn wsl_distros(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}' || c == '\0'))
        .filter(|l| !l.is_empty() && !l.starts_with("Windows Subsystem for Linux"))
        .map(str::to_string)
        .collect()
}

/// The WSL feature is off, or goes off at the restart, from `dism /online
/// /get-featureinfo /featurename:Microsoft-Windows-Subsystem-Linux`.
pub fn feature_off(text: &str) -> bool {
    text.lines().any(|l| {
        let l = l.trim();
        let Some((k, v)) = l.split_once(':') else { return false };
        k.trim().eq_ignore_ascii_case("State") && matches!(v.trim(), "Disabled" | "Disable Pending" | "Disabled with Payload Removed")
    })
}

/// A Linux package is installed, from its query's answer.
fn package_installed(pm: Pm, ok: bool, out: &str) -> bool {
    match pm {
        Pm::Apt => ok && out.contains("ok installed"),
        _ => ok,
    }
}

/// Check that nothing of `expect` is left.
pub fn verify(expect: &Expect, run: Runner, exists: Exists) -> Vec<Check> {
    let w = expect.windows;
    let mut checks = Vec::new();
    let sudo = if w { "" } else { "sudo " };

    if !expect.processes.is_empty() {
        let running = if w { run("tasklist", &["/fo", "csv", "/nh"]) } else { run("ps", &["-eo", "comm="]) };
        let names = running.map(|(_, t)| process_names(&t, w)).unwrap_or_default();
        for (what, procs) in &expect.processes {
            let left: Vec<&String> = procs.iter().filter(|p| names.iter().any(|n| n == &p.to_lowercase() || n == *p)).collect();
            checks.push(if left.is_empty() {
                Check { what: format!("No {what} process running"), ok: true, left: None, fix: None }
            } else {
                let fix = if w {
                    left.iter().map(|p| format!("taskkill /F /IM {p}.exe")).collect::<Vec<_>>().join(" & ")
                } else {
                    left.iter().map(|p| format!("sudo pkill -9 -x {p}")).collect::<Vec<_>>().join("; ")
                };
                Check { what: format!("{what} is still running"), ok: false, left: Some(left.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")), fix: Some(fix) }
            });
        }
    }

    if !expect.ports.is_empty() {
        let out = if w { run("netstat", &["-ano", "-p", "tcp"]) } else { run("ss", &["-Htln"]) };
        let open = out.map(|(_, t)| listening_ports(&t, w)).unwrap_or_default();
        for (what, port) in &expect.ports {
            checks.push(if open.contains(port) {
                let fix = if w { format!("netstat -ano -p tcp | findstr :{port}") } else { format!("sudo ss -tlnp 'sport = :{port}'") };
                Check { what: format!("Port {port} ({what}) is still in use"), ok: false, left: Some(format!("something listens on {port}")), fix: Some(fix) }
            } else {
                Check { what: format!("Port {port} ({what}) is free"), ok: true, left: None, fix: None }
            });
        }
    }

    for unit in &expect.units {
        let present = if w {
            // `sc query` fails with 1060 for a service that does not exist.
            run("sc", &["query", unit]).is_some_and(|(ok, t)| ok || !t.contains("1060"))
        } else {
            run("systemctl", &["list-unit-files", "--no-legend", "--plain", unit]).is_some_and(|(_, t)| listed_units(&t).iter().any(|u| u == unit))
        };
        checks.push(if present {
            let fix = if w { format!("sc stop {unit} & sc delete {unit}") } else { format!("sudo systemctl disable --now {unit} && sudo rm -f /etc/systemd/system/{unit} && sudo systemctl daemon-reload") };
            Check { what: format!("The {unit} service is still registered"), ok: false, left: Some(unit.clone()), fix: Some(fix) }
        } else {
            Check { what: format!("No {unit} service"), ok: true, left: None, fix: None }
        });
    }

    if let Some(pm) = expect.pm {
        let installed: Vec<&str> = expect
            .packages
            .iter()
            .filter(|p| {
                let (prog, args) = pm.query(p);
                let a: Vec<&str> = args.iter().map(String::as_str).collect();
                run(&prog, &a).is_some_and(|(ok, out)| package_installed(pm, ok, &out))
            })
            .map(String::as_str)
            .collect();
        if !expect.packages.is_empty() {
            checks.push(if installed.is_empty() {
                Check { what: format!("Package {} removed", expect.packages[0]), ok: true, left: None, fix: None }
            } else {
                Check { what: "A package is still installed".into(), ok: false, left: Some(installed.join(", ")), fix: Some(pm.remove_command(&installed, false)) }
            });
        }
    }

    if !expect.containers.is_empty() || !expect.images.is_empty() || !expect.volumes.is_empty() {
        let list = |args: &[&str]| run("docker", args).map(|(_, t)| docker_names(&t)).unwrap_or_default();
        if !expect.containers.is_empty() {
            let all = list(&["ps", "-a", "--format", "{{.Names}}"]);
            for c in &expect.containers {
                checks.push(if all.contains(c) {
                    Check { what: format!("The container {c} is still there"), ok: false, left: Some(c.clone()), fix: Some(format!("docker rm -f {c}")) }
                } else {
                    Check { what: format!("No {c} container"), ok: true, left: None, fix: None }
                });
            }
        }
        if !expect.images.is_empty() {
            let all = list(&["images", "--format", "{{.Repository}}:{{.Tag}}"]);
            for i in &expect.images {
                checks.push(if all.contains(i) {
                    Check { what: format!("The image {i} is still there"), ok: false, left: Some(i.clone()), fix: Some(format!("docker rmi {i}")) }
                } else {
                    Check { what: format!("No {i} image"), ok: true, left: None, fix: None }
                });
            }
        }
        if !expect.volumes.is_empty() {
            let all = list(&["volume", "ls", "--format", "{{.Name}}"]);
            for v in &expect.volumes {
                checks.push(if all.contains(v) {
                    Check { what: format!("The volume {v} is still there"), ok: false, left: Some(v.clone()), fix: Some(format!("docker volume rm {v}")) }
                } else {
                    Check { what: format!("No {v} volume"), ok: true, left: None, fix: None }
                });
            }
        }
    }

    for p in &expect.paths {
        checks.push(if exists(p) {
            let fix = if w { format!("rmdir /s /q \"{}\"", p.display()) } else { format!("{sudo}rm -rf {}", shq(&p.display().to_string())) };
            Check { what: format!("{} is still there", p.display()), ok: false, left: Some(p.display().to_string()), fix: Some(fix) }
        } else {
            Check { what: format!("{} removed", p.display()), ok: true, left: None, fix: None }
        });
    }

    if !expect.wsl_distros.is_empty() {
        let have = run("wsl", &["-l", "-q"]).map(|(_, t)| wsl_distros(&t)).unwrap_or_default();
        for d in &expect.wsl_distros {
            checks.push(if have.iter().any(|h| h.eq_ignore_ascii_case(d)) {
                Check { what: format!("The WSL distribution {d} is still registered"), ok: false, left: Some(d.clone()), fix: Some(format!("wsl --unregister {d}")) }
            } else {
                Check { what: format!("No {d} WSL distribution"), ok: true, left: None, fix: None }
            });
        }
    }

    if expect.wsl_feature_off {
        let off = run("dism", &["/online", "/get-featureinfo", "/featurename:Microsoft-Windows-Subsystem-Linux"]).is_some_and(|(_, t)| feature_off(&t));
        checks.push(if off {
            Check { what: "WSL is turned off (finished at the next restart)".into(), ok: true, left: None, fix: None }
        } else {
            Check {
                what: "WSL is still turned on".into(),
                ok: false,
                left: Some("Microsoft-Windows-Subsystem-Linux".into()),
                fix: Some("wsl --uninstall & dism /online /disable-feature /featurename:Microsoft-Windows-Subsystem-Linux /norestart".into()),
            }
        });
    }
    checks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_removal_stops_kills_removes_and_keeps_data_when_asked() {
        let s = linux_postgres_script(Pm::Pacman, DbRemoval::Remove { keep_data: true }, 5432, "sentient", "sentient", false);
        let stop = s.find("systemctl disable --now postgresql.service").unwrap();
        let kill = s.find("pkill -9 -u postgres").unwrap();
        let remove = s.find("pacman -Rns --noconfirm $PKGS").unwrap();
        assert!(stop < kill && kill < remove, "{s}");
        assert!(s.contains("pacman -Qq postgresql postgresql-old-upgrade"));
        assert!(!s.contains("rm -rf '/var/lib/postgres'"), "data kept: {s}");
        let gone = linux_postgres_script(Pm::Pacman, DbRemoval::Remove { keep_data: false }, 5432, "sentient", "sentient", false);
        assert!(gone.contains("rm -rf '/var/lib/postgres'"));
        assert!(gone.contains("SHOW data_directory"));
        let deb = linux_postgres_script(Pm::Apt, DbRemoval::Remove { keep_data: false }, 5433, "s", "s", false);
        assert!(deb.contains("apt-get purge -y -qq --auto-remove $PKGS") && deb.contains("rm -rf '/etc/postgresql'"));
        let deb_keep = linux_postgres_script(Pm::Apt, DbRemoval::Remove { keep_data: true }, 5433, "s", "s", false);
        assert!(deb_keep.contains("apt-get remove") && !deb_keep.contains("purge"));
    }

    #[test]
    fn a_postgres_from_before_is_only_stopped_being_used() {
        let s = linux_postgres_script(Pm::Pacman, DbRemoval::StopUsing { keep_data: false }, 5432, "sentient", "st", false);
        assert!(s.contains("DROP DATABASE IF EXISTS \"sentient\" WITH (FORCE);") && s.contains("DROP USER IF EXISTS \"st\";"));
        assert!(!s.contains("systemctl") && !s.contains("pacman") && !s.contains("rm -rf"), "{s}");
        let keep = linux_postgres_script(Pm::Pacman, DbRemoval::StopUsing { keep_data: true }, 5432, "sentient", "st", false);
        assert!(!keep.contains("DROP") && !keep.contains("systemctl"));
    }

    #[test]
    fn clickhouse_removal_takes_the_binary_links_and_unit() {
        let s = linux_clickhouse_script(DbRemoval::Remove { keep_data: true }, 9000, "sentient", "sentient");
        assert!(s.contains("systemctl disable --now clickhouse-server.service"));
        assert!(s.contains("[ -L /usr/bin/clickhouse-server ] && rm -f /usr/bin/clickhouse-server"));
        assert!(s.contains("rm -f /usr/bin/clickhouse\n"));
        assert!(s.contains("rm -f /etc/systemd/system/clickhouse-server.service"));
        assert!(!s.contains("rm -rf /var/lib/clickhouse"));
        assert!(linux_clickhouse_script(DbRemoval::Remove { keep_data: false }, 9000, "s", "s").contains("rm -rf /var/lib/clickhouse"));
        let only = linux_clickhouse_script(DbRemoval::StopUsing { keep_data: false }, 9001, "sentient", "sentient");
        assert!(only.contains("--port 9001") && only.contains("DROP DATABASE IF EXISTS sentient") && !only.contains("systemctl"));
    }

    #[test]
    fn scripts_are_valid_shell() {
        for pm in [Pm::Pacman, Pm::Apt, Pm::Dnf, Pm::Zypper] {
            for keep in [true, false] {
                for s in [
                    linux_postgres_script(pm, DbRemoval::Remove { keep_data: keep }, 5432, "sentient", "sentient", false),
                    linux_postgres_script(pm, DbRemoval::StopUsing { keep_data: keep }, 5432, "sentient", "sentient", false),
                    linux_clickhouse_script(DbRemoval::Remove { keep_data: keep }, 9000, "sentient", "sentient"),
                ] {
                    if cfg!(unix) {
                        let ok = std::process::Command::new("bash").arg("-n").arg("-c").arg(&s).status().unwrap();
                        assert!(ok.success(), "{s}");
                    }
                }
            }
        }
    }

    #[test]
    fn docker_removes_containers_images_and_only_unkept_volumes() {
        let names = crate::distro::DeployConfig::default().names();
        let all = DockerParts { sentient: true, postgres: true, clickhouse: true, keep_pg_data: true, keep_ch_data: false, keep_files: true, has_clickhouse: true };
        let s = docker_script(&names, &all, &["/opt/sentient/docker-compose.yml".into()]);
        assert!(s.contains("docker rm -f 'sentient-postgres'") && s.contains("docker rmi \"$IMG\""));
        assert!(!s.contains("volume rm 'st-pg-data'") && !s.contains("volume rm 'st-data'"));
        assert!(s.contains("volume rm 'st-ch-data'"));
        assert!(s.contains("rm -f '/opt/sentient/docker-compose.yml'"));
        let only_app = DockerParts { sentient: true, ..Default::default() };
        let s = docker_script(&names, &only_app, &["/x.yml".into()]);
        assert!(s.contains("docker rm -f 'sentient'") && !s.contains("sentient-postgres") && !s.contains("/x.yml"));
        if cfg!(unix) {
            assert!(std::process::Command::new("bash").arg("-n").arg("-c").arg(docker_script(&names, &all, &[])).status().unwrap().success());
        }
    }

    #[test]
    fn ss_and_netstat_give_the_listening_ports() {
        let ss = "LISTEN 0      244        127.0.0.1:5432       0.0.0.0:*\nLISTEN 0 4096 [::]:8123 [::]:*\nLISTEN 0 4096 *:9000 *:*\n";
        assert_eq!(listening_ports(ss, false), vec![5432, 8123, 9000]);
        let ns = "\nActive Connections\n\n  Proto  Local Address          Foreign Address        State           PID\n  TCP    0.0.0.0:135            0.0.0.0:0              LISTENING       1000\n  TCP    127.0.0.1:5432         0.0.0.0:0              LISTENING       4242\n  TCP    10.0.0.2:50000         1.2.3.4:443            ESTABLISHED     7\n";
        assert_eq!(listening_ports(ns, true), vec![135, 5432]);
    }

    #[test]
    fn process_unit_docker_and_wsl_lists_parse() {
        assert_eq!(process_names("systemd\npostgres\nclickhouse-serv\n", false), ["systemd", "postgres", "clickhouse-serv"]);
        assert_eq!(process_names("\"postgres.exe\",\"4242\",\"Services\",\"0\",\"9,000 K\"\n\"wslservice.exe\",\"1\",\"Services\",\"0\",\"1 K\"", true), ["postgres", "wslservice"]);
        assert_eq!(listed_units("postgresql.service disabled disabled\n"), ["postgresql.service"]);
        assert!(listed_units("0 unit files listed.\n").is_empty());
        assert_eq!(docker_names("sentient\n\nsentient-postgres\n"), ["sentient", "sentient-postgres"]);
        assert_eq!(wsl_distros("\u{feff}Ubuntu\r\ndocker-desktop\r\n\r\n"), ["Ubuntu", "docker-desktop"]);
        assert!(feature_off("Feature Name : Microsoft-Windows-Subsystem-Linux\nState : Disable Pending\n"));
        assert!(!feature_off("State : Enabled\n"));
    }

    fn fake(answers: Vec<(&'static str, (bool, &'static str))>) -> impl Fn(&str, &[&str]) -> Option<(bool, String)> {
        move |prog: &str, args: &[&str]| {
            let key = format!("{prog} {}", args.join(" "));
            answers.iter().find(|(k, _)| key.starts_with(k)).map(|(_, (ok, out))| (*ok, out.to_string())).or(Some((false, String::new())))
        }
    }

    #[test]
    fn verify_reports_what_is_left_with_the_command_to_remove_it() {
        let expect = Expect {
            processes: vec![("PostgreSQL".into(), vec!["postgres".into()]), ("ClickHouse".into(), vec!["clickhouse-serv".into()])],
            ports: vec![("PostgreSQL".into(), 5432), ("ClickHouse".into(), 8123)],
            units: vec!["postgresql.service".into(), "clickhouse-server.service".into()],
            packages: vec!["postgresql".into()],
            pm: Some(Pm::Pacman),
            paths: vec![PathBuf::from("/var/lib/postgres"), PathBuf::from("/var/lib/clickhouse")],
            ..Default::default()
        };
        let run = fake(vec![
            ("ps -eo", (true, "systemd\npostgres\nbash\n")),
            ("ss -Htln", (true, "LISTEN 0 244 127.0.0.1:5432 0.0.0.0:*\n")),
            ("systemctl list-unit-files --no-legend --plain postgresql.service", (true, "postgresql.service disabled disabled\n")),
            ("systemctl list-unit-files --no-legend --plain clickhouse-server.service", (true, "")),
            ("pacman -Q postgresql", (true, "postgresql 18.1-1\n")),
        ]);
        let exists = |p: &Path| p == Path::new("/var/lib/postgres");
        let checks = verify(&expect, &run, &exists);
        let bad: Vec<&Check> = checks.iter().filter(|c| !c.ok).collect();
        assert_eq!(bad.len(), 5, "{checks:#?}");
        assert_eq!(bad[0].fix.as_deref(), Some("sudo pkill -9 -x postgres"));
        assert!(bad[1].what.contains("5432") && bad[1].fix.as_deref() == Some("sudo ss -tlnp 'sport = :5432'"));
        assert!(bad[2].fix.as_deref().unwrap().starts_with("sudo systemctl disable --now postgresql.service"));
        assert_eq!(bad[3].fix.as_deref(), Some("sudo pacman -Rns postgresql"));
        assert_eq!(bad[4].fix.as_deref(), Some("sudo rm -rf '/var/lib/postgres'"));
        assert!(checks.iter().any(|c| c.ok && c.what == "Port 8123 (ClickHouse) is free"));
        assert!(checks.iter().any(|c| c.ok && c.what == "No clickhouse-server.service service"));
        // Everything gone: every line is fine.
        let clean = fake(vec![("ps -eo", (true, "systemd\n")), ("ss -Htln", (true, "")), ("pacman -Q", (false, ""))]);
        assert!(verify(&expect, &clean, &|_| false).iter().all(|c| c.ok));
    }

    #[test]
    fn verify_on_windows_and_docker() {
        let expect = Expect {
            windows: true,
            processes: vec![("PostgreSQL".into(), vec!["postgres".into()])],
            ports: vec![("PostgreSQL".into(), 5432)],
            units: vec!["sentient-postgresql".into()],
            wsl_distros: vec!["sentient".into()],
            wsl_feature_off: true,
            ..Default::default()
        };
        let run = fake(vec![
            ("tasklist", (true, "\"explorer.exe\",\"1\",\"Console\",\"1\",\"1 K\"\n")),
            ("netstat", (true, "  TCP    127.0.0.1:5432   0.0.0.0:0   LISTENING   9\n")),
            ("sc query", (false, "[SC] EnumQueryServicesStatus:OpenService FAILED 1060:\n")),
            ("wsl -l -q", (true, "Ubuntu\r\n")),
            ("dism", (true, "State : Enabled\n")),
        ]);
        let checks = verify(&expect, &run, &|_| false);
        let bad: Vec<&str> = checks.iter().filter(|c| !c.ok).map(|c| c.what.as_str()).collect();
        assert_eq!(bad, ["Port 5432 (PostgreSQL) is still in use", "WSL is still turned on"]);
        let docker = Expect { containers: vec!["sentient-postgres".into()], images: vec!["postgres:18-alpine".into()], volumes: vec!["st-pg-data".into()], ..Default::default() };
        let run = fake(vec![("docker ps", (true, "sentient-postgres\n")), ("docker images", (true, "quakestring/sentient:latest\n")), ("docker volume", (true, "st-pg-data\n"))]);
        let checks = verify(&docker, &run, &|_| false);
        assert_eq!(checks.iter().filter(|c| !c.ok).filter_map(|c| c.fix.clone()).collect::<Vec<_>>(), ["docker rm -f sentient-postgres", "docker volume rm st-pg-data"]);
        let line = checks[0].encode();
        assert_eq!(Check::decode(&line).unwrap(), checks[0]);
    }
}
