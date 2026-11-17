// Copyright © 2016-2026 The SENTIENT Authors

//! A native Windows installation whose installation record is missing (a
//! lost record, an older installer): read back from its services. SENTIENT's
//! WinSW descriptor (`services\SentientServer.xml`) carries its whole
//! environment; the PostgreSQL service's command line says where the
//! cluster lives.
//!
//! Every native layout the installers wrote reads back the same way: the
//! services `SentientServer` (WinSW, `<install>\services\SentientServer.xml`)
//! and `sentient-postgresql` (`pg_ctl runservice -N … -D …`). Releases before
//! 0.7.0 wrote `HTTP_PORT`/`MQTT_PORT`/`COAP_PORT` and no storage variables
//! (TimescaleDB always); 0.7.0 on write `SERVER__PORT` and the storage
//! variables; the hybrid (from 0.7.0, f60e0fd) adds `CLICKHOUSE_*`, with
//! ClickHouse in Docker in the WSL distro "sentient" of the hidden account
//! `sentient-clickhouse`, kept running by its scheduled task "SENTIENT
//! ClickHouse". That ClickHouse is reached over HTTP on 127.0.0.1 only: its
//! distro belongs to the hidden account, so `wsl` as anyone else cannot see
//! it, and that is never a reason not to add the installation.
//!
//! The PostgreSQL superuser's password is the one thing nothing on disk
//! holds. The caller supplies it (the secret store, the operator), or
//! `set_admin_password` gives the `postgres` role a new one through
//! SENTIENT's own role, which the installers create as a superuser.

#![cfg_attr(not(windows), allow(dead_code))]

use std::path::{Path, PathBuf};

use super::{APP_SERVICE, ArtifactSource, NativeConfig, PG_SERVICE};
use crate::stack::{self, Flavor};
use crate::sys;

/// A SENTIENT Windows installation read back from its services.
#[derive(Debug, Clone)]
pub struct FoundWindows {
    /// Its configuration with the passwords its descriptor holds (not the
    /// PostgreSQL superuser's).
    pub cfg: NativeConfig,
    pub running: bool,
    pub notes: Vec<String>,
    /// Why it cannot be managed, when it cannot.
    pub problem: Option<String>,
}

/// Variables the engine writes itself (`service_env`, `storage_env`).
const ENGINE_ENV: &[&str] = &[
    "DATABASE_URL", "UI_DIR", "REPORT_OUTPUT_DIR", "VC_REPOS_PATH", "JWT_SECRET", "SERVER__HOST", "SERVER__PORT",
    "LICENSE_SERVER_URL", "CLICKHOUSE_URL", "CLICKHOUSE_DATABASE", "CLICKHOUSE_USER", "CLICKHOUSE_PASSWORD",
    "TS_TYPE", "TS_LATEST_TYPE", "TS_LATEST_CACHE", "TS_LATEST_CACHE_TTL_SECS", "SENTIENT_TIMESCALE_ENABLED",
    // Written by releases before 0.7.0 (the server reads SERVER__PORT now).
    "HTTP_PORT", "MQTT_PORT", "COAP_PORT", "RUST_LOG",
];

/// A service's command line, from the service manager.
fn service_path(name: &str) -> Option<String> {
    let script = format!("(Get-CimInstance Win32_Service -Filter \"Name='{name}'\").PathName");
    let (ok, out, _) = sys::output("powershell.exe", &["-NoProfile", "-Command", &script])?;
    let path = sys::decode(&out).trim().to_string();
    (ok && !path.is_empty()).then_some(path)
}

/// The program of a command line (quoted or not).
fn program(cmdline: &str) -> String {
    let c = cmdline.trim();
    match c.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or_default().to_string(),
        None => c.split_whitespace().next().unwrap_or_default().to_string(),
    }
}

/// The value after `-D` on pg_ctl's command line (quoted or not).
fn data_dir_arg(cmdline: &str) -> Option<String> {
    let i = cmdline.find(" -D ")?;
    let rest = cmdline[i + 4..].trim_start();
    Some(match rest.strip_prefix('"') {
        Some(r) => r.split('"').next()?.to_string(),
        None => rest.split_whitespace().next()?.to_string(),
    })
}

