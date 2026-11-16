//! Progress reporting for provisioning steps — the engine calls a sink; the GUI
//! streams it into the window (app/src/jobs.rs), the CLI prints it.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Progress {
    /// Every step the job will take, in order, sent before the first one
    /// starts: the job view shows them all (pending) from its first frame.
    /// A later plan replaces the steps not started yet (a remote job's own
    /// list replaces the guess made before the agent answered). Older
    /// agents never send it; their steps then show as they start.
    Plan { steps: Vec<PlanStep> },
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

/// One step of a job's plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanStep {
    /// What it does ("Writing the backup"); a `Step` with this name starts it.
    pub name: String,
    /// One line under it ("PostgreSQL and ClickHouse, compressed").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// The starts of other step names that mean this step (the engine's own
    /// words: "Copying the database" for "Copying the database (12 MB) to …").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub when: Vec<String>,
    /// Its share of the whole job's bar (1 = an ordinary step).
    #[serde(default = "one")]
    pub weight: f32,
}

fn one() -> f32 {
    1.0
}

impl PlanStep {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), detail: String::new(), when: Vec::new(), weight: 1.0 }
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    /// Step names starting with any of `starts` mean this step too.
    pub fn when(mut self, starts: &[&str]) -> Self {
        self.when = starts.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn weight(mut self, weight: f32) -> Self {
        self.weight = weight;
        self
    }

    /// Whether the started step `name` is this one.
    pub fn matches(&self, name: &str) -> bool {
        name == self.name || self.when.iter().any(|w| !w.is_empty() && name.starts_with(w.as_str()))
    }
}

pub type ProgressFn = std::sync::Arc<dyn Fn(Progress) + Send + Sync>;

/// A sink that does nothing.
pub fn noop() -> ProgressFn {
    std::sync::Arc::new(|_| {})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plan_survives_the_wire_and_old_lines_still_read() {
        let p = Progress::Plan { steps: vec![PlanStep::new("Writing the backup").detail("all of it").when(&["Writing"])] };
        let line = serde_json::to_string(&p).unwrap();
        match serde_json::from_str::<Progress>(&line).unwrap() {
            Progress::Plan { steps } => {
                assert_eq!(steps[0].detail, "all of it");
                assert_eq!(steps[0].weight, 1.0);
                assert!(steps[0].matches("Writing it"));
            }
            _ => panic!("not a plan"),
        }
        // A step without detail, when or weight (the smallest form).
        let p: Progress = serde_json::from_str(r#"{"type":"plan","steps":[{"name":"A"}]}"#).unwrap();
        assert!(matches!(p, Progress::Plan { steps } if steps[0].name == "A" && steps[0].weight == 1.0));
        assert!(serde_json::from_str::<Progress>(r#"{"type":"step","name":"B"}"#).is_ok());
    }
}
