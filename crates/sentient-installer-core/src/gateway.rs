// Copyright © 2016-2026 The SENTIENT Authors

//! The Sentient Gateway (the Rust gateway) on this computer: whether it is
//! there, which version, running or not, and installing, updating and
//! controlling it.
//!
//! Windows: the gateway's own installer (SentientGatewaySetup-<v>.exe from
//! the gateway-<v> releases on github.com/QuakeString/sentient-release), run
//! silently; it registers the SentientGateway service and keeps the
//! configuration and the local page's password across updates.
//!
//! Linux: the image quakestring/sentient-gateway-rs, run by Docker as the
//! container `sentient-gateway` with named volumes and the host network (so
//! a SENTIENT on this computer is 127.0.0.1). An update starts the new tag
//! with the same settings and volumes.

use crate::progress::{Progress, ProgressFn};
use crate::releases::{self, Release};

/// The local page's port when the gateway's settings name none.
pub const UI_PORT: u16 = 8642;
/// The local page's administrator user name on a fresh gateway (the
/// gateway's local-ui DEFAULT_USERNAME). Its default password ("invenia")
/// signs in only on the gateway's own machine, to set a real one.
pub const UI_USER: &str = "admin";
pub const IMAGE: &str = "quakestring/sentient-gateway-rs";
pub const CONTAINER: &str = "sentient-gateway";

/// What the detection looks at, in order (for support and the gateway
/// page's (i)).
pub const CHECKS: &str = "Docker containers, running or stopped, of the image quakestring/sentient-gateway-rs or whose program is sentient-gateway; the systemd unit sentient-gateway.service, or any unit that runs a program sentient-gateway that answers as the gateway; a running sentient-gateway; /usr/bin, /usr/local/bin and /opt/sentient-gateway; on Windows the service SentientGateway.";

#[cfg(windows)]
const SERVICE: &str = "SentientGateway";

#[derive(Debug, Clone, Default)]
pub struct Status {
    pub installed: bool,
    pub version: Option<String>,
    pub running: bool,
    /// The platform host and MQTT port it reports to.
    pub host: Option<String>,
    pub port: Option<u16>,
    /// The local web page's address, from the gateway's own settings
    /// (`localUi` in its file, `ST_GW_UI`/`ST_GW_UI_LISTEN`): None when the
    /// page is off (it is by default in a container).
    pub page_url: Option<String>,
    /// A Docker container: its name. `managed`: the Platform Manager
    /// started it (and can update it); a gateway from someone's compose
    /// file is shown and controlled, and updated through its compose file.
    pub container: Option<String>,
    pub managed: bool,
    /// Linux without Docker: the systemd unit it runs as, or the program of
    /// a plain process no service started (crate::distro::gateway_native).
    pub service: Option<String>,
    pub process: Option<String>,
    /// A container: its image as it was started, and the compose project
    /// and folder it came from (its labels), and its named volumes.
    pub image: Option<String>,
    pub compose_project: Option<String>,
    pub compose_dir: Option<String>,
    pub volumes: Vec<String>,
    /// A service or program: the program's path, and the package it came
    /// from (None: installed by hand).
    pub program: Option<String>,
    pub package: Option<String>,
    /// A native gateway whose settings folder this process may not read
    /// (`/etc/sentient-gateway` is 0700, the gateway's own account): host,
    /// port and page come from an administrator's read
    /// (app crate::manager::gateway), never by loosening the folder.
    pub config_unreadable: bool,
    /// A container of the older Python gateway (quakestring/sentient-gateway)
    /// on this computer: its name. The Platform Manager supports only the
    /// Rust gateway, so it never takes it for the Rust one (an update would
    /// recreate it on other volumes); the page says it is there.
    pub legacy: Option<String>,
    /// That container's image (its tag is the older gateway's version).
    pub legacy_image: Option<String>,
}

impl Status {
    /// How it runs, in a word for the agent's key=value line: docker,
    /// service, process, windows-service.
    pub fn kind(&self) -> &'static str {
        if self.container.is_some() {
            "docker"
        } else if self.service.is_some() {
            "service"
        } else if self.process.is_some() {
            "process"
        } else if self.installed {
            "windows-service"
        } else {
            ""
        }
    }
}

/// The local page's address as the gateway resolves it
/// (gateway_core::config::LocalUi::resolve): on by default except in a
/// container, `127.0.0.1:8642` (`0.0.0.0:8642` in a container), plain HTTP
/// on loopback and HTTPS on any other address.
pub fn page_url(gateway_file: Option<&str>, env: &dyn Fn(&str) -> Option<String>, in_container: bool) -> Option<String> {
    let mut enabled = !in_container;
    let mut listen = if in_container { format!("0.0.0.0:{UI_PORT}") } else { format!("127.0.0.1:{UI_PORT}") };
    if let Some(ui) = gateway_file
        .and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok())
        .and_then(|v| v.get("localUi").cloned())
    {
        if let Some(b) = ui.get("enabled").and_then(|b| b.as_bool()) {
            enabled = b;
        }
        if let Some(l) = ui.get("listen").and_then(|l| l.as_str()).map(str::trim).filter(|l| !l.is_empty()) {
            listen = l.to_string();
        }
    }
    if let Some(v) = env("ST_GW_UI") {
        match v.trim().to_ascii_lowercase().as_str() {
            "on" | "true" | "1" | "yes" => enabled = true,
            "off" | "false" | "0" | "no" => enabled = false,
            _ => {}
        }
    }
    if let Some(l) = env("ST_GW_UI_LISTEN").map(|l| l.trim().to_string()).filter(|l| !l.is_empty()) {
        listen = l;
    }
    if !enabled {
        return None;
    }
    let (host, port) = match listen.rsplit_once(':') {
        Some((h, p)) => (h.trim_start_matches('[').trim_end_matches(']').to_string(), p.parse().unwrap_or(UI_PORT)),
        None => (listen.clone(), UI_PORT),
    };
    let loopback = host == "localhost" || host.starts_with("127.") || host == "::1";
    // This computer reaches a wildcard listener on loopback, over HTTPS all the same.
    let reach = if host == "0.0.0.0" || host == "::" || host.is_empty() { "127.0.0.1".to_string() } else { host.clone() };
    let reach = if reach.contains(':') { format!("[{reach}]") } else { reach };
    Some(format!("{}://{reach}:{port}/", if loopback { "http" } else { "https" }))
}

/// An image's own name: `registry:5000/org/name:tag@sha` → `name`.
fn image_name(image: &str) -> &str {
    let image = image.split('@').next().unwrap_or(image);
    let repo = image.rsplit_once(':').map(|(r, t)| if t.contains('/') { image } else { r }).unwrap_or(image);
    repo.rsplit('/').next().unwrap_or(repo)
}

/// A gateway image of ours: the Rust gateway (quakestring/sentient-gateway-rs).
/// Not the older Python gateway (is_legacy_image), not other vendors'
/// gateway images.
pub fn is_gateway_image(image: &str) -> bool {
    image_name(image) == "sentient-gateway-rs"
}

