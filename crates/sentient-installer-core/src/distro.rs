//! Docker deployment engine.
//!
//! On Windows the stack runs inside a dedicated `sentient` WSL2 distro that
//! [`setup`] creates (Ubuntu rootfs, `wsl --import`, systemd, Docker Engine).
//! On Linux it runs on the host's own Docker, which [`setup`] installs from
//! the distribution's packages when missing. Everything after "Docker is
//! reachable" is one code path: [`compose`] writes the same stack the
//! reference `docker/docker-compose.<flavor>.yml` files in the SENTIENT
//! repository describe, and every command goes through [`shell_args`], which
//! is `wsl.exe … bash -lc` on Windows and plain `bash -lc` on Linux.

use std::path::Path;

use crate::exec;
use crate::progress::{Progress, ProgressFn};
use crate::stack::{self, ChTuning, ClickhouseConn, EnvVar, Flavor, PgTuning, Tuning};
use crate::sys;

/// The Platform Manager's own WSL distro (Windows). Its lifecycle (import,
/// set up, terminate, unregister, move) always names it. Docker commands
/// run in [`docker_distro`] instead, which is this one unless the
/// installation was found in the operator's own distro
/// (`DeployConfig::wsl_distro`), which is never terminated or unregistered.
pub const DISTRO: &str = "sentient";

pub mod adopt;
pub mod docker_ops;
pub mod hosts;
pub mod idle;
pub mod move_data;
pub mod gateway_docker;
pub mod gateway_legacy;
pub mod port_forward;
#[cfg(not(windows))]
pub mod gateway_native;
#[cfg_attr(not(windows), allow(dead_code))]
const ROOTFS_URL: &str =
    "https://cloud-images.ubuntu.com/wsl/releases/24.04/current/ubuntu-noble-wsl-amd64-24.04lts.rootfs.tar.gz";

/// Images per flavour. Pinned so an install is reproducible; `update` moves
/// only the SENTIENT image, the databases stay where they were installed.
const PG_IMAGE_STOCK: &str = "postgres:18-alpine";
const PG_IMAGE_TIMESCALE: &str = "timescale/timescaledb:2.26.1-pg18";
const CH_IMAGE: &str = "clickhouse/clickhouse-server:24.8-alpine";

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Everything the wizard lets the operator choose, with the reference values
/// as defaults. `#[serde(default)]` so a config saved by an older Platform
/// Manager — or a partial one from the frontend — still deserialises.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct DeployConfig {
    /// Prefix for container and volume names. "sentient" reproduces the
    /// reference names exactly (`sentient`, `sentient-postgres`, `st-pg-data`
    /// …); any other value derives its own, so a second stack can coexist.
    pub stack_name: String,
    /// The SENTIENT image, tag included. An offline site points this at the
    /// tag it side-loaded with `docker load`.
    pub image: String,
    pub flavor: Flavor,
    pub license_server_url: String,

    // PostgreSQL
    pub db_name: String,
    pub db_user: String,
    pub db_password: String,
    pub pg_port: u16,
    #[serde(flatten)]
    pub pg: PgTuning,

    // ClickHouse (hybrid flavour only)
    pub ch_db: String,
    pub ch_user: String,
    pub ch_password: String,
    pub ch_http_port: u16,
    pub ch_native_port: u16,
    #[serde(flatten)]
    pub ch: ChTuning,

    // published ports
    pub http_port: u16,
    pub mqtt_port: u16,
    pub mqtts_port: u16,
    pub coap_port: u16,
    pub coaps_port: u16,

    // container log rotation
    pub log_max_size: String,
    pub log_max_file: u32,

    #[serde(flatten)]
    pub tuning: Tuning,
    pub load_demo: bool,
    /// PostgreSQL runs on the TimescaleDB image whatever the flavour: set once
    /// a database has had the extension (a migration away from TimescaleDB
    /// keeps the old hypertable, which only that image can read).
    #[serde(default)]
    pub keep_timescale_image: bool,
    /// A stack SPM did not write (an adopted installation): it keeps running
    /// from its own compose files, which SPM never rewrites.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external: Option<ExternalCompose>,
    /// Windows: the WSL distro this stack's Docker runs in when it is not
    /// the Platform Manager's own (`sentient`, [`DISTRO`]): a stack found
    /// in the operator's own distro (Ubuntu, …) and added. Every Docker
    /// command for it runs there ([`with_docker_distro`]); that distro is
    /// never stopped, unregistered or set up by the Platform Manager. None:
    /// its own distro (and always on Linux), so older records load as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wsl_distro: Option<String>,
    /// Container and volume names of an adopted stack whose names do not
    /// follow the stack name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub names: Option<Names>,
    /// PostgreSQL / ClickHouse published on 127.0.0.1 only: a stack taken
    /// over whose database was not published at all gets a port the
    /// Platform Manager can reach and nobody else.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub db_loopback: bool,
    /// Where PostgreSQL keeps its cluster inside the volume, when not the
    /// Platform Manager's `/var/lib/postgresql/data` (an adopted stack on the
    /// postgres image's own default, `/var/lib/postgresql/18/docker`): the
    /// same cluster must start after a takeover, never a new empty one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pgdata: Option<String>,
    /// Where each data volume keeps its data when not in Docker's own
    /// folder: a folder on the host the volume is bound to (driver_opts
    /// type none, o bind, device <folder>), the volume's name kept
    /// (move_data).
    #[serde(default, skip_serializing_if = "Locations::is_empty")]
    pub locations: Locations,
    /// The places data was moved away from, kept until removed (move_data).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub old_copies: Vec<move_data::OldCopy>,
}

/// The host folders the data volumes are bound to; None: Docker's own
/// folder (a plain named volume).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Locations {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

impl Locations {
    pub fn is_empty(&self) -> bool {
        self.pg.is_none() && self.ch.is_none() && self.data.is_none()
    }
    pub fn get(&self, store: move_data::Store) -> Option<&String> {
        match store {
            move_data::Store::Pg => self.pg.as_ref(),
            move_data::Store::Ch => self.ch.as_ref(),
            move_data::Store::Data => self.data.as_ref(),
            move_data::Store::Backups => None,
        }
    }
    pub fn set(&mut self, store: move_data::Store, place: Option<String>) {
        match store {
            move_data::Store::Pg => self.pg = place,
            move_data::Store::Ch => self.ch = place,
            move_data::Store::Data => self.data = place,
            move_data::Store::Backups => {}
        }
    }
    /// Every store under one folder (`<dir>/postgres`, `/clickhouse`,
    /// `/files`): Setup's "One folder".
    pub fn under(dir: &str, clickhouse: bool) -> Self {
        let d = dir.trim_end_matches('/');
        Self {
            pg: Some(format!("{d}/postgres")),
            ch: clickhouse.then(|| format!("{d}/clickhouse")),
            data: Some(format!("{d}/files")),
        }
    }
}

/// The compose project an adopted stack was started from, as Docker labels
/// its containers.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ExternalCompose {
    pub project: String,
    pub files: Vec<String>,
    pub working_dir: String,
}

/// What operations that rewrite the compose file say about a stack started
/// from someone else's.
pub const EXTERNAL_COMPOSE: &str = "SENTIENT runs from a compose file the Platform Manager did not write. Let the Platform Manager take it over first (Repair and uninstall → Take over the compose file); the data stays.";

impl Default for DeployConfig {
    fn default() -> Self {
        Self {
            stack_name: "sentient".into(),
            image: "quakestring/sentient:latest".into(),
            flavor: Flavor::default(),
            license_server_url: "https://license.invenia.in".into(),
            db_name: "sentient".into(),
            db_user: "sentient".into(),
            db_password: "sentient".into(),
            pg_port: 5432,
            pg: PgTuning::default(),
            ch_db: "sentient".into(),
            ch_user: "sentient".into(),
            ch_password: "sentient".into(),
            ch_http_port: 8123,
            ch_native_port: 9000,
            ch: ChTuning::default(),
            http_port: 8080,
            mqtt_port: 1883,
            mqtts_port: 8883,
            coap_port: 5683,
            coaps_port: 5684,
            log_max_size: "100m".into(),
            log_max_file: 10,
            tuning: Tuning { instance_id: "sentient-docker-01".into(), ..Tuning::default() },
            load_demo: false,
            keep_timescale_image: false,
            external: None,
            wsl_distro: None,
            names: None,
            db_loopback: false,
            pgdata: None,
            locations: Locations::default(),
            old_copies: Vec::new(),
        }
    }
}

/// Container and volume names for one stack.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Names {
    pub app: String,
    pub pg: String,
    pub ch: String,
    pub vol_pg: String,
    pub vol_data: String,
    pub vol_ch: String,
}

impl DeployConfig {
    pub fn validate(&self) -> Result<(), String> {
        stack::validate_identifier("Stack name", &self.stack_name)
            .map_err(|_| "Stack name may only contain letters, digits and underscores.".to_string())?;
        if self.image.trim().is_empty() || self.image.contains(char::is_whitespace) {
            return Err("Image must be a Docker image reference like quakestring/sentient:latest.".into());
        }
        stack::validate_identifier("Database name", &self.db_name)?;
        stack::validate_identifier("Database user", &self.db_user)?;
        stack::validate_password("Database password", &self.db_password)?;
        if self.flavor.uses_clickhouse() {
            stack::validate_identifier("ClickHouse database", &self.ch_db)?;
            stack::validate_identifier("ClickHouse user", &self.ch_user)?;
            stack::validate_password("ClickHouse password", &self.ch_password)?;
        }
        let mut ports = vec![
            ("Web", self.http_port), ("MQTT", self.mqtt_port), ("MQTTS", self.mqtts_port),
            ("CoAP", self.coap_port), ("CoAPS", self.coaps_port), ("PostgreSQL", self.pg_port),
        ];
        if self.flavor.uses_clickhouse() {
            ports.push(("ClickHouse HTTP", self.ch_http_port));
            ports.push(("ClickHouse native", self.ch_native_port));
        }
        for (n, p) in &ports {
            if *p == 0 {
                return Err(format!("{n} port must be between 1 and 65535."));
            }
        }
        // UDP and TCP ports may legitimately coincide; the reference keeps
        // them all distinct and so does the check — simpler to reason about.
        let mut seen = std::collections::HashSet::new();
        for (n, p) in &ports {
            if !seen.insert(*p) {
                return Err(format!("Port {p} is used twice ({n}); every published port must be different."));
            }
        }
        for e in &self.tuning.extra_env {
            if e.name.is_empty() || !e.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Err(format!("Extra variable name '{}' is not a valid environment variable name.", e.name));
            }
        }
        Ok(())
    }

    pub fn names(&self) -> Names {
        if let Some(n) = &self.names {
            return n.clone();
        }
        if self.stack_name == "sentient" {
            Names {
                app: "sentient".into(),
                pg: "sentient-postgres".into(),
                ch: "sentient-clickhouse".into(),
                vol_pg: "st-pg-data".into(),
                vol_data: "st-data".into(),
                vol_ch: "st-ch-data".into(),
            }
        } else {
            let s = &self.stack_name;
            Names {
                app: s.clone(),
                pg: format!("{s}-postgres"),
                ch: format!("{s}-clickhouse"),
                vol_pg: format!("{s}-pg-data"),
                vol_data: format!("{s}-data"),
                vol_ch: format!("{s}-ch-data"),
            }
        }
    }

    /// Directory holding the compose file. Inside the WSL distro on Windows;
    /// on Linux `/opt/sentient` when this process can write there (root) and
    /// the user's data directory otherwise — the named volumes carry the
    /// data, so the location of the compose file is only a convenience.
    pub fn stack_dir(&self) -> String {
        let base = stack_base_dir();
        if self.stack_name == "sentient" { base } else { format!("{base}/{}", self.stack_name) }
    }
    pub fn compose_path(&self) -> String {
        format!("{}/docker-compose.yml", self.stack_dir())
    }
    pub fn ch_config_path(&self) -> String {
        format!("{}/clickhouse-sentient.xml", self.stack_dir())
    }
    fn compose_cmd(&self, sub: &str) -> String {
        match &self.external {
            Some(ext) => {
                // A file the stack was started with that is gone since (a
                // temporary override) is left out: compose refuses a missing
                // -f, and the project name finds the containers anyway.
                let files: String = ext.files.iter().filter(|f| host_file_exists(f)).map(|f| format!(" -f {}", shell_quote(f))).collect();
                let dir = if ext.working_dir.is_empty() { String::new() } else { format!(" --project-directory {}", shell_quote(&ext.working_dir)) };
                format!("docker compose -p {}{dir}{files} {sub}", shell_quote(&ext.project))
            }
            None => format!("docker compose -f {} {sub}", self.compose_path()),
        }
    }

    /// Compose files the adopted stack was started with that no longer
    /// exist (Status says so quietly).
    pub fn vanished_files(&self) -> Vec<String> {
        self.external.as_ref().map(|e| e.files.iter().filter(|f| !host_file_exists(f)).cloned().collect()).unwrap_or_default()
    }

    /// `start`, `stop` or `restart` without the compose files: by project
    /// name for an adopted stack, so a vanished file cannot stop it.
    fn control_cmd(&self, sub: &str) -> String {
        match &self.external {
            Some(ext) => format!("docker compose -p {} {sub}", shell_quote(&ext.project)),
            None => self.compose_cmd(sub),
        }
    }

    /// The SENTIENT container's name.
    pub fn app_container(&self) -> String {
        self.names().app
    }

    /// The stack's compose definition exists (SPM's own file, or the files
    /// an adopted stack runs from).
    fn stack_defined(&self) -> bool {
        self.external.is_some() || host_file_exists(&self.compose_path())
    }
}

#[cfg(windows)]
fn stack_base_dir() -> String {
    "/opt/sentient".into()
}

/// The Platform Manager's own PostgreSQL cluster directory (in its volume).
pub const PGDATA: &str = "/var/lib/postgresql/data";

#[cfg(not(windows))]
fn stack_base_dir() -> String {
    let opt = Path::new("/opt/sentient");
    let opt_ok = if opt.exists() {
        std::fs::metadata(opt).map(|m| !m.permissions().readonly()).unwrap_or(false)
            && std::fs::write(opt.join(".spm-write-test"), b"").map(|_| { let _ = std::fs::remove_file(opt.join(".spm-write-test")); true }).unwrap_or(false)
    } else {
        std::fs::create_dir_all(opt).is_ok()
    };
    if opt_ok {
        return "/opt/sentient".into();
    }
    let data_home = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{}/.local/share", std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())));
    format!("{data_home}/sentient")
}

// ---------------------------------------------------------------------------
// Compose generation
// ---------------------------------------------------------------------------

/// Double-quoted YAML scalar.
fn yq(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The environment the `sentient` container gets, in reference order.
fn app_env(cfg: &DeployConfig) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = vec![
        ("DATABASE_URL".into(), format!(
            "postgresql://{}:{}@postgres:5432/{}", cfg.db_user, cfg.db_password, cfg.db_name
        )),
        ("POSTGRES_DB".into(), cfg.db_name.clone()),
        ("POSTGRES_USER".into(), cfg.db_user.clone()),
        ("POSTGRES_PASSWORD".into(), cfg.db_password.clone()),
    ];
    let ch = ClickhouseConn {
        url: "http://clickhouse:8123".into(),
        database: cfg.ch_db.clone(),
        user: cfg.ch_user.clone(),
        password: cfg.ch_password.clone(),
    };
    env.extend(stack::storage_env(cfg.flavor, Some(&ch), &cfg.tuning));
    env.push(("LICENSE_SERVER_URL".into(), cfg.license_server_url.clone()));
    env.push(("REPORT_OUTPUT_DIR".into(), "/var/lib/sentient/reports".into()));
    env.push(("VC_REPOS_PATH".into(), "/var/lib/sentient/vc-repos".into()));
    env.extend(stack::tuning_env(&cfg.tuning));
    // Later entries win (an operator's extra variable replaces the value
    // the engine derived): a key written twice is invalid YAML.
    env.into_iter().fold(Vec::new(), |mut out: Vec<(String, String)>, (k, v)| {
        match out.iter_mut().find(|(name, _)| *name == k) {
            Some(slot) => slot.1 = v,
            None => out.push((k, v)),
        }
        out
    })
}

