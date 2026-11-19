// Copyright © 2016-2026 The SENTIENT Authors

//! Uninstall as a choice of parts: SENTIENT, its PostgreSQL and ClickHouse
//! (each with "Keep its data"), the Gateway, the shortcuts, on Windows the
//! SENTIENT WSL distribution and WSL itself, and the Platform Manager last.
//!
//! [`offers`] says which parts this computer has and which can be picked
//! (a shared PostgreSQL cannot; a database that was there before SENTIENT is
//! only stopped being used; WSL only when Setup turned it on and nothing
//! else uses it). [`run`] removes what was picked for real and then checks
//! that nothing of it is left (sentient_installer_core::removal), writing
//! one result line per check into the job's output for the dialog
//! (crate::manager::uninstall_dialog) and the terminal (`--job uninstall`).
//!
//! What Setup itself put on the computer is recorded at install time
//! ([`note_before_install`], InstallationRecord::installed); records from
//! before that are "not recorded".

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use sentient_installer_core::progress::{Progress, ProgressFn};
use sentient_installer_core::removal::{self, Check, DbRemoval, Expect, Pm};
use serde::{Deserialize, Serialize};

use crate::installation::{Engine, InstallationRecord};

// ---------------------------------------------------------------------------
// What Setup installed (recorded at install time)
// ---------------------------------------------------------------------------

/// What Setup put on this computer itself, as opposed to what it found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    /// PostgreSQL (the distribution's package on Linux, the bundled
    /// binaries on Windows, the container under Docker).
    pub postgres: bool,
    pub clickhouse: bool,
    /// Setup turned WSL on (Windows, for ClickHouse).
    #[serde(default)]
    pub wsl: bool,
}

fn installed_marker() -> PathBuf {
    crate::installation::record_path().with_file_name("install-found.json")
}

/// Before an install changes anything: note whether PostgreSQL, ClickHouse
/// and WSL are already here. Kept in a file beside the record, so a Setup
/// that continues after the restart WSL needs keeps what it found first.
pub fn note_before_install(engine: Engine, flavor: sentient_installer_core::stack::Flavor) {
    let found = probe_before(engine, flavor);
    let earlier: Option<Installed> = std::fs::read_to_string(installed_marker()).ok().and_then(|t| serde_json::from_str(&t).ok());
    let noted = match earlier {
        // Setup's own WSL install, then the restart: WSL is there now
        // because of it.
        Some(e) => Installed { postgres: e.postgres && found.postgres, clickhouse: e.clickhouse && found.clickhouse, wsl: e.wsl || found.wsl },
        None => found,
    };
    if let Some(dir) = installed_marker().parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(installed_marker(), serde_json::to_string(&noted).unwrap_or_default());
}

/// What [`note_before_install`] found, for the record (and the marker goes).
pub fn noted() -> Option<Installed> {
    let n = std::fs::read_to_string(installed_marker()).ok().and_then(|t| serde_json::from_str(&t).ok());
    let _ = std::fs::remove_file(installed_marker());
    n
}

/// Installed by Setup = not found before it.
fn probe_before(engine: Engine, flavor: sentient_installer_core::stack::Flavor) -> Installed {
    let mut i = Installed { postgres: true, clickhouse: true, wsl: false };
    if cfg!(target_os = "linux") && engine == Engine::Native {
        let sh = |c: &str| sentient_installer_core::host::host_command("sh").arg("-c").arg(c).output().map(|o| o.status.success()).unwrap_or(false);
        i.postgres = !sh("command -v postgres || command -v pg_ctl || [ -x /usr/bin/postgres ] || systemctl cat postgresql.service >/dev/null 2>&1");
        i.clickhouse = !flavor.uses_clickhouse() || !sh("command -v clickhouse-server || systemctl cat clickhouse-server.service >/dev/null 2>&1");
    }
    if cfg!(windows) && engine == Engine::Native && flavor.uses_clickhouse() {
        i.wsl = !sentient_installer_core::wsl::is_ready();
    }
    i
}

// ---------------------------------------------------------------------------
// The rows of the dialog
// ---------------------------------------------------------------------------

/// One switch of the dialog: a part, or a "Keep …" switch nested under one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Key {
    Sentient,
    /// SENTIENT's file stores and backups (nested under SENTIENT).
    KeepFiles,
    Postgres,
    KeepPgData,
    /// Only SENTIENT's database and role in a PostgreSQL Setup did not install.
    PgDatabase,
    Clickhouse,
    KeepChData,
    ChDatabase,
    /// Windows: the WSL distribution ClickHouse runs in (nested under ClickHouse).
    WslDistro,
    Wsl,
    Gateway,
    Shortcuts,
    /// Docker itself: shown, never removed.
    Docker,
    /// The Platform Manager (on a server: its agent), removed last.
    Manager,
}

impl Key {
    pub const PARTS: [Key; 11] = [Key::Sentient, Key::Postgres, Key::PgDatabase, Key::Clickhouse, Key::ChDatabase, Key::WslDistro, Key::Wsl, Key::Gateway, Key::Shortcuts, Key::Manager, Key::Docker];

    /// Its name in `--parts`.
    pub fn id(self) -> &'static str {
        match self {
            Key::Sentient => "sentient",
            Key::KeepFiles => "keep-files",
            Key::Postgres => "postgres",
            Key::KeepPgData => "keep-pg-data",
            Key::PgDatabase => "pg-database",
            Key::Clickhouse => "clickhouse",
            Key::KeepChData => "keep-ch-data",
            Key::ChDatabase => "ch-database",
            Key::WslDistro => "wsl-distro",
            Key::Wsl => "wsl",
            Key::Gateway => "gateway",
            Key::Shortcuts => "shortcuts",
            Key::Docker => "docker",
            Key::Manager => "manager",
        }
    }

    pub fn is_keep(self) -> bool {
        matches!(self, Key::KeepFiles | Key::KeepPgData | Key::KeepChData)
    }
}

/// Who put a database (or WSL) on this computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    Setup,
    Before,
    /// An install from before this was recorded.
    Unknown,
}

/// What this computer has, as the dialog needs it. Serialised for a
/// server's dialog (`--job uninstall-facts-json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Facts {
    pub windows: bool,
    pub engine: Engine,
    /// TimescaleDB is in the PostgreSQL.
    #[serde(default)]
    pub timescale: bool,
    /// ClickHouse is part of the installation (its flavour, or one a
    /// migration left set up).
    pub has_clickhouse: bool,
    /// ClickHouse runs in WSL (Windows native).
    pub ch_in_wsl: bool,
    pub pg_origin: Origin,
    pub ch_origin: Origin,
    pub wsl_origin: Origin,
    /// The other databases in PostgreSQL: None while being listed.
    pub pg_others: Option<Result<Vec<String>, String>>,
    pub gateway: bool,
    /// Other WSL distributions of this account.
    pub other_distros: Vec<String>,
    pub sizes: Sizes,
    /// What the rows name.
    pub names: Names,
}

/// Bytes on disk, where known.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Sizes {
    pub sentient: Option<u64>,
    pub files: Option<u64>,
    pub pg_program: Option<u64>,
    pub pg_data: Option<u64>,
    pub ch_program: Option<u64>,
    pub ch_data: Option<u64>,
    pub distro: Option<u64>,
    pub manager: Option<u64>,
}

/// Names, folders and ports the rows show.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Names {
    pub install_dir: String,
    pub files: String,
    pub pg_data: String,
    pub db_name: String,
    pub pg_port: u16,
    pub ch_ports: (u16, u16),
    pub app_container: String,
    pub image: String,
    pub vol_data: String,
    pub vol_pg: String,
    pub vol_ch: String,
}

/// One row of the dialog.
#[derive(Debug, Clone)]
pub struct Row {
    pub key: Key,
    /// A nested switch: on only while its parent is.
    pub parent: Option<Key>,
    pub title: String,
    pub detail: String,
    /// Why it cannot be picked (shown in the warning colour); always off.
    pub why: Option<String>,
    /// One more plain line.
    pub extra: Option<String>,
    /// Behind the (i).
    pub help: Option<String>,
    pub bytes: Option<u64>,
    pub default_on: bool,
    /// In the summary.
    pub name: String,
}

/// What was picked: every switch that is on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Choice {
    pub on: BTreeSet<Key>,
}

impl Choice {
    pub fn has(&self, k: Key) -> bool {
        self.on.contains(&k)
    }
    pub fn set(&mut self, k: Key, on: bool) {
        if on {
            self.on.insert(k);
        } else {
            self.on.remove(&k);
        }
    }
}

const NOT_BY_SETUP: &str = n!("not installed by Setup");

