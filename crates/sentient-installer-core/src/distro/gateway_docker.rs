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
    /// The image's id (`.Image`, sha256:…).
    pub image_id: Option<String>,
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

    /// The older Python gateway (its image, and not the Rust gateway's
    /// program as the entrypoint: a local build tagged sentient-gateway is
    /// the Rust one). Never ours: only reported.
    pub fn is_legacy(&self) -> bool {
        crate::gateway::is_legacy_image(&self.image)
            && !serde_json::from_str::<Vec<String>>(&self.entrypoint)
                .ok()
                .and_then(|e| e.first().cloned())
                .is_some_and(|p| crate::gateway::is_gateway_program(&p))
    }
}

/// A serial adapter on this computer, for the gateway's container: its
/// stable path (`/dev/serial/by-id/…` when it has one) and the group that
/// may open it (the device node's gid: `dialout`, or `uucp` on Arch).
#[derive(Debug, Clone, PartialEq)]
pub struct SerialDevice {
    pub path: String,
    pub gid: u32,
}

/// The serial adapters: every `/dev/serial/by-id` link (stable across
/// reboots and re-plugging, where ttyUSB0/ttyUSB1 can swap), then the
/// `ttyUSB*`/`ttyACM*` nodes that no link names. `by_id` and `dev` are the
/// two folders (tests use their own).
#[cfg(unix)]
pub fn serial_devices_in(by_id: &std::path::Path, dev: &std::path::Path) -> Vec<SerialDevice> {
    use std::os::unix::fs::MetadataExt as _;
    let mut found = Vec::new();
    let mut named = std::collections::HashSet::new();
    let mut links: Vec<_> = std::fs::read_dir(by_id).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    links.sort();
    for link in links {
        // The device the link points at: its gid (metadata follows links).
        let (Ok(target), Ok(meta)) = (std::fs::canonicalize(&link), std::fs::metadata(&link)) else { continue };
        named.insert(target);
        found.push(SerialDevice { path: link.to_string_lossy().into_owned(), gid: meta.gid() });
    }
    let mut nodes: Vec<_> = std::fs::read_dir(dev)
        .map(|rd| {
            rd.flatten()
                .filter(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    n.strip_prefix("ttyUSB").or_else(|| n.strip_prefix("ttyACM")).is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()))
                })
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default();
    nodes.sort();
    for node in nodes {
        let real = std::fs::canonicalize(&node).unwrap_or_else(|_| node.clone());
        if named.contains(&real) {
            continue;
        }
        if let Ok(meta) = std::fs::metadata(&node) {
            found.push(SerialDevice { path: node.to_string_lossy().into_owned(), gid: meta.gid() });
        }
    }
    found
}

#[cfg(not(windows))]
pub(crate) fn serial_devices() -> Vec<SerialDevice> {
    serial_devices_in(std::path::Path::new("/dev/serial/by-id"), std::path::Path::new("/dev"))
}

/// The `docker run` line for the Platform Manager's gateway container:
/// the host network, the named volumes, the image's env contract read from
/// `env_file` (see [`env_lines`]: the access token never goes on a
/// command line, where the process list and an error message would show
/// it), and each serial adapter at the same path inside (a by-id path stays
/// the one a Modbus RTU connector names) with its group added, so the
/// image's non-root user may open it.
pub fn run_command(image: &str, env_file: &str, devices: &[SerialDevice]) -> String {
    let q = super::shell_quote;
    let volumes: String = VOLUMES.iter().map(|(v, p)| format!(" -v {v}:{p}")).collect();
    format!(
        "docker run -d --name {CONTAINER} --restart unless-stopped --network host --add-host {HOST_GATEWAY}{volumes}{} --env-file {} {}",
        device_flags(devices),
        q(env_file),
        q(image)
    )
}

/// The image's env contract (`host`, `port`, `accessToken`), the lines of
/// a Docker env file ([`write_env_file`]). Err: a value an env file cannot
/// carry (a line break).
pub fn env_lines(host: &str, port: u16, token: &str) -> Result<Vec<String>, String> {
    if host.contains(['\n', '\r']) || token.contains(['\n', '\r']) {
        return Err("The platform host and the access token are one line each.".into());
    }
    Ok(vec![format!("host={host}"), format!("port={port}"), format!("accessToken={token}")])
}

/// ` --device X:X … --group-add <gid> …` for the serial adapters.
pub(crate) fn device_flags(devices: &[SerialDevice]) -> String {
    let q = super::shell_quote;
    let mut devs = String::new();
    let mut gids: Vec<u32> = Vec::new();
    for d in devices {
        devs.push_str(&format!(" --device {}", q(&format!("{0}:{0}", d.path))));
        // root's group (0) is no group to add.
        if d.gid != 0 && !gids.contains(&d.gid) {
            gids.push(d.gid);
        }
    }
    let groups: String = gids.iter().map(|g| format!(" --group-add {g}")).collect();
    format!("{devs}{groups}")
}

const INSPECT_FORMAT: &str = "{{.Name}}\t{{.Config.Image}}\t{{json .Config.Entrypoint}}\t{{.State.Running}}\t{{index .Config.Labels \"com.docker.compose.project\"}}\t{{index .Config.Labels \"com.docker.compose.project.working_dir\"}}\t{{range .Mounts}}{{if eq .Type \"volume\"}}{{.Name}},{{end}}{{end}}\t{{index .Config.Labels \"org.opencontainers.image.version\"}}\t{{.Image}}";

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
            let image_id = some(f.next());
            (!name.is_empty()).then_some(Container { name, image, entrypoint, running, project, working_dir, volumes, version_label, image_id })
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

/// The first container of the older Python gateway (Container::is_legacy).
pub fn legacy_in(all: &[Container]) -> Option<String> {
    all.iter().find(|c| c.is_legacy()).map(|c| c.name.clone())
}

/// A container of the older Python gateway on this computer: its name.
pub fn legacy() -> Option<String> {
    legacy_in(&containers())
}

pub fn status() -> Status {
    let all = containers();
    let legacy = legacy_in(&all);
    let legacy_image = all.iter().find(|c| c.is_legacy()).map(|c| c.image.clone());
    let Some((c, managed)) = pick(&all) else {
        return Status { legacy, legacy_image, ..Status::default() };
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
        .or_else(|| image_version(&c))
        .or_else(|| c.image_id.as_deref().and_then(probed_version));
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
        legacy,
        legacy_image,
        ..Status::default()
    }
}

/// Where probed versions are kept between runs (the app sets its prefs
/// folder; unset: memory only).
static CACHE_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
/// Image id → version (None: the probe failed; not retried this run).
static PROBED: std::sync::Mutex<Option<std::collections::HashMap<String, Option<String>>>> = std::sync::Mutex::new(None);

pub fn set_cache_dir(dir: std::path::PathBuf) {
    let _ = CACHE_DIR.set(dir);
}

fn cache_file() -> Option<std::path::PathBuf> {
    CACHE_DIR.get().map(|d| d.join("gateway-image-versions.json"))
}

/// A gateway image's version when nothing else says it: `sentient-gateway
/// --version` in a throwaway container of the image (never the gateway's
/// own container, never pulled, no network, 10 seconds at most), once per
/// image id. Answers are kept in memory and in the cache file.
fn probed_version(image_id: &str) -> Option<String> {
    if !image_id.starts_with("sha256:") || image_id.len() < 20 {
        return None;
    }
    {
        let mut g = PROBED.lock().ok()?;
        let map = g.get_or_insert_with(|| {
            cache_file()
                .and_then(|f| std::fs::read_to_string(f).ok())
                .and_then(|t| serde_json::from_str::<std::collections::HashMap<String, String>>(&t).ok())
                .map(|m| m.into_iter().map(|(k, v)| (k, Some(v))).collect())
                .unwrap_or_default()
        });
        if let Some(v) = map.get(image_id) {
            return v.clone();
        }
    }
    let version = run_probe(image_id);
    if let Ok(mut g) = PROBED.lock() {
        let map = g.get_or_insert_with(Default::default);
        map.insert(image_id.to_string(), version.clone());
        if let Some(f) = cache_file() {
            let known: std::collections::HashMap<&String, &String> = map.iter().filter_map(|(k, v)| v.as_ref().map(|v| (k, v))).collect();
            if let Ok(t) = serde_json::to_string(&known) {
                let _ = f.parent().map(std::fs::create_dir_all);
                let _ = std::fs::write(f, t);
            }
        }
    }
    version
}

fn run_probe(image_id: &str) -> Option<String> {
    let name = format!("spm-gw-version-{}", &image_id[7..19]);
    let args = ["run", "--rm", "--pull=never", "--network", "none", "--name", &name, "--entrypoint", "sentient-gateway", image_id, "--version"];
    #[cfg(windows)]
    let out = crate::sys::output_limited("docker", &args, std::time::Duration::from_secs(10)).map(|(ok, o, _)| (ok, crate::sys::decode(&o)));
    #[cfg(not(windows))]
    let out = {
        let line: Vec<String> = args.iter().map(|a| super::shell_quote(a)).collect();
        super::run_output(&format!("timeout -k 2 10 docker {} 2>/dev/null", line.join(" ")))
    };
    // A probe cut short leaves its container behind.
    let _ = docker(&["rm", "-f", &name]);
    out.filter(|(ok, _)| *ok).and_then(|(_, o)| crate::gateway::parse_version(&o))
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
    match find_container() {
        Some((_, true)) => {}
        Some((c, false)) => {
            let s = Status { installed: true, container: Some(c.name.clone()), image: Some(c.image.clone()), compose_dir: c.working_dir.clone(), ..Status::default() };
            return Err(crate::gateway::update_blocker(&s, version, false, cfg!(windows)).unwrap_or_default());
        }
        None => return Err("There is no gateway container to update.".into()),
    }
    #[cfg(windows)]
    {
        let _ = sink;
        Err("On Windows the gateway is installed as a service; use the Windows installer.".into())
    }
    #[cfg(not(windows))]
    {
        let current = inspect_spec(CONTAINER).ok_or("The gateway container could not be read.")?;
        let image = format!("{IMAGE}:{version}");
        sink(Progress::Step { name: format!("Pulling {image}") });
        super::run_stream(sink, &format!("docker pull {}", super::shell_quote(&image)))?;
        let found: Vec<SerialDevice> = serial_devices();
        let spec = update_spec(&current, &image, &image_env(&current.image), &found);
        recreate_container(sink, &spec, None, &format!("Starting Sentient Gateway {version}"), &format!("on {image}"))?;
        sink(Progress::Done { message: format!("Sentient Gateway {version} is running.") });
        Ok(())
    }
}

/// The container `current` started again on `image` with what it has: its
/// volumes and host folders where they are mounted, its network, restart
/// policy and capabilities, and its own environment (the connection, the
/// ST_GW_/TB_GW_ settings, RUST_LOG …) without what its old image set
/// (PATH, the gateway's folders: the new image has its own). Its serial
/// devices stay as chosen (Gateway › Advanced); a container with none is
/// given the adapters found now (`found`), as an update always did.
pub fn update_spec(current: &Spec, image: &str, old_image_env: &[String], found: &[SerialDevice]) -> Spec {
    let image_owned = |kv: &str| {
        let Some((k, _)) = kv.split_once('=') else { return true };
        // A folder setting the image itself gives (its default path) goes,
        // so the new image's comes in; one given for this container stays.
        let dir = k.strip_prefix("TB_GW_").or_else(|| k.strip_prefix("ST_GW_")).is_some_and(|r| matches!(r, "CONFIG_DIR" | "DATA_DIR" | "LOGS_PATH" | "EXTENSIONS_DIR"))
            && kv.split_once('=').is_some_and(|(_, v)| v.starts_with("/sentient_gateway/") || v.starts_with("/thingsboard_gateway/"));
        k == "PATH" || dir || old_image_env.iter().any(|e| e == kv)
    };
    let mut spec = current.clone();
    spec.image = image.to_string();
    spec.env = current.env.iter().filter(|kv| !image_owned(kv)).cloned().collect();
    // A new image brings its own command and entry point.
    spec.cmd.clear();
    spec.entrypoint.clear();
    // Its data folder as the old container had it (an image-owned
    // ST_GW_DATA_DIR is gone from env by now: read it from the old spec).
    if !spec.mounts.iter().any(|m| m.1 == data_dir(current)) {
        let mut probe = current.clone();
        if ensure_data_volume(&mut probe).is_some() {
            spec.mounts.push((DATA_VOLUME.to_string(), data_dir(current), false, false));
        }
    }
    ensure_host_gateway(&mut spec);
    if spec.devices.is_empty() {
        spec.devices = found.iter().map(|d| (d.path.clone(), d.path.clone())).collect();
        for d in found {
            let g = d.gid.to_string();
            if d.gid != 0 && !spec.group_add.contains(&g) {
                spec.group_add.push(g);
            }
        }
    }
    spec
}

