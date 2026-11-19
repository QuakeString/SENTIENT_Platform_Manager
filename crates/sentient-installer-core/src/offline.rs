// Copyright © 2016-2026 The SENTIENT Authors

//! Offline mode: an install or update that runs from a verified offline
//! pack (crate::pack). While it is on, every downloader asks
//! [`refuse_network`] first and stops with a plain message instead of
//! trying the network, and the steps that would download (the WSL package,
//! the Ubuntu image, Docker Engine, the images, the gateway installer) take
//! the pack's files instead ([`pack`]).
//!
//! Process-wide: Setup runs one install at a time. Child processes learn
//! it from [`ENV`] (inherited) or, for the ClickHouse account's agent
//! started by Task Scheduler, from the marker file the installer writes
//! (crate::clickhouse_wsl::offline_marker).

use std::path::Path;
use std::sync::{Arc, RwLock};

use crate::pack::{self, Opened};

/// The pack's folder, for a child process (set by [`activate`]).
pub const ENV: &str = "SPM_OFFLINE_PACK";

static PACK: RwLock<Option<Arc<Opened>>> = RwLock::new(None);

/// Turn offline mode on with `pack` (verified by the caller), or off.
pub fn set(pack: Option<Arc<Opened>>) {
    match &pack {
        Some(p) => std::env::set_var(ENV, &p.dir),
        None => std::env::remove_var(ENV),
    }
    // The guard lives for this statement only.
    *PACK.write().unwrap_or_else(|e| e.into_inner()) = pack;
}

