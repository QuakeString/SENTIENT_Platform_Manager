// Copyright © 2016-2026 The SENTIENT Authors

//! ClickHouse for a native Windows install, in Docker inside WSL2, owned by a
//! dedicated hidden account so it runs from boot with nobody logged in.
//!
//! Why an account of its own: WSL refuses to run as LocalSystem
//! (`Wsl/WSL_E_LOCAL_SYSTEM_NOT_SUPPORTED`), and a WSL distro belongs to the
//! Windows user who imported it and runs only in that user's session. A
//! service account with a stored password gives ClickHouse a session at boot
//! through Task Scheduler ("run whether the user is logged on or not"),
//! independent of who installed SENTIENT or who is logged in. Verified on
//! Windows 11 with WSL 2.7: the distro starts about half a minute after boot
//! and stays up in session 0.
//!
//! Work that must happen *inside* that account (importing the distro,
//! installing Docker, starting ClickHouse) is done by the Platform Manager
//! itself, re-launched as the account in agent mode: it writes its progress
//! as JSON lines to `job.log`, which [`run_as_account`] forwards to the caller.

#[cfg(windows)]
use std::path::{Path, PathBuf};

#[cfg(windows)]
use crate::progress::{Progress, ProgressFn};
#[cfg(windows)]
use crate::sys;

/// The hidden account that owns the ClickHouse distro.
pub const ACCOUNT: &str = "sentient-clickhouse";
/// Its boot-time task: starts Docker in the distro and holds the session open.
pub const KEEPALIVE_TASK: &str = "SENTIENT ClickHouse";
const JOB_TASK: &str = "SENTIENT ClickHouse job";

/// Shared between the installer and the account: config, logs, the distro.
#[cfg(windows)]
pub fn work_dir() -> PathBuf {
    std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join("SENTIENT")
        .join("clickhouse")
}

#[cfg(windows)]
fn powershell(script: &str, env: &[(&str, &str)]) -> Result<String, String> {
    let mut c = sys::command("powershell.exe");
    c.args(["-NoProfile", "-NonInteractive", "-Command", script]);
    for (k, v) in env {
        c.env(k, v);
    }
    let o = c.output().map_err(|e| format!("run powershell: {e}"))?;
    if o.status.success() {
        Ok(sys::decode(&o.stdout))
    } else {
        Err(sys::decode(&o.stderr).trim().to_string())
    }
}

/// A password Windows' complexity policy accepts: 40 random alphanumerics
/// plus one of each class.
#[cfg(windows)]
pub fn new_password() -> String {
    format!("{}aA1!", crate::native::generate_secret().chars().take(40).collect::<String>())
}

/// Create the account (or reset its password), hide it from the sign-in
/// screen, give it the "Log on as a batch job" right Task Scheduler needs, and
/// give it the work directory. Idempotent.
#[cfg(windows)]
pub fn ensure_account(sink: &ProgressFn, password: &str) -> Result<(), String> {
    sink(Progress::Step { name: "Preparing the ClickHouse service account".into() });
    let dir = work_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    // The password travels in an environment variable, not on a command line
    // another process could read.
    let script = format!(
        r#"$ErrorActionPreference = 'Stop'
$pw = ConvertTo-SecureString $env:SPM_ACCOUNT_PW -AsPlainText -Force
if (Get-LocalUser -Name '{ACCOUNT}' -ErrorAction SilentlyContinue) {{
  Set-LocalUser -Name '{ACCOUNT}' -Password $pw -PasswordNeverExpires $true
}} else {{
  New-LocalUser -Name '{ACCOUNT}' -Password $pw -PasswordNeverExpires -AccountNeverExpires `
    -Description 'SENTIENT ClickHouse (WSL2). Do not remove.' | Out-Null
}}
$k = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon\SpecialAccounts\UserList'
New-Item -Path $k -Force | Out-Null
New-ItemProperty -Path $k -Name '{ACCOUNT}' -Value 0 -PropertyType DWord -Force | Out-Null
$sid = (New-Object System.Security.Principal.NTAccount('{ACCOUNT}')).Translate([System.Security.Principal.SecurityIdentifier]).Value
$tmp = Join-Path $env:TEMP ('spm-rights-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
secedit /export /cfg "$tmp\in.inf" /areas USER_RIGHTS | Out-Null
$lines = Get-Content "$tmp\in.inf"
if (-not ($lines | Select-String -SimpleMatch "*$sid")) {{
  if ($lines | Select-String '^SeBatchLogonRight') {{
    $lines = $lines -replace '^(SeBatchLogonRight = .*)$', "`$1,*$sid"
  }} else {{
    $lines = $lines -replace '^\[Privilege Rights\]$', "[Privilege Rights]`r`nSeBatchLogonRight = *$sid"
  }}
  Set-Content "$tmp\out.inf" $lines -Encoding Unicode
  secedit /configure /db "$tmp\rights.sdb" /cfg "$tmp\out.inf" /areas USER_RIGHTS /quiet | Out-Null
}}
Remove-Item -Recurse -Force $tmp
icacls '{dir}' /grant '{ACCOUNT}:(OI)(CI)F' /T /Q | Out-Null
"ok"
"#,
        dir = dir.display(),
    );
    powershell(&script, &[("SPM_ACCOUNT_PW", password)]).map(|_| ())
}

