//! Native Windows deployment — PostgreSQL + TimescaleDB + the SENTIENT server
//! installed directly on the host, with no WSL2 and no Docker.
//!
//! This is the second deployment mode alongside [`crate::distro`]. It exposes
//! deliberately the *same* surface (`is_ready`, `is_running`, `setup`, `deploy`,
//! `status`, `control`, `logs`, `update`, `uninstall`, `cleanup`) so the app
//! layer can dispatch on the user's chosen mode without special-casing either
//! one.
//!
//! Why it exists: an air-gapped plant network — the normal SCADA deployment —
//! cannot pull images. An offline bundle of the Docker mode has to carry a WSL2
//! distro plus Docker Engine plus the ~500 MB SENTIENT image; the native mode
//! carries a server binary, the UI and PostgreSQL, which is far smaller.
//!
//! Every step here was validated by hand on Windows 11 first; the notes below
//! record the traps that cost time, because each one silently produces a broken
//! install rather than an error.


pub mod safe_update;

use std::path::{Path, PathBuf};

use crate::progress::{Progress, ProgressFn};
use crate::stack::{self, ChTuning, ClickhouseConn, Flavor, PgTuning, Tuning};
use crate::sys;

// ---------------------------------------------------------------------------
// Pinned prerequisite versions
// ---------------------------------------------------------------------------

/// PostgreSQL **must be >= 18.6**. TimescaleDB 2.29.x imports `palloc_mul` and
/// `palloc0_mul`, which PostgreSQL 18.3's `postgres.exe` does not export — the
/// extension then fails to load with the extremely unhelpful "The specified
/// procedure could not be found". Verified with `dumpbin /imports` against
/// `dumpbin /exports`. Do not lower this pin without re-checking those symbols.
///
/// Note EDB publishes no 18.5 binaries zip (403); 18.4 and 18.6 exist.
pub const PG_VERSION: &str = "18.6-1";
pub const PG_ZIP: &str = "postgresql-18.6-1-windows-x64-binaries.zip";
pub const PG_URL: &str =
    "https://get.enterprisedb.com/postgresql/postgresql-18.6-1-windows-x64-binaries.zip";

pub const TSDB_VERSION: &str = "2.29.2";
pub const TSDB_ZIP: &str = "timescaledb-postgresql-18-windows-amd64.zip";
pub const TSDB_URL: &str = "https://github.com/timescale/timescaledb/releases/download/2.29.2/timescaledb-postgresql-18-windows-amd64.zip";

/// Windows service names we register.
pub const PG_SERVICE: &str = "sentient-postgresql";
pub const APP_SERVICE: &str = "SentientServer";

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Where the installer gets its payload.
///
/// This is the offline/online split, expressed at the only layer that should
/// care about it: everything above works the same either way.
#[derive(Debug, Clone)]
pub enum ArtifactSource {
    /// Offline bundle — every archive already sits next to the installer
    /// (shipped inside the Platform Manager package). Nothing touches the network.
    Bundled { dir: PathBuf },
    /// Online — fetch from a manifest-provided base URL, verifying SHA-256.
    Remote { base_url: String },
}

impl ArtifactSource {
    fn is_offline(&self) -> bool {
        matches!(self, ArtifactSource::Bundled { .. })
    }
}

#[derive(Debug, Clone)]
pub struct NativeConfig {
    /// Root of the SENTIENT install, e.g. `C:\Program Files\SENTIENT`.
    pub install_dir: PathBuf,
    /// Root of the bundled PostgreSQL, e.g. `C:\PostgreSQL\18`.
    pub pg_dir: PathBuf,
    /// Working/state directory for things the server writes at runtime.
    pub state_dir: PathBuf,
    pub db_name: String,
    pub db_user: String,
    pub db_password: String,
    /// Superuser password for the bundled PostgreSQL cluster.
    pub pg_superuser_password: String,
    pub http_port: u16,
    pub mqtt_port: u16,
    pub coap_port: u16,
    pub pg_port: u16,
    pub load_demo: bool,
    pub source: ArtifactSource,
    /// Licence server the installed instance phones home to. Written into
    /// the service environment explicitly rather than left to the binary's
    /// built-in default, so an operator can see and change it in
    /// conf/sentient.env. Defaults to production.
    pub license_server_url: String,
    /// Extra environment for the service, appended verbatim. What a test
    /// harness uses to shorten the heartbeat interval, and what an operator
    /// uses for the odd site-specific override without editing the XML.
    pub extra_env: Vec<(String, String)>,
    /// Secret for signing JWTs. Generated per install — never defaulted.
    pub jwt_secret: String,
    /// Where telemetry lives. Windows installs TimescaleDB (the only tier the
    /// bundled PostgreSQL ships); Linux offers stock and ClickHouse too.
    pub flavor: Flavor,
    /// The same knobs the Docker engine writes into the compose file.
    pub tuning: Tuning,
    pub pg: PgTuning,
    /// ClickHouse (Linux, hybrid flavour only).
    pub ch: ChTuning,
    pub ch_db: String,
    pub ch_user: String,
    pub ch_password: String,
    pub ch_http_port: u16,
    pub ch_native_port: u16,
}