/// The older Python gateway's image (quakestring/sentient-gateway): no
/// longer supported, only reported.
pub fn is_legacy_image(image: &str) -> bool {
    image_name(image) == "sentient-gateway"
}

/// Why the gateway found as `s` cannot be updated to `version` from here
/// (None: it can). `has_installer`: the release has the Windows installer;
/// `windows`: the computer it runs on is Windows. The reasons say what is
/// true and what to do instead; the update itself (update()) refuses the
/// same cases.
pub fn update_blocker(s: &Status, version: &str, has_installer: bool, windows: bool) -> Option<String> {
    if !s.installed {
        // The older Python gateway alone: updated to the Rust one in Docker.
        return match &s.legacy {
            Some(_) if windows => Some("On Windows the Platform Manager runs the gateway as a Windows service, not in Docker: remove the older gateway's container, then install the gateway here.".into()),
            Some(_) => None,
            None => Some("There is no Sentient Gateway to update.".into()),
        };
    }
    match s.kind() {
        "docker" if windows => Some("On Windows the Platform Manager runs the gateway as a Windows service, not in Docker: update this container the way it was started.".into()),
        "docker" if !s.managed => Some(match &s.compose_dir {
            Some(dir) => format!("This container comes from a compose file in {dir}: Take it over to update it here, or set its image to {IMAGE}:{version} in that file and run docker compose up -d."),
            None => format!(
                "This container ({}) was started outside the Platform Manager: Take it over to update it here, or start it again with the image {IMAGE}:{version} the way it was started.",
                s.container.as_deref().unwrap_or("?")
            ),
        }),
        "docker" => None,
        "service" => Some(match &s.package {
            Some(p) => format!(
                "This gateway came from the package {p}: update it with the system's package manager, using the package in the gateway-{version} release. The Platform Manager does not update a gateway installed without Docker yet."
            ),
            None => format!(
                "This gateway runs as the service {} without a package: run the new version's sentient-gateway --install as administrator (it stops the service, replaces the program and starts it again). The Platform Manager does not update a gateway installed without Docker yet.",
                s.service.as_deref().unwrap_or("?")
            ),
        }),
        "process" => Some("This gateway runs as a plain program that no service started: replace that program by hand. The Platform Manager updates a gateway in Docker or a Windows service only.".into()),
        _ if !has_installer => Some(format!("Gateway {version} has no Windows installer.")),
        _ => None,
    }
}

/// A path whose file name is the gateway's program (`sentient-gateway`,
/// `sentient-gateway.exe`). Only the whole file name counts, so
/// systemd-journal-gatewayd and the like never are.
pub fn is_gateway_program(path: &str) -> bool {
    let name = path.trim().rsplit(['/', '\\']).next().unwrap_or_default();
    name == "sentient-gateway" || name.eq_ignore_ascii_case("sentient-gateway.exe")
}

/// What to install with.
#[derive(Debug, Clone)]
pub struct Settings {
    pub host: String,
    pub port: u16,
    /// The gateway device's access token (Manage credentials in SENTIENT).
    pub token: String,
    /// The local page's administrator password (empty: set it later).
    pub ui_password: String,
}

/// A gateway release offered on the channel.
#[derive(Debug, Clone)]
pub struct Offer {
    pub version: String,
    pub date: String,
    pub notes: String,
    pub page: String,
    /// The Windows installer and its `.sha256`, when the release has them.
    pub installer: Option<releases::Asset>,
    pub checksum: Option<releases::Asset>,
    /// Its image ({IMAGE}:<version>) is not on Docker Hub yet: a gateway in
    /// Docker cannot take it ([`offer_blocker`]). False when not known.
    pub image_missing: bool,
}

/// The gateway's releases in the SENTIENT releases repository.
pub fn is_gateway(r: &Release) -> bool {
    r.tag_name.starts_with("gateway-")
}

/// `gateway-4.13.0` → `4.13.0`.
pub fn version_of(r: &Release) -> String {
    r.tag_name.trim_start_matches("gateway-").to_string()
}

/// The newest gateway release on `channel` (any, when nothing is installed),
/// for a gateway the way this computer runs it (Docker except on Windows).
pub fn newest(channel: releases::Channel) -> Result<Option<Offer>, String> {
    newest_with(channel, !cfg!(windows), false)
}

/// The newest gateway release on `channel`. Its image is looked up on
/// Docker Hub (`image_missing`). `docker`: the gateway runs in Docker, so
/// when GitHub's list cannot be had at all, the newest image tag on the
/// channel stands in (no notes). `force`: an explicit check.
pub fn newest_with(channel: releases::Channel, docker: bool, force: bool) -> Result<Option<Offer>, String> {
    let from_hub = |force: bool| -> Result<Option<Offer>, String> {
        let tags = crate::dockerhub::tags(IMAGE, force)?;
        Ok(crate::dockerhub::newest(&tags.value, channel).map(|version| Offer {
            version,
            date: String::new(),
            notes: String::new(),
            page: String::new(),
            installer: None,
            checksum: None,
            image_missing: false,
        }))
    };
    let (all, stale) = match releases::list(releases::SENTIENT_REPO, force) {
        Ok(l) => (l.value, l.stale.is_some()),
        Err(e) if docker => return from_hub(force).map_err(|_| e),
        Err(e) => return Err(e),
    };
    let listed = newest_in(&all, channel);
    // GitHub refused or could not be reached, so the list is an old one: a
    // release published since is missing from it. A gateway in Docker takes
    // the newest image tag when that is newer (its notes come with the
    // next list).
    if stale && docker {
        if let Ok(Some(hub)) = from_hub(force) {
            if listed.as_ref().is_none_or(|o| releases::is_newer(&hub.version, &o.version, false)) {
                return Ok(Some(hub));
            }
        }
    }
    let Some(mut offer) = listed else { return Ok(None) };
    // Unknown (Docker Hub not reachable): not held against it.
    offer.image_missing = crate::dockerhub::tag_exists(IMAGE, &offer.version, force) == Ok(false);
    Ok(Some(offer))
}

/// The newest gateway release on `channel` in `all`.
pub fn newest_in(all: &[Release], channel: releases::Channel) -> Option<Offer> {
    let mut candidates = releases::on_channel(all, channel, is_gateway);
    // on_channel sorts by the tag; gateway-4.10 vs 4.9 needs the version.
    candidates.sort_by(|a, b| releases::compare(&version_of(b), &version_of(a)));
    candidates.first().map(|r| offer_of(r))
}

/// Why `offer` cannot go to the gateway found as `s` (None: it can): its
/// image is not published yet for a gateway in Docker, else
/// [`update_blocker`]'s reasons.
pub fn offer_blocker(s: &Status, offer: &Offer, windows: bool) -> Option<String> {
    let docker = s.kind() == "docker" || (!s.installed && s.legacy.is_some());
    if offer.image_missing && docker && !windows {
        return Some(crate::dockerhub::not_published(&offer.version));
    }
    update_blocker(s, &offer.version, offer.installer.is_some(), windows)
}

