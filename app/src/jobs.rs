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
use gpui_kit::{App, Context, SharedString, Task};
use sentient_installer_core::progress::{Progress, ProgressFn};

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
    log: Vec<SharedString>,
    /// Every step's name, in the order they started.
    steps: Vec<SharedString>,
    /// The latest completion message a step reported.
    last_done: Option<SharedString>,
    /// When the current step began.
    step_started: Option<std::time::Instant>,
    /// What the global progress strip calls it ("Stopping ClickHouse on
    /// ST-DEV"); None: its current step.
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
            log: Vec::new(),
            steps: Vec::new(),
            last_done: None,
            step_started: None,
            label: None,
            details: None,
            retry: None,
            instance: None,
            on_page: false,
            _task: None,
            _ticker: None,
        }
    }

    /// Name the job for the progress strip (before `start`).
    pub fn set_label(&mut self, label: impl Into<SharedString>) {
        self.label = Some(label.into());
    }

    /// Shown on its page (see `on_page`).
    pub fn set_on_page(&mut self, on: bool) {
        self.on_page = on;
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

    pub fn log(&self) -> &[SharedString] {
        &self.log
    }

    /// How long the current step has been running.
    pub fn step_elapsed(&self) -> Option<std::time::Duration> {
        self.step_started.filter(|_| self.is_running()).map(|t| t.elapsed())
    }

    /// "Installing the SENTIENT schema · 1:24" while running.
    pub fn step_with_time(&self) -> SharedString {
        match self.step_elapsed() {
            Some(d) if !self.step.is_empty() => format!("{} · {}:{:02}", self.step, d.as_secs() / 60, d.as_secs() % 60).into(),
            _ => self.step.clone(),
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
        self.steps.clear();
        self.last_done = None;
        self.step_started = Some(std::time::Instant::now());
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
        let sink: ProgressFn = std::sync::Arc::new(move |p| {
            let _ = tx.unbounded_send(p);
        });
        let worker = cx.background_executor().spawn(async move { work(sink) });
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
            let result = worker.await;
            this.update(cx, |job, cx| {
                job.finish(result);
                cx.notify();
            })
            .ok();
        }));
    }

    fn apply(&mut self, progress: Progress) {
        match progress {
            Progress::Step { name } => {
                activity(&name);
                self.steps.push(name.clone().into());
                self.step = name.into();
                self.percent = None;
                self.step_started = Some(std::time::Instant::now());
            }
            Progress::Log { line } => {
                let line = crate::ui::strip_ansi(&line);
                activity(&format!("  {line}"));
                self.log.push(line.into());
            }
            Progress::Percent { value } => self.percent = Some((value * 100.0).clamp(0.0, 100.0)),
            // Steps inside a job report their own completion ("SENTIENT is
            // serving…" from a deploy in the middle of a migration): the job
            // itself ends only when its work returns, with the last message.
            Progress::Done { message } => self.last_done = Some(message.into()),
            Progress::Error { message } => self.log.push(message.into()),
        }
    }

    fn finish(&mut self, result: Result<(), String>) {
        match &result {
            Ok(()) => activity(&format!("Done: {}", self.last_done.as_deref().unwrap_or("finished"))),
            Err(e) => activity(&format!("Failed: {e}")),
        }
        self.state = match result {
            Err(message) => JobState::Failed(message.into()),
            Ok(()) => JobState::Done(self.last_done.take().unwrap_or_else(|| "Finished".into())),
        };
    }
}

pub use crate::maintenance_ops::activity_log;

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
