//! The backup component model: user-selectable categories and how each maps to
//! the SENTIENT database tables and on-disk file stores.
//!
//! Design: every non-config category declares explicit table matchers. The
//! **Configuration** category is the catch-all — any `public` table not claimed
//! by another category belongs to it. That way a future SENTIENT version that
//! adds a table is captured safely (config is always backed up) instead of
//! being silently dropped. See `docs/RESEARCH_AND_PLAN.md` §3.

use serde::Serialize;

/// How a category is realized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CategoryKind {
    /// Catch-all for all config/entity tables. Locked on.
    Configuration,
    /// The `ts_kv` time-series hypertable — supports none/all/last-N-days.
    TelemetryHistorical,
    /// Ordinary relational tables.
    Db,
    /// Backed by an on-disk directory in addition to (or instead of) DB tables.
    FileStore,
}

/// Matches a live table name to a category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum TableMatch {
    /// Exact table name.
    Exact(&'static str),
    /// Name starts with this prefix — for monthly/yearly partitions
    /// (e.g. `audit_log_2026_07`, `report_2026`).
    Prefix(&'static str),
}

impl TableMatch {
    pub fn matches(&self, name: &str) -> bool {
        match self {
            TableMatch::Exact(n) => *n == name,
            TableMatch::Prefix(p) => name.starts_with(p),
        }
    }

    /// A `pg_dump` object pattern (`--exclude-table-data`). A `Prefix` becomes a
    /// `*` glob so declarative-partition children (e.g. `audit_log_2026_07`) are
    /// covered too.
    pub fn pg_pattern(&self) -> String {
        match self {
            TableMatch::Exact(n) => format!("public.{n}"),
            TableMatch::Prefix(p) => format!("public.{p}*"),
        }
    }
}

impl Category {
    /// `pg_dump` patterns covering this category's tables (for excluding data).
    pub fn pg_patterns(&self) -> Vec<String> {
        self.tables.iter().map(|m| m.pg_pattern()).collect()
    }
}

/// An on-disk store outside the database.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct FileStore {
    pub id: &'static str,
    /// Environment variable SENTIENT reads for this path.
    pub env_var: &'static str,
    pub default_path: &'static str,
}

/// A selectable backup component.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Category {
    pub id: &'static str,
    pub name: &'static str,
    /// Suggested initial checkbox state.
    pub default_selected: bool,
    /// Cannot be deselected (Configuration).
    pub locked: bool,
    pub kind: CategoryKind,
    /// Explicit table matchers (empty for the Configuration catch-all).
    pub tables: &'static [TableMatch],
    /// Optional on-disk store — only backed up when the path is reachable.
    pub file_store: Option<FileStore>,
    pub notes: &'static str,
}

use CategoryKind::*;
use TableMatch::{Exact, Prefix};

const REPORTS_STORE: FileStore = FileStore {
    id: "reports",
    env_var: "REPORT_OUTPUT_DIR",
    default_path: "/var/lib/sentient/reports",
};
const VC_REPOS_STORE: FileStore = FileStore {
    id: "vc-repos",
    env_var: "VC_REPOS_PATH",
    default_path: "/var/lib/sentient/vc-repos",
};