pub fn status() -> Status {
    #[cfg(windows)]
    {
        // The service first; else a gateway container in Docker Desktop.
        let s = windows::status();
        if s.installed {
            Status { legacy: crate::distro::gateway_docker::legacy(), legacy_image: None, ..s }
        } else {
            crate::distro::gateway_docker::status()
        }
    }
    #[cfg(not(windows))]
    {
        // A container first; else a service or a plain process.
        let s = crate::distro::gateway_docker::status();
        if s.installed { s } else { Status { legacy: s.legacy, legacy_image: s.legacy_image, ..crate::distro::gateway_native::status() } }
    }
}

/// Install `offer` with `settings` (none installed yet).
pub fn install(sink: &ProgressFn, offer: &Offer, settings: &Settings) -> Result<(), String> {
    #[cfg(windows)]
    {
        windows::install(sink, offer, Some(settings))
    }
    #[cfg(not(windows))]
    {
        crate::distro::gateway_docker::install(sink, &offer.version, settings)
    }
}

/// Update the installed gateway to `offer`, keeping its configuration.
pub fn update(sink: &ProgressFn, offer: &Offer) -> Result<(), String> {
    #[cfg(windows)]
    {
        if windows::status().installed {
            windows::install(sink, offer, None)
        } else {
            crate::distro::gateway_docker::update(sink, &offer.version)
        }
    }
    #[cfg(not(windows))]
    {
        let s = status();
        if !s.installed && s.legacy.is_some() {
            return crate::distro::gateway_legacy::update(sink, &offer.version).map(|_| ());
        }
        if s.container.is_none() {
            // A service or a program: said, never attempted.
            let _ = sink;
            return Err(offer_blocker(&s, offer, false).unwrap_or_else(|| "There is no gateway container to update.".into()));
        }
        crate::distro::gateway_docker::update(sink, &offer.version)
    }
}

/// The gateway release `version` (any channel): what a server is asked to
/// update to when this computer showed that version.
pub fn release(version: &str) -> Result<Offer, String> {
    let all = releases::fetch(releases::SENTIENT_REPO)?;
    let r = all.iter().find(|r| is_gateway(r) && version_of(r) == version).ok_or_else(|| format!("There is no gateway release {version}."))?;
    Ok(offer_of(r))
}

fn offer_of(r: &Release) -> Offer {
    let v = version_of(r);
    let installer = r.assets.iter().find(|a| a.name == format!("SentientGatewaySetup-{v}.exe")).cloned();
    let checksum = r.assets.iter().find(|a| a.name == format!("SentientGatewaySetup-{v}.exe.sha256")).cloned();
    Offer { version: v, date: r.date(), notes: r.summary(), page: r.html_url.clone(), installer, checksum, image_missing: false }
}

/// Where the Windows service writes its log files, as the gateway resolves
/// it (gw/crates/gateway-core/src/logging.rs `LogFiles::directory`,
/// paths.rs): `ST_GW_LOGS_PATH` (or `TB_GW_LOGS_PATH`), else `logs` in the
/// data folder (`ST_GW_DATA_DIR`/`TB_GW_DATA_DIR`, else
/// `%ProgramData%\Sentient Gateway\data`). `env` is the service's own
/// environment, `NAME=value` each.
pub fn windows_log_dir(env: &[String], program_data: Option<&str>) -> String {
    let var = |suffix: &str| {
        ["ST_GW_", "TB_GW_"].iter().find_map(|p| {
            let k = format!("{p}{suffix}=");
            env.iter().find_map(|l| l.strip_prefix(&k)).map(str::trim).filter(|v| !v.is_empty()).map(str::to_string)
        })
    };
    if let Some(dir) = var("LOGS_PATH") {
        return dir;
    }
    let data = var("DATA_DIR").unwrap_or_else(|| format!("{}\\Sentient Gateway\\data", program_data.unwrap_or(r"C:\ProgramData").trim_end_matches(['\\', '/'])));
    format!("{}\\logs", data.trim_end_matches(['\\', '/']))
}

/// The Windows gateway service's log folder (None: no service here, or not
/// Windows).
pub fn log_dir() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        windows::log_dir()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// A log line that starts a record: the gateway's lines begin with an
/// RFC 3339 time (`2026-11-18T07:12:13.123456Z  INFO …`); others continue
/// the line before (a wrapped message, a backtrace).
fn starts_record(l: &str) -> bool {
    let b = l.as_bytes();
    b.len() >= 19 && b[..4].iter().all(u8::is_ascii_digit) && b[4] == b'-' && b[10] == b'T'
}

/// The gateway's log files (service.log, connector.log, …: one per logger)
/// as one log: their records in time order, the last `tail`, or those since
/// `since` (`YYYY-MM-DDTHH:MM:SS`, UTC) when given.
pub fn merge_logs(files: &[String], tail: usize, since: Option<&str>) -> String {
    let mut records: Vec<(String, usize, String)> = Vec::new();
    for text in files {
        let mut cur: Option<(String, String)> = None;
        for l in text.lines() {
            if starts_record(l) {
                if let Some((k, r)) = cur.take() {
                    records.push((k, records.len(), r));
                }
                cur = Some((l[..19].to_string(), l.to_string()));
            } else if let Some((_, r)) = cur.as_mut() {
                r.push('\n');
                r.push_str(l);
            } else if !l.trim().is_empty() {
                // A file's first lines without a time: kept, first.
                records.push((String::new(), records.len(), l.to_string()));
            }
        }
        if let Some((k, r)) = cur.take() {
            records.push((k, records.len(), r));
        }
    }
    records.sort();
    if let Some(since) = since {
        records.retain(|(k, _, _)| k.as_str() >= since);
    }
    let start = records.len().saturating_sub(tail);
    records[start..].iter().map(|(_, _, r)| r.as_str()).collect::<Vec<_>>().join("\n")
}

/// The newest records of the Windows gateway's log files in `dir`.
pub fn read_log_dir(dir: &std::path::Path, tail: usize, since: Option<&str>) -> Option<String> {
    let mut texts = Vec::new();
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        // The current files only (`service.log`), not the rotated `.log.1`…
        if e.file_name().to_string_lossy().ends_with(".log") {
            if let Ok(t) = std::fs::read_to_string(e.path()) {
                texts.push(t);
            }
        }
    }
    (!texts.is_empty()).then(|| merge_logs(&texts, tail, since))
}

/// `start`, `stop` or `restart`.
pub fn control(sink: &ProgressFn, action: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        if windows::status().installed {
            windows::control(sink, action)
        } else {
            crate::distro::gateway_docker::control(sink, action)
        }
    }
    #[cfg(not(windows))]
    {
        if crate::distro::gateway_docker::status().installed {
            crate::distro::gateway_docker::control(sink, action)
        } else {
            crate::distro::gateway_native::control(sink, action)
        }
    }
}

