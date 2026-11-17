// Copyright © 2016-2026 The SENTIENT Authors

//! What systemd and Docker say about the services in an install's way, as
//! the System check reads them: the PostgreSQL units (installed, or left
//! over from a removed package and still running) and who holds a busy
//! port. The parsing is plain functions over the commands' text, so the
//! tests feed it what the commands print.

/// One unit, from `systemctl show -p Id,LoadState,ActiveState,FragmentPath,MainPID`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnitInfo {
    pub id: String,
    /// "loaded", "not-found" (its file is gone), "masked", …
    pub load_state: String,
    /// "active", "inactive", "failed", …
    pub active_state: String,
    /// Its unit file; empty when there is none.
    pub fragment_path: String,
    /// 0 when it runs nothing.
    pub main_pid: u32,
}

impl UnitInfo {
    /// Installed: systemd has its unit file.
    pub fn loaded(&self) -> bool {
        self.load_state == "loaded"
    }

    pub fn running(&self) -> bool {
        matches!(self.active_state.as_str(), "active" | "activating" | "reloading" | "deactivating")
    }

    /// Its file is gone (the package was removed), yet systemd still runs it.
    pub fn leftover_running(&self) -> bool {
        self.load_state == "not-found" && self.running()
    }
}

/// `systemctl show -p … a b …`: one block per unit, a blank line between.
pub fn parse_show(text: &str) -> Vec<UnitInfo> {
    let mut out = Vec::new();
    let mut cur = UnitInfo::default();
    let mut any = false;
    for line in text.lines().map(str::trim).chain(std::iter::once("")) {
        if line.is_empty() {
            if any {
                out.push(std::mem::take(&mut cur));
                any = false;
            }
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        any = true;
        match k {
            "Id" => cur.id = v.to_string(),
            "LoadState" => cur.load_state = v.to_string(),
            "ActiveState" => cur.active_state = v.to_string(),
            "FragmentPath" => cur.fragment_path = v.to_string(),
            "MainPID" => cur.main_pid = v.parse().unwrap_or(0),
            _ => {}
        }
    }
    out
}

/// The unit names of `systemctl list-units --plain --no-legend` or
/// `list-unit-files --no-legend`: the first word ending in ".service" on
/// each line. Without `--plain`, systemd puts a "●" before a unit that is
/// not found or failed; that marker is never a name.
pub fn parse_unit_names(text: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for line in text.lines() {
        if let Some(name) = line.split_whitespace().find(|t| t.ends_with(".service") && t.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())) {
            if !v.iter().any(|n| n == name) {
                v.push(name.to_string());
            }
        }
    }
    v
}

/// What a PostgreSQL service on this computer is, for the System check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PgService {
    /// No PostgreSQL unit is installed and none is running.
    None,
    /// An installed (loaded) unit; the running one first when several.
    Installed(UnitInfo),
    /// A unit whose package was removed and whose server still runs.
    LeftoverRunning(UnitInfo),
}

/// Pick the unit that counts. A not-found unit (its package removed) does
/// not count as installed; it matters only while its server still runs.
/// Template units (`postgresql@.service`) are never the service itself.
pub fn classify_postgres(units: &[UnitInfo]) -> PgService {
    let real = |u: &&UnitInfo| !u.id.ends_with("@.service");
    let loaded: Vec<&UnitInfo> = units.iter().filter(real).filter(|u| u.loaded()).collect();
    if let Some(u) = loaded.iter().find(|u| u.running() && u.main_pid > 0).or_else(|| loaded.iter().find(|u| u.running())).or_else(|| loaded.first()) {
        return PgService::Installed((*u).clone());
    }
    match units.iter().filter(real).find(|u| u.leftover_running()) {
        Some(u) => PgService::LeftoverRunning(u.clone()),
        None => PgService::None,
    }
}

/// The systemd service a process runs in, from its `/proc/<pid>/cgroup`
/// ("0::/system.slice/postgresql.service").
pub fn unit_of_cgroup(text: &str) -> Option<String> {
    text.lines()
        .filter_map(|l| l.splitn(3, ':').nth(2))
        .flat_map(|path| path.rsplit('/').find(|c| c.ends_with(".service")).map(str::to_string))
        .next()
}

