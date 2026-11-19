// Copyright © 2016-2026 The SENTIENT Authors

//! Whether a native ("In the system") SENTIENT can run on a Linux computer:
//! the SENTIENT server binary is linked against the GNU C library, and the
//! dynamic loader refuses to start it on a glibc older than the newest
//! symbol version it needs (`GLIBC_2.38 not found`, at the very end of an
//! install). SENTIENT does not support older systems, so a native install
//! (and a native update) is refused up front with the reason; Docker
//! carries its own C library and stays allowed.
//!
//! The minimum comes from the release manifest's `min_glibc`; a manifest
//! without it falls back on [`MIN_GLIBC_BY_RELEASE`]. Reading the payload's
//! own server binary instead is not done: the check runs before anything is
//! downloaded (the payload is ~500 MB, the binary ~250 MB of it), and for a
//! server the payload is only ever downloaded there.
//!
//! The C library is read from `ldd --version` (glibc prints
//! `ldd (GNU libc) 2.36` or `ldd (Ubuntu GLIBC 2.39-0ubuntu8) 2.39`, musl
//! `musl libc (x86_64)` / `Version 1.2.4` on standard error). Unknown is
//! warned about, not refused; musl (Alpine) is refused.

use serde::{Deserialize, Serialize};

/// A glibc version: major and minor ("2.38"); a patch level is ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GlibcVersion {
    pub major: u32,
    pub minor: u32,
}

impl GlibcVersion {
    /// "2.38", "GLIBC_2.38", "2.38.1", "2.39-0ubuntu8" → 2.38 / 2.39.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let s = s.strip_prefix("GLIBC_").unwrap_or(s);
        let mut it = s.split(['.', '-', '+', ' ']);
        let major = it.next()?.parse().ok()?;
        let minor = it.next()?.parse().ok()?;
        Some(GlibcVersion { major, minor })
    }
}

impl std::fmt::Display for GlibcVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// The glibc a SENTIENT release's Linux server needs, by its major.minor,
/// for manifests that do not say (`min_glibc`). The release pipeline should
/// fill the manifest field (the newest `GLIBC_x.y` in
/// `objdump -T bin/sentient-server` that is not a weak reference); this
/// table only covers releases published before it did.
///
/// 4.4: measured on 4.4.0.3's sentient-payload-linux-x86_64.tar. Its server
/// needs GLIBC_2.43 (atan2f, acosf, log10f from libm) and GLIBC_2.38
/// (__isoc23_strtol and friends): it was built on Ubuntu 26.04. Debian 12
/// stops at 2.38, the first one its loader names.
pub const MIN_GLIBC_BY_RELEASE: &[(&str, &str)] = &[("4.4", "2.43")];

/// The glibc release `sentient_version` needs: the manifest's `min_glibc`
/// when it has one, else [`MIN_GLIBC_BY_RELEASE`] for its major.minor
/// (None: not known, nothing is checked).
pub fn min_glibc(manifest_field: Option<&str>, sentient_version: &str) -> Option<GlibcVersion> {
    if let Some(v) = manifest_field.and_then(GlibcVersion::parse) {
        return Some(v);
    }
    let mut parts = sentient_version.trim().split('.');
    let key = format!("{}.{}", parts.next()?, parts.next()?);
    MIN_GLIBC_BY_RELEASE.iter().find(|(r, _)| *r == key).and_then(|(_, v)| GlibcVersion::parse(v))
}

/// The C library a Linux computer runs, as read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Libc {
    Glibc { version: String },
    /// musl (Alpine); its version when it said.
    Musl { version: String },
    /// Not read (no `ldd`, an answer not understood, an older agent).
    #[default]
    Unknown,
}

impl Libc {
    /// "glibc 2.36", "musl 1.2.4".
    pub fn label(&self) -> String {
        match self {
            Libc::Glibc { version } => format!("glibc {version}"),
            Libc::Musl { version } if version.is_empty() => "musl".into(),
            Libc::Musl { version } => format!("musl {version}"),
            Libc::Unknown => "an unknown C library".into(),
        }
    }
}