fn xml_unescape(v: &str) -> String {
    v.replace("&quot;", "\"").replace("&lt;", "<").replace("&gt;", ">").replace("&apos;", "'").replace("&amp;", "&")
}

/// The `<env name="…" value="…"/>` elements of a WinSW descriptor.
pub(crate) fn descriptor_env(xml: &str) -> Vec<(String, String)> {
    let attr = |el: &str, name: &str| -> Option<String> {
        let start = el.find(&format!("{name}=\""))? + name.len() + 2;
        let end = el[start..].find('"')? + start;
        Some(xml_unescape(&el[start..end]))
    };
    xml.split("<env ")
        .skip(1)
        .filter_map(|el| {
            let el = el.split("/>").next()?;
            Some((attr(el, "name")?, attr(el, "value")?))
        })
        .collect()
}

/// Where `SentientServer` runs SENTIENT from (`<install>\services\
/// SentientServer.exe`). None when there is no such service.
pub fn installed_dir() -> Option<PathBuf> {
    let exe = PathBuf::from(program(&service_path(APP_SERVICE)?));
    Some(exe.parent()?.parent()?.to_path_buf())
}

pub fn installed_running() -> bool {
    super::service_state(APP_SERVICE) == "running"
}

/// Read the installation `SentientServer` runs. None when there is no such
/// service.
pub fn read_installed() -> Option<Result<FoundWindows, String>> {
    let install_dir = installed_dir()?;
    Some(read_from(&install_dir))
}

/// The settings an installer wrote into SentientServer's descriptor, for
/// every layout (see the module notes).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EnvSettings {
    pub http_port: u16,
    pub mqtt_port: u16,
    pub coap_port: u16,
    pub flavor: Flavor,
    pub ch_http_port: u16,
    pub ch_db: String,
    pub ch_user: String,
    pub ch_password: String,
}

/// The port of a URL (`http://127.0.0.1:8123/?x` → 8123); None without one.
pub(crate) fn url_port(url: &str) -> Option<u16> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let hostport = rest.split(['/', '?', '#']).next()?;
    // An IPv6 host in brackets: the port follows "]:".
    let after = hostport.rsplit_once(']').map(|(_, a)| a).unwrap_or(hostport);
    after.rsplit_once(':').and_then(|(_, p)| p.parse().ok())
}

pub(crate) fn env_settings(env: &[(String, String)]) -> EnvSettings {
    let get = |k: &str| env.iter().rev().find(|(n, _)| n == k).map(|(_, v)| v.trim()).filter(|v| !v.is_empty());
    let port = |keys: &[&str], default: u16| keys.iter().find_map(|k| get(k).and_then(|v| v.parse().ok())).unwrap_or(default);
    let ch_url = get("CLICKHOUSE_URL");
    let flavor = if ch_url.is_some() || get("TS_TYPE") == Some("clickhouse") {
        Flavor::Clickhouse
    } else if get("SENTIENT_TIMESCALE_ENABLED") == Some("false") {
        Flavor::Stock
    } else {
        // Before 0.7.0 the native install always had TimescaleDB.
        Flavor::Timescaledb
    };
    EnvSettings {
        // 0.7.0 on: SERVER__PORT; earlier releases: HTTP_PORT.
        http_port: port(&["SERVER__PORT", "HTTP_PORT"], 8080),
        mqtt_port: port(&["MQTT_PORT"], 1883),
        coap_port: port(&["COAP_PORT"], 5683),
        flavor,
        ch_http_port: ch_url.and_then(url_port).unwrap_or(8123),
        ch_db: get("CLICKHOUSE_DATABASE").unwrap_or("sentient").to_string(),
        ch_user: get("CLICKHOUSE_USER").unwrap_or("sentient").to_string(),
        ch_password: get("CLICKHOUSE_PASSWORD").unwrap_or_default().to_string(),
    }
}

/// The folder above the last `\` or `/` of a Windows path, as text (the
/// tests run on Linux, where Path does not split at `\`).
fn parent_str(p: &str) -> Option<&str> {
    let p = p.trim_end_matches(['\\', '/']);
    p.rfind(['\\', '/']).map(|i| &p[..i]).filter(|d| !d.is_empty())
}

