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
/// the host network, the named volumes, the image's env contract, and each
/// serial adapter at the same path inside (a by-id path stays the one a
/// Modbus RTU connector names) with its group added, so the image's
/// non-root user may open it.
pub fn run_command(image: &str, host: &str, port: u16, token: &str, devices: &[SerialDevice]) -> String {
    let q = super::shell_quote;
    let volumes: String = VOLUMES.iter().map(|(v, p)| format!(" -v {v}:{p}")).collect();
    format!(
        "docker run -d --name {CONTAINER} --restart unless-stopped --network host{volumes}{} -e host={} -e port={port} -e accessToken={} {}",
        device_flags(devices),
        q(host),
        q(token),
        q(image)
    )
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
        let dir = k.strip_prefix("TB_GW_").or_else(|| k.strip_prefix("ST_GW_")).is_some_and(|r| matches!(r, "CONFIG_DIR" | "DATA_DIR" | "LOGS_PATH" | "EXTENSIONS_DIR"));
        k == "PATH" || dir || old_image_env.iter().any(|e| e == kv)
    };
    let mut spec = current.clone();
    spec.image = image.to_string();
    spec.env = current.env.iter().filter(|kv| !image_owned(kv)).cloned().collect();
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
        super::run(sink, &run_command(&image, host, port, token, &devices))?;
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
    Some(Spec {
        name: c.get("Name")?.as_str()?.trim_start_matches('/').to_string(),
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
    a.extend(["--env-file".into(), env_file.to_string(), spec.image.clone()]);
    a
}

/// `docker inspect <name>` read as a Spec.
pub fn inspect_spec(name: &str) -> Option<Spec> {
    docker(&["inspect", name]).filter(|(ok, _)| *ok).and_then(|(_, o)| parse_spec(&o))
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
    if let Some(out) = &out {
        gs::connected_from_read(&mut s, out);
    }
    s.caps = caps;
    let why = not_editable(status);
    let editable = why.is_none();
    s.machine.not_editable = why;
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

/// The container's spec with `plan` applied (crate::gateway_settings::ApplyPlan).
pub fn planned_spec(spec: &Spec, before: &crate::gateway_settings::GwSettings, plan: &crate::gateway_settings::ApplyPlan, gid_of: &dyn Fn(&str) -> Option<u32>) -> Result<Spec, String> {
    let mut s = spec.clone();
    if let Some(on) = plan.ui_enabled {
        set_env(&mut s.env, "ST_GW_UI", Some(if on { "on" } else { "off" }));
    }
    if let Some(l) = &plan.ui_listen {
        set_env(&mut s.env, "ST_GW_UI_LISTEN", Some(l));
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
fn write_env_file(env: &[String]) -> Result<std::path::PathBuf, String> {
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
        recreate_container(sink, &new, Some(format!("Stopping the container {name}")), &format!("Creating {name} again with the same volumes"), "with the new settings")
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
        let started = matches!(docker(&refs), Some((true, _)));
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
    docker(&["inspect", "--format", "{{.State.Running}}", name]).is_some_and(|(ok, o)| ok && o.trim() == "true")
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
        let none = run_command("quakestring/sentient-gateway-rs:4.16.2", "127.0.0.1", 1883, "tok", &[]);
        assert!(!none.contains("--device") && !none.contains("--group-add"));
        assert!(none.contains("--network host -v st-gw-config:/sentient_gateway/config"));
        let devs = [
            SerialDevice { path: "/dev/serial/by-id/usb-Example_RS485_A1-if00-port0".into(), gid: 986 },
            SerialDevice { path: "/dev/ttyACM0".into(), gid: 986 },
            SerialDevice { path: "/dev/ttyUSB3".into(), gid: 0 },
        ];
        let line = run_command("quakestring/sentient-gateway-rs:4.16.2", "127.0.0.1", 1883, "tok", &devs);
        assert!(line.contains(" --device '/dev/serial/by-id/usb-Example_RS485_A1-if00-port0:/dev/serial/by-id/usb-Example_RS485_A1-if00-port0'"), "{line}");
        assert!(line.contains(" --device '/dev/ttyACM0:/dev/ttyACM0'") && line.contains(" --device '/dev/ttyUSB3:/dev/ttyUSB3'"));
        // One group, once; root's group never.
        assert_eq!(line.matches("--group-add").count(), 1);
        assert!(line.contains(" --group-add 986 ") && !line.contains("--group-add 0"));
        assert!(line.ends_with("'quakestring/sentient-gateway-rs:4.16.2'"));
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
        assert_eq!(spec.mounts.len(), 2);
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
        assert!(args.contains("--network host -v st-gw-config:/sentient_gateway/config -v /srv/example/certs:/certs:ro"), "{args}");
        assert!(args.contains("--device /dev/serial/by-id/usb-Example_RS485-if00:/dev/serial/by-id/usb-Example_RS485-if00 --group-add 986"));
        assert!(!args.contains("--restart"));
        assert!(args.ends_with("--env-file /run/user/1000/x.env quakestring/sentient-gateway-rs:4.16.2"));
        // A line break in a value is refused.
        let bad = ApplyPlan { env: Some(vec![("A".into(), "1\n2".into())]), ..ApplyPlan::default() };
        assert!(planned_spec(&spec, &before, &bad, &|_| None).is_err());
    }
}