/// `ldd --version`'s output (standard output and error together) read.
pub fn parse_ldd(text: &str) -> Libc {
    let lower = text.to_ascii_lowercase();
    if lower.contains("musl") {
        let version = text.lines().find_map(|l| l.trim().strip_prefix("Version ")).map(|v| v.trim().to_string()).unwrap_or_default();
        return Libc::Musl { version };
    }
    let first = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let first_lower = first.to_ascii_lowercase();
    if first_lower.starts_with("ldd") && (first_lower.contains("glibc") || first_lower.contains("gnu libc")) {
        // The version is the line's last word: "ldd (Ubuntu GLIBC 2.39-0ubuntu8.6) 2.39".
        if let Some(v) = first.split_whitespace().last().and_then(GlibcVersion::parse) {
            return Libc::Glibc { version: v.to_string() };
        }
    }
    Libc::Unknown
}

/// What a native install of a release can do on a computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeGlibc {
    /// It runs (or nothing is known against it).
    Allowed,
    /// It cannot run: the text says why and what to do.
    Blocked(String),
    /// The C library could not be read: allowed, with this warning.
    Unknown(String),
}

impl NativeGlibc {
    pub fn blocked(&self) -> Option<&str> {
        match self {
            NativeGlibc::Blocked(t) => Some(t),
            _ => None,
        }
    }
}

/// The glibc reason when Docker is not there either (native stays selected
/// only then: Setup and the remote install switch to Docker when it is):
/// both reasons and what to do.
pub fn and_no_docker(why: &str, here: Here) -> String {
    let place = match here {
        Here::Computer => "this computer",
        Here::Server => "this server",
    };
    format!("{why}\nDocker is not installed on {place} either: install Docker Engine there and check again, or upgrade the system.")
}

/// Where the check is made: the text says "this computer" or "this server".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Here {
    Computer,
    Server,
}

impl Here {
    fn name(self) -> &'static str {
        match self {
            Here::Computer => "this computer",
            Here::Server => "this server",
        }
    }
}

/// Releases with their glibc, oldest first: what "upgrade the system"
/// suggests (the first Ubuntu and the first Debian, else Fedora, new
/// enough). Released versions only.
const SYSTEMS: &[(&str, &str, u32)] = &[
    ("Ubuntu", "24.04", 39),
    ("Debian", "13", 41),
    ("Fedora", "42", 41),
    ("Fedora", "43", 42),
    ("Ubuntu", "26.04", 43),
    ("Fedora", "44", 43),
];

/// "for example Ubuntu 24.04 or Debian 13", for a glibc of `min` or newer.
fn examples(min: GlibcVersion) -> String {
    let fits = |name: &str| SYSTEMS.iter().find(|(n, _, m)| *n == name && min.major == 2 && *m >= min.minor).map(|(n, v, _)| format!("{n} {v}"));
    let picked: Vec<String> = [fits("Ubuntu"), fits("Debian").or_else(|| fits("Fedora"))].into_iter().flatten().collect();
    match picked.as_slice() {
        [] => "a newer release".into(),
        [one] => format!("for example {one}"),
        [a, b, ..] => format!("for example {a} or {b}"),
    }
}

/// The reason a native install is refused, as the (i), the notice and the
/// footer say it.
pub fn blocked_text(sentient_version: &str, min: GlibcVersion, found: &Libc, os_name: &str, here: Here) -> String {
    let found = match found {
        Libc::Glibc { version } => version.clone(),
        other => other.label(),
    };
    let os = if os_name.trim().is_empty() { String::new() } else { format!(" ({})", os_name.trim()) };
    format!(
        "SENTIENT {sentient_version} needs glibc {min} or newer; {} has {found}{os}. Install it with Docker instead, or upgrade the system ({}).",
        here.name(),
        examples(min)
    )
}