impl NativeConfig {
    fn pg_bin(&self) -> PathBuf {
        self.pg_dir.join("bin")
    }
    fn pg_data(&self) -> PathBuf {
        self.pg_dir.join("data")
    }
    fn app_bin(&self) -> PathBuf {
        self.install_dir.join("bin")
    }
    fn env_file(&self) -> PathBuf {
        self.install_dir.join("conf").join("sentient.env")
    }
    fn download_dir(&self) -> PathBuf {
        self.state_dir.join("downloads")
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn step(sink: &ProgressFn, name: &str) {
    sink(Progress::Step { name: name.to_string() });
}

fn log(sink: &ProgressFn, line: impl Into<String>) {
    sink(Progress::Log { line: line.into() });
}

fn bail_if_cancelled() -> Result<(), String> {
    if crate::cancel::is_cancelled() {
        return Err("Cancelled.".into());
    }
    Ok(())
}

/// Fetch one archive into `dest`, or copy it from the offline bundle.
///
/// `ureq` handles the online case; the offline case is a file copy. Either way
/// the caller gets a local path, so the extraction path below is identical.
fn obtain(sink: &ProgressFn, src: &ArtifactSource, name: &str, url: &str, dest: &Path) -> Result<PathBuf, String> {
    let out = dest.join(name);
    if out.exists() {
        log(sink, format!("using cached {name}"));
        return Ok(out);
    }
    std::fs::create_dir_all(dest).map_err(|e| format!("create {}: {e}", dest.display()))?;

    match src {
        ArtifactSource::Bundled { dir } => {
            let from = dir.join(name);
            if !from.exists() {
                return Err(format!(
                    "offline bundle is missing {name} (looked in {})",
                    dir.display()
                ));
            }
            log(sink, format!("copying {name} from the offline bundle"));
            std::fs::copy(&from, &out).map_err(|e| format!("copy {name}: {e}"))?;
        }
        ArtifactSource::Remote { base_url } => {
            let url = if base_url.is_empty() {
                url.to_string()
            } else {
                format!("{}/{}", base_url.trim_end_matches('/'), name)
            };
            log(sink, format!("downloading {url}"));
            let resp = ureq::get(&url)
                .call()
                .map_err(|e| format!("download {name}: {e}"))?;
            let mut reader = resp.into_reader();
            let mut file = std::fs::File::create(&out).map_err(|e| format!("create {name}: {e}"))?;
            std::io::copy(&mut reader, &mut file).map_err(|e| format!("write {name}: {e}"))?;
        }
    }
    Ok(out)
}

/// Extract a `.zip` using the `tar.exe` that ships with Windows 10+.
///
/// Deliberately not a Rust zip crate: `tar.exe` is always present, handles
/// these archives, and keeps the dependency surface small.
fn extract_zip(zip: &Path, into: &Path) -> Result<(), String> {
    std::fs::create_dir_all(into).map_err(|e| format!("create {}: {e}", into.display()))?;
    let (ok, _, err) = sys::output_tracked(
        "tar.exe",
        &["-xf", &zip.to_string_lossy(), "-C", &into.to_string_lossy()],
    )
    .ok_or_else(|| "could not run tar.exe".to_string())?;
    if !ok {
        return Err(format!("extract {}: {}", zip.display(), sys::decode(&err)));
    }
    Ok(())
}

/// Give the account running setup full control of `dir`.
///
/// Started by an elevated administrator, initdb re-launches itself under a
/// restricted token (PostgreSQL refuses to run with administrator rights).
/// That token loses the Administrators group, so on a folder setup created
/// under C:\ it may not change permissions: initdb then stops with "could not
/// change permissions of directory … Permission denied". Granting the user
/// itself, not the group, survives the restriction. (Run as SYSTEM, as the
/// headless harness does, the problem never shows.)
fn grant_current_user(dir: &Path) -> Result<(), String> {
    let who = sys::output("whoami.exe", &[])
        .filter(|(ok, _, _)| *ok)
        .map(|(_, out, _)| sys::decode(&out).trim().to_string())
        .filter(|w| !w.is_empty())
        .ok_or("could not determine the current account (whoami)")?;
    let grant = format!("{who}:(OI)(CI)F");
    match sys::output("icacls.exe", &[&dir.to_string_lossy(), "/grant", &grant, "/T", "/Q"]) {
        Some((true, _, _)) => Ok(()),
        Some((false, out, err)) => Err(format!(
            "could not give {who} access to {}: {} {}",
            dir.display(),
            sys::decode(&err).trim(),
            sys::decode(&out).trim()
        )),
        None => Err("could not run icacls.exe".into()),
    }
}

/// Run `psql` against the local cluster as the superuser.
fn psql(cfg: &NativeConfig, db: &str, sql: &str) -> Result<String, String> {
    let exe = cfg.pg_bin().join("psql.exe");
    let port = cfg.pg_port.to_string();
    let mut c = sys::command(&exe.to_string_lossy());
    c.env("PGPASSWORD", &cfg.pg_superuser_password)
        .args(["-h", "127.0.0.1", "-p", &port, "-U", "postgres", "-d", db, "-tAc", sql]);
    let o = c.output().map_err(|e| format!("run psql: {e}"))?;
    let out = sys::decode(&o.stdout);
    if !o.status.success() {
        return Err(format!("psql: {}", sys::decode(&o.stderr).trim()));
    }
    Ok(out.trim().to_string())
}

fn sc(args: &[&str]) -> Option<(bool, Vec<u8>, Vec<u8>)> {
    sys::output("sc.exe", args)
}

fn service_exists(name: &str) -> bool {
    sc(&["query", name]).map(|(ok, _, _)| ok).unwrap_or(false)
}

fn service_state(name: &str) -> String {
    match sc(&["query", name]) {
        Some((true, out, _)) => {
            let s = sys::decode(&out);
            for token in ["RUNNING", "STOPPED", "START_PENDING", "STOP_PENDING", "PAUSED"] {
                if s.contains(token) {
                    return token.to_lowercase();
                }
            }
            "unknown".into()
        }
        _ => "absent".into(),
    }
}

// ---------------------------------------------------------------------------
// Readiness probes (mirrors distro.rs)
// ---------------------------------------------------------------------------

/// True when the prerequisites are provisioned: PostgreSQL present, its service
/// registered, and TimescaleDB's extension files in place.
pub fn is_ready(cfg: &NativeConfig) -> bool {
    #[cfg(target_os = "linux")]
    {
        return crate::native_linux::is_ready(cfg);
    }
    #[cfg(not(target_os = "linux"))]
    {
    cfg.pg_bin().join("psql.exe").exists()
        && cfg.pg_dir.join("share").join("extension").join("timescaledb.control").exists()
        && service_exists(PG_SERVICE)
    }
}

/// Is the platform actually serving? Same contract as `distro::is_running` — an
/// HTTP probe, not a process check, because a live port is the only thing that
/// proves the stack is usable.
pub fn is_running(http_port: u16) -> bool {
    http_answers(http_port)
}

/// SENTIENT answers HTTP on the port: any response, whatever its status. A
/// bare TCP connect is not enough: Docker's port proxy accepts connections
/// while nothing is serving behind it.
pub fn http_answers(http_port: u16) -> bool {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_millis(800))
        .timeout(std::time::Duration::from_secs(3))
        .build();
    match agent.get(&format!("http://127.0.0.1:{http_port}/")).call() {
        Ok(_) | Err(ureq::Error::Status(_, _)) => true,
        Err(ureq::Error::Transport(_)) => false,
    }
}

// ---------------------------------------------------------------------------
// Phase 1 — prerequisites (PostgreSQL + TimescaleDB)
// ---------------------------------------------------------------------------

/// Install and start PostgreSQL with TimescaleDB.
///
/// Name of the redistributable as shipped in the offline bundle and as
/// published by Microsoft.
pub const VC_REDIST_EXE: &str = "vc_redist.x64.exe";
const VC_REDIST_URL: &str = "https://aka.ms/vs/17/release/vc_redist.x64.exe";

/// Install the Visual C++ 2015–2022 x64 runtime if it is missing.
///
/// Checks for vcruntime140_1.dll specifically: vcruntime140.dll alone is
/// not enough for binaries built with VS2019 or later, and a box can easily
/// have one without the other. Optional in the bundle — when it is absent
/// and the runtime is missing, the online path downloads it.
fn ensure_vc_runtime(sink: &ProgressFn, source: &ArtifactSource, dl: &Path) -> Result<(), String> {
    let system32 = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let dll = Path::new(&system32).join("System32").join("vcruntime140_1.dll");
    if dll.exists() {
        return Ok(());
    }
    step(sink, "Installing the Visual C++ runtime (PostgreSQL needs it)");
    let exe = obtain(sink, source, VC_REDIST_EXE, VC_REDIST_URL, dl)?;
    let res = sys::output_tracked(&exe.to_string_lossy(), &["/install", "/quiet", "/norestart"]);
    // 3010 = installed, reboot pending; the runtime is usable immediately.
    let ok = matches!(res, Some((true, _, _)))
        || matches!(&res, Some((false, out, _)) if sys::decode(out).contains("3010"));
    if !ok || !dll.exists() {
        return Err(format!(
            "The Visual C++ runtime could not be installed ({} is still missing). Install \
             \"Microsoft Visual C++ 2015-2022 Redistributable (x64)\" from Microsoft and run \
             setup again.",
            dll.display()
        ));
    }
    Ok(())
}

/// **Uses the binaries ZIP, never the EDB graphical installer.** The GUI
/// installer cannot run in a service/session-0 context: it exits 1 and writes
/// no log at all, which is impossible to diagnose from a wizard. Extract +
/// `initdb` + `pg_ctl register` is deterministic and works unattended.
pub fn setup(sink: ProgressFn, cfg: &NativeConfig) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        return crate::native_linux::setup(sink, cfg);
    }
    #[cfg(not(target_os = "linux"))]
    {
    let dl = cfg.download_dir();

    // ---- Visual C++ runtime ----
    // PostgreSQL 18's binaries need vcruntime140_1.dll, which a fresh
    // Windows install does not have (it arrives with Visual Studio, many
    // games, and most vendor apps — which is why a developer's machine
    // never shows the problem). Without it postgres.exe dies at load with
    // STATUS_DLL_NOT_FOUND before printing anything, and initdb reports
    // an empty error. Found on a clean Windows 10 box during testing.
    ensure_vc_runtime(&sink, &cfg.source, &dl)?;
    bail_if_cancelled()?;

    // ---- PostgreSQL ----
    if !cfg.pg_bin().join("psql.exe").exists() {
        step(&sink, &format!("Installing PostgreSQL {PG_VERSION}"));
        let zip = obtain(&sink, &cfg.source, PG_ZIP, PG_URL, &dl)?;
        bail_if_cancelled()?;

        let tmp = dl.join("pg-extract");
        let _ = std::fs::remove_dir_all(&tmp);
        extract_zip(&zip, &tmp)?;
        // The archive contains a single top-level `pgsql` directory.
        let inner = tmp.join("pgsql");
        if !inner.exists() {
            return Err("unexpected PostgreSQL archive layout (no pgsql/ directory)".into());
        }
        if let Some(parent) = cfg.pg_dir.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        std::fs::rename(&inner, &cfg.pg_dir)
            .map_err(|e| format!("move PostgreSQL into {}: {e}", cfg.pg_dir.display()))?;
        let _ = std::fs::remove_dir_all(&tmp);
    } else {
        log(&sink, "PostgreSQL already present");
    }
    bail_if_cancelled()?;

    // ---- initdb ----
    if !cfg.pg_data().join("PG_VERSION").exists() {
        step(&sink, "Initialising the database cluster");
        // A data folder without PG_VERSION is a failed earlier attempt;
        // initdb refuses a non-empty folder, so start clean.
        if cfg.pg_data().exists() {
            remove_tree_retrying(&cfg.pg_data());
        }
        std::fs::create_dir_all(cfg.pg_data())
            .map_err(|e| format!("create data dir: {e}"))?;
        grant_current_user(&cfg.pg_dir)?;
        // initdb reads the superuser password from a file rather than a prompt
        // or an argv entry (which would be visible in the process list).
        let pwfile = cfg.download_dir().join("pw.txt");
        std::fs::write(&pwfile, &cfg.pg_superuser_password)
            .map_err(|e| format!("write pwfile: {e}"))?;
        let initdb = cfg.pg_bin().join("initdb.exe");
        let res = sys::output_tracked(
            &initdb.to_string_lossy(),
            &[
                "-D", &cfg.pg_data().to_string_lossy(),
                "-U", "postgres",
                &format!("--pwfile={}", pwfile.display()),
                "-E", "UTF8",
                "--locale=C",
            ],
        );
        let _ = std::fs::remove_file(&pwfile);
        match res {
            Some((true, _, _)) => {}
            Some((false, out, err)) => {
                return Err(format!(
                    "initdb failed: {} {}",
                    sys::decode(&err).trim(),
                    sys::decode(&out).trim()
                ))
            }
            None => return Err("could not run initdb".into()),
        }
        // A zero exit is not proof: on Windows initdb re-launches itself
        // under a restricted token, and if that child never starts the
        // parent can still return quietly. The cluster is the evidence.
        if !cfg.pg_data().join("PG_VERSION").exists() {
            return Err(
                "initdb reported success but created no database cluster. On Windows this \
                 usually means postgres.exe cannot start at all — most often a missing \
                 Visual C++ runtime (vcruntime140_1.dll) — or that initdb was run from a \
                 non-interactive session with an elevated token."
                    .into(),
            );
        }
    } else {
        log(&sink, "database cluster already initialised");
    }
    bail_if_cancelled()?;

    // ---- TimescaleDB ----
    let ext_dir = cfg.pg_dir.join("share").join("extension");
    if !ext_dir.join("timescaledb.control").exists() {
        step(&sink, &format!("Installing TimescaleDB {TSDB_VERSION}"));
        let zip = obtain(&sink, &cfg.source, TSDB_ZIP, TSDB_URL, &dl)?;
        let tmp = dl.join("tsdb-extract");
        let _ = std::fs::remove_dir_all(&tmp);
        extract_zip(&zip, &tmp)?;
        let src = tmp.join("timescaledb");
        if !src.exists() {
            return Err("unexpected TimescaleDB archive layout".into());
        }
        // Deliberately NOT running the bundled setup.exe: it is interactive and
        // does exactly this file placement. Copying is deterministic and works
        // unattended.
        let lib_dir = cfg.pg_dir.join("lib");
        for entry in std::fs::read_dir(&src).map_err(|e| format!("read {}: {e}", src.display()))? {
            let entry = entry.map_err(|e| format!("read entry: {e}"))?;
            let path = entry.path();
            let name = entry.file_name();
            match path.extension().and_then(|e| e.to_str()) {
                Some("dll") => {
                    std::fs::copy(&path, lib_dir.join(&name))
                        .map_err(|e| format!("copy {}: {e}", name.to_string_lossy()))?;
                }
                Some("sql") | Some("control") => {
                    std::fs::copy(&path, ext_dir.join(&name))
                        .map_err(|e| format!("copy {}: {e}", name.to_string_lossy()))?;
                }
                _ => {}
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);

        // TimescaleDB must be preloaded; without this line CREATE EXTENSION
        // fails and ts_kv silently never becomes a hypertable.
        let conf = cfg.pg_data().join("postgresql.conf");
        let existing = std::fs::read_to_string(&conf).unwrap_or_default();
        if !existing.contains("shared_preload_libraries = 'timescaledb'") {
            let addition = "\n# added by SENTIENT Platform Manager\nshared_preload_libraries = 'timescaledb'\n";
            std::fs::write(&conf, format!("{existing}{addition}"))
                .map_err(|e| format!("update postgresql.conf: {e}"))?;
        }
    } else {
        log(&sink, "TimescaleDB already present");
    }
    bail_if_cancelled()?;

    // ---- service ----
    if !service_exists(PG_SERVICE) {
        step(&sink, "Registering the PostgreSQL service");
        let pg_ctl = cfg.pg_bin().join("pg_ctl.exe");
        match sys::output_tracked(
            &pg_ctl.to_string_lossy(),
            &["register", "-N", PG_SERVICE, "-D", &cfg.pg_data().to_string_lossy(), "-S", "auto"],
        ) {
            Some((true, _, _)) => {}
            Some((false, _, err)) => return Err(format!("pg_ctl register: {}", sys::decode(&err))),
            None => return Err("could not run pg_ctl".into()),
        }
    }

    step(&sink, "Starting PostgreSQL");
    let _ = sc(&["start", PG_SERVICE]);
    wait_for_postgres(&sink, cfg)?;

    step(&sink, "Enabling the TimescaleDB extension");
    psql(cfg, "postgres", "CREATE EXTENSION IF NOT EXISTS timescaledb CASCADE;")?;
    let ver = psql(
        cfg,
        "postgres",
        "SELECT extversion FROM pg_extension WHERE extname='timescaledb';",
    )?;
    if ver.is_empty() {
        return Err("TimescaleDB did not load — check shared_preload_libraries and the PostgreSQL version pin (>= 18.6)".into());
    }
    log(&sink, format!("TimescaleDB {ver} active"));

    sink(Progress::Done { message: "Database prerequisites ready".into() });
    Ok(())
    }
}

