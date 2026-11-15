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

/// Unit and program names a gateway goes by (ours and the older Python one).
const NAMES: [&str; 6] = ["sentient-gateway", "sentient_gateway", "st-gateway", "thingsboard-gateway", "thingsboard_gateway", "tb-gateway"];

/// Config files a native gateway reads (the platform host and port).
const FILES: [&str; 4] = [
    "/etc/sentient-gateway/config/st_gateway.json",
    "/etc/sentient-gateway/st_gateway.json",
    "/opt/sentient-gateway/config/st_gateway.json",
    "/var/lib/sentient-gateway/config/st_gateway.json",
];

fn out(cmd: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(cmd).args(args).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

/// A unit, container or program name that is a gateway's: one of NAMES,
/// st-gw, or any name with "gateway" in it.
fn is_gateway_name(s: &str) -> bool {
    let s = s.to_ascii_lowercase();
    NAMES.iter().any(|n| s.contains(n)) || s.contains("gateway") || s.starts_with("st-gw") || s.starts_with("st_gw")
}

/// Where a gateway is installed without a service or a running process.
const DIRS: [&str; 2] = ["/opt/sentient-gateway", "/etc/sentient-gateway"];

/// The gateway's program in an install folder (for its version).
fn program_in(dir: &str) -> Option<String> {
    ["bin/sentient-gateway", "sentient-gateway", "bin/st-gateway", "st-gateway"]
        .iter()
        .map(|p| format!("{dir}/{p}"))
        .find(|p| Path::new(p).is_file())
}

/// The gateway's systemd unit, from the unit files there are.
pub fn unit() -> Option<String> {
    let list = out("systemctl", &["list-unit-files", "--type=service", "--no-legend", "--no-pager"])?;
    pick_unit(&list)
}

/// The first unit in a `list-unit-files` listing whose name is a gateway's.
fn pick_unit(list: &str) -> Option<String> {
    list.lines().filter_map(|l| l.split_whitespace().next()).find(|u| is_gateway_name(u)).map(str::to_string)
}

/// A gateway process that no service started: its program path.
fn process() -> Option<String> {
    let ps = out("ps", &["-eo", "args="])?;
    pick_process(&ps)
}

fn pick_process(ps: &str) -> Option<String> {
    ps.lines()
        .map(str::trim)
        .filter(|l| !l.contains("sentient-platform-manager") && !l.starts_with("grep"))
        .find_map(|l| l.split_whitespace().take(3).find(|w| is_gateway_name(w.rsplit('/').next().unwrap_or(w))))
        .map(str::to_string)
}

/// `--version` of a program, or the package's version.
fn version_of(program: Option<&str>) -> Option<String> {
    program
        .and_then(|p| out(p, &["--version"]))
        .and_then(|o| crate::gateway::parse_version(&o))
        .or_else(|| {
            NAMES.iter().find_map(|n| {
                out("dpkg-query", &["-W", "-f=${Version}", n])
                    .or_else(|| out("rpm", &["-q", "--qf", "%{VERSION}", n]))
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty() && !v.contains(' '))
            })
        })
}

/// The program a unit runs (`ExecStart`'s path).
fn unit_program(unit: &str) -> Option<String> {
    let exec = out("systemctl", &["show", "-p", "ExecStart", "--value", unit])?;
    exec.split(" ; ")
        .find_map(|p| p.trim().trim_start_matches('{').trim().strip_prefix("path="))
        .map(|p| p.trim().to_string())
        .or_else(|| exec.split_whitespace().next().map(str::to_string))
}

fn host_port() -> (Option<String>, Option<u16>) {
    FILES.iter().find_map(|f| std::fs::read_to_string(f).ok()).map(|t| crate::gateway::host_of(&t)).unwrap_or((None, None))
}

pub fn status() -> Status {
    if let Some(unit) = unit() {
        let running = out("systemctl", &["is-active", &unit]).is_some_and(|s| s.trim() == "active");
        let program = unit_program(&unit);
        let (host, port) = host_port();
        return Status {
            installed: true,
            version: version_of(program.as_deref()),
            running,
            host,
            port,
            service: Some(unit),
            ..Status::default()
        };
    }
    if let Some(program) = process() {
        // (A plain process; else an install folder below.)
        let (host, port) = host_port();
        return Status { installed: true, version: version_of(Some(&program)), running: true, host, port, process: Some(program), ..Status::default() };
    }
    // Installed in a folder, neither a service nor running.
    if let Some(dir) = DIRS.iter().find(|d| Path::new(d).is_dir()) {
        let program = program_in(dir);
        let (host, port) = host_port();
        return Status { installed: true, version: version_of(program.as_deref()), running: false, host, port, process: Some(program.unwrap_or_else(|| dir.to_string())), ..Status::default() };
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

    #[test]
    fn finds_gateway_units() {
        let list = "ssh.service enabled enabled\nthingsboard-gateway.service enabled enabled\n";
        assert_eq!(pick_unit(list).as_deref(), Some("thingsboard-gateway.service"));
        assert_eq!(pick_unit("cron.service enabled enabled\n"), None);
        assert_eq!(pick_unit("sentient-gateway.service enabled enabled\n").as_deref(), Some("sentient-gateway.service"));
    }

    #[test]
    fn finds_a_plain_process_but_not_ours() {
        let ps = "/opt/sentient-platform-manager/sentient-platform-manager --job gateway-status\n/usr/local/bin/sentient-gateway --config /etc/x.json\n";
        assert_eq!(pick_process(ps).as_deref(), Some("/usr/local/bin/sentient-gateway"));
        assert_eq!(pick_process("python3 -m thingsboard_gateway.tb_gateway\n").as_deref(), Some("thingsboard_gateway.tb_gateway"));
        assert_eq!(pick_process("bash\n"), None);
    }
}