/// The pack offline mode runs from (None: online).
pub fn pack() -> Option<Arc<Opened>> {
    PACK.read().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn is_offline() -> bool {
    pack().is_some()
}

/// Open the pack in `dir` (its signature checked), check it was not
/// changed (`full`: every SHA-256 again; else every file there with its
/// size), and turn offline mode on with it.
pub fn activate(dir: &Path, full: bool) -> Result<Arc<Opened>, String> {
    let opened = pack::open(dir).map_err(|e| e.message())?;
    pack::check_target(&opened.pack, &pack::Host::this()).map_err(|e| e.message())?;
    if full {
        opened.verify_files(&|_| {}, &crate::cancel::is_cancelled).map_err(|e| e.message())?;
    } else {
        opened.check_present().map_err(|e| e.message())?;
    }
    let opened = Arc::new(opened);
    set(Some(opened.clone()));
    Ok(opened)
}

/// A process started with [`ENV`] set (an elevated job, a headless run):
/// offline mode from that pack. Errors are returned for the caller to say.
pub fn init_from_env() -> Result<(), String> {
    match std::env::var_os(ENV) {
        Some(dir) if !dir.is_empty() => activate(Path::new(&dir), false).map(|_| ()),
        _ => Ok(()),
    }
}

/// The one message a downloader gives in offline mode.
pub fn refusal(what: &str) -> String {
    format!(
        "This install runs from an offline pack and does not use the internet, but {what} would need it. \
         The pack does not carry it: use a newer pack, or connect this computer to the internet and run Setup without the pack."
    )
}

/// `Err` when offline mode is on: `what` would have been fetched (the
/// file, the host). Every downloader calls this before it opens a
/// connection.
pub fn refuse_network(what: &str) -> Result<(), String> {
    if is_offline() { Err(refusal(what)) } else { Ok(()) }
}

/// Tests share the process-wide mode: one at a time.
#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn online_lets_downloads_through_and_offline_refuses_them_by_name() {
        let _g = test_lock();
        set(None);
        assert!(refuse_network("the Ubuntu image").is_ok());
        set(Some(Arc::new(pack::debug_fake())));
        assert!(is_offline());
        assert_eq!(std::env::var_os(ENV).map(std::path::PathBuf::from), Some(pack::debug_fake().dir));
        let e = refuse_network("the Ubuntu image").unwrap_err();
        assert!(e.contains("offline pack") && e.contains("the Ubuntu image would need it"), "{e}");
        set(None);
        assert!(!is_offline());
        assert!(std::env::var_os(ENV).is_none());
    }

    /// Every downloader, offline: refused with the offline message, before
    /// any connection (the addresses would not resolve; a refusal that
    /// came from the network would say so instead).
    #[test]
    fn every_downloader_refuses_in_offline_mode() {
        let _g = test_lock();
        set(Some(Arc::new(pack::debug_fake())));
        let sink = crate::progress::noop();
        let url = "https://spm-offline-test.invalid/manifest.json";
        let dir = std::env::temp_dir().join(format!("spm-offline-dl-{}", std::process::id()));
        let art = crate::manifest::Artifact { file: "x.bin".into(), url: url.into(), sha256: "00".into(), size: 1, kind: String::new(), into: String::new() };
        let results: Vec<(&str, String)> = vec![
            ("net::get", crate::net::get(url).map(|_| ()).unwrap_err()),
            ("manifest::fetch", crate::manifest::fetch(url).map(|_| ()).unwrap_err()),
            ("manifest::fetch_signed", crate::manifest::fetch_signed(url, true).map(|_| ()).unwrap_err()),
            ("manifest::download", crate::manifest::download(&sink, &art, &dir).map(|_| ()).unwrap_err()),
            ("signing::fetch_signature", crate::signing::fetch_signature(url).map(|_| ()).unwrap_err()),
            ("releases::request", crate::releases::request("QuakeString/spm-offline-test").map(|_| ()).unwrap_err().message().to_string()),
            ("dockerhub::get", crate::dockerhub::get(url).map(|_| ()).unwrap_err().message().to_string()),
            ("distro::download (rootfs, kernel and WSL MSIs, Chrome)", crate::distro::download(url, &dir.join("y"), &sink).unwrap_err()),
            ("native::obtain (PostgreSQL, TimescaleDB, vc_redist)", crate::native::obtain_for_test(url, &dir).unwrap_err()),
        ];
        set(None);
        let _ = std::fs::remove_dir_all(&dir);
        for (what, e) in results {
            assert!(e.contains("runs from an offline pack and does not use the internet"), "{what}: {e}");
        }
    }

    /// No connection outside crate::net::get: the agent is private there,
    /// and nothing builds its own (localhost probes and the licence
    /// server, which stays online by decision, are the exceptions).
    #[test]
    fn no_download_bypasses_the_offline_check() {
        let here = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let roots = [here.join("src"), here.join("../../app/src")];
        let allowed = [
            // SENTIENT on this computer answers (127.0.0.1).
            "native.rs:fn http_answers_on",
            "distro.rs:fn clickhouse_answers",
            "adopt_windows.rs:",
            // The licence server: activation is online by decision.
            "licence.rs:",
            // This test and the gate itself.
            "net.rs:",
            "offline.rs:",
        ];
        let mut dirs: Vec<std::path::PathBuf> = roots.iter().filter(|r| r.is_dir()).cloned().collect();
        let mut found = Vec::new();
        let mut files = 0;
        while let Some(d) = dirs.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    dirs.push(p);
                    continue;
                }
                if p.extension().is_none_or(|x| x != "rs") {
                    continue;
                }
                files += 1;
                let name = p.file_name().unwrap().to_string_lossy().to_string();
                let text = std::fs::read_to_string(&p).unwrap();
                let mut func = String::new();
                for line in text.lines() {
                    let t = line.trim_start();
                    if t.starts_with("fn ") || t.starts_with("pub fn ") || t.starts_with("pub(crate) fn ") {
                        func = t.trim_start_matches("pub(crate) ").trim_start_matches("pub ").split('(').next().unwrap_or("").to_string();
                    }
                    if t.starts_with("//") {
                        continue;
                    }
                    let opens = ["net::agent()", "ureq::get(", "ureq::post(", "ureq::AgentBuilder", "ureq::agent("].iter().any(|n| line.contains(n));
                    if opens && !allowed.iter().any(|a| a.split_once(':').is_some_and(|(f, func_name)| f == name && (func_name.is_empty() || func == func_name))) {
                        found.push(format!("{name} {func}: {}", line.trim()));
                    }
                }
            }
        }
        assert!(files > 50, "the sources were found");
        assert!(found.is_empty(), "connections that skip crate::net::get (offline mode would not stop them):\n{}", found.join("\n"));
    }
}
