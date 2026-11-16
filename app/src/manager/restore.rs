// Copyright © 2016-2026 The SENTIENT Authors

//! The Restore flow on the Backup page, for this computer and for a server
//! (docs/mockups/restore.html, lanes R, X and B): "Restore from…" with its
//! three sources, then in the page Check, What, Where, Pre-check, Settings
//! and Confirm, the run in the page and the progress strip, and its end:
//! done, failed before the switch (resume), rolled back, or stopped by the
//! free-space watch. Every block says why and how to fix it, with a button
//! wherever the Platform Manager can do the step itself.
//!
//! The work runs in crate::restore_flow: here, as root where the
//! installation needs it (crate::rootjob), or on the server through its
//! agent (`--job restore-*`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, h_flex, v_flex,
    button::ButtonVariants as _,
    checkbox::Checkbox,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    progress::Progress as ProgressBar,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use sentient_backup_core::restore_scope::Scope;
use sentient_installer_core::progress::{Progress, ProgressFn};

use crate::installation::InstallationRecord;
use crate::jobs::{Job, JobState};
use crate::remote::{self, Instance};
use crate::restore_flow::{self as rf, DiskRow, Entry, Inspection, Precheck, Request, Result_, SettingKind, Unreadable};
use crate::ui::{self, StepMark, Tone};

pub(crate) fn icon(name: &str) -> Icon {
    Icon::empty().path(SharedString::from(format!("icons/{name}.svg")))
}

fn gb(b: u64) -> String {
    crate::dbstatus::bytes_label(b)
}

// ---------------------------------------------------------------------------
// Where the restore goes
// ---------------------------------------------------------------------------

/// The instance a restore goes into.
#[derive(Clone)]
pub enum Target {
    Here(InstallationRecord),
    Server(Instance),
}

impl Target {
    pub fn name(&self) -> String {
        match self {
            Target::Here(_) => "this computer".into(),
            Target::Server(i) => i.name.clone(),
        }
    }

    /// "INVENIA IIoT", or "this instance" for this computer.
    fn instance_name(&self) -> String {
        match self {
            Target::Here(_) => "this instance".into(),
            Target::Server(i) => i.name.clone(),
        }
    }

    /// What the confirm step asks to type.
    fn confirm_word(&self) -> String {
        match self {
            Target::Here(_) => "this computer".into(),
            Target::Server(i) => i.name.clone(),
        }
    }

    fn is_server(&self) -> bool {
        matches!(self, Target::Server(_))
    }

    fn windows(&self) -> bool {
        match self {
            Target::Here(_) => cfg!(windows),
            Target::Server(i) => i.os == remote::Os::Windows,
        }
    }

    fn chooser_target(&self) -> crate::manager::folder_chooser::Target {
        match self {
            Target::Here(_) => crate::manager::folder_chooser::Target::Local,
            Target::Server(i) => crate::manager::folder_chooser::Target::Remote(i.clone()),
        }
    }

    /// Run a restore job (`restore-inspect`, `-precheck`, `-run`), here or
    /// there. Blocking (a job's work).
    fn run(&self, job: &str, req: &Request, passphrase: Option<&str>, sink: &ProgressFn) -> Result<(), String> {
        match self {
            Target::Server(inst) => {
                let args = remote::restore_args(inst, job, req, passphrase)?;
                sink(Progress::Log { line: format!("On {}", inst.name) });
                remote::run_followed(inst, &args, sink)
            }
            Target::Here(record) => {
                if crate::rootjob::needed(record) {
                    let mut args = vec![job.to_string()];
                    if job == "restore-inspect" {
                        args.push(req.file.display().to_string());
                    } else {
                        let f = std::env::temp_dir().join(format!(".spm-restore-request-{}.json", std::process::id()));
                        std::fs::write(&f, serde_json::to_string(req).unwrap_or_default()).map_err(|e| e.to_string())?;
                        args.push(f.display().to_string());
                    }
                    if passphrase.is_some() {
                        args.push("--passphrase-stdin".into());
                    }
                    return crate::rootjob::run_with_input(&args, passphrase, sink).map(|_| ());
                }
                match job {
                    "restore-inspect" => match rf::inspect(Some(record), &req.file, passphrase, sink) {
                        Ok(i) => {
                            sink(rf::data_line("inspect", &i));
                            Ok(())
                        }
                        Err(u) => {
                            sink(rf::data_line("unreadable", &u));
                            Err(u.message())
                        }
                    },
                    "restore-precheck" => rf::precheck(record, req, passphrase, sink).map(|p| sink(rf::data_line("precheck", &p))),
                    _ => rf::run(record.clone(), req.clone(), passphrase.map(str::to_string), sink.clone()),
                }
            }
        }
    }

    pub fn history_pub(&self) -> Result<Vec<Entry>, String> {
        self.history()
    }

    fn history(&self) -> Result<Vec<Entry>, String> {
        #[cfg(debug_assertions)]
        if let Some(p) = std::env::var_os("SPM_DEBUG_RESTORE_HISTORY") {
            return std::fs::read_to_string(p).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()));
        }
        match self {
            Target::Here(_) => Ok(rf::history()),
            Target::Server(inst) => remote::restore_history(inst),
        }
    }

    fn log(&self, id: &str) -> Result<String, String> {
        #[cfg(debug_assertions)]
        if let Some(t) = std::env::var_os("SPM_DEBUG_RESTORE_LOG") {
            return Ok(t.to_string_lossy().replace("\\n", "\n"));
        }
        match self {
            Target::Here(_) => rf::log_text(id),
            Target::Server(inst) => remote::restore_log(inst, id),
        }
    }

    fn cancel(&self) {
        match self {
            Target::Here(record) => {
                if crate::rootjob::needed(record) {
                    let _ = crate::rootjob::run(&["restore-cancel".to_string()], &sentient_installer_core::progress::noop());
                } else {
                    let _ = rf::request_cancel();
                }
            }
            Target::Server(inst) => {
                let _ = remote::restore_cancel(inst);
            }
        }
    }
}

/// Where the backup comes from, as the flow names it.
#[derive(Clone, Debug, Default)]
pub struct Source {
    /// The file on the machine the restore runs on.
    pub file: PathBuf,
    /// "sentient-2026-11-14_1712 from Plant B".
    pub sub: String,
    /// For the history: "copied from Plant B, server to server".
    pub note: Option<String>,
}

/// The flow asks its page to leave it (another page may open, crate::manager::restore::go).
pub struct Leave;

impl EventEmitter<Leave> for RestoreFlow {}

// ---------------------------------------------------------------------------
// Small parts shared by the steps
// ---------------------------------------------------------------------------

const STEPS: [&str; 7] = ["Source", "Check", "What", "Where", "Pre-check", "Settings", "Confirm"];

/// The wizard's steps: done ✓, this one filled, the rest outlined.
fn stepper(theme: &gpui_kit::component::Theme, current: usize) -> Div {
    let mut row = h_flex().flex_wrap().gap(px(8.)).items_center().text_sm();
    for (i, s) in STEPS.iter().enumerate() {
        if i > 0 {
            row = row.child(div().w(px(16.)).h(px(1.)).bg(theme.border));
        }
        let (fill, fg, border, text) = if i < current {
            (theme.sidebar_accent, theme.primary, theme.sidebar_accent, "✓".to_string())
        } else if i == current {
            (theme.primary, theme.primary_foreground, theme.primary, (i + 1).to_string())
        } else {
            (theme.background, theme.muted_foreground, theme.border, (i + 1).to_string())
        };
        row = row.child(
            h_flex()
                .gap(px(8.))
                .items_center()
                .child(div().size(px(20.)).flex_shrink_0().rounded_full().bg(fill).border_1().border_color(border).flex().items_center().justify_center().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(fg).child(text))
                .child(div().whitespace_nowrap().when(i == current, |d| d.font_weight(FontWeight::MEDIUM)).when(i > current, |d| d.text_color(theme.muted_foreground)).child(*s)),
        );
    }
    row
}

/// Label / value rows with lines between (the mockup's `kv`).
fn kv(theme: &gpui_kit::component::Theme, rows: Vec<(AnyElement, AnyElement)>) -> Div {
    let n = rows.len();
    v_flex().children(rows.into_iter().enumerate().map(|(i, (k, v))| {
        h_flex()
            .gap_4()
            .py(px(8.))
            .items_start()
            .text_sm()
            .when(i + 1 < n, |r| r.border_b_1().border_color(theme.border))
            .child(h_flex().w(px(if ui::narrow() { 128. } else { 186. })).flex_shrink_0().gap_1().items_center().text_color(theme.muted_foreground).child(k))
            .child(div().flex_1().min_w_0().child(v))
    }))
}

fn txt(t: impl Into<SharedString>) -> AnyElement {
    div().child(t.into()).into_any_element()
}

fn muted(theme: &gpui_kit::component::Theme, t: impl Into<SharedString>) -> Div {
    div().text_sm().text_color(theme.muted_foreground).child(t.into())
}

/// A bordered table: column weights, a header, rows of cells.
fn table(theme: &gpui_kit::component::Theme, weights: &[f32], head: &[&str], rows: Vec<Vec<AnyElement>>, size: f32) -> Div {
    let total: f32 = weights.iter().sum();
    let cell = |i: usize| div().w(relative(weights[i] / total)).min_w_0().px(px(10.)).py(px(7.));
    let mut t = v_flex().rounded(px(8.)).border_1().border_color(theme.border).overflow_hidden().text_size(px(size));
    t = t.child(h_flex().bg(theme.muted.opacity(0.6)).border_b_1().border_color(theme.border).text_size(px(12.5)).text_color(theme.muted_foreground).children(head.iter().enumerate().map(|(i, h)| cell(i).child(h.to_string()))));
    let n = rows.len();
    for (r, row) in rows.into_iter().enumerate() {
        t = t.child(h_flex().items_center().when(r + 1 < n, |d| d.border_b_1().border_color(theme.border)).children(row.into_iter().enumerate().map(|(i, c)| cell(i).child(c))));
    }
    t
}

/// A check line: ✓ done, a spinner now, ✕ bad, ○ not yet; a note at the right.
fn check_line(theme: &gpui_kit::component::Theme, mark: StepMark, text: impl Into<SharedString>, sub: Option<String>) -> Div {
    let m: AnyElement = match mark {
        StepMark::Done => div().text_color(theme.success_foreground).child("✓").into_any_element(),
        StepMark::Failed => div().text_color(theme.danger).child("✕").into_any_element(),
        StepMark::Running => Icon::new(IconName::LoaderCircle).size(px(14.)).text_color(theme.primary).into_any_element(),
        StepMark::Todo => div().text_color(theme.muted_foreground).child("○").into_any_element(),
    };
    h_flex()
        .gap(px(10.))
        .min_h(px(26.))
        .items_center()
        .text_sm()
        .child(div().w(px(14.)).flex_shrink_0().flex().justify_center().child(m))
        .child(div().flex_1().min_w_0().when(mark == StepMark::Todo, |d| d.text_color(theme.muted_foreground)).when(mark == StepMark::Failed, |d| d.text_color(theme.danger)).child(text.into()))
        .children(sub.map(|s| div().flex_shrink_0().text_size(px(12.5)).text_color(theme.muted_foreground).child(s)))
}

/// A blocked step: why, the steps that fix it, and buttons for them (B1–B9).
fn blk(theme: &gpui_kit::component::Theme, warn: bool, title: impl Into<SharedString>, why: impl Into<SharedString>, steps: &[String], actions: Vec<AnyElement>) -> Div {
    let (border, fg, ic) = if warn { (theme.warning_foreground.opacity(0.5), theme.warning_foreground, IconName::TriangleAlert) } else { (theme.danger.opacity(0.5), theme.danger, IconName::CircleX) };
    v_flex()
        .relative()
        .gap(px(8.))
        .py(px(CARD_PY))
        .px(px(ui::CARD_PX))
        .rounded(px(ui::PANEL_RADIUS))
        .bg(theme.background)
        .border_1()
        .border_color(border)
        .child(ui::layout_probe("panel"))
        .child(h_flex().gap(px(10.)).items_center().font_weight(FontWeight::MEDIUM).child(Icon::new(ic).size(px(18.)).text_color(fg)).child(title.into()))
        .child(div().text_sm().child(why.into()))
        .when(!steps.is_empty(), |c| {
            c.child(div().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).text_color(theme.muted_foreground).child("To fix it"))
                .child(v_flex().gap(px(4.)).text_sm().children(steps.iter().enumerate().map(|(i, s)| h_flex().gap(px(8.)).items_start().child(div().w(px(14.)).flex_shrink_0().text_color(theme.muted_foreground).child(format!("{}.", i + 1))).child(div().flex_1().min_w_0().child(s.clone())))))
        })
        .when(!actions.is_empty(), |c| c.child(ui::button_row(actions)))
}

const CARD_PY: f32 = ui::CARD_PY;

/// A card with a title (and something at its right).
fn card(theme: &gpui_kit::component::Theme, title: impl Into<SharedString>, right: Option<AnyElement>) -> Div {
    ui::panel(theme).child(h_flex().gap_3().items_center().min_h(px(24.)).child(div().flex_1().min_w_0().font_weight(FontWeight::MEDIUM).child(title.into())).children(right))
}

/// A tinted box inside a card (Keep or Clear, the date range).
fn tint(theme: &gpui_kit::component::Theme) -> Div {
    v_flex().gap(px(8.)).p(px(12.)).rounded(px(10.)).border_1().border_color(theme.primary).bg(theme.sidebar_accent.opacity(0.5))
}

/// The row of the wizard's buttons: Back at the left, Cancel and Next at
/// the right, at the bottom of the page.
fn footer(back: Option<AnyElement>, cancel: AnyElement, next: Option<AnyElement>) -> Div {
    h_flex().mt_auto().pt(px(4.)).gap_2().items_center().children(back).child(div().flex_1()).child(cancel).children(next)
}

fn progress_bar(theme: &gpui_kit::component::Theme, id: &'static str, value: f32, bad: bool) -> AnyElement {
    if bad {
        h_flex().flex_1().h(px(6.)).rounded_full().bg(theme.muted).child(div().h_full().w(relative((value / 100.).clamp(0., 1.))).rounded_full().bg(theme.danger)).into_any_element()
    } else {
        div().flex_1().child(ProgressBar::new(id).value(value)).into_any_element()
    }
}

// ---------------------------------------------------------------------------
// Opening another page
// ---------------------------------------------------------------------------

static GO: std::sync::Mutex<Option<&'static str>> = std::sync::Mutex::new(None);

/// Ask the shell for another page ("update", "files", "migration", …).
pub fn go(page: &'static str, cx: &mut App) {
    *GO.lock().unwrap() = Some(page);
    cx.refresh_windows();
}

/// The page asked for, once (the shell's render).
pub fn take_go() -> Option<&'static str> {
    GO.lock().unwrap().take()
}

// ---------------------------------------------------------------------------
// The flow
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Step {
    Check,
    What,
    Where,
    Precheck,
    Settings,
    Confirm,
    Run,
}

impl Step {
    fn index(self) -> usize {
        match self {
            Step::Check => 1,
            Step::What => 2,
            Step::Where => 3,
            Step::Precheck => 4,
            Step::Settings => 5,
            Step::Confirm => 6,
            Step::Run => 7,
        }
    }
}

pub struct RestoreFlow {
    target: Target,
    source: Source,
    step: Step,
    check: Entity<Job>,
    inspection: Option<Inspection>,
    unreadable: Option<Unreadable>,
    check_error: Option<String>,
    pass: Entity<InputState>,
    passphrase: Option<String>,
    /// 0 everything, 1 configuration only, 2 telemetry only, 3 a date range.
    scope: usize,
    keep: bool,
    from: Entity<InputState>,
    to: Entity<InputState>,
    skip: Vec<String>,
    /// 0 replace, 1 a new instance here, 2 a new instance on another server.
    place: usize,
    pre: Entity<Job>,
    precheck: Option<Precheck>,
    pre_error: Option<String>,
    safety_dir: Option<PathBuf>,
    temp_dir: Option<PathBuf>,
    skip_safety: bool,
    use_backup: HashMap<String, bool>,
    show_all: bool,
    db_type_backup: bool,
    open_licence_after: bool,
    safety: bool,
    delete_old: bool,
    confirm: Entity<InputState>,
    run: Entity<Job>,
    planned: Vec<String>,
    outcome: Option<Entry>,
    show_log: bool,
    _subs: Vec<Subscription>,
}

impl RestoreFlow {
    pub fn new(target: Target, source: Source, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let pass = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Passphrase"));
        let from = cx.new(|cx| InputState::new(window, cx).placeholder("1 Sep 2026"));
        let to = cx.new(|cx| InputState::new(window, cx).placeholder("14 Nov 2026"));
        let confirm = cx.new(|cx| InputState::new(window, cx));
        let check = cx.new(|_| Job::new());
        let pre = cx.new(|_| Job::new());
        let run = cx.new(|_| Job::new());
        let subs = vec![
            cx.observe(&check, |f: &mut Self, job, cx| {
                if !job.read(cx).is_running() && f.inspection.is_none() && f.unreadable.is_none() {
                    f.read_check(cx);
                }
                cx.notify();
            }),
            cx.observe(&pre, |f: &mut Self, job, cx| {
                if !job.read(cx).is_running() && f.precheck.is_none() {
                    let lines: Vec<String> = job.read(cx).log().iter().map(|l| l.to_string()).collect();
                    f.precheck = rf::read_data(&lines, "precheck");
                    if f.precheck.is_none() {
                        if let JobState::Failed(m) = job.read(cx).state() {
                            f.pre_error = Some(m.to_string());
                        }
                    }
                    f.apply_defaults();
                }
                cx.notify();
            }),
            cx.observe(&run, |f: &mut Self, job, cx| {
                if !job.read(cx).is_running() && f.outcome.is_none() {
                    let lines: Vec<String> = job.read(cx).log().iter().map(|l| l.to_string()).collect();
                    f.outcome = rf::read_data(&lines, "result");
                }
                cx.notify();
            }),
            cx.subscribe(&pass, |f: &mut Self, _, e: &InputEvent, cx| {
                if let InputEvent::PressEnter { .. } = e {
                    f.read_with_passphrase(cx);
                }
            }),
            cx.subscribe(&confirm, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&from, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&to, |_, _, _: &InputEvent, cx| cx.notify()),
        ];
        let mut f = RestoreFlow {
            target,
            source,
            step: Step::Check,
            check,
            inspection: None,
            unreadable: None,
            check_error: None,
            pass,
            passphrase: None,
            scope: 0,
            keep: true,
            from,
            to,
            skip: Vec::new(),
            place: 0,
            pre,
            precheck: None,
            pre_error: None,
            safety_dir: None,
            temp_dir: None,
            skip_safety: false,
            use_backup: HashMap::new(),
            show_all: false,
            db_type_backup: false,
            open_licence_after: true,
            safety: true,
            delete_old: false,
            confirm,
            run,
            planned: Vec::new(),
            outcome: None,
            show_log: false,
            _subs: subs,
        };
        #[cfg(debug_assertions)]
        if let Ok(frame) = std::env::var("SPM_DEBUG_RESTORE") {
            canned::inputs(&frame, &f, window, cx);
        }
        f.start_check(cx);
        f
    }

