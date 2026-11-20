// Copyright © 2016-2026 The SENTIENT Authors

//! A Platform Manager installed on the server, used as its agent.
//!
//! The agent jobs (`--job …`) are the same code in every build, so a
//! Platform Manager someone installed on the server (Windows: Program Files;
//! Linux: /opt/sentient-platform-manager, the package or its own install)
//! can do the work itself: nothing has to be copied there. It says what it
//! is with `--job agent-info` (version, commit, [`PROTOCOL`]); one too old
//! to know that job is protocol 0.
//!
//! - Same protocol, same version: it is used, with no setup.
//! - Same protocol, another version: the pages say so, and offer "Use it
//!   anyway" (kept per instance, for that version) or setting up this
//!   version's agent beside it.
//! - Another protocol: this version's agent is set up beside it (Windows:
//!   ProgramData; Linux: [`LINUX_OWN`]); the installed one is never changed.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

pub use super::PROTOCOL;

/// Where this Platform Manager puts its own agent on a Linux server whose
/// /opt/sentient-platform-manager holds an installed (desktop) Platform
/// Manager that cannot be used: beside it, never over it.
pub const LINUX_OWN: &str = "/var/lib/sentient-platform-manager/agent/sentient-platform-manager";

/// What `--job agent-info` prints (one JSON line).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AgentInfo {
    pub version: String,
    #[serde(default)]
    pub commit: String,
    /// [`PROTOCOL`] of that build; 0 when it is too old to say.
    #[serde(default)]
    pub protocol: u32,
    /// A full Platform Manager (with its window), not a bare agent.
    #[serde(default)]
    pub window: bool,
}

impl AgentInfo {
    /// This program.
    pub fn this() -> Self {
        AgentInfo {
            version: crate::updates::manager_version().to_string(),
            commit: crate::updates::build_commit().to_string(),
            protocol: PROTOCOL,
            window: cfg!(feature = "gui"),
        }
    }
}

/// Read what `--job agent-info` printed. A build without the job answers
/// with its usage text, or refuses for want of rights: protocol 0, version
/// unknown (a Windows program's FileVersion is the crate's, without the rc,
/// so it would mislead).
pub fn parse_info(out: &str) -> AgentInfo {
    if let Some(info) = out.lines().rev().map(str::trim).filter(|l| l.starts_with('{')).find_map(|l| serde_json::from_str::<AgentInfo>(l).ok()) {
        return info;
    }
    AgentInfo::default()
}

/// How an installed Platform Manager fits this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Same protocol, same version: used as it is.
    Same,
    /// Same protocol, another version: usable when the operator says so.
    Compatible,
    /// Another protocol (or too old to say): this version's agent is needed.
    Incompatible,
}

pub fn fit(theirs: &AgentInfo, ours: &AgentInfo) -> Fit {
    if theirs.protocol == 0 || theirs.protocol != ours.protocol {
        Fit::Incompatible
    } else if theirs.version == ours.version {
        Fit::Same
    } else {
        Fit::Compatible
    }
}

/// "older" or "newer" (or "another version", when it is unknown).
fn relation(theirs: &AgentInfo, ours: &AgentInfo) -> &'static str {
    use std::cmp::Ordering::*;
    if theirs.version.is_empty() {
        return t!("an older version");
    }
    match sentient_installer_core::releases::compare(&theirs.version, &ours.version) {
        Less => t!("older than this one"),
        Greater => t!("newer than this one"),
        Equal => t!("another build of this version"),
    }
}

/// Every message about an installed Platform Manager starts with this (the
/// pages tell these from other errors by it: [`is_offer`]).
pub const OFFER_PREFIX: &str = n!("A Platform Manager is installed on this server");

/// The plain words for an installed Platform Manager that is not used yet.
pub fn offer_message(theirs: &AgentInfo, ours: &AgentInfo) -> String {
    let version = if theirs.version.is_empty() { t!("an older version").to_string() } else { tf!("version {}", theirs.version) };
    match fit(theirs, ours) {
        Fit::Same => tf!("{OFFER_PREFIX} ({version}), the same as this one."),
        Fit::Compatible => tf!(
            "{OFFER_PREFIX}: {version}, {} ({}). It does its work the same way, so it can be used as it is; or set up this version's agent beside it.",
            relation(theirs, ours),
            ours.version
        ),
        Fit::Incompatible if theirs.protocol == 0 => tf!(
            "{OFFER_PREFIX}: {version}, too old to work with this one ({}). Set up puts this version's agent beside it; the installed one is not changed. Updating that Platform Manager lets it be used instead.",
            ours.version
        ),
        Fit::Incompatible => tf!(
            "{OFFER_PREFIX}: {version}, {} ({}), and it does its work differently. Set up puts this version's agent beside it; the installed one is not changed.",
            relation(theirs, ours),
            ours.version
        ),
    }
}

