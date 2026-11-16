// Copyright © 2016-2026 The SENTIENT Authors

//! Moving data to another disk, for the window and the server agent: the
//! same calls for this computer (the engine in this process; Docker access
//! is all it needs) and for a server (the agent's `movedata-*` and
//! `move-data` jobs). The engine is
//! sentient_installer_core::distro::move_data; the screens are
//! crate::manager::movedata.
//!
//! Windows (native SENTIENT and PostgreSQL, ClickHouse in WSL): ClickHouse's
//! WSL disk moves whole (clickhouse_wsl::move_disk); the other stores do not
//! move there yet.

// The agent (no window) uses the jobs only.
#![cfg_attr(not(feature = "gui"), allow(dead_code))]

use sentient_installer_core::distro::{docker_ops, move_data::{self, Check, Estimate, Move, Outcome, Overview, Store}};
use sentient_installer_core::progress::{Progress, ProgressFn};

use crate::installation::{Engine, InstallationRecord};

/// Where the move runs.
#[derive(Clone)]
pub enum Target {
    Local,
    Remote(crate::remote::Instance),
}

/// Why this installation's data cannot move here at all.
pub fn unsupported(record: &InstallationRecord) -> Option<&'static str> {
    match record.engine {
        Engine::Docker => None,
        Engine::Native if cfg!(windows) => None,
        Engine::Native => Some("This installation runs SENTIENT and PostgreSQL as services, not in Docker: their data folders are the system's. Moving them is not available yet."),
    }
}

fn backups_dir() -> Option<String> {
    let f = crate::backups::BackupPrefs::load().folder;
    (!f.as_os_str().is_empty()).then(|| f.display().to_string())
}

// ---------------------------------------------------------------------------
// This computer (and the agent on a server, which calls the same)
// ---------------------------------------------------------------------------

fn local_record() -> Result<InstallationRecord, String> {
    let r = InstallationRecord::load().ok_or("No SENTIENT is installed here.")?;
    if let Some(why) = unsupported(&r) {
        return Err(why.into());
    }
    Ok(r)
}

pub fn local_overview() -> Result<Overview, String> {
    let r = local_record()?;
    #[cfg(windows)]
    if r.engine == Engine::Native {
        return Ok(crate::movedata_win::overview(&r));
    }
    move_data::overview(&r.stack, backups_dir().as_deref())
}

pub fn local_check(store: Store, path: &str) -> Result<Check, String> {
    let r = local_record()?;
    #[cfg(windows)]
    if r.engine == Engine::Native {
        return Ok(crate::movedata_win::check(&r, store, path));
    }
    let ov = move_data::overview(&r.stack, backups_dir().as_deref())?;
    let needed = ov.places.iter().find(|p| p.store == store).and_then(|p| p.bytes).unwrap_or(0);
    Ok(move_data::check(&r.stack, store, path, needed, &ov.places))
}

pub fn local_estimate(moves: &[Move]) -> Result<Estimate, String> {
    let r = local_record()?;
    #[cfg(windows)]
    if r.engine == Engine::Native {
        return Ok(crate::movedata_win::estimate(&r, moves));
    }
    let ov = move_data::overview(&r.stack, backups_dir().as_deref())?;
    Ok(move_data::estimate(&r.stack, moves, &ov.places))
}

/// Run the moves here, save where everything is now, and say how it went:
/// Ok when moved; Err with the reason when it switched back (the outcome is
/// in the log after move_data::OUTCOME_MARK either way).
pub fn local_run(moves: &[Move], sink: ProgressFn) -> Result<(), String> {
    let mut r = local_record()?;
    #[cfg(windows)]
    if r.engine == Engine::Native {
        return crate::movedata_win::run(&r, moves, sink);
    }
    let full = docker_ops::with_secrets(&r.stack)?;
    let out = move_data::run(sink.clone(), &full, moves, backups_dir().as_deref());
    save(&mut r, &out)?;
    finish(&out)
}

