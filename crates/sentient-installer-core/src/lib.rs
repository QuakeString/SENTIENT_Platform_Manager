//! UI-agnostic engine for the SENTIENT installer.
//! Phase 0: read-only preflight checks. Phase 1: WSL2 provisioning.

/// Automatic sign-in for operator screens (off unless asked for).
pub mod autologin;
pub mod cancel;
pub mod checks;
pub mod size;
/// ClickHouse for a native Windows install: Docker in WSL2, owned by a
/// hidden service account so it runs from boot.
pub mod clickhouse_wsl;
pub mod distro;
/// One administrator elevation per job (a root helper fed over a pipe).
pub mod elevation;
pub(crate) mod exec;
pub mod stack;
pub mod stack_order;
pub mod gateway;
pub mod kiosk;
pub mod kept_db;
/// Native Windows deployment (PostgreSQL + TimescaleDB + the server as a
/// service) — the no-Docker, no-WSL alternative to [`distro`].
/// Release manifest: where to fetch the SENTIENT payload and what it must
/// hash to. Keeps the download host out of the code.
pub mod manifest;
pub mod native;
pub mod net;
/// A PostgreSQL data folder from an older major: upgrade or keep aside.
pub mod old_pg;
/// What went wrong in a failed step, and the fix (Setup and every SPM job).
pub mod diagnose;
/// Native Linux deployment (distribution PostgreSQL, optional ClickHouse,
/// SENTIENT as a systemd unit) — what [`native`] delegates to on Linux.
#[cfg(target_os = "linux")]
pub mod native_linux;
pub mod progress;
pub mod releases;
/// Uninstall part by part, and the check that nothing is left.
pub mod removal;
pub mod signing;
pub mod vcrt;
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
/// Running the computer's own programs, also from inside a Flatpak.
pub mod host {
    pub use crate::sys::{ensure_valid_cwd, flatpak_init, host_argv, host_command, host_command_env, in_flatpak, safe_dir};
}
/// systemd units and port owners, as the System check reads them.
pub mod units;
pub mod wsl;
