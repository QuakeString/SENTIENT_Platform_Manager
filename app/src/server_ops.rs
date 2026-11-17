// Copyright © 2016-2026 The SENTIENT Authors

//! The Server card's actions on the machine itself (crate::manager::server_card),
//! as `--job`s the agent runs as administrator (here: through the one root
//! helper, crate::rootjob):
//!
//! - `server-info-json`: everything the card and All details show
//!   (crate::server_info), one JSON line.
//! - `server-boot-json`: only when it started (`{"boot": …}`), the restart
//!   job's cheap "is it back, and did it really restart" question.
//! - `server-power <reboot|poweroff>`: SENTIENT and its database stopped
//!   cleanly, then the system's own command a few seconds later, so the
//!   answer gets back first (Linux: `systemctl reboot` / `poweroff` from a
//!   timer unit; Windows: `shutdown /r /t 5` / `/s /t 5`).
//! - `docker-restart`: Docker restarted, waited for, SENTIENT started again.
//! - `system-updates`: the system's updates installed (apt, dnf, pacman,
//!   zypper; Windows Update).

use std::io::{BufRead as _, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use sentient_installer_core::progress::{Progress, ProgressFn};

use crate::installation::{Engine, InstallationRecord};

/// The step names (crate::jobplan lists them before the job starts).
pub const STEP_DOCKER_RESTART: &str = "Restart Docker";
pub const STEP_DOCKER_WAIT: &str = "Wait for Docker to answer";
pub const STEP_DOCKER_SENTIENT: &str = "SENTIENT running again";
pub const STEP_UPD_LOOK: &str = "Look for updates";
pub const STEP_UPD_INSTALL: &str = "Download and install the updates";
pub const STEP_UPD_CHECK: &str = "Check whether a restart is needed";

/// `--job …` names this module answers; None: not one of them.
pub fn job(job: &str, args: &[String], sink: &ProgressFn) -> Option<Result<(), String>> {
    Some(match job {
        "server-info-json" => {
            let record = InstallationRecord::load();
            println!("{}", serde_json::to_string(&crate::server_info::gather(record.as_ref())).unwrap_or_default());
            Ok(())
        }
        "server-boot-json" => {
            println!("{}", serde_json::json!({ "boot": boot_time() }));
            Ok(())
        }
        "server-power" => match args.get(1).map(String::as_str) {
            Some(a @ ("reboot" | "poweroff")) => power(a == "reboot", sink),
            _ => Err("usage: --job server-power <reboot|poweroff>".into()),
        },
        "docker-restart" => {
            let record = InstallationRecord::load();
            sink(Progress::Plan { steps: crate::jobplan::docker_restart(record.as_ref().is_some_and(|r| r.engine == Engine::Docker)) });
            docker_restart(record.as_ref(), sink)
        }
        "system-updates" => {
            sink(Progress::Plan { steps: crate::jobplan::system_updates() });
            let r = system_updates(sink);
            crate::server_info::forget_updates();
            r
        }
        _ => return None,
    })
}

/// When this machine started (Unix seconds).
pub fn boot_time() -> i64 {
    #[cfg(windows)]
    {
        ps("[int64]((Get-CimInstance Win32_OperatingSystem).LastBootUpTime.ToUniversalTime() - [datetime]'1970-01-01').TotalSeconds").ok().and_then(|t| t.trim().lines().last().and_then(|l| l.trim().parse().ok())).unwrap_or(0)
    }
    #[cfg(not(windows))]
    {
        std::fs::read_to_string("/proc/stat").ok().and_then(|t| t.lines().find_map(|l| l.strip_prefix("btime ")).and_then(|v| v.trim().parse().ok())).unwrap_or(0)
    }
}

/// SENTIENT and its database stopped cleanly, then the restart or power off
/// a few seconds later (the answer gets back first).
fn power(reboot: bool, sink: &ProgressFn) -> Result<(), String> {
    if let Some(record) = InstallationRecord::load() {
        sink(Progress::Log { line: "Stopping SENTIENT and its database cleanly".into() });
        record.use_pg_tools();
        // The machine stops anyway: a PostgreSQL other databases use stops too.
        record.platform().control_stack(sink.clone(), "stop", true)?;
    }
    let boot = boot_time();
    #[cfg(windows)]
    let sent = run_status("shutdown", &[if reboot { "/r" } else { "/s" }, "/t", "5", "/c", "Platform Manager"]);
    #[cfg(not(windows))]
    let sent = {
        let verb = if reboot { "reboot" } else { "poweroff" };
        // A timer unit of its own: it outlives this job and the connection.
        run_status("systemd-run", &["--on-active=4", "--timer-property=AccuracySec=1s", "--unit", &format!("spm-power-{}", chrono::Utc::now().timestamp()), "/bin/systemctl", verb])
            .or_else(|_| run_status("sh", &["-c", &format!("(sleep 4; systemctl {verb} || shutdown {} now) >/dev/null 2>&1 </dev/null &", if reboot { "-r" } else { "-h" })]))
    };
    sent?;
    sink(Progress::Log { line: format!("The {} was sent; it starts in a few seconds", if reboot { "restart" } else { "shutdown" }) });
    println!("{}", serde_json::json!({ "boot": boot, "sent": true }));
    Ok(())
}

fn run_status(cmd: &str, args: &[&str]) -> Result<(), String> {
    let o = quiet(cmd).args(args).stdin(Stdio::null()).output().map_err(|e| format!("{cmd}: {e}"))?;
    if o.status.success() {
        Ok(())
    } else {
        let e = String::from_utf8_lossy(&o.stderr).trim().to_string();
        Err(if e.is_empty() { format!("{cmd} ended with {}", o.status) } else { e })
    }
}

fn quiet(cmd: &str) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(cmd);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c
}

