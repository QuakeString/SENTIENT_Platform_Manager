// Copyright © 2016-2026 The SENTIENT Authors

//! The Sentient Gateway as a Docker container: crate::gateway's Docker side.
//!
//! The one the Platform Manager starts (Linux) is `sentient-gateway`, on the
//! host network, with the image's own environment contract (`host`, `port`,
//! `accessToken`) and named volumes for its configuration, data and logs, so
//! an update (a new tag started with the same settings) keeps everything.
//!
//! A gateway someone started from their own compose file (any container of
//! a gateway image, crate::gateway::is_gateway_image) is found too, on
//! Linux and on Windows when the Docker CLI is there: shown, started,
//! stopped and restarted, and updated through its compose file.

use crate::gateway::{CONTAINER, IMAGE, Settings, Status};
use crate::progress::{Progress, ProgressFn};

const VOLUMES: [(&str, &str); 3] = [
    ("st-gw-config", "/sentient_gateway/config"),
    ("st-gw-data", "/sentient_gateway/data"),
    ("st-gw-logs", "/sentient_gateway/logs"),
];

/// `docker <args>`: (success, stdout). Windows runs docker.exe when it is on
/// the PATH (Docker Desktop); Linux goes through the Docker-capable shell.
fn docker(args: &[&str]) -> Option<(bool, String)> {
    #[cfg(windows)]
    {
        crate::sys::output("docker", args).map(|(ok, out, _)| (ok, crate::sys::decode(&out)))
    }
    #[cfg(not(windows))]
    {
        let line: Vec<String> = args.iter().map(|a| super::shell_quote(a)).collect();
        super::run_output(&format!("docker {} 2>/dev/null", line.join(" ")))
    }
}

/// One container, from `docker inspect` (inspect_format).
#[derive(Debug, Clone, PartialEq)]
pub struct Container {
    pub name: String,
    /// The image it was created from, as named then (`.Config.Image`).
    pub image: String,
    /// `.Config.Entrypoint` as JSON (`["/usr/local/bin/sentient-gateway"]`).
    pub entrypoint: String,
}

impl Container {
    /// The Sentient Gateway: a gateway image of ours
    /// (crate::gateway::is_gateway_image), or an image built from the
    /// gateway's Dockerfile (its program is the entrypoint). Never by name.
    pub fn is_ours(&self) -> bool {
        crate::gateway::is_gateway_image(&self.image)
            || serde_json::from_str::<Vec<String>>(&self.entrypoint)
                .ok()
                .and_then(|e| e.first().cloned())
                .is_some_and(|p| crate::gateway::is_gateway_program(&p))
    }
}

const INSPECT_FORMAT: &str = "{{.Name}}\t{{.Config.Image}}\t{{json .Config.Entrypoint}}";

/// `docker inspect --format INSPECT_FORMAT` lines.
pub fn parse_inspect(out: &str) -> Vec<Container> {
    out.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let name = f.next()?.trim().trim_start_matches('/').to_string();
            let image = f.next()?.trim().to_string();
            let entrypoint = f.next().unwrap_or("null").trim().to_string();
            (!name.is_empty()).then_some(Container { name, image, entrypoint })
        })
        .collect()
}

/// The gateway among `all`: the one the Platform Manager starts, else the
/// first other one of ours. (name, managed)
pub fn pick(all: &[Container]) -> Option<(String, bool)> {
    let ours: Vec<&Container> = all.iter().filter(|c| c.is_ours()).collect();
    if ours.iter().any(|c| c.name == CONTAINER) {
        return Some((CONTAINER.to_string(), true));
    }
    ours.first().map(|c| (c.name.clone(), false))
}

/// Every container, running or stopped.
fn containers() -> Vec<Container> {
    let Some((true, ids)) = docker(&["ps", "-aq"]) else {
        return Vec::new();
    };
    let ids: Vec<&str> = ids.split_whitespace().collect();
    if ids.is_empty() {
        return Vec::new();
    }
    let mut args = vec!["inspect", "--format", INSPECT_FORMAT];
    args.extend(ids);
    match docker(&args) {
        // A container removed between the two calls fails the call but
        // prints the others.
        Some((_, out)) => parse_inspect(&out),
        None => Vec::new(),
    }
}

/// The gateway container: ours when it is there, else the first container
/// of the gateway.
fn find() -> Option<(String, bool)> {
    pick(&containers())
}

/// The gateway container's name, running or stopped.
pub fn container() -> Option<String> {
    find().map(|(n, _)| n)
}