/// PostgreSQL's folder from its service's command line: above `bin\pg_ctl.exe`
/// (true wherever its data lives, also after its data moved to another
/// disk), else the folder above the data folder (`-D`), the layout every
/// installer wrote.
pub(crate) fn pg_dir_from_service(cmdline: &str) -> Option<String> {
    let exe = program(cmdline);
    if exe.to_ascii_lowercase().ends_with("pg_ctl.exe") {
        if let Some(dir) = parent_str(&exe).and_then(parent_str) {
            return Some(dir.to_string());
        }
    }
    data_dir_arg(cmdline).and_then(|d| parent_str(&d).map(str::to_string))
}

/// How the hybrid's ClickHouse answered over HTTP.
#[derive(Debug, Clone, PartialEq)]
pub enum ChReach {
    /// `SELECT 1` as SENTIENT's ClickHouse user returned 1.
    Answers,
    /// It answers, but refused SENTIENT's sign-in (the server's words).
    Refused(String),
    /// Nothing answers on the port (not started yet, or stopped).
    Silent,
}

/// `SELECT 1` against 127.0.0.1:<port> with SENTIENT's ClickHouse user and
/// password. Never `wsl`: the distro belongs to the hidden account.
pub fn clickhouse_reach(cfg: &NativeConfig) -> ChReach {
    let r = ureq::post(&format!("http://127.0.0.1:{}/", cfg.ch_http_port))
        .query("query", "SELECT 1")
        .set("X-ClickHouse-User", &cfg.ch_user)
        .set("X-ClickHouse-Key", &cfg.ch_password)
        .timeout(std::time::Duration::from_secs(5))
        // ClickHouse refuses a POST without a length: an empty body.
        .send_string("");
    match r {
        Ok(resp) => match resp.into_string() {
            Ok(t) if t.trim() == "1" => ChReach::Answers,
            _ => ChReach::Refused("it did not return 1 for SELECT 1".into()),
        },
        Err(ureq::Error::Status(_, resp)) => {
            let text = resp.into_string().unwrap_or_default();
            ChReach::Refused(text.lines().next().unwrap_or_default().chars().take(200).collect())
        }
        Err(_) => ChReach::Silent,
    }
}

/// The hidden account's keep-alive task and the account itself are there.
#[cfg(windows)]
fn hybrid_parts() -> (bool, bool) {
    let task = sys::output("schtasks.exe", &["/query", "/tn", crate::clickhouse_wsl::KEEPALIVE_TASK]).is_some_and(|(ok, _, _)| ok);
    (task, crate::clickhouse_wsl::is_installed())
}

#[cfg(not(windows))]
fn hybrid_parts() -> (bool, bool) {
    (false, false)
}

/// What the operator should know about the hybrid's ClickHouse. Notes only:
/// however it answers, the installation can be added (the Status page
/// shows ClickHouse's state from then on).
pub(crate) fn clickhouse_notes(reach: &ChReach, port: u16, task: bool, account: bool) -> Vec<String> {
    use crate::clickhouse_wsl::{ACCOUNT, KEEPALIVE_TASK};
    let mut notes = vec![format!(
        "ClickHouse runs in WSL2 under the hidden account {ACCOUNT} (its scheduled task \"{KEEPALIVE_TASK}\" keeps it running); the Platform Manager reaches it on 127.0.0.1:{port}. Removing ClickHouse from here will not work: the Platform Manager does not have that account's password."
    )];
    match reach {
        ChReach::Answers => {}
        ChReach::Refused(why) => notes.push(format!("ClickHouse answers on 127.0.0.1:{port} but refused SENTIENT's sign-in ({why}). Check the ClickHouse password in SENTIENT's settings.")),
        ChReach::Silent => notes.push(format!("ClickHouse does not answer on 127.0.0.1:{port} now. It starts about half a minute after Windows starts; Status shows when it answers.")),
    }
    if !task {
        notes.push(format!("The scheduled task \"{KEEPALIVE_TASK}\" was not found: ClickHouse may not start with Windows. Repair sets it up again."));
    } else if !account {
        notes.push(format!("The account {ACCOUNT} was not found."));
    }
    notes
}