/// Run `cmd`, every output line into the log; Err with its last lines.
fn stream(cmd: &str, args: &[&str], env: &[(&str, &str)], sink: &ProgressFn) -> Result<(), String> {
    let mut c = quiet(cmd);
    c.args(args).envs(env.iter().copied()).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("{cmd}: {e}"))?;
    let err = child.stderr.take();
    let err_lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let keep = err_lines.clone();
    let s2 = sink.clone();
    let t = std::thread::spawn(move || {
        if let Some(e) = err {
            for l in BufReader::new(e).lines().map_while(Result::ok) {
                if let Ok(mut k) = keep.lock() {
                    k.push(l.clone());
                }
                s2(Progress::Log { line: l });
            }
        }
    });
    if let Some(o) = child.stdout.take() {
        for l in BufReader::new(o).lines().map_while(Result::ok) {
            sink(Progress::Log { line: l });
        }
    }
    let _ = t.join();
    let st = child.wait().map_err(|e| e.to_string())?;
    if st.success() {
        return Ok(());
    }
    let tail = err_lines.lock().map(|v| v.iter().rev().take(4).rev().cloned().collect::<Vec<_>>().join("\n")).unwrap_or_default();
    Err(format!("{cmd} {} ended with {st}{}", args.first().copied().unwrap_or(""), if tail.is_empty() { String::new() } else { format!(":\n{tail}") }))
}

fn docker_restart(record: Option<&InstallationRecord>, sink: &ProgressFn) -> Result<(), String> {
    sink(Progress::Step { name: STEP_DOCKER_RESTART.into() });
    #[cfg(windows)]
    stream("powershell.exe", &["-NoProfile", "-NonInteractive", "-Command", "$s = Get-Service docker, com.docker.service -ErrorAction SilentlyContinue | Select-Object -First 1; if (-not $s) { throw 'Docker is not installed on this computer.' }; Restart-Service -Force -InputObject $s"], &[], sink)?;
    #[cfg(not(windows))]
    stream("systemctl", &["restart", "docker"], &[], sink)?;
    sink(Progress::Step { name: STEP_DOCKER_WAIT.into() });
    let t0 = Instant::now();
    loop {
        if quiet("docker").args(["info", "--format", "{{.ServerVersion}}"]).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success()) {
            break;
        }
        if t0.elapsed() > Duration::from_secs(120) {
            return Err("Docker did not answer within 2 minutes after its restart. Look at its log (journalctl -u docker) on the server.".into());
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    if let Some(r) = record.filter(|r| r.engine == Engine::Docker) {
        sink(Progress::Step { name: STEP_DOCKER_SENTIENT.into() });
        r.platform().control_stack(sink.clone(), "start", false)?;
    }
    sink(Progress::Done { message: "Docker was restarted and answers.".into() });
    Ok(())
}

#[cfg(windows)]
fn ps(script: &str) -> Result<String, String> {
    let o = quiet("powershell.exe").args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script]).output().map_err(|e| e.to_string())?;
    if o.status.success() { Ok(String::from_utf8_lossy(&o.stdout).to_string()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().to_string()) }
}

