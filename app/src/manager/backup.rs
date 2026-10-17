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
    ActiveTheme, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    input::{Input, InputState},
    progress::Progress as ProgressBar,
    radio::RadioGroup,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use sentient_backup_core::backup::{self, BackupOptions, FileStoreSpec, Selection};
use sentient_backup_core::clickhouse::{self, ChConn};
use sentient_backup_core::db::{self, CategoryReport, ConnConfig, DbInspector};
use sentient_backup_core::progress as bk;
use sentient_backup_core::restore::{self, RestoreOptions};
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
    restore_db: Entity<InputState>,
    restore: Entity<Job>,
    /// The database the running restore writes into, then the last one
    /// restored successfully (offered to be put into service).
    restoring: Option<String>,
    restored: Option<String>,
    switch: Entity<Job>,
    _tasks: Vec<Task<()>>,
    _observe: Vec<Subscription>,
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
        let restore_db = cx.new(|cx| InputState::new(window, cx).default_value("sentient_restored"));
        let backup = cx.new(|_| Job::new());
        let restore = cx.new(|_| Job::new());
        let switch = cx.new(|_| Job::new());
        let _observe = vec![
            cx.observe(&backup, |_, _, cx| cx.notify()),
            cx.observe(&restore, |page: &mut Self, job, cx| {
                if matches!(job.read(cx).state(), JobState::Done(_)) {
                    page.restored = page.restoring.take();
                }
                cx.notify();
            }),
            cx.observe(&switch, |page: &mut Self, job, cx| {
                if matches!(job.read(cx).state(), JobState::Done(_)) {
                    page.restored = None;
                }
                cx.notify();
            }),
        ];
        Self {
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
            restore_db,
            restore,
            restoring: None,
            restored: None,
            switch,
            _tasks: Vec::new(),
            _observe,
        }
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
        let dir = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default();
        let name = format!("sentient-{}.sentient-backup", chrono::Local::now().format("%Y%m%d-%H%M"));
        let rx = cx.prompt_for_new_path(&dir, Some(&name));
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
        let opts = BackupOptions { output, selection, file_stores, zstd_level: 10, passphrase, clickhouse };
        let record = self.record.clone();
        self.backup.update(cx, |job, cx| {
            job.start(
                move |sink| {
                    let summary = block_on(backup::run(&conn, &opts, bridge(sink.clone())))?
                        .map_err(|e| e.to_string())?;
                    if let Some(record) = &record {
                        crate::secrets_key::save_beside(record, &summary.output, &sink);
                    }
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
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Restore this backup".into()),
        });
        let task = cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            this.update(cx, |page, cx| {
                page.restore_input = paths.into_iter().next();
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    fn run_restore(&mut self, cx: &mut Context<Self>) {
        let target = self.restore_db.read(cx).value().trim().to_string();
        let (Some(input), Some(admin), Some(conn)) =
            (self.restore_input.clone(), self.conn(None, cx), self.conn(Some(target.clone()), cx))
        else {
            return;
        };
        let state_dir = self.record.as_ref().map(|r| r.state_dir.clone()).unwrap_or_default();
        let opts = RestoreOptions {
            input,
            allow_nonempty: false,
            file_store_paths: vec![
                ("reports".into(), state_dir.join("reports")),
                ("vc-repos".into(), state_dir.join("vc-repos")),
            ],
            passphrase: Some(self.restore_passphrase.read(cx).value().to_string()).filter(|p| !p.is_empty()),
            clickhouse: self.ch_conn(Some(target.clone()), cx),
            telemetry_into: None,
        };
        let live_ch = self.record.as_ref().map(|r| r.stack.ch_db.clone()).unwrap_or_default();
        let record = self.record.clone();
        self.restoring = Some(target.clone());
        self.restored = None;
        self.restore.update(cx, |job, cx| {
            job.start(
                move |sink| {
                    let mut opts = opts;
                    // A backup from another database type is converted to this one.
                    opts.telemetry_into = record.as_ref().map(crate::migration::telemetry_into).transpose()?;
                    block_on(async {
                        sink(Progress::Step { name: format!("Creating the empty database {target}") });
                        db::create_database(&admin, &target).await.map_err(|e| e.to_string())?;
                        if let Some(ch) = &opts.clickhouse {
                            sink(Progress::Step { name: format!("Creating the ClickHouse database {target}") });
                            clickhouse::clone_schema(ch, &live_ch).map_err(|e| e.to_string())?;
                        }
                        restore::run(&conn, &opts, bridge(sink.clone())).await.map_err(|e| e.to_string())
                    })??;
                    if let Some(record) = &record {
                        let ch_password = opts.clickhouse.as_ref().map(|c| c.password.clone()).unwrap_or_default();
                        crate::restore_switch::match_schema(record, &admin, &ch_password, &target, &sink)?;
                    }
                    crate::secrets_key::after_restore(&admin, &target, &opts.input, None, &sink)?;
                    sink(Progress::Done { message: format!("Restored into the database {target}") });
                    Ok(())
                },
                cx,
            )
        });
    }

    /// Put the restored database into service, after a confirmation.
    fn confirm_switch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(record), Some(name)) = (self.record.clone(), self.restored.clone()) else { return };
        let this = cx.entity().downgrade();
        let text = format!(
            "SENTIENT stops and starts again on the data in {name}. Its current database is kept under another name (listed under Migration, Kept after earlier changes); if SENTIENT does not start on the restored data, the current data is put back."
        );
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            let record = record.clone();
            let name = name.clone();
            dialog.title("Put the restored data into service?").description(text.clone()).confirm().ok_text("Switch").on_ok(move |_, _, cx| {
                let (record, name) = (record.clone(), name.clone());
                this.update(cx, |page, cx| {
                    page.switch.update(cx, |job, cx| {
                        if crate::rootjob::needed(&record) {
                            let args = vec!["use-restored".to_string(), name];
                            job.start(move |sink| crate::rootjob::run(&args, &sink).map(|_| ()), cx)
                        } else {
                            job.start(move |sink| crate::restore_switch::run(&record, &name, sink).map(|_| ()), cx)
                        }
                    });
                })
                .ok();
                true
            })
        });
    }

    fn job_view(&self, job: &Entity<Job>, cx: &App) -> Option<Div> {
        let theme = cx.theme().clone();
        let job = job.read(cx);
        let outcome = match job.state() {
            JobState::Idle => return None,
            JobState::Running => None,
            JobState::Done(m) => Some((Tone::Good, m.clone())),
            JobState::Failed(m) => Some((Tone::Bad, m.clone())),
        };
        Some(
            v_flex()
                .gap_2()
                .when(job.is_running(), |c| {
                    c.child(div().text_sm().child(job.step().clone()))
                        .when_some(job.percent(), |c, p| c.child(ProgressBar::new("job-progress").value(p)))
                })
                .children(outcome.map(|(tone, text)| ui::outcome(&theme, tone, text)))
                .children(job.log().iter().rev().take(6).rev().map(|l| {
                    div().text_xs().text_color(theme.muted_foreground).child(l.clone())
                })),
        )
    }
}

