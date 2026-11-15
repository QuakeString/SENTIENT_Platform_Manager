// Copyright © 2016-2026 The SENTIENT Authors

//! Taking an adopted Docker stack over (crate::adopt, distro::adopt): the
//! plan shown for confirmation before anything changes. The same dialog for
//! this computer and for a server; the plan is read by `load` (here, or by
//! the server's agent) and the takeover is started by `run`.
//!
//! It shows the compose file the containers came from (or that it is gone,
//! and then that the new file is rebuilt from the running containers, which
//! needs a tick), the `.env`/`env_file`s carried along, what is kept (the
//! old containers stay stopped under another name until the new ones answer;
//! the volumes are used as they are), the ports and the new file. Another
//! compose file on the server can be named in place of the labels' one.

use std::sync::Arc;

use gpui_kit::component::{
    ActiveTheme, Disableable as _, h_flex, v_flex,
    button::ButtonVariants as _,
    checkbox::Checkbox,
    input::{Input, InputState},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use sentient_installer_core::distro::adopt::TakeOverPlan;

use crate::ui::{self, Tone};

/// Reads the plan, with a compose file picked on the server (or the one the
/// containers' labels name). Runs off the window's thread.
pub type LoadPlan = Arc<dyn Fn(Option<String>) -> Result<TakeOverPlan, String> + Send + Sync>;
/// Starts the takeover with the compose file the plan was read with.
pub type RunTakeOver = std::rc::Rc<dyn Fn(Option<String>, &mut App)>;

enum Plan {
    Loading,
    Got(TakeOverPlan),
    Failed(String),
}

pub struct TakeOverDialog {
    load: LoadPlan,
    run: RunTakeOver,
    plan: Plan,
    /// The compose file picked on the server, when not the labels' one.
    compose: Option<String>,
    input: Entity<InputState>,
    /// The compose file is gone: rebuilding from the running containers was confirmed.
    rebuild_ok: bool,
    /// This computer (a file chooser, "this computer"), else a server.
    local: bool,
    sudo: Option<crate::remote::Instance>,
    _task: Option<Task<()>>,
}

/// Open the dialog and read the plan. `sudo`: the server whose sudo
/// password a failure may ask for (an agent update before the plan).
///
/// Deferred: called from a click, the window is in the middle of its own
/// update, and updating it again from there fails without a word.
pub fn open(local: bool, load: LoadPlan, run: RunTakeOver, sudo: Option<crate::remote::Instance>, cx: &mut App) {
    cx.defer(move |cx| open_now(local, load, run, sudo, cx));
}

fn open_now(local: bool, load: LoadPlan, run: RunTakeOver, sudo: Option<crate::remote::Instance>, cx: &mut App) {
    let Some(handle) = cx.active_window().or_else(|| cx.windows().into_iter().next()) else {
        crate::ui::show_error("The compose file could not be taken over", "The Platform Manager's window was not found.", cx);
        return;
    };
    let opened = handle.update(cx, |_, window, cx| {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("/path/to/docker-compose.yml"));
        let view = cx.new(|cx| {
            let mut d = TakeOverDialog { load, run, plan: Plan::Loading, compose: None, input, rebuild_ok: false, local, sudo, _task: None };
            d.reload(cx);
            d
        });
        crate::modal::open(crate::modal::Modal { title: "Take over the compose file".into(), body: view.into(), width: 620., on_close: None }, cx);
    });
    if let Err(e) = opened {
        crate::ui::show_error("The compose file could not be taken over", format!("The dialog could not open: {e}"), cx);
    }
}