/// The package manager's commands: (look, install), each a command line.
#[cfg(not(windows))]
fn package_commands() -> Option<(Vec<&'static str>, Vec<&'static str>)> {
    let have = |c: &str| ["/usr/bin", "/bin", "/usr/sbin", "/sbin"].iter().any(|d| std::path::Path::new(d).join(c).is_file());
    if have("apt-get") {
        Some((vec!["apt-get", "update"], vec!["apt-get", "-y", "-o", "Dpkg::Options::=--force-confdef", "-o", "Dpkg::Options::=--force-confold", "upgrade"]))
    } else if have("dnf") {
        Some((vec!["dnf", "-y", "makecache"], vec!["dnf", "-y", "upgrade"]))
    } else if have("pacman") {
        Some((vec!["pacman", "-Sy", "--noconfirm"], vec!["pacman", "-Su", "--noconfirm"]))
    } else if have("zypper") {
        Some((vec!["zypper", "-n", "refresh"], vec!["zypper", "-n", "update"]))
    } else {
        None
    }
}

fn system_updates(sink: &ProgressFn) -> Result<(), String> {
    #[cfg(not(windows))]
    {
        let (look, install) = package_commands().ok_or("No package manager this job knows (apt, dnf, pacman or zypper) is on this server.")?;
        let env = [("DEBIAN_FRONTEND", "noninteractive")];
        sink(Progress::Step { name: STEP_UPD_LOOK.into() });
        stream(look[0], &look[1..], &env, sink)?;
        sink(Progress::Step { name: STEP_UPD_INSTALL.into() });
        stream(install[0], &install[1..], &env, sink)?;
        sink(Progress::Step { name: STEP_UPD_CHECK.into() });
        let needed = std::path::Path::new("/var/run/reboot-required").exists();
        sink(Progress::Done { message: if needed { "The updates are installed. The server needs a restart to finish them.".into() } else { "The updates are installed.".into() } });
        Ok(())
    }
    #[cfg(windows)]
    {
        sink(Progress::Step { name: STEP_UPD_LOOK.into() });
        let n = ps(crate::server_info::WINDOWS_UPDATES_SCRIPT)?;
        sink(Progress::Log { line: format!("{} update(s) waiting", n.trim()) });
        sink(Progress::Step { name: STEP_UPD_INSTALL.into() });
        stream("powershell.exe", &["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", WINDOWS_INSTALL], &[], sink)?;
        sink(Progress::Step { name: STEP_UPD_CHECK.into() });
        let needed = ps("(Test-Path 'HKLM:\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\WindowsUpdate\\Auto Update\\RebootRequired')").unwrap_or_default().trim() == "True";
        sink(Progress::Done { message: if needed { "The updates are installed. The server needs a restart to finish them.".into() } else { "The updates are installed.".into() } });
        Ok(())
    }
}

/// Windows Update through its own COM interface: search, download, install.
#[cfg(windows)]
const WINDOWS_INSTALL: &str = "$ErrorActionPreference = 'Stop'; $s = New-Object -ComObject Microsoft.Update.Session; $r = $s.CreateUpdateSearcher().Search(\"IsInstalled=0 and IsHidden=0 and Type='Software'\"); if ($r.Updates.Count -eq 0) { 'No updates waiting'; exit 0 }; $c = New-Object -ComObject Microsoft.Update.UpdateColl; foreach ($u in $r.Updates) { if (-not $u.EulaAccepted) { $u.AcceptEula() }; [void]$c.Add($u); \"Update: $($u.Title)\" }; 'Downloading'; $d = $s.CreateUpdateDownloader(); $d.Updates = $c; [void]$d.Download(); 'Installing'; $i = $s.CreateUpdateInstaller(); $i.Updates = $c; $res = $i.Install(); \"Result code: $($res.ResultCode)\"; if ($res.ResultCode -gt 3) { exit 1 }";
