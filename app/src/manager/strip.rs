// Copyright © 2016-2026 The SENTIENT Authors

//! The global progress strip: while a job runs anywhere in the Platform
//! Manager, its name, time and Details sit in the top bar ("Stopping
//! ClickHouse on DEV-01 · 0:06 · Details") and a thin progress line runs
//! along the bar's bottom edge. Both live inside the bar, so nothing on the
//! page moves. Pages never insert a progress or log panel of their own.
//!
//! Every crate::jobs::Job reports here when it starts (Job::start). Its
//! output goes to the Platform Manager's own log (About → Open the log) and
//! to the Details dialog. The dialog opens by itself only when the job
//! fails; a job that succeeds shows its last message in the strip for a few
//! seconds, then the strip goes.

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::{ActiveTheme, IconName, Sizable as _, button::ButtonVariants as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::jobs::{Job, JobState};
use crate::ui::{self, Tone};

/// How long "Done" stays in the strip.
const SHOW_DONE: Duration = Duration::from_secs(4);

/// The chip's smallest form: spinner and time, kept free in the top bar
/// whether or not a job runs.
pub const CHIP_MIN: f32 = 104.;
/// The name shows only when this much is free beside the monitors.
const WITH_NAME: f32 = 300.;
/// Details shows from this much.
const WITH_DETAILS: f32 = 190.;

/// Something that happened, for the bell: a job that failed, a backup or
/// update that finished. Kept across restarts until cleared.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Event {
    pub id: u64,
    pub failed: bool,
    pub title: String,
    /// One line under the title.
    pub detail: String,
    /// The full text the details dialog shows (error and output lines).
    pub text: String,
    /// Unix seconds.
    pub at: i64,
    pub read: bool,
    /// The instance it belongs to (None: this computer), its name and colour.
    #[serde(default)]
    pub instance: Option<String>,
    #[serde(default)]
    pub instance_name: String,
    #[serde(default)]
    pub color: String,
}

fn events_path() -> std::path::PathBuf {
    crate::updates::prefs_dir().join("notifications.json")
}

