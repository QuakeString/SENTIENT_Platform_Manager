// Copyright © 2016-2026 The SENTIENT Authors

//! The Server card on Statistics (crate::manager::server_card): what the
//! machine is, its processor, memory, disks, clock, network, updates and
//! Docker, read in one call (`--job server-info-json` on a server, in this
//! process here).
//!
//! Cheap: nothing here runs on Statistics' live ticks (the live processor
//! and memory come from the sampler). The slow parts are kept on disk for
//! minutes: update counts ([`UPDATES_EVERY`]), SMART ([`SMART_EVERY`]) and
//! whether the last restart was clean (per boot).
//!
//! Linux: /proc, /sys, lsblk, df, ip, timedatectl or chronyc, smartctl and
//! nvidia-smi when present, apt, dnf or pacman, systemd-detect-virt,
//! docker. Windows: one PowerShell script (CIM, Get-PhysicalDisk, w32tm,
//! Get-NetAdapter, the pending-restart keys). Every parser is a plain
//! function with tests on recorded sample outputs.

// The agent (no window) reads; the window draws with the rest.
#![cfg_attr(not(feature = "gui"), allow(dead_code))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::installation::{Engine, InstallationRecord};

/// Update counts are read again this often (they ask the package manager).
pub const UPDATES_EVERY: Duration = Duration::from_secs(30 * 60);
/// SMART is read again this often (it wakes every disk).
pub const SMART_EVERY: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ServerInfo {
    /// "linux" or "windows".
    pub system: String,
    /// The machine's own name.
    pub host: String,
    #[serde(default)]
    pub machine: Machine,
    /// "Ubuntu 22.04", "Windows 10 Pro 22H2".
    pub os: String,
    /// "6.8" (Linux), the build (Windows).
    #[serde(default)]
    pub kernel: Option<String>,
    #[serde(default)]
    pub cpu: Cpu,
    pub mem_total: u64,
    #[serde(default)]
    pub swap_total: u64,
    #[serde(default)]
    pub swap_used: u64,
    /// Graphics cards (a virtual machine's basic display chip left out).
    #[serde(default)]
    pub gpus: Vec<Gpu>,
    #[serde(default)]
    pub disks: Vec<Disk>,
    /// When it started (Unix seconds).
    pub boot: i64,
    /// The restart before this one was clean (None: not known).
    #[serde(default)]
    pub clean_restart: Option<bool>,
    #[serde(default)]
    pub clock: Clock,
    #[serde(default)]
    pub nets: Vec<Net>,
    /// Updates waiting (None: not known).
    #[serde(default)]
    pub updates: Option<u32>,
    #[serde(default)]
    pub restart_needed: bool,
    /// None: Docker is not installed (a native install: "Not used").
    #[serde(default)]
    pub docker: Option<Docker>,
    /// When this was read (Unix seconds).
    #[serde(default)]
    pub at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Machine {
    #[serde(rename = "virtual")]
    pub is_virtual: bool,
    /// "KVM", "VMware", "Hyper-V".
    #[serde(default)]
    pub hypervisor: Option<String>,
    /// The maker's model name, when it says one.
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Cpu {
    pub model: String,
    pub cores: u32,
    #[serde(default)]
    pub threads: u32,
    /// Where the machine reports it.
    #[serde(default)]
    pub temp_c: Option<f32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Gpu {
    pub name: String,
    #[serde(default)]
    pub mem: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Disk {
    /// The mount point ("/mnt/data"), the drive ("C:"), or the device of a
    /// disk not mounted ("sdc").
    pub name: String,
    /// The physical disk ("sdb", "nvme0n1", "Disk 1").
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub model: Option<String>,
    /// "SSD", "HDD", "USB".
    #[serde(default)]
    pub kind: Option<String>,
    pub size: u64,
    /// None: not mounted.
    #[serde(default)]
    pub free: Option<u64>,
    /// SENTIENT's data is on it.
    #[serde(default)]
    pub sentient: bool,
    /// SMART's overall health (None: not read).
    #[serde(default)]
    pub smart_ok: Option<bool>,
    /// An SSD's wear, 0–100 %.
    #[serde(default)]
    pub wear: Option<u8>,
}

impl Disk {
    pub fn mounted(&self) -> bool {
        self.free.is_some()
    }

    pub fn used_fraction(&self) -> f32 {
        match self.free {
            Some(f) if self.size > 0 => (self.size.saturating_sub(f)) as f32 / self.size as f32,
            _ => 0.,
        }
    }

    /// Less than a tenth or 5 GB free.
    pub fn nearly_full(&self) -> bool {
        match self.free {
            Some(f) if self.size > 0 => (f as f64) < self.size as f64 * 0.10 || f < 5 * (1u64 << 30),
            _ => false,
        }
    }

    /// SSD wear from this much is worth a look.
    pub fn worn(&self) -> bool {
        self.wear.is_some_and(|w| w >= 80)
    }

    /// All details' health pill: a SMART warning first, then wear from
    /// 80 %, then healthy; nothing when SMART was not read.
    pub fn health(&self) -> Health {
        if self.smart_ok == Some(false) {
            Health::Warning
        } else if self.worn() {
            Health::Worn(self.wear.unwrap_or(0))
        } else if self.smart_ok == Some(true) {
            Health::Healthy
        } else {
            Health::NotKnown
        }
    }
}

/// A disk's health, as its pill says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// Green "Healthy".
    Healthy,
    /// Amber "82 % worn".
    Worn(u8),
    /// Red "Health warning".
    Warning,
    /// No pill.
    NotKnown,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Clock {
    /// Synchronised with a time server (None: not known).
    #[serde(default)]
    pub synced: Option<bool>,
    /// Seconds off the time server.
    #[serde(default)]
    pub offset: Option<f64>,
    #[serde(default)]
    pub zone: Option<String>,
}

impl Clock {
    /// Not synced, or more than a second off.
    pub fn out_of_sync(&self) -> bool {
        self.synced == Some(false) || self.offset.is_some_and(|o| o.abs() > 1.)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Net {
    pub name: String,
    #[serde(default)]
    pub ip: Option<String>,
    /// Mbit/s.
    #[serde(default)]
    pub speed: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Docker {
    pub version: String,
    /// It starts when the machine starts (None: not known).
    #[serde(default)]
    pub on_boot: Option<bool>,
}

impl ServerInfo {
    pub fn windows(&self) -> bool {
        self.system == "windows"
    }

    /// The disk SENTIENT's data is on.
    pub fn data_disk(&self) -> Option<&Disk> {
        self.disks.iter().find(|d| d.sentient)
    }

    /// Swap in heavy use: over half of it and over 512 MB.
    pub fn heavy_swap(&self) -> bool {
        self.swap_total > 0 && self.swap_used * 2 > self.swap_total && self.swap_used > 512 << 20
    }

    /// What needs a look, one line each (the card's amber Attention lines).
    pub fn attention(&self) -> Vec<String> {
        let mut v = Vec::new();
        for d in &self.disks {
            let what = if d.sentient { "Data disk".to_string() } else { d.name.clone() };
            if d.smart_ok == Some(false) {
                v.push(format!("{what} reports a health warning"));
            } else if d.worn() {
                v.push(format!("{what} {} % worn", d.wear.unwrap_or(0)));
            }
            if d.nearly_full() {
                v.push(format!("{what} nearly full"));
            }
        }
        if self.clock.synced == Some(false) {
            v.push("Clock not synchronised".into());
        } else if let Some(o) = self.clock.offset.filter(|o| o.abs() > 1.) {
            v.push(format!("Clock {} off", secs_label(o.abs())));
        }
        if self.heavy_swap() {
            v.push("Heavy swap use: memory is short".into());
        }
        v
    }

    /// The card's Attention lines: [`attention`](Self::attention) without
    /// the nearly full disks, which the card shows in their own amber rows
    /// (the data disk's bar and free space turn amber; any other in a
    /// "Nearly full" row), so each is said once.
    pub fn card_attention(&self) -> Vec<String> {
        self.attention().into_iter().filter(|a| !a.ends_with("nearly full")).collect()
    }

    /// The nearly full disks besides the data disk (the card's "Nearly
    /// full" rows).
    pub fn nearly_full_others(&self) -> Vec<&Disk> {
        self.disks.iter().filter(|d| !d.sentient && d.nearly_full()).collect()
    }
}

/// "0.02 s", "3 s", "4 min".
pub fn secs_label(s: f64) -> String {
    if s < 10. {
        let t = format!("{s:.2}");
        let t = t.trim_end_matches('0').trim_end_matches('.');
        format!("{} s", if t.is_empty() { "0" } else { t })
    } else if s < 120. {
        format!("{s:.0} s")
    } else {
        format!("{:.0} min", s / 60.)
    }
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Read this machine (the agent's `server-info-json`, and Statistics on
/// this computer).
pub fn gather(record: Option<&InstallationRecord>) -> ServerInfo {
    #[cfg(debug_assertions)]
    if let Some(i) = debug_info() {
        return i;
    }
    let mut cache = Cache::load();
    #[cfg(windows)]
    let mut info = windows::gather(record, &mut cache);
    #[cfg(not(windows))]
    let mut info = linux::gather(record, &mut cache);
    info.at = chrono::Utc::now().timestamp();
    cache.save();
    info
}

/// Where SENTIENT keeps its data here (the record's database folder, else
/// its install folder).
fn data_path(record: Option<&InstallationRecord>) -> Option<PathBuf> {
    let r = record?;
    Some(if r.pg_dir.as_os_str().is_empty() { r.install_dir.clone() } else { r.pg_dir.clone() })
}

/// Screens and the layout test: SPM_DEBUG_SERVER_INFO names a canned
/// reading (linux-gpu, vm, windows) or a JSON file.
#[cfg(debug_assertions)]
pub fn debug_info() -> Option<ServerInfo> {
    let v = std::env::var("SPM_DEBUG_SERVER_INFO").ok()?;
    let text = fixture(&v).map(str::to_string).or_else(|| std::fs::read_to_string(&v).ok())?;
    let mut i: ServerInfo = serde_json::from_str(&text).ok()?;
    // Canned readings started "41 days ago" whenever they are drawn.
    let now = chrono::Utc::now().timestamp();
    i.boot = now - (i.boot.max(0));
    i.at = now;
    Some(i)
}

/// The canned readings (fictional machines).
pub fn fixture(name: &str) -> Option<&'static str> {
    match name {
        "linux-gpu" => Some(include_str!("server_info_fixtures/linux-gpu.json")),
        "vm" => Some(include_str!("server_info_fixtures/vm.json")),
        "windows" => Some(include_str!("server_info_fixtures/windows.json")),
        _ => None,
    }
}

/// Updates were installed: the next reading asks the package manager again.
pub fn forget_updates() {
    let mut c = Cache::load();
    c.updates = None;
    c.save();
}

/// What is kept between calls (the agent is a new process each time).
#[derive(Debug, Default, Serialize, Deserialize)]
struct Cache {
    #[serde(default)]
    updates: Option<(i64, Option<u32>, bool)>,
    #[serde(default)]
    smart: Option<(i64, HashMap<String, (Option<bool>, Option<u8>)>)>,
    /// (boot, clean) of this boot.
    #[serde(default)]
    clean: Option<(i64, Option<bool>)>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl Cache {
    fn file() -> PathBuf {
        crate::stats::history_path().with_file_name("server-info-cache.json")
    }

    fn load() -> Self {
        let path = Self::file();
        let mut c: Cache = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        c.path = Some(path);
        c
    }

    fn save(&self) {
        if let Some(p) = &self.path {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(p, serde_json::to_string(self).unwrap_or_default());
        }
    }

    fn fresh(at: i64, every: Duration) -> bool {
        let now = chrono::Utc::now().timestamp();
        now >= at && now - at < every.as_secs() as i64
    }
}

// ---------------------------------------------------------------------------
// Parsers (shared and Linux)
// ---------------------------------------------------------------------------

/// PRETTY_NAME without a point release's extras: "Ubuntu 22.04.4 LTS" →
/// "Ubuntu 22.04"; else NAME and VERSION_ID.
pub fn parse_os_release(text: &str) -> Option<String> {
    let get = |k: &str| text.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).map(|v| v.trim().trim_matches('"').to_string()).filter(|v| !v.is_empty());
    let (name, ver) = (get("NAME"), get("VERSION_ID"));
    match (name, ver) {
        (Some(n), Some(v)) => {
            let n = n.trim_end_matches(" GNU/Linux").trim_end_matches(" Linux").to_string();
            let n = if n.is_empty() { "Linux".into() } else { n };
            Some(format!("{n} {v}"))
        }
        _ => get("PRETTY_NAME"),
    }
}

/// "6.8.0-45-generic" → "6.8".
pub fn short_kernel(release: &str) -> String {
    let mut it = release.trim().split(['.', '-']);
    match (it.next(), it.next()) {
        (Some(a), Some(b)) if b.chars().all(|c| c.is_ascii_digit()) => format!("{a}.{b}"),
        _ => release.trim().to_string(),
    }
}

/// systemd-detect-virt's word → (virtual, the hypervisor's name).
pub fn parse_virt(word: &str) -> (bool, Option<String>) {
    let w = word.trim();
    let name = match w {
        "" | "none" => return (false, None),
        "kvm" => "KVM",
        "qemu" => "QEMU",
        "vmware" => "VMware",
        "microsoft" => "Hyper-V",
        "oracle" => "VirtualBox",
        "xen" => "Xen",
        "amazon" => "AWS Nitro",
        "google" => "Google Cloud",
        "parallels" => "Parallels",
        "bhyve" => "bhyve",
        "wsl" => "WSL",
        "docker" | "podman" | "lxc" | "lxc-libvirt" | "openvz" | "systemd-nspawn" => return (true, Some(format!("{w} container"))),
        other => other,
    };
    (true, Some(name.to_string()))
}

/// /proc/cpuinfo → (model, cores, threads).
pub fn parse_cpuinfo(text: &str) -> (String, u32, u32) {
    let mut model = String::new();
    let mut threads = 0u32;
    let mut cores = std::collections::HashSet::new();
    let (mut phys, mut core) = (String::new(), String::new());
    for l in text.lines() {
        let Some((k, v)) = l.split_once(':') else {
            if !core.is_empty() {
                cores.insert((phys.clone(), core.clone()));
            }
            phys.clear();
            core.clear();
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "processor" => threads += 1,
            "model name" | "Model" | "cpu model" if model.is_empty() => model = v.to_string(),
            "physical id" => phys = v.to_string(),
            "core id" => core = v.to_string(),
            _ => {}
        }
    }
    if !core.is_empty() {
        cores.insert((phys, core));
    }
    let model = clean_cpu(&model);
    let n = if cores.is_empty() { threads } else { cores.len() as u32 };
    (model, n.max(1), threads.max(1))
}

/// "Intel(R) Core(TM) i7-9700 CPU @ 3.00GHz" → "Intel Core i7-9700";
/// "AMD EPYC 7551 32-Core Processor" → "AMD EPYC 7551".
pub fn clean_cpu(model: &str) -> String {
    let mut s = model.replace("(R)", "").replace("(TM)", "").replace("(tm)", "").replace("(r)", "");
    if let Some(i) = s.find(" @ ") {
        s.truncate(i);
    }
    let words: Vec<&str> = s.split_whitespace().filter(|w| !matches!(*w, "CPU" | "Processor") && !w.ends_with("-Core")).collect();
    words.join(" ")
}

/// /proc/meminfo → (total, swap total, swap used), bytes.
pub fn parse_meminfo(text: &str) -> (u64, u64, u64) {
    let kb = |k: &str| text.lines().find_map(|l| l.strip_prefix(k)).and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok()).unwrap_or(0) * 1024;
    let (total, st, sf) = (kb("MemTotal:"), kb("SwapTotal:"), kb("SwapFree:"));
    (total, st, st.saturating_sub(sf))
}

/// One line of `lsblk -P -b -o NAME,TYPE,SIZE,ROTA,TRAN,MOUNTPOINT,FSTYPE,PKNAME,MODEL`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Blk {
    pub name: String,
    pub typ: String,
    pub size: u64,
    pub rota: bool,
    pub tran: String,
    pub mount: String,
    pub fstype: String,
    pub parent: String,
    pub model: String,
}

pub fn parse_lsblk(text: &str) -> Vec<Blk> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let kv = pairs(l);
            let g = |k: &str| kv.get(k).cloned().unwrap_or_default();
            Blk { name: g("NAME"), typ: g("TYPE"), size: g("SIZE").parse().unwrap_or(0), rota: g("ROTA") == "1", tran: g("TRAN"), mount: g("MOUNTPOINT"), fstype: g("FSTYPE"), parent: g("PKNAME"), model: g("MODEL").trim().to_string() }
        })
        .collect()
}

/// `KEY="value" KEY2="value"` → map.
fn pairs(line: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let mut rest = line.trim();
    while let Some(eq) = rest.find("=\"") {
        let key = rest[..eq].trim().to_string();
        let after = &rest[eq + 2..];
        let end = after.find('"').unwrap_or(after.len());
        m.insert(key, after[..end].replace("\\x20", " "));
        rest = after.get(end + 1..).unwrap_or("");
    }
    m
}

/// One line of `df -B1 --output=source,target,size,avail`.
#[derive(Debug, Clone, PartialEq)]
pub struct Mount {
    pub source: String,
    pub target: String,
    pub size: u64,
    pub avail: u64,
}

pub fn parse_df(text: &str) -> Vec<Mount> {
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 4 {
                return None;
            }
            let n = f.len();
            Some(Mount { source: f[0].to_string(), target: f[1..n - 2].join(" "), size: f[n - 2].parse().ok()?, avail: f[n - 1].parse().ok()? })
        })
        .collect()
}

/// A mount point the card leaves out: the boot partitions, and anything
/// that is not a disk of its own.
fn skipped_mount(m: &Mount) -> bool {
    let t = m.target.as_str();
    !m.source.starts_with("/dev/") || m.source.starts_with("/dev/loop") || t.starts_with("/boot") || t == "/efi" || t.starts_with("/snap/") || t.starts_with("/var/lib/docker/") || t.starts_with("/run/") || m.size == 0
}

/// The disks: every mounted file system on a disk (one row per device, its
/// shortest mount point), then the disks with nothing mounted, greyed.
/// `data`: where SENTIENT keeps its data; `smart`: per physical disk.
pub fn build_disks(blk: &[Blk], df: &[Mount], data: Option<&Path>, smart: &HashMap<String, (Option<bool>, Option<u8>)>) -> Vec<Disk> {
    let by_name: HashMap<&str, &Blk> = blk.iter().map(|b| (b.name.as_str(), b)).collect();
    // The physical disk under a partition, LVM volume or RAID.
    let top = |name: &str| -> Option<&Blk> {
        let mut cur = by_name.get(name).copied()?;
        for _ in 0..8 {
            if cur.typ == "disk" || cur.parent.is_empty() {
                return Some(cur);
            }
            cur = by_name.get(cur.parent.as_str()).copied()?;
        }
        Some(cur)
    };
    let kind = |d: &Blk| -> String {
        if d.tran == "usb" {
            "USB".into()
        } else if d.rota {
            "HDD".into()
        } else {
            "SSD".into()
        }
    };
    let mut mounts: Vec<&Mount> = df.iter().filter(|m| !skipped_mount(m)).collect();
    mounts.sort_by_key(|m| m.target.len());
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<Disk> = Vec::new();
    let data_mount = data.and_then(|p| {
        let p = p.to_string_lossy();
        mounts.iter().filter(|m| p == m.target || p.starts_with(&format!("{}/", m.target.trim_end_matches('/'))) || m.target == "/").max_by_key(|m| m.target.len()).map(|m| m.source.clone())
    });
    let mut used_disks = std::collections::HashSet::new();
    for m in &mounts {
        if !seen.insert(m.source.clone()) {
            continue;
        }
        let dev = m.source.trim_start_matches("/dev/").trim_start_matches("mapper/").to_string();
        let disk = top(&dev).or_else(|| blk.iter().find(|b| m.source.ends_with(&b.name)).and_then(|b| top(&b.name)));
        if let Some(d) = disk {
            used_disks.insert(d.name.clone());
        }
        let (ok, wear) = disk.and_then(|d| smart.get(&d.name).copied()).unwrap_or((None, None));
        out.push(Disk {
            name: m.target.clone(),
            device: disk.map(|d| d.name.clone()).unwrap_or(dev),
            model: disk.map(|d| d.model.clone()).filter(|m| !m.is_empty()),
            kind: disk.map(kind),
            size: m.size,
            free: Some(m.avail),
            sentient: data_mount.as_deref() == Some(m.source.as_str()),
            smart_ok: ok,
            wear,
        });
    }
    // The root first, then by name.
    out.sort_by(|a, b| (a.name != "/", &a.name).cmp(&(b.name != "/", &b.name)));
    // Disks in use another way (swap, LVM, RAID) count as used.
    for b in blk {
        if !b.mount.is_empty() || matches!(b.fstype.as_str(), "swap" | "LVM2_member" | "linux_raid_member" | "crypto_LUKS" | "zfs_member") {
            if let Some(d) = top(&b.name) {
                used_disks.insert(d.name.clone());
            }
        }
    }
    for b in blk.iter().filter(|b| b.typ == "disk") {
        if used_disks.contains(&b.name) || b.name.starts_with("zram") || b.name.starts_with("loop") || b.name.starts_with("sr") || b.name.starts_with("ram") || b.size == 0 {
            continue;
        }
        let (ok, wear) = smart.get(&b.name).copied().unwrap_or((None, None));
        out.push(Disk { name: b.name.clone(), device: b.name.clone(), model: Some(b.model.clone()).filter(|m| !m.is_empty()), kind: Some(kind(b)), size: b.size, free: None, sentient: false, smart_ok: ok, wear });
    }
    out
}

/// `smartctl -j -H -A` → (health passed, SSD wear %).
pub fn parse_smart(json: &str) -> (Option<bool>, Option<u8>) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return (None, None) };
    let ok = v.pointer("/smart_status/passed").and_then(|p| p.as_bool());
    // NVMe: percentage used; SATA SSDs: a wear attribute's normalised value
    // counts down from 100.
    let wear = v.pointer("/nvme_smart_health_information_log/percentage_used").and_then(|p| p.as_u64()).map(|p| p.min(100) as u8).or_else(|| {
        v.pointer("/ata_smart_attributes/table")?.as_array()?.iter().find_map(|a| {
            let id = a.get("id")?.as_u64()?;
            matches!(id, 177 | 231 | 233 | 202 | 169).then(|| a.get("value")?.as_u64()).flatten().map(|n| 100u64.saturating_sub(n.min(100)) as u8)
        })
    });
    (ok, wear)
}

