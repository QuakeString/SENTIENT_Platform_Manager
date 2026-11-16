// Copyright © 2016-2026 The SENTIENT Authors

//! The order the whole SENTIENT stack starts and stops in, the same for
//! every engine: SENTIENT first down and last up, then ClickHouse, then
//! PostgreSQL. Starting waits for each part to answer before the next.
//!
//! A native Linux install runs on the distribution's own postgresql.service,
//! which may hold other people's databases: stopping the stack keeps it
//! running then, unless the operator asked to stop it anyway.

/// One part of the stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Sentient,
    ClickHouse,
    Postgres,
}

impl Part {
    pub fn label(self) -> &'static str {
        match self {
            Part::Sentient => "SENTIENT",
            Part::ClickHouse => "ClickHouse",
            Part::Postgres => "PostgreSQL",
        }
    }
}

/// One step of a start, stop or restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Stop(Part),
    Start(Part),
    /// Wait until it answers; a part that never does fails the action.
    Ready(Part),
    /// PostgreSQL is left running, and why.
    KeepPostgres(String),
}

/// The steps of `action` (start, stop or restart). `clickhouse`: the
/// install has a ClickHouse of its own. `keep_pg`: PostgreSQL must stay
/// running, and why ([`keep_postgres`]).
pub fn plan(action: &str, clickhouse: bool, keep_pg: Option<String>) -> Result<Vec<Step>, String> {
    let mut down = vec![Step::Stop(Part::Sentient)];
    if clickhouse {
        down.push(Step::Stop(Part::ClickHouse));
    }
    down.push(match keep_pg {
        Some(why) => Step::KeepPostgres(why),
        None => Step::Stop(Part::Postgres),
    });
    let mut up = vec![Step::Start(Part::Postgres), Step::Ready(Part::Postgres)];
    if clickhouse {
        up.extend([Step::Start(Part::ClickHouse), Step::Ready(Part::ClickHouse)]);
    }
    up.extend([Step::Start(Part::Sentient), Step::Ready(Part::Sentient)]);
    match action {
        "stop" => Ok(down),
        "start" => Ok(up),
        "restart" => Ok(down.into_iter().chain(up).collect()),
        other => Err(format!("unknown action: {other}")),
    }
}

/// Whether `action` stops PostgreSQL (and so its databases need a look).
pub fn stops_postgres(action: &str) -> bool {
    matches!(action, "stop" | "restart")
}

/// The databases in `all` that are not SENTIENT's: everything but
/// PostgreSQL's own (postgres, template0/1), SENTIENT's database `own` and
/// the ones SENTIENT set aside from it (`<own>_before_install_…`, restored
/// copies `<own>_…`).
pub fn other_databases(all: &[String], own: &str) -> Vec<String> {
    let prefix = format!("{own}_");
    all.iter()
        .map(|d| d.trim())
        .filter(|d| !d.is_empty() && *d != "postgres" && !d.starts_with("template") && *d != own && !d.starts_with(&prefix))
        .map(str::to_string)
        .collect()
}

/// "PostgreSQL stays running: it also holds a, b".
pub fn stays_running_text(others: &[String]) -> String {
    format!("PostgreSQL stays running: it also holds {}", others.join(", "))
}

/// Why PostgreSQL must stay running, from the listing of its other
/// databases; None when it may stop. `force`: the operator's "Stop it
/// anyway". A listing that failed keeps it running too: nothing is known.
pub fn keep_postgres(others: &Result<Vec<String>, String>, force: bool) -> Option<String> {
    if force {
        return None;
    }
    match others {
        Ok(list) if list.is_empty() => None,
        Ok(list) => Some(format!("{}.", stays_running_text(list))),
        Err(e) => Some(format!("PostgreSQL stays running: its databases could not be listed ({e}).")),
    }
}

/// What the Docker engine runs for `action`, in order: compose sub-commands
/// over all of the project's services (compose orders them by their
/// dependencies). `external`: a stack the Platform Manager did not write,
/// whose containers are started again rather than brought up from a file.
pub fn docker_commands(action: &str, external: bool) -> Result<Vec<&'static str>, String> {
    let start = if external { "start" } else { "up -d" };
    match action {
        "start" => Ok(vec![start]),
        "stop" => Ok(vec!["stop"]),
        "restart" => Ok(vec!["stop", start]),
        other => Err(format!("unknown action: {other}")),
    }
}

/// What a finished action says: the state, and why PostgreSQL stays
/// running when it does (the tray's notification shows the same).
pub fn done_message(action: &str, steps: &[Step]) -> String {
    let state = match action {
        "stop" => "SENTIENT is stopped.",
        "start" => "SENTIENT is running.",
        _ => "SENTIENT was restarted.",
    };
    match steps.iter().find_map(|s| match s {
        Step::KeepPostgres(why) => Some(why),
        _ => None,
    }) {
        Some(why) => format!("{state} {why}"),
        None => state.to_string(),
    }
}

