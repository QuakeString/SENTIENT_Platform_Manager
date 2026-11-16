// Copyright © 2016-2026 The SENTIENT Authors

//! Move data to another disk on a Windows server (native SENTIENT and
//! PostgreSQL, ClickHouse in WSL under the hidden account): ClickHouse's
//! disk file (ext4.vhdx) moves whole, as the account, with
//! `wsl --manage sentient --move` (export and import on WSL before 2.0;
//! clickhouse_wsl::move_disk_here). PostgreSQL's and SENTIENT's folders do
//! not move here yet. Untested on a real Windows server: type-checked only.

use std::path::{Path, PathBuf};

use sentient_installer_core::clickhouse_wsl;
use sentient_installer_core::distro::move_data::{self, Block, BlockKind, Check, Disk, DiskKind, Ended, Estimate, Move, Outcome, Overview, Place, Store};
use sentient_installer_core::progress::{Progress, ProgressFn};

use crate::installation::InstallationRecord;

fn vhdx() -> PathBuf {
    clickhouse_wsl::disk_dir().join("ext4.vhdx")
}

fn dir_bytes(p: &Path) -> Option<u64> {
    let mut total = 0;
    let mut stack = vec![p.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).ok()?.flatten() {
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() {
                stack.push(e.path());
            } else {
                total += m.len();
            }
        }
    }
    Some(total)
}

fn disks() -> Vec<Disk> {
    let list = sysinfo::Disks::new_with_refreshed_list();
    list.list()
        .iter()
        .map(|d| {
            let fs = d.file_system().to_string_lossy().to_string();
            let mount = d.mount_point().display().to_string();
            let kind = if mount.starts_with("\\\\") {
                DiskKind::Network
            } else if fs.eq_ignore_ascii_case("FAT32") || fs.eq_ignore_ascii_case("exFAT") || fs.eq_ignore_ascii_case("FAT") {
                DiskKind::Fat
            } else {
                DiskKind::Local
            };
            Disk { source: mount.trim_end_matches('\\').to_string(), mount: mount.trim_end_matches('\\').to_string(), fstype: fs, size: d.total_space(), avail: d.available_space(), kind }
        })
        .collect()
}

fn mount_of(disks: &[Disk], path: &str) -> Option<String> {
    let p = path.to_ascii_lowercase();
    disks.iter().filter(|d| p.starts_with(&d.mount.to_ascii_lowercase())).max_by_key(|d| d.mount.len()).map(|d| d.mount.clone())
}

pub fn overview(record: &InstallationRecord) -> Overview {
    let disks = disks();
    let mut places = vec![Place { store: Store::Pg, volume: String::new(), path: record.pg_dir.join("data").display().to_string(), bind: true, bytes: dir_bytes(&record.pg_dir.join("data")), mount: None }];
    if record.stack.flavor.uses_clickhouse() && clickhouse_wsl::is_installed() {
        let v = vhdx();
        places.push(Place { store: Store::Ch, volume: "ext4.vhdx".into(), path: v.display().to_string(), bind: true, bytes: std::fs::metadata(&v).ok().map(|m| m.len()), mount: None });
    }
    places.push(Place { store: Store::Data, volume: String::new(), path: record.state_dir.display().to_string(), bind: true, bytes: dir_bytes(&record.state_dir), mount: None });
    let backups = crate::backups::BackupPrefs::load().folder;
    if !backups.as_os_str().is_empty() {
        places.push(Place { store: Store::Backups, volume: String::new(), path: backups.display().to_string(), bind: true, bytes: dir_bytes(&backups), mount: None });
    }
    for p in places.iter_mut() {
        p.mount = mount_of(&disks, &p.path);
    }
    Overview { places, disks, old_copies: Vec::new(), external: false, docker_root: String::new() }
}

fn block(kind: BlockKind, title: String, why: String, fixes: &[&str]) -> Block {
    Block { kind, title, why, fixes: fixes.iter().map(|s| s.to_string()).collect() }
}

