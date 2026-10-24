// Copyright © 2016-2026 The SENTIENT Authors

//! Settings: every setting chosen at install time, changeable afterwards
//! (crate::settings applies them and goes back if SENTIENT does not start).

use std::collections::HashMap;

use gpui_kit::component::{
    ActiveTheme, Disableable as _, IconName, Sizable as _, WindowExt as _, h_flex, v_flex,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    progress::Progress,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::catalog::{self, Group};
use crate::installation::InstallationRecord;
use crate::jobs::{Job, JobState};
use crate::manager::migration::RecordChanged;
use crate::ui::{self, Tone};

pub struct SettingsPage {
    record: Option<InstallationRecord>,
    groups: Vec<Group>,
    /// The values in effect, by catalog name.
    applied: HashMap<String, String>,
    inputs: HashMap<String, Entity<InputState>>,
    search: Entity<InputState>,
    job: Entity<Job>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<RecordChanged> for SettingsPage {}

impl SettingsPage {
    pub fn new(record: Option<InstallationRecord>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let groups = catalog::groups();
        let applied = record.as_ref().map(crate::settings::current).unwrap_or_default();
        let mut subscriptions = Vec::new();
        let mut inputs = HashMap::new();
        for s in groups.iter().flat_map(|g| g.settings.iter()) {
            let value = applied.get(&s.env).cloned().unwrap_or_default();
            let placeholder = s.default.clone();
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(value));
            subscriptions.push(cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify()));
            inputs.insert(s.env.clone(), input);
        }
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search settings"));
        subscriptions.push(cx.subscribe(&search, |_, _, _: &InputEvent, cx| cx.notify()));
        let job = cx.new(|_| Job::new());
        subscriptions.push(cx.observe(&job, |page: &mut Self, job, cx| {
            if matches!(job.read(cx).state(), JobState::Done(_)) {
                page.record = InstallationRecord::load();
                page.applied = page.record.as_ref().map(crate::settings::current).unwrap_or_default();
                cx.emit(RecordChanged);
            }
            cx.notify();
        }));
        Self { record, groups, applied, inputs, search, job, _subscriptions: subscriptions }
    }

    /// The values in the fields, by catalog name (empty ones left out).
    fn values(&self, cx: &App) -> HashMap<String, String> {
        self.inputs
            .iter()
            .filter_map(|(name, input)| {
                let v = input.read(cx).value().trim().to_string();
                (!v.is_empty()).then(|| (name.clone(), v))
            })
            .collect()
    }

    fn changed(&self, cx: &App) -> Vec<String> {
        let values = self.values(cx);
        let mut names: Vec<String> = self
            .inputs
            .keys()
            .filter(|n| values.get(*n) != self.applied.get(*n).filter(|v| !v.is_empty()))
            .cloned()
            .collect();
        names.sort();
        names
    }

    fn revert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (name, input) in &self.inputs {
            let value = self.applied.get(name).cloned().unwrap_or_default();
            input.update(cx, |state, cx| state.set_value(value, window, cx));
        }
        cx.notify();
    }

    fn confirm_apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(record) = self.record.clone() else { return };
        let values = self.values(cx);
        let new_stack = crate::settings::with_values(&record.stack, &values);
        let Some(restarts) = crate::settings::restarts(&record.stack, &new_stack) else { return };
        let changed = self.changed(cx).len();
        let this = cx.entity().downgrade();
        let text = format!(
            "{changed} setting(s) change. {restarts}, so SENTIENT is unavailable for a minute or so. If it does not start with the new settings, the previous ones are put back."
        );
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            let record = record.clone();
            let values = values.clone();
            dialog.title("Apply the new settings?").description(text.clone()).confirm().ok_text("Apply").on_ok(move |_, _, cx| {
                let (record, values) = (record.clone(), values.clone());
                this.update(cx, |page, cx| {
                    page.job.update(cx, |job, cx| {
                        if crate::rootjob::needed(&record) {
                            // The root job reads the values from a file.
                            let path = std::env::temp_dir().join(format!("spm-settings-{}.json", std::process::id()));
                            let written = serde_json::to_string(&values)
                                .map_err(|e| e.to_string())
                                .and_then(|j| std::fs::write(&path, j).map_err(|e| e.to_string()));
                            let args = vec!["apply-settings".to_string(), path.display().to_string()];
                            job.start(
                                move |sink| {
                                    written?;
                                    let result = crate::rootjob::run(&args, &sink).map(|_| ());
                                    let _ = std::fs::remove_file(&path);
                                    result
                                },
                                cx,
                            )
                        } else {
                            job.start(move |sink| crate::settings::apply(record, &values, sink).map(|_| ()), cx)
                        }
                    });
                })
                .ok();
                true
            })
        });
    }
}

