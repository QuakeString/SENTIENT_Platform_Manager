// Copyright © 2016-2026 The SENTIENT Authors

//! Runs blocking engine work (installer-core, backup-core) off the UI thread
//! and streams its progress into a view.
//!
//! The engines report through a `ProgressFn` callback that may be called from
//! any thread. [`Job::start`] hands the engine a callback that forwards into a
//! channel, and a foreground task drains that channel into the job's state,
//! notifying the views that render it. The channel closes when the engine
//! returns and drops the callback, so the drain loop ends on its own.

use futures::StreamExt as _;
use gpui_kit::{App, AppContext as _, Context, SharedString, Task};
use sentient_installer_core::diagnose::{self, Diagnosis, FixAction};
use sentient_installer_core::progress::{PlanStep, Progress, ProgressFn};

/// How many log lines a job keeps in memory; the whole log is in its file
/// (`Job::log_file`), which Save copies.
pub const LOG_KEEP: usize = 5000;

/// Where a planned step is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowState {
    Pending,
    Running,
    Done,
    Failed,
    /// The job ended without needing it.
    Skipped,
}

/// A planned step as the job went through it.
#[derive(Debug, Clone)]
pub struct Row {
    pub step: PlanStep,
    pub state: RowState,
    started: Option<std::time::Instant>,
    took: Option<std::time::Duration>,
    /// The engine's own step inside it ("Dumping database"), while it runs.
    pub sub: Option<SharedString>,
}

impl Row {
    fn new(step: PlanStep) -> Self {
        Self { step, state: RowState::Pending, started: None, took: None, sub: None }
    }

    /// How long it took, or has been running.
    pub fn time(&self) -> Option<std::time::Duration> {
        self.took.or_else(|| self.started.filter(|_| self.state == RowState::Running).map(|t| t.elapsed()))
    }
}

/// A page's own handling of a fix (Setup's "Use port NNNN instead", Ask
/// again): true when it applied it.
pub type FixHandler = std::rc::Rc<dyn Fn(&FixAction, &mut gpui_kit::Window, &mut App) -> bool>;

/// What a page handles itself, and how.
#[derive(Clone)]
pub struct PageFixes {
    pub handler: FixHandler,
    /// It can change a port (Setup's fields).
    pub ports: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum JobState {
    Idle,
    Running,
    Done(SharedString),
    Failed(SharedString),
}

pub struct Job {
    state: JobState,
    step: SharedString,
    percent: Option<f32>,
    /// The last LOG_KEEP lines; `log_total` counts them all.
    log: std::collections::VecDeque<SharedString>,
    log_total: usize,
    /// The whole log of the current run (crate::manager::jobview's Save).
    log_file: Option<std::path::PathBuf>,
    /// The log card's list, and whether it follows the newest line.
    log_scroll: gpui_kit::UniformListScrollHandle,
    follow: std::rc::Rc<std::cell::Cell<bool>>,
    /// The plan of the next run (`set_plan`), and the current run's steps.
    next_plan: Option<Vec<PlanStep>>,
    rows: Vec<Row>,
    /// Whether the job declared its steps (a plan); otherwise its steps
    /// are added as they start.
    planned: bool,
    current: Option<usize>,
    /// When the run began, and how long it took once ended.
    began: Option<std::time::Instant>,
    took: Option<std::time::Duration>,
    /// Every step's name, in the order they started.
    steps: Vec<SharedString>,
    /// The latest completion message a step reported.
    last_done: Option<SharedString>,
    /// When the current step began.
    step_started: Option<std::time::Instant>,
    /// What the global progress strip calls it ("Stopping ClickHouse on
    /// DEV-01"); None: its current step.
    label: Option<SharedString>,
    /// What the strip's Details opens for this job (a page's own result
    /// view); None: the job's steps and output.
    details: Option<std::rc::Rc<dyn Fn(&mut App)>>,
    /// Run it again from its failure dialog (crate::manager::strip).
    retry: Option<std::rc::Rc<dyn Fn(&mut App)>>,
    /// The remote instance it works on (id, name, colour); None: this computer.
    instance: Option<(String, String, String)>,
    /// Followed on its own page (crate::manager::jobview): its result shows
    /// there, so no dialog opens when it fails.
    on_page: bool,
    /// The strip chip's text while it runs ("Copying 2 items · 42%"), and
    /// a panel the chip opens instead of Details (the Files transfers).
    chip: Option<std::rc::Rc<dyn Fn(&App) -> String>>,
    panel: Option<gpui_kit::AnyView>,
    /// What went wrong, when it failed (sentient_installer_core::diagnose):
    /// the agent's own (a `DIAG` line), else made here once the work ended.
    diagnosis: Option<Diagnosis>,
    /// Its error panel (crate::diagnosis), made when it failed.
    diag_view: Option<gpui_kit::Entity<crate::diagnosis::DiagnosisView>>,
    fix_handler: Option<PageFixes>,
    _task: Option<Task<()>>,
    /// Re-renders once a second while running, so the step's time moves.
    _ticker: Option<Task<()>>,
}

impl Job {
    pub fn new() -> Self {
        Self {
            state: JobState::Idle,
            step: SharedString::default(),
            percent: None,
            log: Default::default(),
            log_total: 0,
            log_file: None,
            log_scroll: gpui_kit::UniformListScrollHandle::new(),
            follow: std::rc::Rc::new(std::cell::Cell::new(true)),
            next_plan: None,
            rows: Vec::new(),
            planned: false,
            current: None,
            began: None,
            took: None,
            steps: Vec::new(),
            last_done: None,
            step_started: None,
            label: None,
            details: None,
            retry: None,
            instance: None,
            on_page: false,
            chip: None,
            panel: None,
            diagnosis: None,
            diag_view: None,
            fix_handler: None,
            _task: None,
            _ticker: None,
        }
    }

