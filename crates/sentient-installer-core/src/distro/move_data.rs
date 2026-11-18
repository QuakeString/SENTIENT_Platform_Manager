// Copyright © 2016-2026 The SENTIENT Authors

//! Moving a store's data to another disk: PostgreSQL, ClickHouse, SENTIENT's
//! files (each a named Docker volume) and the backups folder.
//!
//! A volume keeps its name (`st-pg-data`, `st-ch-data`, `st-data`); only its
//! place changes: the compose file binds it to a host folder
//! (`driver: local`, `driver_opts: {type: none, o: bind, device: <folder>}`).
//! Compose refuses a volume whose definition changed, so the switch removes
//! the volume (never its data) and lets Compose create it again.
//!
//! One run moves one store or several in a single stop of SENTIENT:
//! 1. stop SENTIENT; 2. copy each store with progress; 3. verify every file
//! (the same tree, the same SHA-256); 4. point each volume at its new folder;
//! 5. start; 6. check SENTIENT answers and the databases hold the same rows.
//! A failure puts every volume back where it was and starts SENTIENT there.
//! The old copy is kept until [`remove_old`]; a plain named volume's data is
//! set aside next to Docker's volumes (`<docker root>/spm-moved/…`) by a
//! rename, so it costs no space and no time.
//!
//! Everything that touches the data runs in a short-lived helper container
//! from the stack's own PostgreSQL image (always there, busybox inside) with
//! the host's `/` at `/host`: the volumes are root's, and Docker access is
//! what the Platform Manager has.

use serde::{Deserialize, Serialize};

use super::{run_output, run_stream, shell_args, shell_quote, write_host_file, write_stack_files_exact, DeployConfig, CH_IMAGE, PG_IMAGE_STOCK, PG_IMAGE_TIMESCALE};
use crate::progress::{Progress, ProgressFn};
use crate::sys;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A place SENTIENT keeps data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Store {
    Pg,
    Ch,
    Data,
    Backups,
}

impl Store {
    pub const ALL: [Store; 4] = [Store::Pg, Store::Ch, Store::Data, Store::Backups];

    pub fn as_str(self) -> &'static str {
        match self {
            Store::Pg => "pg",
            Store::Ch => "ch",
            Store::Data => "data",
            Store::Backups => "backups",
        }
    }

    pub fn parse(s: &str) -> Option<Store> {
        Store::ALL.into_iter().find(|x| x.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Store::Pg => "PostgreSQL",
            Store::Ch => "ClickHouse",
            Store::Data => "SENTIENT files",
            Store::Backups => "Backups",
        }
    }

    /// The compose service whose container mounts the store's volume.
    fn service(self) -> Option<&'static str> {
        match self {
            Store::Pg => Some("postgres"),
            Store::Ch => Some("clickhouse"),
            Store::Data => Some("sentient"),
            Store::Backups => None,
        }
    }

    /// A database: a network or FAT disk cannot hold it.
    pub fn is_database(self) -> bool {
        matches!(self, Store::Pg | Store::Ch)
    }

    /// Moving it stops SENTIENT.
    pub fn stops(self) -> bool {
        self != Store::Backups
    }
}

/// What kind of disk a folder is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskKind {
    Local,
    /// cifs, nfs, sshfs…: too slow and unsafe for a database.
    Network,
    /// vfat, exfat, ntfs through FUSE: no owners, permissions or (FAT) files
    /// over 4 GB.
    Fat,
    /// Mounted read-only (or a read-only filesystem such as squashfs).
    ReadOnly,
    /// tmpfs, proc, overlay…: not a disk.
    Pseudo,
}

/// One mounted filesystem of the server.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Disk {
    /// The device or share (`/dev/sdb1`, `//nas/plant`).
    pub source: String,
    pub mount: String,
    pub fstype: String,
    pub size: u64,
    pub avail: u64,
    pub kind: DiskKind,
}

/// Where a store keeps its data now.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub store: Store,
    /// The Docker volume (empty for the backups folder).
    pub volume: String,
    /// The folder on the host holding the data.
    pub path: String,
    /// Bound to a folder of the operator's choice (else Docker's own folder).
    pub bind: bool,
    pub bytes: Option<u64>,
    /// The disk it is on (its mount point among [`Overview::disks`]).
    pub mount: Option<String>,
}

/// A place data was moved away from (or a partial copy a failed move left):
/// kept until removed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OldCopy {
    pub store: Store,
    pub path: String,
    pub bytes: u64,
    /// RFC 3339.
    pub at: String,
    /// The copy a failed move left at its target, not the old data.
    #[serde(default)]
    pub partial: bool,
}

/// Everything the move flow shows before it starts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Overview {
    pub places: Vec<Place>,
    pub disks: Vec<Disk>,
    pub old_copies: Vec<OldCopy>,
    /// SENTIENT runs from a compose file the Platform Manager did not write:
    /// take it over first.
    pub external: bool,
    pub docker_root: String,
}

/// Why a folder cannot take a store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    NotAbsolute,
    Network,
    Fat,
    ReadOnly,
    Pseudo,
    NoPermission,
    NotEmpty,
    TooSmall,
    SamePlace,
    /// Inside another store's data, or holding it.
    Overlaps,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub kind: BlockKind,
    pub title: String,
    pub why: String,
    /// The ways out, in order.
    pub fixes: Vec<String>,
}

/// A folder checked for one store.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Check {
    pub path: String,
    pub disk: Option<Disk>,
    pub exists: bool,
    pub empty: bool,
    pub needed: u64,
    pub blocks: Vec<Block>,
}

impl Check {
    pub fn ok(&self) -> bool {
        self.blocks.is_empty()
    }
}

/// One store to move, and where.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Move {
    pub store: Store,
    pub to: String,
}

/// How long the moves take.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Estimate {
    /// (store, bytes, seconds to copy and verify) in order.
    pub moves: Vec<(Store, u64, u64)>,
    /// SENTIENT is down this long (stop, copies, checks, start).
    pub down_secs: u64,
    /// The copy rate measured on these disks, MB/s.
    pub rate_mb_s: f64,
}

/// How a run ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ended {
    Moved,
    /// A step failed and every volume is back where it was.
    SwitchedBack,
    /// Putting it back failed too: the reason says what is where.
    Broken,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub ended: Ended,
    /// Why it did not move (the failed step's error).
    pub reason: Option<String>,
    /// The step that failed (its name as reported).
    pub failed_step: Option<String>,
    /// The stack's locations and old copies after the run (the caller saves
    /// them in its record).
    pub locations: super::Locations,
    pub old_copies: Vec<OldCopy>,
    /// The backups folder after the run, when it moved.
    pub backups_dir: Option<String>,
    /// "ClickHouse holds the same 2 812 440 118 rows as before."
    pub rows: Vec<String>,
    pub down_secs: u64,
}

/// The marker before the outcome's JSON in a job's log lines.
pub const OUTCOME_MARK: &str = "MOVED\t";

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn step(sink: &ProgressFn, name: impl Into<String>) {
    sink(Progress::Step { name: name.into() });
}

fn log(sink: &ProgressFn, line: impl Into<String>) {
    sink(Progress::Log { line: line.into() });
}

fn capture(bash: &str) -> Result<String, String> {
    let (prog, args) = shell_args(bash);
    let argrefs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    // A read (the Data locations card) never starts a stopped distro.
    if super::left_out(&argrefs) {
        return Err("SENTIENT's WSL distro is stopped".into());
    }
    match sys::output(&prog, &argrefs) {
        Some((true, out, _)) => Ok(sys::decode(&out)),
        Some((false, out, err)) => {
            let e = sys::decode(&err);
            Err(if e.trim().is_empty() { sys::decode(&out).trim().to_string() } else { e.trim().to_string() })
        }
        None => Err(format!("could not run {prog}")),
    }
}

/// "612 GB", "2.1 GB", "840 MB".
pub fn human(bytes: u64) -> String {
    crate::size::label(bytes)
}

fn now_rfc3339() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

/// `p` without a trailing slash, `/` kept.
fn norm(p: &str) -> String {
    let t = p.trim();
    let t = t.trim_end_matches('/');
    if t.is_empty() { "/".into() } else { t.to_string() }
}

/// `a` is `b` or inside it.
fn within(a: &str, b: &str) -> bool {
    let (a, b) = (norm(a), norm(b));
    a == b || b == "/" || a.starts_with(&format!("{b}/"))
}

// ---------------------------------------------------------------------------
// Docker facts
// ---------------------------------------------------------------------------

fn docker_root() -> String {
    capture("docker info -f '{{.DockerRootDir}}'").map(|s| s.trim().to_string()).ok().filter(|s| s.starts_with('/')).unwrap_or_else(|| "/var/lib/docker".into())
}

/// A volume as Docker has it: its mount point and, when bound, the folder.
#[derive(Debug, Clone, Default)]
struct VolumeInfo {
    mountpoint: String,
    device: Option<String>,
}

fn volume_info(name: &str) -> Option<VolumeInfo> {
    let out = capture(&format!("docker volume inspect {}", shell_quote(name))).ok()?;
    let v: serde_json::Value = serde_json::from_str(&out).ok()?;
    let v = v.as_array()?.first()?.clone();
    let opts = &v["Options"];
    let bind = opts["o"].as_str().is_some_and(|o| o.split(',').any(|x| x == "bind")) && opts["type"].as_str() == Some("none");
    Some(VolumeInfo {
        mountpoint: v["Mountpoint"].as_str().unwrap_or_default().to_string(),
        device: if bind { opts["device"].as_str().map(str::to_string) } else { None },
    })
}

fn volume_of(cfg: &DeployConfig, store: Store) -> Option<String> {
    let n = cfg.names();
    match store {
        Store::Pg => Some(n.vol_pg),
        Store::Ch => cfg.flavor.uses_clickhouse().then_some(n.vol_ch),
        Store::Data => Some(n.vol_data),
        Store::Backups => None,
    }
}

