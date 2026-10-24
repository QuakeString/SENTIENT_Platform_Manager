// Copyright © 2016-2026 The SENTIENT Authors

//! The body of each wizard page.

use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Sizable as _, Theme, h_flex, v_flex,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    input::{Input, InputState},
    progress::Progress,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use sentient_installer_core::checks::Status;
use sentient_installer_core::stack::Flavor;

use super::{FINISH, INSTALL, Installer};
use crate::installation::{Engine, flavor_label};
use crate::jobs::JobState;
use crate::ui::{self, Tone};

const LICENCE: &str = include_str!("../../assets/licence.txt");

/// A labelled input.
fn field(theme: &Theme, label: &'static str, state: &Entity<InputState>) -> Div {
    v_flex()
        .gap_1p5()
        .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(theme.muted_foreground).child(label))
        .child(Input::new(state))
}

/// A labelled masked input with a show/hide toggle.
fn secret_field(theme: &Theme, label: &'static str, state: &Entity<InputState>) -> Div {
    v_flex()
        .gap_1p5()
        .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(theme.muted_foreground).child(label))
        .child(Input::new(state).mask_toggle())
}

fn lead(theme: &Theme, text: &'static str) -> Div {
    div().max_w(px(640.)).text_color(theme.muted_foreground).child(text)
}

fn check_tone(status: Status) -> (Tone, &'static str) {
    match status {
        Status::Pass => (Tone::Good, "Ready"),
        Status::Setup => (Tone::Warn, "Will be installed"),
        Status::Fail => (Tone::Bad, "Blocker"),
        Status::Warn => (Tone::Warn, "Attention"),
        Status::Unknown => (Tone::Neutral, "Unknown"),
    }
}