    /// Name the job for the progress strip (before `start`).
    pub fn set_label(&mut self, label: impl Into<SharedString>) {
        self.label = Some(label.into());
    }

    /// The steps the next run takes, before `start` (crate::jobplan): its
    /// page shows them all from the first frame. The work may send its own
    /// (`Progress::Plan`), which replaces the ones not started yet.
    pub fn set_plan(&mut self, steps: Vec<PlanStep>) {
        self.next_plan = Some(steps);
    }

    /// The run's steps, in order.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The step running now (an index into `rows`).
    pub fn current(&self) -> Option<usize> {
        self.current
    }

    /// The whole job, 0–100: each step by its weight, the running one by
    /// its own percent when it reports one.
    pub fn overall(&self) -> f32 {
        if matches!(self.state, JobState::Done(_)) {
            return 100.;
        }
        let total: f32 = self.rows.iter().map(|r| r.step.weight.max(0.)).sum();
        if total <= 0. {
            return self.percent.unwrap_or(0.);
        }
        let done: f32 = self.rows.iter().filter(|r| matches!(r.state, RowState::Done | RowState::Skipped)).map(|r| r.step.weight.max(0.)).sum();
        let part = match (self.current, self.percent) {
            (Some(i), Some(p)) if self.rows[i].state == RowState::Running => self.rows[i].step.weight.max(0.) * p / 100.,
            _ => 0.,
        };
        ((done + part) / total * 100.).clamp(0., 100.)
    }

    /// How long the run has been going, or took.
    pub fn elapsed(&self) -> Option<std::time::Duration> {
        self.took.or_else(|| self.began.map(|t| t.elapsed()))
    }

    /// Log lines `range` of the kept ones (oldest first).
    pub fn log_lines(&self, range: std::ops::Range<usize>) -> Vec<SharedString> {
        self.log.range(range.start.min(self.log.len())..range.end.min(self.log.len())).cloned().collect()
    }

    /// How many lines were written, kept or not.
    pub fn log_total(&self) -> usize {
        self.log_total
    }

    /// The file with the whole log of the current run.
    pub fn log_file(&self) -> Option<&std::path::Path> {
        self.log_file.as_deref()
    }

