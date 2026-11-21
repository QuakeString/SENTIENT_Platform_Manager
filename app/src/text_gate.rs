// Copyright © 2016-2026 The SENTIENT Authors

//! The text gate: nothing the Platform Manager shows names the gateway's
//! legacy (pre-SENTIENT) file or variable spellings or the upstream
//! product. A source scan of the lines that build what a person reads:
//! job steps, log lines and results, errors, the page's facts, notices and
//! help. Code that only reads those spellings (scripts, parsers, variable
//! lists) is not user text and does not match the markers.

use std::path::{Path, PathBuf};

/// What a line that builds user-facing text contains.
const MARKERS: &[&str] = &["Progress::Step", "Progress::Log", "Progress::Done", "Err(format!(", "Err(\"", "ui::fact", "notice(", "help: Some(", "desc: Some(", "host_source = Some(", ".why = ", "note: Some("];
/// What must never be in such text.
fn banned() -> Vec<String> {
    vec![["tb", "_"].concat(), ["TB", "_"].concat(), ["Things", "Board"].concat(), ["things", "board"].concat()]
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).expect("read dir").flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// The string literals on a line.
fn literals(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    let mut esc = false;
    for c in line.chars() {
        match (&mut cur, c) {
            (Some(s), _) if esc => {
                s.push(c);
                esc = false;
            }
            (Some(_), '\\') => esc = true,
            (Some(s), '"') => {
                out.push(std::mem::take(s));
                cur = None;
            }
            (Some(s), _) => s.push(c),
            (None, '"') => cur = Some(String::new()),
            _ => {}
        }
    }
    out
}

#[test]
fn no_legacy_names_in_user_text() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    rust_files(&root.join("../crates/sentient-installer-core/src"), &mut files);
    let banned = banned();
    let mut found = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).expect("read");
        // Tests hold fixtures in the old spellings on purpose.
        let body = text.split("#[cfg(test)]").next().unwrap_or(&text);
        for (n, line) in body.lines().enumerate() {
            let t = line.trim_start();
            if t.starts_with("//") || !MARKERS.iter().any(|m| line.contains(m)) {
                continue;
            }
            for lit in literals(line) {
                if banned.iter().any(|b| lit.contains(b.as_str())) {
                    found.push(format!("{}:{}: {}", f.display(), n + 1, lit));
                }
            }
        }
    }
    assert!(found.is_empty(), "user text names a legacy spelling:\n{}", found.join("\n"));
}

#[test]
fn the_card_never_shows_the_legacy_file_name() {
    let legacy = ["/sentient_gateway/config/", "tb", "_gateway.json"].concat();
    let shown = crate::manager::gateway::shown_config_path(&legacy);
    assert!(!shown.contains(&["tb", "_"].concat()), "{shown}");
    assert_eq!(crate::manager::gateway::shown_config_path("/etc/sentient-gateway/st_gateway.json"), "/etc/sentient-gateway/st_gateway.json");
}
