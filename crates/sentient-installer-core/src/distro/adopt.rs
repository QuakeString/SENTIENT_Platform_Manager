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
fn image_repo_name(image: &str) -> &str {
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

/// Let the Platform Manager's own compose file run an adopted stack: the
/// old project's containers are removed (its volumes stay), the same
/// containers come up from the Platform Manager's file on the same volumes,
/// and if SENTIENT does not answer the old project is started again.
/// Returns the configuration without `external`. `cfg` must carry the
/// passwords.
pub fn take_over(sink: ProgressFn, cfg: &DeployConfig) -> Result<DeployConfig, String> {
    let old = cfg.clone();
    if old.external.is_none() {
        return Ok(old);
    }
    if old.pg_port == 0 || (old.flavor.uses_clickhouse() && old.ch_http_port == 0) {
        return Err("Its PostgreSQL or ClickHouse is not published on this machine. Publish their ports in its compose file, then add the instance again before taking it over.".into());
    }
    let mut own = cfg.clone();
    own.external = None;
    own.validate()?;
    sink(Progress::Step { name: "Stopping SENTIENT and removing its containers (the data volumes stay)".into() });
    run_stream(&sink, &old.compose_cmd("down"))?;
    sink(Progress::Step { name: format!("Writing the Platform Manager's compose file ({})", own.compose_path()) });
    let started = write_stack_files(&sink, &own).and_then(|_| {
        sink(Progress::Step { name: "Starting SENTIENT from it".into() });
        run_stream(&sink, &own.compose_cmd("up -d"))
    });
    let up = started.and_then(|_| {
        for _ in 0..90 {
            if is_running(own.http_port) {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        Err("SENTIENT did not answer within 3 minutes".to_string())
    });
    if let Err(e) = up {
        sink(Progress::Step { name: "Starting SENTIENT from its own compose file again".into() });
        let _ = run_stream(&sink, &own.compose_cmd("down"));
        let _ = run_stream(&sink, &old.compose_cmd("up -d"));
        return Err(format!("{e}. SENTIENT was started from its own compose file again; nothing was taken over."));
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