/// `ip -o -4 addr show` → (interface, address), the machine's own
/// interfaces only (no loopback, Docker or bridges).
pub fn parse_ip(text: &str) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = Vec::new();
    for l in text.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        let Some(i) = f.iter().position(|w| *w == "inet") else { continue };
        let name = f.get(1).map(|n| n.trim_end_matches(':').split('@').next().unwrap_or("").to_string()).unwrap_or_default();
        if virtual_if(&name) {
            continue;
        }
        let ip = f.get(i + 1).map(|a| a.split('/').next().unwrap_or("").to_string()).unwrap_or_default();
        if !v.iter().any(|(n, _)| *n == name) {
            v.push((name, ip));
        }
    }
    v
}

fn virtual_if(name: &str) -> bool {
    name == "lo" || ["docker", "br-", "veth", "virbr", "cni", "flannel", "cali", "vxlan", "kube", "tailscale", "zt"].iter().any(|p| name.starts_with(p))
}

/// `timedatectl show` → (synchronised, time zone).
pub fn parse_timedatectl(text: &str) -> (Option<bool>, Option<String>) {
    let get = |k: &str| text.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).map(|v| v.trim().to_string());
    (get("NTPSynchronized").map(|v| v == "yes"), get("Timezone").filter(|z| !z.is_empty()))
}