fn save(r: &mut InstallationRecord, out: &Outcome) -> Result<(), String> {
    r.stack.locations = out.locations.clone();
    r.stack.old_copies = out.old_copies.clone();
    crate::maintenance_ops::save_record(r)?;
    if let Some(dir) = &out.backups_dir {
        let mut prefs = crate::backups::BackupPrefs::load();
        prefs.folder = std::path::PathBuf::from(dir);
        prefs.save()?;
    }
    Ok(())
}

fn finish(out: &Outcome) -> Result<(), String> {
    match out.ended {
        move_data::Ended::Moved => Ok(()),
        move_data::Ended::SwitchedBack => Err(format!(
            "{} Everything is back where it was{}.",
            out.reason.clone().unwrap_or_default(),
            if out.down_secs > 0 { " and SENTIENT runs again" } else { "" }
        )),
        move_data::Ended::Broken => Err(out.reason.clone().unwrap_or_default()),
    }
}

/// Remove a kept old copy (or a failed move's partial one) here.
pub fn local_remove_old(path: &str, sink: ProgressFn) -> Result<(), String> {
    let mut r = local_record()?;
    #[cfg(windows)]
    if r.engine == Engine::Native {
        return crate::movedata_win::remove_old(&r, path, sink);
    }
    let left = move_data::remove_old(&sink, &r.stack, path, backups_dir().as_deref())?;
    r.stack.old_copies = left;
    crate::maintenance_ops::save_record(&r)?;
    sink(Progress::Done { message: format!("{path} is removed.") });
    Ok(())
}

pub fn local_cancel() -> Result<(), String> {
    let r = local_record()?;
    sentient_installer_core::cancel::request();
    move_data::request_cancel(&r.stack)
}

// ---------------------------------------------------------------------------
// Either place
// ---------------------------------------------------------------------------

pub fn overview(t: &Target) -> Result<Overview, String> {
    #[cfg(debug_assertions)]
    if let Some(o) = debug::overview() {
        return o;
    }
    match t {
        Target::Local => local_overview(),
        Target::Remote(inst) => crate::remote::movedata_overview(inst),
    }
}

pub fn check(t: &Target, store: Store, path: &str) -> Result<Check, String> {
    #[cfg(debug_assertions)]
    if let Some(c) = debug::check(store, path) {
        return c;
    }
    match t {
        Target::Local => local_check(store, path),
        Target::Remote(inst) => crate::remote::movedata_check(inst, store, path),
    }
}

pub fn estimate(t: &Target, moves: &[Move]) -> Result<Estimate, String> {
    #[cfg(debug_assertions)]
    if let Some(e) = debug::estimate(moves) {
        return e;
    }
    match t {
        Target::Local => local_estimate(moves),
        Target::Remote(inst) => crate::remote::movedata_estimate(inst, moves),
    }
}

/// The moves as one job argument: `pg=/mnt/a,ch=/mnt/b` (a folder with a
/// comma or quote is refused by the folder check first).
pub fn moves_arg(moves: &[Move]) -> String {
    moves.iter().map(|m| format!("{}={}", m.store.as_str(), m.to)).collect::<Vec<_>>().join(",")
}

pub fn parse_moves(arg: &str) -> Result<Vec<Move>, String> {
    arg.split(',')
        .filter(|s| !s.is_empty())
        .map(|s| {
            let (k, v) = s.split_once('=').ok_or_else(|| format!("not a move: {s}"))?;
            let store = Store::parse(k).ok_or_else(|| format!("not a store: {k}"))?;
            Ok(Move { store, to: v.to_string() })
        })
        .collect()
}

/// The outcome a job logged, if any.
pub fn outcome_in<S: AsRef<str>>(lines: &[S]) -> Option<Outcome> {
    lines.iter().rev().find_map(|l| l.as_ref().strip_prefix(move_data::OUTCOME_MARK).and_then(|j| serde_json::from_str(j).ok()))
}

