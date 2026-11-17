// Copyright © 2016-2026 The SENTIENT Authors

//! Finding a SENTIENT Docker stack the Platform Manager did not install (our
//! compose files, an older install, a lost installation record) and reading
//! its configuration back from Docker, so it can be managed without
//! reinstalling anything.
//!
//! Everything comes from `docker inspect`: SENTIENT's connection string and
//! environment, the PostgreSQL and ClickHouse containers it points at, the
//! published ports, the volumes and the compose project that started it.
//! The stack keeps running from its own compose files (`DeployConfig::
//! external`); operations that would rewrite them are refused until the
//! operator lets the Platform Manager take the stack over (`take_over`).

use serde_json::Value;

use super::{DeployConfig, ExternalCompose, is_running, run_output, run_stream, shell_quote, write_stack_files};
use crate::progress::{Progress, ProgressFn};
use crate::stack::{Flavor, PgTuning, Tuning};

/// A SENTIENT stack found on this machine.
#[derive(Debug, Clone)]
pub struct FoundStack {
    /// Its configuration, passwords included.
    pub cfg: DeployConfig,
    /// From the image's version label.
    pub version: Option<String>,
    /// Whether SENTIENT is running now.
    pub running: bool,
    /// What the operator should know (limits, differences).
    pub notes: Vec<String>,
    /// Why it cannot be managed, when it cannot.
    pub problem: Option<String>,
    /// Whether the Platform Manager can take the compose file over (same
    /// container names, volumes and mount points as its own stack), and why
    /// not otherwise.
    pub take_over: Result<(), String>,
    /// SENTIENT's container id, to tell the same container seen twice
    /// (Docker Desktop's engine through more than one WSL distro).
    pub container_id: String,
}

/// Variables the engine derives itself; everything else in SENTIENT's
/// environment that is not tuning is kept as an extra variable.
const ENGINE_ENV: &[&str] = &[
    "DATABASE_URL",
    "POSTGRES_DB",
    "POSTGRES_USER",
    "POSTGRES_PASSWORD",
    "CLICKHOUSE_URL",
    "CLICKHOUSE_DATABASE",
    "CLICKHOUSE_USER",
    "CLICKHOUSE_PASSWORD",
    "TS_TYPE",
    "TS_LATEST_TYPE",
    "TS_LATEST_CACHE",
    "TS_LATEST_CACHE_TTL_SECS",
    "SENTIENT_TIMESCALE_ENABLED",
    "LICENSE_SERVER_URL",
    "REPORT_OUTPUT_DIR",
    "VC_REPOS_PATH",
];

/// `postgresql://user:pass@host:port/db` → its parts, percent-decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbUrl {
    pub user: String,
    pub password: String,
    pub host: String,
    pub port: u16,
    pub database: String,
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn parse_db_url(url: &str) -> Option<DbUrl> {
    let rest = url.split_once("://")?.1;
    let (cred, hostpart) = rest.rsplit_once('@')?;
    let (user, password) = cred.split_once(':').unwrap_or((cred, ""));
    let (hostport, database) = hostpart.split_once('/').unwrap_or((hostpart, ""));
    let database = database.split('?').next().unwrap_or_default();
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (hostport, 5432),
    };
    Some(DbUrl {
        user: percent_decode(user),
        password: percent_decode(password),
        host: host.to_string(),
        port,
        database: percent_decode(database),
    })
}

/// The password in a PostgreSQL connection string.
pub fn url_password(url: &str) -> Option<String> {
    parse_db_url(url).map(|u| u.password).filter(|p| !p.is_empty())
}

/// `http://clickhouse:8123` → ("clickhouse", 8123).
fn url_host_port(url: &str) -> Option<(String, u16)> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let hostport = rest.split('/').next()?;
    match hostport.rsplit_once(':') {
        Some((h, p)) => Some((h.to_string(), p.parse().ok()?)),
        None => Some((hostport.to_string(), 8123)),
    }
}

/// `registry:5000/quakestring/sentient:4.3` → `sentient`.
pub(super) fn image_repo_name(image: &str) -> &str {
    let last = image.rsplit('/').next().unwrap_or(image);
    last.split(['@', ':']).next().unwrap_or(last)
}

/// A container name as a stack name (letters, digits and underscores).
fn stack_name_for(container: &str) -> String {
    container.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

fn inspect(ids: &[String]) -> Vec<Value> {
    if ids.is_empty() {
        return Vec::new();
    }
    let args: Vec<String> = ids.iter().map(|i| shell_quote(i)).collect();
    run_output(&format!("docker inspect {}", args.join(" ")))
        .filter(|(ok, _)| *ok)
        .and_then(|(_, out)| serde_json::from_str::<Vec<Value>>(&out).ok())
        .unwrap_or_default()
}

fn name(c: &Value) -> String {
    c["Name"].as_str().unwrap_or_default().trim_start_matches('/').to_string()
}

fn env(c: &Value) -> Vec<(String, String)> {
    c["Config"]["Env"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str()?.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

fn env_get<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
    env.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn label<'a>(c: &'a Value, key: &str) -> Option<&'a str> {
    c["Config"]["Labels"][key].as_str().filter(|s| !s.is_empty())
}

/// The host port published for a container port (`"8080/tcp"`).
fn host_port(c: &Value, port: &str) -> Option<u16> {
    c["HostConfig"]["PortBindings"][port]
        .as_array()?
        .iter()
        .find_map(|b| b["HostPort"].as_str()?.parse().ok())
}

/// The named volume mounted at `dest`.
fn volume_at(c: &Value, dest: &str) -> Option<String> {
    c["Mounts"].as_array()?.iter().find_map(|m| {
        (m["Type"].as_str() == Some("volume") && m["Destination"].as_str() == Some(dest))
            .then(|| m["Name"].as_str().map(String::from))
            .flatten()
    })
}

fn image_env(image_id: &str) -> Vec<(String, String)> {
    run_output(&format!("docker image inspect -f '{{{{json .Config.Env}}}}' {}", shell_quote(image_id)))
        .filter(|(ok, _)| *ok)
        .and_then(|(_, out)| serde_json::from_str::<Vec<String>>(out.trim()).ok())
        .map(|v| v.into_iter().filter_map(|e| e.split_once('=').map(|(k, v)| (k.to_string(), v.to_string()))).collect())
        .unwrap_or_default()
}

/// PostgreSQL settings passed as `-c name=value` on the container's command.
fn pg_tuning(pg: &Value) -> PgTuning {
    let mut t = PgTuning::default();
    let args: Vec<&str> = pg["Config"]["Cmd"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let mut settings = Vec::new();
    for (i, a) in args.iter().enumerate() {
        if *a == "-c" {
            if let Some(kv) = args.get(i + 1) {
                settings.push(*kv);
            }
        } else if let Some(kv) = a.strip_prefix("-c") {
            settings.push(kv);
        }
    }
    for kv in settings {
        let Some((k, v)) = kv.split_once('=') else { continue };
        match k.trim() {
            "max_connections" => {
                if let Ok(n) = v.trim().parse() {
                    t.pg_max_connections = n
                }
            }
            "shared_buffers" => t.pg_shared_buffers = v.trim().into(),
            "work_mem" => t.pg_work_mem = v.trim().into(),
            "effective_cache_size" => t.pg_effective_cache_size = v.trim().into(),
            _ => {}
        }
    }
    t
}

/// The container a service name in a connection string points at: the
/// compose service of that name in the same project, else a container of
/// that name.
fn service_container<'a>(all: &'a [Value], project: Option<&str>, host: &str) -> Option<&'a Value> {
    all.iter()
        .find(|c| project.is_some() && label(c, "com.docker.compose.project") == project && label(c, "com.docker.compose.service") == Some(host))
        .or_else(|| all.iter().find(|c| name(c) == host))
}