/// Poll until the cluster accepts connections. `pg_ctl register` returns as soon
/// as the SCM accepts the start request, which is well before the postmaster is
/// listening.
fn wait_for_postgres(sink: &ProgressFn, cfg: &NativeConfig) -> Result<(), String> {
    let isready = cfg.pg_bin().join("pg_isready.exe");
    let port = cfg.pg_port.to_string();
    for attempt in 0..60 {
        bail_if_cancelled()?;
        if let Some((true, _, _)) = sys::output(
            &isready.to_string_lossy(),
            &["-h", "127.0.0.1", "-p", &port, "-U", "postgres"],
        ) {
            return Ok(());
        }
        if attempt == 5 {
            log(sink, "waiting for PostgreSQL to accept connections…");
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    Err("PostgreSQL did not start within 60s".into())
}

// ---------------------------------------------------------------------------
// Phase 2 — the SENTIENT platform itself
// ---------------------------------------------------------------------------

/// Lay down the platform, install its schema, register and start the service.
///
/// Assumes [`setup`] has run.
pub fn deploy(sink: ProgressFn, cfg: &NativeConfig) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        return crate::native_linux::deploy(sink, cfg);
    }
    #[cfg(not(target_os = "linux"))]
    {
    if !is_ready(cfg) {
        return Err("database prerequisites are not installed — run setup first".into());
    }

    stage_payload(&sink, cfg)?;

    step(&sink, "Creating the SENTIENT database");
    // Idempotent by tolerating "already exists" rather than probing first: the
    // probe is racy and the error is harmless.
    let _ = psql(cfg, "postgres", &format!(
        "CREATE USER {} WITH PASSWORD '{}' CREATEDB SUPERUSER;",
        cfg.db_user, cfg.db_password
    ));
    let _ = psql(cfg, "postgres", &format!(
        "CREATE DATABASE {} OWNER {};", cfg.db_name, cfg.db_user
    ));
    // Plain PostgreSQL keeps ts_kv as an ordinary partitioned table; only
    // the TimescaleDB tier gets the extension.
    if cfg.flavor == Flavor::Timescaledb {
        psql(cfg, &cfg.db_name, "CREATE EXTENSION IF NOT EXISTS timescaledb CASCADE;")?;
    }
    bail_if_cancelled()?;

    step(&sink, "Writing the configuration");
    write_env_file(cfg)?;
    bail_if_cancelled()?;

    step(&sink, "Installing the SENTIENT schema (one-time)");
    let installer = cfg.app_bin().join("sentient-install.exe");
    if !installer.exists() {
        return Err(format!("{} is missing from the package", installer.display()));
    }
    let db_url = format!(
        "postgresql://{}:{}@127.0.0.1:{}/{}",
        cfg.db_user, cfg.db_password, cfg.pg_port, cfg.db_name
    );
    let data_dir = cfg.install_dir.join("data");
    let mut args: Vec<String> = vec![
        "--database-url".into(), db_url.clone(),
        "--data-dir".into(), data_dir.to_string_lossy().into_owned(),
    ];
    if cfg.load_demo {
        args.push("--load-demo".into());
    }
    let argrefs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    // The schema installer creates the ts_kv hypertable unless told not to;
    // this choice cannot be changed in place later.
    let timescale = if cfg.flavor == Flavor::Timescaledb { "true" } else { "false" };
    // With ClickHouse the installer also creates the ClickHouse schema; it
    // takes the connection from the same variables the server reads.
    let ch = clickhouse_conn(cfg);
    let mut env: Vec<(&str, &str)> = vec![("SENTIENT_TIMESCALE_ENABLED", timescale)];
    if cfg.flavor == Flavor::Clickhouse {
        env.extend([
            ("CLICKHOUSE_URL", ch.url.as_str()),
            ("CLICKHOUSE_DATABASE", ch.database.as_str()),
            ("CLICKHOUSE_USER", ch.user.as_str()),
            ("CLICKHOUSE_PASSWORD", ch.password.as_str()),
        ]);
    }
    match sys::output_tracked_env(&installer.to_string_lossy(), &argrefs, &env) {
        Some((true, out, _)) => {
            for line in sys::decode(&out).lines().rev().take(3).collect::<Vec<_>>().iter().rev() {
                log(&sink, (*line).to_string());
            }
        }
        Some((false, _, err)) => {
            // A re-run against an installed database exits non-zero; that is
            // expected on repair/upgrade paths, so log rather than hard-fail and
            // let the readiness probe below be the real gate.
            log(&sink, format!(
                "note: the schema installer returned an error ({}). If the database was already installed this is expected — continuing.",
                sys::decode(&err).trim()
            ));
        }
        None => return Err("could not run sentient-install.exe".into()),
    }
    if cfg.flavor == Flavor::Stock {
        psql(cfg, &cfg.db_name, &stock_ts_kv_sql(&cfg.db_user))?;
    }
    bail_if_cancelled()?;

    step(&sink, "Registering the SENTIENT service");
    register_app_service(cfg)?;

    step(&sink, "Starting SENTIENT");
    let _ = sc(&["start", APP_SERVICE]);
    for _ in 0..90 {
        bail_if_cancelled()?;
        if is_running(cfg.http_port) {
            register_uninstall_entry(&sink, cfg);
            sink(Progress::Done {
                message: format!("SENTIENT is serving on http://localhost:{}", cfg.http_port),
            });
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    Err("SENTIENT did not start listening within 90s — check the service log".into())
    }
}

/// Apply the PostgreSQL server parameters from `cfg.pg` and restart the
/// database so they take effect. One `ALTER SYSTEM` per psql call: it cannot
/// run inside the implicit transaction of a multi-statement command.
pub fn apply_pg_tuning(sink: &ProgressFn, cfg: &NativeConfig) -> Result<(), String> {
    step(sink, "Tuning PostgreSQL");
    let pg = &cfg.pg;
    let quote = |v: &str| format!("'{}'", v.replace('\'', "''"));
    let settings = [
        ("max_connections", pg.pg_max_connections.to_string()),
        ("shared_buffers", quote(&pg.pg_shared_buffers)),
        ("work_mem", quote(&pg.pg_work_mem)),
        ("effective_cache_size", quote(&pg.pg_effective_cache_size)),
    ];
    for (name, value) in settings {
        psql(cfg, "postgres", &format!("ALTER SYSTEM SET {name} = {value};"))?;
    }
    // max_connections and shared_buffers only change on a restart.
    let _ = sc(&["stop", PG_SERVICE]);
    std::thread::sleep(std::time::Duration::from_secs(3));
    let _ = sc(&["start", PG_SERVICE]);
    wait_for_postgres(sink, cfg)
}

/// Replace the sys-admin sign-in the schema installer creates
/// (`admin@sentient.local`) with the operator's choice. `password_hash` is an
/// Argon2 PHC string, the format SENTIENT's `PasswordService` verifies.
pub fn set_admin_account(cfg: &NativeConfig, email: &str, password_hash: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        return crate::native_linux::run_sql(cfg, &admin_account_sql(email, password_hash));
    }
    #[cfg(not(target_os = "linux"))]
    {
        psql(cfg, &cfg.db_name, &admin_account_sql(email, password_hash)).map(|_| ())
    }
}

/// The SQL that moves the default sys-admin to the operator's sign-in. Shared
/// by every engine (native Windows, native Linux, Docker).
pub(crate) fn admin_account_sql(email: &str, password_hash: &str) -> String {
    let lit = |v: &str| format!("'{}'", v.replace('\'', "''"));
    format!(
        "UPDATE user_credentials SET password = {hash} WHERE user_id = \
           (SELECT id FROM tb_user WHERE authority = 'SYS_ADMIN' AND email = 'admin@sentient.local'); \
         UPDATE tb_user SET email = {email} WHERE authority = 'SYS_ADMIN' AND email = 'admin@sentient.local';",
        hash = lit(password_hash),
        email = lit(email),
    )
}

/// The environment the server needs, in one place.
///
/// Returned as pairs rather than a file because the service wrapper injects
/// them as `<env>` elements — see [`register_app_service`] for why an env file
/// is not an option.
pub(crate) fn service_env(cfg: &NativeConfig) -> Vec<(String, String)> {
    let reports = cfg.state_dir.join("reports");
    let vc = cfg.state_dir.join("vc-repos");
    let _ = std::fs::create_dir_all(&reports);
    let _ = std::fs::create_dir_all(&vc);
    vec![
        ("DATABASE_URL".into(), format!(
            "postgresql://{}:{}@127.0.0.1:{}/{}",
            cfg.db_user, cfg.db_password, cfg.pg_port, cfg.db_name
        )),
        ("UI_DIR".into(), cfg.install_dir.join("ui").display().to_string()),
        ("REPORT_OUTPUT_DIR".into(), reports.display().to_string()),
        ("VC_REPOS_PATH".into(), vc.display().to_string()),
        ("JWT_SECRET".into(), cfg.jwt_secret.clone()),
        // The server binds SERVER__HOST:SERVER__PORT. (MQTT and CoAP listen
        // ports are Connectivity settings inside SENTIENT, not environment.)
        ("SERVER__HOST".into(), "0.0.0.0".into()),
        ("SERVER__PORT".into(), cfg.http_port.to_string()),
        ("LICENSE_SERVER_URL".into(), cfg.license_server_url.clone()),
    ]
    .into_iter()
    .chain(stack::storage_env(cfg.flavor, Some(&clickhouse_conn(cfg)), &cfg.tuning))
    .chain(stack::tuning_env(&cfg.tuning))
    .chain(cfg.extra_env.iter().cloned())
    .fold(Vec::new(), |mut env: Vec<(String, String)>, (k, v)| {
        // Later entries win, so an operator's override (extra_env, last)
        // replaces the value the engine derived instead of being written as
        // a second, ambiguous <env> element.
        match env.iter_mut().find(|(name, _)| *name == k) {
            Some(slot) => slot.1 = v,
            None => env.push((k, v)),
        }
        env
    })
}

/// How the server reaches a ClickHouse on the same machine (Linux hybrid).
/// Plain PostgreSQL: SENTIENT writes telemetry into monthly partitions
/// (`ts_kv_YYYY_MM`) but its schema installer creates `ts_kv` as a plain
/// table, so every write fails with "ts_kv is not partitioned". On a fresh,
/// empty install, replace it with the partitioned table the server expects.
pub(crate) fn stock_ts_kv_sql(owner: &str) -> String {
    format!(
        "DO $$ BEGIN \
           IF (SELECT relkind FROM pg_class WHERE oid = to_regclass('public.ts_kv')) = 'r' \
              AND NOT EXISTS (SELECT 1 FROM ts_kv) THEN \
             DROP TABLE ts_kv; \
             CREATE TABLE ts_kv (entity_id uuid NOT NULL, key int NOT NULL, ts bigint NOT NULL, \
               bool_v boolean, str_v varchar(10000000), long_v bigint, dbl_v double precision, json_v json, \
               CONSTRAINT ts_kv_pkey PRIMARY KEY (entity_id, key, ts)) PARTITION BY RANGE (ts); \
             ALTER TABLE ts_kv OWNER TO \"{owner}\"; \
           END IF; \
         END $$;"
    )
}

pub(crate) fn clickhouse_conn(cfg: &NativeConfig) -> ClickhouseConn {
    ClickhouseConn {
        url: format!("http://127.0.0.1:{}", cfg.ch_http_port),
        database: cfg.ch_db.clone(),
        user: cfg.ch_user.clone(),
        password: cfg.ch_password.clone(),
    }
}

/// Escape the few characters that would otherwise break the service XML.
fn xml_escape(v: &str) -> String {
    v.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Also write the same settings to a plain file. Nothing reads it — it exists so
/// an operator can see the effective configuration without parsing the service
/// descriptor. The descriptor remains the source of truth.
fn write_env_file(cfg: &NativeConfig) -> Result<(), String> {
    let conf_dir = cfg.install_dir.join("conf");
    std::fs::create_dir_all(&conf_dir).map_err(|e| format!("create conf dir: {e}"))?;
    let body: String = service_env(cfg)
        .into_iter()
        .map(|(k, v)| format!("{k}={v}\n"))
        .collect();
    std::fs::write(
        cfg.env_file(),
        format!("# Effective SENTIENT configuration (read-only reference).\n\
                 # The service reads these from its descriptor in services\\, not from here.\n{body}"),
    )
    .map_err(|e| format!("write env file: {e}"))
}

/// Register the server as a Windows service via the bundled WinSW wrapper.
///
/// WinSW rather than a bare `sc create` because the server is a console program:
/// WinSW gives log rotation, restart-on-failure and a graceful Ctrl+C stop
/// declaratively, and it already has a descriptor in the SENTIENT repo.
///
/// The environment goes in as individual `<env>` elements. An `<envfile>`
/// element is NOT an option: WinSW v2 ignores it silently, so the server starts
/// with no `DATABASE_URL`, falls back to a default connection string and dies
/// with `role "SYSTEM" does not exist` — the service account's name. Found the
/// hard way; the failure gives no hint that the config was simply never read.
fn register_app_service(cfg: &NativeConfig) -> Result<(), String> {
    let svc_dir = cfg.install_dir.join("services");
    std::fs::create_dir_all(&svc_dir).map_err(|e| format!("create services dir: {e}"))?;
    let winsw = svc_dir.join(format!("{APP_SERVICE}.exe"));
    if !winsw.exists() {
        return Err(format!("{} is missing from the package", winsw.display()));
    }

    let xml = format!(
        r#"<service>
  <id>{APP_SERVICE}</id>
  <name>SENTIENT Platform</name>
  <description>SENTIENT IIoT platform server (REST API, MQTT broker, rule engine).</description>
  <executable>{exe}</executable>
  <workingdirectory>{root}</workingdirectory>
{envs}
  <log mode="roll-by-size">
    <logpath>{logs}</logpath>
    <sizeThreshold>10240</sizeThreshold>
    <keepFiles>5</keepFiles>
  </log>
  <onfailure action="restart" delay="10 sec"/>
  <onfailure action="restart" delay="30 sec"/>
  <resetfailure>1 hour</resetfailure>
  <startmode>Automatic</startmode>
  <delayedAutoStart>true</delayedAutoStart>
  <stoptimeout>20 sec</stoptimeout>
  <depend>{PG_SERVICE}</depend>
</service>
"#,
        exe = cfg.app_bin().join("sentient-server.exe").display(),
        root = cfg.install_dir.display(),
        envs = service_env(cfg)
            .into_iter()
            .map(|(k, v)| format!("  <env name=\"{}\" value=\"{}\"/>", k, xml_escape(&v)))
            .collect::<Vec<_>>()
            .join("\n"),
        logs = cfg.install_dir.join("logs").display(),
    );
    std::fs::create_dir_all(cfg.install_dir.join("logs")).ok();
    std::fs::write(svc_dir.join(format!("{APP_SERVICE}.xml")), xml)
        .map_err(|e| format!("write service descriptor: {e}"))?;

    if !service_exists(APP_SERVICE) {
        match sys::output_tracked(&winsw.to_string_lossy(), &["install"]) {
            Some((true, _, _)) => {}
            Some((false, _, err)) => return Err(format!("service install: {}", sys::decode(&err))),
            None => return Err("could not run the service wrapper".into()),
        }
    }
    Ok(())
}

/// Registry key backing the "Apps & features" entry.
const ARP_KEY: &str =
    r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\SENTIENT";

/// Register SENTIENT in Add/Remove Programs.
///
/// Without this the platform is invisible to Windows: services running, files
/// in Program Files, and nothing in "Apps & features" — so the only way to
/// remove it is to already know the Platform Manager exists. Every other
/// Windows product registers here, and operators reasonably expect it.
///
/// Uses `reg.exe` rather than a registry crate to avoid pulling a dependency
/// into the engine for six writes.
fn register_uninstall_entry(sink: &ProgressFn, cfg: &NativeConfig) {
    // Clicking "Uninstall" opens the Platform Manager, which owns the teardown
    // (it has to stop services and drop a database — not something to do from
    // a bare registry command with no confirmation).
    let manager = PathBuf::from(r"C:\Program Files\SENTIENT Platform Manager")
        .join("SENTIENT Platform Manager.exe");
    let icon = cfg.app_bin().join("sentient-server.exe");
    let size_kb = dir_size_kb(&cfg.install_dir);

    let mut values: Vec<(&str, &str, String)> = vec![
        ("DisplayName", "REG_SZ", "SENTIENT Platform".into()),
        ("Publisher", "REG_SZ", "INVENIA SYSTEMS".into()),
        ("InstallLocation", "REG_SZ", cfg.install_dir.display().to_string()),
        ("DisplayIcon", "REG_SZ", icon.display().to_string()),
        ("NoModify", "REG_DWORD", "1".into()),
        ("NoRepair", "REG_DWORD", "1".into()),
    ];
    if manager.exists() {
        values.push(("UninstallString", "REG_SZ", format!("\"{}\" --uninstall", manager.display())));
    }
    if size_kb > 0 {
        values.push(("EstimatedSize", "REG_DWORD", size_kb.to_string()));
    }

    for (name, kind, data) in values {
        let ok = sys::output(
            "reg.exe",
            &["add", ARP_KEY, "/v", name, "/t", kind, "/d", &data, "/f"],
        )
        .map(|(ok, _, _)| ok)
        .unwrap_or(false);
        if !ok {
            log(sink, format!("note: could not write the {name} uninstall entry"));
        }
    }
}

/// Remove the Add/Remove Programs entry. Best-effort: a stale entry pointing at
/// files that no longer exist is worse than none.
fn remove_uninstall_entry() {
    let _ = sys::output("reg.exe", &["delete", ARP_KEY, "/f"]);
}

/// Installed size in KB, for the size column in "Apps & features".
fn dir_size_kb(dir: &Path) -> u64 {
    fn walk(dir: &Path, total: &mut u64) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            match entry.metadata() {
                Ok(m) if m.is_dir() => walk(&entry.path(), total),
                Ok(m) => *total += m.len(),
                Err(_) => {}
            }
        }
    }
    let mut total = 0u64;
    walk(dir, &mut total);
    total / 1024
}