/// The program in `sc qc`'s BINARY_PATH_NAME line (quoted or not, with
/// arguments after it).
#[cfg_attr(not(windows), allow(dead_code))]
fn service_program(qc: &str) -> Option<String> {
    let v = qc.lines().find_map(|l| l.trim().strip_prefix("BINARY_PATH_NAME").map(|r| r.trim_start().trim_start_matches(':').trim().to_string()))?;
    let p = match v.strip_prefix('"') {
        Some(r) => r.split('"').next().unwrap_or_default().to_string(),
        None => v.split(".exe").next().map(|p| if v.contains(".exe") { format!("{p}.exe") } else { p.to_string() }).unwrap_or_default(),
    };
    (!p.is_empty()).then_some(p)
}

/// Remove the Sentient Gateway found here (status()): a container of ours,
/// our systemd unit and program (or package), or the SentientGateway
/// service. `keep_data` keeps its configuration and local buffer (a
/// container's volumes; a dated folder otherwise); `remove_image` also
/// removes a container's image. Never touches anything that is not the
/// gateway's: each kind checks it again before it changes anything.
/// Gives the closing message.
pub fn remove(sink: &ProgressFn, keep_data: bool, remove_image: bool) -> Result<String, String> {
    #[cfg(windows)]
    {
        if windows::status().installed {
            return windows::remove(sink, keep_data);
        }
        crate::distro::gateway_docker::remove(sink, keep_data, remove_image)
    }
    #[cfg(not(windows))]
    {
        if crate::distro::gateway_docker::status().installed {
            crate::distro::gateway_docker::remove(sink, keep_data, remove_image)
        } else {
            let _ = remove_image;
            crate::distro::gateway_native::remove(sink, keep_data)
        }
    }
}

/// The Windows service's settings (crate::gateway_settings).
#[cfg(windows)]
pub fn windows_read_settings() -> crate::gateway_settings::GwSettings {
    windows::read_settings()
}

#[cfg(windows)]
pub fn windows_apply_settings(sink: &ProgressFn, before: &crate::gateway_settings::GwSettings, plan: &crate::gateway_settings::ApplyPlan) -> Result<(), String> {
    windows::apply_settings(sink, before, plan)
}

#[cfg(windows)]
pub fn windows_program() -> String {
    windows::program()
}

#[cfg(windows)]
pub fn windows_test_creds() -> Option<crate::gateway_settings::TestCreds> {
    windows::test_creds()
}

/// `sentient-gateway 4.13.0` → `4.13.0`.
pub fn parse_version(out: &str) -> Option<String> {
    out.lines()
        .find_map(|l| l.trim().strip_prefix("sentient-gateway ").map(|v| v.trim().to_string()))
        .filter(|v| !v.is_empty())
}

/// The platform host and port in a gateway file (`st_gateway.json`).
pub fn host_of(gateway_file: &str) -> (Option<String>, Option<u16>) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(gateway_file) else {
        return (None, None);
    };
    let s = v.get("sentient").or_else(|| v.get("thingsboard"));
    let host = s.and_then(|s| s.get("host")).and_then(|h| h.as_str()).filter(|h| !h.is_empty()).map(str::to_string);
    let port = s.and_then(|s| s.get("port")).and_then(|p| p.as_u64()).map(|p| p as u16);
    (host, port)
}

/// The Windows installer's SHA-256 to check it against: the first word of
/// its `.sha256` file, else the digest GitHub recorded for it
/// (`sha256:<hex>`). None: the release publishes neither.
pub fn installer_sha256(sum_file: Option<&str>, digest: Option<&str>) -> Option<String> {
    let from_file = sum_file.and_then(|t| t.split_whitespace().next()).map(str::to_ascii_lowercase).filter(|h| !h.is_empty());
    from_file.or_else(|| digest.and_then(|d| d.trim().strip_prefix("sha256:")).map(str::to_ascii_lowercase).filter(|h| !h.is_empty()))
}