/// The dialog's rows for this computer (or server), in order.
pub fn rows(f: &Facts) -> Vec<Row> {
    let n = &f.names;
    let row = |key: Key, parent: Option<Key>, title: &str, detail: String, bytes: Option<u64>, default_on: bool, name: &str| Row {
        key,
        parent,
        title: title.into(),
        detail,
        why: None,
        extra: None,
        help: None,
        bytes,
        default_on,
        name: name.into(),
    };
    let docker = f.engine == Engine::Docker;
    let mut out = Vec::new();
    let sentient = if docker {
        row(Key::Sentient, None, t!("SENTIENT container"), tf!("Container {} and image {}", n.app_container, n.image), f.sizes.sentient, true, t!("SENTIENT container"))
    } else {
        let service = if f.windows { "SentientServer" } else { "sentient" };
        row(Key::Sentient, None, t!("SENTIENT server"), tf!("Service {service}, {}", n.install_dir), f.sizes.sentient, true, t!("SENTIENT server"))
    };
    out.push(sentient);
    let files = if docker {
        row(Key::KeepFiles, Some(Key::Sentient), &tf!("Keep volume {}", n.vol_data), t!("File stores and backups").into(), f.sizes.files, true, &tf!("volume {}", n.vol_data))
    } else {
        row(Key::KeepFiles, Some(Key::Sentient), t!("Keep file stores and backups"), n.files.clone(), f.sizes.files, true, t!("file stores and backups"))
    };
    if docker {
        out.push(files.clone());
    }

    // PostgreSQL.
    let pg_title = if f.timescale { t!("PostgreSQL and TimescaleDB") } else { "PostgreSQL" };
    // Stopped: unknown, so off unless switched on (below), never locked.
    let unknown = pg_unknown(f);
    let shared = match &f.pg_others {
        None => Some(t!("Checking what else PostgreSQL holds…").to_string()),
        Some(Err(_)) if unknown => None,
        Some(Ok(list)) if !list.is_empty() => Some(tf!("It also holds {}: another program uses it, so it stays.", and_list(list))),
        Some(Err(e)) => Some(tf!("What else it holds could not be checked, so it stays ({e}).")),
        _ => None,
    };
    let by_setup = f.pg_origin != Origin::Before;
    let mut pg = if docker {
        row(Key::Postgres, None, t!("PostgreSQL container"), t!("Container and its image").into(), f.sizes.pg_program, true, t!("PostgreSQL container"))
    } else if f.windows {
        row(Key::Postgres, None, pg_title, tf!("Service {} and its program folder; port {}", sentient_installer_core::native::PG_SERVICE, n.pg_port), f.sizes.pg_program, true, pg_title)
    } else {
        let what = if f.timescale { t!("Both packages and the service") } else { t!("Package and service") };
        row(Key::Postgres, None, pg_title, tf!("{what}; port {}", n.pg_port), f.sizes.pg_program, true, pg_title)
    };
    if !by_setup {
        pg.detail = NOT_BY_SETUP[..1].to_uppercase() + &NOT_BY_SETUP[1..];
        pg.why = Some(t!("It was on this computer before Setup. Remove it with your package manager.").into());
        pg.bytes = None;
    } else if let Some(why) = shared.clone() {
        pg.why = Some(why);
    } else if unknown {
        pg.default_on = false;
        pg.extra = Some(t!("PostgreSQL is stopped, so what else it holds could not be checked: it stays unless you switch it on. Switch it on only if no other program keeps databases in it, or start PostgreSQL and open this again to have it checked.").into());
    } else if f.pg_origin == Origin::Unknown {
        pg.help = Some(t!("This install is from before the Platform Manager recorded what Setup installed. Setup installs PostgreSQL unless one was already there; another program using it would be listed here.").into());
    }
    let pg_db = row(
        Key::PgDatabase,
        None,
        &tf!("Database {}", n.db_name),
        t!("Drop the database and role SENTIENT created. PostgreSQL itself stays.").into(),
        f.sizes.pg_data,
        false,
        &tf!("database {}", n.db_name),
    );
    let keep_pg = if docker {
        row(Key::KeepPgData, Some(Key::Postgres), &tf!("Keep volume {}", n.vol_pg), tf!("Database {}", n.db_name), f.sizes.pg_data, true, &tf!("volume {}", n.vol_pg))
    } else {
        row(Key::KeepPgData, Some(Key::Postgres), t!("Keep its data"), n.pg_data.clone(), f.sizes.pg_data, true, t!("PostgreSQL's data"))
    };
    if pg.why.is_some() {
        // Setup's database in it can still go.
        out.push(pg_db);
        out.push(pg);
    } else {
        out.push(pg);
        out.push(keep_pg);
    }

    // ClickHouse.
    if f.has_clickhouse {
        let (http, native) = n.ch_ports;
        let mut ch = if docker {
            row(Key::Clickhouse, None, t!("ClickHouse container"), t!("Container and its image").into(), f.sizes.ch_program, true, t!("ClickHouse container"))
        } else if f.ch_in_wsl {
            row(Key::Clickhouse, None, "ClickHouse", t!("Guard service, keep-alive task, hidden account").into(), f.sizes.ch_program, true, "ClickHouse")
        } else {
            row(Key::Clickhouse, None, "ClickHouse", tf!("Program and service; ports {http}, {native}"), f.sizes.ch_program, true, "ClickHouse")
        };
        let keep_ch = if docker {
            row(Key::KeepChData, Some(Key::Clickhouse), &tf!("Keep volume {}", n.vol_ch), t!("Telemetry").into(), f.sizes.ch_data, true, &tf!("volume {}", n.vol_ch))
        } else if f.ch_in_wsl {
            row(Key::KeepChData, Some(Key::Clickhouse), t!("Keep its data"), t!("Telemetry inside the WSL disk (parked, reused by a later install)").into(), f.sizes.ch_data, true, t!("ClickHouse's data"))
        } else {
            row(Key::KeepChData, Some(Key::Clickhouse), t!("Keep its data"), "/var/lib/clickhouse (telemetry)".into(), f.sizes.ch_data, true, t!("ClickHouse's data"))
        };
        if f.ch_origin == Origin::Before {
            ch.detail = t!("Not installed by Setup").into();
            ch.why = Some(t!("It was on this computer before Setup. Remove it with your package manager.").into());
            ch.bytes = None;
            out.push(row(Key::ChDatabase, None, &tf!("ClickHouse database {}", n.db_name), t!("Drop the database and user SENTIENT created. ClickHouse itself stays.").into(), f.sizes.ch_data, false, t!("ClickHouse database")));
            out.push(ch);
        } else {
            out.push(ch);
            out.push(keep_ch);
        }
        if f.ch_in_wsl {
            let mut d = row(Key::WslDistro, Some(Key::Clickhouse), t!("SENTIENT WSL machine"), t!("Unregister machine sentient and delete its disk").into(), f.sizes.distro, true, t!("SENTIENT WSL machine"));
            d.help = Some(t!("ClickHouse runs in Docker in this WSL machine, under its own hidden account. Its disk holds ClickHouse's data.").into());
            out.push(d);
            let mut w = row(Key::Wsl, None, t!("WSL itself"), t!("Turn the Windows feature off. Windows restarts.").into(), None, false, "WSL");
            w.why = wsl_guard(f.wsl_origin, &f.other_distros).err();
            w.extra = Some(if f.other_distros.is_empty() { t!("Other WSL machines: none").into() } else { tf!("Other WSL machines: {}", f.other_distros.join(", ")) });
            out.push(w);
        }
    }
    if !docker {
        out.push(files);
    }
    if f.gateway {
        let mut g = row(Key::Gateway, None, t!("Gateway"), t!("The Sentient Gateway on this computer").into(), None, false, t!("Gateway"));
        g.help = Some(t!("Its service or container and its program go; its settings stay for a new install.").into());
        out.push(g);
    }
    if docker {
        let mut d = row(Key::Docker, None, t!("Docker itself"), t!("Not installed by Setup").into(), None, false, t!("Docker"));
        d.why = Some(t!("Docker is never removed: other programs may use it.").into());
        out.push(d);
    } else {
        out.push(row(Key::Shortcuts, None, t!("Shortcuts"), if f.windows { t!("Start menu and desktop").into() } else { t!("Menu entry and desktop icon").into() }, None, true, "shortcuts"));
    }
    let manager_detail = if f.windows { t!("Tray icon and program; removed last") } else { t!("Removed last") };
    out.push(row(Key::Manager, None, "Platform Manager", manager_detail.into(), f.sizes.manager, false, "Platform Manager"));
    out
}

/// WSL can go only when Setup turned it on and no other distribution uses it.
pub fn wsl_guard(origin: Origin, others: &[String]) -> Result<(), String> {
    match origin {
        Origin::Before => return Err(n!("WSL was on before Setup ran.").into()),
        Origin::Unknown => return Err(n!("Not recorded whether Setup turned WSL on, so it stays.").into()),
        Origin::Setup => {}
    }
    if !others.is_empty() {
        return Err(nf!("Other WSL machines use it: {}.", others.join(", ")));
    }
    Ok(())
}