impl Installer {
    pub(super) fn page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.step {
            0 => self.welcome(cx).into_any_element(),
            1 => self.system_check(cx).into_any_element(),
            2 => self.location(cx).into_any_element(),
            3 => self.database(cx).into_any_element(),
            4 => self.database_settings(cx).into_any_element(),
            5 => self.ports(cx).into_any_element(),
            6 => self.administrator(cx).into_any_element(),
            7 => self.configuration(cx).into_any_element(),
            8 => self.shortcuts(cx).into_any_element(),
            9 => self.summary(cx).into_any_element(),
            INSTALL => self.installing(cx).into_any_element(),
            FINISH => self.finish(window, cx).into_any_element(),
            _ => div().into_any_element(),
        }
    }

    fn welcome(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let package: SharedString = match (&self.bundle, &self.online) {
            (Some(_), _) => "Offline package: everything is included".into(),
            (None, None) => "Looking for the latest SENTIENT online…".into(),
            (None, Some(Ok(m))) => format!(
                "Downloaded during install: SENTIENT {}, {}, checked before use",
                m.version,
                crate::online::size_label(m.download_size())
            )
            .into(),
            (None, Some(Err(_))) if cfg!(windows) => {
                "No SENTIENT package next to Setup and no SENTIENT download for this computer: only the Platform Manager can be installed".into()
            }
            (None, Some(Err(_))) => {
                "No SENTIENT package next to Setup and no SENTIENT download for this computer: Docker installs pull the image; a native install needs the package".into()
            }
        };
        let choice = |manager_only: bool, id: &'static str, title: &'static str, desc: &'static str| {
            let selected = self.manager_only == manager_only;
            h_flex()
                .id(id)
                .flex_1()
                .min_w_0()
                .gap_3()
                .p_3()
                .rounded(theme.radius)
                .bg(theme.background)
                .border_1()
                .border_color(if selected { theme.primary } else { theme.input })
                .when(selected, |c| c.bg(theme.list_active))
                .cursor_pointer()
                .child(
                    div()
                        .size_4()
                        .flex_shrink_0()
                        .rounded_full()
                        .border_2()
                        .border_color(if selected { theme.primary } else { theme.muted_foreground })
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(selected, |r| r.child(div().size_2().rounded_full().bg(theme.primary))),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(div().font_weight(FontWeight::MEDIUM).child(title))
                        .child(div().text_sm().text_color(theme.muted_foreground).child(desc)),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.manager_only = manager_only;
                    this.problem = None;
                    cx.notify();
                }))
        };
        v_flex()
            .gap_4()
            .child(lead(
                &theme,
                "Nothing changes on the computer until you press Install on the Summary page.",
            ))
            .child(
                h_flex()
                    .gap_3()
                    .items_stretch()
                    .child(choice(
                        false,
                        "install-both",
                        "SENTIENT and the Platform Manager",
                        "The platform, its database and the Platform Manager. You choose the database and every setting on the next pages.",
                    ))
                    .child(choice(
                        true,
                        "install-manager",
                        "Only the Platform Manager",
                        "The Platform Manager alone, without SENTIENT and its database. SENTIENT can be installed from it later, from a SENTIENT package.",
                    )),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(ui::panel(&theme).flex_1().gap_1()
                        .child(div().text_xs().text_color(theme.muted_foreground).child("SENTIENT"))
                        .child(div().font_weight(FontWeight::MEDIUM).child(self.sentient_version.clone())))
                    .child(ui::panel(&theme).flex_1().gap_1()
                        .child(div().text_xs().text_color(theme.muted_foreground).child("Platform Manager"))
                        .child(div().font_weight(FontWeight::MEDIUM).child(env!("CARGO_PKG_VERSION"))))
                    .child(ui::panel(&theme).flex_1().min_w_0().gap_1()
                        .child(div().text_xs().text_color(theme.muted_foreground).child("Package"))
                        .child(div().w_full().whitespace_normal().font_weight(FontWeight::MEDIUM).child(package))),
            )
            .child(
                ui::panel(&theme)
                    .id("licence")
                    .h(px(300.))
                    .overflow_y_scroll()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    // One block per paragraph or list item: line breaks
                    // inside a text run are not kept.
                    .children(LICENCE.lines().filter(|l| !l.trim().is_empty()).map(|l| {
                        // Headings are the short all-capitals lines.
                        let heading = l.len() < 60 && l == l.to_uppercase();
                        div().when(heading, |d| d.font_weight(FontWeight::MEDIUM).text_color(theme.foreground).pt_1()).child(l.to_string())
                    })),
            )
            .child(
                Checkbox::new("accept")
                    .label("I accept the licence agreement")
                    .checked(self.accepted)
                    .on_change(cx.listener(|this, value: &bool, _, cx| {
                        this.accepted = *value;
                        this.problem = None;
                        cx.notify();
                    })),
            )
    }

    fn system_check(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let rows = self.checks.clone();
        let engine_choice = (!cfg!(windows)).then(|| {
            let option = |engine: Engine, id: &'static str, title: &'static str, desc: &'static str, recommended: bool| {
                let selected = self.engine == engine;
                h_flex()
                    .id(id)
                    .flex_1()
                    .min_w_0()
                    .gap_3()
                    .p_3()
                    .rounded(theme.radius)
                    .bg(theme.background)
                    .border_1()
                    .border_color(if selected { theme.primary } else { theme.input })
                    .when(selected, |c| c.bg(theme.list_active))
                    .cursor_pointer()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(div().font_weight(FontWeight::MEDIUM).child(title))
                                    .when(recommended, |r| r.child(ui::pill(&theme, Tone::Good, "Recommended"))),
                            )
                            .child(div().text_sm().text_color(theme.muted_foreground).child(desc)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.set_engine(engine, cx)))
            };
            v_flex()
                .gap_2()
                .child(div().font_weight(FontWeight::MEDIUM).child("How should SENTIENT run?"))
                .child(
                    h_flex()
                        .gap_3()
                        .child(option(Engine::Docker, "engine-docker", "Docker on this machine", "SENTIENT and its databases as containers. Every database type.", true))
                        .child(option(Engine::Native, "engine-native", "Natively with systemd", "Services from the distribution's packages. PostgreSQL, or PostgreSQL + ClickHouse.", false)),
                )
        });
        v_flex()
            .gap_4()
            .children(engine_choice)
            .child(lead(&theme, "What SENTIENT needs on this computer. Items marked “Will be installed” are handled by setup."))
            .child(match rows {
                None => ui::panel(&theme).child(div().text_sm().child("Checking this computer…")),
                Some(rows) => ui::panel(&theme).gap_0().children(rows.into_iter().map(|row| {
                    let (tone, text) = check_tone(row.status);
                    h_flex()
                        .gap_3()
                        .py_2()
                        .border_b_1()
                        .border_color(theme.border)
                        .text_sm()
                        .child(div().w_32().flex_shrink_0().child(ui::pill(&theme, tone, text)))
                        .child(div().w_40().flex_shrink_0().font_weight(FontWeight::MEDIUM).child(row.label))
                        .child(div().min_w_0().text_color(theme.muted_foreground).child(row.detail))
                })),
            })
            .child(
                h_flex().child(
                    Button::new("recheck")
                        .outline()
                        .label("Check again")
                        .on_click(cx.listener(|this, _, _, cx| this.run_checks(cx))),
                ),
            )
    }

    fn browse(&mut self, field: Entity<InputState>, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let text = path.display().to_string();
            field
                .update_in(cx, |state, window, cx| state.set_value(text, window, cx))
                .ok();
        })
        .detach();
    }

    fn location(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let program = self.fields.install_dir.clone();
        let data = self.fields.data_dir.clone();
        if self.engine == Engine::Docker {
            return v_flex().gap_4().max_w(px(640.)).child(lead(
                &theme,
                "With Docker, SENTIENT and its databases keep their data in Docker volumes (st-pg-data, st-data, st-ch-data); there are no folders to choose. Make sure the disk that holds Docker's data has room for telemetry to grow.",
            ));
        }
        if !cfg!(windows) {
            return v_flex()
                .gap_4()
                .max_w(px(640.))
                .child(field(&theme, "Program folder", &program))
                .child(div().text_sm().text_color(theme.muted_foreground).child(
                    "PostgreSQL keeps its data where the distribution puts it (/var/lib/postgresql); SENTIENT's own files go to /var/lib/sentient.",
                ));
        }
        v_flex()
            .gap_4()
            .max_w(px(640.))
            .child(
                h_flex()
                    .gap_2()
                    .items_end()
                    .child(field(&theme, "Program folder", &program).flex_1())
                    .child(Button::new("browse-program").outline().label("Browse").on_click(
                        cx.listener(move |this, _, window, cx| this.browse(program.clone(), window, cx)),
                    )),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_end()
                    .child(field(&theme, "Database folder (PostgreSQL and its data)", &data).flex_1())
                    .child(Button::new("browse-data").outline().label("Browse").on_click(
                        cx.listener(move |this, _, window, cx| this.browse(data.clone(), window, cx)),
                    )),
            )
            .child(div().text_sm().text_color(theme.muted_foreground).child(
                "Put the database folder on the disk with the most free space: telemetry grows over time.",
            ))
    }

    fn database(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let (timescale, clickhouse) = match (cfg!(windows), self.engine) {
            (true, _) => (
                "Compressed time-series inside PostgreSQL. Fully native, fully offline.",
                "Best for large telemetry volumes and long history. ClickHouse runs in Docker inside WSL2.",
            ),
            (false, Engine::Docker) => (
                "Compressed time-series inside PostgreSQL.",
                "Best for large telemetry volumes and long history. ClickHouse runs as a container.",
            ),
            (false, Engine::Native) => (
                "",
                "Best for large telemetry volumes and long history. ClickHouse from the vendor's packages.",
            ),
        };
        let options: [(Flavor, &'static str, &'static str, bool); 3] = [
            (Flavor::Stock, "Simplest and smallest. For small sites and a few hundred devices.", "All native", false),
            (Flavor::Timescaledb, timescale, "All native", false),
            (Flavor::Clickhouse, clickhouse, "Native + WSL2", true),
        ];
        let offered = self.flavors();
        let docker_linux = !cfg!(windows) && self.engine == Engine::Docker;
        let cards = options.into_iter().filter(|o| offered.contains(&o.0)).map(|(flavor, desc, runs, recommended)| {
            let runs = if docker_linux { "Docker" } else if cfg!(windows) { runs } else { "Native" };
            let selected = self.flavor == Some(flavor);
            h_flex()
                .id(flavor.as_str())
                .gap_4()
                .p_4()
                .rounded(theme.radius)
                .bg(theme.background)
                .border_1()
                .border_color(if selected { theme.primary } else { theme.input })
                .when(selected, |c| c.bg(theme.list_active))
                .cursor_pointer()
                .child(
                    div()
                        .size_4()
                        .flex_shrink_0()
                        .rounded_full()
                        .border_2()
                        .border_color(if selected { theme.primary } else { theme.muted_foreground })
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(selected, |r| r.child(div().size_2().rounded_full().bg(theme.primary))),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .gap_1()
                        .child(
                            h_flex()
                                .gap_2()
                                .child(div().font_weight(FontWeight::MEDIUM).child(flavor_label(flavor)))
                                .when(recommended, |r| r.child(ui::pill(&theme, Tone::Good, "Recommended"))),
                        )
                        .child(div().text_sm().text_color(theme.muted_foreground).child(desc)),
                )
                .child(div().text_xs().text_color(theme.muted_foreground).child(runs))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.flavor = Some(flavor);
                    this.problem = None;
                    cx.notify();
                }))
        });
        v_flex()
            .gap_3()
            .child(lead(
                &theme,
                if cfg!(windows) {
                    "Choose how SENTIENT stores its data. SENTIENT and PostgreSQL always run directly on Windows. The database type can be changed later with Migration in the Platform Manager."
                } else {
                    "Choose how SENTIENT stores its data. The database type can be changed later with Migration in the Platform Manager."
                },
            ))
            .children(cards)
            .when(cfg!(windows) && self.flavor == Some(Flavor::Clickhouse), |col| {
                col.child(
                    div()
                        .p_3()
                        .rounded(theme.radius)
                        .bg(theme.info)
                        .text_color(theme.info_foreground)
                        .text_sm()
                        .child("ClickHouse runs in Docker inside WSL2 under its own hidden service account, so it starts at boot with nobody signed in. If WSL2 is not enabled yet, Windows restarts once and setup continues by itself."),
                )
            })
    }

    fn database_settings(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let f = &self.fields;
        let pg_title = match self.flavor {
            Some(Flavor::Timescaledb) => "PostgreSQL + TimescaleDB",
            _ => "PostgreSQL",
        };
        v_flex()
            .gap_4()
            .child(
                ui::panel(&theme)
                    .child(div().font_weight(FontWeight::MEDIUM).child(pg_title))
                    .child(
                        h_flex()
                            .gap_3()
                            .child(field(&theme, "Database name", &f.pg_db).flex_1())
                            .child(field(&theme, "User", &f.pg_user).flex_1())
                            .child(field(&theme, "Port", &f.pg_port).w_24()),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .child(secret_field(&theme, "Password", &f.pg_password).flex_1())
                            .child(secret_field(&theme, "PostgreSQL admin password", &f.pg_admin_password).flex_1()),
                    ),
            )
            .when(self.flavor == Some(Flavor::Clickhouse), |col| {
                col.child(
                    ui::panel(&theme)
                        .child(div().font_weight(FontWeight::MEDIUM).child("ClickHouse (Docker in WSL2)"))
                        .child(
                            h_flex()
                                .gap_3()
                                .child(field(&theme, "Database", &f.ch_db).flex_1())
                                .child(field(&theme, "User", &f.ch_user).flex_1())
                                .child(secret_field(&theme, "Password", &f.ch_password).flex_1()),
                        )
                        .child(
                            h_flex()
                                .gap_3()
                                .child(field(&theme, "HTTP port", &f.ch_http_port).w_32())
                                .child(field(&theme, "Native port", &f.ch_native_port).w_32()),
                        ),
                )
            })
            .children(self.port_warning(cx))
    }

    /// The full port message under the fields, when a chosen port is taken.
    fn port_warning(&self, cx: &App) -> Option<Div> {
        let theme = cx.theme();
        self.port_problem(cx).map(|text| {
            h_flex()
                .gap_2()
                .p_3()
                .max_w(px(640.))
                .rounded(theme.radius)
                .bg(theme.warning)
                .text_color(theme.warning_foreground)
                .text_sm()
                .child(Icon::new(IconName::TriangleAlert).small())
                .child(div().min_w_0().child(text))
        })
    }

    fn ports(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let f = &self.fields;
        let row = |label: &'static str, state: &Entity<InputState>| {
            h_flex()
                .gap_3()
                .items_center()
                .child(div().w_48().text_sm().child(label))
                .child(div().w_32().child(Input::new(state)))
        };
        v_flex()
            .gap_4()
            .child(lead(&theme, "Setup checks that each port is free before moving on."))
            .child(
                ui::panel(&theme)
                    .max_w(px(480.))
                    .child(row("Web (HTTP)", &f.http_port))
                    .child(row("MQTT", &f.mqtt_port))
                    .child(row("MQTT over TLS", &f.mqtts_port))
                    .child(row("CoAP", &f.coap_port))
                    .child(row("CoAP over DTLS", &f.coaps_port)),
            )
            .children(self.port_warning(cx))
    }

    fn administrator(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let f = &self.fields;
        v_flex()
            .gap_4()
            .max_w(px(480.))
            .child(lead(&theme, "The first SENTIENT sign-in, with full system rights. More users can be added in SENTIENT."))
            .child(field(&theme, "E-mail", &f.admin_email))
            .child(secret_field(&theme, "Password (at least 8 characters)", &f.admin_password))
            .child(secret_field(&theme, "Confirm password", &f.admin_confirm))
            .child(
                Checkbox::new("demo")
                    .label("Load demo devices and dashboards")
                    .checked(self.load_demo)
                    .on_change(cx.listener(|this, value: &bool, _, cx| {
                        this.load_demo = *value;
                        cx.notify();
                    })),
            )
    }

    fn configuration(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let toggle = h_flex()
            .gap_1()
            .p_0p5()
            .rounded_full()
            .bg(theme.background)
            .border_1()
            .border_color(theme.input)
            .child(
                Button::new("recommended")
                    .label("Recommended")
                    .when(!self.customize, |b| b.primary())
                    .when(self.customize, |b| b.ghost())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.customize = false;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("customize")
                    .label("Customize")
                    .when(self.customize, |b| b.primary())
                    .when(!self.customize, |b| b.ghost())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.customize = true;
                        cx.notify();
                    })),
            );
        let body = if self.customize { self.customize_panel(cx) } else { self.recommended_panel(cx) };
        v_flex().gap_4().child(h_flex().child(toggle)).child(body)
    }

    fn recommended_panel(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let get = |key: &str| self.recommended.get(key).cloned().unwrap_or_else(|| "SENTIENT default".into());
        ui::panel(&theme)
            .child(div().text_sm().text_color(theme.muted_foreground).child(format!(
                "Sized for this computer: {} GB of memory.",
                (self.ram_mb + 512) / 1024
            )))
            .child(ui::fact(&theme, "PostgreSQL shared_buffers", get("pg:shared_buffers")))
            .child(ui::fact(&theme, "PostgreSQL effective_cache_size", get("pg:effective_cache_size")))
            .child(ui::fact(&theme, "Audit log kept", format!("{} days", get("AUDIT_LOG_TTL_DAYS"))))
            .child(ui::fact(&theme, "Notifications kept", format!("{} days", get("NOTIFICATIONS_TTL_DAYS"))))
            .child(ui::fact(&theme, "Everything else", "SENTIENT's tested defaults"))
            .child(div().text_xs().text_color(theme.muted_foreground).child(
                "Choose Customize to see and change every setting. All of them can also be changed later in the Platform Manager.",
            ))
    }

    fn customize_panel(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let needle = self.fields.search.read(cx).value().to_lowercase();
        let total: usize = self.groups.iter().map(|g| g.settings.len()).sum();
        let groups = self.groups.iter().filter_map(|group| {
            let rows: Vec<_> = group
                .settings
                .iter()
                .filter(|s| {
                    needle.is_empty()
                        || format!("{} {} {}", s.label, s.env, group.name).to_lowercase().contains(&needle)
                })
                .map(|s| {
                    h_flex()
                        .gap_3()
                        .items_center()
                        .py_1()
                        .child(div().w_56().flex_shrink_0().text_sm().child(s.label.clone()))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .font_family(theme.mono_font_family.clone())
                                .text_color(theme.muted_foreground)
                                .child(s.env.clone()),
                        )
                        .child(div().w_48().flex_shrink_0().child(Input::new(&self.settings[&s.env]).small()))
                        .child(
                            Button::new(SharedString::from(format!("help-{}", s.env)))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Info)
                                .tooltip(s.help.clone()),
                        )
                })
                .collect();
            (!rows.is_empty()).then(|| {
                v_flex()
                    .child(
                        div()
                            .py_1p5()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.muted_foreground)
                            .child(group.name.to_uppercase()),
                    )
                    .children(rows)
            })
        });
        v_flex()
            .gap_3()
            .child(Input::new(&self.fields.search))
            .child(div().text_xs().text_color(theme.muted_foreground).child(format!(
                "{total} settings. Empty fields keep the value shown in grey. Values are written to the SENTIENT service."
            )))
            .child(ui::panel(&theme).gap_2().children(groups))
    }

    fn shortcuts(&mut self, cx: &mut Context<Self>) -> Div {
        let options: Vec<(&'static str, &'static str, bool)> = if self.manager_only {
            if cfg!(windows) {
                vec![
                    ("desktop-manager", "Desktop shortcut for the Platform Manager", self.desktop_manager),
                    ("start-menu", "Start menu entry", self.start_menu),
                ]
            } else {
                vec![("start-menu", "Application menu entry for the Platform Manager", self.start_menu)]
            }
        } else if cfg!(windows) {
            vec![
                ("autostart", "Start SENTIENT automatically with Windows", self.autostart),
                ("desktop-sentient", "Desktop shortcut for SENTIENT", self.desktop_sentient),
                ("desktop-manager", "Desktop shortcut for the Platform Manager", self.desktop_manager),
                ("start-menu", "Start menu entries", self.start_menu),
                ("kiosk", "Full-screen (kiosk) shortcut for operator screens", self.kiosk),
                ("open-at-signin", "Open SENTIENT full screen when the computer starts (operator screens)", self.open_at_signin),
            ]
        } else {
            vec![
                ("start-menu", "Application menu entries for SENTIENT and the Platform Manager", self.start_menu),
                ("kiosk", "Full-screen (kiosk) launcher for operator screens", self.kiosk),
                ("open-at-signin", "Open SENTIENT full screen when the computer starts (operator screens)", self.open_at_signin),
            ]
        };
        let note = (!cfg!(windows) && !self.manager_only).then(|| {
            let theme = cx.theme().clone();
            div().text_sm().text_color(theme.muted_foreground).child(
                "SENTIENT starts with the computer: as a systemd service, or through Docker's restart policy.",
            )
        });
        let theme = cx.theme().clone();
        // Signing in by itself only matters for the screen that opens SENTIENT.
        let autologin = (!self.manager_only && self.open_at_signin).then(|| {
            v_flex()
                .pl_6()
                .gap_3()
                .child(
                    Checkbox::new("autologin")
                        .label("Also sign in to this computer automatically after a restart")
                        .checked(self.autologin)
                        .on_change(cx.listener(|this, value: &bool, _, cx| {
                            this.autologin = *value;
                            cx.notify();
                        })),
                )
                .when(self.autologin, |col| {
                    col.child(
                        h_flex()
                            .gap_3()
                            .child(div().w_64().child(field(&theme, "Operator account", &self.fields.autologin_user)))
                            .when(cfg!(windows), |row| {
                                row.child(div().w_64().child(secret_field(
                                    &theme,
                                    "Account password",
                                    &self.fields.autologin_password,
                                )))
                            }),
                    )
                    .child(ui::outcome(
                        &theme,
                        Tone::Warn,
                        "Anyone at this screen gets this account without typing a password. Use an operator account with only the rights the operator needs.",
                    ))
                })
        });
        v_flex()
            .gap_3()
            .children(note)
            .children(options.into_iter().map(|(id, label, checked)| {
                Checkbox::new(id)
                    .label(label)
                    .checked(checked)
                    .on_change(cx.listener(move |this, value: &bool, _, cx| {
                        match id {
                            "autostart" => this.autostart = *value,
                            "desktop-sentient" => this.desktop_sentient = *value,
                            "desktop-manager" => this.desktop_manager = *value,
                            "start-menu" => this.start_menu = *value,
                            "open-at-signin" => this.open_at_signin = *value,
                            _ => this.kiosk = *value,
                        }
                        cx.notify();
                    }))
            }))
            .children(autologin)
    }

    fn summary(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let f = &self.fields;
        let v = |s: &Entity<InputState>| self.value(s, cx);
        let mut rows: Vec<(&'static str, String, usize)> = Vec::new();
        if self.manager_only {
            let mut shortcuts = Vec::new();
            if cfg!(windows) && self.desktop_manager {
                shortcuts.push("desktop");
            }
            if self.start_menu {
                shortcuts.push(if cfg!(windows) { "Start menu" } else { "application menu" });
            }
            rows.push(("Installs", format!("SENTIENT Platform Manager {}, without SENTIENT", env!("CARGO_PKG_VERSION")), 0));
            rows.push(("Program folder", crate::self_install::manager_dir().display().to_string(), 0));
            rows.push((
                "Shortcuts",
                if shortcuts.is_empty() { "None".into() } else { shortcuts.join(" and ") },
                super::SHORTCUTS,
            ));
        } else {
            if !cfg!(windows) {
                let runs = if self.engine == Engine::Docker { "Docker on this machine" } else { "Natively with systemd" };
                rows.push(("Runs as", runs.to_string(), 1));
            }
            if self.engine == Engine::Native {
                rows.push(("Program folder", v(&f.install_dir), 2));
            }
            if cfg!(windows) {
                rows.push(("Database folder", v(&f.data_dir), 2));
            }
            rows.extend(vec![
                ("Database", self.flavor.map(flavor_label).unwrap_or("Not chosen").to_string(), 3),
                ("PostgreSQL", format!("{} as {} on port {}", v(&f.pg_db), v(&f.pg_user), v(&f.pg_port)), 4),
            ]);
            if self.flavor == Some(Flavor::Clickhouse) {
                let place = if cfg!(windows) { "Docker in WSL2" } else { "Docker" };
                rows.push((
                    "ClickHouse",
                    format!("{} as {} on port {} ({place})", v(&f.ch_db), v(&f.ch_user), v(&f.ch_http_port)),
                    4,
                ));
            }
            rows.extend(vec![
                (
                    "Ports",
                    format!(
                        "Web {} · MQTT {} / {} · CoAP {} / {}",
                        v(&f.http_port),
                        v(&f.mqtt_port),
                        v(&f.mqtts_port),
                        v(&f.coap_port),
                        v(&f.coaps_port)
                    ),
                    5,
                ),
                ("Administrator", v(&f.admin_email), 6),
                (
                    "Configuration",
                    if self.customize {
                        format!("Customized ({} settings set)", self.chosen_settings(cx).len())
                    } else {
                        "Recommended for this computer".into()
                    },
                    7,
                ),
                (
                    "Startup",
                    if !cfg!(windows) || self.autostart { "Starts with the computer".into() } else { "Started by hand".into() },
                    8,
                ),
            ]);
            if self.open_at_signin {
                let screen = if self.autologin {
                    format!("Signs in as {} and opens SENTIENT full screen after a restart", v(&f.autologin_user))
                } else {
                    "Opens SENTIENT full screen when someone signs in".into()
                };
                rows.push(("Operator screen", screen, 8));
            }
        }
        ui::panel(&theme).gap_0().children(rows.into_iter().map(|(label, value, step)| {
            h_flex()
                .gap_3()
                .py_2()
                .border_b_1()
                .border_color(theme.border)
                .text_sm()
                .child(div().w_40().flex_shrink_0().text_color(theme.muted_foreground).child(label))
                .child(div().flex_1().min_w_0().font_weight(FontWeight::MEDIUM).child(value))
                .child(
                    Button::new(SharedString::from(format!("change-{step}-{label}")))
                        .ghost()
                        .small()
                        .label("Change")
                        .on_click(cx.listener(move |this, _, _, cx| this.go(step, cx))),
                )
        }))
    }

    fn installing(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let job = self.install.read(cx);
        let state = job.state().clone();
        let step = job.step_with_time();
        let percent = job.percent();
        let log: Vec<SharedString> = job.log().iter().rev().take(16).rev().cloned().collect();
        v_flex()
            .gap_4()
            .child(
                v_flex()
                    .gap_2()
                    .child(div().text_sm().child(step))
                    .when_some(percent, |c, p| c.child(Progress::new("install-progress").value(p))),
            )
            .when(matches!(&state, JobState::Failed(m) if m.as_ref() == super::plan::RESTART_REQUIRED), |c| {
                c.child(
                    v_flex()
                        .gap_2()
                        .p_3()
                        .rounded(theme.radius)
                        .bg(theme.info)
                        .text_color(theme.info_foreground)
                        .text_sm()
                        .child(div().font_weight(FontWeight::MEDIUM).child("Restart required"))
                        .child("Windows must restart to finish enabling WSL2 for ClickHouse. Setup continues by itself after you sign in again; your choices are kept.")
                        .child(h_flex().child(
                            Button::new("restart-now")
                                .primary()
                                .label("Restart now")
                                .on_click(|_, _, _| super::plan::restart_now()),
                        )),
                )
            })
            .when_some(
                match &state {
                    JobState::Failed(m) if m.as_ref() != super::plan::RESTART_REQUIRED => Some(m.clone()),
                    _ => None,
                },
                |c, message| {
                    c.child(
                        v_flex()
                            .gap_1()
                            .p_3()
                            .rounded(theme.radius)
                            .bg(theme.danger)
                            .text_color(theme.danger_foreground)
                            .text_sm()
                            .child(div().font_weight(FontWeight::MEDIUM).child("The install stopped"))
                            .child(message)
                            .child("Fix the cause and press Retry. Tasks already done are skipped or repeated safely."),
                    )
                },
            )
            .child(
                ui::panel(&theme)
                    .gap_0p5()
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .text_color(theme.muted_foreground)
                    .children(log.into_iter().map(|line| div().child(line))),
            )
    }

    fn finish(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        if self.manager_only {
            return v_flex()
                .gap_4()
                .child(
                    div()
                        .size_12()
                        .rounded_full()
                        .bg(theme.success)
                        .text_color(theme.success_foreground)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Icon::new(IconName::Check).large()),
                )
                .child(lead(
                    &theme,
                    "The Platform Manager is installed. To install SENTIENT later, open it and choose Install SENTIENT, or run SENTIENT Setup from a SENTIENT package again.",
                ))
                .child(h_flex().child(Button::new("open-manager").primary().label("Open Platform Manager").on_click(
                    |_, _, _| {
                        let _ = std::process::Command::new(crate::self_install::manager_exe()).arg("--manager").spawn();
                    },
                )));
        }
        let url = format!("http://localhost:{}", self.value(&self.fields.http_port, cx));
        let email = self.value(&self.fields.admin_email, cx);
        let open_url = url.clone();
        v_flex()
            .gap_4()
            .child(
                div()
                    .size_12()
                    .rounded_full()
                    .bg(theme.success)
                    .text_color(theme.success_foreground)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::new(IconName::Check).large()),
            )
            .child(
                ui::panel(&theme)
                    .max_w(px(620.))
                    .child(ui::fact(&theme, "Address", url))
                    .child(ui::fact(&theme, "Sign in as", email))
                    .child(ui::fact(
                        &theme,
                        "Managed by",
                        if cfg!(windows) {
                            "SENTIENT Platform Manager (Start menu)"
                        } else {
                            "SENTIENT Platform Manager (application menu, or sentient-platform-manager)"
                        },
                    )),
            )
            .child(
                div()
                    .max_w(px(620.))
                    .p_3()
                    .rounded(theme.radius)
                    .bg(theme.info)
                    .text_color(theme.info_foreground)
                    .text_sm()
                    .child("Next: sign in to SENTIENT as the system administrator and activate your licence under Licence. Activation needs internet once."),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("open-sentient")
                            .primary()
                            .label("Open SENTIENT")
                            .on_click(move |_, _, cx| ui::open_url(&open_url, cx)),
                    )
                    .child(Button::new("open-manager").outline().label("Open Platform Manager").on_click(
                        |_, _, _| {
                            let _ = std::process::Command::new(crate::self_install::manager_exe())
                                .arg("--manager")
                                .spawn();
                        },
                    )),
            )
    }
}