/// `cfg` with each store's folder as Docker has it, where the record has
/// none (a volume bound before the record knew, a stack taken over).
pub fn with_actual_locations(cfg: &DeployConfig) -> DeployConfig {
    let mut out = cfg.clone();
    if cfg.external.is_some() {
        return out;
    }
    for store in [Store::Pg, Store::Ch, Store::Data] {
        if out.locations.get(store).is_some() {
            continue;
        }
        if let Some(dev) = volume_of(cfg, store).and_then(|v| volume_info(&v)).and_then(|i| i.device) {
            out.locations.set(store, Some(dev));
        }
    }
    out
}

/// The image the helper container runs: the stack's PostgreSQL image (it is
/// there), else ClickHouse's, else alpine.
fn helper_image(cfg: &DeployConfig) -> String {
    let pg = if cfg.flavor.uses_timescaledb() || cfg.keep_timescale_image { PG_IMAGE_TIMESCALE } else { PG_IMAGE_STOCK };
    for img in [pg, PG_IMAGE_STOCK, PG_IMAGE_TIMESCALE, CH_IMAGE, "alpine:3.20", "alpine:latest"] {
        if run_output(&format!("docker image inspect {} >/dev/null 2>&1", shell_quote(img))).is_some_and(|(ok, _)| ok) {
            return img.to_string();
        }
    }
    pg.to_string()
}

/// A name of its own per helper run: two readings at once (the card and
/// the flow) must not remove each other's helper.
fn helper_name(cfg: &DeployConfig) -> String {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    format!("{}-spm-move-{}-{}", cfg.stack_name, std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

/// `docker run` of the helper with `script` (sh, busybox), the host's `/`
/// at `/host`.
fn helper_cmd(name: &str, image: &str, script: &str) -> String {
    format!(
        "docker run --rm --name {} --label spm.helper=move-data --network none --user 0:0 --entrypoint sh -v /:/host {} -c {}",
        shell_quote(name),
        shell_quote(image),
        shell_quote(script)
    )
}

fn helper_capture(cfg: &DeployConfig, image: &str, script: &str) -> Result<String, String> {
    capture(&helper_cmd(&helper_name(cfg), image, script))
}

/// The cancel request file of a stack's move (the agent's Cancel writes it).
fn cancel_file(cfg: &DeployConfig) -> String {
    format!("{}/move-data.cancel", cfg.stack_dir())
}

/// Ask a running move to stop and put everything back.
pub fn request_cancel(cfg: &DeployConfig) -> Result<(), String> {
    write_host_file(&crate::progress::noop(), &cancel_file(cfg), "cancel\n")
}

fn cancelled(cfg: &DeployConfig) -> bool {
    crate::cancel::is_cancelled() || run_output(&format!("test -f {}", shell_quote(&cancel_file(cfg)))).is_some_and(|(ok, _)| ok)
}

/// Run the helper with `script`, each output line to `on_line`. A cancel
/// request kills the helper.
fn helper_stream(cfg: &DeployConfig, image: &str, script: &str, mut on_line: impl FnMut(&str)) -> Result<(), String> {
    use std::io::BufRead;
    let name = helper_name(cfg);
    let (prog, args) = shell_args(&format!("{} 2>&1", helper_cmd(&name, image, script)));
    let argrefs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    if super::left_out(&argrefs) {
        return Err("SENTIENT's WSL distro is stopped".into());
    }
    let mut child = sys::command(&prog)
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("could not run {prog}: {e}"))?;
    let stdout = child.stdout.take().expect("piped");
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let killed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watcher = {
        let (done, killed, cfg, name) = (done.clone(), killed.clone(), cfg.clone(), name.clone());
        std::thread::spawn(move || {
            while !done.load(std::sync::atomic::Ordering::SeqCst) {
                if cancelled(&cfg) {
                    killed.store(true, std::sync::atomic::Ordering::SeqCst);
                    let _ = run_output(&format!("docker kill {} >/dev/null 2>&1", shell_quote(&name)));
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        })
    };
    let mut last = Vec::new();
    for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
        on_line(&line);
        if !line.starts_with("SPM") {
            last.push(line);
            if last.len() > 8 {
                last.remove(0);
            }
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = watcher.join();
    if killed.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("Cancelled.".into());
    }
    if status.success() {
        Ok(())
    } else {
        Err(last.join("\n").trim().to_string())
    }
}

/// `host` as the helper sees it.
fn h(path: &str) -> String {
    shell_quote(&format!("/host{}", norm(path)))
}

// ---------------------------------------------------------------------------
// Disks
// ---------------------------------------------------------------------------

/// What a filesystem type means for a database.
pub fn kind_of(fstype: &str, read_only: bool) -> DiskKind {
    let t = fstype.to_ascii_lowercase();
    let t = t.strip_prefix("fuse.").unwrap_or(&t);
    if ["cifs", "smb3", "smbfs", "nfs", "nfs4", "sshfs", "glusterfs", "ceph", "9p", "davfs", "afs", "lustre", "drvfs", "virtiofs"].contains(&t) {
        DiskKind::Network
    } else if ["vfat", "fat", "msdos", "exfat", "ntfs", "ntfs3", "ntfs-3g", "fuseblk"].contains(&t) {
        DiskKind::Fat
    } else if ["tmpfs", "devtmpfs", "proc", "sysfs", "cgroup", "cgroup2", "devpts", "mqueue", "debugfs", "tracefs", "securityfs", "pstore", "efivarfs", "bpf", "autofs", "hugetlbfs", "fusectl", "configfs", "binfmt_misc", "ramfs", "nsfs", "rpc_pipefs", "unknown"].contains(&t) {
        DiskKind::Pseudo
    } else if read_only || ["squashfs", "iso9660", "udf", "erofs"].contains(&t) {
        DiskKind::ReadOnly
    } else {
        DiskKind::Local
    }
}

/// `df -B1 -P -T` output: (source, fstype, size, avail, mount).
pub(crate) fn parse_df(text: &str) -> Vec<(String, String, u64, u64, String)> {
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 7 {
                return None;
            }
            // The mount point is the rest (spaces in it).
            Some((f[0].to_string(), f[1].to_string(), f[2].parse().ok()?, f[4].parse().ok()?, f[6..].join(" ")))
        })
        .collect()
}

fn read_only_mounts() -> std::collections::HashSet<String> {
    capture("cat /proc/self/mounts")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(' ').collect();
            (f.len() > 3 && f[3].split(',').any(|o| o == "ro")).then(|| f[1].replace("\\040", " "))
        })
        .collect()
}

/// The server's disks a store could go to, the system's own pseudo and
/// Docker-internal filesystems left out.
pub fn disks() -> Vec<Disk> {
    let ro = read_only_mounts();
    let out = capture("timeout 15 df -B1 -P -T 2>/dev/null; true").unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    let mut list: Vec<Disk> = parse_df(&out)
        .into_iter()
        .filter(|(_, _, _, _, m)| !(m.starts_with("/var/lib/docker") || m.starts_with("/run") || m.starts_with("/sys") || m.starts_with("/proc") || m.starts_with("/dev") || m.starts_with("/boot") || m.starts_with("/snap/") || m.contains("/containers/") || m == "/etc/hosts" || m == "/etc/hostname" || m == "/etc/resolv.conf"))
        .filter_map(|(src, fs, size, avail, mount)| {
            let kind = kind_of(&fs, ro.contains(&mount));
            if kind == DiskKind::Pseudo || size == 0 {
                return None;
            }
            // One row per filesystem (a bind mount repeats its source).
            if !seen.insert((src.clone(), size)) {
                return None;
            }
            Some(Disk { source: src, mount, fstype: fs, size, avail, kind })
        })
        .collect();
    list.sort_by(|a, b| a.mount.cmp(&b.mount));
    list
}

/// The disk (by longest mount point) a path is on.
pub fn disk_of<'a>(disks: &'a [Disk], path: &str) -> Option<&'a Disk> {
    disks.iter().filter(|d| within(path, &d.mount)).max_by_key(|d| d.mount.len())
}

/// The filesystem holding `path` (its nearest existing folder), from the
/// host's own view.
fn disk_at(path: &str) -> Option<Disk> {
    let mut p = std::path::PathBuf::from(norm(path));
    // Walk up to a folder that exists (a new folder is created later).
    let anc = loop {
        let s = p.display().to_string();
        let there = run_output(&format!("test -e {}", shell_quote(&s))).is_some_and(|(ok, _)| ok);
        if there || !p.pop() {
            break s;
        }
    };
    let out = capture(&format!("timeout 15 df -B1 -P -T {}", shell_quote(&anc))).ok()?;
    let (src, fs, size, avail, mount) = parse_df(&out).into_iter().next()?;
    let ro = read_only_mounts().contains(&mount);
    let kind = kind_of(&fs, ro);
    Some(Disk { source: src, mount, fstype: fs, size, avail, kind })
}

// ---------------------------------------------------------------------------
// Overview
// ---------------------------------------------------------------------------

/// Sizes of host folders in bytes (du as root in the helper), in order.
fn sizes(cfg: &DeployConfig, paths: &[String]) -> Vec<Option<u64>> {
    if paths.is_empty() {
        return Vec::new();
    }
    let script: String = paths.iter().map(|p| format!("echo \"SPMSIZE $(du -sk {} 2>/dev/null | cut -f1)\";", h(p))).collect();
    let out = helper_capture(cfg, &helper_image(cfg), &script).unwrap_or_default();
    let vals: Vec<Option<u64>> = out.lines().filter_map(|l| l.strip_prefix("SPMSIZE")).map(|v| v.trim().parse::<u64>().ok().map(|k| k * 1024)).collect();
    (0..paths.len()).map(|i| vals.get(i).copied().flatten()).collect()
}

