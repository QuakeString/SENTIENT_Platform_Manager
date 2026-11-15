// Copyright © 2016-2026 The SENTIENT Authors

//! The global progress strip: while a job runs anywhere in the Platform
//! Manager, its name, time and Details sit in the top bar ("Stopping
//! ClickHouse on ST-DEV · 0:06 · Details") and a thin progress line runs
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

/// A job that failed: kept for the bell until its details are opened.
pub struct Failure {
    pub label: SharedString,
    /// The error and every output line, as the details dialog shows them.
    pub text: String,
    pub retry: Option<Rc<dyn Fn(&mut App)>>,
    pub seen: bool,
}

pub struct Strip {
    /// Failed jobs for the bell, newest first.
    pub failures: Vec<Failure>,
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
    _observe: Option<Subscription>,
    _hide: Option<Task<()>>,
}

struct StripSlot(Entity<Strip>);
impl Global for StripSlot {}

/// The strip for this window (the manager makes it once).
pub fn install(cx: &mut App) -> Entity<Strip> {
    let strip = cx.new(|_| Strip { failures: Vec::new(), light_bar: std::cell::Cell::new(false), window_w: std::cell::Cell::new(1100.), slot_w: std::cell::Cell::new(400.), job: None, label: None, finished_at: None, opened_for_failure: false, _observe: None, _hide: None });
    cx.set_global(StripSlot(strip.clone()));
    strip
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
                self.failures.insert(0, Failure { label: label.clone(), text, retry: j.retry(), seen: false });
                self.failures.truncate(20);
                self.job = None;
                self._observe = None;
                let lbl = Some(label);
                cx.defer(move |cx| open_details(job, lbl, cx));
            }
            JobState::Done(_) if self.finished_at.is_none() => {
                self.finished_at = Some(Instant::now());
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
    crate::modal::open(crate::modal::Modal { title, body: view.into(), width: 760., on_close: None }, cx);
}

impl Strip {
    /// Failures whose details were not opened since (the bell's red count).
    pub fn unseen(&self) -> usize {
        self.failures.iter().filter(|f| !f.seen).count()
    }

    /// Open a failure's details (from the bell) and count it as seen.
    pub fn open_failure(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(f) = self.failures.get_mut(i) else { return };
        f.seen = true;
        let (title, text, retry) = (format!("{} failed", f.label), f.text.clone(), f.retry.clone());
        cx.defer(move |cx| crate::ui::show_error_with(title, text, retry, cx));
        cx.notify();
    }

    /// A job is shown in the top bar.
    pub fn active(&self) -> bool {
        self.job.is_some()
    }

    /// The progress line under the top bar: (fraction or None while it
    /// cannot say, its colour), or nothing when no job is shown.
    pub fn line(&self, cx: &App) -> Option<AnyElement> {
        let job = self.job.as_ref()?.read(cx);
        let theme = cx.theme();
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
                .bg(accent.opacity(0.25))
                .child(match (running, job.percent()) {
                    (true, Some(p)) => div().h_full().w(relative(p / 100.)).bg(accent).into_any_element(),
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
                        r.child(crate::ui::button("job-details-retry").outline().label("Retry").on_click(move |_, _, cx| {
                            crate::modal::close(cx);
                            retry(cx);
                        }))
                    })
                    .child(crate::ui::button("job-details-close").primary().label("Close").on_click(|_, _, cx| crate::modal::close(cx))),
            )
    }
}