fn step(sink: &ProgressFn, name: &str) {
    sink(Progress::Step { name: name.into() });
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::path::PathBuf;

    fn exe() -> PathBuf {
        let pf = std::env::var_os("ProgramFiles").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
        pf.join("Sentient Gateway").join("sentient-gateway.exe")
    }

    fn config_file() -> PathBuf {
        let pd = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
        pd.join("Sentient Gateway").join("config").join("st_gateway.json")
    }

    pub fn status() -> Status {
        let query = crate::sys::output("sc.exe", &["query", SERVICE]);
        // Our service only when it runs our program (sc qc's BINARY_PATH_NAME).
        let program = crate::sys::output("sc.exe", &["qc", SERVICE]).and_then(|(ok, out, _)| ok.then(|| service_program(&crate::sys::decode(&out))).flatten());
        let service = matches!(query, Some((true, _, _))) && program.as_deref().is_some_and(is_gateway_program);
        let exe_path = program.filter(|p| service && std::path::Path::new(p).is_file()).map(PathBuf::from).unwrap_or_else(exe);
        let version = crate::sys::output(&exe_path.to_string_lossy(), &["--version"])
            .and_then(|(_, out, _)| parse_version(&crate::sys::decode(&out)));
        // The program alone counts when it answers as the gateway.
        let installed = service || version.is_some();
        if !installed {
            return Status::default();
        }
        let running = service && matches!(&query, Some((true, out, _)) if crate::sys::decode(out).contains("RUNNING"));
        let file = std::fs::read_to_string(config_file()).ok();
        let (host, port) = file.as_deref().map(host_of).unwrap_or((None, None));
        // The service's own environment (ST_GW_UI, ST_GW_UI_LISTEN) wins
        // over the file, as it does in the gateway.
        let env = service_environment();
        let page_url = page_url(file.as_deref(), &|k| env.iter().find_map(|l| l.strip_prefix(&format!("{k}=")).map(str::to_string)), false);
        Status { installed, version, running, host, port, page_url, container: None, managed: true, ..Status::default() }
    }

    pub fn log_dir() -> Option<PathBuf> {
        if !status().installed {
            return None;
        }
        let pd = std::env::var("ProgramData").ok();
        Some(PathBuf::from(windows_log_dir(&service_environment(), pd.as_deref())))
    }

    /// The service's Environment (REG_MULTI_SZ), one `NAME=value` per entry.
    fn service_environment() -> Vec<String> {
        let key = format!(r"HKLM\SYSTEM\CurrentControlSet\Services\{SERVICE}");
        match crate::sys::output("reg.exe", &["query", &key, "/v", "Environment"]) {
            Some((true, out, _)) => crate::sys::decode(&out)
                .lines()
                .find_map(|l| l.split("REG_MULTI_SZ").nth(1))
                .map(|v| v.trim().split("\\0").map(str::to_string).filter(|e| !e.is_empty()).collect())
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// Download the installer, check it, run it silently. `settings` for a
    /// first install; None updates in place.
    pub fn install(sink: &ProgressFn, offer: &Offer, settings: Option<&Settings>) -> Result<(), String> {
        let asset = offer
            .installer
            .clone()
            .ok_or_else(|| format!("Gateway {} has no Windows installer.", offer.version))?;
        let dir = std::env::temp_dir().join(format!("sentient-gateway-{}", offer.version));
        std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        // The token and the page's password are written here for the
        // installer: the folder is for SYSTEM, the Administrators and this
        // account only (as a server's agent, SYSTEM's temp folder).
        let has_secrets = settings.is_some_and(|s| !s.token.is_empty() || !s.ui_password.is_empty());
        if let Err(e) = crate::private_acl::make_private(&dir, true) {
            if has_secrets {
                return Err(format!("The gateway's access token has no private folder to wait in: {e}"));
            }
        }
        let file = dir.join(&asset.name);
        step(sink, &format!("Downloading Sentient Gateway {} ({} MB)", offer.version, asset.size >> 20));
        download(&asset.browser_download_url, &file)?;
        let sum_file = match &offer.checksum {
            Some(sum) => Some(fetch_text(&sum.browser_download_url)?),
            None => None,
        };
        match installer_sha256(sum_file.as_deref(), asset.digest.as_deref()) {
            Some(expected) => {
                let actual = crate::manifest::sha256_file(&file)?;
                if expected != actual {
                    let _ = std::fs::remove_file(&file);
                    return Err(format!("The gateway installer did not match its checksum ({actual} instead of {expected})."));
                }
            }
            None => sink(Progress::Log { line: format!("Gateway {} publishes no checksum for its installer; it is used as downloaded.", offer.version) }),
        }
        let mut args: Vec<String> = vec!["/VERYSILENT".into(), "/SUPPRESSMSGBOXES".into(), "/NORESTART".into()];
        let log = dir.join("setup.log");
        args.push(format!("/LOG={}", log.display()));
        let mut secrets = Vec::new();
        if let Some(s) = settings {
            args.push(format!("/HOST={}", s.host));
            args.push(format!("/PORT={}", s.port));
            if !s.token.is_empty() {
                let f = dir.join("token.txt");
                // A fresh file takes the folder's private access (an old
                // one would keep its own).
                let _ = std::fs::remove_file(&f);
                std::fs::write(&f, &s.token).map_err(|e| format!("write the token file: {e}"))?;
                args.push(format!("/TOKENFILE={}", f.display()));
                secrets.push(f);
            }
            if !s.ui_password.is_empty() {
                let f = dir.join("page.txt");
                let _ = std::fs::remove_file(&f);
                std::fs::write(&f, &s.ui_password).map_err(|e| format!("write the password file: {e}"))?;
                args.push(format!("/UIPASSWORDFILE={}", f.display()));
                secrets.push(f);
            }
        }
        step(sink, &format!("Installing Sentient Gateway {}", offer.version));
        let argrefs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let result = crate::sys::output_limited(&file.to_string_lossy(), &argrefs, std::time::Duration::from_secs(600));
        for f in secrets {
            let _ = std::fs::remove_file(f);
        }
        match result {
            Some((true, _, _)) => {}
            Some((false, _, err)) => {
                let detail = std::fs::read_to_string(&log)
                    .ok()
                    .and_then(|t| t.lines().rev().find(|l| l.contains("error") || l.contains("not installed")).map(str::to_string))
                    .unwrap_or_else(|| crate::sys::decode(&err));
                return Err(format!("The gateway installer stopped: {}", detail.trim()));
            }
            None => return Err("Could not start the gateway installer.".into()),
        }
        let _ = std::fs::remove_file(&file);
        let now = status();
        match now.version {
            Some(v) if v == offer.version => {
                // A first install sends at once: the service runs (the
                // installer starts it; else it is started here).
                let mut running = now.running;
                if settings.is_some() && !running {
                    step(sink, "Starting the gateway service");
                    running = control(&crate::progress::noop(), "start").is_ok();
                    if !running {
                        sink(Progress::Log { line: "The gateway service did not start within 30 seconds; its log says why (Gateway, Logs).".into() });
                    }
                }
                let message = if running || settings.is_none() { format!("Sentient Gateway {v} is installed.") } else { format!("Sentient Gateway {v} is installed, but its service is not running.") };
                sink(Progress::Done { message });
                Ok(())
            }
            Some(v) => Err(format!("The installer finished, but the gateway reports {v}, not {}.", offer.version)),
            None => Err("The installer finished, but the gateway does not answer --version.".into()),
        }
    }

    pub fn control(sink: &ProgressFn, action: &str) -> Result<(), String> {
        let run = |verb: &str| crate::sys::output("sc.exe", &[verb, SERVICE]);
        match action {
            "start" => {
                run("start");
            }
            "stop" => {
                run("stop");
            }
            _ => {
                run("stop");
                for _ in 0..30 {
                    if !status().running {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
                run("start");
            }
        }
        let want = action != "stop";
        for _ in 0..30 {
            if status().running == want {
                sink(Progress::Done { message: format!("The gateway is {}.", if want { "running" } else { "stopped" }) });
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        Err(format!("The gateway service did not {action} within 30 seconds."))
    }

    /// The gateway's own uninstaller (Inno Setup's), else the service and
    /// the program folder by hand. The configuration and data in
    /// ProgramData are moved to a dated folder, or deleted.
    pub fn remove(sink: &ProgressFn, keep_data: bool) -> Result<String, String> {
        let s = status();
        if !s.installed {
            return Err("There is no Sentient Gateway on this computer.".into());
        }
        let dir = exe().parent().map(PathBuf::from).unwrap_or_default();
        if !is_gateway_program(&exe().to_string_lossy()) {
            return Err("The gateway's program was not found; nothing was removed.".into());
        }
        step(sink, "Stopping the gateway service");
        let _ = crate::sys::output("sc.exe", &["stop", SERVICE]);
        for _ in 0..30 {
            if !status().running {
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        let pd = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
        let data = pd.join("Sentient Gateway");
        let kept = pd.join(format!("Sentient Gateway kept {}", chrono::Local::now().format("%Y-%m-%d %H%M%S")));
        if data.is_dir() {
            if keep_data {
                std::fs::rename(&data, &kept).map_err(|e| format!("keep {}: {e}", data.display()))?;
            }
        }
        step(sink, "Removing the Sentient Gateway");
        let uninstaller = dir.join("unins000.exe");
        if uninstaller.is_file() {
            match crate::sys::output_limited(&uninstaller.to_string_lossy(), &["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"], std::time::Duration::from_secs(300)) {
                Some((true, _, _)) => {}
                _ => return Err("The gateway's uninstaller stopped.".into()),
            }
        } else {
            let _ = crate::sys::output("sc.exe", &["delete", SERVICE]);
            let _ = std::fs::remove_dir_all(&dir);
        }
        if !keep_data && data.is_dir() {
            let _ = std::fs::remove_dir_all(&data);
        }
        Ok(if keep_data && kept.is_dir() {
            format!("The Sentient Gateway is removed. Its configuration and data are kept in {}.", kept.display())
        } else {
            "The Sentient Gateway is removed.".to_string()
        })
    }

    pub fn program() -> String {
        crate::sys::output("sc.exe", &["qc", SERVICE])
            .and_then(|(ok, out, _)| ok.then(|| service_program(&crate::sys::decode(&out))).flatten())
            .filter(|p| std::path::Path::new(p).is_file())
            .unwrap_or_else(|| exe().display().to_string())
    }

    fn service_pid() -> Option<u32> {
        let (_, out, _) = crate::sys::output("sc.exe", &["queryex", SERVICE])?;
        crate::sys::decode(&out).lines().find_map(|l| l.trim().strip_prefix("PID").map(|r| r.trim_start_matches([' ', ':']).trim().to_string())).and_then(|p| p.parse().ok()).filter(|p| *p != 0)
    }

    /// The service's settings: its file and its registry Environment.
    pub fn read_settings() -> crate::gateway_settings::GwSettings {
        use crate::gateway_settings::{self as gs, GwSettings};
        let file = config_file();
        let dir = file.parent().map(PathBuf::from).unwrap_or_default();
        let text = std::fs::read_to_string(&file).ok().or_else(|| std::fs::read_to_string(dir.join("tb_gateway.json")).ok());
        let mut s = match text {
            Some(t) => gs::from_files(&t, std::fs::read_to_string(dir.join("logs.json")).ok().as_deref()),
            None => GwSettings { source: "none".into(), note: Some("The gateway has no settings file yet (not commissioned).".into()), ..GwSettings::default() },
        };
        s.config_path = Some(file.display().to_string());
        s.ui.password_set = Some(dir.join("local-ui").join("admin.argon2").is_file());
        let program = program();
        s.caps = gs::Caps::of_help(&crate::sys::output(&program, &["--help"]).map(|(_, o, _)| crate::sys::decode(&o)).unwrap_or_default());
        if s.caps.status_json {
            if let Some((true, o, _)) = crate::sys::output(&program, &["--status-json"]) {
                gs::merge_status_json(&mut s, &crate::sys::decode(&o));
            }
        }
        let env = service_environment();
        gs::apply_env(&mut s, &env, false);
        if s.connected.is_none() {
            match (service_pid(), s.conn.port) {
                (None, _) => {
                    s.connected = Some(false);
                    s.connected_how = Some("the service is stopped".into());
                }
                (Some(pid), Some(port)) => {
                    if let Some((true, o, _)) = crate::sys::output("netstat.exe", &["-ano", "-p", "TCP"]) {
                        s.connected = Some(gs::netstat_established(&crate::sys::decode(&o), pid, port));
                        s.connected_how = Some("the gateway's open connections".into());
                    }
                }
                _ => {}
            }
        }
        let pd = std::env::var("ProgramData").ok();
        s.log_files = Some(windows_log_dir(&env, pd.as_deref()));
        let failure = crate::sys::output("sc.exe", &["qfailure", SERVICE]).map(|(_, o, _)| crate::sys::decode(&o)).unwrap_or_default();
        let restarts = failure.to_ascii_uppercase().contains("RESTART");
        s.machine.restart_always = Some(restarts);
        s.machine.restart_word = Some(if restarts { "restart after a failure".into() } else { "no".into() });
        s.machine.env = gs::overrides(&env, &[], |_| true, "the service");
        s
    }

    fn write_environment(env: &[String]) -> Result<(), String> {
        let key = format!(r"HKLM\SYSTEM\CurrentControlSet\Services\{SERVICE}");
        if env.is_empty() {
            let _ = crate::sys::output("reg.exe", &["delete", &key, "/v", "Environment", "/f"]);
            return Ok(());
        }
        let data = env.join("\\0");
        match crate::sys::output("reg.exe", &["add", &key, "/v", "Environment", "/t", "REG_MULTI_SZ", "/d", &data, "/f"]) {
            Some((true, _, _)) => Ok(()),
            Some((false, _, e)) => Err(format!("The service's environment could not be written: {}", crate::sys::decode(&e).trim())),
            None => Err("reg.exe could not be started.".into()),
        }
    }

    /// Apply `plan` to the service: the environment in the service's
    /// registry key and the restart policy (`sc failure`) first, then the
    /// gateway restarts once: through its own `--install` when the page
    /// changes (it restarts the service), else a service restart.
    pub fn apply_settings(sink: &ProgressFn, before: &crate::gateway_settings::GwSettings, plan: &crate::gateway_settings::ApplyPlan) -> Result<(), String> {
        let program = program();
        let page = plan.ui_enabled.is_some() || plan.ui_listen.is_some();
        if page && !before.caps.ui_listen {
            return Err("This gateway is older than 4.12: update it first to change its web page here.".into());
        }
        let mut env = service_environment();
        let conn = plan.conn_host.is_some() || plan.conn_port.is_some();
        let conn_env = conn && env.iter().any(|l| ["host=", "ST_GW_HOST=", "TB_GW_HOST=", "port=", "ST_GW_PORT=", "TB_GW_PORT="].iter().any(|p| l.starts_with(p)));
        let mut set = |n: &str, v: Option<String>| {
            env.retain(|l| !l.starts_with(&format!("{n}=")));
            if let Some(v) = v {
                env.push(format!("{n}={v}"));
            }
        };
        let mut env_changed = false;
        // The connection: written by the gateway's own --install (below);
        // an override of it in the service's environment goes.
        if conn_env {
            for k in ["host", "ST_GW_HOST", "TB_GW_HOST", "port", "ST_GW_PORT", "TB_GW_PORT"] {
                set(k, None);
            }
            env_changed = true;
        }
        if page && before.machine.ui_env {
            if let Some(on) = plan.ui_enabled {
                set("ST_GW_UI", Some(if on { "on" } else { "off" }.into()));
            }
            if let Some(l) = &plan.ui_listen {
                set("ST_GW_UI_LISTEN", Some(l.clone()));
            }
            env_changed = true;
        }
        if let Some(l) = &plan.rust_log {
            set("RUST_LOG", (!l.is_empty()).then(|| l.clone()));
            env_changed = true;
        }
        if let Some(vars) = &plan.env {
            for e in before.machine.env.iter().filter(|e| e.value.is_some()) {
                set(&e.name, None);
            }
            for (n, v) in vars {
                if n.is_empty() || n.contains('=') {
                    return Err(format!("{n}: not a usable variable"));
                }
                set(n, Some(v.clone()));
            }
            env_changed = true;
        }
        if env_changed {
            step(sink, "Writing the service's environment");
            write_environment(&env)?;
        }
        if let Some(on) = plan.restart_always {
            step(sink, "Setting what Windows does when the gateway fails");
            let args: Vec<&str> = if on { vec!["failure", SERVICE, "reset=", "86400", "actions=", "restart/5000/restart/5000/restart/5000"] } else { vec!["failure", SERVICE, "reset=", "0", "actions=", ""] };
            if !matches!(crate::sys::output("sc.exe", &args), Some((true, _, _))) {
                return Err("sc.exe failure could not set the restart policy.".into());
            }
        }
        if page || conn {
            step(sink, "Writing the settings and restarting the gateway once (its own --install)");
            let mut args = vec!["--install".to_string()];
            if let Some(h) = &plan.conn_host {
                args.push("--host".into());
                args.push(h.clone());
            }
            if let Some(p) = plan.conn_port {
                args.push("--port".into());
                args.push(p.to_string());
            }
            match plan.ui_enabled {
                Some(true) => args.push("--ui".into()),
                Some(false) => args.push("--no-ui".into()),
                None => {}
            }
            if let Some(l) = &plan.ui_listen {
                args.push("--ui-listen".into());
                args.push(l.clone());
            }
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            match crate::sys::output_limited(&program, &refs, std::time::Duration::from_secs(120)) {
                Some((true, _, _)) => {}
                Some((false, _, e)) => return Err(format!("The gateway's --install stopped: {}", crate::sys::decode(&e).trim())),
                None => return Err("The gateway's program could not be started.".into()),
            }
        } else if env_changed {
            step(sink, "Restarting the gateway service once");
            control(sink, "restart")?;
        }
        Ok(())
    }

    pub fn test_creds() -> Option<crate::gateway_settings::TestCreds> {
        let file = std::fs::read_to_string(config_file()).ok();
        crate::gateway_settings::creds_of(file.as_deref(), &service_environment())
    }

    fn download(url: &str, to: &std::path::Path) -> Result<(), String> {
        let response = crate::net::agent()
            .get(url)
            .set("User-Agent", "SENTIENT-Platform-Manager")
            .call()
            .map_err(|e| format!("download {url}: {e}"))?;
        let mut reader = response.into_reader();
        let mut out = std::fs::File::create(to).map_err(|e| format!("create {}: {e}", to.display()))?;
        std::io::copy(&mut reader, &mut out).map_err(|e| format!("download {url}: {e}"))?;
        Ok(())
    }

    fn fetch_text(url: &str) -> Result<String, String> {
        crate::net::agent()
            .get(url)
            .set("User-Agent", "SENTIENT-Platform-Manager")
            .call()
            .map_err(|e| format!("download {url}: {e}"))?
            .into_string()
            .map_err(|e| format!("read {url}: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The installer is checked against its .sha256 file, else GitHub's digest.
    #[test]
    fn the_installers_checksum_comes_from_its_file_else_the_digest() {
        let h = "a".repeat(64);
        let file = format!("{}  SentientGatewaySetup-4.16.3.exe\n", h.to_ascii_uppercase());
        assert_eq!(installer_sha256(Some(&file), Some("sha256:bbbb")), Some(h.clone()));
        assert_eq!(installer_sha256(None, Some(&format!("sha256:{h}"))), Some(h.clone()));
        assert_eq!(installer_sha256(Some("  \n"), Some(&format!("sha256:{h}"))), Some(h));
        assert_eq!(installer_sha256(None, Some("md5:abc")), None);
        assert_eq!(installer_sha256(None, None), None);
    }

    #[test]
    fn windows_service_program() {
        let qc = "SERVICE_NAME: SentientGateway\r\n        TYPE               : 10  WIN32_OWN_PROCESS\r\n        BINARY_PATH_NAME   : \"C:\\Program Files\\Sentient Gateway\\sentient-gateway.exe\" --service\r\n";
        let p = service_program(qc).unwrap();
        assert_eq!(p, "C:\\Program Files\\Sentient Gateway\\sentient-gateway.exe");
        assert!(is_gateway_program(&p));
        let other = "        BINARY_PATH_NAME   : C:\\tools\\other-gatewayd.exe -x\r\n";
        assert!(!is_gateway_program(&service_program(other).unwrap()));
    }

    #[test]
    fn version_line() {
        assert_eq!(parse_version("sentient-gateway 4.13.0\n").as_deref(), Some("4.13.0"));
        assert_eq!(parse_version("something else"), None);
    }

    #[test]
    fn page_address_from_the_settings() {
        let none = |_: &str| None;
        assert_eq!(page_url(None, &none, false).as_deref(), Some("http://127.0.0.1:8642/"));
        assert_eq!(page_url(None, &none, true), None);
        let https = r#"{"localUi":{"enabled":true,"listen":"0.0.0.0:9443"}}"#;
        assert_eq!(page_url(Some(https), &none, false).as_deref(), Some("https://127.0.0.1:9443/"));
        let lan = r#"{"localUi":{"listen":"192.168.1.20:8642"}}"#;
        assert_eq!(page_url(Some(lan), &none, false).as_deref(), Some("https://192.168.1.20:8642/"));
        let off = r#"{"localUi":{"enabled":false}}"#;
        assert_eq!(page_url(Some(off), &none, false), None);
        let on = |k: &str| (k == "ST_GW_UI").then(|| "on".to_string());
        assert_eq!(page_url(None, &on, true).as_deref(), Some("https://127.0.0.1:8642/"));
        let port = |k: &str| (k == "ST_GW_UI_LISTEN").then(|| "127.0.0.1:9000".to_string());
        assert_eq!(page_url(None, &port, false).as_deref(), Some("http://127.0.0.1:9000/"));
    }

    #[test]
    fn gateway_images() {
        assert!(is_gateway_image("quakestring/sentient-gateway-rs:4.13.0"));
        // The older Python gateway: not managed, only reported.
        assert!(!is_gateway_image("quakestring/sentient-gateway:3.7.10"));
        assert!(is_legacy_image("quakestring/sentient-gateway:3.7.10"));
        assert!(!is_legacy_image("quakestring/sentient-gateway-rs:4.16.2"));
        assert!(is_gateway_image("sentient-gateway-rs:dev"));
        assert!(is_gateway_image("registry.local:5000/quakestring/sentient-gateway-rs:4.16.2"));
        assert!(!is_gateway_image("example/other-gateway"));
        assert!(!is_gateway_image("kong/kong-gateway:3.8"));
        assert!(!is_gateway_image("my-sentient-gateway:1"));
        assert!(!is_gateway_image("quakestring/sentient:4.3.0.53"));
        assert!(!is_gateway_image("postgres:18"));
    }

    #[test]
    fn host_from_the_gateway_file() {
        let file = r#"{"sentient":{"host":"127.0.0.1","port":1883,"security":{"type":"accessToken"}}}"#;
        assert_eq!(host_of(file), (Some("127.0.0.1".into()), Some(1883)));
        assert_eq!(host_of(r#"{"sentient":{"host":""}}"#), (None, None));
    }

    #[test]
    fn update_reasons_say_what_is_true() {
        let native = Status { installed: true, service: Some("sentient-gateway.service".into()), program: Some("/usr/bin/sentient-gateway".into()), package: Some("sentient-gateway".into()), ..Status::default() };
        let why = update_blocker(&native, "4.16.3", false, false).unwrap();
        assert!(why.contains("package manager") && why.contains("gateway-4.16.3") && !why.contains("compose"), "{why}");
        let by_hand = Status { package: None, service: Some("example-gw.service".into()), ..native.clone() };
        let why = update_blocker(&by_hand, "4.16.3", false, false).unwrap();
        assert!(why.contains("example-gw.service") && why.contains("--install") && !why.contains("compose"), "{why}");
        let process = Status { installed: true, process: Some("/opt/gw/sentient-gateway".into()), ..Status::default() };
        assert!(update_blocker(&process, "4.16.3", false, false).unwrap().contains("plain program"));
        // Docker: ours updates; a compose one names its folder; another is told plainly.
        let ours = Status { installed: true, container: Some(CONTAINER.into()), managed: true, ..Status::default() };
        assert_eq!(update_blocker(&ours, "4.16.3", false, false), None);
        assert!(update_blocker(&ours, "4.16.3", false, true).unwrap().contains("Windows service"));
        let compose = Status { managed: false, container: Some("plant-gw".into()), compose_dir: Some("/srv/example".into()), ..ours.clone() };
        assert!(update_blocker(&compose, "4.16.3", false, false).unwrap().contains("compose file in /srv/example"));
        let other = Status { compose_dir: None, ..compose };
        let why = update_blocker(&other, "4.16.3", false, false).unwrap();
        assert!(why.contains("outside the Platform Manager") && !why.contains("compose"), "{why}");
        // The Windows service: needs the release's installer.
        let win = Status { installed: true, managed: true, ..Status::default() };
        assert_eq!(win.kind(), "windows-service");
        assert_eq!(update_blocker(&win, "4.16.3", true, true), None);
        assert!(update_blocker(&win, "4.16.3", false, true).unwrap().contains("no Windows installer"));
        assert!(update_blocker(&Status::default(), "4.16.3", true, false).is_some());
        // The older Python gateway alone: an update here, not on Windows.
        let older = Status { legacy: Some("st-gateway".into()), legacy_image: Some("quakestring/sentient-gateway:latest".into()), ..Status::default() };
        assert_eq!(update_blocker(&older, "4.16.3", false, false), None);
        assert!(update_blocker(&older, "4.16.3", false, true).unwrap().contains("Windows service"));
    }

    #[test]
    fn a_release_without_its_image_is_not_offered_to_docker() {
        let r = Release {
            tag_name: "gateway-4.16.3".into(),
            name: None,
            body: Some("Faster polling.".into()),
            html_url: String::new(),
            prerelease: false,
            draft: false,
            published_at: Some("2026-11-18T10:00:00Z".into()),
            assets: vec![],
        };
        let all = vec![r, Release { tag_name: "gateway-4.16.2".into(), ..Release::clone(&all_one()) }];
        let mut offer = newest_in(&all, releases::Channel::Stable).unwrap();
        assert_eq!(offer.version, "4.16.3");
        offer.image_missing = true;
        let ours = Status { installed: true, container: Some(CONTAINER.into()), managed: true, ..Status::default() };
        assert_eq!(offer_blocker(&ours, &offer, false).as_deref(), Some("4.16.3 is released; its Docker image is not published yet."));
        // The older Python gateway goes to the image too.
        let older = Status { legacy: Some("st-gateway".into()), ..Status::default() };
        assert_eq!(offer_blocker(&older, &offer, false).as_deref(), Some("4.16.3 is released; its Docker image is not published yet."));
        // A compose container: the image first, not "set it in the file".
        let compose = Status { managed: false, compose_dir: Some("/srv/example".into()), ..ours.clone() };
        assert!(offer_blocker(&compose, &offer, false).unwrap().contains("not published"));
        // A Windows service does not use the image; on Windows the Docker reason stays.
        let win = Status { installed: true, managed: true, ..Status::default() };
        assert!(offer_blocker(&win, &offer, true).unwrap().contains("no Windows installer"));
        assert!(offer_blocker(&ours, &offer, true).unwrap().contains("Windows service"));
        // Published: the usual answer.
        offer.image_missing = false;
        assert_eq!(offer_blocker(&ours, &offer, false), None);
    }

    fn all_one() -> Release {
        Release { tag_name: String::new(), name: None, body: None, html_url: String::new(), prerelease: false, draft: false, published_at: None, assets: vec![] }
    }

    #[test]
    fn the_windows_log_folder_follows_the_gateway() {
        assert_eq!(windows_log_dir(&[], Some(r"C:\ProgramData")), r"C:\ProgramData\Sentient Gateway\data\logs");
        assert_eq!(windows_log_dir(&[], None), r"C:\ProgramData\Sentient Gateway\data\logs");
        assert_eq!(windows_log_dir(&["ST_GW_DATA_DIR=D:\\gw\\data\\".into()], Some(r"C:\ProgramData")), r"D:\gw\data\logs");
        assert_eq!(windows_log_dir(&["TB_GW_LOGS_PATH=E:\\logs".into(), "ST_GW_DATA_DIR=D:\\x".into()], None), r"E:\logs");
        // An empty value counts as unset.
        assert_eq!(windows_log_dir(&["ST_GW_LOGS_PATH=".into()], Some(r"C:\PD")), r"C:\PD\Sentient Gateway\data\logs");
    }

    #[test]
    fn log_files_merge_in_time_order() {
        let service = "2026-11-18T07:00:01.000001Z  INFO gateway: started\n2026-11-18T07:00:05.000000Z  WARN gateway: slow\n  continued line\n".to_string();
        let connector = "2026-11-18T07:00:03.000000Z  INFO connector_modbus: polling\n".to_string();
        let all = merge_logs(&[service.clone(), connector.clone()], 10, None);
        assert_eq!(all, "2026-11-18T07:00:01.000001Z  INFO gateway: started\n2026-11-18T07:00:03.000000Z  INFO connector_modbus: polling\n2026-11-18T07:00:05.000000Z  WARN gateway: slow\n  continued line");
        // The last record keeps its continuation.
        assert_eq!(merge_logs(&[service.clone(), connector.clone()], 1, None), "2026-11-18T07:00:05.000000Z  WARN gateway: slow\n  continued line");
        assert!(merge_logs(&[service, connector], 10, Some("2026-11-18T07:00:02")).starts_with("2026-11-18T07:00:03"));
        let dir = std::env::temp_dir().join(format!("spm-gw-logs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("service.log"), "2026-11-18T07:00:01.000000Z  INFO a\n").unwrap();
        std::fs::write(dir.join("service.log.1"), "2026-11-17T07:00:01.000000Z  INFO old\n").unwrap();
        assert_eq!(read_log_dir(&dir, 10, None).as_deref(), Some("2026-11-18T07:00:01.000000Z  INFO a"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
