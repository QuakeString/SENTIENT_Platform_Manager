// Copyright © 2016-2026 The SENTIENT Authors

//! A Sentient Gateway the Platform Manager did not install but was asked to
//! manage ("Manage it here" on the Gateway page): recorded, not recreated.
//! It is kept as `gateway` in installation.json; on a computer without
//! SENTIENT (no record), in gateway.json next to where that record would be.
//! Removing the gateway clears it.

use serde::{Deserialize, Serialize};
use sentient_installer_core::gateway::Status;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GatewayRecord {
    /// docker, service, process or windows-service (Status::kind).
    pub kind: String,
    /// The container's or the unit's name (the program's path for a plain
    /// program).
    pub name: String,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub compose_project: Option<String>,
    #[serde(default)]
    pub compose_dir: Option<String>,
    #[serde(default)]
    pub volumes: Vec<String>,
    /// RFC 3339.
    pub taken_over_at: String,
}

impl GatewayRecord {
    pub fn of(s: &Status) -> Option<Self> {
        let name = s.container.clone().or_else(|| s.service.clone()).or_else(|| s.process.clone()).or_else(|| s.installed.then(|| "SentientGateway".to_string()))?;
        Some(GatewayRecord {
            kind: s.kind().to_string(),
            name,
            image: s.image.clone(),
            compose_project: s.compose_project.clone(),
            compose_dir: s.compose_dir.clone(),
            volumes: s.volumes.clone(),
            taken_over_at: chrono::Utc::now().to_rfc3339(),
        })
    }

    /// Whether this record is the gateway found as `s`.
    pub fn is(&self, s: &Status) -> bool {
        s.installed && self.kind == s.kind() && Self::of(s).is_some_and(|r| r.name == self.name)
    }
}

fn installation() -> std::path::PathBuf {
    crate::installation::record_path()
}

fn own_file() -> std::path::PathBuf {
    installation().with_file_name("gateway.json")
}

pub fn load() -> Option<GatewayRecord> {
    if let Some(v) = std::fs::read_to_string(installation()).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()) {
        if let Some(g) = v.get("gateway").filter(|g| !g.is_null()) {
            return serde_json::from_value(g.clone()).ok();
        }
    }
    std::fs::read_to_string(own_file()).ok().and_then(|t| serde_json::from_str(&t).ok())
}

/// Record `g` (None clears it). The installation record is edited as JSON,
/// so fields this version does not know stay.
pub fn save(g: Option<&GatewayRecord>) -> Result<(), String> {
    let path = installation();
    if let Some(mut v) = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()) {
        let obj = v.as_object_mut().ok_or(n!("The installation record is not a JSON object."))?;
        match g {
            Some(g) => {
                obj.insert("gateway".into(), serde_json::to_value(g).map_err(|e| e.to_string())?);
            }
            None => {
                if obj.remove("gateway").is_none() && !own_file().exists() {
                    return Ok(());
                }
            }
        }
        let json = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
        crate::self_install::write_record(&path, &json)?;
        if own_file().exists() {
            crate::self_install::remove_record(&own_file());
        }
        return Ok(());
    }
    match g {
        Some(g) => crate::self_install::write_record(&own_file(), &serde_json::to_string_pretty(g).map_err(|e| e.to_string())?),
        None => {
            if own_file().exists() {
                crate::self_install::remove_record(&own_file());
            }
            Ok(())
        }
    }
}

/// Manage the gateway found here (status()): record it as it is.
pub fn adopt() -> Result<String, String> {
    let s = sentient_installer_core::gateway::status();
    let g = GatewayRecord::of(&s).filter(|_| s.installed).ok_or(n!("There is no Sentient Gateway on this computer."))?;
    save(Some(&g))?;
    Ok(tf!("The Platform Manager now manages the gateway {}. Nothing about it was changed.", g.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_is_its_container() {
        let s = Status { installed: true, container: Some("st-gateway".into()), image: Some("quakestring/sentient-gateway-rs:latest".into()), compose_dir: Some("/home/mz/dfal".into()), ..Status::default() };
        let r = GatewayRecord::of(&s).unwrap();
        assert_eq!((r.kind.as_str(), r.name.as_str()), ("docker", "st-gateway"));
        assert!(r.is(&s));
        let other = Status { installed: true, container: Some("plant-gw".into()), ..Status::default() };
        assert!(!r.is(&other));
        assert!(!r.is(&Status::default()));
        assert!(GatewayRecord::of(&Status::default()).is_none());
    }
}
