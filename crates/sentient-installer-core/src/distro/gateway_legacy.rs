// Copyright © 2016-2026 The SENTIENT Authors

//! The update from the older Python gateway (quakestring/sentient-gateway)
//! in Docker to the Rust gateway in Docker: what crate::gateway::update
//! does when the only gateway here is a Python container.
//!
//! The Rust gateway reads the Python gateway's own layout: `tb_gateway.json`
//! in its configuration folder, the `thingsboard` section, the `host`/
//! `accessToken`/`TB_GW_*` environment (gw/README.md, gw/docs/MIGRATION.md
//! §2). So the update keeps the configuration where it is and starts the
//! Rust image on it, in this order:
//!
//! 1. copy the configuration into a dated backup volume;
//! 2. stop the Python container and rename it (never removed yet): two
//!    gateways on one identity would knock each other off the platform;
//! 3. start the Rust gateway as `sentient-gateway` on the Python
//!    configuration, with new data and logs volumes, the same connection
//!    environment (host, port, token, client id) and the serial adapters;
//! 4. wait until it says "connected to platform";
//! 5. only then remove the Python container (its volumes stay);
//! 6. on any failure: remove the Rust container (and volumes this update
//!    created), give the Python container its name back and start it again
//!    if it was running.

use crate::gateway::CONTAINER;
use crate::progress::{Progress, ProgressFn};

/// A mount of a container: a named volume or a host folder.
#[derive(Debug, Clone, PartialEq)]
pub struct Mount {
    /// `volume` or `bind`.
    pub kind: String,
    /// The volume's name, or the host path.
    pub source: String,
    pub dest: String,
}

/// The Python gateway's container, from `docker inspect`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Legacy {
    pub name: String,
    pub image: String,
    pub running: bool,
    pub env: Vec<String>,
    pub mounts: Vec<Mount>,
    /// The compose folder it came from (its label), if any.
    pub compose_dir: Option<String>,
}

/// One container from `docker inspect <name>` (its JSON array).
pub fn parse_inspect_json(json: &str) -> Option<Legacy> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let c = v.as_array()?.first()?;
    let s = |v: Option<&serde_json::Value>| v.and_then(|x| x.as_str()).map(str::to_string);
    let name = s(c.get("Name"))?.trim_start_matches('/').to_string();
    let config = c.get("Config")?;
    let image = s(config.get("Image")).unwrap_or_default();
    let env = config.get("Env").and_then(|e| e.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
    let running = c.get("State").and_then(|st| st.get("Running")).and_then(|r| r.as_bool()).unwrap_or(false);
    let mounts = c
        .get("Mounts")
        .and_then(|m| m.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    let kind = s(m.get("Type"))?;
                    let source = if kind == "volume" { s(m.get("Name"))? } else { s(m.get("Source"))? };
                    let dest = s(m.get("Destination"))?;
                    matches!(kind.as_str(), "volume" | "bind").then_some(Mount { kind, source, dest })
                })
                .collect()
        })
        .unwrap_or_default();
    let compose_dir = config.get("Labels").and_then(|l| s(l.get("com.docker.compose.project.working_dir"))).filter(|d| !d.is_empty());
    Some(Legacy { name, image, running, env, mounts, compose_dir })
}

/// The bare names both gateways read for the platform connection
/// (Python: tb_utility.get_service_environmental_variables; Rust:
/// gateway-core config.rs).
const CONNECTION_ENV: [&str; 18] = [
    "host",
    "port",
    "type",
    "accessToken",
    "caCert",
    "privateKey",
    "cert",
    "clientId",
    "username",
    "password",
    "proxy_host",
    "proxy_port",
    "RATE_LIMITS",
    "DP_RATE_LIMITS",
    "MESSAGES_RATE_LIMITS",
    "DEVICE_RATE_LIMITS",
    "DEVICE_DP_RATE_LIMITS",
    "DEVICE_MESSAGES_RATE_LIMIT",
];

