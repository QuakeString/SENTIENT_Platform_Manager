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
pub const IMAGE: &str = "quakestring/sentient-gateway-rs";
pub const CONTAINER: &str = "sentient-gateway";
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

/// A gateway image: the Rust gateway, or the Python one it replaces.
pub fn is_gateway_image(image: &str) -> bool {
    let repo = image.rsplit_once(':').map(|(r, t)| if t.contains('/') { image } else { r }).unwrap_or(image);
    repo.ends_with("sentient-gateway-rs") || repo.ends_with("sentient-gateway") || repo.ends_with("tb-gateway")
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
}

/// The gateway's releases in the SENTIENT releases repository.
pub fn is_gateway(r: &Release) -> bool {
    r.tag_name.starts_with("gateway-")
}

/// `gateway-4.13.0` → `4.13.0`.
pub fn version_of(r: &Release) -> String {
    r.tag_name.trim_start_matches("gateway-").to_string()
}

/// The newest gateway release on `channel` (any, when nothing is installed).
pub fn newest(channel: releases::Channel) -> Result<Option<Offer>, String> {
    let all = releases::fetch(releases::SENTIENT_REPO)?;
    let mut candidates = releases::on_channel(&all, channel, is_gateway);
    // on_channel sorts by the tag; gateway-4.10 vs 4.9 needs the version.
    candidates.sort_by(|a, b| releases::compare(&version_of(b), &version_of(a)));
    Ok(candidates.first().map(|r| {
        let v = version_of(r);
        let installer = r.assets.iter().find(|a| a.name == format!("SentientGatewaySetup-{v}.exe")).cloned();
        let checksum = r.assets.iter().find(|a| a.name == format!("SentientGatewaySetup-{v}.exe.sha256")).cloned();
        Offer { version: v, date: r.date(), notes: r.notes(), page: r.html_url.clone(), installer, checksum }
    }))
}

pub fn status() -> Status {
    #[cfg(windows)]
    {
        // The service first; else a gateway container in Docker Desktop.
        let s = windows::status();
        if s.installed { s } else { crate::distro::gateway_docker::status() }
    }
    #[cfg(not(windows))]
    {
        crate::distro::gateway_docker::status()
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
        crate::distro::gateway_docker::update(sink, &offer.version)
    }
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
        crate::distro::gateway_docker::control(sink, action)
    }
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
        let installed = matches!(query, Some((true, _, _))) || exe().exists();
        if !installed {
            return Status::default();
        }
        let running = matches!(&query, Some((true, out, _)) if crate::sys::decode(out).contains("RUNNING"));
        let version = crate::sys::output(&exe().to_string_lossy(), &["--version"])
            .and_then(|(_, out, _)| parse_version(&crate::sys::decode(&out)));
        let file = std::fs::read_to_string(config_file()).ok();
        let (host, port) = file.as_deref().map(host_of).unwrap_or((None, None));
        // The service's own environment (ST_GW_UI, ST_GW_UI_LISTEN) wins
        // over the file, as it does in the gateway.
        let env = service_environment();
        let page_url = page_url(file.as_deref(), &|k| env.iter().find_map(|l| l.strip_prefix(&format!("{k}=")).map(str::to_string)), false);
        Status { installed, version, running, host, port, page_url, container: None, managed: true }
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
        let file = dir.join(&asset.name);
        step(sink, &format!("Downloading Sentient Gateway {} ({} MB)", offer.version, asset.size >> 20));
        download(&asset.browser_download_url, &file)?;
        if let Some(sum) = &offer.checksum {
            let expected = fetch_text(&sum.browser_download_url)?
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let actual = crate::manifest::sha256_file(&file)?;
            if expected != actual {
                let _ = std::fs::remove_file(&file);
                return Err(format!("The gateway installer did not match its checksum ({actual} instead of {expected})."));
            }
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
                std::fs::write(&f, &s.token).map_err(|e| format!("write the token file: {e}"))?;
                args.push(format!("/TOKENFILE={}", f.display()));
                secrets.push(f);
            }
            if !s.ui_password.is_empty() {
                let f = dir.join("page.txt");
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
        match status().version {
            Some(v) if v == offer.version => {
                sink(Progress::Done { message: format!("Sentient Gateway {v} is installed.") });
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
        assert!(is_gateway_image("quakestring/sentient-gateway:3.7.10"));
        assert!(is_gateway_image("thingsboard/tb-gateway"));
        assert!(!is_gateway_image("quakestring/sentient:4.3.0.53"));
        assert!(!is_gateway_image("postgres:18"));
    }

    #[test]
    fn host_from_the_gateway_file() {
        let file = r#"{"sentient":{"host":"127.0.0.1","port":1883,"security":{"type":"accessToken"}}}"#;
        assert_eq!(host_of(file), (Some("127.0.0.1".into()), Some(1883)));
        assert_eq!(host_of(r#"{"sentient":{"host":""}}"#), (None, None));
    }
}