/// Whether a native SENTIENT `sentient_version` (needing glibc `min`, None
/// when not known) can run where `found` was read.
pub fn decide(sentient_version: &str, min: Option<GlibcVersion>, found: &Libc, os_name: &str, here: Here) -> NativeGlibc {
    let Some(min) = min else { return NativeGlibc::Allowed };
    match found {
        Libc::Musl { .. } => NativeGlibc::Blocked(blocked_text(sentient_version, min, found, os_name, here)),
        Libc::Glibc { version } => match GlibcVersion::parse(version) {
            Some(v) if v >= min => NativeGlibc::Allowed,
            Some(_) => NativeGlibc::Blocked(blocked_text(sentient_version, min, found, os_name, here)),
            None => NativeGlibc::Unknown(unknown_text(sentient_version, min, here)),
        },
        Libc::Unknown => NativeGlibc::Unknown(unknown_text(sentient_version, min, here)),
    }
}

fn unknown_text(sentient_version: &str, min: GlibcVersion, here: Here) -> String {
    format!(
        "The C library of {} could not be read. SENTIENT {sentient_version} needs glibc {min} or newer: if it does not start, install it with Docker instead.",
        here.name()
    )
}

/// This computer's C library (the host's, from inside a Flatpak too).
/// `SPM_DEBUG_GLIBC` (debug builds): a version to show instead ("2.36"),
/// or "musl" / "unknown".
pub fn local() -> Libc {
    #[cfg(debug_assertions)]
    if let Ok(v) = std::env::var("SPM_DEBUG_GLIBC") {
        return match v.as_str() {
            "musl" => Libc::Musl { version: "1.2.5".into() },
            "unknown" => Libc::Unknown,
            v => Libc::Glibc { version: v.to_string() },
        };
    }
    if !cfg!(target_os = "linux") {
        return Libc::Unknown;
    }
    match crate::sys::output_limited("ldd", &["--version"], std::time::Duration::from_secs(5)) {
        // musl's ldd answers on standard error and fails: read both.
        Some((_, out, err)) => parse_ldd(&format!("{}\n{}", String::from_utf8_lossy(&out), String::from_utf8_lossy(&err))),
        None => Libc::Unknown,
    }
}

/// This computer's system as /etc/os-release names it ("Debian GNU/Linux
/// 12 (bookworm)"); the host's from inside a Flatpak.
pub fn local_os_name() -> String {
    #[cfg(debug_assertions)]
    if std::env::var_os("SPM_DEBUG_GLIBC").is_some() {
        return "Debian GNU/Linux 12 (bookworm)".into();
    }
    let path = if crate::sys::in_flatpak() { "/run/host/os-release" } else { "/etc/os-release" };
    let text = std::fs::read_to_string(path).or_else(|_| std::fs::read_to_string("/usr/lib/os-release")).unwrap_or_default();
    text.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=")).map(|v| v.trim().trim_matches('"').to_string()).unwrap_or_default()
}

#[cfg(test)]
mod tests {

    /// Native blocked and no Docker: both reasons and what to do.
    #[test]
    fn without_docker_both_reasons_are_given() {
        let t = super::and_no_docker("SENTIENT 4.4.0.3 needs glibc 2.43 or newer.", super::Here::Server);
        assert!(t.starts_with("SENTIENT 4.4.0.3 needs glibc 2.43"));
        assert!(t.contains("Docker is not installed on this server either"));
    }

    use super::*;

    fn v(s: &str) -> GlibcVersion {
        GlibcVersion::parse(s).unwrap()
    }

    #[test]
    fn versions_compare_by_number_not_text() {
        assert!(v("2.36") < v("2.38"));
        assert!(v("2.39") > v("2.38"));
        assert!(v("2.38") == v("2.38.1"));
        // "2.4" is older than "2.38" (text order would say otherwise).
        assert!(v("2.4") < v("2.38"));
        assert!(v("2.10") > v("2.9"));
        assert_eq!(v("GLIBC_2.43"), GlibcVersion { major: 2, minor: 43 });
        assert_eq!(v("2.39-0ubuntu8").to_string(), "2.39");
        assert!(GlibcVersion::parse("").is_none() && GlibcVersion::parse("two").is_none() && GlibcVersion::parse("2").is_none());
    }