/// Register (or replace) a Task Scheduler task that runs `cmd_file` as the
/// account, with its password stored by Task Scheduler.
#[cfg(windows)]
fn register_task(name: &str, cmd_file: &Path, at_startup: bool, password: &str) -> Result<(), String> {
    let schedule: &[&str] = if at_startup { &["/sc", "onstart"] } else { &["/sc", "once", "/st", "23:59"] };
    let tr = cmd_file.display().to_string();
    let mut args: Vec<&str> = vec!["/create", "/tn", name, "/tr", &tr];
    args.extend_from_slice(schedule);
    args.extend_from_slice(&["/ru", ACCOUNT, "/rp", password, "/f"]);
    match sys::output("schtasks.exe", &args) {
        Some((true, _, _)) => Ok(()),
        Some((false, out, err)) => Err(format!(
            "could not register the task {name}: {} {}",
            sys::decode(&err).trim(),
            sys::decode(&out).trim()
        )),
        None => Err("could not run schtasks.exe".into()),
    }
}

/// Run the Platform Manager (`exe`) in agent mode as the account and stream
/// its progress. `agent_args` are passed after `--clickhouse-agent`.
#[cfg(windows)]
/// `limit`: how long the job may run before it is ended and reported as
/// stuck (None: as long as it reports progress, for provisioning).
pub fn run_as_account(sink: &ProgressFn, password: &str, exe: &Path, agent_args: &str, limit: Option<std::time::Duration>) -> Result<(), String> {
    let started = std::time::Instant::now();
    let dir = work_dir();
    let log = dir.join("job.log");
    let done = dir.join("job.done");
    let _ = std::fs::remove_file(&log);
    let _ = std::fs::remove_file(&done);
    let cmd = dir.join("job.cmd");
    std::fs::write(
        &cmd,
        format!("@echo off\r\n\"{}\" --clickhouse-agent {agent_args}\r\n", exe.display()),
    )
    .map_err(|e| format!("write {}: {e}", cmd.display()))?;
    register_task(JOB_TASK, &cmd, false, password)?;
    let run = sys::output("schtasks.exe", &["/run", "/tn", JOB_TASK]);
    if !matches!(run, Some((true, _, _))) {
        let _ = sys::output("schtasks.exe", &["/delete", "/tn", JOB_TASK, "/f"]);
        return Err("could not start the ClickHouse job as the service account".into());
    }

    // Forward progress until the agent writes job.done (ok, or the error).
    let mut seen = 0usize;
    let forward = |seen: &mut usize| {
        if let Ok(text) = std::fs::read_to_string(&log) {
            for line in text.lines().skip(*seen) {
                match serde_json::from_str::<Progress>(line) {
                    // The agent's own Done is not the end of the caller's job.
                    Ok(Progress::Done { message }) => sink(Progress::Log { line: message }),
                    Ok(p) => sink(p),
                    Err(_) => sink(Progress::Log { line: line.to_string() }),
                }
                *seen += 1;
            }
        }
    };
    let result = loop {
        forward(&mut seen);
        if let Ok(outcome) = std::fs::read_to_string(&done) {
            forward(&mut seen);
            break if outcome.trim() == "ok" { Ok(()) } else { Err(outcome.trim().to_string()) };
        }
        if crate::cancel::is_cancelled() {
            let _ = sys::output("schtasks.exe", &["/end", "/tn", JOB_TASK]);
            break Err("Cancelled.".into());
        }
        if limit.is_some_and(|l| started.elapsed() > l) {
            let _ = sys::output("schtasks.exe", &["/end", "/tn", JOB_TASK]);
            break Err(format!(
                "the ClickHouse job ({agent_args}) did not finish within {} minutes and was stopped",
                limit.unwrap_or_default().as_secs() / 60
            ));
        }
        // The task never started, or the agent died without reporting.
        if let Some((true, out, _)) = sys::output("schtasks.exe", &["/query", "/tn", JOB_TASK, "/fo", "csv", "/nh"]) {
            let row = sys::decode(&out);
            if row.contains("\"Ready\"") && seen == 0 && !log.exists() {
                std::thread::sleep(std::time::Duration::from_secs(5));
                if !log.exists() && !done.exists() {
                    break Err("the ClickHouse job did not start as the service account (check its Log on as a batch job right)".into());
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(700));
    };
    let _ = sys::output("schtasks.exe", &["/delete", "/tn", JOB_TASK, "/f"]);
    let _ = std::fs::remove_file(&cmd);
    result
}

/// The agent's side: a progress sink that appends JSON lines to `job.log`,
/// and the final `job.done` marker.
#[cfg(windows)]
pub fn agent_sink() -> ProgressFn {
    let log = work_dir().join("job.log");
    std::sync::Arc::new(move |p: Progress| {
        use std::io::Write;
        if let Ok(line) = serde_json::to_string(&p) {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
                let _ = writeln!(f, "{line}");
            }
        }
    })
}

#[cfg(windows)]
pub fn agent_finish(result: &Result<(), String>) {
    let text = match result {
        Ok(()) => "ok".to_string(),
        Err(e) => e.clone(),
    };
    let _ = std::fs::write(work_dir().join("job.done"), text);
}

/// Start ClickHouse at every boot (and now), and stop it cleanly when asked.
///
/// The task holds the account's WSL session open (WSL shuts an idle distro
/// down) by waiting for `stop.request` in the work directory. When that file
/// appears (written by the shutdown guard service, see [`run_guard`], or by an
/// uninstall that keeps the data), ClickHouse is stopped with time to finish
/// its writes, the distro's page cache is flushed, and `stop.done` answers.
///
/// Without this, a Windows restart cut the WSL machine off with ClickHouse
/// still running: the newest parts of its tables were left empty, and after
/// more than 100 of them ClickHouse refused to start
/// (TOO_MANY_UNEXPECTED_DATA_PARTS).
///
/// The container runs `restart: unless-stopped`, so after a clean stop Docker
/// would not start it again at boot: the script starts it itself.
#[cfg(windows)]
pub fn register_keepalive(password: &str) -> Result<(), String> {
    let dir = work_dir();
    let script = dir.join("keepalive.sh");
    std::fs::write(&script, keepalive_script(&wsl_path(&dir))).map_err(|e| format!("write {}: {e}", script.display()))?;
    let cmd = dir.join("keepalive.cmd");
    std::fs::write(
        &cmd,
        format!(
            "@echo off\r\nwsl.exe -d {} -u root -- sh \"{}\"\r\n",
            crate::distro::DISTRO,
            wsl_path(&script)
        ),
    )
    .map_err(|e| format!("write {}: {e}", cmd.display()))?;
    register_task(KEEPALIVE_TASK, &cmd, true, password)?;
    let _ = sys::output("schtasks.exe", &["/run", "/tn", KEEPALIVE_TASK]);
    Ok(())
}

/// The keep-alive script, run as root in the distro. `dir` is the work
/// directory as the distro sees it (`/mnt/c/...`).
pub fn keepalive_script(dir: &str) -> String {
    format!(
        r#"#!/bin/sh
# SENTIENT ClickHouse keep-alive, written by the SENTIENT Platform Manager.
D='{dir}'
CH='label=com.docker.compose.service=clickhouse'
rm -f "$D/stop.request" "$D/stop.done"
systemctl start docker >/dev/null 2>&1
i=0
while [ $i -lt 90 ] && ! docker info >/dev/null 2>&1; do i=$((i + 1)); sleep 1; done
ids=$(docker ps -aq --filter "$CH")
[ -n "$ids" ] && docker start $ids >/dev/null 2>&1
while [ ! -e "$D/stop.request" ]; do sleep 2; done
ids=$(docker ps -q --filter "$CH")
[ -n "$ids" ] && docker stop -t 120 $ids >/dev/null 2>&1
sync
rm -f "$D/stop.request"
touch "$D/stop.done"
"#
    )
}

/// Run as root in the distro: bring an existing install's ClickHouse
/// configuration to this version's settings and restart ClickHouse when
/// anything changed (which also starts again a ClickHouse that an unclean
/// stop had left refusing a table).
///
/// - `max_suspicious_broken_parts` is added when missing (installs from
///   before the clean stop).
/// - The write-through (fsync) settings are removed, or added when `fsync`
///   ([`crate::stack::ChTuning::ch_fsync`]). Installs from 0.8.0-rc11 to
///   0.9.3 got them on Windows, and on a hard disk they kept the disk 100 %
///   busy. A copy of the file is kept as `<file>.bak-fsync`.
pub fn harden_config_script(config_path: &str, container: &str, fsync: bool) -> String {
    let fsync_lines: String = crate::stack::CH_FSYNC_SETTINGS.iter().map(|l| format!("\\n        {l}")).collect();
    let delete = crate::stack::CH_FSYNC_SETTINGS
        .iter()
        .map(|l| format!("/<{}>/d", &l[1..l.find('>').unwrap()]))
        .collect::<Vec<_>>()
        .join("; ");
    let fsync_step = if fsync {
        format!(
            r#"if ! grep -q fsync_after_insert "$F"; then
  sed -i 's#<max_suspicious_broken_parts>100000</max_suspicious_broken_parts>#&{fsync_lines}#' "$F"
  changed=1
fi"#
        )
    } else {
        format!(
            r#"if grep -q fsync "$F"; then
  cp "$F" "$F.bak-fsync"
  sed -i '{delete}' "$F"
  changed=1
fi"#
        )
    };
    format!(
        r#"F='{config_path}'
changed=0
systemctl start docker >/dev/null 2>&1
if ! grep -q max_suspicious_broken_parts "$F"; then
  sed -i 's#^<clickhouse>#<clickhouse>\n    <merge_tree>\n        <max_suspicious_broken_parts>100000</max_suspicious_broken_parts>\n    </merge_tree>#' "$F"
  changed=1
fi
{fsync_step}
[ "$changed" = 1 ] && docker restart -t 60 '{container}'
grep -q max_suspicious_broken_parts "$F"
"#
    )
}

/// The agent job that runs [`harden_config_script`]: `harden` leaves the
/// write-through off, `harden-fsync` turns it on (`SENTIENT_CH_FSYNC=1` in
/// the Platform Manager that starts the job; the job itself runs as the
/// ClickHouse account, which does not see that variable).
pub fn harden_job() -> &'static str {
    if crate::stack::fsync_from_env() { "harden-fsync" } else { "harden" }
}