/// The Done message's PostgreSQL part, if any ("PostgreSQL stays running: …").
pub fn kept_postgres_note(message: &str) -> Option<&str> {
    message.find("PostgreSQL stays running").map(|i| &message[i..])
}

/// Wait up to `secs` for `ready`, once a second.
pub fn wait_until(secs: u32, mut ready: impl FnMut() -> bool) -> bool {
    for _ in 0..secs {
        if ready() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    ready()
}

/// How long SENTIENT may take to answer after it was started (a first
/// start applies its database migrations).
pub const SENTIENT_READY_SECS: u32 = 300;

#[cfg(test)]
mod tests {
    use super::*;
    use Part::*;
    use Step::*;

    #[test]
    fn stop_goes_sentient_clickhouse_postgres() {
        assert_eq!(plan("stop", true, None).unwrap(), vec![Stop(Sentient), Stop(ClickHouse), Stop(Postgres)]);
        assert_eq!(plan("stop", false, None).unwrap(), vec![Stop(Sentient), Stop(Postgres)]);
    }

    #[test]
    fn start_is_the_reverse_and_waits_for_each() {
        assert_eq!(
            plan("start", true, None).unwrap(),
            vec![Start(Postgres), Ready(Postgres), Start(ClickHouse), Ready(ClickHouse), Start(Sentient), Ready(Sentient)]
        );
        assert_eq!(plan("start", false, None).unwrap(), vec![Start(Postgres), Ready(Postgres), Start(Sentient), Ready(Sentient)]);
    }

    #[test]
    fn restart_stops_all_then_starts_all() {
        let p = plan("restart", true, None).unwrap();
        assert_eq!(&p[..3], &[Stop(Sentient), Stop(ClickHouse), Stop(Postgres)]);
        assert_eq!(&p[3..], plan("start", true, None).unwrap().as_slice());
        assert!(plan("reload", true, None).is_err());
    }

    #[test]
    fn shared_postgres_is_kept_running() {
        let all: Vec<String> = ["postgres", "template0", "template1", "sentient", "sentient_before_install_20261101", "erp", "wiki"].iter().map(|s| s.to_string()).collect();
        let others = other_databases(&all, "sentient");
        assert_eq!(others, vec!["erp".to_string(), "wiki".to_string()]);
        let keep = keep_postgres(&Ok(others.clone()), false);
        assert_eq!(keep.as_deref(), Some("PostgreSQL stays running: it also holds erp, wiki."));
        let p = plan("stop", true, keep).unwrap();
        assert_eq!(p[..2], [Stop(Sentient), Stop(ClickHouse)]);
        assert!(matches!(&p[2], KeepPostgres(why) if why.contains("erp, wiki")));
        assert!(!p.contains(&Stop(Postgres)));
        // "Stop it anyway".
        assert_eq!(keep_postgres(&Ok(others), true), None);
        // Only SENTIENT's own: it stops.
        let own = other_databases(&all[..5], "sentient");
        assert!(own.is_empty());
        assert_eq!(keep_postgres(&Ok(own), false), None);
        // Not known: kept.
        assert!(keep_postgres(&Err("no answer".into()), false).is_some());
        // A restart keeps it as well, and still starts it (a no-op when up).
        let r = plan("restart", false, Some("x".into())).unwrap();
        assert!(!r.contains(&Stop(Postgres)) && r.contains(&Start(Postgres)));
    }

    #[test]
    fn the_done_message_says_when_postgres_stays() {
        let kept = plan("stop", true, keep_postgres(&Ok(vec!["erp".into()]), false)).unwrap();
        let m = done_message("stop", &kept);
        assert_eq!(m, "SENTIENT is stopped. PostgreSQL stays running: it also holds erp.");
        assert_eq!(kept_postgres_note(&m), Some("PostgreSQL stays running: it also holds erp."));
        let m = done_message("restart", &plan("restart", false, None).unwrap());
        assert_eq!(m, "SENTIENT was restarted.");
        assert_eq!(kept_postgres_note(&m), None);
    }

    #[test]
    fn docker_acts_on_all_services() {
        assert_eq!(docker_commands("stop", false).unwrap(), vec!["stop"]);
        assert_eq!(docker_commands("start", false).unwrap(), vec!["up -d"]);
        assert_eq!(docker_commands("start", true).unwrap(), vec!["start"]);
        assert_eq!(docker_commands("restart", false).unwrap(), vec!["stop", "up -d"]);
        assert_eq!(docker_commands("restart", true).unwrap(), vec!["stop", "start"]);
        assert!(docker_commands("kill", false).is_err());
    }
}
