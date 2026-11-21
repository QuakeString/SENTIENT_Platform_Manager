// Copyright © 2016-2026 The SENTIENT Authors

//! A Sentient Gateway installed on Linux without Docker: a systemd service
//! (from a package or by hand), or a plain process started some other way.
//! crate::gateway::status asks Docker first, then this.
//!
//! A service is shown, started, stopped and restarted with systemctl, and its
//! log is its journal. A plain process is shown (running, version) but not
//! controlled: nothing would start it again.

use std::path::Path;

use crate::gateway::Status;
use crate::progress::{Progress, ProgressFn};

/// The unit the gateway's packages install (packaging/linux in the gateway
/// repository) and the name of its program.
pub const UNIT: &str = "sentient-gateway.service";
pub const PROGRAM: &str = "sentient-gateway";
/// The gateway's package (deb, rpm and Arch).
const PACKAGE: &str = "sentient-gateway";

/// Config files a native gateway reads (the platform host and port).
const FILES: [&str; 4] = [
    "/etc/sentient-gateway/config/st_gateway.json",
    "/etc/sentient-gateway/st_gateway.json",
    "/opt/sentient-gateway/config/st_gateway.json",
    "/var/lib/sentient-gateway/config/st_gateway.json",
];

/// Where the gateway's program is when it is installed without Docker: the
/// package's, then by hand.
const PROGRAMS: [&str; 4] = [
    "/usr/bin/sentient-gateway",
    "/usr/local/bin/sentient-gateway",
    "/opt/sentient-gateway/bin/sentient-gateway",
    "/opt/sentient-gateway/sentient-gateway",
];

/// Folders holding unit files, for units installed by hand under another name.
const UNIT_DIRS: [&str; 4] = ["/etc/systemd/system", "/usr/local/lib/systemd/system", "/usr/lib/systemd/system", "/lib/systemd/system"];

fn out(cmd: &str, args: &[&str]) -> Option<String> {
    let o = crate::sys::host_command(cmd).args(args).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

/// A path whose file name is the gateway's program. Only the file name
/// counts, so systemd-journal-gatewayd and the like never are.
pub fn is_our_program(path: &str) -> bool {
    crate::gateway::is_gateway_program(path)
}

/// The program a unit runs, from `systemctl show -p ExecStart --value`
/// (`{ path=/usr/bin/x ; argv[]=... }`, or a bare command line).
pub fn exec_program(exec_start: &str) -> Option<String> {
    exec_start
        .split(" ; ")
        .find_map(|p| p.trim().trim_start_matches('{').trim().strip_prefix("path="))
        .map(|p| p.trim().to_string())
        .or_else(|| exec_start.split_whitespace().next().map(str::to_string))
        .filter(|p| !p.is_empty())
}

/// Units worth a closer look in a `list-unit-files` listing: any service
/// whose name hints at a gateway. A hint only; a unit is the gateway's when
/// it runs the gateway's program (our_unit).
fn candidates(list: &str) -> Vec<String> {
    list.lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|u| u.ends_with(".service"))
        .filter(|u| {
            let u = u.to_ascii_lowercase();
            u.contains("gateway") || u.starts_with("st-gw") || u.starts_with("st_gw")
        })
        .map(str::to_string)
        .collect()
}

/// Whether `unit` is the Sentient Gateway: it runs our program (from its
/// ExecStart), and either it is the package's unit or the program answers
/// `--version` as the gateway does. Gives the program and its version.
pub fn our_unit(unit: &str, exec_start: &str, version_out: Option<&str>) -> Option<(String, Option<String>)> {
    let program = exec_program(exec_start)?;
    if !is_our_program(&program) {
        return None;
    }
    let version = version_out.and_then(crate::gateway::parse_version);
    (unit == UNIT || version.is_some()).then_some((program, version))
}

/// A unit found: its name, program and version.
#[derive(Debug, Clone)]
pub struct Found {
    pub unit: String,
    pub program: String,
    pub version: Option<String>,
}

fn version_out(program: &str) -> Option<String> {
    Path::new(program).is_file().then(|| out(program, &["--version"])).flatten()
}

/// The gateway's systemd unit: the package's, or one written by hand that
/// runs the gateway's program.
pub fn found_unit() -> Option<Found> {
    let mut units = out("systemctl", &["list-unit-files", "--type=service", "--no-legend", "--no-pager"]).map(|l| candidates(&l)).unwrap_or_default();
    // Unit files that name the program, whatever the unit is called.
    for dir in UNIT_DIRS {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(".service") && !units.contains(&name) && std::fs::read_to_string(e.path()).is_ok_and(|t| t.contains(PROGRAM)) {
                units.push(name);
            }
        }
    }
    // The package's unit first.
    units.sort_by_key(|u| u != UNIT);
    units.into_iter().find_map(|unit| {
        let exec = out("systemctl", &["show", "-p", "ExecStart", "--value", &unit])?;
        let program = exec_program(&exec)?;
        if !is_our_program(&program) {
            return None;
        }
        let v = version_out(&program);
        our_unit(&unit, &exec, v.as_deref()).map(|(program, version)| Found { unit, program, version })
    })
}

/// The gateway's systemd unit name (for its journal and its controls).
pub fn unit() -> Option<String> {
    found_unit().map(|f| f.unit)
}

