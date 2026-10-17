//! What a SENTIENT deployment is made of, independent of *how* it runs.
//!
//! The Docker engine ([`crate::distro`]) and the two native engines
//! ([`crate::native`], [`crate::native_linux`]) all install the same platform
//! with the same knobs; the reference `docker/docker-compose.*.yml` files in
//! the SENTIENT repository are the source of truth for both the knob list and
//! the default values. This module holds that list once so the compose file
//! the Docker engine writes and the environment the native engines hand to
//! the service cannot drift apart.

use serde::{Deserialize, Serialize};

/// Where telemetry history lives. Chosen at install time and fixed for the
/// life of the database — the schema differs per flavour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Flavor {
    /// Plain PostgreSQL: entities and telemetry in one database, no extension.
    Stock,
    /// PostgreSQL with the TimescaleDB extension: hypertables for `ts_kv`.
    Timescaledb,
    /// PostgreSQL for entities, ClickHouse for all telemetry (the reference
    /// deployment; what a Linux install defaults to).
    #[default]
    Clickhouse,
}

impl Flavor {
    pub fn parse(s: &str) -> Option<Flavor> {
        match s.trim().to_ascii_lowercase().as_str() {
            "stock" | "postgres" | "postgresql" | "sql" => Some(Flavor::Stock),
            "timescaledb" | "timescale" => Some(Flavor::Timescaledb),
            "clickhouse" | "hybrid" => Some(Flavor::Clickhouse),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Flavor::Stock => "stock",
            Flavor::Timescaledb => "timescaledb",
            Flavor::Clickhouse => "clickhouse",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Flavor::Stock => "PostgreSQL only",
            Flavor::Timescaledb => "PostgreSQL + TimescaleDB",
            Flavor::Clickhouse => "PostgreSQL + ClickHouse",
        }
    }
    pub fn uses_clickhouse(self) -> bool {
        matches!(self, Flavor::Clickhouse)
    }
    pub fn uses_timescaledb(self) -> bool {
        matches!(self, Flavor::Timescaledb)
    }
}

/// One free-form environment override. Kept as a struct rather than a tuple
/// so the JSON the wizard sends is `{name, value}` — legible when it is
/// persisted in the settings store and read back by a human.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvVar {
    pub name: String,
    pub value: String,
}

/// Every knob the reference compose files set on the `sentient` service,
/// with the reference values as defaults. Field names follow the environment
/// variable they drive, lower-cased, so the mapping is mechanical.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tuning {
    // identity — two nodes reporting the same service id are treated as one
    pub instance_id: String,
    pub service_id: String,
    // connection pools
    pub db_pool_primary_min: u32,
    pub db_pool_primary_max: u32,
    pub database_pool_min: u32,
    pub database_pool_max: u32,
    // write batching
    pub sql_ts_batch_size: u32,
    pub sql_ts_batch_threads: u32,
    pub sql_ts_batch_max_delay_ms: u32,
    pub sql_ts_latest_batch_size: u32,
    pub sql_ts_latest_batch_threads: u32,
    pub sql_ts_latest_batch_max_delay_ms: u32,
    pub sql_attributes_batch_size: u32,
    pub sql_attributes_batch_threads: u32,
    pub sql_attributes_batch_max_delay_ms: u32,
    // entity caches (TTL in minutes)
    pub cache_devices_max_size: u32,
    pub cache_devices_ttl: u32,
    pub cache_devices_null_ttl: u32,
    pub cache_device_profiles_max_size: u32,
    pub cache_device_profiles_ttl: u32,
    pub cache_asset_profiles_max_size: u32,
    pub cache_asset_profiles_ttl: u32,
    // retention — 0 means keep forever
    pub audit_log_ttl_days: u32,
    pub notifications_ttl_days: u32,
    pub event_cleanup_enabled: bool,
    pub event_cleanup_interval_secs: u64,
    pub events_ttl_secs: u64,
    pub debug_events_ttl_secs: u64,
    // latest-values cache (ClickHouse flavour; entries live until evicted by size)
    pub ts_latest_cache_max_entries: u64,
    // server
    pub rust_log: String,
    pub analytics_enabled: bool,
    /// Appended verbatim, after everything above, so it can override any of it.
    pub extra_env: Vec<EnvVar>,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            instance_id: "sentient-01".into(),
            service_id: "st-node-1".into(),
            db_pool_primary_min: 16,
            db_pool_primary_max: 48,
            database_pool_min: 8,
            database_pool_max: 24,
            sql_ts_batch_size: 8000,
            sql_ts_batch_threads: 5,
            sql_ts_batch_max_delay_ms: 100,
            sql_ts_latest_batch_size: 4000,
            sql_ts_latest_batch_threads: 5,
            sql_ts_latest_batch_max_delay_ms: 50,
            sql_attributes_batch_size: 4000,
            sql_attributes_batch_threads: 5,
            sql_attributes_batch_max_delay_ms: 50,
            cache_devices_max_size: 100_000,
            cache_devices_ttl: 1440,
            cache_devices_null_ttl: 5,
            cache_device_profiles_max_size: 10_000,
            cache_device_profiles_ttl: 1440,
            cache_asset_profiles_max_size: 10_000,
            cache_asset_profiles_ttl: 1440,
            audit_log_ttl_days: 0,
            notifications_ttl_days: 0,
            event_cleanup_enabled: true,
            event_cleanup_interval_secs: 3600,
            events_ttl_secs: 0,
            debug_events_ttl_secs: 604_800,
            ts_latest_cache_max_entries: 1_000_000,
            rust_log: "info,sentient_api=info".into(),
            analytics_enabled: true,
            extra_env: Vec::new(),
        }
    }
}

