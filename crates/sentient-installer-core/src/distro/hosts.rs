// Copyright © 2016-2026 The SENTIENT Authors

//! Where a SENTIENT Docker stack can run on Windows besides the Platform
//! Manager's own `sentient` distro: the operator's own WSL distros (Ubuntu,
//! Debian, …) and Docker Desktop. Discovery (the app's `adopt`) looks in
//! each and reports what it checked, so "nothing found" says where it
//! looked.
//!
//! Policy:
//! - Only running distros are looked in. Looking runs a command in the
//!   distro, which would start a stopped one, and with it its Docker and
//!   every container set to restart: a side effect nobody asked for. A
//!   stopped distro is reported as stopped ("start it and look again").
//! - The Platform Manager's own distro is not looked in here (on Windows it
//!   holds ClickHouse for the native install, under its own hidden account;
//!   an earlier Docker install there is a leftover, app `leftovers`), nor
//!   Docker Desktop's internal distros (`docker-desktop`,
//!   `docker-desktop-data`): Docker Desktop's containers are seen through a
//!   distro with its WSL integration turned on, or through the Windows
//!   `docker` command.
//! - WSL distros belong to a Windows account: these are the ones of the
//!   account the Platform Manager (or its agent, over SSH) runs as.

/// One WSL distro of this Windows account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WslDistro {
    pub name: String,
    pub running: bool,
    /// The account's default distro (`*` in `wsl -l -v`).
    pub default: bool,
    /// 1 or 2, when `wsl -l -v` said.
    pub version: Option<u8>,
}

/// Names from `wsl.exe -l -q` (already decoded from UTF-16).
pub fn parse_list_quiet(text: &str) -> Vec<String> {
    crate::removal::wsl_distros(text)
}

/// `wsl.exe -l -v` (decoded): (name, default, version) per distro. The
/// header and the state column are in Windows' language, so neither is
/// read: a row is a name followed by a version number in the last column.
pub fn parse_list_verbose(text: &str) -> Vec<(String, bool, Option<u8>)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}' || c == '\0');
            let (default, rest) = match line.strip_prefix('*') {
                Some(r) => (true, r.trim_start()),
                None => (false, line),
            };
            let cols: Vec<&str> = rest.split_whitespace().collect();
            if cols.len() < 3 {
                return None;
            }
            let version: u8 = cols.last()?.parse().ok()?;
            Some((cols[0].to_string(), default, Some(version)))
        })
        .collect()
}

/// Distros discovery does not look in: Docker Desktop's own. The Platform
/// Manager's own distro ("sentient") is looked in: an install made by an
/// earlier release (or whose record is gone) lives there.
pub fn is_skipped(name: &str) -> bool {
    let n = name.trim();
    n.eq_ignore_ascii_case("docker-desktop") || n.eq_ignore_ascii_case("docker-desktop-data")
}

/// The distros to report on, from the three `wsl -l` answers: the default
/// one first, then as listed; the skipped ones left out.
pub fn combine(names: &[String], verbose: &[(String, bool, Option<u8>)], running: &[String]) -> Vec<WslDistro> {
    let mut out: Vec<WslDistro> = names
        .iter()
        .filter(|n| !is_skipped(n))
        .map(|n| {
            let v = verbose.iter().find(|(name, _, _)| name.eq_ignore_ascii_case(n));
            WslDistro {
                name: n.clone(),
                running: running.iter().any(|r| r.eq_ignore_ascii_case(n)),
                default: v.is_some_and(|(_, d, _)| *d),
                version: v.and_then(|(_, _, ver)| *ver),
            }
        })
        .collect();
    // Stable: the default first, the rest keep their order.
    out.sort_by_key(|d| !d.default);
    out
}

/// This account's WSL distros (without the skipped ones). Err: WSL did
/// not list them (not installed, not working), with what it said.
#[cfg(windows)]
pub fn wsl_distros() -> Result<Vec<WslDistro>, String> {
    use crate::sys;
    let (ok, out, err) = sys::output_wsl("wsl.exe", &["-l", "-q"], std::time::Duration::from_secs(30)).ok_or("wsl.exe could not be run (WSL is not installed)")?;
    if !ok {
        // No distro at all is a failure too ("… has no installed
        // distributions"), in Windows' language: say what it said.
        let said = sys::decode(&[out, err].concat());
        let said = said.trim().lines().next().unwrap_or("").trim().to_string();
        return Err(if said.is_empty() { "wsl -l gave no answer".into() } else { said });
    }
    let names = parse_list_quiet(&sys::decode(&out));
    // Nothing running is an error exit too: no names, then.
    let running = sys::output_wsl("wsl.exe", &["-l", "--running", "-q"], std::time::Duration::from_secs(30))
        .filter(|(ok, _, _)| *ok)
        .map(|(_, out, _)| parse_list_quiet(&sys::decode(&out)))
        .unwrap_or_default();
    let verbose = sys::output_wsl("wsl.exe", &["-l", "-v"], std::time::Duration::from_secs(30))
        .filter(|(ok, _, _)| *ok)
        .map(|(_, out, _)| parse_list_verbose(&sys::decode(&out)))
        .unwrap_or_default();
    Ok(combine(&names, &verbose, &running))
}