pub fn check(record: &InstallationRecord, store: Store, path: &str) -> Check {
    let ov = overview(record);
    // The whole disk file moves: the target needs its full size free.
    let needed = ov.places.iter().find(|p| p.store == store).and_then(|p| p.bytes).unwrap_or(0);
    let mut c = Check { path: path.to_string(), needed, ..Default::default() };
    if store != Store::Ch {
        c.blocks.push(block(BlockKind::NoPermission, format!("{} does not move on Windows yet", store.label()), "Only ClickHouse's WSL disk moves on a Windows server for now.".into(), &[]));
        return c;
    }
    let p = Path::new(path);
    if !(p.is_absolute() || path.starts_with("\\\\")) {
        c.blocks.push(block(BlockKind::NotAbsolute, "Not a full path".into(), "The folder must be a full path such as D:\\WSL\\sentient.".into(), &["Pick the folder with Browse."]));
        return c;
    }
    let disks = disks();
    c.disk = mount_of(&disks, path).and_then(|m| disks.iter().find(|d| d.mount == m).cloned());
    if path.starts_with("\\\\") {
        c.blocks.push(block(BlockKind::Network, format!("{path} is a network folder"), "WSL's disk must be on a local disk.".into(), &["Pick a local disk."]));
        return c;
    }
    if let Some(d) = &c.disk {
        c.blocks.extend(move_data::disk_blocks(d, store, needed, "ClickHouse's disk"));
    }
    if ov.places.iter().any(|x| x.store == Store::Ch && Path::new(&x.path).parent() == Some(p)) {
        c.blocks.push(block(BlockKind::SamePlace, "ClickHouse's disk is already here".into(), String::new(), &["Pick another folder."]));
    }
    c.exists = p.exists();
    c.empty = !c.exists || std::fs::read_dir(p).map(|mut r| r.next().is_none()).unwrap_or(false);
    if !c.empty {
        c.blocks.push(block(BlockKind::NotEmpty, format!("{path} is not empty"), "WSL needs an empty folder for the disk.".into(), &["Make a new folder inside it, or", "pick an empty folder."]));
    }
    c
}

pub fn estimate(record: &InstallationRecord, moves: &[Move]) -> Estimate {
    let ov = overview(record);
    let mut rate = 0.0f64;
    for m in moves {
        let dir = Path::new(&m.to).ancestors().find(|a| a.exists()).map(Path::to_path_buf).unwrap_or_default();
        let f = dir.join(".spm-speed");
        let t = std::time::Instant::now();
        let ok = (|| -> std::io::Result<()> {
            use std::io::Write as _;
            let mut file = std::fs::File::create(&f)?;
            let buf = vec![0u8; 1 << 20];
            for _ in 0..256 {
                file.write_all(&buf)?;
            }
            file.sync_all()
        })();
        let _ = std::fs::remove_file(&f);
        if ok.is_ok() {
            let r = 256. * 1.048_576 / t.elapsed().as_secs_f64().max(0.01);
            rate = if rate == 0. { r } else { rate.min(r) };
        }
    }
    if rate <= 0. {
        rate = 150.;
    }
    let rate = rate.min(1500.);
    let list: Vec<(Store, u64, u64)> = moves
        .iter()
        .map(|m| {
            let b = ov.places.iter().find(|p| p.store == m.store).and_then(|p| p.bytes).unwrap_or(0);
            // Read for the hash before, moved, read for the hash after.
            (m.store, b, (b as f64 / 1e6 / rate * 3.) as u64)
        })
        .collect();
    let down = list.iter().map(|x| x.2).sum::<u64>() + 180;
    Estimate { moves: list, down_secs: down, rate_mb_s: rate }
}

