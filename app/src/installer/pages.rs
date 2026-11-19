// Copyright © 2016-2026 The SENTIENT Authors

//! The body of each wizard page.

use gpui_kit::component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _, Theme, h_flex, v_flex,
    button::ButtonVariants as _,
    checkbox::Checkbox,
    input::{Input, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    scroll::ScrollableElement as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use sentient_installer_core::checks::Status;
use sentient_installer_core::diagnose::{Fix, FixAction};
use sentient_installer_core::stack::Flavor;

use super::placement::{self, PARTS, Part, Place, Placement, Why, Wsl};
use super::{FINISH, INSTALL, Installer};
use crate::installation::{Engine, flavor_label};
use crate::jobs::JobState;
use crate::ui::{self, Tone};

const LICENCE: &str = include_str!("../../assets/licence.txt");

/// A field's label: the body size, muted. As tall as an (i), so fields
/// side by side line up whether their label has one or not.
fn label(theme: &Theme, text: &'static str) -> Div {
    h_flex().h(px(22.)).gap_1().items_center().text_sm().font_weight(FontWeight::MEDIUM).text_color(theme.muted_foreground).child(text)
}

/// An input reported to the layout test (`LAYOUT field`): the edges of
/// fields side by side are not a card's row divider.
fn probed_input(input: Input) -> Div {
    ui::probed_input(input)
}

/// A labelled input.
fn field(theme: &Theme, text: &'static str, state: &Entity<InputState>) -> Div {
    v_flex().gap_1p5().child(label(theme, text)).child(probed_input(Input::new(state)))
}

/// A labelled masked input with a show/hide toggle.
fn secret_field(theme: &Theme, text: &'static str, state: &Entity<InputState>) -> Div {
    v_flex().gap_1p5().child(label(theme, text)).child(probed_input(Input::new(state).mask_toggle()))
}

/// The help on a database password Setup fills in (an (i) beside its label).
const DEFAULT_PASSWORD_HELP: &str = n!("Default password. Change it if the database port is reachable from other computers.");

/// [`secret_field`] holding Setup's default password, with its (i).
fn default_password_field(theme: &Theme, id: &'static str, text: &'static str, state: &Entity<InputState>) -> Div {
    v_flex()
        .gap_1p5()
        .child(label(theme, text).child(ui::info(id.into(), crate::i18n::t(DEFAULT_PASSWORD_HELP).into())))
        .child(probed_input(Input::new(state).mask_toggle()))
}

/// One choice of several as a Platform Manager card (ui::radio_card),
/// reported to the layout test like every card.
fn choice_card(theme: &Theme, id: impl Into<ElementId>, title: AnyElement, desc: impl Into<SharedString>, selected: bool) -> Stateful<Div> {
    ui::radio_card_el(theme, id, title, desc, selected, true).relative().child(ui::layout_probe("panel"))
}

/// A card's title with an optional "Recommended" pill.
fn choice_title(theme: &Theme, title: impl Into<SharedString>, recommended: bool) -> AnyElement {
    h_flex()
        .gap_2()
        .items_center()
        .child(div().font_weight(FontWeight::MEDIUM).child(title.into()))
        .when(recommended, |r| r.child(ui::pill(theme, Tone::Good, t!("Recommended"))))
        .into_any_element()
}

/// Two choice cards at one height, side by side (ui::card_pair).
fn pair(a: Stateful<Div>, b: Stateful<Div>) -> Div {
    ui::card_pair(v_flex().child(a.flex_1()), v_flex().child(b.flex_1()))
}

/// Why the install stopped: the Platform Manager's error banner
/// (ui::notice, Tone::Bad: a faint red tint, the danger colour, the panel
/// radius), one block of its own under the progress. Retry is the footer's
/// Next.
fn install_failed(theme: &Theme, message: SharedString) -> Div {
    ui::notice(
        theme,
        Tone::Bad,
        tf!("The install stopped\n{message}\nFix the cause and press Retry. Tasks already done are skipped or repeated safely."),
    )
}

/// A page's blocks, a page gap apart.
fn column() -> Div {
    v_flex().w_full().gap(px(ui::GAP))
}

/// A card title that can't be picked: its text faded, its (i) with the
/// reason at full strength.
fn off_title(text: &'static str, why: impl IntoElement) -> AnyElement {
    h_flex().gap_2().items_center().child(div().font_weight(FontWeight::MEDIUM).opacity(ui::OFF_OPACITY).child(text)).child(why).into_any_element()
}

/// A Database card's right side: where it runs, a line per database when
/// they run in different places.
fn runs_column(theme: &Theme, lines: &[&'static str], fade: f32) -> Div {
    v_flex()
        .flex_shrink_0()
        .items_end()
        .gap_0p5()
        .text_sm()
        .text_color(theme.muted_foreground)
        .when(fade < 1., |c| c.opacity(fade))
        .children(lines.iter().map(|l| div().whitespace_nowrap().child(*l)))
}

/// A check's pill: its tone, and its own words when the check gives them
/// ("Already installed", "In use") over the status's.
fn check_tone(status: Status, state: Option<SharedString>) -> (Tone, SharedString) {
    let (tone, text) = match status {
        Status::Pass => (Tone::Good, t!("Ready")),
        Status::Setup => (Tone::Warn, t!("Will be installed")),
        Status::Fail => (Tone::Bad, t!("Blocker")),
        Status::Warn => (Tone::Warn, t!("Attention")),
        Status::Unknown => (Tone::Neutral, t!("Unknown")),
    };
    (tone, state.unwrap_or_else(|| text.into()))
}

fn gb(mb: u64) -> u64 {
    (mb + 512) / 1024
}

/// The Recommended panel's first line, and the (i) that says where a WSL 2
/// VM's memory comes from.
fn sized_for(s: crate::catalog::Sizing, docker: bool) -> (String, Option<String>) {
    let Some(vm) = s.vm_mb else { return (tf!("Sized for this computer: {} GB of memory.", gb(s.host_mb)), None) };
    let source = if s.vm_from_wslconfig {
        t!("the memory= line of .wslconfig in your user folder sets it").to_string()
    } else {
        t!("WSL 2 gives its VM half of the computer's memory unless .wslconfig sets memory=").to_string()
    };
    if docker {
        (
            tf!("Sized for the WSL 2 VM: {} GB of this computer's {} GB.", gb(vm), gb(s.host_mb)),
            Some(tf!("Memory of the WSL 2 VM\n\nSENTIENT and its databases run in Docker inside the WSL 2 VM, which gets {} GB: {source}.", gb(vm))),
        )
    } else {
        (
            tf!("Sized for this computer: {} GB, less {} GB for ClickHouse in WSL 2.", gb(s.host_mb), gb(vm)),
            Some(tf!("Memory of the WSL 2 VM\n\nClickHouse runs in its own WSL 2 VM, which can take {} GB: {source}. PostgreSQL is sized for the rest.", gb(vm))),
        )
    }
}

/// Windows Docker: SENTIENT's CoAP (UDP) is reachable from this computer
/// only. The keep-alive forwards TCP ports from every address of Windows
/// to SENTIENT's WSL distro (distro::forwarded_ports); netsh's port proxy
/// cannot forward UDP, and WSL 2 forwards a UDP port to Windows' own
/// localhost only.
fn coap_local_only(windows: bool, engine: Engine) -> bool {
    windows && engine == Engine::Docker
}

const COAP_LOCAL_HELP: &str = n!("CoAP: this computer only\n\nUDP is not forwarded from WSL 2, so devices on other computers cannot reach SENTIENT's CoAP ports. \
Devices on this computer can. Devices on the network can use MQTT or HTTP, or install SENTIENT “In the system”.");

/// The Summary's Ports row: [web, MQTT, MQTTS, CoAP, CoAPS]. CoAP says so
/// when it is this computer's only (coap_local_only).
fn ports_summary(p: [String; 5], coap_local_only: bool) -> String {
    let [web, mqtt, mqtts, coap, coaps] = p;
    let coap_where = if coap_local_only { t!(" (this computer only)") } else { "" };
    tf!("Web {web} · MQTT {mqtt} / {mqtts} · CoAP {coap} / {coaps}{coap_where}")
}

/// Whether a System check row that is not ready gets "Check again": not
/// on a "Will be installed" row with nothing to do on it (Setup installs
/// that part, and reading the computer again cannot change it), but on
/// every row a re-check can change (an action such as "Start Docker", a
/// blocker, a busy port, the download servers).
fn offers_check_again(id: &str, status: Status, pill: &str, fixes: &[Fix]) -> bool {
    let will_be_installed = status == Status::Setup && pill == t!("Will be installed");
    let has_action = fixes.iter().any(|f| matches!(f, Fix::Action { .. }));
    !will_be_installed || has_action || id == "downloads"
}

/// Grid rows: every cell the same width, the same gap apart, so the column
/// edges line up from row to row and from card to card (an empty cell
/// keeps its column).
fn grid_row(cells: Vec<Option<Div>>) -> Div {
    h_flex().gap_3().items_start().children(cells.into_iter().map(|c| v_flex().flex_1().min_w_0().children(c)))
}

impl Installer {
    /// The line under the step's title (as under every Platform Manager
    /// page's title).
    pub(super) fn subtitle(&self) -> Option<SharedString> {
        let text: &str = match self.step {
            0 => t!("Nothing changes on the computer until you press Install on the Summary page."),
            1 => t!("What SENTIENT needs on this computer. Items marked “Will be installed” are handled by setup."),
            2 if self.engine == Engine::Docker && self.windows => t!("SENTIENT's distribution keeps the containers and all their data on one disk. Put it on the disk with the most room: telemetry grows."),
            2 if self.engine == Engine::Docker => t!("SENTIENT and its databases keep their data in Docker volumes. Put them on the disk with the most room: telemetry grows."),
            2 => t!("Where SENTIENT and its database go. Put the database on the disk with the most free space: telemetry grows."),
            3 => t!("How SENTIENT stores its data. The type can be changed later with Migration in the Platform Manager."),
            4 => t!("The database SENTIENT signs in to. Setup checks that its ports are free."),
            5 => t!("Setup checks that each port is free before moving on."),
            6 => t!("The first SENTIENT sign-in, with full system rights. More users can be added in SENTIENT."),
            7 => t!("SENTIENT's settings, sized for this computer. All of them can be changed later in the Platform Manager."),
            8 if !self.windows && !self.manager_only => t!("Where to find SENTIENT. It starts with the computer: as a systemd service, or through Docker's restart policy."),
            8 => t!("Where to find SENTIENT and how it starts."),
            9 => t!("Check your choices. Nothing has been installed yet."),
            INSTALL => t!("Setup is installing. This takes a few minutes."),
            FINISH if self.manager_only => t!("The Platform Manager is installed. Open it to install SENTIENT later."),
            FINISH => t!("SENTIENT is installed and running."),
            _ => return None,
        };
        Some(text.into())
    }

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

    /// With no package next to Setup: the published versions to install,
    /// the newest picked. None when the list could not be read (the latest
    /// release alone, as before) or a package is next to Setup. A Platform
    /// Manager select: an outline button with the choice and a chevron,
    /// as tall as the top bar's controls.
    fn version_menu(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if self.bundle.is_some() || self.manager_only {
            return None;
        }
        let list = match &self.versions {
            Some(Ok(list)) if !list.is_empty() => list.clone(),
            _ => return None,
        };
        let chosen = self.chosen_version.clone().unwrap_or_else(|| list[0].version.clone());
        let this = cx.entity().downgrade();
        let label = if chosen == list[0].version { tf!("{chosen} (newest)") } else { chosen.clone() };
        Some(
            crate::ui::button("sentient-version")
                .outline()
                .h(px(crate::theme::BAR_CTRL_H))
                .px_3()
                .label(label)
                .dropdown_caret(true)
                .dropdown_menu(move |menu, _, _| {
                    let mut menu = menu.max_h(px(320.)).scrollable(true);
                    for v in &list {
                        let this = this.clone();
                        let pick = v.version.clone();
                        let label = if v.version == list[0].version { tf!("{} (newest)", v.label()) } else { v.label() };
                        menu = menu.item(PopupMenuItem::new(label).checked(v.version == chosen).on_click(move |_, _, cx| {
                            let pick = pick.clone();
                            this.update(cx, |page, cx| page.choose_version(pick, cx)).ok();
                        }));
                    }
                    menu
                }),
        )
    }

    fn welcome(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let package: SharedString = match (&self.bundle, &self.online) {
            (Some(_), _) => t!("Offline package: everything is included").into(),
            (None, None) => t!("Looking for the latest SENTIENT online…").into(),
            (None, Some(Ok(m))) => tf!(
                "Downloaded during install: SENTIENT {}, {}, checked before use",
                m.version,
                crate::online::size_label(m.download_size())
            )
            .into(),
            (None, Some(Err(_))) if cfg!(windows) => {
                t!("No SENTIENT package next to Setup and no SENTIENT download for this computer: only the Platform Manager can be installed").into()
            }
            (None, Some(Err(_))) => {
                t!("No SENTIENT package next to Setup and no SENTIENT download for this computer: Docker installs pull the image; a native install needs the package").into()
            }
        };
        let manager_version = if self.adding_sentient {
            tf!("{}, installed", crate::updates::manager_version())
        } else {
            crate::updates::manager_version().to_string()
        };
        // What is installed: the versions and where SENTIENT comes from.
        let what = ui::panel(&theme)
            .flex_shrink_0()
            .child(
                h_flex()
                    .gap_4()
                    .text_sm()
                    .items_center()
                    .child(div().w(px(ui::CARD_LABEL_W)).flex_shrink_0().text_color(theme.muted_foreground).child("SENTIENT"))
                    .child(match self.version_menu(cx) {
                        Some(menu) => h_flex().child(menu).into_any_element(),
                        None => div().flex_1().min_w_0().child(self.sentient_version.clone()).into_any_element(),
                    }),
            )
            .child(ui::card_fact(&theme, "Platform Manager", manager_version))
            .child(ui::card_fact(&theme, t!("Package"), package));
        let choice = |manager_only: bool, id: &'static str, title: &'static str, desc: &'static str| {
            choice_card(&theme, id, choice_title(&theme, title, false), desc, self.manager_only == manager_only).on_click(cx.listener(move |this, _, _, cx| {
                this.manager_only = manager_only;
                this.problem = None;
                cx.notify();
            }))
        };
        // The licence fills what is left of the window and scrolls inside
        // the card, its scrollbar beside the text (room on the right, as in
        // the notifications panel).
        let licence = ui::panel(&theme)
            .flex_1()
            .min_h(px(120.))
            .child(
                div()
                    .id("licence")
                    .flex_1()
                    .min_h_0()
                    .child(
                        v_flex()
                            .pr(px(12.))
                            .gap_2()
                            .text_sm()
                            .text_color(theme.foreground)
                            // One block per paragraph or list item: line breaks
                            // inside a text run are not kept.
                            .children(LICENCE.lines().filter(|l| !l.trim().is_empty()).map(|l| {
                                // Headings are the short all-capitals lines.
                                let heading = l.len() < 60 && l == l.to_uppercase();
                                div().when(heading, |d| d.font_weight(FontWeight::SEMIBOLD).pt_1()).child(l.to_string())
                            })),
                    )
                    .overflow_y_scrollbar(),
            );
        column()
            .flex_1()
            .min_h_0()
            .when(!self.adding_sentient, |c| {
                c.child(pair(
                    choice(
                        false,
                        "install-both",
                        t!("SENTIENT and the Platform Manager"),
                        t!("The platform, its database and the Platform Manager. You choose the database and every setting on the next pages."),
                    ),
                    choice(
                        true,
                        "install-manager",
                        t!("Only the Platform Manager"),
                        t!("The Platform Manager alone, without SENTIENT and its database. SENTIENT can be installed from it later, from a SENTIENT package."),
                    ),
                ))
            })
            .child(what)
            .child(licence)
            .child(
                div().flex_shrink_0().child(
                    Checkbox::new("accept")
                        .label(t!("I accept the licence agreement"))
                        .checked(self.accepted)
                        .on_change(cx.listener(|this, value: &bool, _, cx| {
                            this.accepted = *value;
                            this.problem = None;
                            cx.notify();
                        })),
                ),
            )
    }

    fn system_check(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        // Windows: the WSL 2 row says how WSL stands for the choice.
        let rows = self.checks.clone().map(|rows| if self.windows { super::with_wsl_row(rows, self.engine, self.docker_choice.as_ref()) } else { rows });
        let engine_choice = self.engine_choice(&theme, cx);
        let cleanup = self.cleanup.read(cx);
        // jobview: short (removing what an earlier install left, on its check's row).
        let (cleaning, cleanup_state, cleanup_step) = (cleanup.is_running(), cleanup.state().clone(), cleanup.step_with_time());
        let leftovers = (!self.leftovers.is_empty() || cleaning || !matches!(cleanup_state, JobState::Idle)).then(|| {
            let items = self.leftovers.clone();
            ui::panel(&theme)
                .child(div().font_weight(FontWeight::MEDIUM).child(t!("An earlier SENTIENT install is still on this computer")))
                .children(items.into_iter().map(|i| div().text_sm().child(format!("• {i}"))))
                .when(!self.leftovers.is_empty(), |p| {
                    p.child(div().text_sm().text_color(theme.muted_foreground).child(
                        t!("Keep it to use its database again (offered under Database settings), or remove it so this install starts clean and its ports are free."),
                    ))
                })
                .child(
                    h_flex()
                        .gap_3()
                        .when(!self.leftovers.is_empty(), |r| {
                            r.child(
                                crate::ui::button("remove-leftovers")
                                    .danger()
                                    .label(t!("Remove the earlier install"))
                                    .disabled(cleaning)
                                    .on_click(cx.listener(|this, _, window, cx| this.confirm_remove_leftovers(window, cx))),
                            )
                        })
                        .when(cleaning, |r| r.child(div().text_sm().child(cleanup_step.clone()))),
                )
                .when_some(
                    match &cleanup_state {
                        JobState::Done(m) => Some(ui::outcome(&theme, Tone::Good, m.clone())),
                        JobState::Failed(m) => Some(ui::outcome(&theme, Tone::Bad, m.clone())),
                        _ => None,
                    },
                    |p, o| p.child(o),
                )
        });
        // An older PostgreSQL's data: upgrade or keep aside, before installing.
        let old_pg = self.old_pg_panel(cx);
        column()
            .child(engine_choice)
            .children(leftovers)
            .children(old_pg)
            .child(match rows {
                None => ui::panel(&theme).child(div().text_sm().child(t!("Checking this computer…"))),
                Some(rows) => {
                    let rows: Vec<AnyElement> = rows.into_iter().enumerate().map(|(i, row)| self.check_row(i, row, cx)).collect();
                    ui::panel(&theme).gap_0().children(rows)
                }
            })
            .children(self.check_fix_outcome(cx))
            .child(
                h_flex().child(
                    crate::ui::button("recheck")
                        .outline()
                        .label(t!("Check again"))
                        .on_click(cx.listener(|this, _, _, cx| this.run_checks(cx))),
                ),
            )
    }

    /// Where SENTIENT runs, on Windows and Linux alike: "In the system"
    /// (the default) or "Docker" (inside WSL 2 on Windows), then the
    /// Customise rows and, for Docker without WSL 2 yet, the restart notice.
    /// A Windows where WSL 2 cannot run gets the Docker card greyed with
    /// the reason (docs/mockups/setup-engine.html, W1 to W3, L1 and L2).
    fn engine_choice(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let host = self.host();
        let (sys_desc, docker_desc) = if host.windows {
            (t!("SENTIENT and PostgreSQL as Windows services. They start with Windows."), t!("SENTIENT and its databases as containers, in Docker Engine inside WSL 2."))
        } else {
            (t!("Services from the distribution's packages, run by systemd."), t!("SENTIENT and its databases as containers. Every database type."))
        };
        let native = choice_card(theme, "engine-native", choice_title(theme, t!("In the system"), true), sys_desc, self.engine == Engine::Native)
            .on_click(cx.listener(|this, _, _, cx| this.set_engine(Engine::Native, cx)));
        let docker = if host.windows && host.wsl == Wsl::Unavailable {
            let title = off_title(t!("Docker"), ui::info("engine-docker-why".into(), tf!("Docker is not possible on this computer\n\n{}", self.wsl_detail()).into()));
            ui::radio_card_off(theme, "engine-docker", title, t!("Not available on this computer.")).relative().child(ui::layout_probe("panel"))
        } else {
            choice_card(theme, "engine-docker", choice_title(theme, t!("Docker"), false), docker_desc, self.engine == Engine::Docker)
                .on_click(cx.listener(|this, _, _, cx| this.set_engine(Engine::Docker, cx)))
        };
        column()
            .child(pair(native, docker))
            .child(self.customise(theme, cx))
            .when(host.windows && self.engine == Engine::Docker && host.wsl == Wsl::TurnOn, |c| {
                c.child(ui::notice(
                    theme,
                    Tone::Neutral,
                    t!("Windows restarts once\nWSL 2 is not on yet. Setup turns it on during the install, Windows restarts, and Setup continues by itself after you sign in."),
                ))
            })
    }

    /// "Customise where each part runs": a row per part (the server,
    /// PostgreSQL with TimescaleDB, ClickHouse), each "In the system |
    /// Docker". In this version the rows mirror the card: a choice that
    /// would mix places is off with its reason (0.9.3), and so is one this
    /// computer can't run (placement::segment).
    fn customise(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let open = self.customise_open;
        let toggle = h_flex()
            .id("customise")
            .gap_2()
            .items_center()
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.primary)
            .cursor_pointer()
            .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).size(px(16.)))
            .child(t!("Customise where each part runs"))
            .on_click(cx.listener(|this, _, _, cx| {
                this.customise_open = !this.customise_open;
                cx.notify();
            }));
        let col = v_flex().gap(px(ui::GAP)).child(h_flex().child(toggle));
        if !open {
            return col;
        }
        let host = self.host();
        let current = Placement::preset(self.engine, host.windows);
        let detail = self.wsl_detail();
        let rows = PARTS.into_iter().enumerate().map(|(i, part)| {
            let (name, sub, help): (&str, Option<&str>, Option<&str>) = match part {
                Part::Server => (t!("SENTIENT server"), None, None),
                Part::Postgres => ("PostgreSQL", Some(t!("and TimescaleDB")), Some(t!("TimescaleDB goes with PostgreSQL\n\nTimescaleDB is an extension inside PostgreSQL, so it always runs where PostgreSQL runs."))),
                Part::Clickhouse => ("ClickHouse", None, Some(t!("Only with PostgreSQL + ClickHouse\n\nUsed when you choose PostgreSQL + ClickHouse on the Database step."))),
            };
            let key = match part {
                Part::Server => "server",
                Part::Postgres => "pg",
                Part::Clickhouse => "ch",
            };
            let seg = [(Place::System, t!("In the system")), (Place::Docker, t!("Docker"))].into_iter().fold(ui::seg_track(theme), |track, (place, label)| {
                let id = SharedString::from(format!("part-{key}-{}", if place == Place::System { "system" } else { "docker" }));
                match placement::segment(host, current, part, place) {
                    Ok(engine) => track.child(ui::seg_item(theme, id, label, current.get(part) == place).on_click(cx.listener(move |this, _, _, cx| this.set_engine(engine, cx)))),
                    Err(why) => track.child(ui::seg_item_off(theme, id.clone(), label, ui::info(format!("{id}-why").into(), why.text(&detail).into()))),
                }
            });
            // The name's first line on the control's line (ui::on_control_line),
            // "and TimescaleDB" under it.
            h_flex()
                .gap_3()
                .items_start()
                .py(px(10.))
                .when(i > 0, |r| r.border_t_1())
                .border_color(theme.border)
                .child(
                    v_flex()
                        .w(px(160.))
                        .flex_shrink_0()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(ui::on_control_line(h_flex().gap_1().items_center().child(name).when_some(help, |r, help| r.child(ui::info(format!("part-{key}-help").into(), help.into())))))
                        .when_some(sub, |c, sub| c.child(div().mt(px(-6.)).text_size(px(12.)).child(sub))),
                )
                .child(seg)
        });
        col.child(ui::panel(theme).gap_0().py(px(4.)).children(rows))
    }

    /// One System check row: the pill, the name, the cause, and, when it
    /// is not ready, what can be done: its actions as buttons, "Check
    /// again" where a re-check can change the row (offers_check_again),
    /// and "Show command" for the exact commands.
    fn check_row(&self, i: usize, row: super::CheckRow, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let (tone, text) = check_tone(row.status, row.state.clone());
        // The WSL 2 row is Setup's own reading of WSL (with_wsl_row), not
        // a check to run again: no buttons on it.
        let ready = row.status == Status::Pass || row.id == "wsl";
        let fix = self.check_fix.read(cx);
        // jobview: short (a check's one-click fix, on its row).
        let (fixing, fix_step) = (fix.is_running(), fix.step_with_time());
        let here = self.check_fix_at.as_ref().filter(|(r, _)| *r == row.id).map(|(_, l)| l.clone());
        let mut buttons: Vec<AnyElement> = Vec::new();
        // (label, command): the actions' own scripts, then the commands to copy.
        let mut commands: Vec<(String, String)> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        for (n, f) in row.fixes.iter().enumerate() {
            match f {
                Fix::Action { label, action } => {
                    if let Some(c) = action.command() {
                        commands.push((label.clone(), c));
                    }
                    let (id, label, action) = (row.id.clone(), label.clone(), action.clone());
                    let danger = matches!(action, FixAction::RemovePostgres { .. });
                    let b = ui::button(SharedString::from(format!("check-fix-{}-{n}", row.id))).label(label.clone()).disabled(fixing);
                    let b = if danger { b.danger() } else { b.outline() };
                    buttons.push(b.on_click(cx.listener(move |this, _, window, cx| this.check_action(id.clone(), label.clone(), action.clone(), window, cx))).into_any_element());
                }
                Fix::Command { label, command } => commands.push((label.clone(), command.clone())),
                Fix::Steps { text } => notes.push(text.clone()),
            }
        }
        let shown = self.commands_shown.contains(&row.id);
        if !ready && offers_check_again(&row.id, row.status, text.as_ref(), &row.fixes) {
            buttons.push(
                ui::button(SharedString::from(format!("check-again-{}", row.id)))
                    .ghost()
                    .label(t!("Check again"))
                    .disabled(fixing)
                    .on_click(cx.listener(|this, _, _, cx| this.run_checks(cx)))
                    .into_any_element(),
            );
        }
        if !ready {
            if !commands.is_empty() {
                let id = row.id.clone();
                buttons.push(
                    ui::button(SharedString::from(format!("check-cmd-{}", row.id)))
                        .ghost()
                        .label(match (shown, commands.len()) {
                            (true, 1) => t!("Hide command"),
                            (true, _) => t!("Hide commands"),
                            (false, 1) => t!("Show command"),
                            (false, _) => t!("Show commands"),
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.commands_shown.remove(&id) {
                                this.commands_shown.insert(id.clone());
                            }
                            cx.notify();
                        }))
                        .into_any_element(),
                );
            }
        }
        let rid = row.id.clone();
        h_flex()
            .gap_3()
            .py(px(10.))
            .items_start()
            .when(i > 0, |r| r.border_t_1())
            .border_color(theme.border)
            .text_sm()
            // The label keeps its own width in a fixed column.
            .child(h_flex().w(px(148.)).flex_shrink_0().child(ui::pill(&theme, tone, text)))
            .child(div().w(px(ui::CARD_LABEL_W + 30.)).flex_shrink_0().font_weight(FontWeight::MEDIUM).child(row.label))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .child(div().text_color(theme.muted_foreground).child(row.detail))
                    .children(notes.into_iter().map(|n| div().text_color(theme.muted_foreground).child(n)))
                    .when(!ready && !buttons.is_empty(), |c| c.child(ui::button_row(buttons)))
                    .when_some(here.filter(|_| fixing), |c, label| c.child(div().child(format!("{label}: {fix_step}"))))
                    .when(shown && !ready, |c| {
                        c.children(commands.into_iter().enumerate().map(|(n, (label, command))| {
                            crate::diagnosis::command_box_in(&theme, SharedString::from(format!("check-copy-{rid}-{n}")), &label, &command, theme.border)
                        }))
                    }),
            )
            .into_any_element()
    }

    /// How the last System check action ended, under the list (the list
    /// itself is read again then).
    fn check_fix_outcome(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (_, label) = self.check_fix_at.clone()?;
        let theme = cx.theme().clone();
        let job = self.check_fix.read(cx);
        match job.state() {
            JobState::Done(m) => Some(ui::outcome(&theme, Tone::Good, format!("{label}: {m}")).into_any_element()),
            JobState::Failed(m) => Some(crate::diagnosis::outcome(&theme, Tone::Bad, tf!("{label} did not finish: {m}"), job)),
            _ => None,
        }
    }

    fn browse(&mut self, field: Entity<InputState>, window: &mut Window, cx: &mut Context<Self>) {
        let start = crate::chooser::start_for(&field.read(cx).value().to_string(), None);
        let rx = crate::chooser::open(
            cx,
            crate::chooser::Ask { title: t!("Choose a folder"), start: start.as_deref(), directory: true },
            PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some(t!("Choose").into()) },
        );
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
        if self.engine == Engine::Docker && self.windows {
            return column().child(ui::panel(&theme).child(
                h_flex()
                    .gap_2()
                    .items_end()
                    .child(field(&theme, t!("Folder for SENTIENT's WSL distribution and its data"), &program).flex_1())
                    .child(crate::ui::button("browse-program").outline().label(t!("Browse")).on_click(
                        cx.listener(move |this, _, window, cx| this.browse(program.clone(), window, cx)),
                    )),
            ));
        }
        if self.engine == Engine::Docker {
            return column().child(self.data_locations(&theme, cx));
        }
        if !self.windows {
            return column().child(ui::panel(&theme)
                .child(field(&theme, t!("Program folder"), &program))
                .child(div().text_sm().text_color(theme.muted_foreground).child(
                    t!("PostgreSQL keeps its data where the distribution puts it (/var/lib/postgresql); SENTIENT's own files go to /var/lib/sentient."),
                )));
        }
        column().child(ui::panel(&theme)
            .gap_4()
            .child(
                h_flex()
                    .gap_2()
                    .items_end()
                    .child(field(&theme, t!("Program folder"), &program).flex_1())
                    .child(crate::ui::button("browse-program").outline().label(t!("Browse")).on_click(
                        cx.listener(move |this, _, window, cx| this.browse(program.clone(), window, cx)),
                    )),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_end()
                    .child(field(&theme, t!("Database folder (PostgreSQL and its data)"), &data).flex_1())
                    .child(crate::ui::button("browse-data").outline().label(t!("Browse")).on_click(
                        cx.listener(move |this, _, window, cx| this.browse(data.clone(), window, cx)),
                    )),
            )
        )
    }

    /// Docker: Advanced, data locations (mockup M14): one folder for
    /// everything (Docker's own when empty), or a place per store; each
    /// volume keeps its name and is bound to its folder.
    fn data_locations(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let per = self.loc_per_store;
        let seg = crate::ui::seg_track(theme)
            .child(crate::ui::seg_item(theme, "loc-one", t!("One folder"), !per).on_click(cx.listener(|this, _, _, cx| {
                this.loc_per_store = false;
                cx.notify();
            })))
            .child(crate::ui::seg_item(theme, "loc-per", t!("A place per store"), per).on_click(cx.listener(|this, _, _, cx| {
                this.loc_per_store = true;
                cx.notify();
            })));
        let row = |label: &'static str, id: &'static str, field: Entity<InputState>, last: bool, cx: &mut Context<Self>| {
            let f2 = field.clone();
            h_flex()
                .gap_3()
                .items_center()
                .py(px(8.))
                .when(!last, |r| r.border_b_1().border_color(theme.border))
                .child(div().w(px(150.)).flex_shrink_0().text_sm().text_color(theme.muted_foreground).child(label))
                .child(div().flex_1().min_w_0().font_family(theme.mono_font_family.clone()).child(probed_input(Input::new(&field))))
                .child(crate::ui::button(id).outline().icon(Icon::new(IconName::Folder)).label(t!("Change")).on_click(cx.listener(move |this, _, window, cx| this.browse(f2.clone(), window, cx))))
                .into_any_element()
        };
        let mut card = crate::ui::panel(theme)
            .gap_0()
            .child(div().pb_2().font_weight(FontWeight::MEDIUM).child(t!("Data locations")))
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .py(px(8.))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(h_flex().w(px(150.)).flex_shrink_0().gap_1().items_center().text_sm().text_color(theme.muted_foreground).child(t!("Data")).child(crate::ui::info(
                        "loc-help".into(),
                        t!("One folder keeps everything together (postgres, clickhouse and files under it; Docker's own folder when left empty); a place per store puts each database on its own disk. The volumes keep their names (st-pg-data, st-ch-data, st-data): they stay in Docker's own folder unless you give them a place here.").into(),
                    )))
                    .child(seg),
            );
        let (one, pg, ch, data) = (self.fields.loc_one.clone(), self.fields.loc_pg.clone(), self.fields.loc_ch.clone(), self.fields.loc_data.clone());
        if per {
            card = card.child(row("PostgreSQL", "loc-pg", pg, false, cx)).child(row("ClickHouse", "loc-ch", ch, false, cx)).child(row(t!("SENTIENT files"), "loc-data", data, true, cx));
        } else {
            card = card.child(row(t!("Folder"), "loc-one-dir", one, true, cx));
        }
        card
    }

    /// The three databases, each saying where it will run; one this way
    /// of running can't take is greyed with the reason in its (i)
    /// (placement::flavor_runs; mockup W2 to W4, L1 and L2).
    fn database(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let host = self.host();
        let detail = self.wsl_detail();
        let options: [(Flavor, &'static str); 3] = [
            (Flavor::Stock, t!("Simplest and smallest. For small sites and a few hundred devices.")),
            (Flavor::Timescaledb, t!("Compressed time-series inside PostgreSQL.")),
            (Flavor::Clickhouse, t!("Best for large telemetry volumes and long history.")),
        ];
        let cards = options.into_iter().map(|(flavor, desc)| {
            let logos = || ui::flavor_logos(flavor, 18.);
            match placement::flavor_runs(host, self.engine, flavor) {
                Ok(runs) => {
                    let recommended = flavor == Flavor::Clickhouse;
                    // Windows: why ClickHouse is in WSL 2 though the rest is not.
                    let help = (host.windows && self.engine == Engine::Native && flavor == Flavor::Clickhouse).then(|| {
                        ui::info(
                            "ch-wsl-help".into(),
                            t!("ClickHouse runs in Docker inside WSL 2\n\nThere is no ClickHouse for Windows. Setup puts it in Docker Engine inside WSL 2, under its own hidden account, so it starts at boot with nobody signed in.").into(),
                        )
                    });
                    let title = h_flex()
                        .gap_2()
                        .items_center()
                        .child(logos())
                        .child(div().font_weight(FontWeight::MEDIUM).child(flavor_label(flavor)))
                        .when(recommended, |r| r.child(ui::pill(&theme, Tone::Good, t!("Recommended"))))
                        .children(help)
                        .into_any_element();
                    choice_card(&theme, flavor.as_str(), title, desc, self.flavor == Some(flavor))
                        .child(runs_column(&theme, &runs, 1.))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.flavor = Some(flavor);
                            this.problem = None;
                            cx.notify();
                        }))
                }
                Err(why) => {
                    let text = match (why, flavor) {
                        (Why::NeedsWsl, Flavor::Clickhouse) => tf!("ClickHouse needs WSL 2\n\nClickHouse runs in Docker inside WSL 2, and WSL 2 cannot run on this computer. {detail}").trim_end().to_string(),
                        _ => why.text(&detail),
                    };
                    let title = h_flex()
                        .gap_2()
                        .items_center()
                        .child(h_flex().gap_2().items_center().opacity(ui::OFF_OPACITY).child(logos()).child(div().font_weight(FontWeight::MEDIUM).child(flavor_label(flavor))))
                        .child(ui::info(SharedString::from(format!("{}-why", flavor.as_str())), text.into()))
                        .into_any_element();
                    let desc = if why == Why::NeedsWsl { t!("Not available on this computer.") } else { desc };
                    ui::radio_card_off(&theme, flavor.as_str(), title, desc).relative().child(ui::layout_probe("panel")).child(runs_column(&theme, &["—"], ui::OFF_OPACITY))
                }
            }
        });
        column().children(cards).when(host.windows && self.engine == Engine::Native && self.flavor == Some(Flavor::Clickhouse) && host.wsl == Wsl::TurnOn, |col| {
            col.child(ui::notice(
                &theme,
                Tone::Neutral,
                t!("Windows restarts once\nWSL 2 is not on yet. Setup turns it on for ClickHouse and continues by itself after you sign in."),
            ))
        })
    }

    fn database_settings(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let f = &self.fields;
        let pg_title = match self.flavor {
            Some(Flavor::Timescaledb) => t!("PostgreSQL + TimescaleDB"),
            _ => "PostgreSQL",
        };
        let existing = self.existing.clone().map(|where_| {
            let keep = self.keep_existing;
            let delete = !keep && self.delete_existing;
            let flavor = self.flavor;
            let native = self.engine == Engine::Native;
            let kept_passwords = self.kept_passwords;
            let choice = |id: &'static str, label: &'static str, selected: bool| {
                let b = crate::ui::button(id).label(label);
                if selected { b.primary() } else { b.outline() }
            };
            ui::panel(&theme)
                .child(div().font_weight(FontWeight::SEMIBOLD).child(t!("Earlier database found")))
                .child(div().text_sm().child(where_))
                .child(
                    h_flex()
                        .gap(crate::ui::button_gap())
                        .child(crate::ui::probed(choice("existing-keep", t!("Keep using it"), keep).on_click(cx.listener(|this, _, _, cx| {
                            this.keep_existing = true;
                            this.delete_existing = false;
                            cx.notify();
                        }))))
                        .child(crate::ui::probed(choice("existing-fresh", t!("Start fresh"), !keep && !delete).on_click(cx.listener(|this, _, _, cx| {
                            this.keep_existing = false;
                            this.delete_existing = false;
                            cx.notify();
                        }))))
                        .when(native, |row| {
                            row.child(crate::ui::probed(choice("existing-delete", t!("Delete it"), delete).on_click(cx.listener(|this, _, _, cx| {
                                this.keep_existing = false;
                                this.delete_existing = true;
                                cx.notify();
                            }))))
                        }),
                )
                .child(div().text_sm().text_color(theme.muted_foreground).child(if keep {
                    tf!(
                        "Its users, devices, dashboards and settings stay. Before anything is changed, the install checks that its history table fits {}: a plain one is converted for TimescaleDB, and a database that cannot work in this mode stops the install with the reason.",
                        flavor.map(crate::installation::flavor_label).unwrap_or(t!("the chosen database"))
                    )
                } else if delete {
                    t!("A new, empty database. The earlier SENTIENT database (and its ClickHouse history) is deleted when the install starts. This cannot be undone.").to_string()
                } else {
                    t!("A new, empty database. The earlier one is not deleted: it is renamed (…_before_install_<date>) and listed under Kept on the Migration screen, where it can be removed.").to_string()
                }))
                .when(cfg!(windows) && native, |p| {
                    p.child(if kept_passwords {
                        ui::outcome(&theme, Tone::Good, t!("The earlier install's passwords were found and filled in below."))
                    } else {
                        ui::outcome(
                            &theme,
                            Tone::Warn,
                            t!("The PostgreSQL admin password below must be the earlier install's: the kept PostgreSQL still uses it."),
                        )
                    })
                })
        });
        column()
            .children(existing)
            .child(
                ui::panel(&theme)
                    .gap_4()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(pg_title))
                    .child(grid_row(vec![
                        Some(field(&theme, t!("Database name"), &f.pg_db)),
                        Some(field(&theme, t!("User"), &f.pg_user)),
                        Some(field(&theme, t!("Port"), &f.pg_port)),
                    ]))
                    .child(grid_row(vec![
                        Some(default_password_field(&theme, "pg-password-help", t!("Password"), &f.pg_password)),
                        Some(default_password_field(&theme, "pg-admin-password-help", t!("PostgreSQL admin password"), &f.pg_admin_password)),
                        None,
                    ])),
            )
            .when(self.flavor == Some(Flavor::Clickhouse), |col| {
                col.child(
                    ui::panel(&theme)
                        .gap_4()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(match placement::clickhouse_place(self.host(), self.engine) {
                            Some(place) => tf!("ClickHouse ({place})"),
                            None => "ClickHouse".to_string(),
                        }))
                        .child(grid_row(vec![
                            Some(field(&theme, t!("Database"), &f.ch_db)),
                            Some(field(&theme, t!("User"), &f.ch_user)),
                            Some(default_password_field(&theme, "ch-password-help", t!("Password"), &f.ch_password)),
                        ]))
                        .child(grid_row(vec![
                            Some(field(&theme, t!("HTTP port"), &f.ch_http_port)),
                            Some(field(&theme, t!("Native port"), &f.ch_native_port)),
                            None,
                        ])),
                )
            })
            .children(self.port_warning(cx))
    }

    /// Under the fields, when a chosen port is taken: which program holds
    /// it (an earlier SENTIENT install is removed on the System check page;
    /// another program is not touched) and the next free port to use.
    fn port_warning(&mut self, cx: &mut Context<Self>) -> Option<Div> {
        let Some(problem) = self.port_problem(cx) else { return None };
        self.lookup_port_owners(cx);
        let theme = cx.theme().clone();
        let busy = self.busy_fields(cx);
        let ours = self.sentient_dirs(cx);
        let box_ = || {
            v_flex()
                .gap_2()
                .p_3()
                .rounded(px(ui::PANEL_RADIUS))
                .bg(theme.warning)
                .text_color(theme.warning_foreground)
                .text_sm()
        };
        if busy.is_empty() {
            // Not a busy port (an invalid number): the message as it is.
            return Some(box_().child(h_flex().gap_2().child(Icon::new(IconName::TriangleAlert).small()).child(div().min_w_0().child(problem))));
        }
        let mut panel = box_();
        for (port, what, field) in busy {
            let owner = self.port_owners.get(&port).cloned().flatten();
            let text = match &owner {
                Some(o) if o.is_sentient(&ours) => tf!(
                    "Port {port} ({what}) is held by an earlier SENTIENT install ({}). Go back to System check to remove it, or use another port.",
                    o.describe()
                ),
                Some(o) => tf!("Port {port} ({what}) is used by another program ({}). It is not part of SENTIENT and is not touched.", o.describe()),
                None => tf!("Port {port} ({what}) is in use by another program."),
            };
            let next = sentient_installer_core::checks::next_free_port(port.saturating_add(1));
            panel = panel
                .child(h_flex().gap_2().items_start().child(Icon::new(IconName::TriangleAlert).small()).child(div().min_w_0().child(text)))
                .child(
                    h_flex()
                        .gap(crate::ui::button_gap())
                        .items_center()
                        .pl_6()
                        .when_some(next, |row, next| {
                            let field = field.clone();
                            row.child(div().child(tf!("{what} port for SENTIENT: {next} is free.")))
                                .child(crate::ui::probed(crate::ui::button(SharedString::from(format!("use-port-{port}"))).outline().label(tf!("Use {next}")).on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        field.update(cx, |state, cx| state.set_value(next.to_string(), window, cx));
                                        this.problem = None;
                                        cx.notify();
                                    },
                                ))))
                        })
                        .child(crate::ui::probed(crate::ui::button(SharedString::from(format!("recheck-port-{port}"))).ghost().label(t!("Check again")).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.port_owners.remove(&port);
                                cx.notify();
                            },
                        )))),
                );
        }
        Some(panel)
    }

    fn ports(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let f = &self.fields;
        let row = |label: &'static str, state: &Entity<InputState>| {
            h_flex()
                .gap_3()
                .items_center()
                .child(div().w(px(ui::CARD_LABEL_W + 30.)).text_sm().child(label))
                .child(div().w(px(140.)).child(Input::new(state)))
        };
        column()
            .child(
                ui::panel(&theme)
                    .child(row(t!("Web (HTTP)"), &f.http_port))
                    .child(row("MQTT", &f.mqtt_port))
                    .child(row(t!("MQTT over TLS"), &f.mqtts_port))
                    .child(row("CoAP", &f.coap_port))
                    .child(row(t!("CoAP over DTLS"), &f.coaps_port)),
            )
            .children(self.port_warning(cx))
    }

    fn administrator(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let f = &self.fields;
        column()
            .child(
                ui::panel(&theme)
                    .gap_4()
                    .child(field(&theme, t!("E-mail"), &f.admin_email))
                    .child(secret_field(&theme, t!("Password (at least 8 characters)"), &f.admin_password))
                    .child(secret_field(&theme, t!("Confirm password"), &f.admin_confirm)),
            )
            .child(
                Checkbox::new("demo")
                    .label(t!("Load demo devices and dashboards"))
                    .checked(self.load_demo)
                    .on_change(cx.listener(|this, value: &bool, _, cx| {
                        this.load_demo = *value;
                        cx.notify();
                    })),
            )
    }

    fn configuration(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let toggle = crate::ui::seg_track(&theme)
            .child(crate::ui::seg_item(&theme, "recommended", t!("Recommended"), !self.customize).on_click(cx.listener(|this, _, _, cx| {
                this.customize = false;
                cx.notify();
            })))
            .child(crate::ui::seg_item(&theme, "customize", t!("Customize"), self.customize).on_click(cx.listener(|this, _, window, cx| {
                this.refill_recommended(window, cx);
                this.customize = true;
                cx.notify();
            })));
        let body = if self.customize { self.customize_panel(cx) } else { self.recommended_panel(cx) };
        column().child(h_flex().child(toggle)).child(body)
    }

    fn recommended_panel(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let recommended = self.recommendation();
        let get = |key: &str| recommended.get(key).cloned().unwrap_or_else(|| t!("SENTIENT default").into());
        let sizing = self.sizing();
        let docker = self.engine == Engine::Docker;
        let clickhouse = self.flavor == Some(Flavor::Clickhouse);
        let (sized_for, why) = sized_for(sizing, docker);
        ui::panel(&theme)
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(sized_for)
                    .children(why.map(|w| ui::info("sized-for-why".into(), w.into()))),
            )
            .child(ui::fact(&theme, t!("PostgreSQL shared_buffers"), get("pg:shared_buffers")))
            .child(ui::fact(&theme, t!("PostgreSQL effective_cache_size"), get("pg:effective_cache_size")))
            .when_some(sizing.vm_mb.filter(|_| clickhouse), |p, vm| {
                p.child(ui::fact(&theme, t!("ClickHouse memory"), tf!("Up to {} GB, the WSL 2 VM's{}", gb(vm), if docker { t!(", shared with PostgreSQL") } else { "" })))
            })
            .child(ui::fact(&theme, t!("Audit log kept"), tf!("{} days", get("AUDIT_LOG_TTL_DAYS"))))
            .child(ui::fact(&theme, t!("Notifications kept"), tf!("{} days", get("NOTIFICATIONS_TTL_DAYS"))))
            .child(ui::fact(&theme, t!("Everything else"), t!("SENTIENT's tested defaults")))
            .child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(
                t!("Choose Customize to see and change every setting. All of them can also be changed later in the Platform Manager."),
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
                                .text_size(px(13.))
                                .font_family(theme.mono_font_family.clone())
                                .text_color(theme.muted_foreground)
                                .child(s.env.clone()),
                        )
                        .child(div().w_48().flex_shrink_0().child(Input::new(&self.settings[&s.env])))
                        .child(
                            crate::ui::button(SharedString::from(format!("help-{}", s.env)))
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
                            .text_size(px(13.))
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
            .child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(tf!(
                "{total} settings. Empty fields keep the value shown in grey. Values are written to the SENTIENT service."
            )))
            .child(ui::panel(&theme).gap_2().children(groups))
    }

    fn shortcuts(&mut self, cx: &mut Context<Self>) -> Div {
        let options: Vec<(&'static str, &'static str, bool)> = if self.manager_only {
            if cfg!(windows) {
                vec![
                    ("desktop-manager", t!("Desktop shortcut for the Platform Manager"), self.desktop_manager),
                    ("start-menu", t!("Start menu entry"), self.start_menu),
                ]
            } else {
                vec![("start-menu", t!("Application menu entry for the Platform Manager"), self.start_menu)]
            }
        } else if cfg!(windows) {
            let mut v = vec![
                ("autostart", t!("Start SENTIENT automatically with Windows"), self.autostart),
                ("desktop-sentient", t!("Desktop shortcut for SENTIENT"), self.desktop_sentient),
                ("desktop-manager", t!("Desktop shortcut for the Platform Manager"), self.desktop_manager),
                ("start-menu", t!("Start menu entries"), self.start_menu),
                ("kiosk", t!("Full-screen (kiosk) shortcut for operator screens"), self.kiosk),
                ("open-at-signin", t!("Open SENTIENT full screen when the computer starts (operator screens)"), self.open_at_signin),
            ];
            // Already installed: its shortcuts are there.
            if self.adding_sentient {
                v.retain(|(id, _, _)| *id != "desktop-manager");
            }
            v
        } else {
            vec![
                ("start-menu", if self.adding_sentient { t!("Application menu entry for SENTIENT") } else { t!("Application menu entries for SENTIENT and the Platform Manager") }, self.start_menu),
                ("kiosk", t!("Full-screen (kiosk) launcher for operator screens"), self.kiosk),
                ("open-at-signin", t!("Open SENTIENT full screen when the computer starts (operator screens)"), self.open_at_signin),
            ]
        };
        let theme = cx.theme().clone();
        // How long SENTIENT waits after Windows starts (crate::ui::start_delay_row).
        let mut start_delay = (cfg!(windows) && !self.manager_only && (self.autostart || self.open_at_signin))
            .then(|| div().pl_6().child(ui::start_delay_row(&theme, &self.fields.start_delay)));
        // Signing in by itself only matters for the screen that opens SENTIENT.
        let autologin = (!self.manager_only && self.open_at_signin).then(|| {
            v_flex()
                .pl_6()
                .gap_3()
                .child(
                    Checkbox::new("autologin")
                        .label(t!("Also sign in to this computer automatically after a restart"))
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
                            .child(div().w_64().child(field(&theme, t!("Operator account"), &self.fields.autologin_user)))
                            .when(cfg!(windows), |row| {
                                row.child(div().w_64().child(secret_field(
                                    &theme,
                                    t!("Account password"),
                                    &self.fields.autologin_password,
                                )))
                            }),
                    )
                    .child(ui::outcome(
                        &theme,
                        Tone::Warn,
                        t!("Anyone at this screen gets this account without typing a password. Use an operator account with only the rights the operator needs."),
                    ))
                })
        });
        column().child(ui::panel(&theme)
            .gap_4()
            .children(options.into_iter().flat_map(|(id, label, checked)| {
                let delay_here = id == "autostart";
                let checkbox = Checkbox::new(id)
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
                    .into_any_element();
                // The delay sits under "Start SENTIENT automatically with Windows".
                let delay = if delay_here { start_delay.take().map(IntoElement::into_any_element) } else { None };
                std::iter::once(checkbox).chain(delay)
            }))
            .children(autologin))
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
                shortcuts.push(if cfg!(windows) { t!("Start menu") } else { "application menu" });
            }
            rows.push((t!("Installs"), tf!("SENTIENT Platform Manager {}, without SENTIENT", crate::updates::manager_version()), 0));
            rows.push((t!("Program folder"), crate::self_install::manager_dir().display().to_string(), 0));
            rows.push((
                t!("Shortcuts"),
                if shortcuts.is_empty() { t!("None").into() } else { shortcuts.join(" and ") },
                super::SHORTCUTS,
            ));
        } else {
            if self.adding_sentient {
                rows.push((t!("Installs"), tf!("SENTIENT {} (the Platform Manager {} is already installed)", self.sentient_version, crate::updates::manager_version()), 0));
            }
            let host = self.host();
            rows.push((t!("Runs as"), placement::runs_as(host, self.engine, self.flavor), 1));
            if self.windows && self.engine == Engine::Docker {
                rows.push((t!("WSL distribution folder"), v(&f.install_dir), 2));
            } else if self.engine == Engine::Native {
                rows.push((t!("Program folder"), v(&f.install_dir), 2));
            }
            if self.windows && self.engine == Engine::Native {
                rows.push((t!("Database folder"), v(&f.data_dir), 2));
            }
            rows.extend(vec![
                (t!("Database"), self.flavor.map(flavor_label).unwrap_or(t!("Not chosen")).to_string(), 3),
                ("PostgreSQL", tf!("{} as {} on port {}", v(&f.pg_db), v(&f.pg_user), v(&f.pg_port)), 4),
            ]);
            if self.flavor == Some(Flavor::Clickhouse) {
                let place = placement::clickhouse_place(host, self.engine).map(|p| format!(" ({p})")).unwrap_or_default();
                rows.push((
                    "ClickHouse",
                    tf!("{} as {} on port {}{place}", v(&f.ch_db), v(&f.ch_user), v(&f.ch_http_port)),
                    4,
                ));
            }
            rows.extend(vec![
                (
                    t!("Ports"),
                    ports_summary(
                        [v(&f.http_port), v(&f.mqtt_port), v(&f.mqtts_port), v(&f.coap_port), v(&f.coaps_port)],
                        coap_local_only(self.windows, self.engine),
                    ),
                    5,
                ),
                (t!("Administrator"), v(&f.admin_email), 6),
                (
                    t!("Configuration"),
                    if self.customize {
                        tf!("Customized ({} settings set)", self.chosen_settings(cx).len())
                    } else {
                        t!("Recommended for this computer").into()
                    },
                    7,
                ),
                (
                    t!("Startup"),
                    if !self.windows || self.autostart { t!("Starts with the computer").into() } else { t!("Started manually").into() },
                    8,
                ),
            ]);
            if self.open_at_signin {
                let screen = if self.autologin {
                    tf!("Signs in as {} and opens SENTIENT full screen after a restart", v(&f.autologin_user))
                } else {
                    t!("Opens SENTIENT full screen when someone signs in").into()
                };
                rows.push((t!("Operator screen"), screen, 8));
            }
        }
        let coap_local = !self.manager_only && coap_local_only(self.windows, self.engine);
        column().child(ui::panel(&theme).gap_0().py(px(8.)).children(rows.into_iter().enumerate().map(|(i, (label, value, step))| {
            let empty = value.trim().is_empty();
            // Windows Docker: why CoAP is this computer's only (an (i), not a wall of text).
            let why = (coap_local && label == t!("Ports")).then(|| ui::info("summary-coap-why".into(), crate::i18n::t(COAP_LOCAL_HELP).into()));
            h_flex()
                .gap_4()
                .py(px(6.))
                .items_center()
                .when(i > 0, |r| r.border_t_1())
                .border_color(theme.border)
                .text_sm()
                .child(div().w(px(ui::CARD_LABEL_W + 30.)).flex_shrink_0().text_color(theme.muted_foreground).child(label))
                .child(if empty {
                    div().flex_1().min_w_0().text_color(theme.muted_foreground).child(t!("Not entered"))
                } else {
                    h_flex().flex_1().min_w_0().gap_1().items_center().child(div().min_w_0().font_weight(FontWeight::MEDIUM).child(value)).children(why)
                })
                .child(
                    crate::ui::button(SharedString::from(format!("change-{step}-{label}")))
                        .ghost()
                        .label(t!("Change"))
                        .on_click(cx.listener(move |this, _, _, cx| this.go(step, cx))),
                )
        })))
    }

    fn installing(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let job = self.install.read(cx);
        let state = job.state().clone();
        let restart = matches!(&state, JobState::Failed(m) if m.as_ref() == super::plan::RESTART_REQUIRED || m.as_ref() == crate::i18n::t(super::plan::RESTART_REQUIRED));
        // What went wrong and the fix (crate::diagnosis), when diagnosed;
        // the shared job view shows it (crate::manager::jobview).
        let undiagnosed = job.diagnosis_view().is_none();
        let mut view = crate::manager::jobview::JobView::new("install", if self.manager_only { t!("Installing the Platform Manager") } else { t!("Installing SENTIENT") }, &self.install)
            .cancel(crate::manager::jobview::Cancel::Cannot(t!("Setup cannot stop half-way: a failed step says what to fix, and Setup can then run again.")));
        if restart {
            // Not a failure: the step waits for the restart, and Restart
            // now is the footer's primary button.
            view = view.pill(t!("Restart"), Tone::Warn).paused(t!("Continues after Windows restarts and you sign in")).result(ui::notice(
                &theme,
                Tone::Neutral,
                t!("Restart required\nWindows must restart to finish turning on WSL 2. Setup continues by itself after you sign in again; your choices are kept."),
            ));
        } else if let JobState::Failed(m) = &state {
            if undiagnosed {
                view = view.result(install_failed(&theme, m.clone()));
            }
        }
        column().child(view.render(cx))
    }

    fn finish(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        if self.manager_only {
            return column()
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
                .child(div().text_sm().child(t!("To install SENTIENT later, open it and choose Download and install SENTIENT (or Install from local files).")))
                .child(h_flex().child(crate::ui::button("open-manager").primary().label(t!("Open Platform Manager")).on_click(
                    |_, _, _| {
                        let _ = crate::self_install::manager_command(&crate::self_install::manager_exe()).arg("--manager").spawn();
                    },
                )));
        }
        let url = format!("http://localhost:{}", self.value(&self.fields.http_port, cx));
        let email = self.value(&self.fields.admin_email, cx);
        let port = self.fields.http_port.read(cx).value().trim().parse::<u16>().unwrap_or(8080);
        let kiosk = self.kiosk;
        column()
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
                    .child(ui::card_fact(&theme, t!("Address"), url))
                    .child(ui::card_fact(&theme, t!("Sign in as"), if email.trim().is_empty() { t!("The e-mail entered under Administrator").to_string() } else { email }))
                    .child(ui::card_fact(
                        &theme,
                        t!("Managed by"),
                        if cfg!(windows) {
                            t!("SENTIENT Platform Manager (Start menu)")
                        } else {
                            t!("SENTIENT Platform Manager (application menu, or sentient-platform-manager)")
                        },
                    )),
            )
            .child(ui::notice(
                &theme,
                Tone::Neutral,
                t!("Next: activate your licence\nSign in to SENTIENT as the system administrator and activate it under Licence. Activation needs internet once."),
            ))
            .child(
                h_flex()
                    .gap(crate::ui::button_gap())
                    .child(crate::ui::probed(
                        crate::ui::button("open-sentient")
                            .primary()
                            .label(t!("Open SENTIENT"))
                            .on_click(move |_, _, cx| ui::open_sentient(port, kiosk, cx)),
                    ))
                    .child(crate::ui::probed(crate::ui::button("open-manager").outline().label(t!("Open Platform Manager")).on_click(
                        |_, _, _| {
                            let _ = crate::self_install::manager_command(&crate::self_install::manager_exe())
                                .arg("--manager")
                                .spawn();
                        },
                    ))),
            )
    }
}