/// Docker inside one distro.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockerState {
    /// No `docker` command there.
    Missing,
    /// The command is there, the engine does not answer.
    NotRunning,
    Running,
}

/// What [`DockerState`] is read from: one word on the last line (a login
/// shell can print other lines first).
pub const DOCKER_PROBE: &str =
    "if ! command -v docker >/dev/null 2>&1; then echo spm-docker-missing; elif docker info >/dev/null 2>&1; then echo spm-docker-running; else echo spm-docker-stopped; fi";

pub fn parse_docker_probe(out: &str) -> DockerState {
    match out.lines().rev().map(str::trim).find(|l| !l.is_empty()) {
        Some("spm-docker-running") => DockerState::Running,
        Some("spm-docker-stopped") => DockerState::NotRunning,
        _ => DockerState::Missing,
    }
}

/// Docker where the Docker commands run now (Linux: this computer;
/// Windows: [`super::docker_distro`]).
pub fn docker_here() -> DockerState {
    super::run_output(DOCKER_PROBE).map(|(_, out)| parse_docker_probe(&out)).unwrap_or(DockerState::Missing)
}

/// Docker in the WSL distro `name` (runs a command there: call for a
/// running distro only).
#[cfg(windows)]
pub fn docker_in(name: &str) -> DockerState {
    super::with_docker_distro(Some(name), docker_here)
}

/// A SENTIENT container the Windows `docker` command lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopContainer {
    pub id: String,
    pub name: String,
    pub image: String,
    pub running: bool,
}

/// Docker on the Windows side (Docker Desktop, or another engine the
/// Windows `docker` command talks to).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Desktop {
    /// No `docker` command on Windows.
    NoCli,
    /// The command is there, its engine does not answer.
    NotRunning,
    /// Its SENTIENT containers (possibly none).
    Running(Vec<DesktopContainer>),
}

/// `docker ps -a --no-trunc --format` for [`parse_desktop_ps`].
pub const DESKTOP_PS_FORMAT: &str = "{{.ID}}\t{{.Names}}\t{{.Image}}\t{{.State}}\t{{.Label \"com.docker.compose.service\"}}";

/// The SENTIENT containers in `docker ps` lines of [`DESKTOP_PS_FORMAT`]:
/// the image is `…/sentient[:tag]` or the compose service is `sentient`
/// (as `adopt::find` picks them).
pub fn parse_desktop_ps(text: &str) -> Vec<DesktopContainer> {
    text.lines()
        .filter_map(|line| {
            let cols: Vec<&str> = line.trim_end_matches(['\r', '\n']).split('\t').collect();
            if cols.len() < 4 {
                return None;
            }
            let image = cols[2].trim();
            let service = cols.get(4).map(|s| s.trim()).unwrap_or("");
            if super::adopt::image_repo_name(image) != "sentient" && service != "sentient" {
                return None;
            }
            Some(DesktopContainer {
                id: cols[0].trim().to_string(),
                name: cols[1].trim().to_string(),
                image: image.to_string(),
                running: cols[3].trim().eq_ignore_ascii_case("running"),
            })
        })
        .collect()
}

/// The Windows `docker` command and its SENTIENT containers.
#[cfg(windows)]
pub fn docker_desktop() -> Desktop {
    use crate::sys;
    match sys::output_wsl("docker", &["version", "--format", "{{.Server.Version}}"], std::time::Duration::from_secs(30)) {
        None => Desktop::NoCli,
        Some((false, _, _)) => Desktop::NotRunning,
        Some((true, _, _)) => match sys::output_wsl("docker", &["ps", "-a", "--no-trunc", "--format", DESKTOP_PS_FORMAT], std::time::Duration::from_secs(30)) {
            Some((true, out, _)) => Desktop::Running(parse_desktop_ps(&sys::decode(&out))),
            _ => Desktop::NotRunning,
        },
    }
}

