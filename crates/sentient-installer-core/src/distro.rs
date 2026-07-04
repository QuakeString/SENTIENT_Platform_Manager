//! Phase 2: the dedicated `sentient` WSL distro + Docker Engine.
//! Downloads an Ubuntu rootfs, `wsl --import`s it, enables systemd, installs
//! Docker Engine inside it, and verifies. Idempotent — re-running repairs.

use std::path::Path;

use crate::progress::{Progress, ProgressFn};
#[cfg(windows)]
use crate::sys;

pub const DISTRO: &str = "sentient";
#[cfg_attr(not(windows), allow(dead_code))]
const ROOTFS_URL: &str =
    "https://cloud-images.ubuntu.com/wsl/releases/24.04/current/ubuntu-noble-wsl-amd64-24.04lts.rootfs.tar.gz";

/// Is the distro present AND Docker responding?
pub fn is_ready() -> bool {
    #[cfg(windows)]
    {
        distro_present()
            && sys::output("wsl.exe", &["-d", DISTRO, "-u", "root", "--", "docker", "version"])
                .map(|(ok, _, _)| ok)
                .unwrap_or(false)
    }
    #[cfg(not(windows))]
    false
}

#[cfg_attr(not(windows), allow(dead_code))]
const COMPOSE_PATH: &str = "/opt/sentient/docker-compose.yml";

/// User-chosen deploy parameters. Defaults match the reference compose; the
/// wizard shows these as "recommended" and lets the user customize them.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct DeployConfig {
    pub db_name: String,
    pub db_user: String,
    pub db_password: String,
    pub http_port: u16,
    pub mqtt_port: u16,
    pub coap_port: u16,
    pub load_demo: bool,
}

impl Default for DeployConfig {
    fn default() -> Self {
        Self {
            db_name: "sentient".into(),
            db_user: "sentient".into(),
            db_password: "sentient".into(),
            http_port: 8080,
            mqtt_port: 1883,
            coap_port: 5683,
            load_demo: false,
        }
    }
}

/// The SENTIENT stack — a trimmed, image-pull version of the reference compose
/// (no `build:`, no host-specific bind mounts). `@@TOKENS@@` are filled from the
/// user's `DeployConfig`. The container's internal port stays 8080; only the
/// published host port varies.
#[cfg_attr(not(windows), allow(dead_code))]
const COMPOSE_TEMPLATE: &str = r#"services:
  postgres:
    image: "timescale/timescaledb:2.26.1-pg18"
    container_name: sentient-postgres
    restart: always
    command: ["postgres","-c","shared_preload_libraries=timescaledb","-c","max_connections=100","-c","shared_buffers=128MB"]
    environment:
      POSTGRES_DB: @@DB_NAME@@
      POSTGRES_USER: @@DB_USER@@
      POSTGRES_PASSWORD: @@DB_PASS@@
      PGDATA: /var/lib/postgresql/data
    volumes:
      - postgres_data:/var/lib/postgresql
    healthcheck:
      test: ["CMD-SHELL","pg_isready -U @@DB_USER@@ -d @@DB_NAME@@"]
      interval: 10s
      timeout: 5s
      retries: 12
  sentient:
    image: "quakestring/sentient:latest"
    container_name: sentient
    restart: always
    depends_on:
      postgres:
        condition: service_healthy
    ports:
      - "@@HTTP_PORT@@:8080"
      - "@@MQTT_PORT@@:1883"
      - "@@COAP_PORT@@:5683/udp"
    extra_hosts:
      - "host.docker.internal:host-gateway"
    environment:
      DATABASE_URL: postgresql://@@DB_USER@@:@@DB_PASS@@@postgres:5432/@@DB_NAME@@
      POSTGRES_USER: @@DB_USER@@
      POSTGRES_PASSWORD: @@DB_PASS@@
      POSTGRES_DB: @@DB_NAME@@
      DATABASE_POOL_MAX: "24"
      DATABASE_POOL_MIN: "8"
      TS_TYPE: sql
      RUST_LOG: info,sentient_api=info
      HOST: "0.0.0.0"
      PORT: "8080"
      SENTIENT_INSTANCE_ID: "sentient-docker-01"
      LICENSE_SERVER_URL: "https://license.invenia.in"
      SENTIENT_SERVICE_ID: "st-bsmpl-1"
      VC_REPOS_PATH: /var/lib/sentient/vc-repos
      REPORT_OUTPUT_DIR: /var/lib/sentient/reports
    volumes:
      - sentient_data:/var/lib/sentient
volumes:
  postgres_data:
  sentient_data:
"#;

