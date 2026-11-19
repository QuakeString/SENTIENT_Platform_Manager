// Copyright © 2016-2026 The SENTIENT Authors

//! Whether a SENTIENT licence covers a SENTIENT build, by SENTIENT's own rule,
//! so the Platform Manager can say before an update whether the instance's
//! licence covers the version it goes to.
//!
//! The rule ([`version_prefix_matches`], [`license_covers_version`],
//! [`check_version_against`], [`VersionMismatch::message`]) is a copy of
//! SENTIENT's `api/src/services/license_verifier.rs` (origin/master), which
//! is itself the licence server's `_sentient_version_matches`. The three must
//! change together: the mirrored tests at the end of this file are SENTIENT's
//! own, verbatim.
//!
//! What is compared is SENTIENT's BUILD version (its `CARGO_PKG_VERSION`,
//! e.g. "4.4.0"), not the release tag ("4.4.0.3"): [`build_version`].

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// SENTIENT's rule (license_verifier.rs). Change both or neither.
// ---------------------------------------------------------------------------

/// Returns true iff `prefix` is a component-wise prefix of `full`.
///
/// Examples:
///   `("4.2", "4.2.5")` → true
///   `("4",   "4.99.0")` → true
///   `("4.2", "4.20.0")` → false (component-wise, not string-wise)
///   `("4.2", "4.3.0")` → false
pub fn version_prefix_matches(prefix: &str, full: &str) -> bool {
    let p: Vec<&str> = prefix.split('.').filter(|s| !s.is_empty()).collect();
    let f: Vec<&str> = full.split('.').filter(|s| !s.is_empty()).collect();
    if p.is_empty() || p.len() > f.len() {
        return false;
    }
    p.iter().zip(f.iter()).all(|(a, b)| a == b)
}

/// The one place that decides whether a licence covers a SENTIENT build:
/// at least one `allowed` entry is a component-wise prefix of `build`.
///
/// One deliberate difference from the licence server: an empty `allowed`
/// list. The server skips its check for it; SENTIENT refuses it in
/// [`check_version_against`] ("not version-bound").
pub fn license_covers_version(allowed: &[String], build: &str) -> bool {
    // The server refuses a blank build before comparing.
    !build.trim().is_empty() && allowed.iter().any(|p| version_prefix_matches(p, build))
}

/// Why a signed licence does not cover a build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionMismatch {
    /// The licence's allowed version prefixes (empty: not version-bound).
    pub allowed: Vec<String>,
    /// The build version compared.
    pub build: String,
}

impl VersionMismatch {
    /// What SENTIENT tells the operator.
    pub fn message(&self) -> String {
        if self.allowed.is_empty() {
            format!(
                "License is not version-bound. Please obtain a fresh license for SENTIENT {}.",
                self.build
            )
        } else {
            format!(
                "License covers SENTIENT version(s) [{}] but this build is {}. Please obtain a license that allows this version.",
                self.allowed.join(", "),
                self.build
            )
        }
    }
}

/// SENTIENT's check of a licence's allowed versions against a build: an
/// empty list is refused.
pub fn check_version_against(allowed: &[String], build: &str) -> Result<(), VersionMismatch> {
    if !allowed.is_empty() && license_covers_version(allowed, build) {
        Ok(())
    } else {
        Err(VersionMismatch { allowed: allowed.to_vec(), build: build.to_string() })
    }
}

// ---------------------------------------------------------------------------
// The build version of a release
// ---------------------------------------------------------------------------

/// The SENTIENT build version a release runs: `explicit` (the manifest's
/// `build_version`) when given, else the first three dot components of the
/// release version, without a pre-release suffix: "4.4.0.3" → "4.4.0",
/// "4.5.0.1-rc2" → "4.5.0", "v4.4.0-nightly.20261025" → "4.4.0".
pub fn build_version(explicit: Option<&str>, release_version: &str) -> String {
    if let Some(b) = explicit.map(str::trim).filter(|b| !b.is_empty()) {
        return b.to_string();
    }
    let v = release_version.trim().trim_start_matches('v');
    let numbers = v.split_once('-').map(|(n, _)| n).unwrap_or(v);
    numbers.split('.').filter(|p| !p.is_empty()).take(3).collect::<Vec<_>>().join(".")
}