/// The variables carried to the Rust container: the connection and rate
/// limits, and every `TB_GW_`/`ST_GW_` setting except where its folders
/// are (the Rust image has its own). The image's own (PATH, PYTHON…,
/// configs…) stay behind.
pub fn carried_env(env: &[String]) -> Vec<String> {
    env.iter()
        .filter(|kv| {
            let Some((k, _)) = kv.split_once('=') else { return false };
            let prefixed = k.strip_prefix("TB_GW_").or_else(|| k.strip_prefix("ST_GW_"));
            match prefixed {
                Some(rest) => !matches!(rest, "CONFIG_DIR" | "DATA_DIR" | "LOGS_PATH" | "EXTENSIONS_DIR"),
                None => CONNECTION_ENV.contains(&k),
            }
        })
        .cloned()
        .collect()
}

/// The environment names a platform connection: a host, and a token or
/// user name (or the X.509 / TLS material).
fn names_connection(env: &[String]) -> bool {
    let has = |names: &[&str]| names.iter().any(|n| env.iter().any(|kv| kv.split_once('=').is_some_and(|(k, v)| k == *n && !v.trim().is_empty())));
    has(&["host", "TB_GW_HOST", "ST_GW_HOST"])
        && has(&["accessToken", "TB_GW_ACCESS_TOKEN", "ST_GW_ACCESS_TOKEN", "username", "TB_GW_USERNAME", "ST_GW_USERNAME", "clientId", "TB_GW_CLIENT_ID", "ST_GW_CLIENT_ID", "cert", "TB_GW_CERT", "ST_GW_CERT"])
}

/// Its configuration folder's mount (`…/config`: `/sentient_gateway/config`
/// in the image, `/thingsboard_gateway/config` in older compose files).
pub fn config_mount(l: &Legacy) -> Option<&Mount> {
    l.mounts.iter().find(|m| {
        let d = m.dest.trim_end_matches('/');
        d == "/sentient_gateway/config" || d == "/thingsboard_gateway/config"
    })
}

/// Its extensions folder (custom Python code the Rust gateway does not run).
fn extensions_mount(l: &Legacy) -> Option<&Mount> {
    l.mounts.iter().find(|m| m.dest.trim_end_matches('/').ends_with("/extensions"))
}

/// What the update cannot carry to the Rust gateway (gw/docs/MIGRATION.md
/// §5), for the (i) before Update and the closing message.
pub const NOT_CARRIED: [&str; 5] = [
    "Custom Python converters a connector names are not loaded; the built-in converter runs instead.",
    "Python code inside a configuration is not run: a REST sub-request's processingFunction, MQTT's useEval.",
    "Connectors that register over gRPC are not supported.",
    "Data still queued in the old gateway's file storage is not sent by the new one.",
    "Some connectors behave differently in detail: see \"What is deliberately different\" in the gateway's documentation.",
];

/// A volume of the Rust container (the ones it gets new when the Python
/// one had none for them).
pub const DATA_VOLUME: (&str, &str) = ("st-gw-data", "/sentient_gateway/data");
pub const LOGS_VOLUME: (&str, &str) = ("st-gw-logs", "/sentient_gateway/logs");

/// The Rust container to start.
#[derive(Debug, Clone, PartialEq)]
pub struct RunSpec {
    pub name: String,
    pub image: String,
    /// (source, destination): a volume name or a host path.
    pub mounts: Vec<(String, String)>,
    pub env: Vec<String>,
}

/// What the update will do, worked out before anything changes.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub legacy: String,
    /// The Python container's name while it waits (stopped) for the result.
    pub parked: String,
    pub was_running: bool,
    /// The configuration to back up (a mount) and the backup volume.
    pub backup: Option<(Mount, String)>,
    pub run: RunSpec,
    /// The volumes the Rust container needs that the update may create.
    pub new_volumes: Vec<String>,
    /// Said before and after: what does not carry over.
    pub not_carried: Vec<String>,
}

