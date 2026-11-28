// Copyright © 2016-2026 The SENTIENT Authors

//! The one progress view of every long job (Backup, Restore, Update, Move
//! data, Repair, Uninstall, Migration, Setup's install; here and on a
//! server). It replaces the page's form while the job runs and stays with
//! its result until the page's "Back to …".
//!
//! Two cards. The first: the title and a fixed-width status pill; one bar
//! for the whole job (each step by its weight, "Step N of M", the time,
//! the percent); every step the job declared before it began (grey, a
//! spinner while it runs, a green check, a red mark), each with its time;
//! at the end one result line (the diagnosis, cause and fix, when it
//! failed) and the next actions at the right end (primary rightmost),
//! Cancel there while it runs. The second: the log, 280 px high, scrolling
//! inside, following the newest line until the user scrolls up ("Latest"
//! jumps back), with Copy and Save (the whole log, from its file).
//!
//! A job declares its steps with `Job::set_plan` (crate::jobplan) before it
//! starts; a server's agent sends its own (`Progress::Plan`). A job without
//! a plan still shows, its steps added as they start
//! (tests/layout/jobview.sh fails on a job page that does that).

use std::rc::Rc;

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, h_flex, progress::Progress, spinner::Spinner, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use sentient_installer_core::progress::PlanStep;

use crate::jobs::{Job, JobState, RowState};
use crate::ui::{self, Tone};

/// The log card's height (logical px): the page never grows with the log.
pub const LOG_HEIGHT: f32 = 280.;
const LINE_H: f32 = 20.;
/// The status pill's width, so the title never moves when it changes.
const PILL_W: f32 = 96.;

/// Cancel while the job runs.
pub enum Cancel {
    /// It can stop: what Cancel does.
    Can(Rc<dyn Fn(&mut Window, &mut App)>),
    /// It cannot stop safely: Cancel disabled, the reason in its tooltip.
    Cannot(&'static str),
    /// Nothing to stop from here (a server restarting): "Hide" leaves the
    /// view, the job goes on in the progress chip.
    Hide(Rc<dyn Fn(&mut Window, &mut App)>),
}

/// The view, built for one render.
pub struct JobView {
    id: &'static str,
    title: String,
    job: Entity<Job>,
    pill: Option<(SharedString, Tone)>,
    result: Option<AnyElement>,
    extra: Vec<AnyElement>,
    actions: Vec<AnyElement>,
    cancel: Cancel,
    hide_line: Option<fn(&str) -> bool>,
    /// The job stopped on purpose and goes on later (Windows restarts):
    /// what its stopped step says instead of a failure.
    paused: Option<SharedString>,
}

impl JobView {
    /// `title` names the action ("Backing up DEV-01").
    pub fn new(id: &'static str, title: impl Into<String>, job: &Entity<Job>) -> Self {
        Self { id, title: title.into(), job: job.clone(), pill: None, result: None, extra: Vec::new(), actions: Vec::new(), cancel: Cancel::Cannot(NO_CANCEL), hide_line: None, paused: None }
    }

    /// A pill of the page's own ("Rolled back") instead of Running/Done/Failed.
    pub fn pill(mut self, text: impl Into<SharedString>, tone: Tone) -> Self {
        self.pill = Some((text.into(), tone));
        self
    }

    /// The result line, in place of the job's own message.
    pub fn result(mut self, el: impl IntoElement) -> Self {
        self.result = Some(el.into_any_element());
        self
    }

    /// More about the result, under its line (a backup's checksums).
    pub fn extra(mut self, el: impl IntoElement) -> Self {
        self.extra.push(el.into_any_element());
        self
    }

    /// The next actions once it ended, secondary first: the last is the
    /// primary one, at the right end.
    pub fn actions(mut self, actions: impl IntoIterator<Item = AnyElement>) -> Self {
        self.actions.extend(actions);
        self
    }

