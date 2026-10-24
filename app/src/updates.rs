// Copyright © 2016-2026 The SENTIENT Authors

//! Checking for updates: SENTIENT and the Platform Manager itself, on the
//! channel the operator chose (sentient_installer_core::releases), by hand
//! from the Update screen and once a day while the manager is open.
//!
//! What was found is shared by the Update screen and the notice bar above
//! every screen ([`UpdateState`]). A notice the operator dismisses stays
//! away for that version only: the next one shows again.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui_kit::*;
use serde::{Deserialize, Serialize};

use sentient_installer_core::manifest;
use sentient_installer_core::progress::{Progress, ProgressFn};
use sentient_installer_core::releases::{self, Channel, Release};

/// How often the daily check runs, and how often the manager looks whether
/// it is due.
const DAILY: Duration = Duration::from_secs(24 * 3600);
const TICK: Duration = Duration::from_secs(15 * 60);

/// This program's version: the release tag CI builds it from
/// (`SPM_VERSION`, e.g. `0.8.0-rc6`), else the crate's.
pub fn manager_version() -> &'static str {
    option_env!("SPM_VERSION").filter(|v| !v.is_empty()).unwrap_or(env!("CARGO_PKG_VERSION"))
}

/// The operator's choices and what they dismissed, kept per computer
/// (Windows) or per user (Linux, where the manager runs as the user).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prefs {
    #[serde(default)]
    pub channel: Channel,
    /// Check once a day while the manager is open.
    #[serde(default = "yes")]
    pub daily: bool,
    #[serde(default)]
    pub dismissed_sentient: Option<String>,
    #[serde(default)]
    pub dismissed_manager: Option<String>,
    /// Unix seconds of the last check that reached the release site.
    #[serde(default)]
    pub last_check: u64,
}

fn yes() -> bool {
    true
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs { channel: Channel::Stable, daily: true, dismissed_sentient: None, dismissed_manager: None, last_check: 0 }
    }
}

fn prefs_path() -> PathBuf {
    #[cfg(windows)]
    {
        crate::mode::installation_record().with_file_name("updates.json")
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("sentient-platform-manager")
            .join("updates.json")
    }
}