/// Every SENTIENT stack Docker knows about here, running or stopped.
pub fn find() -> Vec<FoundStack> {
    let ids: Vec<String> = run_output("docker ps -aq --no-trunc")
        .filter(|(ok, _)| *ok)
        .map(|(_, out)| out.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect())
        .unwrap_or_default();
    let all = inspect(&ids);
    all.iter()
        .filter(|c| {
            let e = env(c);
            let image = c["Config"]["Image"].as_str().unwrap_or_default();
            env_get(&e, "DATABASE_URL").is_some()
                && (image_repo_name(image) == "sentient" || label(c, "com.docker.compose.service") == Some("sentient"))
        })
        .map(|app| describe(app, &all))
        .collect()
}

/// Windows: every SENTIENT stack in the WSL distro `distro` (the
/// operator's own, e.g. Ubuntu), each configuration naming that distro so
/// everything done to it afterwards runs there. Runs commands in the
/// distro: call for a running one (super::hosts).
pub fn find_in_distro(distro: &str) -> Vec<FoundStack> {
    // The Platform Manager's own distro ("sentient", also what the early
    // releases imported Ubuntu as, with /opt/sentient/docker-compose.yml)
    // is recorded as None: its own, which it may stop, repair and remove.
    let own = super::is_own_distro(distro);
    super::with_docker_distro(Some(distro), find)
        .into_iter()
        .map(|mut f| {
            f.cfg.wsl_distro = if own { None } else { Some(distro.trim().to_string()) };
            f
        })
        .collect()
}

fn describe(app: &Value, all: &[Value]) -> FoundStack {
    let app_name = name(app);
    let app_env = env(app);
    let project = label(app, "com.docker.compose.project");
    let mut notes = Vec::new();
    let mut problem = None;
    let mut cfg = DeployConfig { image: app["Config"]["Image"].as_str().unwrap_or_default().to_string(), ..DeployConfig::default() };

    // PostgreSQL, from SENTIENT's connection string.
    let db = env_get(&app_env, "DATABASE_URL").and_then(parse_db_url);
    let pg = db.as_ref().and_then(|d| service_container(all, project, &d.host));
    match (&db, pg) {
        (Some(d), Some(_)) => {
            cfg.db_user = d.user.clone();
            cfg.db_password = d.password.clone();
            cfg.db_name = d.database.clone();
        }
        (Some(d), None) => {
            problem = Some(format!("SENTIENT's database ({}) is not a container of this stack; the Platform Manager manages the stack's own PostgreSQL only.", d.host))
        }
        (None, _) => problem = Some("SENTIENT's DATABASE_URL could not be read.".into()),
    }

    // Container and volume names: the Platform Manager's own when they
    // match, else the names this stack uses.
    cfg.stack_name = if app_name == "sentient" { "sentient".into() } else { stack_name_for(&app_name) };
    // Database type.
    let ch_url = env_get(&app_env, "CLICKHOUSE_URL").filter(|u| !u.is_empty());
    let pg_image = pg.and_then(|p| p["Config"]["Image"].as_str()).unwrap_or_default().to_string();
    let timescale_image = pg_image.contains("timescale");
    cfg.flavor = if ch_url.is_some() {
        Flavor::Clickhouse
    } else if timescale_image && env_get(&app_env, "SENTIENT_TIMESCALE_ENABLED") != Some("false") {
        Flavor::Timescaledb
    } else {
        Flavor::Stock
    };
    cfg.keep_timescale_image = timescale_image && cfg.flavor != Flavor::Timescaledb;

    // ClickHouse.
    let mut ch = None;
    if let Some(url) = ch_url {
        cfg.ch_db = env_get(&app_env, "CLICKHOUSE_DATABASE").unwrap_or("sentient").to_string();
        cfg.ch_user = env_get(&app_env, "CLICKHOUSE_USER").unwrap_or("default").to_string();
        cfg.ch_password = env_get(&app_env, "CLICKHOUSE_PASSWORD").unwrap_or_default().to_string();
        ch = url_host_port(url).and_then(|(host, _)| service_container(all, project, &host));
        match ch {
            Some(c) => {
                // 0, never the default: it could be another ClickHouse.
                cfg.ch_http_port = host_port(c, "8123/tcp").unwrap_or(0);
                cfg.ch_native_port = host_port(c, "9000/tcp").unwrap_or(0);
                if cfg.ch_http_port == 0 {
                    notes.push("ClickHouse is not published on this machine, so backups and changing the database type will not work until it is.".into());
                }
            }
            None if problem.is_none() => {
                problem = Some(format!("SENTIENT's ClickHouse ({url}) is not a container of this stack; the Platform Manager manages the stack's own ClickHouse only."))
            }
            None => {}
        }
    }

    let default_names = cfg.names();
    let actual = super::Names {
        app: app_name.clone(),
        pg: pg.map(name).unwrap_or_else(|| default_names.pg.clone()),
        ch: ch.map(name).unwrap_or_else(|| default_names.ch.clone()),
        vol_pg: pg.and_then(|p| volume_at(p, "/var/lib/postgresql")).unwrap_or_else(|| default_names.vol_pg.clone()),
        vol_data: volume_at(app, "/var/lib/sentient").unwrap_or_else(|| default_names.vol_data.clone()),
        vol_ch: ch.and_then(|c| volume_at(c, "/var/lib/clickhouse")).unwrap_or_else(|| default_names.vol_ch.clone()),
    };
    if actual != default_names {
        cfg.names = Some(actual);
    }

    // Published ports.
    match host_port(app, "8080/tcp") {
        Some(p) => cfg.http_port = p,
        None if problem.is_none() => problem = Some("SENTIENT's web port (8080) is not published on this machine.".into()),
        None => {}
    }
    cfg.mqtt_port = host_port(app, "1883/tcp").unwrap_or(cfg.mqtt_port);
    cfg.mqtts_port = host_port(app, "8883/tcp").unwrap_or(cfg.mqtts_port);
    cfg.coap_port = host_port(app, "5683/udp").unwrap_or(cfg.coap_port);
    cfg.coaps_port = host_port(app, "5684/udp").unwrap_or(cfg.coaps_port);
    match pg.and_then(|p| host_port(p, "5432/tcp")) {
        Some(p) => cfg.pg_port = p,
        // 0, never the default: 5432 here may well be another PostgreSQL.
        None if pg.is_some() => {
            cfg.pg_port = 0;
            notes.push("PostgreSQL is not published on this machine, so backups, the kept-data list and changing the database type will not work until it is.".into())
        }
        None => {}
    }

    // Settings: SENTIENT's environment without what the image sets itself.
    let image_defaults = app["Image"].as_str().map(image_env).unwrap_or_default();
    let own: Vec<(&str, &str)> = app_env
        .iter()
        .filter(|(k, v)| !image_defaults.iter().any(|(ik, iv)| ik == k && iv == v))
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    cfg.tuning = Tuning::from_env(own, ENGINE_ENV);
    if let Some(url) = env_get(&app_env, "LICENSE_SERVER_URL") {
        cfg.license_server_url = url.to_string();
    }
    if let Some(p) = pg {
        cfg.pg = pg_tuning(p);
        // Its cluster's place in the volume (the image's own default unless
        // the compose file set PGDATA): kept, so a takeover starts the same
        // cluster.
        let pg_env = env(p);
        if let Some(d) = env_get(&pg_env, "PGDATA").filter(|d| *d != super::PGDATA) {
            cfg.pgdata = Some(d.to_string());
        }
    }
    let log = &app["HostConfig"]["LogConfig"]["Config"];
    if let Some(v) = log["max-size"].as_str() {
        cfg.log_max_size = v.to_string();
    }
    if let Some(v) = log["max-file"].as_str().and_then(|v| v.parse().ok()) {
        cfg.log_max_file = v;
    }

    // The compose project it was started from. Its own stack (an
    // installation record that was lost) needs no special handling.
    let files: Vec<String> = label(app, "com.docker.compose.project.config_files")
        .map(|f| f.split(',').map(str::trim).filter(|f| !f.is_empty()).map(String::from).collect())
        .unwrap_or_default();
    let own_stack = project == Some(cfg.stack_name.as_str()) && files == [cfg.compose_path()];
    if !own_stack {
        match project {
            Some(p) => {
                let missing: Vec<&String> = files.iter().filter(|f| !super::host_file_exists(f)).collect();
                if !missing.is_empty() {
                    notes.push(format!(
                        "Its compose file is no longer at {}: starting or updating it needs the Platform Manager to take the compose file over.",
                        missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                    ));
                }
                cfg.external = Some(ExternalCompose {
                    project: p.to_string(),
                    files,
                    working_dir: label(app, "com.docker.compose.project.working_dir").unwrap_or_default().to_string(),
                });
            }
            None if problem.is_none() => {
                problem = Some("SENTIENT was started without Docker Compose (docker run); the Platform Manager manages compose stacks.".into())
            }
            None => {}
        }
    }

    // Can the Platform Manager's own compose file run the same containers on
    // the same data? It mounts named volumes at these places.
    let mut differ = Vec::new();
    if volume_at(app, "/var/lib/sentient").is_none() {
        differ.push("SENTIENT's files are not in a named volume at /var/lib/sentient".to_string());
    }
    if pg.is_some_and(|p| volume_at(p, "/var/lib/postgresql").is_none()) {
        differ.push("PostgreSQL's data is not in a named volume at /var/lib/postgresql".to_string());
    }
    if ch.is_some_and(|c| volume_at(c, "/var/lib/clickhouse").is_none()) {
        differ.push("ClickHouse's data is not in a named volume at /var/lib/clickhouse".to_string());
    }
    let take_over = if cfg.external.is_none() {
        Err("It already runs from the Platform Manager's compose file.".into())
    } else if differ.is_empty() {
        Ok(())
    } else {
        Err(format!("its volumes are laid out differently from the Platform Manager's stack ({})", differ.join("; ")))
    };

    FoundStack {
        version: super::docker_ops::image_version(app["Image"].as_str().unwrap_or(&cfg.image)).or_else(|| {
            let tag = cfg.image.rsplit_once(':').map(|(_, t)| t).filter(|t| !t.contains('/'))?;
            (tag != "latest").then(|| tag.to_string())
        }),
        running: app["State"]["Running"].as_bool().unwrap_or(false),
        container_id: app["Id"].as_str().unwrap_or_default().to_string(),
        cfg,
        notes,
        problem,
        take_over,
    }
}

