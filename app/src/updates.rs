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

#[cfg(feature = "gui")]
use gpui_kit::*;
use serde::{Deserialize, Serialize};

use sentient_installer_core::manifest;
use sentient_installer_core::progress::{Progress, ProgressFn};
use sentient_installer_core::gateway;
use sentient_installer_core::releases::{self, Channel, Release};
use sentient_installer_core::signing::{self, Product};

/// How often the daily check runs, and how often the manager looks whether
/// it is due.
const DAILY: Duration = Duration::from_secs(24 * 3600);
const TICK: Duration = Duration::from_secs(15 * 60);

/// This program's version: the release tag CI builds it from
/// (`SPM_VERSION`, e.g. `0.8.0-rc6`), else the crate's.
/// The commit this build is from, short (build.rs).
pub fn build_commit() -> &'static str {
    env!("SPM_BUILD_COMMIT")
}

pub fn manager_version() -> &'static str {
    // Development aid: act as another version (screens that compare them).
    #[cfg(debug_assertions)]
    {
        static DEBUG: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
        if let Some(v) = DEBUG.get_or_init(|| std::env::var("SPM_DEBUG_VERSION").ok().filter(|v| !v.is_empty())) {
            return v.as_str();
        }
    }
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
    #[serde(default)]
    pub dismissed_gateway: Option<String>,
    /// Unix seconds of the last check that reached the release site.
    #[serde(default)]
    pub last_check: u64,
    /// "light", "dark" or "system" (the default).
    #[serde(default)]
    pub theme: Option<String>,
}

fn yes() -> bool {
    true
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs { channel: Channel::Stable, daily: true, dismissed_sentient: None, dismissed_manager: None, dismissed_gateway: None, last_check: 0, theme: None }
    }
}

/// Where the manager keeps its own small files (this one, the last backup).
pub fn prefs_dir() -> PathBuf {
    prefs_path().parent().map(|p| p.to_path_buf()).unwrap_or_default()
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
    /// The installer's published SHA-256 (lower-case hex).
    pub sha256: Option<String>,
    /// The installer's signature file (`<installer>.minisig`).
    pub signature: Option<releases::Asset>,
    /// The release carries no signature: it is from before releases were
    /// signed (crate::signing's rule decides whether it may be used).
    pub unsigned: bool,
    /// The feed holds a signed release of this product: an unsigned one is
    /// then refused.
    pub signing_started: bool,
    /// The notes of every version from the installed one (left out) to this
    /// one, newest first: what updating brings ("What's new").
    pub between: Vec<crate::notes::Notes>,
}

impl Offer {
    fn from(r: &Release, version: String) -> Offer {
        Offer {
            version,
            tag: r.tag_name.clone(),
            date: r.date(),
            notes: r.summary(),
            page: r.html_url.clone(),
            installer: None,
            sha256: None,
            signature: None,
            unsigned: false,
            signing_started: false,
            between: Vec::new(),
        }
    }
}

/// The published SHA-256 of `asset`: GitHub's digest, else the checksum
/// table of the release notes (`| \`name\` | size | \`hex\` |`).
pub(crate) fn published_sha256(r: &Release, asset: &releases::Asset) -> Option<String> {
    if let Some(d) = asset.digest.as_deref().and_then(|d| d.strip_prefix("sha256:")) {
        return Some(d.to_lowercase());
    }
    let body = r.notes();
    body.lines().find(|l| l.contains(&format!("`{}`", asset.name))).and_then(|l| {
        l.split('`').map(str::trim).find(|p| p.len() == 64 && p.bytes().all(|b| b.is_ascii_hexdigit())).map(str::to_lowercase)
    })
}

/// A marker that this computer has seen a signed release of `product`: from
/// then on an unsigned one is refused even if the feed stops showing
/// signatures (crate::signing).
fn signed_seen_flag(product: Product) -> PathBuf {
    prefs_dir().join(match product {
        Product::Sentient => "signed-releases-seen-sentient",
        Product::Manager => "signed-releases-seen-manager",
    })
}