impl TakeOverDialog {
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.plan = Plan::Loading;
        self.rebuild_ok = false;
        let load = self.load.clone();
        let compose = self.compose.clone();
        self._task = Some(cx.spawn(async move |this, cx| {
            let got = cx.background_executor().spawn(async move { load(compose) }).await;
            let _ = this.update(cx, |d, cx| {
                d.plan = match got {
                    Ok(p) => Plan::Got(p),
                    Err(e) => Plan::Failed(e),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// This computer: the desktop's file chooser, in the labels' folder.
    fn choose(&mut self, cx: &mut Context<Self>) {
        let start = match &self.plan {
            Plan::Got(p) => p.files.first().map(|(f, _)| std::path::PathBuf::from(f)),
            _ => None,
        };
        let rx = crate::chooser::open(
            cx,
            crate::chooser::Ask { title: "Choose the compose file", start: start.as_deref(), directory: false },
            PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Use this file".into()) },
        );
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            // The chosen file is listed in place of the labels' one.
            let _ = this.update(cx, |d, cx| {
                d.compose = Some(path.display().to_string());
                d.reload(cx);
            });
        })
        .detach();
    }

    fn use_file(&mut self, cx: &mut Context<Self>) {
        let path = self.input.read(cx).value().trim().to_string();
        if path.is_empty() {
            return;
        }
        self.compose = Some(path);
        self.reload(cx);
    }
}

/// A file and whether it is there.
fn file_row(theme: &gpui_kit::component::Theme, path: &str, found: bool) -> Div {
    h_flex()
        .relative()
        .child(ui::layout_probe("panel"))
        .gap_2()
        .items_center()
        .min_w_0()
        .child(ui::pill(theme, if found { Tone::Good } else { Tone::Warn }, if found { "Found" } else { "Missing" }).flex_shrink_0())
        .child(div().min_w_0().flex_1().overflow_hidden().text_ellipsis().whitespace_nowrap().font_family(theme.mono_font_family.clone()).text_size(px(12.5)).child(path.to_string()))
}

fn section(theme: &gpui_kit::component::Theme, title: &'static str, help: Option<(&'static str, &'static str)>) -> Div {
    h_flex()
        .gap_1()
        .items_center()
        .mt_1()
        .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(theme.foreground).child(title))
        .when_some(help, |r, (id, text)| r.child(ui::info(id.into(), text.into())))
}

impl Render for TakeOverDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let body = v_flex().gap_2().w_full();
        let plan = match &self.plan {
            Plan::Loading => {
                return body
                    .child(div().text_sm().text_color(theme.muted_foreground).child("Reading the containers and their compose file…"))
                    .child(ui::skeleton_line(&theme, "to-1", 360.))
                    .child(ui::skeleton_line(&theme, "to-2", 300.))
                    .child(ui::skeleton_line(&theme, "to-3", 420.))
                    .child(ui::skeleton_line(&theme, "to-4", 260.));
            }
            Plan::Failed(e) => {
                // The reason, Copy and Retry; when the agent update before it
                // needs sudo's password, the prompt, then the plan again.
                let copy = e.clone();
                let sudo = self.sudo.clone().filter(|_| crate::manager::remote_pages::needs_sudo_password(e));
                let this = cx.entity().downgrade();
                return body.child(ui::notice(&theme, Tone::Bad, format!("What taking it over would do could not be read.\n{e}"))).child(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(crate::ui::button("to-copy").outline().label("Copy").on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))))
                        .when_some(sudo, |r, inst| {
                            r.child(crate::ui::button("to-sudo").outline().label("Enter the sudo password…").on_click(move |_, _, cx| {
                                let this = this.clone();
                                crate::manager::remote_pages::ask_sudo_password(inst.clone(), std::rc::Rc::new(move |cx: &mut App| {
                                    this.update(cx, |d, cx| d.reload(cx)).ok();
                                }), cx);
                            }))
                        })
                        .child(crate::ui::button("to-retry").primary().label("Retry").on_click(cx.listener(|d, _, _, cx| d.reload(cx)))),
                );
            }
            Plan::Got(p) => p.clone(),
        };
        let any_found = plan.files.iter().any(|(_, f)| *f);
        let needs_rebuild = !plan.files.is_empty() && !any_found || plan.files.is_empty();
        let blocked = !plan.problems.is_empty();
        let can_go = !blocked && (!needs_rebuild || self.rebuild_ok);

        let mut b = body.child(
            h_flex()
                .gap_1()
                .items_center()
                .child(div().flex_1().min_w_0().text_sm().child("SENTIENT stops for about a minute and starts again from the Platform Manager's own compose file, on the same data."))
                .child(ui::info(
                    "to-how".into(),
                    format!(
                        "{}The old containers are stopped and kept as <name>-before-takeover until the new ones answer, then removed. If the new ones do not answer within 3 minutes, they are removed and the old ones get their names back and start again, exactly as they were.",
                        if plan.project.is_empty() { String::new() } else { format!("Compose project: {}.\n\n", plan.project) }
                    )
                    .into(),
                )),
        );

        // Why it cannot be taken over, first (Take over stays off).
        b = b.children(plan.problems.iter().map(|p| ui::outcome(&theme, Tone::Bad, format!("{p} Nothing will be changed."))));
        // Compose file: from the labels, or picked on the server.
        b = b.child(section(
            &theme,
            "Compose file",
            Some(("to-compose-help", "The file the containers were started from, as their labels name it. If it moved, name it here: the plan is read again with it.")),
        ));
        b = b.children(plan.files.iter().map(|(f, found)| file_row(&theme, f, *found)));
        // Another compose file: typed on a server, chosen on this computer.
        let place = if self.local { "this computer" } else { "this server" };
        b = b
            .child(div().text_size(px(12.5)).text_color(theme.muted_foreground).mt_1().child(format!("Another compose file on {place}")))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().min_w_0().child(Input::new(&self.input)))
                    .when(self.local, |r| r.child(crate::ui::button("to-choose").outline().label("Choose…").on_click(cx.listener(|d, _, _, cx| d.choose(cx)))))
                    .child(crate::ui::button("to-use-file").outline().label("Use this file").on_click(cx.listener(|d, _, _, cx| d.use_file(cx))))
                    .when(self.compose.is_some(), |r| {
                        r.child(crate::ui::button("to-labels").ghost().label("Back to the labels' file").on_click(cx.listener(|d, _, _, cx| {
                            d.compose = None;
                            d.reload(cx);
                        })))
                    }),
            );
        if needs_rebuild {
            b = b
                .child(div().text_sm().text_color(theme.warning_foreground).child(format!(
                    "The compose file is not on {place}. The new one is written from the running containers' own settings (image, ports, volumes, environment)."
                )))
                .child(
                    Checkbox::new("to-rebuild")
                        .label("Write the new compose file from the running containers")
                        .checked(self.rebuild_ok)
                        .disabled(blocked)
                        .on_change(cx.listener(|d, v: &bool, _, cx| {
                            d.rebuild_ok = *v;
                            cx.notify();
                        })),
                );
        }

        // Environment files carried along.
        b = b.child(section(
            &theme,
            "Environment files",
            Some(("to-env-help", "The .env beside the compose file and every env_file it names. Their values are already in the containers' settings and are carried over; a copy of each is kept beside the new file.")),
        ));
        b = if plan.env_files.is_empty() {
            b.child(div().text_sm().text_color(theme.muted_foreground).child("None"))
        } else {
            b.children(plan.env_files.iter().map(|(f, found)| file_row(&theme, f, *found)))
        };

        // What is kept and what changes.
        b = b.child(section(&theme, "What happens", None));
        b = b
            .child(ui::fact(&theme, "Containers", plan.containers.join(", ")))
            .child(ui::fact(&theme, "Old containers", "Kept, stopped, until the new ones answer"))
            .child(ui::fact(&theme, "Data volumes", format!("{} (used as they are)", plan.volumes.join(", "))))
            .child(ui::fact(&theme, "Ports", plan.ports.join(", ")))
            .when(!plan.extra_env.is_empty(), |b| b.child(ui::fact(&theme, "Extra settings", plan.extra_env.join(", "))))
            .when_some(plan.loopback_port, |b, p| b.child(ui::fact(&theme, "PostgreSQL", format!("port {p} on 127.0.0.1 only (it was not published)"))))
            .child(ui::fact(&theme, "New compose file", plan.new_file.clone()))
            .child(ui::fact(&theme, "Image", plan.image.clone()));

        // The plan scrolls in a short window; the buttons stay.
        let max_h = (window.viewport_size().height - px(220.)).max(px(160.));
        v_flex().relative().child(ui::layout_probe("panel")).gap_2().w_full().child(div().id("to-body").max_h(max_h).overflow_y_scroll().pr_1().child(b)).child(
            h_flex()
                .relative()
                .child(ui::layout_probe("button"))
                .gap_2()
                .justify_end()
                .mt_1()
                .child(crate::ui::button("to-cancel").outline().label("Cancel").on_click(|_, _, cx| crate::modal::close(cx)))
                .child(crate::ui::button("to-go").primary().label("Take over").disabled(!can_go).on_click(cx.listener(|d, _, _, cx| {
                    let compose = d.compose.clone();
                    let run = d.run.clone();
                    crate::modal::close(cx);
                    run(compose, cx);
                }))),
        )
    }
}
