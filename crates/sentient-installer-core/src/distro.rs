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

pub const DISTRO: &str = "sentient";

pub mod docker_ops;
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
}

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
        }
    }
}

/// Container and volume names for one stack.
struct Names {
    app: String,
    pg: String,
    ch: String,
    vol_pg: String,
    vol_data: String,
    vol_ch: String,
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

    fn names(&self) -> Names {
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
    fn compose_path(&self) -> String {
        format!("{}/docker-compose.yml", self.stack_dir())
    }
    fn ch_config_path(&self) -> String {
        format!("{}/clickhouse-sentient.xml", self.stack_dir())
    }
    fn compose_cmd(&self, sub: &str) -> String {
        format!("docker compose -f {} {sub}", self.compose_path())
    }
}

#[cfg(windows)]
fn stack_base_dir() -> String {
    "/opt/sentient".into()
}

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
    env
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
    y.push_str(&format!("    ports:\n      - \"{}:8123\"\n      - \"{}:9000\"\n", cfg.ch_http_port, cfg.ch_native_port));
    y.push_str("    healthcheck:\n      test: [\"CMD-SHELL\", \"wget -qO- http://127.0.0.1:8123/ping || exit 1\"]\n      interval: 10s\n      timeout: 5s\n      retries: 12\n\n");
    y
}

/// A stack holding ClickHouse alone.
pub fn compose_clickhouse_only(cfg: &DeployConfig) -> String {
    let n = cfg.names();
    format!(
        "name: {}\n\nservices:\n{}volumes:\n  clickhouse_data:\n    name: {}\n",
        cfg.stack_name,
        clickhouse_service(cfg),
        n.vol_ch
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
        "    environment:\n      POSTGRES_DB: {}\n      POSTGRES_USER: {}\n      POSTGRES_PASSWORD: {}\n      PGDATA: /var/lib/postgresql/data\n",
        yq(&cfg.db_name), yq(&cfg.db_user), yq(&cfg.db_password)
    ));
    y.push_str("    volumes:\n      - postgres_data:/var/lib/postgresql\n");
    y.push_str(&format!("    ports:\n      - \"{}:5432\"\n", cfg.pg_port));
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
    y.push_str(&format!("volumes:\n  postgres_data:\n    name: {}\n  sentient_data:\n    name: {}\n", n.vol_pg, n.vol_data));
    if cfg.flavor.uses_clickhouse() {
        y.push_str(&format!("  clickhouse_data:\n    name: {}\n", n.vol_ch));
    }
    y
}

// ---------------------------------------------------------------------------
// Host shell — the one seam between Windows/WSL and Linux
// ---------------------------------------------------------------------------

