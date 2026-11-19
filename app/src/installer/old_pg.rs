// Copyright © 2016-2026 The SENTIENT Authors

//! Setup's System check, native Linux: a PostgreSQL data folder from an
//! older major, shown before anything is installed, with the two ways
//! forward (sentient_installer_core::old_pg): upgrade it (the data is
//! kept), or keep it aside and start fresh (moved, never deleted). The
//! install applies the choice right after PostgreSQL is installed.
//!
//! Arch and Fedora keep the folder readable by postgres only, so its
//! version may be unknown until "Look inside" reads it as administrator;
//! the choice then applies only if it turns out older.

use gpui_kit::component::{ActiveTheme as _, Disableable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use sentient_installer_core::old_pg::{self, OldPgChoice, OldPgData};

use super::Installer;
use crate::installation::Engine;
use crate::ui;

impl Installer {
    /// Look for an older-major data folder (native Linux only).
    pub(super) fn look_for_old_pg(&mut self, cx: &mut Context<Self>) {
        self.old_pg = None;
        if cfg!(windows) || self.engine != Engine::Native {
            return;
        }
        let task = cx.spawn(async move |this, cx| {
            #[cfg(debug_assertions)]
            let canned = std::env::var("SPM_DEBUG_OLD_PG").ok();
            #[cfg(not(debug_assertions))]
            let canned: Option<String> = None;
            let found = match canned {
                // Screens: SPM_DEBUG_OLD_PG=17 (read) or "unknown".
                Some(v) => Some(OldPgData { family: old_pg::Family::Arch, dir: "/var/lib/postgres/data".into(), have: v.parse().ok(), want: Some(18) }),
                None => cx.background_executor().spawn(async move { old_pg::find() }).await,
            };
            this.update(cx, |page, cx| {
                if found.is_some() && page.old_pg_choice == OldPgChoice::Ask {
                    // Upgrading keeps the data, and does nothing to a
                    // folder that turns out current: the safe default.
                    page.old_pg_choice = OldPgChoice::Upgrade;
                }
                page.old_pg = found;
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    /// Read the folder's version as administrator (one password prompt).
    fn look_inside_old_pg(&mut self, cx: &mut Context<Self>) {
        let Some(found) = self.old_pg.clone() else { return };
        self.old_pg_probing = true;
        cx.notify();
        let task = cx.spawn(async move |this, cx| {
            let read = cx.background_executor().spawn(async move { old_pg::probe_privileged(&found) }).await;
            this.update(cx, |page, cx| {
                page.old_pg_probing = false;
                match read {
                    // The same major: nothing to do.
                    Some(f) if f.have == f.want => {
                        page.old_pg = None;
                        page.old_pg_choice = OldPgChoice::Ask;
                    }
                    Some(f) => page.old_pg = Some(f),
                    None => page.problem = Some(t!("Could not read the PostgreSQL data folder (the password prompt was closed?).").into()),
                }
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    /// The panel on the System check page.
    pub(super) fn old_pg_panel(&mut self, cx: &mut Context<Self>) -> Option<Div> {
        let found = self.old_pg.clone()?;
        let theme = cx.theme().clone();
        let want = found.want.map(|w| tf!("PostgreSQL {w}")).unwrap_or_else(|| t!("the new version").into());
        let title = match found.have {
            Some(h) => tf!("PostgreSQL {h} data is on this computer"),
            None => t!("PostgreSQL data is already on this computer").into(),
        };
        let dir = found.dir.display().to_string();
        let aside = match found.family {
            old_pg::Family::Debian => t!("renamed aside as an old cluster, stopped").to_string(),
            _ => tf!("moved to {dir}.old-<date>"),
        };
        let kept = match found.family {
            old_pg::Family::Arch => t!(" The old folder is kept as /var/lib/postgres/olddata."),
            old_pg::Family::Rhel => t!(" The old folder is kept as /var/lib/pgsql/data-old."),
            old_pg::Family::Debian => t!(" The old cluster is kept, stopped."),
        };
        let help = tf!(
            "{want} cannot open data made by an older major version, so PostgreSQL would stop at start.\n\
             Upgrade: the databases are copied into {want} (pg_upgrade); nothing is lost.{kept}\n\
             Keep it aside: the folder is {aside}, and SENTIENT starts with an empty database. Nothing is deleted."
        );
        let choice = self.old_pg_choice;
        let card = |id: &'static str, c: OldPgChoice, title: String, desc: String| {
            ui::radio_card(&theme, id, title, desc, choice == c).flex_1().min_w_0().on_click(cx.listener(move |this, _, _, cx| {
                this.old_pg_choice = c;
                cx.notify();
            }))
        };
        Some(
            ui::panel(&theme)
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(div().font_weight(FontWeight::MEDIUM).child(title))
                        .child(ui::info("old-pg-help".into(), help.into())),
                )
                .child(div().text_sm().text_color(theme.muted_foreground).child(match found.have {
                    Some(_) => tf!("In {dir}. {want} will be installed. Choose what Setup does with it:"),
                    None => tf!("In {dir}. If it is from an older version than {want}, Setup does this with it:"),
                }))
                .child(
                    h_flex()
                        .gap_3()
                        .items_stretch()
                        .when(ui::narrow(), |r| r.flex_col())
                        .child(card("old-pg-upgrade", OldPgChoice::Upgrade, tf!("Upgrade it to {want}"), t!("Keeps every database. Recommended.").into()))
                        .child(card("old-pg-aside", OldPgChoice::KeepAside, t!("Keep it aside and start fresh").into(), t!("Moves the folder away; nothing is deleted.").into())),
                )
                .when(found.have.is_none(), |p| {
                    p.child(
                        h_flex().child(
                            ui::button("old-pg-look")
                                .outline()
                                .label(if self.old_pg_probing { t!("Looking") } else { t!("Look inside (asks for the password)") })
                                .disabled(self.old_pg_probing)
                                .on_click(cx.listener(|this, _, _, cx| this.look_inside_old_pg(cx))),
                        ),
                    )
                }),
        )
    }
}
