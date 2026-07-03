//! UI-agnostic engine for the SENTIENT installer.
//! Phase 0: read-only preflight checks. Phase 1: WSL2 provisioning.

pub mod checks;
pub mod distro;
pub mod progress;
mod sys;
pub mod wsl;