/// The switches as they first are.
pub fn default_choice(rows: &[Row]) -> Choice {
    Choice { on: rows.iter().filter(|r| r.default_on && r.why.is_none()).map(|r| r.key).collect() }
}

/// Why a row cannot be switched now (None: it can). Its fixed reason, a
/// parent that is off (no reason shown: it just greys), or a rule.
pub fn rule(key: Key, c: &Choice, rows: &[Row]) -> Option<String> {
    let row = rows.iter().find(|r| r.key == key)?;
    if let Some(w) = &row.why {
        return Some(w.clone());
    }
    if let Some(p) = row.parent {
        if !on(p, c, rows) {
            return Some(String::new());
        }
    }
    let present = |k: Key| rows.iter().any(|r| r.key == k);
    match key {
        Key::Postgres | Key::Clickhouse | Key::PgDatabase | Key::ChDatabase if !c.has(Key::Sentient) => Some(t!("SENTIENT uses it: pick SENTIENT too.").into()),
        Key::Manager if !c.has(Key::Sentient) => Some(t!("SENTIENT stays, and the Platform Manager is how it is managed.").into()),
        Key::WslDistro if on(Key::KeepChData, c, rows) => Some(t!("It holds ClickHouse's kept data.").into()),
        Key::Wsl if present(Key::WslDistro) && !on(Key::WslDistro, c, rows) => Some(t!("The SENTIENT WSL machine stays, and it needs WSL.").into()),
        _ => None,
    }
}

/// A switch is on and may be: what the job does.
pub fn on(key: Key, c: &Choice, rows: &[Row]) -> bool {
    c.has(key) && rule(key, c, rows).is_none()
}

/// The effective choice: only what is on and may be.
pub fn effective(c: &Choice, rows: &[Row]) -> Choice {
    Choice { on: rows.iter().map(|r| r.key).filter(|k| on(*k, c, rows)).collect() }
}

/// "X, Y and Z".
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => tf!("{} and {last}", rest.join(", ")),
    }
}


/// What goes, what stays, and the bytes freed (the summary and the result
/// screen's lines).
pub fn gone_and_kept(c: &Choice, rows: &[Row]) -> (Vec<String>, Vec<String>, u64) {
    let (mut gone, mut kept) = (Vec::new(), Vec::new());
    let mut free = 0u64;
    for r in rows {
        let enabled = rule(r.key, c, rows).is_none();
        let picked = enabled && c.has(r.key);
        if r.key.is_keep() {
            if !enabled {
                continue;
            }
            if picked {
                kept.push(r.name.clone());
            } else {
                gone.push(r.name.clone());
                free += r.bytes.unwrap_or(0);
            }
        } else if r.why.is_some() && (r.key == Key::Docker || r.detail == "Not installed by Setup") {
            kept.push(format!("{} ({NOT_BY_SETUP})", r.name));
        } else if picked {
            gone.push(r.name.clone());
            free += r.bytes.unwrap_or(0);
        } else if r.parent.is_none() || enabled {
            kept.push(r.name.clone());
        }
    }
    (gone, kept, free)
}

/// The two summary lines: what goes (and the space it frees), what stays.
pub fn summary(c: &Choice, rows: &[Row]) -> (String, String) {
    let (gone, kept, free) = gone_and_kept(c, rows);
    let removes = if gone.is_empty() {
        t!("Nothing picked.").to_string()
    } else if free > 0 {
        tf!("Removes: {}. Frees {}.", gone.join(", "), human(free))
    } else {
        tf!("Removes: {}.", gone.join(", "))
    };
    let keeps = tf!("Keeps: {}.", if kept.is_empty() { t!("nothing").to_string() } else { kept.join(", ") });
    (removes, keeps)
}

/// The result screen's first line: what was removed (and the space freed).
fn summary_done(c: &Choice, rows: &[Row]) -> String {
    let (gone, _, free) = gone_and_kept(c, rows);
    if gone.is_empty() {
        t!("Nothing picked.").to_string()
    } else if free > 0 {
        tf!("Removed: {}. Freed {}.", gone.join(", "), human(free))
    } else {
        tf!("Removed: {}.", gone.join(", "))
    }
}

/// What a row's size column shows.
pub fn size_text(r: &Row, c: &Choice, rows: &[Row]) -> String {
    if rule(r.key, c, rows).is_some() {
        return String::new();
    }
    match r.bytes {
        Some(b) if b > 0 => {
            if r.key.is_keep() && c.has(r.key) {
                "kept".into()
            } else {
                human(b)
            }
        }
        _ => String::new(),
    }
}

/// A "Keep its data" switch that is off asks for the instance name first.
pub fn deletes_data(c: &Choice, rows: &[Row]) -> bool {
    rows.iter().any(|r| r.key.is_keep() && rule(r.key, c, rows).is_none() && !c.has(r.key)) || on(Key::PgDatabase, c, rows) || on(Key::ChDatabase, c, rows)
}

pub fn human(bytes: u64) -> String {
    sentient_installer_core::size::label(bytes)
}

// ---------------------------------------------------------------------------
// The job's arguments
// ---------------------------------------------------------------------------

/// `uninstall --parts a,b [--keep-files] [--keep-pg-data] [--keep-ch-data]`
/// for the effective choice.
pub fn to_args(c: &Choice, rows: &[Row]) -> Vec<String> {
    let e = effective(c, rows);
    let parts: Vec<&str> = Key::PARTS.iter().filter(|k| e.has(**k)).map(|k| k.id()).collect();
    let mut a = vec!["uninstall".to_string(), "--parts".into(), parts.join(",")];
    for k in [Key::KeepFiles, Key::KeepPgData, Key::KeepChData] {
        if e.has(k) {
            a.push(format!("--{}", k.id()));
        }
    }
    a
}

/// The choice from the job's arguments (after `uninstall`). Without
/// `--parts`, the old meaning: everything Setup installed but the Platform
/// Manager, the data kept unless `--delete-data`.
pub fn from_args(args: &[String]) -> Result<Choice, String> {
    let flag = |f: &str| args.iter().any(|a| a == f);
    let mut c = Choice::default();
    match args.windows(2).find(|w| w[0] == "--parts").map(|w| w[1].clone()) {
        Some(list) => {
            for id in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                let k = Key::PARTS.into_iter().find(|k| k.id() == id).ok_or_else(|| {
                    nf!("unknown part {id}: use {}", Key::PARTS.iter().filter(|k| **k != Key::Docker).map(|k| k.id()).collect::<Vec<_>>().join(", "))
                })?;
                c.set(k, true);
            }
            for k in [Key::KeepFiles, Key::KeepPgData, Key::KeepChData] {
                c.set(k, flag(&format!("--{}", k.id())));
            }
        }
        None => {
            let keep = !flag("--delete-data");
            for k in [Key::Sentient, Key::Postgres, Key::Clickhouse, Key::Gateway, Key::Shortcuts] {
                c.set(k, true);
            }
            for k in [Key::KeepFiles, Key::KeepPgData, Key::KeepChData] {
                c.set(k, keep);
            }
            if !keep {
                // A PostgreSQL that stays (shared, or there before) loses SENTIENT's database.
                c.set(Key::PgDatabase, true);
                c.set(Key::ChDatabase, true);
                c.set(Key::WslDistro, true);
            }
        }
    }
    if flag("--wsl-distro") {
        c.set(Key::WslDistro, true);
    }
    if flag("--wsl") {
        c.set(Key::Wsl, true);
    }
    c.set(Key::Docker, false);
    Ok(c)
}

// ---------------------------------------------------------------------------
// Looking at this computer
// ---------------------------------------------------------------------------

fn origin_of(record: &InstallationRecord, f: impl Fn(&Installed) -> bool) -> Origin {
    match &record.installed {
        Some(i) if f(i) => Origin::Setup,
        Some(_) => Origin::Before,
        None => Origin::Unknown,
    }
}