#[cfg(not(windows))]
fn wait_running(sink: &ProgressFn, version: &str) -> Result<(), String> {
    for _ in 0..30 {
        if status().running {
            sink(Progress::Done { message: format!("Sentient Gateway {version} is running.") });
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    Err(format!("The gateway container did not stay up; see docker logs {CONTAINER}."))
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
        let devices = serial_devices();
        if !devices.is_empty() {
            sink(Progress::Log { line: format!("Serial adapters for the gateway: {}", devices.iter().map(|d| d.path.as_str()).collect::<Vec<_>>().join(", ")) });
        }
        // The settings in a file only this account reads, for the one
        // `docker run`: Docker keeps them in the container's own
        // configuration (root's), and the file goes at once.
        let env = write_env_file(&env_lines(host, port, token)?)?;
        let r = super::run(sink, &run_command(&image, &env.to_string_lossy(), &devices));
        let _ = std::fs::remove_file(&env);
        r?;
        wait_running(sink, version)
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

// ---------------------------------------------------------------------------
// Settings (crate::gateway_settings): read, apply by recreating, password
// ---------------------------------------------------------------------------

/// A container's run settings, from `docker inspect` (the ones a recreate
/// keeps).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Spec {
    pub name: String,
    pub image: String,
    pub env: Vec<String>,
    /// The restart policy's name (no, always, unless-stopped, on-failure).
    pub restart: String,
    pub network: String,
    /// (on the host, in the container).
    pub devices: Vec<(String, String)>,
    pub group_add: Vec<String>,
    pub cap_add: Vec<String>,
    /// (volume name or host path, in the container, read-only, a bind).
    pub mounts: Vec<(String, String, bool, bool)>,
    /// Published ports as `docker run -p` takes them: `[ip:]host:container/proto`.
    pub ports: Vec<String>,
    /// `host:ip` entries (`docker run --add-host`), e.g. host.docker.internal:host-gateway.
    pub extra_hosts: Vec<String>,
    /// Other names on its network (a compose service's name, kept when the
    /// container is taken over).
    pub aliases: Vec<String>,
    /// Its fixed IPv4 address on its network (empty: Docker picks one).
    pub ip: String,
    /// The other networks it is on: (network, aliases, fixed IPv4 or "").
    pub extra_networks: Vec<(String, Vec<String>, String)>,
    /// A command and entry point given when it was created, when they
    /// differ from the image's (empty: the image's).
    pub cmd: Vec<String>,
    pub entrypoint: Vec<String>,
    pub user: String,
    pub privileged: bool,
    /// Labels given when it was created (not the image's, not compose's).
    pub labels: Vec<(String, String)>,
    /// A log driver other than the default, and its options.
    pub log_driver: String,
    pub log_opts: Vec<(String, String)>,
    /// tmpfs mounts: (path, options).
    pub tmpfs: Vec<(String, String)>,
    /// Memory limit in bytes and CPUs in billionths (0: none).
    pub memory: u64,
    pub nano_cpus: u64,
}

/// One container's `docker inspect` (an array of one).
pub fn parse_spec(json: &str) -> Option<Spec> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let c = v.as_array().and_then(|a| a.first()).cloned().unwrap_or(v);
    let strs = |v: Option<&serde_json::Value>| -> Vec<String> { v.and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()).unwrap_or_default() };
    let host = c.get("HostConfig").cloned().unwrap_or_default();
    let devices = host
        .get("Devices")
        .and_then(|d| d.as_array())
        .map(|a| a.iter().filter_map(|d| Some((d.get("PathOnHost")?.as_str()?.to_string(), d.get("PathInContainer")?.as_str()?.to_string()))).collect())
        .unwrap_or_default();
    let mounts = c
        .get("Mounts")
        .and_then(|m| m.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    let kind = m.get("Type")?.as_str()?;
                    let dest = m.get("Destination")?.as_str()?.to_string();
                    let rw = m.get("RW").and_then(|r| r.as_bool()).unwrap_or(true);
                    match kind {
                        "volume" => Some((m.get("Name")?.as_str()?.to_string(), dest, !rw, false)),
                        "bind" => Some((m.get("Source")?.as_str()?.to_string(), dest, !rw, true)),
                        _ => None,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let mut ports: Vec<String> = Vec::new();
    if let Some(b) = host.get("PortBindings").and_then(|b| b.as_object()) {
        for (inside, list) in b {
            for e in list.as_array().into_iter().flatten() {
                let hp = e.get("HostPort").and_then(|x| x.as_str()).unwrap_or_default();
                let ip = e.get("HostIp").and_then(|x| x.as_str()).unwrap_or_default();
                if hp.is_empty() {
                    ports.push(inside.clone());
                } else if ip.is_empty() {
                    ports.push(format!("{hp}:{inside}"));
                } else {
                    ports.push(format!("{ip}:{hp}:{inside}"));
                }
            }
        }
    }
    ports.sort();
    let name = c.get("Name")?.as_str()?.trim_start_matches('/').to_string();
    // Names given on its own network (a taken-over compose service's), not
    // its own name or id, which Docker lists there too.
    let network = host.get("NetworkMode").and_then(|x| x.as_str()).unwrap_or_default();
    let id = c.get("Id").and_then(|x| x.as_str()).unwrap_or_default();
    let mut aliases: Vec<String> = c
        .get("NetworkSettings")
        .and_then(|n| n.get("Networks"))
        .and_then(|n| n.get(network))
        .map(|n| strs(n.get("Aliases")))
        .unwrap_or_default()
        .into_iter()
        .filter(|a| *a != name && !(a.len() >= 12 && id.starts_with(a.as_str())))
        .collect();
    aliases.dedup();
    let nets = c.get("NetworkSettings").and_then(|n| n.get("Networks")).and_then(|n| n.as_object()).cloned().unwrap_or_default();
    let ipv4 = |n: &serde_json::Value| n.get("IPAMConfig").and_then(|i| i.get("IPv4Address")).and_then(|x| x.as_str()).unwrap_or_default().to_string();
    let ip = nets.get(network).map(ipv4).unwrap_or_default();
    let extra_networks: Vec<(String, Vec<String>, String)> = nets
        .iter()
        .filter(|(k, _)| k.as_str() != network && !matches!(network, "host" | "none"))
        .map(|(k, n)| {
            let al: Vec<String> = strs(n.get("Aliases")).into_iter().filter(|a| *a != name && !(a.len() >= 12 && id.starts_with(a.as_str()))).collect();
            (k.clone(), al, ipv4(n))
        })
        .collect();
    let config = c.get("Config").cloned().unwrap_or_default();
    let kv = |v: Option<&serde_json::Value>| -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = v.and_then(|x| x.as_object()).map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string())).collect()).unwrap_or_default();
        out.sort();
        out
    };
    let log = host.get("LogConfig").cloned().unwrap_or_default();
    let log_driver = log.get("Type").and_then(|x| x.as_str()).unwrap_or_default().to_string();
    let log_opts = kv(log.get("Config"));
    Some(Spec {
        ip,
        extra_networks,
        cmd: strs(config.get("Cmd")),
        entrypoint: strs(config.get("Entrypoint")),
        user: config.get("User").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
        privileged: host.get("Privileged").and_then(|x| x.as_bool()).unwrap_or(false),
        labels: kv(config.get("Labels")).into_iter().filter(|(k, _)| !k.starts_with("com.docker.compose.")).collect(),
        log_driver: if log_driver == "json-file" && log_opts.is_empty() { String::new() } else { log_driver },
        log_opts,
        tmpfs: kv(host.get("Tmpfs")),
        memory: host.get("Memory").and_then(|x| x.as_u64()).unwrap_or(0),
        nano_cpus: host.get("NanoCpus").and_then(|x| x.as_u64()).unwrap_or(0),
        ports,
        extra_hosts: strs(host.get("ExtraHosts")),
        aliases,
        name,
        image: c.get("Config").and_then(|x| x.get("Image")).and_then(|x| x.as_str()).unwrap_or_default().to_string(),
        env: strs(c.get("Config").and_then(|x| x.get("Env"))),
        restart: host.get("RestartPolicy").and_then(|r| r.get("Name")).and_then(|x| x.as_str()).unwrap_or_default().to_string(),
        network: host.get("NetworkMode").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
        devices,
        group_add: strs(host.get("GroupAdd")),
        cap_add: strs(host.get("CapAdd")),
        mounts,
    })
}

/// The `docker run` arguments that start `spec` again, its environment
/// from `env_file` (no value on a command line).
pub fn run_args(spec: &Spec, env_file: &str) -> Vec<String> {
    let mut a: Vec<String> = vec!["run".into(), "-d".into(), "--name".into(), spec.name.clone()];
    if !spec.restart.is_empty() && spec.restart != "no" {
        a.extend(["--restart".into(), spec.restart.clone()]);
    }
    if !spec.network.is_empty() && spec.network != "default" {
        a.extend(["--network".into(), spec.network.clone()]);
    }
    // Names on a network of its own only: the host and default bridge
    // networks take none.
    if !matches!(spec.network.as_str(), "" | "default" | "bridge" | "host" | "none") {
        for n in &spec.aliases {
            a.extend(["--network-alias".into(), n.clone()]);
        }
    }
    // A host-network container publishes nothing.
    if spec.network != "host" {
        for p in &spec.ports {
            a.extend(["-p".into(), p.clone()]);
        }
    }
    for h in &spec.extra_hosts {
        a.extend(["--add-host".into(), h.clone()]);
    }
    for (src, dest, ro, _) in &spec.mounts {
        a.extend(["-v".into(), format!("{src}:{dest}{}", if *ro { ":ro" } else { "" })]);
    }
    for (h, c) in &spec.devices {
        a.extend(["--device".into(), format!("{h}:{c}")]);
    }
    for g in &spec.group_add {
        a.extend(["--group-add".into(), g.clone()]);
    }
    for c in &spec.cap_add {
        a.extend(["--cap-add".into(), c.clone()]);
    }
    if !spec.ip.is_empty() && !matches!(spec.network.as_str(), "" | "default" | "bridge" | "host" | "none") {
        a.extend(["--ip".into(), spec.ip.clone()]);
    }
    if !spec.user.is_empty() {
        a.extend(["--user".into(), spec.user.clone()]);
    }
    if spec.privileged {
        a.push("--privileged".into());
    }
    for (k, v) in &spec.labels {
        a.extend(["--label".into(), format!("{k}={v}")]);
    }
    if !spec.log_driver.is_empty() {
        a.extend(["--log-driver".into(), spec.log_driver.clone()]);
    }
    for (k, v) in &spec.log_opts {
        a.extend(["--log-opt".into(), format!("{k}={v}")]);
    }
    for (p, o) in &spec.tmpfs {
        a.extend(["--tmpfs".into(), if o.is_empty() { p.clone() } else { format!("{p}:{o}") }]);
    }
    if spec.memory > 0 {
        a.extend(["--memory".into(), spec.memory.to_string()]);
    }
    if spec.nano_cpus > 0 {
        a.extend(["--cpus".into(), format!("{}", spec.nano_cpus as f64 / 1e9)]);
    }
    if let Some((first, _)) = spec.entrypoint.split_first() {
        a.extend(["--entrypoint".into(), first.clone()]);
    }
    a.extend(["--env-file".into(), env_file.to_string(), spec.image.clone()]);
    // The rest of a given entry point, then a given command.
    if spec.entrypoint.len() > 1 {
        a.extend(spec.entrypoint[1..].iter().cloned());
    }
    a.extend(spec.cmd.iter().cloned());
    a
}

/// What becomes of the compose file a taken-over container came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeChoice {
    /// Its service is commented out (a copy of the file is kept beside it),
    /// so `docker compose up` there no longer starts a second gateway.
    CommentOut,
    /// The file stays as it is; the job log says what to change.
    WarnOnly,
}