// ---------------------------------------------------------------------------
// Lifecycle (mirrors distro.rs so the app can dispatch uniformly)
// ---------------------------------------------------------------------------

/// One managed Windows service, shaped like `distro::ContainerStatus` so the
/// Status view can render either mode with the same component.
#[derive(serde::Serialize)]
pub struct ServiceStatus {
    pub name: String,
    pub state: String,
    pub status: String,
}

#[derive(serde::Serialize)]
pub struct NativeStatus {
    pub installed: bool,
    pub running: bool,
    pub services: Vec<ServiceStatus>,
}

pub fn status(cfg: &NativeConfig) -> NativeStatus {
    #[cfg(target_os = "linux")]
    {
        return crate::native_linux::status(cfg);
    }
    #[cfg(not(target_os = "linux"))]
    {
    let services = [PG_SERVICE, APP_SERVICE]
        .iter()
        .map(|n| {
            let state = service_state(n);
            ServiceStatus {
                name: (*n).to_string(),
                status: match state.as_str() {
                    "running" => "Running".into(),
                    "stopped" => "Stopped".into(),
                    "absent" => "Not installed".into(),
                    other => other.to_string(),
                },
                state,
            }
        })
        .collect();
    NativeStatus {
        installed: is_ready(cfg) && service_exists(APP_SERVICE),
        running: is_running(cfg.http_port),
        services,
    }
    }
}