/// A gateway process that no service started: (pid, program). Only a
/// program named like ours, and never the Platform Manager's own jobs.
fn pick_process(ps: &str) -> Option<(String, String)> {
    ps.lines().map(str::trim).find_map(|l| {
        let mut w = l.split_whitespace();
        let pid = w.next()?;
        let program = w.next()?;
        is_our_program(program).then(|| (pid.to_string(), program.to_string()))
    })
}

/// A running gateway no service started: its program and version, when the
/// program answers as the gateway (a gateway inside a container shows in
/// `ps` too, but its program is not on this computer).
fn process() -> Option<(String, String)> {
    let ps = out("ps", &["-eo", "pid=,args="])?;
    let (pid, program) = pick_process(&ps)?;
    let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok().map(|p| p.to_string_lossy().into_owned()).filter(|p| is_our_program(p));
    let path = exe.or_else(|| program.starts_with('/').then_some(program.clone()))?;
    let version = version_out(&path).as_deref().and_then(crate::gateway::parse_version)?;
    Some((path, version))
}

/// The gateway's program installed in one of PROGRAMS (no service, not
/// running), when it answers as the gateway: (path, version).
fn installed_program() -> Option<(String, String)> {
    PROGRAMS.iter().find_map(|p| version_out(p).as_deref().and_then(crate::gateway::parse_version).map(|v| (p.to_string(), v)))
}

/// The package's version, when the program does not say.
fn package_version() -> Option<String> {
    out("dpkg-query", &["-W", "-f=${Version}", PACKAGE])
        .or_else(|| out("rpm", &["-q", "--qf", "%{VERSION}", PACKAGE]))
        .or_else(|| out("pacman", &["-Q", PACKAGE]).map(|o| o.split_whitespace().nth(1).unwrap_or_default().to_string()))
        .map(|v| v.trim().split('-').next().unwrap_or_default().to_string())
        .filter(|v| !v.is_empty() && !v.contains(' '))
}

/// The package a file belongs to, when it is the gateway's package.
fn owner(path: &str) -> Option<String> {
    let name = out("pacman", &["-Qqo", path])
        .or_else(|| out("dpkg-query", &["-S", path]).map(|o| o.split(':').next().unwrap_or_default().to_string()))
        .or_else(|| out("rpm", &["-qf", "--qf", "%{NAME}", path]))?;
    let name = name.trim().to_string();
    (name == PACKAGE).then_some(name)
}

/// The gateway file, read as this process: (its text, whether a settings
/// folder or file was there but closed to this account). The package keeps
/// `/etc/sentient-gateway` 0700 for its own account, so an unprivileged
/// Platform Manager gets the second.
fn read_file() -> (Option<String>, bool) {
    let mut denied = false;
    for f in FILES {
        match std::fs::read_to_string(f) {
            Ok(t) => return (Some(t), false),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => denied = true,
            Err(_) => {}
        }
    }
    // A closed folder hides even whether the file is there.
    for d in DATA_DIRS.iter().take(1) {
        if let Err(e) = std::fs::read_dir(d) {
            denied |= e.kind() == std::io::ErrorKind::PermissionDenied;
        }
    }
    (None, denied)
}

/// `systemctl show -p Environment --value`: `NAME=value NAME2=value2`.
pub fn parse_environment(out: &str) -> Vec<String> {
    out.split_whitespace().filter(|kv| kv.contains('=')).map(|kv| kv.trim_matches('"').to_string()).collect()
}

fn unit_environment(unit: &str) -> Vec<String> {
    out("systemctl", &["show", "-p", "Environment", "--value", unit]).map(|o| parse_environment(&o)).unwrap_or_default()
}

/// Something accepts connections on the page's loopback address.
fn listening(url: &str) -> bool {
    use std::net::ToSocketAddrs as _;
    let Some(hostport) = url.split("://").nth(1).map(|r| r.trim_end_matches('/')) else { return false };
    let Some(addr) = hostport.to_socket_addrs().ok().and_then(|mut a| a.next()) else { return false };
    addr.ip().is_loopback() && std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(300)).is_ok()
}

/// Host, port and the local page of a native gateway, from its file and
/// its service's environment (env wins, as in the gateway). A file closed
/// to this account: the page from the environment and the defaults, kept
/// only when the gateway runs and something listens there (the real
/// state; an administrator's read gives the rest).
pub fn settings(file: Option<&str>, unreadable: bool, env: &[String], running: bool, listens: &dyn Fn(&str) -> bool) -> (Option<String>, Option<u16>, Option<String>) {
    let (host, port) = file.map(crate::gateway::host_of).unwrap_or((None, None));
    let get = |k: &str| env.iter().find_map(|l| l.strip_prefix(&format!("{k}=")).map(str::to_string));
    let page = crate::gateway::page_url(file, &get, false);
    let page = if unreadable && file.is_none() { page.filter(|u| running && listens(u)) } else { page };
    (host, port, page)
}

fn read_settings(env: &[String], running: bool) -> (Option<String>, Option<u16>, Option<String>, bool) {
    let (file, unreadable) = read_file();
    let (h, p, page) = settings(file.as_deref(), unreadable, env, running, &listening);
    (h, p, page, unreadable && file.is_none())
}