pub fn is_offer(e: &str) -> bool {
    e.starts_with(OFFER_PREFIX)
}

/// An installed Platform Manager found on a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub path: String,
    pub info: AgentInfo,
}

/// The installed Platform Manager each instance has, as last found.
static FOUND: Mutex<Option<HashMap<String, Installed>>> = Mutex::new(None);
/// Instances whose installed Platform Manager did not know a job: their own
/// agent is set up on the next update, whatever its version says.
static REFUSED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

pub fn remember(id: &str, found: Option<Installed>) {
    let mut guard = FOUND.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    match found {
        Some(f) => {
            map.insert(id.to_string(), f);
        }
        None => {
            map.remove(id);
        }
    }
}

/// The installed Platform Manager last found for an instance.
pub fn found(id: &str) -> Option<Installed> {
    FOUND.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(id).cloned())
}

/// The installed one answered a job it does not know (crate::remote::spm_job).
pub fn refuse(id: &str) {
    REFUSED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashSet::new).insert(id.to_string());
}

fn refused(id: &str) -> bool {
    REFUSED.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|s| s.contains(id))
}

/// "Use it anyway": kept per instance for that version and commit.
fn accepted_path() -> PathBuf {
    crate::updates::prefs_dir().join("remote-agents.json")
}

fn accepted_all() -> HashMap<String, String> {
    std::fs::read_to_string(accepted_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn accept_key(info: &AgentInfo) -> String {
    format!("{}+{}", info.version, info.commit)
}

/// The operator chose to use this installed Platform Manager as it is.
pub fn accept(id: &str, info: &AgentInfo) -> Result<(), String> {
    let mut all = accepted_all();
    all.insert(id.to_string(), accept_key(info));
    let path = accepted_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&all).unwrap_or_default()).map_err(|e| format!("write {}: {e}", path.display()))
}

/// The instance was removed (or its server changed): forget the choice.
pub fn forget(id: &str) {
    let mut all = accepted_all();
    if all.remove(id).is_some() {
        let _ = std::fs::write(accepted_path(), serde_json::to_string_pretty(&all).unwrap_or_default());
    }
    remember(id, None);
}

/// Whether the installed Platform Manager does the instance's jobs.
pub fn usable(id: &str, info: &AgentInfo) -> bool {
    decide(fit(info, &AgentInfo::this()), accepted_all().get(id).is_some_and(|k| *k == accept_key(info)), refused(id))
}

/// [`usable`] without the stores (tests).
pub fn decide(fit: Fit, accepted: bool, refused: bool) -> bool {
    !refused
        && match fit {
            Fit::Same => true,
            Fit::Compatible => accepted,
            Fit::Incompatible => false,
        }
}

/// Where a Windows server may have the Platform Manager installed
/// (crate::self_install::manager_exe), as PowerShell: `$i` is the program,
/// or `$null`. The uninstall entry's InstallLocation comes first.
pub fn windows_find_script() -> String {
    let product = crate::self_install::PRODUCT;
    format!(
        "$i = $null; $c = @(); \
         $k = Get-ItemProperty 'HKLM:\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{product}' -ErrorAction SilentlyContinue; \
         if ($k -and $k.InstallLocation) {{ $c += (Join-Path $k.InstallLocation '{product}.exe') }}; \
         $c += (Join-Path $env:ProgramFiles '{product}\\{product}.exe'); \
         foreach ($p in $c) {{ if (Test-Path -PathType Leaf $p) {{ $i = $p; break }} }}"
    )
}

/// Read a probe's answer: `@@OWN`, `@@NONE`, or `@@INSTALLED <path>`
/// followed by what agent-info printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    Own,
    None,
    Installed(Installed),
}