/// "4.5" of "4.5.0": what a licence for that version names.
pub fn major_minor(build: &str) -> String {
    build.split('.').filter(|p| !p.is_empty()).take(2).collect::<Vec<_>>().join(".")
}

/// The allowed versions in a signed licence payload (SENTIENT's
/// `license.license_data`, the licence server's JSON): `allowed_versions`,
/// or its older names `version_majors` / `version_major` (SENTIENT's serde
/// aliases). An absent list is empty, as SENTIENT reads it. None: not a
/// licence payload.
pub fn allowed_in_payload(json: &str) -> Option<Vec<String>> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let obj = v.as_object()?;
    for key in ["allowed_versions", "version_majors", "version_major"] {
        if let Some(list) = obj.get(key) {
            let list = list.as_array()?;
            return list.iter().map(|e| e.as_str().map(str::to_string)).collect();
        }
    }
    Some(Vec::new())
}

// ---------------------------------------------------------------------------
// What the Platform Manager knows, and what Update does with it
// ---------------------------------------------------------------------------

/// An instance's licence, as far as versions go.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "allowed", rename_all = "snake_case")]
pub enum Licence {
    /// No licence is installed (nothing to lose by updating).
    NotInstalled,
    /// A licence for these version prefixes (may be empty: covers none).
    Allowed(Vec<String>),
}

/// What is known of the licence now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LicenceRead {
    /// Not read yet: a sign-in reads it.
    NotRead,
    /// The sign-in was cancelled or the licence could not be read (why).
    Failed(String),
    Read(Licence),
}

/// The label beside an offered version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    Covered,
    NeedsNew,
    NoLicence,
    NotChecked,
}

impl Coverage {
    pub fn label(self) -> &'static str {
        match self {
            Coverage::Covered => "Covered by your licence",
            Coverage::NeedsNew => "Needs a new licence",
            Coverage::NoLicence => "No licence installed",
            Coverage::NotChecked => "Licence not checked",
        }
    }
}

/// The label for going to `build`, by SENTIENT's rule (an empty list covers none).
pub fn coverage(read: &LicenceRead, build: &str) -> Coverage {
    match read {
        LicenceRead::NotRead | LicenceRead::Failed(_) => Coverage::NotChecked,
        LicenceRead::Read(Licence::NotInstalled) => Coverage::NoLicence,
        LicenceRead::Read(Licence::Allowed(a)) if check_version_against(a, build).is_ok() => Coverage::Covered,
        LicenceRead::Read(Licence::Allowed(_)) => Coverage::NeedsNew,
    }
}

/// What pressing Update does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    /// The licence is not known yet: the sign-in comes first.
    SignInFirst,
    /// Covered, or no licence installed: the normal confirmation.
    Confirm,
    /// Not covered (`allowed` empty: the licence lists no versions): the
    /// typed confirmation, Cancel the default.
    NotCovered { allowed: Vec<String> },
    /// The sign-in was cancelled or the licence could not be read: the
    /// typed confirmation, and Sign in again.
    Unchecked,
}

/// The decision for updating to `build`. A demo licence is judged by the
/// same rule as any other.
pub fn decide(read: &LicenceRead, build: &str) -> Gate {
    match read {
        LicenceRead::NotRead => Gate::SignInFirst,
        LicenceRead::Failed(_) => Gate::Unchecked,
        LicenceRead::Read(Licence::NotInstalled) => Gate::Confirm,
        LicenceRead::Read(Licence::Allowed(a)) => match check_version_against(a, build) {
            Ok(()) => Gate::Confirm,
            Err(m) => Gate::NotCovered { allowed: m.allowed },
        },
    }
}

/// The red sentence of the not-covered dialog.
pub fn not_covered_text(allowed: &[String], version: &str, build: &str) -> String {
    let needed = major_minor(build);
    if allowed.is_empty() {
        format!("This instance's licence lists no SENTIENT versions, so it covers none. After updating to {version}, SENTIENT stops working until you have a licence for {needed}.")
    } else {
        format!(
            "This instance's licence covers SENTIENT {}. After updating to {version}, SENTIENT stops working until you have a licence for {needed}.",
            allowed.join(", ")
        )
    }
}