/// The stack as a self-contained compose file. Mirrors the reference flavour
/// files: same images, same PostgreSQL flags, same ClickHouse drop-in (as a
/// sibling file bind-mounted in — inline `configs` need Compose ≥ 2.23 and a
/// distribution's Docker can be older), same volume-name pinning.
/// The ClickHouse service, shared by the full stack and the ClickHouse-only
/// stack (Windows hybrid: SENTIENT and PostgreSQL native, ClickHouse here).
fn clickhouse_service(cfg: &DeployConfig) -> String {
    let n = cfg.names();
    let mut y = String::new();
    y.push_str(&format!("  clickhouse:\n    image: {}\n    container_name: {}\n    restart: unless-stopped\n", yq(CH_IMAGE), n.ch));
    y.push_str(&format!(
        "    environment:\n      CLICKHOUSE_DB: {}\n      CLICKHOUSE_USER: {}\n      CLICKHOUSE_PASSWORD: {}\n      CLICKHOUSE_DEFAULT_ACCESS_MANAGEMENT: \"1\"\n",
        yq(&cfg.ch_db), yq(&cfg.ch_user), yq(&cfg.ch_password)
    ));
    y.push_str("    ulimits:\n      nofile:\n        soft: 262144\n        hard: 262144\n");
    y.push_str(&format!(
        "    volumes:\n      - clickhouse_data:/var/lib/clickhouse\n      - {}\n",
        yq(&format!("{}:/etc/clickhouse-server/config.d/sentient.xml:ro", cfg.ch_config_path()))
    ));
    let bind = if cfg.db_loopback { "127.0.0.1:" } else { "" };
    y.push_str(&format!("    ports:\n      - \"{bind}{}:8123\"\n      - \"{bind}{}:9000\"\n", cfg.ch_http_port, cfg.ch_native_port));
    y.push_str("    healthcheck:\n      test: [\"CMD-SHELL\", \"wget -qO- http://127.0.0.1:8123/ping || exit 1\"]\n      interval: 10s\n      timeout: 5s\n      retries: 12\n\n");
    y
}

/// One entry of the top-level `volumes:`: the name pinned, and bound to a
/// host folder when the store has one (the name stays; only its place
/// changes).
fn volume_def(key: &str, name: &str, place: Option<&String>) -> String {
    let mut y = format!("  {key}:\n    name: {name}\n");
    if let Some(dir) = place {
        y.push_str(&format!("    driver: local\n    driver_opts:\n      type: none\n      o: bind\n      device: {}\n", yq(dir)));
    }
    y
}

/// A stack holding ClickHouse alone.
pub fn compose_clickhouse_only(cfg: &DeployConfig) -> String {
    let n = cfg.names();
    format!(
        "name: {}\n\nservices:\n{}volumes:\n{}",
        cfg.stack_name,
        clickhouse_service(cfg),
        volume_def("clickhouse_data", &n.vol_ch, cfg.locations.ch.as_ref())
    )
}

pub fn compose(cfg: &DeployConfig) -> String {
    let n = cfg.names();
    let mut y = String::new();
    y.push_str(&format!("name: {}\n\nservices:\n", cfg.stack_name));

    // ---- postgres ----
    let (pg_image, preload) = if cfg.flavor.uses_timescaledb() || cfg.keep_timescale_image {
        (PG_IMAGE_TIMESCALE, "timescaledb")
    } else {
        (PG_IMAGE_STOCK, "")
    };
    y.push_str(&format!("  postgres:\n    image: {}\n    container_name: {}\n    restart: always\n", yq(pg_image), n.pg));
    y.push_str("    command:\n      - \"postgres\"\n");
    y.push_str(&format!("      - \"-c\"\n      - \"shared_preload_libraries={preload}\"\n"));
    // PG 18 async I/O: io_uring needs a relaxed seccomp profile, so use the
    // portable worker method.
    y.push_str("      - \"-c\"\n      - \"io_method=worker\"\n      - \"-c\"\n      - \"io_workers=3\"\n");
    for (k, v) in cfg.pg.settings() {
        y.push_str(&format!("      - \"-c\"\n      - {}\n", yq(&format!("{k}={v}"))));
    }
    y.push_str(&format!(
        "    environment:\n      POSTGRES_DB: {}\n      POSTGRES_USER: {}\n      POSTGRES_PASSWORD: {}\n      PGDATA: {}\n",
        yq(&cfg.db_name), yq(&cfg.db_user), yq(&cfg.db_password), yq(cfg.pgdata.as_deref().unwrap_or(PGDATA))
    ));
    y.push_str("    volumes:\n      - postgres_data:/var/lib/postgresql\n");
    y.push_str(&format!("    ports:\n      - \"{}{}:5432\"\n", if cfg.db_loopback { "127.0.0.1:" } else { "" }, cfg.pg_port));
    y.push_str(&format!(
        "    healthcheck:\n      test: [\"CMD-SHELL\", \"pg_isready -U {} -d {}\"]\n      interval: 10s\n      timeout: 5s\n      retries: 12\n\n",
        cfg.db_user, cfg.db_name
    ));

    // ---- clickhouse ----
    if cfg.flavor.uses_clickhouse() {
        y.push_str(&clickhouse_service(cfg));
    }

    // ---- sentient ----
    y.push_str(&format!("  sentient:\n    image: {}\n    container_name: {}\n    restart: always\n", yq(&cfg.image), n.app));
    y.push_str("    extra_hosts:\n      - \"host.docker.internal:host-gateway\"\n");
    y.push_str(&format!(
        "    logging:\n      driver: \"json-file\"\n      options:\n        max-size: {}\n        max-file: {}\n",
        yq(&cfg.log_max_size), yq(&cfg.log_max_file.to_string())
    ));
    y.push_str("    depends_on:\n      postgres:\n        condition: service_healthy\n");
    if cfg.flavor.uses_clickhouse() {
        y.push_str("      clickhouse:\n        condition: service_healthy\n");
    }
    y.push_str("    environment:\n");
    for (k, v) in app_env(cfg) {
        y.push_str(&format!("      {k}: {}\n", yq(&v)));
    }
    y.push_str("    volumes:\n      - sentient_data:/var/lib/sentient\n");
    y.push_str(&format!(
        "    ports:\n      - \"{}:8080\"\n      - \"{}:1883\"\n      - \"{}:8883\"\n      - \"{}:5683/udp\"\n      - \"{}:5684/udp\"\n\n",
        cfg.http_port, cfg.mqtt_port, cfg.mqtts_port, cfg.coap_port, cfg.coaps_port
    ));

    // ---- volumes: names pinned so every invocation attaches the same data ----
    y.push_str("volumes:\n");
    y.push_str(&volume_def("postgres_data", &n.vol_pg, cfg.locations.pg.as_ref()));
    y.push_str(&volume_def("sentient_data", &n.vol_data, cfg.locations.data.as_ref()));
    if cfg.flavor.uses_clickhouse() {
        y.push_str(&volume_def("clickhouse_data", &n.vol_ch, cfg.locations.ch.as_ref()));
    }
    y
}

// ---------------------------------------------------------------------------
// Host shell — the one seam between Windows/WSL and Linux
// ---------------------------------------------------------------------------

// Which WSL distro Docker runs in (Windows). The Platform Manager's own
// (`sentient`) unless the installation was found in another one
// (DeployConfig::wsl_distro). Two layers: the installation record's, set
// whenever the record is read (`use_docker_distro`), and a per-thread
// override for work on a stack that is not the record's yet (looking in
// every distro, adding one) (`with_docker_distro`). Plain values behind a
// std lock, never held past the read.

static DOCKER_DISTRO: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

thread_local! {
    static SCOPED_DISTRO: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// The distro to run in: the thread's override, else the record's, else
/// the Platform Manager's own. Blank names count as none.
fn pick_distro(scoped: Option<&str>, recorded: Option<&str>) -> String {
    scoped
        .filter(|d| !d.trim().is_empty())
        .or(recorded.filter(|d| !d.trim().is_empty()))
        .unwrap_or(DISTRO)
        .trim()
        .to_string()
}

/// The WSL distro Docker commands run in now (Windows; unused on Linux).
pub fn docker_distro() -> String {
    let scoped = SCOPED_DISTRO.with(|s| s.borrow().clone());
    let recorded = DOCKER_DISTRO.read().map(|g| g.clone()).unwrap_or_else(|e| e.into_inner().clone());
    pick_distro(scoped.as_deref(), recorded.as_deref())
}

/// Whether `name` is the Platform Manager's own distro.
pub fn is_own_distro(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case(DISTRO)
}

/// The installation record's distro (its `stack.wsl_distro`): from now
/// on every Docker command of this process runs there. None: the Platform
/// Manager's own.
pub fn use_docker_distro(distro: Option<&str>) {
    let value = distro.filter(|d| !d.trim().is_empty()).map(|d| d.trim().to_string());
    match DOCKER_DISTRO.write() {
        Ok(mut g) => *g = value,
        Err(e) => *e.into_inner() = value,
    }
}

/// Run `f` with this thread's Docker commands in `distro` (None: the
/// Platform Manager's own), whatever the record says; the previous choice
/// comes back afterwards, also on a panic.
pub fn with_docker_distro<T>(distro: Option<&str>, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<String>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            SCOPED_DISTRO.with(|s| *s.borrow_mut() = previous);
        }
    }
    let wanted = pick_distro(distro, None);
    let _restore = Restore(SCOPED_DISTRO.with(|s| s.borrow_mut().replace(wanted)));
    f()
}

/// `wsl.exe` arguments that run `bash -lc <bash>` as root in `distro`.
#[cfg_attr(not(windows), allow(dead_code))]
fn wsl_bash_args(distro: &str, bash: &str) -> Vec<String> {
    ["-d", distro, "-u", "root", "--", "bash", "-lc", bash].iter().map(|s| s.to_string()).collect()
}

/// Program + args that run `bash` as the Docker-capable user: root in the
/// distro Docker runs in ([`docker_distro`]).
#[cfg(windows)]
fn shell_args(bash: &str) -> (String, Vec<String>) {
    ("wsl.exe".into(), wsl_bash_args(&docker_distro(), bash))
}

/// Program + arguments that run `argv` in `container` (`docker exec -i`),
/// where Docker runs: on Windows `wsl.exe -d <distro> -u root --exec`
/// (no shell in between, so nothing in `argv` is expanded; the distro's
/// shell expanded the `$` of an argument before, see
/// [`admin_account_cmd`]); on Linux [`shell_args`]' bash (or `sg docker`
/// right after setup), each argument quoted. Standard input reaches the
/// program in the container.
pub fn docker_exec_command(container: &str, argv: &[String]) -> (String, Vec<String>) {
    #[cfg(windows)]
    {
        docker_exec_args(true, &docker_distro(), false, container, argv)
    }
    #[cfg(not(windows))]
    {
        docker_exec_args(false, "", docker_needs_sg(), container, argv)
    }
}

/// [`docker_exec_command`] for `windows` or Linux, the distro and `sg`
/// given (unit tests build either).
pub fn docker_exec_args(windows: bool, distro: &str, sg: bool, container: &str, argv: &[String]) -> (String, Vec<String>) {
    if windows {
        let mut args: Vec<String> = ["-d", distro, "-u", "root", "--exec", "docker", "exec", "-i", container].iter().map(|s| s.to_string()).collect();
        args.extend(argv.iter().cloned());
        return ("wsl.exe".into(), args);
    }
    let mut line = format!("exec docker exec -i {}", shell_quote(container));
    for a in argv {
        line.push(' ');
        line.push_str(&shell_quote(a));
    }
    if sg {
        ("sg".into(), vec!["docker".into(), "-c".into(), line])
    } else {
        ("bash".into(), vec!["-lc".into(), line])
    }
}

/// On Linux: plain bash, or — right after `setup` added the user to the
/// `docker` group, before they have logged out and in — `sg docker`, which
/// applies the new group to just this command.
#[cfg(not(windows))]
fn shell_args(bash: &str) -> (String, Vec<String>) {
    if docker_needs_sg() {
        ("sg".into(), vec!["docker".into(), "-c".into(), bash.into()])
    } else {
        ("bash".into(), vec!["-lc".into(), bash.into()])
    }
}

#[cfg(not(windows))]
static DOCKER_ACCESS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// 0 = unknown, 1 = direct works, 2 = needs `sg docker`.
#[cfg(not(windows))]
fn docker_needs_sg() -> bool {
    use std::sync::atomic::Ordering;
    match DOCKER_ACCESS.load(Ordering::Relaxed) {
        1 => return false,
        2 => return true,
        _ => {}
    }
    let direct = sys::output("bash", &["-lc", "docker version >/dev/null 2>&1"]).map(|(ok, _, _)| ok).unwrap_or(false);
    let mode = if direct {
        1
    } else {
        let via_sg = sys::output("sg", &["docker", "-c", "docker version >/dev/null 2>&1"]).map(|(ok, _, _)| ok).unwrap_or(false);
        if via_sg { 2 } else { 1 }
    };
    DOCKER_ACCESS.store(mode, Ordering::Relaxed);
    mode == 2
}

#[cfg(not(windows))]
fn reset_docker_access() {
    DOCKER_ACCESS.store(0, std::sync::atomic::Ordering::Relaxed);
}

/// Run a bash command, capture its output, error with the real stderr.
#[cfg_attr(not(windows), allow(dead_code))]
fn run(sink: &ProgressFn, bash: &str) -> Result<(), String> {
    let (prog, args) = shell_args(bash);
    let argrefs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    match shell_output(&prog, &argrefs, RUN_LIMIT) {
        Some((ok, out, err)) => {
            exec::emit(sink, &out);
            exec::emit(sink, &err);
            if ok {
                Ok(())
            } else {
                let detail = sys::decode(&err);
                let detail = detail.trim();
                Err(if detail.is_empty() {
                    format!("command failed: {bash}")
                } else {
                    format!("{detail} (while running: {bash})")
                })
            }
        }
        None => Err(format!("could not run {prog}")),
    }
}

/// Like `run`, streaming output live — for long steps.
fn run_stream(sink: &ProgressFn, bash: &str) -> Result<(), String> {
    let (prog, args) = shell_args(bash);
    let argrefs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    if left_out(&argrefs) {
        return Err(format!("not run: SENTIENT's WSL distro is stopped ({bash})"));
    }
    exec::stream(sink, &prog, &argrefs).map_err(|e| if e == "Cancelled." { e } else { format!("command failed: {bash} ({e})") })
}

/// Quiet probe: (success, stdout).
fn run_output(bash: &str) -> Option<(bool, String)> {
    let (prog, args) = shell_args(bash);
    let argrefs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    shell_output(&prog, &argrefs, PROBE_LIMIT).map(|(ok, out, _)| (ok, sys::decode(&out)))
}

/// How long one command through the Docker shell may take before it is
/// stopped and reported (Windows: a WSL that does not answer must not
/// leave a job waiting for ever). Long steps stream (`run_stream`).
const RUN_LIMIT: std::time::Duration = std::time::Duration::from_secs(600);
/// The same for quick reads and checks.
pub(crate) const PROBE_LIMIT: std::time::Duration = std::time::Duration::from_secs(120);

/// Whether a command (`wsl.exe` arguments on Windows) is left out because
/// it is a read and its distro is stopped: `wsl.exe -d <distro>` starts a
/// stopped distro, and with it SENTIENT. Noted (idle::note_skipped), so a
/// watched read says "stopped". Every command that may run in SENTIENT's
/// distro asks this first (the test `every_distro_command_asks_left_out`).
/// Linux: never.
pub fn left_out(args: &[&str]) -> bool {
    if cfg!(windows) && idle::skip(idle::is_reading(), args, idle::running) {
        idle::note_skipped();
        return true;
    }
    false
}

