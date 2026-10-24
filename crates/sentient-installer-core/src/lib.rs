//! UI-agnostic engine for the SENTIENT installer.
//! Phase 0: read-only preflight checks. Phase 1: WSL2 provisioning.

/// Automatic sign-in for operator screens (off unless asked for).
pub mod autologin;
pub mod cancel;
pub mod checks;
/// ClickHouse for a native Windows install: Docker in WSL2, owned by a
/// hidden service account so it runs from boot.
pub mod clickhouse_wsl;
pub mod distro;
pub(crate) mod exec;
pub mod stack;
pub mod kiosk;
pub mod kept_db;
/// Native Windows deployment (PostgreSQL + TimescaleDB + the server as a
/// service) — the no-Docker, no-WSL alternative to [`distro`].
/// Release manifest: where to fetch the SENTIENT payload and what it must
/// hash to. Keeps the download host out of the code.
pub mod manifest;
pub mod native;
pub mod net;
/// Native Linux deployment (distribution PostgreSQL, optional ClickHouse,
/// SENTIENT as a systemd unit) — what [`native`] delegates to on Linux.
#[cfg(target_os = "linux")]
pub mod native_linux;
pub mod progress;
pub mod releases;
/// Running a script as root from a desktop session (pkexec, else `sudo -n`).
#[cfg(unix)]
pub mod privileged {
    use crate::progress::ProgressFn;
    pub fn run(sink: &ProgressFn, script: &str) -> Result<(), String> {
        crate::exec::run_privileged(sink, script)
    }
    pub fn shell_quote(s: &str) -> String {
        crate::exec::shq(s)
    }
}
mod sys;
pub mod wsl;