pub fn status() -> Status {
    if let Some(f) = found_unit() {
        let running = out("systemctl", &["is-active", &f.unit]).is_some_and(|s| s.trim() == "active");
        let (host, port, page_url, config_unreadable) = read_settings(&unit_environment(&f.unit), running);
        let version = f.version.or_else(|| (f.unit == UNIT).then(package_version).flatten());
        let package = owner(&f.program);
        return Status { installed: true, version, running, host, port, page_url, config_unreadable, service: Some(f.unit), program: Some(f.program), package, ..Status::default() };
    }
    if let Some((program, version)) = process() {
        let (host, port, page_url, config_unreadable) = read_settings(&[], true);
        let package = owner(&program);
        return Status { installed: true, version: Some(version), running: true, host, port, page_url, config_unreadable, process: Some(program.clone()), program: Some(program), package, ..Status::default() };
    }
    // The program installed, neither a service nor running.
    if let Some((program, version)) = installed_program() {
        let (host, port, page_url, config_unreadable) = read_settings(&[], false);
        let package = owner(&program);
        return Status { installed: true, version: Some(version), running: false, host, port, page_url, config_unreadable, process: Some(program.clone()), program: Some(program), package, ..Status::default() };
    }
    Status::default()
}

/// The folders the gateway's package keeps its configuration and its local
/// buffer in (tmpfiles.conf in the gateway repository).
pub const DATA_DIRS: [&str; 2] = ["/etc/sentient-gateway", "/var/lib/sentient-gateway"];

/// The dated folder the configuration and data are kept in on removal.
pub fn kept_dir(now: &str) -> String {
    format!("/var/backups/sentient-gateway-{now}")
}

/// The shell script that removes the gateway found as `s` (status()): run
/// as root. Every step names only the gateway's own unit, program, package
/// and folders, and the unit is checked to run our program again first.
pub fn removal_script(s: &Status, keep_data: bool, now: &str) -> Result<String, String> {
    use crate::exec::shq as q;
    let program = s.program.clone().ok_or("The gateway's program was not found.")?;
    if !is_our_program(&program) {
        return Err(format!("{program} is not the Sentient Gateway's program; nothing was removed."));
    }
    let mut lines = vec!["set -e".to_string()];
    if let Some(unit) = &s.service {
        // ExecStart must still name our program.
        lines.push(format!(
            "exec_start=$(systemctl show -p ExecStart --value {u}); case \"$exec_start\" in *{p}*) ;; *) echo {u} does not run {p}, nothing was removed >&2; exit 3 ;; esac",
            u = q(unit),
            p = q(&program)
        ));
        lines.push(format!("systemctl disable --now {} || systemctl stop {} || true", q(unit), q(unit)));
    } else if s.running {
        return Err("The gateway runs as a plain process that no service started: stop it first, then remove it.".into());
    }
    if keep_data {
        let dir = kept_dir(now);
        lines.push(format!("mkdir -p {}", q(&dir)));
        for d in DATA_DIRS {
            let name = d.trim_start_matches('/').replace('/', "-");
            lines.push(format!("if [ -d {d} ]; then mv {d} {}/{name}; fi", q(&dir)));
        }
    }
    match (&s.package, &s.service) {
        (Some(pkg), _) => {
            let pkg = q(pkg);
            lines.push(format!(
                "if command -v pacman >/dev/null 2>&1; then pacman -R --noconfirm {pkg}; \
                 elif command -v apt-get >/dev/null 2>&1; then DEBIAN_FRONTEND=noninteractive apt-get remove -y {pkg}; \
                 elif command -v dnf >/dev/null 2>&1; then dnf remove -y {pkg}; \
                 else rpm -e {pkg}; fi"
            ));
        }
        (None, Some(unit)) => {
            // Installed by hand: the unit file (only one under systemd's
            // folders) with its drop-ins, then the program.
            lines.push(format!(
                "f=$(systemctl show -p FragmentPath --value {u}); case \"$f\" in /etc/systemd/system/*|/usr/local/lib/systemd/system/*|/usr/lib/systemd/system/*|/lib/systemd/system/*) rm -f \"$f\" ;; esac",
                u = q(unit)
            ));
            lines.push(format!("rm -rf {}", q(&format!("/etc/systemd/system/{unit}.d"))));
            lines.push(format!("rm -f {}", q(&program)));
        }
        (None, None) => lines.push(format!("rm -f {}", q(&program))),
    }
    if !keep_data {
        for d in DATA_DIRS {
            lines.push(format!("rm -rf {d}"));
        }
    }
    if s.service.is_some() {
        lines.push("systemctl daemon-reload || true".into());
        lines.push(format!("systemctl reset-failed {} 2>/dev/null || true", q(s.service.as_deref().unwrap_or_default())));
    }
    Ok(lines.join("\n"))
}