    fn label(&self, what: &str) -> String {
        match &self.target {
            Target::Server(i) => format!("{what} on {}", i.name),
            Target::Here(_) => what.to_string(),
        }
    }

    fn base_request(&self) -> Request {
        Request { file: self.source.file.clone(), ..Default::default() }
    }

    fn start(&self, job: &Entity<Job>, kind: &'static str, label: String, req: Request, cx: &mut Context<Self>) {
        let target = self.target.clone();
        let pass = self.passphrase.clone();
        job.update(cx, |j, cx| {
            j.set_label(label);
            j.set_on_page(true);
            if let Target::Server(i) = &target {
                j.set_instance(i.id.clone(), i.name.clone(), i.color.clone());
            }
            j.start(
                move |sink| {
                    #[cfg(debug_assertions)]
                    if let Ok(frame) = std::env::var("SPM_DEBUG_RESTORE") {
                        return canned::job(&frame, kind, &sink);
                    }
                    target.run(kind, &req, pass.as_deref(), &sink)
                },
                cx,
            )
        });
    }

    // -- Check --------------------------------------------------------------

    fn start_check(&mut self, cx: &mut Context<Self>) {
        self.inspection = None;
        self.unreadable = None;
        self.check_error = None;
        let req = self.base_request();
        let label = self.label("Checking the backup");
        let job = self.check.clone();
        self.start(&job, "restore-inspect", label, req, cx);
        cx.notify();
    }

    fn read_check(&mut self, cx: &mut Context<Self>) {
        let job = self.check.read(cx);
        let lines: Vec<String> = job.log().iter().map(|l| l.to_string()).collect();
        self.inspection = rf::read_data(&lines, "inspect");
        self.unreadable = rf::read_data(&lines, "unreadable");
        if self.inspection.is_none() && self.unreadable.is_none() {
            if let JobState::Failed(m) = job.state() {
                self.check_error = Some(remote::split_failure(m).0);
            }
        }
        if let Some(i) = &self.inspection {
            if !i.has_telemetry() && self.scope >= 2 {
                self.scope = 0;
            }
        }
        #[cfg(debug_assertions)]
        if let Ok(frame) = std::env::var("SPM_DEBUG_RESTORE") {
            if self.inspection.is_some() {
                canned::advance(self, &frame, cx);
            }
        }
    }

    fn read_with_passphrase(&mut self, cx: &mut Context<Self>) {
        let p = self.pass.read(cx).value().to_string();
        if p.is_empty() {
            return;
        }
        self.passphrase = Some(p);
        self.start_check(cx);
    }

    /// The parts as the check reaches them: (label, bytes, mark).
    fn parts(&self, cx: &App) -> (Vec<(String, u64, StepMark)>, u64, u64) {
        let job = self.check.read(cx);
        let lines: Vec<String> = job.log().iter().map(|l| l.to_string()).collect();
        let listed: Vec<(String, u64)> = rf::read_data(&lines, "parts").unwrap_or_default();
        let seen: Vec<(String, u64)> = lines.iter().filter_map(|l| l.strip_prefix("PART\t")).filter_map(|r| r.rsplit_once('\t').map(|(a, b)| (a.to_string(), b.parse().unwrap_or(0)))).collect();
        let running = job.is_running();
        let mut out: Vec<(String, u64, StepMark)> = Vec::new();
        if let Some((first, _)) = seen.first() {
            out.push((first.clone(), 0, StepMark::Done));
        }
        let done: usize = seen.len().saturating_sub(1);
        for (i, (label, bytes)) in listed.iter().enumerate() {
            let mark = if i < done { StepMark::Done } else if i == done && running { StepMark::Running } else { StepMark::Todo };
            out.push((label.clone(), *bytes, mark));
        }
        let total: u64 = listed.iter().map(|l| l.1).sum();
        let read: u64 = seen.iter().skip(1).map(|s| s.1).sum();
        (out, read, total)
    }

    // -- What, Where --------------------------------------------------------

    fn scope(&self, cx: &App) -> Scope {
        match self.scope {
            1 => Scope::Configuration,
            2 => Scope::TelemetryOnly,
            3 => {
                let span = self.inspection.as_ref().and_then(|i| i.telemetry_span()).unwrap_or((0, 0));
                let from = parse_day(&self.from.read(cx).value()).unwrap_or(span.0);
                let to = parse_day(&self.to.read(cx).value()).map(|t| t + 86_400_000).unwrap_or(span.1);
                Scope::Range { from_ms: from, to_ms: to }
            }
            _ => Scope::Everything,
        }
    }

    fn range_valid(&self, cx: &App) -> bool {
        self.scope != 3 || matches!(self.scope(cx), Scope::Range { from_ms, to_ms } if from_ms < to_ms)
    }

    fn set_scope(&mut self, s: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.scope = s;
        if s == 3 {
            if let Some((a, b)) = self.inspection.as_ref().and_then(|i| i.telemetry_span()) {
                if self.from.read(cx).value().is_empty() {
                    let from = day_label(b.saturating_sub(90 * 86_400_000).max(a));
                    self.from.update(cx, |i, cx| i.set_value(from, window, cx));
                    let to = day_label(b - 1);
                    self.to.update(cx, |i, cx| i.set_value(to, window, cx));
                }
            }
        }
        self.precheck = None;
        // Configuration only: the pre-check names the telemetry Keep leaves without a device.
        if s == 1 {
            self.start_precheck(cx);
        }
        cx.notify();
    }

    fn into_live(&self) -> bool {
        self.scope >= 2
    }

    /// The request the choices make.
    fn request(&self, cx: &App) -> Request {
        let mut r = Request {
            file: self.source.file.clone(),
            scope: self.scope(cx),
            keep_telemetry: self.keep,
            skip_members: self.skip.clone(),
            safety_backup: false,
            safety_dir: self.safety_dir.clone(),
            temp_dir: self.temp_dir.clone(),
            delete_old: self.delete_old,
            use_from_backup: self
                .precheck
                .as_ref()
                .map(|p| p.settings.iter().filter(|r| matches!(r.kind, SettingKind::Choice { .. }) && !r.key.starts_with("files:") && self.uses_backup(&r.key, r.kind)).map(|r| r.key.clone()).collect())
                .unwrap_or_default(),
            by: match &self.target {
                Target::Here(_) => rf::by_here(),
                Target::Server(i) => format!("{} on {}", i.user, crate::backup_jobs::host_name().unwrap_or_else(|| "this computer".into())),
            },
            source: self.source.note.clone(),
            resume: false,
            min_free: None,
        };
        r.safety_backup = self.safety && r.safety_offered() && !self.skip_safety;
        r
    }

    fn uses_backup(&self, key: &str, kind: SettingKind) -> bool {
        match kind {
            SettingKind::Choice { backup_default } => *self.use_backup.get(key).unwrap_or(&backup_default),
            SettingKind::DatabaseType => self.db_type_backup,
            _ => false,
        }
    }

    // -- Pre-check ----------------------------------------------------------

    fn start_precheck(&mut self, cx: &mut Context<Self>) {
        self.precheck = None;
        self.pre_error = None;
        // The pre-check measures the safety backup when it may be taken.
        let mut req = self.request(cx);
        req.safety_backup = !self.skip_safety && (self.delete_old || matches!(req.scope, Scope::Range { .. }) || self.safety);
        let label = self.label("Checking the room for the restore");
        let job = self.pre.clone();
        self.start(&job, "restore-precheck", label, req, cx);
        cx.notify();
    }

    fn apply_defaults(&mut self) {
        if let Some(p) = &self.precheck {
            for r in &p.settings {
                if let SettingKind::Choice { backup_default } = r.kind {
                    self.use_backup.entry(r.key.clone()).or_insert(backup_default);
                }
            }
        }
    }

    // -- Steps --------------------------------------------------------------

    fn back(&mut self, cx: &mut Context<Self>) {
        self.step = match self.step {
            Step::Check => return self.leave(None, cx),
            Step::What => Step::Check,
            Step::Where => Step::What,
            Step::Precheck => Step::Where,
            Step::Settings => Step::Precheck,
            Step::Confirm => {
                if self.into_live() {
                    Step::Precheck
                } else {
                    Step::Settings
                }
            }
            Step::Run => Step::Confirm,
        };
        cx.notify();
    }

    fn next(&mut self, cx: &mut Context<Self>) {
        self.step = match self.step {
            Step::Check => Step::What,
            Step::What => Step::Where,
            Step::Where => {
                self.start_precheck(cx);
                Step::Precheck
            }
            Step::Precheck => {
                if self.into_live() {
                    Step::Confirm
                } else {
                    Step::Settings
                }
            }
            Step::Settings => Step::Confirm,
            Step::Confirm => {
                self.start_run(false, cx);
                Step::Run
            }
            Step::Run => Step::Run,
        };
        cx.notify();
    }

    fn leave(&mut self, open: Option<&'static str>, cx: &mut Context<Self>) {
        if let Some(p) = open {
            go(p, cx);
        }
        cx.emit(Leave);
    }

    fn start_run(&mut self, resume: bool, cx: &mut Context<Self>) {
        let mut req = self.request(cx);
        req.resume = resume;
        self.planned = rf::planned_steps(&req);
        self.outcome = None;
        self.show_log = false;
        let label = self.label("Restoring");
        let job = self.run.clone();
        self.start(&job, "restore-run", label, req, cx);
        cx.notify();
    }
}

/// "1 Sep 2026" or "2026-09-01" → ms at the start of that day (UTC).
fn parse_day(t: &str) -> Option<i64> {
    let t = t.trim();
    ["%-d %b %Y", "%d %b %Y", "%Y-%m-%d", "%d.%m.%Y", "%d/%m/%Y"]
        .iter()
        .find_map(|f| chrono::NaiveDate::parse_from_str(t, f).ok())
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|d| d.and_utc().timestamp_millis())
}

fn day_label(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).map(|d| d.format("%-d %b %Y").to_string()).unwrap_or_default()
}

fn mins(secs: u64) -> String {
    let m = secs.div_ceil(60).max(1);
    if m >= 90 { format!("{} h {} min", m / 60, m % 60) } else { format!("{m} min") }
}

// ---------------------------------------------------------------------------
// Drawing the steps
// ---------------------------------------------------------------------------

impl RestoreFlow {
    fn sub(&self) -> String {
        self.source.sub.clone()
    }

    /// The comparison of the backup with the instance (R9, B1).
    fn compare(&self, theme: &gpui_kit::component::Theme, i: &Inspection) -> Div {
        let here = self.target.instance_name();
        let here_cap = if self.target.is_server() { here.clone() } else { "This instance".into() };
        let theirs_v = i.manifest.source.sentient_version.clone().unwrap_or_else(|| "Not recorded".into());
        let bad = |t: String, b: bool| -> AnyElement { div().when(b, |d| d.text_color(theme.danger).font_weight(FontWeight::MEDIUM)).child(t).into_any_element() };
        let type_what = if i.same_type() { "The same type".to_string() } else { format!("History is converted to {}", i.here.database_type.trim_start_matches("PostgreSQL + ")) };
        let version_what = if i.newer_schema() {
            "Newer than this SENTIENT: blocked".to_string()
        } else if theirs_v == i.here.sentient_version {
            "The same version".into()
        } else {
            format!("Brought up to {}", i.here.sentient_version)
        };
        let schema = |s: Option<i64>| s.map(|s| s.to_string()).unwrap_or_else(|| "Not recorded".into());
        let schema_what = match (i.manifest.source.schema_version, i.here.schema_version) {
            (Some(t), Some(o)) if t > o => "Cannot be read here".to_string(),
            (Some(t), Some(o)) if t < o => format!("Upgraded {t} → {o} after the restore"),
            (Some(_), Some(_)) => "The same schema".into(),
            _ => "Checked after the restore".into(),
        };
        let made = format!("{}{}", i.made, i.manifest.source.host.as_ref().map(|h| format!(" on {h}")).unwrap_or_default());
        table(
            theme,
            &[1.1, 1.3, 1.3, 1.7],
            &["", "In the backup", &format!("On {here_cap}"), "What happens"],
            vec![
                vec![txt("Database type"), txt(i.database_type.clone()), txt(i.here.database_type.clone()), txt(type_what)],
                vec![txt("SENTIENT"), bad(theirs_v, i.newer_schema()), txt(i.here.sentient_version.clone()), bad(version_what, i.newer_schema())],
                vec![txt("Schema"), bad(schema(i.manifest.source.schema_version), i.newer_schema()), txt(schema(i.here.schema_version)), bad(schema_what, i.newer_schema())],
                vec![txt("Holds"), txt(capital(&i.holds.join(", "))), txt(""), txt("")],
                vec![txt("Made"), txt(made), txt(""), txt("")],
            ],
            13.5,
        )
    }