/// Check the Platform Manager can do its work on a found stack: SQL in its
/// PostgreSQL, its ClickHouse answering, SENTIENT answering on its port.
pub fn verify(found: &FoundStack) -> Result<(), String> {
    if let Some(p) = &found.problem {
        return Err(p.clone());
    }
    let cfg = &found.cfg;
    let n = cfg.names();
    super::docker_ops::psql(cfg, &cfg.db_name, "SELECT 1")
        .map_err(|e| format!("Could not run SQL in {} as {}: {e}", n.pg, cfg.db_user))?;
    if cfg.flavor.uses_clickhouse() {
        let ok = run_output(&format!(
            "docker exec {} clickhouse-client --user {} --password {} --query 'SELECT 1'",
            shell_quote(&n.ch),
            shell_quote(&cfg.ch_user),
            shell_quote(&cfg.ch_password)
        ))
        .is_some_and(|(ok, out)| ok && out.trim() == "1");
        if !ok {
            return Err(format!("Could not query ClickHouse in {} as {}.", n.ch, cfg.ch_user));
        }
    }
    if found.running && !is_running(cfg.http_port) {
        return Err(format!("SENTIENT is running but does not answer on port {}.", cfg.http_port));
    }
    Ok(())
}

/// What taking a stack over will do, shown for confirmation before anything
/// changes (and checked again when it runs).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TakeOverPlan {
    /// The compose project the containers were started from.
    pub project: String,
    /// Its compose files: (path, found on this machine).
    pub files: Vec<(String, bool)>,
    /// The `.env` beside them and every `env_file` they name: (path, found).
    pub env_files: Vec<(String, bool)>,
    /// The containers that are stopped and kept until the new ones answer.
    pub containers: Vec<String>,
    pub image: String,
    /// The data volumes the new containers use, unchanged.
    pub volumes: Vec<String>,
    /// "8080 web", "1883 MQTT", ...
    pub ports: Vec<String>,
    /// SENTIENT settings carried over beyond the built-in ones (names only).
    pub extra_env: Vec<String>,
    /// PostgreSQL (and ClickHouse) were not published: the port they get on
    /// 127.0.0.1 only.
    pub loopback_port: Option<u16>,
    /// Why it cannot be taken over (nothing is changed then).
    pub problems: Vec<String>,
    /// Where the Platform Manager's compose file will be.
    pub new_file: String,
    /// Where the database password was found ("" when not needed or not
    /// found).
    #[serde(default)]
    pub password_from: String,
    /// The password was found nowhere: the operator gives it (and it is
    /// checked against PostgreSQL) before the takeover.
    #[serde(default)]
    pub needs_password: bool,
    /// The database user the password is for.
    #[serde(default)]
    pub db_user: String,
    /// Where ClickHouse's password was found ("" without ClickHouse).
    #[serde(default)]
    pub ch_password_from: String,
    /// ClickHouse's password was found nowhere: the operator gives it.
    #[serde(default)]
    pub needs_ch_password: bool,
    #[serde(default)]
    pub ch_user: String,
    /// Why a password is asked for (not found, or refused), for the field.
    #[serde(default)]
    pub password_why: String,
    #[serde(default)]
    pub ch_password_why: String,
}