/// Remove the gateway found here (this process is root: the job runs
/// through rootjob, or as the server's agent).
pub fn remove(sink: &ProgressFn, keep_data: bool) -> Result<String, String> {
    let s = status();
    if !s.installed {
        return Err("There is no Sentient Gateway on this computer.".into());
    }
    let now = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let script = removal_script(&s, keep_data, &now)?;
    sink(Progress::Step { name: format!("Removing the Sentient Gateway ({})", s.service.clone().or(s.program.clone()).unwrap_or_default()) });
    let o = crate::sys::host_command("sh").args(["-c", &script]).output().map_err(|e| format!("sh: {e}"))?;
    for l in String::from_utf8_lossy(&o.stdout).lines() {
        sink(Progress::Log { line: l.to_string() });
    }
    if !o.status.success() {
        return Err(format!("The gateway was not removed: {}", String::from_utf8_lossy(&o.stderr).trim()));
    }
    if status().installed {
        return Err("The removal finished, but a gateway is still found.".into());
    }
    let any_kept = keep_data && DATA_DIRS.iter().any(|d| Path::new(&format!("{}/{}", kept_dir(&now), d.trim_start_matches('/').replace('/', "-"))).exists());
    Ok(if any_kept {
        format!("The Sentient Gateway is removed. Its configuration and data are kept in {}.", kept_dir(&now))
    } else {
        "The Sentient Gateway is removed.".to_string()
    })
}

pub fn control(sink: &ProgressFn, action: &str) -> Result<(), String> {
    let verb = match action {
        "start" | "stop" | "restart" => action,
        _ => return Err(format!("unknown action {action}")),
    };
    let Some(unit) = unit() else {
        return Err(if process().is_some() {
            "The gateway runs as a plain process that no service started, so the Platform Manager cannot start or stop it.".into()
        } else {
            "There is no gateway on this computer.".into()
        });
    };
    let o = crate::sys::host_command("systemctl").args([verb, &unit]).output().map_err(|e| format!("systemctl: {e}"))?;
    if !o.status.success() {
        return Err(format!("systemctl {verb} {unit} failed: {}", String::from_utf8_lossy(&o.stderr).trim()));
    }
    sink(Progress::Done { message: format!("The gateway service {unit} is {}.", if verb == "stop" { "stopped" } else { "running" }) });
    Ok(())
}

// ---------------------------------------------------------------------------
// Settings (crate::gateway_settings): read as root, apply through the
// gateway's own command and a drop-in of ours
// ---------------------------------------------------------------------------

/// `systemctl show -p A -p B …` lines as a map.
fn show(unit: &str, props: &[&str]) -> std::collections::HashMap<String, String> {
    let mut args = vec!["show".to_string()];
    for p in props {
        args.push("-p".into());
        args.push((*p).into());
    }
    args.push(unit.to_string());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    out("systemctl", &refs).map(|o| o.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect()).unwrap_or_default()
}

/// The drop-in's path for `unit`.
pub fn dropin_path(unit: &str) -> String {
    format!("/etc/systemd/system/{unit}.d/{}", crate::gateway_settings::DROPIN)
}

/// Our drop-in's settings: (Restart= value, its variables).
pub fn parse_dropin(text: &str) -> (Option<String>, Vec<(String, String)>) {
    let mut restart = None;
    let mut env = Vec::new();
    for l in text.lines().map(str::trim) {
        if let Some(r) = l.strip_prefix("Restart=") {
            restart = Some(r.to_string());
        } else if let Some(e) = l.strip_prefix("Environment=") {
            let e = e.trim();
            let e = e.strip_prefix('"').and_then(|x| x.strip_suffix('"')).map(|x| x.replace("\\\"", "\"").replace("\\\\", "\\")).unwrap_or_else(|| e.to_string());
            let e = e.replace("%%", "%");
            if let Some((n, v)) = e.split_once('=') {
                env.push((n.to_string(), v.to_string()));
            }
        }
    }
    (restart, env)
}

/// Our drop-in: the restart policy and the variables, in systemd's quoting.
pub fn dropin_text(restart: Option<bool>, env: &[(String, String)]) -> String {
    let mut t = String::from("# Written by the SENTIENT Platform Manager (Gateway › Advanced); it rewrites this file.\n[Service]\n");
    match restart {
        Some(true) => t.push_str("Restart=always\nRestartSec=5\n"),
        Some(false) => t.push_str("Restart=no\n"),
        None => {}
    }
    for (n, v) in env {
        let q = format!("{n}={v}").replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%");
        t.push_str(&format!("Environment=\"{q}\"\n"));
    }
    t
}

/// The serial group here: the one serial adapters belong to, else dialout
/// (uucp on Arch).
fn serial_group() -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let groups = std::fs::read_to_string("/etc/group").unwrap_or_default();
        let name_of = |gid: u32| groups.lines().find_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            (f.len() > 2 && f[2].parse::<u32>().ok() == Some(gid)).then(|| f[0].to_string())
        });
        let devs = crate::distro::gateway_docker::serial_devices_in(Path::new("/dev/serial/by-id"), Path::new("/dev"));
        if let Some(g) = devs.iter().find_map(|d| std::fs::metadata(&d.path).ok().map(|m| m.gid())).filter(|g| *g != 0).and_then(name_of) {
            return Some(g);
        }
        ["dialout", "uucp"].iter().find(|g| groups.lines().any(|l| l.starts_with(&format!("{g}:")))).map(|g| g.to_string())
    }
    #[cfg(not(unix))]
    None
}

fn config_dir(env: &[String]) -> String {
    if let Some(d) = env.iter().find_map(|l| l.strip_prefix("ST_GW_CONFIG_DIR=").or_else(|| l.strip_prefix("TB_GW_CONFIG_DIR="))) {
        return d.to_string();
    }
    FILES.iter().find(|f| Path::new(f).is_file()).and_then(|f| Path::new(f).parent()).map(|p| p.display().to_string()).unwrap_or_else(|| DATA_DIRS[0].to_string())
}