pub(crate) fn load_events() -> Vec<Event> {
    std::fs::read_to_string(events_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub struct Strip {
    /// The bell's events, newest first (saved).
    pub events: Vec<Event>,
    /// Retry for events of this run (not kept across restarts).
    retries: std::collections::HashMap<u64, Rc<dyn Fn(&mut App)>>,
    /// The top bar is light (dark text): the chip's inset is fainter.
    pub light_bar: std::cell::Cell<bool>,
    /// The window's width: below 900 the chip shows only its spinner, time
    /// and Details; at the minimum width only the spinner and time.
    pub window_w: std::cell::Cell<f32>,
    /// The room the top bar leaves the chip between the switcher and the
    /// monitors (the manager measures it each frame).
    pub slot_w: std::cell::Cell<f32>,
    job: Option<Entity<Job>>,
    /// The job's name as the page called it (else its current step).
    label: Option<SharedString>,
    finished_at: Option<Instant>,
    opened_for_failure: bool,
    /// Small saves under way (`quiet`): the line runs, nothing else.
    quiet: usize,
    /// The chip's panel (a job with one, the Files transfers) is open.
    panel_open: bool,
    _observe: Option<Subscription>,
    _hide: Option<Task<()>>,
}

/// A job ended (done or failed) on this instance (None: this computer):
/// every page that shows its state reads it again.
pub struct JobEnded {
    pub instance: Option<String>,
}

impl EventEmitter<JobEnded> for Strip {}

struct StripSlot(Entity<Strip>);
impl Global for StripSlot {}

/// The strip, made once per run: a window opened again (crate::bg_app)
/// finds the running job and the events where they were.
pub fn install(cx: &mut App) -> Entity<Strip> {
    if let Some(s) = cx.try_global::<StripSlot>() {
        return s.0.clone();
    }
    let strip = cx.new(|_| Strip { events: load_events(), retries: Default::default(), light_bar: std::cell::Cell::new(false), window_w: std::cell::Cell::new(1100.), slot_w: std::cell::Cell::new(400.), job: None, label: None, finished_at: None, opened_for_failure: false, quiet: 0, panel_open: cfg!(debug_assertions) && std::env::var_os("SPM_DEBUG_STRIP_PANEL").is_some(), _observe: None, _hide: None });
    cx.set_global(StripSlot(strip.clone()));
    strip
}

/// A small save (a setting, a delete) started (`true`) or ended: the thin
/// line under the top bar runs meanwhile, with no chip and nothing in the
/// bell; its page says Saved or what went wrong.
pub fn quiet(started: bool, cx: &mut App) {
    let strip = install(cx);
    strip.update(cx, |s, cx| {
        s.quiet = if started { s.quiet + 1 } else { s.quiet.saturating_sub(1) };
        cx.notify();
    });
}

/// A job is running (closing the window then minimizes it instead).
pub fn busy(cx: &App) -> bool {
    cx.try_global::<StripSlot>().is_some_and(|s| s.0.read(cx).job.as_ref().is_some_and(|j| j.read(cx).is_running()))
}

/// A condition the background checks found (crate::bg_app): a line in the
/// bell, as a job's would be.
pub fn push_condition(inst: Option<(String, String, String)>, failed: bool, title: String, detail: String, cx: &mut App) {
    let strip = install(cx);
    strip.update(cx, |s, cx| {
        s.push_event(inst, failed, title, detail.clone(), detail);
        cx.notify();
    });
}

/// A job started: follow it in the strip. Without a strip (the installer,
/// the maintenance window) nothing happens.
pub fn track(job: Entity<Job>, label: Option<SharedString>, cx: &mut App) {
    let Some(strip) = cx.try_global::<StripSlot>().map(|s| s.0.clone()) else { return };
    strip.update(cx, |s, cx| s.follow(job, label, cx));
}

impl Strip {
    fn follow(&mut self, job: Entity<Job>, label: Option<SharedString>, cx: &mut Context<Self>) {
        self.label = label;
        self.finished_at = None;
        self.opened_for_failure = false;
        self._hide = None;
        self._observe = Some(cx.observe(&job, |s, job, cx| s.changed(job, cx)));
        self.job = Some(job);
        cx.notify();
    }

    fn changed(&mut self, job: Entity<Job>, cx: &mut Context<Self>) {
        let state = job.read(cx).state().clone();
        match state {
            JobState::Running => {}
            JobState::Failed(m) if !self.opened_for_failure => {
                crate::bg_app::request_check();
                // The chip goes; the details open once, and the failure stays
                // in the bell (red count) until its entry is opened.
                self.opened_for_failure = true;
                self.finished_at = Some(Instant::now());
                let j = job.read(cx);
                let mut text = m.to_string();
                let lines: Vec<String> = j.steps().iter().map(|s| format!("▸ {s}")).chain(j.log().iter().map(|l| l.to_string())).collect();
                if !lines.is_empty() && !text.contains(crate::remote::DETAILS) {
                    text = format!("{text}{}{}", crate::remote::DETAILS, lines.join("\n"));
                }
                let label = self.label.clone().unwrap_or_else(|| "A job".into());
                let id = self.push_event(j.instance(), true, format!("{label} failed"), crate::remote::split_failure(&m).0, text);
                let (retry, ended) = (j.retry(), j.instance().map(|i| i.0));
                if let Some(r) = retry {
                    self.retries.insert(id, r);
                }
                cx.emit(JobEnded { instance: ended });
                self.job = None;
                self._observe = None;
                // Its page shows the result; else the details open once.
                if !job.read(cx).on_page() {
                    let lbl = Some(label);
                    cx.defer(move |cx| open_details(job, lbl, cx));
                }
            }
            JobState::Done(m) if self.finished_at.is_none() => {
                crate::bg_app::request_check();
                cx.emit(JobEnded { instance: job.read(cx).instance().map(|i| i.0) });
                self.finished_at = Some(Instant::now());
                // Backups, restores, updates and takeovers that finish are
                // worth a line in the bell; a start or stop is not.
                if let Some(l) = self.label.clone().filter(|l| ["Backing up", "Restoring", "Updating", "Taking over", "Moving", "Changing the database"].iter().any(|w| l.starts_with(w))) {
                    let inst = job.read(cx).instance();
                    self.push_event(inst, false, format!("{l}: done"), m.to_string(), m.to_string());
                }
                self._hide = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(SHOW_DONE).await;
                    // ("Done" for a few seconds, then the chip goes.)
                    this.update(cx, |s, cx| {
                        if s.job.as_ref().is_some_and(|j| matches!(j.read(cx).state(), JobState::Done(_))) {
                            s.job = None;
                            s._observe = None;
                            cx.notify();
                        }
                    })
                    .ok();
                }));
            }
            JobState::Idle => {
                self.job = None;
                self._observe = None;
            }
            _ => {}
        }
        cx.notify();
    }
}