/// The compose service, its files and project, from a container's labels.
fn compose_of(name: &str) -> (Option<String>, Vec<String>, Option<String>) {
    let f = "{{index .Config.Labels \"com.docker.compose.service\"}}\t{{index .Config.Labels \"com.docker.compose.project.config_files\"}}\t{{index .Config.Labels \"com.docker.compose.project\"}}";
    let Some((true, out)) = docker(&["inspect", "--format", f, name]) else { return (None, Vec::new(), None) };
    let mut it = out.trim().split('\t').map(str::trim);
    let some = |s: Option<&str>| s.filter(|s| !s.is_empty() && *s != "<no value>").map(str::to_string);
    let service = some(it.next());
    let files = some(it.next()).map(|f| f.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()).unwrap_or_default();
    (service, files, some(it.next()))
}

/// `yaml` with the compose service `service` commented out (a note line
/// above it), or None when the file has no such service under `services:`.
/// Text only: the file's other lines, comments and order stay.
pub fn comment_out_service(yaml: &str, service: &str, note: &str) -> Option<String> {
    let lines: Vec<&str> = yaml.lines().collect();
    let indent = |l: &str| l.len() - l.trim_start().len();
    let blank = |l: &str| l.trim().is_empty() || l.trim_start().starts_with('#');
    let services = lines.iter().position(|l| indent(l) == 0 && l.trim_end().trim_end_matches(|c: char| c.is_whitespace()).starts_with("services:"))?;
    // The service's key line: the first indent inside services: is the
    // services' own.
    let mut svc_indent = None;
    let mut start = None;
    for (i, l) in lines.iter().enumerate().skip(services + 1) {
        if blank(l) {
            continue;
        }
        let n = indent(l);
        if n == 0 {
            break;
        }
        let level = *svc_indent.get_or_insert(n);
        if n != level {
            continue;
        }
        let key = l.trim().split('#').next().unwrap_or_default().trim_end();
        let key = key.trim_end_matches(':').trim().trim_matches(|c| c == '"' || c == '\'');
        if key == service && l.trim().split('#').next().unwrap_or_default().trim_end().ends_with(':') {
            start = Some(i);
            break;
        }
    }
    let start = start?;
    let level = indent(lines[start]);
    // To the next line at or above the service's indent (blank lines and
    // comments after its last line stay outside the block).
    let mut end = start + 1;
    let mut last = start;
    while end < lines.len() {
        let l = lines[end];
        if !blank(l) {
            if indent(l) <= level {
                break;
            }
            last = end;
        }
        end += 1;
    }
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 1);
    out.extend(lines[..start].iter().map(|l| l.to_string()));
    out.push(format!("{}# {note}", " ".repeat(level)));
    for l in &lines[start..=last] {
        out.push(if l.trim().is_empty() { l.to_string() } else { format!("{}# {}", " ".repeat(level), &l[level.min(indent(l))..]) });
    }
    out.extend(lines[last + 1..].iter().map(|l| l.to_string()));
    let mut text = out.join("\n");
    if yaml.ends_with('\n') {
        text.push('\n');
    }
    Some(text)
}

/// "Take it over": the gateway container started outside the Platform
/// Manager (a compose project's, or by hand) becomes the Platform Manager's
/// own `sentient-gateway`, with everything it runs with: image, environment,
/// volumes and host folders, network (and its compose service name there),
/// published ports, extra hosts, devices, restart policy and capabilities.
/// Its data stays in its volumes; nothing is deleted. The old container is
/// kept, stopped, until the new one runs, and started again when it does
/// not. Its compose service is then commented out (`choice`), so the
/// project does not start a second gateway on the same volumes. `name`: the
/// container the person confirmed (else the gateway container found).
pub fn take_over(sink: &ProgressFn, choice: ComposeChoice, name: Option<&str>) -> Result<String, String> {
    #[cfg(windows)]
    {
        let _ = (sink, choice, name);
        Err("On Windows the Platform Manager manages the gateway as a Windows service, not in Docker.".into())
    }
    #[cfg(not(windows))]
    {
        let (c, managed) = match name {
            Some(n) => {
                let c = containers().into_iter().find(|c| c.name == n).ok_or_else(|| format!("There is no container {n}."))?;
                if !c.is_ours() {
                    return Err(format!("{n} is not a Sentient Gateway container."));
                }
                let managed = c.name == CONTAINER;
                (c, managed)
            }
            None => find_container().ok_or("There is no gateway container to take over.")?,
        };
        if managed {
            return Ok(format!("The gateway already runs as {CONTAINER}, managed here."));
        }
        if docker(&["inspect", "--format", "{{.Name}}", CONTAINER]).is_some_and(|(ok, _)| ok) {
            return Err(format!("A container named {CONTAINER} is there already: remove it first."));
        }
        let old = c.name.clone();
        sink(Progress::Step { name: format!("Reading {old}") });
        let current = inspect_spec(&old).ok_or_else(|| format!("The container {old} could not be read."))?;
        let (service, files, project) = compose_of(&old);
        let mut spec = Spec { name: CONTAINER.to_string(), ..current.clone() };
        if let Some(s) = &service {
            spec.aliases.push(s.clone());
        }
        // Its old name stays a name on its network: other containers that
        // reach the gateway by it still do.
        if !spec.aliases.contains(&old) {
            spec.aliases.push(old.clone());
        }
        if let Some(added) = ensure_data_volume(&mut spec) {
            sink(Progress::Log { line: added });
        }
        if ensure_host_gateway(&mut spec) {
            sink(Progress::Log { line: format!("{HOST_ALIAS} is this machine in the container (--add-host {HOST_GATEWAY}).") });
        }
        sink(Progress::Log { line: format!("Image {}", spec.image) });
        if !spec.network.is_empty() {
            sink(Progress::Log { line: format!("Network {}", spec.network) });
        }
        for m in &spec.mounts {
            sink(Progress::Log { line: format!("{} {} → {}", if m.3 { "Folder" } else { "Volume" }, m.0, m.1) });
        }
        for p in &spec.ports {
            sink(Progress::Log { line: format!("Port {p}") });
        }
        let env_file = write_env_file(&spec.env)?;
        let file = env_file.display().to_string();
        let kept = format!("{old}-spm-takeover");
        let result = (|| {
            sink(Progress::Step { name: format!("Stopping {old}") });
            let _ = docker(&["rm", "-f", &kept]);
            if !matches!(docker(&["stop", "-t", "25", &old]), Some((true, _))) {
                return Err(format!("docker stop {old} failed: nothing was changed."));
            }
            // Kept under another name until the new one runs: put back as
            // it was when it does not.
            if !matches!(docker(&["rename", &old, &kept]), Some((true, _))) {
                let _ = docker(&["start", &old]);
                return Err(format!("docker rename {old} failed; {old} was started again as it was."));
            }
            let put_back = |why: String| -> Result<(), String> {
                sink(Progress::Log { line: format!("{old} is put back and started again.") });
                let _ = docker(&["rm", "-f", CONTAINER]);
                let _ = docker(&["rename", &kept, &old]);
                let _ = docker(&["start", &old]);
                Err(why)
            };
            sink(Progress::Step { name: format!("Starting {CONTAINER}") });
            let args = run_args(&spec, &file);
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            match docker_all(&refs) {
                Some((true, _)) => {
                    if let Err(e) = connect_extra_networks(&spec) {
                        return put_back(format!("{e} {old} runs again, unchanged."));
                    }
                }
                Some((false, out)) => {
                    for l in out.lines().filter(|l| !l.trim().is_empty()).take(6) {
                        sink(Progress::Log { line: strip_ansi(l) });
                    }
                    return put_back(format!("{CONTAINER} could not be started with {old}'s settings (the log says why); {old} runs again, unchanged."));
                }
                None => return put_back("Docker could not be run; the old container runs again, unchanged.".into()),
            }
            let up = (0..30).any(|_| {
                std::thread::sleep(std::time::Duration::from_secs(1));
                container_running(CONTAINER)
            }) && {
                std::thread::sleep(std::time::Duration::from_secs(4));
                container_running(CONTAINER)
            };
            if !up {
                sink(Progress::Log { line: format!("{CONTAINER} did not stay up; its last lines:") });
                if let Some((_, out)) = docker_all(&["logs", "--tail", "15", CONTAINER]) {
                    for l in out.lines() {
                        sink(Progress::Log { line: strip_ansi(l) });
                    }
                }
                return put_back(format!("{CONTAINER} did not stay up (its last lines are in the log); {old} runs again, unchanged."));
            }
            // Its volumes stay (no -v).
            let _ = docker(&["rm", &kept]);
            Ok(())
        })();
        let _ = std::fs::remove_file(&env_file);
        result?;
        // The compose file, so `docker compose up` there does not start a
        // second gateway on the same volumes.
        let mut note = String::new();
        if let (Some(svc), Some(f)) = (&service, files.first()) {
            let p = project.as_deref().unwrap_or("its project");
            match choice {
                ComposeChoice::WarnOnly => {
                    note = format!(" Its compose file {f} still has the service {svc}: remove it there, or docker compose up in {p} starts a second gateway on the same volumes.");
                    sink(Progress::Log { line: note.trim().to_string() });
                }
                ComposeChoice::CommentOut => {
                    sink(Progress::Step { name: format!("Commenting out {svc} in {f}") });
                    let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M");
                    let line = format!("Taken over by the SENTIENT Platform Manager on {stamp}: it runs as the container {CONTAINER} now.");
                    let done = std::fs::read_to_string(f).map_err(|e| e.to_string()).and_then(|t| {
                        let new = comment_out_service(&t, svc, &line).ok_or_else(|| format!("no service {svc} in it"))?;
                        let bak = format!("{f}.spm-bak");
                        std::fs::copy(f, &bak).map_err(|e| format!("copy to {bak}: {e}"))?;
                        std::fs::write(f, new).map_err(|e| e.to_string())?;
                        Ok(bak)
                    });
                    match done {
                        Ok(bak) => sink(Progress::Log { line: format!("{svc} is commented out in {f}; the file as it was is {bak}.") }),
                        Err(e) => {
                            note = format!(" Its compose file {f} could not be changed ({e}): remove the service {svc} there, or docker compose up in {p} starts a second gateway on the same volumes.");
                            sink(Progress::Log { line: note.trim().to_string() });
                        }
                    }
                }
            }
        }
        Ok(format!("The gateway runs as {CONTAINER} now, managed here, with {old}'s settings and data.{note}"))
    }
}

/// `docker inspect <name>` read as a Spec.
pub fn inspect_spec(name: &str) -> Option<Spec> {
    let mut spec = docker(&["inspect", name]).filter(|(ok, _)| *ok).and_then(|(_, o)| parse_spec(&o))?;
    let image = docker(&["image", "inspect", &spec.image]).filter(|(ok, _)| *ok).map(|(_, o)| o).unwrap_or_default();
    without_image_defaults(&mut spec, &image);
    Some(spec)
}

/// Leave out of `spec` what its image sets itself (command, entry point,
/// user, labels): what remains was given when the container was created,
/// and a recreate gives it again.
pub fn without_image_defaults(spec: &mut Spec, image_inspect: &str) {
    let v: serde_json::Value = serde_json::from_str(image_inspect).unwrap_or_default();
    let c = v.as_array().and_then(|a| a.first()).cloned().unwrap_or(v);
    let cfg = c.get("Config").cloned().unwrap_or_default();
    let strs = |x: Option<&serde_json::Value>| -> Vec<String> { x.and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()).unwrap_or_default() };
    if spec.cmd == strs(cfg.get("Cmd")) {
        spec.cmd.clear();
    }
    if spec.entrypoint == strs(cfg.get("Entrypoint")) {
        spec.entrypoint.clear();
    }
    if Some(spec.user.as_str()) == cfg.get("User").and_then(|x| x.as_str()) {
        spec.user.clear();
    }
    let image_labels = cfg.get("Labels").and_then(|l| l.as_object()).cloned().unwrap_or_default();
    spec.labels.retain(|(k, v)| image_labels.get(k).and_then(|x| x.as_str()) != Some(v.as_str()));
}

/// Put a recreated container on its other networks, with their names and
/// fixed addresses (`docker run` takes one network). Err: which failed.
#[cfg(not(windows))]
fn connect_extra_networks(spec: &Spec) -> Result<(), String> {
    for (net, aliases, ip) in &spec.extra_networks {
        let mut a: Vec<String> = vec!["network".into(), "connect".into()];
        for al in aliases {
            a.extend(["--alias".into(), al.clone()]);
        }
        if !ip.is_empty() {
            a.extend(["--ip".into(), ip.clone()]);
        }
        a.extend([net.clone(), spec.name.clone()]);
        let refs: Vec<&str> = a.iter().map(String::as_str).collect();
        if !matches!(docker_all(&refs), Some((true, _))) {
            return Err(format!("{} could not be put on its network {net} again.", spec.name));
        }
    }
    Ok(())
}