#[cfg_attr(not(windows), allow(dead_code))]
fn compose(cfg: &DeployConfig) -> String {
    COMPOSE_TEMPLATE
        .replace("@@DB_NAME@@", &cfg.db_name)
        .replace("@@DB_USER@@", &cfg.db_user)
        .replace("@@DB_PASS@@", &cfg.db_password)
        .replace("@@HTTP_PORT@@", &cfg.http_port.to_string())
        .replace("@@MQTT_PORT@@", &cfg.mqtt_port.to_string())
        .replace("@@COAP_PORT@@", &cfg.coap_port.to_string())
}

/// Is the SENTIENT web server answering on the published HTTP port inside the
/// distro? (The Windows host reaches the same port via WSL localhost-forwarding.)
pub fn is_running(http_port: u16) -> bool {
    #[cfg(windows)]
    {
        let cmd = format!("curl -s -o /dev/null -w '%{{http_code}}' http://localhost:{http_port}");
        sys::output("wsl.exe", &["-d", DISTRO, "-u", "root", "--", "bash", "-lc", &cmd])
            .map(|(_, out, _)| {
                let c = sys::decode(&out);
                let c = c.trim();
                c.len() == 3 && c != "000"
            })
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        let _ = http_port;
        false
    }
}

/// Phase 3: write the compose into the distro, pull images, install the schema,
/// start the stack, and wait for the web server to answer. Cancellable.
pub fn deploy(sink: ProgressFn, cfg: &DeployConfig) -> Result<(), String> {
    #[cfg(windows)]
    {
        macro_rules! bail_if_cancelled {
            () => {
                if crate::cancel::is_cancelled() {
                    return Err("Cancelled.".into());
                }
            };
        }

        sink(Progress::Step { name: "Writing the SENTIENT configuration".into() });
        let script = format!(
            "mkdir -p /opt/sentient && cat > {COMPOSE_PATH} <<'SENTIENTEOF'\n{}\nSENTIENTEOF\n",
            compose(cfg)
        );
        indistro(&sink, &script)?;
        bail_if_cancelled!();

        sink(Progress::Step { name: "Pulling SENTIENT images (first time, a few minutes)".into() });
        indistro_stream(&sink, &format!("docker compose -f {COMPOSE_PATH} pull"))?;
        bail_if_cancelled!();

        // The SENTIENT server validates the DB schema at boot and REFUSES to
        // start on a fresh/empty database (with `restart: always` it would then
        // crash-loop and never answer). Run the one-time installer first —
        // `compose run` brings up postgres via depends_on, installs the schema +
        // system resources, then exits. On a re-run against an already-installed
        // DB the installer exits non-zero ("refuses to overwrite an existing
        // schema"); that's expected, so we log and carry on rather than
        // hard-fail. The readiness probe below is the real gate.
        let demo = if cfg.load_demo { " -e LOAD_DEMO=true" } else { "" };
        sink(Progress::Step { name: "Installing the SENTIENT database (first run, one-time)".into() });
        if let Err(e) = indistro_stream(&sink, &format!(
            "docker compose -f {COMPOSE_PATH} run --rm -e INSTALL_SENTIENT=true{demo} sentient"
        )) {
            bail_if_cancelled!();
            sink(Progress::Log { line: format!(
                "note: install step returned an error ({e}). If the database was already installed this is expected — continuing."
            ) });
        }
        bail_if_cancelled!();

        sink(Progress::Step { name: "Starting SENTIENT".into() });
        indistro_stream(&sink, &format!("docker compose -f {COMPOSE_PATH} up -d"))?;

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
    #[cfg(not(windows))]
    {
        let _ = cfg;
        sink(Progress::Error { message: "Deploy is Windows-only.".into() });
        Err("Windows only".into())
    }
}

/// Tear down a partial/cancelled deploy: stop & remove the SENTIENT containers,
/// network and volumes, and reclaim disk from half-pulled images. Leaves the WSL
/// distro + Docker Engine in place so re-running the deploy starts clean. Every
/// sub-command is best-effort (the stack may not exist yet), so this never fails
/// hard on a missing container/compose file.
pub fn cleanup(sink: ProgressFn) -> Result<(), String> {
    #[cfg(windows)]
    {
        sink(Progress::Step { name: "Stopping and removing SENTIENT containers and volumes".into() });
        let _ = indistro_stream(&sink, &format!(
            "docker compose -f {COMPOSE_PATH} down -v --remove-orphans 2>/dev/null || true"
        ));
        // Belt-and-suspenders in case the containers exist without the compose file.
        let _ = indistro_stream(&sink,
            "docker rm -f sentient sentient-postgres 2>/dev/null || true");

        sink(Progress::Step { name: "Reclaiming disk from partial image pulls".into() });
        let _ = indistro_stream(&sink, "docker image prune -f 2>/dev/null || true");

        sink(Progress::Done { message: "Cleanup complete — you can safely retry the install.".into() });
        Ok(())
    }
    #[cfg(not(windows))]
    {
        sink(Progress::Error { message: "Cleanup is Windows-only.".into() });
        Err("Windows only".into())
    }
}

/// Full Phase-2 setup. `install_dir` is where the distro's disk lives.
pub fn setup(sink: ProgressFn, install_dir: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        std::fs::create_dir_all(install_dir).map_err(|e| e.to_string())?;

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

        sink(Progress::Step { name: "Enabling systemd".into() });
        indistro(&sink, r"printf '[boot]\nsystemd=true\n' > /etc/wsl.conf")?;
        // restart the distro so systemd becomes PID 1
        let _ = sys::output("wsl.exe", &["--terminate", DISTRO]);

        sink(Progress::Step { name: "Installing Docker Engine (a few minutes)".into() });
        indistro_stream(&sink, "command -v docker >/dev/null 2>&1 || (curl -fsSL https://get.docker.com | sh)")?;

        sink(Progress::Step { name: "Starting Docker".into() });
        indistro(&sink, "systemctl enable --now docker")?;

        if is_ready() {
            sink(Progress::Done { message: "Docker Engine is installed and running.".into() });
            Ok(())
        } else {
            Err("Docker was installed but isn't responding yet — try the step again.".into())
        }
    }
    #[cfg(not(windows))]
    {
        let _ = install_dir;
        sink(Progress::Error { message: "Docker setup is Windows-only.".into() });
        Err("Windows only".into())
    }
}

