// Copyright © 2016-2026 The SENTIENT Authors

//! Where each part of SENTIENT runs: the System check's two cards ("In the
//! system" and "Docker"), their "Customise where each part runs" rows, the
//! Database step's "runs" texts, and what a choice makes of the install
//! plan (`InstallPlan.engine`). Plain data, so the choice → plan mapping is
//! tested without a window (docs/mockups/setup-engine.html, v4).
//!
//! In this version every part runs where the chosen card puts it: a mix
//! (the server in one place, the databases in another) comes in 0.9.3, so
//! the Customise rows mirror the card and every choice that would mix is
//! disabled. One mix exists today: on Windows, "In the system" puts
//! ClickHouse in Docker inside WSL 2 (there is no ClickHouse for Windows).

use sentient_installer_core::stack::Flavor;

use crate::installation::Engine;

/// Where one part runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// As a service of the operating system (Windows services, systemd).
    System,
    /// As a container (Docker on Linux, Docker Engine inside WSL 2 on Windows).
    Docker,
}

/// The parts with a place of their own. TimescaleDB is an extension inside
/// PostgreSQL and always runs where PostgreSQL runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Server,
    Postgres,
    Clickhouse,
}

pub const PARTS: [Part; 3] = [Part::Server, Part::Postgres, Part::Clickhouse];

/// Where each part runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub server: Place,
    pub pg: Place,
    pub ch: Place,
}

/// Whether WSL 2 can run Docker here (Windows; from
/// sentient_installer_core::wsl::DockerChoice).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wsl {
    Ready,
    /// Not on yet: Setup turns it on, Windows restarts once.
    TurnOn,
    /// It cannot run here (virtualization off): no Docker, no ClickHouse.
    Unavailable,
}

/// The computer the choice is made for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Host {
    pub windows: bool,
    /// Windows only; ignored on Linux.
    pub wsl: Wsl,
}

/// Why a choice can't be made (its (i) says it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// It would mix the server and the databases' places: 0.9.3.
    Later,
    /// ClickHouse "In the system" on Windows.
    NoWindowsClickhouse,
    /// Docker (or ClickHouse) on a Windows where WSL 2 cannot run.
    NeedsWsl,
    /// TimescaleDB with PostgreSQL from a Linux distribution's packages.
    NoDistroTimescale,
}

impl Why {
    /// One line, for the footer when Next is refused.
    pub fn short(self) -> &'static str {
        match self {
            Why::Later => t!("Running the server and the databases in different places comes in a later version."),
            Why::NoWindowsClickhouse => t!("ClickHouse has no Windows build: it runs in Docker inside WSL 2 only."),
            Why::NeedsWsl => t!("Docker and ClickHouse need WSL 2, and WSL 2 cannot run on this computer."),
            Why::NoDistroTimescale => t!("Linux distributions do not package TimescaleDB. Choose Docker on System check to use it."),
        }
    }

    /// The (i)'s text. `wsl_detail`: why WSL 2 cannot run, as read
    /// (wsl::DockerChoice::Unavailable).
    pub fn text(self, wsl_detail: &str) -> String {
        match self {
            Why::Later => t!("Coming in a later version\n\nIn this version every part runs where the card above puts it. Running the server and the databases in different places comes later.").into(),
            Why::NoWindowsClickhouse => t!("Not on Windows\n\nClickHouse has no Windows build. It runs in Docker inside WSL 2 only.").into(),
            Why::NeedsWsl => tf!("Needs WSL 2\n\nDocker and ClickHouse run inside WSL 2, and WSL 2 cannot run on this computer. {wsl_detail}").trim_end().to_string(),
            Why::NoDistroTimescale => t!("Not with systemd\n\nLinux distributions do not package TimescaleDB. Choose Docker on System check to use it.").into(),
        }
    }
}

/// What Setup starts with: "In the system", on Windows and on Linux.
pub fn default_engine() -> Engine {
    Engine::Native
}

impl Placement {
    /// Where each part runs for a card. Windows' "In the system" keeps
    /// ClickHouse in Docker inside WSL 2.
    pub fn preset(engine: Engine, windows: bool) -> Self {
        match engine {
            Engine::Docker => Placement { server: Place::Docker, pg: Place::Docker, ch: Place::Docker },
            Engine::Native => Placement { server: Place::System, pg: Place::System, ch: if windows { Place::Docker } else { Place::System } },
        }
    }