fn open_details(job: Entity<Job>, label: Option<SharedString>, cx: &mut App) {
    let title = label.clone().unwrap_or_else(|| "Details".into());
    let view = cx.new(|cx| JobDetails::new(job, cx));
    crate::modal::open(crate::modal::Modal { title, body: view.into(), width: 760., icon: None, on_close: None }, cx);
}

impl Strip {
    fn push_event(&mut self, inst: Option<(String, String, String)>, failed: bool, title: String, detail: String, text: String) -> u64 {
        let id = self.events.iter().map(|e| e.id).max().unwrap_or(0) + 1;
        let (instance, instance_name, color) = match inst {
            Some((i, n, c)) => (Some(i), n, c),
            None => (None, "This computer".into(), crate::remote::LOCAL_COLOR.into()),
        };
        self.events.insert(0, Event { id, failed, title, detail: detail.lines().next().unwrap_or("").to_string(), text, at: chrono::Utc::now().timestamp(), read: false, instance, instance_name, color });
        self.events.truncate(50);
        self.save();
        id
    }

    fn save(&self) {
        if let Some(dir) = events_path().parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(events_path(), serde_json::to_string_pretty(&self.events).unwrap_or_default());
    }

    /// Unread events (the bell's count; red when one is a failure).
    pub fn unread(&self) -> (usize, bool) {
        let u: Vec<&Event> = self.events.iter().filter(|e| !e.read).collect();
        (u.len(), u.iter().any(|e| e.failed))
    }

    /// Open an event's details and mark it read.
    pub fn open_event(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(e) = self.events.iter_mut().find(|e| e.id == id) else { return };
        e.read = true;
        let (title, text, failed) = (e.title.clone(), e.text.clone(), e.failed);
        let retry = self.retries.get(&id).cloned();
        self.save();
        // A failure in the error dialog (details, Copy, Retry); anything
        // else as what it is: a note, never "did not finish".
        cx.defer(move |cx| if failed { crate::ui::show_error_with(title, text, retry, cx) } else { crate::ui::show_info(title, text, cx) });
        cx.notify();
    }

    pub fn mark_all_read(&mut self, cx: &mut Context<Self>) {
        for e in &mut self.events {
            e.read = true;
        }
        self.save();
        cx.notify();
    }

    pub fn clear(&mut self, id: Option<u64>, cx: &mut Context<Self>) {
        match id {
            Some(id) => self.events.retain(|e| e.id != id),
            None => self.events.clear(),
        }
        self.save();
        cx.notify();
    }

