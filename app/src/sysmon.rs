// Copyright © 2016-2026 The SENTIENT Authors

//! The top bar's system monitor: processor, memory and the free disk of
//! the instance that is open (this computer until remote instances).

use std::path::Path;

#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Monitor {
    /// 0–100.
    pub cpu: f32,
    pub mem_used: u64,
    pub mem_total: u64,
    pub disk_free: u64,
    pub disk_total: u64,
}

impl Monitor {
    pub fn cpu_high(&self) -> bool {
        self.cpu >= 90.0
    }

    pub fn mem_high(&self) -> bool {
        self.mem_total > 0 && self.mem_used as f64 / self.mem_total as f64 >= 0.9
    }

    pub fn disk_low(&self) -> bool {
        self.disk_total > 0 && self.disk_free < crate::dbstatus::LOW_DISK
    }
}

/// One reading. Blocking (about a quarter of a second for the processor).
pub fn sample(data_dir: &Path) -> Monitor {
    use sysinfo::{CpuRefreshKind, Disks, MemoryRefreshKind, RefreshKind, System};
    let mut sys = System::new_with_specifics(RefreshKind::new().with_cpu(CpuRefreshKind::new().with_cpu_usage()).with_memory(MemoryRefreshKind::new().with_ram()));
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL.max(std::time::Duration::from_millis(250)));
    sys.refresh_cpu_usage();
    let disks = Disks::new_with_refreshed_list();
    // The disk holding the data: the longest mount point that contains it.
    let disk = disks
        .list()
        .iter()
        .filter(|d| data_dir.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .or_else(|| disks.list().iter().max_by_key(|d| d.total_space()));
    Monitor {
        cpu: sys.global_cpu_usage(),
        mem_used: sys.used_memory(),
        mem_total: sys.total_memory(),
        disk_free: disk.map(|d| d.available_space()).unwrap_or(0),
        disk_total: disk.map(|d| d.total_space()).unwrap_or(0),
    }
}