/// PostgreSQL server settings the reference compose passes on the command
/// line. Sizes are PostgreSQL unit strings ("128MB") because that is how an
/// operator knows them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PgTuning {
    pub pg_max_connections: u32,
    pub pg_shared_buffers: String,
    pub pg_work_mem: String,
    pub pg_effective_cache_size: String,
}

impl Default for PgTuning {
    fn default() -> Self {
        Self {
            pg_max_connections: 100,
            pg_shared_buffers: "128MB".into(),
            pg_work_mem: "2MB".into(),
            pg_effective_cache_size: "384MB".into(),
        }
    }
}

impl PgTuning {
    /// `name=value` pairs, for `postgres -c` or `ALTER SYSTEM SET`.
    pub fn settings(&self) -> Vec<(&'static str, String)> {
        vec![
            ("max_connections", self.pg_max_connections.to_string()),
            ("shared_buffers", self.pg_shared_buffers.clone()),
            ("work_mem", self.pg_work_mem.clone()),
            ("effective_cache_size", self.pg_effective_cache_size.clone()),
        ]
    }
}

/// ClickHouse server settings. The defaults assume the whole stack shares
/// one modest machine; ClickHouse's own defaults assume a dedicated server.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChTuning {
    pub ch_mark_cache_bytes: u64,
    pub ch_uncompressed_cache_bytes: u64,
}

impl Default for ChTuning {
    fn default() -> Self {
        Self { ch_mark_cache_bytes: 268_435_456, ch_uncompressed_cache_bytes: 134_217_728 }
    }
}

/// How the server reaches ClickHouse.
#[derive(Clone, Debug)]
pub struct ClickhouseConn {
    pub url: String,
    pub database: String,
    pub user: String,
    pub password: String,
}

/// The storage-selection environment for a flavour. `ch` must be `Some` for
/// [`Flavor::Clickhouse`] — its presence (`CLICKHOUSE_URL`) is what makes the
/// server hybrid.
pub fn storage_env(flavor: Flavor, ch: Option<&ClickhouseConn>, t: &Tuning) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = Vec::new();
    let mut put = |k: &str, val: String| v.push((k.to_string(), val));
    match flavor {
        Flavor::Stock => {
            put("TS_TYPE", "sql".into());
            // The installer skips the TimescaleDB hypertable and ts_kv stays a
            // plain table on stock PostgreSQL.
            put("SENTIENT_TIMESCALE_ENABLED", "false".into());
        }
        Flavor::Timescaledb => {
            put("TS_TYPE", "sql".into());
        }
        Flavor::Clickhouse => {
            let ch = ch.expect("ClickHouse connection for the clickhouse flavour");
            put("CLICKHOUSE_URL", ch.url.clone());
            put("CLICKHOUSE_DATABASE", ch.database.clone());
            put("CLICKHOUSE_USER", ch.user.clone());
            put("CLICKHOUSE_PASSWORD", ch.password.clone());
            put("TS_TYPE", "clickhouse".into());
            put("TS_LATEST_TYPE", "clickhouse".into());
            put("TS_LATEST_CACHE", "inprocess".into());
            put("TS_LATEST_CACHE_MAX_ENTRIES", t.ts_latest_cache_max_entries.to_string());
            put("TS_LATEST_CACHE_TTL_SECS", "0".into());
        }
    }
    v
}