    /// Screens: canned events (SPM_DEBUG_EVENTS=n).
    #[cfg(debug_assertions)]
    pub fn debug_events(&mut self, n: usize) {
        let now = chrono::Utc::now().timestamp();
        let samples = [
            (true, "Stopping ClickHouse failed", "ClickHouse did not stop within 30 seconds.", Some("plantb"), "Plant B server", "#6b4fa0"),
            (false, "Backing up: done", "Backup finished and checked: sentient-2026-11-15-0200, 5.0 GB.", None, "This computer", "#187074"),
            (true, "Updating the server agent failed", "The agent could not be put in place at /opt/sentient-platform-manager (as administrator).", Some("fabrikam"), "Fabrikam IIoT", "#1f9d4d"),
            (false, "Updating SENTIENT: done", "SENTIENT 4.4.0.2 is running.", Some("plantb"), "Plant B server", "#6b4fa0"),
        ];
        self.events = (0..n)
            .map(|i| {
                let (failed, t, d, inst, name, color) = samples[i % samples.len()];
                Event { id: i as u64 + 1, failed, title: t.into(), detail: d.into(), text: d.into(), at: now - (i as i64) * 1900 - 60, read: i >= 2, instance: inst.map(String::from), instance_name: name.into(), color: color.into() }
            })
            .collect();
    }

    /// A job is shown in the top bar.
    pub fn active(&self) -> bool {
        self.job.is_some()
    }

    /// The progress line under the top bar: (fraction or None while it
    /// cannot say, its colour), or nothing when no job is shown.
    pub fn line(&self, window: &mut Window, cx: &App) -> Option<AnyElement> {
        let theme = cx.theme();
        if self.job.is_none() && self.quiet > 0 {
            let accent = theme.primary;
            return Some(
                div()
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .h(px(3.))
                    // The moving part starts left of the bar: clip it, or it
                    // draws over the menu (his rc12-19 screenshot).
                    .overflow_hidden()
                    .bg(accent.opacity(0.25))
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .h_full()
                            .w(relative(0.25))
                            .bg(accent)
                            .with_animation("strip-quiet", Animation::new(Duration::from_millis(1400)).repeat(), |d, t| d.left(relative(-0.25 + t * 1.25))),
                    )
                    .into_any_element(),
            );
        }
        let job = self.job.as_ref()?.read(cx);
        let (running, accent) = match job.state() {
            JobState::Running => (true, theme.primary),
            JobState::Done(_) => (false, theme.success_foreground),
            JobState::Failed(_) => (false, theme.danger),
            JobState::Idle => return None,
        };
        Some(
            div()
                .absolute()
                .bottom_0()
                .left_0()
                .right_0()
                .h(px(3.))
                .overflow_hidden()
                .bg(accent.opacity(0.25))
                .child(match (running, job.percent()) {
                    // The width glides to each new percentage (300 ms).
                    (true, Some(p)) => div().h_full().w(relative(crate::ui::tween("strip-p", p / 100., 300, window))).bg(accent).into_any_element(),
                    (true, None) => div()
                        .absolute()
                        .top_0()
                        .h_full()
                        .w(relative(0.25))
                        .bg(accent)
                        .with_animation("strip-run", Animation::new(Duration::from_millis(1400)).repeat(), |d, t| d.left(relative(-0.25 + t * 1.25)))
                        .into_any_element(),
                    _ => div().h_full().w_full().bg(accent).into_any_element(),
                })
                .into_any_element(),
        )
    }
}

