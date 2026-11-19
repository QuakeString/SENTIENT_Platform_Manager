// Copyright © 2016-2026 The SENTIENT Authors

//! "Checking the offline pack": an offline pack (sentient_installer_core::
//! pack) opened, its signature and target checked, then every file read
//! against its SHA-256, in the background with progress. Setup's System
//! check and the Update screen show it the same way ([`panel`]); nothing
//! is used from the pack until it is Verified.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gpui_kit::component::{ActiveTheme as _, Theme, h_flex, progress::Progress, v_flex};
use gpui_kit::*;
use sentient_installer_core::pack::{self, Checked, Opened};

use crate::ui::{self, Tone};

/// Where the check stands.
#[derive(Debug, Clone, PartialEq)]
pub enum PackState {
    /// Reading `pack.json` and its signature.
    Opening,
    /// Reading the files.
    Verifying(Checked),
    Verified(Arc<Opened>),
    /// Why the pack is not used (sentient_installer_core::pack::Refusal).
    Refused(String),
}

pub struct PackCheck {
    pub dir: PathBuf,
    pub state: PackState,
    /// "Checking the offline pack: 41 files, 3.2 GB", once pack.json is read.
    title: Option<String>,
    /// Asks the reading thread to stop (the check closed).
    stop: Arc<AtomicBool>,
    _task: Option<Task<()>>,
}

/// What the reading thread hands over (behind a std lock, held only to
/// copy it).
#[derive(Default)]
struct Shared {
    title: Option<String>,
    checked: Option<Checked>,
    done: Option<Result<Opened, String>>,
}

impl Drop for PackCheck {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl PackCheck {
    /// Check the pack in `dir`.
    pub fn new(dir: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut this = PackCheck { dir, state: PackState::Opening, title: None, stop: Arc::new(AtomicBool::new(false)), _task: None };
        this.start(cx);
        this
    }

    /// Screens (SPM_DEBUG_PACK): `verifying`, `verified`, `wrong-os`, with
    /// a pack that is not on disk.
    pub fn debug(kind: &str) -> Self {
        let fake = pack::debug_fake();
        let title = Some(fake.pack.check_title());
        let total = fake.pack.total_bytes();
        let state = match kind {
            "verifying" => PackState::Verifying(Checked {
                file: "wsl/ubuntu-noble-wsl-amd64-20261101.rootfs.tar.gz".into(),
                done_files: 8,
                files: fake.pack.files.len(),
                done_bytes: total * 42 / 100,
                bytes: total,
            }),
            "wrong-os" => PackState::Refused(
                pack::Refusal::WrongOs { target: pack::TARGET_WINDOWS_X64.into(), here: "Ubuntu 24.04.1 LTS".into() }.message(),
            ),
            _ => PackState::Verified(Arc::new(fake.clone())),
        };
        PackCheck { dir: fake.dir, state, title, stop: Arc::new(AtomicBool::new(false)), _task: None }
    }

    pub fn verified(&self) -> Option<Arc<Opened>> {
        match &self.state {
            PackState::Verified(p) => Some(p.clone()),
            _ => None,
        }
    }

    pub fn refused(&self) -> Option<&str> {
        match &self.state {
            PackState::Refused(m) => Some(m),
            _ => None,
        }
    }

    pub fn is_checking(&self) -> bool {
        matches!(self.state, PackState::Opening | PackState::Verifying(_))
    }