/// The plan for `l` to the Rust gateway `version`; `now` dates the names
/// (`YYYYmmdd-HHMMSS`). Refused (nothing changed) when the connection
/// could not be carried.
pub fn plan(l: &Legacy, version: &str, now: &str) -> Result<Plan, String> {
    let config = config_mount(l).cloned();
    let env = carried_env(&l.env);
    if config.is_none() && !names_connection(&env) {
        return Err(format!(
            "The older gateway's container {} keeps its configuration inside the container (no volume) and its environment names no platform connection, so the update could not carry it over. Nothing was changed.",
            l.name
        ));
    }
    let mut mounts = Vec::new();
    match &config {
        Some(m) => mounts.push((m.source.clone(), "/sentient_gateway/config".to_string())),
        None => mounts.push(("st-gw-config".to_string(), "/sentient_gateway/config".to_string())),
    }
    // Its own logs (and data) folders when it had them; else new volumes.
    for (vol, dest) in [DATA_VOLUME, LOGS_VOLUME] {
        let own = l.mounts.iter().find(|m| m.dest.trim_end_matches('/') == dest || (dest.ends_with("/logs") && m.dest.trim_end_matches('/') == "/thingsboard_gateway/logs"));
        mounts.push((own.map(|m| m.source.clone()).unwrap_or_else(|| vol.to_string()), dest.to_string()));
    }
    // Volumes the older container did not have: made, or taken when free.
    let new_volumes = mounts.iter().filter(|(s, _)| !s.starts_with('/') && !l.mounts.iter().any(|m| m.source == *s)).map(|(s, _)| s.clone()).collect();
    let mut not_carried: Vec<String> = NOT_CARRIED.iter().map(|s| s.to_string()).collect();
    if let Some(e) = extensions_mount(l) {
        not_carried.push(format!("The extensions folder ({}) is not used: it holds Python code.", e.source));
    }
    if let Some(dir) = &l.compose_dir {
        not_carried.push(format!("The compose file in {dir} still names the older gateway: change it or stop using it, or docker compose up would start the older one again."));
    }
    let backup = config.map(|m| {
        let base = if m.kind == "volume" { m.source.clone() } else { "sentient-gateway-config".to_string() };
        let vol = format!("{base}-backup-{now}");
        (m, vol)
    });
    Ok(Plan {
        legacy: l.name.clone(),
        parked: format!("{}-older-{now}", l.name),
        was_running: l.running,
        backup,
        run: RunSpec { name: CONTAINER.to_string(), image: format!("{}:{version}", crate::gateway::IMAGE), mounts, env },
        new_volumes,
        not_carried,
    })
}

/// A new volume another container already uses (another gateway's
/// st-gw-data) is never shared: this update gets a dated one instead.
pub fn resolve_volumes(p: &mut Plan, in_use: &dyn Fn(&str) -> bool, now: &str) {
    for v in p.new_volumes.iter_mut() {
        if in_use(v) {
            let fresh = format!("{v}-{now}");
            for m in p.run.mounts.iter_mut().filter(|(s, _)| s == v) {
                m.0 = fresh.clone();
            }
            *v = fresh;
        }
    }
}

/// How far the update got, for the way back.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Done {
    pub stopped: bool,
    pub parked: bool,
    pub started: bool,
    /// Volumes this update created (none of them existed before).
    pub created: Vec<String>,
}

/// One step of the way back.
#[derive(Debug, Clone, PartialEq)]
pub enum Undo {
    RemoveRust,
    RemoveVolume(String),
    Unpark,
    StartOlder,
}

/// The way back from `done`: the Rust container and what it created go,
/// the Python container gets its name back, and runs again when it ran
/// before. The backup volume stays.
pub fn undo_steps(p: &Plan, done: &Done) -> Vec<Undo> {
    let mut v = Vec::new();
    if done.started {
        v.push(Undo::RemoveRust);
    }
    for vol in &done.created {
        v.push(Undo::RemoveVolume(vol.clone()));
    }
    if done.parked {
        v.push(Undo::Unpark);
    }
    if done.stopped && p.was_running {
        v.push(Undo::StartOlder);
    }
    v
}

/// The Rust gateway's log line once the platform accepted it
/// (gw/crates/gateway-core/src/uplink.rs).
pub fn connected(logs: &str) -> bool {
    logs.lines().any(|l| l.contains("connected to platform"))
}