    pub fn get(self, part: Part) -> Place {
        match part {
            Part::Server => self.server,
            Part::Postgres => self.pg,
            Part::Clickhouse => self.ch,
        }
    }

    pub fn with(mut self, part: Part, place: Place) -> Self {
        match part {
            Part::Server => self.server = place,
            Part::Postgres => self.pg = place,
            Part::Clickhouse => self.ch = place,
        }
        self
    }

    /// The engine that installs this placement, or why this version can't.
    pub fn engine(self, windows: bool) -> Result<Engine, Why> {
        use Place::*;
        if windows && self.ch == System {
            return Err(Why::NoWindowsClickhouse);
        }
        match (self.server, self.pg, self.ch) {
            (Docker, Docker, Docker) => Ok(Engine::Docker),
            (System, System, Docker) if windows => Ok(Engine::Native),
            (System, System, System) => Ok(Engine::Native),
            _ => Err(Why::Later),
        }
    }
}

/// Whether a Customise segment can be picked: `part` to `place`, starting
/// from `current`. Any choice that makes a mix is refused (0.9.3).
pub fn segment(host: Host, current: Placement, part: Part, place: Place) -> Result<Engine, Why> {
    if host.windows && part == Part::Clickhouse && place == Place::System {
        return Err(Why::NoWindowsClickhouse);
    }
    if host.windows && place == Place::Docker && host.wsl == Wsl::Unavailable {
        return Err(Why::NeedsWsl);
    }
    current.with(part, place).engine(host.windows)
}

/// Where a database runs, as its Database card says it (one line, or one
/// per database when they differ), or why it can't be chosen.
pub fn flavor_runs(host: Host, engine: Engine, flavor: Flavor) -> Result<Vec<&'static str>, Why> {
    let no_wsl = host.windows && host.wsl == Wsl::Unavailable;
    match (host.windows, engine, flavor) {
        (true, Engine::Docker, _) if no_wsl => Err(Why::NeedsWsl),
        (true, Engine::Docker, _) => Ok(vec![t!("Docker (WSL 2)")]),
        (true, Engine::Native, Flavor::Clickhouse) if no_wsl => Err(Why::NeedsWsl),
        (true, Engine::Native, Flavor::Clickhouse) => Ok(vec![t!("PostgreSQL: in the system"), t!("ClickHouse: Docker (WSL 2)")]),
        (true, Engine::Native, _) => Ok(vec![t!("In the system")]),
        (false, Engine::Docker, _) => Ok(vec![t!("Docker")]),
        // sentient_installer_core::native_linux refuses it: no distribution
        // packages TimescaleDB.
        (false, Engine::Native, Flavor::Timescaledb) => Err(Why::NoDistroTimescale),
        (false, Engine::Native, Flavor::Clickhouse) => Ok(vec![t!("PostgreSQL: in the system"), t!("ClickHouse: in the system")]),
        (false, Engine::Native, Flavor::Stock) => Ok(vec![t!("In the system")]),
    }
}

/// The install plan's engine for a placement and a database, or why it is
/// refused. The wizard builds `InstallPlan.engine` from this.
pub fn plan_engine(host: Host, placement: Placement, flavor: Flavor) -> Result<Engine, Why> {
    let engine = placement.engine(host.windows)?;
    if engine == Engine::Docker && host.windows && host.wsl == Wsl::Unavailable {
        return Err(Why::NeedsWsl);
    }
    flavor_runs(host, engine, flavor)?;
    Ok(engine)
}

/// The Summary's "Runs as" line.
pub fn runs_as(host: Host, engine: Engine, flavor: Option<Flavor>) -> String {
    let ch = flavor == Some(Flavor::Clickhouse);
    match (host.windows, engine) {
        (true, Engine::Docker) if host.wsl == Wsl::TurnOn => t!("SENTIENT server and its databases in Docker inside WSL 2 (Setup turns WSL 2 on; Windows restarts once)").into(),
        (true, Engine::Docker) => t!("SENTIENT server and its databases in Docker inside WSL 2").into(),
        (true, Engine::Native) if ch => t!("SENTIENT server and PostgreSQL as Windows services; ClickHouse in Docker inside WSL 2").into(),
        (true, Engine::Native) => t!("SENTIENT server and PostgreSQL as Windows services").into(),
        (false, Engine::Docker) => t!("SENTIENT server and its databases in Docker").into(),
        (false, Engine::Native) if ch => t!("SENTIENT server, PostgreSQL and ClickHouse with systemd").into(),
        (false, Engine::Native) => t!("SENTIENT server and PostgreSQL with systemd").into(),
    }
}