    /// The job stopped on purpose and continues later: its stopped step is
    /// not shown as failed (no red mark, no red bar) and says `next`.
    pub fn paused(mut self, next: impl Into<SharedString>) -> Self {
        self.paused = Some(next.into());
        self
    }

    pub fn cancel(mut self, cancel: Cancel) -> Self {
        self.cancel = cancel;
        self
    }

    /// Log lines not shown (a flow's data lines).
    pub fn hide_lines(mut self, hide: fn(&str) -> bool) -> Self {
        self.hide_line = Some(hide);
        self
    }

    pub fn render(self, cx: &App) -> Div {
        let theme = cx.theme();
        let job = self.job.read(cx);
        let state = job.state().clone();
        let running = matches!(state, JobState::Running);
        let warning = job.warning().map(|(m, _)| m.clone()).filter(|_| matches!(state, JobState::Done(_)));
        let (pill, tone) = self.pill.clone().unwrap_or(match &state {
            JobState::Running | JobState::Idle => ("Running".into(), Tone::Neutral),
            JobState::Done(_) if warning.is_some() => ("Done".into(), Tone::Warn),
            JobState::Done(_) => ("Done".into(), Tone::Good),
            JobState::Failed(_) => ("Failed".into(), Tone::Bad),
        });
        let rows = job.rows();
        let n = rows.len();
        let pct = job.overall();
        let elapsed = job.elapsed().map(clock).unwrap_or_default();
        let at = job.current().or_else(|| rows.iter().position(|r| r.state == RowState::Failed));
        let paused = self.paused.clone().filter(|_| matches!(state, JobState::Failed(_)));
        let nums = match &state {
            JobState::Failed(_) if paused.is_some() => match at {
                Some(i) if n > 0 => format!("Continues at step {} of {n} after the restart · {elapsed}", i + 1),
                _ => format!("Continues after the restart · {elapsed}"),
            },
            JobState::Done(_) if n > 0 => format!("All {n} steps · {elapsed}"),
            JobState::Done(_) => format!("Finished · {elapsed}"),
            JobState::Failed(_) => match at {
                Some(i) if n > 0 => format!("Stopped at step {} of {n} · {elapsed}", i + 1),
                _ => format!("Stopped · {elapsed}"),
            },
            _ => match at {
                Some(i) => format!("Step {} of {n} · {elapsed} · {pct:.0}%", i + 1),
                None if n > 0 => format!("Starting · {n} steps · {elapsed}"),
                None => format!("Starting · {elapsed}"),
            },
        };
        let bar_color = match &state {
            JobState::Done(_) => theme.success_foreground,
            JobState::Failed(_) if paused.is_none() => theme.danger,
            _ => theme.primary,
        };
        let step_pct = job.percent();
        let steps = v_flex().gap_0().children(rows.iter().enumerate().map(|(i, r)| {
            // Paused: the stopped step waits, it did not fail.
            if let (Some(next), RowState::Failed) = (&paused, r.state) {
                let time = r.time().map(clock).unwrap_or_default();
                return step_row(theme, self.id, i, RowState::Pending, r.step.name.clone(), next.to_string(), time);
            }
            let sub = match r.state {
                RowState::Running => {
                    let s = r.sub.clone().map(|s| s.to_string()).unwrap_or_else(|| r.step.detail.clone());
                    match step_pct {
                        Some(p) if !s.is_empty() => format!("{s} · {p:.0}%"),
                        Some(p) => format!("{p:.0}%"),
                        None => s,
                    }
                }
                RowState::Failed => r.sub.clone().map(|s| s.to_string()).unwrap_or_else(|| r.step.detail.clone()),
                _ => r.step.detail.clone(),
            };
            let time = match r.state {
                RowState::Skipped => "Skipped".to_string(),
                RowState::Pending => String::new(),
                _ => r.time().map(clock).unwrap_or_default(),
            };
            step_row(theme, self.id, i, r.state, r.step.name.clone(), sub, time)
        }));
        let result: Option<AnyElement> = match (&state, self.result) {
            (JobState::Running | JobState::Idle, _) => None,
            (_, Some(el)) => Some(el),
            (JobState::Done(m), None) => Some(match &warning {
                Some(w) => v_flex()
                    .gap(px(8.))
                    .child(ui::outcome(theme, Tone::Good, m.to_string()))
                    .child(ui::outcome(theme, Tone::Warn, w.to_string()))
                    .into_any_element(),
                None => ui::outcome(theme, Tone::Good, m.to_string()).into_any_element(),
            }),
            (JobState::Failed(m), None) => {
                let (head, detail) = crate::remote::split_failure(m);
                // What went wrong and the fix, when it was diagnosed.
                Some(crate::diagnosis::outcome(
                    theme,
                    Tone::Bad,
                    match detail {
                        Some(d) if !d.trim().is_empty() => format!("{head}\n{}", d.trim()),
                        _ => head,
                    },
                    job,
                ))
            }
        };
        let mut foot: Vec<AnyElement> = Vec::new();
        if running {
            match self.cancel {
                Cancel::Can(on) => foot.push(ui::button(SharedString::from(format!("{}-cancel", self.id))).outline().label("Cancel").on_click(move |_, w, cx| on(w, cx)).into_any_element()),
                Cancel::Cannot(why) => foot.push(
                    div()
                        .id(SharedString::from(format!("{}-cancel-why", self.id)))
                        .child(ui::button(SharedString::from(format!("{}-cancel", self.id))).outline().label("Cancel").disabled(true))
                        .tooltip(crate::ui::tooltip(why))
                        .into_any_element(),
                ),
                Cancel::Hide(on) => foot.push(ui::button(SharedString::from(format!("{}-hide", self.id))).outline().label("Hide").on_click(move |_, w, cx| on(w, cx)).into_any_element()),
            }
        } else {
            foot.extend(self.actions);
        }
        let status = ui::panel(theme)
            .gap(px(14.))
            .child(
                h_flex()
                    .gap(ui::button_gap())
                    .items_center()
                    .child(div().flex_1().min_w_0().font_weight(FontWeight::MEDIUM).child(self.title))
                    .child(ui::pill(theme, tone, pill).w(px(PILL_W)).flex_shrink_0().justify_center()),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_x(px(14.))
                    .gap_y_1()
                    .items_center()
                    .child(div().flex_1().min_w(px(160.)).child(Progress::new(SharedString::from(format!("{}-bar", self.id))).color(bar_color).value(pct)))
                    .child(div().flex_shrink_0().text_size(px(13.)).text_color(theme.muted_foreground).font_features(ui::tabular()).child(nums)),
            )
            .when(n > 0, |c| c.child(steps))
            .children(result)
            .children(self.extra)
            .when(!foot.is_empty(), |c| c.child(ui::button_row(foot).justify_end()));
        v_flex().gap(px(ui::GAP)).child(status).child(log_card(theme, self.id, &self.job, job, running, self.hide_line))
    }
}

const NO_CANCEL: &str = "This job cannot be stopped half-way without leaving SENTIENT in between; it puts things back itself when a step fails.";

/// "1:05", "12:40", "1:02:03".
pub fn clock(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// One step: its mark, name and line, and its time on the right.
fn step_row(theme: &gpui_kit::component::Theme, id: &'static str, i: usize, state: RowState, name: String, sub: String, time: String) -> Stateful<Div> {
    let ring = |c: Hsla| div().size(px(18.)).rounded_full().border_2().border_color(c);
    let mark: AnyElement = match state {
        RowState::Pending | RowState::Skipped => ring(theme.muted_foreground.opacity(0.5)).into_any_element(),
        RowState::Running if ui::reduced_motion() => ring(theme.primary).into_any_element(),
        RowState::Running => Spinner::new().small().color(theme.primary).into_any_element(),
        RowState::Done => div().size(px(20.)).rounded_full().flex().items_center().justify_center().bg(theme.success).child(Icon::new(IconName::Check).size(px(13.)).text_color(theme.success_foreground)).into_any_element(),
        RowState::Failed => div().size(px(20.)).rounded_full().flex().items_center().justify_center().bg(theme.danger.opacity(0.14)).child(Icon::new(IconName::Close).size(px(13.)).text_color(theme.danger)).into_any_element(),
    };
    let name_color = match state {
        RowState::Pending | RowState::Skipped => theme.muted_foreground,
        RowState::Failed => theme.danger,
        _ => theme.foreground,
    };
    h_flex()
        .id(SharedString::from(format!("{id}-step-{i}")))
        .gap(px(12.))
        .min_h(px(36.))
        .py_1()
        .items_center()
        .text_sm()
        .child(div().w(px(22.)).flex_shrink_0().flex().items_center().justify_center().child(mark))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().text_color(name_color).when(matches!(state, RowState::Running | RowState::Failed), |d| d.font_weight(FontWeight::MEDIUM)).child(name))
                .when(!sub.is_empty(), |c| c.child(div().text_size(px(12.)).text_color(theme.muted_foreground).overflow_hidden().whitespace_nowrap().text_ellipsis().child(sub))),
        )
        .child(div().w(px(72.)).flex_shrink_0().text_right().text_size(px(12.)).text_color(theme.muted_foreground).font_features(ui::tabular()).child(time))
}