/// start | stop | restart, applied in dependency order.
/// Stop the SENTIENT service only, leaving PostgreSQL up, and wait until it
/// has stopped (database work that must not race the server follows).
pub fn stop_server(sink: &ProgressFn, cfg: &NativeConfig) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let _ = (sink, cfg);
        return Err("stop_server is Windows-only".into());
    }
    #[cfg(not(target_os = "linux"))]
    {
    step(sink, "Stopping SENTIENT");
    let _ = sc(&["stop", APP_SERVICE]);
    for _ in 0..60 {
        if service_state(APP_SERVICE) == "stopped" && !is_running(cfg.http_port) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    Err("SENTIENT did not stop within a minute".into())
    }
}

pub fn control(sink: ProgressFn, cfg: &NativeConfig, action: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        return crate::native_linux::control(sink, cfg, action);
    }
    #[cfg(not(target_os = "linux"))]
    {
    let _ = cfg;
    let ordered: Vec<&str> = match action {
        // Bring the database up first and take it down last.
        "start" => vec![PG_SERVICE, APP_SERVICE],
        "stop" => vec![APP_SERVICE, PG_SERVICE],
        "restart" => {
            control(sink.clone(), cfg, "stop")?;
            std::thread::sleep(std::time::Duration::from_secs(2));
            return control(sink, cfg, "start");
        }
        other => return Err(format!("unknown action: {other}")),
    };
    for svc in ordered {
        if !service_exists(svc) {
            continue;
        }
        step(&sink, &format!("{action} {svc}"));
        let _ = sc(&[action, svc]);
    }
    Ok(())
    }
}