fn read_from(install_dir: &Path) -> Result<FoundWindows, String> {
    let descriptor = install_dir.join("services").join(format!("{APP_SERVICE}.xml"));
    let xml = std::fs::read_to_string(&descriptor).map_err(|e| format!("read {}: {e}", descriptor.display()))?;
    let env = descriptor_env(&xml);
    let get = |k: &str| env.iter().rev().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    let mut notes = Vec::new();
    let mut problem = None;
    let db = get("DATABASE_URL").and_then(crate::distro::adopt::parse_db_url).ok_or("SENTIENT's DATABASE_URL could not be read")?;
    if !matches!(db.host.as_str(), "127.0.0.1" | "localhost" | "::1") {
        problem = Some(format!("SENTIENT's database is on {}; the Platform Manager manages a PostgreSQL on this computer only.", db.host));
    }
    let pg_dir = match service_path(PG_SERVICE).and_then(|c| pg_dir_from_service(&c)) {
        Some(dir) => PathBuf::from(dir),
        None => {
            problem.get_or_insert_with(|| format!("The PostgreSQL service {PG_SERVICE} was not found; the Platform Manager manages the PostgreSQL its installers set up."));
            PathBuf::new()
        }
    };
    if !pg_dir.as_os_str().is_empty() && !pg_dir.join("bin").join("psql.exe").exists() {
        problem.get_or_insert_with(|| format!("{} has no bin\\psql.exe.", pg_dir.display()));
    }
    let s = env_settings(&env);
    let own: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let state_dir = get("REPORT_OUTPUT_DIR")
        .and_then(|p| Path::new(p).parent().map(Path::to_path_buf))
        .unwrap_or_else(|| install_dir.join("state"));
    let cfg = NativeConfig {
        install_dir: install_dir.to_path_buf(),
        pg_dir,
        state_dir,
        db_name: db.database.clone(),
        db_user: db.user.clone(),
        db_password: db.password.clone(),
        pg_superuser_password: String::new(),
        http_port: s.http_port,
        mqtt_port: s.mqtt_port,
        coap_port: s.coap_port,
        pg_port: db.port,
        load_demo: false,
        source: ArtifactSource::Remote { base_url: String::new() },
        license_server_url: get("LICENSE_SERVER_URL").unwrap_or("https://license.invenia.in").to_string(),
        extra_env: Vec::new(),
        jwt_secret: get("JWT_SECRET").unwrap_or_default().to_string(),
        flavor: s.flavor,
        tuning: stack::Tuning::from_env(own, ENGINE_ENV),
        pg: stack::PgTuning::default(),
        ch: stack::ChTuning::default(),
        ch_db: s.ch_db,
        ch_user: s.ch_user,
        ch_password: s.ch_password,
        ch_http_port: s.ch_http_port,
        ch_native_port: 9000,
        old_pg: Default::default(),
    };
    if cfg.flavor == Flavor::Clickhouse {
        let (task, account) = hybrid_parts();
        notes.extend(clickhouse_notes(&clickhouse_reach(&cfg), cfg.ch_http_port, task, account));
    }
    if !install_dir.join("bin").join("sentient-install.exe").exists() {
        notes.push(format!("{} has no bin\\sentient-install.exe, which updates need.", install_dir.display()));
    }
    Ok(FoundWindows { cfg, running: installed_running(), notes, problem })
}

/// SQL as SENTIENT's own role (a superuser on installs the Platform
/// Manager's installers made).
fn psql_as_app(cfg: &NativeConfig, sql: &str) -> Result<String, String> {
    let exe = cfg.pg_dir.join("bin").join("psql.exe");
    let port = cfg.pg_port.to_string();
    let mut c = sys::command(&exe.to_string_lossy());
    c.env("PGPASSWORD", &cfg.db_password)
        .args(["-h", "127.0.0.1", "-p", &port, "-U", &cfg.db_user, "-d", &cfg.db_name, "-tAc", sql]);
    let o = c.output().map_err(|e| format!("run psql: {e}"))?;
    if !o.status.success() {
        return Err(format!("psql: {}", sys::decode(&o.stderr).trim()));
    }
    Ok(sys::decode(&o.stdout).trim().to_string())
}

