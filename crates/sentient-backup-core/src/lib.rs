//! `sentient-backup-core` — the UI-agnostic engine behind the SENTIENT Backup &
//! Restore application. The Tauri GUI and the `sbr` CLI are both thin shells
//! over this crate.
//!
//! Phase 0 provides:
//! - [`categories`]: the selectable backup-component model + table mapping.
//! - [`db`]: connect to a SENTIENT PostgreSQL/TimescaleDB and report per-category
//!   sizes and row counts.
//!
//! Later phases add the backup/restore engines (pg_dump/pg_restore + COPY,
//! manifest, compression, encryption, file stores) — see `docs/RESEARCH_AND_PLAN.md`.

pub mod backup;
pub mod categories;
pub mod db;
pub mod error;
pub mod files;
pub mod manifest;
pub mod pg_tools;
pub mod progress;
pub mod restore;
pub mod util;

pub use error::{Error, Result};

/// Crate version (from Cargo).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