    fn check_view(&mut self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> (Div, bool) {
        let running = self.check.read(cx).is_running();
        // R8, B4: the passphrase.
        if let Some(u @ (Unreadable::Passphrase | Unreadable::WrongPassphrase)) = self.unreadable.clone() {
            let wrong = u == Unreadable::WrongPassphrase;
            let body = card(theme, "This backup is encrypted", None)
                .child(muted(theme, "Type its passphrase to read it. Nothing is kept on the server."))
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap_2()
                        .items_center()
                        .child(div().w(px(320.)).max_w_full().when(wrong, |d| d.rounded(theme.radius).border_1().border_color(theme.danger)).child(Input::new(&self.pass).mask_toggle()))
                        .child(ui::button("rs-read").primary().label("Read the backup").disabled(running).on_click(cx.listener(|f, _, _, cx| f.read_with_passphrase(cx)))),
                )
                .when(wrong, |c| {
                    c.child(blk(
                        theme,
                        false,
                        "The passphrase is wrong, or the file is damaged",
                        "The file is encrypted with age; the passphrase did not open it.",
                        &["Type it again: it is case-sensitive.".into(), "If it is surely right, Verify the file from the Backups list.".into()],
                        vec![ui::button("rs-again").primary().label("Try again").on_click(cx.listener(|f, _, _, cx| f.read_with_passphrase(cx))).into_any_element()],
                    ))
                });
            return (body, false);
        }
        // B6: not a backup.
        if let Some(Unreadable::NotABackup(_)) = &self.unreadable {
            return (
                blk(
                    theme,
                    false,
                    "This file is not a SENTIENT backup",
                    "There is no manifest at its start. It may be another program's file, or only part of a split backup.",
                    &["For a split backup (.001, .002, …), choose the first part, or the folder holding all parts.".into(), "Otherwise, choose another file.".into()],
                    vec![ui::button("rs-other-file").outline().label("Choose another file…").on_click(cx.listener(|f, _, _, cx| f.leave(None, cx))).into_any_element()],
                ),
                false,
            );
        }
        if let Some(e) = self.check_error.clone().or_else(|| match &self.unreadable {
            Some(Unreadable::Other(m)) => Some(m.clone()),
            _ => None,
        }) {
            return (
                blk(theme, false, "The backup could not be read", e, &[], vec![ui::button("rs-check-again").primary().label("Try again").on_click(cx.listener(|f, _, _, cx| f.start_check(cx))).into_any_element()]),
                false,
            );
        }
        let Some(i) = self.inspection.clone() else {
            // R7: reading.
            let (parts, read, total) = self.parts(cx);
            let pct = if total > 0 { read as f32 * 100. / total as f32 } else { 0. };
            let mut c = card(theme, "Checking the backup", None).child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(progress_bar(theme, "rs-check-bar", pct, false))
                    .child(div().flex_shrink_0().text_sm().font_features(ui::tabular()).child(if total > 0 { format!("{} of {}", gb(read), gb(total)) } else { "Reading…".into() })),
            );
            let mut lines = v_flex().gap_0();
            for (label, mark, sub) in condense(&parts) {
                lines = lines.child(check_line(theme, mark, label, sub));
            }
            c = c.child(lines);
            return (c, false);
        };
        // B6: a newer format.
        if i.newer_format() {
            return (
                blk(
                    theme,
                    false,
                    "This backup needs a newer Platform Manager",
                    format!("It is in format {}; this Platform Manager reads formats 1 to {}.", i.manifest.format_version, sentient_backup_core::manifest::FORMAT_VERSION),
                    &[format!("Update the Platform Manager on this computer{}.", if self.target.is_server() { format!(" (and the agent on {} with it)", self.target.name()) } else { String::new() }), "Start the restore again.".into()],
                    vec![ui::button("rs-update-pm").primary().icon(IconName::RefreshCw).label("Update the Platform Manager…").on_click(cx.listener(|f, _, _, cx| f.leave(Some("about"), cx))).into_any_element()],
                ),
                false,
            );
        }
        // B1: a newer schema.
        if i.newer_schema() {
            let here = self.target.instance_name();
            let v = i.manifest.source.sentient_version.clone().unwrap_or_else(|| "a newer SENTIENT".into());
            return (
                v_flex()
                    .gap(px(ui::GAP))
                    .child(blk(
                        theme,
                        false,
                        "This backup is from a newer SENTIENT",
                        format!(
                            "Made on SENTIENT {v} (schema {}). {} runs {} (schema {}) and cannot read newer data.",
                            i.manifest.source.schema_version.unwrap_or_default(),
                            capital(&here),
                            i.here.sentient_version,
                            i.here.schema_version.unwrap_or_default()
                        ),
                        &[format!("Update SENTIENT on {here} to {v} or later."), "Start the restore again; this backup stays chosen.".into()],
                        vec![ui::button("rs-update").primary().icon(IconName::RefreshCw).label("Update SENTIENT…").on_click(cx.listener(|f, _, _, cx| f.leave(Some("update"), cx))).into_any_element()],
                    ))
                    .child(self.compare(theme, &i)),
                false,
            );
        }
        // B5: damaged.
        if !i.sound() {
            let damaged = i.damaged();
            let names: Vec<String> = damaged.iter().map(|d| d.1.clone()).collect();
            let sound = i.parts.iter().filter(|p| p.ok).count();
            let mut steps: Vec<String> = Vec::new();
            let mut acts: Vec<AnyElement> = Vec::new();
            if let Some(copy) = i.other_copy.clone() {
                steps.push(format!("Use the copy in {}, or", copy.parent().map(|p| p.display().to_string()).unwrap_or_default()));
                let label = format!("Use the copy in {}", copy.parent().map(|p| p.display().to_string()).unwrap_or_default());
                acts.push(ui::button("rs-copy").primary().label(label).on_click(cx.listener(move |f, _, _, cx| {
                    f.source.file = copy.clone();
                    f.start_check(cx);
                })).into_any_element());
            }
            steps.push(if i.damage_is_months() { "choose another backup, or".into() } else { "choose another backup.".into() });
            acts.push(ui::button("rs-another").outline().label("Choose another backup…").on_click(cx.listener(|f, _, _, cx| f.leave(None, cx))).into_any_element());
            if i.damage_is_months() {
                let months: Vec<String> = names.iter().map(|n| n.trim_start_matches("Telemetry history, ").to_string()).collect();
                steps.push(format!("restore without {}.", months.join(", ")));
                let skip: Vec<String> = damaged.iter().map(|d| d.0.clone()).collect();
                acts.push(ui::button("rs-skip").outline().label(format!("Restore without {}", months.join(", "))).on_click(cx.listener(move |f, _, _, cx| {
                    f.skip = skip.clone();
                    f.step = Step::What;
                    cx.notify();
                })).into_any_element());
            }
            let marks: Vec<(String, u64, StepMark)> = i.parts.iter().map(|p| (p.label.clone(), p.bytes, if p.ok { StepMark::Done } else { StepMark::Failed })).collect();
            let parts = card(theme, "Parts", None).child(v_flex().children(condense(&marks).into_iter().map(|(l, m, sub)| check_line(theme, m, l, Some(if m == StepMark::Failed { "does not match".into() } else { sub.map(|s| s.replace("SHA-256 matches", "match")).unwrap_or_else(|| "matches".into()) })))).children(i.missing.iter().map(|m| check_line(theme, StepMark::Failed, sentient_backup_core::restore::member_label(m), Some("missing".into())))));
            return (
                v_flex()
                    .gap(px(ui::GAP))
                    .child(blk(
                        theme,
                        false,
                        "This backup is damaged",
                        format!("{} {} not match its SHA-256. The other {sound} part{} are sound.", names.join(", "), if names.len() == 1 { "does" } else { "do" }, if sound == 1 { "" } else { "s" }),
                        &steps,
                        acts,
                    ))
                    .child(parts),
                self.skip.len() == damaged.len() && !self.skip.is_empty(),
            );
        }
        // R9: sound.
        let n = i.parts.len();
        let c = card(theme, "The backup is sound", None)
            .child(ui::outcome(theme, Tone::Good, format!("Every part matches its SHA-256 ({n} parts, {}). Format {}.", gb(i.file_bytes), i.manifest.format_version)))
            .child(self.compare(theme, &i));
        (c, true)
    }

    fn what_view(&mut self, theme: &gpui_kit::component::Theme, window: &mut Window, cx: &mut Context<Self>) -> (Div, bool) {
        let Some(i) = self.inspection.clone() else { return (div(), false) };
        let holds = i.has_telemetry();
        let here = self.target.instance_name();
        let opt = |f: &Self, n: usize, title: &str, desc: String, enabled: bool, cx: &mut Context<Self>| {
            let id = SharedString::from(format!("rs-scope-{n}"));
            ui::radio_card_el(theme, id, div().font_weight(FontWeight::MEDIUM).child(title.to_string()).into_any_element(), desc, f.scope == n, enabled)
                .when(!enabled, |c| c.opacity(0.5))
                .flex_1()
                .min_w(px(240.))
                .when(enabled, |c| c.on_click(cx.listener(move |f, _, window, cx| f.set_scope(n, window, cx))))
        };
        let no = "This backup holds no telemetry.".to_string();
        let options = h_flex()
            .flex_wrap()
            .gap_3()
            .child(opt(self, 0, "Everything", "Configuration and all telemetry in the backup.".into(), true, cx))
            .child(opt(self, 1, "Configuration only, without telemetry", "Devices, dashboards, users and settings. Offered even when the backup holds telemetry.".into(), true, cx))
            .child(opt(self, 2, "Telemetry only", if holds { format!("History from the backup into {here}; its configuration stays.") } else { no.clone() }, holds, cx))
            .child(opt(self, 3, "Telemetry for a date range", if holds { "From the backup, within the dates you pick.".into() } else { no }, holds, cx));
        let mut c = card(theme, "What to restore", None).child(options);
        let span = i.telemetry_span();
        match self.scope {
            1 => {
                let live = self.precheck.as_ref().map(|p| p.live.clone());
                let help = live.as_ref().map(|l| format!("{} of history now. Kept history stays tied to device ids; a device that the backup does not have keeps its history, but nothing shows it.", gb(l.telemetry_bytes))).unwrap_or_else(|| "Kept history stays tied to device ids; a device that the backup does not have keeps its history, but nothing shows it.".into());
                let mut t = tint(theme)
                    .child(h_flex().gap_2().items_center().child(div().font_weight(FontWeight::MEDIUM).child(format!("The telemetry on {here} now"))).child(ui::info("rs-keep-help".into(), help.into())))
                    .child({
                        let mut seg = ui::seg_track(theme);
                        seg = seg.child(ui::seg_item(theme, "rs-keep", "Keep it", self.keep).on_click(cx.listener(|f, _, _, cx| {
                            f.keep = true;
                            f.start_precheck(cx);
                        })));
                        seg = seg.child(ui::seg_item(theme, "rs-clear", "Clear it", !self.keep).on_click(cx.listener(|f, _, _, cx| {
                            f.keep = false;
                            f.precheck = None;
                            cx.notify();
                        })));
                        seg
                    });
                let orphans = self.precheck.as_ref().and_then(|p| p.orphans.clone());
                t = if self.keep {
                    t.child(ui::outcome(theme, Tone::Neutral, match orphans {
                        Some((names, n)) if n > 0 => format!("Telemetry of {n} device{} not in the backup stays without a device ({}{}). It is removed with the next retention clean-up or by hand.", if n == 1 { "" } else { "s" }, names.join(", "), if n > names.len() { ", …" } else { "" }),
                        Some(_) => "Every device with telemetry here is in the backup: the kept history stays with its devices.".into(),
                        None if self.pre.read(cx).is_running() => "Looking for telemetry the backup has no device for…".into(),
                        None => "Telemetry of devices the backup does not have stays without a device. It is removed with the next retention clean-up or by hand.".into(),
                    }))
                } else {
                    t.child(ui::outcome(theme, Tone::Warn, format!("The history on {here} is deleted after the switch. The data from before is kept beside it until you remove it.")))
                };
                c = c.child(t);
            }
            3 => {
                let scope = self.scope(cx);
                let (months, bytes, all) = match (&scope, &self.inspection) {
                    (Scope::Range { .. }, Some(i)) => {
                        let wanted = sentient_backup_core::restore_scope::members_for(&i.manifest, &scope, &[]);
                        let m: Vec<_> = i.months();
                        let inr: Vec<_> = m.iter().filter(|(y, mo, _)| i.manifest.files.iter().any(|f| sentient_backup_core::restore_scope::member_month(&f.path) == Some((*y, *mo)) && wanted.contains(&f.path))).collect();
                        (inr.len(), inr.iter().map(|x| x.2).sum::<u64>(), m.len())
                    }
                    _ => (0, 0, 0),
                };
                let _ = window;
                c = c.child(
                    tint(theme)
                        .child(
                            h_flex()
                                .flex_wrap()
                                .gap_2()
                                .items_center()
                                .child(div().w(px(44.)).text_sm().text_color(theme.muted_foreground).child("From"))
                                .child(div().w(px(150.)).child(Input::new(&self.from).suffix(icon("calendar").size(px(15.)).text_color(theme.muted_foreground))))
                                .child(div().w(px(24.)).text_sm().text_color(theme.muted_foreground).child("to"))
                                .child(div().w(px(150.)).child(Input::new(&self.to).suffix(icon("calendar").size(px(15.)).text_color(theme.muted_foreground))))
                                .child(muted(theme, if self.range_valid(cx) { format!("{months} of {all} months · about {}", gb(bytes)) } else { "The first day must come before the last.".into() })),
                        )
                        .child(muted(theme, format!("Telemetry on {here} for the same dates is replaced; other dates stay."))),
                );
            }
            _ => {}
        }
        if !holds {
            c = c.child(muted(theme, "This backup was made with “Configuration only”, so it holds no telemetry. Everything restores its configuration."));
        } else if let Some((a, b)) = span {
            c = c.child(muted(theme, format!("The backup holds telemetry from {} to {}.", day_label(a), day_label(b - 1))));
        }
        if !self.skip.is_empty() {
            c = c.child(ui::outcome(theme, Tone::Warn, format!("Left out: {}.", self.skip.iter().map(|p| sentient_backup_core::restore::member_label(p)).collect::<Vec<_>>().join(", "))));
        }
        (c, self.range_valid(cx))
    }

    fn where_view(&mut self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> (Div, bool) {
        let here = self.target.instance_name();
        if self.into_live() {
            let c = card(theme, "Where to restore", None).child(ui::radio_card(theme, "rs-place-live", format!("Into the running data of {here}"), "SENTIENT keeps running; the backup's telemetry is added to its own. Rows already there for the same device, key and time stay as they are.", true));
            return (c, true);
        }
        let opt = |f: &Self, n: usize, title: String, desc: &str, enabled: bool, why: Option<&str>, cx: &mut Context<Self>| {
            let id = SharedString::from(format!("rs-place-{n}"));
            let mut t = h_flex().gap_2().items_center().child(div().font_weight(FontWeight::MEDIUM).child(title));
            if let Some(w) = why {
                t = t.child(ui::info(SharedString::from(format!("rs-place-why-{n}")), w.to_string().into()));
            }
            ui::radio_card_el(theme, id, t.into_any_element(), desc.to_string(), f.place == n, enabled)
                .when(!enabled, |c| c.opacity(0.55))
                .when(enabled, |c| c.on_click(cx.listener(move |f, _, _, cx| {
                    f.place = n;
                    cx.notify();
                })))
        };
        let server = self.target.is_server();
        let c = card(theme, "Where to restore", None).child(
            v_flex()
                .gap_3()
                .child(opt(self, 0, format!("Replace the data of {here}"), &format!("{} keeps its own PostgreSQL{}, ports and licence; only the data inside is replaced.", if server { "This server" } else { "This computer" }, if self.inspection.as_ref().is_some_and(|i| i.here.database_type.contains("ClickHouse")) { ", ClickHouse" } else { "" }), true, None, cx))
                .child(opt(self, 1, format!("A new instance on {}", if server { here.clone() } else { "this computer".into() }), "Beside the current one, on other ports. Good to look at the backup first.", false, Some("The Platform Manager runs one SENTIENT per server for now. Set up another server in Instances, then restore there with “Replace the data”."), cx))
                .child(opt(self, 2, "A new instance on another server".into(), "Pick a server; SENTIENT is set up there and the backup restored into it.", false, Some("Set up SENTIENT on that server first (Instances, Set up), then restore the backup there with “Replace the data”; Restore from… copies it across."), cx)),
        );
        (c, self.place == 0)
    }
}