/// `env_file` entries in a compose file: `env_file: x` or a `- x` list.
pub fn env_files_of(compose: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_list: Option<usize> = None;
    for line in compose.lines() {
        let indent = line.len() - line.trim_start().len();
        let t = line.trim();
        if let Some(at) = in_list {
            if let Some(item) = t.strip_prefix("- ") {
                if indent >= at {
                    // `- path: x` (long form) or `- x`.
                    let v = item.strip_prefix("path:").unwrap_or(item);
                    out.push(v.trim().trim_matches(['"', '\'']).to_string());
                    continue;
                }
            }
            if !t.is_empty() {
                in_list = None;
            }
        }
        if let Some(rest) = t.strip_prefix("env_file:") {
            let rest = rest.trim();
            if rest.is_empty() {
                in_list = Some(indent);
            } else if let Some(list) = rest.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
                out.extend(list.split(',').map(|v| v.trim().trim_matches(['"', '\'']).to_string()).filter(|v| !v.is_empty()));
            } else {
                out.push(rest.trim_matches(['"', '\'']).to_string());
            }
        }
    }
    out.dedup();
    out
}

fn resolve(dir: &str, path: &str) -> String {
    if path.starts_with('/') || dir.is_empty() { path.to_string() } else { format!("{}/{}", dir.trim_end_matches('/'), path.trim_start_matches("./")) }
}

fn read_host_file(path: &str) -> Option<String> {
    run_output(&format!("cat {} 2>/dev/null", shell_quote(path))).filter(|(ok, _)| *ok).map(|(_, t)| t)
}

/// A free TCP port on 127.0.0.1 from `from` up.
fn free_loopback_port(from: u16) -> u16 {
    (from..from.saturating_add(200)).find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok()).unwrap_or(from)
}

/// The plan for taking `cfg` over; `compose` overrides the compose file the
/// labels name (one picked on the server when that one is gone).
/// The compose files a container was started from, as its labels name
/// them now (empty when it has none or is not there).
fn live_compose_files(container: &str) -> Vec<String> {
    let cmd = format!("docker inspect -f '{{{{index .Config.Labels \"com.docker.compose.project.config_files\"}}}}' {}", shell_quote(container));
    match run_output(&cmd) {
        Some((true, out)) => out.trim().split(',').map(str::trim).filter(|f| !f.is_empty() && *f != "<no value>").map(String::from).collect(),
        _ => Vec::new(),
    }
}

pub fn take_over_plan(cfg: &DeployConfig, compose: Option<&str>) -> (TakeOverPlan, DeployConfig) {
    let mut plan = TakeOverPlan::default();
    let mut own = cfg.clone();
    let Some(ext) = cfg.external.clone() else {
        plan.problems.push("It already runs from the Platform Manager's compose file.".into());
        return (plan, own);
    };
    own.external = None;
    plan.project = ext.project.clone();
    // The files the containers name NOW: the record keeps the list from
    // when the installation was added, which goes stale when the stack is
    // brought up again with other files (a temporary override, since
    // deleted, kept showing as "Missing").
    let live = live_compose_files(&cfg.names().app);
    let files: Vec<String> = match compose {
        Some(c) if !c.trim().is_empty() => vec![c.trim().to_string()],
        _ if !live.is_empty() => live,
        _ => ext.files.clone(),
    };
    plan.files = files.iter().map(|f| (f.clone(), super::host_file_exists(f))).collect();
    // The .env beside the (first) compose file, and every env_file named.
    let dir = files.first().and_then(|f| f.rsplit_once('/').map(|(d, _)| d.to_string())).filter(|d| !d.is_empty()).unwrap_or(ext.working_dir.clone());
    let mut envs: Vec<String> = Vec::new();
    let dot_env = resolve(&dir, ".env");
    if super::host_file_exists(&dot_env) {
        envs.push(dot_env);
    }
    for f in files.iter().filter(|f| super::host_file_exists(f)) {
        if let Some(text) = read_host_file(f) {
            envs.extend(env_files_of(&text).into_iter().map(|e| resolve(&dir, &e)));
        }
    }
    envs.dedup();
    plan.env_files = envs.iter().map(|e| (e.clone(), super::host_file_exists(e))).collect();

    let n = cfg.names();
    plan.containers = std::iter::once(n.app.clone()).chain(std::iter::once(n.pg.clone())).chain(cfg.flavor.uses_clickhouse().then(|| n.ch.clone())).collect();
    plan.image = cfg.image.clone();
    plan.volumes = std::iter::once(n.vol_data.clone()).chain(std::iter::once(n.vol_pg.clone())).chain(cfg.flavor.uses_clickhouse().then(|| n.vol_ch.clone())).collect();
    plan.ports = vec![
        format!("{} web", cfg.http_port),
        format!("{} MQTT", cfg.mqtt_port),
        format!("{} MQTTS", cfg.mqtts_port),
        format!("{} CoAP", cfg.coap_port),
        format!("{} CoAPS", cfg.coaps_port),
    ];
    plan.extra_env = cfg.tuning.extra_env.iter().map(|e| e.name.clone()).collect();
    // A database that was not published gets a port on 127.0.0.1 only.
    if cfg.pg_port == 0 || (cfg.flavor.uses_clickhouse() && cfg.ch_http_port == 0) {
        own.db_loopback = true;
        if cfg.pg_port == 0 {
            own.pg_port = free_loopback_port(15432);
            plan.loopback_port = Some(own.pg_port);
        }
        if cfg.flavor.uses_clickhouse() && cfg.ch_http_port == 0 {
            own.ch_http_port = free_loopback_port(18123);
            own.ch_native_port = free_loopback_port(19000);
        }
    } else {
        plan.ports.push(format!("{} PostgreSQL", cfg.pg_port));
    }
    // ClickHouse's native port not published (only its HTTP port is): one
    // that is free here, where the HTTP port is published.
    if cfg.flavor.uses_clickhouse() && own.ch_native_port == 0 {
        own.ch_native_port = free_loopback_port(19000);
        plan.ports.push(format!("{} ClickHouse native (was not published)", own.ch_native_port));
    }
    plan.db_user = cfg.db_user.clone();
    // The password: given (cfg), else from the containers and compose
    // files, else not needed (trust), else asked for.
    if own.db_password.is_empty() {
        match super::docker_ops::db_password_of(cfg) {
            super::docker_ops::DbPassword::Found(p, from) => {
                own.db_password = p;
                plan.password_from = from.to_string();
            }
            super::docker_ops::DbPassword::Trust => {
                own.db_password = generated_password();
                plan.password_from = "PostgreSQL's trust setting: none needed".into();
            }
            super::docker_ops::DbPassword::Unknown => {
                plan.needs_password = true;
                plan.password_why = "It is not in the containers or the compose file.".into();
                // Checked once given; validated with a stand-in meanwhile.
                own.db_password = "given_later".into();
            }
            super::docker_ops::DbPassword::Refused(why) => {
                plan.needs_password = true;
                plan.password_why = why;
                own.db_password = "given_later".into();
            }
        }
    } else {
        plan.password_from = "given".into();
    }
    // ClickHouse's password: given, else found and checked with SELECT 1
    // (an empty one too: the default user has none). The new container
    // takes the password from the compose file when it starts, so an empty
    // one becomes a generated one there.
    if cfg.flavor.uses_clickhouse() {
        plan.ch_user = cfg.ch_user.clone();
        if own.ch_password.is_empty() {
            match super::docker_ops::ch_password_of(cfg) {
                Some((p, from)) if p.is_empty() => {
                    own.ch_password = generated_password();
                    plan.ch_password_from = format!("{from}; the new compose file gives it one");
                }
                Some((p, from)) => {
                    own.ch_password = p;
                    plan.ch_password_from = from.to_string();
                }
                None => {
                    plan.needs_ch_password = true;
                    plan.ch_password_why = "None of the passwords in the containers or the compose file (nor an empty one) is accepted by ClickHouse.".into();
                    own.ch_password = "given_later".into();
                }
            }
        } else {
            plan.ch_password_from = "given".into();
        }
    }
    if let Err(e) = own.validate() {
        plan.problems.push(e);
    }
    for c in &plan.containers {
        if !container_exists(c) {
            plan.problems.push(format!("The container {c} is not there."));
        }
    }
    plan.new_file = own.compose_path();
    (plan, own)
}