impl Prefs {
    pub fn load() -> Prefs {
        std::fs::read_to_string(prefs_path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = prefs_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }

    fn due(&self) -> bool {
        self.daily && now().saturating_sub(self.last_check) >= DAILY.as_secs()
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A newer version on the channel.
#[derive(Debug, Clone)]
pub struct Offer {
    pub version: String,
    /// The release tag (`nightly` for a nightly), what downloads name.
    pub tag: String,
    pub date: String,
    pub notes: String,
    pub page: String,
    /// The Platform Manager's installer for this computer.
    pub installer: Option<releases::Asset>,
}

impl Offer {
    fn from(r: &Release, version: String) -> Offer {
        Offer { version, tag: r.tag_name.clone(), date: r.date(), notes: r.notes(), page: r.html_url.clone(), installer: None }
    }
}

/// One check's answer for each product: `Ok(None)` is "up to date".
#[derive(Debug, Clone)]
pub struct Found {
    pub sentient: Result<Option<Offer>, String>,
    pub manager: Result<Option<Offer>, String>,
    pub at: u64,
}

/// Look on the channel. Blocking: run it in the background.
/// `installed` is SENTIENT's version here (None when it is not installed);
/// `native` says whether this computer needs a package for its system.
pub fn check(channel: Channel, installed: Option<String>, native: bool) -> Found {
    let sentient = match installed {
        None => Ok(None),
        Some(installed) => sentient_offer(channel, &installed, native),
    };
    Found { sentient, manager: manager_offer(channel), at: now() }
}

fn sentient_offer(channel: Channel, installed: &str, native: bool) -> Result<Option<Offer>, String> {
    let all = releases::fetch(releases::SENTIENT_REPO)?;
    let candidates = releases::on_channel(&all, channel, releases::is_sentient);
    if candidates.is_empty() {
        return match channel {
            Channel::Nightly => Err("No nightly SENTIENT build is published yet.".into()),
            _ => Ok(None),
        };
    }
    // A release may carry only some systems' files: the newest that has
    // this computer's (its manifest says, and names the version a nightly is).
    for r in candidates.into_iter().take(5) {
        let version = if native || r.is_nightly() {
            let m = manifest::fetch(&manifest::release_url(&r.tag_name))?;
            if native && m.wanted().is_err() {
                continue;
            }
            m.version
        } else {
            r.version()
        };
        return Ok(releases::is_newer(&version, installed, r.is_nightly()).then(|| Offer::from(r, version)));
    }
    Ok(None)
}

fn manager_offer(channel: Channel) -> Result<Option<Offer>, String> {
    let all = releases::fetch(releases::MANAGER_REPO)?;
    let candidates = releases::on_channel(&all, channel, |_| true);
    let Some(r) = candidates.first() else {
        return match channel {
            Channel::Nightly => Err("No nightly Platform Manager build is published yet.".into()),
            _ => Ok(None),
        };
    };
    let version = r.version();
    if !releases::is_newer(&version, manager_version(), r.is_nightly()) {
        return Ok(None);
    }
    let mut offer = Offer::from(r, version);
    offer.installer = r.assets.iter().find(|a| installer_for_this_system(&a.name)).cloned();
    Ok(Some(offer))
}

fn installer_for_this_system(name: &str) -> bool {
    if !name.starts_with("SENTIENT-Setup-manager") {
        return false;
    }
    if cfg!(windows) {
        name.ends_with(".exe")
    } else {
        name.ends_with("-linux-x86_64") && cfg!(target_arch = "x86_64")
    }
}

/// Download the Platform Manager `offer` and hand over to it: it replaces
/// this program and starts the manager again (`--self-update`). Returns
/// once the new program runs; the caller then quits.
pub fn install_manager(offer: &Offer, sink: &ProgressFn) -> Result<(), String> {
    #[cfg(not(windows))]
    if crate::self_install::installed_by_package_manager() {
        return Err("This Platform Manager was installed with pacman: update it with pacman (the new package is on the release page).".into());
    }
    let asset = offer
        .installer
        .clone()
        .ok_or_else(|| format!("Platform Manager {} has no installer for this computer.", offer.version))?;
    sink(Progress::Step { name: format!("Downloading Platform Manager {}", offer.version) });
    let dir = std::env::temp_dir().join(format!("sentient-manager-{}", offer.version));
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let file = dir.join(&asset.name);
    let response = sentient_installer_core::net::agent()
        .get(&asset.browser_download_url)
        .set("User-Agent", "SENTIENT-Platform-Manager")
        .call()
        .map_err(|e| format!("download {}: {e}", asset.name))?;
    let mut reader = response.into_reader();
    let mut out = std::fs::File::create(&file).map_err(|e| format!("create {}: {e}", file.display()))?;
    let copied = std::io::copy(&mut reader, &mut out).map_err(|e| format!("download {}: {e}", asset.name))?;
    drop(out);
    if asset.size > 0 && copied != asset.size {
        return Err(format!("The download of {} is incomplete ({copied} of {} bytes).", asset.name, asset.size));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755));
    }
    sink(Progress::Step { name: format!("Starting Platform Manager {}", offer.version) });
    std::process::Command::new(&file)
        .arg("--self-update")
        .spawn()
        .map_err(|e| format!("start {}: {e}", file.display()))?;
    Ok(())
}

/// `--self-update`, run by the downloaded program: put itself where the
/// manager lives and start it there.
pub fn finish_self_update() -> i32 {
    // The previous manager quits once this has started.
    std::thread::sleep(Duration::from_secs(2));
    match crate::self_install::install_manager() {
        Ok(path) => {
            #[cfg(windows)]
            crate::self_install::register_manager_entry(&path);
            let _ = std::process::Command::new(&path).spawn();
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// What the manager knows about updates, shared by its screens.
pub struct UpdateState {
    pub prefs: Prefs,
    pub found: Option<Found>,
    pub checking: bool,
    installed: Option<String>,
    native: bool,
    _tasks: Vec<Task<()>>,
}

impl UpdateState {
    pub fn new(installed: Option<String>, native: bool, cx: &mut Context<Self>) -> Self {
        let mut state = Self { prefs: Prefs::load(), found: None, checking: false, installed, native, _tasks: Vec::new() };
        // Due now (start-up), and every quarter of an hour whether a day passed.
        let ticker = cx.spawn(async move |this, cx| loop {
            let due = this.update(cx, |s, _| s.prefs.due() && !s.checking).unwrap_or(false);
            if due {
                this.update(cx, |s, cx| s.check(cx)).ok();
            }
            cx.background_executor().timer(TICK).await;
        });
        state._tasks.push(ticker);
        state
    }

    pub fn check(&mut self, cx: &mut Context<Self>) {
        if self.checking {
            return;
        }
        self.checking = true;
        cx.notify();
        let (channel, installed, native) = (self.prefs.channel, self.installed.clone(), self.native);
        let task = cx.spawn(async move |this, cx| {
            let found = cx.background_executor().spawn(async move { check(channel, installed, native) }).await;
            this.update(cx, |s, cx| {
                s.checking = false;
                if found.sentient.is_ok() || found.manager.is_ok() {
                    s.prefs.last_check = found.at;
                    s.prefs.save();
                }
                s.found = Some(found);
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    pub fn set_channel(&mut self, channel: Channel, cx: &mut Context<Self>) {
        if self.prefs.channel != channel {
            self.prefs.channel = channel;
            self.prefs.save();
            self.found = None;
            self.check(cx);
        }
    }

    pub fn set_daily(&mut self, daily: bool, cx: &mut Context<Self>) {
        self.prefs.daily = daily;
        self.prefs.save();
        cx.notify();
    }

    /// The SENTIENT the channel offers, if any.
    pub fn sentient(&self) -> Option<&Offer> {
        self.found.as_ref().and_then(|f| f.sentient.as_ref().ok()).and_then(Option::as_ref)
    }

    pub fn manager(&self) -> Option<&Offer> {
        self.found.as_ref().and_then(|f| f.manager.as_ref().ok()).and_then(Option::as_ref)
    }

    /// The notices to show above the screens: not dismissed for that version.
    pub fn notices(&self) -> (Option<Offer>, Option<Offer>) {
        let s = self.sentient().filter(|o| self.prefs.dismissed_sentient.as_deref() != Some(&o.version)).cloned();
        let m = self.manager().filter(|o| self.prefs.dismissed_manager.as_deref() != Some(&o.version)).cloned();
        (s, m)
    }

    pub fn dismiss_sentient(&mut self, version: String, cx: &mut Context<Self>) {
        self.prefs.dismissed_sentient = Some(version);
        self.prefs.save();
        cx.notify();
    }

    pub fn dismiss_manager(&mut self, version: String, cx: &mut Context<Self>) {
        self.prefs.dismissed_manager = Some(version);
        self.prefs.save();
        cx.notify();
    }

    pub fn last_checked(&self) -> String {
        if self.prefs.last_check == 0 {
            return "never".into();
        }
        chrono::DateTime::from_timestamp(self.prefs.last_check as i64, 0)
            .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default()
    }
}