/// The check's lines, short: a run of three or more telemetry months
/// already checked reads as one line, and the parts still to come after
/// the next two as "and N more".
fn condense(parts: &[(String, u64, StepMark)]) -> Vec<(String, StepMark, Option<String>)> {
    const T: &str = "Telemetry history, ";
    let mut out: Vec<(String, StepMark, Option<String>)> = Vec::new();
    let mut i = 0;
    let mut todo = 0usize;
    while i < parts.len() {
        let (label, _, mark) = &parts[i];
        if *mark == StepMark::Done && label.starts_with(T) {
            let mut j = i;
            while j < parts.len() && parts[j].2 == StepMark::Done && parts[j].0.starts_with(T) {
                j += 1;
            }
            if j - i >= 3 {
                out.push((format!("{T}{} – {}", &parts[i].0[T.len()..], &parts[j - 1].0[T.len()..]), StepMark::Done, Some(format!("{} parts, SHA-256 matches", j - i))));
                i = j;
                continue;
            }
        }
        if *mark == StepMark::Todo {
            todo += 1;
            if todo > 2 {
                let rest = parts.len() - i;
                out.push((format!("and {rest} more part{}", if rest == 1 { "" } else { "s" }), StepMark::Todo, None));
                break;
            }
        }
        let sub = (*mark == StepMark::Done && i > 0).then(|| "SHA-256 matches".to_string());
        out.push((label.clone(), *mark, sub));
        i += 1;
    }
    out
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

impl RestoreFlow {
    fn disk_table(&self, theme: &gpui_kit::component::Theme, p: &Precheck, short: bool) -> Div {
        let rows: Vec<Vec<AnyElement>> = p
            .disks
            .iter()
            .map(|d: &DiskRow| {
                let ok = d.ok(p.min_free);
                let uses = d.uses.iter().map(|(w, b)| format!("{w} {}", gb(*b))).collect::<Vec<_>>().join(" · ");
                let mut r = vec![
                    h_flex().flex_wrap().gap(px(6.)).items_baseline().child(div().child(d.disk.clone())).child(div().text_size(px(12.5)).text_color(theme.muted_foreground).child(d.detail.clone())).into_any_element(),
                    div().text_size(px(13.)).child(if uses.is_empty() { "—".into() } else { uses }).into_any_element(),
                    div().w_full().text_right().whitespace_nowrap().font_features(ui::tabular()).child(gb(d.need)).into_any_element(),
                    div().w_full().text_right().whitespace_nowrap().font_features(ui::tabular()).when(!ok, |x| x.text_color(theme.danger)).child(d.free.map(gb).unwrap_or_else(|| "?".into())).into_any_element(),
                ];
                if short {
                    r.push(div().w_full().text_right().font_features(ui::tabular()).text_color(if ok { theme.success_foreground } else { theme.danger }).child(if ok { "✓".to_string() } else { gb(d.short(p.min_free)) }).into_any_element());
                } else {
                    r.push(div().w_full().text_right().text_color(if ok { theme.success_foreground } else { theme.danger }).child(if ok { "✓" } else { "✕" }).into_any_element());
                }
                r
            })
            .collect();
        if short {
            table(theme, &[1.3, 2.6, 0.75, 0.75, 0.75], &["Disk", "Used for", "Needs", "Free", "Short"], rows, 13.5)
        } else {
            table(theme, &[1.3, 2.6, 0.8, 0.8, 0.4], &["Disk", "Used for", "Needs", "Free", ""], rows, 13.5)
        }
    }

    fn precheck_view(&mut self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> (Div, bool) {
        let here = self.target.instance_name();
        if let Some(e) = self.pre_error.clone() {
            return (blk(theme, false, "The pre-check could not run", e, &[], vec![ui::button("rs-pre-again").primary().label("Try again").on_click(cx.listener(|f, _, _, cx| f.start_precheck(cx))).into_any_element()]), false);
        }
        let Some(p) = self.precheck.clone() else {
            let job = self.pre.read(cx);
            let step = job.step().to_string();
            return (
                card(theme, "Pre-check", None)
                    .child(h_flex().gap_2().items_center().child(Icon::new(IconName::LoaderCircle).size(px(14.)).text_color(theme.primary)).child(muted(theme, if step.is_empty() { "Checking…".to_string() } else { format!("{step}…") })))
                    .child(ui::skeleton_block(theme, "rs-pre-sk1", relative(1.).into(), 120.))
                    .child(ui::skeleton_line(theme, "rs-pre-sk2", 280.)),
                false,
            );
        };
        let Some(i) = self.inspection.clone() else { return (div(), false) };
        let blocked = p.blocked_disks();
        let safety_on = self.delete_old || self.scope == 3 || self.safety;
        let mut out = v_flex().gap(px(ui::GAP));
        // B3: ClickHouse not answering.
        if let Some(ch) = &p.live.ch_problem {
            out = out.child(blk(
                theme,
                false,
                format!("ClickHouse on {here} is not answering"),
                format!("The history goes into ClickHouse. {ch}"),
                &["Start ClickHouse in Control, or Repair if it does not start.".into()],
                vec![
                    ui::button("rs-open-control").outline().icon(icon("power")).label("Open Control…").on_click(cx.listener(|f, _, _, cx| f.leave(Some("control"), cx))).into_any_element(),
                    ui::button("rs-open-repair").outline().icon(icon("wrench")).label("Repair…").on_click(cx.listener(|f, _, _, cx| f.leave(Some("repair"), cx))).into_any_element(),
                ],
            ));
        }
        if !blocked.is_empty() {
            // B2: not enough disk.
            let d = blocked[0];
            let others_ok = p.disks.len() > blocked.len();
            let what = d.uses.iter().map(|u| u.0.to_lowercase()).collect::<Vec<_>>();
            let mut steps = Vec::new();
            let mut acts: Vec<AnyElement> = Vec::new();
            let safety_here = what.iter().any(|w| w.contains("safety"));
            let temp_here = what.iter().any(|w| w.contains("unpacking"));
            if safety_here || temp_here {
                steps.push("Put the safety backup or the temporary files on a disk with room, or".to_string());
            }
            if i.has_telemetry() && self.scope != 1 {
                steps.push("restore a shorter range of telemetry, or".into());
            }
            steps.push(format!("free space on {}, or", d.disk));
            if safety_here {
                steps.push("skip the safety backup.".into());
            } else if let Some(l) = steps.last_mut() {
                *l = l.trim_end_matches(", or").to_string() + ".";
            }
            if safety_here {
                acts.push(ui::button("rs-safety-dir").outline().icon(IconName::Folder).label("Safety backup on another disk…").on_click(cx.listener(|f, _, window, cx| f.choose_dir(true, window, cx))).into_any_element());
            }
            if temp_here {
                acts.push(ui::button("rs-temp-dir").outline().icon(IconName::Folder).label("Temporary files on another disk…").on_click(cx.listener(|f, _, window, cx| f.choose_dir(false, window, cx))).into_any_element());
            }
            if i.has_telemetry() && self.scope != 1 {
                acts.push(ui::button("rs-shorter").outline().icon(icon("calendar")).label("Restore a shorter date range").on_click(cx.listener(|f, _, window, cx| {
                    f.set_scope(3, window, cx);
                    f.step = Step::What;
                    cx.notify();
                })).into_any_element());
            }
            let mut acts2: Vec<AnyElement> = vec![
                h_flex().gap_1().items_center().child(ui::button("rs-free-files").outline().icon(IconName::Folder).label("Free space in Files").on_click(cx.listener(|f, _, _, cx| f.leave(Some("files"), cx)))).child(ui::info("rs-free-files-i".into(), format!("Opens Files on {here} at the largest folders on {}.", d.disk).into())).into_any_element(),
                ui::button("rs-move-data").outline().icon(icon("move-disk")).label("Move data to another disk…").on_click(cx.listener(|f, _, window, cx| {
                    // TODO(worker A): wire the M lane's move-data flow here.
                    crate::manager::movedata::open(f.target.chooser_target(), window, cx);
                })).into_any_element(),
            ];
            if safety_here {
                acts2.push(ui::button("rs-skip-safety").ghost().label("Skip the safety backup…").on_click(cx.listener(|f, _, window, cx| f.ask_skip_safety(window, cx))).into_any_element());
            }
            out = out.child(
                blk(
                    theme,
                    false,
                    format!("Not enough free disk on {}", d.disk),
                    format!("The restore needs about {} on {} with {} left free, and {} is free.{}", gb(d.need), d.disk, gb(p.min_free), d.free.map(gb).unwrap_or_default(), if others_ok { " The other disks have room." } else { "" }),
                    &steps,
                    acts,
                )
                .child(self.disk_table(theme, &p, true))
                .child(ui::button_row(acts2)),
            );
            return (out, false);
        }
        let windows = self.target.windows();
        let mut rows: Vec<(AnyElement, AnyElement)> = vec![(txt("Size"), txt(format!("{} file · about {} once restored", gb(i.file_bytes), gb(p.restored_bytes))))];
        let down = if self.into_live() {
            "SENTIENT keeps running".to_string()
        } else if windows {
            format!("SENTIENT is down about {}: the SENTIENT service stops and starts", mins(p.estimate.downtime_secs))
        } else {
            format!("SENTIENT is down about {}, at the switch", mins(p.estimate.downtime_secs))
        };
        let c = card(theme, "Pre-check", None)
            .child(kv(theme, std::mem::take(&mut rows)))
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Space, per disk"))
            .child(self.disk_table(theme, &p, false))
            .child(kv(
                theme,
                vec![
                    (txt("Takes about"), txt(format!("{} · {down}", mins(p.estimate.total_secs)))),
                    (txt("After the restore"), txt(p.upgrade.clone().unwrap_or_else(|| if self.into_live() { "Nothing to upgrade: only telemetry is added".into() } else { "The same schema: nothing to upgrade".into() }))),
                    (
                        h_flex().gap_1().items_center().child("While it runs").child(ui::info("rs-watch".into(), format!("The restore stops and rolls back when free space on any of these disks falls under {}.", gb(p.min_free)).into())).into_any_element(),
                        txt(format!("Stops cleanly under {} free on any disk", gb(p.min_free))),
                    ),
                ],
            ))
            .when_some(p.orphans.clone().filter(|o| o.1 > 0 && self.scope == 1 && self.keep), |c, (names, n)| {
                c.child(ui::outcome(theme, Tone::Neutral, format!("Telemetry of {n} device{} not in the backup stays without a device ({}{}). It is removed with the next retention clean-up or by hand.", if n == 1 { "" } else { "s" }, names.join(", "), if n > names.len() { ", …" } else { "" })))
            })
            .when(safety_on && self.skip_safety, |c| c.child(ui::outcome(theme, Tone::Warn, "No safety backup: you chose to restore without it.")));
        (out.child(c), true)
    }

    fn settings_view(&mut self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> (Div, bool) {
        let Some(p) = self.precheck.clone() else { return (div(), false) };
        let here = self.target.instance_name();
        let here_cap = if self.target.is_server() { here.clone() } else { "This instance".into() };
        let mut c = card(theme, "Settings that differ", None);
        if !p.recorded {
            c = c.child(ui::outcome(theme, Tone::Neutral, format!("This backup was made before settings were recorded (Platform Manager 0.8.0 or earlier). Only the SENTIENT version, database type and file store paths can be compared; everything else stays as it is on {here}.")));
        } else {
            c = c.child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(muted(theme, format!("{} differ · {} are the same", p.settings.len(), p.same)))
                    .when(p.settings.len() > 8, |r| {
                        r.child(ui::button("rs-show-all").ghost().small().label(if self.show_all { "Show fewer" } else { "Show all" }).on_click(cx.listener(|f, _, _, cx| {
                            f.show_all = !f.show_all;
                            cx.notify();
                        })))
                    }),
            );
        }
        let shown: Vec<_> = if self.show_all || p.settings.len() <= 8 { p.settings.clone() } else { p.settings.iter().take(8).cloned().collect() };
        let mut rows: Vec<Vec<AnyElement>> = Vec::new();
        let mut licence = None;
        for r in shown {
            let key = r.key.clone();
            let choice: AnyElement = match r.kind {
                SettingKind::Fixed => muted(theme, if self.target.is_server() { "This server's" } else { "This computer's" }).into_any_element(),
                SettingKind::Gateway => ui::button(SharedString::from(format!("rs-gw-{key}"))).ghost().small().label("Open Gateway after").on_click(cx.listener(|f, _, _, cx| f.leave(Some("gateway"), cx))).into_any_element(),
                SettingKind::Locked => {
                    licence = Some(r.backup.clone());
                    let mut seg = ui::seg_track(theme);
                    seg = seg.child(ui::seg_item(theme, SharedString::from(format!("rs-k-{key}")), "Keep current", true).text_size(px(12.5)).h(px(26.)));
                    seg = seg.child(ui::seg_item(theme, SharedString::from(format!("rs-b-{key}")), "Use from backup", false).text_size(px(12.5)).h(px(26.)).opacity(0.45).cursor_default());
                    seg.into_any_element()
                }
                SettingKind::Choice { .. } | SettingKind::DatabaseType => {
                    let on = self.uses_backup(&key, r.kind);
                    let (k1, k2) = (key.clone(), key.clone());
                    let db = r.kind == SettingKind::DatabaseType;
                    let mut seg = ui::seg_track(theme);
                    seg = seg.child(ui::seg_item(theme, SharedString::from(format!("rs-k-{key}")), "Keep current", !on).text_size(px(12.5)).h(px(26.)).on_click(cx.listener(move |f, _, _, cx| {
                        if db {
                            f.db_type_backup = false;
                        } else {
                            f.use_backup.insert(k1.clone(), false);
                        }
                        cx.notify();
                    })));
                    seg = seg.child(ui::seg_item(theme, SharedString::from(format!("rs-b-{key}")), "Use from backup", on).text_size(px(12.5)).h(px(26.)).on_click(cx.listener(move |f, _, _, cx| {
                        if db {
                            f.db_type_backup = true;
                        } else {
                            f.use_backup.insert(k2.clone(), true);
                        }
                        cx.notify();
                    })));
                    seg.into_any_element()
                }
            };
            if ui::narrow() {
                // Narrow: one block per setting, the choice under its values.
                rows.push(vec![v_flex()
                    .gap_1()
                    .child(div().font_weight(FontWeight::MEDIUM).child(r.label.clone()))
                    .child(div().text_size(px(12.5)).child(format!("In the backup: {} · Here: {}", r.backup, r.here)))
                    .child(h_flex().child(choice))
                    .child(div().text_size(px(12.5)).text_color(theme.muted_foreground).child(r.why.clone()))
                    .into_any_element()]);
                continue;
            }
            rows.push(vec![txt(r.label.clone()), div().text_size(px(12.5)).child(r.backup.clone()).into_any_element(), div().text_size(px(12.5)).child(r.here.clone()).into_any_element(), choice, div().text_size(px(12.5)).text_color(theme.muted_foreground).child(r.why.clone()).into_any_element()]);
        }
        if !p.recorded && !ui::narrow() {
            rows.push(vec![txt("Ports, URL, TLS, environment, gateways"), muted(theme, "Not recorded").into_any_element(), txt("As now"), muted(theme, "Keep current").into_any_element(), muted(theme, "Nothing to compare with").into_any_element()]);
        }
        if rows.is_empty() {
            c = c.child(ui::outcome(theme, Tone::Good, "Every recorded setting is the same as here."));
        } else {
            c = c.child(if ui::narrow() { table(theme, &[1.0], &["Setting"], rows, 13.) } else { table(theme, &[1.0, 1.25, 1.25, 1.75, 1.35], &["Setting", "In the backup", &format!("On {here_cap}"), "Use", "Why"], rows, 13.) });
        }
        let mut out = v_flex().gap(px(ui::GAP));
        let mut ok = true;
        if self.db_type_backup {
            // B3: the backup's type on this instance.
            ok = false;
            let i = self.inspection.clone().unwrap_or_else(|| unreachable_inspection());
            out = out.child(blk(
                theme,
                false,
                format!("{} cannot run on {} as it is", capital(&here), i.database_type),
                format!("You chose “Use from backup” for the database type. {} runs {}; a restore keeps the instance's type and converts the history.", capital(&here), i.here.database_type),
                &[format!("Keep this {}'s type: the history is converted to {} (every direction works), or", if self.target.is_server() { "server" } else { "computer" }, i.here.database_type.trim_start_matches("PostgreSQL + ")), format!("change {here} to {} in Migration, then restore again.", i.database_type)],
                vec![
                    ui::button("rs-keep-type").primary().label(if self.target.is_server() { "Keep this server's type" } else { "Keep this computer's type" }).on_click(cx.listener(|f, _, _, cx| {
                        f.db_type_backup = false;
                        cx.notify();
                    })).into_any_element(),
                    ui::button("rs-open-migration").outline().icon(icon("arrow-left-right")).label("Open Migration…").on_click(cx.listener(|f, _, _, cx| f.leave(Some("migration"), cx))).into_any_element(),
                ],
            ));
        }
        out = out.child(c);
        if let Some(b) = licence {
            // B7: the licence stays here.
            let bound = b.split("bound to ").nth(1).unwrap_or("its machine").to_string();
            out = out.child(blk(
                theme,
                true,
                format!("The licence stays with {here}"),
                format!("The backup comes from {bound}, whose licence is bound to that machine's hardware. It is not carried over. {} keeps its own licence.", capital(&here)),
                &[format!("After the restore, check Licence on {here}."), format!("If {here} has none yet, activate one or request a demo licence.")],
                vec![Checkbox::new("rs-open-licence").label("Open Licence when the restore is done").checked(self.open_licence_after).on_change(cx.listener(|f, v: &bool, _, cx| {
                    f.open_licence_after = *v;
                    cx.notify();
                })).into_any_element()],
            ));
        }
        (out, ok)
    }

    fn confirm_view(&mut self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> (Div, bool) {
        let here = self.target.instance_name();
        let word = self.target.confirm_word();
        let typed = self.confirm.read(cx).value().trim().to_string();
        let ok = typed.eq_ignore_ascii_case(&word);
        let p = self.precheck.clone();
        let i = self.inspection.clone();
        let backup = i.as_ref().map(|i| i.name.clone()).unwrap_or_default();
        let live = p.as_ref().map(|p| p.live.clone()).unwrap_or_default();
        let into_live = self.into_live();
        let scope = self.scope(cx);
        let title = if into_live { format!("Add the backup's telemetry to {here}?") } else { format!("Replace the data of {here}?") };
        let replaced = match &scope {
            Scope::Everything => format!("Devices, dashboards, users and settings ({} devices, {} dashboards, {} users), and all telemetry, from {backup}", live.devices, live.dashboards, live.users),
            Scope::Configuration => format!("Devices, dashboards, users and settings ({} devices, {} dashboards, {} users), from {backup}; the telemetry {}", live.devices, live.dashboards, live.users, if self.keep { "stays" } else { "is cleared" }),
            Scope::TelemetryOnly => format!("Nothing: the telemetry from {backup} is added; rows already there stay"),
            Scope::Range { .. } => format!("The telemetry {}", scope.label().trim_start_matches("Telemetry ")),
        };
        let kept: Vec<String> = {
            let mut v = vec!["Licence".to_string()];
            if let Some(p) = &p {
                for r in &p.settings {
                    if !self.uses_backup(&r.key, r.kind) && matches!(r.kind, SettingKind::Choice { .. }) {
                        v.push(r.label.clone());
                    }
                }
            }
            v.truncate(6);
            v
        };
        let mut c = card(theme, title, None).child(kv(
            theme,
            vec![
                (txt(if into_live { "Changes" } else { "Replaced" }), txt(replaced)),
                (txt("Kept as now"), txt(if into_live { "Devices, dashboards, users, settings and the licence".to_string() } else { kept.join(", ") })),
                (txt("Downtime"), txt(if into_live { "None: SENTIENT keeps running".to_string() } else { format!("About {}, at the switch", p.as_ref().map(|p| mins(p.estimate.downtime_secs)).unwrap_or_else(|| "1 min".into())) })),
            ],
        ));
        let offered = self.delete_old || matches!(scope, Scope::Range { .. });
        if !into_live {
            c = c.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Checkbox::new("rs-delete-old").label("Delete the data from before once the restored data runs").checked(self.delete_old).on_change(cx.listener(|f, v: &bool, _, cx| {
                        f.delete_old = *v;
                        cx.notify();
                    })))
                    .child(ui::info("rs-delete-old-i".into(), "Without this, the data from before is kept under “Kept after earlier changes” until you remove it; it is the way back.".into())),
            );
        }
        if offered && !self.skip_safety {
            let s = p.as_ref().map(|p| format!("{} · {} · {}", p.safety.name, gb(p.safety.bytes), mins(p.safety.secs))).unwrap_or_default();
            c = c.child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .items_center()
                    .child(Checkbox::new("rs-safety").label("Back up the current data first").checked(self.safety).on_change(cx.listener(|f, v: &bool, _, cx| {
                        f.safety = *v;
                        cx.notify();
                    })))
                    .child(muted(theme, s))
                    .child(ui::info("rs-safety-i".into(), "Offered because data is deleted: a full backup file of what runs now, in the backup folder.".into())),
            );
        }
        c = c.child(
            h_flex()
                .flex_wrap()
                .gap_3()
                .items_center()
                .pt(px(4.))
                .child(h_flex().gap_1().text_sm().child("Type").child(div().font_weight(FontWeight::SEMIBOLD).child(word.clone())).child("to confirm"))
                .child(div().w(px(240.)).child(Input::new(&self.confirm))),
        );
        (c, ok)
    }
}

/// Only reached when there is no inspection, which the steps after Check
/// never are: a placeholder keeps the drawing total.
fn unreachable_inspection() -> Inspection {
    Inspection {
        file: PathBuf::new(),
        name: String::new(),
        file_bytes: 0,
        encrypted: false,
        manifest: serde_json::from_value(serde_json::json!({"format_version": 2, "tool_version": "", "created_at": "2026-01-01T00:00:00Z", "source": {"database": "", "postgres_version": "", "timescaledb_version": null}, "components": [], "telemetry": {"mode": "none"}, "files": [], "file_stores": [], "encryption": {"scheme": "none"}})).expect("a valid manifest"),
        parts: Vec::new(),
        missing: Vec::new(),
        sidecar: None,
        here: Default::default(),
        database_type: String::new(),
        made: String::new(),
        holds: Vec::new(),
        restored_bytes: 0,
        other_copy: None,
    }
}

impl RestoreFlow {
    /// B2: another disk for the safety backup (`safety`) or the temporary files.
    fn choose_dir(&mut self, safety: bool, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let current = if safety { self.safety_dir.clone() } else { self.temp_dir.clone() }.or_else(|| self.inspection.as_ref().map(|i| i.here.backup_folder.clone())).unwrap_or_else(|| PathBuf::from("/"));
        crate::manager::folder_chooser::open(
            crate::manager::folder_chooser::Opening {
                target: self.target.chooser_target(),
                on: self.target.name(),
                current,
                here: (0, 0),
                keep: 0,
                full_bytes: None,
                done: std::rc::Rc::new(move |dir, _, cx| {
                    this.update(cx, |f, cx| {
                        if safety {
                            f.safety_dir = Some(dir);
                        } else {
                            f.temp_dir = Some(dir);
                        }
                        f.start_precheck(cx);
                    })
                    .ok();
                }),
                title: Some(format!("{} on {}", if safety { "The safety backup's folder" } else { "The folder for temporary files" }, self.target.name())),
            },
            window,
            cx,
        );
    }

    /// B2s: restore without the safety backup, the name typed.
    fn ask_skip_safety(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let flow = cx.entity();
        let word = self.target.confirm_word();
        let newest = self.precheck.as_ref().and_then(|p| p.newest_backup.clone());
        let view = cx.new(|cx| SkipSafety::new(flow, word, newest, _window, cx));
        crate::modal::open(crate::modal::Modal { title: "Restore without a safety backup?".into(), body: view.clone().into(), width: 520., icon: Some(Icon::new(IconName::TriangleAlert)), on_close: None }, cx);
        let footer = cx.new(|cx| SkipFooter { _o: cx.observe(&view, |_, _, cx| cx.notify()), dialog: view });
        crate::modal::set_footer(footer.into(), cx);
    }
}

/// B2s's body: the warning and the name to type.
struct SkipSafety {
    flow: Entity<RestoreFlow>,
    word: String,
    newest: Option<String>,
    input: Entity<InputState>,
    _s: Subscription,
}

impl SkipSafety {
    fn new(flow: Entity<RestoreFlow>, word: String, newest: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx));
        let _s = cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify());
        SkipSafety { flow, word, newest, input, _s }
    }

    fn ok(&self, cx: &App) -> bool {
        self.input.read(cx).value().trim().eq_ignore_ascii_case(&self.word)
    }
}