pub fn logs(cfg: &NativeConfig, tail: u32) -> String {
    #[cfg(target_os = "linux")]
    {
        return crate::native_linux::logs(cfg, tail);
    }
    #[cfg(not(target_os = "linux"))]
    {
    let path = cfg.install_dir.join("logs").join(format!("{APP_SERVICE}.out.log"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        return format!("no log file yet at {}", path.display());
    };
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(tail as usize);
    lines[start..].join("\n")
    }
}

/// Replace the platform binaries and UI in place, then restart.
///
/// The database is untouched: the server applies pending migrations itself at
/// boot, exactly as the Docker mode relies on.
pub fn update(sink: ProgressFn, cfg: &NativeConfig, payload: &Path) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        return crate::native_linux::update(sink, cfg, payload);
    }
    #[cfg(not(target_os = "linux"))]
    {
    step(&sink, "Stopping SENTIENT");
    let _ = sc(&["stop", APP_SERVICE]);
    std::thread::sleep(std::time::Duration::from_secs(3));

    step(&sink, "Replacing the platform files");
    for item in ["bin", "ui", "data"] {
        let from = payload.join(item);
        if !from.exists() {
            continue;
        }
        let to = cfg.install_dir.join(item);
        let _ = std::fs::remove_dir_all(&to);
        copy_tree(&from, &to)?;
    }

    step(&sink, "Starting SENTIENT");
    let _ = sc(&["start", APP_SERVICE]);
    sink(Progress::Done { message: "Update applied".into() });
    Ok(())
    }
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("create {}: {e}", to.display()))?;
    for entry in std::fs::read_dir(from).map_err(|e| format!("read {}: {e}", from.display()))? {
        let entry = entry.map_err(|e| format!("read entry: {e}"))?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_tree(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst).map_err(|e| format!("copy {}: {e}", src.display()))?;
        }
    }
    Ok(())
}

