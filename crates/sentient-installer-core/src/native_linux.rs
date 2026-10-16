//! Native Linux deployment — PostgreSQL (and ClickHouse for the hybrid
//! flavour) from the distribution's packages, SENTIENT as a systemd unit
//! under `/opt/sentient`, no containers.
//!
//! Same surface as [`crate::native`] (which delegates here on Linux) and
//! [`crate::distro`], so the app dispatches on the chosen mode
//! without special-casing. Everything that writes outside the user's home
//! runs as root through [`crate::exec::run_privileged`] — one script per
//! phase, so polkit asks for the password once per phase rather than once
//! per command.
//!
//! Why distribution packages rather than shipped binaries: Linux has no
//! vendor "binaries zip" the way Windows does, the distribution's PostgreSQL
//! is what the rest of the system (backups, monitoring, updates) expects, and
//! it keeps the offline bundle down to the platform itself. TimescaleDB is
//! not reliably packaged by distributions, so the Linux native tiers are
//! stock PostgreSQL and PostgreSQL + ClickHouse; the wizard hides the third.
//! ClickHouse comes from the vendor's apt/yum repositories where those exist
//! and from the vendor's static binary elsewhere (Arch, openSUSE).

use std::path::{Path, PathBuf};

use crate::exec::{self, shq};
use crate::native::{ArtifactSource, NativeConfig, NativeStatus, ServiceStatus};
use crate::progress::{Progress, ProgressFn};
use crate::stack;
use crate::sys;

const APP_UNIT: &str = "sentient";
const PG_UNIT: &str = "postgresql";
const CH_UNIT: &str = "clickhouse-server";
const APP_USER: &str = "sentient";
const ENV_FILE: &str = "/etc/sentient/sentient.env";
const UNIT_FILE: &str = "/etc/systemd/system/sentient.service";
const LOG_DIR: &str = "/var/log/sentient";

/// ClickHouse version installed natively — the same 24.8 LTS line the Docker
/// engine pins (`clickhouse/clickhouse-server:24.8`). Newer majors changed the
/// HTTP compression framing and SENTIENT's client fails its health check
/// ("decompression error: incorrect magic number"); seen with 26.9.
pub const CH_VERSION: &str = "24.8.14.39";

fn step(sink: &ProgressFn, name: &str) {
    sink(Progress::Step { name: name.into() });
}
fn log(sink: &ProgressFn, line: impl Into<String>) {
    sink(Progress::Log { line: line.into() });
}
fn bail_if_cancelled() -> Result<(), String> {
    if crate::cancel::is_cancelled() { Err("Cancelled.".into()) } else { Ok(()) }
}

/// Which package manager family this machine has.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pm { Pacman, Apt, Dnf, Zypper }

fn package_manager() -> Option<Pm> {
    if exec::have("pacman") { Some(Pm::Pacman) }
    else if exec::have("apt-get") { Some(Pm::Apt) }
    else if exec::have("dnf") { Some(Pm::Dnf) }
    else if exec::have("zypper") { Some(Pm::Zypper) }
    else { None }
}

fn unit_exists(unit: &str) -> bool {
    sys::output("systemctl", &["cat", &format!("{unit}.service")]).map(|(ok, _, _)| ok).unwrap_or(false)
}

fn unit_state(unit: &str) -> String {
    match sys::output("systemctl", &["is-active", &format!("{unit}.service")]) {
        Some((_, out, _)) => {
            let s = String::from_utf8_lossy(&out).trim().to_string();
            match s.as_str() {
                "active" => "running".into(),
                "inactive" | "failed" | "deactivating" | "activating" => "stopped".into(),
                _ if !unit_exists(unit) => "absent".into(),
                other => other.to_string(),
            }
        }
        None => "unknown".into(),
    }
}

// ---------------------------------------------------------------------------
// Readiness
// ---------------------------------------------------------------------------

/// PostgreSQL (and ClickHouse when chosen) installed as system services.
pub fn is_ready(cfg: &NativeConfig) -> bool {
    exec::have("psql") && unit_exists(PG_UNIT)
        && (!cfg.flavor.uses_clickhouse() || unit_exists(CH_UNIT))
}