impl Render for SkipSafety {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let here = self.flow.read(cx).target.instance_name();
        v_flex()
            .gap_3()
            .child(ui::outcome(&theme, Tone::Warn, format!("Without it, the data on {here} now can only come back from the data kept beside it, until that is removed, or from an older backup{}.", self.newest.as_ref().map(|n| format!(" (newest: {n})")).unwrap_or_default())))
            .child(h_flex().flex_wrap().gap_3().items_center().child(h_flex().gap_1().text_sm().child("Type").child(div().font_weight(FontWeight::SEMIBOLD).child(self.word.clone())).child("to restore without it")).child(div().w(px(200.)).child(Input::new(&self.input))))
    }
}

struct SkipFooter {
    dialog: Entity<SkipSafety>,
    _o: Subscription,
}

impl Render for SkipFooter {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ok = self.dialog.read(cx).ok(cx);
        let flow = self.dialog.read(cx).flow.clone();
        h_flex()
            .gap_2()
            .justify_end()
            .child(ui::button("rs-skip-back").outline().label("Back").on_click(|_, _, cx| crate::modal::close(cx)))
            .child(ui::button("rs-skip-go").danger().label("Restore without it").disabled(!ok).on_click(move |_, _, cx| {
                crate::modal::close(cx);
                flow.update(cx, |f, cx| {
                    f.skip_safety = true;
                    f.safety = false;
                    f.start_precheck(cx);
                });
            }))
    }
}

// ---------------------------------------------------------------------------
// The run (R16, R17, X1–X3)
// ---------------------------------------------------------------------------

impl RestoreFlow {
    fn remove_restored(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.target.clone();
        let here = target.instance_name();
        crate::modal::alert(window, cx, move |d, _, _| {
            let (target, name) = (target.clone(), name.clone());
            d.title(format!("Remove {name}?")).description(format!("The restored database that did not start is deleted from {here}. The data SENTIENT runs on is not touched.")).confirm().ok_text("Remove").on_ok(move |_, _, cx| {
                let (target, name) = (target.clone(), name.clone());
                cx.spawn(async move |cx| {
                    let r = cx
                        .background_executor()
                        .spawn(async move {
                            let args = vec!["restore-remove-db".to_string(), name.clone()];
                            let quiet = sentient_installer_core::progress::noop();
                            match &target {
                                Target::Server(inst) => remote::run_followed(inst, &args, &quiet),
                                Target::Here(record) if crate::rootjob::needed(record) => crate::rootjob::run(&args, &quiet).map(|_| ()),
                                Target::Here(record) => rf::remove_restored(record, &name, &quiet),
                            }
                        })
                        .await;
                    cx.update(|cx| match r {
                        Ok(()) => ui::show_info("Removed", "The restored database was removed.", cx),
                        Err(e) => ui::show_error("Not removed", e, cx),
                    });
                })
                .detach();
                true
            })
        });
    }

    fn run_view(&mut self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> Div {
        let here = self.target.instance_name();
        let job = self.run.read(cx);
        let state = job.state().clone();
        let finished = match &state {
            JobState::Done(_) => Some(true),
            JobState::Failed(_) => Some(false),
            _ => None,
        };
        let steps: Vec<SharedString> = job.steps().to_vec();
        let percent = job.percent();
        let lines: Vec<String> = job.log().iter().map(|l| l.to_string()).filter(|l| !rf::is_data(l)).collect();
        let last = lines.iter().rev().find(|l| !l.trim().is_empty() && !l.starts_with(">>")).cloned();
        let outcome = self.outcome.clone();
        let result = outcome.as_ref().map(|o| o.result).or(match &state {
            JobState::Done(_) => Some(Result_::Done),
            JobState::Failed(_) => Some(Result_::Failed),
            _ => None,
        });
        let message = outcome.as_ref().map(|o| o.message.clone()).or(match &state {
            JobState::Done(m) | JobState::Failed(m) => Some(m.to_string()),
            _ => None,
        });
        let into_live = self.into_live();
        let mut marked = ui::mark_steps(&self.planned, &steps, finished);
        let rolled = result == Some(Result_::RolledBack);
        if rolled {
            for m in marked.iter_mut() {
                if m.0 == rf::STEP_SWITCH {
                    *m = ("SENTIENT did not answer on the restored data within 3 minutes".into(), StepMark::Failed);
                }
            }
            marked.push(("Putting the previous data back".into(), StepMark::Done));
            marked.push(("SENTIENT answers on its previous data".into(), StepMark::Done));
        }
        let safety_size = lines.iter().find(|l| l.contains("sentient-before-restore") && l.contains(", checked")).and_then(|l| l.rsplit(" · ").next()).map(str::to_string);
        let title = match result {
            Some(Result_::Done) if into_live => format!("The backup's telemetry is in {here}"),
            Some(Result_::Done) => if self.target.is_server() { "This instance runs on the restored data".to_string() } else { "This computer runs on the restored data".into() },
            Some(Result_::RolledBack) => "The data was not replaced".into(),
            _ if into_live => format!("Adding telemetry to {here}"),
            _ => format!("Replacing the data of {here}"),
        };
        let (pill, tone) = match result {
            None => ("Running", Tone::Neutral),
            Some(Result_::Done) => ("Done", Tone::Good),
            Some(Result_::Failed) => ("Failed", Tone::Bad),
            Some(Result_::RolledBack) => ("Rolled back", Tone::Warn),
            Some(Result_::Stopped) => ("Stopped", Tone::Bad),
        };
        let mut c = card(theme, title, Some(ui::pill(theme, tone, pill).into_any_element()));
        let mut list = v_flex().gap_0();
        for (name, mark) in &marked {
            let sub = match mark {
                StepMark::Running => {
                    let p = percent.map(|p| format!("{p:.0}%"));
                    let l = last.clone().map(|l| ui::middle_ellipsis(l.trim(), 48));
                    match (p, l) {
                        (Some(p), Some(l)) => Some(format!("{p} · {l}")),
                        (p, l) => p.or(l),
                    }
                }
                StepMark::Done if name == rf::STEP_SAFETY => safety_size.clone(),
                _ => None,
            };
            let name = if name == rf::STEP_SCHEMA { self.precheck.as_ref().and_then(|p| p.upgrade.clone()).map(|u| format!("Bringing the data up to schema {}", u.split(" → ").nth(1).and_then(|s| s.split_whitespace().next()).unwrap_or(""))).unwrap_or_else(|| name.clone()) } else { name.clone() };
            list = list.child(check_line(theme, *mark, name, sub));
        }
        c = c.child(list);
        let back_btn = |id: &'static str, label: &'static str, cx: &mut Context<Self>| ui::button(id).outline().label(label).on_click(cx.listener(|f, _, _, cx| f.leave(None, cx))).into_any_element();
        match result {
            None => {
                c = c.child(
                    h_flex()
                        .flex_wrap()
                        .gap_3()
                        .items_center()
                        .child(ui::button("rs-cancel-run").outline().label("Cancel").on_click(cx.listener(|f, _, _, cx| {
                            let t = f.target.clone();
                            cx.background_executor().spawn(async move { t.cancel() }).detach();
                        })))
                        .child(muted(theme, if into_live { format!("Stops and takes out what was added; nothing on {here} changes.") } else { format!("Stops before the switch; nothing on {here} changes.") })),
                );
            }
            Some(Result_::Done) => {
                c = c.child(ui::outcome(theme, Tone::Good, message.clone().unwrap_or_default()));
                if !into_live {
                    let port = self.inspection.as_ref().map(|i| i.here.http_port).unwrap_or(8080);
                    let url = match &self.target {
                        Target::Server(i) => format!("http://{}:{port}", i.host),
                        Target::Here(_) => format!("http://localhost:{port}"),
                    };
                    let mut next: Vec<AnyElement> = vec![
                        ui::button("rs-open").primary().label("Open SENTIENT").on_click(move |_, _, cx| ui::open_url(&url, cx)).into_any_element(),
                        ui::button("rs-reset").outline().label("Reset password…").on_click(cx.listener(|f, _, _, cx| f.leave(Some("settings"), cx))).into_any_element(),
                    ];
                    if outcome.as_ref().is_some_and(|o| o.licence_note) || self.precheck.as_ref().is_some_and(|p| p.settings.iter().any(|r| r.kind == SettingKind::Locked)) {
                        next.push(ui::button("rs-licence").outline().icon(icon("key-round")).label("Activate the licence…").on_click(cx.listener(|f, _, _, cx| f.leave(Some("licence"), cx))).into_any_element());
                    }
                    if self.precheck.as_ref().is_some_and(|p| p.settings.iter().any(|r| r.kind == SettingKind::Gateway)) {
                        next.push(ui::button("rs-gateways").outline().label("Point gateways here…").on_click(cx.listener(|f, _, _, cx| f.leave(Some("gateway"), cx))).into_any_element());
                    }
                    c = c.child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Next")).child(ui::button_row(next));
                }
                let licence_after = self.open_licence_after && outcome.as_ref().is_some_and(|o| o.licence_note);
                c = c.child(
                    h_flex()
                        .flex_wrap()
                        .gap_2()
                        .pt(px(10.))
                        .border_t_1()
                        .border_color(theme.border)
                        .child(back_btn("rs-back-page", "Back to Backup and restore", cx))
                        .child(ui::button("rs-show-log").outline().label(if self.show_log { "Hide log" } else { "Show log" }).on_click(cx.listener(|f, _, _, cx| {
                            f.show_log = !f.show_log;
                            cx.notify();
                        })))
                        .child(div().flex_1())
                        .child(ui::button("rs-done").primary().label("Done").on_click(cx.listener(move |f, _, _, cx| f.leave(if licence_after { Some("licence") } else { None }, cx)))),
                );
            }
            Some(Result_::Failed) => {
                c = c.child(ui::outcome(theme, Tone::Bad, message.clone().unwrap_or_default()));
                let resumable = outcome.as_ref().and_then(|o| o.failed_step.clone()).is_some_and(|s| s != rf::STEP_CHECK && s != rf::STEP_SAFETY);
                let mut acts: Vec<AnyElement> = Vec::new();
                if resumable {
                    acts.push(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(ui::button("rs-resume").primary().label("Resume").on_click(cx.listener(|f, _, _, cx| f.start_run(true, cx))))
                            .child(ui::info("rs-resume-i".into(), "Goes on after the last finished step: the safety backup and the check are not repeated.".into()))
                            .into_any_element(),
                    );
                }
                acts.push(ui::button("rs-again").outline().label("Start again").on_click(cx.listener(|f, _, _, cx| f.start_run(false, cx))).into_any_element());
                acts.push(back_btn("rs-back-page", "Back to Backup and restore", cx));
                c = c.child(ui::button_row(acts));
            }
            Some(Result_::Stopped) => {
                c = c.child(ui::outcome(theme, Tone::Bad, message.clone().unwrap_or_default()));
                c = c.child(ui::button_row(vec![
                    ui::button("rs-free-files").outline().icon(IconName::Folder).label("Free space in Files").on_click(cx.listener(|f, _, _, cx| f.leave(Some("files"), cx))).into_any_element(),
                    ui::button("rs-move-data").outline().icon(icon("move-disk")).label("Move data to another disk…").on_click(cx.listener(|f, _, window, cx| {
                        // TODO(worker A): wire the M lane's move-data flow here.
                        crate::manager::movedata::open(f.target.chooser_target(), window, cx);
                    })).into_any_element(),
                    ui::button("rs-again").outline().label("Start again").on_click(cx.listener(|f, _, _, cx| f.start_run(false, cx))).into_any_element(),
                    back_btn("rs-back-page", "Back", cx),
                ]));
            }
            Some(Result_::RolledBack) => {
                c = c.child(ui::outcome(theme, Tone::Bad, message.clone().unwrap_or_default()));
                let restored = outcome.as_ref().and_then(|o| o.restored_db.clone());
                let mut acts: Vec<AnyElement> = vec![ui::button("rs-sentient-log").primary().label("Show SENTIENT's log").on_click(cx.listener(|f, _, _, cx| f.leave(Some("logs"), cx))).into_any_element()];
                if let Some(name) = restored {
                    acts.push(ui::button("rs-remove-restored").outline().label("Remove the restored database…").on_click(cx.listener(move |f, _, window, cx| f.remove_restored(name.clone(), window, cx))).into_any_element());
                }
                acts.push(back_btn("rs-back-page", "Back to Backup and restore", cx));
                c = c.child(ui::button_row(acts));
            }
        }
        let show_log = result.is_none() || result != Some(Result_::Done) || self.show_log;
        let mut out = v_flex().gap(px(ui::GAP)).child(c);
        if show_log {
            let tail: Vec<String> = lines.iter().rev().take(400).rev().cloned().collect();
            out = out.child(
                card(theme, "Log", None).child(
                    div()
                        .id("rs-log")
                        .max_h(px(300.))
                        .overflow_y_scroll()
                        .p(px(10.))
                        .rounded(px(8.))
                        .bg(theme.muted)
                        .font_family(theme.mono_font_family.clone())
                        .text_size(px(12.5))
                        .children(if tail.is_empty() { vec![div().text_color(theme.muted_foreground).child("Waiting for the first lines…")] } else { tail.into_iter().map(|l| div().child(l)).collect() }),
                ),
            );
        }
        out
    }
}

impl Render for RestoreFlow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        if self.step == Step::Run {
            let body = self.run_view(&theme, cx);
            let summary: &'static str = if self.target.is_server() { "The backups on this server." } else { "Backups of this computer, and restores into it." };
            return ui::page(&theme, "Backup and restore", summary).child(body);
        }
        let (body, can_next) = match self.step {
            Step::Check => self.check_view(&theme, cx),
            Step::What => self.what_view(&theme, window, cx),
            Step::Where => self.where_view(&theme, cx),
            Step::Precheck => self.precheck_view(&theme, cx),
            Step::Settings => self.settings_view(&theme, cx),
            Step::Confirm => self.confirm_view(&theme, cx),
            Step::Run => unreachable!(),
        };
        let busy = self.check.read(cx).is_running() || self.pre.read(cx).is_running();
        let next: AnyElement = if self.step == Step::Confirm {
            let label = if self.into_live() { "Add the telemetry" } else { "Replace the data" };
            let b = ui::button("rs-next").label(label).disabled(!can_next || busy).on_click(cx.listener(|f, _, _, cx| f.next(cx)));
            if self.into_live() { b.primary() } else { b.danger() }.into_any_element()
        } else {
            ui::button("rs-next").primary().label("Next").disabled(!can_next || busy).on_click(cx.listener(|f, _, _, cx| f.next(cx))).into_any_element()
        };
        let back = ui::button("rs-back").outline().label("Back").on_click(cx.listener(|f, _, _, cx| f.back(cx))).into_any_element();
        let cancel = ui::button("rs-cancel").ghost().label("Cancel").on_click(cx.listener(|f, _, _, cx| f.leave(None, cx))).into_any_element();
        v_flex()
            .id("page")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .p(px(ui::PAGE_PAD))
            .gap(px(ui::GAP))
            .bg(theme.muted)
            .child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .child(ui::button("rs-title-back").outline().icon(IconName::ChevronLeft).on_click(cx.listener(|f, _, _, cx| f.back(cx))).mt(px(2.)))
                    .child(v_flex().flex_1().min_w_0().gap_0p5().child(div().text_size(px(22.)).font_weight(FontWeight::MEDIUM).child("Restore")).child(div().text_sm().text_color(theme.muted_foreground).child(self.sub()))),
            )
            .child(stepper(&theme, self.step.index()))
            .child(body)
            .child(footer(Some(back), cancel, Some(next)))
    }
}

// ---------------------------------------------------------------------------
// Restore from… (R4–R6, B8, B9)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Read<T> {
    Idle,
    Loading,
    Got(T),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq)]
enum Transfer {
    Idle,
    Running { sent: u64, total: u64, started: std::time::Instant },
    Broken { sent: u64, total: u64, why: String },
    Done(PathBuf),
}

pub type OnPick = std::rc::Rc<dyn Fn(Source, &mut App)>;

pub struct RestoreFrom {
    target: Target,
    on_pick: OnPick,
    /// 0: the instance's own backups; 1: a file on this computer (a server);
    /// 2: another instance.
    tab: usize,
    folder: PathBuf,
    list: Read<Vec<crate::backups::Entry>>,
    selected: Option<usize>,
    local: Option<(PathBuf, u64)>,
    upload_to: String,
    upload_free: Option<u64>,
    writable: Option<bool>,
    upload: Transfer,
    instances: Vec<Instance>,
    src: Option<Instance>,
    src_list: Read<Vec<crate::backups::Entry>>,
    src_sel: Option<usize>,
    way: remote::CopyWay,
    reaches: Option<bool>,
    copy: Transfer,
    progress: std::sync::Arc<std::sync::Mutex<(u64, u64)>>,
    _tasks: Vec<Task<()>>,
}

impl RestoreFrom {
    pub fn open(target: Target, folder: PathBuf, free: Option<u64>, on_pick: OnPick, cx: &mut App) {
        let view = cx.new(|cx| RestoreFrom::new(target, folder, free, on_pick, cx));
        crate::modal::open(crate::modal::Modal { title: "Restore from…".into(), body: view.clone().into(), width: 720., icon: Some(icon("history")), on_close: None }, cx);
        let footer = cx.new(|cx| FromFooter { _o: cx.observe(&view, |_, _, cx| cx.notify()), dialog: view });
        crate::modal::set_footer(footer.into(), cx);
    }

    fn new(target: Target, folder: PathBuf, free: Option<u64>, on_pick: OnPick, cx: &mut Context<Self>) -> Self {
        let instances: Vec<Instance> = remote::load().into_iter().filter(|i| !matches!(&target, Target::Server(t) if t.id == i.id)).collect();
        let mut d = RestoreFrom {
            upload_to: folder.display().to_string(),
            target,
            on_pick,
            tab: 0,
            folder,
            list: Read::Idle,
            selected: None,
            local: None,
            upload_free: free,
            writable: None,
            upload: Transfer::Idle,
            src: None,
            instances,
            src_list: Read::Idle,
            src_sel: None,
            way: remote::CopyWay::Direct,
            reaches: None,
            copy: Transfer::Idle,
            progress: Default::default(),
            _tasks: Vec::new(),
        };
        #[cfg(debug_assertions)]
        if let Ok(frame) = std::env::var("SPM_DEBUG_RESTORE_FROM") {
            canned::from_dialog(&mut d, &frame);
            return d;
        }
        d.read_list(cx);
        d
    }

