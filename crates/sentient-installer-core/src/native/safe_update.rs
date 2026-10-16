// Copyright © 2016-2026 The SENTIENT Authors

//! Safe update of a native Windows installation: everything needed to go
//! back is in place before anything changes, and a failure at any point puts
//! the previous version back and starts it.
//!
//! 1. Stop SENTIENT (PostgreSQL stays up).
//! 2. Dry-run the new version's schema upgrade. Breaking changes stop the
//!    update here unless the operator accepted them.
//! 3. Copy the database inside PostgreSQL (`CREATE DATABASE … TEMPLATE`): a
//!    file-level copy, much faster than a dump, checked for disk space first.
//! 4. Move the program files aside, put the new ones in, run the upgrade.
//! 5. Start SENTIENT and check it answers and stays up.
//!
//! Failure in 4 or 5: the new files are removed, the old ones and the
//! database copy are put back (the failed database is kept for diagnosis),
//! and the previous version is started. After success the database copy and
//! the old files stay until the next update replaces them, so an update that
//! turns out bad later can still be reversed by hand.

use std::path::{Path, PathBuf};

use super::{
    APP_SERVICE, NativeConfig, clickhouse_conn, copy_tree, is_running, log, psql, sc, service_state, step,
    stop_server,
};
use crate::progress::{Progress, ProgressFn};
use crate::stack::Flavor;
use crate::sys;

const ITEMS: [&str; 3] = ["bin", "ui", "data"];

#[derive(Debug, Clone, Default)]
pub struct UpdateOptions {
    /// The operator read the breaking changes and goes ahead.
    pub accept_breaking: bool,
}

#[derive(Debug, Clone)]
pub struct UpdateReport {
    /// The copy of the database from before the update.
    pub snapshot: String,
    /// Where the previous program files are.
    pub previous_files: PathBuf,
}

