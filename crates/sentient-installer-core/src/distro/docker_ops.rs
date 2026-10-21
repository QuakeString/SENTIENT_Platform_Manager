// Copyright © 2016-2026 The SENTIENT Authors

//! Looking after a Docker installation beyond deploying it: what migration
//! and the safe update need. The stack's passwords (kept only in its compose
//! file), SQL in the PostgreSQL container, stopping SENTIENT alone, the
//! client tools for backups, and the safe update itself.
//!
//! The safe update mirrors the Windows one (native::safe_update): the image
//! upgrades the schema itself when it starts, so the database is copied
//! before the new image ever runs, and the previous image is kept under a
//! `:spm-previous` tag to go back to (`rollback`).

use super::{
    pull_images, run, run_output, run_stream, shell_args, shell_quote, write_host_file, write_stack_files,
    DeployConfig, is_running,
};
use crate::native::safe_update::{RollbackPoint, breaking_changes, last_lines, quote_ident, snapshot_time};
use crate::progress::{Progress, ProgressFn};
use crate::sys;

/// The stack's passwords, read back from its compose file.
#[derive(Debug, Clone)]
pub struct StackSecrets {
    pub db_password: String,
    pub ch_password: String,
}

fn step(sink: &ProgressFn, name: &str) {
    sink(Progress::Step { name: name.into() });
}

fn log(sink: &ProgressFn, line: impl Into<String>) {
    sink(Progress::Log { line: line.into() });
}

/// Run a shell command where Docker runs; Ok(stdout) or Err(stderr).
fn capture(bash: &str) -> Result<String, String> {
    let (prog, args) = shell_args(bash);
    let argrefs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    match sys::output(&prog, &argrefs) {
        Some((true, out, _)) => Ok(sys::decode(&out)),
        Some((false, out, err)) => {
            let e = sys::decode(&err);
            Err(if e.trim().is_empty() { sys::decode(&out).trim().to_string() } else { e.trim().to_string() })
        }
        None => Err(format!("could not run {prog}")),
    }
}