impl Render for SettingsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let page = ui::page(
            &theme,
            "Settings",
            "Every setting chosen at install time. Change a value and apply it; only what changed restarts.",
        );
        if self.record.is_none() {
            return page.child(ui::not_installed(&theme));
        }
        let _ = window;
        let job = self.job.read(cx);
        let busy = job.is_running();
        let step = job.step_with_time();
        let percent = job.percent();
        let outcome = match job.state() {
            JobState::Done(m) => Some((Tone::Good, m.clone())),
            JobState::Failed(m) => Some((Tone::Bad, m.clone())),
            _ => None,
        };
        let log: Vec<SharedString> = job.log().iter().rev().take(8).rev().cloned().collect();
        let changed = self.changed(cx);
        let problems = crate::settings::check(&self.values(cx));
        let needle = self.search.read(cx).value().to_lowercase();

        let groups = self.groups.iter().filter_map(|group| {
            let rows: Vec<_> = group
                .settings
                .iter()
                .filter(|s| {
                    needle.is_empty() || format!("{} {} {}", s.label, s.env, group.name).to_lowercase().contains(&needle)
                })
                .map(|s| {
                    let is_changed = changed.contains(&s.env);
                    h_flex()
                        .gap_3()
                        .items_center()
                        .py_1()
                        .child(
                            div()
                                .w_56()
                                .flex_shrink_0()
                                .text_sm()
                                .when(is_changed, |d| d.font_weight(FontWeight::MEDIUM).text_color(theme.primary))
                                .child(s.label.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .font_family(theme.mono_font_family.clone())
                                .text_color(theme.muted_foreground)
                                .child(s.env.clone()),
                        )
                        .child(div().w_48().flex_shrink_0().child(Input::new(&self.inputs[&s.env]).small().disabled(busy)))
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

        page.child(
            ui::panel(&theme)
                .child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(div().flex_1().child(Input::new(&self.search)))
                        .child(div().text_sm().text_color(theme.muted_foreground).child(if changed.is_empty() {
                            "No changes".to_string()
                        } else {
                            format!("{} changed", changed.len())
                        }))
                        .child(
                            Button::new("revert")
                                .ghost()
                                .label("Revert")
                                .disabled(busy || changed.is_empty())
                                .on_click(cx.listener(|this, _, window, cx| this.revert(window, cx))),
                        )
                        .child(
                            Button::new("apply")
                                .primary()
                                .label("Apply changes")
                                .disabled(busy || changed.is_empty() || !problems.is_empty())
                                .on_click(cx.listener(|this, _, window, cx| this.confirm_apply(window, cx))),
                        ),
                )
                .child(div().text_xs().text_color(theme.muted_foreground).child(
                    "Empty fields use the value shown in grey. Changed settings are highlighted.",
                ))
                .children(problems.into_iter().map(|p| div().text_sm().text_color(theme.danger).child(p))),
        )
        .when(busy || outcome.is_some(), |page| {
            page.child(
                ui::panel(&theme)
                    .when(busy, |p| {
                        p.child(div().text_sm().child(step.clone()))
                            .when_some(percent, |p, v| p.child(Progress::new("settings-progress").value(v)))
                    })
                    .children(outcome.map(|(tone, text)| ui::outcome(&theme, tone, text)))
                    .children(log.into_iter().map(|line| div().text_xs().text_color(theme.muted_foreground).child(line))),
            )
        })
        .child(ui::panel(&theme).gap_2().children(groups))
    }
}