/// Program + args that run `bash` as the Docker-capable user.
#[cfg(windows)]
fn shell_args(bash: &str) -> (String, Vec<String>) {
    ("wsl.exe".into(), ["-d", DISTRO, "-u", "root", "--", "bash", "-lc", bash].iter().map(|s| s.to_string()).collect())
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
    match sys::output(&prog, &argrefs) {
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
    sys::output(&prog, &argrefs).map(|(ok, out, _)| (ok, sys::decode(&out)))
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
        distro_present() && run_output("docker version").map(|(ok, _)| ok).unwrap_or(false)
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
        sys::output("wsl.exe", &["-d", DISTRO, "-u", "root", "--", "curl", "-s", "-o", "/dev/null", "--max-time", "4", &url])
            .map(|(ok, _, _)| ok)
            .unwrap_or(false)
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
        return Err("Docker is not reachable — run the Docker setup step first.".into());
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
    cfg.validate()?;
    if !is_ready() {
        return Err("Docker is not reachable — run the Docker setup step first.".into());
    }

    sink(Progress::Step { name: "Writing the SENTIENT configuration".into() });
    write_stack_files(&sink, cfg)?;
    sink(Progress::Log { line: format!("{} — {}", cfg.compose_path(), cfg.flavor.label()) });
    bail_if_cancelled!();

    sink(Progress::Step { name: "Pulling SENTIENT images (first time, a few minutes)".into() });
    pull_images(&sink, cfg)?;
    bail_if_cancelled!();

    // The server validates the schema at boot and REFUSES to start on an
    // empty database (with `restart: always` it would crash-loop). Run the
    // one-time installer first: `compose run` brings the databases up via
    // depends_on, installs the schema and system resources, then exits. On a
    // re-run against an installed DB it exits non-zero ("refuses to overwrite
    // an existing schema"); expected, so log and carry on — the readiness
    // probe below is the real gate.
    let demo = if cfg.load_demo { " -e LOAD_DEMO=true" } else { "" };
    sink(Progress::Step { name: "Installing the SENTIENT database (first run, one-time)".into() });
    if let Err(e) = run_stream(&sink, &cfg.compose_cmd(&format!("run --rm -e INSTALL_SENTIENT=true{demo} sentient"))) {
        bail_if_cancelled!();
        sink(Progress::Log { line: format!(
            "note: install step returned an error ({e}). If the database was already installed this is expected — continuing."
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
    Err("SENTIENT didn't answer in time — it may still be initializing the database.".into())
}

/// Tear down a partial/cancelled deploy: containers, network, volumes, and
/// half-pulled images. Leaves Docker (and the WSL distro) in place so a retry
/// starts clean. Best-effort throughout — the stack may not exist yet.
pub fn cleanup(sink: ProgressFn, cfg: &DeployConfig) -> Result<(), String> {
    let n = cfg.names();
    sink(Progress::Step { name: "Stopping and removing SENTIENT containers and volumes".into() });
    if host_file_exists(&cfg.compose_path()) {
        let _ = run_stream(&sink, &format!("{} 2>/dev/null || true", cfg.compose_cmd("down -v --remove-orphans")));
    }
    // Belt-and-suspenders in case the containers exist without the compose file.
    let _ = run_stream(&sink, &format!("docker rm -f {} {} {} 2>/dev/null || true", n.app, n.pg, n.ch));
    sink(Progress::Step { name: "Reclaiming disk from partial image pulls".into() });
    let _ = run_stream(&sink, "docker image prune -f 2>/dev/null || true");
    sink(Progress::Done { message: "Cleanup complete — you can safely retry the install.".into() });
    Ok(())
}

/// Remove SENTIENT entirely.
///
/// Windows: unregister the WSL distro, which deletes containers, volumes,
/// images, Docker and the compose file in one go (everything lives on the
/// distro's disk). Linux: take the stack down with its volumes and images and
/// remove the compose directory; Docker itself stays — it is the system's.
pub fn uninstall(sink: ProgressFn, cfg: &DeployConfig) -> Result<(), String> {
    #[cfg(windows)]
    {
        let _ = cfg;
        if distro_present() {
            sink(Progress::Step { name: "Removing the SENTIENT WSL distro and all its data".into() });
            let _ = sys::output("wsl.exe", &["--terminate", DISTRO]);
            let _ = sys::output("wsl.exe", &["--unregister", DISTRO]);
        } else {
            sink(Progress::Log { line: "No SENTIENT distro found — nothing to remove.".into() });
        }
        sink(Progress::Done { message: "SENTIENT and its Docker environment were removed.".into() });
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let n = cfg.names();
        sink(Progress::Step { name: "Removing SENTIENT containers, volumes and images".into() });
        if host_file_exists(&cfg.compose_path()) {
            let _ = run_stream(&sink, &format!("{} 2>/dev/null || true", cfg.compose_cmd("down -v --remove-orphans --rmi all")));
        }
        let _ = run_stream(&sink, &format!("docker rm -f {} {} {} 2>/dev/null || true", n.app, n.pg, n.ch));
        let _ = run_stream(&sink, &format!("docker volume rm {} {} {} 2>/dev/null || true", n.vol_pg, n.vol_data, n.vol_ch));
        let dir = cfg.stack_dir();
        let _ = std::fs::remove_file(cfg.compose_path());
        let _ = std::fs::remove_file(cfg.ch_config_path());
        let _ = std::fs::remove_dir(&dir); // only if now empty
        sink(Progress::Done { message: "SENTIENT was removed. Docker itself was left installed.".into() });
        Ok(())
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

#[cfg(windows)]
fn setup_windows(sink: ProgressFn, install_dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(install_dir).map_err(|e| e.to_string())?;

    // A distro left half-imported by a cancelled or failed run makes every
    // later command fail cryptically. If one exists but doesn't answer,
    // unregister it so it re-imports clean.
    if distro_present() && !distro_healthy() {
        sink(Progress::Step { name: "Resetting an unresponsive SENTIENT distro".into() });
        let _ = sys::output("wsl.exe", &["--terminate", DISTRO]);
        let _ = sys::output("wsl.exe", &["--unregister", DISTRO]);
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
        Err("Docker was installed but isn't responding yet — try the step again.".into())
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
  echo "No supported package manager (apt, pacman, dnf, zypper) — install Docker Engine by hand and re-run." >&2
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
    let present = distro_present();
    #[cfg(not(windows))]
    let present = exec::have("docker");

    let installed = present && host_file_exists(&cfg.compose_path());
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
    let (label, sub) = match action {
        "start" => ("Starting SENTIENT", "up -d"),
        "stop" => ("Stopping SENTIENT", "stop"),
        "restart" => ("Restarting SENTIENT", "restart"),
        _ => return Err(format!("unknown action: {action}")),
    };
    sink(Progress::Step { name: label.into() });
    run_stream(&sink, &cfg.compose_cmd(sub))?;
    sink(Progress::Done { message: format!("{label} — done.") });
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
fn distro_present() -> bool {
    sys::output("wsl.exe", &["-l", "-q"])
        .map(|(_, out, _)| sys::decode(&out).lines().any(|l| l.trim().eq_ignore_ascii_case(DISTRO)))
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
    let resp = ureq::get(url).call().map_err(|e| e.to_string())?;
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
    use super::*;

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