/// The facts without the slow parts (sizes, the PostgreSQL listing).
pub fn facts_quick(record: &InstallationRecord) -> Facts {
    let windows = cfg!(windows);
    let native = record.engine == Engine::Native;
    let linux_native = !windows && native;
    let ch_in_wsl = windows && native && ch_wsl_installed();
    let ch_left = linux_native && Path::new("/usr/bin/clickhouse").exists() && unit_file_exists("clickhouse-server.service");
    let mut pg_origin = if record.engine == Engine::Docker { Origin::Setup } else { origin_of(record, |i| i.postgres) };
    // Windows' PostgreSQL is always the one Setup bundled.
    if windows && native && pg_origin == Origin::Unknown {
        pg_origin = Origin::Setup;
    }
    let mut ch_origin = if record.engine == Engine::Docker || ch_in_wsl { Origin::Setup } else { origin_of(record, |i| i.clickhouse) };
    // Setup writes its own unit under /etc; a packaged ClickHouse has its unit under /usr/lib.
    if linux_native && ch_origin == Origin::Unknown && Path::new("/usr/lib/systemd/system/clickhouse-server.service").exists() {
        ch_origin = Origin::Before;
    }
    let st = &record.stack;
    let n = st.names();
    let names = Names {
        install_dir: record.install_dir.display().to_string(),
        files: if windows {
            tf!("{}\\data, backups", record.state_dir.display())
        } else if record.engine == Engine::Docker {
            String::new()
        } else {
            format!("{}, {LINUX_BACKUPS}", record.state_dir.display())
        },
        pg_data: if windows {
            record.pg_dir.join("data").display().to_string()
        } else {
            Pm::detect().map(|pm| pm.postgres_data_dirs()[0].to_string()).unwrap_or_else(|| "/var/lib/postgres".into())
        },
        db_name: st.db_name.clone(),
        pg_port: st.pg_port,
        ch_ports: (st.ch_http_port, st.ch_native_port),
        app_container: n.app.clone(),
        image: st.image.clone(),
        vol_data: n.vol_data.clone(),
        vol_pg: n.vol_pg.clone(),
        vol_ch: n.vol_ch.clone(),
    };
    Facts {
        windows,
        engine: record.engine,
        timescale: record.flavor().uses_timescaledb(),
        has_clickhouse: record.flavor().uses_clickhouse() || ch_left || ch_in_wsl,
        ch_in_wsl,
        pg_origin,
        ch_origin,
        wsl_origin: origin_of(record, |i| i.wsl),
        pg_others: Some(Ok(Vec::new())),
        gateway: sentient_installer_core::gateway::status().installed,
        other_distros: if ch_in_wsl { sentient_installer_core::wsl::other_distros() } else { Vec::new() },
        sizes: Sizes::default(),
        names,
    }
}

/// Every fact, sizes and the PostgreSQL listing included (blocking; may ask
/// for the administrator password once, inside the user's action).
pub fn facts_full(record: &InstallationRecord) -> Facts {
    let mut f = facts_quick(record);
    f.pg_others = Some(pg_others(record));
    f.sizes = sizes(record, &f);
    f
}

/// Where native Linux backups go by default.
pub const LINUX_BACKUPS: &str = "/var/backups/sentient";

#[cfg(windows)]
fn ch_wsl_installed() -> bool {
    sentient_installer_core::clickhouse_wsl::is_installed()
}
#[cfg(not(windows))]
fn ch_wsl_installed() -> bool {
    false
}

fn unit_file_exists(unit: &str) -> bool {
    ["/etc/systemd/system", "/usr/lib/systemd/system", "/lib/systemd/system"].iter().any(|d| Path::new(d).join(unit).exists())
}

/// What [`pg_others`] says when PostgreSQL is stopped: unknown, never
/// "none" (a stopped cluster lists nothing, yet may hold other programs'
/// databases). The dialog keeps PostgreSQL unless the operator switches it
/// on ([`rows`]). An older Platform Manager reading it sees an error and
/// keeps PostgreSQL fixed: safe either way.
pub const PG_STOPPED: &str = n!("PostgreSQL is stopped, so the databases it holds could not be listed");

/// The other databases in PostgreSQL (Linux native, where it is shared).
pub fn pg_others(record: &InstallationRecord) -> Result<Vec<String>, String> {
    if cfg!(target_os = "linux") && record.engine == Engine::Native {
        pg_others_when(sentient_installer_core::native::pg_stopped(), || record.platform().pg_other_databases())
    } else {
        Ok(Vec::new())
    }
}

/// [`pg_others`] given whether PostgreSQL is stopped: unknown then
/// ([`PG_STOPPED`]), else its listing.
fn pg_others_when(stopped: bool, list: impl FnOnce() -> Result<Vec<String>, String>) -> Result<Vec<String>, String> {
    if stopped {
        Err(PG_STOPPED.into())
    } else {
        list()
    }
}

/// What else PostgreSQL holds is unknown because it is stopped.
pub fn pg_unknown(f: &Facts) -> bool {
    matches!(&f.pg_others, Some(Err(e)) if e == PG_STOPPED)
}

/// Bytes under `dir` (what this account can read).
pub fn dir_bytes(dir: &Path) -> Option<u64> {
    fn walk(dir: &Path, total: &mut u64) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() {
                walk(&e.path(), total);
            } else {
                *total += m.len();
            }
        }
    }
    let m = std::fs::metadata(dir).ok()?;
    if m.is_file() {
        return Some(m.len());
    }
    std::fs::read_dir(dir).ok()?;
    let mut total = 0;
    walk(dir, &mut total);
    Some(total)
}

/// Sizes of the parts (slow: walks folders; on native Linux the root-only
/// folders are measured in the root helper the PostgreSQL listing already
/// started, so no second password is asked).
pub fn sizes(record: &InstallationRecord, facts: &Facts) -> Sizes {
    let mut s = Sizes { manager: std::fs::metadata(crate::self_install::manager_exe()).ok().map(|m| m.len()), ..Default::default() };
    match record.engine {
        Engine::Docker => {
            let image = |name: &str| docker_out(&["image", "inspect", "-f", "{{.Size}}", name]).and_then(|t| t.parse().ok());
            let n = record.stack.names();
            s.sentient = image(&record.stack.image);
            s.pg_program = docker_out(&["inspect", "-f", "{{.Config.Image}}", &n.pg]).and_then(|i| image(&i));
            s.ch_program = docker_out(&["inspect", "-f", "{{.Config.Image}}", &n.ch]).and_then(|i| image(&i));
            // Volume sizes need root (their folders are Docker's): `docker system df -v`.
            if let Some(t) = docker_out(&["system", "df", "-v", "--format", "{{range .Volumes}}{{.Name}}\t{{.Size}}\n{{end}}"]) {
                for l in t.lines() {
                    if let Some((name, size)) = l.split_once('\t') {
                        let b = parse_docker_size(size);
                        if name == n.vol_pg {
                            s.pg_data = b;
                        } else if name == n.vol_ch {
                            s.ch_data = b;
                        } else if name == n.vol_data {
                            s.files = b;
                        }
                    }
                }
            }
        }
        Engine::Native if facts.windows => {
            s.sentient = dir_bytes(&record.install_dir);
            let data = dir_bytes(&record.pg_dir.join("data"));
            s.pg_program = dir_bytes(&record.pg_dir).map(|all| all.saturating_sub(data.unwrap_or(0)));
            s.pg_data = data;
            s.files = dir_bytes(&record.state_dir.join("data"));
            #[cfg(windows)]
            {
                s.distro = dir_bytes(&sentient_installer_core::clickhouse_wsl::disk_dir().join("ext4.vhdx"));
            }
        }
        Engine::Native => {
            s.sentient = dir_bytes(&record.install_dir);
            s.ch_program = dir_bytes(Path::new("/usr/bin/clickhouse"));
            if let Some(pm) = Pm::detect() {
                s.pg_program = package_bytes(pm);
            }
            #[cfg(unix)]
            if sentient_installer_core::elevation::alive() || crate::rootjob::is_root() {
                let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
                let l2 = lines.clone();
                let sink: ProgressFn = std::sync::Arc::new(move |p| {
                    if let Progress::Log { line } = p {
                        if let Ok(mut v) = l2.lock() {
                            v.push(line);
                        }
                    }
                });
                let dirs = format!("/var/lib/postgres /var/lib/postgresql /var/lib/pgsql /var/lib/clickhouse {} {LINUX_BACKUPS}", removal_shq(&record.state_dir.display().to_string()));
                let _ = sentient_installer_core::privileged::run(&sink, &format!("for d in {dirs}; do [ -d \"$d\" ] && printf 'SIZE\\t%s\\t%s\\n' \"$d\" \"$(du -sb \"$d\" 2>/dev/null | cut -f1)\"; done; true"));
                for line in lines.lock().map(|v| v.clone()).unwrap_or_default() {
                    let mut f = line.split('\t');
                    if let (Some("SIZE"), Some(dir), Some(n)) = (f.next(), f.next(), f.next().and_then(|n| n.trim().parse::<u64>().ok())) {
                        let add = |slot: &mut Option<u64>| *slot = Some(slot.unwrap_or(0) + n);
                        match dir {
                            "/var/lib/clickhouse" => add(&mut s.ch_data),
                            "/var/lib/postgres" | "/var/lib/postgresql" | "/var/lib/pgsql" => add(&mut s.pg_data),
                            _ => add(&mut s.files),
                        }
                    }
                }
            }
        }
    }
    s
}