/// The log card.
fn log_card(theme: &gpui_kit::component::Theme, id: &'static str, entity: &Entity<Job>, job: &Job, running: bool, hide: Option<fn(&str) -> bool>) -> Div {
    let count = job.log().len();
    let handle = job.log_scroll().clone();
    let follow = job.follow();
    // Follow the newest line while at the bottom; stay put once scrolled up
    // (a jump still pending is not a scroll by the user).
    let pending = handle.0.borrow().deferred_scroll_to_item.is_some();
    if !pending {
        if let Some(end) = handle.is_scrolled_to_end() {
            follow.set(end);
        }
    }
    if follow.get() && count > 0 {
        handle.scroll_to_item(count - 1, gpui_kit::ScrollStrategy::Bottom);
    }
    let kept_note = (job.log_total() > count).then(|| format!("The last {count} of {} lines; Save has them all", job.log_total()));
    let (copy_job, save_job) = (entity.clone(), entity.clone());
    let colors = (theme.foreground, theme.primary, theme.danger, theme.muted_foreground);
    let list_job = entity.clone();
    let list = uniform_list(SharedString::from(format!("{id}-log")), count, move |range, _, cx| {
        let (fg, step, bad, muted) = colors;
        list_job
            .read(cx)
            .log_lines(range)
            .into_iter()
            .map(|l| {
                let hidden = hide.is_some_and(|h| h(&l));
                let color = if l.starts_with(">> ") {
                    step
                } else if l.starts_with("error") || l.starts_with("✗") || l.starts_with("Error") {
                    bad
                } else if hidden {
                    muted
                } else {
                    fg
                };
                div().h(px(LINE_H)).w_full().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(color).child(if hidden { SharedString::from("") } else { l })
            })
            .collect()
    })
    .track_scroll(&handle)
    .size_full()
    .px_3()
    .py_2();
    let at_end = follow.clone();
    let jump_handle = handle.clone();
    let jump_job = entity.clone();
    ui::panel(theme)
        .child(
            h_flex()
                .gap(ui::button_gap())
                .items_center()
                .child(div().flex_1().min_w_0().font_weight(FontWeight::MEDIUM).child("Log"))
                .when_some(kept_note, |r, t| r.child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(12.)).text_color(theme.muted_foreground).child(t)))
                .child(ui::probed(ui::button(SharedString::from(format!("{id}-log-copy"))).outline().label("Copy").disabled(count == 0).on_click(move |_, _, cx| {
                    let text = copy_job.read(cx).log_text();
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                })))
                .child(ui::probed(ui::button(SharedString::from(format!("{id}-log-save"))).outline().label("Save").disabled(count == 0).on_click(move |_, _, cx| save_log(&save_job, cx)))),
        )
        .child(
            div()
                .id(SharedString::from(format!("{id}-log-box")))
                .relative()
                .h(px(LOG_HEIGHT))
                .w_full()
                // Scrolling the log scrolls the log only, not the page.
                .on_scroll_wheel({
                    let h = handle.0.borrow().base_handle.clone();
                    move |_, _, cx| ui::keep_wheel(&h, cx)
                })
                .rounded(px(8.))
                .border_1()
                .border_color(theme.border)
                .bg(theme.muted)
                .font_family(theme.mono_font_family.clone())
                .text_size(px(12.5))
                .line_height(px(LINE_H))
                .overflow_hidden()
                .child(if count == 0 {
                    div().p_3().text_color(theme.muted_foreground).child(if running { "Waiting for the first lines…" } else { "No log lines." }).into_any_element()
                } else {
                    list.into_any_element()
                })
                .when(!follow.get() && count > 0, |d| {
                    d.child(div().absolute().right(px(14.)).bottom(px(12.)).child(ui::probed(ui::button(SharedString::from(format!("{id}-log-latest"))).primary().icon(IconName::ChevronDown).label("Latest").on_click(move |_, _, cx| {
                        at_end.set(true);
                        let n = jump_job.read(cx).log().len();
                        if n > 0 {
                            jump_handle.scroll_to_item(n - 1, gpui_kit::ScrollStrategy::Bottom);
                        }
                        jump_job.update(cx, |_, cx| cx.notify());
                    }))))
                }),
        )
}

