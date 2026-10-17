// Copyright © 2016-2026 The SENTIENT Authors

//! Changing an installation's settings after install: the catalog's SENTIENT
//! environment settings (crate::catalog) and PostgreSQL's parameters.
//!
//! The values live in the installation record: SENTIENT settings in
//! `stack.tuning.extra_env`, which every engine writes last so it wins over
//! derived values, and PostgreSQL's in `stack.pg`. Applying writes the
//! engine's configuration, restarts what changed and waits for SENTIENT to
//! answer; if it does not within three minutes, the previous settings are
//! applied again.

use std::collections::HashMap;

use sentient_installer_core::distro::{DeployConfig, docker_ops};
use sentient_installer_core::native;
use sentient_installer_core::progress::{Progress, ProgressFn};
use sentient_installer_core::stack::{EnvVar, PgTuning};

use crate::catalog::{self, PG_PREFIX};
use crate::installation::{Engine, InstallationRecord};

const LICENSE_SERVER_URL: &str = "LICENSE_SERVER_URL";

/// The values set on this installation, by catalog name (`pg:` names for
/// PostgreSQL). Settings left at their default are absent.
pub fn current(record: &InstallationRecord) -> HashMap<String, String> {
    let st = &record.stack;
    let mut values: HashMap<String, String> =
        st.tuning.extra_env.iter().map(|e| (e.name.clone(), e.value.clone())).collect();
    for (name, value) in st.pg.settings() {
        values.insert(format!("{PG_PREFIX}{name}"), value);
    }
    if !st.license_server_url.is_empty() {
        values.insert(LICENSE_SERVER_URL.into(), st.license_server_url.clone());
    }
    values
}

/// `stack` with the catalog settings replaced by `values` (name → value; a
/// missing or empty value means the default). Variables outside the catalog
/// that were already set are kept.
pub fn with_values(stack: &DeployConfig, values: &HashMap<String, String>) -> DeployConfig {
    let catalog: Vec<String> = catalog::groups().into_iter().flat_map(|g| g.settings).map(|s| s.env).collect();
    let set = |name: &str| values.get(name).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let mut st = stack.clone();
    st.tuning.extra_env.retain(|e| !catalog.contains(&e.name));
    let pg_default = PgTuning::default();
    st.pg = PgTuning {
        pg_max_connections: set("pg:max_connections").and_then(|v| v.parse().ok()).unwrap_or(pg_default.pg_max_connections),
        pg_shared_buffers: set("pg:shared_buffers").unwrap_or(pg_default.pg_shared_buffers),
        pg_work_mem: set("pg:work_mem").unwrap_or(pg_default.pg_work_mem),
        pg_effective_cache_size: set("pg:effective_cache_size").unwrap_or(pg_default.pg_effective_cache_size),
    };
    st.license_server_url = set(LICENSE_SERVER_URL).unwrap_or_else(|| DeployConfig::default().license_server_url);
    for name in &catalog {
        if name.starts_with(PG_PREFIX) || name == LICENSE_SERVER_URL {
            continue;
        }
        if let Some(value) = set(name) {
            st.tuning.extra_env.push(EnvVar { name: name.clone(), value });
        }
    }
    st
}

/// Values SENTIENT would not understand. It falls back to its default
/// without saying so, which would look applied while doing nothing: numbers
/// where the default is a number, true/false where it is one of those, and
/// ports in range. Returns one message per problem.
pub fn check(values: &HashMap<String, String>) -> Vec<String> {
    let mut problems = Vec::new();
    for setting in catalog::groups().into_iter().flat_map(|g| g.settings) {
        let Some(value) = values.get(&setting.env).map(|v| v.trim()).filter(|v| !v.is_empty()) else { continue };
        let default = setting.default.as_str();
        let is_port = setting.env.ends_with("_PORT");
        if is_port {
            if !value.parse::<u16>().is_ok_and(|p| p > 0) {
                problems.push(format!("{}: a port number from 1 to 65535", setting.label));
            }
        } else if !default.is_empty() && default.chars().all(|c| c.is_ascii_digit()) {
            if value.parse::<u64>().is_err() {
                problems.push(format!("{}: a whole number", setting.label));
            }
        } else if matches!(default, "true" | "false") && !matches!(value, "true" | "false") {
            problems.push(format!("{}: true or false", setting.label));
        }
    }
    problems
}

