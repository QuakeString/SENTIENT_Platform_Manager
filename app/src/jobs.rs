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
use gpui_kit::{Context, SharedString, Task};
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
    /// The latest completion message a step reported.
    last_done: Option<SharedString>,
    /// When the current step began.
    step_started: Option<std::time::Instant>,
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
            last_done: None,
            step_started: None,
            _task: None,
            _ticker: None,
        }
    }

    pub fn state(&self) -> &JobState {
        &self.state
    }

    pub fn step(&self) -> &SharedString {
        &self.step
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
        self.last_done = None;
        self.step_started = Some(std::time::Instant::now());
        cx.notify();
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
                self.step = name.into();
                self.percent = None;
                self.step_started = Some(std::time::Instant::now());
            }
            Progress::Log { line } => self.log.push(crate::ui::strip_ansi(&line).into()),
            Progress::Percent { value } => self.percent = Some((value * 100.0).clamp(0.0, 100.0)),
            // Steps inside a job report their own completion ("SENTIENT is
            // serving…" from a deploy in the middle of a migration): the job
            // itself ends only when its work returns, with the last message.
            Progress::Done { message } => self.last_done = Some(message.into()),
            Progress::Error { message } => self.log.push(message.into()),
        }
    }

    fn finish(&mut self, result: Result<(), String>) {
        self.state = match result {
            Err(message) => JobState::Failed(message.into()),
            Ok(()) => JobState::Done(self.last_done.take().unwrap_or_else(|| "Finished".into())),
        };
    }
}