    fn read_list(&mut self, cx: &mut Context<Self>) {
        self.list = Read::Loading;
        let (target, folder) = (self.target.clone(), self.folder.clone());
        let t = cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move {
                    match &target {
                        Target::Here(_) => Ok(crate::backups::list(&folder)),
                        Target::Server(inst) => {
                            let b = remote::backups(inst)?;
                            if b.folder == folder { Ok(b.list) } else { remote::backups_in(inst, &folder.display().to_string()) }
                        }
                    }
                })
                .await;
            this.update(cx, |d, cx| {
                d.list = match r {
                    Ok(l) => Read::Got(l),
                    Err(e) => Read::Failed(remote::split_failure(&e).0),
                };
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(t);
    }

    fn set_tab(&mut self, tab: usize, cx: &mut Context<Self>) {
        self.tab = tab;
        if tab == 2 && self.src.is_none() {
            if let Some(i) = self.instances.first().cloned() {
                self.pick_src(i, cx);
            }
        }
        cx.notify();
    }

    fn pick_src(&mut self, inst: Instance, cx: &mut Context<Self>) {
        self.src = Some(inst.clone());
        self.src_list = Read::Loading;
        self.src_sel = None;
        self.reaches = None;
        let target = self.target.clone();
        let t = cx.spawn(async move |this, cx| {
            let i2 = inst.clone();
            let (list, reach) = cx
                .background_executor()
                .spawn(async move {
                    let list = remote::backups(&i2).map(|b| b.list);
                    let reach = match &target {
                        Target::Server(dst) if list.is_ok() => Some(remote::reaches(&i2, dst)),
                        _ => None,
                    };
                    (list, reach)
                })
                .await;
            this.update(cx, |d, cx| {
                d.src_list = match list {
                    Ok(l) => Read::Got(l),
                    Err(e) => Read::Failed(remote::split_failure(&e).0),
                };
                d.reaches = reach;
                if reach == Some(false) {
                    d.way = remote::CopyWay::ThroughHere;
                }
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(t);
        cx.notify();
    }

    fn choose_local(&mut self, cx: &mut Context<Self>) {
        let rx = crate::chooser::open(
            cx,
            crate::chooser::Ask { title: "Choose the backup to restore", start: None, directory: false },
            PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Restore this backup".into()) },
        );
        let t = cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(p) = paths.into_iter().next() else { return };
            this.update(cx, |d, cx| {
                let bytes = sentient_backup_core::parts::total_size(&p);
                d.local = Some((p.clone(), bytes));
                match d.target.clone() {
                    // This computer: the file is used where it is.
                    Target::Here(_) => {
                        let name = p.file_name().map(|n| n.to_string_lossy().trim_end_matches(".sentient-backup").to_string()).unwrap_or_default();
                        (d.on_pick)(Source { file: p.clone(), sub: name, note: None }, cx);
                        crate::modal::close(cx);
                    }
                    Target::Server(_) => d.start_upload(cx),
                }
            })
            .ok();
        });
        self._tasks.push(t);
    }

    fn poll_progress(&mut self, copy: bool, cx: &mut Context<Self>) {
        let p = self.progress.clone();
        let t = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
            let (sent, total) = *p.lock().unwrap();
            let go = this
                .update(cx, |d, cx| {
                    let tr = if copy { &mut d.copy } else { &mut d.upload };
                    if let Transfer::Running { sent: s, total: t, .. } = tr {
                        *s = sent;
                        if total > 0 {
                            *t = total;
                        }
                        cx.notify();
                        true
                    } else {
                        false
                    }
                })
                .unwrap_or(false);
            if !go {
                break;
            }
        });
        self._tasks.push(t);
    }

    fn start_upload(&mut self, cx: &mut Context<Self>) {
        let (Target::Server(inst), Some((local, bytes))) = (self.target.clone(), self.local.clone()) else { return };
        let folder = self.upload_to.clone();
        self.writable = None;
        self.upload = Transfer::Running { sent: 0, total: bytes, started: std::time::Instant::now() };
        *self.progress.lock().unwrap() = (0, bytes);
        let p = self.progress.clone();
        let t = cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move {
                    if !remote::can_write(&inst, &folder)? {
                        return Err("NOT-WRITABLE".to_string());
                    }
                    remote::upload_backup(&inst, &local, &folder, &move |s, t| *p.lock().unwrap() = (s, t))
                })
                .await;
            this.update(cx, |d, cx| {
                d.upload = match r {
                    Ok(path) => Transfer::Done(PathBuf::from(path)),
                    Err(e) if e == "NOT-WRITABLE" => {
                        d.writable = Some(false);
                        Transfer::Idle
                    }
                    Err(e) => match e.split_once('\n').filter(|(h, _)| *h == remote::UPLOAD_BROKE).and_then(|(_, j)| serde_json::from_str::<serde_json::Value>(j).ok()) {
                        Some(v) => Transfer::Broken { sent: v["sent"].as_u64().unwrap_or(0), total: v["total"].as_u64().unwrap_or(0), why: v["why"].as_str().unwrap_or("").to_string() },
                        None => Transfer::Broken { sent: 0, total: bytes, why: e },
                    },
                };
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(t);
        self.poll_progress(false, cx);
        cx.notify();
    }

    fn make_writable(&mut self, cx: &mut Context<Self>) {
        let Target::Server(inst) = self.target.clone() else { return };
        let folder = self.upload_to.clone();
        let t = cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { remote::dir_make_writable(&inst, &folder) }).await;
            this.update(cx, |d, cx| match r {
                Ok(_) => d.start_upload(cx),
                Err(e) => ui::show_error("Not made writable", remote::split_failure(&e).0, cx),
            })
            .ok();
        });
        self._tasks.push(t);
    }

    fn change_folder(&mut self, for_upload: bool, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity();
        let (target, on_pick) = (self.target.clone(), self.on_pick.clone());
        let current = if for_upload { PathBuf::from(&self.upload_to) } else { self.folder.clone() };
        let free = self.upload_free;
        crate::manager::folder_chooser::open(
            crate::manager::folder_chooser::Opening {
                target: target.chooser_target(),
                on: target.name(),
                current: current.clone(),
                here: (0, 0),
                keep: 0,
                full_bytes: None,
                done: std::rc::Rc::new(move |dir, _, cx| {
                    // The chooser took the dialog's place: open it again on the folder.
                    let (t, p) = (target.clone(), on_pick.clone());
                    let old = this.clone();
                    let view = cx.new(|cx| {
                        let mut d = RestoreFrom::new(t, if for_upload { old.read(cx).folder.clone() } else { dir.clone() }, free, p, cx);
                        if for_upload {
                            d.upload_to = dir.display().to_string();
                            d.tab = 1;
                            d.local = old.read(cx).local.clone();
                        }
                        d
                    });
                    crate::modal::open(crate::modal::Modal { title: "Restore from…".into(), body: view.clone().into(), width: 720., icon: Some(icon("history")), on_close: None }, cx);
                    let footer = cx.new(|cx| FromFooter { _o: cx.observe(&view, |_, _, cx| cx.notify()), dialog: view });
                    crate::modal::set_footer(footer.into(), cx);
                }),
                title: Some(format!("{} on {}", if for_upload { "Upload to a folder" } else { "Backups in another folder" }, self.target.name())),
            },
            window,
            cx,
        );
    }

    fn start_copy(&mut self, cx: &mut Context<Self>) {
        let (Some(src), Read::Got(list), Some(sel)) = (self.src.clone(), self.src_list.clone(), self.src_sel) else { return };
        let Some(e) = list.get(sel).cloned() else { return };
        let target = self.target.clone();
        let (folder, way) = (self.folder.clone(), self.way);
        self.copy = Transfer::Running { sent: 0, total: e.bytes, started: std::time::Instant::now() };
        *self.progress.lock().unwrap() = (0, e.bytes);
        let p = self.progress.clone();
        let path = e.path.display().to_string();
        let t = cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move {
                    let prog = move |s: u64, t: u64| *p.lock().unwrap() = (s, t);
                    match &target {
                        Target::Server(dst) => remote::copy_between(&src, dst, &path, &folder.display().to_string(), way, &prog).map(PathBuf::from),
                        Target::Here(_) => remote::download_backup(&src, &path, &folder, &prog),
                    }
                })
                .await;
            this.update(cx, |d, cx| {
                d.copy = match r {
                    Ok(p) => {
                        let src = d.src.as_ref().map(|s| s.name.clone()).unwrap_or_default();
                        let how = match (&d.target, d.way) {
                            (Target::Here(_), _) => "copied to this computer",
                            (_, remote::CopyWay::Direct) => "server to server",
                            _ => "through this computer",
                        };
                        let name = p.file_name().map(|n| n.to_string_lossy().trim_end_matches(".sentient-backup").to_string()).unwrap_or_default();
                        (d.on_pick)(Source { file: p.clone(), sub: format!("{name} from {src}"), note: Some(format!("copied from {src}, {how}")) }, cx);
                        crate::modal::close(cx);
                        Transfer::Done(p)
                    }
                    Err(e) => Transfer::Broken { sent: 0, total: 0, why: remote::split_failure(&e).0 },
                };
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(t);
        self.poll_progress(true, cx);
        cx.notify();
    }

    fn pick(&mut self, cx: &mut Context<Self>) {
        match self.tab {
            0 => {
                let Read::Got(list) = &self.list else { return };
                let Some(e) = self.selected.and_then(|i| list.get(i)) else { return };
                (self.on_pick)(Source { file: e.path.clone(), sub: e.name.clone(), note: None }, cx);
                crate::modal::close(cx);
            }
            1 => {
                if let Transfer::Done(p) = &self.upload {
                    let name = p.file_name().map(|n| n.to_string_lossy().trim_end_matches(".sentient-backup").to_string()).unwrap_or_default();
                    (self.on_pick)(Source { file: p.clone(), sub: format!("{name} from this computer"), note: Some("uploaded from this computer".into()) }, cx);
                    crate::modal::close(cx);
                }
            }
            _ => self.start_copy(cx),
        }
    }

    fn list_table(&self, theme: &gpui_kit::component::Theme, list: &[crate::backups::Entry], selected: Option<usize>, src: bool, cx: &mut Context<Self>) -> Div {
        let mut t = v_flex().px(px(12.)).py(px(4.)).rounded(px(10.)).border_1().border_color(theme.border).text_sm();
        t = t.child(h_flex().h(px(30.)).text_size(px(12.5)).text_color(theme.muted_foreground).border_b_1().border_color(theme.border).child(div().flex_1().min_w_0().px(px(4.)).child("File")).child(div().w(px(120.)).flex_shrink_0().child("Made")).child(div().w(px(70.)).flex_shrink_0().child("Size")).child(div().w(px(100.)).flex_shrink_0().child("Integrity")));
        for (i, e) in list.iter().enumerate() {
            let damaged = e.check.as_ref().is_some_and(|c| !c.ok);
            let (tone, text) = match &e.check {
                Some(c) if c.ok => (Tone::Good, "Verified"),
                Some(_) => (Tone::Bad, "Damaged"),
                None => (Tone::Neutral, "Not verified"),
            };
            let sel = selected == Some(i);
            t = t.child(
                h_flex()
                    .id(SharedString::from(format!("rf-row-{src}-{i}")))
                    .min_h(px(40.))
                    .items_center()
                    .rounded(px(6.))
                    .when(i > 0, |r| r.border_t_1().border_color(theme.border))
                    .when(sel, |r| r.bg(theme.sidebar_accent))
                    .when(damaged, |r| r.opacity(0.5))
                    .when(!damaged, |r| {
                        r.cursor_pointer().hover(|r| r.bg(theme.list_hover)).on_click(cx.listener(move |d, _, _, cx| {
                            if src {
                                d.src_sel = Some(i);
                            } else {
                                d.selected = Some(i);
                            }
                            cx.notify();
                        }))
                    })
                    .child(div().flex_1().min_w_0().px(px(4.)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(e.name.clone()))
                    .child(div().w(px(120.)).flex_shrink_0().font_features(ui::tabular()).child(crate::backups::short_made_label(&e.made)))
                    .child(div().w(px(70.)).flex_shrink_0().font_features(ui::tabular()).child(gb(e.bytes)))
                    .child(div().w(px(100.)).flex_shrink_0().child(ui::pill(theme, tone, text).px(px(7.)).py(px(1.)).text_size(px(12.)))),
            );
        }
        t
    }

    fn skeleton(&self, theme: &gpui_kit::component::Theme) -> Div {
        let mut t = v_flex().px(px(12.)).py(px(4.)).rounded(px(10.)).border_1().border_color(theme.border);
        t = t.child(h_flex().h(px(30.)).text_size(px(12.5)).text_color(theme.muted_foreground).border_b_1().border_color(theme.border).child(div().flex_1().child("File")).child(div().w(px(120.)).child("Made")).child(div().w(px(70.)).child("Size")).child(div().w(px(100.)).child("Integrity")));
        for (n, w) in [210., 180., 196.].iter().enumerate() {
            t = t.child(
                h_flex()
                    .min_h(px(40.))
                    .items_center()
                    .when(n > 0, |r| r.border_t_1().border_color(theme.border))
                    .child(div().flex_1().child(ui::skeleton_line(theme, SharedString::from(format!("rf-sk-a{n}")), *w)))
                    .child(div().w(px(120.)).child(ui::skeleton_line(theme, SharedString::from(format!("rf-sk-b{n}")), 90.)))
                    .child(div().w(px(70.)).child(ui::skeleton_line(theme, SharedString::from(format!("rf-sk-c{n}")), 50.)))
                    .child(div().w(px(100.)).child(ui::skeleton_line(theme, SharedString::from(format!("rf-sk-d{n}")), 70.))),
            );
        }
        t
    }

    fn transfer_card(&self, theme: &gpui_kit::component::Theme, tr: &Transfer, what: &str) -> Option<Div> {
        let box_ = || v_flex().gap(px(6.)).p(px(12.)).rounded(px(10.)).border_1().border_color(theme.border);
        match tr {
            Transfer::Running { sent, total, started } => {
                let pct = if *total > 0 { *sent as f32 * 100. / *total as f32 } else { 0. };
                let secs = started.elapsed().as_secs_f64().max(1.);
                let rate = *sent as f64 / secs;
                let left = if rate > 0. { ((*total - (*sent).min(*total)) as f64 / rate) as u64 } else { 0 };
                Some(
                    box_()
                        .child(h_flex().gap_2().items_center().child(icon("upload").size(px(16.)).text_color(theme.primary)).child(div().font_weight(FontWeight::MEDIUM).child(what.to_string())).child(div().flex_1()).child(muted(theme, format!("{} of {} · {}/s · about {} left", gb(*sent), gb(*total), gb(rate as u64), mins(left))).font_features(ui::tabular())))
                        .child(h_flex().gap_3().items_center().child(progress_bar(theme, "rf-transfer", pct, false)).child(div().text_sm().child(format!("{pct:.0}%")))),
                )
            }
            Transfer::Broken { sent, total, why } => {
                let pct = if *total > 0 { *sent as f32 * 100. / *total as f32 } else { 0. };
                let name = self.target.name();
                Some(
                    box_()
                        .when(*total > 0, |b| b.child(div().font_weight(FontWeight::MEDIUM).child(format!("{what} stopped at {} of {}", gb(*sent), gb(*total)))).child(h_flex().gap_3().items_center().child(progress_bar(theme, "rf-transfer", pct, true)).child(div().text_sm().child(format!("{pct:.0}%")))))
                        .child(ui::outcome(theme, Tone::Bad, format!("{} {}", if why.to_lowercase().contains("connection") || why.contains("timed out") { format!("The connection to {name} dropped.") } else { why.clone() }, if *sent > 0 { format!("The {} already sent is kept.", gb(*sent)) } else { String::new() }))),
                )
            }
            _ => None,
        }
    }
}

impl Render for RestoreFrom {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let server = self.target.is_server();
        let mut seg = ui::seg_track(&theme);
        if server {
            seg = seg.child(ui::seg_item_with(&theme, "rf-tab-0", icon("server"), "This server", self.tab == 0).on_click(cx.listener(|d, _, _, cx| d.set_tab(0, cx))));
            seg = seg.child(ui::seg_item_with(&theme, "rf-tab-1", icon("laptop"), "This computer", self.tab == 1).on_click(cx.listener(|d, _, _, cx| d.set_tab(1, cx))));
        } else {
            seg = seg.child(ui::seg_item_with(&theme, "rf-tab-0", icon("laptop"), "This computer", self.tab == 0).on_click(cx.listener(|d, _, _, cx| d.set_tab(0, cx))));
        }
        seg = seg.child(ui::seg_item_with(&theme, "rf-tab-2", icon("server"), "Another instance", self.tab == 2).on_click(cx.listener(|d, _, _, cx| d.set_tab(2, cx))));
        let mut body = v_flex().gap_3().child(h_flex().child(seg));
        match self.tab {
            0 => {
                body = body.child(match self.list.clone() {
                    Read::Idle | Read::Loading => self.skeleton(&theme),
                    Read::Failed(e) => ui::outcome(&theme, Tone::Bad, e),
                    Read::Got(l) if l.is_empty() => muted(&theme, format!("No backups in {}.", self.folder.display())),
                    Read::Got(l) => self.list_table(&theme, &l, self.selected, false, cx),
                });
                let mut row = h_flex().flex_wrap().gap_2().child(ui::button("rf-folder").ghost().icon(IconName::Folder).label(if server { "Another folder on the server…" } else { "Another folder…" }).on_click(cx.listener(|d, _, window, cx| d.change_folder(false, window, cx))));
                if !server {
                    row = row.child(ui::button("rf-file").ghost().icon(IconName::File).label("A file elsewhere…").on_click(cx.listener(|d, _, _, cx| d.choose_local(cx))));
                }
                body = body.child(row);
            }
            1 => {
                let (file, size) = self.local.clone().map(|(p, b)| (p.display().to_string(), gb(b))).unwrap_or_else(|| ("No file chosen".into(), String::new()));
                body = body
                    .child(crate::manager::backup::card_row(
                        &theme,
                        false,
                        "File",
                        None,
                        h_flex().min_w_0().gap_2().items_baseline().child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().font_family(theme.mono_font_family.clone()).text_size(px(13.5)).child(file)).child(muted(&theme, size)),
                        Some(ui::button("rf-choose").outline().icon(IconName::Folder).label("Choose…").disabled(matches!(self.upload, Transfer::Running { .. })).on_click(cx.listener(|d, _, _, cx| d.choose_local(cx))).into_any_element()),
                        false,
                    ))
                    .child(crate::manager::backup::card_row(
                        &theme,
                        false,
                        "Upload to",
                        Some("The server keeps the file in its backup folder, so it shows in Backups afterwards and can be restored again."),
                        h_flex().min_w_0().gap_2().items_baseline().child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().font_family(theme.mono_font_family.clone()).text_size(px(13.5)).child(self.upload_to.clone())).children(self.upload_free.map(|f| muted(&theme, format!("{} free", gb(f))))),
                        (self.writable == Some(false)).then(|| ui::button("rf-upload-to").outline().icon(IconName::Folder).label("Change…").on_click(cx.listener(|d, _, window, cx| d.change_folder(true, window, cx))).into_any_element()),
                        true,
                    ));
                if self.writable == Some(false) {
                    body = body.child(blk(
                        &theme,
                        false,
                        format!("SENTIENT cannot write in {}", self.upload_to),
                        "The folder belongs to another account than the one the Platform Manager signs in with.",
                        &["Make it writable (asks for the administrator password once), or".into(), "upload to another folder.".into()],
                        vec![
                            ui::button("rf-make-writable").primary().icon(icon("lock")).label("Make it writable…").on_click(cx.listener(|d, _, _, cx| d.make_writable(cx))).into_any_element(),
                            ui::button("rf-other-folder").outline().icon(IconName::Folder).label("Another folder…").on_click(cx.listener(|d, _, window, cx| d.change_folder(true, window, cx))).into_any_element(),
                        ],
                    ));
                }
                body = body.children(self.transfer_card(&theme, &self.upload.clone(), "Uploading")).when(matches!(self.upload, Transfer::Running { .. }), |b| {
                    b.child(muted(&theme, "Over the same SSH connection. A broken upload goes on from where it stopped."))
                });
                if let Transfer::Done(p) = &self.upload {
                    body = body.child(ui::outcome(&theme, Tone::Good, format!("Uploaded to {}. Its size matches; the check reads every part next.", p.display())));
                }
            }
            _ => {
                let others: Vec<Instance> = self.instances.clone();
                let label = self.src.as_ref().map(|s| format!("{} · {}", s.name, s.host)).unwrap_or_else(|| "No other instance".into());
                let this = cx.entity().downgrade();
                let picker = ui::button("rf-src").outline().label(label).icon(IconName::ChevronDown).disabled(others.is_empty() || matches!(self.copy, Transfer::Running { .. })).dropdown_menu(move |menu, _, _| {
                    let mut m = menu;
                    for i in others.clone() {
                        let this = this.clone();
                        m = m.item(PopupMenuItem::new(format!("{} · {}", i.name, i.host)).on_click(move |_, _, cx| {
                            this.update(cx, |d, cx| d.pick_src(i.clone(), cx)).ok();
                        }));
                    }
                    m
                });
                body = body.child(crate::manager::backup::card_row(&theme, false, "Instance", None, h_flex().gap_2().items_center().child(picker).child(muted(&theme, "from Instances")), None, false));
                let src_name = self.src.as_ref().map(|s| s.name.clone()).unwrap_or_default();
                match self.src_list.clone() {
                    Read::Idle => {}
                    Read::Loading => body = body.child(self.skeleton(&theme)).child(muted(&theme, format!("Reading the backups on {src_name}…"))),
                    Read::Failed(e) => {
                        let host = self.src.as_ref().map(|s| format!("{}:{}", s.host, s.port)).unwrap_or_default();
                        body = body.child(blk(
                            &theme,
                            false,
                            format!("{src_name} is not answering"),
                            if e.contains("did not answer") || e.contains("timed out") { format!("No answer over SSH from {host} within 15 s.") } else { e },
                            &[format!("Check that {src_name} is on and on the network."), format!("Try again, or take its backup from a USB disk with “This computer”.")],
                            vec![
                                ui::button("rf-src-again").primary().label("Try again").on_click(cx.listener(|d, _, _, cx| {
                                    if let Some(s) = d.src.clone() {
                                        d.pick_src(s, cx);
                                    }
                                })).into_any_element(),
                                ui::button("rf-instances").outline().icon(icon("server")).label("Open Instances…").on_click(|_, _, cx| {
                                    crate::modal::close(cx);
                                    go("instances", cx);
                                }).into_any_element(),
                            ],
                        ));
                    }
                    Read::Got(l) => {
                        body = body.child(if l.is_empty() { muted(&theme, format!("No backups on {src_name}.")) } else { self.list_table(&theme, &l, self.src_sel, true, cx) });
                        if let Target::Server(dst) = &self.target {
                            let help = format!("Server to server: {src_name} sends the file straight to {}; it needs {src_name} to reach {} on SSH port {}. Through this computer: down to here, then up; slower, works whenever this computer reaches both.", dst.name, dst.name, dst.port);
                            let mut seg = ui::seg_track(&theme);
                            seg = seg.child(ui::seg_item(&theme, "rf-way-0", "Server to server", self.way == remote::CopyWay::Direct).when(self.reaches != Some(false), |s| s.on_click(cx.listener(|d, _, _, cx| {
                                d.way = remote::CopyWay::Direct;
                                cx.notify();
                            }))).when(self.reaches == Some(false), |s| s.opacity(0.5)));
                            seg = seg.child(ui::seg_item(&theme, "rf-way-1", "Through this computer", self.way == remote::CopyWay::ThroughHere).on_click(cx.listener(|d, _, _, cx| {
                                d.way = remote::CopyWay::ThroughHere;
                                cx.notify();
                            })));
                            let reach = match self.reaches {
                                None => "Checking whether it reaches…".to_string(),
                                Some(true) => format!("{src_name} reaches {} ✓", dst.name),
                                Some(false) => format!("{src_name} does not reach {}", dst.name),
                            };
                            body = body.child(
                                h_flex()
                                    .flex_wrap()
                                    .gap_3()
                                    .items_center()
                                    .child(h_flex().w(px(150.)).gap_1().items_center().text_sm().text_color(theme.muted_foreground).child("Copy").child(ui::info("rf-way-i".into(), help.into())))
                                    .child(seg)
                                    .child(muted(&theme, reach)),
                            );
                        }
                        body = body.children(self.transfer_card(&theme, &self.copy.clone(), "Copying"));
                    }
                }
            }
        }
        body
    }
}