/// The image's own environment (PATH, the gateway's folders): kept, not listed.
fn image_env(image: &str) -> Vec<String> {
    docker(&["image", "inspect", "--format", "{{json .Config.Env}}", image]).filter(|(ok, _)| *ok).and_then(|(_, o)| serde_json::from_str(o.trim()).ok()).unwrap_or_default()
}

/// Why this container's settings are not changed from here (None: they are).
fn not_editable(s: &Status) -> Option<String> {
    if cfg!(windows) {
        return Some("On Windows the Platform Manager manages the gateway as a Windows service, not in Docker: change this container the way it was started.".into());
    }
    if s.managed {
        return None;
    }
    Some(match &s.compose_dir {
        Some(dir) => format!("This container comes from a compose file in {dir}: change its settings in that file and run docker compose up -d."),
        None => "This container was started outside the Platform Manager: change its settings the way it was started.".into(),
    })
}

/// The config volume read by the read script, in the container (running)
/// or in a throwaway one of its image on the config volume (stopped).
fn read_script_out(spec: &Spec, running: bool) -> Option<String> {
    let script = crate::gateway_settings::read_script();
    if running {
        let line = format!("D=\"${{ST_GW_CONFIG_DIR:-/sentient_gateway/config}}\"; P=1; {script}");
        // The gateway's file capability (NET_RAW) makes its process
        // undumpable: not even root in the container lists its sockets, so
        // "connected" is known here only from --status-json.
        return docker(&["exec", &spec.name, "sh", "-c", &line]).filter(|(ok, _)| *ok).map(|(_, o)| o);
    }
    let dir = env_value(&spec.env, "ST_GW_CONFIG_DIR").unwrap_or_else(|| VOLUMES[0].1.to_string());
    let (src, _, _, _) = spec.mounts.iter().find(|(_, d, _, _)| *d == dir)?;
    let mount = format!("{src}:/c:ro");
    let line = format!("D=/c; P=0; {script}");
    docker(&["run", "--rm", "--pull=never", "--network", "none", "-v", &mount, "--entrypoint", "sh", &spec.image, "-c", &line]).filter(|(ok, _)| *ok).map(|(_, o)| o)
}

fn env_value(env: &[String], name: &str) -> Option<String> {
    env.iter().find_map(|l| l.strip_prefix(&format!("{name}="))).map(str::to_string).filter(|v| !v.is_empty())
}

/// The container's settings (crate::gateway_settings::read).
pub fn read_settings(status: &Status) -> crate::gateway_settings::GwSettings {
    use crate::gateway_settings::{self as gs, GwSettings};
    let Some(spec) = status.container.as_deref().and_then(inspect_spec) else {
        return GwSettings { source: "none".into(), note: Some("The gateway container could not be read.".into()), ..GwSettings::default() };
    };
    let help = status.running.then(|| docker(&["exec", &spec.name, "sentient-gateway", "--help"])).flatten().map(|(_, o)| o).unwrap_or_default();
    let caps = gs::Caps::of_help(&help);
    let out = read_script_out(&spec, status.running);
    let mut s = match &out {
        Some(out) => gs::from_read(out),
        None => GwSettings { source: "none".into(), note: Some("The gateway's configuration volume could not be read.".into()), ..GwSettings::default() },
    };
    if caps.status_json {
        if let Some((true, out)) = docker(&["exec", &spec.name, "sentient-gateway", "--status-json"]) {
            gs::merge_status_json(&mut s, &out);
        }
    }
    if !status.running {
        s.connected = Some(false);
        s.connected_how = Some("the container is stopped".into());
    }
    gs::apply_env(&mut s, &spec.env, true);
    // localhost typed for a gateway on its own network is written as
    // host.docker.internal: shown as typed, with how it is reached.
    if needs_publish(&spec) && s.conn.host.as_deref() == Some(HOST_ALIAS) {
        let typed = spec.labels.iter().find(|(k, _)| k == TYPED_HOST_LABEL).map(|(_, v)| v.clone()).unwrap_or_else(|| "localhost".into());
        s.conn.via = Some(format!("This machine: the gateway's container reaches it as {HOST_ALIAS}."));
        s.conn.host = Some(typed);
    }
    // Its own network: the page's access is the published address.
    if let Some(l) = published_ui_listen(&spec, &s.ui.listen) {
        s.ui.listen = l;
    } else if needs_publish(&spec) && s.ui.enabled {
        s.ui.unreachable = Some(format!("The web page is on inside the container, but its port is not published on the {} network: it cannot be reached. Apply the web page again to publish it.", spec.network));
    }
    if let Some(out) = &out {
        gs::connected_from_read(&mut s, out);
    }
    s.caps = caps;
    let why = not_editable(status);
    let editable = why.is_none();
    s.machine.not_editable = why;
    s.machine.network = Some(if spec.network.is_empty() || spec.network == "default" { "bridge".into() } else { spec.network.clone() });
    s.machine.restart_always = Some(matches!(spec.restart.as_str(), "always" | "unless-stopped" | "on-failure"));
    s.machine.restart_word = Some(if spec.restart.is_empty() { "no".into() } else { spec.restart.clone() });
    s.machine.serial.mode = "devices".into();
    s.machine.serial.chosen = spec.devices.iter().map(|(h, _)| h.clone()).collect();
    #[cfg(unix)]
    {
        s.machine.serial.available = serial_devices_in(std::path::Path::new("/dev/serial/by-id"), std::path::Path::new("/dev")).into_iter().map(|d| d.path).collect();
    }
    s.machine.env = gs::overrides(&spec.env, &image_env(&spec.image), |_| editable, "container");
    s
}

/// `env` with `name` set to `value` (None: removed).
fn set_env(env: &mut Vec<String>, name: &str, value: Option<&str>) {
    env.retain(|l| !l.starts_with(&format!("{name}=")));
    if let Some(v) = value {
        env.push(format!("{name}={v}"));
    }
}

/// Where the gateway keeps its buffer (data.db) and logs inside the
/// container: ST_GW_DATA_DIR (TB_GW_DATA_DIR), else the image's default.
pub fn data_dir(spec: &Spec) -> String {
    let get = |k: &str| spec.env.iter().find_map(|e| e.strip_prefix(&format!("{k}=")).map(str::to_string));
    get("ST_GW_DATA_DIR").or_else(|| get("TB_GW_DATA_DIR")).filter(|d| d.starts_with('/')).unwrap_or_else(|| "/sentient_gateway/data".into())
}

/// Give the data folder a volume when none keeps it ([`DATA_VOLUME`]): a
/// container created without one loses its buffered data and its logs each
/// time it is created again (a take-over, an update, Apply). Returns what
/// was added, for the job's log and the review.
pub fn ensure_data_volume(spec: &mut Spec) -> Option<String> {
    let dir = data_dir(spec);
    let kept = spec.mounts.iter().any(|(_, dest, _, _)| {
        let d = dest.trim_end_matches('/');
        dir == d || dir.starts_with(&format!("{d}/")) || d == ""
    });
    if kept {
        return None;
    }
    spec.mounts.push((DATA_VOLUME.to_string(), dir.clone(), false, false));
    Some(format!("A volume {DATA_VOLUME} keeps {dir} (the gateway's buffer and logs), which had none."))
}

/// The volume [`ensure_data_volume`] adds.
pub const DATA_VOLUME: &str = "st-gw-data";

/// (address, port) of a listen value such as `0.0.0.0:8642`.
fn listen_parts(l: &str) -> (String, u16) {
    match l.rsplit_once(':') {
        Some((a, p)) => (a.trim_matches(['[', ']']).to_string(), p.parse().unwrap_or(crate::gateway::UI_PORT)),
        None => ("0.0.0.0".into(), crate::gateway::UI_PORT),
    }
}

/// Whether the container has its own network (a bridge): its ports must be
/// published to be reached from outside it. `host` shares the machine's.
pub fn needs_publish(spec: &Spec) -> bool {
    !matches!(spec.network.as_str(), "host" | "none")
}

/// The web page on a container with its own network: the page listens on
/// every address inside it, and the access choice (`listen`: all
/// interfaces `0.0.0.0:P`, this machine only `127.0.0.1:P`) becomes the
/// published address `ADDR:P:P`; off, the port is no longer published.
/// On the host network the gateway's own listen value decides.
pub fn set_ui_publish(spec: &mut Spec, enabled: bool, listen: &str) {
    let (addr, port) = listen_parts(listen);
    if !needs_publish(spec) {
        set_env(&mut spec.env, "ST_GW_UI_LISTEN", Some(listen));
        return;
    }
    set_env(&mut spec.env, "ST_GW_UI_LISTEN", Some(&format!("0.0.0.0:{port}")));
    spec.ports.retain(|p| {
        let c = p.split('/').next().unwrap_or(p);
        c.rsplit(':').next() != Some(&port.to_string())
    });
    if enabled {
        spec.ports.push(format!("{addr}:{port}:{port}"));
    }
}

/// The web page's access as published (`ADDR:P`), when the container has
/// its own network and publishes the page's port: what the card shows as
/// "Listen on" (the value inside the container is always every address).
pub fn published_ui_listen(spec: &Spec, inner: &str) -> Option<String> {
    if !needs_publish(spec) {
        return None;
    }
    let (_, port) = listen_parts(inner);
    spec.ports.iter().find_map(|p| {
        let c = p.split('/').next().unwrap_or(p);
        let parts: Vec<&str> = c.split(':').collect();
        match parts.as_slice() {
            [addr, host, cont] if cont.parse() == Ok(port) => Some(format!("{}:{host}", if addr.is_empty() { "0.0.0.0" } else { addr })),
            [host, cont] if cont.parse() == Ok(port) => Some(format!("0.0.0.0:{host}")),
            _ => None,
        }
    })
}

/// The name a gateway container reaches this machine by.
pub const HOST_ALIAS: &str = "host.docker.internal";
/// `--add-host` that makes [`HOST_ALIAS`] this machine (Docker's host-gateway).
pub const HOST_GATEWAY: &str = "host.docker.internal:host-gateway";
/// The label that keeps the host typed when it is written as [`HOST_ALIAS`].
pub const TYPED_HOST_LABEL: &str = "in.sentient.gateway.typed-host";

/// localhost, ::1 or 127.x.
pub fn is_loopback(host: &str) -> bool {
    let h = host.trim().trim_matches(['[', ']']).to_ascii_lowercase();
    h == "localhost" || h == "::1" || h.starts_with("127.")
}

/// Give the container [`HOST_GATEWAY`] when it has no host.docker.internal
/// of its own, so this machine always has a name in it. true: added.
pub fn ensure_host_gateway(spec: &mut Spec) -> bool {
    if spec.extra_hosts.iter().any(|h| h.split(':').next() == Some(HOST_ALIAS)) {
        return false;
    }
    spec.extra_hosts.push(HOST_GATEWAY.to_string());
    true
}

/// The host written to the config file for `typed`: in a container on its
/// own network, loopback is the container itself, so this machine is
/// reached as [`HOST_ALIAS`]; anything else as typed.
pub fn conn_host_for(spec: &Spec, typed: &str) -> String {
    if needs_publish(spec) && is_loopback(typed) {
        HOST_ALIAS.to_string()
    } else {
        typed.to_string()
    }
}

/// The address host-gateway is on this machine (the default bridge's
/// gateway; Docker's own default when it cannot be read).
#[cfg(not(windows))]
fn host_gateway_ip() -> String {
    docker(&["network", "inspect", "bridge", "--format", "{{range .IPAM.Config}}{{.Gateway}} {{end}}"])
        .filter(|(ok, _)| *ok)
        .and_then(|(_, o)| o.split_whitespace().find(|a| a.parse::<std::net::Ipv4Addr>().is_ok()).map(str::to_string))
        .unwrap_or_else(|| "172.17.0.1".into())
}

