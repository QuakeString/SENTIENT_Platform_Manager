// Copyright © 2016-2026 The SENTIENT Authors

//! Agent mode: `--clickhouse-agent <job>`. The installer starts this program
//! as the hidden ClickHouse account (sentient_installer_core::clickhouse_wsl)
//! to do the work only that account can do, because a WSL distro belongs to
//! the user who imported it. No window: progress goes to the job log the
//! installer is reading.
//!
//! Jobs:
//! - `provision`: import the WSL distro, install Docker, start ClickHouse
//!   from `config.json` (a DeployConfig) in the work directory.
//! - `unregister`: delete the distro and with it ClickHouse's data.

#[cfg(windows)]
pub fn run(job: &str) -> i32 {
    use sentient_installer_core::clickhouse_wsl::{agent_finish, agent_sink, work_dir};
    use sentient_installer_core::distro::{self, DeployConfig};

    let sink = agent_sink();
    let result = match job {
        "provision" => (|| {
            let path = work_dir().join("config.json");
            let text = std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
            // It holds the ClickHouse password: gone as soon as it is read.
            let _ = std::fs::remove_file(&path);
            let cfg: DeployConfig = serde_json::from_str(&text).map_err(|e| format!("config.json: {e}"))?;
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