// ---------------------------------------------------------------------------
// Phase 1 — prerequisites
// ---------------------------------------------------------------------------

/// Install and start the databases from the distribution, apply the
/// PostgreSQL tuning and port, and (hybrid) the ClickHouse drop-in.
pub fn setup(sink: ProgressFn, cfg: &NativeConfig) -> Result<(), String> {
    if cfg.flavor.uses_timescaledb() {
        return Err(
            "TimescaleDB is not packaged by Linux distributions. Choose “PostgreSQL + ClickHouse” or \
             “PostgreSQL only” for a native Linux install, or use the Docker mode for TimescaleDB."
                .into(),
        );
    }
    let pm = package_manager().ok_or("No supported package manager found (apt, pacman, dnf, zypper).")?;
    let ch = cfg.flavor.uses_clickhouse();

    step(&sink, if ch { "Installing PostgreSQL and ClickHouse from your distribution" } else { "Installing PostgreSQL from your distribution" });
    let install = match pm {
        // PostgreSQL from the distribution. ClickHouse is appended below,
        // always from the vendor's static build (pinned; see CH_VERSION).
        Pm::Pacman => format!(
            "pacman -Sy --noconfirm --needed postgresql{curl}\n\
             DATA=/var/lib/postgres/data\n\
             [ -f \"$DATA/PG_VERSION\" ] || runuser -u postgres -- initdb -D \"$DATA\" --locale=C.UTF-8 --encoding=UTF8 >/dev/null\n",
            curl = if ch { " curl" } else { "" }
        ),
        Pm::Apt => format!(
            "export DEBIAN_FRONTEND=noninteractive\napt-get update -qq\napt-get install -y -qq postgresql{curl}\n",
            curl = if ch { " curl ca-certificates" } else { "" }
        ),
        Pm::Dnf => format!(
            "dnf install -y postgresql-server postgresql{curl}\n\
             [ -f /var/lib/pgsql/data/PG_VERSION ] || postgresql-setup --initdb >/dev/null\n",
            curl = if ch { " curl" } else { "" }
        ),
        Pm::Zypper => format!(
            "zypper --non-interactive install postgresql-server postgresql{curl}\n",
            curl = if ch { " curl" } else { "" }
        ),
    };
    let install = if ch { format!("{install}{}", clickhouse_static_install()) } else { install };

    // Tuning and port through ALTER SYSTEM (postgresql.auto.conf): the same
    // statement on every distribution, whatever their config layout. Reached
    // through the socket on the current port, then restarted onto the new one.
    // One `-c` per statement: ALTER SYSTEM refuses to run inside the single
    // transaction psql wraps a multi-statement `-c` in.
    let mut alter = format!("-c {}", shq(&format!("ALTER SYSTEM SET port = {};", cfg.pg_port)));
    for (k, v) in cfg.pg.settings() {
        alter.push_str(&format!(" -c {}", shq(&format!("ALTER SYSTEM SET {k} = '{v}';"))));
    }
    alter.push_str(&format!(" -c {}", shq("ALTER SYSTEM SET listen_addresses = 'localhost';")));

    // `--no-block`: a container or a box with a stuck dependency can leave
    // `systemctl start` waiting on a job; the readiness loop is the real gate.
    // Up to 4 minutes: the distribution's unit waits for network-online, and
    // where systemd-networkd is enabled but manages nothing (many servers,
    // every container) that target only arrives after wait-online's own
    // 2-minute timeout.
    let mut script = format!(
        "set -e\n{install}\
         systemctl enable --no-block --now {PG_UNIT}\n\
         echo '>> Waiting for PostgreSQL to start (can take two minutes while the system waits for the network)'\n\
         for i in $(seq 1 240); do runuser -u postgres -- pg_isready -q && break; sleep 1; done\n\
         runuser -u postgres -- psql -v ON_ERROR_STOP=1 -qAt {alter}\n\
         systemctl restart --no-block {PG_UNIT}\n\
         sleep 2\n\
         for i in $(seq 1 240); do runuser -u postgres -- pg_isready -q -p {port} && break; sleep 1; done\n\
         runuser -u postgres -- pg_isready -p {port}\n",
        port = cfg.pg_port,
    );

    if ch {
        let (http, tcp) = (
            (cfg.ch_http_port != 8123).then_some(cfg.ch_http_port),
            (cfg.ch_native_port != 9000).then_some(cfg.ch_native_port),
        );
        let config = stack::clickhouse_config_xml(&cfg.ch, http, tcp);
        // The `default` user needs access management to create the SENTIENT
        // user; the container image gets this from an environment variable.
        let access = "<clickhouse><users><default><access_management>1</access_management><named_collection_control>1</named_collection_control></default></users></clickhouse>\n";
        script.push_str(&format!(
            "mkdir -p /etc/clickhouse-server/config.d /etc/clickhouse-server/users.d\n\
             cat > /etc/clickhouse-server/config.d/sentient.xml <<'SENTIENTEOF'\n{config}SENTIENTEOF\n\
             cat > /etc/clickhouse-server/users.d/sentient-access.xml <<'SENTIENTEOF'\n{access}SENTIENTEOF\n\
             systemctl enable --no-block {CH_UNIT}\n\
             systemctl restart --no-block {CH_UNIT}\n\
             for i in $(seq 1 90); do clickhouse-client --port {tcp} -q 'SELECT 1' >/dev/null 2>&1 && break; sleep 1; done\n\
             clickhouse-client --port {tcp} -q 'SELECT version()'\n",
            tcp = cfg.ch_native_port,
        ));
    }
    exec::run_privileged(&sink, &script)?;
    bail_if_cancelled()?;

    if is_ready(cfg) {
        sink(Progress::Done { message: if ch {
            "PostgreSQL and ClickHouse are installed and running.".into()
        } else {
            "PostgreSQL is installed and running.".into()
        } });
        Ok(())
    } else {
        Err("The database packages installed but their services are not registered — check the log above.".into())
    }
}