fn wait(mut ok: impl FnMut() -> bool, secs: u64) -> bool {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        if ok() {
            return true;
        }
        if std::time::Instant::now() > until {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}

fn agent_move(record: &InstallationRecord, dest: &str, sink: &ProgressFn) -> Result<(), String> {
    let password = crate::secrets::get(crate::secrets::Secret::ClickhouseAccount).ok_or("The ClickHouse account's password is not kept on this computer; run Repair first.")?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    std::fs::write(clickhouse_wsl::move_request_path(), dest).map_err(|e| e.to_string())?;
    let _ = record;
    clickhouse_wsl::run_as_account(sink, &password, &exe, "move-disk", None)
}

fn start_clickhouse(record: &InstallationRecord) -> bool {
    let _ = std::process::Command::new("schtasks.exe").args(["/end", "/tn", clickhouse_wsl::KEEPALIVE_TASK]).output();
    let _ = std::process::Command::new("schtasks.exe").args(["/run", "/tn", clickhouse_wsl::KEEPALIVE_TASK]).output();
    wait(|| sentient_installer_core::distro::clickhouse_answers(record.stack.ch_http_port), 240)
}

/// Move ClickHouse's disk: stop SENTIENT and ClickHouse, move the file as
/// the account, start both, check. A failure after the move moves it back.
pub fn run(record: &InstallationRecord, moves: &[Move], sink: ProgressFn) -> Result<(), String> {
    let Some(m) = moves.iter().find(|m| m.store == Store::Ch).cloned() else {
        return Err("Only ClickHouse's WSL disk moves on Windows.".into());
    };
    let c = check(record, Store::Ch, &m.to);
    if let Some(b) = c.blocks.first() {
        return Err(format!("{} {}", b.title, b.why));
    }
    let old_dir = clickhouse_wsl::disk_dir().display().to_string();
    let t0 = std::time::Instant::now();
    let mut out = Outcome { ended: Ended::SwitchedBack, reason: None, failed_step: None, locations: Default::default(), old_copies: Vec::new(), backups_dir: None, rows: Vec::new(), down_secs: 0 };
    let platform = record.platform();
    sink(Progress::Step { name: "Stopping SENTIENT".into() });
    let mut result = platform.control(sink.clone(), "stop");
    if result.is_ok() && !clickhouse_wsl::stop_clickhouse(record.stack.ch_http_port, std::time::Duration::from_secs(150), |_| {}) {
        result = Err("ClickHouse did not stop cleanly in time; nothing was moved.".into());
    }
    let mut moved = false;
    if result.is_ok() {
        result = agent_move(record, &m.to, &sink);
        moved = result.is_ok();
    }
    if result.is_ok() {
        sink(Progress::Step { name: "Starting ClickHouse, then the SENTIENT service".into() });
        if !start_clickhouse(record) {
            result = Err("ClickHouse did not answer within 4 minutes after the move.".into());
        } else {
            result = platform.control(sink.clone(), "start");
        }
    }
    if result.is_ok() {
        sink(Progress::Step { name: "Checking SENTIENT answers".into() });
        if !wait(|| sentient_installer_core::native::http_answers(record.stack.http_port), 180) {
            result = Err("SENTIENT did not answer within 3 minutes after the move.".into());
        }
    }
    out.down_secs = t0.elapsed().as_secs();
    match result {
        Ok(()) => {
            out.ended = Ended::Moved;
            sink(Progress::Log { line: format!("{}{}", move_data::OUTCOME_MARK, serde_json::to_string(&out).unwrap_or_default()) });
            sink(Progress::Done { message: format!("ClickHouse's disk is in {}.", m.to) });
            Ok(())
        }
        Err(e) => {
            if moved {
                sink(Progress::Step { name: "Pointing the volumes back at their old places".into() });
                let _ = clickhouse_wsl::stop_clickhouse(record.stack.ch_http_port, std::time::Duration::from_secs(150), |_| {});
                if let Err(b) = agent_move(record, &old_dir, &sink) {
                    out.ended = Ended::Broken;
                    out.reason = Some(format!("{e} Moving it back failed too: {b}"));
                }
            }
            sink(Progress::Step { name: "Starting SENTIENT on the old places".into() });
            let _ = start_clickhouse(record);
            let _ = platform.control(sink.clone(), "start");
            if out.reason.is_none() {
                out.reason = Some(e.clone());
            }
            sink(Progress::Log { line: format!("{}{}", move_data::OUTCOME_MARK, serde_json::to_string(&out).unwrap_or_default()) });
            Err(out.reason.clone().unwrap_or(e))
        }
    }
}

pub fn remove_old(_record: &InstallationRecord, path: &str, _sink: ProgressFn) -> Result<(), String> {
    Err(format!("{path}: WSL moves the disk itself, so no old copy is kept on Windows."))
}
