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

/// One container, from `docker inspect` (INSPECT_FORMAT).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Container {
    pub name: String,
    /// The image it was created from, as named then (`.Config.Image`).
    pub image: String,
    /// `.Config.Entrypoint` as JSON (`["/usr/local/bin/sentient-gateway"]`).
    pub entrypoint: String,
    pub running: bool,
    /// The compose project and its folder (compose's labels), when it came
    /// from a compose file.
    pub project: Option<String>,
    pub working_dir: Option<String>,
    /// Its named volumes.
    pub volumes: Vec<String>,
    /// The image's `org.opencontainers.image.version` label.
    pub version_label: Option<String>,
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

const INSPECT_FORMAT: &str = "{{.Name}}\t{{.Config.Image}}\t{{json .Config.Entrypoint}}\t{{.State.Running}}\t{{index .Config.Labels \"com.docker.compose.project\"}}\t{{index .Config.Labels \"com.docker.compose.project.working_dir\"}}\t{{range .Mounts}}{{if eq .Type \"volume\"}}{{.Name}},{{end}}{{end}}\t{{index .Config.Labels \"org.opencontainers.image.version\"}}";

/// `docker inspect --format INSPECT_FORMAT` lines.
pub fn parse_inspect(out: &str) -> Vec<Container> {
    let some = |s: Option<&str>| s.map(str::trim).filter(|s| !s.is_empty() && *s != "<no value>").map(str::to_string);
    out.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let name = f.next()?.trim().trim_start_matches('/').to_string();
            let image = f.next()?.trim().to_string();
            let entrypoint = f.next().unwrap_or("null").trim().to_string();
            let running = f.next().is_some_and(|r| r.trim() == "true");
            let project = some(f.next());
            let working_dir = some(f.next());
            let volumes = f.next().unwrap_or_default().split(',').map(str::trim).filter(|v| !v.is_empty()).map(str::to_string).collect();
            let version_label = some(f.next());
            (!name.is_empty()).then_some(Container { name, image, entrypoint, running, project, working_dir, volumes, version_label })
        })
        .collect()
}

/// The gateway among `all`: the one the Platform Manager starts, else the
/// first other one of ours. (container, managed)
pub fn pick(all: &[Container]) -> Option<(Container, bool)> {
    let ours: Vec<&Container> = all.iter().filter(|c| c.is_ours()).collect();
    if let Some(c) = ours.iter().find(|c| c.name == CONTAINER) {
        return Some(((*c).clone(), true));
    }
    ours.first().map(|c| ((*c).clone(), false))
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
    find_container().map(|(c, m)| (c.name, m))
}

/// The gateway container, read in full: (container, managed).
pub fn find_container() -> Option<(Container, bool)> {
    pick(&containers())
}

/// The gateway container's name, running or stopped.
pub fn container() -> Option<String> {
    find().map(|(n, _)| n)
}

pub fn status() -> Status {
    let Some((c, managed)) = find_container() else {
        return Status::default();
    };
    let name = c.name.clone();
    let env: Vec<String> = docker(&["inspect", &name, "--format", "{{range .Config.Env}}{{println .}}{{end}}"])
        .filter(|(ok, _)| *ok)
        .map(|(_, o)| o.lines().map(str::to_string).collect())
        .unwrap_or_default();
    let get = |k: &str| env.iter().find_map(|l| l.strip_prefix(&format!("{k}=")).map(str::to_string));
    let running = c.running;
    let version = running
        .then(|| docker(&["exec", &name, "sentient-gateway", "--version"]))
        .flatten()
        .filter(|(ok, _)| *ok)
        .and_then(|(_, o)| crate::gateway::parse_version(&o))
        .or_else(|| image_version(&c));
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
        image: Some(c.image.clone()),
        compose_project: c.project.clone(),
        compose_dir: c.working_dir.clone(),
        volumes: c.volumes.clone(),
        ..Status::default()
    }
}