/// What applying `new` over `old` restarts, for the confirmation.
pub fn restarts(old: &DeployConfig, new: &DeployConfig) -> Option<&'static str> {
    if old == new {
        None
    } else if old.pg != new.pg {
        Some("PostgreSQL and SENTIENT restart")
    } else {
        Some("SENTIENT restarts")
    }
}

fn reconfigure(record: &InstallationRecord, pg_changed: bool, sink: &ProgressFn) -> Result<(), String> {
    match record.engine {
        Engine::Native => native::reconfigure(sink, &record.native_config_with_secrets()?, pg_changed),
        Engine::Docker => docker_ops::reconfigure(sink, &docker_ops::with_secrets(&record.stack)?),
    }
}

fn answers(port: u16) -> bool {
    for _ in 0..90 {
        if native::http_answers(port) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    false
}

/// Apply `values` to the installation. Returns the record as saved.
pub fn apply(record: InstallationRecord, values: &HashMap<String, String>, sink: ProgressFn) -> Result<InstallationRecord, String> {
    let problems = check(values);
    if !problems.is_empty() {
        return Err(format!("Some values are not valid:\n{}", problems.join("\n")));
    }
    let old = record.clone();
    let mut new = record;
    new.stack = with_values(&old.stack, values);
    if new.stack == old.stack {
        sink(Progress::Done { message: "Nothing changed.".into() });
        return Ok(new);
    }
    // The record holds no passwords: validate with them filled in.
    if new.engine == Engine::Docker {
        docker_ops::with_secrets(&new.stack)?.validate()?;
    }
    let pg_changed = old.stack.pg != new.stack.pg;

    let failure = match reconfigure(&new, pg_changed, &sink) {
        Err(e) => Some(format!("The new settings could not be applied ({e})")),
        Ok(()) => {
            sink(Progress::Step { name: "Waiting for SENTIENT to answer".into() });
            (!answers(new.stack.http_port)).then(|| "SENTIENT did not start with the new settings".to_string())
        }
    };
    if let Some(why) = failure {
        sink(Progress::Log { line: format!("{why}; putting the previous ones back") });
        reconfigure(&old, pg_changed, &sink)?;
        let back = if answers(old.stack.http_port) { "it runs with the previous settings again" } else { "the previous settings are back, but SENTIENT is not answering yet; check Status and Logs" };
        return Err(format!("{why}; {back}."));
    }
    let json = serde_json::to_string_pretty(&new).map_err(|e| e.to_string())?;
    crate::self_install::write_record(&crate::installation::record_path(), &json)?;
    sink(Progress::Done { message: "The new settings are in effect.".into() });
    Ok(new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_values_sentient_would_ignore() {
        let mut values = HashMap::new();
        values.insert("DB_POOL_PRIMARY_MAX".to_string(), "not-a-number".to_string());
        values.insert("EDGE_GRPC_PORT".to_string(), "70000".to_string());
        values.insert("SECURITY_CLAIM_ALLOW_CLAIMING_BY_DEFAULT".to_string(), "yes".to_string());
        assert_eq!(check(&values).len(), 3);
        values.clear();
        values.insert("DB_POOL_PRIMARY_MAX".to_string(), "64".to_string());
        values.insert("pg:shared_buffers".to_string(), "2GB".to_string());
        assert!(check(&values).is_empty());
    }

    #[test]
    fn values_round_trip_through_the_stack() {
        let mut stack = DeployConfig::default();
        stack.tuning.extra_env.push(EnvVar { name: "NOT_IN_CATALOG".into(), value: "kept".into() });
        let mut values = HashMap::new();
        values.insert("DB_POOL_PRIMARY_MAX".to_string(), "64".to_string());
        values.insert("pg:work_mem".to_string(), "32MB".to_string());
        let st = with_values(&stack, &values);
        assert!(st.tuning.extra_env.iter().any(|e| e.name == "DB_POOL_PRIMARY_MAX" && e.value == "64"));
        assert!(st.tuning.extra_env.iter().any(|e| e.name == "NOT_IN_CATALOG"));
        assert_eq!(st.pg.pg_work_mem, "32MB");
        assert_eq!(restarts(&stack, &st), Some("PostgreSQL and SENTIENT restart"));

        // Clearing a value returns it to the default.
        let st2 = with_values(&st, &HashMap::new());
        assert!(!st2.tuning.extra_env.iter().any(|e| e.name == "DB_POOL_PRIMARY_MAX"));
        assert_eq!(st2.pg, PgTuning::default());
    }
}
