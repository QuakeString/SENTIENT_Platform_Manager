// Copyright © 2016-2026 The SENTIENT Authors

//! Platform settings › Startup: whether SENTIENT opens in the browser when
//! someone signs in (operator screens; kiosk::set_open_at_signin), whether
//! the computer signs in by itself after a restart (autologin), and on
//! Windows whether SENTIENT starts with Windows and after what delay.

use gpui_kit::component::{
    ActiveTheme, v_flex,
    input::InputState,
};
use gpui_kit::*;

use crate::jobs::{Job, JobState};
use crate::ui::{self, Tone};

pub struct StartupPanel {
    /// Opening at sign-in, as found: 0 off, 1 full screen, 2 window; None
    /// while looking.
    signin: Option<usize>,
    /// What the operator picked.
    signin_choice: usize,
    /// The account the computer signs in to by itself, as found: None
    /// while looking, Some(None) when it does not.
    autologin: Option<Option<String>>,
    autologin_on: bool,
    autologin_user: Entity<InputState>,
    autologin_password: Entity<InputState>,
    signin_job: Entity<Job>,
    _signin_observe: Subscription,
    /// Starting with Windows, as found: (with Windows, delay seconds); None
    /// while looking, or where there is nothing to choose (Linux).
    start: Option<(bool, u32)>,
    start_on: bool,
    start_delay: Entity<InputState>,
    start_job: Entity<Job>,
    _start_observe: Subscription,
    _tasks: Vec<Task<()>>,
    /// Enter on the last field: Platform settings' Apply (set by it).
    pub on_submit: Option<std::rc::Rc<dyn Fn(&mut Window, &mut App)>>,
    _enter: Vec<Subscription>,
}

fn signin_index(mode: Option<bool>) -> usize {
    match mode {
        None => 0,
        Some(true) => 1,
        Some(false) => 2,
    }
}