/// The `docker run` line for `spec`, with the serial adapters (gid added).
pub fn run_line(spec: &RunSpec, devices: &[super::gateway_docker::SerialDevice]) -> String {
    let q = super::shell_quote;
    let mut line = format!("docker run -d --name {} --restart unless-stopped --network host", q(&spec.name));
    for (s, d) in &spec.mounts {
        line.push_str(&format!(" -v {}", q(&format!("{s}:{d}"))));
    }
    line.push_str(&super::gateway_docker::device_flags(devices));
    for e in &spec.env {
        line.push_str(&format!(" -e {}", q(e)));
    }
    line.push_str(&format!(" {}", q(&spec.image)));
    line
}

/// A Docker command through the Docker shell: its output, or its error
/// output (never the command: it can hold the token).
#[cfg(not(windows))]
pub(super) fn sh(cmd: &str) -> Result<String, String> {
    let (prog, args) = super::shell_args(cmd);
    let argrefs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    match super::shell_output(&prog, &argrefs, super::RUN_LIMIT) {
        Some((true, out, _)) => Ok(crate::sys::decode(&out)),
        Some((false, out, err)) => {
            let e = crate::sys::decode(&err);
            let e = if e.trim().is_empty() { crate::sys::decode(&out) } else { e };
            Err(e.trim().to_string())
        }
        None => Err("Docker could not be run.".into()),
    }
}

/// The older Python gateway's container here, read in full.
#[cfg(not(windows))]
pub fn find() -> Option<Legacy> {
    let name = super::gateway_docker::legacy()?;
    sh(&format!("docker inspect {}", super::shell_quote(&name))).ok().as_deref().and_then(parse_inspect_json)
}

/// What the update would leave behind, for the page's (i): the fixed list
/// and this container's own notes.
#[cfg(not(windows))]
pub fn not_carried() -> Vec<String> {
    find().and_then(|l| plan(&l, "x", "x").ok()).map(|p| p.not_carried).unwrap_or_else(|| NOT_CARRIED.iter().map(|s| s.to_string()).collect())
}

