// Copyright © 2016-2026 The SENTIENT Authors

//! A long action's progress on its own page (Migration, Update, Backup,
//! Restore, Repair, Takeover): it replaces the page's form while the job
//! runs, and stays with its result (done or failed) until "Back to …".
//! The stages ticked off, the current step with its time and a progress
//! bar, the result, and the job's log lines below. The bell's line is an
//! extra; the page is where the job is followed.

use gpui_kit::component::{ActiveTheme as _, h_flex, progress::Progress, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::jobs::{Job, JobState};
use crate::ui::{self, StepMark, Tone};

/// One stage of an action, ticked off when one of the job's steps names it.
pub struct Stage {
    pub label: &'static str,
    /// Words of the job's step names that mean this stage.
    pub when: &'static [&'static str],
}

/// The migration's four stages.
pub const MIGRATION: &[Stage] = &[
    Stage { label: "A full backup, history included", when: &["Backing up"] },
    Stage { label: "SENTIENT stops", when: &["Stopping", "Preparing ClickHouse", "Preparing the database"] },
    Stage { label: "SENTIENT starts on the new type", when: &["Starting SENTIENT on"] },
    Stage { label: "The history is copied month by month", when: &["Copying the history"] },
];

/// The stages as marked for the job now: with `stages`, the furthest one a
/// step reached; without, the job's own steps.
pub fn marks(stages: &[Stage], steps: &[SharedString], state: &JobState) -> Vec<(String, StepMark)> {
    let done = matches!(state, JobState::Done(_));
    let failed = matches!(state, JobState::Failed(_));
    if stages.is_empty() {
        let n = steps.len();
        return steps
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let last = i + 1 == n;
                let mark = if !last || done { StepMark::Done } else if failed { StepMark::Failed } else { StepMark::Running };
                (s.to_string(), mark)
            })
            .collect();
    }
    let reached = steps.iter().filter_map(|s| stages.iter().rposition(|st| st.when.iter().any(|w| s.contains(w)))).max();
    stages
        .iter()
        .enumerate()
        .map(|(i, st)| {
            let mark = match reached {
                _ if done => StepMark::Done,
                Some(r) if i < r => StepMark::Done,
                Some(r) if i == r => {
                    if failed {
                        StepMark::Failed
                    } else {
                        StepMark::Running
                    }
                }
                None if i == 0 && failed => StepMark::Failed,
                None if i == 0 => StepMark::Running,
                _ => StepMark::Todo,
            };
            (st.label.to_string(), mark)
        })
        .collect()
}

/// The view. `title` names the action ("Changing the database to …");
/// `back` is "Back to <page>" (shown once the job ended); `retry` when it
/// failed and can run again.
pub fn view(
    id: &'static str,
    title: String,
    stages: &[Stage],
    job: &Job,
    back: Option<AnyElement>,
    retry: Option<AnyElement>,
    cx: &App,
) -> Div {
    let theme = cx.theme();
    let state = job.state();
    let (pill, tone) = match state {
        JobState::Running => ("Running", Tone::Neutral),
        JobState::Done(_) => ("Done", Tone::Good),
        JobState::Failed(_) => ("Failed", Tone::Bad),
        JobState::Idle => ("", Tone::Neutral),
    };
    let marked = marks(stages, job.steps(), state);
    let running = matches!(state, JobState::Running);
    let lines: Vec<SharedString> = job.log().iter().rev().take(400).rev().cloned().collect();
    v_flex()
        .gap_4()
        .child(
            ui::panel(theme)
                .child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(div().flex_1().min_w_0().font_weight(FontWeight::MEDIUM).child(title))
                        .when(!pill.is_empty(), |r| r.child(ui::pill(theme, tone, pill))),
                )
                .child(ui::steps(theme, &marked))
                .when(running, |c| {
                    c.child(div().text_sm().text_color(theme.muted_foreground).child(job.step_with_time()))
                        .when_some(job.percent(), |c, v| c.child(Progress::new(SharedString::from(format!("{id}-progress"))).value(v)))
                })
                .children(match state {
                    JobState::Done(m) => Some(ui::outcome(theme, Tone::Good, m.to_string())),
                    JobState::Failed(m) => {
                        let (head, detail) = crate::remote::split_failure(m);
                        Some(ui::outcome(theme, Tone::Bad, match detail {
                            Some(d) if !d.trim().is_empty() => format!("{head}\n{}", d.trim()),
                            _ => head,
                        }))
                    }
                    _ => None,
                })
                .when(!running && (back.is_some() || retry.is_some()), |c| c.child(h_flex().gap_2().children(retry).children(back))),
        )
        .child(
            ui::panel(theme)
                .child(div().font_weight(FontWeight::MEDIUM).child("Log"))
                .child(
                    div()
                        .id(SharedString::from(format!("{id}-log")))
                        .max_h(px(320.))
                        .overflow_y_scroll()
                        .p_2()
                        .rounded(theme.radius)
                        .bg(theme.muted)
                        .font_family(theme.mono_font_family.clone())
                        .text_size(px(12.5))
                        .children(if lines.is_empty() {
                            vec![div().text_color(theme.muted_foreground).child(if running { "Waiting for the first lines…" } else { "No log lines." })]
                        } else {
                            lines.into_iter().map(|l| div().child(l)).collect()
                        }),
                ),
        )
}

/// "Back to <page>".
pub fn back_button(id: &'static str, page: &str, on: impl Fn(&mut Window, &mut App) + 'static) -> AnyElement {
    crate::ui::button(id).outline().label(format!("Back to {page}")).on_click(move |_, window, cx| on(window, cx)).into_any_element()
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's own `test` attribute would shadow the standard one.
    use super::{JobState, MIGRATION, StepMark, marks};
    use gpui_kit::SharedString;

    #[test]
    fn migration_stages_follow_the_steps() {
        let s = |v: &[&str]| v.iter().map(|x| SharedString::from(x.to_string())).collect::<Vec<_>>();
        let m = marks(MIGRATION, &s(&["Moving the database", "Backing up everything first", "Preparing ClickHouse"]), &JobState::Running);
        assert_eq!(m.iter().map(|(_, k)| *k).collect::<Vec<_>>(), vec![StepMark::Done, StepMark::Running, StepMark::Todo, StepMark::Todo]);
        let m = marks(MIGRATION, &s(&["Starting SENTIENT on PostgreSQL + ClickHouse", "Copying the history from before the switch"]), &JobState::Failed("x".into()));
        assert_eq!(m[3].1, StepMark::Failed);
        let m = marks(MIGRATION, &s(&["Copying the history from before the switch"]), &JobState::Done("ok".into()));
        assert!(m.iter().all(|(_, k)| *k == StepMark::Done));
        // Without stages: the job's own steps, the last one current.
        let m = marks(&[], &s(&["Getting the new SENTIENT image", "Applying the database changes"]), &JobState::Running);
        assert_eq!(m.iter().map(|(_, k)| *k).collect::<Vec<_>>(), vec![StepMark::Done, StepMark::Running]);
    }
}