/// Err: nothing on this machine answers where a container reaches it
/// (`ip`:`port`, host.docker.internal): SENTIENT's MQTT port listens on
/// 127.0.0.1 only, or is not there.
pub fn host_gateway_reachable(ip: &str, port: u16, typed: &str) -> Result<(), String> {
    let addr: std::net::SocketAddr = format!("{ip}:{port}").parse().map_err(|_| format!("{ip}:{port} is not an address"))?;
    match std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(3)) {
        Ok(_) => Ok(()),
        Err(_) => Err(format!(
            "{typed} in the gateway's container is the container itself, so it reaches this machine as {HOST_ALIAS} ({ip}), and nothing answers on {ip}:{port}: SENTIENT's MQTT port listens on 127.0.0.1 only, or not on {port}. Publish it on all interfaces (\"{port}:{port}\") or on {ip}, then Apply again. Nothing was changed."
        )),
    }
}

/// ": <the gateway's last line about its connection>", or "".
#[cfg(not(windows))]
fn last_connect_line(name: &str) -> String {
    let last = docker_all(&["logs", "--tail", "40", name]).map(|(_, o)| o).unwrap_or_default();
    last.lines().rev().map(strip_ansi).find(|l| l.contains("uplink") || l.contains("connect")).map(|l| format!(": {}", l.trim())).unwrap_or_default()
}

/// Whether the gateway in `name` reports being connected to SENTIENT
/// within `secs` seconds (its --status-json, else its log).
#[cfg(not(windows))]
fn wait_connected(name: &str, secs: u64) -> bool {
    for _ in 0..secs / 3 {
        std::thread::sleep(std::time::Duration::from_secs(3));
        if let Some((true, out)) = docker(&["exec", name, "sentient-gateway", "--status-json"]) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(out.trim()) {
                if v.get("connected").and_then(|c| c.as_bool()) == Some(true) {
                    return true;
                }
                continue;
            }
        }
        if let Some((_, log)) = docker_all(&["logs", "--since", "90s", name]) {
            if connected_in_log(&strip_ansi(&log)) {
                return true;
            }
        }
    }
    false
}

/// Whether a gateway's log says it is connected now: its last "connected to
/// platform" comes after its last connection error or loss (a gateway
/// without --status-json).
pub fn connected_in_log(log: &str) -> bool {
    let lines: Vec<&str> = log.lines().collect();
    let last = |f: &dyn Fn(&str) -> bool| lines.iter().rposition(|l| f(l));
    let up = last(&|l| l.contains("connected to platform"));
    let down = last(&|l| l.contains("uplink connection error") || l.contains("the platform is lost") || l.contains("disconnected from platform"));
    match (up, down) {
        (Some(u), Some(d)) => u > d,
        (Some(_), None) => true,
        _ => false,
    }
}

/// (path, content) of the config file the gateway in container `name` uses.
#[cfg(not(windows))]
fn read_config_file(name: &str) -> Option<(String, String)> {
    let find = "d=\"${ST_GW_CONFIG_DIR:-/sentient_gateway/config}\"; for f in st_gateway.json tb_gateway.json; do if [ -f \"$d/$f\" ]; then echo \"$d/$f\"; cat \"$d/$f\"; exit 0; fi; done; exit 3";
    let (ok, out) = docker(&["exec", name, "sh", "-c", find])?;
    let (p, j) = out.split_once('\n')?;
    ok.then(|| (p.trim().to_string(), j.to_string()))
}

/// `json` (the gateway's config file) with the platform host and port set:
/// under `sentient`, or `thingsboard` in an older file that has only that.
pub fn with_connection(json: &str, host: Option<&str>, port: Option<u16>) -> Result<String, String> {
    let mut v: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("the gateway's config file is not valid JSON ({e})"))?;
    let obj = v.as_object_mut().ok_or("the gateway's config file is not a JSON object")?;
    let key = if obj.contains_key("sentient") || !obj.contains_key("thingsboard") { "sentient" } else { "thingsboard" };
    let section = obj.entry(key).or_insert_with(|| serde_json::json!({}));
    let section = section.as_object_mut().ok_or("the platform section of the config file is not an object")?;
    if let Some(h) = host {
        section.insert("host".into(), serde_json::Value::String(h.to_string()));
    }
    if let Some(p) = port {
        section.insert("port".into(), serde_json::Value::from(p));
    }
    serde_json::to_string_pretty(&v).map_err(|e| e.to_string())
}

/// Write the platform connection into the config file the gateway in
/// container `name` uses (st_gateway.json, else the older name), as the
/// container's own user, through a temporary file and a rename.
#[cfg(not(windows))]
fn write_connection(name: &str, host: Option<&str>, port: Option<u16>) -> Result<(), String> {
    // A gateway set up only by its environment has no file yet: it gets one.
    let (path, json) = match read_config_file(name) {
        Some(f) => f,
        None => {
            let dir = docker(&["exec", name, "sh", "-c", "echo \"${ST_GW_CONFIG_DIR:-/sentient_gateway/config}\""]).filter(|(ok, _)| *ok).map(|(_, o)| o.trim().to_string()).ok_or("The gateway's config folder could not be read in its container.")?;
            (format!("{dir}/st_gateway.json"), "{}".to_string())
        }
    };
    let new = with_connection(&json, host, port)?;
    write_config_raw(name, &path, &new)
}

/// Write `content` to `path` in container `name`, as its own user, through a
/// temporary file and a rename.
#[cfg(not(windows))]
fn write_config_raw(name: &str, path: &str, content: &str) -> Result<(), String> {
    let tmp = write_env_file(&[])?;
    std::fs::write(&tmp, content.as_bytes()).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    let q = super::shell_quote;
    let line = format!(
        "docker exec -i {} sh -c {} sh {} < {} 2>&1",
        q(name),
        q("cat > \"$1.spm-new\" && mv \"$1.spm-new\" \"$1\""),
        q(path.trim()),
        q(&tmp.display().to_string())
    );
    let r = super::run_output(&line);
    let _ = std::fs::remove_file(&tmp);
    match r {
        Some((true, _)) => Ok(()),
        Some((false, o)) => Err(format!("The gateway's config file could not be written: {}", o.trim())),
        None => Err("Docker could not be run to write the gateway's config file.".into()),
    }
}

/// The container's spec with `plan` applied (crate::gateway_settings::ApplyPlan).
pub fn planned_spec(spec: &Spec, before: &crate::gateway_settings::GwSettings, plan: &crate::gateway_settings::ApplyPlan, gid_of: &dyn Fn(&str) -> Option<u32>) -> Result<Spec, String> {
    let mut s = spec.clone();
    if let Some(on) = plan.ui_enabled {
        set_env(&mut s.env, "ST_GW_UI", Some(if on { "on" } else { "off" }));
    }
    // The page's access: published on a container with its own network
    // (set_ui_publish), else the gateway's listen value.
    if plan.ui_enabled.is_some() || plan.ui_listen.is_some() {
        let on = plan.ui_enabled.unwrap_or(before.ui.enabled);
        let listen = plan.ui_listen.clone().unwrap_or_else(|| before.ui.listen.clone());
        set_ui_publish(&mut s, on, if listen.is_empty() { "0.0.0.0:8642" } else { &listen });
    }
    ensure_data_volume(&mut s);
    ensure_host_gateway(&mut s);
    // localhost typed for a container on its own network: what was typed,
    // to show it again (the file holds host.docker.internal).
    if let Some(h) = &plan.conn_host {
        s.labels.retain(|(k, _)| k != TYPED_HOST_LABEL);
        if needs_publish(&s) && is_loopback(h) {
            s.labels.push((TYPED_HOST_LABEL.to_string(), h.clone()));
        }
    }
    // The connection goes to the config file (write_connection, before the
    // recreate); the environment no longer overrides it.
    if plan.conn_host.is_some() || plan.conn_port.is_some() {
        for k in ["host", "ST_GW_HOST", "TB_GW_HOST", "port", "ST_GW_PORT", "TB_GW_PORT"] {
            set_env(&mut s.env, k, None);
        }
    }
    if let Some(l) = &plan.rust_log {
        set_env(&mut s.env, "RUST_LOG", (!l.is_empty()).then_some(l.as_str()));
    }
    if let Some(vars) = &plan.env {
        // The listed overrides that have a value go; the new list comes in
        // (a secret is kept as it was).
        for e in before.machine.env.iter().filter(|e| e.editable && e.value.is_some()) {
            set_env(&mut s.env, &e.name, None);
        }
        for (n, v) in vars {
            if n.is_empty() || n.contains('=') || v.contains('\n') {
                return Err(format!("{n}: not a usable variable"));
            }
            set_env(&mut s.env, n, Some(v));
        }
    }
    if let Some(r) = plan.restart_always {
        s.restart = if r { "unless-stopped".into() } else { "no".into() };
    }
    if let Some(devs) = &plan.serial_devices {
        s.devices = devs.iter().map(|d| (d.clone(), d.clone())).collect();
        let mut gids: Vec<String> = Vec::new();
        for d in devs {
            if let Some(g) = gid_of(d).filter(|g| *g != 0).map(|g| g.to_string()) {
                if !gids.contains(&g) {
                    gids.push(g);
                }
            }
        }
        s.group_add = gids;
    }
    Ok(s)
}

#[cfg(unix)]
fn gid_of(path: &str) -> Option<u32> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path).ok().map(|m| m.gid())
}

/// A private file for the container's environment (0600, the account's
/// runtime folder), removed after the recreate.
#[cfg(unix)]
pub(crate) fn write_env_file(env: &[String]) -> Result<std::path::PathBuf, String> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).filter(|d| d.is_dir()).unwrap_or_else(std::env::temp_dir);
    let path = dir.join(format!("spm-gateway-{}.env", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).map_err(|e| format!("write {}: {e}", path.display()))?;
    for l in env {
        if l.contains('\n') {
            return Err("An environment value holds a line break.".into());
        }
        writeln!(f, "{l}").map_err(|e| e.to_string())?;
    }
    Ok(path)
}