/// The tuning knobs as environment, in the order the reference compose lists
/// them. `extra_env` comes last so it can override anything.
pub fn tuning_env(t: &Tuning) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = vec![
        ("SENTIENT_INSTANCE_ID".into(), t.instance_id.clone()),
        ("SENTIENT_SERVICE_ID".into(), t.service_id.clone()),
        ("DB_POOL_PRIMARY_MIN".into(), t.db_pool_primary_min.to_string()),
        ("DB_POOL_PRIMARY_MAX".into(), t.db_pool_primary_max.to_string()),
        ("DATABASE_POOL_MIN".into(), t.database_pool_min.to_string()),
        ("DATABASE_POOL_MAX".into(), t.database_pool_max.to_string()),
        ("SQL_TS_BATCH_SIZE".into(), t.sql_ts_batch_size.to_string()),
        ("SQL_TS_BATCH_THREADS".into(), t.sql_ts_batch_threads.to_string()),
        ("SQL_TS_BATCH_MAX_DELAY_MS".into(), t.sql_ts_batch_max_delay_ms.to_string()),
        ("SQL_TS_LATEST_BATCH_SIZE".into(), t.sql_ts_latest_batch_size.to_string()),
        ("SQL_TS_LATEST_BATCH_THREADS".into(), t.sql_ts_latest_batch_threads.to_string()),
        ("SQL_TS_LATEST_BATCH_MAX_DELAY_MS".into(), t.sql_ts_latest_batch_max_delay_ms.to_string()),
        ("SQL_ATTRIBUTES_BATCH_SIZE".into(), t.sql_attributes_batch_size.to_string()),
        ("SQL_ATTRIBUTES_BATCH_THREADS".into(), t.sql_attributes_batch_threads.to_string()),
        ("SQL_ATTRIBUTES_BATCH_MAX_DELAY_MS".into(), t.sql_attributes_batch_max_delay_ms.to_string()),
        ("CACHE_SPECS_DEVICES_MAX_SIZE".into(), t.cache_devices_max_size.to_string()),
        ("CACHE_SPECS_DEVICES_TTL".into(), t.cache_devices_ttl.to_string()),
        ("CACHE_SPECS_DEVICES_NULL_TTL".into(), t.cache_devices_null_ttl.to_string()),
        ("CACHE_SPECS_DEVICE_PROFILES_MAX_SIZE".into(), t.cache_device_profiles_max_size.to_string()),
        ("CACHE_SPECS_DEVICE_PROFILES_TTL".into(), t.cache_device_profiles_ttl.to_string()),
        ("CACHE_SPECS_ASSET_PROFILES_MAX_SIZE".into(), t.cache_asset_profiles_max_size.to_string()),
        ("CACHE_SPECS_ASSET_PROFILES_TTL".into(), t.cache_asset_profiles_ttl.to_string()),
        ("AUDIT_LOG_TTL_DAYS".into(), t.audit_log_ttl_days.to_string()),
        ("NOTIFICATIONS_TTL_DAYS".into(), t.notifications_ttl_days.to_string()),
        ("SENTIENT__EVENT_CLEANUP__ENABLED".into(), t.event_cleanup_enabled.to_string()),
        ("SENTIENT__EVENT_CLEANUP__CLEANUP_INTERVAL_SECS".into(), t.event_cleanup_interval_secs.to_string()),
        ("SENTIENT__EVENT_CLEANUP__EVENTS_TTL_SECS".into(), t.events_ttl_secs.to_string()),
        ("SENTIENT__EVENT_CLEANUP__DEBUG_EVENTS_TTL_SECS".into(), t.debug_events_ttl_secs.to_string()),
        ("RUST_LOG".into(), t.rust_log.clone()),
        ("RUST_BACKTRACE".into(), "1".into()),
        ("ANALYTICS_ENABLED".into(), t.analytics_enabled.to_string()),
    ];
    v.extend(t.extra_env.iter().map(|e| (e.name.clone(), e.value.clone())));
    v
}