/// `1.2GB`, `350MB`, `0B` from `docker system df`.
pub fn parse_docker_size(t: &str) -> Option<u64> {
    let t = t.trim();
    let split = t.find(|c: char| c.is_ascii_alphabetic())?;
    let (n, unit) = t.split_at(split);
    let n: f64 = n.trim().parse().ok()?;
    let mul = match unit.trim() {
        "B" => 1.,
        "kB" | "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        _ => return None,
    };
    Some((n * mul) as u64)
}

/// The PostgreSQL server package's installed size.
fn package_bytes(pm: Pm) -> Option<u64> {
    let pkg = pm.postgres_packages()[0];
    let run = |p: &str, a: &[&str]| sentient_installer_core::host::host_command_env(p, &[("LC_ALL", "C")]).args(a).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    match pm {
        Pm::Pacman => parse_pacman_size(&run("pacman", &["-Qi", pkg])?),
        Pm::Apt => {
            // Debian's server is postgresql-NN; postgresql is a metapackage.
            let out = run("dpkg-query", &["-W", "-f=${Installed-Size}\\n", "postgresql-[0-9]*"])?;
            let kib: u64 = out.lines().filter_map(|l| l.trim().parse::<u64>().ok()).sum();
            (kib > 0).then_some(kib * 1024)
        }
        Pm::Dnf | Pm::Zypper => run("rpm", &["-q", "--qf", "%{SIZE}", pkg])?.trim().parse().ok(),
    }
}

/// `Installed Size  : 56.20 MiB` from `pacman -Qi`.
pub fn parse_pacman_size(text: &str) -> Option<u64> {
    let line = text.lines().find(|l| l.starts_with("Installed Size"))?;
    let v = line.split_once(':')?.1.trim();
    let (n, unit) = v.split_once(' ')?;
    let n: f64 = n.replace(',', ".").parse().ok()?;
    let mul = match unit.trim() {
        "B" => 1.,
        "KiB" => 1024.,
        "MiB" => 1024. * 1024.,
        "GiB" => 1024. * 1024. * 1024.,
        _ => return None,
    };
    Some((n * mul) as u64)
}

// ---------------------------------------------------------------------------
// Removing
// ---------------------------------------------------------------------------

fn step(sink: &ProgressFn, name: &str) {
    sink(Progress::Step { name: name.into() });
}
fn log(sink: &ProgressFn, line: impl Into<String>) {
    sink(Progress::Log { line: line.into() });
}

/// What happens to PostgreSQL. None: it is left as it is.
fn pg_action(e: &Choice) -> Option<DbRemoval> {
    if e.has(Key::Postgres) {
        Some(DbRemoval::Remove { keep_data: e.has(Key::KeepPgData) })
    } else if e.has(Key::PgDatabase) {
        Some(DbRemoval::StopUsing { keep_data: false })
    } else {
        None
    }
}

fn ch_action(e: &Choice) -> Option<DbRemoval> {
    if e.has(Key::Clickhouse) {
        Some(DbRemoval::Remove { keep_data: e.has(Key::KeepChData) })
    } else if e.has(Key::ChDatabase) {
        Some(DbRemoval::StopUsing { keep_data: false })
    } else {
        None
    }
}

/// Remove what `c` picks (the Platform Manager excepted: it goes last, as
/// the account that runs it, [`remove_manager_last`]), then check that
/// nothing of it is left. Each check is a line in the output
/// (removal::CHECK_PREFIX). Fails only when nothing could run; what could
/// not be removed is in the checks.
pub fn run(record: InstallationRecord, c: Choice, facts: Facts, sink: ProgressFn) -> Result<(), String> {
    let rows = rows(&facts);
    let e = effective(&c, &rows);
    for k in c.on.difference(&e.on) {
        if let Some(why) = rule(*k, &c, &rows).filter(|w| !w.is_empty()) {
            log(&sink, tf!("{} stays: {why}", rows.iter().find(|r| r.key == *k).map(|r| r.title.clone()).unwrap_or_default()));
        }
    }
    let pg = pg_action(&e);
    let ch = ch_action(&e);
    let keep_files = e.has(Key::KeepFiles);
    let mut problems: Vec<String> = Vec::new();
    let mut note = |r: Result<(), String>, what: &str| {
        if let Err(err) = r {
            problems.push(format!("{what}: {err}"));
        }
    };
    let cfg = record.native_config();
    let nested = crate::maintenance_ops::nested(&sink);
    let windows = facts.windows;
    // Docker images, read before their containers go (the check looks for them).
    let mut images: Vec<String> = Vec::new();

    if e.has(Key::Gateway) {
        step(&sink, t!("Removing the Gateway"));
        let r = sentient_installer_core::gateway::remove(&nested, true, true).map(|msg| {
            let _ = crate::gateway_record::save(None);
            log(&sink, msg);
        });
        note(r, t!("The Gateway"));
    }

    if e.has(Key::Sentient) {
        step(&sink, t!("Removing SENTIENT"));
        match record.engine {
            Engine::Native => note(sentient_installer_core::native::remove_sentient_only(nested.clone(), &cfg), "SENTIENT"),
            Engine::Docker if windows => {
                // Everything is on the distribution's disk.
                let r = if pg.is_some_and(|p| !p.keeps_data()) || ch.is_some_and(|h| !h.keeps_data()) {
                    sentient_installer_core::distro::uninstall(nested.clone(), &record.stack)
                } else {
                    sentient_installer_core::distro::remove_keeping_data(nested.clone(), &record.stack)
                };
                note(r, "SENTIENT");
            }
            Engine::Docker => {
                let n = record.stack.names();
                let parts = removal::DockerParts {
                    sentient: true,
                    postgres: e.has(Key::Postgres),
                    clickhouse: e.has(Key::Clickhouse),
                    keep_pg_data: e.has(Key::KeepPgData),
                    keep_ch_data: e.has(Key::KeepChData),
                    keep_files,
                    has_clickhouse: facts.has_clickhouse,
                };
                for (picked, name) in [(true, &n.app), (parts.postgres, &n.pg), (parts.clickhouse, &n.ch)] {
                    if picked {
                        if let Some(i) = docker_out(&["inspect", "-f", "{{.Config.Image}}", name]) {
                            images.push(i);
                        }
                    }
                }
                let files = if record.stack.external.is_some() { Vec::new() } else { vec![record.stack.compose_path(), record.stack.ch_config_path()] };
                note(run_shell(&nested, &removal::docker_script(&n, &parts, &files)), t!("SENTIENT's containers"));
                if parts.postgres && (parts.clickhouse || !facts.has_clickhouse) {
                    let _ = std::fs::remove_dir(record.stack.stack_dir());
                }
            }
        }
    }

    // ClickHouse before PostgreSQL: the reverse of how they start.
    if record.engine == Engine::Native {
        if facts.ch_in_wsl {
            #[cfg(windows)]
            if e.has(Key::Clickhouse) {
                if e.has(Key::WslDistro) {
                    note(crate::kept::remove_clickhouse(&sink), "ClickHouse");
                } else {
                    // Kept: stopped cleanly and not started at boot.
                    sentient_installer_core::clickhouse_wsl::park(&sink, record.stack.ch_http_port);
                }
            } else if e.has(Key::Sentient) {
                sentient_installer_core::clickhouse_wsl::park(&sink, record.stack.ch_http_port);
            }
        } else if let Some(how) = ch {
            step(&sink, if matches!(how, DbRemoval::StopUsing { .. }) { t!("Removing SENTIENT's database from ClickHouse") } else { t!("Removing ClickHouse") });
            #[cfg(unix)]
            note(removal::run_root(&nested, &removal::linux_clickhouse_script(how, record.stack.ch_native_port, &record.stack.ch_db, &record.stack.ch_user)), "ClickHouse");
        }
        match pg {
            Some(how) if !windows => {
                step(&sink, if matches!(how, DbRemoval::StopUsing { .. }) { t!("Removing SENTIENT's database from PostgreSQL") } else { t!("Removing PostgreSQL") });
                match Pm::detect() {
                    #[cfg(unix)]
                    Some(pm) => {
                        let script = removal::linux_postgres_script(pm, how, record.stack.pg_port, &record.stack.db_name, &record.stack.db_user, facts.timescale);
                        note(removal::run_root(&nested, &script), "PostgreSQL");
                    }
                    _ => note(Err(n!("no supported package manager found (apt, pacman, dnf, zypper).").into()), "PostgreSQL"),
                }
            }
            #[cfg(windows)]
            Some(DbRemoval::Remove { keep_data }) => note(sentient_installer_core::native::remove_postgres_windows(&sink, &cfg, keep_data), "PostgreSQL"),
            // Kept on Windows: stopped and started by hand, for a new install to reuse.
            #[cfg(windows)]
            _ if e.has(Key::Sentient) => sentient_installer_core::native::park_postgres_windows(&sink),
            _ => {}
        }
        // SENTIENT's file stores, backups, logs and service account.
        if e.has(Key::Sentient) && !keep_files {
            step(&sink, t!("Removing SENTIENT's file stores and backups"));
            #[cfg(unix)]
            if !windows {
                let state = removal_shq(&record.state_dir.display().to_string());
                note(removal::run_root(&nested, &format!("rm -rf {state} {LINUX_BACKUPS} /var/log/sentient\nuserdel sentient 2>/dev/null || true\ngroupdel sentient-admin 2>/dev/null || true\ntrue\n")), t!("SENTIENT's files"));
            }
            if windows {
                for d in ["data", "backups", "logs", "downloads"] {
                    let p = record.state_dir.join(d);
                    if p.exists() && std::fs::remove_dir_all(&p).is_err() {
                        note(Err(nf!("{} could not be deleted", p.display())), t!("SENTIENT's files"));
                    }
                }
            }
        }
    }

    if e.has(Key::Wsl) {
        note(sentient_installer_core::wsl::remove(&sink), "WSL");
    }

    if e.has(Key::Sentient) {
        let data_left = pg.is_none_or(|p| p.keeps_data()) || (facts.has_clickhouse && ch.is_none_or(|h| h.keeps_data()));
        if !data_left && !keep_files {
            crate::secrets::forget_all();
        }
    }

    if e.has(Key::Shortcuts) {
        step(&sink, t!("Removing SENTIENT's shortcuts"));
        crate::self_install::remove_sentient_shortcuts();
        sentient_installer_core::kiosk::remove_shortcuts(&sink, &record.state_dir);
        sentient_installer_core::kiosk::remove_user_data(&sink);
        crate::shortcuts::remove(crate::shortcuts::Owner::Sentient, &record.shortcuts, &sink);
    }

    if e.has(Key::Sentient) {
        #[cfg(windows)]
        sentient_installer_core::native::remove_start_task();
        if sentient_installer_core::kiosk::open_at_signin().is_some() {
            let _ = sentient_installer_core::kiosk::set_open_at_signin(&sentient_installer_core::progress::noop(), &crate::self_install::manager_exe(), None);
        }
        if sentient_installer_core::autologin::set_by_manager() {
            let _ = sentient_installer_core::autologin::set(&sink, None);
        }
        if !e.has(Key::Manager) {
            // SENTIENT's Apps & features entry went with it: the Platform
            // Manager, which stays, gets its own back.
            crate::self_install::register_manager_entry(&crate::self_install::manager_exe());
        }
        crate::self_install::remove_record(&crate::installation::record_path());
    }

    step(&sink, t!("Checking that nothing is left"));
    let expect = expect(&record, &e, &facts, pg, ch, &images);
    let mut checks = removal::verify(&expect, &removal::system_runner, &|p: &Path| p.exists());
    if e.has(Key::Gateway) {
        let left = sentient_installer_core::gateway::status().installed;
        checks.push(Check {
            what: if left { t!("The Gateway is still installed").into() } else { t!("No Gateway").into() },
            ok: !left,
            left: left.then(|| "the Gateway".into()),
            fix: left.then(|| if windows { "\"Sentient Platform Manager.exe\" --job gateway-remove --remove-image".into() } else { "sentient-platform-manager --job gateway-remove --remove-image".into() }),
        });
    }
    for p in &problems {
        log(&sink, tf!("note: {p}"));
    }
    for ch in &checks {
        sink(Progress::Log { line: ch.encode() });
    }
    sink(Progress::Done { message: done_message(&e, &rows, &checks, &problems) });
    Ok(())
}

