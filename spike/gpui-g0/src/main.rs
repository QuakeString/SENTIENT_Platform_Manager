// Copyright © 2016-2026 The SENTIENT Authors
//
// G0 spike: one window with the controls the installer needs (text input,
// dropdown, radio group, checkbox, progress driven from a background task,
// folder picker). The question it answers is not "does it compile" but "does
// it render and respond acceptably" on the win11 VM, over Remote Desktop and
// on the physical Windows 10 box.

use std::time::Duration;

use gpui_kit::assets::Assets;
use gpui_kit::component::{
    ActiveTheme, h_flex, v_flex,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    form::{Field, Form},
    input::{Input, InputState},
    progress::Progress,
    radio::RadioGroup,
    select::{Select, SelectState},
};
use gpui_kit::*;

struct Spike {
    folder: Entity<InputState>,
    flavor: Entity<SelectState<Vec<&'static str>>>,
    mode: Option<usize>,
    autostart: bool,
    progress: f32,
    status: SharedString,
    _ticker: Option<Task<()>>,
}

impl Spike {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let folder = cx.new(|cx| {
            InputState::new(window, cx).default_value(r"C:\Program Files\SENTIENT")
        });
        let flavor = cx.new(|cx| {
            SelectState::new(
                vec!["PostgreSQL only", "PostgreSQL + TimescaleDB", "PostgreSQL + ClickHouse"],
                None,
                window,
                cx,
            )
        });
        Self {
            folder,
            flavor,
            mode: Some(0),
            autostart: true,
            progress: 0.0,
            status: "Idle".into(),
            _ticker: None,
        }
    }

    fn browse(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Install here".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(p) = paths.first() {
                    let text = p.display().to_string();
                    this.update(cx, |this, cx| {
                        this.status = format!("Picked {text}").into();
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        self.progress = 0.0;
        self.status = "Running…".into();
        self._ticker = Some(cx.spawn(async move |this, cx| {
            for step in 1..=100 {
                cx.background_executor().timer(Duration::from_millis(40)).await;
                let gone = this
                    .update(cx, |this, cx| {
                        this.progress = step as f32;
                        if step == 100 {
                            this.status = "Done".into();
                        }
                        cx.notify();
                    })
                    .is_err();
                if gone {
                    break;
                }
            }
        }));
    }
}

impl Render for Spike {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let flavor = self
            .flavor
            .read(cx)
            .selected_value()
            .map(|v| v.to_string())
            .unwrap_or_else(|| "none".into());
        v_flex()
            .size_full()
            .p_6()
            .gap_4()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(div().text_xl().child("SENTIENT Setup — GPUI spike"))
            .child(
                Form::new()
                    .child(
                        Field::new().label("Install folder").child(
                            h_flex()
                                .gap_2()
                                .child(div().flex_1().child(Input::new(&self.folder)))
                                .child(
                                    Button::new("browse")
                                        .label("Browse")
                                        .on_click(cx.listener(|this, _, _, cx| this.browse(cx))),
                                ),
                        ),
                    )
                    .child(
                        Field::new()
                            .label("Database")
                            .child(Select::new(&self.flavor).placeholder("Choose a database")),
                    )
                    .child(
                        Field::new().label("Configuration").child(
                            RadioGroup::new("mode")
                                .children(["Recommended", "Customize"])
                                .selected_index(self.mode)
                                .on_change(cx.listener(|this, value, _, cx| {
                                    this.mode = Some(*value);
                                    cx.notify();
                                })),
                        ),
                    )
                    .child(
                        Field::new().label_indent(false).child(
                            Checkbox::new("autostart")
                                .label("Start SENTIENT automatically with Windows")
                                .checked(self.autostart)
                                .on_change(cx.listener(|this, value, _, cx| {
                                    this.autostart = *value;
                                    cx.notify();
                                })),
                        ),
                    ),
            )
            .child(Progress::new("progress").value(self.progress))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("run")
                            .primary()
                            .label("Install")
                            .on_click(cx.listener(|this, _, _, cx| this.run(cx))),
                    )
                    .child(div().child(self.status.clone())),
            )
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(format!(
                "database: {flavor} · scale factor: {:.2}",
                window.scale_factor()
            )))
    }
}

fn main() {
    gpui_kit::application().with_assets(Assets).run(|cx| {
        gpui_kit::init(cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(760.), px(560.)), cx)),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| Spike::new(window, cx)))
            .expect("open window");
    });
}