/// The `config.d` drop-in the reference compose inlines for ClickHouse, with
/// optional port overrides for a native install (a container remaps ports
/// from the outside instead). Same content in both engines on purpose.
pub fn clickhouse_config_xml(ch: &ChTuning, http_port: Option<u16>, tcp_port: Option<u16>) -> String {
    let ports = match (http_port, tcp_port) {
        (None, None) => String::new(),
        _ => format!(
            "\n    {}{}",
            http_port.map(|p| format!("<http_port>{p}</http_port>\n    ")).unwrap_or_default(),
            tcp_port.map(|p| format!("<tcp_port>{p}</tcp_port>\n")).unwrap_or_default(),
        ),
    };
    format!(
        r#"<?xml version="1.0"?>
<clickhouse>
    <!-- Recomputing several hundred async metrics every second walks every
         table, part and disk: 8-12% of a core while fully idle. -->
    <asynchronous_metrics_update_period_s>60</asynchronous_metrics_update_period_s>

    <!-- ClickHouse's defaults assume a dedicated analytics server; SENTIENT
         often runs the whole stack on one modest box. -->
    <mark_cache_size>{mark}</mark_cache_size>
    <uncompressed_cache_size>{uncompressed}</uncompressed_cache_size>

    <!-- ClickHouse logs about itself continuously. These are diagnostics for
         tuning ClickHouse itself; query_log and part_log are kept. -->
    <metric_log remove="1"/>
    <asynchronous_metric_log remove="1"/>
    <trace_log remove="1"/>
    <text_log remove="1"/>
    <processors_profile_log remove="1"/>

    <query_log replace="1">
        <database>system</database>
        <table>query_log</table>
        <engine>ENGINE = MergeTree PARTITION BY toYYYYMM(event_date) ORDER BY (event_date, event_time) TTL event_date + INTERVAL 14 DAY DELETE</engine>
        <flush_interval_milliseconds>30000</flush_interval_milliseconds>
    </query_log>{ports}
</clickhouse>
"#,
        mark = ch.ch_mark_cache_bytes,
        uncompressed = ch.ch_uncompressed_cache_bytes,
    )
}

/// Reject anything that would break a connection URL or a shell-quoted
/// script. Names and passwords come from a text box; the engines splice them
/// into `postgresql://…` URLs and SQL, so keep them to the safe set instead
/// of trying to escape every context.
pub fn validate_identifier(what: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{what} cannot be empty."));
    }
    if !value.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(format!("{what} may only contain letters, digits and underscores."));
    }
    Ok(())
}

pub fn validate_password(what: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{what} cannot be empty."));
    }
    if value.chars().any(|c| c.is_whitespace() || "'\"\\@/:#?&$`".contains(c)) {
        return Err(format!("{what} may not contain spaces, quotes or the characters @ / : # ? & $ ` \\."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_reference_compose() {
        let t = Tuning::default();
        let env = tuning_env(&t);
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap();
        assert_eq!(get("DB_POOL_PRIMARY_MAX"), "48");
        assert_eq!(get("SQL_TS_BATCH_SIZE"), "8000");
        assert_eq!(get("CACHE_SPECS_DEVICES_MAX_SIZE"), "100000");
        assert_eq!(get("SENTIENT__EVENT_CLEANUP__DEBUG_EVENTS_TTL_SECS"), "604800");
    }

    #[test]
    fn extra_env_comes_last_so_it_overrides() {
        let mut t = Tuning::default();
        t.extra_env.push(EnvVar { name: "RUST_LOG".into(), value: "debug".into() });
        let env = tuning_env(&t);
        let last = env.iter().rev().find(|(n, _)| n == "RUST_LOG").unwrap();
        assert_eq!(last.1, "debug");
    }

    #[test]
    fn partial_json_fills_defaults() {
        let t: Tuning = serde_json::from_str(r#"{"rust_log":"warn"}"#).unwrap();
        assert_eq!(t.rust_log, "warn");
        assert_eq!(t.db_pool_primary_max, 48);
    }

    #[test]
    fn hybrid_env_carries_clickhouse_url() {
        let ch = ClickhouseConn { url: "http://clickhouse:8123".into(), database: "sentient".into(), user: "sentient".into(), password: "x".into() };
        let env = storage_env(Flavor::Clickhouse, Some(&ch), &Tuning::default());
        assert!(env.iter().any(|(k, v)| k == "CLICKHOUSE_URL" && v == "http://clickhouse:8123"));
        assert!(env.iter().any(|(k, v)| k == "TS_TYPE" && v == "clickhouse"));
        let stock = storage_env(Flavor::Stock, None, &Tuning::default());
        assert!(stock.iter().any(|(k, v)| k == "SENTIENT_TIMESCALE_ENABLED" && v == "false"));
    }
}