/// Where each store is, its size and disk; the server's disks; the old
/// copies kept. `backups`: the backups folder, when the caller has one.
pub fn overview(cfg: &DeployConfig, backups: Option<&str>) -> Result<Overview, String> {
    let root = docker_root();
    let mut places = Vec::new();
    for store in [Store::Pg, Store::Ch, Store::Data] {
        let Some(vol) = volume_of(cfg, store) else { continue };
        let info = volume_info(&vol);
        let (path, bind) = match (&info, cfg.locations.get(store)) {
            (Some(VolumeInfo { device: Some(d), .. }), _) => (d.clone(), true),
            (Some(i), _) if !i.mountpoint.is_empty() => (i.mountpoint.clone(), false),
            (_, Some(l)) => (l.clone(), true),
            _ => (format!("{root}/volumes/{vol}/_data"), false),
        };
        places.push(Place { store, volume: vol, path, bind, bytes: None, mount: None });
    }
    if let Some(b) = backups.filter(|b| !b.trim().is_empty()) {
        places.push(Place { store: Store::Backups, volume: String::new(), path: norm(b), bind: true, bytes: None, mount: None });
    }
    let paths: Vec<String> = places.iter().map(|p| p.path.clone()).collect();
    for (p, s) in places.iter_mut().zip(sizes(cfg, &paths)) {
        p.bytes = s;
    }
    let mut disks = disks();
    // The disks the stores are on, even when the list left them out (a
    // store under /var/lib/docker on its own filesystem).
    for p in &places {
        if disk_of(&disks, &p.path).is_none() {
            if let Some(d) = disk_at(&p.path) {
                if !disks.iter().any(|x| x.mount == d.mount) {
                    disks.push(d);
                }
            }
        }
    }
    for p in places.iter_mut() {
        p.mount = disk_of(&disks, &p.path).map(|d| d.mount.clone());
    }
    Ok(Overview { places, disks, old_copies: cfg.old_copies.clone(), external: cfg.external.is_some(), docker_root: root })
}

// ---------------------------------------------------------------------------
// Checking a target folder
// ---------------------------------------------------------------------------

fn block(kind: BlockKind, title: String, why: String, fixes: &[&str]) -> Block {
    Block { kind, title, why, fixes: fixes.iter().map(|s| s.to_string()).collect() }
}

/// The disk's own verdict for `store` (network, FAT, read-only, too small),
/// for the disk list and for [`check`].
pub fn disk_blocks(disk: &Disk, store: Store, needed: u64, label: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let name = label;
    match disk.kind {
        DiskKind::Network if store.is_database() => out.push(block(
            BlockKind::Network,
            format!("{} cannot hold a database", disk.mount),
            format!("It is a network disk ({}): too slow and unsafe for a database; {name} could lose data on it.", disk.fstype),
            &["Pick a local disk."],
        )),
        DiskKind::Fat if store != Store::Backups => out.push(block(
            BlockKind::Fat,
            format!("{} cannot hold {}", disk.mount, if store.is_database() { "a database" } else { name }),
            format!("It is {} ({}): no owners or permissions, and no file over 4 GB. {name} would not start on it.", if disk.fstype.contains("ntfs") { "NTFS" } else { "FAT" }, disk.fstype),
            &["Pick another disk, or", "format it as ext4 or xfs outside the Platform Manager (this erases it)."],
        )),
        DiskKind::ReadOnly => out.push(block(
            BlockKind::ReadOnly,
            format!("{} is read-only", disk.mount),
            format!("Nothing can be written to it ({}, mounted read-only).", disk.fstype),
            &["Pick another disk, or", "mount it read-write first."],
        )),
        DiskKind::Pseudo => out.push(block(
            BlockKind::Pseudo,
            format!("{} is not a disk", disk.mount),
            format!("It is a {} filesystem, held in memory or by the system.", disk.fstype),
            &["Pick a disk."],
        )),
        _ => {}
    }
    if out.is_empty() && needed > 0 && disk.avail < needed {
        out.push(block(
            BlockKind::TooSmall,
            format!("{name} does not fit on {}", disk.mount),
            format!("It needs {}; {} has {} free.", human(needed), disk.mount, human(disk.avail)),
            &["Pick a disk with room, or", &format!("free space on {} first.", disk.mount)],
        ));
    }
    out
}

/// Whether `path` can take `store` (whose data is `needed` bytes): its disk,
/// whether it is empty, whether root can write there, and that it is not
/// any store's current place. Creates nothing.
pub fn check(cfg: &DeployConfig, store: Store, path: &str, needed: u64, places: &[Place]) -> Check {
    let label = store.label();
    let mut c = Check { path: norm(path), needed, ..Default::default() };
    if !path.trim().starts_with('/') {
        c.blocks.push(block(BlockKind::NotAbsolute, "Not a full path".into(), "The folder must be a full path from /, such as /mnt/disk-c/sentient/clickhouse.".into(), &["Pick the folder with Browse."]));
        return c;
    }
    let path = c.path.clone();
    if path == "/" {
        c.blocks.push(block(BlockKind::NotEmpty, "/ cannot be the folder".into(), "Pick a folder on the disk, not the disk's top.".into(), &["Make a new folder, such as sentient/clickhouse."]));
        return c;
    }
    for p in places {
        if p.store == store && norm(&p.path) == path {
            c.blocks.push(block(BlockKind::SamePlace, format!("{label} is already here"), format!("{path} is where {label} keeps its data now."), &["Pick another folder."]));
            return c;
        }
        if within(&path, &p.path) || within(&p.path, &path) {
            c.blocks.push(block(
                BlockKind::Overlaps,
                format!("{path} overlaps {}'s data", p.store.label()),
                format!("{} keeps its data in {}; one store's folder cannot be inside another's.", p.store.label(), p.path),
                &["Pick a folder of its own, such as a new one beside it."],
            ));
            return c;
        }
    }
    let disk = disk_at(&path);
    if let Some(d) = &disk {
        // The data's own disk already holds it: moving within one disk
        // needs no room for a second copy only when it is renamed, which
        // the move does not do; it copies.
        c.blocks.extend(disk_blocks(d, store, needed, label));
    }
    c.disk = disk;
    if !c.blocks.is_empty() {
        return c;
    }
    // Exists, empty, root can write in its nearest existing folder.
    let script = format!(
        "p={p}; if [ -e \"$p\" ]; then echo SPMEXISTS; if [ -d \"$p\" ] && [ -z \"$(ls -A \"$p\" 2>/dev/null | grep -v '^lost+found$')\" ]; then echo SPMEMPTY; fi; fi; \
         d=\"$p\"; while [ ! -e \"$d\" ]; do d=$(dirname \"$d\"); done; \
         t=\"$d/.spm-write-test-$$\"; if ( : > \"$t\" ) 2>/dev/null; then rm -f \"$t\"; echo SPMWRITABLE; fi",
        p = h(&path)
    );
    match helper_capture(cfg, &helper_image(cfg), &script) {
        Ok(out) => {
            c.exists = out.contains("SPMEXISTS");
            c.empty = !c.exists || out.contains("SPMEMPTY");
            if c.exists && !c.empty {
                c.blocks.push(block(BlockKind::NotEmpty, format!("{path} is not empty"), format!("{label} needs an empty folder; this one has files in it."), &["Make a new folder inside it, or", "pick an empty folder."]));
            }
            if !out.contains("SPMWRITABLE") {
                c.blocks.push(block(
                    BlockKind::NoPermission,
                    format!("{label} cannot write here"),
                    format!("Even an administrator cannot write in {path} (a read-only or locked filesystem, or a share that maps root to nobody)."),
                    &["Pick another folder, or", "make it writable on the server first."],
                ));
            }
        }
        Err(e) => c.blocks.push(block(BlockKind::NoPermission, format!("{path} could not be checked"), e, &["Try again, or pick another folder."])),
    }
    c
}

// ---------------------------------------------------------------------------
// Estimate
// ---------------------------------------------------------------------------

/// Write 256 MB to each target's disk (synced) to measure the rate, and
/// work out SENTIENT's downtime: stop, copy, verify, start.
pub fn estimate(cfg: &DeployConfig, moves: &[Move], places: &[Place]) -> Estimate {
    let mut rate: f64 = 0.;
    let image = helper_image(cfg);
    for m in moves {
        let script = format!(
            "d={}; while [ ! -e \"$d\" ]; do d=$(dirname \"$d\"); done; f=\"$d/.spm-speed-$$\"; \
             s=$(cut -d' ' -f1 /proc/uptime); dd if=/dev/zero of=\"$f\" bs=1M count=256 conv=fsync 2>/dev/null; \
             e=$(cut -d' ' -f1 /proc/uptime); rm -f \"$f\"; echo \"SPMUP $s $e\"",
            h(&m.to)
        );
        let secs = helper_capture(cfg, &image, &script).ok().and_then(|o| {
            o.lines().find_map(|l| {
                let mut p = l.strip_prefix("SPMUP ")?.split_whitespace().map(|v| v.parse::<f64>().ok());
                let (start, end) = (p.next()??, p.next()??);
                Some(end - start)
            })
        });
        if let Some(secs) = secs {
            if secs > 0. {
                let r = 256. * 1.048_576 / secs;
                rate = if rate == 0. { r } else { rate.min(r) };
            }
        }
    }
    if rate <= 0. {
        rate = 150.;
    }
    // A cache can make a 256 MB write look faster than a disk keeps up.
    let rate = rate.min(1500.);
    let verify_rate = (rate * 1.5).min(400.);
    let mut total = 0;
    let mut stops = false;
    let list = moves
        .iter()
        .map(|m| {
            let bytes = places.iter().find(|p| p.store == m.store).and_then(|p| p.bytes).unwrap_or(0);
            let mb = bytes as f64 / 1e6;
            let secs = (mb / rate + mb / verify_rate).ceil() as u64;
            if m.store.stops() {
                total += secs;
                stops = true;
            }
            (m.store, bytes, secs)
        })
        .collect();
    let down = if stops { total + 30 + 90 } else { 0 };
    Estimate { moves: list, down_secs: down, rate_mb_s: rate }
}

