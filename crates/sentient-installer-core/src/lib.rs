//! UI-agnostic engine for the SENTIENT installer.
//! Phase 0: read-only preflight checks. Phase 1: WSL2 provisioning.

pub mod cancel;
pub mod checks;
pub mod distro;
pub mod kiosk;
/// Native Windows deployment (PostgreSQL + TimescaleDB + the server as a
/// service) — the no-Docker, no-WSL alternative to [`distro`].
pub mod native;
pub mod progress;
mod sys;
pub mod wsl;