/// A native gateway's settings, read as root (the job
/// `gateway-settings-json`); unprivileged, says so.
pub fn gw_settings(status: &Status) -> crate::gateway_settings::GwSettings {
    use crate::gateway_settings::{self as gs, GwSettings};
    let unit = status.service.clone();
    let props = unit.as_deref().map(|u| show(u, &["Restart", "User", "MainPID", "Environment"])).unwrap_or_default();
    let env = props.get("Environment").map(|e| parse_environment(e)).unwrap_or_default();
    let dir = config_dir(&env);
    if !crate::exec::is_root() && std::fs::read_dir(&dir).is_err() {
        return GwSettings { source: "none".into(), note: Some("Readable by an administrator only.".into()), ..GwSettings::default() };
    }
    let pid = props.get("MainPID").cloned().filter(|p| p != "0").unwrap_or_else(|| "0".into());
    let line = format!("D={}; P={}; {}", crate::exec::shq(&dir), crate::exec::shq(&pid), gs::read_script());
    let read = out("sh", &["-c", &line]);
    let mut s = read.as_deref().map(gs::from_read).unwrap_or_else(|| GwSettings { source: "none".into(), note: Some("The gateway's settings could not be read.".into()), ..GwSettings::default() });
    let program = status.program.clone().unwrap_or_else(|| PROGRAM.into());
    s.caps = gs::Caps::of_help(&out(&program, &["--help"]).unwrap_or_default());
    if s.caps.status_json {
        if let Some(o) = out(&program, &["--status-json"]) {
            gs::merge_status_json(&mut s, &o);
        }
    }
    if !status.running {
        s.connected = Some(false);
        s.connected_how = Some("the gateway is stopped".into());
    }
    gs::apply_env(&mut s, &env, false);
    if let Some(r) = &read {
        gs::connected_from_read(&mut s, r);
    }
    s.log_files = Some(env.iter().find_map(|l| l.strip_prefix("ST_GW_LOGS_PATH=").or_else(|| l.strip_prefix("TB_GW_LOGS_PATH="))).map(str::to_string).unwrap_or_else(|| "/var/lib/sentient-gateway/logs".into()));
    let Some(unit) = unit else {
        s.machine.not_editable = Some("This gateway runs as a plain process that no service started, so how it runs is not changed from here.".into());
        return s;
    };
    let (_, ours) = parse_dropin(&std::fs::read_to_string(dropin_path(&unit)).unwrap_or_default());
    let restart = props.get("Restart").cloned().unwrap_or_default();
    s.machine.restart_always = Some(!restart.is_empty() && restart != "no");
    s.machine.restart_word = Some(if restart.is_empty() { "no".into() } else { restart });
    let account = props.get("User").cloned().filter(|u| !u.is_empty() && u != "root");
    let group = serial_group();
    let in_group = match (&account, &group) {
        (None, _) => true,
        (Some(a), Some(g)) => out("id", &["-nG", a]).is_some_and(|o| o.split_whitespace().any(|x| x == g)),
        (Some(_), None) => false,
    };
    s.machine.serial = gs::Serial {
        mode: if account.is_some() { "group".into() } else { String::new() },
        group,
        account,
        in_group,
        chosen: Vec::new(),
        available: crate::distro::gateway_docker::serial_devices_in(Path::new("/dev/serial/by-id"), Path::new("/dev")).into_iter().map(|d| d.path).collect(),
    };
    s.machine.env = gs::overrides(&env, &[], |n| ours.iter().any(|(o, _)| o == n), "the unit file");
    s
}

/// How the gateway restarts once to take a native Apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restart {
    /// The gateway's own `--install …` (it stops and starts the service).
    ByInstall(Vec<String>),
    /// `systemctl restart <unit>`.
    Systemctl,
    /// Nothing to restart for.
    None,
}

/// A native Apply's steps, in order: our drop-in (then daemon-reload),
/// the serial group, then ONE restart: by `--install` when the page
/// changes (it restarts the service itself), else by systemctl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeSteps {
    pub dropin: Option<String>,
    /// (join, account, group).
    pub serial: Option<(bool, String, String)>,
    pub restart: Restart,
}