/// The offset from the time server, in seconds: chronyc tracking ("System
/// time : 0.000012 seconds fast of NTP time") or timedatectl
/// timesync-status ("Offset: -1.234ms").
pub fn parse_offset(text: &str) -> Option<f64> {
    for l in text.lines() {
        let l = l.trim();
        if let Some(rest) = l.strip_prefix("System time") {
            let rest = rest.trim_start_matches([' ', ':']);
            let n: f64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(if rest.contains("slow") { -n } else { n });
        }
        if let Some(rest) = l.strip_prefix("Offset:") {
            let r = rest.trim();
            let (num, unit) = r.find(|c: char| c.is_ascii_alphabetic() || c == 'µ').map(|i| (&r[..i], &r[i..])).unwrap_or((r, "s"));
            let n: f64 = num.trim().trim_start_matches('+').parse().ok()?;
            return Some(match unit {
                "ms" => n / 1000.,
                "us" | "µs" => n / 1e6,
                "ns" => n / 1e9,
                "min" => n * 60.,
                _ => n,
            });
        }
    }
    None
}

/// `apt-get -s upgrade`: one "Inst" line per package.
pub fn parse_apt(text: &str) -> u32 {
    text.lines().filter(|l| l.starts_with("Inst ")).count() as u32
}

/// `dnf -q -C check-update`: one line per package, up to "Obsoleting".
pub fn parse_dnf(text: &str) -> u32 {
    text.lines().take_while(|l| !l.starts_with("Obsoleting")).filter(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        f.len() == 3 && f[0].contains('.')
    }).count() as u32
}