/// Save the whole log to a file the user picks.
fn save_log(job: &Entity<Job>, cx: &mut App) {
    let (file, text) = {
        let j = job.read(cx);
        (j.log_file().map(|p| p.to_path_buf()), j.log_text())
    };
    let name = file.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "job.log".into());
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(std::path::PathBuf::from).unwrap_or_default();
    let rx = cx.prompt_for_new_path(&home, Some(&name));
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(path))) = rx.await else { return };
        let r = match &file {
            Some(f) if f.exists() => std::fs::copy(f, &path).map(|_| ()),
            _ => std::fs::write(&path, text),
        };
        if let Err(e) = r {
            cx.update(|cx| ui::show_error("The log was not saved", format!("{}: {e}", path.display()), cx));
        }
    })
    .detach();
}

/// "Back to <page>".
pub fn back_button(id: &'static str, page: &str, on: impl Fn(&mut Window, &mut App) + 'static) -> AnyElement {
    crate::ui::button(id).outline().label(format!("Back to {page}")).on_click(move |_, window, cx| on(window, cx)).into_any_element()
}

/// Screens and the layout test: SPM_DEBUG_JOB_LOG=<lines>[:<step>] fills
/// the page's job with a plan stopped at <step> and <lines> log lines.
#[cfg(debug_assertions)]
pub fn debug_job(job: &Entity<Job>, plan: Vec<PlanStep>, cx: &mut App) -> bool {
    let Ok(v) = std::env::var("SPM_DEBUG_JOB_LOG") else { return false };
    let (lines, at) = v.split_once(':').map(|(a, b)| (a.parse().unwrap_or(4000), b.parse().unwrap_or(2))).unwrap_or((v.parse().unwrap_or(4000), 2));
    job.update(cx, |j, cx| j.debug_fill(plan, at, lines, cx));
    true
}

#[cfg(test)]
mod tests {
    use super::clock;

    #[test]
    fn times_read_as_a_clock() {
        assert_eq!(clock(std::time::Duration::from_secs(65)), "1:05");
        assert_eq!(clock(std::time::Duration::from_secs(3723)), "1:02:03");
    }
}