    /// The whole log as text: its file, else the kept lines.
    pub fn log_text(&self) -> String {
        if let Some(t) = self.log_file.as_ref().and_then(|f| std::fs::read_to_string(f).ok()) {
            return t;
        }
        self.log.iter().map(|l| l.as_ref()).collect::<Vec<_>>().join("\n")
    }

    pub fn log_scroll(&self) -> &gpui_kit::UniformListScrollHandle {
        &self.log_scroll
    }

    /// Whether the log card follows the newest line (false once the user
    /// scrolled up; "Latest" sets it again).
    pub fn follow(&self) -> std::rc::Rc<std::cell::Cell<bool>> {
        self.follow.clone()
    }

    /// Screens and the layout test: a run in the middle, with `lines` log
    /// lines (SPM_DEBUG_JOB_LOG, crate::manager::jobview::debug_job).
    #[cfg(debug_assertions)]
    pub fn debug_fill(&mut self, plan: Vec<PlanStep>, at: usize, lines: usize, cx: &mut Context<Self>) {
        self.state = JobState::Running;
        self.rows = plan.into_iter().map(Row::new).collect();
        self.planned = true;
        self.began = Some(std::time::Instant::now() - std::time::Duration::from_secs(97));
        self.took = None;
        let at = at.min(self.rows.len().saturating_sub(1));
        for (i, r) in self.rows.iter_mut().enumerate() {
            if i < at {
                r.state = RowState::Done;
                r.took = Some(std::time::Duration::from_secs(12 + 7 * i as u64));
            } else if i == at {
                r.state = RowState::Running;
                r.started = Some(std::time::Instant::now() - std::time::Duration::from_secs(41));
                r.sub = Some("Exporting ClickHouse telemetry, month 2026-09 of 2026-11".into());
            }
        }
        self.current = Some(at);
        self.percent = Some(37.);
        self.log.clear();
        for i in 0..lines {
            let l: SharedString = match i % 9 {
                0 => format!(">> Step {i}").into(),
                4 => format!("clickhouse: ts_kv partition 2026-{:02} part {i}: 412 MB written, {} rows, a long line that runs past the width of the card to show it is cut, not wrapped, and never scrolls the page sideways", i % 12 + 1, i * 1031).into(),
                _ => format!("pg_dump: dumping contents of table public.table_{i}").into(),
            };
            self.push_line(l);
        }
        cx.notify();
    }

    fn push_line(&mut self, line: SharedString) {
        if self.log.len() >= LOG_KEEP {
            self.log.pop_front();
        }
        self.log.push_back(line);
        self.log_total += 1;
    }

    /// Shown on its page (see `on_page`).
    pub fn set_on_page(&mut self, on: bool) {
        self.on_page = on;
    }

    pub fn set_chip(&mut self, chip: std::rc::Rc<dyn Fn(&App) -> String>, panel: gpui_kit::AnyView) {
        self.chip = Some(chip);
        self.panel = Some(panel);
    }

    pub fn chip(&self) -> Option<(std::rc::Rc<dyn Fn(&App) -> String>, gpui_kit::AnyView)> {
        Some((self.chip.clone()?, self.panel.clone()?))
    }

    pub fn on_page(&self) -> bool {
        self.on_page
    }

    /// What the strip's Details opens (see `details`).
    pub fn set_details(&mut self, open: std::rc::Rc<dyn Fn(&mut App)>) {
        self.details = Some(open);
    }

    pub fn details(&self) -> Option<std::rc::Rc<dyn Fn(&mut App)>> {
        self.details.clone()
    }

    /// The remote instance this job works on (the bell names it).
    pub fn set_instance(&mut self, id: String, name: String, color: String) {
        self.instance = Some((id, name, color));
    }

    pub fn instance(&self) -> Option<(String, String, String)> {
        self.instance.clone()
    }

    /// What Retry in its failure dialog runs.
    pub fn set_retry(&mut self, retry: std::rc::Rc<dyn Fn(&mut App)>) {
        self.retry = Some(retry);
    }

    pub fn retry(&self) -> Option<std::rc::Rc<dyn Fn(&mut App)>> {
        self.retry.clone()
    }