/// Does `password` sign the `postgres` role in?
pub fn admin_password_works(cfg: &NativeConfig, password: &str) -> bool {
    let probe = NativeConfig { pg_superuser_password: password.to_string(), ..cfg.clone() };
    super::psql(&probe, "postgres", "SELECT 1").is_ok_and(|o| o.trim() == "1")
}

/// Give the `postgres` role `password`, through SENTIENT's own role.
pub fn set_admin_password(cfg: &NativeConfig, password: &str) -> Result<(), String> {
    let is_super = psql_as_app(cfg, "SELECT rolsuper FROM pg_roles WHERE rolname = current_user")?;
    if is_super.trim() != "t" {
        return Err(format!("{} is not a PostgreSQL superuser, so it cannot set the admin password; enter the existing one instead.", cfg.db_user));
    }
    psql_as_app(cfg, &format!("ALTER ROLE postgres WITH PASSWORD '{}'", password.replace('\'', "''"))).map(|_| ())
}

/// Check the Platform Manager can do its work on a found installation;
/// `cfg.pg_superuser_password` must be set.
pub fn verify(found: &FoundWindows, cfg: &NativeConfig) -> Result<(), String> {
    if let Some(p) = &found.problem {
        return Err(p.clone());
    }
    psql_as_app(cfg, "SELECT 1").map_err(|e| format!("Could not sign in to the database as {}: {e}", cfg.db_user))?;
    if !admin_password_works(cfg, &cfg.pg_superuser_password) {
        return Err("The PostgreSQL admin (postgres) password does not work.".into());
    }
    if found.running && !super::is_running(cfg.http_port) {
        return Err(format!("SENTIENT is running but does not answer on port {}.", cfg.http_port));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_environment_is_read_back() {
        let xml = "<service>\n  <env name=\"DATABASE_URL\" value=\"postgresql://sentient:p&amp;w@127.0.0.1:5432/sentient\"/>\n  <env name=\"RUST_LOG\" value=\"info\"/>\n</service>";
        let env = descriptor_env(xml);
        assert_eq!(env[0], ("DATABASE_URL".into(), "postgresql://sentient:p&w@127.0.0.1:5432/sentient".into()));
        assert_eq!(env[1], ("RUST_LOG".into(), "info".into()));
    }

    #[test]
    fn service_command_lines_are_split() {
        assert_eq!(program("\"C:\\Program Files\\SENTIENT\\services\\SentientServer.exe\""), "C:\\Program Files\\SENTIENT\\services\\SentientServer.exe");
        assert_eq!(program("C:\\x\\a.exe --flag"), "C:\\x\\a.exe");
        assert_eq!(
            data_dir_arg("\"C:\\PostgreSQL\\18\\bin\\pg_ctl.exe\" runservice -N \"sentient-postgresql\" -D \"C:\\PostgreSQL\\18\\data\" -w").as_deref(),
            Some("C:\\PostgreSQL\\18\\data")
        );
        assert_eq!(data_dir_arg("pg_ctl.exe runservice -D C:\\pg\\data -w").as_deref(), Some("C:\\pg\\data"));
    }

    fn env(xml: &str) -> Vec<(String, String)> {
        descriptor_env(xml)
    }

    #[test]
    fn a_release_before_0_7_reads_back_with_its_own_port_names() {
        // ff215df..02bc9d6: HTTP_PORT, no storage variables (TimescaleDB).
        let xml = "<service>\n  <id>SentientServer</id>\n  <env name=\"DATABASE_URL\" value=\"postgresql://sentient:Xy12@127.0.0.1:5433/sentient\"/>\n  <env name=\"HTTP_PORT\" value=\"8090\"/>\n  <env name=\"MQTT_PORT\" value=\"1884\"/>\n  <env name=\"COAP_PORT\" value=\"5684\"/>\n  <env name=\"RUST_LOG\" value=\"info\"/>\n</service>";
        let s = env_settings(&env(xml));
        assert_eq!((s.http_port, s.mqtt_port, s.coap_port), (8090, 1884, 5684));
        assert_eq!(s.flavor, Flavor::Timescaledb);
    }

    #[test]
    fn the_hybrid_reads_back_with_clickhouse_from_the_descriptor() {
        let xml = "<service>\n  <env name=\"DATABASE_URL\" value=\"postgresql://sentient:Ab@127.0.0.1:5432/sentient\"/>\n  <env name=\"SERVER__HOST\" value=\"0.0.0.0\"/>\n  <env name=\"SERVER__PORT\" value=\"8080\"/>\n  <env name=\"CLICKHOUSE_URL\" value=\"http://127.0.0.1:18123\"/>\n  <env name=\"CLICKHOUSE_DATABASE\" value=\"plant\"/>\n  <env name=\"CLICKHOUSE_USER\" value=\"writer\"/>\n  <env name=\"CLICKHOUSE_PASSWORD\" value=\"c&amp;h\"/>\n  <env name=\"TS_TYPE\" value=\"clickhouse\"/>\n</service>";
        let s = env_settings(&env(xml));
        assert_eq!(s.flavor, Flavor::Clickhouse);
        assert_eq!((s.http_port, s.ch_http_port), (8080, 18123));
        assert_eq!((s.ch_db.as_str(), s.ch_user.as_str(), s.ch_password.as_str()), ("plant", "writer", "c&h"));
        // Defaults when the URL names no port, and the plain flavours.
        let s = env_settings(&env("<env name=\"TS_TYPE\" value=\"clickhouse\"/>"));
        assert_eq!((s.flavor, s.ch_http_port, s.ch_user.as_str()), (Flavor::Clickhouse, 8123, "sentient"));
        let s = env_settings(&env("<env name=\"SENTIENT_TIMESCALE_ENABLED\" value=\"false\"/><env name=\"SERVER__PORT\" value=\"9090\"/><env name=\"HTTP_PORT\" value=\"1\"/>"));
        assert_eq!((s.flavor, s.http_port), (Flavor::Stock, 9090));
    }

    #[test]
    fn clickhouse_ports_come_from_its_url() {
        assert_eq!(url_port("http://127.0.0.1:8123"), Some(8123));
        assert_eq!(url_port("http://127.0.0.1:8124/"), Some(8124));
        assert_eq!(url_port("http://localhost:9123/?database=x"), Some(9123));
        assert_eq!(url_port("http://[::1]:8125"), Some(8125));
        assert_eq!(url_port("http://127.0.0.1"), None);
        assert_eq!(url_port("http://[::1]"), None);
    }

    #[test]
    fn postgresql_folder_comes_from_pg_ctl_even_with_its_data_elsewhere() {
        let svc = "\"C:\\PostgreSQL\\18\\bin\\pg_ctl.exe\" runservice -N \"sentient-postgresql\" -D \"D:\\SENTIENT data\\pg\" -w";
        assert_eq!(pg_dir_from_service(svc).as_deref(), Some("C:\\PostgreSQL\\18"));
        // The layout every installer wrote: <pg>\data.
        let svc = "\"C:\\Program Files\\SENTIENT\\pgsql\\bin\\pg_ctl.exe\" runservice -N \"sentient-postgresql\" -D \"C:\\Program Files\\SENTIENT\\pgsql\\data\" -w";
        assert_eq!(pg_dir_from_service(svc).as_deref(), Some("C:\\Program Files\\SENTIENT\\pgsql"));
        // Some other program: the folder above the data folder.
        assert_eq!(pg_dir_from_service("C:\\x\\wrapper.exe -D C:\\pg\\data").as_deref(), Some("C:\\pg"));
        assert_eq!(pg_dir_from_service("C:\\x\\wrapper.exe"), None);
    }

    #[test]
    fn the_hybrids_clickhouse_is_only_ever_a_note() {
        let n = clickhouse_notes(&ChReach::Answers, 8123, true, true);
        assert_eq!(n.len(), 1);
        assert!(n[0].contains("sentient-clickhouse") && n[0].contains("SENTIENT ClickHouse") && n[0].contains("127.0.0.1:8123"));
        let n = clickhouse_notes(&ChReach::Silent, 8123, true, true);
        assert!(n[1].contains("does not answer"));
        let n = clickhouse_notes(&ChReach::Refused("Authentication failed".into()), 8123, false, true);
        assert!(n[1].contains("Authentication failed") && n[2].contains("was not found"));
    }
}