/// Whether signing has begun for `product`: the feed holds a signed release
/// of it, or this computer saw one before. Remembers a first sighting.
pub fn signing_started(all: Option<&[Release]>, product: Product) -> bool {
    let flag = signed_seen_flag(product);
    if flag.exists() {
        return true;
    }
    let in_feed = all.is_some_and(|all| signing::signing_started(all, product));
    if in_feed {
        if let Some(dir) = flag.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&flag, b"");
    }
    in_feed
}

/// One check's answer for each product: `Ok(None)` is "up to date".
#[derive(Debug, Clone)]
pub struct Found {
    pub sentient: Result<Option<Offer>, String>,
    pub manager: Result<Option<Offer>, String>,
    /// On Stable: a newer release candidate that the channel leaves out, so
    /// the screen can say one exists.
    pub sentient_candidate: Option<String>,
    pub manager_candidate: Option<String>,
    /// The newest gateway release on the channel (crate's Gateway screen
    /// installs or updates to it), and the gateway installed here.
    pub gateway: Result<Option<gateway::Offer>, String>,
    pub gateway_installed: Option<String>,
    pub at: u64,
}

/// Look on the channel. Blocking: run it in the background.
/// `installed` is SENTIENT's version here (None when it is not installed);
/// `native` says whether this computer needs a package for its system.
pub fn check(channel: Channel, installed: Option<String>, native: bool) -> Found {
    // One list serves both products: they share the repository.
    let all = releases::fetch(releases::SENTIENT_REPO);
    let (sentient, sentient_candidate) = match (&installed, &all) {
        (None, _) => (Ok(None), None),
        (Some(_), Err(e)) => (Err(e.clone()), None),
        (Some(v), Ok(all)) => {
            let offer = sentient_offer(all, channel, v, native);
            let candidate = (channel == Channel::Stable)
                .then(|| sentient_offer(all, Channel::Candidate, v, native).ok().flatten())
                .flatten()
                .filter(|c| offer.as_ref().ok().and_then(|o| o.as_ref()).is_none_or(|o| releases::compare(&c.version, &o.version).is_gt()))
                .map(|c| c.version);
            (offer, candidate)
        }
    };
    let (manager, manager_candidate) = match &all {
        Err(e) => (Err(e.clone()), None),
        Ok(all) => {
            let offer = newest_manager(all, channel, manager_version());
            let candidate = (channel == Channel::Stable)
                .then(|| newest_manager(all, Channel::Candidate, manager_version()))
                .flatten()
                .filter(|c| offer.as_ref().is_none_or(|o| releases::compare(&c.version, &o.version).is_gt()))
                .map(|c| c.version);
            (Ok(offer), candidate)
        }
    };
    let here = gateway::status();
    Found {
        sentient,
        manager,
        sentient_candidate,
        manager_candidate,
        gateway: gateway::newest(channel),
        gateway_installed: here.installed.then(|| here.version.unwrap_or_default()),
        at: now(),
    }
}