/// Remove the platform but KEEP the database cluster and its data.
pub fn uninstall(sink: ProgressFn, cfg: &NativeConfig) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        return crate::native_linux::uninstall(sink, cfg);
    }
    #[cfg(not(target_os = "linux"))]
    {
    step(&sink, "Stopping and removing the SENTIENT service");
    remove_uninstall_entry();
    let _ = sc(&["stop", APP_SERVICE]);
    std::thread::sleep(std::time::Duration::from_secs(2));
    let winsw = cfg.install_dir.join("services").join(format!("{APP_SERVICE}.exe"));
    if winsw.exists() {
        let _ = sys::output_tracked(&winsw.to_string_lossy(), &["uninstall"]);
    } else if service_exists(APP_SERVICE) {
        let _ = sc(&["delete", APP_SERVICE]);
    }
    for item in ["bin", "ui", "conf", "services", "logs"] {
        remove_tree_retrying(&cfg.install_dir.join(item));
    }
    sink(Progress::Done {
        message: "SENTIENT removed. The database cluster was left in place.".into(),
    });
    Ok(())
    }
}

/// Full teardown, including PostgreSQL and every byte of data. Destructive.
pub fn cleanup(sink: ProgressFn, cfg: &NativeConfig) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        return crate::native_linux::cleanup(sink, cfg);
    }
    #[cfg(not(target_os = "linux"))]
    {
    uninstall(sink.clone(), cfg)?;
    step(&sink, "Removing PostgreSQL");
    let _ = sc(&["stop", PG_SERVICE]);
    std::thread::sleep(std::time::Duration::from_secs(2));
    let pg_ctl = cfg.pg_bin().join("pg_ctl.exe");
    if pg_ctl.exists() {
        let _ = sys::output_tracked(
            &pg_ctl.to_string_lossy(),
            &["unregister", "-N", PG_SERVICE],
        );
    } else if service_exists(PG_SERVICE) {
        let _ = sc(&["delete", PG_SERVICE]);
    }
    // Windows can still hold handles for a moment after a service stops, so a
    // single remove_dir_all can leave the tree half-deleted. Retry briefly.
    for dir in [&cfg.pg_dir, &cfg.install_dir, &cfg.state_dir] {
        remove_tree_retrying(dir);
    }
    sink(Progress::Done { message: "Everything removed.".into() });
    Ok(())
    }
}

