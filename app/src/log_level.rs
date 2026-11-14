// Copyright © 2016-2026 The SENTIENT Authors

//! The level of a log line, for colouring the Logs screen: SENTIENT's own
//! lines (`2026-11-14T06:33:25.123Z  INFO sentient_api: …`), PostgreSQL's
//! (`2026-11-14 12:33:25.123 IST [1234] LOG:  …`, WARNING, ERROR, FATAL,
//! PANIC) and the service wrapper's (`2026-11-14 12:33:27,123 INFO  - …`).
//! Only the start of a line is looked at, so it stays cheap on long lines.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    /// No level found (a continuation line, a stack frame).
    Plain,
}

/// How far into a line the level is looked for.
const LOOK: usize = 120;

/// The level of `line` and the byte length of its leading timestamp (0 when
/// it has none).
pub fn classify(line: &str) -> (Level, usize) {
    (level(line), timestamp_len(line))
}

fn level(line: &str) -> Level {
    let head = match line.char_indices().nth(LOOK) {
        Some((i, _)) => &line[..i],
        None => line,
    };
    for word in head.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()) {
        let found = match word {
            "ERROR" | "FATAL" | "PANIC" | "CRITICAL" | "CRIT" | "SEVERE" => Level::Error,
            "WARN" | "WARNING" => Level::Warn,
            "INFO" | "LOG" | "NOTICE" => Level::Info,
            "DEBUG" | "TRACE" | "DEBUG1" | "DEBUG2" | "DEBUG3" | "DEBUG4" | "DEBUG5" => Level::Debug,
            _ => continue,
        };
        return found;
    }
    Level::Plain
}

/// The leading date and time: whitespace-separated words that start with a
/// digit, such as `2026-11-14T06:33:25.123Z` or `2026-11-14 12:33:25,123`,
/// as long as the first one looks like a date.
fn timestamp_len(line: &str) -> usize {
    let looks_like_date = line.len() >= 10
        && line.as_bytes()[..4].iter().all(u8::is_ascii_digit)
        && matches!(line.as_bytes()[4], b'-' | b'/' | b'.');
    if !looks_like_date {
        return 0;
    }
    let mut end = 0;
    let mut rest = line;
    loop {
        let trimmed = rest.trim_start();
        let skipped = rest.len() - trimmed.len();
        if !trimmed.starts_with(|c: char| c.is_ascii_digit()) {
            break;
        }
        let word = trimmed.split_whitespace().next().unwrap_or("");
        end += skipped + word.len();
        rest = &trimmed[word.len()..];
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentient_lines() {
        let l = "2026-11-14T06:33:25.123456Z  INFO sentient_api::server: listening on 0.0.0.0:8080";
        assert_eq!(classify(l), (Level::Info, "2026-11-14T06:33:25.123456Z".len()));
        assert_eq!(classify("2026-11-14T06:33:25Z  WARN ws: slow consumer").0, Level::Warn);
        assert_eq!(classify("2026-11-14T06:33:25Z ERROR dao: connection refused").0, Level::Error);
        assert_eq!(classify("2026-11-14T06:33:25Z DEBUG rule: tick").0, Level::Debug);
        assert_eq!(classify("2026-11-14T06:33:25Z TRACE mqtt: frame").0, Level::Debug);
    }

    #[test]
    fn postgresql_lines() {
        let l = "2026-11-14 12:33:25.123 IST [4312] LOG:  database system is ready to accept connections";
        assert_eq!(classify(l), (Level::Info, "2026-11-14 12:33:25.123".len()));
        assert_eq!(classify("2026-11-14 12:33:25.123 IST [4312] WARNING:  could not flush").0, Level::Warn);
        assert_eq!(classify("2026-11-14 12:33:25.123 IST [4312] FATAL:  password authentication failed").0, Level::Error);
        assert_eq!(classify("2026-11-14 12:33:25.123 IST [4312] PANIC:  could not write").0, Level::Error);
    }

    #[test]
    fn service_wrapper_lines() {
        let l = "2026-11-14 12:33:27,123 INFO  - Starting SentientServer";
        assert_eq!(classify(l), (Level::Info, "2026-11-14 12:33:27,123".len()));
        assert_eq!(classify("2026-11-14 12:33:27,123 ERROR - Failed to start").0, Level::Error);
    }

    #[test]
    fn words_must_stand_alone_and_continuations_are_plain() {
        // ERRORS, Information and a lowercase "error" in a message are not levels.
        assert_eq!(classify("ERRORS counted: 3").0, Level::Plain);
        assert_eq!(classify("   at sentient_api::main (src/main.rs:10)").0, Level::Plain);
        assert_eq!(classify("    0: <unknown>").1, 0);
        assert_eq!(classify("Information only").0, Level::Plain);
        // The first level word wins: the level, not a word in the message.
        assert_eq!(classify("2026-11-14T06:33:25Z  INFO rule: ERROR count reset").0, Level::Info);
    }

    #[test]
    fn a_level_far_into_a_long_line_is_not_looked_for() {
        let l = format!("{} ERROR", "x".repeat(200));
        assert_eq!(classify(&l).0, Level::Plain);
        // Multi-byte text before the cut does not split a character.
        let l = format!("{}ü ERROR", "é".repeat(LOOK));
        assert_eq!(classify(&l).0, Level::Plain);
    }
}