/// "about 42 minutes".
pub fn about(secs: u64) -> String {
    let m = (secs + 59) / 60;
    if m <= 1 {
        "about a minute".into()
    } else if m < 90 {
        format!("about {m} minutes")
    } else {
        format!("about {:.1} hours", m as f64 / 60.)
    }
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

/// A move as it goes.
struct Live {
    m: Move,
    volume: String,
    /// The data's place before the move.
    from: String,
    /// The volume was a plain named one (its data in Docker's folder).
    was_named: bool,
    bytes: u64,
    copied: bool,
    /// The named volume's data, set aside here by the switch.
    set_aside: Option<String>,
    switched: bool,
}

fn rows_pg(cfg: &DeployConfig) -> Option<u64> {
    // Exact counts of every user table under 1 GB (the bigger ones are
    // verified byte for byte by the copy's checksums).
    let sql = "SELECT coalesce(sum((xpath('/row/c/text()', query_to_xml(format('select count(*) as c from %I.%I', schemaname, relname), false, true, '')))[1]::text::bigint),0) FROM pg_stat_user_tables WHERE pg_relation_size(relid) < 1073741824";
    super::docker_ops::psql(cfg, &cfg.db_name, sql).ok().and_then(|s| s.trim().parse().ok())
}

fn rows_ch(cfg: &DeployConfig) -> Option<u64> {
    let n = cfg.names();
    capture(&format!(
        "docker exec {} clickhouse-client --user {} --password {} -q {}",
        shell_quote(&n.ch),
        shell_quote(&cfg.ch_user),
        shell_quote(&cfg.ch_password),
        shell_quote("SELECT sum(rows) FROM system.parts WHERE active AND lower(database) NOT IN ('system', 'information_schema')")
    ))
    .ok()
    .and_then(|s| s.trim().parse().ok())
}

fn wait_rows(cfg: &DeployConfig, store: Store, secs: u64) -> Option<u64> {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        let r = match store {
            Store::Pg => rows_pg(cfg),
            Store::Ch => rows_ch(cfg),
            _ => None,
        };
        if r.is_some() || std::time::Instant::now() > until {
            return r;
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}

/// "2 812 440 118".
pub fn grouped(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push('\u{2009}');
        }
        out.push(c);
    }
    out
}

/// A test may make one step fail (debug builds only): `verify` corrupts
/// one copied file, `health` fails the check after the switch.
fn forced_failure(at: &str) -> bool {
    cfg!(debug_assertions) && std::env::var("SPM_MOVE_FAIL").is_ok_and(|v| v == at)
}

fn copy_one(cfg: &DeployConfig, image: &str, sink: &ProgressFn, l: &Live) -> Result<(), String> {
    let src = h(&l.from);
    let dst = h(&l.m.to);
    let script = format!(
        "set -e -o pipefail; src={src}; dst={dst}; mkdir -p \"$dst\"; \
         chown \"$(stat -c %u:%g \"$src\")\" \"$dst\"; chmod \"$(stat -c %a \"$src\")\" \"$dst\"; \
         echo \"SPMTOTAL $(du -sk \"$src\" | cut -f1)\"; \
         ( while :; do sleep 2; echo \"SPMDONE $(du -sk \"$dst\" 2>/dev/null | cut -f1)\"; done ) & w=$!; \
         tar -C \"$src\" --numeric-owner -cf - . | tar -C \"$dst\" --numeric-owner -xf -; \
         kill $w 2>/dev/null || true; echo \"SPMDONE $(du -sk \"$dst\" | cut -f1)\"; sync"
    );
    let mut total: f64 = 0.;
    let started = std::time::Instant::now();
    helper_stream(cfg, image, &script, |line| {
        if let Some(v) = line.strip_prefix("SPMTOTAL ") {
            total = v.trim().parse::<f64>().unwrap_or(0.) * 1024.;
        } else if let Some(v) = line.strip_prefix("SPMDONE ") {
            let done = v.trim().parse::<f64>().unwrap_or(0.) * 1024.;
            if total > 0. {
                let frac = (done / total).clamp(0., 1.) as f32;
                sink(Progress::Percent { value: frac });
                let secs = started.elapsed().as_secs_f64().max(0.5);
                let rate = done / secs;
                let left = if rate > 0. { ((total - done).max(0.) / rate) as u64 } else { 0 };
                log(sink, format!("{} of {} · {:.0} MB/s · {} left", human(done as u64), human(total as u64), rate / 1e6, if left >= 60 { format!("{} min", left.div_ceil(60)) } else { format!("{left} s") }));
            }
        } else if !line.trim().is_empty() {
            log(sink, line.to_string());
        }
    })
    .map_err(|e| format!("The copy of {} to {} failed: {e}", l.m.store.label(), l.m.to))
}