pub fn status() -> Status {
    let Some((name, managed)) = find() else {
        return Status::default();
    };
    let inspect = docker(&["inspect", &name, "--format", "{{.State.Running}}\t{{.Config.Image}}\n{{range .Config.Env}}{{println .}}{{end}}"])
        .filter(|(ok, _)| *ok)
        .map(|(_, o)| o)
        .unwrap_or_default();
    let mut lines = inspect.lines();
    let (running, image) = lines.next().and_then(|h| h.split_once('\t')).map(|(r, i)| (r == "true", i.to_string())).unwrap_or_default();
    let env: Vec<String> = lines.map(str::to_string).collect();
    let get = |k: &str| env.iter().find_map(|l| l.strip_prefix(&format!("{k}=")).map(str::to_string));
    let version = running
        .then(|| docker(&["exec", &name, "sentient-gateway", "--version"]))
        .flatten()
        .filter(|(ok, _)| *ok)
        .and_then(|(_, o)| crate::gateway::parse_version(&o))
        .or_else(|| image.rsplit_once(':').map(|(_, t)| t.to_string()).filter(|t| t != "latest"));
    // Its gateway file, for the page's settings and (compose files often
    // commission through it) the platform host.
    let file = running
        .then(|| docker(&["exec", &name, "sh", "-c", "cat /sentient_gateway/config/st_gateway.json 2>/dev/null || cat /thingsboard_gateway/config/tb_gateway.json 2>/dev/null"]))
        .flatten()
        .filter(|(ok, _)| *ok)
        .map(|(_, o)| o);
    let (file_host, file_port) = file.as_deref().map(crate::gateway::host_of).unwrap_or((None, None));
    let page_url = crate::gateway::page_url(file.as_deref(), &|k| get(k), true);
    Status {
        installed: true,
        version,
        running,
        host: get("host").or(file_host),
        port: get("port").and_then(|p| p.parse().ok()).or(file_port),
        page_url,
        container: Some(name),
        managed,
        ..Status::default()
    }
}

pub fn install(sink: &ProgressFn, version: &str, settings: &Settings) -> Result<(), String> {
    start(sink, version, &settings.host, settings.port, &settings.token)
}

/// The new tag with the same settings and volumes (the container the
/// Platform Manager started; another one is updated through its compose file).
pub fn update(sink: &ProgressFn, version: &str) -> Result<(), String> {
    match find() {
        Some((_, true)) => {}
        Some((name, false)) => {
            return Err(format!(
                "The gateway container {name} was started from a compose file: change its image tag to {IMAGE}:{version} there and run docker compose up -d."
            ))
        }
        None => return Err("There is no gateway container to update.".into()),
    }
    let Some((true, out)) = docker(&["inspect", CONTAINER, "--format", "{{range .Config.Env}}{{println .}}{{end}}"]) else {
        return Err("The gateway container could not be read.".into());
    };
    let get = |k: &str| out.lines().find_map(|l| l.strip_prefix(&format!("{k}=")).map(str::to_string)).unwrap_or_default();
    let port = get("port").parse().unwrap_or(1883);
    start(sink, version, &get("host"), port, &get("accessToken"))
}

fn start(sink: &ProgressFn, version: &str, host: &str, port: u16, token: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        let _ = (sink, version, host, port, token);
        Err("On Windows the gateway is installed as a service; use the Windows installer.".into())
    }
    #[cfg(not(windows))]
    {
        let image = format!("{IMAGE}:{version}");
        sink(Progress::Step { name: format!("Pulling {image}") });
        super::run_stream(sink, &format!("docker pull {}", super::shell_quote(&image)))?;
        sink(Progress::Step { name: format!("Starting Sentient Gateway {version}") });
        let _ = super::run(sink, &format!("docker rm -f {CONTAINER} >/dev/null 2>&1 || true"));
        let volumes: String = VOLUMES.iter().map(|(v, p)| format!(" -v {v}:{p}")).collect();
        super::run(
            sink,
            &format!(
                "docker run -d --name {CONTAINER} --restart unless-stopped --network host{volumes} -e host={} -e port={port} -e accessToken={} {}",
                super::shell_quote(host),
                super::shell_quote(token),
                super::shell_quote(&image)
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
}

pub fn control(sink: &ProgressFn, action: &str) -> Result<(), String> {
    let verb = match action {
        "start" | "stop" | "restart" => action,
        _ => return Err(format!("unknown action {action}")),
    };
    let Some((name, _)) = find() else {
        return Err("There is no gateway container.".into());
    };
    match docker(&[verb, &name]) {
        Some((true, _)) => {
            sink(Progress::Done { message: format!("The gateway container {name} is {}.", if verb == "stop" { "stopped" } else { "running" }) });
            Ok(())
        }
        _ => Err(format!("docker {verb} {name} failed.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_gateway_container_by_image_or_program() {
        let out = "/sentient\tquakestring/sentient:4.3.0.58\t[\"/app/entrypoint.sh\"]\n\
/kong-gateway\tkong/kong-gateway:3.8\t[\"/entrypoint.sh\"]\n\
/st-gateway\tquakestring/sentient-gateway-rs:latest\t[\"/usr/local/bin/sentient-gateway\"]\n";
        let all = parse_inspect(out);
        assert_eq!(all.len(), 3);
        assert_eq!(pick(&all), Some(("st-gateway".into(), false)));
        // A locally built image: its entrypoint is the gateway's program.
        let local = parse_inspect("/plant-gw\tsha256:fdd9ed12884e\t[\"/usr/local/bin/sentient-gateway\"]\n");
        assert_eq!(pick(&local), Some(("plant-gw".into(), false)));
        // Named like a gateway, but not ours.
        let other = parse_inspect("/sentient-gateway\tnginx:1.27\t[\"/docker-entrypoint.sh\"]\n/journal-gatewayd\tdebian:12\tnull\n");
        assert_eq!(pick(&other), None);
        // Ours, started by the Platform Manager, wins.
        let both = parse_inspect("/st-gateway\tquakestring/sentient-gateway-rs:4.16.0\tnull\n/sentient-gateway\tquakestring/sentient-gateway-rs:4.16.2\tnull\n");
        assert_eq!(pick(&both), Some(("sentient-gateway".into(), true)));
    }
}