/// The full category catalog, in match-priority order. Non-config categories
/// are evaluated first; anything unmatched falls to Configuration.
pub fn catalog() -> &'static [Category] {
    &[
        Category {
            id: "telemetry_historical",
            name: "Telemetry (historical)",
            default_selected: true,
            locked: false,
            kind: TelemetryHistorical,
            tables: &[Exact("ts_kv")],
            file_store: None,
            notes: "The ts_kv hypertable — usually the largest component. Supports none/all/last-N-days.",
        },
        Category {
            id: "telemetry_latest",
            name: "Telemetry (latest)",
            default_selected: true,
            locked: false,
            kind: Db,
            tables: &[Exact("ts_kv_latest")],
            file_store: None,
            notes: "Current value per key.",
        },
        Category {
            id: "attributes",
            name: "Attributes",
            default_selected: true,
            locked: false,
            kind: Db,
            tables: &[Exact("attribute_kv")],
            file_store: None,
            notes: "Client / shared / server attributes.",
        },
        Category {
            id: "alarms",
            name: "Alarms",
            default_selected: true,
            locked: false,
            kind: Db,
            tables: &[
                Exact("alarm"),
                Exact("entity_alarm"),
                Exact("alarm_types"),
                Prefix("alarm_comment"),
            ],
            file_store: None,
            notes: "Alarms, propagation index and comments.",
        },
        Category {
            id: "rpc_history",
            name: "RPC history",
            default_selected: false,
            locked: false,
            kind: Db,
            tables: &[Exact("rpc")],
            file_store: None,
            notes: "Persistent / one-way RPC log. Rarely needed on restore.",
        },
        Category {
            id: "audit_event_logs",
            name: "Audit & event logs",
            default_selected: false,
            locked: false,
            kind: Db,
            tables: &[
                Prefix("audit_log"),
                Prefix("edge_event"),
                Prefix("error_event"),
                Prefix("lc_event"),
                Prefix("stats_event"),
                Prefix("rule_chain_debug_event"),
                Prefix("rule_node_debug_event"),
                Prefix("cf_debug_event"),
                Exact("analytics_pipeline_debug_event"),
            ],
            file_store: None,
            notes: "Noisy, monthly-partitioned diagnostic logs. Usually skippable.",
        },
        Category {
            id: "notifications",
            name: "Notifications (delivered)",
            default_selected: false,
            locked: false,
            // Careful: notification_rule/target/template are CONFIG, not history.
            kind: Db,
            tables: &[
                Exact("notification"),
                Prefix("notification_2"),
                Exact("notification_request"),
                Exact("user_notification_destination"),
            ],
            file_store: None,
            notes: "Delivered-notification history (not the rules/targets/templates, which are Configuration).",
        },
        Category {
            id: "reports",
            name: "Reports",
            default_selected: false,
            locked: false,
            kind: FileStore,
            // report_template is CONFIG; report_2024..2028 are history partitions.
            tables: &[
                Exact("report"),
                Prefix("report_2"),
                Exact("report_delivery"),
                Exact("report_job"),
            ],
            file_store: Some(REPORTS_STORE),
            notes: "Generated report rows + files. File store enabled only when reachable.",
        },
        Category {
            id: "version_control",
            name: "Version control",
            default_selected: false,
            locked: false,
            kind: FileStore,
            tables: &[Exact("entity_version"), Exact("vc_request")],
            file_store: Some(VC_REPOS_STORE),
            notes: "Entity-version history + git repos. File store enabled only when reachable.",
        },
        Category {
            id: "api_usage_stats",
            name: "API usage & operational state",
            default_selected: false,
            locked: false,
            kind: Db,
            tables: &[
                Exact("api_usage_state"),
                Exact("queue_stats"),
                Exact("task_job"),
                Exact("telegram_link_code"),
                Exact("whatsapp_verification_code"),
            ],
            file_store: None,
            notes: "Operational counters and transient state.",
        },
        Category {
            id: "licensing",
            name: "Licensing",
            default_selected: false,
            locked: false,
            kind: Db,
            tables: &[Prefix("license")],
            file_store: None,
            notes: "Machine-bound (anti-piracy HW tuple). Off by default so a portable backup doesn't carry a license that won't validate on the target host.",
        },
        // MUST be last — the catch-all.
        Category {
            id: "configuration",
            name: "Configuration (entities, dashboards, users, …)",
            default_selected: true,
            locked: true,
            kind: Configuration,
            tables: &[],
            file_store: None,
            notes: "All config/entity tables (catch-all for anything not claimed above). Mandatory.",
        },
    ]
}

/// The Configuration catch-all category.
pub fn configuration() -> &'static Category {
    catalog()
        .iter()
        .find(|c| c.kind == CategoryKind::Configuration)
        .expect("catalog always contains Configuration")
}

/// Return the id of the category a table belongs to (first non-config match,
/// else Configuration).
pub fn category_for_table(table: &str) -> &'static str {
    for c in catalog() {
        if c.kind == CategoryKind::Configuration {
            continue;
        }
        if c.tables.iter().any(|m| m.matches(table)) {
            return c.id;
        }
    }
    configuration().id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_is_last_and_locked() {
        let last = catalog().last().unwrap();
        assert_eq!(last.kind, CategoryKind::Configuration);
        assert!(last.locked);
    }

    #[test]
    fn known_tables_route_correctly() {
        assert_eq!(category_for_table("ts_kv"), "telemetry_historical");
        assert_eq!(category_for_table("attribute_kv"), "attributes");
        assert_eq!(category_for_table("alarm_comment_2026_07"), "alarms");
        assert_eq!(category_for_table("audit_log_2026_07"), "audit_event_logs");
        assert_eq!(category_for_table("report_2026"), "reports");
        assert_eq!(category_for_table("license_state"), "licensing");
        // config rules must NOT be swallowed by the notifications history category
        assert_eq!(category_for_table("notification_rule"), "configuration");
        assert_eq!(category_for_table("notification"), "notifications");
        assert_eq!(category_for_table("report_template"), "configuration");
        // unknown/new table → configuration catch-all
        assert_eq!(category_for_table("some_future_table"), "configuration");
        assert_eq!(category_for_table("device"), "configuration");
    }
}