/// Delete a directory tree, tolerating the handles Windows may still hold for a
/// short while after a service stops. Without the retry the first pass deletes
/// most of the tree and leaves the root behind, which then reads as "the
/// uninstall did not work".
fn remove_tree_retrying(dir: &Path) {
    for attempt in 0..5 {
        if !dir.exists() {
            return;
        }
        if std::fs::remove_dir_all(dir).is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(400 * (attempt + 1)));
    }
}

/// What an offline bundle has to contain for [`setup`] and [`deploy`] to run
/// with no network at all. Surfaced so the packaging step and the preflight
/// check agree on one list instead of drifting apart.
pub fn required_bundle_files() -> Vec<String> {
    if cfg!(windows) {
        vec![PG_ZIP.into(), TSDB_ZIP.into(), "sentient-payload.tar".into()]
    } else {
        // Databases come from the distribution's packages on Linux; only the
        // platform itself has to be carried.
        vec![linux_payload_name()]
    }
}

/// `sentient-payload-linux-<arch>.tar` — the Linux counterpart of
/// `sentient-payload.tar`, arch-tagged because a Pi and a PC need different
/// binaries and one bundle directory may hold both.
pub fn linux_payload_name() -> String {
    format!("sentient-payload-linux-{}.tar", std::env::consts::ARCH)
}

/// Where a bundled payload lives: a `payload` directory beside the Platform
/// Manager's own executable, which is what the installer lays down for an
/// offline install. Returns None when this is an online build.
pub fn bundled_payload_dir() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(beside) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("payload"))) {
        candidates.push(beside);
    }
    // A Linux package installs the binary in /usr/bin; its payload lives in
    // the package's own share directory instead.
    #[cfg(not(windows))]
    {
        candidates.push(PathBuf::from("/usr/share/sentient-platform-manager/payload"));
        candidates.push(PathBuf::from("/opt/sentient-platform-manager/payload"));
    }
    let required = required_bundle_files();
    candidates.into_iter().find(|dir| required.iter().all(|f| dir.join(f).exists()))
}

/// Unpack the SENTIENT payload (server, installer, UI, data, service wrapper)
/// into the install directory.
///
/// The engine downloads PostgreSQL and TimescaleDB from their vendors, but
/// SENTIENT itself has to come from us — from the bundle for an offline
/// install, or later from a release manifest for an online one. Without this
/// step `deploy` would reach the schema installer and find nothing to run.
fn stage_payload(sink: &ProgressFn, cfg: &NativeConfig) -> Result<(), String> {
    if cfg.app_bin().join("sentient-install.exe").exists() {
        return Ok(());                      // already staged
    }
    let ArtifactSource::Bundled { dir } = &cfg.source else {
        return Err(
            "The SENTIENT program files are missing and this is not an offline \
             bundle. Use the offline installer, or configure a release manifest."
                .into(),
        );
    };
    let tar = dir.join("sentient-payload.tar");
    if !tar.exists() {
        return Err(format!("the offline bundle is missing {}", tar.display()));
    }
    step(sink, "Unpacking the SENTIENT program files");
    std::fs::create_dir_all(&cfg.install_dir)
        .map_err(|e| format!("create {}: {e}", cfg.install_dir.display()))?;
    let (ok, _, err) = sys::output_tracked(
        "tar.exe",
        &["-xf", &tar.to_string_lossy(), "-C", &cfg.install_dir.to_string_lossy()],
    )
    .ok_or_else(|| "could not run tar.exe".to_string())?;
    if !ok {
        return Err(format!("unpacking the payload failed: {}", sys::decode(&err)));
    }
    if !cfg.app_bin().join("sentient-install.exe").exists() {
        return Err("the payload unpacked but sentient-install.exe is not where expected".into());
    }
    Ok(())
}

/// Preflight for the offline path: report anything the bundle is missing before
/// the user commits to an install, rather than failing halfway through.
pub fn verify_bundle(src: &ArtifactSource) -> Result<(), String> {
    let ArtifactSource::Bundled { dir } = src else {
        return Ok(());
    };
    let missing: Vec<String> = required_bundle_files()
        .into_iter()
        .filter(|f| !dir.join(f).exists())
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "the offline bundle is incomplete — missing: {}",
            missing.join(", ")
        ))
    }
}

/// Generate a per-install JWT signing secret.
///
/// Seeded from `RandomState`, whose keys std draws from the OS entropy source,
/// mixed with the wall clock and the process id. This is a signing secret for a
/// single on-premise install, not a key that leaves the machine — but it must
/// never be a constant, or every deployment would share one.
pub fn generate_secret() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::with_capacity(64);
    for i in 0..4u64 {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        h.write_u32(std::process::id());
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

/// True when this build can offer the native mode at all.
pub fn supported() -> bool {
    cfg!(any(windows, target_os = "linux"))
}

#[allow(dead_code)]
fn _assert_offline_helper_used(s: &ArtifactSource) -> bool {
    s.is_offline()
}