/// A password for a PostgreSQL that does not ask for one (trust): letters
/// and digits, as the Platform Manager's own compose file needs one.
fn generated_password() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    format!("st{:016x}", h.finish())
}

/// Whether `password` lets SENTIENT's user into the stack's PostgreSQL
/// (over TCP inside its container, so the password is really checked).
pub fn check_db_password(cfg: &DeployConfig, password: &str) -> Result<(), String> {
    crate::stack::validate_password("The database password", password)?;
    // Over the container's own network address, as SENTIENT connects: the
    // image trusts 127.0.0.1, so a loopback connection would take any password.
    let inner = format!(
        "psql -h \"$(hostname -i | cut -d' ' -f1)\" -U {} -d {} -tAc 'select 1'",
        shell_quote(&cfg.db_user),
        shell_quote(&cfg.db_name)
    );
    let out = run_output(&format!(
        "docker exec -e PGPASSWORD={} {} sh -c {} 2>&1",
        shell_quote(password),
        shell_quote(&cfg.names().pg),
        shell_quote(&inner)
    ));
    match out {
        Some((true, o)) if o.trim() == "1" => Ok(()),
        Some((_, o)) if o.contains("password authentication failed") => Err(format!("PostgreSQL refused this password for the user {}.", cfg.db_user)),
        Some((_, o)) => Err(format!("PostgreSQL could not be asked: {}", o.trim())),
        None => Err("PostgreSQL could not be asked.".into()),
    }
}

fn container_exists(name: &str) -> bool {
    run_output(&format!("docker inspect -f '{{{{.Id}}}}' {} 2>/dev/null", shell_quote(name))).is_some_and(|(ok, _)| ok)
}

/// The name an old container is kept under while the new one starts.
fn kept_name(name: &str) -> String {
    format!("{name}-before-takeover")
}

