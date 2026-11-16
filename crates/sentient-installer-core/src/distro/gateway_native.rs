// Copyright © 2016-2026 The SENTIENT Authors

//! A Sentient Gateway installed on Linux without Docker: a systemd service
//! (from a package or by hand), or a plain process started some other way.
//! crate::gateway::status asks Docker first, then this.
//!
//! A service is shown, started, stopped and restarted with systemctl, and its
//! log is its journal. A plain process is shown (running, version) but not
//! controlled: nothing would start it again.

use std::path::Path;
use std::process::Command;

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
    let o = Command::new(cmd).args(args).output().ok()?;
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

fn host_port() -> (Option<String>, Option<u16>) {
    FILES.iter().find_map(|f| std::fs::read_to_string(f).ok()).map(|t| crate::gateway::host_of(&t)).unwrap_or((None, None))
}

pub fn status() -> Status {
    if let Some(f) = found_unit() {
        let running = out("systemctl", &["is-active", &f.unit]).is_some_and(|s| s.trim() == "active");
        let (host, port) = host_port();
        let version = f.version.or_else(|| (f.unit == UNIT).then(package_version).flatten());
        return Status { installed: true, version, running, host, port, service: Some(f.unit), ..Status::default() };
    }
    if let Some((program, version)) = process() {
        let (host, port) = host_port();
        return Status { installed: true, version: Some(version), running: true, host, port, process: Some(program), ..Status::default() };
    }
    // The program installed, neither a service nor running.
    if let Some((program, version)) = installed_program() {
        let (host, port) = host_port();
        return Status { installed: true, version: Some(version), running: false, host, port, process: Some(program), ..Status::default() };
    }
    Status::default()
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
    let o = Command::new("systemctl").args([verb, &unit]).output().map_err(|e| format!("systemctl: {e}"))?;
    if !o.status.success() {
        return Err(format!("systemctl {verb} {unit} failed: {}", String::from_utf8_lossy(&o.stderr).trim()));
    }
    sink(Progress::Done { message: format!("The gateway service {unit} is {}.", if verb == "stop" { "stopped" } else { "running" }) });
    Ok(())
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
        assert_eq!(our_unit(UNIT, "{ path=/usr/bin/python3 ; argv[]=/usr/bin/python3 -m thingsboard_gateway }", None), None);
    }

    #[test]
    fn finds_a_plain_process_but_not_ours() {
        let ps = "  12 /opt/sentient-platform-manager/sentient-platform-manager --job gateway-status\n 40 /usr/lib/systemd/systemd-journal-gatewayd\n 77 /usr/local/bin/sentient-gateway --config /etc/x.json\n";
        assert_eq!(pick_process(ps), Some(("77".into(), "/usr/local/bin/sentient-gateway".into())));
        assert_eq!(pick_process(" 1 python3 -m thingsboard_gateway.tb_gateway\n 40 /usr/lib/systemd/systemd-journal-gatewayd\n"), None);
        assert_eq!(pick_process(" 2 bash\n"), None);
    }
}