/// A command through the Docker shell, stopped after `limit` on Windows.
pub(crate) fn shell_output(prog: &str, args: &[&str], limit: std::time::Duration) -> Option<(bool, Vec<u8>, Vec<u8>)> {
    if cfg!(windows) {
        // A read does not start a stopped distro (idle): it fails, as a
        // stopped stack's commands do.
        if left_out(args) {
            return None;
        }
        sys::output_wsl(prog, args, limit)
    } else {
        let _ = limit;
        sys::output(prog, args)
    }
}

/// Write `content` to `path` on the Docker host.
/// Write a file on the Docker host (in the WSL distro on Windows) readable
/// by others: a container's own config (ClickHouse reads its config.d as
/// its own user, not root), a script.
fn write_host_file(sink: &ProgressFn, path: &str, content: &str) -> Result<(), String> {
    write_host_file_as(sink, path, content, false)
}

/// [`write_host_file`] that only its owner reads: the compose file holds
/// the database passwords (audit I6); an existing file is made private too.
fn write_private_host_file(sink: &ProgressFn, path: &str, content: &str) -> Result<(), String> {
    write_host_file_as(sink, path, content, true)
}

fn write_host_file_as(sink: &ProgressFn, path: &str, content: &str, private: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        let dir = path.rsplit_once('/').map(|(d, _)| d).unwrap_or(".");
        if private {
            run(sink, &format!("mkdir -p {dir} && (umask 077; cat > {path} <<'SENTIENTEOF'\n{content}\nSENTIENTEOF\n) && chmod 600 {path}\n"))
        } else {
            run(sink, &format!("mkdir -p {dir} && cat > {path} <<'SENTIENTEOF'\n{content}\nSENTIENTEOF\nchmod 644 {path}\n"))
        }
    }
    #[cfg(not(windows))]
    {
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let _ = sink;
        if let Some(dir) = Path::new(path).parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        let mode = if private { 0o600 } else { 0o644 };
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(mode).open(path).map_err(|e| format!("write {path}: {e}"))?;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
        f.write_all(content.as_bytes()).map_err(|e| format!("write {path}: {e}"))
    }
}

fn host_file_exists(path: &str) -> bool {
    #[cfg(windows)]
    {
        run_output(&format!("test -f {path}")).map(|(ok, _)| ok).unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        Path::new(path).is_file()
    }
}

fn write_stack_files(sink: &ProgressFn, cfg: &DeployConfig) -> Result<(), String> {
    // A volume already bound to a folder keeps that folder, whatever the
    // record says (a record saved before a move finished, an older one):
    // compose would otherwise refuse the volume as changed.
    let synced = move_data::with_actual_locations(cfg);
    write_stack_files_exact(sink, &synced)
}

