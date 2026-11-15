//! The `manifest.json` embedded in every `.sentient-backup` archive — describes
//! what the backup contains and lets restore check compatibility & integrity.

use serde::{Deserialize, Serialize};

/// 2: PostgreSQL telemetry stored month by month (`db/telemetry/YYYY-MM.copy.zst`),
/// so a damaged part costs one month; 1 had one telemetry member.
pub const FORMAT_VERSION: u32 = 2;

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
    /// Telemetry held in ClickHouse (PostgreSQL + ClickHouse installs).
    #[serde(default)]
    pub clickhouse: Option<crate::clickhouse::ChSection>,
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
    /// Where the backup comes from, so a restore can say so (and refuse a
    /// backup from a newer SENTIENT) before anything is restored. Absent in
    /// backups made before these were recorded.
    #[serde(default)]
    pub sentient_version: Option<String>,
    #[serde(default)]
    pub schema_version: Option<i64>,
    /// "PostgreSQL + ClickHouse", "PostgreSQL + TimescaleDB", "PostgreSQL".
    #[serde(default)]
    pub database_type: Option<String>,
    /// The computer it was made on.
    #[serde(default)]
    pub host: Option<String>,
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
