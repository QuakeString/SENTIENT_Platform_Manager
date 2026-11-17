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

fn host_port() -> (Option<String>, Option<u16>) {
    FILES.iter().find_map(|f| std::fs::read_to_string(f).ok()).map(|t| crate::gateway::host_of(&t)).unwrap_or((None, None))
}

pub fn status() -> Status {
    if let Some(f) = found_unit() {
        let running = out("systemctl", &["is-active", &f.unit]).is_some_and(|s| s.trim() == "active");
        let (host, port) = host_port();
        let version = f.version.or_else(|| (f.unit == UNIT).then(package_version).flatten());
        let package = owner(&f.program);
        return Status { installed: true, version, running, host, port, service: Some(f.unit), program: Some(f.program), package, ..Status::default() };
    }
    if let Some((program, version)) = process() {
        let (host, port) = host_port();
        let package = owner(&program);
        return Status { installed: true, version: Some(version), running: true, host, port, process: Some(program.clone()), program: Some(program), package, ..Status::default() };
    }
    // The program installed, neither a service nor running.
    if let Some((program, version)) = installed_program() {
        let (host, port) = host_port();
        let package = owner(&program);
        return Status { installed: true, version: Some(version), running: false, host, port, process: Some(program.clone()), program: Some(program), package, ..Status::default() };
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
}