#[cfg(unix)]
fn removal_shq(s: &str) -> String {
    sentient_installer_core::privileged::shell_quote(s)
}

fn docker_out(args: &[&str]) -> Option<String> {
    let o = sentient_installer_core::host::host_command("docker").args(args).output().ok()?;
    Some(String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| o.status.success() && !s.is_empty())
}

fn run_shell(sink: &ProgressFn, script: &str) -> Result<(), String> {
    let out = sentient_installer_core::host::host_command("sh").arg("-c").arg(script).output().map_err(|e| nf!("run sh: {e}"))?;
    for l in String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.trim().is_empty()) {
        log(sink, l.trim_start_matches(">> ").to_string());
    }
    if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
}

/// What must be gone, as chosen (`e`: the effective choice).
fn expect(record: &InstallationRecord, e: &Choice, facts: &Facts, pg: Option<DbRemoval>, ch: Option<DbRemoval>, images: &[String]) -> Expect {
    let mut x = Expect { windows: facts.windows, ..Default::default() };
    let st = &record.stack;
    let n = st.names();
    let docker = record.engine == Engine::Docker && !facts.windows;
    if e.has(Key::Sentient) {
        x.ports.push(("SENTIENT".into(), st.http_port));
        if docker {
            x.containers.push(n.app.clone());
            if !e.has(Key::KeepFiles) {
                x.volumes.push(n.vol_data.clone());
            }
        } else if record.engine == Engine::Native {
            x.processes.push(("SENTIENT".into(), vec!["sentient-server".into()]));
            x.units.push(if facts.windows { sentient_installer_core::native::APP_SERVICE.into() } else { "sentient.service".into() });
            x.paths.push(record.install_dir.join("bin"));
            if !e.has(Key::KeepFiles) && !facts.windows {
                x.paths.push(record.state_dir.clone());
                x.paths.push(PathBuf::from(LINUX_BACKUPS));
            }
        }
    }
    let removed = |a: Option<DbRemoval>| matches!(a, Some(DbRemoval::Remove { .. }));
    if removed(pg) {
        x.ports.push(("PostgreSQL".into(), st.pg_port));
        if docker {
            x.containers.push(n.pg.clone());
            if !e.has(Key::KeepPgData) {
                x.volumes.push(n.vol_pg.clone());
            }
        } else if facts.windows {
            x.processes.push(("PostgreSQL".into(), vec!["postgres".into()]));
            x.units.push(sentient_installer_core::native::PG_SERVICE.into());
            x.paths.push(record.pg_dir.join("bin"));
            if !e.has(Key::KeepPgData) {
                x.paths.push(record.pg_dir.join("data"));
            }
        } else {
            x.processes.push(("PostgreSQL".into(), vec!["postgres".into()]));
            x.units.push("postgresql.service".into());
            if let Some(pm) = Pm::detect() {
                x.pm = Some(pm);
                x.packages.push(pm.postgres_packages()[0].into());
                if !e.has(Key::KeepPgData) {
                    x.paths.extend(pm.postgres_data_dirs().iter().map(PathBuf::from));
                }
            }
        }
    }
    if removed(ch) {
        if facts.ch_in_wsl {
            x.ports.push(("ClickHouse".into(), st.ch_http_port));
            if e.has(Key::WslDistro) {
                #[cfg(windows)]
                x.paths.push(sentient_installer_core::clickhouse_wsl::disk_dir().join("ext4.vhdx"));
            }
        } else {
            x.ports.push(("ClickHouse".into(), st.ch_http_port));
            x.ports.push(("ClickHouse".into(), st.ch_native_port));
            if docker {
                x.containers.push(n.ch.clone());
                if !e.has(Key::KeepChData) {
                    x.volumes.push(n.vol_ch.clone());
                }
            } else {
                x.processes.push(("ClickHouse".into(), vec!["clickhouse-serv".into(), "clickhouse-server".into()]));
                x.units.push("clickhouse-server.service".into());
                x.paths.push(PathBuf::from("/usr/bin/clickhouse"));
                if !e.has(Key::KeepChData) {
                    x.paths.push(PathBuf::from("/var/lib/clickhouse"));
                }
            }
        }
    }
    x.images = images.to_vec();
    x.wsl_feature_off = e.has(Key::Wsl);
    x
}

/// The job's last line.
pub fn done_message(e: &Choice, rows: &[Row], checks: &[Check], problems: &[String]) -> String {
    let left = checks.iter().filter(|c| !c.ok).count();
    let mut m = summary_done(e, rows);
    if left > 0 {
        m.push_str(&if left == 1 { tf!(" {left} thing is left: see the list.") } else { tf!(" {left} things are left: see the list.") });
    } else if !problems.is_empty() {
        m.push_str(t!(" Some steps reported a problem: see the log."));
    }
    if e.has(Key::Wsl) {
        m.push_str(t!(" WSL is off after Windows restarts."));
    }
    m
}