fn sentient_offer(all: &[Release], channel: Channel, installed: &str, native: bool) -> Result<Option<Offer>, String> {
    let candidates = releases::on_channel(all, channel, releases::is_sentient);
    if candidates.is_empty() {
        return match channel {
            Channel::Nightly => Err("No nightly SENTIENT build is published.".into()),
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
        if !releases::is_newer(&version, installed, r.is_nightly()) {
            return Ok(None);
        }
        let mut offer = Offer::from(r, version.clone());
        offer.unsigned = !r.has_asset(&signing::signature_name("manifest.json"));
        offer.signing_started = signing_started(Some(all), Product::Sentient);
        offer.between = if r.is_nightly() {
            vec![crate::notes::Notes::from_release(r, &version)]
        } else {
            crate::notes::between(all, installed, &version, |x| releases::is_sentient(x) && (channel != Channel::Stable || !x.prerelease), Release::version)
        };
        return Ok(Some(offer));
    }
    Ok(None)
}

/// The newest Platform Manager release on `channel` that is newer than
/// `installed`. No nightly manager is published: that channel gets the
/// release candidates.
fn newest_manager(all: &[Release], channel: Channel, installed: &str) -> Option<Offer> {
    let channel = if channel == Channel::Nightly { Channel::Candidate } else { channel };
    let mut candidates = releases::on_channel(all, channel, releases::is_manager);
    // on_channel sorts by the tag; rc10 against rc9 needs the version.
    candidates.sort_by(|a, b| releases::compare(&releases::manager_version_of(b), &releases::manager_version_of(a)));
    let r = candidates.first()?;
    let version = releases::manager_version_of(r);
    if !releases::is_newer(&version, installed, false) {
        return None;
    }
    let mut offer = Offer::from(r, version.clone());
    offer.installer = r.assets.iter().find(|a| installer_for_this_system(&a.name)).cloned();
    offer.sha256 = offer.installer.as_ref().and_then(|a| published_sha256(r, a));
    offer.signature = offer.installer.as_ref().and_then(|a| r.assets.iter().find(|s| s.name == signing::signature_name(&a.name)).cloned());
    offer.unsigned = offer.signature.is_none();
    offer.signing_started = signing_started(Some(all), Product::Manager);
    offer.between = crate::notes::between(all, installed, &version, |x| releases::is_manager(x) && (channel != Channel::Stable || !x.prerelease), releases::manager_version_of);
    Some(offer)
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

/// The steps "Update and restart" shows.
pub const SELF_UPDATE_STEPS: [&str; 4] =
    ["Checking the download address", "Downloading", "Checking the file", "Replacing the program and reopening"];

fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

/// Download the Platform Manager `offer` and hand over to it: it replaces
/// this program and starts the manager again (`--self-update`). Returns
/// once the new program runs; the caller then quits. The file is checked
/// against its published SHA-256 before anything is replaced; without one
/// it is not used.
pub fn install_manager(offer: &Offer, sink: &ProgressFn) -> Result<(), String> {
    #[cfg(not(windows))]
    if crate::self_install::installed_by_package_manager() {
        return Err("This Platform Manager was installed with pacman: update it with pacman (the new package is on the release page).".into());
    }
    let [address, downloading, checking, replacing] = SELF_UPDATE_STEPS;
    sink(Progress::Step { name: address.into() });
    let asset = offer
        .installer
        .clone()
        .ok_or_else(|| format!("Platform Manager {} has no installer for this computer. Nothing was changed.", offer.version))?;
    let expected = offer
        .sha256
        .clone()
        .ok_or_else(|| format!("Platform Manager {} has no published checksum, so its download cannot be checked. Nothing was changed.", offer.version))?;
    let dir = std::env::temp_dir().join(format!("sentient-manager-{}", offer.version));
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let file = dir.join(&asset.name);
    let response = sentient_installer_core::net::agent()
        .get(&asset.browser_download_url)
        .set("User-Agent", "SENTIENT-Platform-Manager")
        .call()
        .map_err(|e| format!("The download of Platform Manager {} could not start ({e}). Nothing was changed.", offer.version))?;
    sink(Progress::Step { name: downloading.into() });
    let mut reader = response.into_reader();
    let mut out = std::fs::File::create(&file).map_err(|e| format!("create {}: {e}", file.display()))?;
    let mut copied: u64 = 0;
    let mut buf = vec![0u8; 256 * 1024];
    let incomplete = |copied: u64| {
        format!(
            "The download of Platform Manager {} is incomplete ({} of {}). Nothing was changed.",
            offer.version,
            mb(copied),
            mb(asset.size)
        )
    };
    loop {
        use std::io::{Read as _, Write as _};
        let n = match reader.read(&mut buf) {
            Ok(n) => n,
            Err(_) => return Err(incomplete(copied)),
        };
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| format!("write {}: {e}", file.display()))?;
        copied += n as u64;
        if asset.size > 0 {
            sink(Progress::Percent { value: copied as f32 / asset.size as f32 });
            sink(Progress::Log { line: format!("Downloading {} of {}", mb(copied), mb(asset.size)) });
        }
    }
    drop(out);
    if asset.size > 0 && copied != asset.size {
        return Err(incomplete(copied));
    }
    sink(Progress::Step { name: checking.into() });
    let actual = manifest::sha256_file(&file)?;
    if actual != expected {
        let _ = std::fs::remove_file(&file);
        return Err(format!("The downloaded Platform Manager {} is damaged (its checksum does not match the published one). Nothing was changed.", offer.version));
    }
    // Its signature, by crate::signing's rule; refused before it is run.
    let refuse = |e: String| {
        let _ = std::fs::remove_file(&file);
        e
    };
    let signature = match &offer.signature {
        Some(a) => signing::fetch_signature(&a.browser_download_url).map_err(|e| refuse(format!("The signature of Platform Manager {}: {e}. Nothing was changed.", offer.version)))?,
        None => None,
    };
    let data = std::fs::read(&file).map_err(|e| refuse(format!("read {}: {e}", file.display())))?;
    let what = format!("Platform Manager {}", offer.version);
    let trust = signing::check(&what, &data, signature.as_deref(), offer.signing_started || signing_started(None, Product::Manager)).map_err(refuse)?;
    drop(data);
    if let Some(w) = trust.warning(&what) {
        sink(Progress::Log { line: w });
    }
    // Windows: the installer's Authenticode signature too, before it runs.
    #[cfg(windows)]
    if let Some(w) = crate::authenticode::check(&file).map_err(|e| refuse(format!("{what} is not used: {e}. Nothing was changed.")))? {
        sink(Progress::Log { line: format!("{what}: {w} (allowed until releases are code-signed).") });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755));
    }
    sink(Progress::Step { name: replacing.into() });
    std::process::Command::new(&file)
        .args(["--self-update", "--from", manager_version()])
        .spawn()
        .map_err(|e| format!("start {}: {e}", file.display()))?;
    Ok(())
}

