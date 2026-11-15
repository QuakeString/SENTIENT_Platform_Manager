// Copyright © 2016-2026 The SENTIENT Authors

//! Release notes shown before an update ("What's new"): a release's body
//! read into its summary and its Added / Changed / Fixed lists, and the line
//! that says what the release does to the database.
//!
//! Bodies are made from each repository's CHANGELOG.md
//! (`scripts/release-notes.py`): a summary paragraph, optional `Built from`
//! and `Database:` lines, then `###` sections of `- ` entries, then the
//! downloads (left out here). The same parser reads a CHANGELOG section, so
//! the Platform Manager's own notes travel inside the program
//! ([`own`]) and SENTIENT's inside an offline package (`NOTES.md`,
//! [`bundled`]), for computers without internet.

use std::path::Path;

use sentient_installer_core::releases::{self, Release};

/// The Platform Manager's changelog, built into the program.
const OWN_CHANGELOG: &str = include_str!("../../CHANGELOG.md");

/// One version's notes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Notes {
    pub version: String,
    /// `2026-11-14`, or empty.
    pub date: String,
    /// The one-line summary.
    pub summary: String,
    /// `("Added", entries)`, in the order they appear.
    pub sections: Vec<(String, Vec<String>)>,
    /// What the release does to the database, from its `Database:` line.
    pub database: Option<String>,
}

impl Notes {
    /// Read a release body or a CHANGELOG section (without its `##` heading).
    pub fn parse(version: &str, date: &str, body: &str) -> Notes {
        let mut notes = Notes { version: version.into(), date: date.into(), ..Default::default() };
        let mut summary: Vec<String> = Vec::new();
        let mut summary_done = false;
        let mut current: Option<(String, Vec<String>)> = None;
        for raw in body.lines() {
            let line = raw.trim_end();
            let trimmed = line.trim();
            if let Some(h) = trimmed.strip_prefix("### ") {
                if let Some(s) = current.take() {
                    notes.push_section(s);
                }
                let name = h.trim();
                // Downloads, checksums and the like are not news.
                current = KNOWN.contains(&name).then(|| (name.to_string(), Vec::new()));
                summary_done = true;
                continue;
            }
            if trimmed.starts_with("## ") || trimmed.starts_with("# ") {
                summary_done = true;
                continue;
            }
            if let Some(db) = trimmed.strip_prefix("Database:") {
                notes.database = Some(plain(db.trim()));
                continue;
            }
            match &mut current {
                Some((_, entries)) => {
                    if let Some(entry) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
                        entries.push(entry.to_string());
                    } else if !trimmed.is_empty() && raw.starts_with(' ') {
                        // A wrapped entry continues on an indented line.
                        if let Some(last) = entries.last_mut() {
                            last.push(' ');
                            last.push_str(trimmed);
                        }
                    }
                }
                None if !summary_done => {
                    if trimmed.is_empty() {
                        if !summary.is_empty() {
                            summary_done = true;
                        }
                    } else if !trimmed.starts_with("Built from") {
                        summary.push(trimmed.to_string());
                    }
                }
                None => {}
            }
        }
        if let Some(s) = current.take() {
            notes.push_section(s);
        }
        // Always Added, Changed, Fixed, … whatever order the file used.
        notes.sections.sort_by_key(|(name, _)| KNOWN.iter().position(|k| k == name).unwrap_or(KNOWN.len()));
        notes.summary = plain(&summary.join(" "));
        notes
    }

    fn push_section(&mut self, (name, entries): (String, Vec<String>)) {
        let entries: Vec<String> = entries.iter().map(|e| plain(&drop_links(e))).filter(|e| !e.is_empty()).collect();
        if !entries.is_empty() {
            self.sections.push((name, entries));
        }
    }

    pub fn from_release(r: &Release, version: &str) -> Notes {
        Notes::parse(version, &r.date(), &r.notes())
    }

    /// Whether there is more than the summary to show.
    pub fn has_entries(&self) -> bool {
        !self.sections.is_empty()
    }
}