pub fn parse_probe(out: &str) -> Probe {
    let mut path = None;
    for l in out.lines().map(str::trim) {
        if l == "@@OWN" {
            return Probe::Own;
        }
        if let Some(p) = l.strip_prefix("@@INSTALLED ") {
            path = Some(p.trim().to_string());
        }
    }
    match path {
        Some(path) => Probe::Installed(Installed { path, info: parse_info(out) }),
        None => Probe::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_eight_agent_is_older_than_nine() {
        let ours = info("0.9.0", PROTOCOL);
        for old in ["0.8.0-rc11", "0.8.0-rc12", "0.8.0"] {
            assert_eq!(relation(&info(old, PROTOCOL), &ours), "older than this one", "{old}");
            assert_eq!(fit(&info(old, PROTOCOL), &ours), Fit::Compatible);
        }
    }

    fn info(v: &str, p: u32) -> AgentInfo {
        AgentInfo { version: v.into(), commit: "abc1234".into(), protocol: p, window: true }
    }

    #[test]
    fn agent_info_reads_back() {
        let me = AgentInfo::this();
        let line = serde_json::to_string(&me).unwrap();
        assert_eq!(parse_info(&format!("noise\n{line}\n")), me);
        assert_eq!(me.protocol, PROTOCOL);
    }

    #[test]
    fn an_agent_without_agent_info_is_protocol_0() {
        let old = parse_info("usage: --job install <plan.json> | install-manager");
        assert_eq!(old, AgentInfo::default());
        let refused = parse_info("Run this from an elevated prompt (Run as administrator).");
        assert_eq!(refused.protocol, 0);
        assert_eq!(refused.version, "");
    }

    #[test]
    fn the_protocol_decides() {
        let ours = info("0.8.0-rc12", PROTOCOL);
        assert_eq!(fit(&info("0.8.0-rc12", PROTOCOL), &ours), Fit::Same);
        assert_eq!(fit(&info("0.8.0-rc11", PROTOCOL), &ours), Fit::Compatible);
        assert_eq!(fit(&info("0.8.0-rc13", PROTOCOL), &ours), Fit::Compatible);
        assert_eq!(fit(&info("0.8.0-rc12", PROTOCOL + 1), &ours), Fit::Incompatible);
        assert_eq!(fit(&info("0.8.0-rc12", 0), &ours), Fit::Incompatible);
        // Same: used; compatible only when chosen; never once refused.
        assert!(decide(Fit::Same, false, false));
        assert!(!decide(Fit::Compatible, false, false));
        assert!(decide(Fit::Compatible, true, false));
        assert!(!decide(Fit::Incompatible, true, false));
        assert!(!decide(Fit::Same, false, true));
    }

    #[test]
    fn the_offer_says_older_or_newer() {
        let ours = info("0.8.0-rc12", PROTOCOL);
        let older = offer_message(&info("0.8.0-rc11", PROTOCOL), &ours);
        assert!(is_offer(&older));
        assert!(older.contains("0.8.0-rc11, older than this one (0.8.0-rc12)"), "{older}");
        assert!(older.contains("can be used as it is"));
        let newer = offer_message(&info("0.9.0", PROTOCOL + 1), &ours);
        assert!(newer.contains("newer than this one") && newer.contains("Set up puts this version's agent beside it"), "{newer}");
        let old = offer_message(&AgentInfo::default(), &ours);
        assert!(old.contains("an older version, too old to work with this one"), "{old}");
    }

    #[test]
    fn the_windows_probe_reads_back() {
        assert_eq!(parse_probe("@@OWN\n"), Probe::Own);
        assert_eq!(parse_probe("@@NONE\r\n"), Probe::None);
        let line = serde_json::to_string(&info("0.8.0-rc12", PROTOCOL)).unwrap();
        let p = parse_probe(&format!("@@INSTALLED C:\\Program Files\\SENTIENT Platform Manager\\SENTIENT Platform Manager.exe\r\n{line}\r\n"));
        assert_eq!(
            p,
            Probe::Installed(Installed { path: r"C:\Program Files\SENTIENT Platform Manager\SENTIENT Platform Manager.exe".into(), info: info("0.8.0-rc12", PROTOCOL) })
        );
        // An older one: PowerShell wraps its usage text (stderr) as an error.
        let p = parse_probe("@@INSTALLED C:\\x\\spm.exe\r\nSENTIENT Platform Manager.exe : usage: --job install <plan.json>\r\n");
        let Probe::Installed(i) = p else { panic!() };
        assert_eq!((i.path.as_str(), i.info.protocol), (r"C:\x\spm.exe", 0));
    }
}