// ============================ manage (M3) ====================================

#[derive(serde::Serialize)]
pub struct ContainerStatus {
    pub name: String,
    pub state: String,  // running | exited | created | …
    pub status: String, // human ("Up 3 minutes", "Exited (0) 1 min ago")
}

#[derive(serde::Serialize)]
pub struct StackStatus {
    pub installed: bool, // distro present AND a compose file written
    pub running: bool,   // the `sentient` container is up
    pub containers: Vec<ContainerStatus>,
}

/// Snapshot of the deployed stack — used by the Status section.
pub fn status() -> StackStatus {
    #[cfg(windows)]
    {
        let present = distro_present();
        let installed = present
            && sys::output("wsl.exe", &["-d", DISTRO, "-u", "root", "--", "bash", "-lc",
                &format!("test -f {COMPOSE_PATH}")])
                .map(|(ok, _, _)| ok)
                .unwrap_or(false);

        let mut containers = Vec::new();
        if present {
            if let Some((_, out, _)) = sys::output("wsl.exe", &[
                "-d", DISTRO, "-u", "root", "--", "docker", "ps", "-a",
                "--filter", "name=sentient",
                "--format", "{{.Names}}|{{.State}}|{{.Status}}",
            ]) {
                for line in sys::decode(&out).lines() {
                    let parts: Vec<&str> = line.trim().splitn(3, '|').collect();
                    if parts.len() == 3 && !parts[0].is_empty() {
                        containers.push(ContainerStatus {
                            name: parts[0].to_string(),
                            state: parts[1].to_string(),
                            status: parts[2].to_string(),
                        });
                    }
                }
            }
        }
        let running = containers.iter().any(|c| c.name == "sentient" && c.state == "running");
        StackStatus { installed, running, containers }
    }
    #[cfg(not(windows))]
    {
        StackStatus { installed: false, running: false, containers: Vec::new() }
    }
}

/// Start / stop / restart the deployed stack. `action` ∈ start|stop|restart.
pub fn control(sink: ProgressFn, action: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        let (label, sub) = match action {
            "start" => ("Starting SENTIENT", "up -d"),
            "stop" => ("Stopping SENTIENT", "stop"),
            "restart" => ("Restarting SENTIENT", "restart"),
            _ => return Err(format!("unknown action: {action}")),
        };
        sink(Progress::Step { name: label.into() });
        indistro_stream(&sink, &format!("docker compose -f {COMPOSE_PATH} {sub}"))?;
        sink(Progress::Done { message: format!("{label} — done.") });
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = action;
        sink(Progress::Error { message: "Stack control is Windows-only.".into() });
        Err("Windows only".into())
    }
}

/// The last `tail` lines of the combined container logs.
pub fn logs(tail: u32) -> String {
    #[cfg(windows)]
    {
        sys::output("wsl.exe", &["-d", DISTRO, "-u", "root", "--", "bash", "-lc",
            &format!("docker compose -f {COMPOSE_PATH} logs --no-color --tail {tail} 2>&1")])
            .map(|(_, out, _)| sys::decode(&out))
            .unwrap_or_else(|| "Could not read logs.".into())
    }
    #[cfg(not(windows))]
    {
        let _ = tail;
        String::new()
    }
}