const KNOWN: [&str; 6] = ["Added", "Changed", "Fixed", "Removed", "Security", "Deprecated"];

/// What updating over `notes` (newest first, every version that would be
/// skipped) does to the database: the first version that says it changes
/// it, else whether every one says it does not.
pub fn database_line(notes: &[Notes]) -> String {
    let changes = notes.iter().find(|n| {
        n.database.as_deref().is_some_and(|d| {
            let d = d.to_lowercase();
            !(d.starts_with("no change") || d.starts_with("none") || d.starts_with("unchanged"))
        })
    });
    if let Some(n) = changes {
        return format!("Database: changed by {} ({}).", n.version, n.database.clone().unwrap_or_default().trim_end_matches('.'));
    }
    if !notes.is_empty() && notes.iter().all(|n| n.database.is_some()) {
        return if notes.len() == 1 { "Database: no changes in this version.".into() } else { "Database: no changes in these versions.".into() };
    }
    "The notes do not say whether the database changes.".into()
}

/// The releases between `installed` (left out) and `target` (included),
/// newest first: what an update to `target` brings. `version_of` names a
/// release's version (SENTIENT: its tag; Platform Manager: without `spm-v`).
pub fn between(all: &[Release], installed: &str, target: &str, belongs: impl Fn(&Release) -> bool, version_of: impl Fn(&Release) -> String) -> Vec<Notes> {
    let mut found: Vec<(String, &Release)> = all
        .iter()
        .filter(|r| !r.draft && !r.is_nightly() && belongs(r))
        .map(|r| (version_of(r), r))
        .filter(|(v, _)| releases::compare(v, installed).is_gt() && releases::compare(v, target).is_le())
        .collect();
    found.sort_by(|a, b| releases::compare(&b.0, &a.0));
    found.into_iter().map(|(v, r)| Notes::from_release(r, &v)).collect()
}

/// Sections of a Keep-a-Changelog file: `(version, date, body)`, file order.
fn changelog_sections(text: &str) -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("## [") {
            if let Some((version, after)) = rest.split_once(']') {
                let date = after.trim().trim_start_matches(['-', '–', '—']).trim().to_string();
                out.push((version.to_string(), date, String::new()));
                continue;
            }
        }
        if let Some((_, _, body)) = out.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    out
}

/// The notes of every version in `changelog` newer than `installed` up to
/// `target`, newest first.
pub fn from_changelog(changelog: &str, installed: &str, target: &str) -> Vec<Notes> {
    changelog_sections(changelog)
        .into_iter()
        .filter(|(v, _, _)| v != "Unreleased")
        .filter(|(v, _, _)| releases::compare(v, installed).is_gt() && releases::compare(v, target).is_le())
        .map(|(v, d, b)| Notes::parse(&v, &d, &b))
        .collect()
}

/// The Platform Manager's notes from `installed` up to this program,
/// from the changelog built into it.
pub fn own(installed: &str) -> Vec<Notes> {
    let this = crate::updates::manager_version();
    let mut notes = from_changelog(OWN_CHANGELOG, installed, this);
    // A build made before its release section was written (a nightly, a
    // test build) carries its changes under Unreleased.
    if !notes.iter().any(|n| n.version == this) {
        if let Some((_, _, body)) = changelog_sections(OWN_CHANGELOG).into_iter().find(|(v, _, _)| v == "Unreleased") {
            let n = Notes::parse(this, "", &body);
            if n.has_entries() {
                notes.insert(0, n);
            }
        }
    }
    notes
}

/// SENTIENT's notes carried by an offline package: `NOTES.md` beside
/// `VERSION`, either one version's section or a changelog.
pub fn bundled(package: &Path, installed: &str, target: &str) -> Vec<Notes> {
    let Ok(text) = std::fs::read_to_string(package.join("NOTES.md")) else { return Vec::new() };
    if text.contains("## [") {
        return from_changelog(&text, installed, target);
    }
    vec![Notes::parse(target, "", &text)]
}

