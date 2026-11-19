// Copyright © 2016-2026 The SENTIENT Authors

//! The error panel: what went wrong in a failed job, why, and the fix
//! (sentient_installer_core::diagnose), in SPM's soft banner style (a
//! faint red tint, danger text, 12 px radius). One component for every
//! place a job fails: Setup's install step, a job's own page
//! (crate::manager::jobview), and the job details dialog
//! (crate::manager::strip), for jobs on this computer and on a server.
//!
//! - A one-click fix runs here, as its own small job: on this computer
//!   through the engine (administrator password where it needs root), on a
//!   server through its agent (`--job diagnose-fix <json>`). "Use port NNNN
//!   instead" is the page's own (Setup changes the field); "Ask for the
//!   password again" is the host's Retry.
//! - A command fix is shown to copy; steps are plain text.
//! - The real last error lines always show; the full log and "Copy details"
//!   are one click away. Retry stays the host's own button.

use std::rc::Rc;

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use sentient_installer_core::diagnose::{Diagnosis, Fix, FixAction};

use crate::jobs::{Job, JobState, PageFixes};
use crate::ui;

pub struct DiagnosisView {
    diag: Diagnosis,
    /// The server the failed job ran on (id, name, colour); None: here.
    instance: Option<(String, String, String)>,
    retry: Option<Rc<dyn Fn(&mut App)>>,
    handler: Option<PageFixes>,
    /// The one-click fix that runs (or ran), and its label.
    fix: Entity<Job>,
    fix_label: Option<String>,
    show_log: bool,
    copied: bool,
    _observe: Subscription,
}

impl DiagnosisView {
    pub fn new(diag: Diagnosis, instance: Option<(String, String, String)>, retry: Option<Rc<dyn Fn(&mut App)>>, handler: Option<PageFixes>, cx: &mut Context<Self>) -> Self {
        let fix = cx.new(|_| Job::new());
        let _observe = cx.observe(&fix, |_, _, cx| cx.notify());
        Self { diag, instance, retry, handler, fix, fix_label: None, show_log: false, copied: false, _observe }
    }

    /// Whether this fix can be offered here.
    fn offered(&self, action: &FixAction) -> bool {
        match action {
            FixAction::AskAgain => self.retry.is_some() || self.handler.is_some(),
            FixAction::UsePort { .. } => self.handler.as_ref().is_some_and(|h| h.ports),
            #[cfg(windows)]
            FixAction::StartDocker => self.instance.is_some(),
            #[cfg(not(windows))]
            FixAction::StartWsl => self.instance.is_some(),
            _ => true,
        }
    }

    fn run_fix(&mut self, label: String, action: FixAction, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(h) = self.handler.clone() {
            if (h.handler)(&action, window, cx) {
                return;
            }
        }
        if action == FixAction::AskAgain {
            if let Some(r) = self.retry.clone() {
                crate::modal::close(cx);
                r(cx);
            }
            return;
        }
        if action.is_page_action() || self.fix.read(cx).is_running() {
            return;
        }
        self.fix_label = Some(label.clone());
        let inst = self.instance.as_ref().and_then(|(id, ..)| crate::remote::load().into_iter().find(|i| &i.id == id));
        let remote = self.instance.is_some();
        self.fix.update(cx, |job, cx| {
            job.start(
                move |sink| match inst {
                    Some(inst) => {
                        let json = serde_json::to_string(&action).map_err(|e| e.to_string())?;
                        let id = crate::remote::start_job(&inst, &["diagnose-fix", &json])?;
                        crate::remote::follow(&inst, &id, &sink)
                    }
                    None if remote => Err(n!("The server is no longer in the list.").into()),
                    None => sentient_installer_core::diagnose::apply(&action, &sink).map(|message| sink(sentient_installer_core::progress::Progress::Done { message })),
                },
                cx,
            )
        });
        cx.notify();
    }
}

/// A command to copy: monospace on a faint panel, with Copy.
fn command_box(theme: &gpui_kit::component::Theme, id: SharedString, label: &str, command: &str) -> Div {
    command_box_in(theme, id, label, command, theme.danger.opacity(0.18))
}

