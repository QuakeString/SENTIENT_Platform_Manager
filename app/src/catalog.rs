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
    // In the language shown (its texts are in the catalogue, tools/i18n).
    let tr = |s: &mut String| *s = crate::i18n::dynamic(s).into_owned();
    for g in groups.iter_mut() {
        tr(&mut g.name);
        for s in g.settings.iter_mut() {
            tr(&mut s.label);
            tr(&mut s.help);
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

/// Where the databases' memory comes from: what PostgreSQL is sized for,
/// and the WSL 2 VM's memory where a database runs in one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sizing {
    /// The computer's memory.
    pub host_mb: u64,
    /// The memory PostgreSQL's recommendation is based on.
    pub pg_mb: u64,
    /// The WSL 2 VM's memory, where a database runs in WSL 2 (Windows:
    /// Docker, or ClickHouse beside PostgreSQL "in the system").
    pub vm_mb: Option<u64>,
    /// The VM's memory is .wslconfig's `memory=` (not WSL 2's default half).
    pub vm_from_wslconfig: bool,
}

/// How the recommendation is sized. Windows Docker: SENTIENT and its
/// databases run in the WSL 2 VM, which gets `vm_mb` (half the computer's
/// memory unless .wslconfig says otherwise), so PostgreSQL is sized for
/// that. Windows "in the system" with ClickHouse: ClickHouse runs in its
/// own WSL 2 VM (`vm_mb`, its account's .wslconfig sets no memory), so
/// PostgreSQL is sized for what that leaves. Otherwise the computer's.
pub fn sizing(windows: bool, docker: bool, clickhouse: bool, host_mb: u64, vm_mb: u64, vm_from_wslconfig: bool) -> Sizing {
    let (pg_mb, vm) = match (windows, docker, clickhouse) {
        (true, true, _) => (vm_mb, Some(vm_mb)),
        (true, false, true) => (host_mb.saturating_sub(vm_mb), Some(vm_mb)),
        _ => (host_mb, None),
    };
    Sizing { host_mb, pg_mb, vm_mb: vm, vm_from_wslconfig: vm.is_some() && vm_from_wslconfig }
}

/// The WSL 2 VM's memory for SENTIENT on this computer: Docker runs in this
/// account's VM (its .wslconfig counts); ClickHouse "in the system" in its
/// hidden account's, whose .wslconfig sets no memory. (memory MB, from
/// .wslconfig).
pub fn wsl_vm_mb(host_mb: u64, docker: bool) -> (u64, bool) {
    use sentient_installer_core::wsl;
    let config = if docker && cfg!(windows) { wsl::account_wslconfig() } else { None };
    let from_config = config.as_deref().and_then(wsl::wslconfig_memory_mb).is_some();
    (wsl::vm_memory_mb(host_mb, config.as_deref()), from_config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_docker_is_sized_for_the_wsl_vm() {
        // 16 GB, the VM gets the default half.
        let d = sizing(true, true, false, 16384, 8192, false);
        assert_eq!((d.pg_mb, d.vm_mb), (8192, Some(8192)));
        assert_eq!(recommended(&[], d.pg_mb)["pg:shared_buffers"], "2048MB");
        // .wslconfig's memory=12GB.
        let d = sizing(true, true, true, 16384, 12288, true);
        assert_eq!(d.pg_mb, 12288);
        assert!(d.vm_from_wslconfig);
        // Native with ClickHouse in WSL 2: PostgreSQL gets what the VM leaves.
        let h = sizing(true, false, true, 16384, 8192, false);
        assert_eq!((h.pg_mb, h.vm_mb), (8192, Some(8192)));
        // Native PostgreSQL only, and Linux: the computer's memory.
        assert_eq!(sizing(true, false, false, 16384, 8192, true), Sizing { host_mb: 16384, pg_mb: 16384, vm_mb: None, vm_from_wslconfig: false });
        assert_eq!(sizing(false, true, true, 16384, 8192, false).pg_mb, 16384);
    }

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

/// How a setting is edited, from its name, label and built-in value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    /// On or off.
    Switch,
    /// A file on the computer that runs SENTIENT (certificates, keys).
    File,
    /// A whole number, with its unit when it has one.
    Number(Option<&'static str>),
    Text,
}

impl Setting {
    pub fn kind(&self) -> ValueKind {
        let env = self.env.as_str();
        if matches!(self.default.as_str(), "true" | "false") || env.ends_with("_ENABLED") {
            return ValueKind::Switch;
        }
        if env.contains("_PEM_") || env.ends_with("_FILE") {
            return ValueKind::File;
        }
        let unit = [("(ms)", "ms"), ("(s)", "s"), ("(min)", "min"), ("(days)", "days"), ("(MB)", "MB")]
            .into_iter()
            .find(|(tag, _)| self.label.ends_with(tag))
            .map(|(_, u)| u);
        let whole = !self.default.is_empty() && self.default.chars().all(|c| c.is_ascii_digit());
        let counted = ["_THREADS", "_PORT", "_SIZE", "_MAX", "_MIN", "_CONNECTIONS", "_WORKERS", "_CONSUMERS", "_COUNT"].iter().any(|s| env.ends_with(s));
        if unit.is_some() || whole || (counted && !env.starts_with("pg:")) {
            return ValueKind::Number(unit);
        }
        ValueKind::Text
    }

    /// The range a number setting takes: ports 1 to 65535, counts of
    /// threads, workers and connections at least 1, durations and sizes up
    /// to a bound no real value comes near (ten years, a terabyte).
    pub fn range(&self) -> (u64, u64) {
        let env = self.env.as_str();
        if env.ends_with("_PORT") {
            return (1, 65_535);
        }
        let unit = match self.kind() {
            ValueKind::Number(u) => u,
            _ => None,
        };
        match unit {
            Some("ms") => return (0, 315_360_000_000),
            Some("s") => return (0, 315_360_000),
            Some("min") => return (0, 5_256_000),
            Some("days") => return (0, 36_500),
            Some("MB") => return (0, 1_048_576),
            _ => {}
        }
        if ["_THREADS", "_WORKERS", "_CONSUMERS", "_CONNECTIONS", "_MAX"].iter().any(|s| env.ends_with(s)) {
            return (1, 1_000_000);
        }
        (0, u32::MAX as u64)
    }

    /// Why a typed value cannot be used, as the one line under its field
    /// ("Enter whole seconds from 0 to 315360000."); None when it can (an
    /// empty field is SENTIENT's own value). Numbers and switches only:
    /// text and files are taken as typed.
    pub fn problem(&self, value: &str) -> Option<String> {
        let v = value.trim();
        if v.is_empty() {
            return None;
        }
        match self.kind() {
            ValueKind::Number(unit) => {
                let (lo, hi) = self.range();
                let ok = v.chars().all(|c| c.is_ascii_digit()) && v.parse::<u64>().is_ok_and(|n| (lo..=hi).contains(&n));
                if ok {
                    return None;
                }
                let what = match unit {
                    Some("s") => "whole seconds",
                    Some("ms") => "whole milliseconds",
                    Some("min") => "whole minutes",
                    Some("days") => "whole days",
                    Some("MB") => "whole megabytes",
                    _ if self.env.ends_with("_PORT") => "a port number",
                    _ => "a whole number",
                };
                Some(tf!("Enter {what} from {lo} to {hi}."))
            }
            ValueKind::Switch => (!matches!(v, "true" | "false")).then(|| t!("Enter true or false.").to_string()),
            _ => None,
        }
    }

    /// The label without its unit, which the field shows after the number.
    pub fn short_label(&self) -> String {
        match self.kind() {
            ValueKind::Number(Some(u)) => self.label.trim_end_matches(&format!("({u})")).trim_end().to_string(),
            _ => self.label.clone(),
        }
    }
}

#[cfg(test)]
mod kind_tests {
    use super::*;

    #[test]
    fn every_setting_gets_a_fitting_field() {
        let all: Vec<Setting> = groups().into_iter().flat_map(|g| g.settings).collect();
        let kind = |env: &str| all.iter().find(|s| s.env == env).unwrap().kind();
        assert_eq!(kind("SECURITY_CLAIM_ALLOW_CLAIMING_BY_DEFAULT"), ValueKind::Switch);
        assert_eq!(kind("AUDIT_LOG_CLEANUP_ENABLED"), ValueKind::Switch);
        assert_eq!(kind("SSL_PEM_CERT"), ValueKind::File);
        assert_eq!(kind("COAP_SSL_PEM_KEY"), ValueKind::File);
        assert_eq!(kind("SECURITY_CLAIM_DURATION"), ValueKind::Number(Some("ms")));
        assert_eq!(kind("SQL_TS_BATCH_THREADS"), ValueKind::Number(None));
        assert_eq!(kind("EDGE_GRPC_PORT"), ValueKind::Number(None));
        assert_eq!(kind("pg:shared_buffers"), ValueKind::Text);
        assert_eq!(kind("RUST_LOG"), ValueKind::Text);
        assert_eq!(kind("LICENSE_SERVER_URL"), ValueKind::Text);
    }

    /// Numbers refuse what is not a whole number and what is out of range;
    /// every built-in value passes its own check.
    #[test]
    fn numeric_fields_refuse_words_and_out_of_range() {
        let all: Vec<Setting> = groups().into_iter().flat_map(|g| g.settings).collect();
        let s = |env: &str| all.iter().find(|s| s.env == env).unwrap().clone();
        let binding = s("LICENSE_BINDING_CHECK_INTERVAL_SECS");
        let msg = Some("Enter whole seconds from 0 to 315360000.".to_string());
        assert_eq!(binding.problem("jj"), msg);
        assert_eq!(binding.problem("12.5"), msg);
        assert_eq!(binding.problem("-5"), msg);
        assert_eq!(binding.problem("999999999999"), msg);
        assert_eq!(binding.problem("99999999999999999999999"), msg);
        assert_eq!(binding.problem(" 3600 "), None);
        assert_eq!(binding.problem(""), None);
        let port = s("EDGE_GRPC_PORT");
        assert_eq!(port.problem("0").as_deref(), Some("Enter a port number from 1 to 65535."));
        assert_eq!(port.problem("65536").as_deref(), Some("Enter a port number from 1 to 65535."));
        assert_eq!(port.problem("7070"), None);
        assert_eq!(s("SQL_TS_BATCH_THREADS").problem("0").as_deref(), Some("Enter a whole number from 1 to 1000000."));
        // Text and files are taken as typed.
        assert_eq!(s("LICENSE_SERVER_URL").problem("jj"), None);
        assert_eq!(s("RUST_LOG").problem("info,x=debug"), None);
        for st in &all {
            assert_eq!(st.problem(&st.default), None, "{} built-in {:?}", st.env, st.default);
        }
    }
}