/// Where ClickHouse runs, after its Summary line and as its Database
/// settings heading's note.
pub fn clickhouse_place(host: Host, engine: Engine) -> Option<&'static str> {
    match (host.windows, engine) {
        (true, _) => Some(t!("Docker in WSL 2")),
        (false, Engine::Docker) => Some(t!("Docker")),
        (false, Engine::Native) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Place::*;

    const WIN: Host = Host { windows: true, wsl: Wsl::Ready };
    const WIN_NO_WSL: Host = Host { windows: true, wsl: Wsl::Unavailable };
    const LINUX: Host = Host { windows: false, wsl: Wsl::Unavailable };
    const FLAVORS: [Flavor; 3] = [Flavor::Stock, Flavor::Timescaledb, Flavor::Clickhouse];

    fn all(p: Place) -> Placement {
        Placement { server: p, pg: p, ch: p }
    }

    /// Every placement there is: 2 × 2 × 2.
    fn every() -> Vec<Placement> {
        let mut v = Vec::new();
        for server in [System, Docker] {
            for pg in [System, Docker] {
                for ch in [System, Docker] {
                    v.push(Placement { server, pg, ch });
                }
            }
        }
        v
    }

    #[test]
    fn the_default_is_in_the_system_on_both() {
        assert_eq!(default_engine(), Engine::Native);
        for host in [WIN, WIN_NO_WSL, LINUX] {
            for f in [Flavor::Stock, Flavor::Clickhouse] {
                let preset = Placement::preset(default_engine(), host.windows);
                if host == WIN_NO_WSL && f == Flavor::Clickhouse {
                    continue;
                }
                assert_eq!(plan_engine(host, preset, f), Ok(Engine::Native), "{host:?} {f:?}");
            }
        }
    }

    #[test]
    fn windows_in_the_system_is_native_with_every_database() {
        let p = Placement::preset(Engine::Native, true);
        assert_eq!(p, Placement { server: System, pg: System, ch: Docker });
        for f in FLAVORS {
            assert_eq!(plan_engine(WIN, p, f), Ok(Engine::Native));
        }
        assert_eq!(flavor_runs(WIN, Engine::Native, Flavor::Stock), Ok(vec!["In the system"]));
        assert_eq!(flavor_runs(WIN, Engine::Native, Flavor::Timescaledb), Ok(vec!["In the system"]));
    }

    #[test]
    fn windows_docker_is_the_docker_engine_in_wsl() {
        let p = Placement::preset(Engine::Docker, true);
        assert_eq!(p, all(Docker));
        for f in FLAVORS {
            assert_eq!(plan_engine(WIN, p, f), Ok(Engine::Docker));
            assert_eq!(plan_engine(Host { wsl: Wsl::TurnOn, ..WIN }, p, f), Ok(Engine::Docker));
            assert_eq!(flavor_runs(WIN, Engine::Docker, f), Ok(vec!["Docker (WSL 2)"]));
        }
    }

    #[test]
    fn the_windows_hybrid_is_native_with_clickhouse_in_wsl() {
        let p = Placement::preset(Engine::Native, true);
        assert_eq!(plan_engine(WIN, p, Flavor::Clickhouse), Ok(Engine::Native));
        assert_eq!(flavor_runs(WIN, Engine::Native, Flavor::Clickhouse), Ok(vec!["PostgreSQL: in the system", "ClickHouse: Docker (WSL 2)"]));
        // Its Customise row: Docker, and "In the system" off with its reason.
        assert_eq!(segment(WIN, p, Part::Clickhouse, Docker), Ok(Engine::Native));
        assert_eq!(segment(WIN, p, Part::Clickhouse, System), Err(Why::NoWindowsClickhouse));
        assert_eq!(clickhouse_place(WIN, Engine::Native), Some("Docker in WSL 2"));
    }

    #[test]
    fn windows_without_wsl_offers_neither_docker_nor_clickhouse() {
        assert_eq!(plan_engine(WIN_NO_WSL, all(Docker), Flavor::Stock), Err(Why::NeedsWsl));
        assert_eq!(plan_engine(WIN_NO_WSL, Placement::preset(Engine::Native, true), Flavor::Clickhouse), Err(Why::NeedsWsl));
        assert_eq!(plan_engine(WIN_NO_WSL, Placement::preset(Engine::Native, true), Flavor::Timescaledb), Ok(Engine::Native));
        let p = Placement::preset(Engine::Native, true);
        for part in PARTS {
            assert_eq!(segment(WIN_NO_WSL, p, part, Docker), Err(Why::NeedsWsl));
        }
    }

    #[test]
    fn linux_in_the_system_with_timescaledb_is_refused() {
        let p = Placement::preset(Engine::Native, false);
        assert_eq!(p, all(System));
        assert_eq!(plan_engine(LINUX, p, Flavor::Timescaledb), Err(Why::NoDistroTimescale));
        assert_eq!(flavor_runs(LINUX, Engine::Native, Flavor::Timescaledb), Err(Why::NoDistroTimescale));
        assert!(Why::NoDistroTimescale.text("").contains("Choose Docker on System check"));
        // The rest is fine: in the system, or Docker for TimescaleDB.
        assert_eq!(plan_engine(LINUX, p, Flavor::Stock), Ok(Engine::Native));
        assert_eq!(plan_engine(LINUX, p, Flavor::Clickhouse), Ok(Engine::Native));
        assert_eq!(plan_engine(LINUX, all(Docker), Flavor::Timescaledb), Ok(Engine::Docker));
    }

    #[test]
    fn no_mix_can_be_produced() {
        for host in [WIN, LINUX] {
            let supported: Vec<Placement> = [Engine::Native, Engine::Docker].map(|e| Placement::preset(e, host.windows)).to_vec();
            for p in every() {
                for f in FLAVORS {
                    match plan_engine(host, p, f) {
                        Ok(e) => assert_eq!(Placement::preset(e, host.windows), p, "{host:?} {p:?} {f:?} made a plan"),
                        Err(_) => {}
                    }
                }
                if !supported.contains(&p) {
                    assert!(p.engine(host.windows).is_err(), "{host:?} {p:?}");
                }
            }
            // From either card, every segment either keeps that card's
            // placement or is refused: the rows mirror the card.
            for start in &supported {
                for part in PARTS {
                    for place in [System, Docker] {
                        if let Ok(e) = segment(host, *start, part, place) {
                            assert_eq!(Placement::preset(e, host.windows), *start, "{host:?} {part:?} {place:?}");
                        }
                    }
                }
            }
        }
        // The two mixed frames of the mockup (W5, L3) are 0.9.3.
        assert_eq!(segment(WIN, Placement::preset(Engine::Native, true), Part::Postgres, Docker), Err(Why::Later));
        assert_eq!(segment(LINUX, all(System), Part::Clickhouse, Docker), Err(Why::Later));
        assert!(Why::Later.text("").starts_with("Coming in a later version"));
    }

    #[test]
    fn the_summary_says_where_each_part_runs() {
        assert_eq!(runs_as(WIN, Engine::Native, Some(Flavor::Stock)), "SENTIENT server and PostgreSQL as Windows services");
        assert!(runs_as(WIN, Engine::Native, Some(Flavor::Clickhouse)).ends_with("ClickHouse in Docker inside WSL 2"));
        assert!(runs_as(Host { wsl: Wsl::TurnOn, ..WIN }, Engine::Docker, None).contains("Windows restarts once"));
        assert_eq!(runs_as(LINUX, Engine::Native, Some(Flavor::Clickhouse)), "SENTIENT server, PostgreSQL and ClickHouse with systemd");
        assert_eq!(runs_as(LINUX, Engine::Docker, None), "SENTIENT server and its databases in Docker");
        assert_eq!(clickhouse_place(LINUX, Engine::Native), None);
    }
}