/// `last -x -F reboot shutdown`, newest first: the restart before this one
/// was clean when a shutdown comes before the previous boot's entry.
pub fn parse_last(text: &str) -> Option<bool> {
    let mut entries = text.lines().filter_map(|l| {
        let w = l.split_whitespace().next()?;
        matches!(w, "reboot" | "shutdown").then_some(w)
    });
    // This boot.
    if entries.next()? != "reboot" {
        return None;
    }
    Some(entries.next()? == "shutdown")
}

/// `nvidia-smi --query-gpu=name,memory.total --format=csv,noheader,nounits`.
pub fn parse_nvidia(text: &str) -> Vec<Gpu> {
    text.lines()
        .filter_map(|l| {
            let (n, m) = l.rsplit_once(',')?;
            let n = n.trim();
            let name = if n.starts_with("NVIDIA") { n.to_string() } else { format!("NVIDIA {n}") };
            Some(Gpu { name, mem: m.trim().parse::<u64>().ok().map(|mib| mib << 20) })
        })
        .collect()
}

/// `lspci -mm -nn` display controllers → names, a virtual machine's or a
/// server board's basic display chip left out.
pub fn parse_lspci(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| {
            let q: Vec<&str> = l.split('"').collect();
            // slot "class [cccc]" "vendor [vvvv]" "device [dddd]" ...
            let (class, vendor, device) = (q.get(1)?, q.get(3)?, q.get(5)?);
            let code = |s: &str| s.rsplit_once('[').map(|(_, c)| c.trim_end_matches(']').to_string()).unwrap_or_default();
            if !(class.contains("[0300]") || class.contains("[0302]") || class.contains("[0380]")) {
                return None;
            }
            if BASIC_DISPLAY_VENDORS.contains(&code(vendor).as_str()) {
                return None;
            }
            let strip = |s: &str| s.rsplit_once(" [").map(|(a, _)| a.to_string()).unwrap_or_else(|| s.to_string());
            let v = strip(vendor).replace(" Corporation", "").replace(", Inc.", "").replace(" Inc.", "");
            let v = if v.starts_with("Advanced Micro Devices") { "AMD".to_string() } else { v };
            let d = strip(device);
            // "GA106 [RTX A2000 12GB]": the marketing name inside brackets.
            let d = d.rsplit_once('[').map(|(_, n)| n.trim_end_matches(']').to_string()).unwrap_or(d);
            Some(format!("{v} {d}"))
        })
        .collect()
}

/// PCI vendors whose display chips are a basic one: QEMU/Bochs, virtio,
/// VMware, Hyper-V, VirtualBox, Cirrus, QXL, ASPEED and Matrox (server
/// boards' remote console chips).
const BASIC_DISPLAY_VENDORS: &[&str] = &["1234", "1af4", "15ad", "1414", "80ee", "1013", "1b36", "1a03", "102b"];

/// Windows display adapters that are not a graphics card.
pub fn basic_windows_adapter(name: &str) -> bool {
    let n = name.to_lowercase();
    ["basic display", "hyper-v video", "vmware svga", "virtualbox graphics", "qxl", "remote display", "remote desktop", "aspeed", "matrox g200", "parsec virtual", "citrix"].iter().any(|w| n.contains(w))
}

// ---------------------------------------------------------------------------
// Linux
// ---------------------------------------------------------------------------

#[cfg(not(windows))]
mod linux {
    use super::*;
    use std::process::{Command, Stdio};

    fn out(cmd: &str, args: &[&str]) -> Option<String> {
        let o = Command::new(cmd).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        Some(String::from_utf8_lossy(&o.stdout).to_string())
    }

    fn read(p: &str) -> String {
        std::fs::read_to_string(p).unwrap_or_default()
    }

