// Copyright © 2016-2026 The SENTIENT Authors

//! Backup and restore of this installation's PostgreSQL database (and the
//! report and version-control file stores), on the backup engine
//! (`sentient-backup-core`): pick what to include, optionally encrypt, write
//! one `.sentient-backup` file with a checksum per member; restore into an
//! empty database.
//!
//! On a PostgreSQL + ClickHouse install the backup also holds ClickHouse's
//! telemetry, and a restore puts it in a new ClickHouse database of the same
//! name as the new PostgreSQL one, so neither touches the live data.
//!
//! A backup made on another database type is converted while it is restored
//! (`restore::TelemetryInto`), and its schema is matched to this SENTIENT's
//! (`restore_switch::match_schema`).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::{
    ActiveTheme, Disableable as _, IconName, h_flex, v_flex,
    button::ButtonVariants as _,
    checkbox::Checkbox,
    input::{Input, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    radio::RadioGroup,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use sentient_backup_core::backup::{self, BackupOptions, FileStoreSpec, Selection};
use sentient_backup_core::clickhouse::ChConn;
use sentient_backup_core::db::{self, CategoryReport, ConnConfig, DbInspector};
use sentient_backup_core::progress as bk;
use sentient_installer_core::progress::{Progress, ProgressFn};
use sentient_installer_core::stack::Flavor;

use crate::installation::InstallationRecord;
use crate::jobs::{Job, JobState};
use crate::ui::{self, Tone};

/// What the inspection found: the database and its categories with sizes.
struct Inspection {
    summary: SharedString,
    categories: Vec<CategoryReport>,
}

pub struct BackupPage {
    record: Option<InstallationRecord>,
    password: Entity<InputState>,
    /// ClickHouse installs only.
    ch_password: Entity<InputState>,
    inspection: Option<Inspection>,
    inspect_error: Option<SharedString>,
    include: HashSet<String>,
    /// 0 = all telemetry, 1 = last N days, 2 = none.
    telemetry: usize,
    telemetry_days: Entity<InputState>,
    passphrase: Entity<InputState>,
    output: Option<PathBuf>,
    backup: Entity<Job>,
    /// Replacing this instance's data with a backup (crate::replace).
    restore: Entity<Job>,
    /// The Platform Manager's backups: settings, the one being made, the
    /// folder's list and the last check (crate::backups).
    prefs: crate::backups::BackupPrefs,
    /// "Choose…": the tick list of parts (the earlier, by-hand backup).
    choose: bool,
    made: Option<crate::backups::Made>,
    /// The backup's run is shown in place of the page (crate::manager::jobview)
    /// until "Back to Backup and restore".
    shown_run: bool,
    list: Vec<crate::backups::Entry>,
    /// The backup being checked, and what the check found.
    checked: Option<(PathBuf, Option<Result<sentient_backup_core::restore::CheckReport, String>>)>,
    /// Sizes for the estimate: (database, ClickHouse, telemetry history in PostgreSQL), and free disk.
    sizes: Option<(u64, u64, u64, Option<u64>)>,
    /// The sizes were not read: SENTIENT is stopped (crate::dbstatus::STOPPED).
    sizes_stopped: bool,
    schedule_time: Entity<InputState>,
    /// Automatic backup is on (its switch).
    auto_on: bool,
    keep: Entity<InputState>,
    part_gb: Entity<InputState>,
    schedule_note: Option<(Tone, String)>,
    /// B5: "Saved" (or why not) beside the folder row ("folder") or the
    /// card's title ("card") after a change; the tick goes after a while.
    saved: Option<(&'static str, Result<(), String>)>,
    /// Free space where the backup folder is (read with the list).
    folder_free: Option<u64>,
    /// The stack's passwords are being read (Docker): Read the database
    /// waits for them (`inspect_waits`).
    secrets_pending: bool,
    inspect_waits: bool,
    _tasks: Vec<Task<()>>,
    _observe: Vec<Subscription>,
    /// The Restore flow, in place of the page while it is open.
    flow: Option<Entity<crate::manager::restore::RestoreFlow>>,
    /// A backup picked in Restore from…: the flow opens on the next draw.
    pending: Option<crate::manager::restore::Source>,
    /// The restore history (R1).
    history: Option<Result<Vec<crate::restore_flow::Entry>, String>>,
}

/// The database and ClickHouse passwords: a Docker stack's from its compose
/// file or containers (`docker`, read already), else the secret store's.
/// Blocking (a shell and docker): never on the UI thread.
fn stored_passwords(docker: Option<sentient_installer_core::distro::docker_ops::StackSecrets>) -> (String, String) {
    let db = docker.as_ref().map(|s| s.db_password.clone()).or_else(|| crate::secrets::get(crate::secrets::Secret::Database)).unwrap_or_default();
    let ch = docker
        .as_ref()
        .map(|s| s.ch_password.clone())
        .filter(|p| !p.is_empty())
        .or_else(|| crate::secrets::get(crate::secrets::Secret::Clickhouse))
        .unwrap_or_default();
    (db, ch)
}

/// Forward the backup engine's progress into a job's sink.
fn bridge(sink: ProgressFn) -> bk::ProgressFn {
    Arc::new(move |p| {
        sink(match p {
            bk::Progress::Step { name, index, total } => Progress::Step { name: format!("{name} ({index}/{total})") },
            bk::Progress::Log { line } => Progress::Log { line },
            bk::Progress::Percent { value } => Progress::Percent { value },
            bk::Progress::Done { message } => Progress::Done { message },
        })
    })
}

/// The engine is async on tokio; a job thread runs it on its own runtime.
fn block_on<F: std::future::Future>(future: F) -> Result<F::Output, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map(|rt| rt.block_on(future))
        .map_err(|e| format!("start the backup runtime: {e}"))
}

impl BackupPage {
    pub fn new(record: Option<InstallationRecord>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Docker keeps the stack's passwords in its compose file (or its
        // containers): read behind the page, never while it opens (a shell
        // and up to two docker calls, 0.3–0.6 s on the UI thread before).
        let docker_stack = record.as_ref().filter(|r| r.engine == crate::installation::Engine::Docker).map(|r| r.stack.clone());
        let (saved, saved_ch) = if docker_stack.is_some() { (String::new(), String::new()) } else { stored_passwords(None) };
        let password = cx.new(|cx| InputState::new(window, cx).masked(true).default_value(saved));
        let ch_password = cx.new(|cx| InputState::new(window, cx).masked(true).default_value(saved_ch));
        let telemetry_days = cx.new(|cx| InputState::new(window, cx).default_value("30"));
        let passphrase = cx.new(|cx| {
            InputState::new(window, cx).masked(true).placeholder("Optional: encrypt the backup")
        });
        let prefs = crate::backups::BackupPrefs::load();
        let prefs_folder = prefs.folder.clone();
        let schedule_time = cx.new(|cx| InputState::new(window, cx).placeholder("23:30").default_value(prefs.daily_at.clone().unwrap_or_default()));
        let keep = cx.new(|cx| InputState::new(window, cx).default_value(prefs.keep.to_string()));
        let part_gb = cx.new(|cx| InputState::new(window, cx).default_value(prefs.part_gb.to_string()));
        let backup = cx.new(|_| Job::new());
        let restore = cx.new(|_| Job::new());
        let mut _observe = vec![
            cx.observe(&backup, |page: &mut Self, job, cx| {
                if !job.read(cx).is_running() {
                    page.refresh_list();
                }
                cx.notify();
            }),
            cx.observe(&restore, |_, _, cx| cx.notify()),
        ];
        _observe.push(cx.subscribe(&schedule_time, |_, _, _: &gpui_kit::component::input::InputEvent, cx| cx.notify()));
        // The note beside an empty passphrase comes and goes with it.
        _observe.push(cx.subscribe(&passphrase, |_, _, _: &gpui_kit::component::input::InputEvent, cx| cx.notify()));
        // Keep: saved on Enter or when the field is left (1 to 365).
        _observe.push(cx.subscribe_in(&keep, window, |page: &mut Self, input, e: &gpui_kit::component::input::InputEvent, window, cx| {
            use gpui_kit::component::input::InputEvent;
            if matches!(e, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                match keep_value(&input.read(cx).value()) {
                    Some(k) => {
                        if input.read(cx).value().trim() != k.to_string() {
                            input.update(cx, |s, cx| s.set_value(k.to_string(), window, cx));
                        }
                        if k != page.prefs.keep {
                            page.save_prefs(true, cx);
                        }
                    }
                    None => {
                        let k = page.prefs.keep.to_string();
                        input.update(cx, |s, cx| s.set_value(k, window, cx));
                    }
                }
            }
            cx.notify();
        }));
        // Split files over: saved on Enter or when the field is left.
        _observe.push(cx.subscribe(&part_gb, |page: &mut Self, input, e: &gpui_kit::component::input::InputEvent, cx| {
            use gpui_kit::component::input::InputEvent;
            if matches!(e, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                let gb: u64 = input.read(cx).value().trim().parse().unwrap_or(page.prefs.part_gb);
                if gb != page.prefs.part_gb {
                    page.prefs.part_gb = gb;
                    page.save_prefs(false, cx);
                }
            }
            cx.notify();
        }));
        let mut page = Self {
            record,
            password,
            ch_password,
            inspection: None,
            inspect_error: None,
            include: HashSet::new(),
            telemetry: 0,
            telemetry_days,
            passphrase,
            output: None,
            backup,
            restore,
            list: crate::backups::list_all(&prefs),
            auto_on: prefs.daily_at.is_some(),
            prefs,
            choose: false,
            made: None,
            shown_run: false,
            checked: None,
            sizes: None,
            sizes_stopped: false,
            schedule_time,
            keep,
            part_gb,
            schedule_note: None,
            // Screens: "Saved" shown on a section (SPM_DEBUG_SAVED=folder).
            saved: std::env::var("SPM_DEBUG_SAVED").ok().filter(|_| cfg!(debug_assertions)).map(|s| (&*s.leak(), Ok(()))),
            folder_free: crate::backups::free_space(&prefs_folder),
            secrets_pending: docker_stack.is_some(),
            inspect_waits: false,
            _tasks: Vec::new(),
            _observe,
            flow: None,
            pending: None,
            history: None,
        };
        // Enter: the passwords end on Read the database; the telemetry days
        // and passphrase on Back up now.
        let pw = [page.password.clone(), page.ch_password.clone()];
        let subs = crate::ui::enter_moves_on(
            &pw,
            window,
            cx,
            |p: &Self, _| {
                let ch = p.record.as_ref().is_some_and(|r| r.flavor() == Flavor::Clickhouse);
                let mut v = vec![p.password.clone()];
                if ch {
                    v.push(p.ch_password.clone());
                }
                v
            },
            |p: &mut Self, _, cx| {
                if !(p.backup.read(cx).is_running() || p.restore.read(cx).is_running()) {
                    p.inspect(cx);
                }
                None
            },
        );
        page._observe.extend(subs);
        let subs = crate::ui::enter_moves_on(
            &[page.telemetry_days.clone(), page.passphrase.clone()],
            window,
            cx,
            |p: &Self, _| {
                let mut v = Vec::new();
                if p.telemetry == 1 {
                    v.push(p.telemetry_days.clone());
                }
                v.push(p.passphrase.clone());
                v
            },
            |p: &mut Self, _, cx| {
                let busy = p.backup.read(cx).is_running() || p.restore.read(cx).is_running();
                if !busy && p.inspection.is_some() && p.output.is_some() {
                    p.run_backup(cx);
                }
                None
            },
        );
        page._observe.extend(subs);
        page.read_sizes(cx);
        page.read_history(cx);
        if let Some(stack) = docker_stack {
            page.read_secrets(stack, window, cx);
        }
        #[cfg(debug_assertions)]
        if let Ok(frame) = std::env::var("SPM_DEBUG_RESTORE") {
            page.pending = Some(crate::manager::restore::canned::source(&frame));
        }
        // Development aid: open the Restore flow on a file (screens, click tests).
        #[cfg(debug_assertions)]
        if let Some(f) = std::env::var_os("SPM_DEBUG_RESTORE_FILE") {
            let file = PathBuf::from(f);
            let sub = file.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            page.pending = Some(crate::manager::restore::Source { file, sub, note: None });
        }
        page
    }

    fn conn(&self, dbname: Option<String>, cx: &App) -> Option<ConnConfig> {
        let st = &self.record.as_ref()?.stack;
        Some(ConnConfig {
            host: "127.0.0.1".into(),
            port: st.pg_port,
            dbname: dbname.unwrap_or_else(|| st.db_name.clone()),
            user: st.db_user.clone(),
            password: self.password.read(cx).value().to_string(),
        })
    }

    /// This install's ClickHouse, when it has one, signed in as SENTIENT's user.
    fn ch_conn(&self, database: Option<String>, cx: &App) -> Option<ChConn> {
        let record = self.record.as_ref()?;
        let st = &record.stack;
        (record.flavor() == Flavor::Clickhouse).then(|| ChConn {
            url: format!("http://127.0.0.1:{}", st.ch_http_port),
            database: database.unwrap_or_else(|| st.ch_db.clone()),
            user: st.ch_user.clone(),
            password: self.ch_password.read(cx).value().to_string(),
        })
    }

    fn inspect(&mut self, cx: &mut Context<Self>) {
        // The passwords are still being read: once they are.
        if self.secrets_pending {
            self.inspect_waits = true;
            return;
        }
        let Some(conn) = self.conn(None, cx) else { return };
        self.inspect_error = None;
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    block_on(async move {
                        let db = DbInspector::connect(&conn).await.map_err(|e| e.to_string())?;
                        let server = db.server_info().await.map_err(|e| e.to_string())?;
                        let tables = db.tables_with_true_sizes().await.map_err(|e| e.to_string())?;
                        Ok::<_, String>((server, db::build_report(&tables)))
                    })?
                })
                .await;
            this.update(cx, |page, cx| {
                match result {
                    Ok((server, categories)) => {
                        page.include = categories
                            .iter()
                            .filter(|c| c.default_selected || c.locked)
                            .map(|c| c.id.clone())
                            .collect();
                        let total: i64 = categories.iter().map(|c| c.bytes).sum();
                        page.inspection = Some(Inspection {
                            summary: format!(
                                "{} · {}{} · {}",
                                server.database,
                                // version() reads "PostgreSQL 18.6 on x86_64-…, compiled by …".
                                server.postgres_version.split(" on ").next().unwrap_or_default(),
                                server.timescaledb_version.map(|v| format!(" · TimescaleDB {v}")).unwrap_or_default(),
                                db::human_bytes(total),
                            )
                            .into(),
                            categories,
                        });
                    }
                    Err(e) => page.inspect_error = Some(format!("Could not read the database: {e}").into()),
                }
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    fn choose_output(&mut self, cx: &mut Context<Self>) {
        let dir = if self.prefs.folder.is_dir() { self.prefs.folder.clone() } else { std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(std::path::PathBuf::from).unwrap_or_default() };
        let name = format!("sentient-{}.sentient-backup", chrono::Local::now().format("%Y%m%d-%H%M"));
        let rx = crate::chooser::save(cx, "Save the backup as", &dir, Some(&name));
        let task = cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(mut path))) = rx.await else { return };
            // The Windows dialog drops an extension it does not know.
            if path.extension().and_then(|e| e.to_str()) != Some("sentient-backup") {
                let name = format!("{}.sentient-backup", path.file_name().unwrap_or_default().to_string_lossy());
                path.set_file_name(name);
            }
            this.update(cx, |page, cx| {
                page.output = Some(path);
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    fn run_backup(&mut self, cx: &mut Context<Self>) {
        let (Some(conn), Some(output), Some(inspection)) = (self.conn(None, cx), self.output.clone(), &self.inspection)
        else {
            return;
        };
        let skip: Vec<String> = inspection
            .categories
            .iter()
            .filter(|c| !self.include.contains(&c.id))
            .map(|c| c.id.clone())
            .collect();
        let mut selection = Selection::skipping(&skip);
        match self.telemetry {
            1 => selection.telemetry_days = self.telemetry_days.read(cx).value().trim().parse().ok(),
            2 => {
                selection.include.remove("telemetry_historical");
            }
            _ => {}
        }
        let state_dir = self.record.as_ref().map(|r| r.state_dir.clone()).unwrap_or_default();
        let file_stores = inspection
            .categories
            .iter()
            .filter(|c| selection.is_included(&c.id))
            .filter_map(|c| {
                let id = c.file_store_id.clone()?;
                let path = state_dir.join(&id);
                path.is_dir().then(|| FileStoreSpec { id, category_id: c.id.clone(), path })
            })
            .collect();
        let passphrase = Some(self.passphrase.read(cx).value().to_string()).filter(|p| !p.is_empty());
        let clickhouse = self.ch_conn(None, cx);
        let mut opts = BackupOptions { output, selection, file_stores, zstd_level: 10, passphrase, clickhouse, origin: Default::default(), part_size: None };
        let record = self.record.clone();
        self.shown_run = true;
        self.made = None;
        self.backup.update(cx, |job, cx| {
            job.set_plan(crate::jobplan::backup_file());
            job.set_label("Backing up");
            job.start(
                move |sink| {
                    if let Some(r) = &record {
                        opts.origin = crate::backup_jobs::origin(r, &conn);
                    }
                    let summary = block_on(backup::run(&conn, &opts, bridge(sink.clone())))?
                        .map_err(|e| e.to_string())?;
                    if let Some(record) = &record {
                        crate::secrets_key::save_beside(record, &summary.output, opts.passphrase.as_deref(), &sink);
                    }
                    // For the Status screen's "Last backup".
                    crate::dbstatus::record_backup(&summary.output);
                    sink(Progress::Done {
                        message: format!(
                            "Saved {} ({}), checksum {}…",
                            summary.output.display(),
                            db::human_bytes(summary.archive_bytes as i64),
                            summary.dump_sha256.get(..12).unwrap_or_default()
                        ),
                    });
                    Ok(())
                },
                cx,
            )
        });
    }

    fn read_history(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        #[cfg(debug_assertions)]
        if std::env::var("SPM_DEBUG_RESTORE_HISTORY").is_ok_and(|v| v == "canned") {
            self.history = Some(Ok(crate::manager::restore::canned::history()));
            return;
        }
        let task = cx.spawn(async move |this, cx| {
            let h = cx.background_executor().spawn(async move { crate::manager::restore::Target::Here(record).history_pub() }).await;
            this.update(cx, |p, cx| {
                p.history = Some(h);
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    /// The Restore flow for `src`, in place of the page.
    fn open_flow(&mut self, src: crate::manager::restore::Source, window: &mut Window, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        let flow = cx.new(|cx| crate::manager::restore::RestoreFlow::new(crate::manager::restore::Target::Here(record), src, window, cx));
        self._observe.push(cx.subscribe(&flow, |p: &mut Self, _, _: &crate::manager::restore::Leave, cx| {
            p.flow = None;
            p.refresh_list();
            p.read_history(cx);
            cx.notify();
        }));
        self.flow = Some(flow);
        cx.notify();
    }

    /// Restore from… (R4).
    fn restore_from(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        let this = cx.entity().downgrade();
        crate::manager::restore::RestoreFrom::open(
            crate::manager::restore::Target::Here(record),
            self.prefs.folder.clone(),
            self.folder_free,
            std::rc::Rc::new(move |src, cx| {
                this.update(cx, |p, cx| {
                    p.pending = Some(src);
                    cx.notify();
                })
                .ok();
            }),
            cx,
        );
    }

    #[cfg(debug_assertions)]
    fn debug_dialogs(&mut self, cx: &mut Context<Self>) {
        static SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if std::env::var("SPM_DEBUG_RESTORE_FROM").is_ok() && !SHOWN.swap(true, std::sync::atomic::Ordering::SeqCst) {
            self.restore_from(cx);
        }
        if std::env::var("SPM_DEBUG_RESTORE_DETAILS").is_ok() && !SHOWN.swap(true, std::sync::atomic::Ordering::SeqCst) {
            if let (Some(record), Some(Ok(h))) = (self.record.clone(), &self.history) {
                if let Some(e) = h.first().cloned() {
                    crate::manager::restore::HistoryDialog::open(crate::manager::restore::Target::Here(record), e, cx);
                }
            }
        }
    }

    fn refresh_list(&mut self) {
        self.list = crate::backups::list_all(&self.prefs);
        self.folder_free = crate::backups::free_space(&self.prefs.folder);
    }

    /// "Saved" after a change of `section` (it fades in), or why not.
    fn saved_note(&self, theme: &gpui_kit::component::Theme, section: &'static str) -> Option<AnyElement> {
        match &self.saved {
            Some((s, Ok(()))) if *s == section => Some(
                h_flex()
                    .flex_shrink_0()
                    .gap_1()
                    .items_center()
                    .text_sm()
                    .text_color(theme.success_foreground)
                    .child(gpui_kit::component::Icon::new(IconName::Check).size_4())
                    .child("Saved")
                    .with_animation(SharedString::from(format!("{section}-saved")), Animation::new(std::time::Duration::from_millis(150)).with_easing(crate::ui::ease_out), |el, t| el.opacity(t))
                    .into_any_element(),
            ),
            Some((s, Err(e))) if *s == section => Some(div().min_w_0().text_sm().text_color(theme.danger).overflow_hidden().whitespace_nowrap().text_ellipsis().child(format!("Not saved: {e}")).into_any_element()),
            _ => None,
        }
    }

    /// What to back up: a preset, or Choose… (the parts, saved anywhere).
    fn set_what(&mut self, key: &str, cx: &mut Context<Self>) {
        use crate::backups::{History, Preset};
        if key == "choose" {
            self.choose = true;
            if self.inspection.is_none() {
                self.inspect(cx);
            }
            cx.notify();
            return;
        }
        (self.prefs.preset, self.prefs.history) = match key {
            "last90" => (Preset::Everything, History::LastDays(90)),
            "configuration" => (Preset::Configuration, self.prefs.history),
            _ => (Preset::Everything, History::All),
        };
        self.choose = false;
        self.save_prefs(false, cx);
    }

    /// Show "Saved" (or why not) for `section`; a tick goes after 4 s.
    fn show_saved(&mut self, section: &'static str, r: Result<(), String>, cx: &mut Context<Self>) {
        let ok = r.is_ok();
        self.saved = Some((section, r));
        if ok {
            let hide = cx.spawn(async move |this, cx| {
                cx.background_executor().timer(std::time::Duration::from_secs(4)).await;
                this.update(cx, |p, cx| {
                    if p.saved.as_ref().is_some_and(|(s, r)| *s == section && r.is_ok()) {
                        p.saved = None;
                        cx.notify();
                    }
                })
                .ok();
            });
            self._tasks.push(hide);
        }
        cx.notify();
    }

    /// The database's sizes and free disk, for the estimate (background).
    /// A Docker stack's passwords, read behind the page; they fill the
    /// fields left empty, then a Read the database asked for meanwhile runs.
    fn read_secrets(&mut self, stack: sentient_installer_core::distro::DeployConfig, window: &mut Window, cx: &mut Context<Self>) {
        let task = cx.spawn_in(window, async move |this, cx| {
            let (db, ch) = cx.background_executor().spawn(async move { stored_passwords(sentient_installer_core::distro::docker_ops::read_secrets(&stack).ok()) }).await;
            this.update_in(cx, |p, window, cx| {
                for (input, value) in [(p.password.clone(), db), (p.ch_password.clone(), ch)] {
                    if !value.is_empty() && input.read(cx).value().is_empty() {
                        input.update(cx, |s, cx| s.set_value(value, window, cx));
                    }
                }
                p.secrets_pending = false;
                if std::mem::take(&mut p.inspect_waits) {
                    p.inspect(cx);
                }
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    fn read_sizes(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        if crate::dbstatus::needs_admin(&record) {
            return;
        }
        let task = cx.spawn(async move |this, cx| {
            let st = cx.background_executor().spawn(async move { sentient_installer_core::distro::idle::reading(|| crate::dbstatus::read(&record)) }).await;
            this.update(cx, |page, cx| {
                page.sizes_stopped = st.stopped;
                if let Some(pg) = &st.pg {
                    let history = pg.tables.iter().find(|t| t.name == "ts_kv").map(|t| t.bytes).unwrap_or(0);
                    page.sizes = Some((pg.db_bytes, st.ch.as_ref().map(|c| c.disk_bytes).unwrap_or(0), history, pg.free_bytes));
                }
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    /// Back up now with the chosen preset (crate::backups::run).
    pub(crate) fn run_preset_backup(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        let prefs = self.prefs.clone();
        let pass = Some(self.passphrase.read(cx).value().to_string()).filter(|p| !p.is_empty());
        self.made = None;
        self.shown_run = true;
        let plan = crate::jobplan::backup(&prefs, false, pass.is_some());
        let made = cx.entity().downgrade();
        let (tx, rx) = std::sync::mpsc::channel::<crate::backups::Made>();
        let me = cx.entity();
        self.backup.update(cx, |job, cx| {
            job.set_plan(plan);
            job.set_label("Backing up");
            job.set_on_page(true);
            job.set_details(std::rc::Rc::new(move |cx| BackupDialog::open(me.clone(), false, "Backup".into(), cx)));
            job.start(
                move |sink| {
                    // Native Linux: the database password is root-only.
                    if crate::rootjob::needed(&record) {
                        let mut args = vec!["backup-now".to_string()];
                        if pass.is_some() {
                            args.push("--passphrase-stdin".into());
                        }
                        return crate::rootjob::run_with_input(&args, pass.as_deref(), &sink).map(|_| ());
                    }
                    let m = crate::backups::run(&record, &prefs, pass, false, sink)?;
                    let _ = tx.send(m);
                    Ok(())
                },
                cx,
            )
        });
        let task = cx.spawn(async move |_, cx| {
            let got = cx.background_executor().spawn(async move { rx.recv().ok() }).await;
            if let Some(m) = got {
                made.update(cx, |page, cx| {
                    page.made = Some(m);
                    cx.notify();
                })
                .ok();
            }
        });
        self._tasks.push(task);
        cx.notify();
    }

    fn run_check(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let pass = Some(self.passphrase.read(cx).value().to_string()).filter(|p| !p.is_empty());
        self.checked = Some((path.clone(), None));
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        BackupDialog::open(cx.entity(), true, format!("Verify {name}"), cx);
        let task = cx.spawn(async move |this, cx| {
            let p = path.clone();
            let r = cx.background_executor().spawn(async move { crate::backups::check(&p, pass.as_deref()) }).await;
            this.update(cx, |page, cx| {
                page.checked = Some((path, Some(r)));
                page.refresh_list();
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
        cx.notify();
    }

    fn confirm_delete(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        crate::modal::alert(window, cx, move |dialog, _, _| {
            let this = this.clone();
            let path = path.clone();
            dialog.title(format!("Delete {name}?")).description("The backup file (all its parts) and its .sha256 are deleted. This cannot be undone.").confirm().ok_text("Delete").on_ok(move |_, _, cx| {
                let path = path.clone();
                this.update(cx, |page, cx| {
                    page.schedule_note = crate::backups::delete(&path).err().map(|e| (Tone::Bad, e));
                    page.refresh_list();
                    cx.notify();
                })
                .ok();
                true
            })
        });
    }

    fn choose_folder(&mut self, second: bool, cx: &mut Context<Self>) {
        let now = if second { self.prefs.copy_to.clone() } else { Some(self.prefs.folder.clone()) };
        let start = now.filter(|d| d.is_dir());
        let rx = crate::chooser::open(
            cx,
            crate::chooser::Ask { title: if second { "Choose where to copy backups" } else { "Choose the backup folder" }, start: start.as_deref(), directory: true },
            PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Use this folder".into()) },
        );
        let task = cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(dir) = paths.into_iter().next() else { return };
            this.update(cx, |page, cx| {
                if second { page.prefs.copy_to = Some(dir) } else { page.prefs.folder = dir }
                page.save_prefs_in(if second { "card" } else { "folder" }, false, cx);
                page.refresh_list();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    /// B2–B4 on this computer: the chooser, then the folder is changed
    /// (the backups there moved along or left).
    fn change_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let here: Vec<&crate::backups::Entry> = self.list.iter().filter(|e| !e.old_folder).collect();
        let this = cx.entity().downgrade();
        crate::manager::folder_chooser::open(
            crate::manager::folder_chooser::Opening {
                target: crate::manager::folder_chooser::Target::Local,
                on: "this computer".into(),
                current: self.prefs.folder.clone(),
                here: (here.len(), here.iter().map(|e| e.bytes).sum()),
                keep: self.prefs.keep,
                full_bytes: self.list.first().map(|e| e.bytes),
                done: std::rc::Rc::new(move |dir, move_them, cx| {
                    this.update(cx, |page, cx| page.set_folder(dir, move_them, cx)).ok();
                }),
                title: None,
            },
            window,
            cx,
        );
    }

    fn set_folder(&mut self, dir: PathBuf, move_them: bool, cx: &mut Context<Self>) {
        crate::manager::strip::quiet(true, cx);
        let task = cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { crate::backups::set_folder(&dir, move_them, &sentient_installer_core::progress::noop()) }).await;
            let _ = cx.update(|cx| crate::manager::strip::quiet(false, cx));
            this.update(cx, |page, cx| {
                page.prefs = crate::backups::BackupPrefs::load();
                page.refresh_list();
                page.show_saved("folder", r.map(|_| ()), cx);
            })
            .ok();
        });
        self._tasks.push(task);
        cx.notify();
    }

    fn add_backup_file(&mut self, cx: &mut Context<Self>) {
        let start = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(std::path::PathBuf::from);
        let rx = crate::chooser::open(
            cx,
            crate::chooser::Ask { title: "Choose a backup file to add", start: start.as_deref(), directory: false },
            PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Add this backup".into()) },
        );
        let folder = self.prefs.folder.clone();
        let task = cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(file) = paths.into_iter().next() else { return };
            let r = cx.background_executor().spawn(async move { crate::backups::add(&file, &folder) }).await;
            this.update(cx, |page, cx| {
                page.schedule_note = Some(match r {
                    Ok(p) => (Tone::Good, format!("Added {}", p.display())),
                    Err(e) => (Tone::Bad, e),
                });
                page.refresh_list();
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    /// Save the backup settings; with `schedule`, also (re)register the
    /// daily backup.
    fn save_prefs(&mut self, schedule: bool, cx: &mut Context<Self>) {
        self.save_prefs_in("card", schedule, cx);
    }

    fn save_prefs_in(&mut self, section: &'static str, schedule: bool, cx: &mut Context<Self>) {
        if schedule {
            let t = self.schedule_time.read(cx).value().trim().to_string();
            let t = if t.is_empty() { "23:30".to_string() } else { t };
            self.prefs.daily_at = self.auto_on.then_some(t);
            self.prefs.keep = keep_value(&self.keep.read(cx).value()).unwrap_or(self.prefs.keep);
            self.prefs.part_gb = self.part_gb.read(cx).value().trim().parse().unwrap_or(4);
        }
        let prefs = self.prefs.clone();
        let record = self.record.clone();
        // A small save: the thin line under the top bar, then the note here.
        crate::manager::strip::quiet(true, cx);
        let task = cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move {
                    prefs.save()?;
                    if !schedule {
                        return Ok(None);
                    }
                    // Saved by the user: it may ask for the password, once.
                    let _action = sentient_installer_core::elevation::user_action();
                    match record.as_ref().filter(|r| crate::rootjob::needed(r)) {
                        Some(_) => crate::rootjob::run(&["backup-schedule".to_string()], &sentient_installer_core::progress::noop()).map(|_| ()),
                        None => crate::backups::apply_schedule(&prefs),
                    }?;
                    Ok::<_, String>(Some(match crate::backups::next_run(&prefs) {
                        Some(n) => format!("Saved. The next automatic backup is {n}."),
                        None => "Saved. No automatic backup.".to_string(),
                    }))
                })
                .await;
            let _ = cx.update(|cx| crate::manager::strip::quiet(false, cx));
            this.update(cx, |page, cx| {
                page.show_saved(section, r.map(|_| ()), cx);
            })
            .ok();
        });
        self._tasks.push(task);
        cx.notify();
    }

}

impl BackupPage {
    /// J3/J4: a backup's run and its result, in the shared job view: on the
    /// page (`on_page` with its actions) and in the strip's Details.
    fn run_view(&self, theme: &gpui_kit::component::Theme, actions: Vec<AnyElement>, cx: &App) -> Option<Div> {
        let job = self.backup.read(cx);
        if matches!(job.state(), JobState::Idle) {
            return None;
        }
        let mut view = crate::manager::jobview::JobView::new("bk-run", format!("Backing up to {}", self.prefs.folder.display()), &self.backup)
            .cancel(crate::manager::jobview::Cancel::Cannot("A backup cannot be stopped half-way. SENTIENT keeps running while it is made."));
        if let (JobState::Done(_), Some(m)) = (job.state(), &self.made) {
            let name = m.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let whole = m.files.first().map(|(_, h, _)| h.clone()).unwrap_or_default();
            let short = if whole.len() > 16 { format!("{}…{}", &whole[..8], &whole[whole.len() - 8..]) } else { whole.clone() };
            view = view
                .result(ui::outcome(theme, Tone::Good, format!("Backup finished and checked: {name}, {}.", crate::dbstatus::bytes_label(m.bytes))))
                .extra(
                    v_flex()
                        .gap_2()
                        .child(brow(theme, "Each part", None, div().text_sm().child(format!("SHA-256 stored inside the file, {} parts", m.members))))
                        .child(if m.files.len() == 1 {
                            brow(
                                theme,
                                "Whole file SHA-256",
                                None,
                                h_flex()
                                    .gap(crate::ui::button_gap())
                                    .child(div().text_sm().font_family(theme.mono_font_family.clone()).child(short))
                                    .child(crate::ui::button("copy-hash").outline().label("Copy").on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(whole.clone())))),
                            )
                        } else {
                            brow(theme, "Files", None, div().text_sm().child(format!("{} parts, each with its SHA-256", m.files.len())))
                        })
                        .child(brow(theme, "Saved beside it", None, div().text_sm().child(m.sidecar.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())))
                        .child(brow(theme, "Read back", None, div().text_sm().child(m.check.summary.clone())))
                        .when_some(m.copied_to.clone(), |c, p| c.child(brow(theme, "Second copy", None, div().text_sm().child(p.display().to_string())))),
                );
        }
        view = view.actions(actions);
        Some(view.render(cx))
    }

    /// The page's actions under a backup's result: back, then the primary.
    fn run_actions(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let back = crate::ui::button("bk-run-back").outline().label("Back to Backup and restore").on_click(cx.listener(|p, _, _, cx| {
            p.shown_run = false;
            p.backup.update(cx, |j, cx| j.reset(cx));
            p.refresh_list();
            cx.notify();
        }));
        let next = match self.backup.read(cx).state() {
            JobState::Done(_) => {
                let folder = self.prefs.folder.display().to_string();
                Some(crate::ui::button("open-folder").primary().label("Open folder").on_click(move |_, _, cx| ui::open_url(&folder, cx)).into_any_element())
            }
            JobState::Failed(_) => Some(crate::ui::button("bk-run-again").primary().label("Try again").on_click(cx.listener(|p, _, _, cx| p.run_preset_backup(cx))).into_any_element()),
            _ => None,
        };
        std::iter::once(back.into_any_element()).chain(next).collect()
    }

    /// J6: a check's result, part by part (in its dialog).
    fn check_view(&self, theme: &gpui_kit::component::Theme) -> Option<Div> {
        if let Some((path, result)) = &self.checked {
            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let mut c = v_flex().gap_2();
            match result {
                None => c = c.child(div().text_sm().text_color(theme.muted_foreground).child(format!("Checking {name}: reading every part…"))),
                Some(Err(e)) => c = c.child(ui::outcome(theme, Tone::Bad, format!("{name} could not be checked: {e}"))),
                Some(Ok(r)) => {
                    let n = r.members.len() + r.missing.len();
                    c = c.child(ui::outcome(
                        theme,
                        if r.ok() { Tone::Good } else { Tone::Bad },
                        if r.ok() { format!("{name}: all {n} parts match.") } else { format!("{name} is damaged. Do not rely on this file.") },
                    ));
                    let mark = |ok: bool| ui::StepMark::from_ok(ok);
                    let mut rows: Vec<(String, ui::StepMark)> = Vec::new();
                    if let Some(files) = &r.files {
                        let all = files.iter().all(|(_, ok)| *ok);
                        rows.push((format!("File{} against its .sha256", if files.len() > 1 { "s" } else { "" }), mark(all)));
                    }
                    for m in &r.members {
                        rows.push((format!("{} ({})", m.label, crate::dbstatus::bytes_label(m.bytes)), mark(m.ok)));
                    }
                    for missing in &r.missing {
                        rows.push((format!("{}: missing", sentient_backup_core::restore::member_label(missing)), ui::StepMark::Failed));
                    }
                    c = c.child(ui::steps(theme, &rows));
                }
            }
            return Some(c);
        }
        None
    }
}

/// A Backup page result in the shared dialog, following the page.
struct BackupDialog {
    page: Entity<BackupPage>,
    check: bool,
    _observe: Subscription,
}

impl BackupDialog {
    fn open(page: Entity<BackupPage>, check: bool, title: String, cx: &mut App) {
        let view = cx.new(|cx| BackupDialog { _observe: cx.observe(&page, |_, _, cx| cx.notify()), page, check });
        crate::modal::open(crate::modal::Modal { title: title.into(), body: view.into(), width: 640., icon: None, on_close: None }, cx);
    }
}

impl Render for BackupDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let page = self.page.read(cx);
        let body = if self.check { page.check_view(&theme) } else { page.run_view(&theme, Vec::new(), cx) };
        v_flex()
            .gap_3()
            .children(body)
            .child(h_flex().justify_end().child(crate::ui::button("bk-dialog-close").primary().label("Close").on_click(|_, _, cx| crate::modal::close(cx))))
    }
}


/// A row: label, then its control.
fn brow(theme: &gpui_kit::component::Theme, label: &'static str, help: Option<&'static str>, control: impl IntoElement) -> Div {
    // On a narrow window the control moves under the label, right-aligned.
    h_flex()
        .flex_wrap()
        .gap_x_4()
        .gap_y_1()
        .items_center()
        .py_2()
        .border_t_1()
        .border_color(theme.border)
        // The mockup's row: label and its line on the left, the control
        // right-aligned.
        .child(
            // Help as an (i) tooltip beside the label, never an inline line.
            h_flex()
                .flex_1()
                .min_w(px(180.))
                .gap_1()
                .items_center()
                .child(div().text_sm().child(label))
                .when_some(help, |c, h| c.child(crate::ui::button(SharedString::from(format!("bhelp-{label}"))).ghost().icon(IconName::Info).tooltip(h))),
        )
        .child(h_flex().flex_shrink_0().ml_auto().max_w(px(460.)).gap_2().items_center().justify_end().child(control))
}

impl BackupPage {
    /// J2–J6: the last backup, back up now, the automatic backup, and the
    /// backups in the folder with their checks.
    fn backup_panels(&self, theme: &gpui_kit::component::Theme, narrow: bool, busy: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        use crate::backups::{History, Preset};
        use crate::dbstatus::bytes_label;
        let mut out = Vec::new();
        let p = &self.prefs;

        // B1: the Backup card, as a server's, with this computer's extra
        // rows (encryption, a second copy, split files). A change is saved
        // at once; "Saved" shows beside the card's title or the folder.
        let folder_row = card_row(
            theme,
            narrow,
            "Backup folder",
            None,
            folder_value(theme, &p.folder, self.folder_free, self.saved_note(theme, "folder")),
            Some(change_button("folder", busy).on_click(cx.listener(|this, _, window, cx| this.change_folder(window, cx))).into_any_element()),
            false,
        );
        let on = self.auto_on;
        let time = {
            let t = self.schedule_time.read(cx).value().trim().to_string();
            if t.is_empty() { "02:00".to_string() } else { t }
        };
        let auto_row = card_row(theme, narrow, "Automatic backup", Some("A backup every day at this time, on this computer's clock."), {
            let this = cx.entity().downgrade();
            h_flex()
                .gap_3()
                .items_center()
                .child(gpui_kit::component::switch::Switch::new("auto-on").checked(on).disabled(busy).on_click(cx.listener(|this, v: &bool, _, cx| {
                    this.auto_on = *v;
                    this.save_prefs(true, cx);
                })))
                .child(
                    crate::ui::button("auto-time")
                        .ghost()
                        
                        .disabled(!on || busy)
                        .label(if on { format!("Every day at {time}") } else { "Off".to_string() })
                        .dropdown_menu(move |menu, _, _| {
                            let mut menu = menu;
                            for h in 0..24 {
                                for m in [0, 30] {
                                    let t = format!("{h:02}:{m:02}");
                                    let (this, t2) = (this.clone(), t.clone());
                                    menu = menu.item(PopupMenuItem::new(t).on_click(move |_, window, cx| {
                                        let t3 = t2.clone();
                                        this.update(cx, |p, cx| {
                                            p.schedule_time.update(cx, |s, cx| s.set_value(t3, window, cx));
                                            p.save_prefs(true, cx);
                                        })
                                        .ok();
                                    }));
                                }
                            }
                            menu
                        }),
                )
        }, None, false);
        let keep_row = card_row(theme, narrow, "Keep", Some("Older automatic backups are deleted, across the folder and any folder used before."), keep_field(theme, &self.keep, busy), None, false);
        let seg_theme = cx.theme().clone();
        let what = match (p.preset, p.history) {
            (Preset::Configuration, _) => "configuration",
            (_, History::LastDays(_)) => "last90",
            _ => "everything",
        };
        // Wide: the mockup's segments; narrow: the same as a menu.
        let options = [("everything", "Everything"), ("last90", "Last 90 days"), ("configuration", "Configuration only"), ("choose", "Choose")];
        let on = |key: &str| if key == "choose" { self.choose } else { what == key && !self.choose };
        let what_control = if narrow {
            let this = cx.entity().downgrade();
            choice_menu(
                "preset-menu",
                busy,
                options
                    .iter()
                    .map(|&(key, label)| {
                        let this = this.clone();
                        (label, on(key), std::rc::Rc::new(move |_: &mut Window, cx: &mut App| {
                            this.update(cx, |p, cx| p.set_what(key, cx)).ok();
                        }) as std::rc::Rc<dyn Fn(&mut Window, &mut App)>)
                    })
                    .collect(),
            )
            .into_any_element()
        } else {
            let mut seg = ui::seg_track(&seg_theme);
            for (key, label) in options {
                seg = seg.child(ui::seg_item(&seg_theme, SharedString::from(format!("preset-{key}")), label, on(key)).on_click(cx.listener(move |this, _, _, cx| this.set_what(key, cx))));
            }
            seg.into_any_element()
        };
        let what_row = card_row(
            theme,
            narrow,
            "What to back up",
            Some("Everything: settings and all telemetry.\nLast 90 days: settings and the last 90 days of telemetry.\nConfiguration only: devices, dashboards, users and settings, no telemetry."),
            what_control,
            None,
            false,
        );
        let pass_empty = self.passphrase.read(cx).value().is_empty();
        let pass_row = card_row(
            theme,
            narrow,
            "Encrypt",
            Some("A passphrase for Back up now; without it the file cannot be read. Automatic backups are not encrypted."),
            h_flex().min_w_0().flex_wrap().gap_2().items_center().child(div().w(px(260.)).child(Input::new(&self.passphrase).mask_toggle())).children(key_plain_note(theme, pass_empty)),
            None,
            false,
        );
        let copy_row = card_row(
            theme,
            narrow,
            "Also copy to",
            Some("A second disk or a network folder, in case this computer fails."),
            h_flex()
                .min_w_0()
                .gap(crate::ui::button_gap())
                .items_center()
                .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_sm().when(p.copy_to.is_some(), |d| d.font_family(theme.mono_font_family.clone()).text_size(px(13.5))).when(p.copy_to.is_none(), |d| d.text_color(theme.muted_foreground)).child(p.copy_to.as_ref().map(|d| d.display().to_string()).unwrap_or_else(|| "Nowhere".into())))
                .when(p.copy_to.is_some(), |r| {
                    r.child(crate::ui::button("copy-to-off").ghost().label("Stop copying").on_click(cx.listener(|this, _, _, cx| {
                        this.prefs.copy_to = None;
                        this.save_prefs(false, cx);
                    })))
                }),
            Some(crate::ui::button("copy-to").outline().label("Choose").disabled(busy).on_click(cx.listener(|this, _, _, cx| this.choose_folder(true, cx))).into_any_element()),
            false,
        );
        let split_row = card_row(
            theme,
            narrow,
            "Split files over",
            Some("Numbered parts for USB disks; 0 = never. Saved when you press Enter or leave the field."),
            h_flex().gap_2().items_center().child(div().w(px(72.)).child(Input::new(&self.part_gb))).child(div().text_sm().text_color(theme.muted_foreground).child("GB")),
            None,
            true,
        );
        let card = ui::panel(theme)
            .gap_0()
            .child(h_flex().pb_1().gap_3().items_center().min_h(px(28.)).child(div().font_weight(FontWeight::MEDIUM).child("Backup settings")).children(self.saved_note(theme, "card")))
            .child(folder_row)
            .child(auto_row)
            .child(keep_row)
            .child(what_row)
            .child(pass_row)
            .child(copy_row)
            .child(split_row);

        // The Backups table: Back up now at its title, as a server's.
        let estimate = self.sizes.map(|(pg, ch, hist, free)| {
            let (bytes, secs) = crate::backups::estimate(p, pg, ch, hist);
            let mins = secs.div_ceil(60);
            let free_text = free.map(|f| format!(" · {} free", bytes_label(f))).unwrap_or_default();
            let low = free.is_some_and(|f| f < bytes.saturating_mul(2));
            (format!("Estimated file: {} · takes about {} minute{}{free_text}", bytes_label(bytes), mins, if mins == 1 { "" } else { "s" }), low)
        });
        let low = estimate.as_ref().filter(|(_, low)| *low).map(|(t, _)| t.clone());
        let now_tip = estimate.map(|(t, _)| t).unwrap_or_else(|| {
            // Stopped: its size is read when it runs (a backup starts it).
            if self.sizes_stopped { format!("A backup now, with what the card says. {}", crate::dbstatus::STOPPED) } else { "A backup now, with what the card says.".into() }
        });
        let mut list = ui::panel(theme).gap_0().child(
            h_flex()
                .relative()
                .gap(crate::ui::button_gap())
                .pb_2()
                .items_center()
                .child(ui::layout_probe("header-actions"))
                .child(div().flex_1().min_w_0().font_weight(FontWeight::MEDIUM).child("Backups"))
                .child(crate::ui::probed(crate::ui::button("add-backup").ghost().label("Add a file").tooltip("Add a backup file from elsewhere to the folder.").disabled(busy).on_click(cx.listener(|this, _, _, cx| this.add_backup_file(cx)))))
                .when(!self.choose, |r| {
                    r.child(crate::ui::probed(crate::ui::button("backup-now").outline().label(if busy { "Backing up" } else { "Back up now" }).tooltip(now_tip).disabled(busy).on_click(cx.listener(|this, _, _, cx| this.run_preset_backup(cx)))))
                }),
        );
        if let Some(t) = low {
            list = list.child(div().pb_2().text_sm().text_color(theme.danger).child(t));
        }
        if let Some((tone, text)) = self.schedule_note.clone() {
            list = list.child(div().pb_2().child(ui::outcome(theme, tone, text)));
        }
        if self.list.is_empty() {
            list = list.child(div().py_2().text_sm().text_color(theme.muted_foreground).child("No backups in this folder. Restore from file (under Restore history) can still take a file from elsewhere or another instance."));
        } else {
            list = list.child(table_header(theme, narrow));
        }
        let this = cx.entity().downgrade();
        for (i, e) in self.list.iter().enumerate() {
            let damaged = e.check.as_ref().is_some_and(|c| !c.ok);
            let (p1, p2, p3) = (e.path.clone(), e.path.clone(), e.path.clone());
            let (t1, t2, t3) = (this.clone(), this.clone(), this.clone());
            list = list.child(table_row(
                theme,
                narrow,
                i,
                e,
                busy,
                RowActions {
                    details: {
                        let (entry, tw) = (e.clone(), this.clone());
                        std::rc::Rc::new(move |_, cx| {
                            let restore = (!damaged).then(|| -> std::rc::Rc<dyn Fn(&mut Window, &mut App)> {
                                let (tw, file, name) = (tw.clone(), entry.path.clone(), entry.name.clone());
                                std::rc::Rc::new(move |_, cx| {
                                    tw.update(cx, |this, cx| {
                                        this.pending = Some(crate::manager::restore::Source { file: file.clone(), sub: name.clone(), note: None });
                                        cx.notify();
                                    })
                                    .ok();
                                })
                            });
                            let (entry, file) = (entry.clone(), entry.path.clone());
                            open_details_dialog(
                                std::sync::Arc::new(move || Ok(crate::backups::details(&entry))),
                                Some(std::sync::Arc::new(move || crate::backups::check(&file, None).map(|_| ()))),
                                restore,
                                cx,
                            )
                        })
                    },
                    restore: (!damaged).then(|| -> std::rc::Rc<dyn Fn(&mut Window, &mut App)> {
                        let name = e.name.clone();
                        std::rc::Rc::new(move |_, cx| {
                            t1.update(cx, |this, cx| {
                                this.pending = Some(crate::manager::restore::Source { file: p1.clone(), sub: name.clone(), note: None });
                                cx.notify();
                            })
                            .ok();
                        })
                    }),
                    reveal: Some({
                        let dir = e.path.parent().map(|d| d.display().to_string()).unwrap_or_default();
                        std::rc::Rc::new(move |_, cx| crate::ui::open_url(&dir, cx))
                    }),
                    verify: std::rc::Rc::new(move |_, cx| {
                        t2.update(cx, |this, cx| this.run_check(p2.clone(), cx)).ok();
                    }),
                    delete: std::rc::Rc::new(move |window, cx| {
                        t3.update(cx, |this, cx| this.confirm_delete(p3.clone(), window, cx)).ok();
                    }),
                },
            ));
        }
        out.push(list.into_any_element());
        // Last: the page reads Backups, Restores, Backup settings (render puts
        // the Restores card before this one).
        out.push(card.into_any_element());
        out
    }
}

impl Render for BackupPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let page = ui::page(
            &theme,
            "Backup and restore",
            "One file with the database, telemetry and file stores, a checksum for every part, and optional encryption.",
        );
        // Screens and the layout test: a backup in the middle with a long
        // log (SPM_DEBUG_JOB_LOG=<lines>[:<step>], crate::manager::jobview).
        #[cfg(debug_assertions)]
        {
            static FILLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !FILLED.swap(true, std::sync::atomic::Ordering::SeqCst) {
                let plan = crate::jobplan::backup(&self.prefs, false, true);
                if crate::manager::jobview::debug_job(&self.backup, plan, cx) {
                    self.shown_run = true;
                }
            }
        }
        if self.shown_run && self.record.is_none() {
            let actions = self.run_actions(cx);
            if let Some(run) = self.run_view(&theme, actions, cx) {
                return page.child(run).into_any_element();
            }
        }
        // Screens and the layout test: the folder chooser open
        // (SPM_DEBUG_FOLDER_CHOOSER=1, with SPM_DEBUG_FOLDER_PICK to list a
        // long folder and SPM_DEBUG_CHOOSER_SCROLL=bottom to scroll it).
        #[cfg(debug_assertions)]
        {
            static OPENED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if std::env::var_os("SPM_DEBUG_FOLDER_CHOOSER").is_some() && !OPENED.swap(true, std::sync::atomic::Ordering::SeqCst) {
                self.change_folder(window, cx);
            }
        }
        let Some(record) = self.record.clone() else {
            return page.child(ui::not_installed(&theme)).into_any_element();
        };
        let record = &record;
        if let Some(src) = self.pending.take() {
            self.open_flow(src, window, cx);
        }
        if let Some(flow) = &self.flow {
            return flow.clone().into_any_element();
        }
        #[cfg(debug_assertions)]
        self.debug_dialogs(cx);
        // A backup's run, in place of the page (crate::manager::jobview).
        if self.shown_run {
            let actions = self.run_actions(cx);
            if let Some(run) = self.run_view(&theme, actions, cx) {
                return page.child(run).into_any_element();
            }
            self.shown_run = false;
        }
        let clickhouse = record.flavor() == Flavor::Clickhouse;
        let busy = self.backup.read(cx).is_running() || self.restore.read(cx).is_running();

        let categories = self.inspection.as_ref().map(|i| {
            i.categories
                .iter()
                .map(|c| {
                    let id = c.id.clone();
                    Checkbox::new(SharedString::from(format!("cat-{}", c.id)))
                        .label(format!("{} · {}", c.name, db::human_bytes(c.bytes)))
                        .checked(c.locked || self.include.contains(&c.id))
                        .disabled(c.locked)
                        .on_change(cx.listener(move |this, value: &bool, _, cx| {
                            if *value {
                                this.include.insert(id.clone());
                            } else {
                                this.include.remove(&id);
                            }
                            cx.notify();
                        }))
                })
                .collect::<Vec<_>>()
        });
        let can_backup = !busy && self.inspection.is_some() && self.output.is_some();

        let narrow = narrow(window);
        let mut panels = self.backup_panels(&theme, narrow, busy, cx);
        // Backups, Restores, Backup settings: the settings card goes last.
        let settings = panels.pop();
        page.children(panels).when(self.choose, |page| page.child(
            ui::panel(&theme)
                .child(div().font_weight(FontWeight::MEDIUM).child("Choose what to back up"))
                .child(
                    h_flex()
                        .gap_2()
                        .items_end()
                        .child(
                            v_flex()
                                .gap_1p5()
                                .w_64()
                                .child(div().text_size(px(13.)).text_color(theme.muted_foreground).child("Database password"))
                                .child(Input::new(&self.password).mask_toggle()),
                        )
                        .when(clickhouse, |r| {
                            r.child(
                                v_flex()
                                    .gap_1p5()
                                    .w_64()
                                    .child(div().text_size(px(13.)).text_color(theme.muted_foreground).child("ClickHouse password"))
                                    .child(Input::new(&self.ch_password).mask_toggle()),
                            )
                        })
                        .child(
                            crate::ui::button("inspect")
                                .outline()
                                .label(if self.inspection.is_some() { "Read again" } else { "Read the database" })
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, _, cx| this.inspect(cx))),
                        ),
                )
                .children(self.inspect_error.clone().map(|e| div().text_sm().text_color(theme.danger).child(e)))
                .when_some(self.inspection.as_ref().map(|i| i.summary.clone()), |p, s| {
                    p.child(div().text_sm().text_color(theme.muted_foreground).child(s))
                })
                .when_some(categories, |p, boxes| p.child(v_flex().gap_2().children(boxes)))
                .when(self.inspection.is_some(), |p| {
                    p.child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(div().text_sm().child("Telemetry history"))
                            .child(
                                RadioGroup::horizontal("telemetry")
                                    .children(["All", "Last days", "None"])
                                    .selected_index(Some(self.telemetry))
                                    .on_change(cx.listener(|this, value: &usize, _, cx| {
                                        this.telemetry = *value;
                                        cx.notify();
                                    })),
                            )
                            .when(self.telemetry == 1, |r| r.child(div().w_20().child(Input::new(&self.telemetry_days)))),
                    )
                    .child(h_flex().gap_2().items_center().child(div().w_80().child(Input::new(&self.passphrase).mask_toggle())).children(key_plain_note(&theme, self.passphrase.read(cx).value().is_empty())))
                    .child(
                        h_flex()
                            .gap(crate::ui::button_gap())
                            .items_center()
                            .child(
                                crate::ui::button("output")
                                    .outline()
                                    .label("Save as")
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| this.choose_output(cx))),
                            )
                            .child(div().text_sm().text_color(theme.muted_foreground).child(
                                self.output.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "No file chosen".into()),
                            )),
                    )
                    .child(
                        h_flex().child(
                            crate::ui::button("run-backup")
                                .primary()
                                .label("Back up now")
                                .disabled(!can_backup)
                                .on_click(cx.listener(|this, _, _, cx| this.run_backup(cx))),
                        ),
                    )
                })
                ,
        ))
        .child(crate::manager::restore::history_card(
            &theme,
            &crate::manager::restore::Target::Here(record.clone()),
            self.history.as_ref(),
            narrow,
            Some(crate::ui::probed(crate::ui::button("restore-from").outline().icon(crate::manager::restore::icon("history")).label("Restore from file").disabled(busy).on_click(cx.listener(|this, _, _, cx| this.restore_from(cx)))).into_any_element()),
        ))
        .children(settings)
        .into_any_element()
    }
}

/// When a check ran, on the clock of the machine that ran it (as the
/// backup's own time beside it is): "2026-11-15 17:14".
fn check_time(at: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(at)
        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| at.get(..16).unwrap_or(at).replace('T', " "))
}

/// Keep: how many backups, typed (the mockup's number field, 1 to 365),
/// "backups" after it. Saved by the caller on Enter or when the field is
/// left ([`keep_value`]). Shared with a remote server's.
pub(crate) fn keep_field(theme: &gpui_kit::component::Theme, state: &Entity<InputState>, disabled: bool) -> Div {
    h_flex()
        .gap_2()
        .items_center()
        .child(div().w(px(72.)).child(Input::new(state).disabled(disabled)))
        .child(div().text_sm().text_color(theme.muted_foreground).child("backups"))
}

/// The number typed in a Keep field, kept between 1 and 365; None when it
/// is not a number.
pub(crate) fn keep_value(text: &str) -> Option<u32> {
    text.trim().parse::<u32>().ok().map(|k| k.clamp(1, 365))
}

// -- B1: the Backup card and the Backups table (this computer and a server) --

/// The label column of the Backup card's rows.
const CARD_LABEL_W: f32 = 186.;
/// The action column at the right (Change…).
const CARD_ACTION_W: f32 = 118.;

/// A Backup card row: label, value, and an action column whose line is
/// drawn apart from the row's (the mockup). `last`: no line under it.
/// Beside an empty passphrase: SENTIENT's secrets key goes beside the
/// backup as a plain file. With a passphrase it is encrypted with it.
fn key_plain_note(theme: &gpui_kit::component::Theme, pass_empty: bool) -> Option<AnyElement> {
    pass_empty.then(|| {
        h_flex()
            .flex_shrink_0()
            .gap_1()
            .items_center()
            .child(ui::pill(theme, Tone::Warn, "Key not encrypted"))
            .child(ui::info(
                "key-plain".into(),
                "Without a passphrase, SENTIENT's secrets key (it opens the stored mail, SMS and integration passwords) is saved beside the backup as a plain file, readable by this account only. Keep it apart from the backup when the backup leaves this computer. With a passphrase, the key is encrypted with it too.".into(),
            ))
            .into_any_element()
    })
}

pub(crate) fn card_row(theme: &gpui_kit::component::Theme, narrow: bool, label: &'static str, help: Option<&'static str>, value: impl IntoElement, action: Option<AnyElement>, last: bool) -> Div {
    let line = |d: Div| if last { d } else { d.border_b_1().border_color(theme.border) };
    let label_el = h_flex()
        .gap_1()
        .items_center()
        .text_sm()
        .text_color(theme.muted_foreground)
        .child(label)
        .when_some(help, |c, h| c.child(crate::ui::info(SharedString::from(format!("bhelp-{label}")), h.into())));
    // A narrow window: the label above, the value and its action under it.
    if narrow {
        return line(v_flex().py(px(8.)).gap_1())
            .child(label_el)
            .child(h_flex().min_w_0().gap(crate::ui::button_gap()).items_center().child(h_flex().flex_1().min_w_0().gap_2().items_center().child(value)).children(action));
    }
    // One divider under the whole row, label to action (his rc12-22 note:
    // a line per column left a gap before the action).
    line(h_flex().items_stretch().gap(px(16.)))
        .child(
            h_flex().flex_1().min_w_0().min_h(px(53.)).py(px(8.)).items_center().flex_wrap().gap_y_1()
                .child(label_el.w(px(CARD_LABEL_W)).flex_shrink_0())
                .child(h_flex().flex_1().min_w_0().gap_2().items_center().child(value)),
        )
        .child(h_flex().w(px(CARD_ACTION_W)).flex_shrink_0().items_center().justify_end().children(action))
}

/// The folder row's value: the path, its free space, and what the page
/// says after a change ("Saved", or why not).
pub(crate) fn folder_value(theme: &gpui_kit::component::Theme, folder: &std::path::Path, free: Option<u64>, note: Option<AnyElement>) -> Div {
    h_flex()
        .min_w_0()
        .gap(px(8.))
        .items_center()
        .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().font_family(theme.mono_font_family.clone()).text_size(px(13.5)).child(folder.display().to_string()))
        .when_some(free, |r, f| r.child(div().flex_shrink_0().text_size(px(12.5)).text_color(theme.muted_foreground).child(format!("{} free", free_label(f)))))
        .children(note)
}

/// Free space as the mockup writes it: "412 GB", "64.1 GB", "1.8 TB".
pub(crate) fn free_label(b: u64) -> String {
    sentient_installer_core::size::label(b)
}

/// A choice as a menu button (the card's segments on a narrow window).
pub(crate) fn choice_menu(id: &'static str, disabled: bool, items: Vec<(&'static str, bool, std::rc::Rc<dyn Fn(&mut Window, &mut App)>)>) -> impl IntoElement {
    let current = items.iter().find(|i| i.1).map(|i| i.0).unwrap_or("Choose");
    crate::ui::button(id).outline().disabled(disabled).label(current).icon(IconName::ChevronDown).dropdown_menu(move |menu, _, _| {
        let mut menu = menu;
        for (label, on, f) in items.iter().cloned() {
            menu = menu.item(PopupMenuItem::new(label).checked(on).on_click(move |_, window, cx| f(window, cx)));
        }
        menu
    })
}

/// A narrow window: the Backup card stacks its rows and the table puts
/// Made and Size under the name.
pub(crate) fn narrow(window: &Window) -> bool {
    window.viewport_size().width < px(760.)
}

/// "Change…" on the folder row.
pub(crate) fn change_button(id: &'static str, disabled: bool) -> gpui_kit::component::button::Button {
    crate::ui::button(id).outline().icon(gpui_kit::component::Icon::new(gpui_kit::assets::IconName::Folder)).label("Change").disabled(disabled)
}

/// Column widths of the Backups table: Made, Size, Integrity, the chevron.
const COL_MADE: f32 = 132.;
const COL_SIZE: f32 = 100.;
const COL_CHECK: f32 = 130.;
const COL_MENU: f32 = 36.;
/// The row's Restore… button (an icon alone when narrow).
const COL_RESTORE: f32 = 108.;
const COL_RESTORE_NARROW: f32 = 40.;
/// Integrity on a narrow window (Made and Size go under the name).
const COL_CHECK_NARROW: f32 = 104.;

pub(crate) fn table_header(theme: &gpui_kit::component::Theme, narrow: bool) -> Div {
    h_flex()
        .h(px(30.))
        .px(px(4.))
        .items_center()
        .text_size(px(12.5))
        .text_color(theme.muted_foreground)
        .border_b_1()
        .border_color(theme.border)
        .child(div().flex_1().min_w_0().child("File"))
        .when(!narrow, |r| r.child(div().w(px(COL_MADE)).flex_shrink_0().child("Made")).child(div().w(px(COL_SIZE)).flex_shrink_0().child("Size")))
        .child(div().w(px(if narrow { COL_CHECK_NARROW } else { COL_CHECK })).flex_shrink_0().child("Integrity"))
        .child(div().w(px(if narrow { COL_RESTORE_NARROW } else { COL_RESTORE })).flex_shrink_0())
        .child(div().w(px(COL_MENU)).flex_shrink_0())
}

/// "16 Nov, 02:00" from the list's "2026-11-16 02:00".
pub(crate) fn short_made(made: &str) -> String {
    chrono::NaiveDateTime::parse_from_str(made, "%Y-%m-%d %H:%M").map(|t| t.format("%-d %b, %H:%M").to_string()).unwrap_or_else(|_| made.to_string())
}

/// What a row offers: Restore beside it, the rest in its three-dot menu.
pub(crate) struct RowActions {
    pub restore: Option<std::rc::Rc<dyn Fn(&mut Window, &mut App)>>,
    /// Show the file in its folder (a backup on this computer only).
    pub reveal: Option<std::rc::Rc<dyn Fn(&mut Window, &mut App)>>,
    pub verify: std::rc::Rc<dyn Fn(&mut Window, &mut App)>,
    pub delete: std::rc::Rc<dyn Fn(&mut Window, &mut App)>,
    /// Details: what the backup holds (open_details_dialog).
    pub details: std::rc::Rc<dyn Fn(&mut Window, &mut App)>,
}

/// One backup in the table: file (and "Old folder"), made, size, its
/// check, and the chevron with Restore, Verify and Delete.
pub(crate) fn table_row(theme: &gpui_kit::component::Theme, narrow: bool, i: usize, e: &crate::backups::Entry, busy: bool, actions: RowActions) -> Div {
    let (tone, text, tip): (Tone, &str, String) = match &e.check {
        Some(c) if c.ok => (Tone::Good, "Verified", format!("Every part of the file was read back on {} and matched its SHA-256 checksum. {}", check_time(&c.at), c.summary)),
        Some(c) => (Tone::Bad, "Damaged", format!("Checked {}: {} Do not rely on this backup.", check_time(&c.at), c.summary)),
        None => (Tone::Neutral, "Not verified", "Integrity check not run yet. Verify reads every part of the file and compares it with its SHA-256 checksum.".to_string()),
    };
    let made_tip = format!(
        "{} · {}{}{}",
        e.made,
        if e.automatic { "Automatic" } else { "Manual" },
        if e.parts > 1 { format!(" · {} parts", e.parts) } else { String::new() },
        if e.old_folder { format!("\nIn {}", e.path.parent().map(|p| p.display().to_string()).unwrap_or_default()) } else { String::new() }
    );
    let RowActions { restore, reveal, verify, delete, details } = actions;
    let _ = (&made_tip, &tip);
    let path = e.path.display().to_string();
    let mut items = vec![
        crate::ui::RowMenuItem::Item {
            label: "Details".into(),
            icon: Some(IconName::Info),
            action: details,
        },
        crate::ui::RowMenuItem::Item { label: "Check integrity".into(), icon: Some(IconName::CircleCheck), action: verify },
    ];
    if let Some(r) = reveal {
        items.push(crate::ui::RowMenuItem::Item { label: "Show in folder".into(), icon: Some(IconName::Folder), action: r });
    }
    items.push(crate::ui::RowMenuItem::Item {
        label: "Copy path".into(),
        icon: Some(IconName::Copy),
        action: std::rc::Rc::new(move |_, cx| cx.write_to_clipboard(ClipboardItem::new_string(path.clone()))),
    });
    items.push(crate::ui::RowMenuItem::Divider);
    items.push(crate::ui::RowMenuItem::Danger { label: "Delete".into(), icon: IconName::Delete, action: delete });
    let menu = crate::ui::row_menu(format!("bk-row-{i}"), "More", busy, items);
    // Restore… on every row, visible; off for a damaged backup.
    let damaged = restore.is_none();
    let mut restore_btn = crate::ui::button(SharedString::from(format!("bk-restore-{i}"))).outline().icon(crate::manager::restore::icon("history")).disabled(busy || damaged);
    if !narrow {
        restore_btn = restore_btn.label("Restore");
    }
    if damaged {
        restore_btn = restore_btn.tooltip("A damaged backup cannot be restored");
    } else if narrow {
        restore_btn = restore_btn.tooltip("Restore");
    }
    if let Some(r) = restore {
        restore_btn = restore_btn.on_click(move |_, window, cx| r(window, cx));
    }
    h_flex()
        .min_h(px(43.))
        .px(px(4.))
        .items_center()
        .text_sm()
        .when(i > 0, |r| r.border_t_1().border_color(theme.border))
        .when(narrow, |r| {
            // Made and size under the name, the pill beside them.
            r.py_1().child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(e.name.clone()))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .items_center()
                            .text_size(px(12.5))
                            .text_color(theme.muted_foreground)
                            .child(format!("{} · {}", short_made(&e.made), crate::dbstatus::bytes_label(e.bytes)))
                            .when(e.old_folder, |r| r.child(old_folder_pill(theme))),
                    ),
            )
        })
        .when(!narrow, |r| r.child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap(px(8.))
                .items_center()
                .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(e.name.clone()))
                .when(e.old_folder, |r| r.child(old_folder_pill(theme))),
        )
        .child(
            div()
                .id(SharedString::from(format!("bk-made-{i}")))
                .w(px(COL_MADE))
                .flex_shrink_0()
                .font_features(crate::ui::tabular())
                .child(short_made(&e.made))
                .tooltip(crate::ui::tooltip(made_tip.clone())),
        )
        .child(div().w(px(COL_SIZE)).flex_shrink_0().font_features(crate::ui::tabular()).child(crate::dbstatus::bytes_label(e.bytes))))
        .child(
            h_flex()
                .id(SharedString::from(format!("bk-check-{i}")))
                .w(px(if narrow { COL_CHECK_NARROW } else { COL_CHECK }))
                .flex_shrink_0()
                .child(small_pill(theme, tone, text))
                .tooltip(crate::ui::tooltip(tip.clone())),
        )
        // Restore… and the menu: one button group at the row's end (the
        // header's two columns' width).
        .child(
            ui::button_group(vec![restore_btn.into_any_element(), menu.into_any_element()])
                .w(px(if narrow { COL_RESTORE_NARROW } else { COL_RESTORE }) + px(COL_MENU))
                .flex_shrink_0()
                .justify_end(),
        )
}