/// Let the Platform Manager's own compose file run an adopted stack, on the
/// same volumes. Nothing is removed until the new containers answer:
///
/// 1. the Platform Manager's compose file is written (nothing stopped yet);
/// 2. the old containers are stopped and renamed (`<name>-before-takeover`);
/// 3. the new ones start from the new file and must answer within 3 minutes;
/// 4. then the old ones are removed. On any failure the new ones are removed,
///    the old ones get their names back and start again, exactly as they
///    were, without needing the original compose file.
///
/// `compose`: a compose file picked on the server in place of the one the
/// labels name. Returns the configuration without `external`. `cfg` must
/// carry the passwords.
pub fn take_over(sink: ProgressFn, cfg: &DeployConfig, compose: Option<&str>) -> Result<DeployConfig, String> {
    if cfg.external.is_none() {
        return Ok(cfg.clone());
    }
    let (plan, own) = take_over_plan(cfg, compose);
    // Volumes bound to a folder (moved to another disk) stay bound there:
    // the new file says so, and so does the configuration returned.
    let own = super::move_data::with_actual_locations(&own);
    if plan.needs_password {
        return Err("The database password was not found; give it in the takeover dialog. Nothing was changed.".into());
    }
    if plan.needs_ch_password {
        return Err("ClickHouse's password was not found; give it in the takeover dialog. Nothing was changed.".into());
    }
    if !plan.problems.is_empty() {
        return Err(format!("{} Nothing was changed.", plan.problems.join(" ")));
    }
    if let Some(p) = plan.loopback_port {
        sink(Progress::Log { line: format!("PostgreSQL was not published: it gets port {p} on 127.0.0.1 only.") });
    }
    // 1. The new file first: if it cannot be written, nothing has stopped.
    sink(Progress::Step { name: format!("Writing the Platform Manager's compose file ({})", own.compose_path()) });
    // The old file at the same place is kept, never overwritten.
    if plan.files.iter().any(|(f, found)| *found && *f == own.compose_path()) {
        let _ = run_output(&format!("cp -p {} {}", shell_quote(&own.compose_path()), shell_quote(&format!("{}.before-takeover", own.compose_path()))));
    }
    write_stack_files(&sink, &own).map_err(|e| format!("{e}. Nothing was changed."))?;
    for (f, found) in &plan.env_files {
        if *found {
            // Kept beside the new file for reference; their values are in
            // effect already (they came with the containers' environment).
            let dest = format!("{}/{}", own.stack_dir(), f.rsplit('/').next().unwrap_or(".env"));
            let _ = run_output(&format!("cp -p {} {}", shell_quote(f), shell_quote(&format!("{dest}.from-{}", plan.project))));
        }
    }
    // 2. Stop the old containers and keep them under another name.
    sink(Progress::Step { name: "Stopping SENTIENT and keeping its containers until the new ones answer".into() });
    let mut kept: Vec<String> = Vec::new();
    let restore = |sink: &ProgressFn, kept: &[String]| {
        let _ = run_stream(sink, &own.compose_cmd("down"));
        // The database first, SENTIENT last.
        for c in kept.iter().rev() {
            let _ = run_output(&format!("docker rename {} {}", shell_quote(&kept_name(c)), shell_quote(c)));
        }
        for c in kept.iter().rev() {
            let _ = run_output(&format!("docker start {}", shell_quote(c)));
        }
    };
    for c in &plan.containers {
        let ok = run_output(&format!("docker stop {} && docker rename {} {}", shell_quote(c), shell_quote(c), shell_quote(&kept_name(c)))).is_some_and(|(ok, _)| ok);
        if !ok {
            restore(&sink, &kept);
            let _ = run_output(&format!("docker start {}", shell_quote(c)));
            return Err(format!("The container {c} could not be stopped and kept. SENTIENT runs as before; nothing was changed."));
        }
        kept.push(c.clone());
    }
    // 3. The new containers, on the same volumes.
    sink(Progress::Step { name: "Starting SENTIENT from the Platform Manager's compose file".into() });
    let up = run_stream(&sink, &own.compose_cmd("up -d")).and_then(|_| {
        if std::env::var_os("SPM_TEST_TAKEOVER_FAIL").is_some() {
            return Err("test: the new containers are made to fail".to_string());
        }
        for _ in 0..90 {
            if is_running(own.http_port) {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        Err("SENTIENT did not answer within 3 minutes".to_string())
    });
    if let Err(e) = up {
        sink(Progress::Step { name: "Starting the previous containers again".into() });
        restore(&sink, &kept);
        for _ in 0..60 {
            if is_running(cfg.http_port) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        return Err(format!("{e}. The previous containers run again as they were; nothing was taken over."));
    }
    // 4. Only now the old containers go (their volumes are the new ones').
    for c in &kept {
        let _ = run_output(&format!("docker rm {}", shell_quote(&kept_name(c))));
    }
    sink(Progress::Done {
        message: format!(
            "SENTIENT now runs from {}. Start and stop it with the Platform Manager (or docker compose -f that file) from now on: the old compose file would bring back the old settings.",
            own.compose_path()
        ),
    });
    Ok(own)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Takeover against throwaway compose projects on this machine's Docker
    /// (`spmtest/sentient:fake` answering on 8080, postgres:18-alpine), each
    /// on its own names, ports and volumes. XDG_DATA_HOME points the new
    /// compose files at a scratch folder. Run one at a time:
    /// `cargo test -p sentient-installer-core takeover_e2e -- --ignored --test-threads=1`.
    mod takeover_e2e {
        use super::super::*;
        use std::process::Command;

        fn sh(cmd: &str) -> (bool, String) {
            let o = Command::new("sh").args(["-c", cmd]).output().unwrap();
            (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
        }

        struct Case {
            n: u16,
            dir: std::path::PathBuf,
        }

        impl Case {
            fn p(&self) -> String {
                format!("spmft{}", self.n)
            }
            fn app(&self) -> String {
                format!("spmft{}-sentient", self.n)
            }
            fn pg(&self) -> String {
                format!("spmft{}-postgres", self.n)
            }
            fn http(&self) -> u16 {
                18080 + self.n
            }

            /// A compose project like a customer's: own names, an env_file and
            /// a .env, PostgreSQL published or not.
            fn up(n: u16, pg_published: bool) -> Case {
                Case::up_with(n, pg_published, 0)
            }

            /// `pw`: 0 the password in both containers; 1 only in SENTIENT's
            /// DATABASE_URL (PostgreSQL reads it from POSTGRES_PASSWORD_FILE);
            /// 2 only in POSTGRES_PASSWORD_FILE (the URL has none).
            fn up_with(n: u16, pg_published: bool, pw: u8) -> Case {
                let dir = std::env::temp_dir().join(format!("spm-takeover-{n}-{}", std::process::id()));
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).unwrap();
                let c = Case { n, dir };
                c.down();
                std::fs::write(c.dir.join(".env"), format!("HTTP_PORT={}\n", c.http())).unwrap();
                std::fs::write(c.dir.join("sentient.env"), "MY_EXTRA_SETTING=kept\n").unwrap();
                let pg_ports = if pg_published { format!("    ports:\n      - \"{}:5432\"\n", 25480 + n) } else { String::new() };
                let y = format!(
                    "services:\n  sentient:\n    image: spmtest/sentient:fake\n    container_name: {app}\n    env_file: ./sentient.env\n    environment:\n      DATABASE_URL: postgresql://sentient:pw{n}@postgres:5432/sentient\n    ports:\n      - \"${{HTTP_PORT}}:8080\"\n      - \"{m}:1883\"\n      - \"{ms}:8883\"\n      - \"{c}:5683/udp\"\n      - \"{cs}:5684/udp\"\n    volumes:\n      - data:/var/lib/sentient\n  postgres:\n    image: postgres:18-alpine\n    container_name: {pg}\n    environment:\n      POSTGRES_DB: sentient\n      POSTGRES_USER: sentient\n      POSTGRES_PASSWORD: pw{n}\n{pg_ports}    volumes:\n      - pg:/var/lib/postgresql\nvolumes:\n  data:\n    name: spmft{n}-data\n  pg:\n    name: spmft{n}-pg\n",
                    app = c.app(),
                    pg = c.pg(),
                    m = 21880 + n,
                    ms = 22880 + n,
                    c = 23680 + n,
                    cs = 24680 + n,
                );
                let y = match pw {
                    0 => y,
                    _ => {
                        std::fs::write(c.dir.join("pgpass"), format!("pw{n}\n")).unwrap();
                        let y = y.replace(&format!("      POSTGRES_PASSWORD: pw{n}\n"), "      POSTGRES_PASSWORD_FILE: /run/pgpass\n").replace("      - pg:/var/lib/postgresql\n", "      - pg:/var/lib/postgresql\n      - ./pgpass:/run/pgpass:ro\n");
                        if pw == 2 { y.replace(&format!("postgresql://sentient:pw{n}@postgres"), "postgresql://sentient@postgres") } else { y }
                    }
                };
                std::fs::write(c.dir.join("compose.yml"), y).unwrap();
                let (ok, out) = sh(&format!("cd {} && docker compose -p {} -f compose.yml up -d", c.dir.display(), c.p()));
                assert!(ok, "{out}");
                // A row in the database: it must survive the takeover.
                for _ in 0..60 {
                    if sh(&format!("docker exec {} psql -U sentient -d sentient -c \"CREATE TABLE IF NOT EXISTS marker(v text); INSERT INTO marker VALUES ('before')\"", c.pg())).0 {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
                c
            }

            fn found(&self) -> DeployConfig {
                find().into_iter().find(|f| f.cfg.names().app == self.app()).expect("the stack is found").cfg
            }

            fn marker(&self) -> String {
                sh(&format!("docker exec {} psql -U sentient -d sentient -tAc 'SELECT v FROM marker'", self.pg())).1.trim().to_string()
            }

            fn running(&self, name: &str) -> bool {
                sh(&format!("docker inspect -f '{{{{.State.Running}}}}' {name}")).1.trim() == "true"
            }

            fn project_of(&self, name: &str) -> String {
                sh(&format!("docker inspect -f '{{{{index .Config.Labels \"com.docker.compose.project.config_files\"}}}}' {name}")).1.trim().to_string()
            }

            fn down(&self) {
                for c in [self.app(), self.pg(), format!("{}-before-takeover", self.app()), format!("{}-before-takeover", self.pg())] {
                    let _ = sh(&format!("docker rm -f {c}"));
                }
                let _ = sh(&format!("docker network rm {}_default spmft{}_sentient_default {}_sentient 2>/dev/null; docker volume rm spmft{n}-data spmft{n}-pg", self.p(), self.n, self.p(), n = self.n));
            }
        }

        fn sink() -> ProgressFn {
            std::sync::Arc::new(|p| eprintln!("  {p:?}"))
        }

        fn scratch() {
            let d = std::env::temp_dir().join(format!("spm-takeover-xdg-{}", std::process::id()));
            std::fs::create_dir_all(&d).unwrap();
            // SAFETY: tests run one at a time (--test-threads=1).
            unsafe { std::env::set_var("XDG_DATA_HOME", &d) };
        }

        fn taken_over(c: &Case, own: &DeployConfig) {
            assert!(own.external.is_none());
            assert!(c.running(&c.app()) && c.running(&c.pg()));
            assert_eq!(c.project_of(&c.app()), own.compose_path(), "runs from the Platform Manager's file");
            assert_eq!(c.marker(), "before", "the data stayed");
            assert!(sh(&format!("docker inspect {}-before-takeover", c.app())).0 == false, "the old container is gone");
            let env = sh(&format!("docker inspect -f '{{{{range .Config.Env}}}}{{{{println .}}}}{{{{end}}}}' {}", c.app())).1;
            assert!(env.contains("MY_EXTRA_SETTING=kept"), "the env_file's value is carried over: {env}");
            assert!(is_running(c.http()));
        }

        /// A dev box case: the record still names a temporary override the
        /// stack was once brought up with (since deleted); the containers'
        /// labels name only the real file now. The plan lists what the
        /// containers name, and the new compose file is the Platform
        /// Manager's own, outside the old project's folder.
        #[test]
        #[ignore]
        fn a_stale_override_in_the_record_is_not_used() {
            scratch();
            let c = Case::up(9, true);
            let mut cfg = c.found();
            let gone = c.dir.join("compose.override-deleted.yml").display().to_string();
            if let Some(ext) = cfg.external.as_mut() {
                ext.files.push(gone.clone());
            }
            let (plan, _) = take_over_plan(&cfg, None);
            assert!(plan.files.iter().all(|(f, found)| *found && *f != gone), "only the live file: {:?}", plan.files);
            assert_eq!(plan.files.len(), 1, "{:?}", plan.files);
            let own = take_over(sink(), &cfg, None).unwrap();
            taken_over(&c, &own);
            let xdg = std::env::var("XDG_DATA_HOME").unwrap();
            assert!(own.compose_path().starts_with(&xdg), "the Platform Manager's own place: {}", own.compose_path());
            assert!(!own.compose_path().starts_with(&c.dir.display().to_string()), "not the old project's folder");
            c.down();
        }

        #[test]
        #[ignore]
        fn with_its_compose_file_env_and_env_file() {
            scratch();
            let c = Case::up(1, true);
            let cfg = c.found();
            let (plan, _) = take_over_plan(&cfg, None);
            assert!(plan.problems.is_empty(), "{:?}", plan.problems);
            assert!(plan.files.iter().all(|(_, f)| *f), "{:?}", plan.files);
            assert_eq!(plan.env_files.iter().filter(|(_, f)| *f).count(), 2, "{:?}", plan.env_files);
            assert!(plan.extra_env.iter().any(|e| e == "MY_EXTRA_SETTING"), "{:?}", plan.extra_env);
            let own = take_over(sink(), &cfg, None).unwrap();
            taken_over(&c, &own);
            let _ = sh(&format!("docker compose -f {} down -v", own.compose_path()));
            c.down();
        }

        /// A PostgreSQL + ClickHouse stack like docker-compose.clickhouse.yml.
        /// `mode`: 0 the ClickHouse password in both containers; 1 none at all
        /// (the default user); 2 only in SENTIENT's environment (ClickHouse
        /// reads it from a users.d file); 3 only in the ClickHouse container.
        fn up_ch(n: u16, mode: u8) -> Case {
            let c = Case::up_with(n, true, 0);
            let (ok, out) = sh(&format!("cd {} && docker compose -p {} -f compose.yml down", c.dir.display(), c.p()));
            assert!(ok, "{out}");
            let user = if mode == 1 { "default" } else { "sentient" };
            let pw = format!("ch{n}");
            let app_pw = if matches!(mode, 0 | 2) { format!("      CLICKHOUSE_PASSWORD: {pw}\n") } else { String::new() };
            let ch_env = match mode {
                0 | 3 => format!("      CLICKHOUSE_USER: {user}\n      CLICKHOUSE_PASSWORD: {pw}\n      CLICKHOUSE_DB: sentient\n"),
                _ => "      CLICKHOUSE_DB: sentient\n".to_string(),
            };
            let users_d = if mode == 2 {
                std::fs::write(c.dir.join("chuser.xml"), format!("<clickhouse><users><sentient><password>{pw}</password><networks><ip>::/0</ip></networks><profile>default</profile><quota>default</quota><access_management>1</access_management></sentient></users></clickhouse>")).unwrap();
                "      - ./chuser.xml:/etc/clickhouse-server/users.d/sentient.xml:ro\n".to_string()
            } else {
                String::new()
            };
            let y = std::fs::read_to_string(c.dir.join("compose.yml")).unwrap();
            let y = y.replace(
                "    environment:\n      DATABASE_URL:",
                &format!("    environment:\n      CLICKHOUSE_URL: http://clickhouse:8123\n      CLICKHOUSE_DATABASE: sentient\n      CLICKHOUSE_USER: {user}\n{app_pw}      DATABASE_URL:"),
            );
            let y = y.replace(
                "volumes:\n  data:\n",
                &format!(
                    "  clickhouse:\n    image: clickhouse/clickhouse-server:24.8-alpine\n    container_name: spmft{n}-clickhouse\n    environment:\n{ch_env}    ports:\n      - \"{p}:8123\"\n    volumes:\n      - ch:/var/lib/clickhouse\n{users_d}volumes:\n  ch:\n    name: spmft{n}-ch\n  data:\n",
                    p = 26480 + n
                ),
            );
            std::fs::write(c.dir.join("compose.yml"), y).unwrap();
            let (ok, out) = sh(&format!("cd {} && docker compose -p {} -f compose.yml up -d", c.dir.display(), c.p()));
            assert!(ok, "{out}");
            for _ in 0..60 {
                if sh(&format!("docker exec spmft{n}-clickhouse wget -qO- http://127.0.0.1:8123/ping")).0 {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
            c
        }

        fn takes_over_ch(n: u16, mode: u8, from: &str) {
            scratch();
            let c = up_ch(n, mode);
            let mut cfg = c.found();
            assert!(cfg.flavor.uses_clickhouse(), "found as PostgreSQL + ClickHouse");
            // As the record keeps it: no passwords.
            cfg.db_password.clear();
            cfg.ch_password.clear();
            let (plan, _) = take_over_plan(&cfg, None);
            assert!(plan.problems.is_empty() && !plan.needs_ch_password, "{:?}", plan);
            assert!(plan.ch_password_from.starts_with(from), "{}", plan.ch_password_from);
            let own = take_over(sink(), &cfg, None).unwrap();
            taken_over(&c, &own);
            assert!(c.running(&format!("spmft{n}-clickhouse")));
            // SENTIENT's new settings and ClickHouse's agree.
            assert!(crate::distro::docker_ops::check_ch_password(&own, &own.ch_password).is_ok());
            let _ = sh(&format!("docker compose -f {} down -v", own.compose_path()));
            let _ = sh(&format!("docker rm -f spmft{n}-clickhouse spmft{n}-clickhouse-before-takeover; docker volume rm spmft{n}-ch"));
            c.down();
        }

        #[test]
        #[ignore]
        fn clickhouse_password_in_both_containers() {
            takes_over_ch(8, 0, "SENTIENT's CLICKHOUSE_PASSWORD");
        }

        #[test]
        #[ignore]
        fn clickhouse_without_a_password() {
            takes_over_ch(9, 1, "none (the default user)");
        }

        #[test]
        #[ignore]
        fn clickhouse_password_only_in_sentients_environment() {
            takes_over_ch(10, 2, "SENTIENT's CLICKHOUSE_PASSWORD");
        }

        #[test]
        #[ignore]
        fn clickhouse_password_only_in_the_clickhouse_container() {
            takes_over_ch(11, 3, "the ClickHouse container's CLICKHOUSE_PASSWORD");
        }

        /// The password only in SENTIENT's DATABASE_URL; the record (as the
        /// app keeps it) has none: the plan finds it, the takeover runs.
        #[test]
        #[ignore]
        fn password_only_in_database_url() {
            scratch();
            let c = Case::up_with(6, true, 1);
            let mut cfg = c.found();
            cfg.db_password.clear();
            let (plan, _) = take_over_plan(&cfg, None);
            assert!(plan.problems.is_empty() && !plan.needs_password, "{:?}", plan);
            assert_eq!(plan.password_from, "SENTIENT's DATABASE_URL");
            let own = take_over(sink(), &cfg, None).unwrap();
            taken_over(&c, &own);
            let _ = sh(&format!("docker compose -f {} down -v", own.compose_path()));
            c.down();
        }

        /// The password only in PostgreSQL's POSTGRES_PASSWORD_FILE.
        #[test]
        #[ignore]
        fn password_only_in_a_password_file() {
            scratch();
            let c = Case::up_with(7, true, 2);
            let mut cfg = c.found();
            cfg.db_password.clear();
            let (plan, _) = take_over_plan(&cfg, None);
            assert!(plan.problems.is_empty() && !plan.needs_password, "{:?}", plan);
            assert_eq!(plan.password_from, "POSTGRES_PASSWORD_FILE");
            assert!(check_db_password(&cfg, &format!("pw{}", 7)).is_ok());
            assert!(check_db_password(&cfg, "wrong").unwrap_err().contains("refused"));
            let own = take_over(sink(), &cfg, None).unwrap();
            taken_over(&c, &own);
            let _ = sh(&format!("docker compose -f {} down -v", own.compose_path()));
            c.down();
        }

        #[test]
        #[ignore]
        fn file_gone_then_picked_on_the_server() {
            scratch();
            let c = Case::up(2, true);
            let moved = c.dir.join("elsewhere.yml");
            std::fs::rename(c.dir.join("compose.yml"), &moved).unwrap();
            let cfg = c.found();
            let (plan, _) = take_over_plan(&cfg, None);
            assert!(plan.files.iter().all(|(_, f)| !*f), "the labels' file is gone: {:?}", plan.files);
            let (plan, _) = take_over_plan(&cfg, Some(&moved.display().to_string()));
            assert!(plan.files.iter().all(|(_, f)| *f), "{:?}", plan.files);
            assert!(plan.env_files.iter().any(|(p, f)| p.ends_with("sentient.env") && *f), "{:?}", plan.env_files);
            let own = take_over(sink(), &cfg, Some(&moved.display().to_string())).unwrap();
            taken_over(&c, &own);
            let _ = sh(&format!("docker compose -f {} down -v", own.compose_path()));
            c.down();
        }

        #[test]
        #[ignore]
        fn no_compose_file_at_all() {
            scratch();
            let c = Case::up(3, true);
            std::fs::remove_dir_all(&c.dir).unwrap();
            let cfg = c.found();
            let (plan, _) = take_over_plan(&cfg, None);
            assert!(plan.problems.is_empty(), "rebuilt from the containers: {:?}", plan.problems);
            let own = take_over(sink(), &cfg, None).unwrap();
            taken_over(&c, &own);
            let _ = sh(&format!("docker compose -f {} down -v", own.compose_path()));
            c.down();
        }

        #[test]
        #[ignore]
        fn postgres_not_published() {
            scratch();
            let c = Case::up(4, false);
            let cfg = c.found();
            assert_eq!(cfg.pg_port, 0);
            let (plan, _) = take_over_plan(&cfg, None);
            assert!(plan.problems.is_empty(), "{:?}", plan.problems);
            let port = plan.loopback_port.expect("a loopback port");
            let own = take_over(sink(), &cfg, None).unwrap();
            taken_over(&c, &own);
            let bound = sh(&format!("docker port {} 5432", c.pg())).1;
            assert!(bound.trim() == format!("127.0.0.1:{port}"), "PostgreSQL on 127.0.0.1 only: {bound}");
            let _ = sh(&format!("docker compose -f {} down -v", own.compose_path()));
            c.down();
        }

        #[test]
        #[ignore]
        fn a_failure_after_the_switch_starts_the_old_containers_again() {
            scratch();
            let c = Case::up(5, true);
            let cfg = c.found();
            // SAFETY: tests run one at a time (--test-threads=1).
            unsafe { std::env::set_var("SPM_TEST_TAKEOVER_FAIL", "1") };
            let e = take_over(sink(), &cfg, None).unwrap_err();
            unsafe { std::env::remove_var("SPM_TEST_TAKEOVER_FAIL") };
            eprintln!("failure: {e}");
            assert!(e.contains("previous containers run again"), "{e}");
            assert!(c.running(&c.app()) && c.running(&c.pg()), "the old containers run again under their names");
            assert!(c.project_of(&c.app()).ends_with("compose.yml") && !c.project_of(&c.app()).contains("docker-compose"), "still the customer's containers");
            assert_eq!(c.marker(), "before");
            assert!(is_running(c.http()));
            c.down();
        }
    }

    #[test]
    fn env_files_are_read_from_every_form() {
        let y = "services:\n  sentient:\n    image: x\n    env_file: ./sentient.env\n  postgres:\n    env_file:\n      - db.env\n      - path: ./opt.env\n        required: false\n    environment:\n      A: b\n  ch:\n    env_file: [a.env, \"b.env\"]\n";
        assert_eq!(env_files_of(y), vec!["./sentient.env", "db.env", "./opt.env", "a.env", "b.env"]);
        assert_eq!(resolve("/srv/st", "./x.env"), "/srv/st/x.env");
        assert_eq!(resolve("/srv/st", "/etc/y.env"), "/etc/y.env");
    }

    #[test]
    fn connection_strings_are_read_with_escapes() {
        let u = parse_db_url("postgresql://sentient:p%40ss%3Aw@postgres:5432/sentient?sslmode=disable").unwrap();
        assert_eq!(u.user, "sentient");
        assert_eq!(u.password, "p@ss:w");
        assert_eq!(u.host, "postgres");
        assert_eq!(u.port, 5432);
        assert_eq!(u.database, "sentient");
        let u = parse_db_url("postgres://u:pw@127.0.0.1/db").unwrap();
        assert_eq!((u.host.as_str(), u.port), ("127.0.0.1", 5432));
        assert_eq!(url_password("postgresql://u@h/db"), None);
    }

    #[test]
    fn clickhouse_urls_give_host_and_port() {
        assert_eq!(url_host_port("http://clickhouse:8123"), Some(("clickhouse".into(), 8123)));
        assert_eq!(url_host_port("http://ch/"), Some(("ch".into(), 8123)));
    }

    #[test]
    fn only_sentient_images_count() {
        assert_eq!(image_repo_name("quakestring/sentient:latest"), "sentient");
        assert_eq!(image_repo_name("registry:5000/quakestring/sentient@sha256:ab"), "sentient");
        assert_eq!(image_repo_name("quakestring/sentient-license-server:1"), "sentient-license-server");
        assert_eq!(stack_name_for("st-tr-sentient"), "st_tr_sentient");
    }

    #[test]
    fn postgres_settings_come_from_the_command() {
        let pg: Value = serde_json::json!({"Config": {"Cmd": ["postgres", "-c", "max_connections=300", "-c", "shared_buffers=1GB", "-cwork_mem=8MB"]}});
        let t = pg_tuning(&pg);
        assert_eq!(t.pg_max_connections, 300);
        assert_eq!(t.pg_shared_buffers, "1GB");
        assert_eq!(t.pg_work_mem, "8MB");
        assert_eq!(t.pg_effective_cache_size, PgTuning::default().pg_effective_cache_size);
    }
}