struct FromFooter {
    dialog: Entity<RestoreFrom>,
    _o: Subscription,
}

impl Render for FromFooter {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let d = self.dialog.read(cx);
        let dialog = self.dialog.clone();
        let (label, enabled, cancel) = match d.tab {
            0 => ("Check this backup", d.selected.is_some(), "Cancel"),
            1 => match &d.upload {
                Transfer::Running { .. } => ("Check this backup", false, "Cancel upload"),
                Transfer::Broken { .. } => ("Resume upload", true, "Cancel"),
                Transfer::Done(_) => ("Check this backup", true, "Cancel"),
                Transfer::Idle if d.writable == Some(false) => ("Upload", false, "Cancel"),
                Transfer::Idle => ("Check this backup", false, "Cancel"),
            },
            _ => ("Copy and check", d.src_sel.is_some() && matches!(d.src_list, Read::Got(_)) && !matches!(d.copy, Transfer::Running { .. }), "Cancel"),
        };
        let resume = d.tab == 1 && matches!(d.upload, Transfer::Broken { .. });
        h_flex()
            .gap_2()
            .child(ui::button("rf-cancel").outline().label(cancel).on_click(|_, _, cx| crate::modal::close(cx)))
            .child(div().flex_1())
            .child(ui::button("rf-go").primary().label(label).disabled(!enabled).on_click(move |_, _, cx| {
                dialog.update(cx, |d, cx| if resume { d.start_upload(cx) } else { d.pick(cx) });
            }))
    }
}

// ---------------------------------------------------------------------------
// The restore history (R1–R3)
// ---------------------------------------------------------------------------

/// "14 Nov, 17:40" in this computer's time.
fn when(at: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(at).map(|d| d.with_timezone(&chrono::Local).format("%-d %b, %H:%M").to_string()).unwrap_or_else(|_| at.to_string())
}

fn result_pill(theme: &gpui_kit::component::Theme, r: Result_) -> Div {
    let tone = match r {
        Result_::Done => Tone::Good,
        Result_::RolledBack => Tone::Warn,
        _ => Tone::Bad,
    };
    ui::pill(theme, tone, r.label()).px(px(7.)).py(px(1.)).text_size(px(12.))
}

/// The Restores card. `entries`: None while it is read.
pub fn history_card(theme: &gpui_kit::component::Theme, target: &Target, entries: Option<&Result<Vec<Entry>, String>>, narrow: bool) -> Div {
    let here = if target.is_server() { "Restores on this server, who ran them from which computer, and their logs. Kept for a year." } else { "Restores on this computer, who ran them, and their logs. Kept for a year." };
    let mut c = ui::panel(theme).gap_0().child(h_flex().pb_1().gap_2().items_center().min_h(px(28.)).child(div().flex_1().font_weight(FontWeight::MEDIUM).child("Restores")).child(ui::info("rs-history-i".into(), here.into())));
    match entries {
        None => {
            for n in 0..2 {
                c = c.child(h_flex().min_h(px(43.)).items_center().child(ui::skeleton_line(theme, SharedString::from(format!("rh-sk{n}")), 320.)));
            }
        }
        Some(Err(e)) => c = c.child(div().py_2().text_sm().text_color(theme.muted_foreground).child(e.clone())),
        Some(Ok(l)) if l.is_empty() => c = c.child(div().py_2().text_sm().text_color(theme.muted_foreground).child(if target.is_server() { "No restores on this server yet." } else { "No restores on this computer yet." })),
        Some(Ok(l)) => {
            c = c.child(
                h_flex()
                    .h(px(30.))
                    .px(px(4.))
                    .items_center()
                    .text_size(px(12.5))
                    .text_color(theme.muted_foreground)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().w(px(if narrow { 96. } else { 132. })).flex_shrink_0().child("When"))
                    .child(div().flex_1().min_w_0().child("From backup"))
                    .when(!narrow, |r| r.child(div().w(px(160.)).flex_shrink_0().child("By")))
                    .child(div().w(px(110.)).flex_shrink_0().child("Result"))
                    .child(div().w(px(56.)).flex_shrink_0().text_right().child("Log")),
            );
            for (i, e) in l.iter().enumerate() {
                let (t, e2) = (target.clone(), e.clone());
                c = c.child(
                    h_flex()
                        .min_h(px(43.))
                        .px(px(4.))
                        .items_center()
                        .text_sm()
                        .when(i > 0, |r| r.border_t_1().border_color(theme.border))
                        .child(div().w(px(if narrow { 96. } else { 132. })).flex_shrink_0().font_features(ui::tabular()).child(when(&e.at)))
                        .child(
                            h_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_2()
                                .items_baseline()
                                .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(e.backup.clone()))
                                .when_some(e.source.as_ref().and_then(|s| s.strip_prefix("copied ")).and_then(|s| s.split(',').next()).map(str::to_string), |r, s| r.child(div().flex_shrink_0().text_size(px(12.5)).text_color(theme.muted_foreground).child(s))),
                        )
                        .when(!narrow, |r| r.child(div().w(px(160.)).flex_shrink_0().text_size(px(13.)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(e.by.clone())))
                        .child(h_flex().w(px(110.)).flex_shrink_0().child(result_pill(theme, e.result)))
                        .child(h_flex().w(px(56.)).flex_shrink_0().justify_end().child(ui::button(SharedString::from(format!("rh-log-{i}"))).ghost().small().label("Log").on_click(move |_, _, cx| HistoryDialog::open(t.clone(), e2.clone(), cx)))),
                );
            }
        }
    }
    c
}

/// R3: one restore, what ran, by whom, and its log.
pub struct HistoryDialog {
    entry: Entry,
    log: Read<String>,
    _t: Option<Task<()>>,
}

impl HistoryDialog {
    pub fn open(target: Target, entry: Entry, cx: &mut App) {
        let title = format!("Restore on {}", when(&entry.at));
        let id = entry.id.clone();
        let view = cx.new(|cx| {
            let t = cx.spawn(async move |this, cx| {
                let r = cx.background_executor().spawn(async move { target.log(&id) }).await;
                this.update(cx, |d: &mut HistoryDialog, cx| {
                    d.log = match r {
                        Ok(t) => Read::Got(t),
                        Err(e) => Read::Failed(remote::split_failure(&e).0),
                    };
                    cx.notify();
                })
                .ok();
            });
            HistoryDialog { entry, log: Read::Loading, _t: Some(t) }
        });
        crate::modal::open(crate::modal::Modal { title: title.into(), body: view.clone().into(), width: 680., icon: Some(icon("log-file")), on_close: None }, cx);
        let footer = cx.new(|cx| HistoryFooter { _o: cx.observe(&view, |_, _, cx| cx.notify()), dialog: view });
        crate::modal::set_footer(footer.into(), cx);
    }
}

impl Render for HistoryDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let e = &self.entry;
        let took = |s: u64| if s >= 60 { format!("{} min {:02} s", s / 60, s % 60) } else { format!("{s} s") };
        let mut rows: Vec<(AnyElement, AnyElement)> = vec![
            (txt("Result"), v_flex().gap_1().child(h_flex().child(result_pill(&theme, e.result))).child(div().text_sm().child(e.message.lines().next().unwrap_or("").to_string())).into_any_element()),
            (txt("From"), h_flex().flex_wrap().gap_2().items_baseline().child(div().font_family(theme.mono_font_family.clone()).text_size(px(13.)).child(e.backup.clone())).children(e.source.clone().map(|s| muted(&theme, s))).into_any_element()),
            (txt("What"), txt(format!("{}{}", e.scope, e.converted.as_ref().map(|c| format!("; {c}")).unwrap_or_default()))),
            (txt("By"), txt(format!("{}{}", e.by, if e.manager.is_empty() { String::new() } else { format!(", Platform Manager {}", e.manager) }))),
            (txt("Took"), txt(format!("{}{}", took(e.took_secs), e.down_secs.map(|d| format!(" · SENTIENT down {}", took(d))).unwrap_or_default()))),
        ];
        if let Some(s) = &e.safety {
            rows.push((txt("Safety backup"), div().font_family(theme.mono_font_family.clone()).text_size(px(13.)).child(s.clone()).into_any_element()));
        }
        if let Some(a) = &e.aside {
            rows.push((txt("Data from before"), div().font_family(theme.mono_font_family.clone()).text_size(px(13.)).child(a.clone()).into_any_element()));
        }
        v_flex().gap_3().child(kv(&theme, rows)).child(
            div()
                .id("rh-log")
                .max_h(px(160.))
                .overflow_y_scroll()
                .p(px(10.))
                .rounded(px(8.))
                .bg(theme.muted)
                .font_family(theme.mono_font_family.clone())
                .text_size(px(12.5))
                .children(match &self.log {
                    Read::Got(t) => t.lines().map(|l| div().child(l.to_string())).collect::<Vec<_>>(),
                    Read::Failed(e) => vec![div().text_color(theme.muted_foreground).child(e.clone())],
                    _ => vec![div().text_color(theme.muted_foreground).child("Reading the log…")],
                }),
        )
    }
}

struct HistoryFooter {
    dialog: Entity<HistoryDialog>,
    _o: Subscription,
}

impl Render for HistoryFooter {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let d = self.dialog.read(cx);
        let log = match &d.log {
            Read::Got(t) => Some(t.clone()),
            _ => None,
        };
        let name = format!("restore-{}.log", d.entry.id);
        h_flex()
            .gap_2()
            .child(ui::button("rh-save").outline().label("Save log…").disabled(log.is_none()).on_click(move |_, _, cx| {
                let Some(text) = log.clone() else { return };
                let rx = cx.prompt_for_new_path(&dirs_home(), Some(&name));
                cx.spawn(async move |_| {
                    if let Ok(Ok(Some(p))) = rx.await {
                        let _ = std::fs::write(p, text);
                    }
                })
                .detach();
            }))
            .child(div().flex_1())
            .child(ui::button("rh-close").primary().label("Close").on_click(|_, _, cx| crate::modal::close(cx)))
    }
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

#[allow(dead_code)]
fn _paths(_: &Path) {}

// ---------------------------------------------------------------------------
// Screens: the mockup's frames with canned answers (debug builds only)
// ---------------------------------------------------------------------------

/// SPM_DEBUG_RESTORE=<frame> opens the flow at that frame (R7–R17, X1–X3,
/// B1–B7); SPM_DEBUG_RESTORE_FROM=<frame> the Restore from… dialog (R4–R6l,
/// B8, B9); SPM_DEBUG_RESTORE_HISTORY=canned the history (R1, R3).
#[cfg(debug_assertions)]
pub(crate) mod canned {
    use super::*;
    use crate::restore_flow::Part;

    const GIB: u64 = 1 << 30;

    fn g(x: f64) -> u64 {
        (x * GIB as f64) as u64
    }

    pub fn source(frame: &str) -> Source {
        if frame == "R10e" {
            return Source { file: "/var/backups/sentient/sentient-2026-11-10_config.sentient-backup".into(), sub: "sentient-2026-11-10_config from INVENIA IIoT".into(), note: None };
        }
        if frame == "R13w" {
            return Source { file: "C:\\ProgramData\\SENTIENT\\backups\\sentient-2026-11-14_1712.sentient-backup".into(), sub: "sentient-2026-11-14_1712 from INVENIA IIoT".into(), note: None };
        }
        Source { file: "/var/backups/sentient/sentient-2026-11-14_1712.sentient-backup".into(), sub: "sentient-2026-11-14_1712 from Plant B".into(), note: Some("copied from Plant B, server to server".into()) }
    }

    fn months(config: bool) -> Vec<(String, u64)> {
        if config {
            return Vec::new();
        }
        let mut v = Vec::new();
        for y in [2025, 2026] {
            for m in 1..=12 {
                if y == 2026 && m > 11 {
                    break;
                }
                v.push((format!("db/telemetry/{y}-{m:02}.copy.zst"), g(if y == 2025 { 0.08 } else { 0.14 })));
            }
        }
        v
    }