/// Apply `plan` by recreating the container: the same image, volumes,
/// network, environment and capabilities, the changes over them. The old
/// container waits under another name until the new one stays up, and
/// comes back when it does not.
pub fn apply_settings(sink: &ProgressFn, status: &Status, before: &crate::gateway_settings::GwSettings, plan: &crate::gateway_settings::ApplyPlan) -> Result<(), String> {
    #[cfg(windows)]
    {
        let _ = (sink, status, before, plan);
        Err(not_editable(status).unwrap_or_default())
    }
    #[cfg(not(windows))]
    {
        let name = status.container.clone().ok_or("There is no gateway container.")?;
        let spec = inspect_spec(&name).ok_or("The gateway container could not be read.")?;
        let new = planned_spec(&spec, before, plan, &gid_of)?;
        let name = new.name.clone();
        let conn = plan.conn_host.is_some() || plan.conn_port.is_some();
        // localhost for a container on its own network: this machine, as
        // host.docker.internal, where SENTIENT's MQTT port has to answer.
        let file_host = plan.conn_host.as_deref().map(|h| conn_host_for(&new, h));
        if let (Some(typed), Some(fh)) = (plan.conn_host.as_deref(), file_host.as_deref()) {
            if fh == HOST_ALIAS {
                let ip = host_gateway_ip();
                host_gateway_reachable(&ip, plan.conn_port.or(before.conn.port).unwrap_or(1883), typed)?;
                sink(Progress::Log { line: format!("{typed} is this machine: the gateway's container reaches it as {HOST_ALIAS} ({ip}).") });
            }
        }
        // An image that sets the connection itself wins over the file even
        // once the container's own variables are gone: said before anything
        // changes.
        if conn {
            if let Some(v) = image_env(&spec.image).into_iter().find(|e| ["host=", "ST_GW_HOST=", "TB_GW_HOST=", "port=", "ST_GW_PORT=", "TB_GW_PORT="].iter().any(|p| e.starts_with(p))) {
                return Err(format!("The gateway's image {} sets {v} itself, which wins over its config file: nothing was changed. Use the standard image (Update the gateway), or set the connection in the image.", spec.image));
            }
        }
        // The config file as it was, to put back when the new connection
        // does not connect.
        let old_file = if conn { read_config_file(&spec.name) } else { None };
        if conn {
            sink(Progress::Step { name: "Writing the connection to the gateway's config file".into() });
            write_connection(&spec.name, file_host.as_deref(), plan.conn_port)?;
        }
        if let Err(e) = recreate_container(sink, &new, Some(format!("Stopping the container {name}")), &format!("Creating {name} again with the same volumes"), "with the new settings") {
            // The previous container runs again: so does its file.
            if conn {
                match &old_file {
                    Some((path, json)) => {
                        let _ = write_config_raw(&name, path, json);
                    }
                    None => {
                        let _ = docker(&["exec", &name, "sh", "-c", "rm -f \"${ST_GW_CONFIG_DIR:-/sentient_gateway/config}/st_gateway.json\""]);
                    }
                }
                let _ = docker(&["restart", &name]);
            }
            return Err(e);
        }
        // A new connection counts only once the gateway is connected with it;
        // otherwise the old one comes back, and the error says why.
        if conn {
            let target = format!("{}:{}", plan.conn_host.clone().or_else(|| before.conn.host.clone()).unwrap_or_default(), plan.conn_port.or(before.conn.port).unwrap_or(1883));
            sink(Progress::Step { name: format!("Checking the gateway connects to SENTIENT at {target}") });
            if !wait_connected(&name, 60) {
                let why = last_connect_line(&name);
                sink(Progress::Log { line: format!("Not connected within 60 seconds{why}") });
                sink(Progress::Step { name: "Putting the previous connection back".into() });
                match &old_file {
                    Some((path, json)) => {
                        let _ = write_config_raw(&name, path, json);
                    }
                    // It had none: the one written for the new connection goes.
                    None => {
                        let _ = docker(&["exec", &name, "sh", "-c", "rm -f \"${ST_GW_CONFIG_DIR:-/sentient_gateway/config}/st_gateway.json\""]);
                    }
                }
                recreate_container(sink, &spec, None, "Creating the gateway again with its previous settings", "with its previous settings")?;
                let via = if file_host.as_deref() == Some(HOST_ALIAS) { format!(" (this machine, reached from the container as {HOST_ALIAS}: a firewall may stop containers reaching it)") } else { String::new() };
                return Err(format!("The gateway did not connect to SENTIENT at {target}{via} within 60 seconds{why}, so its previous connection was put back. Check the address from the gateway's machine."));
            }
            sink(Progress::Log { line: format!("Connected to SENTIENT at {target}.") });
        }
        // The real effect, not the setting: the page answers where it is
        // published (or on the host network, where it listens).
        if plan.ui_enabled.is_some() || plan.ui_listen.is_some() {
            let on = plan.ui_enabled.unwrap_or(before.ui.enabled);
            let inner = new.env.iter().find_map(|e| e.strip_prefix("ST_GW_UI_LISTEN=")).unwrap_or("0.0.0.0:8642").to_string();
            let (_, port) = listen_parts(&inner);
            if on {
                sink(Progress::Step { name: format!("Checking the web page answers on port {port}") });
                if needs_publish(&new) && !matches!(docker(&["port", &name, &port.to_string()]), Some((true, o)) if !o.trim().is_empty()) {
                    return Err(format!("The container runs, but Docker does not publish port {port}: the web page cannot be reached."));
                }
                let up = (0..20).any(|_| {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    std::net::TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], port)), std::time::Duration::from_secs(1)).is_ok()
                });
                if !up {
                    return Err(format!("The gateway runs with its web page on, but nothing answers on port {port} of this machine within 20 seconds."));
                }
                let (addr, _) = listen_parts(&published_ui_listen(&new, &inner).unwrap_or(inner.clone()));
                sink(Progress::Log { line: format!("The web page answers: https://{}:{port}/", if addr == "0.0.0.0" { "<this machine's address>".to_string() } else { addr }) });
            }
        }
        Ok(())
    }
}

/// The one way the gateway's container is started again (an update to a
/// new image, Apply of its settings): `spec` holds everything it runs with
/// (crate inspect_spec, update_spec, planned_spec). Its environment goes
/// in a private env file (the token never on a command line); the old
/// container is stopped and waits under another name until the new one
/// stays up, and is started again, unchanged, when it does not (with the
/// new one's last log lines in the job's log). `stop_step`/`start_step`:
/// the job's step names; `what` ends the failure ("on <image>").
#[cfg(not(windows))]
pub(crate) fn recreate_container(sink: &ProgressFn, spec: &Spec, stop_step: Option<String>, start_step: &str, what: &str) -> Result<(), String> {
    let name = spec.name.clone();
    let old = format!("{name}-spm-previous");
    let env_file = write_env_file(&spec.env)?;
    let file = env_file.display().to_string();
    let result = (|| {
        match &stop_step {
            Some(step) => sink(Progress::Step { name: step.clone() }),
            None => sink(Progress::Log { line: format!("Stopping the container {name}") }),
        }
        let _ = docker(&["rm", "-f", &old]);
        if !matches!(docker(&["stop", "-t", "25", &name]), Some((true, _))) {
            return Err(format!("docker stop {name} failed."));
        }
        if !matches!(docker(&["rename", &name, &old]), Some((true, _))) {
            let _ = docker(&["start", &name]);
            return Err(format!("docker rename {name} failed; the gateway was started again as it was."));
        }
        sink(Progress::Step { name: start_step.to_string() });
        let args = run_args(spec, &file);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let started = matches!(docker(&refs), Some((true, _))) && match connect_extra_networks(spec) {
            Ok(()) => true,
            Err(e) => {
                sink(Progress::Log { line: e });
                false
            }
        };
        let up = started && (0..30).any(|_| {
            std::thread::sleep(std::time::Duration::from_secs(1));
            container_running(&name)
        }) && {
            // Still up a few seconds later (a bad setting exits at once).
            std::thread::sleep(std::time::Duration::from_secs(4));
            container_running(&name)
        };
        if !up {
            sink(Progress::Log { line: "The new container did not stay up; its last lines:".into() });
            if let Some((_, out)) = docker_all(&["logs", "--tail", "15", &name]) {
                for l in out.lines() {
                    sink(Progress::Log { line: strip_ansi(l) });
                }
            }
            sink(Progress::Log { line: "The previous container is started again.".into() });
            let _ = docker(&["rm", "-f", &name]);
            let _ = docker(&["rename", &old, &name]);
            let _ = docker(&["start", &name]);
            return Err(format!("The gateway did not start {what} (its last lines are in the log); the previous container {name} runs again, unchanged."));
        }
        let _ = docker(&["rm", &old]);
        Ok(())
    })();
    let _ = std::fs::remove_file(&env_file);
    result
}