/// The same tree, and the same SHA-256 for every file. Err names how many
/// files differ.
fn verify_one(cfg: &DeployConfig, image: &str, sink: &ProgressFn, l: &Live) -> Result<(), String> {
    if forced_failure("verify") {
        // A real difference for the check to find: one byte more in the
        // biggest copied file.
        let _ = helper_capture(cfg, image, &format!("f=$(find {} -type f -exec ls -s {{}} + | sort -n | tail -1 | awk '{{print $2}}'); [ -n \"$f\" ] && printf x >> \"$f\"", h(&l.m.to)));
    }
    let script = format!(
        "src={src}; dst={dst}; \
         (cd \"$src\" && find . | sort) > /tmp/a.l; (cd \"$dst\" && find . | sort) > /tmp/b.l; \
         n=$(grep -c . /tmp/a.l); echo \"SPMFILES $n\"; \
         (cd \"$src\" && find . -type f -exec sha256sum {{}} + | sort -k2) > /tmp/a.s & p1=$!; \
         (cd \"$dst\" && find . -type f -exec sha256sum {{}} + | sort -k2) > /tmp/b.s & p2=$!; \
         m=$(grep -c . /tmp/a.l); while kill -0 $p2 2>/dev/null; do sleep 2; echo \"SPMHASHED $(grep -c . /tmp/b.s 2>/dev/null)\"; done; wait $p1; wait $p2; \
         echo \"SPMMISSING $(diff /tmp/a.l /tmp/b.l | grep -c '^[-+][^-+]')\"; \
         diff /tmp/a.s /tmp/b.s | grep '^[-+][^-+]' | awk '{{print $2}}' | sort -u > /tmp/d; \
         echo \"SPMDIFFER $(grep -c . /tmp/d)\"; head -5 /tmp/d | sed 's/^/SPMFILE /'",
        src = h(&l.from),
        dst = h(&l.m.to)
    );
    let mut files = 0f64;
    let (mut missing, mut differ) = (0u64, 0u64);
    let mut names = Vec::new();
    helper_stream(cfg, image, &script, |line| {
        if let Some(v) = line.strip_prefix("SPMFILES ") {
            files = v.trim().parse().unwrap_or(0.);
        } else if let Some(v) = line.strip_prefix("SPMHASHED ") {
            if files > 0. {
                sink(Progress::Percent { value: (v.trim().parse::<f64>().unwrap_or(0.) / files).clamp(0., 1.) as f32 });
            }
        } else if let Some(v) = line.strip_prefix("SPMMISSING ") {
            missing = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("SPMDIFFER ") {
            differ = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("SPMFILE ") {
            names.push(v.trim_start_matches("./").to_string());
        }
    })?;
    if missing == 0 && differ == 0 {
        log(sink, format!("{}: {} entries, every file the same", l.m.store.label(), files as u64));
        return Ok(());
    }
    let mut why = Vec::new();
    if differ > 0 {
        why.push(format!("{differ} file{} differ{}", if differ == 1 { "" } else { "s" }, if differ == 1 { "s" } else { "" }));
    }
    if missing > 0 {
        why.push(format!("{missing} entr{} missing or extra", if missing == 1 { "y is" } else { "ies are" }));
    }
    let at = if names.is_empty() { String::new() } else { format!(" ({})", names.join(", ")) };
    Err(format!("The copy of {} did not match: {}{at}.", l.m.store.label(), why.join(", ")))
}

/// Remove the containers that mount `stores`' volumes (Compose makes them
/// again), so the volumes can go.
fn release(sink: &ProgressFn, cfg: &DeployConfig, stores: &[Store]) -> Result<(), String> {
    let svcs: Vec<&str> = stores.iter().filter_map(|s| s.service()).collect();
    if svcs.is_empty() {
        return Ok(());
    }
    super::run(sink, &cfg.compose_cmd(&format!("rm -f -s {}", svcs.join(" "))))
}

fn switch_one(cfg: &mut DeployConfig, image: &str, sink: &ProgressFn, l: &mut Live, root: &str) -> Result<(), String> {
    release(sink, cfg, &[l.m.store])?;
    if l.was_named {
        let kept = format!("{root}/spm-moved/{}-{}", l.volume, chrono::Local::now().format("%Y%m%d-%H%M%S"));
        helper_capture(cfg, image, &format!("set -e; mkdir -p {}; mv {} {}", h(&format!("{root}/spm-moved")), h(&l.from), h(&kept)))
            .map_err(|e| format!("Setting the old data of {} aside failed: {e}", l.volume))?;
        l.set_aside = Some(kept);
    }
    super::run(sink, &format!("docker volume rm {}", shell_quote(&l.volume))).map_err(|e| format!("Removing the volume {} (its data stays) failed: {e}", l.volume))?;
    cfg.locations.set(l.m.store, Some(norm(&l.m.to)));
    write_stack_files_exact(sink, cfg)?;
    l.switched = true;
    log(sink, format!("{} now points at {}", l.volume, l.m.to));
    Ok(())
}

/// Put one switched volume back where it was.
fn switch_back_one(cfg: &mut DeployConfig, image: &str, sink: &ProgressFn, l: &mut Live, before: &super::Locations) -> Result<(), String> {
    release(sink, cfg, &[l.m.store])?;
    let _ = super::run(sink, &format!("docker volume rm {}", shell_quote(&l.volume)));
    cfg.locations.set(l.m.store, before.get(l.m.store).cloned());
    if let Some(kept) = l.set_aside.take() {
        // A plain volume again, its data renamed back into it.
        let project = cfg.stack_name.clone();
        let key = match l.m.store {
            Store::Pg => "postgres_data",
            Store::Ch => "clickhouse_data",
            _ => "sentient_data",
        };
        super::run(sink, &format!("docker volume create --label com.docker.compose.project={} --label com.docker.compose.volume={key} {}", shell_quote(&project), shell_quote(&l.volume)))?;
        let mp = volume_info(&l.volume).map(|i| i.mountpoint).filter(|m| !m.is_empty()).ok_or("the recreated volume has no folder")?;
        helper_capture(cfg, image, &format!("set -e; rmdir {mp}; mv {kept} {mp}", mp = h(&mp), kept = h(&kept)))
            .map_err(|e| format!("Putting the data of {} back failed (it is in {kept}): {e}", l.volume))?;
    }
    write_stack_files_exact(sink, cfg)?;
    l.switched = false;
    log(sink, format!("{} points at {} again", l.volume, l.from));
    Ok(())
}

fn start_and_wait(sink: &ProgressFn, cfg: &DeployConfig) -> Result<(), String> {
    run_stream(sink, &cfg.compose_cmd("up -d"))?;
    for _ in 0..90 {
        if super::is_running(cfg.http_port) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    Err("SENTIENT did not answer within 3 minutes".into())
}

/// Move `moves` in one stop. `cfg` carries the stack's passwords
/// (docker_ops::with_secrets). `backups`: the backups folder now, when it
/// is among the moves. Never returns Err: a failure ends in
/// [`Ended::SwitchedBack`] (or [`Ended::Broken`]) with its reason; the
/// outcome is also logged after [`OUTCOME_MARK`] for the agent's caller.
pub fn run(sink: ProgressFn, cfg: &DeployConfig, moves: &[Move], backups: Option<&str>) -> Outcome {
    let out = run_inner(&sink, cfg, moves, backups);
    if let Ok(json) = serde_json::to_string(&out) {
        log(&sink, format!("{OUTCOME_MARK}{json}"));
    }
    let _ = run_output(&format!("rm -f {}", shell_quote(&cancel_file(cfg))));
    out
}

fn run_inner(sink: &ProgressFn, cfg0: &DeployConfig, moves: &[Move], backups: Option<&str>) -> Outcome {
    let mut cfg = with_actual_locations(cfg0);
    let before = cfg.locations.clone();
    let mut outcome = Outcome {
        ended: Ended::SwitchedBack,
        reason: None,
        failed_step: None,
        locations: before.clone(),
        old_copies: cfg.old_copies.clone(),
        backups_dir: None,
        rows: Vec::new(),
        down_secs: 0,
    };
    let fail = |mut o: Outcome, step: &str, why: String| {
        o.failed_step = Some(step.to_string());
        o.reason = Some(why);
        o
    };
    if cfg.external.is_some() {
        return fail(outcome, "Check", super::EXTERNAL_COMPOSE.into());
    }
    let _ = run_output(&format!("rm -f {}", shell_quote(&cancel_file(&cfg))));
    let root = docker_root();
    let image = helper_image(&cfg);
    let ov = match overview(&cfg, backups) {
        Ok(o) => o,
        Err(e) => return fail(outcome, "Check", e),
    };
    // Each move checked again just before it starts.
    let mut live = Vec::new();
    for m in moves {
        let Some(place) = ov.places.iter().find(|p| p.store == m.store) else {
            return fail(outcome, "Check", format!("{} is not part of this installation.", m.store.label()));
        };
        let others: Vec<Place> = ov.places.iter().filter(|p| p.store != m.store).cloned().chain(std::iter::once(place.clone())).collect();
        let mut c = check(&cfg, m.store, &m.to, place.bytes.unwrap_or(0), &others);
        // Two moves to one disk need room for both.
        let same_disk: u64 = moves.iter().filter(|o| o.store != m.store && disk_at(&o.to).map(|d| d.mount) == c.disk.as_ref().map(|d| d.mount.clone())).filter_map(|o| ov.places.iter().find(|p| p.store == o.store).and_then(|p| p.bytes)).sum();
        if let Some(d) = &c.disk {
            if same_disk > 0 && d.avail < place.bytes.unwrap_or(0) + same_disk {
                c.blocks.push(block(BlockKind::TooSmall, format!("The moves do not fit on {}", d.mount), format!("They need {} together; {} has {} free.", human(place.bytes.unwrap_or(0) + same_disk), d.mount, human(d.avail)), &["Move them to different disks."]));
            }
        }
        if let Some(b) = c.blocks.first() {
            return fail(outcome, "Check", format!("{} {}", b.title, b.why));
        }
        live.push(Live { m: m.clone(), volume: place.volume.clone(), from: place.path.clone(), was_named: !place.bind, bytes: place.bytes.unwrap_or(0), copied: false, set_aside: None, switched: false });
    }
    let stops = live.iter().any(|l| l.m.store.stops());
    let rows_of: Vec<Store> = live.iter().map(|l| l.m.store).filter(|s| s.is_database()).collect();
    let t0 = std::time::Instant::now();

    // 1. Stop: SENTIENT first, so the rows are counted with nothing writing,
    // then the databases.
    let mut rows_before: Vec<(Store, Option<u64>)> = Vec::new();
    if stops {
        step(sink, "Stopping SENTIENT");
        let stopped = super::run_stream(sink, &cfg.compose_cmd("stop sentient")).and_then(|_| {
            rows_before = rows_of.iter().map(|s| (*s, wait_rows(&cfg, *s, 10))).collect();
            super::run_stream(sink, &cfg.control_cmd("stop"))
        });
        if let Err(e) = stopped {
            let _ = start_and_wait(sink, &cfg);
            return fail(outcome, "Stopping SENTIENT", e);
        }
    }

    // 2–3. Copy and verify each.
    let mut failure: Option<(String, String)> = None;
    for l in live.iter_mut() {
        let name = format!("Copying {} data to {}", l.m.store.label(), l.m.to);
        step(sink, &name);
        if let Err(e) = copy_one(&cfg, &image, sink, l) {
            l.copied = true;
            failure = Some((name, e));
            break;
        }
        l.copied = true;
        if cancelled(&cfg) {
            failure = Some((name, "Cancelled.".into()));
            break;
        }
        let name = format!("Verifying {} (sizes and checksums)", l.m.store.label());
        step(sink, &name);
        if let Err(e) = verify_one(&cfg, &image, sink, l) {
            failure = Some((name, e));
            break;
        }
    }

    // 4. Switch.
    if failure.is_none() {
        for l in live.iter_mut().filter(|l| l.m.store != Store::Backups) {
            let name = format!("Pointing the {} volume at {}", l.volume, l.m.to);
            step(sink, &name);
            if let Err(e) = switch_one(&mut cfg, &image, sink, l, &root) {
                failure = Some((name, e));
                break;
            }
        }
    }

    // 5–6. Start (the databases first, counted before SENTIENT writes) and
    // check.
    let mut rows_after: Vec<Option<u64>> = Vec::new();
    if failure.is_none() && stops {
        step(sink, "Starting SENTIENT");
        let dbs: Vec<&str> = rows_of.iter().filter_map(|s| s.service()).collect();
        let started = if dbs.is_empty() { Ok(()) } else { run_stream(sink, &cfg.compose_cmd(&format!("up -d {}", dbs.join(" ")))) };
        rows_after = rows_before.iter().map(|(s, _)| wait_rows(&cfg, *s, 120)).collect();
        if let Err(e) = started.and_then(|_| start_and_wait(sink, &cfg)) {
            failure = Some(("Starting SENTIENT".into(), e));
        }
    }
    let mut rows = Vec::new();
    if failure.is_none() {
        let name = "Checking SENTIENT answers and the databases hold the same rows";
        step(sink, name);
        if !super::is_running(cfg.http_port) {
            failure = Some((name.into(), "SENTIENT stopped answering after it started.".into()));
        }
        for ((s, b), after) in rows_before.iter().zip(rows_after.iter().copied()) {
            if failure.is_some() {
                break;
            }
            match (b, after) {
                (Some(b), Some(a)) if *b == a => rows.push(format!("{} holds the same {} rows as before.", s.label(), grouped(a))),
                (Some(b), Some(a)) => {
                    failure = Some((name.into(), format!("{} held {} rows before and {} now.", s.label(), grouped(*b), grouped(a))));
                    break;
                }
                (_, None) => {
                    failure = Some((name.into(), format!("{} does not answer after the move.", s.label())));
                    break;
                }
                (None, Some(a)) => rows.push(format!("{} answers ({} rows).", s.label(), grouped(a))),
            }
        }
        if failure.is_none() && forced_failure("health") {
            failure = Some((name.into(), "SENTIENT did not answer after the move (a test).".into()));
        }
    }

    match failure {
        None => {
            let at = now_rfc3339();
            for l in &live {
                let old = l.set_aside.clone().unwrap_or_else(|| l.from.clone());
                outcome.old_copies.retain(|o| !(o.store == l.m.store && o.path == old));
                outcome.old_copies.push(OldCopy { store: l.m.store, path: old, bytes: l.bytes, at: at.clone(), partial: false });
                if l.m.store == Store::Backups {
                    outcome.backups_dir = Some(norm(&l.m.to));
                }
            }
            outcome.ended = Ended::Moved;
            outcome.locations = cfg.locations.clone();
            outcome.rows = rows;
            outcome.down_secs = if stops { t0.elapsed().as_secs() } else { 0 };
            sink(Progress::Done { message: format!("Moved {}.", live.iter().map(|l| l.m.store.label()).collect::<Vec<_>>().join(" and ")) });
            outcome
        }
        Some((at_step, why)) => {
            log(sink, format!("{at_step}: {why}"));
            // Back where it was, newest first.
            let mut broken = Vec::new();
            if live.iter().any(|l| l.switched) {
                step(sink, "Pointing the volumes back at their old places");
                for l in live.iter_mut().rev().filter(|l| l.switched || l.set_aside.is_some()) {
                    if let Err(e) = switch_back_one(&mut cfg, &image, sink, l, &before) {
                        broken.push(e);
                    }
                }
            }
            if stops {
                step(sink, "Starting SENTIENT on the old places");
                if let Err(e) = start_and_wait(sink, &cfg) {
                    broken.push(e);
                }
            }
            let at = now_rfc3339();
            for l in live.iter().filter(|l| l.copied && !l.switched) {
                outcome.old_copies.retain(|o| o.path != norm(&l.m.to));
                outcome.old_copies.push(OldCopy { store: l.m.store, path: norm(&l.m.to), bytes: l.bytes, at: at.clone(), partial: true });
            }
            outcome.locations = cfg.locations.clone();
            outcome.down_secs = if stops { t0.elapsed().as_secs() } else { 0 };
            outcome.ended = if broken.is_empty() { Ended::SwitchedBack } else { Ended::Broken };
            let reason = if broken.is_empty() { why } else { format!("{why} Putting it back failed too: {}", broken.join("; ")) };
            fail(outcome, &at_step, reason)
        }
    }
}

/// A new install with data locations: make each store's folder, and give
/// an empty one what a new named volume would get from its image (Docker
/// copies the image's files into an empty volume, not into a bind mount).
/// Run after the images are pulled, before anything starts.
pub fn prepare_locations(sink: &ProgressFn, cfg: &DeployConfig) -> Result<(), String> {
    let pg_image = if cfg.flavor.uses_timescaledb() || cfg.keep_timescale_image { PG_IMAGE_TIMESCALE } else { PG_IMAGE_STOCK };
    let stores: Vec<(Store, String, &str, String)> = [
        (Store::Pg, pg_image.to_string(), "/var/lib/postgresql"),
        (Store::Ch, CH_IMAGE.to_string(), "/var/lib/clickhouse"),
        (Store::Data, cfg.image.clone(), "/var/lib/sentient"),
    ]
    .into_iter()
    .filter(|(s, ..)| *s != Store::Ch || cfg.flavor.uses_clickhouse())
    .filter_map(|(s, img, at)| cfg.locations.get(s).map(|dir| (s, img, at, dir.clone())))
    .collect();
    if stores.is_empty() {
        return Ok(());
    }
    step(sink, "Preparing the data folders");
    let helper = helper_image(cfg);
    for (s, image, at, dir) in stores {
        if !dir.starts_with('/') {
            return Err(format!("{}'s data folder must be a full path: {dir}", s.label()));
        }
        helper_capture(cfg, &helper, &format!("mkdir -p {}", h(&dir))).map_err(|e| format!("make {dir}: {e}"))?;
        let empty = helper_capture(cfg, &helper, &format!("[ -z \"$(ls -A {} 2>/dev/null)\" ] && echo SPMEMPTY", h(&dir))).unwrap_or_default().contains("SPMEMPTY");
        if !empty {
            log(sink, format!("{}: {dir} has data already; it is used as it is.", s.label()));
            continue;
        }
        let script = format!("cp -a {at}/. /spm-to/ 2>/dev/null; chown \"$(stat -c %u:%g {at})\" /spm-to; chmod \"$(stat -c %a {at})\" /spm-to; true");
        capture(&format!(
            "docker run --rm --network none --user 0:0 --entrypoint sh -v {}:/spm-to {} -c {}",
            shell_quote(&dir),
            shell_quote(&image),
            shell_quote(&script)
        ))
        .map_err(|e| format!("prepare {dir} for {}: {e}", s.label()))?;
        log(sink, format!("{} keeps its data in {dir}", s.label()));
    }
    Ok(())
}

/// Remove a kept old copy (or a failed move's partial copy). Never a place
/// a store uses now.
pub fn remove_old(sink: &ProgressFn, cfg: &DeployConfig, path: &str, backups: Option<&str>) -> Result<Vec<OldCopy>, String> {
    let path = norm(path);
    let Some(copy) = cfg.old_copies.iter().find(|o| norm(&o.path) == path).cloned() else {
        return Err(format!("{path} is not an old copy the Platform Manager kept."));
    };
    let ov = overview(cfg, backups)?;
    if let Some(p) = ov.places.iter().find(|p| within(&p.path, &path) || within(&path, &p.path)) {
        return Err(format!("{path} holds {}'s data now; it is not removed.", p.store.label()));
    }
    if path == "/" || path.matches('/').count() < 2 {
        return Err(format!("{path} is too near the top of the disk to remove."));
    }
    step(sink, format!("Removing the old copy of {} in {path}", copy.store.label()));
    helper_capture(cfg, &helper_image(cfg), &format!("rm -rf {}", h(&path))).map_err(|e| format!("Removing {path} failed: {e}"))?;
    Ok(cfg.old_copies.iter().filter(|o| norm(&o.path) != path).cloned().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(kind_of("cifs", false), DiskKind::Network);
        assert_eq!(kind_of("nfs4", false), DiskKind::Network);
        assert_eq!(kind_of("fuse.sshfs", false), DiskKind::Network);
        assert_eq!(kind_of("vfat", false), DiskKind::Fat);
        assert_eq!(kind_of("exfat", false), DiskKind::Fat);
        assert_eq!(kind_of("ext4", false), DiskKind::Local);
        assert_eq!(kind_of("xfs", true), DiskKind::ReadOnly);
        assert_eq!(kind_of("squashfs", false), DiskKind::ReadOnly);
        assert_eq!(kind_of("tmpfs", false), DiskKind::Pseudo);
    }

    #[test]
    fn df_parse_keeps_spaces_in_mounts() {
        let t = "Filesystem Type 1B-blocks Used Available Capacity Mounted on\n/dev/sdb1 ext4 2000000000000 600000000000 1400000000000 30% /mnt/disk a\n//nas/plant cifs 8000 1 5200 1% /mnt/nas\n";
        let v = parse_df(t);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].4, "/mnt/disk a");
        assert_eq!(v[1].1, "cifs");
        assert_eq!(v[0].3, 1_400_000_000_000);
    }

    #[test]
    fn disk_blocks_name_their_fix() {
        let d = |fs: &str, kind, avail| Disk { source: "x".into(), mount: "/m".into(), fstype: fs.into(), size: 10_000_000_000, avail, kind };
        let b = disk_blocks(&d("cifs", DiskKind::Network, 9_000_000_000), Store::Ch, 1, "ClickHouse");
        assert_eq!(b[0].kind, BlockKind::Network);
        assert!(b[0].why.contains("network disk"));
        let b = disk_blocks(&d("vfat", DiskKind::Fat, 9_000_000_000), Store::Pg, 1, "PostgreSQL");
        assert_eq!(b[0].kind, BlockKind::Fat);
        assert!(b[0].fixes.iter().any(|f| f.contains("ext4")));
        let b = disk_blocks(&d("ext4", DiskKind::Local, 1_000), Store::Pg, 2_000, "PostgreSQL");
        assert_eq!(b[0].kind, BlockKind::TooSmall);
        assert!(disk_blocks(&d("ext4", DiskKind::Local, 3_000), Store::Pg, 2_000, "PostgreSQL").is_empty());
        // Backups may go to a network share.
        assert!(disk_blocks(&d("nfs", DiskKind::Network, 3_000), Store::Backups, 2_000, "Backups").is_empty());
    }

    #[test]
    fn within_and_human() {
        assert!(within("/mnt/a/b", "/mnt/a"));
        assert!(!within("/mnt/ab", "/mnt/a"));
        assert!(within("/x", "/"));
        // The shared formatter (binary units, crate::size).
        assert_eq!(human(612 << 30), "612 GB");
        assert_eq!(human((21 << 30) / 10), "2.1 GB");
        assert_eq!(grouped(2812440118), "2\u{2009}812\u{2009}440\u{2009}118");
        assert_eq!(about(42 * 60), "about 42 minutes");
    }

    #[test]
    fn compose_binds_a_moved_volume_and_keeps_its_name() {
        let mut cfg = DeployConfig::default();
        cfg.locations.ch = Some("/mnt/disk-b/sentient/clickhouse".into());
        let y = super::super::compose(&cfg);
        assert!(y.contains("  clickhouse_data:\n    name: st-ch-data\n    driver: local\n    driver_opts:\n      type: none\n      o: bind\n      device: \"/mnt/disk-b/sentient/clickhouse\"\n"), "{y}");
        assert!(y.contains("  postgres_data:\n    name: st-pg-data\n  sentient_data:"), "PostgreSQL stays a plain volume: {y}");
        let j = serde_json::to_string(&cfg).unwrap();
        let back: DeployConfig = serde_json::from_str(&j).unwrap();
        assert_eq!(back.locations, cfg.locations);
        // A config without them still loads and writes no driver_opts.
        let plain = serde_json::to_string(&DeployConfig::default()).unwrap();
        assert!(!plain.contains("locations"));
    }
}

/// The move on scratch stacks of their own (names `spmfmv<n>`, ports
/// 3xxxx, volumes `spmfmv<n>-*`), with real folders under
/// `SPM_MOVE_TEST_DIR` (default: spm-move-test in the temp folder). Needs Docker,
/// the postgres, clickhouse and spmtest/sentient:fake images:
/// `cargo test -p sentient-installer-core move_e2e -- --ignored --test-threads=1`.
#[cfg(all(test, not(windows)))]
mod move_e2e {
    use super::*;
    use crate::stack::Flavor;
    use std::process::Command;

    fn sh(cmd: &str) -> (bool, String) {
        let o = Command::new("sh").args(["-c", cmd]).output().unwrap();
        (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
    }

    fn base() -> String {
        std::env::var("SPM_MOVE_TEST_DIR").unwrap_or_else(|_| std::env::temp_dir().join("spm-move-test").to_string_lossy().into_owned())
    }

    fn sink() -> ProgressFn {
        std::sync::Arc::new(|p| eprintln!("  {p:?}"))
    }

    struct Stack {
        cfg: DeployConfig,
        dir: String,
    }

    impl Stack {
        fn up(n: u16, flavor: Flavor) -> Stack {
            let xdg = std::env::temp_dir().join(format!("spm-move-xdg-{}", std::process::id()));
            std::fs::create_dir_all(&xdg).unwrap();
            // SAFETY: one test at a time (--test-threads=1).
            unsafe { std::env::set_var("XDG_DATA_HOME", &xdg) };
            let cfg = DeployConfig {
                stack_name: format!("spmfmv{n}"),
                image: "spmtest/sentient:fake".into(),
                flavor,
                pg_port: 30400 + n,
                ch_http_port: 30500 + n,
                ch_native_port: 30600 + n,
                http_port: 30700 + n,
                mqtt_port: 30800 + n,
                mqtts_port: 30900 + n,
                coap_port: 31000 + n,
                coaps_port: 31100 + n,
                ..DeployConfig::default()
            };
            assert!(!cfg.stack_dir().starts_with("/opt/sentient"), "never the live stack's folder");
            let dir = format!("{}/t{n}", base());
            let s = Stack { cfg, dir };
            s.down();
            std::fs::create_dir_all(&s.dir).unwrap();
            write_stack_files_exact(&sink(), &s.cfg).unwrap();
            let (ok, out) = sh(&s.cfg.compose_cmd("up -d"));
            assert!(ok, "{out}");
            // Rows to count before and after.
            let n_ = s.cfg.names();
            for _ in 0..90 {
                if sh(&format!("docker exec {} psql -U sentient -d sentient -c \"CREATE TABLE IF NOT EXISTS marker(v int); INSERT INTO marker SELECT generate_series(1,1000)\"", n_.pg)).0 {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
            if flavor.uses_clickhouse() {
                for _ in 0..90 {
                    if sh(&format!("docker exec {} clickhouse-client --user sentient --password sentient -q \"CREATE TABLE IF NOT EXISTS sentient.marker (v UInt64) ENGINE = MergeTree ORDER BY v\"", n_.ch)).0 {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
                let (ok, out) = sh(&format!("docker exec {} clickhouse-client --user sentient --password sentient -q \"INSERT INTO sentient.marker SELECT number FROM numbers(5000)\"", n_.ch));
                assert!(ok, "{out}");
            }
            s
        }

        fn pg_rows(&self) -> String {
            sh(&format!("docker exec {} psql -U sentient -d sentient -tAc 'SELECT count(*), sum(v) FROM marker'", self.cfg.names().pg)).1.trim().to_string()
        }

        fn ch_rows(&self) -> String {
            sh(&format!("docker exec {} clickhouse-client --user sentient --password sentient -q 'SELECT count(), sum(v) FROM sentient.marker'", self.cfg.names().ch)).1.trim().to_string()
        }

        fn device(&self, vol: &str) -> Option<String> {
            volume_info(vol).and_then(|i| i.device)
        }

        fn exists(&self, path: &str) -> bool {
            helper_capture(&self.cfg, &helper_image(&self.cfg), &format!("test -e {} && echo yes || echo no", h(path))).unwrap_or_default().trim() == "yes"
        }

        fn rm_host(&self, path: &str) {
            let _ = helper_capture(&self.cfg, &helper_image(&self.cfg), &format!("rm -rf {}", h(path)));
        }

        fn down(&self) {
            let n = self.cfg.names();
            let _ = sh(&format!("{} 2>/dev/null", self.cfg.compose_cmd("down")));
            let _ = sh(&format!("docker rm -f {} {} {} 2>/dev/null", n.app, n.pg, n.ch));
            let _ = sh(&format!("docker volume rm {} {} {} 2>/dev/null", n.vol_pg, n.vol_ch, n.vol_data));
            // The folders (root's, after a copy) and anything set aside.
            if std::path::Path::new(&self.dir).exists() {
                self.rm_host(&self.dir);
            }
            let root = docker_root();
            for v in [n.vol_pg, n.vol_ch, n.vol_data] {
                let _ = helper_capture(&self.cfg, &helper_image(&self.cfg), &format!("rm -rf /host{root}/spm-moved/{v}-*"));
            }
        }

        fn places(&self) -> Vec<Place> {
            overview(&self.cfg, None).unwrap().places
        }

        fn saved(&mut self, out: &Outcome) {
            self.cfg.locations = out.locations.clone();
            self.cfg.old_copies = out.old_copies.clone();
        }
    }

    #[test]
    #[ignore]
    fn moves_postgres_only_then_removes_the_old_copy() {
        let mut s = Stack::up(1, Flavor::Clickhouse);
        let (pg0, ch0) = (s.pg_rows(), s.ch_rows());
        assert_eq!(pg0, "1000|500500");
        let to = format!("{}/pg", s.dir);
        let est = estimate(&s.cfg, &[Move { store: Store::Pg, to: to.clone() }], &s.places());
        assert!(est.down_secs > 0 && est.rate_mb_s > 0. && est.rate_mb_s != 150., "measured, not the fallback: {est:?}");
        let out = run(sink(), &s.cfg, &[Move { store: Store::Pg, to: to.clone() }], None);
        assert_eq!(out.ended, Ended::Moved, "{out:?}");
        assert!(out.rows.iter().any(|r| r.starts_with("PostgreSQL holds the same")), "{:?}", out.rows);
        s.saved(&out);
        let n = s.cfg.names();
        assert_eq!(s.device(&n.vol_pg).as_deref(), Some(to.as_str()), "the volume keeps its name, bound to the new folder");
        assert_eq!(s.device(&n.vol_ch), None, "ClickHouse stays where it was");
        assert_eq!(s.pg_rows(), pg0);
        assert_eq!(s.ch_rows(), ch0);
        assert!(is_running_port(s.cfg.http_port));
        let old = out.old_copies.iter().find(|o| o.store == Store::Pg).unwrap().path.clone();
        assert!(old.contains("/spm-moved/") && s.exists(&old), "the old data is kept: {old}");
        // The overview says where it is now.
        let p = s.places().into_iter().find(|p| p.store == Store::Pg).unwrap();
        assert!(p.bind && p.path == to && p.bytes.unwrap_or(0) > 1_000_000);
        // Remove old copy.
        let left = remove_old(&sink(), &s.cfg, &old, None).unwrap();
        assert!(left.is_empty() && !s.exists(&old));
        // Never the place in use.
        s.cfg.old_copies.push(OldCopy { store: Store::Pg, path: to.clone(), bytes: 0, at: String::new(), partial: false });
        assert!(remove_old(&sink(), &s.cfg, &to, None).is_err());
        assert!(s.exists(&format!("{to}/data/PG_VERSION")));
        s.down();
    }

    #[test]
    #[ignore]
    fn moves_clickhouse_only_and_again_from_a_folder() {
        let mut s = Stack::up(2, Flavor::Clickhouse);
        let (pg0, ch0) = (s.pg_rows(), s.ch_rows());
        assert_eq!(ch0, "5000\t12497500");
        let a = format!("{}/ch-a", s.dir);
        let out = run(sink(), &s.cfg, &[Move { store: Store::Ch, to: a.clone() }], None);
        assert_eq!(out.ended, Ended::Moved, "{out:?}");
        assert!(out.rows.iter().any(|r| r.starts_with("ClickHouse holds the same")), "{:?}", out.rows);
        s.saved(&out);
        let n = s.cfg.names();
        assert_eq!(s.device(&n.vol_ch).as_deref(), Some(a.as_str()));
        assert_eq!(s.device(&n.vol_pg), None);
        assert_eq!((s.pg_rows(), s.ch_rows()), (pg0.clone(), ch0.clone()));
        // From a folder to another: the old folder is the old copy.
        let b = format!("{}/ch-b", s.dir);
        let out = run(sink(), &s.cfg, &[Move { store: Store::Ch, to: b.clone() }], None);
        assert_eq!(out.ended, Ended::Moved, "{out:?}");
        s.saved(&out);
        assert_eq!(s.device(&n.vol_ch).as_deref(), Some(b.as_str()));
        assert!(out.old_copies.iter().any(|o| o.path == a));
        assert_eq!(s.ch_rows(), ch0);
        let compose = std::fs::read_to_string(s.cfg.compose_path()).unwrap();
        assert!(compose.contains(&format!("device: \"{b}\"")) && compose.contains(&format!("name: {}", n.vol_ch)));
        remove_old(&sink(), &s.cfg, &a, None).unwrap();
        assert!(!s.exists(&a) && s.exists(&b));
        s.down();
    }

    #[test]
    #[ignore]
    fn moves_both_in_one_stop() {
        let mut s = Stack::up(3, Flavor::Clickhouse);
        let (pg0, ch0) = (s.pg_rows(), s.ch_rows());
        let (pg, ch) = (format!("{}/pg", s.dir), format!("{}/ch", s.dir));
        let moves = [Move { store: Store::Ch, to: ch.clone() }, Move { store: Store::Pg, to: pg.clone() }];
        let est = estimate(&s.cfg, &moves, &s.places());
        assert_eq!(est.moves.len(), 2);
        let out = run(sink(), &s.cfg, &moves, None);
        assert_eq!(out.ended, Ended::Moved, "{out:?}");
        assert_eq!(out.rows.len(), 2, "{:?}", out.rows);
        s.saved(&out);
        let n = s.cfg.names();
        assert_eq!(s.device(&n.vol_ch).as_deref(), Some(ch.as_str()));
        assert_eq!(s.device(&n.vol_pg).as_deref(), Some(pg.as_str()));
        assert_eq!((s.pg_rows(), s.ch_rows()), (pg0, ch0));
        assert_eq!(out.old_copies.len(), 2);
        for o in &out.old_copies {
            remove_old(&sink(), &s.cfg, &o.path, None).unwrap();
        }
        s.down();
    }

    #[test]
    #[ignore]
    fn a_bad_copy_switches_back() {
        let mut s = Stack::up(4, Flavor::Clickhouse);
        let (pg0, ch0) = (s.pg_rows(), s.ch_rows());
        let to = format!("{}/ch", s.dir);
        // SAFETY: one test at a time.
        unsafe { std::env::set_var("SPM_MOVE_FAIL", "verify") };
        let out = run(sink(), &s.cfg, &[Move { store: Store::Ch, to: to.clone() }], None);
        unsafe { std::env::remove_var("SPM_MOVE_FAIL") };
        assert_eq!(out.ended, Ended::SwitchedBack, "{out:?}");
        let why = out.reason.clone().unwrap();
        assert!(why.contains("did not match") && why.contains("1 file differs"), "{why}");
        assert!(out.failed_step.as_deref().unwrap().starts_with("Verifying"));
        s.saved(&out);
        let n = s.cfg.names();
        assert_eq!(s.device(&n.vol_ch), None, "ClickHouse runs from its old place");
        assert_eq!((s.pg_rows(), s.ch_rows()), (pg0, ch0));
        assert!(is_running_port(s.cfg.http_port));
        // The partial copy is left for a look, and can be removed.
        let partial = out.old_copies.iter().find(|o| o.partial).unwrap();
        assert_eq!(partial.path, to);
        remove_old(&sink(), &s.cfg, &to, None).unwrap();
        assert!(!s.exists(&to));
        s.down();
    }

    #[test]
    #[ignore]
    fn a_failure_after_the_switch_puts_both_back() {
        let mut s = Stack::up(5, Flavor::Clickhouse);
        let (pg0, ch0) = (s.pg_rows(), s.ch_rows());
        let moves = [Move { store: Store::Pg, to: format!("{}/pg", s.dir) }, Move { store: Store::Ch, to: format!("{}/ch", s.dir) }];
        unsafe { std::env::set_var("SPM_MOVE_FAIL", "health") };
        let out = run(sink(), &s.cfg, &moves, None);
        unsafe { std::env::remove_var("SPM_MOVE_FAIL") };
        assert_eq!(out.ended, Ended::SwitchedBack, "{out:?}");
        s.saved(&out);
        let n = s.cfg.names();
        assert_eq!(s.device(&n.vol_pg), None);
        assert_eq!(s.device(&n.vol_ch), None);
        assert!(out.locations.is_empty());
        let compose = std::fs::read_to_string(s.cfg.compose_path()).unwrap();
        assert!(!compose.contains("driver_opts"), "{compose}");
        assert_eq!((s.pg_rows(), s.ch_rows()), (pg0, ch0), "the data is back in the old volumes");
        assert!(is_running_port(s.cfg.http_port));
        s.down();
    }

    #[test]
    #[ignore]
    fn unsuitable_targets_are_blocked() {
        let s = Stack::up(6, Flavor::Stock);
        let places = s.places();
        let kinds = |path: &str, needed: u64| check(&s.cfg, Store::Pg, path, needed, &places).blocks.iter().map(|b| b.kind).collect::<Vec<_>>();
        assert_eq!(kinds("relative/dir", 1), vec![BlockKind::NotAbsolute]);
        // Not empty.
        std::fs::create_dir_all(format!("{}/full", s.dir)).unwrap();
        std::fs::write(format!("{}/full/a", s.dir), "x").unwrap();
        assert_eq!(kinds(&format!("{}/full", s.dir), 1), vec![BlockKind::NotEmpty]);
        // Too small.
        assert_eq!(kinds(&format!("{}/new", s.dir), u64::MAX / 2), vec![BlockKind::TooSmall]);
        // The store's own place, and inside another's.
        let pg = places.iter().find(|p| p.store == Store::Pg).unwrap().path.clone();
        assert_eq!(kinds(&pg, 1), vec![BlockKind::SamePlace]);
        let data = places.iter().find(|p| p.store == Store::Data).unwrap().path.clone();
        assert_eq!(kinds(&format!("{data}/pg"), 1), vec![BlockKind::Overlaps]);
        // FAT (the EFI partition, never written) and read-only (a snap).
        if sh("findmnt -no FSTYPE /boot/efi").1.trim() == "vfat" {
            assert_eq!(kinds("/boot/efi/sentient", 1), vec![BlockKind::Fat]);
        }
        if let Some(snap) = sh("findmnt -rn -t squashfs -o TARGET | head -1").1.lines().next().map(str::to_string).filter(|s| !s.is_empty()) {
            assert_eq!(kinds(&format!("{snap}/sentient"), 1), vec![BlockKind::ReadOnly]);
        }
        // A fine one.
        let ok = check(&s.cfg, Store::Pg, &format!("{}/new", s.dir), 1, &places);
        assert!(ok.ok(), "{:?}", ok.blocks);
        assert!(!ok.exists && ok.empty && ok.disk.is_some());
        // The disk list: this machine's, with none of Docker's own.
        let d = disks();
        assert!(d.iter().any(|x| x.mount == "/"));
        assert!(d.iter().all(|x| !x.mount.starts_with("/var/lib/docker")));
        s.down();
    }

    #[test]
    #[ignore]
    fn cancel_during_the_copy_puts_it_back() {
        let mut s = Stack::up(8, Flavor::Stock);
        let pg0 = s.pg_rows();
        // A big file, so the copy takes a while.
        let pg_place = s.places().into_iter().find(|p| p.store == Store::Pg).unwrap().path;
        helper_capture(&s.cfg, &helper_image(&s.cfg), &format!("dd if=/dev/urandom of={}/spm-big bs=1M count=1500 2>/dev/null", h(&pg_place))).unwrap();
        let to = format!("{}/pg", s.dir);
        let cfg = s.cfg.clone();
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let a2 = asked.clone();
        let sink: ProgressFn = std::sync::Arc::new(move |p| {
            if let Progress::Step { name } = &p {
                if name.starts_with("Copying") && !a2.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    let c = cfg.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_secs(2));
                        request_cancel(&c).unwrap();
                    });
                }
            }
            eprintln!("  {p:?}");
        });
        let out = run(sink, &s.cfg, &[Move { store: Store::Pg, to: to.clone() }], None);
        assert_eq!(out.ended, Ended::SwitchedBack, "{out:?}");
        assert!(out.reason.as_deref().unwrap_or("").contains("Cancelled"), "{out:?}");
        s.saved(&out);
        assert_eq!(s.device(&s.cfg.names().vol_pg), None);
        assert_eq!(s.pg_rows(), pg0);
        assert!(is_running_port(s.cfg.http_port));
        assert!(!std::path::Path::new(&cancel_file(&s.cfg)).exists(), "the request is cleared");
        s.down();
    }

    #[test]
    #[ignore]
    fn takeover_keeps_a_moved_volume_bound() {
        let mut s = Stack::up(7, Flavor::Stock);
        let to = format!("{}/pg", s.dir);
        let out = run(sink(), &s.cfg, &[Move { store: Store::Pg, to: to.clone() }], None);
        assert_eq!(out.ended, Ended::Moved, "{out:?}");
        // A record that missed the move (saved before it): the next write of
        // the compose file still binds the volume where it is.
        let stale = s.cfg.clone();
        super::super::write_stack_files(&sink(), &stale).unwrap();
        let compose = std::fs::read_to_string(stale.compose_path()).unwrap();
        assert!(compose.contains(&format!("device: \"{to}\"")), "{compose}");
        let (ok, o) = sh(&stale.compose_cmd("up -d"));
        assert!(ok, "{o}");
        assert_eq!(s.device(&stale.names().vol_pg).as_deref(), Some(to.as_str()));
        s.saved(&out);
        let old = out.old_copies[0].path.clone();
        remove_old(&sink(), &s.cfg, &old, None).unwrap();
        s.down();
    }

    #[test]
    #[ignore]
    fn a_new_install_puts_each_store_where_asked() {
        let s = Stack::up(0, Flavor::Stock);
        s.down();
        // A new stack of its own whose stores are bound from the start.
        let mut cfg = s.cfg.clone();
        cfg.locations = crate::distro::Locations::under(&format!("{}/one", s.dir), false);
        std::fs::create_dir_all(&s.dir).unwrap();
        write_stack_files_exact(&sink(), &cfg).unwrap();
        prepare_locations(&sink(), &cfg).unwrap();
        let (ok, out) = sh(&cfg.compose_cmd("up -d"));
        assert!(ok, "{out}");
        let n = cfg.names();
        let mut up = false;
        for _ in 0..90 {
            if sh(&format!("docker exec {} psql -U sentient -d sentient -c 'CREATE TABLE marker(v int); INSERT INTO marker VALUES (1)'", n.pg)).0 {
                up = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        assert!(up, "PostgreSQL starts in its bound folder");
        assert_eq!(s.device(&n.vol_pg).as_deref(), Some(format!("{}/one/postgres", s.dir).as_str()));
        assert_eq!(s.device(&n.vol_data).as_deref(), Some(format!("{}/one/files", s.dir).as_str()));
        assert!(s.exists(&format!("{}/one/postgres/data/PG_VERSION", s.dir)));
        assert!(is_running_port(cfg.http_port));
        let t = Stack { cfg, dir: s.dir.clone() };
        t.down();
    }

    fn is_running_port(p: u16) -> bool {
        (0..30).any(|_| {
            let ok = super::super::is_running(p);
            if !ok {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
            ok
        })
    }
}