    pub fn inspection(frame: &str) -> Inspection {
        let config = frame == "R10e";
        let mut files: Vec<(String, u64)> = vec![("db/dump.pgc.zst".into(), g(0.4))];
        files.extend(months(config));
        files.push(("files/reports.tar.zst".into(), g(0.05)));
        let windows = frame == "R13w";
        let (sv, schema) = match frame {
            "B1" => ("4.5.0.3", 33),
            _ => ("4.3.0.58", 30),
        };
        let manifest: sentient_backup_core::manifest::Manifest = serde_json::from_value(serde_json::json!({
            "format_version": if frame == "B6" { 3 } else { 2 }, "tool_version": "0.8.0", "created_at": "2026-11-14T11:42:03Z",
            "source": {"database": "sentient", "postgres_version": "18", "timescaledb_version": "2.17.2", "sentient_version": sv, "schema_version": schema, "database_type": "PostgreSQL + TimescaleDB", "host": "PLANT-B"},
            "components": [{"id": "configuration", "name": "Configuration", "selected": true, "tables": [], "bytes": g(1.2), "rows": 0}, {"id": "telemetry_historical", "name": "Telemetry", "selected": !config, "tables": [], "bytes": if config { 0 } else { g(8.0) }, "rows": 0}],
            "telemetry": if config { serde_json::json!({"mode": "none"}) } else { serde_json::json!({"mode": "all"}) },
            "files": files.iter().map(|(p, b)| serde_json::json!({"path": p, "bytes": b, "sha256": ""})).collect::<Vec<_>>(),
            "file_stores": [{"id": "reports", "category_id": "reports", "source_path": "/var/lib/sentient/reports", "member": "files/reports.tar.zst"}],
            "encryption": {"scheme": "none"},
            "platform": if frame == "R14o" { serde_json::Value::Null } else { serde_json::json!({"settings": {}}) }
        }))
        .expect("canned manifest");
        let parts: Vec<Part> = files
            .iter()
            .map(|(p, b)| Part { path: p.clone(), label: sentient_backup_core::restore::member_label(p), bytes: *b, ok: !(frame == "B5" && p == "db/telemetry/2026-10.copy.zst") })
            .collect();
        let name = if config { "sentient-2026-11-10_config" } else { "sentient-2026-11-14_1712" };
        Inspection {
            file: source(frame).file,
            name: name.into(),
            file_bytes: g(2.8),
            encrypted: false,
            manifest,
            parts,
            missing: Vec::new(),
            sidecar: None,
            here: rf::Here {
                sentient_version: "4.4.0.2".into(),
                schema_version: Some(31),
                database_type: "PostgreSQL + ClickHouse".into(),
                host: Some(if windows { "PLANT-B" } else { "IIOT-01" }.into()),
                windows,
                native: windows,
                http_port: 8080,
                backup_folder: if windows { "C:\\ProgramData\\SENTIENT\\backups".into() } else { "/var/backups/sentient".into() },
            },
            database_type: "PostgreSQL + TimescaleDB".into(),
            made: "14 Nov 2026, 5:12 PM".into(),
            holds: if config { vec!["platform data".into(), "report files and version control".into()] } else { vec!["platform data".into(), "telemetry history (all)".into(), "report files and version control".into()] },
            restored_bytes: g(9.2),
            other_copy: (frame == "B5").then(|| PathBuf::from("/mnt/nas/sentient/sentient-2026-11-14_1712.sentient-backup")),
        }
    }

    fn disk(disk: &str, detail: &str, uses: &[(&str, f64)], need: f64, free: f64) -> DiskRow {
        DiskRow { disk: disk.into(), detail: detail.into(), uses: uses.iter().map(|(w, b)| (w.to_string(), g(*b))).collect(), need: g(need), free: Some(g(free)) }
    }

    fn row(key: &str, label: &str, b: &str, h: &str, kind: SettingKind, why: &str) -> rf::SettingRow {
        rf::SettingRow { key: key.into(), label: label.into(), backup: b.into(), here: h.into(), kind, why: why.into() }
    }

    pub fn precheck(frame: &str) -> Precheck {
        let windows = frame == "R13w";
        let disks = if windows {
            vec![
                disk("C:", "System", &[("Unpacking", 3.1), ("Safety backup", 2.9)], 6.0, 41.0),
                disk("D:", "PostgreSQL", &[("Restored database beside the live one", 4.2)], 4.2, 812.0),
                disk("C:", "ClickHouse (WSL)", &[("Restored history inside the WSL disk", 5.0)], 5.0, 41.0),
            ]
        } else if frame == "B2" || frame == "B2s" {
            vec![
                disk("/", "/dev/sda1", &[("Upload", 2.8), ("Unpacking", 3.1), ("Safety backup", 2.9)], 8.8, 3.9),
                disk("/mnt/disk-a", "PostgreSQL", &[("Restored database, beside the live one", 4.2)], 4.2, 1400.0),
                disk("/mnt/disk-b", "ClickHouse", &[("Restored history, beside the live one", 5.0)], 5.0, 3100.0),
            ]
        } else {
            vec![
                disk("/", "/dev/sda1", &[("Unpacking", 3.1), ("Safety backup", 2.9)], 6.0, 64.1),
                disk("/mnt/disk-a", "PostgreSQL", &[("Restored database beside the live one", 4.2)], 4.2, 1400.0),
                disk("/mnt/disk-b", "ClickHouse", &[("Restored history beside the live one", 5.0)], 5.0, 3100.0),
            ]
        };
        let old = frame == "R14o";
        let mut settings = vec![
            row("sentient_version", "SENTIENT version", "4.3.0.58", "4.4.0.2", SettingKind::Fixed, "This server's; the data is upgraded"),
            row("database_type", "Database type", "PostgreSQL + TimescaleDB", "PostgreSQL + ClickHouse", SettingKind::DatabaseType, "Converted to this server's type. To use the backup's, change the type in Migration first"),
        ];
        if !old {
            settings.extend([
                row("port:mqtt", "MQTT port", "1883", "21883", SettingKind::Choice { backup_default: false }, "Tied to this machine and its firewall"),
                row("public_url", "Public URL", "https://plantb.local", "https://iiot.invenia.in", SettingKind::Choice { backup_default: false }, "Devices and links reach this server by its own name"),
                row("tls", "TLS certificate", "plantb.local, until 3 Mar 2027", "iiot.invenia.in, until 9 Jan 2027", SettingKind::Choice { backup_default: false }, "A certificate belongs to the host name"),
                row("REPORT_TZ", "Time zone (reports)", "Asia/Kolkata", "UTC", SettingKind::Choice { backup_default: true }, "A setting of the plant, not the machine"),
                row("AUDIT_LOG_TTL_DAYS", "Telemetry retention", "365 days", "90 days", SettingKind::Choice { backup_default: true }, "Kept data would be cut to 90 days otherwise"),
                row("licence", "Licence", "Active, bound to PLANT-B", "Active, bound to this server", SettingKind::Locked, "Bound to the hardware: not carried over"),
                row("gateways", "Gateway connections", "3 gateways → plantb.local:1883", "—", SettingKind::Gateway, "Gateways keep their own address; point them here after"),
            ]);
        }
        settings.push(row("files:reports", "Report files", "/var/lib/sentient/reports", "/mnt/disk-a/sentient/reports", SettingKind::Choice { backup_default: false }, "Paths of this machine"));
        Precheck {
            disks,
            min_free: 2 << 30,
            estimate: sentient_backup_core::restore_scope::Estimate { total_secs: if windows { 21 * 60 } else { 18 * 60 }, downtime_secs: if windows { 120 } else { 60 } },
            restored_bytes: g(if windows { 9.3 } else { 9.2 }),
            upgrade: Some("Schema upgrade 30 → 31 (SENTIENT 4.3.0.58 → 4.4.0.2)".into()),
            settings,
            same: 14,
            recorded: !old,
            live: rf::Live { devices: 1284, dashboards: 96, users: 41, telemetry_bytes: g(84.0), ch_problem: (frame == "B3").then(|| "nothing answered on port 8123.".into()) },
            orphans: Some((vec!["Press 7".into(), "Press 8".into(), "Line 2 meter".into()], 3)),
            safety: rf::Safety { name: "sentient-before-restore-2026-11-16_1102".into(), folder: "/var/backups/sentient".into(), bytes: g(2.9), secs: 240 },
            newest_backup: Some("16 Nov, 02:00".into()),
        }
    }

    /// The canned job: the answers a real one gives, at once (or, for a
    /// running frame, partly and then waiting).
    pub fn job(frame: &str, kind: &str, sink: &ProgressFn) -> Result<(), String> {
        let hang = || loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        };
        match kind {
            "restore-inspect" => {
                sink(Progress::Step { name: "Checking the backup".into() });
                match frame {
                    "R8" => {
                        sink(rf::data_line("unreadable", &Unreadable::Passphrase));
                        return Err(Unreadable::Passphrase.message());
                    }
                    "B4" => {
                        sink(rf::data_line("unreadable", &Unreadable::WrongPassphrase));
                        return Err(Unreadable::WrongPassphrase.message());
                    }
                    "B6b" => {
                        let u = Unreadable::NotABackup("this file is not a SENTIENT backup (no manifest at its start)".into());
                        sink(rf::data_line("unreadable", &u));
                        return Err(u.message());
                    }
                    _ => {}
                }
                let i = inspection(frame);
                let listed: Vec<(String, u64)> = i.parts.iter().map(|p| (p.label.clone(), p.bytes)).collect();
                sink(rf::data_line("parts", &listed));
                sink(Progress::Log { line: "PART\tFormat 2, made by Platform Manager 0.8.0\t0".into() });
                let upto = if frame == "R7" { 21 } else { listed.len() };
                for (l, b) in listed.iter().take(upto) {
                    sink(Progress::Log { line: format!("PART\t{l}\t{b}") });
                }
                if frame == "R7" {
                    hang();
                }
                sink(rf::data_line("inspect", &i));
                Ok(())
            }
            "restore-precheck" => {
                sink(Progress::Step { name: "Measuring the disks".into() });
                if frame == "R13p" {
                    hang();
                }
                sink(rf::data_line("precheck", &precheck(frame)));
                Ok(())
            }
            _ => {
                let steps = [rf::STEP_SAFETY, rf::STEP_CHECK, rf::STEP_RESTORE, rf::STEP_SCHEMA, rf::STEP_SWITCH];
                let log = |l: &str| sink(Progress::Log { line: l.into() });
                let mut entry = Entry { id: "20261116110200".into(), at: "2026-11-16T05:32:00Z".into(), backup: "sentient-2026-11-14_1712".into(), source: Some("copied from Plant B, server to server".into()), by: "mz on MZ-LAPTOP".into(), scope: "Everything".into(), safety: Some("sentient-before-restore-2026-11-16_1102".into()), manager: "0.8.0".into(), ..Default::default() };
                sink(Progress::Step { name: steps[0].into() });
                log("/var/backups/sentient/sentient-before-restore-2026-11-16_1102.sentient-backup · 2.9 GB, checked");
                sink(Progress::Step { name: steps[1].into() });
                log("Every part matches its SHA-256 (26 parts).");
                sink(Progress::Step { name: steps[2].into() });
                for l in [">> Creating the empty database sentient_restore_202611161106", ">> Restoring database (pg_restore)", ">> Converting telemetry", "  2026-08: copied and checked", "  2026-09: copied and checked"] {
                    log(l);
                }
                match frame {
                    "R16" => {
                        sink(Progress::Percent { value: 0.46 });
                        log("  history 190 of 412 M rows");
                        hang();
                        Ok(())
                    }
                    "X1" | "X3" => {
                        let stop = frame == "X3";
                        if stop {
                            entry.result = Result_::Stopped;
                            entry.message = "Stopped: /mnt/disk-a (PostgreSQL) fell to 1.9 GB free, under the safe level of 2 GB. Something else is writing there (sentient-postgres WAL grew 12 GB). Nothing on INVENIA IIoT changed; the half-restored database was removed.".into();
                        } else {
                            log("pg_restore: error: could not extend file \"base/16384/2619\"");
                            log("pg_restore: error: No space left on device");
                            entry.result = Result_::Failed;
                            entry.message = "pg_restore failed: could not extend file \"base/16384/2619\": No space left on device. Nothing on INVENIA IIoT changed: SENTIENT runs on its own data. The half-restored database was removed.".into();
                        }
                        entry.failed_step = Some(rf::STEP_RESTORE.into());
                        sink(rf::data_line("result", &entry));
                        Err(entry.message)
                    }
                    _ => {
                        sink(Progress::Step { name: steps[3].into() });
                        sink(Progress::Step { name: steps[4].into() });
                        if frame == "X2" {
                            entry.result = Result_::RolledBack;
                            entry.restored_db = Some("sentient_restore_202611161106".into());
                            entry.message = "SENTIENT did not start on the restored data. It runs on its previous data again; the restored database sentient_restore_202611161106 is unchanged, for a look.".into();
                            sink(rf::data_line("result", &entry));
                            return Err(entry.message);
                        }
                        entry.result = Result_::Done;
                        entry.licence_note = true;
                        entry.message = "SENTIENT runs on the data from sentient-2026-11-14_1712. The data from before is kept as sentient_before_restore_202611161118, and as the safety backup.".into();
                        sink(rf::data_line("result", &entry));
                        sink(Progress::Done { message: entry.message.clone() });
                        Ok(())
                    }
                }
            }
        }
    }

    /// The fields a frame shows filled in.
    pub fn inputs(frame: &str, f: &RestoreFlow, window: &mut Window, cx: &mut Context<RestoreFlow>) {
        match frame {
            "R15" => {
                let w = f.target.confirm_word();
                f.confirm.update(cx, |i, cx| i.set_value(w, window, cx));
            }
            "R10d" => {
                f.from.update(cx, |i, cx| i.set_value("1 Sep 2026", window, cx));
                f.to.update(cx, |i, cx| i.set_value("14 Nov 2026", window, cx));
            }
            "B4" => f.pass.update(cx, |i, cx| i.set_value("wrong-pass", window, cx)),
            _ => {}
        }
    }

    /// After the canned check: on to the frame's step.
    pub fn advance(f: &mut RestoreFlow, frame: &str, cx: &mut Context<RestoreFlow>) {
        let step = match frame {
            "R10" | "R10b" | "R10c" | "R10d" | "R10e" => Step::What,
            "R11" => Step::Where,
            "R13" | "R13w" | "B2" | "B2s" | "R13p" => Step::Precheck,
            "R14" | "R14o" | "B3" | "B7" => Step::Settings,
            "R15" => Step::Confirm,
            "R16" | "R17" | "X1" | "X2" | "X3" => Step::Run,
            _ => return,
        };
        f.scope = match frame {
            "R10b" | "R10c" => 1,
            "R10d" => 3,
            _ => 0,
        };
        f.keep = frame != "R10c";
        f.db_type_backup = frame == "B3";
        f.delete_old = matches!(frame, "R15" | "R16" | "R17" | "X1" | "X2" | "X3" | "B2" | "B2s" | "R13" | "R13w");
        f.safety = true;
        f.step = step;
        if step >= Step::Precheck || matches!(frame, "R10b") {
            f.precheck = Some(precheck(frame));
            f.apply_defaults();
        }
        if step == Step::Run {
            let req = f.request(cx);
            f.planned = rf::planned_steps(&Request { delete_old: false, ..req });
            let job = f.run.clone();
            let label = f.label("Restoring");
            f.start(&job, "restore-run", label, Request::default(), cx);
        }
    }

    pub fn history() -> Vec<Entry> {
        vec![
            Entry {
                id: "20261114174000".into(),
                at: "2026-11-14T12:10:00Z".into(),
                backup: "sentient-2026-11-14_1712".into(),
                source: Some("copied from Plant B, server to server".into()),
                by: "mz on MZ-LAPTOP".into(),
                scope: "Everything".into(),
                result: Result_::Done,
                message: "SENTIENT answered on the restored data after 48 s.".into(),
                took_secs: 1040,
                down_secs: Some(65),
                safety: Some("sentient-before-restore-2026-11-14_1722".into()),
                aside: Some("sentient_before_restore_202611141740".into()),
                converted: Some("history converted from PostgreSQL + TimescaleDB".into()),
                manager: "0.8.0".into(),
                ..Default::default()
            },
            Entry { id: "20261102091200".into(), at: "2026-11-02T03:42:00Z".into(), backup: "sentient-2026-11-01_0200".into(), by: "ops on PLANT-B".into(), scope: "Everything".into(), result: Result_::RolledBack, message: "SENTIENT did not start on the restored data.".into(), took_secs: 780, manager: "0.8.0".into(), ..Default::default() },
        ]
    }

    pub fn from_dialog(d: &mut RestoreFrom, frame: &str) {
        let list = |n: usize| -> Vec<crate::backups::Entry> {
            [("sentient-2026-11-16_0200", "2026-11-16 02:00", 2.9, Some(true)), ("sentient-2026-11-15_0200", "2026-11-15 02:00", 2.9, Some(true)), ("sentient-2026-11-14_1712", "2026-11-14 17:12", 2.8, None), ("sentient-2026-11-13_0200", "2026-11-13 02:00", 2.8, Some(false))]
                .iter()
                .take(n)
                .map(|(name, made, gbs, ok)| crate::backups::Entry {
                    path: format!("/var/backups/sentient/{name}.sentient-backup").into(),
                    name: name.to_string(),
                    bytes: g(*gbs),
                    parts: 1,
                    made: made.to_string(),
                    automatic: false,
                    check: ok.map(|ok| crate::backups::CheckRecord { at: "2026-11-16T03:00:00+05:30".into(), ok, summary: String::new() }),
                    old_folder: false,
                })
                .collect()
        };
        d.list = Read::Got(list(4));
        d.upload_free = Some(g(64.1));
        match frame {
            "R4" => d.selected = Some(2),
            "R5" | "R5e" | "B9" => {
                d.tab = 1;
                d.local = Some(("D:\\Backups\\plant-b-2026-11-14_1712.sentient-backup".into(), g(2.8)));
                match frame {
                    "R5" => d.upload = Transfer::Running { sent: g(1.1), total: g(2.8), started: std::time::Instant::now() - std::time::Duration::from_secs(47) },
                    "R5e" => d.upload = Transfer::Broken { sent: g(1.6), total: g(2.8), why: "Connection closed: no answer for 30 s".into() },
                    _ => d.writable = Some(false),
                }
            }
            _ => {
                d.tab = 2;
                d.src = d.instances.iter().find(|i| i.name != "INVENIA IIoT").cloned().or_else(|| d.instances.first().cloned());
                match frame {
                    "R6l" => d.src_list = Read::Loading,
                    "B8" => d.src_list = Read::Failed("did not answer".into()),
                    _ => {
                        d.src_list = Read::Got(list(4).into_iter().skip(0).take(2).collect());
                        d.src_sel = Some(1);
                        d.reaches = Some(true);
                    }
                }
            }
        }
    }
}