impl Render for BackupPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        let busy = self.backup.read(cx).is_running() || self.restore.read(cx).is_running() || self.switch.read(cx).is_running();
        let backup_status = self.job_view(&self.backup, cx);
        let restore_status = self.job_view(&self.restore, cx);
        let switch_status = self.job_view(&self.switch, cx);

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

        page.child(
            ui::panel(&theme)
                .child(div().font_weight(FontWeight::MEDIUM).child("Back up"))
                .child(
                    h_flex()
                        .gap_2()
                        .items_end()
                        .child(
                            v_flex()
                                .gap_1p5()
                                .w_64()
                                .child(div().text_xs().text_color(theme.muted_foreground).child("Database password"))
                                .child(Input::new(&self.password).mask_toggle()),
                        )
                        .when(clickhouse, |r| {
                            r.child(
                                v_flex()
                                    .gap_1p5()
                                    .w_64()
                                    .child(div().text_xs().text_color(theme.muted_foreground).child("ClickHouse password"))
                                    .child(Input::new(&self.ch_password).mask_toggle()),
                            )
                        })
                        .child(
                            Button::new("inspect")
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
                            .when(self.telemetry == 1, |r| r.child(div().w_20().child(Input::new(&self.telemetry_days).small()))),
                    )
                    .child(div().w_80().child(Input::new(&self.passphrase).mask_toggle()))
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(
                                Button::new("output")
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
                            Button::new("run-backup")
                                .primary()
                                .label("Back up now")
                                .disabled(!can_backup)
                                .on_click(cx.listener(|this, _, _, cx| this.run_backup(cx))),
                        ),
                    )
                })
                .children(backup_status),
        )
        .child(
            ui::panel(&theme)
                .child(div().font_weight(FontWeight::MEDIUM).child("Restore"))
                .child(div().text_sm().text_color(theme.muted_foreground).child(
                    "Restores into a new, empty database next to the current one (in ClickHouse too, on a ClickHouse install). Every part is checked against its checksum first. A backup from another database type is converted to this one; one from an older SENTIENT is upgraded.",
                ))
                .child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(
                            Button::new("restore-input")
                                .outline()
                                .label("Choose backup…")
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, _, cx| this.choose_restore_input(cx))),
                        )
                        .child(div().text_sm().text_color(theme.muted_foreground).child(
                            self.restore_input.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "No file chosen".into()),
                        )),
                )
                .child(
                    h_flex()
                        .gap_3()
                        .child(
                            v_flex()
                                .gap_1p5()
                                .w_64()
                                .child(div().text_xs().text_color(theme.muted_foreground).child("New database name"))
                                .child(Input::new(&self.restore_db)),
                        )
                        .child(
                            v_flex()
                                .gap_1p5()
                                .w_64()
                                .child(div().text_xs().text_color(theme.muted_foreground).child("Passphrase"))
                                .child(Input::new(&self.restore_passphrase).mask_toggle()),
                        ),
                )
                .child(
                    h_flex().child(
                        Button::new("run-restore")
                            .primary()
                            .label("Restore")
                            .disabled(busy || self.restore_input.is_none())
                            .on_click(cx.listener(|this, _, _, cx| this.run_restore(cx))),
                    ),
                )
                .children(restore_status)
                .when_some(self.restored.clone(), |p, name| {
                    p.child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(div().text_sm().child(format!("The backup is restored into {name}, next to the data SENTIENT uses.")))
                            .child(
                                Button::new("use-restored")
                                    .outline()
                                    .label("Put the restored data into service")
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, window, cx| this.confirm_switch(window, cx))),
                            ),
                    )
                })
                .children(switch_status),
        )
    }
}