/// ClickHouse from the vendor's static build, pinned to [`CH_VERSION`] on
/// every distribution: Arch has no package at all, and the vendor's apt/yum
/// repositories track the newest release, which SENTIENT's client cannot
/// talk to. `clickhouse install` lays out the user, directories and default
/// configuration exactly as the deb/rpm do; the unit file is the one those
/// packages ship, written here because the tarball has none. Downloaded
/// under /var/lib rather than /tmp, which is often mounted noexec.
fn clickhouse_static_install() -> String {
    let arch = match std::env::consts::ARCH { "aarch64" => "arm64", _ => "amd64" };
    let tgz = format!("clickhouse-common-static-{CH_VERSION}-{arch}.tgz");
    let url = format!("https://packages.clickhouse.com/tgz/stable/{tgz}");
    let unit = "[Unit]\nDescription=ClickHouse Server\nAfter=network-online.target\nWants=network-online.target\n\n\
                [Service]\nType=simple\nUser=clickhouse\nGroup=clickhouse\n\
                ExecStart=/usr/bin/clickhouse-server --config=/etc/clickhouse-server/config.xml --pid-file=/run/clickhouse-server/clickhouse-server.pid\n\
                RuntimeDirectory=clickhouse-server\nRestart=always\nRestartSec=30\n\
                LimitCORE=infinity\nLimitNOFILE=500000\n\
                CapabilityBoundingSet=CAP_NET_ADMIN CAP_IPC_LOCK CAP_SYS_NICE CAP_NET_BIND_SERVICE\n\
                AmbientCapabilities=CAP_NET_ADMIN CAP_IPC_LOCK CAP_SYS_NICE CAP_NET_BIND_SERVICE\n\n\
                [Install]\nWantedBy=multi-user.target\n";
    format!(
        "if ! command -v clickhouse-server >/dev/null 2>&1; then\n\
           echo '>> Downloading ClickHouse {CH_VERSION} (vendor static build)'\n\
           tmp=/var/lib/sentient/downloads/clickhouse && rm -rf \"$tmp\" && mkdir -p \"$tmp\" && cd \"$tmp\"\n\
           curl -fsSL -o {tgz} {url}\n\
           tar -xzf {tgz}\n\
           ./clickhouse-common-static-{CH_VERSION}/usr/bin/clickhouse install --noninteractive --binary-path /usr/bin --config-path /etc/clickhouse-server \\\n\
             --log-path /var/log/clickhouse-server --data-path /var/lib/clickhouse --pid-path /run/clickhouse-server \\\n\
             --user clickhouse --group clickhouse >/dev/null\n\
           cd / && rm -rf \"$tmp\"\n\
         fi\n\
         [ -f /etc/systemd/system/{CH_UNIT}.service ] || [ -f /usr/lib/systemd/system/{CH_UNIT}.service ] || cat > /etc/systemd/system/{CH_UNIT}.service <<'SENTIENTEOF'\n{unit}SENTIENTEOF\n\
         systemctl daemon-reload\n"
    )
}

