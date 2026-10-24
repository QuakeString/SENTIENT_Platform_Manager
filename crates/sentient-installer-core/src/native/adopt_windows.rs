// Copyright © 2016-2026 The SENTIENT Authors

//! A native Windows installation whose installation record is missing (a
//! lost record, an older installer): read back from its services. SENTIENT's
//! WinSW descriptor (`services\SentientServer.xml`) carries its whole
//! environment; the PostgreSQL service's command line says where the
//! cluster lives.
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
    let pg_dir = match service_path(PG_SERVICE).and_then(|c| data_dir_arg(&c)) {
        Some(data) => PathBuf::from(data).parent().map(Path::to_path_buf).unwrap_or_default(),
        None => {
            problem.get_or_insert_with(|| format!("The PostgreSQL service {PG_SERVICE} was not found; the Platform Manager manages the PostgreSQL its installers set up."));
            PathBuf::new()
        }
    };
    if !pg_dir.as_os_str().is_empty() && !pg_dir.join("bin").join("psql.exe").exists() {
        problem.get_or_insert_with(|| format!("{} has no bin\\psql.exe.", pg_dir.display()));
    }
    let ch_url = get("CLICKHOUSE_URL").filter(|u| !u.is_empty());
    let flavor = if ch_url.is_some() {
        Flavor::Clickhouse
    } else if get("SENTIENT_TIMESCALE_ENABLED") == Some("false") {
        Flavor::Stock
    } else {
        Flavor::Timescaledb
    };
    let ch_http_port = ch_url
        .and_then(|u| u.rsplit(':').next())
        .and_then(|p| p.trim_end_matches('/').parse().ok())
        .unwrap_or(8123);
    if flavor == Flavor::Clickhouse {
        notes.push("ClickHouse runs in WSL2 under a hidden account whose password the Platform Manager does not have: removing ClickHouse from here will not work.".into());
    }
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
        http_port: get("SERVER__PORT").and_then(|p| p.parse().ok()).unwrap_or(8080),
        mqtt_port: 1883,
        coap_port: 5683,
        pg_port: db.port,
        load_demo: false,
        source: ArtifactSource::Remote { base_url: String::new() },
        license_server_url: get("LICENSE_SERVER_URL").unwrap_or("https://license.invenia.in").to_string(),
        extra_env: Vec::new(),
        jwt_secret: get("JWT_SECRET").unwrap_or_default().to_string(),
        flavor,
        tuning: stack::Tuning::from_env(own, ENGINE_ENV),
        pg: stack::PgTuning::default(),
        ch: stack::ChTuning::default(),
        ch_db: get("CLICKHOUSE_DATABASE").unwrap_or("sentient").to_string(),
        ch_user: get("CLICKHOUSE_USER").unwrap_or("sentient").to_string(),
        ch_password: get("CLICKHOUSE_PASSWORD").unwrap_or_default().to_string(),
        ch_http_port,
        ch_native_port: 9000,
    };
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
}