/// `([#160](https://…))` and `[text](url)` links as text; a trailing
/// parenthesis that held only links goes.
fn drop_links(s: &str) -> String {
    let mut out = s.to_string();
    // "( [#12](…) )" at the end, as the changelogs write PR links.
    if let Some(i) = out.rfind(" ([#") {
        if out.ends_with("))") || out.ends_with(").") {
            let tail = &out[i..];
            if tail.matches("[#").count() >= 1 && tail.trim_end_matches('.').ends_with(')') {
                let dot = out.ends_with('.');
                out.truncate(i);
                if dot {
                    out.push('.');
                }
            }
        }
    }
    out
}

/// Markdown links, emphasis and code as plain text.
fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('[') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find("](").and_then(|j| after[j + 2..].find(')').map(|k| (j, j + 2 + k))) {
            Some((j, end)) => {
                out.push_str(&after[..j]);
                rest = &after[end + 1..];
            }
            None => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.replace("**", "").replace('`', "").split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = "Licence dialogs and editor fixes.\n\nBuilt from master `3c37a1db`.\n\nDatabase: no changes.\n\n### Added\n- Licence key limits show in the\n  licence dialog ([#230](https://x/pull/230)).\n\n### Fixed\n- Dashboard editor keeps the selection after undo.\n\n### Downloads\n| file | size |\n";

    #[test]
    fn a_release_body_reads_into_sections() {
        let n = Notes::parse("4.4.0.2", "2026-11-14", BODY);
        assert_eq!(n.summary, "Licence dialogs and editor fixes.");
        assert_eq!(n.database.as_deref(), Some("no changes."));
        assert_eq!(n.sections.len(), 2);
        assert_eq!(n.sections[0].0, "Added");
        assert_eq!(n.sections[0].1, vec!["Licence key limits show in the licence dialog.".to_string()]);
        assert_eq!(n.sections[1].1, vec!["Dashboard editor keeps the selection after undo.".to_string()]);
        let order = Notes::parse("x", "", "S.\n\n### Fixed\n- f\n\n### Changed\n- c\n\n### Added\n- a\n");
        let names: Vec<&str> = order.sections.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["Added", "Changed", "Fixed"]);
    }

    #[test]
    fn the_database_line_names_the_version_that_changes_it() {
        let a = Notes::parse("4.4.0.2", "", BODY);
        let b = Notes::parse("4.4.0.1", "", "New UI.\n\nDatabase: one-time change, about 1 minute.\n");
        assert_eq!(database_line(&[a.clone()]), "Database: no changes in this version.");
        assert_eq!(database_line(&[a.clone(), b]), "Database: changed by 4.4.0.1 (one-time change, about 1 minute).");
        let c = Notes::parse("4.4.0.0", "", "No line.\n");
        assert_eq!(database_line(&[a, c]), "The notes do not say whether the database changes.");
    }

    #[test]
    fn the_changelog_gives_every_version_after_the_installed_one() {
        let log = "# Changelog\n\n## [Unreleased]\n\n### Fixed\n- y\n\n## [0.8.0-rc12] - 2026-11-15\n\nClearer updates.\n\n### Added\n- What's new.\n\n## [0.8.0-rc11] - 2026-11-14\n\nOlder.\n\n### Fixed\n- x\n\n## [0.8.0-rc10] - 2026-11-13\n\nOldest.\n";
        let n = from_changelog(log, "0.8.0-rc10", "0.8.0-rc12");
        let v: Vec<&str> = n.iter().map(|n| n.version.as_str()).collect();
        assert_eq!(v, ["0.8.0-rc12", "0.8.0-rc11"]);
        assert_eq!(n[0].date, "2026-11-15");
        assert_eq!(n[0].summary, "Clearer updates.");
    }

    #[test]
    fn the_built_in_changelog_parses() {
        assert!(!changelog_sections(OWN_CHANGELOG).is_empty());
    }
}