    /// A page that applies some fixes itself (see [`FixHandler`]).
    pub fn set_fix_handler(&mut self, handler: FixHandler, ports: bool) {
        self.fix_handler = Some(PageFixes { handler, ports });
    }

    /// What went wrong (when it failed).
    pub fn diagnosis(&self) -> Option<&Diagnosis> {
        self.diagnosis.as_ref()
    }

    /// Its error panel, while it is failed.
    pub fn diagnosis_view(&self) -> Option<gpui_kit::AnyView> {
        matches!(self.state, JobState::Failed(_)).then(|| self.diag_view.clone().map(Into::into)).flatten()
    }

    pub fn state(&self) -> &JobState {
        &self.state
    }

    pub fn step(&self) -> &SharedString {
        &self.step
    }

    /// Back to idle (a failure the operator closed). Ignored while running.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        if self.is_running() {
            return;
        }
        self.state = JobState::Idle;
        self.steps.clear();
        self.log.clear();
        self.log_total = 0;
        self.rows.clear();
        self.current = None;
        self.planned = false;
        self.diagnosis = None;
        self.diag_view = None;
        cx.notify();
    }

    /// The steps started so far, oldest first.
    pub fn steps(&self) -> &[SharedString] {
        &self.steps
    }

    /// Fraction of the current step, 0–100, when the engine reports one.
    pub fn percent(&self) -> Option<f32> {
        self.percent
    }

    /// The kept log lines (the last LOG_KEEP), oldest first.
    pub fn log(&self) -> &std::collections::VecDeque<SharedString> {
        &self.log
    }

    /// How long the current step has been running.
    pub fn step_elapsed(&self) -> Option<std::time::Duration> {
        self.step_started.filter(|_| self.is_running()).map(|t| t.elapsed())
    }

    /// "Installing the SENTIENT schema · 1:24" while running.
    pub fn step_with_time(&self) -> SharedString {
        match self.step_elapsed() {
            Some(d) if !self.step.is_empty() => format!("{} · {}:{:02}", crate::i18n::dynamic(&self.step), d.as_secs() / 60, d.as_secs() % 60).into(),
            _ => crate::i18n::dynamic(&self.step).into_owned().into(),
        }
    }

    pub fn is_running(&self) -> bool {
        self.state == JobState::Running
    }

    /// Run `work` on a background thread. It receives the progress sink and
    /// returns `Err(message)` on failure. Starting a job while one runs is
    /// ignored.
    pub fn start(
        &mut self,
        work: impl FnOnce(ProgressFn) -> Result<(), String> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.is_running() {
            return;
        }
        self.state = JobState::Running;
        self.step = SharedString::default();
        self.percent = None;
        self.log.clear();
        self.log_total = 0;
        self.follow.set(true);
        self.steps.clear();
        let plan = self.next_plan.take();
        self.planned = plan.as_ref().is_some_and(|p| !p.is_empty());
        self.rows = plan.unwrap_or_default().into_iter().map(Row::new).collect();
        self.current = None;
        self.began = Some(std::time::Instant::now());
        self.took = None;
        self.last_done = None;
        self.diagnosis = None;
        self.diag_view = None;
        self.step_started = Some(std::time::Instant::now());
        self.log_file = new_log_file(self.label.as_deref().unwrap_or("job"));
        cx.notify();
        // The global progress strip follows it (crate::manager::strip).
        let me = cx.entity();
        let label = self.label.take();
        if let Some(l) = &label {
            activity(l);
        }
        cx.defer(move |cx| crate::manager::strip::track(me, label, cx));
        self._ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                let running = this
                    .update(cx, |job, cx| {
                        cx.notify();
                        job.is_running()
                    })
                    .unwrap_or(false);
                if !running {
                    break;
                }
            }
        }));

        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Progress>();
        // The last output lines, for the diagnosis when it fails (a sync
        // lock, held only to push or copy; never across an await).
        let tail: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>> = Default::default();
        let lines = tail.clone();
        // The whole log, line by line, in its file (a sync lock, held only
        // to write a line).
        let file = std::sync::Mutex::new(self.log_file.as_ref().and_then(|p| std::fs::File::create(p).ok()).map(std::io::LineWriter::new));
        let sink: ProgressFn = std::sync::Arc::new(move |p| {
            if let Progress::Log { line } = &p {
                if let Ok(mut t) = lines.lock() {
                    if t.len() >= 200 {
                        t.pop_front();
                    }
                    t.push_back(line.clone());
                }
            }
            let text = match &p {
                Progress::Log { line } if !line.starts_with(diagnose::DIAG_PREFIX) => Some(crate::ui::strip_ansi(line)),
                Progress::Step { name } => Some(format!(">> {name}")),
                Progress::Error { message } => Some(message.clone()),
                _ => None,
            };
            if let (Some(t), Ok(mut f)) = (text, file.lock()) {
                if let Some(f) = f.as_mut() {
                    use std::io::Write as _;
                    let _ = writeln!(f, "{t}");
                }
            }
            let _ = tx.unbounded_send(p);
        });
        // A job is something the user started: its root steps may ask for the
        // administrator password, once (sentient_installer_core::elevation).
        // A remote job: only what it printed is known here (its agent
        // diagnoses itself and sends the answer as a DIAG line).
        let remote = self.instance.is_some();
        let worker = cx.background_executor().spawn(async move {
            let _action = sentient_installer_core::elevation::user_action();
            let result = work(sink);
            let diag = result.as_ref().err().map(|e| {
                let lines: Vec<String> = tail.lock().map(|t| t.iter().cloned().collect()).unwrap_or_default();
                if let Some(d) = lines.iter().rev().find_map(|l| Diagnosis::decode(l)) {
                    return d;
                }
                let shown: Vec<String> = lines.into_iter().filter(|l| !l.starts_with(diagnose::DIAG_PREFIX)).collect();
                if remote {
                    diagnose::diagnose(&diagnose::Evidence::from_text(e, &shown))
                } else {
                    diagnose::diagnose_failure(e, &shown, crate::online::downloads_dir("").parent())
                }
            });
            // Screens: a canned cause (SPM_DEBUG_DIAGNOSIS, crate::diagnosis).
            #[cfg(debug_assertions)]
            let diag = diag.map(|d| crate::diagnosis::debug_diagnosis().unwrap_or(d));
            (result, diag)
        });
        self._task = Some(cx.spawn(async move |this, cx| {
            while let Some(progress) = rx.next().await {
                if this
                    .update(cx, |job, cx| {
                        job.apply(progress);
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
            let (result, diag) = worker.await;
            this.update(cx, |job, cx| {
                job.finish(result);
                if let Some(d) = diag {
                    job.diagnosis = Some(d.clone());
                    let (instance, retry, handler) = (job.instance.clone(), job.retry.clone(), job.fix_handler.clone());
                    job.diag_view = Some(cx.new(|cx| crate::diagnosis::DiagnosisView::new(d, instance, retry, handler, cx)));
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn apply(&mut self, progress: Progress) {
        match progress {
            Progress::Plan { steps } => self.take_plan(steps),
            Progress::Step { name } => {
                activity(&name);
                self.steps.push(name.clone().into());
                self.step = name.clone().into();
                self.percent = None;
                self.step_started = Some(std::time::Instant::now());
                self.push_line(format!(">> {name}").into());
                self.enter(&name);
            }
            // An agent's diagnosis: kept by the worker, not a log line.
            Progress::Log { line } if line.starts_with(diagnose::DIAG_PREFIX) => {}
            Progress::Log { line } => {
                let line = crate::ui::strip_ansi(&line);
                activity(&format!("  {line}"));
                // An engine step an older agent printed as a line
                // (">> Dumping database"): it still moves the plan on.
                if let Some(name) = line.strip_prefix(">> ").filter(|_| self.planned) {
                    let name = name.to_string();
                    self.enter(&name);
                }
                self.push_line(line.into());
            }
            Progress::Percent { value } => self.percent = Some((value * 100.0).clamp(0.0, 100.0)),
            // Steps inside a job report their own completion ("SENTIENT is
            // serving…" from a deploy in the middle of a migration): the job
            // itself ends only when its work returns, with the last message.
            Progress::Done { message } => self.last_done = Some(message.into()),
            Progress::Error { message } => self.push_line(message.into()),
        }
    }

    /// A plan from the work: it replaces the steps not started yet.
    fn take_plan(&mut self, steps: Vec<PlanStep>) {
        let keep = match self.current {
            Some(i) => i + 1,
            None => 0,
        };
        self.rows.truncate(keep);
        // A step already shown (the one running) is not listed twice.
        let running: Vec<String> = self.rows.iter().map(|r| r.step.name.clone()).collect();
        self.rows.extend(steps.into_iter().filter(|s| !running.contains(&s.name)).map(Row::new));
        self.planned = !self.rows.is_empty();
    }

    /// A step started: the planned one it names (never one already
    /// passed), else a step inside the one running.
    fn enter(&mut self, name: &str) {
        let now = std::time::Instant::now();
        if !self.planned {
            // Not declared: each step is added as it starts.
            self.close_current(RowState::Done, now);
            self.rows.push(Row::new(PlanStep::new(name)));
            self.open(self.rows.len() - 1, now);
            return;
        }
        let from = self.current.unwrap_or(0);
        match (from..self.rows.len()).find(|&i| self.rows[i].step.matches(name)) {
            Some(i) if Some(i) == self.current => {
                self.rows[i].sub = (self.rows[i].step.name != name).then(|| name.to_string().into());
            }
            Some(i) => {
                self.close_current(RowState::Done, now);
                for r in &mut self.rows[from..i] {
                    if r.state == RowState::Pending {
                        r.state = RowState::Skipped;
                    }
                }
                self.open(i, now);
            }
            None => match self.current {
                Some(i) => self.rows[i].sub = Some(name.to_string().into()),
                None if !self.rows.is_empty() => {
                    self.open(0, now);
                    self.rows[0].sub = Some(name.to_string().into());
                }
                None => {}
            },
        }
    }

    fn open(&mut self, i: usize, now: std::time::Instant) {
        let r = &mut self.rows[i];
        r.state = RowState::Running;
        r.started = Some(now);
        r.sub = None;
        self.current = Some(i);
    }

    fn close_current(&mut self, state: RowState, now: std::time::Instant) {
        if let Some(i) = self.current.take() {
            let r = &mut self.rows[i];
            r.state = state;
            r.took = r.started.map(|t| now.duration_since(t));
            if state == RowState::Done {
                r.sub = None;
            }
        }
    }

    fn finish(&mut self, result: Result<(), String>) {
        match &result {
            Ok(()) => activity(&tf!("Done: {}", self.last_done.as_deref().unwrap_or("finished"))),
            Err(e) => activity(&tf!("Failed: {e}")),
        }
        let now = std::time::Instant::now();
        self.took = self.began.map(|t| now.duration_since(t));
        match &result {
            Ok(()) => {
                self.close_current(RowState::Done, now);
                for r in &mut self.rows {
                    if r.state == RowState::Pending {
                        r.state = RowState::Skipped;
                    }
                }
            }
            Err(_) => {
                if self.current.is_none() {
                    // It failed before its first step: that step failed.
                    if let Some(i) = self.rows.iter().position(|r| r.state == RowState::Pending) {
                        self.open(i, now);
                    }
                }
                self.close_current(RowState::Failed, now);
            }
        }
        self.state = match result {
            // Shown as they are: in the language shown (the job wrote English).
            Err(message) => JobState::Failed(crate::i18n::dynamic(&message).into_owned().into()),
            Ok(()) => JobState::Done(self.last_done.take().map(|m| crate::i18n::dynamic(&m).into_owned().into()).unwrap_or_else(|| t!("Finished").into())),
        };
    }
}

pub use crate::maintenance_ops::activity_log;

/// A new file for a run's whole log, beside the activity log (the last 20
/// runs are kept).
fn new_log_file(label: &str) -> Option<std::path::PathBuf> {
    let dir = activity_log().parent()?.join("jobs");
    std::fs::create_dir_all(&dir).ok()?;
    let mut old: Vec<std::path::PathBuf> = std::fs::read_dir(&dir).ok()?.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "log")).collect();
    old.sort();
    while old.len() >= 20 {
        let _ = std::fs::remove_file(old.remove(0));
    }
    let slug: String = label.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect::<String>().split('-').filter(|s| !s.is_empty()).take(6).collect::<Vec<_>>().join("-");
    Some(dir.join(format!("{}-{}.log", chrono::Local::now().format("%Y%m%d-%H%M%S%3f"), if slug.is_empty() { "job".into() } else { slug })))
}

/// A line in the Platform Manager's own log (About → Open the log).
pub fn log_line(line: &str) {
    activity(line);
}

fn activity(line: &str) {
    use std::io::Write as _;
    let path = activity_log();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1 << 20) {
        let _ = std::fs::rename(&path, path.with_extension("log.1"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{} {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), line.lines().next().unwrap_or(""));
    }
}

#[cfg(test)]
mod plan_tests {
    // Not `super::*`: gpui's own `test` attribute would shadow the standard one.
    use super::{Job, JobState, PlanStep, Progress, RowState};

    fn states(j: &Job) -> Vec<RowState> {
        j.rows().iter().map(|r| r.state).collect()
    }

    #[test]
    fn steps_follow_the_plan_inside_and_out() {
        let mut j = Job::new();
        j.apply(Progress::Plan { steps: vec![PlanStep::new("Connect"), PlanStep::new("Write").when(&["Writing"]), PlanStep::new("Copy"), PlanStep::new("Check")] });
        assert_eq!(states(&j), [RowState::Pending; 4]);
        j.apply(Progress::Step { name: "Connect".into() });
        // A step of the engine inside the one running.
        j.apply(Progress::Step { name: "Dumping database".into() });
        assert_eq!(j.rows()[0].sub.as_deref(), Some("Dumping database"));
        j.apply(Progress::Step { name: "Writing manifest".into() });
        assert_eq!(states(&j)[..2], [RowState::Done, RowState::Running]);
        j.apply(Progress::Percent { value: 0.5 });
        assert!((j.overall() - 37.5).abs() < 0.1, "{}", j.overall());
        // Copy is passed over.
        j.apply(Progress::Step { name: "Check".into() });
        assert_eq!(states(&j), [RowState::Done, RowState::Done, RowState::Skipped, RowState::Running]);
        j.finish(Err("no".into()));
        assert_eq!(states(&j)[3], RowState::Failed);
        assert!(matches!(j.state(), JobState::Failed(_)));
    }

    #[test]
    fn a_later_plan_replaces_what_did_not_start() {
        let mut j = Job::new();
        j.apply(Progress::Plan { steps: vec![PlanStep::new("Connect to the server"), PlanStep::new("Guess")] });
        j.apply(Progress::Step { name: "Connect to the server".into() });
        // The agent's own list (it may name the running step again).
        j.apply(Progress::Plan { steps: vec![PlanStep::new("A"), PlanStep::new("B")] });
        let names: Vec<&str> = j.rows().iter().map(|r| r.step.name.as_str()).collect();
        assert_eq!(names, ["Connect to the server", "A", "B"]);
        // An older agent's engine step, printed as a line.
        j.apply(Progress::Log { line: ">> A".into() });
        assert_eq!(j.rows()[1].state, RowState::Running);
        j.finish(Ok(()));
        assert_eq!(states(&j), [RowState::Done, RowState::Done, RowState::Skipped]);
        assert_eq!(j.overall(), 100.);
    }

    #[test]
    fn without_a_plan_steps_are_added_and_the_log_is_bounded() {
        let mut j = Job::new();
        j.apply(Progress::Step { name: "One".into() });
        j.apply(Progress::Step { name: "Two".into() });
        assert_eq!(states(&j), [RowState::Done, RowState::Running]);
        for i in 0..super::LOG_KEEP + 10 {
            j.apply(Progress::Log { line: format!("line {i}") });
        }
        assert_eq!(j.log().len(), super::LOG_KEEP);
        assert_eq!(j.log_total(), super::LOG_KEEP + 12);
    }
}