/// The job's name, time and Details as a chip in the top bar (its text takes
/// the bar's colour); the line under the bar is `Strip::line`.
impl Render for Strip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(job_e) = self.job.clone() else { return div().into_any_element() };
        let job = job_e.read(cx);
        let (tone, text, running) = match job.state() {
            JobState::Running => {
                let step = job.step().clone();
                let name = match (&self.label, step.is_empty()) {
                    (Some(l), true) => l.to_string(),
                    // The step adds something only when the name does not say it.
                    (Some(l), false) if !l.to_lowercase().contains(&step.to_lowercase()) => format!("{l}: {step}"),
                    (Some(l), false) => l.to_string(),
                    (None, _) => step.to_string(),
                };
                (Tone::Neutral, name, true)
            }
            JobState::Done(_) => (Tone::Good, "Done".to_string(), false),
            JobState::Failed(m) => (Tone::Bad, crate::remote::split_failure(m).0, false),
            JobState::Idle => return div().into_any_element(),
        };
        let label = self.label.clone();
        // A job with its own chip text and panel (the Files transfers): the
        // chip is a button that opens the panel under it.
        if let (Some((chip, panel)), true) = (job.chip(), running) {
            let open = self.panel_open;
            let theme = cx.theme().clone();
            let text = chip(cx);
            return div()
                .relative()
                .child(
                    h_flex()
                        .id("progress-strip")
                        .relative()
                        .child(crate::ui::layout_probe("chip"))
                        .min_w_0()
                        .max_w(px(self.slot_w.get().max(CHIP_MIN).min(560.)))
                        .overflow_hidden()
                        .h(px(28.))
                        .px(px(12.))
                        .gap_2()
                        .items_center()
                        .rounded(px(14.))
                        .bg(black().opacity(if self.light_bar.get() { 0.10 } else { 0.22 }))
                        .when(open, |c| c.bg(black().opacity(if self.light_bar.get() { 0.16 } else { 0.30 })))
                        .text_size(px(13.))
                        .cursor_pointer()
                        .child(gpui_kit::component::Icon::new(IconName::ArrowRight).small())
                        .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(text))
                        .on_click(cx.listener(|s, _, _, cx| {
                            s.panel_open = !s.panel_open;
                            cx.notify();
                        })),
                )
                .when(open, |c| {
                    c.child(deferred(
                        div().absolute().top(px(28. + 10.)).right(px(-120.)).child(
                            v_flex()
                                .id("strip-panel")
                                .relative()
                                .child(crate::ui::layout_probe("overlay"))
                                .occlude()
                                .w(px(400.))
                                .rounded(px(crate::ui::PANEL_RADIUS))
                                .bg(theme.popover)
                                .text_color(theme.popover_foreground)
                                .border_1()
                                .border_color(theme.border)
                                .shadow_lg()
                                .child(panel)
                                .on_mouse_down_out(cx.listener(|s, _, _, cx| {
                                    s.panel_open = false;
                                    cx.notify();
                                })),
                        ),
                    ))
                })
                .into_any_element();
        }
        // What fits the slot: name + time + Details, then time + Details,
        // then the time alone; the chip never reaches the monitors.
        let slot = (self.slot_w.get() - 16.).max(CHIP_MIN - 16.);
        let compact = running && slot < WITH_NAME;
        let tiny = running && slot < WITH_DETAILS;
        let mark = match tone {
            Tone::Good => gpui_kit::component::Icon::new(IconName::CircleCheck).small().into_any_element(),
            Tone::Bad => gpui_kit::component::Icon::new(IconName::CircleX).small().text_color(rgb(0xfca5a5)).into_any_element(),
            _ => gpui_kit::component::Icon::new(IconName::LoaderCircle)
                .small()
                .with_animation("strip-spin", Animation::new(Duration::from_millis(1000)).repeat(), |i, t| i.transform(Transformation::rotate(percentage(t))))
                .into_any_element(),
        };
        h_flex()
            .id("progress-strip")
            .min_w_0()
            .max_w(px(self.slot_w.get().max(CHIP_MIN).min(560.)))
            .when(running, |c| c.min_w(px(CHIP_MIN - 8.)))
            .overflow_hidden()
            .relative()
            .child(crate::ui::layout_probe("chip"))
            .h(px(28.))
            .pl(px(10.))
            .pr(px(4.))
            .gap_2()
            .items_center()
            .rounded(px(8.))
            // The top bar's one inset style (the switcher's): darker than the
            // bar, fainter on a light bar.
            .bg(black().opacity(if self.light_bar.get() { 0.10 } else { 0.22 }))
            .text_size(px(13.))
            .child(mark)
            .when(!compact || !running, |r| r.child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(text)))
            // The time in a fixed slot, so Details does not move each second.
            .when(running, |r| {
                let secs = job.step_elapsed().map(|d| d.as_secs()).unwrap_or(0);
                let t = if secs >= 3600 { format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60) } else { format!("{}:{:02}", secs / 60, secs % 60) };
                r.child(div().relative().child(crate::ui::live_value_left("00:00:00", 13., t).opacity(0.85)).child(crate::ui::layout_probe("chip-time")))
            })
            .when(!tiny, |r| r.child(
                div()
                    .id("strip-details")
                    .flex_shrink_0()
                    .px(px(8.))
                    .py(px(2.))
                    .rounded(px(6.))
                    .font_weight(FontWeight::MEDIUM)
                    .underline()
                    .cursor_pointer()
                    .hover(|d| d.bg(white().opacity(0.12)))
                    .child("Details")
                    .on_click(move |_, _, cx| match job_e.read(cx).details() {
                        Some(open) => open(cx),
                        None => open_details(job_e.clone(), label.clone(), cx),
                    }),
            ))
            .when(!running, |r| {
                r.child(
                    div()
                        .id("strip-close")
                        .flex_shrink_0()
                        .p(px(3.))
                        .rounded(px(6.))
                        .cursor_pointer()
                        .hover(|d| d.bg(white().opacity(0.12)))
                        .child(gpui_kit::component::Icon::new(IconName::Close).xsmall())
                        .on_click(cx.listener(|s, _, _, cx| {
                            s.job = None;
                            s._observe = None;
                            cx.notify();
                        })),
                )
            })
            .into_any_element()
    }
}

