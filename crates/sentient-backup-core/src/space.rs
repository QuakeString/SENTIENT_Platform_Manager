// Copyright © 2016-2026 The SENTIENT Authors

//! Free space during a restore: a watch that reads every disk the restore
//! writes to (through a probe the caller gives, since only it knows where
//! each store lives: a Docker volume, a native data folder, a WSL disk) and
//! trips when one falls under the safe level. The restore checks it between
//! its steps and stops pg_restore at once, so a disk never fills.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The safe level: a restore stops when a disk it uses falls under it.
pub const DEFAULT_MIN_FREE: u64 = 2_000_000_000;

/// How often the watch reads the disks.
pub const DEFAULT_EVERY: Duration = Duration::from_secs(10);

/// One disk's free space, by the name the operator knows it by
/// ("/mnt/disk-a (PostgreSQL)").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskFree {
    pub label: String,
    pub free: u64,
}

/// What to watch. `probe` reads the free space of every disk the restore uses.
#[derive(Clone)]
pub struct SpaceWatch {
    pub probe: Arc<dyn Fn() -> Vec<DiskFree> + Send + Sync>,
    pub min_free: u64,
    pub every: Duration,
}

impl std::fmt::Debug for SpaceWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpaceWatch").field("min_free", &self.min_free).field("every", &self.every).finish()
    }
}

impl SpaceWatch {
    pub fn new(probe: impl Fn() -> Vec<DiskFree> + Send + Sync + 'static) -> Self {
        Self { probe: Arc::new(probe), min_free: DEFAULT_MIN_FREE, every: DEFAULT_EVERY }
    }

    /// Start watching (a thread). It reads at once, then every `every`,
    /// until the guard is dropped.
    pub fn start(&self) -> WatchGuard {
        let tripped: Arc<Mutex<Option<String>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (t, s, w) = (tripped.clone(), stop.clone(), self.clone());
        std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                if let Some(low) = (w.probe)().into_iter().find(|d| d.free < w.min_free) {
                    *t.lock().unwrap() = Some(stopped_message(&low, w.min_free));
                    return;
                }
                // Sleep in short slices so a dropped guard ends the thread soon.
                let mut slept = Duration::ZERO;
                while slept < w.every && !s.load(Ordering::Relaxed) {
                    let d = Duration::from_millis(200).min(w.every - slept);
                    std::thread::sleep(d);
                    slept += d;
                }
            }
        });
        WatchGuard { tripped, stop }
    }
}

/// A running watch; dropping it stops the thread.
#[derive(Clone)]
pub struct WatchGuard {
    tripped: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
}

impl WatchGuard {
    /// A guard that never trips (no watch asked for).
    pub fn none() -> Self {
        Self { tripped: Arc::default(), stop: Arc::new(AtomicBool::new(true)) }
    }

    /// Why the restore must stop, once a disk is low.
    pub fn tripped(&self) -> Option<String> {
        self.tripped.lock().unwrap().clone()
    }

    /// `Err` with the reason once tripped.
    pub fn check(&self) -> crate::Result<()> {
        match self.tripped() {
            Some(m) => Err(crate::Error::SpaceLow(m)),
            None => Ok(()),
        }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// "Stopped: /mnt/disk-a (PostgreSQL) fell to 1.9 GB free, under the safe level of 2 GB."
pub fn stopped_message(d: &DiskFree, min_free: u64) -> String {
    format!("Stopped: {} fell to {} free, under the safe level of {}.", d.label, gb(d.free), gb(min_free))
}

/// Bytes as the operator reads them: "1.9 GB", "2 GB", "640 MB".
pub fn gb(b: u64) -> String {
    let g = b as f64 / 1e9;
    if g >= 10.0 || (g - g.round()).abs() < 0.05 && g >= 1.0 {
        format!("{} GB", g.round() as u64)
    } else if g >= 1.0 {
        format!("{g:.1} GB")
    } else {
        format!("{} MB", (b as f64 / 1e6).round() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trips_on_the_low_disk_and_says_which() {
        let free = Arc::new(std::sync::atomic::AtomicU64::new(5_000_000_000));
        let f2 = free.clone();
        let mut w = SpaceWatch::new(move || {
            vec![
                DiskFree { label: "/ (/dev/sda1)".into(), free: 50_000_000_000 },
                DiskFree { label: "/mnt/disk-a (PostgreSQL)".into(), free: f2.load(Ordering::Relaxed) },
            ]
        });
        w.every = Duration::from_millis(20);
        let g = w.start();
        std::thread::sleep(Duration::from_millis(60));
        assert!(g.tripped().is_none());
        free.store(1_900_000_000, Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(g.tripped().as_deref(), Some("Stopped: /mnt/disk-a (PostgreSQL) fell to 1.9 GB free, under the safe level of 2 GB."));
        assert!(matches!(g.check(), Err(crate::Error::SpaceLow(_))));
        g.stop();
    }

    #[test]
    fn sizes_read_plainly() {
        assert_eq!(gb(1_900_000_000), "1.9 GB");
        assert_eq!(gb(2_000_000_000), "2 GB");
        assert_eq!(gb(64_100_000_000), "64 GB");
        assert_eq!(gb(640_000_000), "640 MB");
    }
}
