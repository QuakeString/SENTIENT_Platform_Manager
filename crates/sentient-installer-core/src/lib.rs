//! UI-agnostic engine for the SENTIENT installer.
//! Phase 0: read-only preflight checks. Phase 1: WSL2 provisioning.

pub mod cancel;
pub mod checks;
pub mod distro;
pub(crate) mod exec;
pub mod stack;
pub mod kiosk;
/// Native Windows deployment (PostgreSQL + TimescaleDB + the server as a
/// service) — the no-Docker, no-WSL alternative to [`distro`].
/// Release manifest: where to fetch the SENTIENT payload and what it must
/// hash to. Keeps the download host out of the code.
pub mod manifest;
pub mod native;
/// Native Linux deployment (distribution PostgreSQL, optional ClickHouse,
/// SENTIENT as a systemd unit) — what [`native`] delegates to on Linux.
#[cfg(target_os = "linux")]
pub mod native_linux;
pub mod progress;
mod sys;
pub mod wsl;
