//! Progress reporting shared by the backup/restore engines. The engine calls a
//! sink; the CLI prints it, the GUI forwards it to the webview as Tauri events.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Progress {
    /// A new high-level step started (e.g. "Dumping database").
    Step { name: String, index: u32, total: u32 },
    /// A line of tool output / info.
    Log { line: String },
    /// Fraction of the current step, 0.0..=1.0 (best-effort).
    Percent { value: f32 },
    /// Finished successfully.
    Done { message: String },
}

/// A thread-safe progress sink.
pub type ProgressFn = std::sync::Arc<dyn Fn(Progress) + Send + Sync>;

/// A sink that does nothing.
pub fn noop() -> ProgressFn {
    std::sync::Arc::new(|_| {})
}

/// Small helper to emit sequential steps.
pub struct Steps {
    sink: ProgressFn,
    total: u32,
    i: u32,
}

impl Steps {
    pub fn new(sink: ProgressFn, total: u32) -> Self {
        Self { sink, total, i: 0 }
    }
    pub fn step(&mut self, name: impl Into<String>) {
        self.i += 1;
        (self.sink)(Progress::Step {
            name: name.into(),
            index: self.i,
            total: self.total,
        });
    }
    pub fn log(&self, line: impl Into<String>) {
        (self.sink)(Progress::Log { line: line.into() });
    }
    pub fn done(&self, message: impl Into<String>) {
        (self.sink)(Progress::Done {
            message: message.into(),
        });
    }
}