/// [`command_box`] with its own border colour (outside the error panel).
pub(crate) fn command_box_in(theme: &gpui_kit::component::Theme, id: SharedString, label: &str, command: &str, border: Hsla) -> Div {
    let copy = command.to_string();
    v_flex()
        .gap_1()
        .child(div().text_sm().child(label.to_string()))
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .pl_3()
                .pr_1()
                .py_1()
                .rounded(theme.radius)
                .bg(theme.background.opacity(0.7))
                .border_1()
                .border_color(border)
                .child(div().flex_1().min_w_0().font_family(theme.mono_font_family.clone()).text_size(px(12.5)).text_color(theme.foreground).child(command.to_string()))
                .child(
                    ui::button(id)
                        .ghost()
                        .small()
                        .icon(IconName::Copy)
                        .tooltip(t!("Copy the command"))
                        .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))),
                ),
        )
}

impl Render for DiagnosisView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let d = self.diag.clone();
        let (fg, bg) = (theme.danger, theme.danger.opacity(0.12));
        let fix = self.fix.read(cx);
        let (fix_state, fix_step) = (fix.state().clone(), fix.step_with_time());
        let fixing = fix.is_running();
        let fix_log: Vec<SharedString> = fix.log().iter().rev().take(6).rev().cloned().collect();

        let mut actions: Vec<AnyElement> = Vec::new();
        let mut commands: Vec<Div> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        let mut first = true;
        for (i, f) in d.fixes.iter().enumerate() {
            match f {
                Fix::Action { label, action } if self.offered(action) => {
                    let (label, action) = (label.clone(), action.clone());
                    let mut b = ui::button(SharedString::from(format!("diag-fix-{i}"))).label(label.clone()).disabled(fixing);
                    b = if first { b.primary() } else { b.outline() };
                    first = false;
                    actions.push(b.on_click(cx.listener(move |this, _, window, cx| this.run_fix(label.clone(), action.clone(), window, cx))).into_any_element());
                }
                Fix::Action { .. } => {}
                Fix::Command { label, command } => commands.push(command_box(&theme, SharedString::from(format!("diag-copy-{i}")), label, command)),
                Fix::Steps { text } => notes.push(text.clone()),
            }
        }
        let details = d.details.clone();
        let copied = self.copied;
        let show_log = self.show_log;
        let full: Vec<String> = d.details.split("\nLast output lines:\n").nth(1).unwrap_or("").lines().map(str::to_string).collect();

        h_flex()
            .id("diagnosis")
            .gap_3()
            .items_start()
            .px_4()
            .py_3()
            .rounded(px(ui::PANEL_RADIUS))
            .bg(bg)
            .text_color(fg)
            .child(Icon::new(IconName::CircleX).text_color(fg).mt(px(2.)).flex_shrink_0())
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_3()
                    .text_sm()
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(d.cause.clone()))
                            .child(div().child(d.why.clone())),
                    )
                    .when(!actions.is_empty(), |c| c.child(ui::button_row(actions)))
                    .when(fixing || !matches!(fix_state, JobState::Idle), |c| {
                        let label = self.fix_label.clone().unwrap_or_default();
                        c.child(match &fix_state {
                            JobState::Running => div().text_color(theme.foreground).child(format!("{label}: {fix_step}")).into_any_element(),
                            JobState::Done(m) => ui::outcome(&theme, ui::Tone::Good, m.to_string()).into_any_element(),
                            JobState::Failed(m) => ui::outcome(&theme, ui::Tone::Bad, tf!("{label} did not finish: {}", crate::remote::split_failure(m).0)).into_any_element(),
                            JobState::Idle => div().into_any_element(),
                        })
                        .when(fixing && !fix_log.is_empty(), |c| {
                            c.child(v_flex().font_family(theme.mono_font_family.clone()).text_size(px(12.)).text_color(theme.muted_foreground).children(fix_log.into_iter().map(|l| div().child(l))))
                        })
                    })
                    .children(commands)
                    .children(notes.into_iter().map(|n| div().child(n)))
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_xs().font_weight(FontWeight::MEDIUM).child(if d.is_known() { t!("What it printed last") } else { t!("The last lines it printed") }))
                            .child(
                                v_flex()
                                    .id("diag-last")
                                    .max_h(px(160.))
                                    .overflow_y_scroll()
                                    .px_2()
                                    .py_1p5()
                                    .rounded(theme.radius)
                                    .bg(theme.background.opacity(0.7))
                                    .font_family(theme.mono_font_family.clone())
                                    .text_size(px(12.))
                                    .text_color(theme.foreground)
                                    .children(d.last_lines.iter().map(|l| div().child(l.clone()))),
                            ),
                    )
                    .when(show_log, |c| {
                        c.child(
                            v_flex()
                                .id("diag-full-log")
                                .max_h(px(260.))
                                .overflow_y_scroll()
                                .px_2()
                                .py_1p5()
                                .rounded(theme.radius)
                                .bg(theme.background.opacity(0.7))
                                .font_family(theme.mono_font_family.clone())
                                .text_size(px(12.))
                                .text_color(theme.muted_foreground)
                                .children(full.into_iter().map(|l| div().child(l))),
                        )
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                ui::button("diag-show-log")
                                    .ghost()
                                    .small()
                                    .label(if show_log { t!("Hide the full log") } else { t!("Show the full log") })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.show_log = !this.show_log;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                ui::button("diag-copy-details")
                                    .ghost()
                                    .small()
                                    .label(if copied { t!("Copied") } else { t!("Copy details") })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(details.clone()));
                                        this.copied = true;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
    }
}

