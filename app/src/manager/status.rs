// Copyright © 2016-2026 The SENTIENT Authors

//! Status: what is installed and whether each service is running. Refreshes
//! every few seconds while the screen is open.

use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme, h_flex,
    button::{Button, ButtonVariants as _},
};
use gpui_kit::*;

use crate::installation::{InstallationRecord, Snapshot, flavor_label};
use crate::ui::{self, Tone};

const REFRESH: Duration = Duration::from_secs(5);

pub struct StatusPage {
    record: Option<InstallationRecord>,
    snapshot: Option<Snapshot>,
    _poll: Option<Task<()>>,
}

impl StatusPage {
    pub fn new(record: Option<InstallationRecord>, cx: &mut Context<Self>) -> Self {
        let poll = record.as_ref().map(|record| {
            let platform = record.platform();
            cx.spawn(async move |this, cx| {
                loop {
                    let platform = platform.clone();
                    let snapshot = cx
                        .background_executor()
                        .spawn(async move { platform.snapshot() })
                        .await;
                    let alive = this
                        .update(cx, |page, cx| {
                            page.snapshot = Some(snapshot);
                            cx.notify();
                        })
                        .is_ok();
                    if !alive {
                        break;
                    }
                    cx.background_executor().timer(REFRESH).await;
                }
            })
        });
        Self { record, snapshot: None, _poll: poll }
    }
}

impl Render for StatusPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let page = ui::page(&theme, "Status", "Services, version, database type and ports.");
        let Some(record) = &self.record else {
            return page.child(ui::not_installed(&theme));
        };

        let (overall, tone) = match &self.snapshot {
            None => ("Checking…", Tone::Neutral),
            Some(s) if s.serving => ("SENTIENT is running", Tone::Good),
            Some(s) if s.services.iter().any(|svc| svc.running) => ("Starting or partly stopped", Tone::Warn),
            Some(_) => ("SENTIENT is stopped", Tone::Bad),
        };
        let url = record.url();
        let services = self.snapshot.as_ref().map(|s| s.services.clone()).unwrap_or_default();
        let st = &record.stack;

        page.child(
            ui::panel(&theme)
                .child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(ui::pill(&theme, tone, overall))
                        .child(div().flex_1())
                        .child(
                            Button::new("open")
                                .primary()
                                .label("Open SENTIENT")
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                        ),
                )
                .child(ui::fact(&theme, "SENTIENT version", record.sentient_version.clone()))
                .child(ui::fact(&theme, "Database", flavor_label(record.flavor())))
                .child(ui::fact(
                    &theme,
                    "Runs as",
                    match record.engine {
                        crate::installation::Engine::Native => "Windows services",
                        crate::installation::Engine::Docker => "Docker containers",
                    },
                ))
                .child(ui::fact(&theme, "Address", record.url()))
                .child(ui::fact(
                    &theme,
                    "Ports",
                    format!(
                        "Web {} · MQTT {} / {} · CoAP {} / {} · PostgreSQL {}",
                        st.http_port, st.mqtt_port, st.mqtts_port, st.coap_port, st.coaps_port, st.pg_port
                    ),
                ))
                .child(ui::fact(&theme, "Installed in", record.install_dir.display().to_string())),
        )
        .child(
            ui::panel(&theme)
                .child(div().font_weight(FontWeight::MEDIUM).child("Services"))
                .children(services.into_iter().map(|svc| {
                    h_flex()
                        .gap_3()
                        .text_sm()
                        .child(ui::pill(
                            &theme,
                            if svc.running { Tone::Good } else { Tone::Bad },
                            if svc.running { "Running" } else { "Stopped" },
                        ))
                        .child(div().w_48().child(svc.name))
                        .child(div().text_color(theme.muted_foreground).child(svc.detail))
                })),
        )
    }
}