/// `C:\ProgramData\SENTIENT\clickhouse` as WSL sees it:
/// `/mnt/c/ProgramData/SENTIENT/clickhouse`.
pub fn wsl_path(path: &std::path::Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    match s.as_bytes() {
        [d, b':', rest @ ..] if d.is_ascii_alphabetic() => {
            format!("/mnt/{}{}", (*d as char).to_ascii_lowercase(), String::from_utf8_lossy(rest))
        }
        _ => s,
    }
}

/// Ask the keep-alive task to stop ClickHouse cleanly and wait for it, up to
/// `limit`. `tick` is called about every second while waiting (the guard
/// service reports progress to Windows with it). Returns whether ClickHouse
/// confirmed the stop; true also when it was not running at all.
#[cfg(windows)]
pub fn stop_clickhouse(http_port: u16, limit: std::time::Duration, mut tick: impl FnMut(u32)) -> bool {
    if !crate::distro::clickhouse_answers(http_port) {
        return true;
    }
    let dir = work_dir();
    let done = dir.join("stop.done");
    let _ = std::fs::remove_file(&done);
    if std::fs::write(dir.join("stop.request"), b"stop").is_err() {
        return false;
    }
    let started = std::time::Instant::now();
    let mut n = 0;
    while started.elapsed() < limit {
        if done.exists() {
            let _ = std::fs::remove_file(&done);
            return true;
        }
        n += 1;
        tick(n);
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    false
}

/// The Windows service that stops ClickHouse before Windows shuts down.
pub const GUARD_SERVICE: &str = "SentientClickHouse";

/// How long Windows lets the guard work at shutdown before it moves on.
#[cfg(windows)]
const GUARD_PRESHUTDOWN_MS: u32 = 180_000;
/// How long the guard waits for ClickHouse (below the time Windows gives).
#[cfg(windows)]
const GUARD_STOP_LIMIT: std::time::Duration = std::time::Duration::from_secs(150);

/// Register (or update) and start the shutdown guard: a LocalSystem service
/// running `manager --clickhouse-guard <http_port>`. It does nothing until
/// Windows announces a shutdown (pre-shutdown, before other services and
/// WSL are stopped); then it stops ClickHouse cleanly through the keep-alive
/// task. Idempotent.
#[cfg(windows)]
pub fn install_guard(manager: &Path, http_port: u16) -> Result<(), String> {
    let bin = format!("\"{}\" --clickhouse-guard {http_port}", manager.display());
    let exists = sys::output("sc.exe", &["query", GUARD_SERVICE]).is_some_and(|(ok, _, _)| ok);
    let args: Vec<&str> = if exists {
        vec!["config", GUARD_SERVICE, "binPath=", &bin, "start=", "auto"]
    } else {
        vec!["create", GUARD_SERVICE, "binPath=", &bin, "start=", "auto", "DisplayName=", "SENTIENT ClickHouse"]
    };
    match sys::output("sc.exe", &args) {
        Some((true, _, _)) => {}
        Some((false, out, err)) => {
            return Err(format!("register the {GUARD_SERVICE} service: {} {}", sys::decode(&out).trim(), sys::decode(&err).trim()))
        }
        None => return Err("could not run sc.exe".into()),
    }
    let _ = sys::output(
        "sc.exe",
        &["description", GUARD_SERVICE, "Stops ClickHouse (SENTIENT's history database in WSL2) cleanly before Windows shuts down."],
    );
    guard_service::set_preshutdown_timeout(GUARD_PRESHUTDOWN_MS)?;
    let _ = sys::output("sc.exe", &["start", GUARD_SERVICE]);
    Ok(())
}

/// Stop ClickHouse cleanly (stopping the guard does that) and remove the
/// guard service. Safe when it is not installed.
#[cfg(windows)]
pub fn remove_guard(sink: &ProgressFn) {
    if !sys::output("sc.exe", &["query", GUARD_SERVICE]).is_some_and(|(ok, _, _)| ok) {
        return;
    }
    sink(Progress::Step { name: "Removing the ClickHouse shutdown service".into() });
    let _ = sys::output("sc.exe", &["stop", GUARD_SERVICE]);
    let started = std::time::Instant::now();
    while started.elapsed() < GUARD_STOP_LIMIT + std::time::Duration::from_secs(10) {
        match sys::output("sc.exe", &["query", GUARD_SERVICE]) {
            Some((true, out, _)) if !sys::decode(&out).contains("STOPPED") => std::thread::sleep(std::time::Duration::from_secs(1)),
            _ => break,
        }
    }
    let _ = sys::output("sc.exe", &["delete", GUARD_SERVICE]);
}

/// Uninstall keeping the data: stop ClickHouse cleanly, and keep it from
/// starting at boot (it held ports 8123 and 9000 after SENTIENT was gone,
/// so a new install found them taken). The distro, its data and the account
/// stay for a new install, which registers the boot task again.
#[cfg(windows)]
pub fn park(sink: &ProgressFn, http_port: u16) {
    sink(Progress::Step { name: "Stopping ClickHouse (its data is kept)".into() });
    remove_guard(sink);
    if !stop_clickhouse(http_port, GUARD_STOP_LIMIT, |_| {}) {
        sink(Progress::Log { line: "ClickHouse did not confirm a clean stop in time.".into() });
    }
    let _ = sys::output("schtasks.exe", &["/end", "/tn", KEEPALIVE_TASK]);
    let _ = sys::output("schtasks.exe", &["/change", "/tn", KEEPALIVE_TASK, "/disable"]);
}

/// End the hidden account's WSL machine. Removing the account does not stop
/// it: it kept ClickHouse's ports (8123, 9000) until Windows restarted. Its
/// processes go first; if the ports are still held, the WSL service is
/// restarted (which also stops this computer's other WSL distributions).
#[cfg(windows)]
pub fn stop_account_wsl(sink: &ProgressFn, ports: &[u16]) {
    sink(Progress::Step { name: "Stopping ClickHouse's WSL machine".into() });
    let ports = ports.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(",");
    let script = format!(
        r#"Get-Process -Name wsl, wslhost, wslrelay -IncludeUserName -ErrorAction SilentlyContinue |
  Where-Object {{ $_.UserName -like '*\{ACCOUNT}' }} | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep 2
$held = Get-NetTCPConnection -State Listen -LocalPort {ports} -ErrorAction SilentlyContinue |
  Where-Object {{ (Get-Process -Id $_.OwningProcess -ErrorAction SilentlyContinue).ProcessName -match '^(wsl|wslrelay|wslhost|wslservice|vmmem)' }}
if ($held) {{
  foreach ($s in 'WslService', 'LxssManager') {{ Restart-Service $s -Force -ErrorAction SilentlyContinue }}
  Start-Sleep 3
}}
[bool](Get-NetTCPConnection -State Listen -LocalPort {ports} -ErrorAction SilentlyContinue)"#
    );
    if powershell(&script, &[]).map(|o| o.trim().ends_with("True")).unwrap_or(false) {
        sink(Progress::Log { line: format!("Port {ports} is still in use by another program.") });
    }
}

/// The guard's side: `--clickhouse-guard <http_port>`, started by the
/// service control manager.
#[cfg(windows)]
pub fn run_guard(http_port: u16) -> i32 {
    guard_service::run(http_port)
}

#[cfg(windows)]
mod guard_service {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering};

    use windows_sys::Win32::Foundation::{ERROR_CALL_NOT_IMPLEMENTED, NO_ERROR};
    use windows_sys::Win32::System::Services::*;
    use windows_sys::core::PWSTR;

    use super::{GUARD_SERVICE, GUARD_STOP_LIMIT, KEEPALIVE_TASK};

    static PORT: AtomicU16 = AtomicU16::new(8123);
    static HANDLE: AtomicUsize = AtomicUsize::new(0);
    static STOP: AtomicBool = AtomicBool::new(false);

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn run(port: u16) -> i32 {
        PORT.store(port, Ordering::SeqCst);
        let mut name = wide(GUARD_SERVICE);
        let table = [
            SERVICE_TABLE_ENTRYW { lpServiceName: name.as_mut_ptr(), lpServiceProc: Some(service_main) },
            SERVICE_TABLE_ENTRYW { lpServiceName: std::ptr::null_mut(), lpServiceProc: None },
        ];
        // Returns when the service has stopped.
        if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
            eprintln!("--clickhouse-guard runs as the {GUARD_SERVICE} Windows service only");
            return 1;
        }
        0
    }

    fn report(state: SERVICE_STATUS_CURRENT_STATE, checkpoint: u32, wait_hint: u32) {
        let handle = HANDLE.load(Ordering::SeqCst);
        if handle == 0 {
            return;
        }
        let status = SERVICE_STATUS {
            dwServiceType: SERVICE_WIN32_OWN_PROCESS,
            dwCurrentState: state,
            dwControlsAccepted: if state == SERVICE_RUNNING { SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_PRESHUTDOWN } else { 0 },
            dwWin32ExitCode: NO_ERROR,
            dwServiceSpecificExitCode: 0,
            dwCheckPoint: checkpoint,
            dwWaitHint: wait_hint,
        };
        unsafe {
            SetServiceStatus(handle as SERVICE_STATUS_HANDLE, &status);
        }
    }

    unsafe extern "system" fn handler(control: u32, _event: u32, _data: *mut c_void, _ctx: *mut c_void) -> u32 {
        match control {
            SERVICE_CONTROL_STOP | SERVICE_CONTROL_PRESHUTDOWN => {
                STOP.store(true, Ordering::SeqCst);
                report(SERVICE_STOP_PENDING, 1, 10_000);
                NO_ERROR
            }
            SERVICE_CONTROL_INTERROGATE => NO_ERROR,
            _ => ERROR_CALL_NOT_IMPLEMENTED,
        }
    }

    unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
        let name = wide(GUARD_SERVICE);
        let handle = unsafe { RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(handler), std::ptr::null_mut()) };
        if handle.is_null() {
            return;
        }
        HANDLE.store(handle as usize, Ordering::SeqCst);
        report(SERVICE_RUNNING, 0, 0);
        // Started by hand after a stop: bring ClickHouse back. At boot the
        // task's own start trigger has usually run it already, and a second
        // run of a running task is ignored.
        let _ = crate::sys::output("schtasks.exe", &["/run", "/tn", KEEPALIVE_TASK]);
        while !STOP.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        let port = PORT.load(Ordering::SeqCst);
        super::stop_clickhouse(port, GUARD_STOP_LIMIT, |n| report(SERVICE_STOP_PENDING, n + 1, 10_000));
        report(SERVICE_STOPPED, 0, 0);
    }

    /// Windows' default pre-shutdown wait is not documented as stable; ask
    /// for enough time to stop ClickHouse.
    pub fn set_preshutdown_timeout(ms: u32) -> Result<(), String> {
        let name = wide(GUARD_SERVICE);
        unsafe {
            let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
            if scm.is_null() {
                return Err("open the service control manager".into());
            }
            let svc = OpenServiceW(scm, name.as_ptr(), SERVICE_CHANGE_CONFIG);
            if svc.is_null() {
                CloseServiceHandle(scm);
                return Err(format!("open the {GUARD_SERVICE} service"));
            }
            let mut info = SERVICE_PRESHUTDOWN_INFO { dwPreshutdownTimeout: ms };
            let ok = ChangeServiceConfig2W(svc, SERVICE_CONFIG_PRESHUTDOWN_INFO, &mut info as *mut _ as *const c_void);
            CloseServiceHandle(svc);
            CloseServiceHandle(scm);
            if ok == 0 {
                return Err(format!("set the {GUARD_SERVICE} shutdown time"));
            }
        }
        Ok(())
    }
}

