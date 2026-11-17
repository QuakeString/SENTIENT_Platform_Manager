// Copyright © 2016-2026 The SENTIENT Authors

//! The open-source components the Platform Manager is built from, for
//! About > Third-party licences: assets/third_party_licences.json, made by
//! scripts/third-party-licences.py from `cargo metadata` (checked in: cargo
//! cannot be run from inside a build). The test below fails when Cargo.lock
//! changed and the list was not made again.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Component {
    pub name: String,
    #[serde(default)]
    pub version: String,
    pub license: String,
    #[serde(default)]
    pub repository: String,
}

#[derive(Debug, Deserialize)]
struct List {
    /// Cargo.lock's fingerprint when the list was made (the test).
    #[cfg_attr(not(test), allow(dead_code))]
    cargo_lock: String,
    crates: Vec<Component>,
    assets: Vec<Component>,
}

const JSON: &str = include_str!("../assets/third_party_licences.json");

fn list() -> &'static List {
    static LIST: std::sync::OnceLock<List> = std::sync::OnceLock::new();
    LIST.get_or_init(|| serde_json::from_str(JSON).expect("assets/third_party_licences.json"))
}

/// The bundled files that are not crates (icons, logos, the terminal
/// font), then every crate by name.
pub fn components() -> impl Iterator<Item = &'static Component> {
    list().assets.iter().chain(list().crates.iter())
}

/// The registry and git packages of a Cargo.lock as sorted "name version"
/// lines (path packages, the app's own among them, left out). The same
/// rule as the script's lock_fingerprint.
#[cfg(test)]
fn lock_fingerprint(text: &str) -> Vec<String> {
    let text = text.replace('\r', "");
    let mut out: Vec<String> = text
        .split("[[package]]")
        .skip(1)
        .filter_map(|block| {
            let field = |k: &str| block.lines().find_map(|l| l.strip_prefix(&format!("{k} = ")).map(|v| v.trim().trim_matches('"').to_string()));
            field("source")?;
            Some(format!("{} {}", field("name")?, field("version")?))
        })
        .collect();
    out.sort();
    out
}

#[cfg(test)]
fn fnv1a(lines: &[String]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in lines.join("\n").bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_matches_cargo_lock() {
        let lock = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.lock")).unwrap();
        assert_eq!(
            fnv1a(&lock_fingerprint(&lock)),
            list().cargo_lock,
            "Cargo.lock changed: run scripts/third-party-licences.py and commit app/assets/third_party_licences.json"
        );
    }

    #[test]
    fn every_component_has_a_licence() {
        assert!(list().crates.len() > 100);
        for c in components() {
            assert!(!c.name.is_empty() && !c.license.is_empty(), "{c:?}");
        }
    }
}