/// `--self-update`, run by the downloaded program: put itself where the
/// manager lives and start it there, telling it which version it replaced
/// (`--updated-from`), so it can say so and list what it changes.
pub fn finish_self_update() -> i32 {
    let args: Vec<String> = std::env::args().collect();
    let from = args.windows(2).find(|w| w[0] == "--from").map(|w| w[1].clone());
    // The previous manager quits once this has started.
    std::thread::sleep(Duration::from_secs(2));
    match crate::self_install::install_manager() {
        Ok(path) => {
            #[cfg(windows)]
            {
                crate::self_install::register_manager_entry(&path);
                // The Start menu entry under its searchable name (SPM).
                crate::self_install::restore_menu_entry(&path);
            }
            crate::self_install::refresh_operator_screen(&sentient_installer_core::progress::noop());
            if let Some(mut record) = crate::installation::InstallationRecord::load() {
                record.manager_version = manager_version().into();
                if let Ok(json) = serde_json::to_string_pretty(&record) {
                    let _ = crate::self_install::write_record(&crate::installation::record_path(), &json);
                }
            }
            let mut cmd = std::process::Command::new(&path);
            cmd.arg("--manager");
            if let Some(from) = from {
                cmd.args(["--updated-from", &from]);
            }
            let _ = cmd.spawn();
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn found_flag() -> PathBuf {
    prefs_dir().join("found-installation")
}

/// Setup installed this Platform Manager beside a SENTIENT it found: the
/// manager says so once when it opens.
pub fn mark_found_installation() {
    let path = found_flag();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, "1");
}

/// Whether to say "Found SENTIENT…" now; the notice shows once.
pub fn take_found_installation() -> bool {
    std::fs::remove_file(found_flag()).is_ok()
}

/// What the manager knows about updates, shared by its screens.
#[cfg(feature = "gui")]
pub struct UpdateState {
    pub prefs: Prefs,
    pub found: Option<Found>,
    pub checking: bool,
    installed: Option<String>,
    native: bool,
    _tasks: Vec<Task<()>>,
}

#[cfg(feature = "gui")]
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
                if found.sentient.is_ok() || found.manager.is_ok() || found.gateway.is_ok() {
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

    /// A newer gateway than the one installed here, not dismissed.
    pub fn gateway_notice(&self) -> Option<gateway::Offer> {
        let f = self.found.as_ref()?;
        let installed = f.gateway_installed.as_deref()?;
        let offer = f.gateway.as_ref().ok()?.as_ref()?;
        (releases::is_newer(&offer.version, installed, false) && self.prefs.dismissed_gateway.as_deref() != Some(&offer.version))
            .then(|| offer.clone())
    }

    pub fn dismiss_gateway(&mut self, version: String, cx: &mut Context<Self>) {
        self.prefs.dismissed_gateway = Some(version);
        self.prefs.save();
        cx.notify();
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

#[cfg(test)]
mod manager_offer_tests {
    // Not `super::*`: gpui's own `test` attribute would shadow the standard one.
    use super::{newest_manager, releases, Channel, Release};

    fn rel(tag: &str, pre: bool) -> Release {
        Release {
            tag_name: tag.into(),
            name: None,
            body: Some("Summary.\n\n### Fixed\n- x".into()),
            html_url: String::new(),
            prerelease: pre,
            draft: false,
            published_at: Some("2026-11-14T10:00:00Z".into()),
            assets: vec![releases::Asset {
                name: if cfg!(windows) { "SENTIENT-Setup-manager-0.8.0.exe".into() } else { "SENTIENT-Setup-manager-0.8.0-linux-x86_64".into() },
                browser_download_url: String::new(),
                size: 1,
                digest: Some("sha256:ab".into()),
            }],
        }
    }

    fn shared_repository() -> Vec<Release> {
        vec![
            rel("4.4.0.2", false),
            rel("spm-v0.8.0-rc9", true),
            rel("spm-v0.8.0-rc11", true),
            rel("gateway-4.16.2", false),
            rel("spm-v0.8.0-rc10", true),
            rel("nightly", true),
        ]
    }

    #[test]
    fn the_newest_manager_release_is_found_among_the_other_products() {
        let offer = newest_manager(&shared_repository(), Channel::Candidate, "0.8.0-rc10").expect("rc11 is newer");
        assert_eq!(offer.version, "0.8.0-rc11");
        assert_eq!(offer.tag, "spm-v0.8.0-rc11");
        assert!(offer.installer.is_some() || !cfg!(any(windows, target_arch = "x86_64")));
    }

    #[test]
    fn nothing_is_offered_when_up_to_date() {
        assert!(newest_manager(&shared_repository(), Channel::Candidate, "0.8.0-rc11").is_none());
    }

    #[test]
    fn stable_skips_release_candidates_and_nightly_gets_them() {
        assert!(newest_manager(&shared_repository(), Channel::Stable, "0.8.0-rc9").is_none());
        let mut all = shared_repository();
        all.push(rel("spm-v0.8.0", false));
        assert_eq!(newest_manager(&all, Channel::Stable, "0.8.0-rc11").map(|o| o.version).as_deref(), Some("0.8.0"));
        assert_eq!(newest_manager(&shared_repository(), Channel::Nightly, "0.8.0-rc9").map(|o| o.version).as_deref(), Some("0.8.0-rc11"));
    }
}