/// One `ss -ltnpH` line: the listening process and its pid
/// (`users:(("postgres",pid=812,fd=6))`). None when ss may not show it (a
/// process of another user, read without administrator rights).
pub fn parse_ss(line: &str) -> Option<(String, u32)> {
    let name = line.split("((\"").nth(1)?.split('"').next()?.to_string();
    let pid = line.split("pid=").nth(1)?.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()?;
    Some((name, pid))
}

/// `docker ps --format '{{.Names}}|{{.Ports}}'`: the container that
/// publishes host `port` ("0.0.0.0:5432->5432/tcp, [::]:5432->5432/tcp").
pub fn container_on(text: &str, port: u16) -> Option<String> {
    text.lines().find_map(|line| {
        let (name, ports) = line.split_once('|')?;
        ports
            .split(',')
            .filter_map(|p| p.trim().split_once("->").map(|(host, _)| host))
            .any(|host| host.rsplit(':').next().and_then(|p| p.split('-').next()).and_then(|p| p.parse::<u16>().ok()) == Some(port)
                || host.rsplit(':').next().and_then(|r| r.split_once('-')).is_some_and(|(a, b)| a.parse::<u16>().is_ok_and(|a| a <= port) && b.parse::<u16>().is_ok_and(|b| port <= b)))
            .then(|| name.trim().to_string())
    })
}

/// `dpkg -S <path>`: "postgresql-common: /lib/systemd/system/postgresql.service".
pub fn parse_dpkg_s(text: &str) -> Option<String> {
    let pkgs = text.lines().find_map(|l| l.split_once(": ").filter(|(_, path)| path.starts_with('/')).map(|(p, _)| p))?;
    let first = pkgs.split(',').next()?.trim();
    // "postgresql-16:amd64" → "postgresql-16"
    Some(first.split(':').next().unwrap_or(first).to_string()).filter(|p| !p.is_empty() && !p.contains(' '))
}

