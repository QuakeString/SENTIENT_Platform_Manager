//! The `manifest.json` embedded in every `.sentient-backup` archive — describes
//! what the backup contains and lets restore check compatibility & integrity.

use serde::{Deserialize, Serialize};

pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u32,
    pub tool_version: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub source: SourceInfo,
    /// What was selected and how big it was at backup time.
    pub components: Vec<ComponentEntry>,
    /// Whether telemetry data was included, and (if ranged) the window.
    pub telemetry: TelemetrySelection,
    /// Archive members + their checksums.
    pub files: Vec<FileEntry>,
    /// File stores (vc-repos/reports) captured in this backup.
    #[serde(default)]
    pub file_stores: Vec<FileStoreEntry>,
    pub encryption: EncryptionInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileStoreEntry {
    pub id: String,
    pub category_id: String,
    /// Where it lived on the source machine.
    pub source_path: String,
    /// Archive member holding the tar.zst.
    pub member: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceInfo {
    pub database: String,
    pub postgres_version: String,
    pub timescaledb_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentEntry {
    pub id: String,
    pub name: String,
    pub selected: bool,
    pub tables: Vec<String>,
    pub bytes: i64,
    pub rows: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum TelemetrySelection {
    None,
    All,
    /// Last N days.
    Range { days: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptionInfo {
    /// "none" | "age" (age lands in a later phase).
    pub scheme: String,
}

impl EncryptionInfo {
    pub fn none() -> Self {
        Self {
            scheme: "none".into(),
        }
    }
}