/// Run as the hidden account before its first WSL start: no WSLg. With no
/// display in a service session its compositor (weston) crashes over and over,
/// each crash leaving a ~120 MB dump in the account's Temp folder.
#[cfg(windows)]
pub fn write_account_wslconfig() -> Result<(), String> {
    let home = std::env::var_os("USERPROFILE").ok_or("USERPROFILE is not set")?;
    let path = std::path::Path::new(&home).join(".wslconfig");
    std::fs::write(&path, "[wsl2]\r\nguiApplications=false\r\n").map_err(|e| format!("write {}: {e}", path.display()))
}

/// The hidden account exists: ClickHouse is (or was) set up on this machine,
/// whatever the installation uses now.
#[cfg(windows)]
pub fn is_installed() -> bool {
    powershell(&format!("[bool](Get-LocalUser -Name '{ACCOUNT}' -ErrorAction SilentlyContinue)"), &[])
        .map(|out| out.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// One-shot start-up task that removes profiles Windows still had loaded.
#[cfg(windows)]
const CLEANUP_TASK: &str = "SENTIENT ClickHouse cleanup";

/// Remove the boot task, the account and its profiles. The distro must
/// already be unregistered (agent `unregister`), since only the account can
/// do that. Returns true when everything is gone now; false when Windows
/// still holds a profile, which a one-shot task then removes at the next
/// start-up.
#[cfg(windows)]
pub fn remove_account(sink: &ProgressFn) -> bool {
    remove_guard(sink);
    sink(Progress::Step { name: "Removing the ClickHouse service account".into() });
    let _ = sys::output("schtasks.exe", &["/end", "/tn", KEEPALIVE_TASK]);
    let _ = sys::output("schtasks.exe", &["/delete", "/tn", KEEPALIVE_TASK, "/f"]);
    // Every profile the account ever had (a recreated account gets a new
    // SID and a new folder), not only the current one.
    let remove_profiles = format!(
        r#"$left = 0
Get-CimInstance Win32_UserProfile | Where-Object {{ (Split-Path $_.LocalPath -Leaf) -like '{ACCOUNT}*' }} | ForEach-Object {{
  if ($_.Loaded) {{ $left++ }} else {{ $_ | Remove-CimInstance -ErrorAction SilentlyContinue }}
}}
$left"#
    );
    let script = format!(
        r#"Remove-LocalUser -Name '{ACCOUNT}' -ErrorAction SilentlyContinue
{remove_profiles}"#
    );
    let left: u32 = powershell(&script, &[]).ok().and_then(|o| o.trim().lines().last()?.trim().parse().ok()).unwrap_or(0);
    if left == 0 {
        let _ = sys::output("schtasks.exe", &["/delete", "/tn", CLEANUP_TASK, "/f"]);
        return true;
    }
    // Loaded profiles cannot be removed until Windows lets go of them.
    let cmd_path = work_dir().join("cleanup.ps1");
    let body = format!("{remove_profiles}\nif ($left -eq 0) {{ schtasks /delete /tn '{CLEANUP_TASK}' /f | Out-Null }}\n");
    if std::fs::create_dir_all(work_dir()).is_ok() && std::fs::write(&cmd_path, body).is_ok() {
        let tr = format!("powershell.exe -NoProfile -ExecutionPolicy Bypass -File \"{}\"", cmd_path.display());
        let _ = sys::output("schtasks.exe", &["/create", "/tn", CLEANUP_TASK, "/sc", "onstart", "/ru", "SYSTEM", "/rl", "highest", "/tr", &tr, "/f"]);
        sink(Progress::Log { line: "Windows still holds the account's profile; it is removed at the next restart.".into() });
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_paths_become_wsl_paths() {
        assert_eq!(wsl_path(std::path::Path::new(r"C:\ProgramData\SENTIENT\clickhouse")), "/mnt/c/ProgramData/SENTIENT/clickhouse");
        assert_eq!(wsl_path(std::path::Path::new(r"D:\x y\keepalive.sh")), "/mnt/d/x y/keepalive.sh");
    }

    #[test]
    fn hardening_adds_the_settings_once() {
        let s = harden_config_script("/opt/sentient/clickhouse-sentient.xml", "sentient-clickhouse", false);
        assert!(s.contains("if ! grep -q max_suspicious_broken_parts"));
        assert!(s.contains(r"<clickhouse>\n    <merge_tree>"));
        assert!(s.contains("[ \"$changed\" = 1 ] && docker restart -t 60 'sentient-clickhouse'"));
    }

    /// Runs the script's sed and grep steps on real files (no docker).
    fn run_harden(xml: &str, fsync: bool) -> (String, bool) {
        let dir = std::env::temp_dir().join(format!("spm-harden-{}-{fsync}-{}", std::process::id(), xml.len()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("c.xml");
        std::fs::write(&f, xml).unwrap();
        let s = harden_config_script(f.to_str().unwrap(), "x", fsync)
            .replace("systemctl start docker", "true")
            .replace("docker restart -t 60 'x'", "echo RESTARTED");
        let out = std::process::Command::new("sh").arg("-c").arg(&s).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let restarted = String::from_utf8_lossy(&out.stdout).contains("RESTARTED");
        (std::fs::read_to_string(&f).unwrap(), restarted)
    }

    #[cfg(unix)]
    #[test]
    fn hardening_removes_the_write_through_from_older_installs() {
        let old = crate::stack::clickhouse_config_xml(&crate::stack::ChTuning { ch_fsync: true, ..Default::default() }, None, None);
        assert!(old.contains("fsync_after_insert"));
        let (new, restarted) = run_harden(&old, false);
        assert!(!new.contains("fsync"), "{new}");
        assert!(new.contains("<max_suspicious_broken_parts>100000</max_suspicious_broken_parts>\n    </merge_tree>"), "{new}");
        assert!(restarted);
        // Second run: nothing to do, no restart.
        let (again, restarted) = run_harden(&new, false);
        assert_eq!(again, new);
        assert!(!restarted);
        // Asked for: put back, inside <merge_tree>.
        let (on, restarted) = run_harden(&new, true);
        assert!(restarted);
        assert_eq!(on, old);
    }

    #[cfg(unix)]
    #[test]
    fn hardening_an_install_from_before_the_clean_stop() {
        let bare = "<?xml version=\"1.0\"?>\n<clickhouse>\n    <mark_cache_size>1</mark_cache_size>\n</clickhouse>\n";
        let (new, restarted) = run_harden(bare, false);
        assert!(restarted);
        assert!(new.contains("<merge_tree>\n        <max_suspicious_broken_parts>100000</max_suspicious_broken_parts>\n    </merge_tree>"), "{new}");
        assert!(!new.contains("fsync"));
    }

    #[test]
    fn keepalive_waits_for_the_stop_request_and_answers_after_a_clean_stop() {
        let s = keepalive_script("/mnt/c/ProgramData/SENTIENT/clickhouse");
        assert!(!s.contains('\r'), "the distro's sh needs LF line ends");
        let wait = s.find(r#"while [ ! -e "$D/stop.request" ]"#).unwrap();
        let stop = s.find("docker stop -t 120").unwrap();
        let sync = s.find("\nsync\n").unwrap();
        let done = s.find(r#"touch "$D/stop.done""#).unwrap();
        assert!(s.find("docker start").unwrap() < wait && wait < stop && stop < sync && sync < done);
        // A stop request left from the last shutdown must not stop it at once.
        assert!(s.find(r#"rm -f "$D/stop.request" "$D/stop.done""#).unwrap() < wait);
    }
}

// ---------------------------------------------------------------------------
// Moving ClickHouse's WSL disk (Move data to another disk)
// ---------------------------------------------------------------------------

/// Where the distro's disk (ext4.vhdx) was moved to, when it was; else the
/// folder Setup imported it into (`<work dir>\distro`).
#[cfg(windows)]
pub fn disk_dir() -> PathBuf {
    std::fs::read_to_string(work_dir().join("disk-location.txt"))
        .ok()
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| p.join("ext4.vhdx").exists())
        .unwrap_or_else(|| work_dir().join("distro"))
}

/// The request [`move_disk_here`] reads (the agent's command line carries no
/// paths: Windows' quoting would mangle them).
#[cfg(windows)]
pub fn move_request_path() -> PathBuf {
    work_dir().join("move-disk.txt")
}

#[cfg(windows)]
fn wsl_out(args: &[&str]) -> Result<String, String> {
    match sys::output("wsl.exe", args) {
        Some((true, out, _)) => Ok(sys::decode(&out)),
        Some((false, out, err)) => Err(format!("{} {}", sys::decode(&err).trim(), sys::decode(&out).trim()).trim().to_string()),
        None => Err("could not run wsl.exe".into()),
    }
}

#[cfg(windows)]
fn sha256_of(path: &Path) -> Result<String, String> {
    powershell(&format!("(Get-FileHash -Algorithm SHA256 -LiteralPath '{}').Hash", path.display().to_string().replace('\'', "''")), &[]).map(|s| s.trim().to_string())
}

/// Run as the account (agent job `move-disk`): move the distro's disk to the
/// folder in [`move_request_path`] with `wsl --manage sentient --move`, or by
/// export and import where WSL is older than 2.0, and check the file is the
/// same (SHA-256 before and after). The distro is shut down first; the
/// caller stopped ClickHouse cleanly before.
#[cfg(windows)]
pub fn move_disk_here(sink: &ProgressFn) -> Result<(), String> {
    let dest = std::fs::read_to_string(move_request_path()).map_err(|e| format!("read the move request: {e}"))?;
    let _ = std::fs::remove_file(move_request_path());
    let dest = PathBuf::from(dest.trim());
    let src_dir = disk_dir();
    let src = src_dir.join("ext4.vhdx");
    if !src.exists() {
        return Err(format!("ClickHouse's disk is not at {}", src.display()));
    }
    std::fs::create_dir_all(&dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
    sink(Progress::Step { name: "Shutting down the WSL distribution sentient".into() });
    let _ = wsl_out(&["--terminate", crate::distro::DISTRO]);
    sink(Progress::Step { name: format!("Copying ext4.vhdx to {} and comparing SHA-256", dest.display()) });
    let before = sha256_of(&src)?;
    sink(Progress::Log { line: format!("SHA-256 before: {before}") });
    sink(Progress::Step { name: "Pointing the distribution at the new file".into() });
    let d = dest.display().to_string();
    match wsl_out(&["--manage", crate::distro::DISTRO, "--move", &d]) {
        Ok(_) => {
            let after = sha256_of(&dest.join("ext4.vhdx"))?;
            sink(Progress::Log { line: format!("SHA-256 after: {after}") });
            if after != before {
                let back = src_dir.display().to_string();
                let _ = wsl_out(&["--manage", crate::distro::DISTRO, "--move", &back]);
                return Err("The moved disk did not match the original (SHA-256); it was moved back.".into());
            }
        }
        Err(e) if e.contains("--manage") || e.to_lowercase().contains("invalid command") || e.to_lowercase().contains("unknown") => {
            // WSL before 2.0: export, unregister, import at the new place;
            // the old place again if the import fails.
            sink(Progress::Log { line: "This WSL has no --manage --move: exporting and importing instead.".into() });
            let tar = dest.join("sentient-move.tar");
            let t = tar.display().to_string();
            wsl_out(&["--export", crate::distro::DISTRO, &t])?;
            wsl_out(&["--unregister", crate::distro::DISTRO])?;
            if let Err(e) = wsl_out(&["--import", crate::distro::DISTRO, &d, &t, "--version", "2"]) {
                let back = src_dir.display().to_string();
                let again = wsl_out(&["--import", crate::distro::DISTRO, &back, &t, "--version", "2"]);
                return Err(match again {
                    Ok(_) => format!("The import at {d} failed ({e}); the distribution is back at {back}."),
                    Err(e2) => format!("The import at {d} failed ({e}), and at {back} too ({e2}). The exported disk is kept as {t}."),
                });
            }
            let _ = std::fs::remove_file(&tar);
        }
        Err(e) => return Err(format!("wsl --manage --move failed: {e}")),
    }
    std::fs::write(work_dir().join("disk-location.txt"), dest.display().to_string()).map_err(|e| e.to_string())?;
    sink(Progress::Done { message: format!("ClickHouse's disk is in {}", dest.display()) });
    Ok(())
}
