//! Progress reporting for provisioning steps — the engine calls a sink; the GUI
//! forwards it to the webview over a Tauri Channel, the CLI prints it.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Progress {
    /// A new high-level step started.
    Step { name: String },
    /// A line of tool output.
    Log { line: String },
    /// Fraction complete of the current step, 0.0..=1.0 (e.g. a download).
    Percent { value: f32 },
    /// Finished successfully.
    Done { message: String },
    /// Failed.
    Error { message: String },
}

pub type ProgressFn = std::sync::Arc<dyn Fn(Progress) + Send + Sync>;

/// A sink that does nothing.
pub fn noop() -> ProgressFn {
    std::sync::Arc::new(|_| {})
}