/// The value of a `KEY: "value"` line, YAML double-quote escapes undone.
fn yaml_value(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    let line = text.lines().map(str::trim).find(|l| l.starts_with(&prefix))?;
    let raw = line[prefix.len()..].trim();
    let inner = raw.strip_prefix('"').and_then(|r| r.strip_suffix('"')).unwrap_or(raw);
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

pub fn read_secrets(cfg: &DeployConfig) -> Result<StackSecrets, String> {
    let text = match capture(&format!("cat {}", shell_quote(&cfg.compose_path()))) {
        Ok(text) => text,
        // The compose file is gone (what a repair may be for): the database
        // containers still carry the passwords they were created with.
        Err(e) => return container_secrets(cfg).ok_or_else(|| format!("read the stack's compose file: {e}")),
    };
    Ok(StackSecrets {
        db_password: yaml_value(&text, "POSTGRES_PASSWORD").ok_or("the compose file has no POSTGRES_PASSWORD")?,
        ch_password: yaml_value(&text, "CLICKHOUSE_PASSWORD").unwrap_or_default(),
    })
}

/// A variable from a container's environment, running or stopped.
fn container_env(container: &str, key: &str) -> Option<String> {
    let out = capture(&format!(
        "docker inspect --format '{{{{range .Config.Env}}}}{{{{println .}}}}{{{{end}}}}' {}",
        shell_quote(container)
    ))
    .ok()?;
    let prefix = format!("{key}=");
    out.lines().find_map(|l| l.strip_prefix(&prefix)).map(str::to_string)
}

fn container_secrets(cfg: &DeployConfig) -> Option<StackSecrets> {
    let names = cfg.names();
    Some(StackSecrets {
        db_password: container_env(&names.pg, "POSTGRES_PASSWORD").filter(|p| !p.is_empty())?,
        ch_password: if cfg.flavor.uses_clickhouse() {
            container_env(&names.ch, "CLICKHOUSE_PASSWORD").unwrap_or_default()
        } else {
            String::new()
        },
    })
}

/// A file from SENTIENT's container, running or stopped; `None` if it is not
/// there.
pub fn read_app_file(cfg: &DeployConfig, path: &str) -> Result<Option<String>, String> {
    let dir = private_temp_dir()?;
    let out = capture(&format!(
        "docker cp {}:{} {} 2>&1",
        shell_quote(&cfg.names().app),
        shell_quote(path),
        shell_quote(&dir.join("f").display().to_string())
    ));
    let text = std::fs::read_to_string(dir.join("f")).ok();
    let _ = std::fs::remove_dir_all(&dir);
    match (text, out) {
        (Some(t), _) => Ok(Some(t)),
        (None, Err(e)) if e.contains("Could not find the file") || e.contains("No such") => Ok(None),
        (None, Err(e)) => Err(format!("read {path} from SENTIENT's container: {e}")),
        (None, Ok(_)) => Ok(None),
    }
}

/// Write a file into SENTIENT's container (running or stopped), owned by
/// root and readable by it only.
pub fn write_app_file(cfg: &DeployConfig, path: &str, text: &str) -> Result<(), String> {
    let (parent, name) = path.rsplit_once('/').ok_or_else(|| format!("{path} is not an absolute path"))?;
    let dir = private_temp_dir()?;
    let file = dir.join(name);
    let result = (|| {
        std::fs::write(&file, text).map_err(|e| format!("write {}: {e}", file.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600));
        }
        // Through tar, so the file is root's in the container rather than
        // this user's id.
        capture(&format!(
            "tar -C {} --owner=0 --group=0 --numeric-owner -cf - {} | docker cp - {}:{}",
            shell_quote(&dir.display().to_string()),
            shell_quote(name),
            shell_quote(&cfg.names().app),
            shell_quote(if parent.is_empty() { "/" } else { parent })
        ))
        .map(|_| ())
        .map_err(|e| format!("write {path} into SENTIENT's container: {e}"))
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// A temporary folder only this user can read.
fn private_temp_dir() -> Result<std::path::PathBuf, String> {
    let dir = std::env::temp_dir().join(format!("spm-{}-{}", std::process::id(), crate::native::generate_secret().get(..8).unwrap_or("x")));
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(dir)
}

/// `cfg` with the passwords from the running stack, for anything that
/// rewrites the compose file or signs in to the databases.
pub fn with_secrets(cfg: &DeployConfig) -> Result<DeployConfig, String> {
    let s = read_secrets(cfg)?;
    let mut full = cfg.clone();
    full.db_password = s.db_password;
    if !s.ch_password.is_empty() {
        full.ch_password = s.ch_password;
    }
    Ok(full)
}

/// SQL in the PostgreSQL container, as the SENTIENT user (a superuser).
pub fn psql(cfg: &DeployConfig, db: &str, sql: &str) -> Result<String, String> {
    capture(&format!(
        "docker exec -i {} psql -U {} -d {} -v ON_ERROR_STOP=1 -tAc {}",
        cfg.names().pg,
        shell_quote(&cfg.db_user),
        shell_quote(db),
        shell_quote(sql)
    ))
    .map(|s| s.trim().to_string())
    .map_err(|e| format!("psql: {e}"))
}

/// Stop the SENTIENT container only; the databases keep running.
pub fn stop_app(sink: &ProgressFn, cfg: &DeployConfig) -> Result<(), String> {
    step(sink, "Stopping SENTIENT");
    run(sink, &cfg.compose_cmd("stop sentient"))?;
    for _ in 0..30 {
        if !is_running(cfg.http_port) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    Err("SENTIENT did not stop within 30 seconds".into())
}

/// Write `cfg`'s compose file and (re)create the PostgreSQL container alone,
/// e.g. on the TimescaleDB image before a migration to TimescaleDB, then
/// wait until it accepts connections.
pub fn recreate_postgres(sink: &ProgressFn, cfg: &DeployConfig) -> Result<(), String> {
    step(sink, "Starting PostgreSQL for the new database type");
    write_stack_files(sink, cfg)?;
    run_stream(sink, &cfg.compose_cmd("up -d --no-deps postgres"))?;
    for _ in 0..60 {
        let ready = run_output(&format!(
            "docker exec {} pg_isready -U {} -d {}",
            cfg.names().pg,
            shell_quote(&cfg.db_user),
            shell_quote(&cfg.db_name)
        ))
        .map(|(ok, _)| ok)
        .unwrap_or(false);
        if ready {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    Err("PostgreSQL did not accept connections within two minutes".into())
}

/// A file from SENTIENT's image (its data/sql directory, e.g.).
pub fn image_file(cfg: &DeployConfig, path: &str) -> Result<String, String> {
    capture(&format!("docker run --rm --entrypoint cat {} {}", shell_quote(&cfg.image), shell_quote(path)))
        .map_err(|e| format!("read {path} from {}: {e}", cfg.image))
}

/// Scripts that run pg_dump and pg_restore in the PostgreSQL container, for
/// the backup engine (SBR_PG_DUMP / SBR_PG_RESTORE): the host need not have
/// PostgreSQL's client tools, let alone the server's version. File arguments
/// (pg_restore's archive and list) are copied into the container first.
/// Returns their paths.
pub fn pg_tool_wrappers(cfg: &DeployConfig) -> Result<(String, String), String> {
    let pg = cfg.names().pg;
    let port = cfg.pg_port;
    let dir = cfg.stack_dir();
    let dump = format!("{dir}/spm-pg_dump.sh");
    let restore = format!("{dir}/spm-pg_restore.sh");
    let sink = crate::progress::noop();
    // Inside the container PostgreSQL listens on 5432, not on the port the
    // host publishes; the connection arguments are rewritten to match.
    let rewrite = format!(
        r#"args=()
next_port=0
for a in "$@"; do
  if [ "$next_port" = 1 ]; then args+=("5432"); next_port=0; continue; fi
  case "$a" in
    -p) next_port=1; args+=("$a") ;;
    --port=*) args+=("--port=5432") ;;
    *) a="${{a//port={port}/port=5432}}"; FILE_ARG ;;
  esac
done"#
    );
    write_host_file(
        &sink,
        &dump,
        &format!(
            "#!/bin/bash\n# pg_dump in the {pg} container (written by the Platform Manager)\n{}\nexec docker exec -i -e PGPASSWORD=\"$PGPASSWORD\" {pg} pg_dump \"${{args[@]}}\"\n",
            rewrite.replace("FILE_ARG", "args+=(\"$a\")")
        ),
    )?;
    // pg_restore also takes files (the archive, a list): copied in first.
    write_host_file(
        &sink,
        &restore,
        &format!(
            "#!/bin/bash\n# pg_restore in the {pg} container (written by the Platform Manager)\n{}\nexec docker exec -i -e PGPASSWORD=\"$PGPASSWORD\" {pg} pg_restore \"${{args[@]}}\"\n",
            rewrite.replace(
                "FILE_ARG",
                &format!("if [ -f \"$a\" ]; then b=/tmp/spm-$(basename \"$a\"); docker cp -q \"$a\" {pg}:\"$b\"; args+=(\"$b\"); else args+=(\"$a\"); fi")
            )
        ),
    )?;
    run(&sink, &format!("chmod 755 {} {}", shell_quote(&dump), shell_quote(&restore)))?;
    Ok((dump, restore))
}

fn container_exists(name: &str) -> bool {
    run_output(&format!("docker container inspect {} >/dev/null 2>&1", shell_quote(name)))
        .map(|(ok, _)| ok)
        .unwrap_or(false)
}

/// The stack's ClickHouse container is there although the flavour no longer
/// uses it (a migration away from ClickHouse keeps it, with its history).
pub fn clickhouse_left_behind(cfg: &DeployConfig) -> bool {
    !cfg.flavor.uses_clickhouse() && container_exists(&cfg.names().ch)
}

/// Remove that ClickHouse container and its data volume. Cannot be undone.
pub fn remove_clickhouse(sink: &ProgressFn, cfg: &DeployConfig) -> Result<(), String> {
    let n = cfg.names();
    step(sink, "Removing ClickHouse and its data");
    run(sink, &format!("docker rm -f {}", shell_quote(&n.ch)))?;
    let _ = run(sink, &format!("docker volume rm {}", shell_quote(&n.vol_ch)));
    Ok(())
}

/// Apply changed settings: rewrite the compose file and let Compose recreate
/// the containers whose configuration changed (SENTIENT for its environment,
/// PostgreSQL for its parameters). `cfg` carries the stack's passwords.
/// Waiting for SENTIENT to answer is the caller's.
pub fn reconfigure(sink: &ProgressFn, cfg: &DeployConfig) -> Result<(), String> {
    step(sink, "Writing the SENTIENT configuration");
    write_stack_files(sink, cfg)?;
    step(sink, "Recreating what changed");
    run_stream(sink, &cfg.compose_cmd("up -d"))
}

// ---------------------------------------------------------------------------
// Safe update
// ---------------------------------------------------------------------------

/// The image reference without its tag: `quakestring/sentient:4.3` →
/// `quakestring/sentient` (a registry port is not a tag).
fn repository(image: &str) -> &str {
    match image.rfind(':') {
        Some(i) if !image[i..].contains('/') => &image[..i],
        _ => image,
    }
}

fn image_id(image: &str) -> Option<String> {
    capture(&format!("docker image inspect -f '{{{{.Id}}}}' {}", shell_quote(image)))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Run `f` with no one connected to the database, then let connections in
/// again whatever happened.
fn exclusive<T>(cfg: &DeployConfig, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let q = quote_ident(&cfg.db_name);
    psql(cfg, "postgres", &format!("ALTER DATABASE {q} WITH ALLOW_CONNECTIONS false"))?;
    let _ = psql(
        cfg,
        "postgres",
        &format!(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '{}' AND pid <> pg_backend_pid()",
            cfg.db_name.replace('\'', "''")
        ),
    );
    std::thread::sleep(std::time::Duration::from_millis(500));
    let result = f();
    let _ = psql(cfg, "postgres", &format!("ALTER DATABASE {q} WITH ALLOW_CONNECTIONS true"));
    result
}

fn snapshot_database(sink: &ProgressFn, cfg: &DeployConfig, snapshot: &str) -> Result<(), String> {
    let size: u64 = psql(cfg, "postgres", &format!("SELECT pg_database_size('{}')", cfg.db_name.replace('\'', "''")))?
        .parse()
        .map_err(|e| format!("database size: {e}"))?;
    let need = size + size / 5 + (1 << 30);
    let free: Option<u64> = capture(&format!(
        "docker exec {} df -Pk /var/lib/postgresql | tail -1 | awk '{{print $4}}'",
        cfg.names().pg
    ))
    .ok()
    .and_then(|s| s.trim().parse::<u64>().ok())
    .map(|kb| kb * 1024);
    if let Some(free) = free {
        if free < need {
            return Err(format!(
                "Not enough disk space for the copy of the database taken before updating: it is {} GB and {} GB are free. Free some space and run the update again; nothing has been changed.",
                size >> 30,
                free >> 30
            ));
        }
    }
    step(sink, &format!("Copying the database ({} MB) to {snapshot}", size >> 20));
    exclusive(cfg, || {
        psql(cfg, "postgres", &format!("CREATE DATABASE {} TEMPLATE {}", quote_ident(snapshot), quote_ident(&cfg.db_name)))
    })?;
    Ok(())
}

fn restore_database(cfg: &DeployConfig, snapshot: &str, failed: &str) -> Result<(), String> {
    exclusive(cfg, || {
        psql(cfg, "postgres", &format!("ALTER DATABASE {} RENAME TO {}", quote_ident(&cfg.db_name), quote_ident(failed)))
    })?;
    let _ = psql(cfg, "postgres", &format!("ALTER DATABASE {} WITH ALLOW_CONNECTIONS true", quote_ident(failed)));
    psql(cfg, "postgres", &format!("ALTER DATABASE {} RENAME TO {}", quote_ident(snapshot), quote_ident(&cfg.db_name)))?;
    Ok(())
}

fn older_snapshots(cfg: &DeployConfig, keep: &str) -> Vec<String> {
    let prefix = format!("{}_before_update_", cfg.db_name).replace('_', "\\_");
    psql(cfg, "postgres", &format!("SELECT datname FROM pg_database WHERE datname LIKE '{prefix}%' ORDER BY datname"))
        .map(|out| out.lines().map(str::trim).filter(|n| !n.is_empty() && *n != keep).map(String::from).collect())
        .unwrap_or_default()
}

/// Start the stack and wait until SENTIENT answers and is still up a little
/// later.
fn start_and_check(sink: &ProgressFn, cfg: &DeployConfig) -> Result<(), String> {
    step(sink, "Starting SENTIENT");
    run_stream(sink, &cfg.compose_cmd("up -d"))?;
    let mut up = false;
    for _ in 0..90 {
        if is_running(cfg.http_port) {
            up = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    if !up {
        return Err("SENTIENT did not start answering within 3 minutes".into());
    }
    step(sink, "Checking SENTIENT stays up");
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let running = run_output(&format!("docker inspect -f '{{{{.State.Running}}}} {{{{.RestartCount}}}}' {}", cfg.names().app))
            .map(|(_, out)| out.trim().to_string())
            .unwrap_or_default();
        if !running.starts_with("true") || !is_running(cfg.http_port) {
            return Err("SENTIENT started but stopped again".into());
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct UpdateReport {
    pub snapshot: String,
    pub previous_image: Option<String>,
}

/// Update to `cfg.image` (pulled, or already loaded on an offline site).
/// `cfg` must carry the stack's passwords (`with_secrets`).
pub fn safe_update(sink: ProgressFn, cfg: &DeployConfig, accept_breaking: bool) -> Result<UpdateReport, String> {
    // Keep what runs now under a tag of its own, to go back to: the image of
    // the SENTIENT container, not the tag, which an offline site may already
    // have pointed at the new image (`docker load`).
    let previous_tag = previous_tag(cfg);
    let previous = capture(&format!("docker inspect -f '{{{{.Image}}}}' {}", cfg.names().app))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| image_id(&cfg.image));
    if let Some(id) = &previous {
        run(&sink, &format!("docker tag {} {}", shell_quote(id), shell_quote(&previous_tag)))?;
    }

    step(&sink, "Getting the new SENTIENT image");
    if let Err(e) = pull_images(&sink, cfg) {
        log(&sink, format!("note: {e}; using the images already on this machine"));
    }
    if image_id(&cfg.image).is_none() {
        return Err(format!("The image {} is not on this machine and could not be pulled. Nothing has been changed.", cfg.image));
    }

    stop_app(&sink, cfg).inspect_err(|_| {
        let _ = run(&sink, &cfg.compose_cmd("up -d"));
    })?;

    step(&sink, "Checking the new version's database changes (dry run)");
    let dry = match capture(&cfg.compose_cmd("run --rm -e UPGRADE_SENTIENT=true sentient --dry-run")) {
        Ok(out) => out,
        Err(out) => {
            put_image_back(&sink, cfg, &previous_tag, previous.is_some());
            let _ = start_and_check(&sink, cfg);
            return Err(format!(
                "The dry run of the new version's database changes failed, so nothing was changed:\n{}",
                last_lines(&out, 8)
            ));
        }
    };
    let breaking = breaking_changes(&dry);
    for b in &breaking {
        log(&sink, format!("breaking: {b}"));
    }
    if !breaking.is_empty() && !accept_breaking {
        put_image_back(&sink, cfg, &previous_tag, previous.is_some());
        let _ = start_and_check(&sink, cfg);
        return Err(format!(
            "This update has breaking changes:\n• {}\n\nRead them, then run the update again with “Accept breaking changes” ticked. Nothing has been changed.",
            breaking.join("\n• ")
        ));
    }

    let snapshot = format!("{}_before_update_{}", cfg.db_name, chrono::Local::now().format("%Y%m%d%H%M"));
    if let Err(e) = snapshot_database(&sink, cfg, &snapshot) {
        put_image_back(&sink, cfg, &previous_tag, previous.is_some());
        let _ = start_and_check(&sink, cfg);
        return Err(e);
    }

    let undo = |sink: &ProgressFn, why: String| -> String {
        log(sink, format!("Update failed: {why}"));
        step(sink, "Putting the previous version back");
        let _ = run(sink, &cfg.compose_cmd("stop sentient"));
        let mut notes = Vec::new();
        if previous.is_some() {
            put_image_back(sink, cfg, &previous_tag, true);
            notes.push("the previous image is back".to_string());
        }
        let failed = format!("{}_failed_update_{}", cfg.db_name, chrono::Local::now().format("%Y%m%d%H%M"));
        match restore_database(cfg, &snapshot, &failed) {
            Ok(()) => notes.push(format!("the database is back as it was (the updated one is kept as {failed})")),
            Err(e) => notes.push(format!("the database could not be put back ({e}); the copy from before is {snapshot}")),
        }
        match start_and_check(sink, cfg) {
            Ok(()) => notes.push("the previous version is running".into()),
            Err(e) => notes.push(format!("the previous version did not start: {e}")),
        }
        format!("{why}\n\nRolled back: {}.", notes.join("; "))
    };

    step(&sink, "Applying the database changes");
    let accept = if accept_breaking { " --accept-breaking" } else { "" };
    match capture(&cfg.compose_cmd(&format!("run --rm -e UPGRADE_SENTIENT=true sentient{accept}"))) {
        Ok(out) => log(&sink, last_lines(&out, 3)),
        Err(out) => return Err(undo(&sink, format!("The database changes failed:\n{}", last_lines(&out, 8)))),
    }
    if let Err(e) = start_and_check(&sink, cfg) {
        return Err(undo(&sink, e));
    }

    for old in older_snapshots(cfg, &snapshot) {
        if psql(cfg, "postgres", &format!("DROP DATABASE {}", quote_ident(&old))).is_ok() {
            log(&sink, format!("removed the copy from an earlier update, {old}"));
        }
    }
    Ok(UpdateReport { snapshot, previous_image: previous.map(|_| previous_tag) })
}

fn previous_tag(cfg: &DeployConfig) -> String {
    format!("{}:spm-previous", repository(&cfg.image))
}

fn database_exists(cfg: &DeployConfig, name: &str) -> bool {
    psql(cfg, "postgres", &format!("SELECT 1 FROM pg_database WHERE datname = '{}'", name.replace('\'', "''")))
        .map(|out| out.trim() == "1")
        .unwrap_or(false)
}

/// The version kept by the last update: the `:spm-previous` image and the
/// newest copy of the database from before an update. `cfg` must carry the
/// stack's passwords (`with_secrets`).
pub fn rollback_point(cfg: &DeployConfig) -> Option<RollbackPoint> {
    let tag = previous_tag(cfg);
    let previous = image_id(&tag)?;
    // Nothing to go back to when the stack already runs that image.
    if image_id(&cfg.image).as_deref() == Some(previous.as_str()) {
        return None;
    }
    let snapshot = older_snapshots(cfg, "").pop()?;
    Some(RollbackPoint { version: image_version(&tag), taken: snapshot_time(&snapshot), snapshot: Some(snapshot) })
}

/// The SENTIENT version an image says it holds (its OCI version label).
pub fn image_version(image: &str) -> Option<String> {
    capture(&format!(
        "docker image inspect -f '{{{{index .Config.Labels \"org.opencontainers.image.version\"}}}}' {}",
        shell_quote(image)
    ))
    .ok()
    .map(|v| v.trim().to_string())
    .filter(|v| !v.is_empty() && v != "<no value>" && v != "dev")
}

#[derive(Debug, Clone)]
pub struct RollbackReport {
    pub point: RollbackPoint,
    /// The database that was in use, kept aside.
    pub set_aside: String,
}

/// Go back to the image and database the last update replaced. The
/// database in use is kept aside under `<db>_rolled_back_<time>`. If the
/// previous version does not come up, the current image and database are put
/// back and started again. `cfg` must carry the stack's passwords.
pub fn rollback(sink: ProgressFn, cfg: &DeployConfig) -> Result<RollbackReport, String> {
    let point = rollback_point(cfg).ok_or("No earlier version is kept on this computer: there is nothing to roll back to.")?;
    let snapshot = point.snapshot.clone().unwrap_or_default();
    let previous = previous_tag(cfg);
    let current_tag = format!("{}:spm-rolled-back", repository(&cfg.image));
    let current = capture(&format!("docker inspect -f '{{{{.Image}}}}' {}", cfg.names().app))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| image_id(&cfg.image))
        .ok_or("Could not tell which image SENTIENT runs now. Nothing has been changed.")?;
    run(&sink, &format!("docker tag {} {}", shell_quote(&current), shell_quote(&current_tag)))?;

    stop_app(&sink, cfg).inspect_err(|_| {
        let _ = run(&sink, &cfg.compose_cmd("up -d"));
    })?;
    let set_aside = format!("{}_rolled_back_{}", cfg.db_name, chrono::Local::now().format("%Y%m%d%H%M"));

    let undo = |sink: &ProgressFn, why: String, db: bool| -> String {
        log(sink, format!("Roll back failed: {why}"));
        step(sink, "Putting the current version back");
        let _ = run(sink, &cfg.compose_cmd("stop sentient"));
        let mut notes = Vec::new();
        let _ = run(sink, &format!("docker tag {} {}", shell_quote(&current_tag), shell_quote(&cfg.image)));
        if db {
            match restore_database(cfg, &set_aside, &snapshot) {
                Ok(()) => notes.push("the database is back as it was".to_string()),
                Err(e) => notes.push(format!("the database could not be put back ({e}); it is {set_aside}")),
            }
        }
        match start_and_check(sink, cfg) {
            Ok(()) => notes.push("the current version is running again".into()),
            Err(e) => notes.push(format!("the current version did not start: {e}")),
        }
        let _ = run(sink, &format!("docker image rm {}", shell_quote(&current_tag)));
        format!("{why}\n\nNothing was rolled back: {}.", notes.join("; "))
    };

    step(&sink, "Putting the previous image back");
    if let Err(e) = run(&sink, &format!("docker tag {} {}", shell_quote(&previous), shell_quote(&cfg.image))) {
        return Err(undo(&sink, e, false));
    }
    step(&sink, &format!("Putting the database from before the update back ({snapshot})"));
    if let Err(e) = restore_database(cfg, &snapshot, &set_aside) {
        let swapped = !database_exists(cfg, &snapshot);
        if !swapped && !database_exists(cfg, &cfg.db_name) && database_exists(cfg, &set_aside) {
            let _ = psql(cfg, "postgres", &format!("ALTER DATABASE {} RENAME TO {}", quote_ident(&set_aside), quote_ident(&cfg.db_name)));
        }
        return Err(undo(&sink, e, swapped));
    }
    if let Err(e) = start_and_check(&sink, cfg) {
        return Err(undo(&sink, e, true));
    }
    // Only the tags go; the images stay while anything else names them.
    let _ = run(&sink, &format!("docker image rm {} {}", shell_quote(&previous), shell_quote(&current_tag)));
    Ok(RollbackReport { point, set_aside })
}

/// Point the image tag the stack runs back at the previous image.
fn put_image_back(sink: &ProgressFn, cfg: &DeployConfig, previous_tag: &str, have_previous: bool) {
    if have_previous {
        let _ = run(sink, &format!("docker tag {} {}", shell_quote(previous_tag), shell_quote(&cfg.image)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_quoted_yaml_values() {
        let text = "    environment:\n      POSTGRES_PASSWORD: \"p\\\"w\\\\x\"\n      CLICKHOUSE_PASSWORD: \"ch\"\n";
        assert_eq!(yaml_value(text, "POSTGRES_PASSWORD").as_deref(), Some("p\"w\\x"));
        assert_eq!(yaml_value(text, "CLICKHOUSE_PASSWORD").as_deref(), Some("ch"));
        assert_eq!(yaml_value(text, "MISSING"), None);
    }

    #[test]
    fn repository_drops_only_the_tag() {
        assert_eq!(repository("quakestring/sentient:4.3.0.24"), "quakestring/sentient");
        assert_eq!(repository("registry:5000/sentient"), "registry:5000/sentient");
        assert_eq!(repository("registry:5000/sentient:latest"), "registry:5000/sentient");
    }
}