/// The result screen's "Checked" line, from the checks that passed.
pub fn checked_line(checks: &[Check]) -> Option<String> {
    let ok: Vec<&Check> = checks.iter().filter(|c| c.ok).collect();
    if ok.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    if ok.iter().any(|c| c.what.ends_with("process running")) {
        parts.push("no process".to_string());
    }
    let ports: Vec<String> = ok.iter().filter_map(|c| c.what.strip_prefix("Port ")?.split(' ').next().map(str::to_string)).collect();
    let mut ports_d = ports.clone();
    ports_d.dedup();
    match ports_d.len() {
        0 => {}
        1 => parts.push(tf!("port {} free", ports_d[0])),
        _ => parts.push(tf!("ports {} free", ports_d.join(", "))),
    }
    let units = ok.iter().any(|c| c.what.ends_with(" service") && c.what.starts_with("No "));
    let containers = ok.iter().any(|c| c.what.ends_with(" container"));
    match (units, containers) {
        (true, true) => parts.push("no units or containers left".into()),
        (true, false) => parts.push("no units left".into()),
        (false, true) => parts.push("no containers left".into()),
        _ => {}
    }
    if ok.iter().any(|c| c.what.starts_with("Package ")) {
        parts.push("packages removed".into());
    }
    if ok.iter().any(|c| c.what.ends_with(" removed") && !c.what.starts_with("Package ")) {
        parts.push("folders removed".into());
    }
    Some(tf!("Checked: {}", parts.join(", ")))
}

/// The Platform Manager goes last, after the result screen, as the account
/// that runs it (never in a root job: its files are in this account's home).
pub fn remove_manager_last(sink: &ProgressFn) {
    let quiet = crate::maintenance_ops::nested(sink);
    let _ = crate::maintenance_ops::run_remove_manager(quiet);
}

/// The checks in a job's output.
pub fn checks_in(lines: &[impl AsRef<str>]) -> Vec<Check> {
    lines.iter().filter_map(|l| Check::decode(l.as_ref())).collect()
}

// ---------------------------------------------------------------------------
// Screens: one canned computer per deployment (SPM_DEBUG_UNINSTALL=<name>)
// ---------------------------------------------------------------------------

/// The eight deployments the dialog is drawn for.
#[cfg_attr(not(test), allow(dead_code))]
pub const FIXTURES: [&str; 8] = ["lx-pg", "lx-ts", "lx-ch", "lx-dk", "lx-ext", "win-pg", "win-ch", "remote"];