// ---------------------------------------------------------------------------
// Phase 2 — the platform
// ---------------------------------------------------------------------------

/// Where the payload tar is before the privileged step unpacks it.
fn obtain_payload(sink: &ProgressFn, cfg: &NativeConfig) -> Result<PathBuf, String> {
    let name = crate::native::linux_payload_name();
    match &cfg.source {
        ArtifactSource::Bundled { dir } => {
            let tar = dir.join(&name);
            if tar.exists() { Ok(tar) } else { Err(format!("the offline bundle is missing {}", tar.display())) }
        }
        ArtifactSource::Remote { base_url } if !base_url.is_empty() => {
            let dl = std::env::temp_dir().join("sentient-spm");
            std::fs::create_dir_all(&dl).map_err(|e| format!("create {}: {e}", dl.display()))?;
            let dest = dl.join(&name);
            step(sink, "Downloading the SENTIENT program files");
            crate::distro::download(&format!("{}/{name}", base_url.trim_end_matches('/')), &dest, sink)?;
            Ok(dest)
        }
        ArtifactSource::Remote { .. } => Err(
            "The SENTIENT program files are missing and this is not an offline bundle. Use the offline \
             installer, or configure a release manifest.".into(),
        ),
    }
}

fn env_file_body(cfg: &NativeConfig) -> String {
    let mut body = String::from("# SENTIENT service environment — read by systemd (EnvironmentFile=).\n# Written by the SENTIENT Platform Manager; edit and `systemctl restart sentient`.\n");
    for (k, v) in crate::native::service_env(cfg) {
        // systemd's EnvironmentFile takes shell-like quoting.
        body.push_str(&format!("{k}={}\n", shq(&v)));
    }
    body
}

fn unit_body(cfg: &NativeConfig) -> String {
    let ch = cfg.flavor.uses_clickhouse();
    let deps = if ch { format!("{PG_UNIT}.service {CH_UNIT}.service") } else { format!("{PG_UNIT}.service") };
    format!(
        "[Unit]\n\
         Description=SENTIENT IIoT platform\n\
         After=network-online.target {deps}\n\
         Wants=network-online.target\n\
         Requires={deps}\n\n\
         [Service]\n\
         Type=simple\n\
         User={APP_USER}\n\
         Group={APP_USER}\n\
         EnvironmentFile={ENV_FILE}\n\
         WorkingDirectory={root}\n\
         ExecStart={exe}\n\
         Restart=on-failure\n\
         RestartSec=10\n\
         LimitNOFILE=65536\n\
         StandardOutput=append:{LOG_DIR}/sentient.log\n\
         StandardError=inherit\n\
         AmbientCapabilities=CAP_NET_BIND_SERVICE\n\n\
         [Install]\n\
         WantedBy=multi-user.target\n",
        root = cfg.install_dir.display(),
        exe = cfg.install_dir.join("bin").join("sentient-server").display(),
    )
}

/// SQL that creates the role and database, tolerant of both existing.
fn pg_provision_sql(cfg: &NativeConfig) -> (String, String) {
    (
        format!("CREATE USER {} WITH PASSWORD '{}' CREATEDB SUPERUSER;", cfg.db_user, cfg.db_password),
        format!("CREATE DATABASE {} OWNER {};", cfg.db_name, cfg.db_user),
    )
}