/// The steps for `plan` on a service whose drop-in reads `dropin_now`.
pub fn native_steps(before: &crate::gateway_settings::GwSettings, plan: &crate::gateway_settings::ApplyPlan, dropin_now: &str) -> Result<NativeSteps, String> {
    let page = plan.ui_enabled.is_some() || plan.ui_listen.is_some();
    if page && !before.caps.ui_listen {
        return Err("This gateway is older than 4.12: its web page is set up on the gateway's own command line only. Update it first.".into());
    }
    let (restart_before, mut env) = parse_dropin(dropin_now);
    let set = |env: &mut Vec<(String, String)>, n: &str, v: Option<String>| {
        env.retain(|(k, _)| k != n);
        if let Some(v) = v {
            env.push((n.to_string(), v));
        }
    };
    // The environment wins over the file: ours follows the page too.
    let page_env = page && before.machine.ui_env;
    if page_env {
        if let Some(on) = plan.ui_enabled {
            set(&mut env, "ST_GW_UI", Some(if on { "on" } else { "off" }.into()));
        }
        if let Some(l) = &plan.ui_listen {
            set(&mut env, "ST_GW_UI_LISTEN", Some(l.clone()));
        }
    }
    if let Some(vars) = &plan.env {
        let keep = ["ST_GW_UI", "ST_GW_UI_LISTEN", "RUST_LOG"];
        env.retain(|(n, _)| keep.contains(&n.as_str()) || before.machine.env.iter().any(|e| &e.name == n && e.value.is_none()));
        for (n, v) in vars {
            if n.is_empty() || n.contains('=') || v.contains('\n') {
                return Err(format!("{n}: not a usable variable"));
            }
            set(&mut env, n, Some(v.clone()));
        }
    }
    if let Some(l) = &plan.rust_log {
        set(&mut env, "RUST_LOG", (!l.is_empty()).then(|| l.clone()));
    }
    // The connection: written by the gateway's own --install (below) into
    // its config file; an override of it in our drop-in goes.
    let conn = plan.conn_host.is_some() || plan.conn_port.is_some();
    let conn_env = conn && env.iter().any(|(k, _)| matches!(k.as_str(), "host" | "ST_GW_HOST" | "TB_GW_HOST" | "port" | "ST_GW_PORT" | "TB_GW_PORT"));
    if conn_env {
        env.retain(|(k, _)| !matches!(k.as_str(), "host" | "ST_GW_HOST" | "TB_GW_HOST" | "port" | "ST_GW_PORT" | "TB_GW_PORT"));
    }
    let restart_policy = plan.restart_always.or_else(|| restart_before.map(|r| r != "no"));
    let dropin = (plan.env.is_some() || plan.rust_log.is_some() || plan.restart_always.is_some() || page_env || conn_env).then(|| dropin_text(restart_policy, &env));
    let serial = match plan.serial_group {
        None => None,
        Some(join) => match (&before.machine.serial.account, &before.machine.serial.group) {
            (Some(a), Some(g)) => Some((join, a.clone(), g.clone())),
            _ => return Err("No serial group was found on this computer (dialout or uucp).".into()),
        },
    };
    let restart = if page || conn {
        let mut args = vec!["--install".to_string()];
        if let Some(h) = &plan.conn_host {
            args.push("--host".into());
            args.push(h.clone());
        }
        if let Some(p) = plan.conn_port {
            args.push("--port".into());
            args.push(p.to_string());
        }
        match plan.ui_enabled {
            Some(true) => args.push("--ui".into()),
            Some(false) => args.push("--no-ui".into()),
            None => {}
        }
        if let Some(l) = &plan.ui_listen {
            args.push("--ui-listen".into());
            args.push(l.clone());
        }
        Restart::ByInstall(args)
    } else if dropin.is_some() || serial.is_some() {
        Restart::Systemctl
    } else {
        Restart::None
    };
    Ok(NativeSteps { dropin, serial, restart })
}