/// A stopped container's version: its image's tag (not `latest`), else
/// the image's version label.
pub fn image_version(c: &Container) -> Option<String> {
    let image = c.image.split('@').next().unwrap_or(&c.image);
    image
        .rsplit_once(':')
        .filter(|(_, t)| !t.contains('/'))
        .map(|(_, t)| t.to_string())
        .filter(|t| t != "latest" && t.chars().next().is_some_and(|ch| ch.is_ascii_digit()))
        .or_else(|| c.version_label.clone())
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

/// Remove the gateway container (only a container of ours, read again
/// here): stop it, remove it, then its named volumes unless `keep_data`
/// (a volume another container still uses is left), and its image when
/// `remove_image` and no other container uses it. A compose file it came
/// from is not changed.
pub fn remove(sink: &ProgressFn, keep_data: bool, remove_image: bool) -> Result<String, String> {
    let Some((c, _)) = find_container() else {
        return Err("There is no Sentient Gateway container.".into());
    };
    // pick() returns only ours; checked once more, the last line of defence.
    if !c.is_ours() {
        return Err(format!("The container {} is not the Sentient Gateway's; it was left alone.", c.name));
    }
    let name = c.name.clone();
    if c.running {
        sink(Progress::Step { name: format!("Stopping the container {name}") });
        match docker(&["stop", "-t", "25", &name]) {
            Some((true, _)) => {}
            _ => return Err(format!("docker stop {name} failed.")),
        }
    }
    sink(Progress::Step { name: format!("Removing the container {name}") });
    match docker(&["rm", &name]) {
        Some((true, _)) => {}
        _ => return Err(format!("docker rm {name} failed.")),
    }
    let mut kept = Vec::new();
    let mut notes = Vec::new();
    if keep_data {
        kept.extend(c.volumes.iter().cloned());
    } else {
        for v in &c.volumes {
            let users = docker(&["ps", "-aq", "--filter", &format!("volume={v}")]).map(|(_, o)| o.trim().to_string()).unwrap_or_default();
            if !users.is_empty() {
                notes.push(format!("the volume {v} is used by another container and was kept"));
                continue;
            }
            sink(Progress::Step { name: format!("Removing the volume {v}") });
            if !matches!(docker(&["volume", "rm", v]), Some((true, _))) {
                notes.push(format!("the volume {v} could not be removed"));
            }
        }
    }
    if remove_image {
        let users = docker(&["ps", "-aq", "--filter", &format!("ancestor={}", c.image)]).map(|(_, o)| o.trim().to_string()).unwrap_or_default();
        if users.is_empty() {
            sink(Progress::Step { name: format!("Removing the image {}", c.image) });
            if !matches!(docker(&["image", "rm", &c.image]), Some((true, _))) {
                notes.push(format!("the image {} could not be removed", c.image));
            }
        } else {
            notes.push(format!("the image {} is used by another container and was kept", c.image));
        }
    }
    let mut msg = format!("The Sentient Gateway container {name} is removed.");
    if !kept.is_empty() {
        msg.push_str(&format!(" Its configuration and data are kept in the volumes {}.", kept.join(", ")));
    }
    if let Some(dir) = &c.working_dir {
        msg.push_str(&format!(" Its compose file in {dir} is unchanged."));
    }
    if !notes.is_empty() {
        msg.push_str(&format!(" Note: {}.", notes.join("; ")));
    }
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(p: Option<(Container, bool)>) -> Option<(String, bool)> {
        p.map(|(c, m)| (c.name, m))
    }

    #[test]
    fn finds_the_gateway_container_by_image_or_program() {
        let out = "/sentient\tquakestring/sentient:4.3.0.58\t[\"/app/entrypoint.sh\"]\ttrue\t\t\t\t\n\
/kong-gateway\tkong/kong-gateway:3.8\t[\"/entrypoint.sh\"]\ttrue\t\t\t\t\n\
/st-gateway\tquakestring/sentient-gateway-rs:latest\t[\"/usr/local/bin/sentient-gateway\"]\tfalse\tdfal\t/home/mz/dfal\tst-gw-config,st-gw-data,st-gw-logs,\t<no value>\n";
        let all = parse_inspect(out);
        assert_eq!(all.len(), 3);
        let (c, managed) = pick(&all).unwrap();
        assert_eq!((c.name.as_str(), managed, c.running), ("st-gateway", false, false));
        assert_eq!(c.project.as_deref(), Some("dfal"));
        assert_eq!(c.working_dir.as_deref(), Some("/home/mz/dfal"));
        assert_eq!(c.volumes, vec!["st-gw-config", "st-gw-data", "st-gw-logs"]);
        assert_eq!(c.version_label, None);
        // A locally built image: its entrypoint is the gateway's program.
        let local = parse_inspect("/plant-gw\tsha256:fdd9ed12884e\t[\"/usr/local/bin/sentient-gateway\"]\ttrue\t\t\t\t4.16.2\n");
        assert_eq!(names(pick(&local)), Some(("plant-gw".into(), false)));
        assert_eq!(image_version(&local[0]).as_deref(), Some("4.16.2"));
        // Named like a gateway, but not ours.
        let other = parse_inspect("/sentient-gateway\tnginx:1.27\t[\"/docker-entrypoint.sh\"]\ttrue\n/journal-gatewayd\tdebian:12\tnull\tfalse\n");
        assert_eq!(names(pick(&other)), None);
        // Ours, started by the Platform Manager, wins.
        let both = parse_inspect("/st-gateway\tquakestring/sentient-gateway-rs:4.16.0\tnull\ttrue\n/sentient-gateway\tquakestring/sentient-gateway-rs:4.16.2\tnull\tfalse\n");
        assert_eq!(names(pick(&both)), Some(("sentient-gateway".into(), true)));
        assert_eq!(image_version(&both[0]).as_deref(), Some("4.16.0"));
        assert_eq!(image_version(&all[2]), None);
    }
}