/// A log line without its colour codes (`ESC [ … m`).
pub fn strip_ansi(l: &str) -> String {
    let mut out = String::new();
    let mut it = l.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' && it.peek() == Some(&'[') {
            it.next();
            for d in it.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// `docker <args>` with its standard error too (docker logs writes the
/// container's stderr there).
#[cfg(not(windows))]
fn docker_all(args: &[&str]) -> Option<(bool, String)> {
    let line: Vec<String> = args.iter().map(|a| super::shell_quote(a)).collect();
    super::run_output(&format!("docker {} 2>&1", line.join(" ")))
}

fn container_running(name: &str) -> bool {
    docker(&["inspect", "--format", "{{.State.Running}} {{.State.Restarting}} {{.RestartCount}}", name]).is_some_and(|(ok, o)| ok && stays_up(&o))
}

/// `{{.State.Running}} {{.State.Restarting}} {{.RestartCount}}` of a
/// container just created: up only when running, not restarting and never
/// restarted. Docker reports a container its restart policy brings back
/// again and again (a setting that makes the gateway exit) as running, so
/// Running alone never noticed one.
pub fn stays_up(state: &str) -> bool {
    let mut w = state.split_whitespace();
    matches!((w.next(), w.next(), w.next()), (Some("true"), Some("false"), Some("0")))
}

/// The page's password through the gateway in its container
/// (`docker exec -i … --ui-password -`, the password on standard input).
pub fn set_ui_password(status: &Status, password: &str) -> Result<String, String> {
    let name = status.container.clone().ok_or("There is no gateway container.")?;
    if !status.running {
        return Err("Start the gateway first: its password is set by the gateway itself.".into());
    }
    #[cfg(windows)]
    {
        crate::gateway_settings::run_with_stdin("docker", &["exec", "-i", &name, "sentient-gateway", "--ui-password", "-"], password)
    }
    #[cfg(not(windows))]
    {
        let line = format!("docker exec -i {} sentient-gateway --ui-password -", super::shell_quote(&name));
        let (prog, args) = super::shell_args(&line);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        crate::gateway_settings::run_with_stdin(&prog, &refs, password)
    }
}

/// The credentials to test with: the container's environment over its file.
pub fn test_creds(status: &Status) -> Option<crate::gateway_settings::TestCreds> {
    let spec = inspect_spec(status.container.as_deref()?)?;
    let file = status
        .running
        .then(|| docker(&["exec", &spec.name, "sh", "-c", "d=\"${ST_GW_CONFIG_DIR:-/sentient_gateway/config}\"; cat \"$d/st_gateway.json\" 2>/dev/null || cat \"$d/tb_gateway.json\" 2>/dev/null"]))
        .flatten()
        .filter(|(ok, _)| *ok)
        .map(|(_, o)| o);
    crate::gateway_settings::creds_of(file.as_deref(), &spec.env)
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
/st-gateway\tquakestring/sentient-gateway-rs:latest\t[\"/usr/local/bin/sentient-gateway\"]\tfalse\tdfal\t/home/mz/dfal\tst-gw-config,st-gw-data,st-gw-logs,\t<no value>\tsha256:fdd9ed12884efda7151420b1fc003afe7c9b73e48e14d2e60ff0248f944cc95b\n";
        let all = parse_inspect(out);
        assert_eq!(all.len(), 3);
        let (c, managed) = pick(&all).unwrap();
        assert_eq!((c.name.as_str(), managed, c.running), ("st-gateway", false, false));
        assert_eq!(c.project.as_deref(), Some("dfal"));
        assert_eq!(c.working_dir.as_deref(), Some("/home/mz/dfal"));
        assert_eq!(c.volumes, vec!["st-gw-config", "st-gw-data", "st-gw-logs"]);
        assert_eq!(c.version_label, None);
        assert_eq!(c.image_id.as_deref(), Some("sha256:fdd9ed12884efda7151420b1fc003afe7c9b73e48e14d2e60ff0248f944cc95b"));
        // A locally built image: its entrypoint is the gateway's program.
        let local = parse_inspect("/plant-gw\tsha256:fdd9ed12884e\t[\"/usr/local/bin/sentient-gateway\"]\ttrue\t\t\t\t4.16.2\n");
        assert_eq!(names(pick(&local)), Some(("plant-gw".into(), false)));
        assert_eq!(image_version(&local[0]).as_deref(), Some("4.16.2"));
        // The older Python gateway under our name: not ours, only reported.
        let py = parse_inspect("/sentient-gateway\tquakestring/sentient-gateway:3.7.10\t[\"/bin/sh\",\"-c\"]\ttrue\n");
        assert_eq!(names(pick(&py)), None);
        assert_eq!(legacy_in(&py).as_deref(), Some("sentient-gateway"));
        // A local build of the Rust gateway tagged like the old image is ours.
        let rs = parse_inspect("/sentient-gateway\tsentient-gateway:dev\t[\"/usr/local/bin/sentient-gateway\"]\ttrue\n");
        assert_eq!(names(pick(&rs)), Some(("sentient-gateway".into(), true)));
        assert_eq!(legacy_in(&rs), None);
        // Named like a gateway, but not ours.
        let other = parse_inspect("/sentient-gateway\tnginx:1.27\t[\"/docker-entrypoint.sh\"]\ttrue\n/journal-gatewayd\tdebian:12\tnull\tfalse\n");
        assert_eq!(names(pick(&other)), None);
        // Ours, started by the Platform Manager, wins.
        let both = parse_inspect("/st-gateway\tquakestring/sentient-gateway-rs:4.16.0\tnull\ttrue\n/sentient-gateway\tquakestring/sentient-gateway-rs:4.16.2\tnull\tfalse\n");
        assert_eq!(names(pick(&both)), Some(("sentient-gateway".into(), true)));
        assert_eq!(image_version(&both[0]).as_deref(), Some("4.16.0"));
        assert_eq!(image_version(&all[2]), None);
    }

    // std::os::unix and the #[cfg(unix)] serial_devices_in: Unix only (it
    // broke the core tests on windows-latest).
    #[cfg(unix)]
    #[test]
    fn serial_adapters_prefer_their_stable_names() {
        use std::os::unix::fs::MetadataExt as _;
        let root = std::env::temp_dir().join(format!("spm-gw-serial-test-{}", std::process::id()));
        let (dev, by_id) = (root.join("dev"), root.join("dev/serial/by-id"));
        std::fs::create_dir_all(&by_id).unwrap();
        for n in ["ttyUSB0", "ttyUSB1", "ttyACM0", "ttyS0", "ttyUSBx"] {
            std::fs::write(dev.join(n), b"").unwrap();
        }
        std::os::unix::fs::symlink(dev.join("ttyUSB1"), by_id.join("usb-Example_RS485_A1-if00-port0")).unwrap();
        let gid = std::fs::metadata(dev.join("ttyUSB0")).unwrap().gid();
        let found = serial_devices_in(&by_id, &dev);
        let paths: Vec<String> = found.iter().map(|d| d.path.replace(&*root.to_string_lossy(), "")).collect();
        // ttyUSB1 only by its by-id name; ttyS0 (a built-in port) and odd names left out.
        assert_eq!(paths, vec!["/dev/serial/by-id/usb-Example_RS485_A1-if00-port0", "/dev/ttyACM0", "/dev/ttyUSB0"]);
        assert!(found.iter().all(|d| d.gid == gid));
        std::fs::remove_dir_all(&root).unwrap();
        // No folder at all: no devices.
        assert!(serial_devices_in(std::path::Path::new("/nonexistent/by-id"), std::path::Path::new("/nonexistent")).is_empty());
    }

    #[test]
    fn the_run_line_passes_serial_adapters_with_their_group() {
        let none = run_command("quakestring/sentient-gateway-rs:4.16.2", "/tmp/gw.env", &[]);
        assert!(!none.contains("--device") && !none.contains("--group-add"));
        assert!(none.contains("--network host --add-host host.docker.internal:host-gateway -v st-gw-config:/sentient_gateway/config"));
        let devs = [
            SerialDevice { path: "/dev/serial/by-id/usb-Example_RS485_A1-if00-port0".into(), gid: 986 },
            SerialDevice { path: "/dev/ttyACM0".into(), gid: 986 },
            SerialDevice { path: "/dev/ttyUSB3".into(), gid: 0 },
        ];
        let line = run_command("quakestring/sentient-gateway-rs:4.16.2", "/tmp/gw.env", &devs);
        assert!(line.contains(" --device '/dev/serial/by-id/usb-Example_RS485_A1-if00-port0:/dev/serial/by-id/usb-Example_RS485_A1-if00-port0'"), "{line}");
        assert!(line.contains(" --device '/dev/ttyACM0:/dev/ttyACM0'") && line.contains(" --device '/dev/ttyUSB3:/dev/ttyUSB3'"));
        // One group, once; root's group never.
        assert_eq!(line.matches("--group-add").count(), 1);
        assert!(line.contains(" --group-add 986 ") && !line.contains("--group-add 0"));
        assert!(line.ends_with("'quakestring/sentient-gateway-rs:4.16.2'"));
    }

    #[test]
    fn the_run_line_carries_no_token() {
        let line = run_command("quakestring/sentient-gateway-rs:4.16.2", "/tmp/gw.env", &[]);
        assert!(line.contains(" --env-file '/tmp/gw.env' "), "{line}");
        assert!(!line.contains("accessToken") && !line.contains("-e "), "{line}");
        let lines = env_lines("10.0.0.5", 1883, "Xk7pQ2mW9rTzL4vB8nYd").unwrap();
        assert_eq!(lines, vec!["host=10.0.0.5", "port=1883", "accessToken=Xk7pQ2mW9rTzL4vB8nYd"]);
        assert!(env_lines("h", 1883, "a\nb=c").is_err());
    }

    const COMPOSE: &str = "version: '3.4'\nservices:\n  # The gateway\n  st-gateway:\n    image: quakestring/sentient-gateway-rs:latest\n    container_name: st-gateway\n\n    ports:\n        - \"5000:5000\" # REST\n#        - \"1052:1052\"\n    volumes:\n      - st-gw-config:/sentient_gateway/config\n\n  other:\n    image: nginx\n\nvolumes:\n  st-gw-config:\n    name: st-gw-config\n";

    #[test]
    fn a_taken_over_service_is_commented_out() {
        let out = comment_out_service(COMPOSE, "st-gateway", "Taken over.").unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[3], "  # Taken over.");
        assert_eq!(lines[4], "  # st-gateway:");
        assert_eq!(lines[5], "  #   image: quakestring/sentient-gateway-rs:latest");
        assert_eq!(lines[8], "  #   ports:");
        assert_eq!(lines[9], "  #       - \"5000:5000\" # REST");
        assert_eq!(lines[10], "  # #        - \"1052:1052\"");
        assert_eq!(lines[11], "  #   volumes:");
        assert_eq!(lines[12], "  #     - st-gw-config:/sentient_gateway/config");
        assert_eq!(lines[13], "");
        // The other service, the volumes and the comment above stay.
        assert!(out.contains("\n  other:\n    image: nginx\n"), "{out}");
        assert!(out.contains("\nvolumes:\n  st-gw-config:\n    name: st-gw-config\n"), "{out}");
        assert!(out.contains("\n  # The gateway\n"), "{out}");
        assert!(out.ends_with('\n'));
        // Every line of the service, and nothing else, is commented.
        let svc: Vec<&str> = out.lines().skip(3).take(10).collect();
        assert!(svc.iter().all(|l| l.trim().is_empty() || l.trim_start().starts_with('#')), "{svc:?}");
        assert!(comment_out_service(COMPOSE, "missing", "x").is_none());
        // Only a service's key, not a key inside one.
        assert!(comment_out_service(COMPOSE, "ports", "x").is_none());
        let other = comment_out_service(COMPOSE, "other", "x").unwrap();
        assert!(other.contains("\n  # other:\n  #   image: nginx\n\nvolumes:"), "{other}");
    }

    #[test]
    fn ports_hosts_and_aliases_are_kept() {
        let json = r#"[{"Name":"/st-gateway","Config":{"Image":"quakestring/sentient-gateway-rs:latest","Env":["host=h"]},"HostConfig":{"NetworkMode":"dfal_default","RestartPolicy":{"Name":"always"},"PortBindings":{"5000/tcp":[{"HostIp":"","HostPort":"5000"}],"502/tcp":[{"HostIp":"127.0.0.1","HostPort":"5020"}]},"ExtraHosts":["host.docker.internal:host-gateway"]},"Mounts":[]}]"#;
        let mut spec = parse_spec(json).unwrap();
        assert!(spec.aliases.is_empty());
        let with = json.replace(r#""Mounts":[]"#, r#""Mounts":[],"Id":"0123456789abcdef","NetworkSettings":{"Networks":{"dfal_default":{"Aliases":["st-gateway","0123456789ab","gw"]}}}"#);
        assert_eq!(parse_spec(&with).unwrap().aliases, vec!["gw".to_string()]);
        assert_eq!(spec.ports, vec!["127.0.0.1:5020:502/tcp".to_string(), "5000:5000/tcp".to_string()]);
        assert_eq!(spec.extra_hosts, vec!["host.docker.internal:host-gateway".to_string()]);
        spec.aliases.push("st-gateway".into());
        spec.name = CONTAINER.into();
        let args = run_args(&spec, "/x.env").join(" ");
        assert!(args.contains("--network dfal_default --network-alias st-gateway -p 127.0.0.1:5020:502/tcp -p 5000:5000/tcp --add-host host.docker.internal:host-gateway"), "{args}");
        // On the host network: no ports, no aliases.
        spec.network = "host".into();
        let args = run_args(&spec, "/x.env").join(" ");
        assert!(!args.contains(" -p ") && !args.contains("--network-alias"), "{args}");
    }

    #[test]
    fn an_update_recreates_with_what_the_container_has() {
        // One the update from the older gateway started: its configuration
        // volume and environment are kept, not replaced by st-gw-config.
        let current = Spec {
            name: CONTAINER.into(),
            image: "quakestring/sentient-gateway-rs:4.16.2".into(),
            env: vec!["accessToken=EXAMPLETOKEN".into(), "TB_GW_CLIENT_ID=gw-example".into(), "host=10.0.0.5".into(), "ST_GW_CONFIG_DIR=/sentient_gateway/config".into(), "PATH=/usr/bin".into()],
            restart: "unless-stopped".into(),
            network: "host".into(),
            mounts: vec![("gw-config".into(), "/sentient_gateway/config".into(), false, false), ("st-gw-data".into(), "/sentient_gateway/data".into(), false, false)],
            ..Spec::default()
        };
        let found = [SerialDevice { path: "/dev/ttyACM0".into(), gid: 986 }];
        let spec = update_spec(&current, "quakestring/sentient-gateway-rs:4.16.3", &[], &found);
        assert_eq!(spec.mounts[0], ("gw-config".to_string(), "/sentient_gateway/config".to_string(), false, false));
        assert_eq!(spec.env, vec!["accessToken=EXAMPLETOKEN", "TB_GW_CLIENT_ID=gw-example", "host=10.0.0.5"]);
        let args = run_args(&spec, "/run/user/1000/x.env").join(" ");
        assert!(args.contains(" --device /dev/ttyACM0:/dev/ttyACM0 --group-add 986"), "{args}");
        assert!(args.ends_with("'quakestring/sentient-gateway-rs:4.16.3'") || args.ends_with("quakestring/sentient-gateway-rs:4.16.3"), "{args}");
        assert!(!args.contains("EXAMPLETOKEN"), "{args}");
    }

    /// An update keeps what Apply set (Gateway › Advanced): the chosen
    /// devices, the restart policy, RUST_LOG and the page's variables; the
    /// old image's own variables give way to the new image's.
    #[test]
    fn an_update_keeps_what_apply_set() {
        let current = parse_spec(INSPECT).unwrap();
        let mut current = Spec { restart: "no".into(), ..current };
        current.env.push("RUST_LOG=debug".into());
        current.env.push("ST_GW_UI=on".into());
        let image_env = vec!["PATH=/usr/bin".to_string(), "ST_GW_CONFIG_DIR=/sentient_gateway/config".to_string(), "RUST_LOG=info".to_string()];
        let found = [SerialDevice { path: "/dev/ttyACM0".into(), gid: 986 }];
        let spec = update_spec(&current, "quakestring/sentient-gateway-rs:4.16.3", &image_env, &found);
        assert_eq!(spec.env, vec!["host=127.0.0.1", "port=1883", "accessToken=Q1w2E3r4T5y6U7i8O9p0", "RUST_LOG=debug", "ST_GW_UI=on"]);
        assert_eq!((spec.restart.as_str(), spec.network.as_str()), ("no", "host"));
        // Its own device, not every adapter found now.
        assert_eq!(spec.devices, vec![("/dev/ttyUSB0".to_string(), "/dev/ttyUSB0".to_string())]);
        // Its two mounts, and a volume for the data folder, which had none.
        assert_eq!(spec.mounts.len(), 3);
        assert_eq!(spec.mounts[2], (DATA_VOLUME.to_string(), "/sentient_gateway/data".to_string(), false, false));
    }

    /// Connected only when "connected to platform" comes after the last loss.
    #[test]
    fn connected_is_read_from_the_log() {
        assert!(connected_in_log("a\nINFO gateway_core::uplink: connected to platform epoch=1\nWARN uplink: no session limits"));
        assert!(!connected_in_log("INFO connected to platform\nWARN uplink connection error; retrying"));
        assert!(connected_in_log("WARN uplink connection error\nINFO connected to platform epoch=2"));
        assert!(!connected_in_log("WARN the platform is lost: telling 0 connectors"));
    }

    /// A container with no volume for the gateway's data folder gets one;
    /// one that has it (or a parent of it) keeps its own.
    #[test]
    fn the_data_folder_gets_a_volume() {
        let mut s = Spec { mounts: vec![("st-gw-config".into(), "/sentient_gateway/config".into(), false, false)], ..Spec::default() };
        assert!(ensure_data_volume(&mut s).is_some());
        assert_eq!(s.mounts[1], (DATA_VOLUME.to_string(), "/sentient_gateway/data".to_string(), false, false));
        assert!(ensure_data_volume(&mut s).is_none());
        let mut custom = Spec { env: vec!["ST_GW_DATA_DIR=/data/gw".into()], mounts: vec![("/srv/gw".into(), "/data".into(), false, true)], ..Spec::default() };
        assert!(ensure_data_volume(&mut custom).is_none(), "a parent folder's mount keeps it");
    }

    /// The web page on a container's own network is published per the
    /// access choice and inside listens on every address; off, it is no
    /// longer published; on the host network nothing is published.
    #[test]
    fn the_web_page_is_published_on_a_bridge_only() {
        let mut s = Spec { network: "st_default".into(), ports: vec!["5000:5000".into()], ..Spec::default() };
        set_ui_publish(&mut s, true, "127.0.0.1:8642");
        assert_eq!(s.ports, vec!["5000:5000", "127.0.0.1:8642:8642"]);
        assert!(s.env.contains(&"ST_GW_UI_LISTEN=0.0.0.0:8642".to_string()));
        assert_eq!(published_ui_listen(&s, "0.0.0.0:8642").as_deref(), Some("127.0.0.1:8642"));
        set_ui_publish(&mut s, true, "0.0.0.0:8642");
        assert_eq!(s.ports, vec!["5000:5000", "0.0.0.0:8642:8642"]);
        set_ui_publish(&mut s, false, "0.0.0.0:8642");
        assert_eq!(s.ports, vec!["5000:5000"]);
        let mut h = Spec { network: "host".into(), ..Spec::default() };
        set_ui_publish(&mut h, true, "127.0.0.1:8642");
        assert!(h.ports.is_empty());
        assert!(h.env.contains(&"ST_GW_UI_LISTEN=127.0.0.1:8642".to_string()));
        assert_eq!(published_ui_listen(&h, "127.0.0.1:8642"), None);
    }

    /// localhost for a container on its own network is this machine as
    /// host.docker.internal (kept as typed in a label); on the host network
    /// and for any other host, as typed. Every container gets the name.
    #[test]
    fn localhost_in_a_bridge_container_is_this_machine() {
        let bridge = Spec { network: "st_default".into(), ..Spec::default() };
        let host = Spec { network: "host".into(), ..Spec::default() };
        for h in ["localhost", "127.0.0.1", "::1", "[::1]", "LOCALHOST"] {
            assert_eq!(conn_host_for(&bridge, h), HOST_ALIAS, "{h}");
            assert_eq!(conn_host_for(&host, h), h, "{h}");
        }
        assert_eq!(conn_host_for(&bridge, "192.168.0.20"), "192.168.0.20");
        assert_eq!(conn_host_for(&bridge, "sentient"), "sentient");
        let mut s = bridge.clone();
        assert!(ensure_host_gateway(&mut s));
        assert!(!ensure_host_gateway(&mut s));
        assert_eq!(s.extra_hosts, vec![HOST_GATEWAY.to_string()]);
        let mut own = Spec { extra_hosts: vec!["host.docker.internal:10.0.0.1".into()], ..Spec::default() };
        assert!(!ensure_host_gateway(&mut own), "its own host.docker.internal stays");
        assert!(run_args(&s, "/e").join(" ").contains("--add-host host.docker.internal:host-gateway"));
        assert!(run_command("img", "/e", &[]).contains("--add-host host.docker.internal:host-gateway"));
    }

    /// Apply's plan: the typed host goes in a label for a bridge container
    /// (localhost), not for another host; the container gets the name.
    #[test]
    fn the_typed_host_is_kept_for_the_page() {
        let bridge = Spec { network: "st_default".into(), ..Spec::default() };
        let before = crate::gateway_settings::GwSettings::default();
        let plan = crate::gateway_settings::ApplyPlan { conn_host: Some("127.0.0.1".into()), ..Default::default() };
        let s = planned_spec(&bridge, &before, &plan, &|_| None).unwrap();
        assert!(s.labels.contains(&(TYPED_HOST_LABEL.to_string(), "127.0.0.1".to_string())));
        assert!(s.extra_hosts.contains(&HOST_GATEWAY.to_string()));
        let plan = crate::gateway_settings::ApplyPlan { conn_host: Some("192.168.0.20".into()), ..Default::default() };
        let s2 = planned_spec(&s, &before, &plan, &|_| None).unwrap();
        assert!(!s2.labels.iter().any(|(k, _)| k == TYPED_HOST_LABEL));
    }

    /// Where nothing answers, the reason names host.docker.internal and
    /// 127.0.0.1-only publishing; something listening passes.
    #[test]
    fn the_host_gateway_port_is_checked() {
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let p = l.local_addr().unwrap().port();
        assert!(host_gateway_reachable("127.0.0.1", p, "localhost").is_ok());
        drop(l);
        let e = host_gateway_reachable("127.0.0.1", p, "localhost").unwrap_err();
        assert!(e.contains(HOST_ALIAS) && e.contains("127.0.0.1 only") && e.contains("Nothing was changed"), "{e}");
    }

    /// The connection is written into the config file's platform section
    /// (an older file's too), the rest of it kept.
    #[test]
    fn the_connection_goes_into_the_config_file() {
        let st = with_connection(r#"{"sentient":{"host":"saf.example.in","port":1883,"security":{"type":"accessToken"}},"localUi":{"enabled":false}}"#, Some("localhost"), None).unwrap();
        let v: serde_json::Value = serde_json::from_str(&st).unwrap();
        assert_eq!(v["sentient"]["host"], "localhost");
        assert_eq!(v["sentient"]["port"], 1883);
        assert_eq!(v["sentient"]["security"]["type"], "accessToken");
        assert_eq!(v["localUi"]["enabled"], false);
        let tb = with_connection(r#"{"thingsboard":{"host":"a","port":1883}}"#, Some("192.168.0.20"), Some(1884)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&tb).unwrap();
        assert_eq!((v["thingsboard"]["host"].as_str(), v["thingsboard"]["port"].as_u64()), (Some("192.168.0.20"), Some(1884)));
        assert!(v.get("sentient").is_none());
        assert!(with_connection("not json", Some("h"), None).is_err());
    }

    /// Everything a recreate must give again is read and given: every
    /// network with its names and fixed address, a given command, user,
    /// privileged, labels (not compose's, not the image's), log driver,
    /// tmpfs, limits.
    #[test]
    fn a_recreate_keeps_the_whole_container() {
        let inspect = r#"[{"Name":"/gw","Id":"abcdef0123456789","Config":{"Image":"img:1","Env":[],"Cmd":["--verbose"],"Entrypoint":["/bin/gw"],"User":"1000","Labels":{"com.docker.compose.service":"gw","site":"saf","org.image":"x"}},
            "HostConfig":{"NetworkMode":"st_default","Privileged":true,"LogConfig":{"Type":"local","Config":{"max-size":"10m"}},"Tmpfs":{"/run":"size=64m"},"Memory":268435456,"NanoCpus":1500000000,"RestartPolicy":{"Name":"always"}},
            "NetworkSettings":{"Networks":{"st_default":{"Aliases":["gw","abcdef012345"],"IPAMConfig":{"IPv4Address":"172.30.0.9"}},"plant":{"Aliases":["gateway"],"IPAMConfig":null}}},"Mounts":[]}]"#;
        let mut spec = parse_spec(inspect).unwrap();
        without_image_defaults(&mut spec, r#"[{"Config":{"Cmd":null,"Entrypoint":["/bin/gw"],"User":"","Labels":{"org.image":"x"}}}]"#);
        assert_eq!(spec.ip, "172.30.0.9");
        assert_eq!(spec.extra_networks, vec![("plant".to_string(), vec!["gateway".to_string()], String::new())]);
        assert_eq!((spec.cmd.clone(), spec.entrypoint.clone()), (vec!["--verbose".to_string()], Vec::<String>::new()));
        assert_eq!(spec.labels, vec![("site".to_string(), "saf".to_string())]);
        let args = run_args(&spec, "/tmp/e.env").join(" ");
        for want in ["--network st_default", "--ip 172.30.0.9", "--user 1000", "--privileged", "--label site=saf", "--log-driver local", "--log-opt max-size=10m", "--tmpfs /run:size=64m", "--memory 268435456", "--cpus 1.5", "img:1 --verbose"] {
            assert!(args.contains(want), "{want} missing in {args}");
        }
        assert!(!args.contains("compose"), "{args}");
    }

    /// An update keeps a data or config folder given for this container,
    /// and lets the image's default ones go.
    #[test]
    fn an_update_keeps_custom_folders() {
        let current = Spec { image: "img:1".into(), env: vec!["ST_GW_CONFIG_DIR=/etc/gw".into(), "ST_GW_DATA_DIR=/sentient_gateway/data".into()], mounts: vec![("/srv/gw".into(), "/etc/gw".into(), false, true)], ..Spec::default() };
        let spec = update_spec(&current, "img:2", &[], &[]);
        assert!(spec.env.contains(&"ST_GW_CONFIG_DIR=/etc/gw".to_string()));
        assert!(!spec.env.iter().any(|e| e.starts_with("ST_GW_DATA_DIR=")));
    }

    const INSPECT: &str = r#"[{"Name":"/sentient-gateway","Config":{"Image":"quakestring/sentient-gateway-rs:4.16.2","Env":["host=127.0.0.1","port=1883","accessToken=Q1w2E3r4T5y6U7i8O9p0","PATH=/usr/bin","ST_GW_CONFIG_DIR=/sentient_gateway/config"]},
        "HostConfig":{"RestartPolicy":{"Name":"unless-stopped"},"NetworkMode":"host","Devices":[{"PathOnHost":"/dev/ttyUSB0","PathInContainer":"/dev/ttyUSB0","CgroupPermissions":"rwm"}],"GroupAdd":["986"],"CapAdd":null},
        "Mounts":[{"Type":"volume","Name":"st-gw-config","Destination":"/sentient_gateway/config","RW":true},{"Type":"bind","Source":"/srv/example/certs","Destination":"/certs","RW":false}]}]"#;

    #[test]
    fn log_lines_lose_their_colours() {
        assert_eq!(strip_ansi("\u{1b}[2m2026-11-18T05:43:01Z\u{1b}[0m \u{1b}[33m WARN\u{1b}[0m uplink"), "2026-11-18T05:43:01Z  WARN uplink");
    }

    #[test]
    fn a_recreate_keeps_volumes_network_and_secrets() {
        use crate::gateway_settings::{ApplyPlan, EnvVar, GwSettings};
        let spec = parse_spec(INSPECT).unwrap();
        assert_eq!((spec.name.as_str(), spec.restart.as_str(), spec.network.as_str()), ("sentient-gateway", "unless-stopped", "host"));
        assert_eq!(spec.mounts.len(), 2);
        let mut before = GwSettings::default();
        before.machine.env = vec![EnvVar { name: "ST_GW_OLD".into(), value: Some("1".into()), editable: true, origin: "container".into() }];
        let plan = ApplyPlan {
            ui_enabled: Some(true),
            ui_listen: Some("0.0.0.0:8642".into()),
            restart_always: Some(false),
            serial_devices: Some(vec!["/dev/serial/by-id/usb-Example_RS485-if00".into()]),
            env: Some(vec![("ST_GW_NEW".into(), "2".into())]),
            rust_log: Some("debug".into()),
            ..ApplyPlan::default()
        };
        let new = planned_spec(&spec, &before, &plan, &|_| Some(986)).unwrap();
        assert!(new.env.contains(&"accessToken=Q1w2E3r4T5y6U7i8O9p0".to_string()));
        assert!(new.env.contains(&"ST_GW_UI=on".to_string()) && new.env.contains(&"RUST_LOG=debug".to_string()) && new.env.contains(&"ST_GW_NEW=2".to_string()));
        assert_eq!((new.restart.as_str(), new.group_add.clone()), ("no", vec!["986".to_string()]));
        let args = run_args(&new, "/run/user/1000/x.env").join(" ");
        assert!(!args.contains("Q1w2E3r4"), "{args}");
        assert!(args.contains("--network host --add-host host.docker.internal:host-gateway -v st-gw-config:/sentient_gateway/config -v /srv/example/certs:/certs:ro"), "{args}");
        assert!(args.contains("--device /dev/serial/by-id/usb-Example_RS485-if00:/dev/serial/by-id/usb-Example_RS485-if00 --group-add 986"));
        assert!(!args.contains("--restart"));
        assert!(args.ends_with("--env-file /run/user/1000/x.env quakestring/sentient-gateway-rs:4.16.2"));
        // A line break in a value is refused.
        let bad = ApplyPlan { env: Some(vec![("A".into(), "1\n2".into())]), ..ApplyPlan::default() };
        assert!(planned_spec(&spec, &before, &bad, &|_| None).is_err());
    }

    #[test]
    fn a_restart_loop_is_not_up() {
        assert!(stays_up("true false 0\n"));
        // A setting that makes the gateway exit: the restart policy keeps
        // bringing it back, and Docker still says running.
        assert!(!stays_up("true true 3"));
        assert!(!stays_up("true false 2"));
        assert!(!stays_up("false false 0"));
        assert!(!stays_up(""));
    }
}