#[cfg(test)]
mod check_again_tests {
    use super::{Fix, FixAction, Status, offers_check_again};

    #[test]
    fn the_recommendation_says_what_it_is_sized_for() {
        use crate::catalog::sizing;
        let (line, why) = super::sized_for(sizing(true, true, false, 16384, 8192, false), true);
        assert_eq!(line, "Sized for the WSL 2 VM: 8 GB of this computer's 16 GB.");
        assert!(why.unwrap().contains("half of the computer's memory"));
        let (_, why) = super::sized_for(sizing(true, true, false, 16384, 12288, true), true);
        assert!(why.unwrap().contains("memory= line of .wslconfig"));
        let (line, _) = super::sized_for(sizing(true, false, true, 16384, 8192, false), false);
        assert_eq!(line, "Sized for this computer: 16 GB, less 8 GB for ClickHouse in WSL 2.");
        assert_eq!(super::sized_for(sizing(false, false, false, 16384, 8192, false), false), ("Sized for this computer: 16 GB of memory.".into(), None));
    }

    #[test]
    fn windows_docker_lists_coap_as_this_computers_only() {
        use super::{Engine, coap_local_only, ports_summary};
        let p = || ["8080", "1883", "8883", "5683", "5684"].map(String::from);
        assert!(coap_local_only(true, Engine::Docker));
        assert!(!coap_local_only(true, Engine::Native) && !coap_local_only(false, Engine::Docker));
        assert_eq!(ports_summary(p(), true), "Web 8080 · MQTT 1883 / 8883 · CoAP 5683 / 5684 (this computer only)");
        assert_eq!(ports_summary(p(), false), "Web 8080 · MQTT 1883 / 8883 · CoAP 5683 / 5684");
        assert!(super::COAP_LOCAL_HELP.contains("UDP is not forwarded from WSL 2"));
    }