fn rollback_dir(cfg: &NativeConfig) -> PathBuf {
    cfg.install_dir.join("previous-version")
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Free bytes on the drive holding `path`.
fn free_bytes(path: &Path) -> Option<u64> {
    let drive = path.to_string_lossy().chars().next()?;
    let script = format!("(Get-PSDrive -Name '{drive}').Free");
    let (ok, out, _) = sys::output("powershell.exe", &["-NoProfile", "-Command", &script])?;
    if !ok {
        return None;
    }
    sys::decode(&out).trim().parse().ok()
}

/// Run `f` with no one connected to the database, then let connections in
/// again whatever happened.
fn exclusive<T>(cfg: &NativeConfig, db: &str, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let q = quote_ident(db);
    psql(cfg, "postgres", &format!("ALTER DATABASE {q} WITH ALLOW_CONNECTIONS false"))?;
    let _ = psql(
        cfg,
        "postgres",
        &format!("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '{}' AND pid <> pg_backend_pid()", db.replace('\'', "''")),
    );
    std::thread::sleep(std::time::Duration::from_millis(500));
    let result = f();
    let _ = psql(cfg, "postgres", &format!("ALTER DATABASE {q} WITH ALLOW_CONNECTIONS true"));
    result
}

fn snapshot_database(sink: &ProgressFn, cfg: &NativeConfig, snapshot: &str) -> Result<(), String> {
    let size: u64 = psql(cfg, "postgres", &format!("SELECT pg_database_size('{}')", cfg.db_name.replace('\'', "''")))?
        .parse()
        .map_err(|e| format!("database size: {e}"))?;
    let need = size + size / 5 + (1 << 30);
    if let Some(free) = free_bytes(&cfg.pg_dir) {
        if free < need {
            return Err(format!(
                "Not enough disk space for the copy of the database taken before updating: it is {} GB and {} GB are free. Free some space and run the update again; nothing has been changed.",
                size >> 30,
                free >> 30
            ));
        }
    }
    step(sink, &format!("Copying the database ({} MB) to {snapshot}", size >> 20));
    exclusive(cfg, &cfg.db_name, || {
        psql(cfg, "postgres", &format!("CREATE DATABASE {} TEMPLATE {}", quote_ident(snapshot), quote_ident(&cfg.db_name)))
    })?;
    Ok(())
}

/// Put the snapshot back under the database's name; the database it
/// replaces is kept as `failed`.
fn restore_database(cfg: &NativeConfig, snapshot: &str, failed: &str) -> Result<(), String> {
    exclusive(cfg, &cfg.db_name, || {
        psql(cfg, "postgres", &format!("ALTER DATABASE {} RENAME TO {}", quote_ident(&cfg.db_name), quote_ident(failed)))
    })?;
    let _ = psql(cfg, "postgres", &format!("ALTER DATABASE {} WITH ALLOW_CONNECTIONS true", quote_ident(failed)));
    psql(cfg, "postgres", &format!("ALTER DATABASE {} RENAME TO {}", quote_ident(snapshot), quote_ident(&cfg.db_name)))?;
    Ok(())
}

/// Copies from earlier updates, oldest first.
fn older_snapshots(cfg: &NativeConfig, keep: &str) -> Vec<String> {
    let prefix = format!("{}_before_update_", cfg.db_name);
    psql(cfg, "postgres", &format!("SELECT datname FROM pg_database WHERE datname LIKE '{}%' ORDER BY datname", prefix.replace('\'', "''").replace('_', "\\_")))
        .map(|out| out.lines().map(str::trim).filter(|n| !n.is_empty() && *n != keep).map(String::from).collect())
        .unwrap_or_default()
}

/// The new version's schema tool against this database. Returns its output.
fn run_upgrade(cfg: &NativeConfig, bin: &Path, data: &Path, dry_run: bool, accept_breaking: bool) -> Result<String, String> {
    let installer = bin.join("sentient-install.exe");
    if !installer.exists() {
        return Err(format!("{} is missing from the package", installer.display()));
    }
    let db_url = format!("postgresql://{}:{}@127.0.0.1:{}/{}", cfg.db_user, cfg.db_password, cfg.pg_port, cfg.db_name);
    let data_dir = data.to_string_lossy().into_owned();
    let mut args = vec!["--upgrade", "--database-url", db_url.as_str(), "--data-dir", data_dir.as_str()];
    if dry_run {
        args.push("--dry-run");
    }
    if accept_breaking {
        args.push("--accept-breaking");
    }
    let ch = clickhouse_conn(cfg);
    let timescale = if cfg.flavor == Flavor::Timescaledb { "true" } else { "false" };
    let mut env: Vec<(&str, &str)> = vec![("SENTIENT_TIMESCALE_ENABLED", timescale)];
    if cfg.flavor == Flavor::Clickhouse {
        env.extend([
            ("CLICKHOUSE_URL", ch.url.as_str()),
            ("CLICKHOUSE_DATABASE", ch.database.as_str()),
            ("CLICKHOUSE_USER", ch.user.as_str()),
            ("CLICKHOUSE_PASSWORD", ch.password.as_str()),
        ]);
    }
    match sys::output_tracked_env(&installer.to_string_lossy(), &args, &env) {
        Some((ok, out, err)) => {
            let text = format!("{}{}", sys::decode(&out), sys::decode(&err));
            if ok { Ok(text) } else { Err(text) }
        }
        None => Err("could not run sentient-install.exe".into()),
    }
}

/// The `[BREAKING]` lines of a dry run, with what follows each.
fn breaking_changes(output: &str) -> Vec<String> {
    let lines: Vec<&str> = output.lines().collect();
    let mut found = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.contains("[BREAKING]") {
            let mut entry = line.trim().trim_start_matches('•').trim().to_string();
            for next in lines.iter().skip(i + 1).take(3) {
                let t = next.trim();
                if t.starts_with("safe") || t.starts_with("changelog") || t.starts_with("resource") {
                    entry.push_str(&format!(" — {t}"));
                }
            }
            found.push(entry);
        }
    }
    found
}

fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

fn swap_in(cfg: &NativeConfig, staging: &Path) -> Result<(), String> {
    let back = rollback_dir(cfg);
    let _ = std::fs::remove_dir_all(&back);
    std::fs::create_dir_all(&back).map_err(|e| format!("create {}: {e}", back.display()))?;
    for item in ITEMS {
        let live = cfg.install_dir.join(item);
        if live.exists() {
            std::fs::rename(&live, back.join(item)).map_err(|e| format!("move {} aside: {e}", live.display()))?;
        }
    }
    for item in ITEMS {
        let from = staging.join(item);
        if from.exists() {
            copy_tree(&from, &cfg.install_dir.join(item))?;
        }
    }
    Ok(())
}