fn ch_provision_sql(cfg: &NativeConfig) -> String {
    format!(
        "CREATE USER IF NOT EXISTS {u} IDENTIFIED WITH sha256_password BY '{p}'; \
         CREATE DATABASE IF NOT EXISTS {d}; \
         GRANT ALL ON {d}.* TO {u};",
        u = cfg.ch_user, p = cfg.ch_password, d = cfg.ch_db
    )
}

/// Lay down the platform, create the database, install the schema, register
/// and start the unit. Assumes [`setup`] has run.
/// Run SQL against the SENTIENT database as the postgres superuser.
pub(crate) fn run_sql(cfg: &NativeConfig, sql: &str) -> Result<(), String> {
    let script = format!(
        "runuser -u postgres -- psql -p {} -d {} -v ON_ERROR_STOP=1 -qAt -c {}",
        cfg.pg_port,
        shq(&cfg.db_name),
        shq(sql)
    );
    exec::run_privileged(&crate::progress::noop(), &script)
}

pub fn deploy(sink: ProgressFn, cfg: &NativeConfig) -> Result<(), String> {
    if !is_ready(cfg) {
        return Err("database prerequisites are not installed — run setup first".into());
    }
    let install_dir = cfg.install_dir.display().to_string();
    let state_dir = cfg.state_dir.display().to_string();
    let staged = cfg.install_dir.join("bin").join("sentient-install").exists();
    let payload = if staged { None } else { Some(obtain_payload(&sink, cfg)?) };
    bail_if_cancelled()?;

    let (create_user, create_db) = pg_provision_sql(cfg);
    let db_url = format!("postgresql://{}:{}@127.0.0.1:{}/{}", cfg.db_user, cfg.db_password, cfg.pg_port, cfg.db_name);
    let demo = if cfg.load_demo { " --load-demo" } else { "" };

    step(&sink, "Installing the SENTIENT program files and service");
    let mut script = format!(
        "set -e\n\
         id -u {APP_USER} >/dev/null 2>&1 || useradd --system --home-dir {state} --shell \"$(command -v nologin || echo /bin/false)\" {APP_USER}\n\
         mkdir -p {inst} {state}/reports {state}/vc-repos {LOG_DIR} /etc/sentient\n",
        inst = shq(&install_dir), state = shq(&state_dir),
    );
    if let Some(tar) = &payload {
        script.push_str(&format!(
            "tar -xf {} -C {}\n", shq(&tar.display().to_string()), shq(&install_dir)
        ));
    }
    script.push_str(&format!(
        "chmod 755 {inst}/bin/*\n\
         if [ -n \"$(ls -A {inst}/fonts 2>/dev/null)\" ]; then mkdir -p /usr/share/fonts/truetype/sentient && cp -f {inst}/fonts/* /usr/share/fonts/truetype/sentient/ && (fc-cache -f >/dev/null 2>&1 || true); fi\n\
         chown -R {APP_USER}:{APP_USER} {state} {LOG_DIR}\n\
         chmod 755 {LOG_DIR}\n\
         [ -f {inst}/bin/sentient-install ] || {{ echo 'the payload unpacked but bin/sentient-install is not where expected' >&2; exit 1; }}\n\
         echo '>> Creating the SENTIENT database'\n\
         runuser -u postgres -- psql -p {pgport} -qAt -c {cu} 2>/dev/null || true\n\
         runuser -u postgres -- psql -p {pgport} -qAt -c {cd} 2>/dev/null || true\n",
        inst = shq(&install_dir), state = shq(&state_dir), pgport = cfg.pg_port,
        cu = shq(&create_user), cd = shq(&create_db),
    ));
    if cfg.flavor.uses_clickhouse() {
        script.push_str(&format!(
            "clickhouse-client --port {} -n -q {}\n", cfg.ch_native_port, shq(&ch_provision_sql(cfg))
        ));
    }
    script.push_str(&format!(
        "echo '>> Writing the configuration'\n\
         cat > {ENV_FILE} <<'SENTIENTEOF'\n{env}SENTIENTEOF\n\
         chown root:{APP_USER} {ENV_FILE} && chmod 640 {ENV_FILE}\n\
         echo '>> Installing the SENTIENT schema (one-time)'\n\
         set -a; . {ENV_FILE}; set +a\n\
         if ! {inst}/bin/sentient-install --database-url {url} --data-dir {inst}/data{demo} 2>&1 | tail -n 3; then\n\
           echo '>> note: the schema installer returned an error. If the database was already installed this is expected — continuing.'\n\
         fi\n\
         echo '>> Registering the SENTIENT service'\n\
         cat > {UNIT_FILE} <<'SENTIENTEOF'\n{unit}SENTIENTEOF\n\
         systemctl daemon-reload\n\
         systemctl enable {APP_UNIT} >/dev/null 2>&1\n\
         systemctl restart {APP_UNIT}\n",
        env = env_file_body(cfg), unit = unit_body(cfg), inst = shq(&install_dir), url = shq(&db_url),
    ));
    exec::run_privileged(&sink, &script)?;
    bail_if_cancelled()?;

    step(&sink, "Starting SENTIENT");
    for _ in 0..90 {
        bail_if_cancelled()?;
        if crate::native::is_running(cfg.http_port) {
            sink(Progress::Done { message: format!("SENTIENT is serving on http://localhost:{}", cfg.http_port) });
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    log(&sink, logs(cfg, 20));
    Err("SENTIENT did not start listening within 90s — check the service log".into())
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

pub fn status(cfg: &NativeConfig) -> NativeStatus {
    let mut units = vec![PG_UNIT];
    if cfg.flavor.uses_clickhouse() {
        units.push(CH_UNIT);
    }
    units.push(APP_UNIT);
    let services = units
        .iter()
        .map(|u| {
            let state = unit_state(u);
            ServiceStatus {
                name: format!("{u}.service"),
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
        installed: Path::new(UNIT_FILE).exists() && cfg.install_dir.join("bin").join("sentient-server").exists(),
        running: crate::native::is_running(cfg.http_port),
        services,
    }
}

/// start | stop | restart. systemd orders the databases through `Requires=`.
pub fn control(sink: ProgressFn, _cfg: &NativeConfig, action: &str) -> Result<(), String> {
    if !matches!(action, "start" | "stop" | "restart") {
        return Err(format!("unknown action: {action}"));
    }
    step(&sink, &format!("{action} {APP_UNIT}"));
    exec::run_privileged(&sink, &format!("systemctl {action} {APP_UNIT}"))?;
    sink(Progress::Done { message: format!("{action} — done.") });
    Ok(())
}

/// The service log — the file the unit appends to, or the journal when the
/// file is not readable from this account.
pub fn logs(_cfg: &NativeConfig, tail: u32) -> String {
    let path = format!("{LOG_DIR}/sentient.log");
    if let Ok(text) = std::fs::read_to_string(&path) {
        let lines: Vec<&str> = text.lines().collect();
        let start = lines.len().saturating_sub(tail as usize);
        return lines[start..].join("\n");
    }
    sys::output("journalctl", &["-u", APP_UNIT, "-n", &tail.to_string(), "--no-pager", "-o", "cat"])
        .filter(|(ok, _, _)| *ok)
        .map(|(_, out, _)| String::from_utf8_lossy(&out).into_owned())
        .unwrap_or_else(|| format!("no log yet at {path}"))
}

/// Replace the platform files from a new payload tar and restart. The
/// database is untouched: the server applies pending migrations at boot.
pub fn update(sink: ProgressFn, cfg: &NativeConfig, payload: &Path) -> Result<(), String> {
    let inst = shq(&cfg.install_dir.display().to_string());
    step(&sink, "Replacing the platform files");
    exec::run_privileged(&sink, &format!(
        "set -e\nsystemctl stop {APP_UNIT} || true\n\
         for d in bin ui data fonts; do rm -rf {inst}/$d; done\n\
         tar -xf {} -C {inst}\nchmod 755 {inst}/bin/*\n\
         systemctl start {APP_UNIT}\n",
        shq(&payload.display().to_string())
    ))?;
    sink(Progress::Done { message: "Update applied".into() });
    Ok(())
}

/// Remove the platform but KEEP the databases and their data.
pub fn uninstall(sink: ProgressFn, cfg: &NativeConfig) -> Result<(), String> {
    let inst = shq(&cfg.install_dir.display().to_string());
    step(&sink, "Stopping and removing the SENTIENT service");
    exec::run_privileged(&sink, &format!(
        "systemctl disable --now {APP_UNIT} >/dev/null 2>&1 || true\n\
         rm -f {UNIT_FILE}\nsystemctl daemon-reload\n\
         for d in bin ui data fonts conf; do rm -rf {inst}/$d; done\n\
         rmdir {inst} 2>/dev/null || true\n\
         rm -rf /etc/sentient\n"
    ))?;
    sink(Progress::Done { message: "SENTIENT removed. The databases were left in place.".into() });
    Ok(())
}

/// Full teardown: the platform, its database and user, its state and logs.
/// The PostgreSQL/ClickHouse *packages* stay — they are the system's, and
/// may serve other software. Destructive.
pub fn cleanup(sink: ProgressFn, cfg: &NativeConfig) -> Result<(), String> {
    uninstall(sink.clone(), cfg)?;
    step(&sink, "Dropping the SENTIENT database and data");
    let state = shq(&cfg.state_dir.display().to_string());
    let mut script = format!(
        "runuser -u postgres -- psql -p {p} -qAt -c 'DROP DATABASE IF EXISTS {db};' 2>/dev/null || true\n\
         runuser -u postgres -- psql -p {p} -qAt -c 'DROP USER IF EXISTS {u};' 2>/dev/null || true\n",
        p = cfg.pg_port, db = cfg.db_name, u = cfg.db_user
    );
    if cfg.flavor.uses_clickhouse() {
        script.push_str(&format!(
            "clickhouse-client --port {} -n -q 'DROP DATABASE IF EXISTS {}; DROP USER IF EXISTS {};' 2>/dev/null || true\n",
            cfg.ch_native_port, cfg.ch_db, cfg.ch_user
        ));
    }
    script.push_str(&format!(
        "rm -rf {state} {LOG_DIR}\nuserdel {APP_USER} 2>/dev/null || true\n"
    ));
    exec::run_privileged(&sink, &script)?;
    sink(Progress::Done { message: "Everything removed. The PostgreSQL and ClickHouse packages were left installed.".into() });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stack::{ChTuning, Flavor, PgTuning, Tuning};

    fn cfg() -> NativeConfig {
        NativeConfig {
            install_dir: PathBuf::from("/opt/sentient"),
            pg_dir: PathBuf::new(),
            state_dir: PathBuf::from("/var/lib/sentient"),
            db_name: "sentient".into(), db_user: "sentient".into(), db_password: "pw".into(),
            pg_superuser_password: String::new(),
            http_port: 8080, mqtt_port: 1883, coap_port: 5683, pg_port: 5432,
            load_demo: false,
            source: ArtifactSource::Remote { base_url: String::new() },
            license_server_url: "https://license.invenia.in".into(),
            extra_env: Vec::new(),
            jwt_secret: "s".into(),
            flavor: Flavor::Clickhouse,
            tuning: Tuning::default(), pg: PgTuning::default(), ch: ChTuning::default(),
            ch_db: "sentient".into(), ch_user: "sentient".into(), ch_password: "pw".into(),
            ch_http_port: 8123, ch_native_port: 9000,
        }
    }

    #[test]
    fn unit_requires_both_databases_for_hybrid() {
        let u = unit_body(&cfg());
        assert!(u.contains("Requires=postgresql.service clickhouse-server.service"));
        assert!(u.contains("ExecStart=/opt/sentient/bin/sentient-server"));
        let mut c = cfg();
        c.flavor = Flavor::Stock;
        assert!(unit_body(&c).contains("Requires=postgresql.service\n"));
    }

    #[test]
    fn env_file_carries_port_and_hybrid_settings() {
        let e = env_file_body(&cfg());
        assert!(e.contains("SERVER__PORT='8080'"));
        assert!(e.contains("CLICKHOUSE_URL='http://127.0.0.1:8123'"));
        assert!(e.contains("TS_TYPE='clickhouse'"));
        assert!(e.contains("DB_POOL_PRIMARY_MAX='48'"));
        assert!(!e.contains("HTTP_PORT="));
    }
}