#[cfg(unix)]
fn out(program: &str, args: &[&str]) -> Option<String> {
    let o = std::process::Command::new(program).args(args).env("LC_ALL", "C").output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// `systemctl show` for these units.
#[cfg(unix)]
pub fn show(units: &[String]) -> Vec<UnitInfo> {
    if units.is_empty() {
        return Vec::new();
    }
    let mut args = vec!["show", "-p", "Id,LoadState,ActiveState,FragmentPath,MainPID"];
    args.extend(units.iter().map(String::as_str));
    out("systemctl", &args).map(|t| parse_show(&t)).unwrap_or_default()
}

#[cfg(not(unix))]
pub fn show(_units: &[String]) -> Vec<UnitInfo> {
    Vec::new()
}

/// Every PostgreSQL unit systemd knows: the ones it has loaded (a removed
/// package's still-running one among them) and the installed unit files.
#[cfg(unix)]
pub fn postgres_units() -> Vec<UnitInfo> {
    let mut names = parse_unit_names(&out("systemctl", &["list-units", "--type=service", "--all", "--plain", "--no-legend", "postgresql*"]).unwrap_or_default());
    for n in parse_unit_names(&out("systemctl", &["list-unit-files", "--type=service", "--no-legend", "postgresql*"]).unwrap_or_default()) {
        if !names.contains(&n) {
            names.push(n);
        }
    }
    names.retain(|n| !n.ends_with("@.service"));
    show(&names)
}

#[cfg(not(unix))]
pub fn postgres_units() -> Vec<UnitInfo> {
    Vec::new()
}

/// The package that installed `path`, asked of the package manager.
#[cfg(unix)]
pub fn package_owning(path: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    let pm = crate::diagnose::package_manager_here()?;
    let name = match pm {
        "pacman" => out("pacman", &["-Qqo", path]),
        "apt-get" => out("dpkg", &["-S", path]).and_then(|t| parse_dpkg_s(&t)),
        _ => out("rpm", &["-qf", "--qf", "%{NAME}", path]),
    }?;
    let name = name.lines().next()?.trim().to_string();
    (!name.is_empty() && !name.contains(' ')).then_some(name)
}

#[cfg(not(unix))]
pub fn package_owning(_path: &str) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(id: &str, load: &str, active: &str, path: &str, pid: u32) -> UnitInfo {
        UnitInfo { id: id.into(), load_state: load.into(), active_state: active.into(), fragment_path: path.into(), main_pid: pid }
    }

    #[test]
    fn the_not_found_marker_is_never_a_unit_name() {
        // What `systemctl list-units --all` prints after `pacman -R postgresql`.
        let text = "● postgresql.service not-found inactive dead postgresql.service\n";
        assert_eq!(parse_unit_names(text), vec!["postgresql.service"]);
        let plain = "postgresql.service loaded active running PostgreSQL database server\npostgresql@16-main.service loaded active running PostgreSQL Cluster 16-main\n";
        assert_eq!(parse_unit_names(plain), vec!["postgresql.service", "postgresql@16-main.service"]);
        assert!(parse_unit_names("").is_empty());
    }

    #[test]
    fn show_blocks_are_read() {
        let text = "Id=postgresql.service\nLoadState=not-found\nActiveState=inactive\nFragmentPath=\nMainPID=0\n\n\
                    Id=docker.service\nLoadState=loaded\nActiveState=active\nFragmentPath=/usr/lib/systemd/system/docker.service\nMainPID=2441\n";
        let v = parse_show(text);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0], unit("postgresql.service", "not-found", "inactive", "", 0));
        assert_eq!(v[1].main_pid, 2441);
        assert!(v[1].loaded() && v[1].running());
    }

    #[test]
    fn a_removed_unit_does_not_count_and_a_loaded_one_does() {
        // Removed and stopped: nothing is there.
        let gone = [unit("postgresql.service", "not-found", "inactive", "", 0)];
        assert_eq!(classify_postgres(&gone), PgService::None);
        // Removed but its server still runs: a leftover, not an install.
        let left = [unit("postgresql.service", "not-found", "active", "", 812)];
        assert!(matches!(classify_postgres(&left), PgService::LeftoverRunning(u) if u.id == "postgresql.service" && u.main_pid == 812));
        // Installed: counts, and is named.
        let inst = [
            unit("postgresql@.service", "loaded", "inactive", "/lib/systemd/system/postgresql@.service", 0),
            unit("postgresql.service", "loaded", "active", "/lib/systemd/system/postgresql.service", 0),
            unit("postgresql@16-main.service", "loaded", "active", "/lib/systemd/system/postgresql@.service", 913),
        ];
        assert!(matches!(classify_postgres(&inst), PgService::Installed(u) if u.id == "postgresql@16-main.service"));
        let stopped = [unit("postgresql.service", "loaded", "inactive", "/usr/lib/systemd/system/postgresql.service", 0)];
        assert!(matches!(classify_postgres(&stopped), PgService::Installed(u) if u.id == "postgresql.service"));
    }

    #[test]
    fn process_units_ss_and_docker_ports_are_read() {
        assert_eq!(unit_of_cgroup("0::/system.slice/postgresql.service\n").as_deref(), Some("postgresql.service"));
        assert_eq!(unit_of_cgroup("0::/user.slice/user-1000.slice/user@1000.service/app.slice/x.scope").as_deref(), Some("user@1000.service"));
        assert_eq!(unit_of_cgroup("0::/init.scope"), None);
        let ss = "LISTEN 0 244 127.0.0.1:5432 0.0.0.0:* users:((\"postgres\",pid=812,fd=6))";
        assert_eq!(parse_ss(ss), Some(("postgres".into(), 812)));
        assert_eq!(parse_ss("LISTEN 0 244 127.0.0.1:5432 0.0.0.0:*"), None);
        let ps = "spm-e2e|127.0.0.1:2242->22/tcp\nsentient-postgres|0.0.0.0:5432->5432/tcp, [::]:5432->5432/tcp\nrange|0.0.0.0:9000-9010->9000-9010/tcp\n";
        assert_eq!(container_on(ps, 5432).as_deref(), Some("sentient-postgres"));
        assert_eq!(container_on(ps, 2242).as_deref(), Some("spm-e2e"));
        assert_eq!(container_on(ps, 9005).as_deref(), Some("range"));
        assert_eq!(container_on(ps, 22), None);
        assert_eq!(parse_dpkg_s("postgresql-common: /lib/systemd/system/postgresql.service\n").as_deref(), Some("postgresql-common"));
        assert_eq!(parse_dpkg_s("dpkg-query: no path found matching pattern /x\n"), None);
    }
}