fn swap_back(cfg: &NativeConfig) -> Result<(), String> {
    let back = rollback_dir(cfg);
    for item in ITEMS {
        let saved = back.join(item);
        if !saved.exists() {
            continue;
        }
        let live = cfg.install_dir.join(item);
        let _ = std::fs::remove_dir_all(&live);
        std::fs::rename(&saved, &live).map_err(|e| format!("put {} back: {e}", live.display()))?;
    }
    Ok(())
}

/// Start SENTIENT and wait until it answers and is still up a little later.
fn start_and_check(sink: &ProgressFn, cfg: &NativeConfig) -> Result<(), String> {
    step(sink, "Starting SENTIENT");
    let _ = sc(&["start", APP_SERVICE]);
    let mut up = false;
    for _ in 0..180 {
        if is_running(cfg.http_port) {
            up = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    if !up {
        return Err("SENTIENT did not start answering within 3 minutes".into());
    }
    step(sink, "Checking SENTIENT stays up");
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        if service_state(APP_SERVICE) != "running" || !is_running(cfg.http_port) {
            return Err("SENTIENT started but stopped again".into());
        }
    }
    Ok(())
}

/// Update to the program files unpacked in `staging` (bin, ui, data).
pub fn safe_update(sink: ProgressFn, cfg: &NativeConfig, staging: &Path, opts: &UpdateOptions) -> Result<UpdateReport, String> {
    if !staging.join("bin").join("sentient-server.exe").exists() {
        return Err(format!("{} does not hold a SENTIENT package (bin\\sentient-server.exe)", staging.display()));
    }

    if let Err(e) = stop_server(&sink, cfg) {
        let _ = sc(&["start", APP_SERVICE]);
        return Err(e);
    }

    step(&sink, "Checking the new version's database changes (dry run)");
    let dry = match run_upgrade(cfg, &staging.join("bin"), &staging.join("data"), true, opts.accept_breaking) {
        Ok(out) => out,
        Err(out) => {
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
    if !breaking.is_empty() && !opts.accept_breaking {
        let _ = start_and_check(&sink, cfg);
        return Err(format!(
            "This update has breaking changes:\n• {}\n\nRead them, then run the update again with “Accept breaking changes” ticked. Nothing has been changed.",
            breaking.join("\n• ")
        ));
    }

    let snapshot = format!("{}_before_update_{}", cfg.db_name, chrono::Local::now().format("%Y%m%d%H%M"));
    if let Err(e) = snapshot_database(&sink, cfg, &snapshot) {
        let _ = start_and_check(&sink, cfg);
        return Err(e);
    }
    // From here every failure goes through `undo`.
    let undo = |sink: &ProgressFn, why: String, files_swapped: bool| -> String {
        sink(Progress::Log { line: format!("Update failed: {why}") });
        step(sink, "Putting the previous version back");
        let _ = stop_server(sink, cfg);
        let mut notes = Vec::new();
        if files_swapped {
            if let Err(e) = swap_back(cfg) {
                notes.push(format!("the previous program files could not be put back ({e}); they are in {}", rollback_dir(cfg).display()));
            }
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

    step(&sink, "Replacing the program files");
    if let Err(e) = swap_in(cfg, staging) {
        return Err(undo(&sink, e, true));
    }
    step(&sink, "Applying the database changes");
    match run_upgrade(cfg, &cfg.install_dir.join("bin"), &cfg.install_dir.join("data"), false, opts.accept_breaking) {
        Ok(out) => log(&sink, last_lines(&out, 3)),
        Err(out) => return Err(undo(&sink, format!("The database changes failed:\n{}", last_lines(&out, 8)), true)),
    }
    if let Err(e) = start_and_check(&sink, cfg) {
        return Err(undo(&sink, e, true));
    }

    for old in older_snapshots(cfg, &snapshot) {
        if psql(cfg, "postgres", &format!("DROP DATABASE {}", quote_ident(&old))).is_ok() {
            log(&sink, format!("removed the copy from an earlier update, {old}"));
        }
    }
    Ok(UpdateReport { snapshot, previous_files: rollback_dir(cfg) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breaking_lines_are_collected_with_their_details() {
        let out = "⚠ BREAKING / NOTABLE SYSTEM-RESOURCE CHANGES in this upgrade (21 → 30):\n  • [BREAKING] Widget X removed\n      resource : widget_types/x\n      safe     : re-add from library\n  • [INFO] Something else\n";
        let b = breaking_changes(out);
        assert_eq!(b.len(), 1);
        assert!(b[0].starts_with("[BREAKING] Widget X removed"));
        assert!(b[0].contains("safe"));
    }
}