/// The table's pills: smaller than the page's.
fn small_pill(theme: &gpui_kit::component::Theme, tone: Tone, text: &'static str) -> Div {
    ui::pill(theme, tone, text).px(px(7.)).py(px(1.)).text_size(px(12.))
}

/// B5: a backup left in a folder used before.
pub(crate) fn old_folder_pill(theme: &gpui_kit::component::Theme) -> Div {
    h_flex().flex_shrink_0().px(px(7.)).py(px(1.)).rounded_full().bg(theme.secondary).text_color(theme.muted_foreground).text_size(px(12.)).child("Old folder")
}

// ---------------------------------------------------------------------------
// Backup details: what one backup holds
// ---------------------------------------------------------------------------

/// Where the Details dialog gets its facts (this computer's file, or the
/// server's agent). Blocking; run in the background.
pub(crate) type DetailsLoad = std::sync::Arc<dyn Fn() -> Result<crate::backups::Details, String> + Send + Sync>;
/// Check the backup now (the row menu's Check integrity), for the dialog's
/// Check now. Blocking; run in the background.
pub(crate) type DetailsCheck = std::sync::Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

/// The Backup details dialog: the file and its check, the facts its
/// manifest records, the data it holds, its parts with their last check,
/// and the platform at backup time. `restore`: the footer's Restore (None
/// for a damaged backup).
pub(crate) fn open_details_dialog(load: DetailsLoad, check: Option<DetailsCheck>, restore: Option<std::rc::Rc<dyn Fn(&mut Window, &mut App)>>, cx: &mut App) {
    let view = cx.new(|cx: &mut Context<DetailsDialog>| {
        let mut d = DetailsDialog { state: None, load, check, checking: false, check_error: None, _task: None };
        d.read(false, cx);
        d
    });
    crate::modal::open(
        crate::modal::Modal { title: "Backup details".into(), body: view.into(), width: 640., icon: Some(crate::manager::restore::icon("database")), on_close: None },
        cx,
    );
    let footer = cx.new(|_| DetailsFooter { restore });
    crate::modal::set_footer(footer.into(), cx);
}

