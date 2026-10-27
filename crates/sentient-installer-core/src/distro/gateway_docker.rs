// Copyright © 2016-2026 The SENTIENT Authors

//! The Sentient Gateway as a Docker container (Linux): crate::gateway's
//! Docker side. One container, `sentient-gateway`, on the host network, with
//! the image's own environment contract (`host`, `port`, `accessToken`) and
//! named volumes for its configuration, data and logs, so an update (a new
//! tag started with the same settings) keeps everything.

use super::{run, run_output, run_stream, shell_quote};
use crate::gateway::{CONTAINER, IMAGE, Settings, Status};
use crate::progress::{Progress, ProgressFn};

const VOLUMES: [(&str, &str); 3] = [
    ("st-gw-config", "/sentient_gateway/config"),
    ("st-gw-data", "/sentient_gateway/data"),
    ("st-gw-logs", "/sentient_gateway/logs"),
];

pub fn status() -> Status {
    let Some((true, out)) = run_output(&format!(
        "docker inspect {CONTAINER} --format '{{{{.State.Running}}}}|{{{{.Config.Image}}}}|{{{{range .Config.Env}}}}{{{{println .}}}}{{{{end}}}}'"
    )) else {
        return Status::default();
    };
    let mut lines = out.lines();
    let head = lines.next().unwrap_or_default();
    let mut parts = head.splitn(2, '|');
    let running = parts.next() == Some("true");
    let image = parts.next().unwrap_or_default().split('|').next().unwrap_or_default().to_string();
    let env: Vec<String> = std::iter::once(head.splitn(3, '|').nth(2).unwrap_or_default().to_string())
        .chain(lines.map(str::to_string))
        .collect();
    let get = |k: &str| env.iter().find_map(|l| l.strip_prefix(&format!("{k}=")).map(str::to_string));
    let version = run_output(&format!("docker exec {CONTAINER} sentient-gateway --version"))
        .filter(|(ok, _)| *ok)
        .and_then(|(_, o)| crate::gateway::parse_version(&o))
        .or_else(|| image.rsplit_once(':').map(|(_, t)| t.to_string()));
    Status {
        installed: true,
        version,
        running,
        host: get("host"),
        port: get("port").and_then(|p| p.parse().ok()),
        page: false,
    }
}

pub fn install(sink: &ProgressFn, version: &str, settings: &Settings) -> Result<(), String> {
    start(sink, version, &settings.host, settings.port, &settings.token)
}

/// The new tag with the same settings and volumes.
pub fn update(sink: &ProgressFn, version: &str) -> Result<(), String> {
    let Some((true, out)) = run_output(&format!("docker inspect {CONTAINER} --format '{{{{range .Config.Env}}}}{{{{println .}}}}{{{{end}}}}'")) else {
        return Err("The gateway container is not there to update.".into());
    };
    let get = |k: &str| out.lines().find_map(|l| l.strip_prefix(&format!("{k}=")).map(str::to_string)).unwrap_or_default();
    let port = get("port").parse().unwrap_or(1883);
    start(sink, version, &get("host"), port, &get("accessToken"))
}

fn start(sink: &ProgressFn, version: &str, host: &str, port: u16, token: &str) -> Result<(), String> {
    let image = format!("{IMAGE}:{version}");
    sink(Progress::Step { name: format!("Pulling {image}") });
    run_stream(sink, &format!("docker pull {}", shell_quote(&image)))?;
    sink(Progress::Step { name: format!("Starting Sentient Gateway {version}") });
    let _ = run(sink, &format!("docker rm -f {CONTAINER} >/dev/null 2>&1 || true"));
    let volumes: String = VOLUMES.iter().map(|(v, p)| format!(" -v {v}:{p}")).collect();
    run(
        sink,
        &format!(
            "docker run -d --name {CONTAINER} --restart unless-stopped --network host{volumes} -e host={} -e port={port} -e accessToken={} {}",
            shell_quote(host),
            shell_quote(token),
            shell_quote(&image)
        ),
    )?;
    for _ in 0..30 {
        if status().running {
            sink(Progress::Done { message: format!("Sentient Gateway {version} is running.") });
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    Err(format!("The gateway container did not stay up; see docker logs {CONTAINER}."))
}

pub fn control(sink: &ProgressFn, action: &str) -> Result<(), String> {
    let verb = match action {
        "start" | "stop" | "restart" => action,
        _ => return Err(format!("unknown action {action}")),
    };
    run(sink, &format!("docker {verb} {CONTAINER}"))?;
    sink(Progress::Done { message: format!("The gateway container is {}.", if verb == "stop" { "stopped" } else { "running" }) });
    Ok(())
}