impl StartupPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let signin_job = cx.new(|_| Job::new());
        let _signin_observe = cx.observe(&signin_job, |page: &mut Self, job, cx| {
            if matches!(job.read(cx).state(), JobState::Done(_) | JobState::Failed(_)) {
                page.load_signin(cx);
            }
            cx.notify();
        });
        let start_job = cx.new(|_| Job::new());
        let _start_observe = cx.observe(&start_job, |page: &mut Self, job, cx| {
            if matches!(job.read(cx).state(), JobState::Done(_) | JobState::Failed(_)) {
                page.load_start(cx);
            }
            cx.notify();
        });
        let mut page = Self {
            signin: None,
            signin_choice: 0,
            autologin: None,
            autologin_on: false,
            autologin_user: cx.new(|cx| {
                InputState::new(window, cx).default_value(sentient_installer_core::autologin::this_user())
            }),
            autologin_password: cx.new(|cx| InputState::new(window, cx).masked(true)),
            signin_job,
            _signin_observe,
            start: None,
            start_on: true,
            start_delay: cx.new(|cx| InputState::new(window, cx)),
            start_job,
            _start_observe,
            _tasks: Vec::new(),
            on_submit: None,
            _enter: Vec::new(),
        };
        // Enter moves to the next field shown; on the last one it applies
        // (Platform settings' bar), or goes to the field that is not valid.
        let fields = [page.autologin_user.clone(), page.autologin_password.clone(), page.start_delay.clone()];
        page._enter = crate::ui::enter_moves_on(
            &fields,
            window,
            cx,
            |p: &Self, _| p.shown_fields(),
            |p: &mut Self, window, cx| {
                if p.invalid_count(cx) > 0 {
                    return Some(p.start_delay.clone());
                }
                if let Some(f) = p.on_submit.clone() {
                    window.defer(cx, move |window, cx| f(window, cx));
                }
                None
            },
        );
        page.load_signin(cx);
        page.load_start(cx);
        page
    }

    /// The fields shown, in order (Enter).
    fn shown_fields(&self) -> Vec<Entity<InputState>> {
        let mut out = Vec::new();
        if self.autologin.is_some() && self.autologin_on {
            out.push(self.autologin_user.clone());
            if cfg!(windows) {
                out.push(self.autologin_password.clone());
            }
        }
        if self.start.is_some() && self.start_on {
            out.push(self.start_delay.clone());
        }
        out
    }

    /// The changes not applied (Platform settings' bar).
    pub fn pending_count(&self, cx: &App) -> usize {
        let (signin, autologin) = self.pending(cx);
        signin as usize + autologin as usize + self.start_changed(cx) as usize
    }

    /// The start delay cannot be used (one field).
    pub fn invalid_count(&self, cx: &App) -> usize {
        self.delay_problem(cx).is_some() as usize
    }

    fn delay_problem(&self, cx: &App) -> Option<String> {
        if self.start.is_none() || !self.start_on {
            return None;
        }
        crate::installer::parse_start_delay(&self.start_delay.read(cx).value()).err()
    }

    /// Apply what changed (the bar's Apply).
    pub fn apply_pending(&mut self, cx: &mut Context<Self>) {
        let (signin, autologin) = self.pending(cx);
        if (signin || autologin) && !self.signin_job.read(cx).is_running() {
            self.apply_signin(cx);
        }
        if self.start_changed(cx) && self.invalid_count(cx) == 0 && !self.start_job.read(cx).is_running() {
            self.apply_start(cx);
        }
    }

    /// Back to what is set (the bar's Discard).
    pub fn discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(s) = self.signin {
            self.signin_choice = s;
        }
        if let Some(found) = self.autologin.clone() {
            self.autologin_on = found.is_some();
            let user = found.unwrap_or_else(sentient_installer_core::autologin::this_user);
            self.autologin_user.update(cx, |i, cx| i.set_value(user, window, cx));
            self.autologin_password.update(cx, |i, cx| i.set_value("", window, cx));
        }
        if let Some((on, delay)) = self.start {
            self.start_on = on;
            self.start_delay.update(cx, |i, cx| i.set_value(delay.to_string(), window, cx));
        }
        cx.notify();
    }

    fn load_start(&mut self, cx: &mut Context<Self>) {
        let task = cx.spawn(async move |this, cx| {
            let found = cx.background_executor().spawn(async move { crate::self_install::autostart() }).await;
            this.update_in(cx, |page, window, cx| {
                if let Some((on, delay)) = found {
                    page.start_on = on;
                    page.start_delay.update(cx, |input, cx| input.set_value(delay.to_string(), window, cx));
                }
                page.start = found;
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    /// The start choice differs from what is set.
    fn start_changed(&self, cx: &App) -> bool {
        let Some((on, delay)) = self.start else { return false };
        let typed = self.start_delay.read(cx).value().trim().to_string();
        on != self.start_on || (self.start_on && typed != delay.to_string())
    }

    fn apply_start(&mut self, cx: &mut Context<Self>) {
        let on = self.start_on;
        let typed = self.start_delay.read(cx).value().to_string();
        self.start_job.update(cx, |job, cx| {
            job.start(
                move |sink| {
                    let delay = if on { crate::installer::parse_start_delay(&typed)? } else { 0 };
                    crate::self_install::set_autostart(on, delay)?;
                    sink(sentient_installer_core::progress::Progress::Done {
                        message: match (on, delay) {
                            (false, _) => n!("SENTIENT no longer starts with Windows: use Start on the Status page.").to_string(),
                            (true, 0) => n!("SENTIENT starts together with Windows.").to_string(),
                            (true, d) => nf!("SENTIENT starts {d} seconds after Windows."),
                        },
                    });
                    Ok(())
                },
                cx,
            )
        });
    }

    fn load_signin(&mut self, cx: &mut Context<Self>) {
        let task = cx.spawn(async move |this, cx| {
            let (found, account) = cx
                .background_executor()
                .spawn(async move {
                    (sentient_installer_core::kiosk::open_at_signin(), sentient_installer_core::autologin::current())
                })
                .await;
            this.update_in(cx, |page, window, cx| {
                page.signin = Some(signin_index(found));
                page.signin_choice = signin_index(found);
                page.autologin_on = account.is_some();
                if let Some(user) = &account {
                    page.autologin_user.update(cx, |input, cx| input.set_value(user.clone(), window, cx));
                }
                page.autologin_password.update(cx, |input, cx| input.set_value("", window, cx));
                page.autologin = Some(account);
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    fn autologin_user(&self, cx: &App) -> String {
        self.autologin_user.read(cx).value().trim().to_string()
    }

    /// What Apply would change: (opening at sign-in, automatic sign-in).
    fn pending(&self, cx: &App) -> (bool, bool) {
        let signin = self.signin.is_some_and(|s| s != self.signin_choice);
        let autologin = match &self.autologin {
            None => false,
            Some(found) => {
                found.is_some() != self.autologin_on
                    || (self.autologin_on
                        && (found.as_deref() != Some(self.autologin_user(cx).as_str())
                            || !self.autologin_password.read(cx).value().is_empty()))
            }
        };
        (signin, autologin)
    }

    fn apply_signin(&mut self, cx: &mut Context<Self>) {
        let (signin, autologin) = self.pending(cx);
        let mode = match self.signin_choice {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        };
        let account = self
            .autologin_on
            .then(|| (self.autologin_user(cx), self.autologin_password.read(cx).value().to_string()));
        self.signin_job.update(cx, |job, cx| {
            job.start(
                move |sink| {
                    if signin {
                        crate::self_install::set_open_at_signin(&sink, mode)?;
                    }
                    if autologin {
                        let account = account.as_ref().map(|(u, p)| (u.as_str(), p.as_str()));
                        sentient_installer_core::autologin::set(&sink, account)?;
                    }
                    Ok(())
                },
                cx,
            )
        });
    }
}

impl Render for StartupPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let sjob = self.signin_job.read(cx);
        let sbusy = sjob.is_running();
        let soutcome = match sjob.state() {
            JobState::Done(m) => Some((Tone::Good, m.to_string())),
            JobState::Failed(m) => Some((Tone::Bad, m.to_string())),
            _ => None,
        };
        const SIGNIN: [&str; 3] = [n!("Off"), n!("Full screen (kiosk)"), n!("Browser window")];
        let (signin_changed, autologin_changed) = self.pending(cx);
        // Setting rows like every other tab: label left, the field, what is
        // set now.
        let mut rows = vec![ui::SettingRow {
            label: t!("Open SENTIENT at sign-in").into(),
            help: Some(t!("Open SENTIENT in the browser when someone signs in to this computer, for example a control-room screen after a power cut. It opens at once and shows SENTIENT as soon as it answers.").into()),
            desc: None,
            restart: false,
            changed: signin_changed,
            control: match self.signin {
                None => ui::row_text(t!("Checking…")).into_any_element(),
                Some(_) => {
                    let this = cx.entity().downgrade();
                    ui::chips(&theme, "signin", &[t!("Off"), t!("Kiosk"), t!("Window")], Some(self.signin_choice), std::rc::Rc::new(move |i, _, cx| {
                        this.update(cx, |p, cx| {
                            if !p.signin_job.read(cx).is_running() {
                                p.signin_choice = i;
                                cx.notify();
                            }
                        })
                        .ok();
                    }))
                    .into_any_element()
                }
            },
            wide: false,
            current: self.signin.map(|i| crate::i18n::t(SIGNIN[i]).to_string()),
            first: false,
        }];
        if let Some(found) = self.autologin.clone() {
            rows.push(ui::SettingRow {
                label: t!("Sign in automatically after a restart").into(),
                help: None,
                desc: None,
                restart: false,
                changed: autologin_changed,
                control: ui::toggle(&theme, "autologin", self.autologin_on, None, cx.listener(move |this, value: &bool, _, cx| {
                    if !sbusy {
                        this.autologin_on = *value;
                        cx.notify();
                    }
                }))
                .into_any_element(),
                wide: false,
                current: Some(found.clone().map(|u| tf!("As {u}")).unwrap_or_else(|| t!("Off").into())),
                first: false,
            });
            if self.autologin_on {
                rows.push(ui::SettingRow {
                    label: t!("Operator account").into(),
                    help: None,
                    desc: None,
                    restart: false,
                    changed: false,
                    control: ui::field_input(&theme, &self.autologin_user, None, None, false, window, cx).into_any_element(),
                    wide: false,
                    current: found.clone(),
                    first: false,
                });
                if cfg!(windows) {
                    rows.push(ui::SettingRow {
                        label: if found.is_some() { t!("Password (only to change it)").into() } else { t!("Account password").into() },
                        help: None,
                        desc: None,
                        restart: false,
                        changed: false,
                        control: ui::field_input(&theme, &self.autologin_password, None, None, true, window, cx).into_any_element(),
                        wide: false,
                        current: None,
                        first: false,
                    });
                }
            }
        }
        let mut extra: Vec<AnyElement> = Vec::new();
        if self.autologin.is_some() && self.autologin_on {
            extra.push(
                ui::outcome(
                    &theme,
                    Tone::Warn,
                    if cfg!(windows) {
                        t!("Anyone at this screen gets this account without typing a password. Use an operator account with only the rights the operator needs. Windows keeps the password in its secret store, readable only by administrators.")
                    } else {
                        t!("Anyone at this screen gets this account without typing a password. Use an operator account with only the rights the operator needs.")
                    },
                )
                .into_any_element(),
            );
        }
        extra.extend(soutcome.map(|(tone, text)| ui::outcome(&theme, tone, text).into_any_element()));
        let mut out = v_flex().child(ui::group(&theme, t!("Operator screen"), rows, extra, cx));
        if let Some((was_on, was_delay)) = self.start {
            let job = self.start_job.read(cx);
            let busy = job.is_running();
            let outcome = match job.state() {
                JobState::Done(m) => Some((Tone::Good, m.to_string())),
                JobState::Failed(m) => Some((Tone::Bad, m.to_string())),
                _ => None,
            };
            let typed = self.start_delay.read(cx).value().trim().to_string();
            let mut rows = vec![ui::SettingRow {
                label: t!("Start SENTIENT with Windows").into(),
                help: None,
                desc: None,
                restart: false,
                changed: was_on != self.start_on,
                control: ui::toggle(&theme, "start-with-windows", self.start_on, None, cx.listener(move |this, value: &bool, _, cx| {
                    if !busy {
                        this.start_on = *value;
                        cx.notify();
                    }
                }))
                .into_any_element(),
                wide: false,
                current: Some(if was_on { t!("On").into() } else { t!("Off").into() }),
                first: false,
            }];
            if self.start_on {
                rows.push(ui::SettingRow {
                    label: t!("Start delay").into(),
                    help: Some(crate::i18n::t(ui::START_DELAY_HELP).into()),
                    desc: None,
                    restart: false,
                    changed: typed != was_delay.to_string(),
                    control: ui::field_input(&theme, &self.start_delay, Some(ui::Slot::Unit("seconds".into())), self.delay_problem(cx), false, window, cx).into_any_element(),
                    wide: false,
                    current: Some(tf!("{was_delay} seconds")),
                    first: false,
                });
            }
            out = out.child(ui::group(&theme, t!("Start with Windows"), rows, outcome.map(|(tone, text)| ui::outcome(&theme, tone, text).into_any_element()).into_iter().collect(), cx));
        }
        out
    }
}