/// Run the update (module doc). Gives the closing message.
#[cfg(not(windows))]
pub fn update(sink: &ProgressFn, version: &str) -> Result<String, String> {
    let q = super::shell_quote;
    let step = |n: String| sink(Progress::Step { name: n });
    let l = find().ok_or("There is no older gateway container here.")?;
    let now = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let p = plan(&l, version, &now)?;
    // Before anything changes: the image, and the backup.
    step(format!("Pulling {}", p.run.image));
    super::run_stream(sink, &format!("docker pull {}", q(&p.run.image)))?;
    if let Some((m, vol)) = &p.backup {
        step(format!("Backing up the gateway's configuration to the volume {vol}"));
        sh(&format!("docker volume create {} >/dev/null", q(vol))).map_err(|e| format!("The backup volume could not be made: {e}. Nothing was changed."))?;
        // The older image's own shell copies, so no other image is needed.
        let copy = format!(
            "docker run --rm --pull=never --network none --entrypoint sh -v {}:/from:ro -v {}:/to {} -c 'cp -a /from/. /to/'",
            q(&m.source),
            q(vol),
            q(&l.image)
        );
        if let Err(e) = sh(&copy) {
            let _ = sh(&format!("docker volume rm {}", q(vol)));
            return Err(format!("The configuration could not be backed up: {e}. Nothing was changed."));
        }
    }
    let mut p = p;
    resolve_volumes(&mut p, &|v| sh(&format!("docker ps -aq --filter volume={}", q(v))).is_ok_and(|o| !o.trim().is_empty()), &now);
    let p = p;
    let mut done = Done::default();
    let result = (|| -> Result<(), String> {
        if l.running {
            step(format!("Stopping the older gateway ({})", l.name));
            sh(&format!("docker stop -t 25 {}", q(&l.name)))?;
            done.stopped = true;
        }
        sh(&format!("docker rename {} {}", q(&l.name), q(&p.parked)))?;
        done.parked = true;
        for v in &p.new_volumes {
            if sh(&format!("docker volume inspect {} >/dev/null", q(v))).is_err() {
                sh(&format!("docker volume create {} >/dev/null", q(v)))?;
                done.created.push(v.clone());
            }
        }
        step(format!("Starting Sentient Gateway {version}"));
        let devices = super::gateway_docker::serial_devices();
        // Counted before the run: a container created but not started is
        // removed on the way back too.
        done.started = true;
        sh(&run_line(&p.run, &devices)).map_err(|e| format!("The new gateway did not start: {e}"))?;
        // Tests: a failure after the new gateway started (the way back).
        #[cfg(debug_assertions)]
        if std::env::var_os("SPM_DEBUG_GATEWAY_UPDATE_FAIL").is_some() {
            std::thread::sleep(std::time::Duration::from_secs(5));
            return Err("a failure made on purpose for a test (SPM_DEBUG_GATEWAY_UPDATE_FAIL)".into());
        }
        step("Waiting for the new gateway to connect to SENTIENT".into());
        wait_connected(&p.run.name, std::time::Duration::from_secs(120))
    })();
    match result {
        Ok(()) => {
            step(format!("Removing the older gateway's container ({})", p.parked));
            let _ = sh(&format!("docker rm {}", q(&p.parked)));
            let mut msg = format!("The gateway is updated to Sentient Gateway {version} and connected to SENTIENT.");
            if let Some((_, vol)) = &p.backup {
                msg.push_str(&format!(" The older gateway's configuration is backed up in the volume {vol}."));
            }
            if let Some(dir) = &l.compose_dir {
                msg.push_str(&format!(" Its compose file in {dir} still names the older gateway."));
            }
            sink(Progress::Done { message: msg.clone() });
            Ok(msg)
        }
        Err(e) => {
            step("Putting the older gateway back".into());
            let mut notes = Vec::new();
            for u in undo_steps(&p, &done) {
                let r = match &u {
                    Undo::RemoveRust => sh(&format!("docker rm -f {}", q(&p.run.name))).or_else(|e| if e.contains("No such container") { Ok(String::new()) } else { Err(e) }),
                    Undo::RemoveVolume(v) => sh(&format!("docker volume rm {}", q(v))),
                    Undo::Unpark => sh(&format!("docker rename {} {}", q(&p.parked), q(&p.legacy))),
                    Undo::StartOlder => sh(&format!("docker start {}", q(&p.legacy))),
                };
                if let Err(x) = r {
                    notes.push(format!("{u:?}: {x}"));
                }
            }
            let back = if notes.is_empty() {
                format!("The older gateway ({}) is as it was{}.", p.legacy, if p.was_running { " and running" } else { "" })
            } else {
                format!("Putting the older gateway back did not fully work ({}).", notes.join("; "))
            };
            Err(format!("The update stopped: {e}. {back}"))
        }
    }
}