    fn have(cmd: &str) -> bool {
        std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file())) || ["/usr/sbin", "/sbin"].iter().any(|d| Path::new(d).join(cmd).is_file())
    }

    pub fn gather(record: Option<&InstallationRecord>, cache: &mut Cache) -> ServerInfo {
        let host = read("/proc/sys/kernel/hostname").trim().to_string();
        let virt = out("systemd-detect-virt", &[]).unwrap_or_default();
        let (mut is_virtual, mut hypervisor) = parse_virt(&virt);
        let product = read("/sys/class/dmi/id/product_name").trim().to_string();
        let vendor = read("/sys/class/dmi/id/sys_vendor").trim().to_string();
        if virt.trim().is_empty() && read("/proc/cpuinfo").lines().any(|l| l.starts_with("flags") && l.contains(" hypervisor")) {
            is_virtual = true;
            hypervisor = None;
        }
        let model = (!product.is_empty() && !is_virtual).then(|| if vendor.is_empty() || product.starts_with(&vendor) { product.clone() } else { format!("{vendor} {product}") });
        let os = parse_os_release(&read("/etc/os-release")).unwrap_or_else(|| "Linux".into());
        let kernel = Some(short_kernel(&read("/proc/sys/kernel/osrelease"))).filter(|k| !k.is_empty());
        let (cpu_model, cores, threads) = parse_cpuinfo(&read("/proc/cpuinfo"));
        let (mem_total, swap_total, swap_used) = parse_meminfo(&read("/proc/meminfo"));
        let boot = read("/proc/stat").lines().find_map(|l| l.strip_prefix("btime ")).and_then(|v| v.trim().parse().ok()).unwrap_or(0);

        // Graphics: NVIDIA's own tool for its memory, else lspci.
        let mut gpus: Vec<Gpu> = if have("nvidia-smi") { parse_nvidia(&out("nvidia-smi", &["--query-gpu=name,memory.total", "--format=csv,noheader,nounits"]).unwrap_or_default()) } else { Vec::new() };
        if have("lspci") {
            for name in parse_lspci(&out("lspci", &["-mm", "-nn"]).unwrap_or_default()) {
                if name.starts_with("NVIDIA") && !gpus.is_empty() {
                    continue;
                }
                gpus.push(Gpu { name, mem: None });
            }
        }

        // Docker first: its root folder is where a Docker install keeps data.
        let docker = have("docker").then(|| out("docker", &["version", "--format", "{{.Server.Version}}"])).flatten().map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).map(|version| {
            let on_boot = out("systemctl", &["is-enabled", "docker"]).map(|s| s.trim() == "enabled");
            Docker { version, on_boot }
        });
        let data = match record {
            Some(r) if r.engine == Engine::Docker => out("docker", &["info", "--format", "{{.DockerRootDir}}"]).map(|s| PathBuf::from(s.trim())).filter(|p| !p.as_os_str().is_empty()).or_else(|| data_path(record)),
            _ => data_path(record),
        };

        // SMART, root only, kept for minutes.
        let blk = parse_lsblk(&out("lsblk", &["-P", "-b", "-o", "NAME,TYPE,SIZE,ROTA,TRAN,MOUNTPOINT,FSTYPE,PKNAME,MODEL"]).unwrap_or_default());
        let smart = match &cache.smart {
            Some((at, m)) if Cache::fresh(*at, SMART_EVERY) => m.clone(),
            _ if have("smartctl") && crate::rootjob::is_root() => {
                let m: HashMap<String, (Option<bool>, Option<u8>)> = blk.iter().filter(|b| b.typ == "disk" && !b.name.starts_with("zram") && !b.name.starts_with("loop")).map(|b| (b.name.clone(), parse_smart(&out("smartctl", &["-j", "-H", "-A", &format!("/dev/{}", b.name)]).unwrap_or_default()))).collect();
                cache.smart = Some((chrono::Utc::now().timestamp(), m.clone()));
                m
            }
            _ => HashMap::new(),
        };
        let df = parse_df(&out("df", &["-B1", "--output=source,target,size,avail"]).unwrap_or_default());
        let disks = build_disks(&blk, &df, data.as_deref(), &smart);

        // The clock.
        let (synced, zone) = parse_timedatectl(&out("timedatectl", &["show"]).unwrap_or_default());
        let offset = if have("chronyc") { out("chronyc", &["tracking"]).as_deref().and_then(parse_offset) } else { None }.or_else(|| out("timedatectl", &["timesync-status"]).as_deref().and_then(parse_offset));
        let zone = zone.or_else(|| std::fs::read_link("/etc/localtime").ok().and_then(|p| p.to_string_lossy().split("zoneinfo/").nth(1).map(str::to_string)));

        let nets = parse_ip(&out("ip", &["-o", "-4", "addr", "show"]).unwrap_or_default())
            .into_iter()
            .map(|(name, ip)| {
                let speed = read(&format!("/sys/class/net/{name}/speed")).trim().parse::<i64>().ok().filter(|s| *s > 0).map(|s| s as u64);
                Net { name, ip: Some(ip).filter(|i| !i.is_empty()), speed }
            })
            .collect();

        // Updates, kept for half an hour.
        let (updates, restart_needed) = match cache.updates {
            Some((at, n, r)) if Cache::fresh(at, UPDATES_EVERY) => (n, r || Path::new("/var/run/reboot-required").exists()),
            _ => {
                let n = if have("apt-get") {
                    out("apt-get", &["-s", "-o", "Debug::NoLocking=1", "upgrade"]).map(|t| parse_apt(&t))
                } else if have("dnf") {
                    out("dnf", &["-q", "-C", "check-update"]).map(|t| parse_dnf(&t))
                } else if have("pacman") {
                    // checkupdates (pacman-contrib) reads a copy of the sync
                    // database; pacman -Qu the last one synced.
                    if have("checkupdates") { out("checkupdates", &[]) } else { out("pacman", &["-Qu"]) }.map(|t| t.lines().filter(|l| !l.trim().is_empty()).count() as u32)
                } else {
                    None
                };
                let r = Path::new("/var/run/reboot-required").exists()
                    || (have("needs-restarting") && Command::new("needs-restarting").arg("-r").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.code() == Some(1)))
                    || (Path::new("/usr/lib/modules").exists() && !Path::new(&format!("/usr/lib/modules/{}", read("/proc/sys/kernel/osrelease").trim())).exists());
                cache.updates = Some((chrono::Utc::now().timestamp(), n, r));
                (n, r)
            }
        };

        let clean_restart = match cache.clean {
            Some((b, c)) if b == boot => c,
            _ => {
                let c = have("last").then(|| out("last", &["-x", "-F", "-n", "6", "reboot", "shutdown"])).flatten().as_deref().and_then(parse_last);
                cache.clean = Some((boot, c));
                c
            }
        };

        ServerInfo {
            system: "linux".into(),
            host,
            machine: Machine { is_virtual, hypervisor, model },
            os,
            kernel,
            cpu: Cpu { model: cpu_model, cores, threads, temp_c: cpu_temp() },
            mem_total,
            swap_total,
            swap_used,
            gpus,
            disks,
            boot,
            clean_restart,
            clock: Clock { synced, offset, zone },
            nets,
            updates,
            restart_needed,
            docker,
            at: 0,
        }
    }

    /// The processor's temperature, where a sensor reports it.
    fn cpu_temp() -> Option<f32> {
        let rd = std::fs::read_dir("/sys/class/hwmon").ok()?;
        for e in rd.flatten() {
            let name = read(&e.path().join("name").to_string_lossy());
            if matches!(name.trim(), "coretemp" | "k10temp" | "zenpower" | "cpu_thermal" | "soc_thermal") {
                if let Ok(t) = read(&e.path().join("temp1_input").to_string_lossy()).trim().parse::<f32>() {
                    return Some(t / 1000.);
                }
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

/// What the Windows script prints (one JSON object), raw.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct WinRaw {
    pub host: String,
    pub maker: String,
    pub model: String,
    pub os: String,
    pub display_version: String,
    pub build: String,
    pub cpu: String,
    pub cores: u32,
    pub threads: u32,
    pub temp_dk: Option<f64>,
    pub mem: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub boot: i64,
    pub gpus: Vec<WinGpu>,
    pub drives: Vec<WinDrive>,
    pub disks: Vec<WinDisk>,
    pub w32tm: String,
    pub zone: String,
    pub nets: Vec<WinNet>,
    pub reboot: bool,
    pub unclean: Option<bool>,
    pub docker: String,
    pub docker_start: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct WinGpu {
    pub name: String,
    pub ram: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct WinDrive {
    pub id: String,
    pub size: u64,
    pub free: u64,
    pub disk: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct WinDisk {
    pub number: String,
    pub name: String,
    pub media: String,
    pub bus: String,
    pub health: String,
    pub size: u64,
    pub wear: Option<u8>,
    pub mounted: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct WinNet {
    pub name: String,
    pub ip: Option<String>,
    pub bps: Option<u64>,
}

/// The Windows reading from the script's object; `data`: where SENTIENT
/// keeps its data ("D:\SENTIENT\data").
pub fn from_windows(raw: WinRaw, data: Option<&str>, updates: Option<u32>) -> ServerInfo {
    let (is_virtual, hypervisor) = windows_virt(&raw.maker, &raw.model);
    let model = (!is_virtual && !raw.model.trim().is_empty()).then(|| {
        let m = raw.model.trim();
        let mk = raw.maker.trim();
        if mk.is_empty() || m.starts_with(mk) || mk.eq_ignore_ascii_case("System manufacturer") { m.to_string() } else { format!("{mk} {m}") }
    });
    let os = {
        let base = raw.os.trim().trim_start_matches("Microsoft ").to_string();
        if raw.display_version.trim().is_empty() || base.contains(raw.display_version.trim()) { base } else { format!("{base} {}", raw.display_version.trim()) }
    };
    let data_drive = data.and_then(|d| d.get(..2)).map(|d| d.to_ascii_uppercase());
    let kind_of = |d: &WinDisk| -> Option<String> {
        if d.bus.eq_ignore_ascii_case("USB") {
            Some("USB".into())
        } else if d.media.eq_ignore_ascii_case("SSD") || d.bus.eq_ignore_ascii_case("NVMe") {
            Some("SSD".into())
        } else if d.media.eq_ignore_ascii_case("HDD") {
            Some("HDD".into())
        } else {
            None
        }
    };
    let health = |d: &WinDisk| -> Option<bool> {
        match d.health.as_str() {
            "Healthy" => Some(true),
            "Warning" | "Unhealthy" => Some(false),
            _ => None,
        }
    };
    let mut disks: Vec<Disk> = raw
        .drives
        .iter()
        .map(|v| {
            let pd = v.disk.as_ref().and_then(|n| raw.disks.iter().find(|d| &d.number == n));
            Disk {
                name: v.id.clone(),
                device: pd.map(|d| format!("Disk {}", d.number)).unwrap_or_default(),
                model: pd.map(|d| d.name.clone()).filter(|n| !n.is_empty()),
                kind: pd.and_then(kind_of),
                size: v.size,
                free: Some(v.free),
                sentient: data_drive.as_deref() == Some(v.id.to_ascii_uppercase().as_str()),
                smart_ok: pd.and_then(health),
                wear: pd.and_then(|d| d.wear),
            }
        })
        .collect();
    for d in raw.disks.iter().filter(|d| !d.mounted) {
        disks.push(Disk { name: format!("Disk {}", d.number), device: format!("Disk {}", d.number), model: Some(d.name.clone()).filter(|n| !n.is_empty()), kind: kind_of(d), size: d.size, free: None, sentient: false, smart_ok: health(d), wear: d.wear });
    }
    let w = &raw.w32tm;
    let source = w.lines().find_map(|l| l.trim().strip_prefix("Source:")).map(|s| s.trim().to_string());
    let synced = if w.trim().is_empty() {
        None
    } else {
        Some(!w.contains("Unspecified") && !source.as_deref().is_some_and(|s| s.starts_with("Local CMOS Clock") || s.starts_with("Free-running")))
    };
    let offset = w.lines().find_map(|l| l.trim().strip_prefix("Phase Offset:")).and_then(|s| s.trim().trim_end_matches('s').parse::<f64>().ok());
    ServerInfo {
        system: "windows".into(),
        host: raw.host,
        machine: Machine { is_virtual, hypervisor, model },
        os,
        kernel: Some(raw.build).filter(|b| !b.is_empty()).map(|b| format!("build {b}")),
        cpu: Cpu { model: clean_cpu(&raw.cpu), cores: raw.cores.max(1), threads: raw.threads.max(raw.cores), temp_c: raw.temp_dk.map(|t| (t / 10. - 273.15) as f32).filter(|t| *t > 0. && *t < 125.) },
        mem_total: raw.mem,
        swap_total: raw.swap_total,
        swap_used: raw.swap_used,
        gpus: raw.gpus.into_iter().filter(|g| !basic_windows_adapter(&g.name)).map(|g| Gpu { name: g.name.replace("(R)", "").replace("(TM)", "").split_whitespace().collect::<Vec<_>>().join(" "), mem: g.ram.filter(|r| *r > 0) }).collect(),
        disks,
        boot: raw.boot,
        clean_restart: raw.unclean.map(|u| !u),
        clock: Clock { synced, offset, zone: Some(raw.zone).filter(|z| !z.is_empty()) },
        nets: raw.nets.into_iter().filter(|n| !n.name.is_empty()).map(|n| Net { name: n.name, ip: n.ip.filter(|i| !i.is_empty()), speed: n.bps.filter(|b| *b > 0).map(|b| b / 1_000_000) }).collect(),
        updates,
        restart_needed: raw.reboot,
        docker: Some(raw.docker.trim().to_string()).filter(|v| !v.is_empty()).map(|version| Docker { version, on_boot: match raw.docker_start.trim() { "" => None, s => Some(s.eq_ignore_ascii_case("Automatic") || s.eq_ignore_ascii_case("auto")) } }),
        at: 0,
    }
}

/// Win32_ComputerSystem's maker and model → (virtual, the hypervisor).
pub fn windows_virt(maker: &str, model: &str) -> (bool, Option<String>) {
    let (mk, m) = (maker.to_lowercase(), model.to_lowercase());
    let h = if m.contains("virtual machine") && mk.contains("microsoft") {
        "Hyper-V"
    } else if m.contains("vmware") || mk.contains("vmware") {
        "VMware"
    } else if m.contains("virtualbox") || mk.contains("innotek") {
        "VirtualBox"
    } else if mk.contains("qemu") || m.contains("kvm") || m.starts_with("standard pc") {
        "KVM"
    } else if mk.contains("xen") || m.contains("hvm domu") {
        "Xen"
    } else if mk.contains("amazon ec2") {
        "AWS Nitro"
    } else if mk.contains("google") {
        "Google Cloud"
    } else if mk.contains("parallels") {
        "Parallels"
    } else {
        return (false, None);
    };
    (true, Some(h.to_string()))
}

/// The script the agent runs on Windows: one JSON object (WinRaw).
pub const WINDOWS_SCRIPT: &str = r#"$ErrorActionPreference = 'SilentlyContinue'
$cs = Get-CimInstance Win32_ComputerSystem
$os = Get-CimInstance Win32_OperatingSystem
$cpu = @(Get-CimInstance Win32_Processor)
$cv = Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion'
$vram = @{}
Get-ItemProperty 'HKLM:\SYSTEM\ControlSet001\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}\0*' | ForEach-Object { if ($_.DriverDesc -and $_.'HardwareInformation.qwMemorySize') { $vram[$_.DriverDesc] = [int64]$_.'HardwareInformation.qwMemorySize' } }
$gpus = @(Get-CimInstance Win32_VideoController | ForEach-Object { $r = $vram[$_.Name]; if (-not $r) { $r = [int64]$_.AdapterRAM }; @{ name = [string]$_.Name; ram = $r } })
$parts = @{}
Get-Partition | Where-Object DriveLetter | ForEach-Object { $parts[[string]$_.DriveLetter + ':'] = [string]$_.DiskNumber }
$drives = @(Get-CimInstance Win32_LogicalDisk -Filter 'DriveType=3 OR DriveType=2' | Where-Object { $_.Size -gt 0 } | ForEach-Object { @{ id = [string]$_.DeviceID; size = [int64]$_.Size; free = [int64]$_.FreeSpace; disk = $parts[[string]$_.DeviceID] } })
$used = @($parts.Values)
$disks = @(Get-PhysicalDisk | ForEach-Object { $w = ($_ | Get-StorageReliabilityCounter).Wear; @{ number = [string]$_.DeviceId; name = [string]$_.FriendlyName; media = [string]$_.MediaType; bus = [string]$_.BusType; health = [string]$_.HealthStatus; size = [int64]$_.Size; wear = $w; mounted = ($used -contains [string]$_.DeviceId) } })
$nets = @(Get-NetAdapter -Physical | Where-Object Status -eq 'Up' | ForEach-Object { $ip = (Get-NetIPAddress -InterfaceIndex $_.ifIndex -AddressFamily IPv4 | Select-Object -First 1).IPAddress; @{ name = [string]$_.Name; ip = [string]$ip; bps = [int64]$_.ReceiveLinkSpeed } })
$reboot = (Test-Path 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\RebootRequired') -or (Test-Path 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Component Based Servicing\RebootPending')
$boot = $os.LastBootUpTime
$unclean = [bool](Get-WinEvent -FilterHashtable @{ LogName = 'System'; Id = 6008; StartTime = $boot.AddMinutes(-10) } -MaxEvents 1)
$temp = (Get-CimInstance -Namespace root/wmi MSAcpi_ThermalZoneTemperature | Select-Object -First 1).CurrentTemperature
$dv = ''
if (Get-Command docker -ErrorAction SilentlyContinue) { $dv = [string](docker version --format '{{.Server.Version}}' 2>$null) }
$ds = (Get-Service com.docker.service, docker | Select-Object -First 1).StartType
@{
  host = $env:COMPUTERNAME; maker = [string]$cs.Manufacturer; model = [string]$cs.Model
  os = [string]$os.Caption; display_version = [string]$cv.DisplayVersion; build = [string]$os.BuildNumber
  cpu = [string]$cpu[0].Name; cores = [int](($cpu | Measure-Object NumberOfCores -Sum).Sum); threads = [int](($cpu | Measure-Object NumberOfLogicalProcessors -Sum).Sum)
  temp_dk = $temp
  mem = [int64]$cs.TotalPhysicalMemory
  swap_total = [int64]$os.SizeStoredInPagingFiles * 1024; swap_used = ([int64]$os.SizeStoredInPagingFiles - [int64]$os.FreeSpaceInPagingFiles) * 1024
  boot = [int64](($boot.ToUniversalTime() - [datetime]'1970-01-01').TotalSeconds)
  gpus = $gpus; drives = $drives; disks = $disks
  w32tm = ((w32tm /query /status /verbose) -join "`n"); zone = [string](Get-TimeZone).Id
  nets = $nets; reboot = [bool]$reboot; unclean = $unclean
  docker = $dv.Trim(); docker_start = [string]$ds
} | ConvertTo-Json -Depth 4 -Compress
"#;

/// Windows Update's count of updates waiting (slow: kept half an hour).
pub const WINDOWS_UPDATES_SCRIPT: &str = "$ErrorActionPreference = 'Stop'; $s = New-Object -ComObject Microsoft.Update.Session; $r = $s.CreateUpdateSearcher().Search(\"IsInstalled=0 and IsHidden=0 and Type='Software'\"); $r.Updates.Count";

#[cfg(windows)]
mod windows {
    use super::*;

    pub fn ps(script: &str) -> Option<String> {
        use std::os::windows::process::CommandExt;
        let o = std::process::Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script]).creation_flags(0x0800_0000).output().ok()?;
        Some(String::from_utf8_lossy(&o.stdout).to_string())
    }

    pub fn gather(record: Option<&InstallationRecord>, cache: &mut Cache) -> ServerInfo {
        let raw: WinRaw = ps(WINDOWS_SCRIPT).and_then(|t| t.lines().rev().find(|l| l.trim_start().starts_with('{')).and_then(|l| serde_json::from_str(l).ok())).unwrap_or_default();
        let updates = match cache.updates {
            Some((at, n, _)) if Cache::fresh(at, UPDATES_EVERY) => n,
            _ => {
                let n = ps(WINDOWS_UPDATES_SCRIPT).and_then(|t| t.trim().lines().last().and_then(|l| l.trim().parse().ok()));
                cache.updates = Some((chrono::Utc::now().timestamp(), n, false));
                n
            }
        };
        let data = data_path(record).map(|p| p.to_string_lossy().to_string());
        from_windows(raw, data.as_deref(), updates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_release_and_kernel() {
        let t = "PRETTY_NAME=\"Ubuntu 22.04.4 LTS\"\nNAME=\"Ubuntu\"\nVERSION_ID=\"22.04\"\nID=ubuntu\n";
        assert_eq!(parse_os_release(t).as_deref(), Some("Ubuntu 22.04"));
        assert_eq!(parse_os_release("NAME=\"Debian GNU/Linux\"\nVERSION_ID=\"12\"\n").as_deref(), Some("Debian 12"));
        assert_eq!(parse_os_release("NAME=\"Arch Linux\"\nPRETTY_NAME=\"Arch Linux\"\n").as_deref(), Some("Arch Linux"));
        assert_eq!(short_kernel("6.8.0-45-generic\n"), "6.8");
        assert_eq!(short_kernel("6.17.9-arch1-1"), "6.17");
    }

    #[test]
    fn virtualisation() {
        assert_eq!(parse_virt("none\n"), (false, None));
        assert_eq!(parse_virt("kvm\n"), (true, Some("KVM".into())));
        assert_eq!(parse_virt("microsoft"), (true, Some("Hyper-V".into())));
        assert_eq!(windows_virt("Microsoft Corporation", "Virtual Machine"), (true, Some("Hyper-V".into())));
        assert_eq!(windows_virt("QEMU", "Standard PC (Q35 + ICH9, 2009)"), (true, Some("KVM".into())));
        assert_eq!(windows_virt("Dell Inc.", "OptiPlex 7090"), (false, None));
    }

    #[test]
    fn cpuinfo_counts_cores_and_threads() {
        let mut t = String::new();
        for (p, c) in [(0, 0), (1, 1), (2, 0), (3, 1)] {
            t.push_str(&format!("processor\t: {p}\nmodel name\t: AMD EPYC 7551 32-Core Processor\nphysical id\t: 0\ncore id\t\t: {c}\n\n"));
        }
        assert_eq!(parse_cpuinfo(&t), ("AMD EPYC 7551".to_string(), 2, 4));
        assert_eq!(clean_cpu("Intel(R) Core(TM) i7-9700 CPU @ 3.00GHz"), "Intel Core i7-9700");
    }

    #[test]
    fn meminfo() {
        let t = "MemTotal:        1000000 kB\nMemFree:          20000 kB\nSwapTotal:       2000000 kB\nSwapFree:        1500000 kB\n";
        assert_eq!(parse_meminfo(t), (1_024_000_000, 2_048_000_000, 512_000_000));
    }

    const LSBLK: &str = r#"NAME="sda" TYPE="disk" SIZE="104857600000" ROTA="0" TRAN="sata" MOUNTPOINT="" FSTYPE="" PKNAME="" MODEL="Example SSD 100"
NAME="sda1" TYPE="part" SIZE="536870912" ROTA="0" TRAN="" MOUNTPOINT="/boot/efi" FSTYPE="vfat" PKNAME="sda" MODEL=""
NAME="sda2" TYPE="part" SIZE="104000000000" ROTA="0" TRAN="" MOUNTPOINT="/" FSTYPE="ext4" PKNAME="sda" MODEL=""
NAME="sdb" TYPE="disk" SIZE="500000000000" ROTA="0" TRAN="sata" MOUNTPOINT="" FSTYPE="" PKNAME="" MODEL="Example SSD 500"
NAME="sdb1" TYPE="part" SIZE="500000000000" ROTA="0" TRAN="" MOUNTPOINT="/mnt/data" FSTYPE="ext4" PKNAME="sdb" MODEL=""
NAME="sdc" TYPE="disk" SIZE="2000000000000" ROTA="1" TRAN="sata" MOUNTPOINT="" FSTYPE="" PKNAME="" MODEL="Example HDD 2T"
NAME="sdc1" TYPE="part" SIZE="2000000000000" ROTA="1" TRAN="" MOUNTPOINT="/mnt/backup" FSTYPE="ext4" PKNAME="sdc" MODEL=""
NAME="sdd" TYPE="disk" SIZE="1000204886016" ROTA="1" TRAN="usb" MOUNTPOINT="" FSTYPE="" PKNAME="" MODEL="Example USB"
NAME="sdd1" TYPE="part" SIZE="1000204886016" ROTA="1" TRAN="" MOUNTPOINT="" FSTYPE="exfat" PKNAME="sdd" MODEL=""
NAME="loop0" TYPE="loop" SIZE="67108864" ROTA="0" TRAN="" MOUNTPOINT="/snap/core/1" FSTYPE="squashfs" PKNAME="" MODEL=""
NAME="zram0" TYPE="disk" SIZE="4294967296" ROTA="0" TRAN="" MOUNTPOINT="[SWAP]" FSTYPE="swap" PKNAME="" MODEL=""
"#;

    const DF: &str = "Filesystem                 1B-blocks         Avail
/dev/sda2      /                104000000000  68000000000
tmpfs          /run               1000000000   1000000000
/dev/sda1      /boot/efi           536870912    500000000
/dev/sdb1      /mnt/data        500000000000 310000000000
/dev/sdb1      /var/lib/docker/overlay2/x 500000000000 310000000000
/dev/sdc1      /mnt/backup     2000000000000 1600000000000
/dev/loop0     /snap/core/1         67108864            0
";

    #[test]
    fn disks_from_lsblk_and_df() {
        // df's header has four columns of its own; the parser skips it.
        let blk = parse_lsblk(LSBLK);
        assert_eq!(blk.len(), 11);
        assert_eq!(blk[3].model, "Example SSD 500");
        let df = parse_df(DF);
        let mut smart = HashMap::new();
        smart.insert("sdb".to_string(), (Some(true), Some(82u8)));
        let disks = build_disks(&blk, &df, Some(Path::new("/mnt/data/sentient/postgres")), &smart);
        let names: Vec<&str> = disks.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["/", "/mnt/backup", "/mnt/data", "sdd"]);
        assert!(disks[2].sentient && !disks[0].sentient);
        assert_eq!(disks[2].wear, Some(82));
        assert!(disks[2].worn());
        assert_eq!(disks[1].kind.as_deref(), Some("HDD"));
        assert_eq!(disks[3].kind.as_deref(), Some("USB"));
        assert!(!disks[3].mounted());
        // Data on the root file system.
        let disks = build_disks(&blk, &df, Some(Path::new("/opt/sentient")), &HashMap::new());
        assert!(disks[0].sentient);
    }

    #[test]
    fn smart_health_and_wear() {
        let nvme = r#"{"smart_status":{"passed":true},"nvme_smart_health_information_log":{"percentage_used":7}}"#;
        assert_eq!(parse_smart(nvme), (Some(true), Some(7)));
        let sata = r#"{"smart_status":{"passed":false},"ata_smart_attributes":{"table":[{"id":5,"value":100},{"id":177,"value":18}]}}"#;
        assert_eq!(parse_smart(sata), (Some(false), Some(82)));
        let hdd = r#"{"smart_status":{"passed":true},"ata_smart_attributes":{"table":[{"id":5,"value":100}]}}"#;
        assert_eq!(parse_smart(hdd), (Some(true), None));
        assert_eq!(parse_smart("not json"), (None, None));
    }

    #[test]
    fn network_interfaces() {
        let t = "1: lo    inet 127.0.0.1/8 scope host lo\\       valid_lft forever
2: eth0    inet 192.0.2.10/24 brd 192.0.2.255 scope global eth0\\       valid_lft forever
3: docker0    inet 172.17.0.1/16 brd 172.17.255.255 scope global docker0
4: eth1    inet 198.51.100.12/24 brd 198.51.100.255 scope global eth1
";
        assert_eq!(parse_ip(t), vec![("eth0".to_string(), "192.0.2.10".to_string()), ("eth1".to_string(), "198.51.100.12".to_string())]);
    }

    #[test]
    fn clock() {
        let t = "Timezone=Asia/Kolkata\nLocalRTC=no\nNTP=yes\nNTPSynchronized=yes\n";
        assert_eq!(parse_timedatectl(t), (Some(true), Some("Asia/Kolkata".into())));
        let chrony = "Reference ID    : C0000201 (ntp.example.local)\nSystem time     : 0.020000000 seconds slow of NTP time\n";
        assert_eq!(parse_offset(chrony), Some(-0.02));
        let sd = "       Server: 192.0.2.1 (ntp.example.local)\n       Offset: +1.500ms\n";
        assert_eq!(parse_offset(sd), Some(0.0015));
        assert_eq!(secs_label(0.02), "0.02 s");
        assert_eq!(secs_label(3.), "3 s");
        assert!(Clock { synced: Some(true), offset: Some(1.4), zone: None }.out_of_sync());
    }

    #[test]
    fn updates_and_last_restart() {
        let apt = "Reading package lists...\nInst libexample1 [1.0] (1.1 Ubuntu:22.04/jammy-updates [amd64])\nInst example-tool [2.0] (2.1 Ubuntu:22.04/jammy-updates [amd64])\nConf libexample1 (1.1 Ubuntu:22.04/jammy-updates [amd64])\n";
        assert_eq!(parse_apt(apt), 2);
        let dnf = "\nexample-lib.x86_64      1.2-3.el9      baseos\nexample-tool.noarch     2.0-1.el9      appstream\nObsoleting Packages\nold.x86_64   1-1   baseos\n";
        assert_eq!(parse_dnf(dnf), 2);
        let clean = "reboot   system boot  6.8.0 Mon Oct  6 07:20:09 2026   still running\nshutdown system down  6.8.0 Mon Oct  6 07:19:58 2026 - Mon Oct  6 07:20:09 2026  (00:00)\nreboot   system boot  6.8.0 Sun Aug 24 10:00:00 2026 - Mon Oct  6 07:19:58 2026\n";
        assert_eq!(parse_last(clean), Some(true));
        let crash = "reboot   system boot  6.8.0 Mon Oct  6 07:20:09 2026   still running\nreboot   system boot  6.8.0 Sun Aug 24 10:00:00 2026 - crash\n";
        assert_eq!(parse_last(crash), Some(false));
        assert_eq!(parse_last(""), None);
    }

    #[test]
    fn graphics() {
        assert_eq!(parse_nvidia("NVIDIA RTX A2000 12GB, 12282\n"), vec![Gpu { name: "NVIDIA RTX A2000 12GB".into(), mem: Some(12282 << 20) }]);
        let lspci = "00:02.0 \"VGA compatible controller [0300]\" \"Red Hat, Inc. [1b36]\" \"QXL paravirtual graphic card [0100]\" -r05 \"\" \"\"\n\
01:00.0 \"VGA compatible controller [0300]\" \"NVIDIA Corporation [10de]\" \"GA106 [RTX A2000 12GB] [2571]\" -ra1 \"\" \"\"\n\
02:00.0 \"Ethernet controller [0200]\" \"Intel Corporation [8086]\" \"I210 [1533]\" \"\" \"\"\n\
03:00.0 \"VGA compatible controller [0300]\" \"ASPEED Technology, Inc. [1a03]\" \"ASPEED Graphics Family [2000]\" -r41 \"\" \"\"\n";
        assert_eq!(parse_lspci(lspci), vec!["NVIDIA RTX A2000 12GB".to_string()]);
        assert!(basic_windows_adapter("Microsoft Basic Display Adapter"));
        assert!(!basic_windows_adapter("NVIDIA T400"));
    }

    #[test]
    fn windows_reading() {
        let raw: WinRaw = serde_json::from_str(include_str!("server_info_fixtures/windows-raw.json")).unwrap();
        let i = from_windows(raw, Some("D:\\SENTIENT\\data"), Some(3));
        assert_eq!(i.os, "Windows 10 Pro 22H2");
        assert_eq!(i.host, "PLANT-HMI-01");
        assert!(!i.machine.is_virtual);
        assert_eq!(i.machine.model.as_deref(), Some("Example Computers EX-7000"));
        assert_eq!(i.cpu.model, "Intel Core i5-10500");
        assert_eq!(i.gpus.len(), 1);
        assert_eq!(i.disks.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["C:", "D:", "Disk 2"]);
        assert!(i.disks[1].sentient);
        assert_eq!(i.disks[1].kind.as_deref(), Some("HDD"));
        assert_eq!(i.clock.synced, Some(true));
        assert_eq!(i.clock.offset, Some(0.0123));
        assert_eq!(i.clean_restart, Some(true));
        assert!(i.restart_needed);
        assert!(i.docker.is_none());
        assert_eq!(i.nets[0].speed, Some(1000));
    }

    #[test]
    fn fixtures_parse_and_attention() {
        for n in ["linux-gpu", "vm", "windows"] {
            let i: ServerInfo = serde_json::from_str(fixture(n).unwrap()).unwrap_or_else(|e| panic!("{n}: {e}"));
            assert!(!i.host.is_empty());
        }
        let i: ServerInfo = serde_json::from_str(fixture("linux-gpu").unwrap()).unwrap();
        let a = i.attention();
        assert!(a.iter().any(|l| l.contains("nearly full")), "{a:?}");
        let vm: ServerInfo = serde_json::from_str(fixture("vm").unwrap()).unwrap();
        assert!(vm.attention().iter().any(|l| l == "Data disk 82 % worn"), "{:?}", vm.attention());
        let mut ok = vm.clone();
        ok.disks.iter_mut().for_each(|d| d.wear = None);
        ok.clock.offset = Some(0.02);
        ok.swap_used = 0;
        assert!(ok.attention().is_empty(), "{:?}", ok.attention());
    }

    fn disk(name: &str, size_gb: u64, free_gb: Option<u64>) -> Disk {
        Disk { name: name.into(), size: size_gb << 30, free: free_gb.map(|f| f << 30), ..Default::default() }
    }

    #[test]
    fn the_card_says_each_problem_once() {
        let mut i = ServerInfo { disks: vec![disk("/", 100, Some(60)), disk("/mnt/data", 500, Some(20)), disk("/mnt/backup", 200, Some(4))], ..Default::default() };
        i.disks[1].sentient = true;
        i.disks[1].wear = Some(82);
        // Everything that needs a look, the nearly full disks among it.
        let all = i.attention();
        assert!(all.contains(&"Data disk 82 % worn".to_string()), "{all:?}");
        assert!(all.contains(&"Data disk nearly full".to_string()), "{all:?}");
        assert!(all.contains(&"/mnt/backup nearly full".to_string()), "{all:?}");
        // The card: the nearly full disks have their own rows instead.
        assert_eq!(i.card_attention(), ["Data disk 82 % worn"]);
        assert_eq!(i.nearly_full_others().iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["/mnt/backup"]);
        // Clock and swap.
        i.clock = Clock { synced: Some(true), offset: Some(3.2), zone: None };
        i.swap_total = 2 << 30;
        i.swap_used = 1500 << 20;
        let c = i.card_attention();
        assert!(c.contains(&"Clock 3.2 s off".to_string()), "{c:?}");
        assert!(c.contains(&"Heavy swap use: memory is short".to_string()), "{c:?}");
        i.clock.synced = Some(false);
        assert!(i.card_attention().contains(&"Clock not synchronised".to_string()));
        // Nothing to say: no Attention row at all.
        let calm = ServerInfo { disks: vec![disk("/", 100, Some(60))], clock: Clock { synced: Some(true), offset: Some(0.4), zone: None }, swap_total: 2 << 30, swap_used: 100 << 20, ..Default::default() };
        assert!(calm.card_attention().is_empty(), "{:?}", calm.card_attention());
        // Half the swap but under 512 MB is not heavy.
        let small = ServerInfo { swap_total: 512 << 20, swap_used: 400 << 20, ..Default::default() };
        assert!(!small.heavy_swap());
    }

    #[test]
    fn disk_health_pill() {
        let mut d = disk("/", 100, Some(50));
        assert_eq!(d.health(), Health::NotKnown);
        d.smart_ok = Some(true);
        assert_eq!(d.health(), Health::Healthy);
        d.wear = Some(79);
        assert_eq!(d.health(), Health::Healthy);
        d.wear = Some(80);
        assert_eq!(d.health(), Health::Worn(80));
        d.smart_ok = Some(false);
        assert_eq!(d.health(), Health::Warning);
        // Nearly full: under a tenth, or under 5 GB.
        assert!(disk("/a", 100, Some(9)).nearly_full());
        assert!(disk("/b", 30, Some(4)).nearly_full());
        assert!(!disk("/c", 100, Some(11)).nearly_full());
        assert!(!disk("sdz", 100, None).nearly_full());
    }

    #[test]
    fn only_real_disks_are_listed() {
        let blk = parse_lsblk(concat!(
            "NAME=\"vda\" TYPE=\"disk\" SIZE=\"107374182400\" ROTA=\"0\" TRAN=\"\" MOUNTPOINT=\"\" FSTYPE=\"\" PKNAME=\"\" MODEL=\"\"\n",
            "NAME=\"vda1\" TYPE=\"part\" SIZE=\"107374182400\" ROTA=\"0\" TRAN=\"\" MOUNTPOINT=\"/\" FSTYPE=\"ext4\" PKNAME=\"vda\" MODEL=\"\"\n",
            "NAME=\"loop3\" TYPE=\"loop\" SIZE=\"67108864\" ROTA=\"0\" TRAN=\"\" MOUNTPOINT=\"/snap/lxd/1\" FSTYPE=\"squashfs\" PKNAME=\"\" MODEL=\"\"\n",
            "NAME=\"sr0\" TYPE=\"rom\" SIZE=\"1048576\" ROTA=\"1\" TRAN=\"sata\" MOUNTPOINT=\"\" FSTYPE=\"\" PKNAME=\"\" MODEL=\"\"\n",
        ));
        let df = parse_df(concat!(
            "Filesystem 1B-blocks Avail\n",
            "/dev/vda1 / 107374182400 53687091200\n",
            "tmpfs /dev/shm 1073741824 1073741824\n",
            "tmpfs /run/user/1000 104857600 104857600\n",
            "overlay /var/lib/docker/overlay2/abc/merged 107374182400 53687091200\n",
            "/dev/loop3 /snap/lxd/1 67108864 0\n",
        ));
        let disks = build_disks(&blk, &df, None, &HashMap::new());
        assert_eq!(disks.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["/"]);
    }
}
