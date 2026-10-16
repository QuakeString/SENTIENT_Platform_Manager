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
pub fn run_as_account(sink: &ProgressFn, password: &str, exe: &Path, agent_args: &str) -> Result<(), String> {
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

/// Start ClickHouse at every boot (and now): Docker in the distro brings the
/// container back through its restart policy; `sleep infinity` holds the
/// session so WSL does not shut the distro down when idle.
#[cfg(windows)]
pub fn register_keepalive(password: &str) -> Result<(), String> {
    let cmd = work_dir().join("keepalive.cmd");
    std::fs::write(
        &cmd,
        format!(
            "@echo off\r\nwsl.exe -d {} -u root -- sh -c \"systemctl start docker >/dev/null 2>&1; exec sleep infinity\"\r\n",
            crate::distro::DISTRO
        ),
    )
    .map_err(|e| format!("write {}: {e}", cmd.display()))?;
    register_task(KEEPALIVE_TASK, &cmd, true, password)?;
    let _ = sys::output("schtasks.exe", &["/run", "/tn", KEEPALIVE_TASK]);
    Ok(())
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