/// Apply `plan` to the native service (as root): [`native_steps`] — the
/// drop-in and the serial group first, then the gateway restarts once.
pub fn apply_settings(sink: &ProgressFn, status: &Status, before: &crate::gateway_settings::GwSettings, plan: &crate::gateway_settings::ApplyPlan) -> Result<(), String> {
    let unit = status.service.clone().ok_or("This gateway runs as a plain process that no service started.")?;
    let program = status.program.clone().unwrap_or_else(|| PROGRAM.into());
    let path = dropin_path(&unit);
    let steps = native_steps(before, plan, &std::fs::read_to_string(&path).unwrap_or_default())?;
    if let Some(text) = &steps.dropin {
        sink(Progress::Step { name: format!("Writing {path}") });
        if let Some(d) = Path::new(&path).parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("create {}: {e}", d.display()))?;
        }
        std::fs::write(&path, text).map_err(|e| format!("write {path}: {e}"))?;
        let _ = crate::sys::host_command("systemctl").arg("daemon-reload").output();
    }
    if let Some((join, account, group)) = &steps.serial {
        sink(Progress::Step { name: format!("{} {account} {} the group {group}", if *join { "Adding" } else { "Removing" }, if *join { "to" } else { "from" }) });
        let o = if *join { crate::sys::host_command("usermod").args(["-aG", group, account]).output() } else { crate::sys::host_command("gpasswd").args(["-d", account, group]).output() }.map_err(|e| e.to_string())?;
        if !o.status.success() {
            return Err(format!("The group {group} could not be changed: {}", String::from_utf8_lossy(&o.stderr).trim()));
        }
    }
    match &steps.restart {
        Restart::ByInstall(args) => {
            sink(Progress::Step { name: "Setting the web page and restarting the gateway once (its own --install)".into() });
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let o = crate::sys::host_command(&program).args(&refs).output().map_err(|e| format!("{program}: {e}"))?;
            crate::exec::emit(sink, &o.stdout);
            if !o.status.success() {
                return Err(format!("{program} {} failed: {}", args.join(" "), String::from_utf8_lossy(&o.stderr).trim()));
            }
        }
        Restart::Systemctl => {
            sink(Progress::Step { name: format!("Restarting {unit} once") });
            let o = crate::sys::host_command("systemctl").args(["restart", &unit]).output().map_err(|e| e.to_string())?;
            if !o.status.success() {
                return Err(format!("systemctl restart {unit} failed: {}", String::from_utf8_lossy(&o.stderr).trim()));
            }
        }
        Restart::None => return Ok(()),
    }
    for _ in 0..30 {
        if out("systemctl", &["is-active", &unit]).is_some_and(|s| s.trim() == "active") {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    Err(format!("The gateway service {unit} did not come back within 30 seconds; see its log."))
}

/// The credentials to test with (as root): the file, the unit's environment over it.
pub fn test_creds() -> Option<crate::gateway_settings::TestCreds> {
    let env = unit().map(|u| unit_environment(&u)).unwrap_or_default();
    let dir = config_dir(&env);
    let file = std::fs::read_to_string(format!("{dir}/st_gateway.json")).or_else(|_| std::fs::read_to_string(format!("{dir}/tb_gateway.json"))).ok();
    crate::gateway_settings::creds_of(file.as_deref(), &env)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `systemctl list-unit-files --type=service --no-legend --no-pager` and
    /// `systemctl show -p ExecStart --value systemd-journal-gatewayd.service`
    /// on Arch (systemd 262), recorded.
    const ARCH_UNITS: &str = "systemd-journal-catalog-update.service        static          -\n\
systemd-journal-flush.service                 static          -\n\
systemd-journal-gatewayd.service             indirect        disabled\n\
systemd-journal-remote.service               disabled        disabled\n\
systemd-journald.service                     static          -\n\
sshd.service                                 disabled        disabled\n";
    const JOURNAL_EXEC: &str = "{ path=/usr/lib/systemd/systemd-journal-gatewayd ; argv[]=/usr/lib/systemd/systemd-journal-gatewayd ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }";
    const OUR_EXEC: &str = "{ path=/usr/bin/sentient-gateway ; argv[]=/usr/bin/sentient-gateway ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }";

    #[test]
    fn the_journal_gateway_is_not_ours() {
        // A hint by name, but it runs systemd's program: never the gateway.
        assert_eq!(candidates(ARCH_UNITS), vec!["systemd-journal-gatewayd.service".to_string()]);
        assert_eq!(our_unit("systemd-journal-gatewayd.service", JOURNAL_EXEC, Some("systemd 262 (262.1-1-arch)\n")), None);
        assert_eq!(our_unit("systemd-journal-gatewayd.service", JOURNAL_EXEC, None), None);
        assert!(!is_our_program("/usr/lib/systemd/systemd-journal-gatewayd"));
    }

    #[test]
    fn the_package_unit_is_ours() {
        assert_eq!(
            our_unit(UNIT, OUR_EXEC, Some("sentient-gateway 4.16.2\n")),
            Some(("/usr/bin/sentient-gateway".into(), Some("4.16.2".into())))
        );
        // Its program not answering: still ours, its version unknown.
        assert_eq!(our_unit(UNIT, OUR_EXEC, None), Some(("/usr/bin/sentient-gateway".into(), None)));
    }

    #[test]
    fn a_unit_by_hand_needs_the_program_to_answer() {
        let exec = "/opt/sentient-gateway/bin/sentient-gateway --config /etc/plant/gw.json";
        assert_eq!(
            our_unit("plant-collector.service", exec, Some("sentient-gateway 4.15.0")),
            Some(("/opt/sentient-gateway/bin/sentient-gateway".into(), Some("4.15.0".into())))
        );
        assert_eq!(our_unit("plant-collector.service", exec, None), None);
        assert_eq!(our_unit("plant-collector.service", exec, Some("something else 1.0")), None);
        // Another program under our unit's name is not ours.
        assert_eq!(our_unit(UNIT, "{ path=/usr/bin/python3 ; argv[]=/usr/bin/python3 -m other_gateway }", None), None);
    }

    #[test]
    fn finds_a_plain_process_but_not_ours() {
        let ps = "  12 /opt/sentient-platform-manager/sentient-platform-manager --job gateway-status\n 40 /usr/lib/systemd/systemd-journal-gatewayd\n 77 /usr/local/bin/sentient-gateway --config /etc/x.json\n";
        assert_eq!(pick_process(ps), Some(("77".into(), "/usr/local/bin/sentient-gateway".into())));
        assert_eq!(pick_process(" 1 python3 -m other_gateway.main\n 40 /usr/lib/systemd/systemd-journal-gatewayd\n"), None);
        assert_eq!(pick_process(" 2 bash\n"), None);
    }

    #[test]
    fn removal_refuses_anything_but_our_program() {
        // The journal gateway as it was once shown: never removed.
        let journal = Status {
            installed: true,
            service: Some("systemd-journal-gatewayd.service".into()),
            program: Some("/usr/lib/systemd/systemd-journal-gatewayd".into()),
            ..Status::default()
        };
        assert!(removal_script(&journal, true, "20261116-210000").is_err());
        assert!(removal_script(&Status { installed: true, service: Some(UNIT.into()), ..Status::default() }, true, "x").is_err());
    }

    #[test]
    fn removal_of_a_unit_by_hand() {
        let s = Status {
            installed: true,
            service: Some("plant-gw.service".into()),
            program: Some("/opt/sentient-gateway/bin/sentient-gateway".into()),
            ..Status::default()
        };
        let keep = removal_script(&s, true, "20261116-210000").unwrap();
        assert!(keep.contains("in *'/opt/sentient-gateway/bin/sentient-gateway'*) ;;"));
        assert!(keep.contains("systemctl disable --now 'plant-gw.service'"));
        assert!(keep.contains("mv /etc/sentient-gateway '/var/backups/sentient-gateway-20261116-210000'/etc-sentient-gateway"));
        assert!(keep.contains("rm -f '/opt/sentient-gateway/bin/sentient-gateway'"));
        assert!(!keep.contains("rm -rf /var/lib/sentient-gateway"));
        let delete = removal_script(&s, false, "x").unwrap();
        assert!(delete.contains("rm -rf /var/lib/sentient-gateway") && !delete.contains("mkdir -p"));
        // The package's: its package manager removes it.
        let pkg = Status { package: Some(PACKAGE.into()), service: Some(UNIT.into()), program: Some("/usr/bin/sentient-gateway".into()), ..s };
        let p = removal_script(&pkg, true, "x").unwrap();
        assert!(p.contains("pacman -R --noconfirm 'sentient-gateway'") && !p.contains("rm -f '/usr/bin/sentient-gateway'"));
    }

    #[test]
    fn the_page_and_host_of_a_native_gateway() {
        let never = |_: &str| false;
        let always = |_: &str| true;
        // Read: the file's settings; the page is on by default (B2).
        let file = r#"{"sentient":{"host":"10.0.0.5","port":1883}}"#;
        assert_eq!(settings(Some(file), false, &[], true, &never), (Some("10.0.0.5".into()), Some(1883), Some("http://127.0.0.1:8642/".into())));
        let off = r#"{"sentient":{"host":"10.0.0.5","port":1883},"localUi":{"enabled":false}}"#;
        assert_eq!(settings(Some(off), false, &[], true, &always).2, None);
        // The service's environment wins over the file.
        assert_eq!(settings(Some(off), false, &["ST_GW_UI=on".into()], true, &never).2.as_deref(), Some("http://127.0.0.1:8642/"));
        // Closed to this account (B3): no host; the page only when it answers.
        assert_eq!(settings(None, true, &[], true, &always), (None, None, Some("http://127.0.0.1:8642/".into())));
        assert_eq!(settings(None, true, &[], true, &never).2, None);
        assert_eq!(settings(None, true, &[], false, &always).2, None);
        assert_eq!(parse_environment("ST_GW_UI=off RUST_LOG=info\n"), vec!["ST_GW_UI=off".to_string(), "RUST_LOG=info".to_string()]);
        assert!(parse_environment("\n").is_empty());
    }

    #[test]
    fn our_drop_in_round_trips() {
        let env = vec![("RUST_LOG".to_string(), "debug".to_string()), ("ST_GW_X".to_string(), "a \"b\" 100%".to_string())];
        let t = dropin_text(Some(true), &env);
        assert!(t.contains("Restart=always\nRestartSec=5\n"));
        assert!(t.contains("Environment=\"ST_GW_X=a \\\"b\\\" 100%%\""), "{t}");
        let (r, back) = parse_dropin(&t);
        assert_eq!(r.as_deref(), Some("always"));
        assert_eq!(back, env);
        assert!(!dropin_text(None, &[]).contains("Restart="));
        assert_eq!(dropin_path("sentient-gateway.service"), "/etc/systemd/system/sentient-gateway.service.d/50-platform-manager.conf");
    }

    #[test]
    fn a_native_apply_restarts_once() {
        use crate::gateway_settings::{ApplyPlan, Caps, GwSettings, Serial};
        let mut before = GwSettings::default();
        before.caps = Caps { ui_listen: true, ui_password: true, ..Caps::default() };
        before.machine.serial = Serial { mode: "group".into(), group: Some("dialout".into()), account: Some("sentient-gateway".into()), ..Serial::default() };
        // Page and drop-in: the drop-in first, then --install restarts it, once.
        let both = ApplyPlan { ui_enabled: Some(true), ui_listen: Some("0.0.0.0:8642".into()), rust_log: Some("debug".into()), restart_always: Some(true), ..ApplyPlan::default() };
        let s = native_steps(&before, &both, "").unwrap();
        assert!(s.dropin.as_deref().is_some_and(|d| d.contains("RUST_LOG=debug") && d.contains("Restart=always")));
        assert_eq!(s.restart, Restart::ByInstall(vec!["--install".into(), "--ui".into(), "--ui-listen".into(), "0.0.0.0:8642".into()]));
        // No page change: one systemctl restart.
        let env = ApplyPlan { env: Some(vec![("ST_GW_X".into(), "1".into())]), serial_group: Some(true), ..ApplyPlan::default() };
        let s = native_steps(&before, &env, "Environment=\"ST_GW_OLD=1\"\n").unwrap();
        assert_eq!(s.restart, Restart::Systemctl);
        assert_eq!(s.serial, Some((true, "sentient-gateway".into(), "dialout".into())));
        let d = s.dropin.unwrap();
        assert!(d.contains("ST_GW_X=1") && !d.contains("ST_GW_OLD"), "{d}");
        // The page alone: --install only, no drop-in.
        let page = ApplyPlan { ui_enabled: Some(false), ..ApplyPlan::default() };
        let s = native_steps(&before, &page, "").unwrap();
        assert_eq!((s.dropin, s.restart), (None, Restart::ByInstall(vec!["--install".into(), "--no-ui".into()])));
        // An old gateway: refused before anything.
        before.caps.ui_listen = false;
        assert!(native_steps(&before, &page, "").is_err());
        assert_eq!(native_steps(&before, &ApplyPlan::default(), "").unwrap().restart, Restart::None);
    }
}