/// [`write_stack_files`] exactly as `cfg` says (the move engine, which
/// changes the volumes itself).
fn write_stack_files_exact(sink: &ProgressFn, cfg: &DeployConfig) -> Result<(), String> {
    if cfg.external.is_some() {
        return Err(EXTERNAL_COMPOSE.into());
    }
    write_private_host_file(sink, &cfg.compose_path(), &compose(cfg))?;
    if cfg.flavor.uses_clickhouse() {
        write_host_file(sink, &cfg.ch_config_path(), &stack::clickhouse_config_xml(&cfg.ch, None, None))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Readiness
// ---------------------------------------------------------------------------

/// Is Docker (and Compose) reachable where the stack will run?
pub fn is_ready() -> bool {
    #[cfg(windows)]
    {
        distro_listed(&docker_distro()) && run_output("docker version").map(|(ok, _)| ok).unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        run_output("docker info >/dev/null 2>&1 && docker compose version >/dev/null 2>&1")
            .map(|(ok, _)| ok)
            .unwrap_or(false)
    }
}

/// Is the SENTIENT web server answering on the published HTTP port?
pub fn is_running(http_port: u16) -> bool {
    #[cfg(windows)]
    {
        // curl inside the distro, judged by EXIT CODE, not stdout: a login
        // shell can print MOTD noise. curl exits 0 on any HTTP response and
        // non-zero when the connection is refused.
        let url = format!("http://localhost:{http_port}");
        let distro = docker_distro();
        // A read does not start a stopped distro (idle): nothing answers.
        if idle::is_reading() && !idle::running(&distro) {
            idle::note_skipped();
            return false;
        }
        let curl: [&str; 12] = ["-d", &distro, "-u", "root", "--", "curl", "-s", "-o", "/dev/null", "--max-time", "4", &url];
        if left_out(&curl) {
            return false;
        }
        let inside = sys::output_wsl("wsl.exe", &curl, std::time::Duration::from_secs(20))
            .map(|(ok, _, _)| ok)
            .unwrap_or(false);
        // The operator's distro may have no curl: WSL forwards the port to
        // Windows' localhost, so ask from here too.
        inside || (!is_own_distro(&distro) && crate::native::http_answers(http_port))
    }
    #[cfg(not(windows))]
    {
        crate::native::http_answers(http_port)
    }
}

// ---------------------------------------------------------------------------
// Deploy / cleanup / uninstall
// ---------------------------------------------------------------------------

macro_rules! bail_if_cancelled {
    () => {
        if crate::cancel::is_cancelled() {
            return Err("Cancelled.".into());
        }
    };
}

/// Bring up ClickHouse alone (Windows hybrid). The ports are published inside
/// the WSL distro; WSL2's localhost forwarding makes them reachable from
/// Windows as 127.0.0.1, which is where the native SENTIENT looks.
pub fn deploy_clickhouse(sink: ProgressFn, cfg: &DeployConfig) -> Result<(), String> {
    cfg.validate()?;
    if !is_ready() {
        return Err("Docker is not reachable. Run the Docker setup step first.".into());
    }
    sink(Progress::Step { name: "Writing the ClickHouse configuration".into() });
    write_private_host_file(&sink, &cfg.compose_path(), &compose_clickhouse_only(cfg))?;
    write_host_file(&sink, &cfg.ch_config_path(), &stack::clickhouse_config_xml(&cfg.ch, None, None))?;
    bail_if_cancelled!();
    sink(Progress::Step { name: "Pulling the ClickHouse image".into() });
    run_stream(&sink, &cfg.compose_cmd("pull"))?;
    bail_if_cancelled!();
    sink(Progress::Step { name: "Starting ClickHouse".into() });
    run_stream(&sink, &cfg.compose_cmd("up -d"))?;
    for i in 0..60 {
        bail_if_cancelled!();
        if clickhouse_answers(cfg.ch_http_port) {
            sink(Progress::Log { line: format!("ClickHouse answers on 127.0.0.1:{}", cfg.ch_http_port) });
            return Ok(());
        }
        sink(Progress::Log { line: format!("waiting for ClickHouse… ({}s)", i * 2) });
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    Err(format!("ClickHouse did not answer on 127.0.0.1:{} within two minutes", cfg.ch_http_port))
}

/// GET /ping from this side (Windows, through WSL2's localhost forwarding).
pub fn clickhouse_answers(http_port: u16) -> bool {
    ureq::get(&format!("http://127.0.0.1:{http_port}/ping"))
        .timeout(std::time::Duration::from_secs(3))
        .call()
        .map(|r| r.status() == 200)
        .unwrap_or(false)
}

/// Write the stack, pull images, install the schema, start, wait for HTTP.
pub fn deploy(sink: ProgressFn, cfg: &DeployConfig) -> Result<(), String> {
    deploy_with(sink, cfg, None)
}

/// [`deploy`], deciding first what happens to a database an earlier
/// install left in the volume: `Some(true)` keeps it (checked against this
/// mode, crate::kept_db), `Some(false)` sets it aside and starts fresh.
pub fn deploy_with(sink: ProgressFn, cfg: &DeployConfig, keep_existing: Option<bool>) -> Result<(), String> {
    cfg.validate()?;
    if !is_ready() {
        return Err("Docker is not reachable. Run the Docker setup step first.".into());
    }

    sink(Progress::Step { name: "Writing the SENTIENT configuration".into() });
    write_stack_files(&sink, cfg)?;
    sink(Progress::Log { line: format!("{}: {}", cfg.compose_path(), cfg.flavor.label()) });
    bail_if_cancelled!();

    sink(Progress::Step { name: "Pulling SENTIENT images (first time, a few minutes)".into() });
    pull_images(&sink, cfg)?;
    bail_if_cancelled!();

    // Data locations chosen at Setup: the folders the volumes are bound to.
    move_data::prepare_locations(&sink, cfg)?;
    bail_if_cancelled!();

    // The server validates the schema at boot and REFUSES to start on an
    // empty database (with `restart: always` it would crash-loop). Run the
    // one-time installer first: `compose run` brings the databases up via
    // depends_on, installs the schema and system resources, then exits. On a
    // re-run against an installed DB it exits non-zero ("refuses to overwrite
    // an existing schema"); expected, so log and carry on — the readiness
    // probe below is the real gate.
    if let Some(keep) = keep_existing {
        prepare_existing(&sink, cfg, keep)?;
        bail_if_cancelled!();
    }

    let demo = if cfg.load_demo { " -e LOAD_DEMO=true" } else { "" };
    sink(Progress::Step { name: "Installing the SENTIENT database (first run, one-time)".into() });
    if let Err(e) = run_stream(&sink, &cfg.compose_cmd(&format!("run --rm -e INSTALL_SENTIENT=true{demo} sentient"))) {
        bail_if_cancelled!();
        sink(Progress::Log { line: format!(
            "note: install step returned an error ({e}). If the database was already installed this is expected. Continuing."
        ) });
    }
    bail_if_cancelled!();

    sink(Progress::Step { name: "Starting SENTIENT".into() });
    run_stream(&sink, &cfg.compose_cmd("up -d"))?;

    let url = format!("http://localhost:{}", cfg.http_port);
    sink(Progress::Step { name: format!("Waiting for SENTIENT to become ready (its first answer on {url}, up to {} minutes)", FIRST_ANSWER_SECS / 60) });
    let started = std::time::Instant::now();
    let mut said = 0;
    loop {
        bail_if_cancelled!();
        if is_running(cfg.http_port) {
            sink(Progress::Done { message: format!("SENTIENT is running at {url}") });
            return Ok(());
        }
        let elapsed = started.elapsed().as_secs();
        if elapsed >= FIRST_ANSWER_SECS {
            break;
        }
        sink(Progress::Percent { value: elapsed as f32 / FIRST_ANSWER_SECS as f32 });
        // Every 15 s: how long, and what the container is doing.
        if elapsed >= said {
            said = elapsed + 15;
            let state = run_output(&format!("docker inspect -f '{{{{.State.Status}}}} {{{{.RestartCount}}}}' {} 2>/dev/null", cfg.names().app))
                .filter(|(ok, _)| *ok)
                .map(|(_, out)| out.trim().to_string());
            sink(Progress::Log { line: first_answer_line(elapsed, &url, state.as_deref()) });
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
    Err(format!(
        "SENTIENT did not answer on {url} within {} minutes. It may still be initializing the database: its log (Logs, container {}) says what it is doing.",
        FIRST_ANSWER_SECS / 60,
        cfg.names().app
    ))
}

/// How long a new install waits for SENTIENT's first answer: the first
/// start loads its system resources into a new database, which takes
/// minutes on a small VM (WSL 2 on a 4-core Windows 10 took over 6).
pub const FIRST_ANSWER_SECS: u64 = 600;

/// The wait's line in the log: elapsed time of the most, what it waits for,
/// and the container's state (`docker inspect`: "running 0").
pub fn first_answer_line(elapsed_secs: u64, url: &str, container: Option<&str>) -> String {
    let at = |s: u64| format!("{}:{:02}", s / 60, s % 60);
    let state = match container.and_then(|c| c.split_once(' ')) {
        Some((status, restarts)) if restarts != "0" => format!("; container {status}, restarted {restarts} times"),
        Some((status, _)) => format!("; container {status}"),
        None => String::new(),
    };
    format!("Waiting for SENTIENT's first answer on {url}: {} of up to {}{state}", at(elapsed_secs), at(FIRST_ANSWER_SECS))
}

/// Tear down a partial/cancelled deploy: containers, network, volumes, and
/// half-pulled images. Leaves Docker (and the WSL distro) in place so a retry
/// starts clean. Best-effort throughout — the stack may not exist yet.
/// The PostgreSQL volume an earlier install of this stack left, if any.
pub fn existing_volume(cfg: &DeployConfig) -> Option<String> {
    let volume = cfg.names().vol_pg;
    match run_output(&format!("docker volume inspect {} >/dev/null 2>&1", shell_quote(&volume))) {
        Some((true, _)) => Some(volume),
        _ => None,
    }
}

/// The kept-database step: only PostgreSQL up, nothing of SENTIENT yet.
fn prepare_existing(sink: &ProgressFn, cfg: &DeployConfig, keep: bool) -> Result<(), String> {
    sink(Progress::Step { name: "Checking the database an earlier install left".into() });
    run_stream(sink, &cfg.compose_cmd("up -d postgres"))?;
    let sql = |db: &str, q: &str| docker_ops::psql(cfg, db, q);
    let mut up = false;
    for _ in 0..60 {
        if sql("postgres", "SELECT 1").is_ok() {
            up = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    if !up {
        return Err("PostgreSQL did not start within two minutes.".into());
    }
    crate::kept_db::sync_role_password(&sql, &cfg.db_user, &cfg.db_password)?;
    let Some(existing) = crate::kept_db::inspect(&sql, &cfg.db_name)? else {
        return Ok(());
    };
    if keep {
        crate::kept_db::prepare_keep(&sql, &existing, cfg.flavor, crate::kept_db::TIMESCALE_SQL, sink)
    } else {
        crate::kept_db::set_aside(&sql, &cfg.db_name, sink)?;
        // The image creates its database only in an empty volume.
        sql(
            "postgres",
            &format!("CREATE DATABASE \"{}\" OWNER \"{}\"", cfg.db_name.replace('"', ""), cfg.db_user.replace('"', "")),
        )
        .map(|_| ())
        .map_err(|e| format!("create the new database: {e}"))
    }
}

pub fn cleanup(sink: ProgressFn, cfg: &DeployConfig) -> Result<(), String> {
    let n = cfg.names();
    sink(Progress::Step { name: "Stopping and removing SENTIENT containers and volumes".into() });
    if cfg.stack_defined() {
        let _ = run_stream(&sink, &format!("{} 2>/dev/null || true", cfg.compose_cmd("down -v --remove-orphans")));
    }
    // Belt-and-suspenders in case the containers exist without the compose file.
    let _ = run_stream(&sink, &format!("docker rm -f {} {} {} 2>/dev/null || true", n.app, n.pg, n.ch));
    // No `docker image prune`: it removes every untagged image on the host,
    // other projects' included, and a partial pull leaves no image behind.
    sink(Progress::Done { message: "Cleanup complete. You can safely retry the install.".into() });
    Ok(())
}

/// Remove SENTIENT but keep its data: the containers go, the volumes (the
/// databases and SENTIENT's files) and the compose file stay, so a new
/// install of the same stack name picks them up again.
///
/// Windows, the Platform Manager's own distro: it is stopped and stays
/// registered (the data is on its disk). A stack in the operator's own
/// distro (`cfg.wsl_distro`): its containers only, as on Linux; that
/// distro is never stopped.
pub fn remove_keeping_data(sink: ProgressFn, cfg: &DeployConfig) -> Result<(), String> {
    #[cfg(windows)]
    if cfg.wsl_distro.is_none() {
        // The data is on the distro's disk: stop it and leave it registered.
        sink(Progress::Step { name: "Stopping the SENTIENT WSL distro (its data is kept)".into() });
        remove_keepalive(&forwarded_ports(cfg));
        let _ = sys::output("wsl.exe", &["--terminate", DISTRO]);
        sink(Progress::Done { message: "SENTIENT was removed; its data volumes were kept.".into() });
        return Ok(());
    }
    with_docker_distro(cfg.wsl_distro.as_deref(), || {
        let n = cfg.names();
        sink(Progress::Step { name: "Removing the SENTIENT containers (volumes kept)".into() });
        if cfg.stack_defined() {
            let _ = run_stream(&sink, &format!("{} 2>/dev/null || true", cfg.compose_cmd("down --remove-orphans")));
        }
        let _ = run_stream(&sink, &format!("docker rm -f {} {} {} 2>/dev/null || true", n.app, n.pg, n.ch));
    });
    sink(Progress::Done { message: "SENTIENT was removed; its data volumes were kept.".into() });
    Ok(())
}

/// Remove SENTIENT entirely.
///
/// Windows, the Platform Manager's own distro: unregister it, which deletes
/// containers, volumes, images, Docker and the compose file in one go
/// (everything lives on the distro's disk). Linux, and a stack in the
/// operator's own WSL distro (`cfg.wsl_distro`, e.g. Ubuntu): take the
/// stack down with its volumes and images and remove the Platform
/// Manager's compose file; Docker and that distro stay — they are the
/// operator's. The operator's distro is never terminated or unregistered.
pub fn uninstall(sink: ProgressFn, cfg: &DeployConfig) -> Result<(), String> {
    #[cfg(windows)]
    if cfg.wsl_distro.is_none() {
        if distro_present() {
            sink(Progress::Step { name: "Removing the SENTIENT WSL distro and all its data".into() });
            remove_keepalive(&forwarded_ports(cfg));
            let _ = sys::output("wsl.exe", &["--terminate", DISTRO]);
            let _ = sys::output("wsl.exe", &["--unregister", DISTRO]);
        } else {
            remove_keepalive(&forwarded_ports(cfg));
            sink(Progress::Log { line: "No SENTIENT distro found. Nothing to remove.".into() });
        }
        sink(Progress::Done { message: "SENTIENT and its Docker environment were removed.".into() });
        return Ok(());
    }
    with_docker_distro(cfg.wsl_distro.as_deref(), || {
        let n = cfg.names();
        sink(Progress::Step { name: "Removing SENTIENT containers, volumes and images".into() });
        if cfg.stack_defined() {
            let _ = run_stream(&sink, &format!("{} 2>/dev/null || true", cfg.compose_cmd("down -v --remove-orphans --rmi all")));
        }
        let _ = run_stream(&sink, &format!("docker rm -f {} {} {} 2>/dev/null || true", n.app, n.pg, n.ch));
        let _ = run_stream(&sink, &format!("docker volume rm {} {} {} 2>/dev/null || true", n.vol_pg, n.vol_data, n.vol_ch));
        // An adopted stack's compose files are the operator's: they stay.
        if cfg.external.is_none() {
            remove_stack_files(cfg);
        }
    });
    let message = match &cfg.wsl_distro {
        Some(d) => format!("SENTIENT was removed. Docker and the WSL distro {d} were left as they are."),
        None => "SENTIENT was removed. Docker itself was left installed.".to_string(),
    };
    sink(Progress::Done { message });
    Ok(())
}

/// The Platform Manager's compose file and ClickHouse settings, and their
/// folder when it is then empty, on the Docker host.
fn remove_stack_files(cfg: &DeployConfig) {
    #[cfg(windows)]
    {
        let _ = run_output(&format!(
            "rm -f {} {}; rmdir {} 2>/dev/null; true",
            shell_quote(&cfg.compose_path()),
            shell_quote(&cfg.ch_config_path()),
            shell_quote(&cfg.stack_dir())
        ));
    }
    #[cfg(not(windows))]
    {
        let _ = std::fs::remove_file(cfg.compose_path());
        let _ = std::fs::remove_file(cfg.ch_config_path());
        let _ = std::fs::remove_dir(cfg.stack_dir()); // only if now empty
    }
}

// ---------------------------------------------------------------------------
// Setup — get to "Docker is reachable"
// ---------------------------------------------------------------------------

/// Windows: create the WSL distro and install Docker Engine inside it
/// (`install_dir` is where the distro's disk lives). Linux: install Docker
/// from the distribution when missing, start it, and let this user talk to
/// it. Idempotent — re-running repairs.
pub fn setup(sink: ProgressFn, install_dir: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        setup_windows(sink, install_dir)
    }
    #[cfg(not(windows))]
    {
        let _ = install_dir;
        setup_linux(sink)
    }
}

/// Set (to anything) for `setup` to take back a distro disk an earlier
/// install left in its folder, instead of setting it aside.
pub const REUSE_KEPT_DISK: &str = "SPM_REUSE_KEPT_DISK";

#[cfg(windows)]
fn setup_windows(sink: ProgressFn, install_dir: &Path) -> Result<(), String> {
    // Setup creates, resets and configures the Platform Manager's own
    // distro only: an installation in the operator's distro (Ubuntu, …)
    // has its Docker set up by the operator.
    let host = docker_distro();
    if !is_own_distro(&host) {
        return Err(format!(
            "SENTIENT runs in Docker in the WSL distro {host}, which the Platform Manager does not set up or repair. Check Docker there yourself (wsl -d {host} -- docker info)."
        ));
    }
    std::fs::create_dir_all(install_dir).map_err(|e| e.to_string())?;

    // A distro left half-imported by a cancelled or failed run makes every
    // later command fail cryptically. If one exists but doesn't answer,
    // unregister it so it re-imports clean.
    if distro_present() && !distro_healthy() {
        sink(Progress::Step { name: "Resetting an unresponsive SENTIENT distro".into() });
        let _ = sys::output("wsl.exe", &["--terminate", DISTRO]);
        let _ = sys::output("wsl.exe", &["--unregister", DISTRO]);
    }

    // The disk of a distro this account no longer has registered (an
    // earlier install, a recreated service account) blocks a new import in
    // the same folder ("install location already in use"). A reinstall over
    // kept data registers it again in place (its history comes back);
    // otherwise, or if that fails, it is set aside and the distro starts
    // empty.
    let leftover = install_dir.join("distro").join("ext4.vhdx");
    if !distro_present() && leftover.exists() {
        let reused = std::env::var_os(REUSE_KEPT_DISK).is_some() && {
            sink(Progress::Step { name: "Taking back the SENTIENT distro kept from the earlier install".into() });
            wsl_native(&sink, &["--import-in-place", DISTRO, &leftover.to_string_lossy()]).is_ok() && distro_present() && distro_healthy()
        };
        if !reused {
            let _ = sys::output("wsl.exe", &["--unregister", DISTRO]);
            let aside = leftover.with_extension(format!("vhdx.unused-{}", chrono::Local::now().format("%Y%m%d%H%M")));
            std::fs::rename(&leftover, &aside).map_err(|e| format!("set {} aside: {e}", leftover.display()))?;
            sink(Progress::Log { line: format!("The distro an earlier install left was kept as {}", aside.display()) });
        }
    }

    if !distro_present() {
        let rootfs = install_dir.join("ubuntu.rootfs.tar.gz");
        sink(Progress::Step { name: "Downloading Ubuntu base image (~350 MB)".into() });
        download(ROOTFS_URL, &rootfs, &sink)?;

        sink(Progress::Step { name: "Creating the SENTIENT WSL distro".into() });
        let distro_dir = install_dir.join("distro");
        std::fs::create_dir_all(&distro_dir).ok();
        let (dir, tar) = (distro_dir.to_string_lossy(), rootfs.to_string_lossy());
        let args: [&str; 6] = ["--import", DISTRO, &dir, &tar, "--version", "2"];
        let mut attempt = 0;
        loop {
            match wsl_native_output(&sink, &args) {
                Ok(()) => break,
                // The first start of WSL 2's VM right after the kernel was
                // installed can miss Windows' deadline: once, stop WSL and
                // import again instead of failing the install.
                Err((_, output)) if retry_after_vm_timeout(attempt, &output) => {
                    sink(Progress::Log { line: VM_TIMEOUT_RETRY_LINE.into() });
                    let _ = sys::output("wsl.exe", &["--shutdown"]);
                    if distro_present() {
                        let _ = sys::output("wsl.exe", &["--unregister", DISTRO]);
                    }
                    attempt += 1;
                }
                Err((e, _)) => return Err(e),
            }
        }
        let _ = std::fs::remove_file(&rootfs);
    }

    // Verify the distro actually runs a command before configuring it —
    // catches a broken import or a WSL that still needs a Windows restart.
    if let Err(e) = distro_probe_patient(&sink) {
        return Err(format!(
            "The SENTIENT WSL distro isn't responding ({e}). If you just installed WSL2, restart Windows and try again; otherwise use “Clean up leftovers” and retry."
        ));
    }

    sink(Progress::Step { name: "Enabling systemd".into() });
    // The distro is SENTIENT's alone: without Windows' PATH, a Docker
    // Desktop docker.exe there cannot pass for the distro's own Docker.
    run(&sink, r"printf '[boot]\nsystemd=true\n[interop]\nappendWindowsPath=false\n' > /etc/wsl.conf")?;
    // restart the distro so systemd becomes PID 1
    let _ = sys::output("wsl.exe", &["--terminate", DISTRO]);

    sink(Progress::Step { name: "Installing Docker Engine (a few minutes)".into() });
    run_stream(&sink, "[ -x /usr/bin/docker ] || (curl -fsSL https://get.docker.com | sh)")?;

    sink(Progress::Step { name: "Starting Docker".into() });
    // The inbox WSL 2 kernel (5.10) has no nf_tables NAT: Docker's
    // iptables (nf_tables by default) cannot add its rules there and
    // dockerd stops. The legacy iptables works on every WSL kernel.
    run(&sink, IPTABLES_LEGACY_IF_NEEDED)?;
    // Docker's networks out of WSL 2's range (ADDRESS_POOLS); the
    // installer may have started Docker already, so it starts again.
    run(&sink, &address_pools_cmd())?;
    // A WSL without systemd (Windows 10 builds that cannot run the WSL
    // package keep the inbox WSL): Docker's own init script.
    run(&sink, DOCKER_START)?;
    run(&sink, DOCKER_RESTART)?;

    // dockerd takes a few seconds to answer after it was started.
    for _ in 0..30 {
        if is_ready() {
            sink(Progress::Done { message: "Docker Engine is installed and running.".into() });
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    Err("Docker was installed but isn't responding yet. Try the step again.".into())
}

/// Restart Docker in the distro (systemd, or its init script).
pub const DOCKER_RESTART: &str = "if [ -d /run/systemd/system ]; then systemctl restart docker; else service docker restart; fi";

/// Docker's own address ranges in SENTIENT's distro. WSL 2 gives the
/// distro a new 172.16.0.0/12 subnet at every boot; Docker's default
/// networks come from that range too, so after a reboot the stack's
/// network could overlap the distro's own (seen on spm-win10-test: WSL
/// 172.19.128.0/20 against a compose network 172.19.0.0/16). The
/// container's answers to Windows then stayed on its bridge, and SENTIENT
/// answered inside the distro but not from Windows or the network.
pub const ADDRESS_POOLS: &str = r#"{"bip": "10.213.0.1/24", "default-address-pools": [{"base": "10.214.0.0/16", "size": 24}]}"#;

/// Writes ADDRESS_POOLS to /etc/docker/daemon.json unless it already sets
/// address pools; prints "changed" when it wrote it. Passed as base64: no
/// quotes to cross wsl.exe.
pub fn address_pools_cmd() -> String {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(format!("{ADDRESS_POOLS}\n"));
    format!("mkdir -p /etc/docker; if ! grep -qs default-address-pools /etc/docker/daemon.json; then echo {b64} | base64 -d > /etc/docker/daemon.json && echo changed; fi")
}

/// An existing install in SENTIENT's own distro (Windows): Docker's
/// networks moved out of WSL 2's range (ADDRESS_POOLS) when they are not
/// yet. The stack's containers and network are taken down (its volumes
/// stay), Docker restarts, and the caller brings the stack up again on a
/// new network. True when it changed something.
#[cfg(windows)]
pub fn pin_address_pools(sink: &ProgressFn, cfg: &DeployConfig) -> Result<bool, String> {
    if cfg.wsl_distro.is_some() || cfg.external.is_some() {
        return Ok(false);
    }
    let changed = run_output(&address_pools_cmd()).is_some_and(|(ok, out)| ok && out.contains("changed"));
    if !changed {
        return Ok(false);
    }
    sink(Progress::Log { line: "Docker's networks move out of WSL 2's address range (10.214.0.0/16)".into() });
    if cfg.stack_defined() {
        run_stream(sink, &cfg.compose_cmd("down --remove-orphans"))?;
    }
    run(sink, DOCKER_RESTART)?;
    for _ in 0..30 {
        if is_ready() {
            return Ok(true);
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    Err("Docker did not answer after its restart.".into())
}

/// Switch the distro to the legacy iptables when the kernel cannot do
/// what Docker asks of nf_tables (a NAT rule with addrtype), probed with
/// that very rule and removed again.
pub const IPTABLES_LEGACY_IF_NEEDED: &str = "if command -v iptables-legacy >/dev/null && ! { iptables-nft -t nat -A PREROUTING -m addrtype --dst-type LOCAL -j RETURN 2>/dev/null && iptables-nft -t nat -D PREROUTING -m addrtype --dst-type LOCAL -j RETURN; }; then update-alternatives --set iptables /usr/sbin/iptables-legacy && update-alternatives --set ip6tables /usr/sbin/ip6tables-legacy && echo 'This WSL kernel has no nf_tables NAT: Docker uses the legacy iptables'; fi";

/// Start Docker in the distro: systemd when WSL booted it, else Docker's
/// init script (the inbox WSL of older Windows 10 has no systemd).
pub const DOCKER_START: &str = "if [ -d /run/systemd/system ]; then systemctl enable --now docker; else service docker start; fi";

/// The scheduled task that keeps SENTIENT's distro (and its Docker) up.
pub const KEEPALIVE_TASK: &str = "SENTIENT Docker";

/// The TCP ports the keep-alive forwards from every address of this
/// computer to the distro (WSL 2 forwards a published port to Windows'
/// own localhost only). UDP (CoAP) cannot be forwarded this way.
pub fn forwarded_ports(cfg: &DeployConfig) -> Vec<u16> {
    let mut v = vec![cfg.http_port, cfg.mqtt_port, cfg.mqtts_port];
    v.retain(|p| *p > 0);
    v.dedup();
    v
}

/// What the keep-alive task runs: the distro started (its eth0 address
/// read, asked again for up to half a minute while the distro starts),
/// `ports` forwarded from every address of this computer to that address
/// (it changes at every boot), then Docker started and a process that
/// never ends, so WSL does not stop the idle distro. A start from the
/// Platform Manager writes the forward again once the distro is up
/// ([`port_forward::refresh`]). The wait is `ping`: `timeout` refuses to
/// run without a console, which a task has not.
pub fn keepalive_command(distro: &str, ports: &[u16]) -> String {
    let mut s = String::from("@echo off\r\nrem SENTIENT Docker keep-alive, written by the SENTIENT Platform Manager.\r\nset SENTIENT_IP=\r\nset SENTIENT_TRY=0\r\n:address\r\n");
    s.push_str(&format!("for /f \"tokens=4\" %%i in ('wsl.exe -d {distro} -u root -- ip -4 -o addr show eth0') do for /f \"delims=/\" %%j in (\"%%i\") do set SENTIENT_IP=%%j\r\n"));
    s.push_str(&format!("if not defined SENTIENT_IP for /f \"tokens=1\" %%i in ('wsl.exe -d {distro} -u root -- hostname -I') do set SENTIENT_IP=%%i\r\n"));
    s.push_str("if not defined SENTIENT_IP if %SENTIENT_TRY% lss 10 (set /a SENTIENT_TRY+=1 >nul & ping -n 4 127.0.0.1 >nul & goto address)\r\n");
    for p in ports {
        s.push_str(&format!("netsh interface portproxy delete v4tov4 listenport={p} listenaddress=0.0.0.0 >nul 2>&1\r\n"));
        s.push_str(&format!("if defined SENTIENT_IP netsh interface portproxy add v4tov4 listenport={p} listenaddress=0.0.0.0 connectport={p} connectaddress=%SENTIENT_IP% >nul\r\n"));
    }
    s.push_str(&format!("wsl.exe -d {distro} -u root -- sh -c \"{DOCKER_START}; {KEEPALIVE_HOLD}\"\r\n"));
    s
}

/// The process that holds the distro up, once: a second keep-alive (the
/// task run again while one runs) finds the lock taken and ends at once
/// instead of holding the distro a second time. `flock` is in every
/// Ubuntu (util-linux); without it the holder runs unguarded.
pub const KEEPALIVE_HOLD: &str = "if command -v flock >/dev/null; then exec flock -n /run/sentient-keepalive.lock sleep infinity; fi; exec sleep infinity";

/// The firewall rule that lets other computers reach a forwarded port
/// (no spaces: one netsh argument).
#[cfg_attr(not(windows), allow(dead_code))]
fn firewall_rule(port: u16) -> String {
    format!("name=SENTIENT-Docker-TCP-{port}")
}

/// This account as Windows names it (`computer\user`): over SSH
/// USERDOMAIN says WORKGROUP, which schtasks cannot map.
#[cfg(windows)]
fn this_account() -> String {
    sys::output("whoami.exe", &[])
        .map(|(_, out, _)| sys::decode(&out).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(sys::account_name)
}

/// The PowerShell that registers the keep-alive task running `cmd` as
/// `user` (S4U: no window, nobody needs to sign in). `start`: Some(delay)
/// starts it when Windows starts, `delay` seconds after (0: at once); None
/// gives it no trigger at all, so it runs only when the Platform Manager
/// starts SENTIENT ([`wake_keepalive`]). `start_now` runs it at once.
pub fn keepalive_task_script(cmd: &str, user: &str, start: Option<u32>, start_now: bool) -> String {
    let q = |s: &str| s.replace('\'', "''");
    let trigger = match start {
        Some(0) => "$r.Trigger = New-ScheduledTaskTrigger -AtStartup; ".to_string(),
        Some(d) => format!("$t = New-ScheduledTaskTrigger -AtStartup; $t.Delay = 'PT{d}S'; $r.Trigger = $t; "),
        None => String::new(),
    };
    let now = if start_now { format!("Start-ScheduledTask -TaskName '{KEEPALIVE_TASK}'") } else { String::new() };
    format!(
        "$ErrorActionPreference='Stop'; $r = @{{ TaskName = '{KEEPALIVE_TASK}'; Action = (New-ScheduledTaskAction -Execute '{cmd}'); Principal = (New-ScheduledTaskPrincipal -UserId '{user}' -LogonType S4U -RunLevel Highest); Settings = (New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries); Force = $true }}; {trigger}Register-ScheduledTask @r | Out-Null; {now}",
        cmd = q(cmd),
        user = q(user),
    )
}

/// The PowerShell that changes only when the keep-alive task starts
/// (`start` as in [`keepalive_task_script`]): its action, account and
/// settings are kept, and a running task keeps running (SENTIENT is not
/// stopped by changing how it starts).
pub fn keepalive_start_script(start: Option<u32>) -> String {
    let trigger = match start {
        Some(0) => "$r.Trigger = New-ScheduledTaskTrigger -AtStartup; ".to_string(),
        Some(d) => format!("$t = New-ScheduledTaskTrigger -AtStartup; $t.Delay = 'PT{d}S'; $r.Trigger = $t; "),
        None => String::new(),
    };
    format!(
        "$ErrorActionPreference='Stop'; $o = Get-ScheduledTask -TaskName '{KEEPALIVE_TASK}'; $was = $o.State -eq 'Running'; $r = @{{ TaskName = '{KEEPALIVE_TASK}'; Action = $o.Actions; Principal = $o.Principal; Settings = $o.Settings; Force = $true }}; {trigger}Register-ScheduledTask @r | Out-Null; if ($was -and (Get-ScheduledTask -TaskName '{KEEPALIVE_TASK}').State -ne 'Running') {{ Start-ScheduledTask -TaskName '{KEEPALIVE_TASK}' }}"
    )
}

/// How the keep-alive task starts, from its XML (`schtasks /query /xml`):
/// (with Windows, delay seconds). No boot trigger, or a disabled one: not
/// with Windows.
pub fn keepalive_start_from_xml(xml: &str) -> (bool, u32) {
    let Some(at) = xml.find("<BootTrigger") else { return (false, crate::native::DEFAULT_START_DELAY_SECS) };
    let body = &xml[at..];
    let body = &body[..body.find("</BootTrigger>").unwrap_or(body.len())];
    if body.contains("<Enabled>false</Enabled>") {
        return (false, crate::native::DEFAULT_START_DELAY_SECS);
    }
    (true, crate::native::task_delay_secs(body).unwrap_or(0))
}

#[cfg(windows)]
fn keepalive_cmd_path() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into())).join("SENTIENT").join("docker-keepalive.cmd")
}

#[cfg(windows)]
fn powershell(script: &str, what: &str) -> Result<(), String> {
    match sys::output_limited("powershell.exe", &["-NoProfile", "-NonInteractive", "-Command", script], std::time::Duration::from_secs(60)) {
        Some((true, _, _)) => Ok(()),
        Some((false, out, err)) => Err(format!("{what}: {} {}", sys::decode(&err).trim(), sys::decode(&out).trim())),
        None => Err("could not run powershell.exe".into()),
    }
}

/// Windows: keep SENTIENT's distro up, now and (with `start`: Some(delay))
/// from every boot, `delay` seconds after Windows starts; its TCP `ports`
/// reachable from other computers. Without `start` the task only runs
/// when SENTIENT is started (now, and from the Platform Manager).
#[cfg(windows)]
pub fn register_keepalive(ports: &[u16], start: Option<u32>) -> Result<(), String> {
    let cmd = keepalive_cmd_path();
    if let Some(dir) = cmd.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    std::fs::write(&cmd, keepalive_command(DISTRO, ports)).map_err(|e| format!("write {}: {e}", cmd.display()))?;
    for p in ports {
        let name = firewall_rule(*p);
        let port = format!("localport={p}");
        let _ = sys::output("netsh.exe", &["advfirewall", "firewall", "delete", "rule", &name]);
        let _ = sys::output("netsh.exe", &["advfirewall", "firewall", "add", "rule", &name, "dir=in", "action=allow", "protocol=TCP", &port]);
    }
    // S4U through the task scheduler's PowerShell module: `schtasks /np`
    // asks for the password anyway (and waits for ever without a console).
    powershell(&keepalive_task_script(&cmd.display().to_string(), &this_account(), start, true), &format!("could not register the task {KEEPALIVE_TASK}"))
}

/// Windows: whether SENTIENT's distro starts with Windows, and after how
/// many seconds (the keep-alive task's boot trigger). None: no task (a
/// stack in the operator's own distro, or none installed).
#[cfg(windows)]
pub fn keepalive_start() -> Option<(bool, u32)> {
    match sys::output("schtasks.exe", &["/query", "/tn", KEEPALIVE_TASK, "/xml"]) {
        Some((true, out, _)) => Some(keepalive_start_from_xml(&sys::decode(&out))),
        _ => None,
    }
}

/// Windows: start SENTIENT's distro with Windows after `delay_secs`, or
/// only when SENTIENT is started (`automatic` false: the task stays, without
/// its boot trigger). A running SENTIENT keeps running.
#[cfg(windows)]
pub fn set_keepalive_start(automatic: bool, delay_secs: u32) -> Result<(), String> {
    if keepalive_start().is_none() {
        return Err(format!("The task {KEEPALIVE_TASK} that starts SENTIENT is missing. Repair puts it back."));
    }
    // A busy computer can take more than a minute just to load the task
    // scheduler's module: a long limit, and once more when it passes.
    let script = keepalive_start_script(automatic.then_some(delay_secs));
    idle::twice("Changing SENTIENT's start at boot (scheduled task)", idle::BOOT_TASK_LIMIT, || {
        sys::output_limited("powershell.exe", &["-NoProfile", "-NonInteractive", "-Command", &script], idle::BOOT_TASK_LIMIT)
            .map(|(ok, out, err)| (ok, sys::decode(&out), sys::decode(&err)))
    })
}

/// Windows: before SENTIENT is started in its own distro, the keep-alive
/// task runs (it boots the distro, forwards the ports and holds the distro
/// up): when it does not start with Windows nothing else has run it since
/// the boot. Waits for Docker to answer. Without the task: nothing.
///
/// Then, also when the task already ran, the forwarded ports are pointed at
/// the distro's address of now ([`port_forward::wake_steps`]): the
/// distro's address changes with every start of WSL, and a forward to an
/// earlier one leaves SENTIENT unreachable from other computers while it
/// runs.
#[cfg(windows)]
pub fn wake_keepalive(sink: &ProgressFn, cfg: &DeployConfig) {
    use port_forward::{Task, WakeStep};
    if cfg.wsl_distro.is_some() || cfg.external.is_some() {
        return;
    }
    let task = match sys::output("schtasks.exe", &["/query", "/tn", KEEPALIVE_TASK, "/fo", "csv", "/nh"]) {
        Some((ok, out, _)) => port_forward::task_from_query(ok, &sys::decode(&out)),
        None => Task::Missing,
    };
    for step in port_forward::wake_steps(task) {
        match step {
            WakeStep::RunTask => {
                sink(Progress::Log { line: format!("Starting SENTIENT's WSL distro (task {KEEPALIVE_TASK})") });
                let _ = sys::output("schtasks.exe", &["/run", "/tn", KEEPALIVE_TASK]);
            }
            WakeStep::WaitDocker => {
                if !(0..60).any(|i| {
                    if i > 0 {
                        std::thread::sleep(std::time::Duration::from_secs(2));
                    }
                    is_ready()
                }) {
                    sink(Progress::Log { line: "Docker in SENTIENT's distro did not answer within 2 minutes; starting anyway".into() });
                }
            }
            WakeStep::ForwardPorts => {
                if let Err(e) = port_forward::refresh(sink, cfg) {
                    sink(Progress::Log { line: format!("The ports could not be forwarded to SENTIENT's distro: {e}. Other computers may not reach SENTIENT; Repair writes the forward again.") });
                }
            }
        }
    }
}

#[cfg(not(windows))]
pub fn wake_keepalive(_sink: &ProgressFn, _cfg: &DeployConfig) {}

/// Remove [`register_keepalive`]'s task and the command file it runs, end
/// what it runs, and remove its forwarding and firewall rules for `ports`.
#[cfg(windows)]
pub fn remove_keepalive(ports: &[u16]) {
    let _ = sys::output("schtasks.exe", &["/end", "/tn", KEEPALIVE_TASK]);
    let _ = sys::output("schtasks.exe", &["/delete", "/tn", KEEPALIVE_TASK, "/f"]);
    let _ = std::fs::remove_file(keepalive_cmd_path());
    for p in ports {
        let port = format!("listenport={p}");
        let _ = sys::output("netsh.exe", &["interface", "portproxy", "delete", "v4tov4", &port, "listenaddress=0.0.0.0"]);
        let _ = sys::output("netsh.exe", &["advfirewall", "firewall", "delete", "rule", &firewall_rule(*p)]);
    }
}

/// Linux: the distribution's Docker, via the package manager the preflight
/// found. One privileged script so polkit asks for the password once.
#[cfg(not(windows))]
fn setup_linux(sink: ProgressFn) -> Result<(), String> {
    reset_docker_access();
    if is_ready() {
        sink(Progress::Done { message: "Docker is installed and reachable.".into() });
        return Ok(());
    }
    let have_cli = exec::have("docker");
    let have_compose = run_output("docker compose version >/dev/null 2>&1").map(|(ok, _)| ok).unwrap_or(false);
    let user = std::env::var("USER").or_else(|_| std::env::var("LOGNAME")).unwrap_or_default();

    sink(Progress::Step { name: if have_cli && have_compose {
        "Starting Docker and granting this user access".into()
    } else {
        "Installing Docker from your distribution's packages".into()
    } });

    let install = if have_cli && have_compose {
        String::new()
    } else {
        // `--needed`/no-op semantics on every branch: installing an installed
        // package is fine, so one line covers "no docker" and "no compose".
        r#"
if command -v pacman >/dev/null 2>&1; then
  pacman -Sy --noconfirm --needed docker docker-compose
elif command -v apt-get >/dev/null 2>&1; then
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq
  apt-get install -y -qq docker.io docker-compose-v2 || apt-get install -y -qq docker.io docker-compose-plugin
elif command -v dnf >/dev/null 2>&1; then
  dnf install -y moby-engine docker-compose || dnf install -y docker docker-compose
elif command -v zypper >/dev/null 2>&1; then
  zypper --non-interactive install docker docker-compose
else
  echo "No supported package manager (apt, pacman, dnf, zypper). Install Docker Engine by hand and re-run." >&2
  exit 1
fi
"#.to_string()
    };
    let script = format!(
        "set -e\n{install}\nsystemctl enable --now docker\ngetent group docker >/dev/null || groupadd docker\n\
         if [ -n {u} ]; then usermod -aG docker {u}; fi\n\
         for i in $(seq 1 20); do docker info >/dev/null 2>&1 && break; sleep 1; done\n\
         docker info --format 'Docker {{{{.ServerVersion}}}} is running.'\n",
        u = exec::shq(&user)
    );
    exec::run_privileged(&sink, &script)?;

    reset_docker_access();
    if is_ready() {
        sink(Progress::Done { message: "Docker is installed and reachable.".into() });
        Ok(())
    } else {
        Err(format!(
            "Docker is installed but this user ({user}) still cannot reach it. Log out and back in so the \
             `docker` group membership applies, then run the install again."
        ))
    }
}

// ============================ manage (M3) ====================================

#[derive(serde::Serialize)]
pub struct ContainerStatus {
    pub name: String,
    pub state: String,  // running | stopped
    pub status: String, // human ("Up 3 minutes", "Exited (0) 1 min ago")
}

#[derive(serde::Serialize)]
pub struct StackStatus {
    pub installed: bool, // Docker reachable AND a compose file written
    pub running: bool,   // SENTIENT answers where other computers reach it (distro::answers)
    pub containers: Vec<ContainerStatus>,
}

/// Snapshot of the deployed stack for the Status view. `http_port` doubles as
/// the "is it actually serving?" probe.
pub fn status(cfg: &DeployConfig) -> StackStatus {
    let n = cfg.names();
    #[cfg(windows)]
    let present = distro_listed(&docker_distro());
    #[cfg(not(windows))]
    let present = exec::have("docker");

    // Its distro stopped: SENTIENT is stopped, and asking the distro
    // anything would start it (idle).
    #[cfg(windows)]
    if present && !idle::running(&docker_distro()) {
        return stopped_distro_status(cfg);
    }
    let installed = present && cfg.stack_defined();
    let containers = if present {
        // Only `.Status` — universally supported, unlike `.State` and
        // `--filter` — and derive the state from it ("Up …" == running).
        containers_from_ps(cfg, run_output("docker ps -a --format '{{.Names}}|{{.Status}}'"))
    } else {
        Vec::new()
    };
    // Running is answering, where other computers reach it (Windows:
    // through the port forward): a container that is up may still be
    // starting, or not reachable. A stopped app container is not asked.
    let app_down = containers.iter().any(|c| c.name == n.app) && !containers.iter().any(|c| c.name == n.app && c.state == "running");
    let running = installed && !app_down && answers(cfg);
    StackStatus { installed, running, containers }
}

/// The stack's containers from `docker ps -a` (`name|status` lines). Docker
/// not answering (not started yet: on Windows its distro just booted, or
/// the daemon is down): each of the stack's containers stopped, never an
/// empty list ("0 of 0 running").
pub fn containers_from_ps(cfg: &DeployConfig, ps: Option<(bool, String)>) -> Vec<ContainerStatus> {
    let n = cfg.names();
    match ps {
        Some((true, out)) => out
            .lines()
            .filter_map(|line| line.trim().split_once('|'))
            .filter(|(name, _)| *name == n.app || *name == n.pg || *name == n.ch)
            .map(|(name, status)| ContainerStatus { name: name.to_string(), state: if status.starts_with("Up") { "running" } else { "stopped" }.to_string(), status: status.to_string() })
            .collect(),
        _ => stack_names(cfg).into_iter().map(|name| ContainerStatus { name, state: "stopped".into(), status: "Not running (Docker is not running)".into() }).collect(),
    }
}

/// The stack's own containers: SENTIENT, PostgreSQL, and ClickHouse when
/// it is the hybrid.
fn stack_names(cfg: &DeployConfig) -> Vec<String> {
    let n = cfg.names();
    let mut names = vec![n.app, n.pg];
    if cfg.flavor == Flavor::Clickhouse {
        names.push(n.ch);
    }
    names
}

/// [`status`] while the stack's WSL distro is stopped: installed (its
/// compose file cannot be looked at without starting the distro; the
/// installation record says it is there), not running, each container
/// stopped.
#[cfg_attr(not(windows), allow(dead_code))]
fn stopped_distro_status(cfg: &DeployConfig) -> StackStatus {
    let containers = stack_names(cfg).into_iter().map(|name| ContainerStatus { name, state: "stopped".into(), status: "Not running (its WSL distro is stopped)".into() }).collect();
    StackStatus { installed: true, running: false, containers }
}

/// Replace the default sys-admin sign-in inside the stack's PostgreSQL.
pub fn set_admin_account(cfg: &DeployConfig, email: &str, password_hash: &str) -> Result<(), String> {
    let n = cfg.names();
    let sql = crate::native::admin_account_sql(email, password_hash);
    let cmd = admin_account_cmd(&n.pg, &cfg.db_user, &cfg.db_name, &sql);
    match run_output(&cmd) {
        Some((true, _)) => Ok(()),
        _ => Err("could not set the administrator sign-in in the database container".into()),
    }
}

/// Single-quote for bash, which runs every Docker command here.
/// The command that runs `sql` in the database container. The SQL goes
/// in as base64 on standard input: on Windows `wsl.exe -- bash -lc …`
/// runs through the distro's shell first, which expanded the `$` of the
/// argon2 hash ("$argon2id$v=19$…") and left SENTIENT a hash it could not
/// read (every sign-in answered 500).
fn admin_account_cmd(container: &str, user: &str, db: &str, sql: &str) -> String {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(sql);
    format!("echo {b64} | base64 -d | docker exec -i {container} psql -U {user} -d {db} -v ON_ERROR_STOP=1 -qAt -f -")
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Start / stop / restart the deployed stack.
pub fn control(sink: ProgressFn, cfg: &DeployConfig, action: &str) -> Result<(), String> {
    let external = cfg.external.is_some();
    let (label, sub) = match action {
        // An adopted stack starts its own containers again; SPM's own stack
        // is brought up from its file.
        "start" if external => ("Starting SENTIENT", "start"),
        "start" => ("Starting SENTIENT", "up -d"),
        "stop" => ("Stopping SENTIENT", "stop"),
        "restart" => ("Restarting SENTIENT", "restart"),
        _ => return Err(format!("unknown action: {action}")),
    };
    sink(Progress::Step { name: label.into() });
    if action != "stop" {
        wake_keepalive(&sink, cfg);
    }
    run_stream(&sink, &cfg.control_cmd(sub))?;
    sink(Progress::Done { message: format!("{label}: done.") });
    Ok(())
}

/// One step of [`control_stack`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackStep {
    /// Bring the distro up and forward the ports to it ([`wake_keepalive`]).
    Wake,
    /// A compose command over the whole project.
    Compose(&'static str),
    /// Wait until SENTIENT answers where other computers reach it
    /// ([`answers`]).
    WaitAnswer,
}

/// [`control_stack`]'s steps for `action`, in order: a start brings the
/// distro up and points the forwarded ports at it first, and is reported
/// done only once SENTIENT answers.
pub fn stack_steps(action: &str, external: bool) -> Result<Vec<StackStep>, String> {
    let subs = crate::stack_order::docker_commands(action, external)?;
    let starts = action != "stop";
    let mut v = Vec::new();
    if starts {
        v.push(StackStep::Wake);
    }
    v.extend(subs.into_iter().map(StackStep::Compose));
    if starts {
        v.push(StackStep::WaitAnswer);
    }
    Ok(v)
}

/// Whether SENTIENT answers on its web port the way other computers reach
/// it. Windows, SENTIENT's own distro: on this computer's network address,
/// through the port forward (Windows' own localhost can be served by WSL's
/// relay and answer while the forward points at an earlier address of the
/// distro). Elsewhere [`is_running`].
pub fn answers(cfg: &DeployConfig) -> bool {
    if cfg!(windows) && cfg.wsl_distro.is_none() && cfg.external.is_none() {
        port_forward::answers_from_outside(cfg.http_port)
    } else {
        is_running(cfg.http_port)
    }
}

/// Start, stop or restart the whole stack: compose over all of the
/// project's services, which it orders by their dependencies
/// (crate::stack_order::docker_commands); a start waits for SENTIENT to
/// answer ([`stack_steps`]).
pub fn control_stack(sink: ProgressFn, cfg: &DeployConfig, action: &str) -> Result<(), String> {
    use crate::stack_order::{wait_until, SENTIENT_READY_SECS};
    for step in stack_steps(action, cfg.external.is_some())? {
        match step {
            StackStep::Wake => wake_keepalive(&sink, cfg),
            StackStep::Compose(sub) => {
                sink(Progress::Step { name: if sub == "stop" { "Stopping SENTIENT and its databases".into() } else { "Starting the databases, then SENTIENT".into() } });
                run_stream(&sink, &cfg.control_cmd(sub))?;
            }
            StackStep::WaitAnswer => {
                sink(Progress::Step { name: "Waiting for SENTIENT to answer".into() });
                if !wait_until(SENTIENT_READY_SECS, || answers(cfg)) {
                    // Inside the distro it answers: only the way in is missing.
                    if cfg!(windows) && is_running(cfg.http_port) {
                        return Err(format!(
                            "SENTIENT runs, but other computers cannot reach it on port {}: the forward from Windows to SENTIENT's WSL distro does not answer. Start it again, or run Repair.",
                            cfg.http_port
                        ));
                    }
                    return Err(format!(
                        "SENTIENT was started but does not answer on port {} after {} minutes. Its log says why.",
                        cfg.http_port,
                        SENTIENT_READY_SECS / 60
                    ));
                }
            }
        }
    }
    sink(Progress::Done { message: crate::stack_order::done_message(action, &[]) });
    Ok(())
}

/// Start, stop or restart one container of the stack. `container` must come
/// from the stack's own list (the caller checks it).
pub fn service_control(sink: ProgressFn, container: &str, action: &str) -> Result<(), String> {
    if !matches!(action, "start" | "stop" | "restart") {
        return Err(format!("unknown action: {action}"));
    }
    sink(Progress::Step { name: format!("{action} {container}") });
    run_stream(&sink, &format!("docker {action} {}", shell_quote(container)))?;
    sink(Progress::Done { message: format!("{container}: {action} done.") });
    Ok(())
}

/// What separates `docker stats`' lines from `docker inspect`'s in
/// [`container_stats_cmd`]'s output.
const STATS_SEP: &str = "--sentient-restarts--";

/// One command, where Docker runs, for each container's processor and
/// memory (`docker stats` lines `name|cpu|memory`) and restarts (`docker
/// inspect` lines `/name|count`), [`STATS_SEP`] between them.
pub fn container_stats_cmd(containers: &[String]) -> String {
    let names = containers.iter().map(|c| shell_quote(c)).collect::<Vec<_>>().join(" ");
    format!(
        "docker stats --no-stream --format '{{{{.Name}}}}|{{{{.CPUPerc}}}}|{{{{.MemUsage}}}}' {names} 2>/dev/null; echo {STATS_SEP}; docker inspect --format '{{{{.Name}}}}|{{{{.RestartCount}}}}' {names} 2>/dev/null"
    )
}

/// Each container's `docker stats` and `docker inspect` lines (see
/// [`container_stats_cmd`]), read where Docker runs: on Windows inside
/// SENTIENT's WSL distro (there is no `docker` on Windows itself), on Linux
/// through Docker's shell. A read leaves a stopped distro stopped: None,
/// and the left-out command is noted (idle::note_skipped).
pub fn container_stats(containers: &[String]) -> Option<(String, String)> {
    if containers.is_empty() {
        return Some((String::new(), String::new()));
    }
    let (_, out) = run_output(&container_stats_cmd(containers))?;
    Some(split_container_stats(&out))
}

/// [`container_stats`]' output, as its two parts.
pub fn split_container_stats(out: &str) -> (String, String) {
    match out.split_once(STATS_SEP) {
        Some((stats, restarts)) => (stats.trim().to_string(), restarts.trim().to_string()),
        None => (out.trim().to_string(), String::new()),
    }
}

/// One container's `docker logs --timestamps` (its last `tail` lines, or
/// the last 24 hours with `tail` None), stdout and stderr together, read
/// where Docker runs: on Windows inside SENTIENT's WSL distro (there is no
/// `docker` on Windows itself; the Logs page read the host's and said
/// "Could not read", rc6 build 3 VM), on Linux through Docker's shell.
/// None: it could not be read (a stopped distro is left stopped).
pub fn container_logs(container: &str, tail: Option<u32>) -> Option<String> {
    run_output(&container_logs_cmd(container, tail)).map(|(_, out)| out)
}

/// [`container_logs`]' command.
pub fn container_logs_cmd(container: &str, tail: Option<u32>) -> String {
    let range = match tail {
        Some(n) => format!("--tail {n}"),
        None => "--since 24h".into(),
    };
    format!("docker logs --timestamps {range} {} 2>&1", shell_quote(container))
}

/// The last `tail` lines of the combined container logs.
pub fn logs(cfg: &DeployConfig, tail: u32) -> String {
    run_output(&cfg.compose_cmd(&format!("logs --no-color --tail {tail} 2>&1")))
        .map(|(_, out)| out)
        .unwrap_or_else(|| "Could not read logs.".into())
}

/// Update: pull the latest images, apply DB migrations, restart. Follows the
/// reference upgrade flow (stop server → migrate → start) so a running server
/// never races the migration.
pub fn update(sink: ProgressFn, cfg: &DeployConfig) -> Result<(), String> {
    sink(Progress::Step { name: "Pulling the latest SENTIENT images".into() });
    pull_images(&sink, cfg)?;
    bail_if_cancelled!();

    sink(Progress::Step { name: "Stopping the server for migration".into() });
    let _ = run_stream(&sink, &cfg.compose_cmd("stop sentient"));
    bail_if_cancelled!();

    // Refuses (non-zero) if the jump has breaking changes — surface that.
    sink(Progress::Step { name: "Applying database migrations (if any)".into() });
    if let Err(e) = run_stream(&sink, &cfg.compose_cmd("run --rm -e UPGRADE_SENTIENT=true sentient")) {
        bail_if_cancelled!();
        return Err(format!(
            "Migration step failed: {e}. If it reported breaking changes, review the log and run the upgrade manually."
        ));
    }
    bail_if_cancelled!();

    sink(Progress::Step { name: "Starting SENTIENT on the new image".into() });
    run_stream(&sink, &cfg.compose_cmd("up -d"))?;
    sink(Progress::Done { message: "SENTIENT updated to the latest image.".into() });
    Ok(())
}

// ---- helpers ------------------------------------------------------------------

/// Run a trivial command in the distro; returns the stderr on failure. The
/// exact `bash -lc` path the real steps use, so it catches a broken import
/// *and* a login shell that won't start.
#[cfg(windows)]
fn distro_probe() -> Result<(), String> {
    let probe = ["-d", DISTRO, "-u", "root", "--", "bash", "-lc", "echo ok"];
    if left_out(&probe) {
        return Err("SENTIENT's WSL distro is stopped".into());
    }
    match sys::output("wsl.exe", &probe) {
        Some((true, out, _)) if sys::decode(&out).contains("ok") => Ok(()),
        // The inbox wsl.exe prints its errors on standard output.
        Some((_, out, err)) => {
            let e = format!("{} {}", sys::decode(&err).trim(), sys::decode(&out).trim());
            let e = e.trim();
            Err(if e.is_empty() { "no response".into() } else { e.to_string() })
        }
        None => Err("could not run wsl.exe".into()),
    }
}

/// [`distro_probe`] for a distro just imported: its first start (WSL's
/// virtual machine starting too, on an older Windows 10 the first time
/// after the kernel was installed) can fail once, so it is asked again
/// for up to a minute.
#[cfg(windows)]
fn distro_probe_patient(sink: &ProgressFn) -> Result<(), String> {
    let mut last = String::new();
    let mut restarted = 0;
    for attempt in 0..12 {
        match distro_probe() {
            Ok(()) => return Ok(()),
            Err(e) => {
                if attempt == 0 {
                    sink(Progress::Log { line: format!("The new distro did not answer yet ({e}); asking again") });
                }
                // Its first start missed WSL's deadline: once, stop WSL so
                // the next ask starts the VM afresh.
                if retry_after_vm_timeout(restarted, &e) {
                    sink(Progress::Log { line: VM_TIMEOUT_RETRY_LINE.into() });
                    let _ = sys::output("wsl.exe", &["--shutdown"]);
                    restarted += 1;
                }
                last = e;
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
    Err(last)
}

#[cfg(windows)]
fn distro_healthy() -> bool {
    distro_probe().is_ok()
}

#[cfg(windows)]
/// The SENTIENT distribution is registered for the current Windows user.
pub fn is_registered() -> bool {
    cfg!(windows) && distro_present()
}

/// The Platform Manager's own distro is registered (its lifecycle: setup,
/// uninstall, the ClickHouse agent).
#[cfg_attr(not(windows), allow(dead_code))]
fn distro_present() -> bool {
    distro_listed(DISTRO)
}

/// A WSL distro of this name is registered for this Windows account.
#[cfg_attr(not(windows), allow(dead_code))]
fn distro_listed(name: &str) -> bool {
    sys::output("wsl.exe", &["-l", "-q"])
        .map(|(_, out, _)| hosts::parse_list_quiet(&sys::decode(&out)).iter().any(|l| l.eq_ignore_ascii_case(name.trim())))
        .unwrap_or(false)
}

/// Run a native `wsl.exe` command (UTF-16 output), stream it, error on failure.
#[cfg(windows)]
fn wsl_native(sink: &ProgressFn, args: &[&str]) -> Result<(), String> {
    wsl_native_output(sink, args).map_err(|(e, _)| e)
}

/// [`wsl_native`], with the command's output on failure (to tell why).
#[cfg(windows)]
fn wsl_native_output(sink: &ProgressFn, args: &[&str]) -> Result<(), (String, String)> {
    match sys::output_tracked("wsl.exe", args) {
        Some((ok, out, err)) => {
            exec::emit(sink, &out);
            exec::emit(sink, &err);
            if ok {
                Ok(())
            } else {
                Err((format!("wsl {} failed", args.join(" ")), format!("{}\n{}", sys::decode(&out), sys::decode(&err))))
            }
        }
        None => Err(("could not run wsl.exe".into(), String::new())),
    }
}

/// The log line of [`retry_after_vm_timeout`]'s one retry.
#[cfg_attr(not(windows), allow(dead_code))]
const VM_TIMEOUT_RETRY_LINE: &str =
    "WSL 2's virtual machine did not answer in time (its first start after the kernel install can be slow); stopping WSL and trying once more";

/// WSL's own words for its VM not answering in time (HCS_E_CONNECTION_TIMEOUT).
pub(crate) fn is_vm_timeout(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    lower.contains("a response was not received from the virtual machine or container")
        || lower.contains("hcs_e_connection_timeout")
        || lower.contains("0x80370109")
}

/// Whether a failed import or first start is tried again: once (`retries`
/// done so far is 0), and only when WSL's VM did not answer in time.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn retry_after_vm_timeout(retries: u32, output: &str) -> bool {
    retries == 0 && is_vm_timeout(output)
}

/// Parse a Docker size token like "5.243MB", "1.049kB", "128B" into bytes.
fn parse_size(s: &str) -> Option<f64> {
    let s = s.trim();
    let (num, mult) = if let Some(n) = s.strip_suffix("GB") { (n, 1e9) }
        else if let Some(n) = s.strip_suffix("MB") { (n, 1e6) }
        else if let Some(n) = s.strip_suffix("kB") { (n, 1e3) }
        else if let Some(n) = s.strip_suffix("KB") { (n, 1e3) }
        else if let Some(n) = s.strip_suffix("B") { (n, 1.0) }
        else { return None };
    num.trim().parse::<f64>().ok().map(|v| v * mult)
}

/// `docker compose pull` with a *clean* summary instead of the per-layer
/// firehose: aggregates bytes across all layers and emits one line/second —
/// "Downloaded 342 MB  (12.4 MB/s)" — plus a "✓ <image> pulled" per image.
///
/// `--ignore-pull-failures` so a SENTIENT image that only exists locally
/// (side-loaded with `docker load` on an offline site) does not abort the
/// deploy; a genuinely missing image still fails at `up`.
fn pull_images(sink: &ProgressFn, cfg: &DeployConfig) -> Result<(), String> {
    use std::collections::HashMap;
    use std::io::BufRead;
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let (prog, args) = shell_args(&cfg.compose_cmd("pull --ignore-pull-failures"));
    if left_out(&args.iter().map(String::as_str).collect::<Vec<_>>()) {
        return Err("SENTIENT's WSL distro is stopped".into());
    }
    let mut child = sys::command(&prog)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    crate::cancel::register_pid(child.id());
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");

    let (tx, rx) = mpsc::channel::<String>();
    let tx2 = tx.clone();
    let t1 = std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let t2 = std::thread::spawn(move || {
        for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = tx2.send(line);
        }
    });

    let mut layers: HashMap<String, (f64, f64)> = HashMap::new(); // id -> (done, total)
    let mut last_emit = Instant::now();
    let mut last_bytes = 0.0f64;
    let start = Instant::now();
    let mut pulled: Vec<String> = Vec::new();

    let flush = |sink: &ProgressFn, layers: &HashMap<String, (f64, f64)>, last_bytes: &mut f64, elapsed: f64| {
        let done: f64 = layers.values().map(|(d, _)| *d).sum();
        let total: f64 = layers.values().map(|(_, t)| *t).sum();
        let rate = (done - *last_bytes).max(0.0) / elapsed.max(0.001);
        *last_bytes = done;
        let mb = |b: f64| b / 1e6;
        let line = if total > 0.0 {
            format!("Downloaded {:.0} / {:.0} MB  ({:.1} MB/s)", mb(done), mb(total), mb(rate))
        } else {
            format!("Downloaded {:.0} MB  ({:.1} MB/s)", mb(done), mb(rate))
        };
        sink(Progress::Log { line });
        if total > 0.0 {
            sink(Progress::Percent { value: (done / total) as f32 });
        }
    };

    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(line) => {
                let l = line.trim();
                let lower = l.to_ascii_lowercase();
                if l.contains("Pulled") {
                    // "✔ postgres Pulled" / " sentient Pulled"
                    let name = l.split_whitespace().find(|w| !w.starts_with('✔') && !w.starts_with('[') && *w != "Pulled").unwrap_or("image");
                    if !pulled.iter().any(|p| p == name) {
                        pulled.push(name.to_string());
                        sink(Progress::Log { line: format!("✓ {name} pulled") });
                    }
                } else if lower.contains("error") || lower.contains("failed") || lower.contains("warning") {
                    sink(Progress::Log { line: l.to_string() });
                } else if let Some(idx) = l.find("Downloading") {
                    // "<id> Downloading [====>   ]  5.243MB/28.31MB"
                    let id = l[..idx].split_whitespace().last().unwrap_or("").to_string();
                    if let Some(frac) = l.rsplit(']').next() {
                        if let Some((d, t)) = frac.trim().split_once('/') {
                            if let (Some(d), Some(t)) = (parse_size(d), parse_size(t)) {
                                layers.insert(id, (d, t));
                            }
                        }
                    }
                } else if l.contains("Download complete") || l.contains("Pull complete") {
                    let id = l.split_whitespace().next().unwrap_or("").to_string();
                    if let Some(e) = layers.get_mut(&id) {
                        e.0 = e.1;
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if crate::cancel::is_cancelled() {
            let _ = child.kill();
            break;
        }
        if last_emit.elapsed() >= Duration::from_secs(1) && !layers.is_empty() {
            flush(sink, &layers, &mut last_bytes, last_emit.elapsed().as_secs_f64());
            last_emit = Instant::now();
        }
    }
    let _ = t1.join();
    let _ = t2.join();
    let status = child.wait().map_err(|e| e.to_string())?;
    crate::cancel::clear_pid();
    if !layers.is_empty() {
        flush(sink, &layers, &mut last_bytes, start.elapsed().as_secs_f64().max(1.0));
    }
    if crate::cancel::is_cancelled() {
        Err("Cancelled.".into())
    } else if status.success() {
        Ok(())
    } else {
        Err("docker compose pull failed".into())
    }
}

/// Download a URL to a file, reporting percent progress.
pub(crate) fn download(url: &str, dest: &Path, sink: &ProgressFn) -> Result<(), String> {
    use std::io::{Read, Write};
    let resp = crate::net::agent().get(url).call().map_err(|e| e.to_string())?;
    let total: u64 = resp.header("Content-Length").and_then(|s| s.parse().ok()).unwrap_or(0);
    let mut reader = resp.into_reader();
    let mut file = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 1 << 16];
    let mut done: u64 = 0;
    let mut last = -1i64;
    loop {
        if crate::cancel::is_cancelled() {
            return Err("Cancelled.".into());
        }
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        if total > 0 {
            let pct = (done * 100 / total) as i64;
            if pct != last {
                last = pct;
                sink(Progress::Percent { value: pct as f32 / 100.0 });
            }
        }
    }
    Ok(())
}

// Keep the unused-import lints honest on every platform.
#[allow(dead_code)]
fn _uses(_: &EnvVar) {}

#[cfg(test)]
mod keepalive_tests {
    use super::*;

    #[test]
    fn the_admin_sql_crosses_any_shell_without_a_dollar_or_quote() {
        let sql = crate::native::admin_account_sql("ada@example.test", "$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA");
        let cmd = admin_account_cmd("sentient-postgres", "sentient", "sentient", &sql);
        assert!(!cmd.contains('$') && !cmd.contains('\'') && !cmd.contains('"'));
        let b64 = cmd.split_whitespace().nth(1).unwrap();
        use base64::Engine as _;
        assert_eq!(String::from_utf8(base64::engine::general_purpose::STANDARD.decode(b64).unwrap()).unwrap(), sql);
        assert!(cmd.ends_with("docker exec -i sentient-postgres psql -U sentient -d sentient -v ON_ERROR_STOP=1 -qAt -f -"));
    }

    #[test]
    fn a_command_in_a_container_runs_where_docker_runs() {
        let argv: Vec<String> = ["/usr/share/sentient/bin/sentient-admin", "unlock", "--email", "o'neil$x@example.test"].iter().map(|s| s.to_string()).collect();
        // Windows: inside the distro, no shell in between (nothing expanded).
        let (prog, args) = docker_exec_args(true, "Ubuntu", false, "sentient", &argv);
        assert_eq!(prog, "wsl.exe");
        assert_eq!(
            args,
            ["-d", "Ubuntu", "-u", "root", "--exec", "docker", "exec", "-i", "sentient", "/usr/share/sentient/bin/sentient-admin", "unlock", "--email", "o'neil$x@example.test"]
        );
        // Linux: bash, each argument quoted; `sg docker` right after setup.
        let (prog, args) = docker_exec_args(false, "", false, "sentient", &argv);
        assert_eq!(prog, "bash");
        assert_eq!(args, ["-lc", "exec docker exec -i 'sentient' '/usr/share/sentient/bin/sentient-admin' 'unlock' '--email' 'o'\\''neil$x@example.test'"]);
        let (prog, args) = docker_exec_args(false, "", true, "sentient", &argv);
        assert_eq!(prog, "sg");
        assert_eq!(args[..2], ["docker", "-c"]);
        assert!(args[2].starts_with("exec docker exec -i 'sentient' "));
    }

    #[test]
    fn the_keepalive_forwards_the_tcp_ports_to_the_distro_and_holds_it() {
        let cfg = DeployConfig::default();
        let ports = forwarded_ports(&cfg);
        assert_eq!(ports, vec![cfg.http_port, cfg.mqtt_port, cfg.mqtts_port]);
        let cmd = keepalive_command("sentient", &ports);
        assert!(cmd.starts_with("@echo off\r\n"));
        // eth0's address, hostname -I when ip gives none, asked again
        // while the distro starts (ping waits: timeout needs a console).
        assert!(cmd.contains("('wsl.exe -d sentient -u root -- ip -4 -o addr show eth0') do for /f \"delims=/\" %%j in (\"%%i\") do set SENTIENT_IP=%%j"));
        assert!(cmd.contains("if not defined SENTIENT_IP for /f \"tokens=1\" %%i in ('wsl.exe -d sentient -u root -- hostname -I') do set SENTIENT_IP=%%i"));
        assert!(cmd.contains("if not defined SENTIENT_IP if %SENTIENT_TRY% lss 10 (set /a SENTIENT_TRY+=1 >nul & ping -n 4 127.0.0.1 >nul & goto address)"));
        assert!(!cmd.contains("timeout "));
        assert!(cmd.contains(&format!("listenport={} listenaddress=0.0.0.0 connectport={} connectaddress=%SENTIENT_IP%", cfg.http_port, cfg.http_port)));
        // The holder comes last: the cmd never ends while the distro runs.
        assert!(cmd.trim_end().ends_with(&format!("{DOCKER_START}; {KEEPALIVE_HOLD}\"")));
        // One holder: a second run finds the lock taken and ends.
        assert!(KEEPALIVE_HOLD.contains("exec flock -n /run/sentient-keepalive.lock sleep infinity"));
        assert!(!KEEPALIVE_HOLD.contains('"') && !KEEPALIVE_HOLD.contains('%') && !KEEPALIVE_HOLD.contains('&'));
        assert!(cmd.lines().all(|l| !l.contains('\n')));
        // In order: the address read (with its retry), then every forward,
        // then Docker and the holder.
        let at = |needle: &str| cmd.find(needle).unwrap_or_else(|| panic!("{needle} missing"));
        let read = at(":address");
        let retry = at("goto address");
        let first_forward = at("netsh interface portproxy delete");
        let last_forward = cmd.rfind("connectaddress=%SENTIENT_IP%").unwrap();
        let holder = at("sleep infinity");
        assert!(read < retry && retry < first_forward && first_forward < last_forward && last_forward < holder);
        for p in &ports {
            let del = at(&format!("delete v4tov4 listenport={p} "));
            let add = at(&format!("add v4tov4 listenport={p} "));
            assert!(del < add, "port {p}: removed before it is added");
        }
    }

    #[test]
    fn a_start_wakes_and_forwards_before_compose_and_reports_only_once_answered() {
        use StackStep::*;
        assert_eq!(stack_steps("start", false).unwrap(), vec![Wake, Compose("up -d"), WaitAnswer]);
        assert_eq!(stack_steps("start", true).unwrap(), vec![Wake, Compose("start"), WaitAnswer]);
        assert_eq!(stack_steps("restart", false).unwrap(), vec![Wake, Compose("stop"), Compose("up -d"), WaitAnswer]);
        // A stop neither wakes the distro nor waits for an answer.
        assert_eq!(stack_steps("stop", false).unwrap(), vec![Compose("stop")]);
        assert!(stack_steps("pause", false).is_err());
    }

    #[test]
    fn dockers_networks_stay_out_of_wsl2s_range() {
        let v: serde_json::Value = serde_json::from_str(ADDRESS_POOLS).unwrap();
        assert_eq!(v["default-address-pools"][0]["base"], "10.214.0.0/16");
        assert!(v["bip"].as_str().unwrap().starts_with("10."));
        let cmd = address_pools_cmd();
        // No quote reaches wsl.exe; an existing pool setting is kept.
        assert!(!cmd.contains('"') && !cmd.contains('\''));
        assert!(cmd.contains("grep -qs default-address-pools /etc/docker/daemon.json"));
        use base64::Engine as _;
        let b64 = cmd.split("echo ").nth(1).unwrap().split(' ').next().unwrap();
        let json = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(b64).unwrap()).unwrap();
        assert_eq!(json.trim_end(), ADDRESS_POOLS);
    }

    #[test]
    fn the_first_answer_wait_says_how_long_and_what_for() {
        assert_eq!(FIRST_ANSWER_SECS, 600);
        let url = "http://localhost:8080";
        assert_eq!(first_answer_line(85, url, Some("running 0")), "Waiting for SENTIENT's first answer on http://localhost:8080: 1:25 of up to 10:00; container running");
        assert_eq!(first_answer_line(0, url, None), "Waiting for SENTIENT's first answer on http://localhost:8080: 0:00 of up to 10:00");
        assert!(first_answer_line(300, url, Some("restarting 3")).ends_with("5:00 of up to 10:00; container restarting, restarted 3 times"));
    }

    #[test]
    fn the_start_toggle_and_delay_go_into_the_boot_trigger() {
        let on = keepalive_task_script(r"C:\ProgramData\SENTIENT\docker-keepalive.cmd", r"PC\o'neil", Some(25), true);
        assert!(on.contains("$t = New-ScheduledTaskTrigger -AtStartup; $t.Delay = 'PT25S'; $r.Trigger = $t; Register-ScheduledTask @r"));
        assert!(on.contains("-UserId 'PC\\o''neil' -LogonType S4U"));
        assert!(on.ends_with(&format!("Start-ScheduledTask -TaskName '{KEEPALIVE_TASK}'")));
        let at_once = keepalive_task_script("x.cmd", "u", Some(0), true);
        assert!(at_once.contains("$r.Trigger = New-ScheduledTaskTrigger -AtStartup; Register") && !at_once.contains("Delay"));
        // Off: the task stays (the Platform Manager's Start runs it), without a trigger.
        let off = keepalive_task_script("x.cmd", "u", None, false);
        assert!(!off.contains("Trigger") && off.contains("Register-ScheduledTask @r") && !off.contains("Start-ScheduledTask"));
        // A change keeps the task's own action and account and a running SENTIENT.
        let change = keepalive_start_script(Some(90));
        assert!(change.contains("Action = $o.Actions; Principal = $o.Principal") && change.contains("$t.Delay = 'PT90S'"));
        assert!(change.contains("if ($was -and"));
        assert!(!keepalive_start_script(None).contains("Trigger"));
    }

    #[test]
    fn the_start_setting_is_read_back_from_the_task() {
        let task = |triggers: &str| format!("<Task><Triggers>{triggers}</Triggers><Actions><Exec><Command>x</Command></Exec></Actions></Task>");
        assert_eq!(keepalive_start_from_xml(&task("<BootTrigger><Enabled>true</Enabled><Delay>PT1M30S</Delay></BootTrigger>")), (true, 90));
        assert_eq!(keepalive_start_from_xml(&task("<BootTrigger><Enabled>true</Enabled></BootTrigger>")), (true, 0));
        assert!(!keepalive_start_from_xml(&task("<BootTrigger><Enabled>false</Enabled><Delay>PT10S</Delay></BootTrigger>")).0);
        assert!(!keepalive_start_from_xml(&task("")).0);
        // Another trigger's delay is not the boot trigger's.
        assert!(!keepalive_start_from_xml(&task("<LogonTrigger><Delay>PT5S</Delay></LogonTrigger>")).0);
    }

    #[test]
    fn a_stopped_distro_is_a_stopped_sentient() {
        let mut cfg = DeployConfig { flavor: Flavor::Clickhouse, ..Default::default() };
        let st = stopped_distro_status(&cfg);
        assert!(st.installed && !st.running);
        let names: Vec<&str> = st.containers.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["sentient", "sentient-postgres", "sentient-clickhouse"]);
        assert!(st.containers.iter().all(|c| c.state == "stopped"));
        cfg.flavor = Flavor::Stock;
        assert_eq!(stopped_distro_status(&cfg).containers.len(), 2);
    }
}

#[cfg(test)]
mod tests {

    /// A container's log, read where Docker runs: its lines with their
    /// timestamps, stderr included, the last N or the last day.
    #[test]
    fn container_logs_read_through_dockers_shell() {
        assert_eq!(super::container_logs_cmd("sentient", Some(500)), "docker logs --timestamps --tail 500 'sentient' 2>&1");
        assert_eq!(super::container_logs_cmd("sentient-postgres", None), "docker logs --timestamps --since 24h 'sentient-postgres' 2>&1");
    }

    /// Every command that may run in SENTIENT's WSL distro asks
    /// [`super::left_out`] (or goes through `shell_output`, which does)
    /// first: a read must never start a stopped distro, and with it
    /// SENTIENT (the rc5 VM: Platform settings' Data locations card read
    /// through a path that skipped the check, and booted the distro with
    /// "Start with Windows" off). Scans this crate and the Platform Manager.
    #[test]
    fn every_distro_command_asks_left_out() {
        let here = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let roots = [here.join("src"), here.join("../../app/src")];
        // Builders of the arguments (their callers are checked), the
        // installer's own job that hardens ClickHouse right after setup,
        // and the gateway page's password: an action the user presses
        // (Linux only; Windows runs Docker Desktop's docker.exe there).
        let allowed = ["fn wsl_bash_args", "fn docker_exec_args", "\"harden\" | \"harden-fsync\" =>", "pub fn set_ui_password"];
        let mut found = Vec::new();
        let mut files = Vec::new();
        let mut dirs: Vec<std::path::PathBuf> = roots.iter().filter(|r| r.is_dir()).cloned().collect();
        while let Some(d) = dirs.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    dirs.push(p);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    files.push(p);
                }
            }
        }
        assert!(files.len() > 50, "the sources were found");
        for f in files {
            let text = std::fs::read_to_string(&f).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            // Tests at the end are not run by a page.
            let end = lines.windows(2).position(|w| w[0].trim() == "#[cfg(test)]" && w[1].trim_start().starts_with("mod ")).unwrap_or(lines.len());
            for (i, l) in lines[..end].iter().enumerate() {
                let t = l.trim_start();
                if t.starts_with("//") {
                    continue;
                }
                let from = i.saturating_sub(15);
                let around = lines[from..(i + 16).min(end)].join("\n");
                // `-d <distro>` of wsl.exe only (gpasswd -d, psql -d are
                // the computer's own programs, no distro).
                let wsl_d = t.contains("[\"-d\",") && (around.contains("wsl") || around.to_lowercase().contains("distro"));
                let hit = (t.contains("shell_args(") && !t.contains("fn shell_args")) || (t.contains("docker_exec_command") && !t.contains("fn docker_exec_command")) || wsl_d;
                if !hit {
                    continue;
                }
                if around.contains("left_out(") || around.contains("shell_output(") || allowed.iter().any(|a| around.contains(a)) {
                    continue;
                }
                found.push(format!("{}:{}: {}", f.display(), i + 1, t));
            }
        }
        assert!(found.is_empty(), "a command in the distro without the read guard:\n{}", found.join("\n"));
        // The guard itself: a read, its distro stopped, is left out.
        assert!(super::idle::skip(true, &["-d", "sentient", "--", "docker", "ps"], |_| false));
    }

    #[test]
    fn docker_not_answering_lists_the_containers_stopped() {
        use super::{containers_from_ps, DeployConfig};
        let cfg = DeployConfig::default();
        let n = cfg.names();
        // The rc5 VM: the distro up, Docker not started: never "0 of 0".
        for ps in [None, Some((false, String::new()))] {
            let list = containers_from_ps(&cfg, ps);
            assert!(list.len() >= 2, "{}", list.len());
            assert!(list.iter().all(|c| c.state == "stopped"));
            assert!(list.iter().any(|c| c.name == n.app) && list.iter().any(|c| c.name == n.pg));
        }
        // Docker answers: its lines, others' containers left out.
        let out = format!("{}|Up 3 minutes\n{}|Exited (0) 1 minute ago\nother|Up 1 hour\n", n.app, n.pg);
        let list = containers_from_ps(&cfg, Some((true, out)));
        assert_eq!(list.len(), 2);
        assert_eq!((list[0].state.as_str(), list[1].state.as_str()), ("running", "stopped"));
    }

    #[test]
    fn container_stats_are_one_command_where_docker_runs() {
        use super::{container_stats, container_stats_cmd, split_container_stats};
        let cmd = container_stats_cmd(&["sentient".into(), "sentient-postgres".into()]);
        // One shell line (one wsl.exe on Windows), each name quoted, no tab
        // (the Docker shell's arguments pass through wsl.exe).
        assert!(cmd.starts_with("docker stats --no-stream --format '{{.Name}}|{{.CPUPerc}}|{{.MemUsage}}' 'sentient' 'sentient-postgres'"), "{cmd}");
        assert!(cmd.contains("docker inspect --format '{{.Name}}|{{.RestartCount}}' 'sentient' 'sentient-postgres'"), "{cmd}");
        assert!(!cmd.contains('\t'));
        let (stats, restarts) = split_container_stats("sentient|3.10%|1.2GiB / 7.7GiB\nsentient-postgres|0.50%|160MiB / 7.7GiB\n--sentient-restarts--\n/sentient|0\n/sentient-postgres|2\n");
        assert_eq!(stats.lines().count(), 2);
        assert_eq!(restarts, "/sentient|0\n/sentient-postgres|2");
        // No containers: nothing run.
        assert_eq!(container_stats(&[]), Some((String::new(), String::new())));
    }

    #[test]
    fn the_runner_picks_the_scoped_then_the_recorded_then_its_own_distro() {
        use super::{DISTRO, pick_distro};
        assert_eq!(pick_distro(None, None), DISTRO);
        assert_eq!(pick_distro(None, Some("Ubuntu")), "Ubuntu");
        assert_eq!(pick_distro(Some("Debian"), Some("Ubuntu")), "Debian");
        assert_eq!(pick_distro(Some("  "), Some(" Ubuntu ")), "Ubuntu");
        assert_eq!(pick_distro(Some(""), Some("")), DISTRO);
    }

    #[test]
    fn a_scoped_distro_applies_inside_only_and_nests() {
        use super::{DISTRO, docker_distro, with_docker_distro};
        // The record's choice is process-wide: this test leaves it alone
        // and checks the per-thread override, on a thread of its own.
        std::thread::spawn(|| {
            let before = docker_distro();
            let inner = with_docker_distro(Some("Ubuntu"), || {
                let outer = docker_distro();
                let nested = with_docker_distro(None, docker_distro);
                (outer, nested, docker_distro())
            });
            assert_eq!(inner, ("Ubuntu".to_string(), DISTRO.to_string(), "Ubuntu".to_string()));
            assert_eq!(docker_distro(), before);
            // Restored after a panic too.
            let _ = std::panic::catch_unwind(|| with_docker_distro(Some("Debian"), || panic!("boom")));
            assert_eq!(docker_distro(), before);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn the_wsl_command_names_the_distro() {
        assert_eq!(
            super::wsl_bash_args("Ubuntu", "docker ps"),
            vec!["-d", "Ubuntu", "-u", "root", "--", "bash", "-lc", "docker ps"]
        );
    }

    #[test]
    fn a_config_without_wsl_distro_loads_and_saves_as_before() {
        use super::{DeployConfig, ExternalCompose};
        // Saved by 0.9.0: no wsl_distro.
        let old = r#"{"stack_name":"sentient","image":"quakestring/sentient:4.4.0.1","external":{"project":"sentient","files":["/home/op/docker-compose.yml"],"working_dir":"/home/op"}}"#;
        let cfg: DeployConfig = serde_json::from_str(old).unwrap();
        assert_eq!(cfg.wsl_distro, None);
        assert!(!serde_json::to_string(&cfg).unwrap().contains("wsl_distro"));
        // Found in the operator's distro: kept through a save and a load.
        let found = DeployConfig {
            wsl_distro: Some("Ubuntu".into()),
            external: Some(ExternalCompose { project: "plant_a".into(), files: vec![], working_dir: String::new() }),
            ..Default::default()
        };
        let text = serde_json::to_string(&found).unwrap();
        assert!(text.contains(r#""wsl_distro":"Ubuntu""#), "{text}");
        let back: DeployConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(back.wsl_distro.as_deref(), Some("Ubuntu"));
        assert_eq!(back, found);
    }

    #[cfg(not(windows))]
    #[test]
    fn a_vanished_override_does_not_stop_control() {
        let dir = std::env::temp_dir().join(format!("spm-vanished-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let main = dir.join("docker-compose.yml");
        std::fs::write(&main, "services: {}\n").unwrap();
        let gone = dir.join("override.yml");
        let cfg = DeployConfig {
            external: Some(ExternalCompose { project: "sentient".into(), files: vec![main.display().to_string(), gone.display().to_string()], working_dir: String::new() }),
            ..Default::default()
        };
        let up = cfg.compose_cmd("up -d");
        assert!(up.contains(&main.display().to_string()) && !up.contains("override.yml"), "{up}");
        assert_eq!(cfg.control_cmd("stop"), "docker compose -p 'sentient' stop".replace('\'', if cfg.control_cmd("stop").contains('\'') { "'" } else { "" }));
        assert_eq!(cfg.vanished_files(), vec![gone.display().to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::*;

    #[test]
    fn extra_env_overrides_without_duplicate_keys() {
        let mut cfg = DeployConfig::default();
        cfg.tuning.extra_env.push(crate::stack::EnvVar { name: "DB_POOL_PRIMARY_MAX".into(), value: "40".into() });
        let y = compose(&cfg);
        assert_eq!(y.matches("DB_POOL_PRIMARY_MAX:").count(), 1);
        assert!(y.contains("DB_POOL_PRIMARY_MAX: \"40\""));
    }

    #[test]
    fn default_stack_reproduces_reference_names() {
        let cfg = DeployConfig::default();
        let y = compose(&cfg);
        assert!(y.contains("container_name: sentient-postgres"));
        assert!(y.contains("container_name: sentient-clickhouse"));
        assert!(y.contains("name: st-pg-data"));
        assert!(y.contains("name: st-ch-data"));
        assert!(y.contains("CLICKHOUSE_URL: \"http://clickhouse:8123\""));
        assert!(y.contains("image: \"postgres:18-alpine\""));
    }

    #[test]
    fn clickhouse_only_stack_has_clickhouse_and_nothing_else() {
        let cfg = DeployConfig { stack_name: "sentient-ch".into(), flavor: Flavor::Clickhouse, ..DeployConfig::default() };
        let y = compose_clickhouse_only(&cfg);
        assert!(y.contains("  clickhouse:\n"));
        assert!(y.contains("container_name: \"sentient-ch-clickhouse\"") || y.contains("container_name: sentient-ch-clickhouse"));
        assert!(!y.contains("  postgres:\n") && !y.contains("  sentient:\n"));
        assert!(y.contains("\"8123:8123\"") && y.contains("\"9000:9000\""));
        assert!(y.contains("name: sentient-ch-ch-data"));
    }

    /// Setup's installs (db_loopback): PostgreSQL and ClickHouse published
    /// on 127.0.0.1 only, SENTIENT's own ports on every address; a stack
    /// without it keeps its ports as they were.
    #[test]
    fn loopback_publishes_the_databases_on_127_0_0_1_only() {
        for flavor in [Flavor::Stock, Flavor::Timescaledb, Flavor::Clickhouse] {
            let cfg = DeployConfig { flavor, db_loopback: true, ..DeployConfig::default() };
            let y = compose(&cfg);
            assert!(y.contains("- \"127.0.0.1:5432:5432\""), "{flavor:?}:\n{y}");
            assert!(!y.contains("- \"5432:5432\""));
            if flavor.uses_clickhouse() {
                assert!(y.contains("- \"127.0.0.1:8123:8123\"") && y.contains("- \"127.0.0.1:9000:9000\""), "{y}");
            } else {
                assert!(!y.contains(":8123\"") && !y.contains(":9000\""));
            }
            // Every published database port is on loopback.
            for l in y.lines().map(str::trim).filter(|l| l.starts_with("- \"") && (l.ends_with(":5432\"") || l.ends_with(":8123\"") || l.ends_with(":9000\""))) {
                assert!(l.starts_with("- \"127.0.0.1:"), "{l}");
            }
            // SENTIENT itself stays reachable from the network.
            assert!(y.contains("- \"8080:8080\"") && y.contains("- \"1883:1883\""));
        }
        let ch = compose_clickhouse_only(&DeployConfig { flavor: Flavor::Clickhouse, db_loopback: true, ..DeployConfig::default() });
        assert!(ch.contains("- \"127.0.0.1:8123:8123\"") && ch.contains("- \"127.0.0.1:9000:9000\""));
        // A record without the flag (a stack installed earlier, or taken
        // over as it was published) is written as before.
        let old: DeployConfig = serde_json::from_str("{}").unwrap();
        assert!(!old.db_loopback);
        assert!(compose(&old).contains("- \"5432:5432\""));
    }

    #[test]
    fn clickhouse_bind_mount_is_one_quoted_scalar() {
        let y = compose(&DeployConfig::default());
        let line = y.lines().find(|l| l.contains("config.d/sentient.xml")).unwrap();
        assert!(line.trim().starts_with("- \"") && line.trim().ends_with(":ro\""), "{line}");
    }

    #[test]
    fn timescale_flavour_uses_timescale_image_and_preload() {
        let cfg = DeployConfig { flavor: Flavor::Timescaledb, ..DeployConfig::default() };
        let y = compose(&cfg);
        assert!(y.contains(PG_IMAGE_TIMESCALE));
        assert!(y.contains("shared_preload_libraries=timescaledb"));
        assert!(!y.contains("clickhouse:"));
        assert!(!y.contains("CLICKHOUSE_URL"));
    }

    #[test]
    fn custom_stack_gets_its_own_names_and_ports() {
        let cfg = DeployConfig { stack_name: "lab".into(), http_port: 18080, ..DeployConfig::default() };
        let y = compose(&cfg);
        assert!(y.contains("container_name: lab-postgres"));
        assert!(y.contains("name: lab-pg-data"));
        assert!(y.contains("\"18080:8080\""));
        assert!(cfg.compose_path().ends_with("/lab/docker-compose.yml"));
    }

    #[test]
    fn validate_rejects_port_clash_and_bad_names() {
        let mut cfg = DeployConfig::default();
        cfg.mqtt_port = cfg.http_port;
        assert!(cfg.validate().is_err());
        let mut cfg = DeployConfig::default();
        cfg.db_password = "has space".into();
        assert!(cfg.validate().is_err());
        assert!(DeployConfig::default().validate().is_ok());
    }

    #[test]
    fn old_partial_config_still_loads() {
        let cfg: DeployConfig = serde_json::from_str(
            r#"{"db_name":"x","db_user":"u","db_password":"p","http_port":8080,"mqtt_port":1883,"coap_port":5683,"load_demo":false}"#,
        ).unwrap();
        assert_eq!(cfg.db_name, "x");
        assert_eq!(cfg.flavor, Flavor::Clickhouse);
        assert_eq!(cfg.tuning.db_pool_primary_max, 48);
    }
}

#[cfg(test)]
mod vm_timeout_tests {
    use super::*;

    #[test]
    fn a_vm_timeout_is_retried_once_and_nothing_else_is() {
        // wsl --import on the Windows 10 19044 test VM, right after the kernel MSI.
        let out = "The operation timed out because a response was not received from the virtual machine or container.\r\n";
        assert!(retry_after_vm_timeout(0, out));
        assert!(!retry_after_vm_timeout(1, out), "only once");
        assert!(retry_after_vm_timeout(0, "Error code: Wsl/Service/CreateInstance/HCS_E_CONNECTION_TIMEOUT"));
        assert!(retry_after_vm_timeout(0, "Error: 0x80370109"));
        // Other failures are reported as they are.
        assert!(!retry_after_vm_timeout(0, "The Windows Subsystem for Linux is not installed."));
        assert!(!retry_after_vm_timeout(0, "Error: 0x80370102 Please enable the Virtual Machine Platform"));
        assert!(!retry_after_vm_timeout(0, ""));
        assert!(!VM_TIMEOUT_RETRY_LINE.to_ascii_lowercase().starts_with("error"));
    }
}