pub fn fixture(name: &str) -> Option<Facts> {
    let mb = |n: u64| Some(n << 20);
    let mut f = Facts {
        windows: false,
        engine: Engine::Native,
        timescale: false,
        has_clickhouse: false,
        ch_in_wsl: false,
        pg_origin: Origin::Setup,
        ch_origin: Origin::Setup,
        wsl_origin: Origin::Unknown,
        pg_others: Some(Ok(Vec::new())),
        gateway: false,
        other_distros: Vec::new(),
        sizes: Sizes { sentient: mb(420), files: mb(5100), pg_program: mb(160), pg_data: mb(38200), ch_program: mb(610), ch_data: mb(94100), distro: mb(1900), manager: mb(48) },
        names: Names {
            install_dir: "/opt/sentient".into(),
            files: format!("/var/lib/sentient, {LINUX_BACKUPS}"),
            pg_data: "/var/lib/postgresql/17 (database sentient)".into(),
            db_name: "sentient".into(),
            pg_port: 5432,
            ch_ports: (8123, 9000),
            app_container: "sentient".into(),
            image: "quakestring/sentient:4.4.0.3".into(),
            vol_data: "st-data".into(),
            vol_pg: "st-pg-data".into(),
            vol_ch: "st-ch-data".into(),
        },
    };
    match name {
        "lx-pg" => f.gateway = true,
        "lx-ts" => f.timescale = true,
        "lx-ch" => f.has_clickhouse = true,
        "lx-dk" => {
            f.engine = Engine::Docker;
            f.has_clickhouse = true;
            f.sizes.sentient = mb(980);
            f.sizes.pg_program = mb(410);
            f.sizes.ch_program = mb(720);
        }
        "lx-ext" => {
            f.pg_origin = Origin::Before;
            f.pg_others = Some(Ok(vec!["erp".into(), "wiki".into()]));
        }
        "win-pg" | "win-ch" => {
            f.windows = true;
            f.gateway = name == "win-pg";
            f.names.install_dir = "C:\\Program Files\\SENTIENT".into();
            f.names.files = "C:\\ProgramData\\SENTIENT\\data, backups".into();
            f.names.pg_data = "C:\\ProgramData\\SENTIENT\\postgres".into();
            f.sizes.sentient = mb(450);
            f.sizes.pg_program = mb(240);
            f.sizes.manager = mb(52);
            if name == "win-ch" {
                f.has_clickhouse = true;
                f.ch_in_wsl = true;
                f.wsl_origin = Origin::Setup;
                f.sizes.ch_program = mb(30);
            }
        }
        "remote" => {
            f.has_clickhouse = true;
            f.gateway = true;
        }
        _ => return None,
    }
    Some(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(rows: &[Row]) -> Vec<Key> {
        rows.iter().map(|r| r.key).collect()
    }

    #[test]
    fn rows_follow_what_is_installed() {
        use Key::*;
        assert_eq!(keys(&rows(&fixture("lx-pg").unwrap())), [Sentient, Postgres, KeepPgData, KeepFiles, Gateway, Shortcuts, Manager]);
        assert_eq!(keys(&rows(&fixture("lx-ch").unwrap())), [Sentient, Postgres, KeepPgData, Clickhouse, KeepChData, KeepFiles, Shortcuts, Manager]);
        assert_eq!(keys(&rows(&fixture("lx-dk").unwrap())), [Sentient, KeepFiles, Postgres, KeepPgData, Clickhouse, KeepChData, Docker, Manager]);
        assert_eq!(keys(&rows(&fixture("win-ch").unwrap())), [Sentient, Postgres, KeepPgData, Clickhouse, KeepChData, WslDistro, Wsl, KeepFiles, Shortcuts, Manager]);
        assert_eq!(rows(&fixture("lx-ts").unwrap())[1].title, "PostgreSQL and TimescaleDB");
        // Defaults: data kept, the Platform Manager and the Gateway stay.
        let r = rows(&fixture("lx-pg").unwrap());
        let c = default_choice(&r);
        assert!(c.has(Sentient) && c.has(Postgres) && c.has(KeepPgData) && c.has(KeepFiles) && c.has(Shortcuts));
        assert!(!c.has(Manager) && !c.has(Gateway));
        // Docker itself is shown, never picked.
        let r = rows(&fixture("lx-dk").unwrap());
        assert!(rule(Docker, &default_choice(&r), &r).unwrap().contains("never removed"));
    }

    #[test]
    fn an_existing_or_shared_postgres_cannot_be_picked() {
        let r = rows(&fixture("lx-ext").unwrap());
        assert_eq!(keys(&r), [Key::Sentient, Key::PgDatabase, Key::Postgres, Key::KeepFiles, Key::Shortcuts, Key::Manager]);
        let pg = r.iter().find(|r| r.key == Key::Postgres).unwrap();
        assert!(pg.why.as_deref().unwrap().contains("before Setup"));
        let mut c = default_choice(&r);
        // Only SENTIENT's database can go, off by default.
        assert!(!c.has(Key::Postgres) && !c.has(Key::PgDatabase));
        assert_eq!(pg_action(&effective(&c, &r)), None);
        c.set(Key::PgDatabase, true);
        assert_eq!(pg_action(&effective(&c, &r)), Some(DbRemoval::StopUsing { keep_data: false }));
        assert!(deletes_data(&c, &r));
        // Installed by Setup but shared.
        let mut f = fixture("lx-pg").unwrap();
        f.pg_others = Some(Ok(vec!["erp".into()]));
        let r = rows(&f);
        assert_eq!(r.iter().find(|r| r.key == Key::Postgres).unwrap().why.as_deref(), Some("It also holds erp: another program uses it, so it stays."));
        f.pg_others = None;
        assert!(rows(&f).iter().find(|r| r.key == Key::Postgres).unwrap().why.as_deref().unwrap().starts_with("Checking"));
        // A listing that failed: fixed, kept.
        f.pg_others = Some(Err("psql: connection refused".into()));
        assert!(rows(&f).iter().find(|r| r.key == Key::Postgres).unwrap().why.as_deref().unwrap().contains("could not be checked"));
        // Setup's own: removed for real, data kept by default.
        let r = rows(&fixture("lx-pg").unwrap());
        assert_eq!(pg_action(&effective(&default_choice(&r), &r)), Some(DbRemoval::Remove { keep_data: true }));
    }

    #[test]
    fn nested_switches_follow_their_part_and_databases_need_sentient() {
        let r = rows(&fixture("lx-ch").unwrap());
        let mut c = default_choice(&r);
        c.set(Key::Postgres, false);
        assert_eq!(rule(Key::KeepPgData, &c, &r).as_deref(), Some(""));
        assert!(!effective(&c, &r).has(Key::KeepPgData));
        c.set(Key::Sentient, false);
        c.set(Key::Manager, true);
        let e = effective(&c, &r);
        assert!(!e.has(Key::Clickhouse) && !e.has(Key::Manager) && e.has(Key::Shortcuts));
        assert_eq!(rule(Key::Clickhouse, &c, &r).unwrap(), "SENTIENT uses it: pick SENTIENT too.");
    }

    #[test]
    fn the_summary_names_what_goes_and_what_stays() {
        let r = rows(&fixture("lx-pg").unwrap());
        let mut c = default_choice(&r);
        assert_eq!(
            summary(&c, &r),
            ("Removes: SENTIENT server, PostgreSQL, shortcuts. Frees 580 MB.".into(), "Keeps: PostgreSQL's data, file stores and backups, Gateway, Platform Manager.".into())
        );
        c.set(Key::KeepPgData, false);
        let (rm, kp) = summary(&c, &r);
        assert!(rm.contains("PostgreSQL's data") && rm.contains("Frees 38 GB"), "{rm}");
        assert!(!kp.contains("PostgreSQL's data"));
        assert_eq!(size_text(&r[2], &c, &r), "37 GB");
        c.set(Key::KeepPgData, true);
        assert_eq!(size_text(&r[2], &c, &r), "kept");
        let none = Choice::default();
        assert_eq!(summary(&none, &r).0, "Nothing picked.");
        let r = rows(&fixture("lx-dk").unwrap());
        assert!(summary(&default_choice(&r), &r).1.contains("Docker (not installed by Setup)"));
    }

    #[test]
    fn the_wsl_guard() {
        assert!(wsl_guard(Origin::Setup, &[]).is_ok());
        assert_eq!(wsl_guard(Origin::Setup, &["Ubuntu".into(), "docker-desktop".into()]).unwrap_err(), "Other WSL machines use it: Ubuntu, docker-desktop.");
        assert!(wsl_guard(Origin::Before, &[]).unwrap_err().contains("before Setup"));
        assert!(wsl_guard(Origin::Unknown, &[]).is_err());
        let mut f = fixture("win-ch").unwrap();
        let r = rows(&f);
        let mut c = default_choice(&r);
        // Off by default; the machine holds the kept data.
        assert!(!c.has(Key::Wsl));
        assert!(rule(Key::WslDistro, &c, &r).unwrap().contains("kept data"));
        c.set(Key::KeepChData, false);
        c.set(Key::Wsl, true);
        let e = effective(&c, &r);
        assert!(e.has(Key::WslDistro) && e.has(Key::Wsl));
        assert_eq!(r.iter().find(|r| r.key == Key::Wsl).unwrap().extra.as_deref(), Some("Other WSL machines: none"));
        f.other_distros = vec!["Ubuntu".into()];
        let r = rows(&f);
        assert!(!effective(&c, &r).has(Key::Wsl));
        assert!(r.iter().find(|r| r.key == Key::Wsl).unwrap().why.as_deref().unwrap().contains("Ubuntu"));
    }

    #[test]
    fn arguments_round_trip_and_the_old_form_still_works() {
        let r = rows(&fixture("lx-ch").unwrap());
        let c = default_choice(&r);
        let args = to_args(&c, &r);
        assert_eq!(args.join(" "), "uninstall --parts sentient,postgres,clickhouse,shortcuts --keep-files --keep-pg-data --keep-ch-data");
        assert_eq!(from_args(&args[1..]).unwrap(), effective(&c, &r));
        let old = effective(&from_args(&[]).unwrap(), &r);
        assert!(old.has(Key::KeepPgData) && old.has(Key::Postgres) && !old.has(Key::Manager));
        let wipe = from_args(&["--delete-data".into()]).unwrap();
        assert!(!wipe.has(Key::KeepPgData) && !wipe.has(Key::KeepFiles) && wipe.has(Key::WslDistro) && !wipe.has(Key::Manager));
        // On an existing PostgreSQL the old --delete-data drops SENTIENT's database.
        let ext = rows(&fixture("lx-ext").unwrap());
        assert_eq!(pg_action(&effective(&wipe, &ext)), Some(DbRemoval::StopUsing { keep_data: false }));
        assert!(from_args(&["--parts".into(), "sentient,bogus".into()]).unwrap_err().contains("unknown part bogus"));
    }

    #[test]
    fn sizes_parse() {
        assert_eq!(parse_pacman_size("Name : postgresql\nInstalled Size  : 56.20 MiB\n"), Some((56.20 * 1048576.) as u64));
        assert_eq!(parse_pacman_size("Installed Size : 1,5 GiB"), Some((1.5 * 1073741824.) as u64));
        assert_eq!(parse_docker_size("1.2GB"), Some(1_200_000_000));
        assert_eq!(parse_docker_size("0B"), Some(0));
    }

    #[test]
    fn every_fixture_draws_and_round_trips() {
        for name in FIXTURES {
            let f = fixture(name).unwrap();
            let back: Facts = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
            let r = rows(&back);
            assert_eq!(r[0].key, Key::Sentient, "{name}");
            assert_eq!(r.last().unwrap().key, Key::Manager, "{name}");
            let c = default_choice(&r);
            assert!(!c.has(Key::Manager), "{name}");
            assert!(summary(&c, &r).0.starts_with("Removes: "), "{name}");
        }
    }

    #[test]
    fn result_lines() {
        let checks = vec![
            Check { what: "No PostgreSQL process running".into(), ok: true, left: None, fix: None },
            Check { what: "Port 5432 (PostgreSQL) is free".into(), ok: true, left: None, fix: None },
            Check { what: "Port 8123 (ClickHouse) is free".into(), ok: true, left: None, fix: None },
            Check { what: "No postgresql.service service".into(), ok: true, left: None, fix: None },
            Check { what: "/etc/postgresql/17 is still there".into(), ok: false, left: None, fix: Some("sudo rm -rf '/etc/postgresql/17'".into()) },
        ];
        assert_eq!(checked_line(&checks).unwrap(), "Checked: no process, ports 5432, 8123 free, no units left");
        let r = rows(&fixture("lx-pg").unwrap());
        let m = done_message(&effective(&default_choice(&r), &r), &r, &checks, &[]);
        assert!(m.starts_with("Removed: SENTIENT server") && m.ends_with("1 thing is left: see the list."), "{m}");
    }

    #[test]
    fn a_stopped_postgres_is_unknown_never_none() {
        // Stopped: unknown, and the cluster is not even asked.
        assert_eq!(pg_others_when(true, || panic!("a stopped PostgreSQL is not asked")), Err(PG_STOPPED.to_string()));
        assert_eq!(pg_others_when(false, || Ok(vec!["erp".into()])), Ok(vec!["erp".to_string()]));
        // It survives the server's JSON (a remote Uninstall dialog).
        let mut f = fixture("lx-pg").unwrap();
        f.pg_others = Some(pg_others_when(true, || Ok(Vec::new())));
        let back: Facts = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert!(pg_unknown(&back));
    }

    #[test]
    fn an_unknown_postgres_stays_unless_switched_on() {
        let mut f = fixture("lx-pg").unwrap();
        f.pg_others = Some(Err(PG_STOPPED.into()));
        let r = rows(&f);
        assert_eq!(keys(&r), [Key::Sentient, Key::Postgres, Key::KeepPgData, Key::KeepFiles, Key::Gateway, Key::Shortcuts, Key::Manager]);
        let pg = r.iter().find(|r| r.key == Key::Postgres).unwrap();
        // Says why, and can still be picked.
        assert!(pg.why.is_none());
        assert!(pg.extra.as_deref().unwrap().contains("PostgreSQL is stopped"));
        let mut c = default_choice(&r);
        assert!(!c.has(Key::Postgres));
        assert_eq!(pg_action(&effective(&c, &r)), None);
        let (_, kept, _) = gone_and_kept(&c, &r);
        assert!(kept.contains(&"PostgreSQL".to_string()));
        // The operator's explicit choice.
        c.set(Key::Postgres, true);
        assert_eq!(rule(Key::Postgres, &c, &r), None);
        // Its data kept by default, as for any PostgreSQL Setup installed.
        assert_eq!(pg_action(&effective(&c, &r)), Some(DbRemoval::Remove { keep_data: true }));
        c.set(Key::KeepPgData, false);
        assert_eq!(pg_action(&effective(&c, &r)), Some(DbRemoval::Remove { keep_data: false }));
    }
}

/// This computer's name: what the dialog asks to be typed before data is deleted.
pub fn computer_name() -> String {
    std::env::var("COMPUTERNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .or_else(|| sentient_installer_core::host::host_command("hostname").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "this computer".into())
}
