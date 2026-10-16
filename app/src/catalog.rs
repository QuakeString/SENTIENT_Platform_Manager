// Copyright © 2016-2026 The SENTIENT Authors

//! The settings catalog (`assets/settings_catalog.json`): every SENTIENT
//! environment setting the installer's Customize page and the manager's
//! Settings screen offer. One file, so a new SENTIENT variable is added in
//! one place.

use std::collections::HashMap;

use sentient_installer_core::stack::{self, PgTuning, Tuning};
use serde::Deserialize;

const CATALOG: &str = include_str!("../assets/settings_catalog.json");

/// Settings with this prefix are PostgreSQL server parameters, not service
/// environment.
pub const PG_PREFIX: &str = "pg:";

#[derive(Debug, Clone, Deserialize)]
pub struct Group {
    pub name: String,
    pub settings: Vec<Setting>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Setting {
    pub env: String,
    pub label: String,
    /// SENTIENT's built-in value; empty when it has none.
    #[serde(default)]
    pub default: String,
    /// What the installer writes under the recommended configuration.
    #[serde(default)]
    pub recommended: Option<String>,
    pub help: String,
}

#[derive(Deserialize)]
struct Catalog {
    groups: Vec<Group>,
}

pub fn groups() -> Vec<Group> {
    let mut groups = serde_json::from_str::<Catalog>(CATALOG)
        .expect("settings_catalog.json is valid")
        .groups;
    // Show the value that will really apply: where the install engine writes
    // a value of its own, that value replaces SENTIENT's built-in default.
    let engine = engine_defaults();
    for setting in groups.iter_mut().flat_map(|g| g.settings.iter_mut()) {
        if let Some(value) = engine.get(&setting.env) {
            setting.default = value.clone();
        }
    }
    groups
}

/// The values the engine writes when nothing is overridden.
fn engine_defaults() -> HashMap<String, String> {
    let mut map: HashMap<String, String> = stack::tuning_env(&Tuning::default()).into_iter().collect();
    let pg = PgTuning::default();
    map.insert(format!("{PG_PREFIX}max_connections"), pg.pg_max_connections.to_string());
    map.insert(format!("{PG_PREFIX}shared_buffers"), pg.pg_shared_buffers);
    map.insert(format!("{PG_PREFIX}work_mem"), pg.pg_work_mem);
    map.insert(format!("{PG_PREFIX}effective_cache_size"), pg.pg_effective_cache_size);
    map
}

/// The recommended configuration for a machine with `ram_mb` of memory:
/// the catalog's recommendations plus PostgreSQL sized to the RAM
/// (shared_buffers a quarter, effective_cache_size three quarters).
pub fn recommended(groups: &[Group], ram_mb: u64) -> HashMap<String, String> {
    let mut map: HashMap<String, String> = groups
        .iter()
        .flat_map(|g| g.settings.iter())
        .filter_map(|s| s.recommended.clone().map(|v| (s.env.clone(), v)))
        .collect();
    if ram_mb >= 2048 {
        map.insert(format!("{PG_PREFIX}shared_buffers"), format!("{}MB", ram_mb / 4));
        map.insert(format!("{PG_PREFIX}effective_cache_size"), format!("{}MB", ram_mb * 3 / 4));
        map.insert(format!("{PG_PREFIX}work_mem"), "16MB".into());
        map.insert(format!("{PG_PREFIX}max_connections"), "200".into());
    }
    map
}

pub fn total_ram_mb() -> u64 {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    sys.total_memory() / 1024 / 1024
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_parses_and_names_are_unique() {
        let groups = groups();
        assert!(!groups.is_empty());
        let mut seen = std::collections::HashSet::new();
        for g in &groups {
            for s in &g.settings {
                assert!(seen.insert(s.env.clone()), "duplicate setting {}", s.env);
            }
        }
    }

    #[test]
    fn shows_the_engine_value_where_the_engine_writes_one() {
        let groups = groups();
        let find = |env: &str| {
            groups.iter().flat_map(|g| g.settings.iter()).find(|s| s.env == env).unwrap().default.clone()
        };
        // SENTIENT's own default is 16; the engine writes 48.
        assert_eq!(find("DB_POOL_PRIMARY_MAX"), "48");
        // The engine leaves this one alone, so SENTIENT's default shows.
        assert_eq!(find("RULE_ENGINE_CONSUMERS"), "4");
    }
}