struct DetailsFooter {
    restore: Option<std::rc::Rc<dyn Fn(&mut Window, &mut App)>>,
}

impl Render for DetailsFooter {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let restore = self.restore.clone();
        h_flex()
            .gap(crate::ui::button_gap())
            .justify_end()
            .when_some(restore, |r, f| {
                r.child(crate::ui::probed(crate::ui::button("bk-details-restore").outline().label("Restore").on_click(move |_, window, cx| {
                    crate::modal::close(cx);
                    f(window, cx);
                })))
            })
            .child(crate::ui::probed(crate::ui::button("bk-details-close").primary().label("Close").on_click(|_, _, cx| crate::modal::close(cx))))
    }
}

pub(crate) struct DetailsDialog {
    state: Option<Result<crate::backups::Details, String>>,
    load: DetailsLoad,
    check: Option<DetailsCheck>,
    /// Check now is running.
    checking: bool,
    check_error: Option<String>,
    _task: Option<Task<()>>,
}

impl DetailsDialog {
    /// Read the details (after checking the backup when `check_first`).
    fn read(&mut self, check_first: bool, cx: &mut Context<Self>) {
        let (load, check) = (self.load.clone(), self.check.clone().filter(|_| check_first));
        self.checking = check.is_some();
        self.check_error = None;
        self._task = Some(cx.spawn(async move |this, cx| {
            let (checked, got) = cx
                .background_executor()
                .spawn(async move {
                    let checked = check.map(|c| c()).transpose();
                    (checked, load())
                })
                .await;
            this.update(cx, |d, cx| {
                d.checking = false;
                d.check_error = checked.err();
                d.state = Some(got);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }
}

/// "17 Nov 2026, 04:25" from "2026-11-17 04:25".
fn long_made(made: &str) -> String {
    chrono::NaiveDateTime::parse_from_str(made, "%Y-%m-%d %H:%M").map(|t| t.format("%-d %b %Y, %H:%M").to_string()).unwrap_or_else(|_| made.to_string())
}

/// "12,904,331".
fn grouped(n: i64) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// A section heading in the dialog: small capitals, muted, an optional
/// note at its right.
fn details_heading(theme: &gpui_kit::component::Theme, title: &'static str, right: Option<String>) -> Div {
    h_flex()
        .pt_3()
        .pb_2()
        .items_center()
        .gap_2()
        .child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child(title.to_uppercase()))
        .child(div().flex_1())
        .children(right.map(|r| div().text_size(px(12.5)).text_color(theme.muted_foreground).child(r)))
}

/// A bordered table with every row: the dialog's body is the one thing
/// that scrolls (a table scrolling inside it took the wheel from it too).
fn details_table(theme: &gpui_kit::component::Theme, header: Div, rows: Vec<Div>) -> Div {
    let mut body = v_flex();
    for (i, r) in rows.into_iter().enumerate() {
        body = body.child(r.when(i > 0, |r| r.border_t_1().border_color(theme.border)));
    }
    v_flex().rounded(px(8.)).border_1().border_color(theme.border).overflow_hidden().child(header.border_b_1().border_color(theme.border)).child(body)
}

fn details_row() -> Div {
    h_flex().min_h(px(32.)).px_3().gap_3().items_center().text_sm()
}

/// A fact, or "Not recorded" when the backup did not record it.
fn fact_or(theme: &gpui_kit::component::Theme, label: &'static str, value: Option<String>) -> Div {
    match value {
        Some(v) => ui::fact(theme, label, v),
        None => h_flex()
            .gap_4()
            .text_sm()
            .child(div().w(px(168.)).flex_shrink_0().text_color(theme.muted_foreground).child(label))
            .child(div().flex_1().min_w_0().text_color(theme.muted_foreground.opacity(0.75)).child("Not recorded")),
    }
}

impl Render for DetailsDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let d = match &self.state {
            None => return v_flex().gap_3().child(div().text_sm().text_color(theme.muted_foreground).child("Reading the backup…")),
            Some(Err(e)) => return v_flex().gap_3().child(ui::outcome(&theme, Tone::Bad, format!("The backup's details cannot be read: {e}"))),
            Some(Ok(d)) => d.clone(),
        };

        // Top: the file and its check, the date, Manual or Automatic, size.
        let (tone, pill) = match &d.check {
            Some(c) if c.ok => (Tone::Good, if c.parts.is_empty() { "Verified".to_string() } else { format!("All {} parts match", c.parts.len()) }),
            Some(c) => {
                let bad = c.parts.iter().filter(|(_, ok)| !ok).count();
                (Tone::Warn, if c.parts.is_empty() { "Damaged".to_string() } else { format!("{bad} of {} parts damaged", c.parts.len()) })
            }
            None => (Tone::Neutral, "Not verified".to_string()),
        };
        let size = if d.files > 1 { format!("{} in {} files", crate::dbstatus::bytes_label(d.bytes), d.files) } else { crate::dbstatus::bytes_label(d.bytes) };
        let top = h_flex()
            .gap_3()
            .items_start()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(d.name.clone()))
                    .child(div().text_sm().text_color(theme.muted_foreground).child(format!("{} · {} · {size}", long_made(&d.made), if d.automatic { "Automatic" } else { "Manual" }))),
            )
            .child(ui::pill(&theme, tone, pill).flex_shrink_0());

        // Facts.
        let sentient = d.sentient_version.clone().map(|v| match d.schema_version {
            Some(s) => format!("{v} (schema {s})"),
            None => v,
        });
        let database = match (&d.database_type, &d.postgres_version) {
            (Some(t), Some(pg)) => Some(format!("{t} (PostgreSQL {pg})")),
            (Some(t), None) => Some(t.clone()),
            (None, Some(pg)) => Some(format!("PostgreSQL {pg}")),
            (None, None) => None,
        };
        let readable = d.unreadable.is_none();
        let mut facts = v_flex().gap_2().pt_3();
        if readable {
            facts = facts
                .child(fact_or(&theme, "Made on", d.host.clone()))
                .child(fact_or(&theme, "SENTIENT", sentient))
                .child(fact_or(&theme, "Database", database))
                .child(fact_or(&theme, "What was backed up", d.scope.clone()));
        }
        facts = facts.child(ui::fact(&theme, "Encrypted", if d.encrypted { "Yes, with a passphrase" } else { "No" }));
        if readable {
            facts = facts.child(fact_or(&theme, "Made by", d.made_by.clone().map(|v| format!("Platform Manager {v}"))));
        }
        facts = facts.child(h_flex().gap_4().text_sm().child(div().w(px(168.)).flex_shrink_0().text_color(theme.muted_foreground).child("Folder")).child(
            div().flex_1().min_w_0().font_family(theme.mono_font_family.clone()).text_size(px(12.5)).child(d.folder.clone()),
        ));

        let mut body = v_flex().child(top).child(facts);
        if let Some(why) = &d.unreadable {
            body = body.child(div().pt_3().text_sm().text_color(theme.muted_foreground).child(why.clone()));
            return body;
        }

        // Data: what the backup holds, from its components.
        if !d.data.is_empty() {
            let header = details_row()
                .text_size(px(12.5))
                .text_color(theme.muted_foreground)
                .child(div().flex_1().min_w_0().child("Kind"))
                .child(div().w(px(110.)).flex_shrink_0().text_right().child("Rows"))
                .child(div().w(px(80.)).flex_shrink_0().text_right().child("Size"));
            let rows = d
                .data
                .iter()
                .map(|k| {
                    let r = details_row().child(div().flex_1().min_w_0().when(!k.included, |t| t.text_color(theme.muted_foreground)).child(k.name.clone()));
                    if k.included {
                        r.child(div().w(px(110.)).flex_shrink_0().text_right().font_features(ui::tabular()).when(k.rows == 0, |t| t.text_color(theme.muted_foreground)).child(if k.rows > 0 { grouped(k.rows) } else { "files".into() }))
                            .child(div().w(px(80.)).flex_shrink_0().text_right().font_features(ui::tabular()).child(crate::dbstatus::bytes_label(k.bytes.max(0) as u64)))
                    } else {
                        r.child(div().w(px(190.)).flex_shrink_0().text_right().text_color(theme.muted_foreground).child("Not included"))
                    }
                })
                .collect();
            body = body.child(details_heading(&theme, "Data", None)).child(details_table(&theme, header, rows));
        }

        // Parts: the archive's members; each one's last check only when the
        // stored check has per-part results (else no Check column, and a
        // way to run one).
        let marks = d.check.as_ref().filter(|c| !c.parts.is_empty());
        let checked = marks.map(|c| {
            let at = chrono::DateTime::parse_from_rfc3339(&c.at).map(|t| t.with_timezone(&chrono::Local).format("%-d %b %Y, %H:%M").to_string()).unwrap_or_else(|_| check_time(&c.at));
            format!("Checked {at}")
        });
        let mark_of = |path: &str| marks.and_then(|c| c.parts.iter().find(|(p, _)| p == path).map(|(_, ok)| *ok));
        let with_marks = marks.is_some();
        let header = details_row()
            .text_size(px(12.5))
            .text_color(theme.muted_foreground)
            .child(div().flex_1().min_w_0().child("Part"))
            .child(div().w(px(80.)).flex_shrink_0().text_right().child("Size"))
            .when(with_marks, |r| r.child(div().w(px(100.)).flex_shrink_0().text_right().child("Check")));
        let rows = d
            .contents
            .iter()
            .map(|c| {
                let row = details_row()
                    .child(div().flex_1().min_w_0().child(c.label.clone()))
                    .child(div().w(px(80.)).flex_shrink_0().text_right().font_features(ui::tabular()).child(crate::dbstatus::bytes_label(c.bytes)));
                if !with_marks {
                    return row;
                }
                let mark = match mark_of(&c.path) {
                    // The Verified pill's green (theme.success is its pale background).
                    Some(true) => div().text_color(theme.success_foreground).font_weight(FontWeight::MEDIUM).child("✓ Matches").into_any_element(),
                    Some(false) => ui::pill(&theme, Tone::Warn, "Damaged").into_any_element(),
                    None => div().into_any_element(),
                };
                row.child(h_flex().w(px(100.)).flex_shrink_0().justify_end().child(mark))
            })
            .collect();
        body = body.child(details_heading(&theme, "Parts", checked));
        if !with_marks {
            let can_check = self.check.is_some();
            let checking = self.checking;
            body = body.child(
                h_flex()
                    .pb_2()
                    .gap_3()
                    .items_center()
                    .child(div().flex_1().min_w_0().text_sm().text_color(theme.muted_foreground).child("Run a check to see each part's result."))
                    .when(can_check, |r| {
                        r.child(crate::ui::probed(
                            crate::ui::button("bk-details-check")
                                .outline()
                                .label(if checking { "Checking" } else { "Check now" })
                                .loading(checking)
                                .disabled(checking)
                                .on_click(cx.listener(|d, _, _, cx| d.read(true, cx))),
                        ))
                    }),
            );
            if let Some(e) = &self.check_error {
                body = body.child(div().pb_2().child(ui::outcome(&theme, Tone::Bad, e.clone())));
            }
        }
        body = body.child(details_table(&theme, header, rows));

        // The platform at backup time (backups that recorded it).
        if let Some(p) = &d.platform {
            let mut plat = v_flex().gap_2();
            plat = plat.child(fact_or(&theme, "Address", p.address.clone()));
            plat = plat.child(fact_or(&theme, "HTTPS certificate", p.certificate.clone()));
            plat = plat.child(fact_or(&theme, "Licence", p.licence.clone()));
            plat = plat.child(fact_or(&theme, "Gateways", (!p.gateways.is_empty()).then(|| p.gateways.join("\n"))));
            body = body.child(details_heading(&theme, "Platform at backup time", None)).child(plat);
        }
        body
    }
}
