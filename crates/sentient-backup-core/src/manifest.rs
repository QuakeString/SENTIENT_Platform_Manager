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
    /// The platform's settings when the backup was made, for a restore to
    /// compare with where it goes. Never secrets. Absent in backups made
    /// before they were recorded (Platform Manager 0.8.0 and earlier).
    #[serde(default)]
    pub platform: Option<PlatformInfo>,
}

/// Platform settings at backup time (`manifest.platform`). Never a secret,
/// key or licence: names, ports, paths, expiry dates.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlatformInfo {
    /// Settings by the Platform Manager's catalog name: ports (`port:http`,
    /// `port:mqtt`, …), SENTIENT's environment settings, PostgreSQL's
    /// parameters (`pg:…`). Secret-looking names are never recorded.
    #[serde(default)]
    pub settings: std::collections::BTreeMap<String, String>,
    /// The address devices and people reach SENTIENT by.
    #[serde(default)]
    pub public_url: Option<String>,
    #[serde(default)]
    pub tls: Option<TlsInfo>,
    #[serde(default)]
    pub licence: Option<LicenceInfo>,
    /// Gateways that send to this SENTIENT: "name → host:port".
    #[serde(default)]
    pub gateways: Vec<String>,
    /// Where file stores live on that machine, by store id.
    #[serde(default)]
    pub file_store_paths: std::collections::BTreeMap<String, String>,
}

/// The HTTPS certificate: its name, issuer and expiry (never the key).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TlsInfo {
    pub name: String,
    #[serde(default)]
    pub issuer: Option<String>,
    /// RFC 3339.
    #[serde(default)]
    pub expires: Option<String>,
}

/// The licence the instance had: its kind and the machine it is bound to
/// (never the licence itself; a licence is bound to hardware).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LicenceInfo {
    pub kind: String,
    #[serde(default)]
    pub bound_to: Option<String>,
    /// A hash of the hardware binding, to tell whether it is this machine.
    #[serde(default)]
    pub hardware_hash: Option<String>,
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