/// Whether a container id found through a distro is one Docker Desktop
/// listed (the same engine, seen through its WSL integration). Docker
/// prints full ids with `--no-trunc`; a short one matches its prefix.
pub fn same_container(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim(), b.trim());
    !a.is_empty() && !b.is_empty() && (a.starts_with(b) || b.starts_with(a))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
    }

    #[test]
    fn quiet_list_in_utf16_with_bom() {
        let bytes = utf16("\u{feff}Ubuntu\r\nsentient\r\nDebian\r\n\r\n");
        let text = crate::sys::decode(&bytes);
        assert_eq!(parse_list_quiet(&text), vec!["Ubuntu", "sentient", "Debian"]);
    }

    #[test]
    fn quiet_list_in_utf8() {
        assert_eq!(parse_list_quiet("Ubuntu-24.04\ndocker-desktop\n"), vec!["Ubuntu-24.04", "docker-desktop"]);
    }

    #[test]
    fn verbose_list_marks_the_default_and_skips_the_header() {
        let text = "  NAME              STATE           VERSION\r\n* Ubuntu            Running         2\r\n  sentient          Stopped         2\r\n  Legacy            Stopped         1\r\n";
        assert_eq!(
            parse_list_verbose(text),
            vec![("Ubuntu".to_string(), true, Some(2)), ("sentient".to_string(), false, Some(2)), ("Legacy".to_string(), false, Some(1))]
        );
    }

    #[test]
    fn verbose_list_in_another_language_and_utf16() {
        // A state of two words, the header translated.
        let text = crate::sys::decode(&utf16("  NAME      STATUS            VERSION\r\n* Ubuntu    Wird ausgeführt   2\r\n  Debian    Beendet           2\r\n"));
        assert_eq!(parse_list_verbose(&text), vec![("Ubuntu".to_string(), true, Some(2)), ("Debian".to_string(), false, Some(2))]);
    }

    #[test]
    fn combine_puts_the_default_first_and_skips_docker_desktop() {
        let names: Vec<String> = ["Debian", "sentient", "Ubuntu", "docker-desktop", "docker-desktop-data"].iter().map(|s| s.to_string()).collect();
        let verbose = vec![("Ubuntu".to_string(), true, Some(2)), ("Debian".to_string(), false, Some(2))];
        let running = vec!["ubuntu".to_string(), "docker-desktop".to_string()];
        let got = combine(&names, &verbose, &running);
        assert_eq!(
            got,
            vec![
                WslDistro { name: "Ubuntu".into(), running: true, default: true, version: Some(2) },
                WslDistro { name: "Debian".into(), running: false, default: false, version: Some(2) },
                // The Platform Manager's own: an earlier release's install.
                WslDistro { name: "sentient".into(), running: false, default: false, version: None },
            ]
        );
    }

    #[test]
    fn docker_probe_reads_the_last_line() {
        assert_eq!(parse_docker_probe("Welcome to Ubuntu\nspm-docker-running\n"), DockerState::Running);
        assert_eq!(parse_docker_probe("spm-docker-stopped"), DockerState::NotRunning);
        assert_eq!(parse_docker_probe("spm-docker-missing\n"), DockerState::Missing);
        assert_eq!(parse_docker_probe(""), DockerState::Missing);
    }

    #[test]
    fn desktop_ps_keeps_sentient_containers_only() {
        let text = "aaa111\tsentient\tquakestring/sentient:4.4.0.1\trunning\tsentient\r\n\
                    bbb222\tsentient-postgres\tpostgres:18-alpine\trunning\tpostgres\r\n\
                    ccc333\tplant-a-app\tregistry.plant-a:5000/sentient\texited\t\r\n\
                    ddd444\tother\tnginx:latest\trunning\tsentient\n";
        let got = parse_desktop_ps(text);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], DesktopContainer { id: "aaa111".into(), name: "sentient".into(), image: "quakestring/sentient:4.4.0.1".into(), running: true });
        assert_eq!(got[1].name, "plant-a-app");
        assert!(!got[1].running);
        assert_eq!(got[2].name, "other");
    }

    #[test]
    fn same_container_by_full_or_short_id() {
        assert!(same_container("abcdef0123456789", "abcdef012345"));
        assert!(same_container("abcdef012345", "abcdef0123456789"));
        assert!(!same_container("abcdef", "123456"));
        assert!(!same_container("", "abc"));
    }
}