/// The typed confirmation: the version exactly (spaces around it do not count).
pub fn typed_matches(typed: &str, version: &str) -> bool {
    let v = version.trim();
    !v.is_empty() && typed.trim() == v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(list: &[&str]) -> Vec<String> {
        list.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn build_version_is_the_first_three_components() {
        assert_eq!(build_version(None, "4.4.0.3"), "4.4.0");
        assert_eq!(build_version(None, "4.5.0.1-rc2"), "4.5.0");
        assert_eq!(build_version(None, "v4.4.0-nightly.20261025"), "4.4.0");
        assert_eq!(build_version(None, "4.3.0.58"), "4.3.0");
        assert_eq!(build_version(None, "4.4.0"), "4.4.0");
        // The manifest's own build version wins.
        assert_eq!(build_version(Some("4.4.1"), "4.4.0.3"), "4.4.1");
        assert_eq!(build_version(Some("  "), "4.4.0.3"), "4.4.0");
        assert_eq!(major_minor("4.5.0"), "4.5");
    }

    #[test]
    fn decision_for_each_case() {
        let covered = LicenceRead::Read(Licence::Allowed(s(&["4.4"])));
        assert_eq!(decide(&covered, "4.4.0"), Gate::Confirm);
        assert_eq!(coverage(&covered, "4.4.0"), Coverage::Covered);
        // Not covered.
        assert_eq!(decide(&covered, "4.5.0"), Gate::NotCovered { allowed: s(&["4.4"]) });
        assert_eq!(coverage(&covered, "4.5.0"), Coverage::NeedsNew);
        // An empty list covers nothing (SENTIENT refuses it).
        let empty = LicenceRead::Read(Licence::Allowed(vec![]));
        assert_eq!(decide(&empty, "4.4.0"), Gate::NotCovered { allowed: vec![] });
        assert_eq!(coverage(&empty, "4.4.0"), Coverage::NeedsNew);
        // Unknown: sign in first; cancelled or unreadable: unchecked.
        assert_eq!(decide(&LicenceRead::NotRead, "4.4.0"), Gate::SignInFirst);
        assert_eq!(decide(&LicenceRead::Failed("cancelled".into()), "4.4.0"), Gate::Unchecked);
        assert_eq!(coverage(&LicenceRead::NotRead, "4.4.0"), Coverage::NotChecked);
        // No licence installed: the normal confirm.
        assert_eq!(decide(&LicenceRead::Read(Licence::NotInstalled), "4.5.0"), Gate::Confirm);
        assert_eq!(coverage(&LicenceRead::Read(Licence::NotInstalled), "4.5.0"), Coverage::NoLicence);
        // A demo is judged by the same rule: its list is all that counts.
        assert_eq!(decide(&LicenceRead::Read(Licence::Allowed(s(&["4"]))), "4.5.0"), Gate::Confirm);
        // The release tag would wrongly fail an exact build entry: the build is compared.
        let exact = LicenceRead::Read(Licence::Allowed(s(&["4.4.0"])));
        assert_eq!(decide(&exact, &build_version(None, "4.4.0.3")), Gate::Confirm);
    }

    #[test]
    fn dialog_text_and_typed_confirmation() {
        assert_eq!(
            not_covered_text(&s(&["4.3", "4.4"]), "4.5.0.1", "4.5.0"),
            "This instance's licence covers SENTIENT 4.3, 4.4. After updating to 4.5.0.1, SENTIENT stops working until you have a licence for 4.5."
        );
        assert!(not_covered_text(&[], "4.5.0.1", "4.5.0").contains("lists no SENTIENT versions"));
        assert!(typed_matches(" 4.5.0.1 ", "4.5.0.1"));
        assert!(!typed_matches("4.5.0", "4.5.0.1"));
        assert!(!typed_matches("", ""));
    }

    #[test]
    fn allowed_versions_from_the_signed_payload() {
        assert_eq!(allowed_in_payload(r#"{"id":"x","allowed_versions":["4.4","5"]}"#), Some(s(&["4.4", "5"])));
        assert_eq!(allowed_in_payload(r#"{"id":"x","version_majors":["4"]}"#), Some(s(&["4"])));
        assert_eq!(allowed_in_payload(r#"{"id":"x"}"#), Some(vec![]));
        assert_eq!(allowed_in_payload("not json"), None);
        assert_eq!(allowed_in_payload(r#"{"allowed_versions":"4.4"}"#), None);
        assert_eq!(
            serde_json::to_string(&Licence::Allowed(s(&["4.4"]))).unwrap(),
            r#"{"kind":"allowed","allowed":["4.4"]}"#
        );
        assert_eq!(serde_json::from_str::<Licence>(r#"{"kind":"not_installed"}"#).unwrap(), Licence::NotInstalled);
    }
}

/// SENTIENT's tests of the rule (license_verifier.rs,
/// `version_rule_mirror_tests`), copied verbatim. This copy and SENTIENT's
/// must change together: if one changes, the rule changed (or must).
#[cfg(test)]
mod version_rule_mirror_tests {
    use super::*;

    /// SENTIENT's tests read its own build version; here a fixed one.
    const SENTIENT_VERSION: &str = "4.4.0";

    fn covers(allowed: &[&str], build: &str) -> bool {
        let allowed: Vec<String> = allowed.iter().map(|s| s.to_string()).collect();
        license_covers_version(&allowed, build)
    }

    #[test]
    fn server_docstring_examples_for_4_2_2() {
        assert!(covers(&["4"], "4.2.2"));
        assert!(covers(&["4.2"], "4.2.2"));
        assert!(covers(&["4.2.2"], "4.2.2"));
        assert!(!covers(&["4.0", "5.0"], "4.2.2"));
        assert!(!covers(&["4.3"], "4.2.2"));
    }

    #[test]
    fn server_branches() {
        // build empty or blank → False
        assert!(!covers(&["4"], ""));
        assert!(!covers(&["4"], "   "));
        // build of only dots → no parts → False
        assert!(!covers(&["4"], "..."));
        // an empty / dots-only prefix is skipped, the next one still counts
        assert!(!covers(&[""], "4.2.2"));
        assert!(covers(&["", "4.2"], "4.2.2"));
        // a prefix longer than the build is skipped
        assert!(!covers(&["4.2.2.1"], "4.2.2"));
        assert!(covers(&["4.2.2.1", "4"], "4.2.2"));
        // empty parts are dropped on both sides
        assert!(covers(&["4..2"], "4.2.2"));
        assert!(covers(&["4.2."], "4.2.2"));
        // component-wise, not string-wise
        assert!(!covers(&["4.2"], "4.20.0"));
        assert!(!covers(&["4.2"], "4.3.0"));
        // any entry may match
        assert!(covers(&["5", "4.2"], "4.2.0"));
    }

    #[test]
    fn upgrade_past_the_licensed_versions_is_refused() {
        // saf: licence for 4.2 / 4.3 / 5.0, server upgraded to 4.4.0.
        assert!(!covers(&["4.2", "4.3", "5.0"], "4.4.0"));
        let err = check_version_against(&["4.2".into(), "4.3".into(), "5.0".into()], "4.4.0").unwrap_err();
        assert_eq!(err.allowed, vec!["4.2", "4.3", "5.0"]);
        assert_eq!(err.build, "4.4.0");
        assert!(err.message().contains("[4.2, 4.3, 5.0]") && err.message().contains("4.4.0"), "{}", err.message());
    }

    #[test]
    fn empty_list_is_refused_here_unlike_the_server() {
        // The server skips its check for an empty list; SENTIENT refuses it
        // (BP-12: not version-bound).
        assert!(!covers(&[], "4.4.0"));
        let err = check_version_against(&[], "4.4.0").unwrap_err();
        assert!(err.message().contains("not version-bound"));
    }

    #[test]
    fn this_build_is_covered_by_its_own_major() {
        let major = SENTIENT_VERSION.split('.').next().unwrap().to_string();
        assert!(check_version_against(&[major], SENTIENT_VERSION).is_ok());
    }
}