    /// The card's title: what is being checked, then how it ended.
    pub fn title(&self) -> String {
        let checking = self.title.clone().unwrap_or_else(|| "Checking the offline pack".into());
        match &self.state {
            PackState::Verified(p) => format!("Offline pack checked: {} files, {}", p.pack.files.len(), pack::size_text(p.pack.total_bytes())),
            PackState::Refused(_) => checking.replacen("Checking the offline pack", "Offline pack not used", 1),
            _ => checking,
        }
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (dir, stop, out) = (self.dir.clone(), self.stop.clone(), shared.clone());
        std::thread::spawn(move || {
            let put = |f: &dyn Fn(&mut Shared)| {
                let mut g = out.lock().unwrap_or_else(|e| e.into_inner());
                f(&mut g);
            };
            let result = (|| {
                let opened = pack::open(&dir).map_err(|e| e.message())?;
                let title = opened.pack.check_title();
                put(&|s| s.title = Some(title.clone()));
                pack::check_target(&opened.pack, &pack::Host::this()).map_err(|e| e.message())?;
                opened
                    .verify_files(&|c| put(&|s| s.checked = Some(c.clone())), &|| stop.load(Ordering::SeqCst))
                    .map_err(|e| e.message())?;
                Ok(opened)
            })();
            put(&|s| s.done = Some(result.clone()));
        });
        self._task = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(std::time::Duration::from_millis(120)).await;
            // Copied out, the lock released before anything else happens.
            let (title, checked, done) = {
                let mut g = shared.lock().unwrap_or_else(|e| e.into_inner());
                (g.title.clone(), g.checked.clone(), g.done.take())
            };
            let finished = done.is_some();
            let alive = this
                .update(cx, |p, cx| {
                    p.title = title.or(p.title.take());
                    p.state = match done {
                        Some(Ok(opened)) => PackState::Verified(Arc::new(opened)),
                        Some(Err(why)) => PackState::Refused(why),
                        None => checked.map(PackState::Verifying).unwrap_or(PackState::Opening),
                    };
                    cx.notify();
                })
                .is_ok();
            if finished || !alive {
                break;
            }
        }));
    }
}

/// The pack check as a System-check-like card: the title with its state,
/// the progress while the files are read, then what the pack holds or why
/// it is refused.
pub fn panel(theme: &Theme, check: &PackCheck) -> Div {
    let (tone, word) = match &check.state {
        PackState::Opening | PackState::Verifying(_) => (Tone::Neutral, "Checking"),
        PackState::Verified(_) => (Tone::Good, "Verified"),
        PackState::Refused(_) => (Tone::Bad, "Refused"),
    };
    let head = h_flex()
        .gap_3()
        .items_center()
        .child(div().flex_1().min_w_0().font_weight(FontWeight::MEDIUM).child(check.title()))
        .child(ui::pill(theme, tone, word));
    let body: AnyElement = match &check.state {
        PackState::Opening => div().text_sm().text_color(theme.muted_foreground).child("Reading the pack's list of files and its signature…").into_any_element(),
        PackState::Verifying(c) => v_flex()
            .gap_2()
            .child(Progress::new("pack-check-bar").value(c.fraction() * 100.))
            .child(
                h_flex()
                    .gap_3()
                    .text_size(px(13.))
                    .text_color(theme.muted_foreground)
                    .child(div().flex_1().min_w_0().child(format!("Reading {}", ui::middle_ellipsis(&c.file, 64))))
                    .child(div().flex_shrink_0().font_features(ui::tabular()).child(format!(
                        "{} of {} files, {} of {}",
                        c.done_files,
                        c.files,
                        pack::size_text(c.done_bytes),
                        pack::size_text(c.bytes)
                    ))),
            )
            .into_any_element(),
        PackState::Verified(p) => {
            let gateway = p.pack.gateway_version.as_deref().map(|v| format!(", Sentient Gateway {v}")).unwrap_or_default();
            v_flex()
                .gap_2()
                .text_sm()
                .child(div().child(format!(
                    "{}; every file matches its checksum. SENTIENT {}, Platform Manager {}{gateway}.",
                    p.key.sentence(),
                    p.pack.sentient_version,
                    p.pack.spm_version
                )))
                .child(div().text_color(theme.muted_foreground).child("Everything is installed from the pack: nothing is downloaded."))
                .into_any_element()
        }
        PackState::Refused(why) => div().text_sm().text_color(theme.danger).child(why.clone()).into_any_element(),
    };
    ui::panel(theme)
        .child(head)
        .child(body)
        .child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(ui::middle_ellipsis(&check.dir.display().to_string(), 96)))
}

/// The licence line an offline install or update shows (Summary, Finish,
/// the Update screen): activation is online once, nothing else is.
pub const LICENCE_ONLINE: &str = "Activating the SENTIENT licence needs an internet connection once (from this computer). Everything else works offline.";

/// The pack card for `check`, read from its entity.
pub fn panel_of(check: &Entity<PackCheck>, cx: &App) -> Div {
    let theme = cx.theme().clone();
    panel(&theme, check.read(cx))
}