    #[test]
    fn ldd_output_reads_glibc_and_musl() {
        let debian = "ldd (Debian GLIBC 2.36-9+deb12u10) 2.36\nCopyright (C) 2022 Free Software Foundation, Inc.\n";
        assert_eq!(parse_ldd(debian), Libc::Glibc { version: "2.36".into() });
        let ubuntu = "ldd (Ubuntu GLIBC 2.39-0ubuntu8.6) 2.39\nCopyright (C) 2024 Free Software Foundation, Inc.";
        assert_eq!(parse_ldd(ubuntu), Libc::Glibc { version: "2.39".into() });
        let fedora = "ldd (GNU libc) 2.41\nCopyright (C) 2025 Free Software Foundation, Inc.";
        assert_eq!(parse_ldd(fedora), Libc::Glibc { version: "2.41".into() });
        // musl answers on standard error (read with the output).
        let alpine = "\nmusl libc (x86_64)\nVersion 1.2.5\nDynamic Program Loader\nUsage: ldd [options] [--] pathname\n";
        assert_eq!(parse_ldd(alpine), Libc::Musl { version: "1.2.5".into() });
        assert_eq!(parse_ldd(""), Libc::Unknown);
        assert_eq!(parse_ldd("sh: ldd: not found"), Libc::Unknown);
    }

    #[test]
    fn native_is_blocked_on_an_older_glibc_and_on_musl() {
        let min = Some(v("2.38"));
        let g = |s: &str| Libc::Glibc { version: s.into() };
        assert_eq!(decide("4.4.0.3", min, &g("2.38"), "", Here::Computer), NativeGlibc::Allowed);
        assert_eq!(decide("4.4.0.3", min, &g("2.41"), "", Here::Computer), NativeGlibc::Allowed);
        let blocked = decide("4.4.0.3", min, &g("2.36"), "Debian GNU/Linux 12 (bookworm)", Here::Server);
        assert_eq!(
            blocked.blocked(),
            Some("SENTIENT 4.4.0.3 needs glibc 2.38 or newer; this server has 2.36 (Debian GNU/Linux 12 (bookworm)). Install it with Docker instead, or upgrade the system (for example Ubuntu 24.04 or Debian 13).")
        );
        let musl = decide("4.4.0.3", min, &Libc::Musl { version: "1.2.5".into() }, "Alpine Linux v3.22", Here::Computer);
        assert!(musl.blocked().is_some_and(|t| t.contains("this computer has musl 1.2.5 (Alpine Linux v3.22)")));
        // Unknown: warned, not refused.
        assert!(matches!(decide("4.4.0.3", min, &Libc::Unknown, "", Here::Computer), NativeGlibc::Unknown(_)));
        // No minimum known: nothing to check.
        assert_eq!(decide("4.3.0.58", None, &g("2.17"), "", Here::Computer), NativeGlibc::Allowed);
    }

    #[test]
    fn the_suggested_systems_are_new_enough() {
        assert_eq!(examples(v("2.38")), "for example Ubuntu 24.04 or Debian 13");
        assert_eq!(examples(v("2.41")), "for example Ubuntu 26.04 or Debian 13");
        assert_eq!(examples(v("2.43")), "for example Ubuntu 26.04 or Fedora 44");
        assert_eq!(examples(v("2.50")), "a newer release");
    }

    #[test]
    fn the_manifest_field_overrides_the_table() {
        assert_eq!(min_glibc(None, "4.4.0.3"), Some(v("2.43")));
        assert_eq!(min_glibc(Some("2.39"), "4.4.0.3"), Some(v("2.39")));
        // A field that does not read falls back on the table.
        assert_eq!(min_glibc(Some("soon"), "4.4.0.3"), Some(v("2.43")));
        assert_eq!(min_glibc(None, "4.3.0.58"), None);
        assert_eq!(min_glibc(Some("2.35"), "4.3.0.58"), Some(v("2.35")));
        assert_eq!(min_glibc(None, "nightly"), None);
    }
}