    #[test]
    fn will_be_installed_rows_have_no_check_again() {
        assert!(!offers_check_again("wsl", Status::Setup, "Will be installed", &[]));
        assert!(!offers_check_again("postgres", Status::Setup, "Will be installed", &[]));
        // A command to copy is not something a re-check reads.
        let copy = [Fix::Command { label: "Copy".into(), command: "x".into() }];
        assert!(!offers_check_again("docker-cli", Status::Setup, "Will be installed", &copy));
    }

    #[test]
    fn rows_a_recheck_can_change_keep_it() {
        let start = [Fix::Action { label: "Start Docker".into(), action: FixAction::StartDocker }];
        assert!(offers_check_again("docker-daemon", Status::Setup, "Will be started", &start));
        assert!(offers_check_again("docker-daemon", Status::Setup, "Will be installed", &start));
        assert!(offers_check_again("docker-daemon", Status::Setup, "Will be set up", &[]));
        assert!(offers_check_again("virtualization", Status::Fail, "Blocker", &[]));
        assert!(offers_check_again("ports", Status::Warn, "Attention", &[]));
        assert!(offers_check_again("x", Status::Unknown, "Unknown", &[]));
        assert!(offers_check_again("downloads", Status::Setup, "Will be installed", &[]));
    }
}
