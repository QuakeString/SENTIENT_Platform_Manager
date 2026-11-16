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
    ActiveTheme, Disableable as _, IconName, Sizable as _, h_flex, v_flex,
    button::ButtonVariants as _,
    checkbox::Checkbox,
    input::{Input, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    progress::Progress as ProgressBar,
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
    restore_input: Option<PathBuf>,
    restore_passphrase: Entity<InputState>,
    /// Replacing this instance's data with a backup (crate::replace).
    restore: Entity<Job>,
    summary: Option<Result<crate::replace::Summary, crate::replace::Unreadable>>,
    reading_summary: bool,
    /// The backup compared with this installation (after Next).
    check: Option<crate::replace::Check>,
    checking: bool,
    replace_stage: ReplaceStage,
    delete_old: bool,
    planned: Vec<String>,
    show_restore_log: bool,
    /// The Platform Manager's backups: settings, the one being made, the
    /// folder's list and the last check (crate::backups).
    prefs: crate::backups::BackupPrefs,
    /// "Choose…": the tick list of parts (the earlier, by-hand backup).
    choose: bool,
    made: Option<crate::backups::Made>,
    planned_backup: Vec<String>,
    list: Vec<crate::backups::Entry>,
    /// The backup being checked, and what the check found.
    checked: Option<(PathBuf, Option<Result<sentient_backup_core::restore::CheckReport, String>>)>,
    /// Sizes for the estimate: (database, ClickHouse, telemetry history in PostgreSQL), and free disk.
    sizes: Option<(u64, u64, u64, Option<u64>)>,
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
    _tasks: Vec<Task<()>>,
    _observe: Vec<Subscription>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReplaceStage {
    /// Choose a file; its summary shows below (I1).
    Choose,
    /// The backup against this installation, and what happens to the data now (I2, I6).
    Compare,
    /// Running, finished or failed (I3–I5).
    Run,
}

/// A button that leads to another screen.
#[derive(Clone, Copy)]
pub enum Go {
    Update,
    Settings,
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
        // Docker keeps the stack's passwords in its compose file.
        let docker = record
            .as_ref()
            .filter(|r| r.engine == crate::installation::Engine::Docker)
            .and_then(|r| sentient_installer_core::distro::docker_ops::read_secrets(&r.stack).ok());
        let saved = docker
            .as_ref()
            .map(|s| s.db_password.clone())
            .or_else(|| crate::secrets::get(crate::secrets::Secret::Database))
            .unwrap_or_default();
        let password = cx.new(|cx| InputState::new(window, cx).masked(true).default_value(saved));
        let saved_ch = docker
            .as_ref()
            .map(|s| s.ch_password.clone())
            .filter(|p| !p.is_empty())
            .or_else(|| crate::secrets::get(crate::secrets::Secret::Clickhouse))
            .unwrap_or_default();
        let ch_password = cx.new(|cx| InputState::new(window, cx).masked(true).default_value(saved_ch));
        let telemetry_days = cx.new(|cx| InputState::new(window, cx).default_value("30"));
        let passphrase = cx.new(|cx| {
            InputState::new(window, cx).masked(true).placeholder("Optional: encrypt the backup")
        });
        let restore_passphrase = cx.new(|cx| {
            InputState::new(window, cx).masked(true).placeholder("Only for an encrypted backup")
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
            restore_input: None,
            restore_passphrase,
            restore,
            summary: None,
            reading_summary: false,
            check: None,
            checking: false,
            replace_stage: ReplaceStage::Choose,
            delete_old: true,
            planned: Vec::new(),
            show_restore_log: false,
            list: crate::backups::list_all(&prefs),
            auto_on: prefs.daily_at.is_some(),
            prefs,
            choose: false,
            made: None,
            planned_backup: Vec::new(),
            checked: None,
            sizes: None,
            schedule_time,
            keep,
            part_gb,
            schedule_note: None,
            // Screens: "Saved" shown on a section (SPM_DEBUG_SAVED=folder).
            saved: std::env::var("SPM_DEBUG_SAVED").ok().filter(|_| cfg!(debug_assertions)).map(|s| (&*s.leak(), Ok(()))),
            folder_free: crate::backups::free_space(&prefs_folder),
            _tasks: Vec::new(),
            _observe,
        };
        page.read_sizes(cx);
        // Development aid: open with a backup chosen (screens).
        #[cfg(debug_assertions)]
        if let Some(f) = std::env::var_os("SPM_DEBUG_RESTORE_FILE") {
            page.restore_input = Some(PathBuf::from(f));
            page.read_summary(cx);
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
        self.backup.update(cx, |job, cx| {
            job.start(
                move |sink| {
                    if let Some(r) = &record {
                        opts.origin = crate::backup_jobs::origin(r, &conn);
                    }
                    let summary = block_on(backup::run(&conn, &opts, bridge(sink.clone())))?
                        .map_err(|e| e.to_string())?;
                    if let Some(record) = &record {
                        crate::secrets_key::save_beside(record, &summary.output, &sink);
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

    fn choose_restore_input(&mut self, cx: &mut Context<Self>) {
        let start = Some(self.prefs.folder.clone()).filter(|d| d.is_dir());
        let rx = crate::chooser::open(
            cx,
            crate::chooser::Ask { title: "Choose the backup to restore", start: start.as_deref(), directory: false },
            PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Restore this backup".into()) },
        );
        let task = cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            this.update(cx, |page, cx| {
                page.restore_input = paths.into_iter().next();
                page.replace_stage = ReplaceStage::Choose;
                page.read_summary(cx);
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
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
    fn read_sizes(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        if crate::dbstatus::needs_admin(&record) {
            return;
        }
        let task = cx.spawn(async move |this, cx| {
            let st = cx.background_executor().spawn(async move { crate::dbstatus::read(&record) }).await;
            this.update(cx, |page, cx| {
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
        self.planned_backup = crate::backups::planned_steps(&prefs, false);
        self.made = None;
        let made = cx.entity().downgrade();
        let (tx, rx) = std::sync::mpsc::channel::<crate::backups::Made>();
        let me = cx.entity();
        self.backup.update(cx, |job, cx| {
            job.set_label("Backing up");
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

    /// Read the chosen file's summary (with the passphrase when typed).
    fn read_summary(&mut self, cx: &mut Context<Self>) {
        let Some(file) = self.restore_input.clone() else { return };
        let pass = Some(self.restore_passphrase.read(cx).value().to_string()).filter(|p| !p.is_empty());
        self.reading_summary = true;
        self.summary = None;
        self.check = None;
        let task = cx.spawn(async move |this, cx| {
            let s = cx.background_executor().spawn(async move { crate::replace::summary(&file, pass.as_deref()) }).await;
            this.update(cx, |page, cx| {
                page.summary = Some(s);
                page.reading_summary = false;
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    /// I2: compare the backup with this installation.
    fn compare(&mut self, cx: &mut Context<Self>) {
        let (Some(record), Some(Ok(summary))) = (self.record.clone(), self.summary.clone()) else { return };
        self.replace_stage = ReplaceStage::Compare;
        self.checking = true;
        let task = cx.spawn(async move |this, cx| {
            let c = cx.background_executor().spawn(async move { crate::replace::check(&record, &summary) }).await;
            this.update(cx, |page, cx| {
                page.check = Some(c);
                page.checking = false;
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
        cx.notify();
    }

    fn confirm_replace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let text = "Users, passwords, devices and dashboards will be those of the backup. Everything on this instance now is replaced. SENTIENT stops for about a minute while it switches.";
        crate::modal::alert(window, cx, move |dialog, _, _| {
            let this = this.clone();
            dialog.title("Replace this instance's data?").description(text).confirm().ok_text("Replace").on_ok(move |_, _, cx| {
                this.update(cx, |page, cx| page.run_replace(cx)).ok();
                true
            })
        });
    }

    fn run_replace(&mut self, cx: &mut Context<Self>) {
        let (Some(record), Some(file)) = (self.record.clone(), self.restore_input.clone()) else { return };
        let pass = Some(self.restore_passphrase.read(cx).value().to_string()).filter(|p| !p.is_empty());
        let delete_old = self.delete_old;
        self.planned = crate::replace::planned_steps(delete_old);
        self.replace_stage = ReplaceStage::Run;
        self.show_restore_log = false;
        self.restore.update(cx, |job, cx| {
            if crate::rootjob::needed(&record) {
                let mut args = vec!["replace".to_string(), file.display().to_string()];
                if delete_old {
                    args.push("--delete-old".into());
                }
                if pass.is_some() {
                    args.push("--passphrase-stdin".into());
                }
                // The passphrase goes on the job's standard input, never on its command line.
                job.start(move |sink| crate::rootjob::run_with_input(&args, pass.as_deref(), &sink).map(|_| ()), cx)
            } else {
                job.start(move |sink| crate::replace::run(record, file, pass, delete_old, sink), cx)
            }
        });
        cx.notify();
    }

    /// I1–I6.
    fn replace_panel(&self, theme: &gpui_kit::component::Theme, busy: bool, cx: &mut Context<Self>) -> Div {
        use crate::replace::{Fix, Unreadable};
        let mut panel = ui::panel(theme)
            .child(div().font_weight(FontWeight::MEDIUM).child("Restore from a backup"))
            .child(div().text_sm().text_color(theme.muted_foreground).child(
                "Bring back this instance's data, or replace it with another instance's. This installation keeps its own database setup; a backup from another type is converted, one from an older SENTIENT is brought up to this version.",
            ));
        let fact = |label: &'static str, value: String| {
            h_flex().gap_3().text_sm().child(div().w_40().flex_shrink_0().text_color(theme.muted_foreground).child(label)).child(div().min_w_0().child(value))
        };
        match self.replace_stage {
            ReplaceStage::Choose => {
                panel = panel.child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(crate::ui::button("restore-input").outline().label("Choose backup…").disabled(busy).on_click(cx.listener(|this, _, _, cx| this.choose_restore_input(cx))))
                        .child(div().text_sm().text_color(theme.muted_foreground).child(
                            self.restore_input.as_ref().and_then(|p| p.file_name()).map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|| "No file chosen".into()),
                        )),
                );
                if self.reading_summary {
                    panel = panel.child(div().text_sm().text_color(theme.muted_foreground).child("Reading the backup…"));
                }
                match &self.summary {
                    Some(Ok(s)) => {
                        panel = panel
                            .child(fact("Made", format!("{}{}", s.made, s.host.as_ref().map(|h| format!(", on {h}")).unwrap_or_default())))
                            .child(fact("SENTIENT", s.sentient_version.clone().unwrap_or_else(|| "not recorded".into())))
                            .child(
                                h_flex()
                                    .gap_3()
                                    .text_sm()
                                    .child(div().w_40().flex_shrink_0().text_color(theme.muted_foreground).child("Database type"))
                                    .child(ui::database_logo("postgresql", 15.))
                                    .when(s.database_type.contains("ClickHouse"), |r| r.child(ui::database_logo("clickhouse", 13.)))
                                    .when(s.database_type.contains("TimescaleDB"), |r| r.child(ui::database_logo("timescale", 15.)))
                                    .child(div().min_w_0().child(s.database_type.clone())),
                            )
                            .child(fact("Size", format!("{} (about {} restored)", crate::dbstatus::bytes_label(s.file_bytes), crate::dbstatus::bytes_label(s.restored_bytes))))
                            .child(fact("Holds", s.holds.join(", ")))
                            .child(fact("Protected", if s.encrypted { "encrypted with a passphrase".into() } else { "not encrypted".to_string() }))
                            .child(h_flex().child(crate::ui::button("replace-next").primary().label("Next").disabled(busy || self.record.is_none()).on_click(cx.listener(|this, _, _, cx| this.compare(cx)))));
                    }
                    Some(Err(Unreadable::Passphrase(e))) => {
                        panel = panel
                            .child(div().text_sm().child(if e.contains("wrong") { "The passphrase is wrong, or the file is damaged." } else { "This backup is encrypted. Type its passphrase to read it." }))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(div().w_64().child(Input::new(&self.restore_passphrase).mask_toggle()))
                                    .child(crate::ui::button("read-encrypted").outline().label("Read").on_click(cx.listener(|this, _, _, cx| this.read_summary(cx)))),
                            );
                    }
                    Some(Err(Unreadable::Other(e))) => panel = panel.child(ui::outcome(theme, Tone::Bad, format!("This file cannot be used: {e}"))),
                    None => {}
                }
            }
            ReplaceStage::Compare => {
                let here = self.record.as_ref().map(|r| r.sentient_version.clone()).unwrap_or_default();
                panel = panel.child(div().text_lg().child("This backup into this installation"));
                match &self.check {
                    None => panel = panel.child(div().text_sm().text_color(theme.muted_foreground).child("Comparing with this installation…")),
                    Some(c) => {
                        panel = panel
                            .child(fact("Database type", c.type_line.clone()))
                            .child(fact("SENTIENT", c.version_line.clone()))
                            .child(fact("Disk", c.disk_line.clone()))
                            .child(fact("Licence", "This computer keeps its own licence.".into()));
                        if let Some((why, fix)) = &c.refusal {
                            let fix = *fix;
                            panel = panel.child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(div().flex_1().child(ui::outcome(theme, Tone::Bad, why.clone())))
                                    .when(fix == Fix::UpdateSentient, |r| {
                                        r.child(crate::ui::button("fix-update").outline().label("Update SENTIENT first").on_click(cx.listener(|_, _, _, cx| cx.emit(Go::Update))))
                                    }),
                            );
                        } else {
                            let live = c.live_bytes.map(crate::dbstatus::bytes_label).unwrap_or_else(|| "size unknown".into());
                            panel = panel
                                .child(div().pt_2().font_weight(FontWeight::MEDIUM).child(format!("The data on this instance now ({live})")))
                                .child(
                                    v_flex()
                                        .gap_1()
                                        .child(Checkbox::new("delete-old").label("Delete it once the restored data runs").checked(self.delete_old).on_change(cx.listener(|this, v: &bool, _, cx| {
                                            this.delete_old = *v;
                                            cx.notify();
                                        })))
                                        .child(div().pl_6().text_sm().text_color(theme.muted_foreground).child(if self.delete_old {
                                            format!("Frees {live}. It is deleted only after SENTIENT has started on the restored data.")
                                        } else {
                                            "Kept under \"Kept after earlier changes\" until you remove it.".to_string()
                                        })),
                                )
                                .child(ui::outcome(theme, Tone::Warn, format!("Users, passwords, devices and dashboards will be those of the backup. Everything on this instance now is replaced (SENTIENT {here} keeps running until the switch).")));
                        }
                        panel = panel.child(
                            h_flex()
                                .gap_2()
                                .child(crate::ui::button("replace-go").primary().label("Replace this instance's data").disabled(busy || c.refusal.is_some()).on_click(cx.listener(|this, _, window, cx| this.confirm_replace(window, cx))))
                                .child(crate::ui::button("replace-back").outline().label("Back").on_click(cx.listener(|this, _, _, cx| {
                                    this.replace_stage = ReplaceStage::Choose;
                                    cx.notify();
                                }))),
                        );
                    }
                }
            }
            ReplaceStage::Run => {
                let job = self.restore.read(cx);
                let finished = match job.state() {
                    JobState::Done(_) => Some(true),
                    JobState::Failed(_) => Some(false),
                    _ => None,
                };
                let marked = ui::mark_steps(&self.planned, job.steps(), finished);
                let log: Vec<SharedString> = job.log().iter().rev().take(12).rev().cloned().collect();
                panel = panel.child(div().text_lg().child(match finished {
                    None => "Replacing this instance's data",
                    Some(true) => "This instance runs on the restored data",
                    Some(false) => "The data was not replaced",
                }));
                if finished.is_none() {
                    panel = panel.child(div().text_sm().text_color(theme.muted_foreground).child("SENTIENT keeps running on the current data until the switch."));
                }
                match job.state() {
                    JobState::Done(m) => panel = panel.child(ui::outcome(theme, Tone::Good, m.clone())),
                    JobState::Failed(m) => panel = panel.child(ui::outcome(theme, Tone::Bad, format!("{m} Nothing was deleted."))),
                    _ => {}
                }
                panel = panel.child(ui::steps(theme, &marked));
                if finished.is_none() {
                    if let Some(p) = job.percent() {
                        panel = panel.child(ProgressBar::new("replace-progress").value(p));
                    }
                    if let Some(l) = log.last() {
                        panel = panel.child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(l.clone()));
                    }
                }
                if finished == Some(true) {
                    let port = self.record.as_ref().map(|r| r.stack.http_port).unwrap_or(8080);
                    panel = panel
                        .child(div().pt_2().font_weight(FontWeight::MEDIUM).child("Sign in"))
                        .child(div().text_sm().child("Use the accounts of the backup. If you do not know its system administrator password, reset it in Platform settings."))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(crate::ui::button("open-sentient").primary().label("Open SENTIENT").on_click(move |_, _, cx| ui::open_sentient(port, false, cx)))
                                .child(crate::ui::button("reset-password").outline().label("Reset password…").on_click(cx.listener(|_, _, _, cx| cx.emit(Go::Settings)))),
                        )
                        .child(div().pt_2().font_weight(FontWeight::MEDIUM).child("Devices"))
                        .child(div().text_sm().child("Devices connect with the credentials stored in the backup. Point them or the gateway to this computer's address."));
                }
                if finished == Some(false) {
                    panel = panel
                        .child(
                            h_flex()
                                .gap_2()
                                .child(crate::ui::button("restore-log").outline().label(if self.show_restore_log { "Hide log" } else { "Show log" }).on_click(cx.listener(|this, _, _, cx| {
                                    this.show_restore_log = !this.show_restore_log;
                                    cx.notify();
                                })))
                                .child(crate::ui::button("restore-again").outline().label("Back").on_click(cx.listener(|this, _, _, cx| {
                                    this.replace_stage = ReplaceStage::Choose;
                                    cx.notify();
                                }))),
                        )
                        .when(self.show_restore_log, |p| p.children(log.into_iter().map(|l| div().text_size(px(13.)).text_color(theme.muted_foreground).child(l))));
                }
            }
        }
        panel
    }

}

impl BackupPage {
    /// J3/J4: a backup's run and its result (the strip's Details).
    fn run_view(&self, theme: &gpui_kit::component::Theme, cx: &App) -> Option<Div> {
        let job = self.backup.read(cx);
        if !matches!(job.state(), JobState::Idle) {
            let finished = match job.state() {
                JobState::Done(_) => Some(true),
                JobState::Failed(_) => Some(false),
                _ => None,
            };
            let marked = ui::mark_steps(&self.planned_backup, job.steps(), finished);
            let mut run = v_flex().gap_3();
            match (finished, &self.made) {
                (None, _) => {
                    run = run
                        .child(div().font_weight(FontWeight::MEDIUM).child(format!("Backing up to {}", self.prefs.folder.display())))
                        .when_some(job.log().last().cloned(), |c, l| c.child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(l)))
                        .when_some(job.percent(), |c, v| c.child(ProgressBar::new("backup-progress").value(v)))
                        .child(div().text_size(px(13.)).text_color(theme.muted_foreground).child("SENTIENT keeps running during a backup."));
                }
                (Some(true), Some(m)) => {
                    let name = m.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    let whole = m.files.first().map(|(_, h, _)| h.clone()).unwrap_or_default();
                    let short = if whole.len() > 16 { format!("{}…{}", &whole[..8], &whole[whole.len() - 8..]) } else { whole.clone() };
                    let folder = self.prefs.folder.display().to_string();
                    run = run
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(div().flex_1().child(ui::outcome(theme, Tone::Good, format!("Backup finished and checked: {name}, {}.", crate::dbstatus::bytes_label(m.bytes)))))
                                .child(crate::ui::button("open-folder").outline().label("Open folder").on_click(move |_, _, cx| ui::open_url(&folder, cx))),
                        )
                        .child(div().font_weight(FontWeight::MEDIUM).child("Integrity"))
                        .child(brow(theme, "Each part", None, div().text_sm().child(format!("SHA-256 stored inside the file, {} parts", m.members))))
                        .child(if m.files.len() == 1 {
                            brow(
                                theme,
                                "Whole file SHA-256",
                                None,
                                h_flex()
                                    .gap_2()
                                    .child(div().text_sm().font_family(theme.mono_font_family.clone()).child(short))
                                    .child(crate::ui::button("copy-hash").outline().label("Copy").on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(whole.clone())))),
                            )
                        } else {
                            brow(theme, "Files", None, div().text_sm().child(format!("{} parts, each with its SHA-256", m.files.len())))
                        })
                        .child(brow(theme, "Saved beside it", None, div().text_sm().child(m.sidecar.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())))
                        .child(brow(theme, "Read back", None, div().text_sm().child(m.check.summary.clone())))
                        .when_some(m.copied_to.clone(), |c, p| c.child(brow(theme, "Second copy", None, div().text_sm().child(p.display().to_string()))));
                }
                (Some(true), None) => run = run.child(ui::outcome(theme, Tone::Good, "Backup finished and checked.")),
                (Some(false), _) => {
                    if let JobState::Failed(e) = job.state() {
                        run = run.child(ui::outcome(theme, Tone::Bad, e.clone()));
                    }
                }
            }
            run = run.child(ui::steps(theme, &marked));
            return Some(run);
        }
        None

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
        let body = if self.check { page.check_view(&theme) } else { page.run_view(&theme, cx) };
        v_flex()
            .gap_3()
            .children(body)
            .child(h_flex().justify_end().child(crate::ui::button("bk-dialog-close").primary().label("Close").on_click(|_, _, cx| crate::modal::close(cx))))
    }
}

impl EventEmitter<Go> for BackupPage {}

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
                .when_some(help, |c, h| c.child(crate::ui::button(SharedString::from(format!("bhelp-{label}"))).ghost().xsmall().icon(IconName::Info).tooltip(h))),
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
                        .small()
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
        let options = [("everything", "Everything"), ("last90", "Last 90 days"), ("configuration", "Configuration only"), ("choose", "Choose…")];
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
            Some("Everything: the whole database and its files. Last 90 days: the same with only the last 90 days of telemetry. Configuration only: devices, dashboards, users and settings, without telemetry history. Choose: pick the parts and save the file anywhere."),
            what_control,
            None,
            false,
        );
        let pass_row = card_row(theme, narrow, "Encrypt", Some("A passphrase for Back up now; without it the file cannot be read. Automatic backups are not encrypted."), div().w(px(260.)).child(Input::new(&self.passphrase).mask_toggle()), None, false);
        let copy_row = card_row(
            theme,
            narrow,
            "Also copy to",
            Some("A second disk or a network folder, in case this computer fails."),
            h_flex()
                .min_w_0()
                .gap_2()
                .items_center()
                .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_sm().when(p.copy_to.is_some(), |d| d.font_family(theme.mono_font_family.clone()).text_size(px(13.5))).when(p.copy_to.is_none(), |d| d.text_color(theme.muted_foreground)).child(p.copy_to.as_ref().map(|d| d.display().to_string()).unwrap_or_else(|| "Nowhere".into())))
                .when(p.copy_to.is_some(), |r| {
                    r.child(crate::ui::button("copy-to-off").ghost().small().label("Stop copying").on_click(cx.listener(|this, _, _, cx| {
                        this.prefs.copy_to = None;
                        this.save_prefs(false, cx);
                    })))
                }),
            Some(crate::ui::button("copy-to").outline().label("Choose…").disabled(busy).on_click(cx.listener(|this, _, _, cx| this.choose_folder(true, cx))).into_any_element()),
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
            .child(h_flex().pb_1().gap_3().items_center().min_h(px(28.)).child(div().font_weight(FontWeight::MEDIUM).child("Backup")).children(self.saved_note(theme, "card")))
            .child(folder_row)
            .child(auto_row)
            .child(keep_row)
            .child(what_row)
            .child(pass_row)
            .child(copy_row)
            .child(split_row);
        out.push(card.into_any_element());

        // The Backups table: Back up now at its title, as a server's.
        let estimate = self.sizes.map(|(pg, ch, hist, free)| {
            let (bytes, secs) = crate::backups::estimate(p, pg, ch, hist);
            let mins = secs.div_ceil(60);
            let free_text = free.map(|f| format!(" · {} free", bytes_label(f))).unwrap_or_default();
            let low = free.is_some_and(|f| f < bytes.saturating_mul(2));
            (format!("Estimated file: {} · takes about {} minute{}{free_text}", bytes_label(bytes), mins, if mins == 1 { "" } else { "s" }), low)
        });
        let low = estimate.as_ref().filter(|(_, low)| *low).map(|(t, _)| t.clone());
        let now_tip = estimate.map(|(t, _)| t).unwrap_or_else(|| "A backup now, with what the card says.".into());
        let mut list = ui::panel(theme).gap_0().child(
            h_flex()
                .gap_2()
                .pb_2()
                .items_center()
                .child(div().flex_1().min_w_0().font_weight(FontWeight::MEDIUM).child("Backups"))
                .child(crate::ui::button("add-backup").ghost().label("Add a file…").tooltip("Add a backup file from elsewhere to the folder.").disabled(busy).on_click(cx.listener(|this, _, _, cx| this.add_backup_file(cx))))
                .when(!self.choose, |r| {
                    r.child(crate::ui::button("backup-now").outline().label(if busy { "Backing up…" } else { "Back up now" }).tooltip(now_tip).disabled(busy).on_click(cx.listener(|this, _, _, cx| this.run_preset_backup(cx))))
                }),
        );
        if let Some(t) = low {
            list = list.child(div().pb_2().text_sm().text_color(theme.danger).child(t));
        }
        if let Some((tone, text)) = self.schedule_note.clone() {
            list = list.child(div().pb_2().child(ui::outcome(theme, tone, text)));
        }
        if self.list.is_empty() {
            list = list.child(div().py_2().text_sm().text_color(theme.muted_foreground).child("No backups in this folder."));
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
                    restore: (!damaged).then(|| -> std::rc::Rc<dyn Fn(&mut Window, &mut App)> {
                        std::rc::Rc::new(move |_, cx| {
                            t1.update(cx, |this, cx| {
                                this.restore_input = Some(p1.clone());
                                this.replace_stage = ReplaceStage::Choose;
                                this.read_summary(cx);
                            })
                            .ok();
                        })
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
        let Some(record) = &self.record else {
            return page.child(ui::not_installed(&theme));
        };
        let clickhouse = record.flavor() == Flavor::Clickhouse;
        let busy = self.backup.read(cx).is_running() || self.restore.read(cx).is_running();
        let replace_panel = self.replace_panel(&theme, busy, cx);

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
        let panels = self.backup_panels(&theme, narrow, busy, cx);
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
                    .child(div().w_80().child(Input::new(&self.passphrase).mask_toggle()))
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(
                                crate::ui::button("output")
                                    .outline()
                                    .label("Save as…")
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
        .child(replace_panel)
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
pub(crate) fn card_row(theme: &gpui_kit::component::Theme, narrow: bool, label: &'static str, help: Option<&'static str>, value: impl IntoElement, action: Option<AnyElement>, last: bool) -> Div {
    let line = |d: Div| if last { d } else { d.border_b_1().border_color(theme.border) };
    let label_el = h_flex()
        .gap_1()
        .items_center()
        .text_sm()
        .text_color(theme.muted_foreground)
        .child(label)
        .when_some(help, |c, h| c.child(crate::ui::button(SharedString::from(format!("bhelp-{label}"))).ghost().xsmall().icon(IconName::Info).tooltip(h)));
    // A narrow window: the label above, the value and its action under it.
    if narrow {
        return line(v_flex().py(px(8.)).gap_1())
            .child(label_el)
            .child(h_flex().min_w_0().gap_2().items_center().child(h_flex().flex_1().min_w_0().gap_2().items_center().child(value)).children(action));
    }
    h_flex()
        .items_stretch()
        .gap(px(16.))
        .child(
            line(h_flex().flex_1().min_w_0().min_h(px(53.)).py(px(8.)).items_center().flex_wrap().gap_y_1())
                .child(label_el.w(px(CARD_LABEL_W)).flex_shrink_0())
                .child(h_flex().flex_1().min_w_0().gap_2().items_center().child(value)),
        )
        .child(line(h_flex().w(px(CARD_ACTION_W)).flex_shrink_0().items_center().justify_end()).children(action))
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
    let l = crate::dbstatus::bytes_label(b);
    match l.split_once(' ') {
        Some((n, unit)) if n.parse::<f64>().is_ok_and(|v| v >= 100.) => format!("{:.0} {unit}", n.parse::<f64>().unwrap_or(0.)),
        _ => l,
    }
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
    crate::ui::button(id).outline().icon(gpui_kit::component::Icon::new(gpui_kit::assets::IconName::Folder)).label("Change…").disabled(disabled)
}

/// Column widths of the Backups table: Made, Size, Integrity, the chevron.
const COL_MADE: f32 = 132.;
const COL_SIZE: f32 = 100.;
const COL_CHECK: f32 = 130.;
const COL_MENU: f32 = 36.;
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
        .child(div().w(px(COL_MENU)).flex_shrink_0())
}

/// "16 Nov, 02:00" from the list's "2026-11-16 02:00".
pub(crate) fn short_made(made: &str) -> String {
    chrono::NaiveDateTime::parse_from_str(made, "%Y-%m-%d %H:%M").map(|t| t.format("%-d %b, %H:%M").to_string()).unwrap_or_else(|_| made.to_string())
}

/// What a row's chevron offers.
pub(crate) struct RowActions {
    pub restore: Option<std::rc::Rc<dyn Fn(&mut Window, &mut App)>>,
    pub verify: std::rc::Rc<dyn Fn(&mut Window, &mut App)>,
    pub delete: std::rc::Rc<dyn Fn(&mut Window, &mut App)>,
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
    let RowActions { restore, verify, delete } = actions;
    let menu = crate::ui::button(SharedString::from(format!("bk-row-{i}")))
        .ghost()
        .small()
        .icon(IconName::ChevronRight)
        .disabled(busy)
        .dropdown_menu(move |menu, _, _| {
            let mut menu = menu;
            if let Some(r) = restore.clone() {
                menu = menu.item(PopupMenuItem::new("Restore…").on_click(move |_, window, cx| r(window, cx)));
            }
            let (v, d) = (verify.clone(), delete.clone());
            menu.item(PopupMenuItem::new("Verify").on_click(move |_, window, cx| v(window, cx))).separator().item(PopupMenuItem::new("Delete…").on_click(move |_, window, cx| d(window, cx)))
        });
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
                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(made_tip.clone()).build(window, cx)),
        )
        .child(div().w(px(COL_SIZE)).flex_shrink_0().font_features(crate::ui::tabular()).child(crate::dbstatus::bytes_label(e.bytes))))
        .child(
            h_flex()
                .id(SharedString::from(format!("bk-check-{i}")))
                .w(px(if narrow { COL_CHECK_NARROW } else { COL_CHECK }))
                .flex_shrink_0()
                .child(small_pill(theme, tone, text))
                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)),
        )
        .child(h_flex().w(px(COL_MENU)).flex_shrink_0().justify_end().child(menu))
}

/// The table's pills: smaller than the page's.
fn small_pill(theme: &gpui_kit::component::Theme, tone: Tone, text: &'static str) -> Div {
    ui::pill(theme, tone, text).px(px(7.)).py(px(1.)).text_size(px(12.))
}

/// B5: a backup left in a folder used before.
pub(crate) fn old_folder_pill(theme: &gpui_kit::component::Theme) -> Div {
    h_flex().flex_shrink_0().px(px(7.)).py(px(1.)).rounded_full().bg(theme.secondary).text_color(theme.muted_foreground).text_size(px(12.)).child("Old folder")
}