// ---------------------------------------------------------------------------
// The agent's jobs
// ---------------------------------------------------------------------------

/// `movedata-json` | `movedata-check <store> <folder>` |
/// `movedata-estimate <moves>`: one JSON line. Some(exit code) when `job`
/// is one of them.
pub fn data_job(job: &str, args: &[String]) -> Option<i32> {
    let r: Result<String, String> = match job {
        "movedata-json" => local_overview().and_then(|o| serde_json::to_string(&o).map_err(|e| e.to_string())),
        "movedata-check" => match (args.get(1).and_then(|s| Store::parse(s)), args.get(2)) {
            (Some(store), Some(path)) => local_check(store, path).and_then(|c| serde_json::to_string(&c).map_err(|e| e.to_string())),
            _ => Err("usage: movedata-check <pg|ch|data|backups> <folder>".into()),
        },
        "movedata-estimate" => match args.get(1) {
            Some(a) => parse_moves(a).and_then(|m| local_estimate(&m)).and_then(|e| serde_json::to_string(&e).map_err(|e| e.to_string())),
            None => Err("usage: movedata-estimate <store=folder,…>".into()),
        },
        "move-data-cancel" => local_cancel().map(|_| "\"ok\"".to_string()),
        _ => return None,
    };
    Some(match r {
        Ok(line) => {
            println!("{line}");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    })
}

// ---------------------------------------------------------------------------
// Screens: canned answers (debug builds)
// ---------------------------------------------------------------------------

#[cfg(debug_assertions)]
pub mod debug {
    use super::*;
    use sentient_installer_core::distro::move_data::{Disk, DiskKind, OldCopy, Place};

    /// `SPM_DEBUG_MOVEDATA=1`: the mockup's server (disks A–C, a NAS, a USB
    /// stick); `=win`: the Windows server.
    pub fn on() -> Option<String> {
        std::env::var("SPM_DEBUG_MOVEDATA").ok().filter(|v| !v.is_empty())
    }

    fn slow() {
        if let Some(secs) = std::env::var("SPM_DEBUG_REMOTE_SLOW").ok().and_then(|s| s.parse::<u64>().ok()) {
            std::thread::sleep(std::time::Duration::from_secs(secs));
        }
    }

    const G: u64 = 1 << 30;
    const T: u64 = 1 << 40;

    pub fn canned() -> Overview {
        let d = |source: &str, mount: &str, fstype: &str, size: u64, avail: u64, kind| Disk { source: source.into(), mount: mount.into(), fstype: fstype.into(), size, avail, kind };
        let moved = std::env::var("SPM_DEBUG_MOVEDATA_MOVED").is_ok();
        let windows = on().as_deref() == Some("win");
        if windows {
            return Overview {
                places: vec![
                    Place { store: Store::Pg, volume: String::new(), path: r"C:\SENTIENT\pg\data".into(), bind: true, bytes: Some(96 * G), mount: Some("C:".into()) },
                    Place { store: Store::Ch, volume: "ext4.vhdx".into(), path: r"C:\Users\sentient-clickhouse\AppData\Local\Packages\CanonicalGroupLimited\LocalState\ext4.vhdx".into(), bind: true, bytes: Some(310 * G), mount: Some("C:".into()) },
                    Place { store: Store::Data, volume: String::new(), path: r"C:\ProgramData\SENTIENT".into(), bind: true, bytes: Some(2400 * (1 << 20)), mount: Some("C:".into()) },
                    Place { store: Store::Backups, volume: String::new(), path: r"C:\ProgramData\SENTIENT\backups".into(), bind: true, bytes: Some(17 * G), mount: Some("C:".into()) },
                ],
                disks: vec![d("C:", "C:", "NTFS", 512 * G, 41 * G, DiskKind::Local), d("D:", "D:", "NTFS", 2 * T, 1700 * G, DiskKind::Local)],
                old_copies: Vec::new(),
                external: false,
                docker_root: String::new(),
            };
        }
        Overview {
            places: vec![
                Place { store: Store::Pg, volume: "st-pg-data".into(), path: "/mnt/disk-a/sentient/postgres".into(), bind: true, bytes: Some(184 * G), mount: Some("/mnt/disk-a".into()) },
                Place {
                    store: Store::Ch,
                    volume: "st-ch-data".into(),
                    path: if moved { "/mnt/disk-c/sentient/clickhouse".into() } else { "/mnt/disk-b/sentient/clickhouse".into() },
                    bind: true,
                    bytes: Some(612 * G),
                    mount: Some(if moved { "/mnt/disk-c".into() } else { "/mnt/disk-b".into() }),
                },
                Place { store: Store::Data, volume: "st-data".into(), path: "/var/lib/docker/volumes/st-data/_data".into(), bind: false, bytes: Some(2150 * (1 << 20)), mount: Some("/".into()) },
                Place { store: Store::Backups, volume: String::new(), path: "/var/backups/sentient".into(), bind: true, bytes: Some(17 * G + 400 * (1 << 20)), mount: Some("/".into()) },
            ],
            disks: vec![
                d("/dev/sda1", "/", "ext4", 500 * G, 64 * G + 100 * (1 << 20), DiskKind::Local),
                d("/dev/sdb1", "/mnt/disk-a", "ext4", 2 * T, 1400 * G, DiskKind::Local),
                d("/dev/sdc1", "/mnt/disk-b", "xfs", 4 * T, 3100 * G, DiskKind::Local),
                d("/dev/sdd1", "/mnt/disk-c", "xfs", 2 * T, if moved { 1300 * G } else { 1900 * G }, DiskKind::Local),
                d("//nas/plant", "/mnt/nas", "cifs", 8 * T, 5200 * G, DiskKind::Network),
                d("/dev/sde1", "/media/usb", "vfat", 256 * G, 240 * G, DiskKind::Fat),
            ],
            old_copies: if moved { vec![OldCopy { store: Store::Ch, path: "/mnt/disk-b/sentient/clickhouse".into(), bytes: 612 * G, at: "2026-11-16T09:12:00+05:30".into(), partial: false }] } else { Vec::new() },
            external: false,
            docker_root: "/var/lib/docker".into(),
        }
    }

    pub fn overview() -> Option<Result<Overview, String>> {
        on()?;
        slow();
        Some(Ok(canned()))
    }

    pub fn check(store: Store, path: &str) -> Option<Result<Check, String>> {
        on()?;
        slow();
        let ov = canned();
        let needed = ov.places.iter().find(|p| p.store == store).and_then(|p| p.bytes).unwrap_or(0);
        let disk = move_data::disk_of(&ov.disks, path).cloned();
        let mut blocks = disk.as_ref().map(|d| move_data::disk_blocks(d, store, needed, store.label())).unwrap_or_default();
        if ov.places.iter().any(|p| p.store == store && p.path == path) {
            blocks.push(move_data::Block { kind: move_data::BlockKind::SamePlace, title: format!("{} is already here", store.label()), why: String::new(), fixes: vec![] });
        }
        Some(Ok(Check { path: path.into(), disk, exists: true, empty: true, needed, blocks }))
    }

    pub fn estimate(moves: &[Move]) -> Option<Result<Estimate, String>> {
        on()?;
        slow();
        let ov = canned();
        let rate = if on().as_deref() == Some("win") { 220. } else { 260. };
        let list: Vec<(Store, u64, u64)> = moves
            .iter()
            .map(|m| {
                let b = ov.places.iter().find(|p| p.store == m.store).and_then(|p| p.bytes).unwrap_or(0);
                (m.store, b, (b as f64 / 1048576. / rate) as u64)
            })
            .collect();
        let total: u64 = list.iter().map(|x| x.2).sum::<u64>() + 60;
        Some(Ok(Estimate { moves: list, down_secs: total, rate_mb_s: rate }))
    }
}