/// Until the container logs "connected to platform"; a container that
/// stopped, or no line within `limit`, fails with its last lines.
#[cfg(not(windows))]
fn wait_connected(name: &str, limit: std::time::Duration) -> Result<(), String> {
    let q = super::shell_quote;
    let start = std::time::Instant::now();
    loop {
        let logs = sh(&format!("docker logs {} 2>&1", q(name))).unwrap_or_default();
        if connected(&logs) {
            return Ok(());
        }
        let running = sh(&format!("docker inspect -f '{{{{.State.Running}}}}' {}", q(name))).is_ok_and(|o| o.trim() == "true");
        let tail: Vec<&str> = logs.lines().rev().take(3).collect();
        let tail = tail.into_iter().rev().collect::<Vec<_>>().join(" / ");
        if !running {
            return Err(format!("the new gateway stopped ({tail})"));
        }
        if start.elapsed() > limit {
            return Err(format!("the new gateway did not connect to SENTIENT within {} seconds ({tail})", limit.as_secs()));
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `docker inspect` of a Python gateway container from a generated
    /// compose file (fictional names, token shortened).
    const INSPECT: &str = r#"[{"Name":"/st-gateway","State":{"Running":true},
      "Config":{"Image":"quakestring/sentient-gateway:latest",
        "Env":["host=192.168.10.5","port=1883","accessToken=EXAMPLETOKEN","TB_GW_CLIENT_ID=gw-example","TB_GW_CONFIG_DIR=/thingsboard_gateway/config","PATH=/usr/local/bin:/usr/bin","PYTHONPATH=/:/sentient_gateway:","configs=/sentient_gateway/config","GPG_KEY=ABC"],
        "Labels":{"com.docker.compose.project":"example","com.docker.compose.project.working_dir":"/srv/example-site"}},
      "Mounts":[{"Type":"volume","Name":"gw-config","Source":"/var/lib/docker/volumes/gw-config/_data","Destination":"/sentient_gateway/config"},
                {"Type":"volume","Name":"gw-logs","Source":"/var/lib/docker/volumes/gw-logs/_data","Destination":"/sentient_gateway/logs"},
                {"Type":"bind","Source":"/srv/example-site/extensions","Destination":"/sentient_gateway/extensions"}]}]"#;

    #[test]
    fn reads_the_older_container() {
        let l = parse_inspect_json(INSPECT).unwrap();
        assert_eq!((l.name.as_str(), l.running), ("st-gateway", true));
        assert_eq!(l.compose_dir.as_deref(), Some("/srv/example-site"));
        assert_eq!(config_mount(&l).map(|m| m.source.as_str()), Some("gw-config"));
        assert_eq!(l.mounts[2], Mount { kind: "bind".into(), source: "/srv/example-site/extensions".into(), dest: "/sentient_gateway/extensions".into() });
        assert!(parse_inspect_json("[]").is_none());
    }

    #[test]
    fn the_connection_and_identity_carry_over() {
        let l = parse_inspect_json(INSPECT).unwrap();
        // Host, port, token and client id; never the image's own or its folders.
        assert_eq!(carried_env(&l.env), vec!["host=192.168.10.5", "port=1883", "accessToken=EXAMPLETOKEN", "TB_GW_CLIENT_ID=gw-example"]);
        assert!(names_connection(&carried_env(&l.env)));
        assert!(!names_connection(&["host=10.0.0.1".to_string()]));
        assert!(names_connection(&["TB_GW_HOST=10.0.0.1".to_string(), "TB_GW_USERNAME=u".to_string()]));
    }

    #[test]
    fn the_plan_keeps_the_configuration_where_it_is() {
        let l = parse_inspect_json(INSPECT).unwrap();
        let p = plan(&l, "4.16.3", "20261118-120000").unwrap();
        assert_eq!(p.parked, "st-gateway-older-20261118-120000");
        assert_eq!(p.run.name, "sentient-gateway");
        assert_eq!(p.run.image, "quakestring/sentient-gateway-rs:4.16.3");
        assert_eq!(
            p.run.mounts,
            vec![
                ("gw-config".to_string(), "/sentient_gateway/config".to_string()),
                ("st-gw-data".to_string(), "/sentient_gateway/data".to_string()),
                // Its own logs volume goes on.
                ("gw-logs".to_string(), "/sentient_gateway/logs".to_string()),
            ]
        );
        assert_eq!(p.new_volumes, vec!["st-gw-data"]);
        // Another gateway's st-gw-data is never shared.
        let mut q = p.clone();
        resolve_volumes(&mut q, &|v| v == "st-gw-data", "20261118-120000");
        assert_eq!(q.new_volumes, vec!["st-gw-data-20261118-120000"]);
        assert_eq!(q.run.mounts[1].0, "st-gw-data-20261118-120000");
        let mut free = p.clone();
        resolve_volumes(&mut free, &|_| false, "x");
        assert_eq!(free, p);
        assert_eq!(p.backup.as_ref().map(|(m, v)| (m.source.as_str(), v.as_str())), Some(("gw-config", "gw-config-backup-20261118-120000")));
        assert!(p.not_carried.iter().any(|n| n.contains("/srv/example-site/extensions")));
        assert!(p.not_carried.iter().any(|n| n.contains("compose file in /srv/example-site")));
        // It is an update: never called anything else to the user.
        assert!(p.not_carried.iter().all(|n| !n.to_lowercase().contains("migrat")));
        let line = run_line(&p.run, &[]);
        assert!(line.starts_with("docker run -d --name 'sentient-gateway' --restart unless-stopped --network host -v 'gw-config:/sentient_gateway/config'"), "{line}");
        assert!(line.contains(" -e 'accessToken=EXAMPLETOKEN'") && line.contains(" -e 'TB_GW_CLIENT_ID=gw-example'") && !line.contains("PYTHONPATH"));
        assert!(line.ends_with(" 'quakestring/sentient-gateway-rs:4.16.3'"));
    }

    #[test]
    fn a_bind_mounted_configuration_and_none() {
        let mut l = parse_inspect_json(INSPECT).unwrap();
        l.mounts[0] = Mount { kind: "bind".into(), source: "/srv/example-site/config".into(), dest: "/thingsboard_gateway/config".into() };
        let p = plan(&l, "4.16.3", "t").unwrap();
        assert_eq!(p.run.mounts[0], ("/srv/example-site/config".to_string(), "/sentient_gateway/config".to_string()));
        assert_eq!(p.backup.unwrap().1, "sentient-gateway-config-backup-t");
        // No configuration volume: the environment must hold the connection.
        l.mounts.clear();
        let p = plan(&l, "4.16.3", "t").unwrap();
        assert_eq!(p.run.mounts[0].0, "st-gw-config");
        assert_eq!(p.new_volumes, vec!["st-gw-config", "st-gw-data", "st-gw-logs"]);
        assert!(p.backup.is_none());
        l.env = vec!["PATH=/usr/bin".into()];
        assert!(plan(&l, "4.16.3", "t").unwrap_err().contains("Nothing was changed"));
    }

    #[test]
    fn the_way_back_matches_how_far_it_got() {
        let l = parse_inspect_json(INSPECT).unwrap();
        let p = plan(&l, "4.16.3", "t").unwrap();
        // Stopped and parked, the new one not started: name back, start again.
        assert_eq!(undo_steps(&p, &Done { stopped: true, parked: true, ..Done::default() }), vec![Undo::Unpark, Undo::StartOlder]);
        // Started but never connected: the new one and its new volumes go first.
        let all = Done { stopped: true, parked: true, started: true, created: vec!["st-gw-data".into()] };
        assert_eq!(undo_steps(&p, &all), vec![Undo::RemoveRust, Undo::RemoveVolume("st-gw-data".into()), Undo::Unpark, Undo::StartOlder]);
        // A stopped older gateway stays stopped.
        let stopped = Plan { was_running: false, ..p.clone() };
        assert_eq!(undo_steps(&stopped, &Done { parked: true, ..Done::default() }), vec![Undo::Unpark]);
        assert!(undo_steps(&p, &Done::default()).is_empty());
    }

    #[test]
    fn connected_is_the_uplink_line() {
        assert!(connected("2026-11-18T07:00:01Z  INFO gateway_core::uplink: connected to platform epoch=1\n"));
        assert!(!connected("2026-11-18T07:00:01Z  WARN gateway_core::uplink: connection refused\n"));
    }

    /// The real update, against Docker here (SPM_GW_REALTEST_VERSION, a
    /// Python gateway container being the only older one): run by hand.
    #[cfg(not(windows))]
    #[test]
    #[ignore]
    fn real_update() {
        let v = std::env::var("SPM_GW_REALTEST_VERSION").expect("SPM_GW_REALTEST_VERSION");
        let sink: ProgressFn = std::sync::Arc::new(|p| eprintln!("PROGRESS {p:?}"));
        let r = update(&sink, &v);
        eprintln!("RESULT {r:?}");
    }

    /// After it: the normal update of the gateway it started (a recreate
    /// that keeps its volumes and environment). Run by hand.
    #[cfg(not(windows))]
    #[test]
    #[ignore]
    fn real_recreate() {
        let v = std::env::var("SPM_GW_REALTEST_VERSION").expect("SPM_GW_REALTEST_VERSION");
        let sink: ProgressFn = std::sync::Arc::new(|p| eprintln!("PROGRESS {p:?}"));
        eprintln!("RESULT {:?}", super::super::gateway_docker::update(&sink, &v));
    }
}