/// What a job did: its state, its steps and every output line, to copy;
/// Retry when it failed and can run again.
pub struct JobDetails {
    job: Entity<Job>,
    _observe: Subscription,
}

impl JobDetails {
    fn new(job: Entity<Job>, cx: &mut Context<Self>) -> Self {
        let _observe = cx.observe(&job, |_, _, cx| cx.notify());
        Self { job, _observe }
    }
}

impl Render for JobDetails {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let job = self.job.read(cx);
        let state = match job.state() {
            JobState::Running => ui::outcome(&theme, Tone::Neutral, job.step_with_time().to_string()),
            JobState::Done(m) => ui::outcome(&theme, Tone::Good, m.to_string()),
            JobState::Failed(m) => {
                let (head, detail) = crate::remote::split_failure(m);
                ui::outcome(&theme, Tone::Bad, match detail {
                    Some(d) if !d.trim().is_empty() => format!("{head}\n{}", d.trim()),
                    _ => head,
                })
            }
            JobState::Idle => div(),
        };
        let retry = matches!(job.state(), JobState::Failed(_)).then(|| job.retry()).flatten();
        let again = match job.state() {
            JobState::Failed(m) => crate::ui::retry_label(m, "Retry"),
            _ => "Retry",
        };
        let lines: Vec<SharedString> = job.steps().iter().map(|s| SharedString::from(format!("▸ {s}"))).chain(job.log().iter().cloned()).collect();
        let text = lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n");
        v_flex()
            .gap_3()
            .child(state)
            .child(
                div()
                    .id("job-details-lines")
                    .max_h(px(380.))
                    .overflow_y_scroll()
                    .p_2()
                    .rounded(theme.radius)
                    .bg(theme.muted)
                    .font_family(theme.mono_font_family.clone())
                    .text_size(px(12.5))
                    .when(lines.is_empty(), |d| d.child(div().text_color(theme.muted_foreground).child("No output yet.")))
                    .children(lines.into_iter().map(|l| div().child(l))),
            )
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(crate::ui::button("job-details-copy").outline().label("Copy").on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))))
                    .when_some(retry.clone(), |r, retry| {
                        r.child(crate::ui::button("job-details-retry").outline().label(again).on_click(move |_, _, cx| {
                            crate::modal::close(cx);
                            retry(cx);
                        }))
                    })
                    .child(crate::ui::button("job-details-close").primary().label("Close").on_click(|_, _, cx| crate::modal::close(cx))),
            )
    }
}
