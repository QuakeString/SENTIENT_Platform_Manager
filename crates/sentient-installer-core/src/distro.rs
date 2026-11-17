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
pub mod move_data;
pub mod gateway_docker;
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

/// A command through the Docker shell, stopped after `limit` on Windows.
pub(crate) fn shell_output(prog: &str, args: &[&str], limit: std::time::Duration) -> Option<(bool, Vec<u8>, Vec<u8>)> {
    if cfg!(windows) {
        sys::output_wsl(prog, args, limit)
    } else {
        let _ = limit;
        sys::output(prog, args)
    }
}

/// Write `content` to `path` on the Docker host.
fn write_host_file(sink: &ProgressFn, path: &str, content: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        let dir = path.rsplit_once('/').map(|(d, _)| d).unwrap_or(".");
        run(sink, &format!("mkdir -p {dir} && cat > {path} <<'SENTIENTEOF'\n{content}\nSENTIENTEOF\n"))
    }
    #[cfg(not(windows))]
    {
        let _ = sink;
        if let Some(dir) = Path::new(path).parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        std::fs::write(path, content).map_err(|e| format!("write {path}: {e}"))
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
    write_host_file(sink, &cfg.compose_path(), &compose(cfg))?;
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
        let inside = sys::output_wsl("wsl.exe", &["-d", &distro, "-u", "root", "--", "curl", "-s", "-o", "/dev/null", "--max-time", "4", &url], std::time::Duration::from_secs(20))
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
    write_host_file(&sink, &cfg.compose_path(), &compose_clickhouse_only(cfg))?;
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

    sink(Progress::Step { name: "Waiting for SENTIENT to become ready".into() });
    let url = format!("http://localhost:{}", cfg.http_port);
    for i in 0..72 {
        bail_if_cancelled!();
        if is_running(cfg.http_port) {
            sink(Progress::Done { message: format!("SENTIENT is running at {url}") });
            return Ok(());
        }
        sink(Progress::Log { line: format!("waiting for SENTIENT to start… ({}s)", i * 5) });
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
    Err("SENTIENT didn't answer in time. It may still be initializing the database.".into())
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
            let _ = sys::output("wsl.exe", &["--terminate", DISTRO]);
            let _ = sys::output("wsl.exe", &["--unregister", DISTRO]);
        } else {
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
        wsl_native(&sink, &[
            "--import", DISTRO,
            &distro_dir.to_string_lossy(),
            &rootfs.to_string_lossy(),
            "--version", "2",
        ])?;
        let _ = std::fs::remove_file(&rootfs);
    }

    // Verify the distro actually runs a command before configuring it —
    // catches a broken import or a WSL that still needs a Windows restart.
    if let Err(e) = distro_probe() {
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
    run(&sink, "systemctl enable --now docker")?;

    if is_ready() {
        sink(Progress::Done { message: "Docker Engine is installed and running.".into() });
        Ok(())
    } else {
        Err("Docker was installed but isn't responding yet. Try the step again.".into())
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
    pub running: bool,   // the app container is up
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

    let installed = present && cfg.stack_defined();
    let mut containers = Vec::new();
    if present {
        // Only `.Status` — universally supported, unlike `.State` and
        // `--filter` — and derive the state from it ("Up …" == running).
        if let Some((_, out)) = run_output("docker ps -a --format '{{.Names}}|{{.Status}}'") {
            for line in out.lines() {
                if let Some((name, status)) = line.trim().split_once('|') {
                    if name == n.app || name == n.pg || name == n.ch {
                        let state = if status.starts_with("Up") { "running" } else { "stopped" };
                        containers.push(ContainerStatus {
                            name: name.to_string(),
                            state: state.to_string(),
                            status: status.to_string(),
                        });
                    }
                }
            }
        }
    }
    let container_up = containers.iter().any(|c| c.name == n.app && c.state == "running");
    let running = installed && (container_up || is_running(cfg.http_port));
    StackStatus { installed, running, containers }
}

/// Replace the default sys-admin sign-in inside the stack's PostgreSQL.
pub fn set_admin_account(cfg: &DeployConfig, email: &str, password_hash: &str) -> Result<(), String> {
    let n = cfg.names();
    let sql = crate::native::admin_account_sql(email, password_hash);
    let cmd = format!(
        "docker exec {} psql -U {} -d {} -v ON_ERROR_STOP=1 -qAt -c {}",
        n.pg,
        cfg.db_user,
        cfg.db_name,
        shell_quote(&sql)
    );
    match run_output(&cmd) {
        Some((true, _)) => Ok(()),
        _ => Err("could not set the administrator sign-in in the database container".into()),
    }
}

/// Single-quote for bash, which runs every Docker command here.
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
    run_stream(&sink, &cfg.control_cmd(sub))?;
    sink(Progress::Done { message: format!("{label}: done.") });
    Ok(())
}

/// Start, stop or restart the whole stack: compose over all of the
/// project's services, which it orders by their dependencies
/// (crate::stack_order::docker_commands); a start waits for SENTIENT to
/// answer.
pub fn control_stack(sink: ProgressFn, cfg: &DeployConfig, action: &str) -> Result<(), String> {
    use crate::stack_order::{docker_commands, wait_until, SENTIENT_READY_SECS};
    let subs = docker_commands(action, cfg.external.is_some())?;
    for sub in &subs {
        sink(Progress::Step { name: if *sub == "stop" { "Stopping SENTIENT and its databases".into() } else { "Starting the databases, then SENTIENT".into() } });
        run_stream(&sink, &cfg.control_cmd(sub))?;
    }
    if action != "stop" {
        sink(Progress::Step { name: "Waiting for SENTIENT to answer".into() });
        if !wait_until(SENTIENT_READY_SECS, || crate::native::http_answers(cfg.http_port)) {
            return Err(format!(
                "SENTIENT was started but does not answer on port {} after {} minutes. Its log says why.",
                cfg.http_port,
                SENTIENT_READY_SECS / 60
            ));
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
    match sys::output("wsl.exe", &["-d", DISTRO, "-u", "root", "--", "bash", "-lc", "echo ok"]) {
        Some((true, out, _)) if sys::decode(&out).contains("ok") => Ok(()),
        Some((_, _, err)) => {
            let e = sys::decode(&err);
            let e = e.trim();
            Err(if e.is_empty() { "no response".into() } else { e.to_string() })
        }
        None => Err("could not run wsl.exe".into()),
    }
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
    match sys::output_tracked("wsl.exe", args) {
        Some((ok, out, err)) => {
            exec::emit(sink, &out);
            exec::emit(sink, &err);
            if ok { Ok(()) } else { Err(format!("wsl {} failed", args.join(" "))) }
        }
        None => Err("could not run wsl.exe".into()),
    }
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
mod tests {

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
