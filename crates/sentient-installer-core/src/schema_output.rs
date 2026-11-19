// Copyright © 2016-2026 The SENTIENT Authors

//! The schema installer's output as the job log shows it. Repair and a
//! reinstall over a kept database run `sentient-install` against a database
//! that already has SENTIENT's data; the installer then stops at the first
//! row that is already there ("duplicate key value violates unique
//! constraint \"tb_user_pkey\"") and exits non-zero. That is the expected
//! case, so its error lines become one plain line instead of a red one.
//! Every other error line is passed on as it is, and stays red.

use crate::progress::{Progress, ProgressFn};
use std::sync::{Arc, Mutex};

/// The line shown in place of the installer's "already there" error.
pub(crate) const KEPT_LINE: &str = "The database already has its data; kept as it is";

/// Whether `text` (a line, or the installer's whole error output) says
/// only that what it would create is already in the database.
pub(crate) fn is_existing_data(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("duplicate key value violates unique constraint") || lower.contains(" already exists")
}

/// What to do with one line of the installer's output.
#[derive(Debug, PartialEq, Eq)]
enum Line {
    Show(String),
    Drop,
}

/// Line-by-line state: the "already there" error spans several lines
/// (anyhow's `Error: …`, `Caused by:` and the indented cause).
#[derive(Default)]
struct Filter {
    kept: bool,
}

impl Filter {
    fn line(&mut self, line: &str) -> Line {
        if is_existing_data(line) {
            if self.kept {
                return Line::Drop;
            }
            self.kept = true;
            return Line::Show(KEPT_LINE.to_string());
        }
        let t = line.trim();
        if self.kept && (t.is_empty() || t == "Caused by:") {
            return Line::Drop;
        }
        Line::Show(line.to_string())
    }
}

/// `sink`, with the schema installer's "already there" error turned into
/// [`KEPT_LINE`]. Everything else goes through unchanged.
pub(crate) fn filter(sink: ProgressFn) -> ProgressFn {
    let state = Arc::new(Mutex::new(Filter::default()));
    Arc::new(move |p| match p {
        Progress::Log { line } => {
            let shown = state.lock().map(|mut f| f.line(&line)).unwrap_or(Line::Show(line));
            if let Line::Show(line) = shown {
                sink(Progress::Log { line });
            }
        }
        other => sink(other),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(lines: &[&str]) -> Vec<String> {
        let out = Arc::new(Mutex::new(Vec::new()));
        let o = out.clone();
        let sink: ProgressFn = Arc::new(move |p| {
            if let Progress::Log { line } = p {
                o.lock().unwrap().push(line);
            }
        });
        let f = filter(sink);
        for l in lines {
            f(Progress::Log { line: l.to_string() });
        }
        let v = out.lock().unwrap().clone();
        v
    }

    #[test]
    fn an_existing_database_is_one_plain_line() {
        let shown = run(&[
            "2026-11-18T06:00:00.000000Z  INFO Creating SYS_ADMIN user...",
            "Error: error returned from database: duplicate key value violates unique constraint \"tb_user_pkey\"",
            "",
            "Caused by:",
            "    duplicate key value violates unique constraint \"tb_user_pkey\"",
        ]);
        assert_eq!(shown, vec!["2026-11-18T06:00:00.000000Z  INFO Creating SYS_ADMIN user...", KEPT_LINE]);
        // Not red in the job view: no line starts with "error"/"Error".
        assert!(shown.iter().all(|l| !l.to_ascii_lowercase().starts_with("error")));
    }

    #[test]
    fn a_table_already_there_is_the_same_case() {
        assert_eq!(run(&["Error: error returned from database: relation \"tb_user\" already exists"]), vec![KEPT_LINE]);
    }

    #[test]
    fn real_errors_stay_as_they_are() {
        let lines = [
            "Error: error returned from database: password authentication failed for user \"sentient\"",
            "",
            "Caused by:",
            "    password authentication failed for user \"sentient\"",
        ];
        assert_eq!(run(&lines), lines.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert!(!is_existing_data("Error: pool timed out while waiting for an open connection"));
    }

    #[test]
    fn other_progress_passes_through() {
        let steps = Arc::new(Mutex::new(0));
        let s = steps.clone();
        let f = filter(Arc::new(move |p| {
            if matches!(p, Progress::Step { .. }) {
                *s.lock().unwrap() += 1;
            }
        }));
        f(Progress::Step { name: "Starting SENTIENT".into() });
        assert_eq!(*steps.lock().unwrap(), 1);
    }
}