/// Screens and the layout gate: a canned failure, SPM_DEBUG_DIAGNOSIS=<cause
/// id> (pg-old-major, port-in-use, auth-cancelled, disk-full, unknown).
#[cfg(debug_assertions)]
pub fn debug_diagnosis() -> Option<Diagnosis> {
    use sentient_installer_core::diagnose::{Evidence, PortUse, UnitLog, diagnose};
    let id = std::env::var("SPM_DEBUG_DIAGNOSIS").ok()?;
    let mut ev = match id.as_str() {
        "pg-old-major" => {
            let mut ev = Evidence::from_text("pkexec exited with 42", &["!! /var/lib/postgres/data holds a PostgreSQL 17 database; PostgreSQL 18 is installed.".into()]);
            ev.units.push(UnitLog {
                unit: "postgresql.service".into(),
                journal: vec![
                    t!("An old version of the database format was found.").into(),
                    t!("See https://wiki.archlinux.org/index.php/PostgreSQL#Upgrading_PostgreSQL").into(),
                    t!("postgresql.service: Failed with result 'exit-code'.").into(),
                ],
            });
            ev
        }
        "port-in-use" => {
            let mut ev = Evidence::from_text("pkexec exited with 43", &[
                t!("!! PostgreSQL stopped while starting. Its log:").into(),
                "LOG:  could not bind IPv4 address \"127.0.0.1\": Address already in use".into(),
                "HINT:  Is another postmaster already running on port 5432? If not, wait a few seconds and retry.".into(),
                "FATAL:  could not create any TCP/IP sockets".into(),
            ]);
            ev.ports.push(PortUse { port: 5432, owner: "postgres in /opt/pgsql-17/bin".into(), free: Some(5433) });
            ev
        }
        "auth-cancelled" => Evidence::from_text(t!("Administrator authorisation was refused or cancelled."), &[t!("Error executing command as another user: Request dismissed").into()]),
        "disk-full" => {
            let mut ev = Evidence::from_text("tar exited with 2", &["tar: /opt/sentient/ui/assets/main.js: Cannot write: No space left on device".into()]);
            ev.disks.push(sentient_installer_core::diagnose::Disk { mount: "/".into(), avail: 52_000_000, used_pct: 100 });
            ev.cache = Some(("/home/ops/.cache/sentient-platform-manager/downloads".into(), 1_380_000_000));
            ev
        }
        _ => Evidence::from_text("pkexec exited with 3", &["copying files".into(), t!("tar: sentient.tar: Cannot open: Input/output error").into()]),
    };
    ev.lines.insert(0, t!(">> Installing PostgreSQL from your distribution").into());
    Some(diagnose(&ev))
}

/// A job's result where a page shows it: its error panel when it failed
/// and was diagnosed, else the plain outcome block.
pub fn outcome(theme: &gpui_kit::component::Theme, tone: ui::Tone, text: impl Into<SharedString>, job: &Job) -> AnyElement {
    match job.diagnosis_view() {
        Some(view) if tone == ui::Tone::Bad => view.into_any_element(),
        _ => ui::outcome(theme, tone, text).into_any_element(),
    }
}
