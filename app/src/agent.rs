// Copyright © 2016-2026 The SENTIENT Authors

//! Agent mode: `--clickhouse-agent <job>`. The installer starts this program
//! as the hidden ClickHouse account (sentient_installer_core::clickhouse_wsl)
//! to do the work only that account can do, because a WSL distro belongs to
//! the user who imported it. No window: progress goes to the job log the
//! installer is reading.
//!
//! Jobs:
//! - `provision`: import the WSL distro, install Docker, start ClickHouse
//!   from `config.json` (a DeployConfig) in the work directory; from the
//!   offline pack's part the installer copied when `offline-pack.txt`
//!   names it (sentient_installer_core::offline).
//! - `unregister`: delete the distro and with it ClickHouse's data.
//! - `harden`: add the crash-safe ClickHouse settings to an install made
//!   before them, and restart ClickHouse.
//! - `move-disk`: move the distro's disk to another folder (Move data to
//!   another disk).

#[cfg(windows)]
pub fn run(job: &str) -> i32 {
    use sentient_installer_core::clickhouse_wsl::{agent_finish, agent_sink, work_dir};
    use sentient_installer_core::distro::{self, DeployConfig};

    let sink = agent_sink();
    let result = match job {
        "provision" | "provision-reuse" => (|| {
            if job == "provision-reuse" {
                // Read by distro::setup: take back a disk an earlier install left.
                std::env::set_var(distro::REUSE_KEPT_DISK, "1");
            }
            let path = work_dir().join("config.json");
            let text = std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
            // It holds the ClickHouse password: gone as soon as it is read.
            let _ = std::fs::remove_file(&path);
            let cfg: DeployConfig = serde_json::from_str(&text).map_err(|e| format!("config.json: {e}"))?;
            // An offline install: the installer copied the part of the pack
            // this account needs and named it here; offline mode on, so the
            // Ubuntu image, Docker and ClickHouse come from it.
            if let Ok(dir) = std::fs::read_to_string(sentient_installer_core::clickhouse_wsl::offline_marker()) {
                sentient_installer_core::offline::activate(std::path::Path::new(dir.trim()), false)?;
            }
            sentient_installer_core::clickhouse_wsl::write_account_wslconfig()?;
            distro::setup(sink.clone(), &work_dir())?;
            distro::deploy_clickhouse(sink.clone(), &cfg)
        })(),
        "unregister" => {
            let _ = std::process::Command::new("wsl.exe").args(["--terminate", distro::DISTRO]).output();
            match std::process::Command::new("wsl.exe").args(["--unregister", distro::DISTRO]).output() {
                Ok(o) if o.status.success() => Ok(()),
                Ok(o) => Err(String::from_utf8_lossy(&o.stderr).into_owned()),
                Err(e) => Err(e.to_string()),
            }
        }
        "harden" => {
            let cfg = DeployConfig::default();
            let script = sentient_installer_core::clickhouse_wsl::harden_config_script(&cfg.ch_config_path(), &cfg.names().ch);
            // A file, not `sh -c`: the script's quotes would not survive
            // Windows' command-line quoting.
            let file = work_dir().join("harden.sh");
            let ran = std::fs::write(&file, script).map_err(|e| e.to_string()).and_then(|()| {
                std::process::Command::new("wsl.exe")
                    .args(["-d", distro::DISTRO, "-u", "root", "--", "sh", &sentient_installer_core::clickhouse_wsl::wsl_path(&file)])
                    .output()
                    .map_err(|e| e.to_string())
            });
            let _ = std::fs::remove_file(&file);
            match ran {
                Ok(o) if o.status.success() => Ok(()),
                Ok(o) => Err(format!("the ClickHouse settings were not changed: {}", String::from_utf8_lossy(&o.stderr).trim())),
                Err(e) => Err(e),
            }
        }
        // Move data to another disk: the distro's disk to the folder in
        // the request file (crate::movedata_win).
        "move-disk" => sentient_installer_core::clickhouse_wsl::move_disk_here(&sink),
        other => Err(format!("unknown agent job {other}")),
    };
    agent_finish(&result);
    if result.is_ok() { 0 } else { 1 }
}

#[cfg(not(windows))]
pub fn run(_job: &str) -> i32 {
    eprintln!("the ClickHouse agent is Windows-only");
    2
}