/// Update the stack: pull the latest images, apply DB migrations, restart.
/// Follows the reference upgrade flow (stop server → migrate → start) so a
/// running server never races the migration. Cancellable.
pub fn update(sink: ProgressFn) -> Result<(), String> {
    #[cfg(windows)]
    {
        macro_rules! bail_if_cancelled {
            () => { if crate::cancel::is_cancelled() { return Err("Cancelled.".into()); } };
        }

        sink(Progress::Step { name: "Pulling the latest SENTIENT images".into() });
        indistro_stream(&sink, &format!("docker compose -f {COMPOSE_PATH} pull"))?;
        bail_if_cancelled!();

        // Stop the server so it doesn't run against the DB during migration
        // (postgres stays up).
        sink(Progress::Step { name: "Stopping the server for migration".into() });
        let _ = indistro_stream(&sink, &format!("docker compose -f {COMPOSE_PATH} stop sentient"));
        bail_if_cancelled!();

        // Apply schema migrations for the new image. Refuses (non-zero) if the
        // jump has breaking changes — surface that so the user can act.
        sink(Progress::Step { name: "Applying database migrations (if any)".into() });
        if let Err(e) = indistro_stream(&sink, &format!(
            "docker compose -f {COMPOSE_PATH} run --rm -e UPGRADE_SENTIENT=true sentient"
        )) {
            bail_if_cancelled!();
            return Err(format!(
                "Migration step failed: {e}. If it reported breaking changes, review the log and run the upgrade manually."
            ));
        }
        bail_if_cancelled!();

        sink(Progress::Step { name: "Starting SENTIENT on the new image".into() });
        indistro_stream(&sink, &format!("docker compose -f {COMPOSE_PATH} up -d"))?;
        sink(Progress::Done { message: "SENTIENT updated to the latest image.".into() });
        Ok(())
    }
    #[cfg(not(windows))]
    {
        sink(Progress::Error { message: "Update is Windows-only.".into() });
        Err("Windows only".into())
    }
}

// ---- helpers (Windows) -------------------------------------------------------

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
            emit(sink, &out);
            emit(sink, &err);
            if ok { Ok(()) } else { Err(format!("wsl {} failed", args.join(" "))) }
        }
        None => Err("could not run wsl.exe".into()),
    }
}

/// Run a bash command inside the distro as root (output is UTF-8), error on fail.
#[cfg(windows)]
fn indistro(sink: &ProgressFn, bash: &str) -> Result<(), String> {
    match sys::output("wsl.exe", &["-d", DISTRO, "-u", "root", "--", "bash", "-lc", bash]) {
        Some((ok, out, err)) => {
            emit(sink, &out);
            emit(sink, &err);
            if ok { Ok(()) } else { Err(format!("in-distro command failed: {bash}")) }
        }
        None => Err("could not run wsl.exe".into()),
    }
}

/// Like `indistro`, but streams stdout/stderr line-by-line live (for long steps
/// like the Docker install).
#[cfg(windows)]
fn indistro_stream(sink: &ProgressFn, bash: &str) -> Result<(), String> {
    use std::io::BufRead;
    use std::process::Stdio;
    let mut child = sys::command("wsl.exe")
        .args(["-d", DISTRO, "-u", "root", "--", "bash", "-lc", bash])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    crate::cancel::register_pid(child.id());
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let (s1, s2) = (sink.clone(), sink.clone());
    let t1 = std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
            s1(Progress::Log { line });
        }
    });
    let t2 = std::thread::spawn(move || {
        for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok) {
            s2(Progress::Log { line });
        }
    });
    let status = child.wait().map_err(|e| e.to_string())?;
    let _ = t1.join();
    let _ = t2.join();
    crate::cancel::clear_pid();
    if crate::cancel::is_cancelled() {
        Err("Cancelled.".into())
    } else if status.success() {
        Ok(())
    } else {
        Err(format!("in-distro command failed: {bash}"))
    }
}

#[cfg(windows)]
fn emit(sink: &ProgressFn, bytes: &[u8]) {
    for line in sys::decode(bytes).lines() {
        let l = line.trim();
        if !l.is_empty() {
            sink(Progress::Log { line: l.into() });
        }
    }
}

/// Download a URL to a file, reporting percent progress.
#[cfg(windows)]
fn download(url: &str, dest: &Path, sink: &ProgressFn) -> Result<(), String> {
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
