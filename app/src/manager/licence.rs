// Copyright © 2016-2026 The SENTIENT Authors

//! Licence: SENTIENT's licence, for this computer and for a remote
//! instance (crate::licence calls SENTIENT's own licence functions). The
//! page asks for SENTIENT's system administrator once per visit; nothing
//! about the licence shows before that. Signed in: the licence card in the
//! centre with Details, Limits and Manage under it; with no licence, a key to
//! activate, a key file to import, a demo to request or one to buy.
//! Activate, Deactivate and Force-delete ask for the password again in their
//! own dialog.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable as _, Icon, button::ButtonVariants as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::licence::{Reading, Target};
use crate::manager::licence_card;
use crate::ui::{self, Tone};

/// The page asks the Manager to open Platform settings ("Forgot it?").
pub struct GoSettings;

fn icon(name: &str) -> Icon {
    Icon::empty().path(SharedString::from(format!("icons/licence/{name}.svg")))
}

#[derive(Clone)]
enum View {
    /// Finding out whether SENTIENT runs there.
    Checking,
    NotInstalled,
    Stopped,
    AgentOld,
    Failed(String),
    SignIn,
    /// Signed in, reading the licence.
    Loading,
    Ready(Box<Reading>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    Details,
    Limits,
}

pub struct LicencePage {
    target: Target,
    /// "Fabrikam IIoT", or "this computer".
    name: String,
    view: View,
    open: Option<Panel>,
    menu: bool,
    email: Entity<InputState>,
    password: Entity<InputState>,
    key: Entity<InputState>,
    signing_in: bool,
    /// Remember me: keep the password too (on by default).
    remember: bool,
    sign_in_error: Option<String>,
    /// A read is under way (the Refresh button turns).
    reading: bool,
    /// The last action's result, above the card.
    note: Option<(Tone, String)>,
    /// Start SENTIENT, or update the server's agent.
    job: Entity<crate::jobs::Job>,
    /// The page's content width, measured.
    width: Rc<Cell<f32>>,
    /// A dialog to open on the next frame (screens: SPM_DEBUG_LICENCE_OPEN).
    pending: Option<Act>,
    _watch: Option<Subscription>,
    _tasks: Vec<Task<()>>,
    /// Enter in the email or password field signs in.
    _enter: Vec<Subscription>,
}

impl EventEmitter<GoSettings> for LicencePage {}

impl LicencePage {
    pub fn new(target: Target, name: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let email = cx.new(|cx| InputState::new(window, cx).placeholder("admin@example.com"));
        let password = cx.new(|cx| InputState::new(window, cx).masked(true));
        let key = cx.new(|cx| InputState::new(window, cx).placeholder("Enter your licence key"));
        // The last administrator signed in here, as a start.
        if let Some(s) = crate::licence::email_for(&target.key()) {
            email.update(cx, |i, cx| i.set_value(s, window, cx));
        }
        let mut page = Self {
            target,
            name,
            view: View::Checking,
            open: None,
            menu: false,
            email,
            password,
            key,
            signing_in: false,
            remember: true,
            sign_in_error: None,
            reading: false,
            note: None,
            job: cx.new(|_| crate::jobs::Job::new()),
            width: Rc::new(Cell::new(0.)),
            pending: None,
            _watch: None,
            _tasks: Vec::new(),
            _enter: Vec::new(),
        };
        for input in [page.email.clone(), page.password.clone()] {
            page._enter.push(cx.subscribe_in(&input, window, |p: &mut Self, _, e: &InputEvent, window, cx| {
                if matches!(e, InputEvent::PressEnter { .. }) && matches!(p.view, View::SignIn) && !p.signing_in {
                    p.sign_in(window, cx);
                }
            }));
        }
        #[cfg(debug_assertions)]
        if std::env::var_os("SPM_DEBUG_LICENCE").is_some() && std::env::var_os("SPM_DEBUG_LICENCE_SIGNED_IN").is_some() {
            crate::licence::set_session(&page.target.key(), "admin@sentient.local", "debug");
        }
        page.check(cx);
        page
    }

    /// Whether SENTIENT runs there; then the licence when signed in.
    fn check(&mut self, cx: &mut Context<Self>) {
        self.reading = true;
        let target = self.target.clone();
        let task = cx.spawn(async move |this, cx| {
            let got = cx.background_executor().spawn(async move { target.serving() }).await;
            this.update(cx, |p, cx| {
                p.reading = false;
                match got {
                    Ok((false, _)) => p.view = View::NotInstalled,
                    Ok((true, false)) => p.view = View::Stopped,
                    Ok((true, true)) => match crate::licence::session(&p.target.key()) {
                        Some(s) => p.load(s.token, cx),
                        // Signed in before: sign in again from what is kept
                        // (refresh token, then Remember me), without asking.
                        None if crate::licence::stored(&p.target.key()).is_some() => p.silent(cx),
                        None => p.view = View::SignIn,
                    },
                    Err(e) => p.view = View::Failed(e),
                }
                p.debug_open(cx);
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
        cx.notify();
    }

    /// Sign in from what is kept; the form (email filled in, the reason
    /// shown) only when that fails.
    fn silent(&mut self, cx: &mut Context<Self>) {
        self.view = View::Loading;
        self.reading = true;
        let target = self.target.clone();
        let task = cx.spawn(async move |this, cx| {
            let got = cx.background_executor().spawn(async move { target.silent_sign_in() }).await;
            this.update(cx, |p, cx| {
                p.reading = false;
                match got {
                    Ok(s) => p.load(s.token, cx),
                    Err(e) if e == crate::remote::AGENT_OLD => p.view = View::AgentOld,
                    Err(e) => {
                        p.sign_in_error = (!e.is_empty()).then_some(e);
                        p.view = View::SignIn;
                    }
                }
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
        cx.notify();
    }

    /// Read the licence with `token`.
    fn load(&mut self, token: String, cx: &mut Context<Self>) {
        if !matches!(self.view, View::Ready(_)) {
            self.view = View::Loading;
        }
        self.reading = true;
        let target = self.target.clone();
        let key = target.key();
        let task = cx.spawn(async move |this, cx| {
            let got = cx.background_executor().spawn(async move { target.read(&token) }).await;
            this.update(cx, |p, cx| {
                p.reading = false;
                match got {
                    Ok(r) => p.view = View::Ready(Box::new(r)),
                    Err(e) if e == crate::licence::SESSION_ENDED => {
                        crate::licence::end_session(&key);
                        // The token ran out: renew it from what is kept.
                        if crate::licence::stored(&key).is_some() {
                            p.silent(cx);
                        } else {
                            p.sign_in_error = Some(e);
                            p.view = View::SignIn;
                        }
                    }
                    Err(e) if e == crate::remote::AGENT_OLD => p.view = View::AgentOld,
                    Err(e) => p.view = View::Failed(e),
                }
                p.debug_open(cx);
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
        cx.notify();
    }

    /// Read again (the title's Refresh, and after an action).
    fn reload(&mut self, cx: &mut Context<Self>) {
        match crate::licence::session(&self.target.key()) {
            Some(s) if matches!(self.view, View::Ready(_) | View::Loading) => self.load(s.token, cx),
            _ => self.check(cx),
        }
    }

    fn sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let email = self.email.read(cx).value().trim().to_string();
        let password = self.password.read(cx).value().to_string();
        if email.is_empty() || password.is_empty() {
            self.sign_in_error = Some("Enter the email and the password.".into());
            cx.notify();
            return;
        }
        self.signing_in = true;
        self.sign_in_error = None;
        self.password.update(cx, |i, cx| i.set_value("", window, cx));
        let target = self.target.clone();
        let remember = self.remember;
        let task = cx.spawn(async move |this, cx| {
            let (e2, pw) = (email.clone(), password.clone());
            let got = cx.background_executor().spawn(async move { target.sign_in(&e2, &pw) }).await;
            this.update(cx, |p, cx| {
                p.signing_in = false;
                match got {
                    Ok((token, refresh)) => {
                        let key = p.target.key();
                        crate::licence::set_session(&key, &email, &token);
                        // Kept for the next start: the refresh token always,
                        // the password only with Remember me (off removes it).
                        crate::licence::store(&key, &crate::licence::Stored { email: email.clone(), password: remember.then(|| password.clone()), refresh });
                        p.load(token, cx);
                    }
                    Err(e) if e == crate::remote::AGENT_OLD => p.view = View::AgentOld,
                    Err(e) => p.sign_in_error = Some(e),
                }
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
        cx.notify();
    }

    fn sign_out(&mut self, cx: &mut Context<Self>) {
        crate::licence::end_session(&self.target.key());
        crate::licence::forget_stored(&self.target.key());
        self.view = View::SignIn;
        self.open = None;
        self.menu = false;
        self.note = None;
        cx.notify();
    }

    /// Start SENTIENT (L7), then look again.
    fn start_sentient(&mut self, cx: &mut Context<Self>) {
        let target = self.target.clone();
        self.job.update(cx, |job, cx| {
            match &target {
                Target::Remote(i) => {
                    job.set_label(format!("Starting SENTIENT on {}", i.name));
                    job.set_instance(i.id.clone(), i.name.clone(), i.color.clone());
                }
                Target::Local(_) => job.set_label("Starting SENTIENT"),
            }
            job.start(
                move |sink| match target {
                    Target::Local(Some(r)) => r.platform().control(sink, "start"),
                    Target::Local(None) => Err("SENTIENT isn't installed on this computer.".into()),
                    Target::Remote(inst) => {
                        let args = ["control", "start"];
                        crate::remote::start_job(&inst, &args).and_then(|id| crate::remote::follow(&inst, &id, &sink))
                    }
                },
                cx,
            )
        });
        self.watch_job(cx);
    }

    /// Update the server's agent (it predates the licence calls), then look again.
    fn update_agent(&mut self, cx: &mut Context<Self>) {
        let Target::Remote(inst) = self.target.clone() else { return };
        self.job.update(cx, |job, cx| {
            job.set_label(format!("Updating the server agent on {}", inst.name));
            job.set_instance(inst.id.clone(), inst.name.clone(), inst.color.clone());
            job.start(move |sink| crate::remote::deploy(&inst, &sink), cx)
        });
        self.watch_job(cx);
    }

    fn watch_job(&mut self, cx: &mut Context<Self>) {
        self._watch = Some(cx.observe(&self.job, |p, job, cx| {
            if matches!(job.read(cx).state(), crate::jobs::JobState::Done(_)) {
                p._watch = None;
                p.check(cx);
            }
            cx.notify();
        }));
    }

    /// The signed-in session's token, or back to the sign-in.
    fn token(&mut self, cx: &mut Context<Self>) -> Option<crate::licence::Session> {
        let s = crate::licence::session(&self.target.key());
        if s.is_none() {
            self.sign_in_error = Some(crate::licence::SESSION_ENDED.into());
            self.view = View::SignIn;
            cx.notify();
        }
        s
    }

    /// Manage → Refresh, after its confirmation.
    fn refresh_licence(&mut self, cx: &mut Context<Self>) {
        let Some(s) = self.token(cx) else { return };
        self.reading = true;
        self.note = None;
        let target = self.target.clone();
        let task = cx.spawn(async move |this, cx| {
            let t = s.token.clone();
            let got = cx.background_executor().spawn(async move { target.refresh(&t) }).await;
            this.update(cx, |p, cx| {
                if let Err(e) = got {
                    p.note = Some((Tone::Bad, format!("The licence could not be refreshed: {e}")));
                }
                p.load(s.token.clone(), cx);
            })
            .ok();
        });
        self._tasks.push(task);
        cx.notify();
    }

    fn open_action(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = false;
        let Some(s) = self.token(cx) else { return };
        let page = cx.entity().downgrade();
        let target = self.target.clone();
        let name = self.name.clone();
        let title = act.title();
        let view = cx.new(|cx| ActionDialog { page, target, name, email: s.email.clone(), act, password: cx.new(|cx| InputState::new(window, cx).masked(true)), busy: false, error: None, _task: None });
        crate::modal::open(crate::modal::Modal { title: title.into(), body: view.into(), width: 520., icon: None, on_close: None }, cx);
    }

    /// An action finished in its dialog.
    fn acted(&mut self, msg: String, cx: &mut Context<Self>) {
        self.note = Some((Tone::Good, msg));
        self.open = None;
        self.key.update(cx, |_, cx| cx.notify());
        self.reload(cx);
    }

    fn choose_key_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let start = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(std::path::PathBuf::from);
        let rx = crate::chooser::open(
            cx,
            crate::chooser::Ask { title: "Choose a licence key file", start: start.as_deref(), directory: false },
            PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Import this key file".into()) },
        );
        let task = cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(file) = paths.into_iter().next() else { return };
            let name = file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "licence.key".into());
            let read = std::fs::metadata(&file).map_err(|e| e.to_string()).and_then(|m| {
                if m.len() > 1024 * 1024 {
                    Err("The file is larger than 1 MB: not a licence key file.".to_string())
                } else {
                    std::fs::read(&file).map_err(|e| e.to_string())
                }
            });
            this.update_in(cx, |p, window, cx| match read {
                Ok(bytes) => p.open_action(Act::Import(name, bytes), window, cx),
                Err(e) => {
                    p.note = Some((Tone::Bad, format!("{name} could not be read: {e}")));
                    cx.notify();
                }
            })
            .ok();
        });
        self._tasks.push(task);
    }

    /// Screens: a panel, the menu or a dialog opened by SPM_DEBUG_LICENCE_OPEN
    /// (details, limits, manage, deactivate, force-delete, all-limits).
    fn debug_open(&mut self, cx: &mut Context<Self>) {
        #[cfg(debug_assertions)]
        if let (View::Ready(r), Ok(what)) = (&self.view, std::env::var("SPM_DEBUG_LICENCE_OPEN")) {
            let r = (**r).clone();
            match what.as_str() {
                "details" => self.open = Some(Panel::Details),
                "limits" => self.open = Some(Panel::Limits),
                "manage" => self.menu = true,
                "all-limits" => LimitsDialog::open(r, cx),
                "deactivate" => self.pending = Some(Act::Deactivate),
                "force-delete" => self.pending = Some(Act::ForceDelete),
                _ => {}
            }
        }
        let _ = cx;
    }
}

// ---------------------------------------------------------------------------
// The actions that change the licence: each asks for the password again
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum Act {
    Activate(String),
    Import(String, Vec<u8>),
    Deactivate,
    ForceDelete,
}

impl Act {
    fn title(&self) -> &'static str {
        match self {
            Act::Activate(_) => "Activate licence",
            Act::Import(..) => "Import licence key file",
            Act::Deactivate => "Deactivate licence",
            Act::ForceDelete => "Force-delete local licence",
        }
    }
}

struct ActionDialog {
    page: WeakEntity<LicencePage>,
    target: Target,
    name: String,
    email: String,
    act: Act,
    password: Entity<InputState>,
    busy: bool,
    error: Option<String>,
    _task: Option<Task<()>>,
}

impl ActionDialog {
    fn go(&mut self, cx: &mut Context<Self>) {
        let password = self.password.read(cx).value().to_string();
        if password.is_empty() {
            self.error = Some("Enter the system administrator password.".into());
            cx.notify();
            return;
        }
        self.busy = true;
        self.error = None;
        let (target, email, act) = (self.target.clone(), self.email.clone(), self.act.clone());
        let name = self.name.clone();
        self._task = Some(cx.spawn(async move |this, cx| {
            let key = target.key();
            let got = cx
                .background_executor()
                .spawn(async move {
                    // The password again: a fresh sign-in, so a forgotten
                    // open session cannot change the licence.
                    let (token, _) = target.sign_in(&email, &password)?;
                    crate::licence::set_session(&key, &email, &token);
                    match act {
                        Act::Activate(k) => target.activate(&token, &k).map(|_| format!("The licence is active on {name}.")),
                        Act::Import(file, bytes) => target.import(&token, &file, &bytes).map(|_| format!("The licence from {file} is active on {name}.")),
                        Act::Deactivate => target.remove(&token, false).map(|_| format!("The licence is deactivated on {name}. The key can now be activated on another computer.")),
                        Act::ForceDelete => target.remove(&token, true).map(|_| format!("The licence is deleted from {name}. Its binding on the licence server is not released.")),
                    }
                })
                .await;
            this.update(cx, |d, cx| {
                d.busy = false;
                match got {
                    Ok(msg) => {
                        crate::modal::close(cx);
                        d.page.update(cx, |p, cx| p.acted(msg, cx)).ok();
                    }
                    Err(e) => d.error = Some(e),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }
}

impl Render for ActionDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let name = &self.name;
        let (danger, ok): (bool, &str) = match &self.act {
            Act::Activate(_) | Act::Import(..) => (false, "Activate"),
            Act::Deactivate => (true, "Deactivate"),
            Act::ForceDelete => (true, "Delete locally"),
        };
        let mut body = v_flex().gap_3();
        body = match &self.act {
            Act::Activate(_) => body.child(div().text_sm().child(format!("SENTIENT checks this key with the licence server and binds it to {}'s hardware.", name_or_this(name)))),
            Act::Import(file, _) => body.child(div().text_sm().child(format!("SENTIENT activates the licence in {file} and binds it to {}'s hardware.", name_or_this(name)))),
            Act::Deactivate => body.child(div().text_sm().child(format!("This releases the binding on the licence server and removes the licence from {name}. The same key can then be activated on another computer."))),
            Act::ForceDelete => body
                .child(banner(&theme, Tone::Bad, None, format!("Deletes the licence from {name} without contacting the licence server. The binding there is not released and the slot stays taken until an administrator force-unbinds it on the licence server."), None).px(px(12.)).py(px(10.)))
                .child(div().text_size(px(13.)).text_color(theme.muted_foreground).child("Use this only when Deactivate keeps failing, for example when the licence server cannot be reached.")),
        };
        body = body
            .child(
                v_flex()
                    .gap(px(6.))
                    .child(div().text_size(px(13.)).text_color(theme.muted_foreground).child("System administrator password"))
                    .child(Input::new(&self.password).mask_toggle().disabled(self.busy)),
            )
            .when_some(self.error.clone(), |c, e| c.child(ui::outcome(&theme, Tone::Bad, e)));
        let click = cx.listener(|d, _, _, cx| d.go(cx));
        let ok_button: AnyElement = if danger {
            h_flex()
                .id("licence-act-ok")
                .h(px(32.))
                .px_3()
                .items_center()
                .rounded(theme.radius)
                .bg(theme.danger)
                .text_color(gpui_kit::white())
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .when(!self.busy, |b| b.cursor_pointer().hover(|d| d.opacity(0.9)).on_click(click))
                .when(self.busy, |b| b.opacity(0.6))
                .child(if self.busy { "Working…" } else { ok })
                .into_any_element()
        } else {
            ui::button("licence-act-ok").primary().label(if self.busy { "Working…" } else { ok }).disabled(self.busy).on_click(click).into_any_element()
        };
        body.child(
            h_flex()
                .gap(crate::ui::button_gap())
                .justify_end()
                .pt_1()
                .child(crate::ui::probed(ui::button("licence-act-cancel").outline().label("Cancel").disabled(self.busy).on_click(|_, _, cx| crate::modal::close(cx))))
                .child(crate::ui::probed(ok_button)),
        )
    }
}

/// The sign-in card (and its placeholder) in the middle of the page, like
/// the licence card.
fn centred(el: Div) -> Div {
    v_flex().flex_1().w_full().items_center().justify_center().pb(px(40.)).child(el)
}

fn name_or_this(name: &str) -> String {
    if name == "this computer" { "this computer".into() } else { name.to_string() }
}

/// "All limits…": every limit SENTIENT has, with the usage where it counts it.
struct LimitsDialog {
    reading: Reading,
}

impl LimitsDialog {
    fn open(reading: Reading, cx: &mut App) {
        let view = cx.new(|_| LimitsDialog { reading });
        crate::modal::open(crate::modal::Modal { title: "All limits".into(), body: view.into(), width: 560., icon: None, on_close: None }, cx);
    }
}

impl Render for LimitsDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let limits = self.reading.status.limits.clone().unwrap_or_default();
        let usage = self.reading.usage.as_ref().map(|u| u.usage.clone()).unwrap_or_default();
        let mut list = v_flex();
        let mut first = true;
        for (label, key, used) in crate::licence::ALL_LIMITS {
            let Some(v) = limits.get(*key) else { continue };
            let mut value = crate::licence::limit_text(key, v);
            if let Some(n) = used.and_then(|u| usage.get(u)).and_then(serde_json::Value::as_i64) {
                value = format!("{} of {value}", crate::licence::grouped(n));
            }
            list = list.child(
                h_flex()
                    .gap_4()
                    .py(px(7.))
                    .when(!first, |r| r.border_t_1().border_color(theme.border))
                    .text_sm()
                    .child(div().flex_1().min_w_0().text_color(theme.muted_foreground).child(*label))
                    .child(div().flex_shrink_0().font_family(theme.mono_font_family.clone()).text_size(px(13.)).child(value)),
            );
            first = false;
        }
        if first {
            list = list.child(div().text_sm().text_color(theme.muted_foreground).child("SENTIENT reported no limits."));
        }
        v_flex().gap_3().child(list).child(h_flex().justify_end().child(ui::button("all-limits-close").primary().label("Close").on_click(|_, _, cx| crate::modal::close(cx))))
    }
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// A banner as in the mockup: icon, a bold lead-in then the text on one
/// flow, and an optional action at the right.
fn banner(theme: &gpui_kit::component::Theme, tone: Tone, lead: Option<&str>, text: String, action: Option<AnyElement>) -> Div {
    let (bg, fg, ic) = match tone {
        Tone::Warn => (theme.warning, theme.warning_foreground, gpui_kit::component::IconName::TriangleAlert),
        Tone::Good => (theme.success, theme.success_foreground, gpui_kit::component::IconName::CircleCheck),
        Tone::Neutral => (theme.secondary, theme.secondary_foreground, gpui_kit::component::IconName::Info),
        Tone::Bad => (theme.danger.opacity(0.12), theme.danger, gpui_kit::component::IconName::TriangleAlert),
    };
    let full = match lead {
        Some(l) => format!("{l} {text}"),
        None => text,
    };
    let bold = lead.map(|l| l.len()).unwrap_or(0);
    let styled = StyledText::new(full).with_highlights(if bold > 0 { vec![(0..bold, HighlightStyle { font_weight: Some(FontWeight::MEDIUM), ..Default::default() })] } else { vec![] });
    h_flex()
        .gap_3()
        .items_start()
        .px_4()
        .py_3()
        .rounded(px(ui::PANEL_RADIUS))
        .bg(bg)
        .text_color(fg)
        .child(Icon::new(ic).size(px(18.)).text_color(fg).mt(px(2.)).flex_shrink_0())
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .text_sm()
                .line_height(px(20.))
                .child(div().child(styled)),
        )
        .children(action.map(|a| div().relative().flex_shrink_0().child(a).child(ui::layout_probe("button"))))
}

/// A row of the Details and Limits panels: a top hairline but on the first.
fn kv_row(theme: &gpui_kit::component::Theme, first: bool) -> Div {
    h_flex().min_h(px(52.)).py(px(10.)).items_center().text_sm().when(!first, |r| r.border_t_1().border_color(theme.border))
}

impl LicencePage {
    fn title_action(&self, signed: Option<&crate::licence::Session>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let refresh = ui::refresh_button("licence-refresh", self.reading || self.signing_in, window, {
            let me = cx.entity().downgrade();
            move |_, cx| {
                me.update(cx, |p, cx| p.reload(cx)).ok();
            }
        });
        h_flex()
            .gap(crate::ui::button_gap())
            .items_center()
            .when_some(signed, |r, s| {
                r.child(
                    h_flex()
                        .gap(px(6.))
                        .items_center()
                        .min_w_0()
                        .text_size(px(13.))
                        .text_color(theme.muted_foreground)
                        .child(icon("user").size(px(16.)).text_color(theme.muted_foreground))
                        .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(s.email.clone())),
                )
                .child(crate::ui::probed(ui::button("licence-sign-out").outline().label("Sign out").on_click(cx.listener(|p, _, _, cx| p.sign_out(cx)))))
            })
            .child(crate::ui::probed(refresh))
            .into_any_element()
    }

    fn sign_in_card(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> Div {
        let field = |label: &'static str, input: AnyElement| v_flex().gap(px(6.)).child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(label)).child(input);
        ui::panel(theme)
            .w_full()
            .max_w(px(520.))
            .child(
                h_flex()
                    .gap(px(10.))
                    .items_center()
                    .child(icon("shield").size(px(22.)).text_color(theme.primary))
                    .child(div().text_size(px(16.)).font_weight(FontWeight::MEDIUM).child("Sign in to manage the licence")),
            )
            .child(div().text_sm().text_color(theme.muted_foreground).child(format!("Use the SENTIENT system administrator of {}.", self.name)))
            .child(field("Email", Input::new(&self.email).disabled(self.signing_in).into_any_element()))
            .child(field("Password", Input::new(&self.password).mask_toggle().disabled(self.signing_in).into_any_element()))
            .child(
                gpui_kit::component::checkbox::Checkbox::new("licence-remember")
                    .label("Remember me")
                    .checked(self.remember)
                    .disabled(self.signing_in)
                    .on_change(cx.listener(|p, v: &bool, _, cx| {
                        p.remember = *v;
                        cx.notify();
                    })),
            )
            .when_some(self.sign_in_error.clone(), |c, e| c.child(ui::outcome(theme, Tone::Bad, e)))
            .child(
                h_flex()
                    .flex_wrap()
                    .gap(crate::ui::button_gap())
                    .items_center()
                    .child(
                        div().relative().flex_shrink_0().child(
                            ui::button("licence-sign-in").primary().label(if self.signing_in { "Signing in…" } else { "Sign in" }).disabled(self.signing_in).on_click(cx.listener(|p, _, window, cx| p.sign_in(window, cx))),
                        ),
                    )
                    .child(div().flex_1())
                    .child(
                        h_flex()
                            .id("licence-forgot")
                            .gap_1()
                            .text_size(px(13.))
                            .text_color(theme.muted_foreground)
                            .child("Forgot it?")
                            .child(div().cursor_pointer().hover(|d| d.text_color(theme.primary)).child("Reset it in Platform settings."))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(GoSettings))),
                    ),
            )
    }

    /// While checking or reading: the shapes the page will show.
    fn skeleton(&self, theme: &gpui_kit::component::Theme, card: bool, w: f32) -> Div {
        if card {
            let f = (w / licence_card::W).min(1.);
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap(px(16.))
                .pb(px(40.))
                .child(ui::skeleton_block_r(theme, "licence-card", px(w).into(), licence_card::H * f, 22. * f))
                .child(h_flex().w(px(w)).gap(px(8.)).children((0..3).map(|i| div().flex_1().child(ui::skeleton_block(theme, format!("licence-btn-{i}"), relative(1.).into(), 32.)))))
        } else {
            ui::panel(theme)
                .w_full()
                .max_w(px(520.))
                .child(ui::skeleton_line(theme, "licence-si-1", 240.))
                .child(ui::skeleton_line(theme, "licence-si-2", 380.))
                .child(ui::skeleton_block(theme, "licence-si-3", relative(1.).into(), 32.))
                .child(ui::skeleton_block(theme, "licence-si-4", relative(1.).into(), 32.))
                .child(ui::skeleton_block(theme, "licence-si-5", px(80.).into(), 32.))
        }
    }

    /// A toggle under the card (Details, Limits) or the Manage button.
    fn toggle(&self, theme: &gpui_kit::component::Theme, id: &'static str, ic: &str, label: &'static str, on: bool, chevron_up: bool) -> Stateful<Div> {
        let fg = if on { theme.primary } else { theme.foreground };
        h_flex()
            .id(id)
            .flex_1()
            .min_w_0()
            .h(px(32.))
            .gap(px(8.))
            .px(px(10.))
            .justify_center()
            .items_center()
            .rounded(px(8.))
            .border_1()
            .border_color(if on { theme.primary } else { theme.input })
            .bg(if on { theme.primary.opacity(0.06) } else { theme.background })
            .text_color(fg)
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .whitespace_nowrap()
            .cursor_pointer()
            .when(!on, |d| d.hover(|d| d.bg(theme.list_hover)))
            .child(if ic == "info" { Icon::new(gpui_kit::component::IconName::Info) } else { icon(ic) }.size(px(16.)).text_color(fg).flex_shrink_0())
            .child(label)
            .child(Icon::new(if chevron_up { gpui_kit::component::IconName::ChevronUp } else { gpui_kit::component::IconName::ChevronDown }).size(px(16.)).text_color(fg).flex_shrink_0())
    }

    fn details(&self, theme: &gpui_kit::component::Theme, r: &Reading, w: f32) -> Div {
        let st = &r.status;
        let local = matches!(self.target, Target::Local(_));
        let days = match (st.days_remaining, crate::licence::day_label(st.expiration_date.as_deref())) {
            (_, None) => "No end date".to_string(),
            (Some(d), Some(day)) if d > 0 => format!("{d} day{}, until {day}", if d == 1 { "" } else { "s" }),
            (_, Some(day)) => format!("Ended on {day}"),
        };
        let row = |label: &'static str, value: AnyElement, first: bool| kv_row(theme, first).child(div().w(px(150.)).flex_shrink_0().text_color(theme.muted_foreground).child(label)).child(h_flex().flex_1().min_w_0().gap_2().items_center().flex_wrap().child(value));
        let hw = if local { "This computer's hardware" } else { "This server's hardware" };
        let bound = match &st.binding_block_reason {
            None => h_flex().gap_2().items_center().child(hw).child(ui::pill(theme, Tone::Good, "Matches").text_size(px(12.)).py(px(0.))).into_any_element(),
            Some(why) => v_flex().gap_1().child(h_flex().gap_2().items_center().child(hw).child(ui::pill(theme, Tone::Bad, "Does not match").text_size(px(12.)).py(px(0.)))).child(div().text_size(px(13.)).text_color(theme.danger).child(why.clone())).into_any_element(),
        };
        let allowed = st.license_allowed_versions.clone().unwrap_or_default();
        let build = st.binary_version.clone().unwrap_or_else(|| "Not known".into());
        let covered = if allowed.is_empty() { String::new() } else { format!("· covered by v{}", allowed.join(", v")) };
        let build_el = h_flex().gap(px(6.)).items_baseline().child(build).when(!covered.is_empty(), |r| r.child(div().text_size(px(12.5)).text_color(theme.muted_foreground).child(covered))).into_any_element();
        let check = match st.binary_integrity.as_ref().map(|i| i.kind.as_str()) {
            Some("known") => "Licence server knows this build".to_string(),
            Some("unknown") => "Licence server does not know this build".to_string(),
            Some("server-unreachable") => "Licence server not reachable".to_string(),
            _ => "Not checked yet".to_string(),
        };
        ui::panel(theme)
            .w(px(w))
            .gap_0()
            .py(px(6.))
            .px(px(18.))
            .child(row("Days left", div().child(days).into_any_element(), true))
            .child(row("Bound to", bound, false))
            .child(row("This build", build_el, false))
            .child(row("Last check", div().child(check).into_any_element(), false))
    }

    fn limits(&self, theme: &gpui_kit::component::Theme, r: &Reading, w: f32, cx: &mut Context<Self>) -> Div {
        let limits = r.status.limits.clone().unwrap_or_default();
        let usage = r.usage.as_ref().map(|u| u.usage.clone()).unwrap_or_default();
        let narrow = w < 470.;
        let mut col = ui::panel(theme)
            .w(px(w))
            .gap_0()
            .pt(px(14.))
            .pb(px(6.))
            .px(px(18.))
            .child(
                h_flex()
                    .min_h(px(28.))
                    .items_center()
                    .child(div().flex_1().text_sm().font_weight(FontWeight::MEDIUM).child("Limits"))
                    .child(ui::button("licence-all-limits").ghost().text_color(theme.primary).label("All limits…").on_click({
                        let r = r.clone();
                        cx.listener(move |_, _, _, cx| LimitsDialog::open(r.clone(), cx))
                    })),
            )
            .child(div().h(px(8.)));
        for (i, (label, lkey, ukey)) in crate::licence::MAIN_LIMITS.iter().enumerate() {
            let max = limits.get(*lkey).and_then(serde_json::Value::as_i64).unwrap_or(0);
            let used = usage.get(*ukey).and_then(serde_json::Value::as_i64).unwrap_or(0);
            let over = max > 0 && used > max;
            let frac = if max > 0 { (used as f32 / max as f32).clamp(0., 1.) } else { 0. };
            let value = format!("{} / {}", crate::licence::grouped(used), if max == 0 { "Unlimited".to_string() } else if max < 0 { "Not allowed".into() } else { crate::licence::grouped(max) });
            let bar_c = if over { theme.danger } else { theme.primary };
            col = col.child(
                kv_row(theme, i == 0)
                    .child(div().w(px(if narrow { 110. } else { 170. })).flex_shrink_0().text_color(theme.muted_foreground).child(*label))
                    .child(div().flex_1().min_w_0().pr_4().child(div().max_w(px(260.)).child(ui::segment_bar(theme.foreground.opacity(0.06), 5., &[(bar_c, frac)]))))
                    .child(div().w(px(if narrow { 110. } else { 140. })).flex_shrink_0().flex().justify_end().font_family(theme.mono_font_family.clone()).text_size(px(13.)).when(over, |d| d.text_color(theme.danger)).child(value)),
            );
        }
        col
    }

    fn manage_menu(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> AnyElement {
        let item = |id: &'static str, ic: &'static str, label: &'static str, danger: bool| {
            let c = if danger { theme.danger } else { theme.foreground };
            h_flex()
                .id(id)
                .gap(px(10.))
                .items_center()
                .px(px(10.))
                .py(px(7.))
                .rounded(px(6.))
                .text_sm()
                .text_color(c)
                .cursor_pointer()
                .hover(|d| d.bg(theme.list_hover))
                .child(if ic == "refresh" { Icon::new(gpui_kit::component::IconName::RefreshCw) } else { icon(ic) }.size(px(16.)).text_color(if danger { theme.danger } else { theme.muted_foreground }))
                .child(label)
        };
        let close = cx.entity().downgrade();
        deferred(
            div().absolute().top(px(40.)).right_0().child(
                v_flex()
                    .id("licence-manage-menu")
                    .occlude()
                    .min_w(px(230.))
                    .p_1()
                    .rounded(px(8.))
                    .bg(theme.popover)
                    .text_color(theme.popover_foreground)
                    .border_1()
                    .border_color(theme.border)
                    .shadow_lg()
                    .child(item("licence-m-refresh", "refresh", "Refresh", false).on_click(cx.listener(|p, _, window, cx| {
                        p.menu = false;
                        let me = cx.entity().downgrade();
                        crate::modal::alert(window, cx, move |a, _, _| {
                            a.title("Refresh the licence")
                                .description("SENTIENT asks the licence server about this licence now, and the page shows its answer.")
                                .ok_text("Refresh")
                                .on_ok(move |_, _, cx| {
                                    me.update(cx, |p, cx| p.refresh_licence(cx)).ok();
                                    true
                                })
                        });
                        cx.notify();
                    })))
                    .child(item("licence-m-deactivate", "unlink", "Deactivate…", false).on_click(cx.listener(|p, _, window, cx| p.open_action(Act::Deactivate, window, cx))))
                    .child(item("licence-m-force", "trash", "Force-delete local licence…", true).on_click(cx.listener(|p, _, window, cx| p.open_action(Act::ForceDelete, window, cx))))
                    .on_mouse_down_out(move |_, _, cx| {
                        close
                            .update(cx, |p, cx| {
                                p.menu = false;
                                cx.notify();
                            })
                            .ok();
                    }),
            ),
        )
        .with_priority(1)
        .into_any_element()
    }

    fn licensed(&self, theme: &gpui_kit::component::Theme, r: &Reading, w: f32, cx: &mut Context<Self>) -> Div {
        let open = self.open;
        let menu = self.menu;
        let buttons = h_flex()
            .relative()
            .w(px(w))
            .gap(px(8.))
            .child(self.toggle(theme, "licence-details", "info", "Details", open == Some(Panel::Details), open == Some(Panel::Details)).on_click(cx.listener(|p, _, _, cx| {
                p.open = if p.open == Some(Panel::Details) { None } else { Some(Panel::Details) };
                cx.notify();
            })))
            .child(self.toggle(theme, "licence-limits", "chart", "Limits", open == Some(Panel::Limits), open == Some(Panel::Limits)).on_click(cx.listener(|p, _, _, cx| {
                p.open = if p.open == Some(Panel::Limits) { None } else { Some(Panel::Limits) };
                cx.notify();
            })))
            .child(self.toggle(theme, "licence-manage", "key", "Manage", menu, false).on_click(cx.listener(|p, _, _, cx| {
                p.menu = !p.menu;
                cx.notify();
            })))
            .when(menu, |b| b.child(self.manage_menu(theme, cx)));
        v_flex()
            .flex_1()
            .items_center()
            .gap(px(16.))
            .when(open.is_none(), |c| c.justify_center().pb(px(40.)))
            .child(licence_card::card(&r.status, w, theme.mono_font_family.clone()))
            .child(buttons)
            .when(open == Some(Panel::Details), |c| c.child(self.details(theme, r, w)))
            .when(open == Some(Panel::Limits), |c| c.child(self.limits(theme, r, w, cx)))
    }

    fn no_licence(&self, theme: &gpui_kit::component::Theme, avail: f32, cx: &mut Context<Self>) -> Div {
        let w = avail.min(600.);
        let title = if self.name == "this computer" { "No licence on this computer".to_string() } else { format!("No licence on {}", self.name) };
        v_flex().flex_1().items_center().justify_center().pb(px(40.)).child(
            ui::panel(theme)
                .w(px(w))
                .child(h_flex().gap_2().items_center().child(div().flex_1().min_w_0().text_size(px(16.)).font_weight(FontWeight::MEDIUM).child(title)).child(ui::pill(theme, Tone::Neutral, "Not licensed").text_size(px(12.)).py(px(0.))))
                .child(
                    h_flex().gap(px(10.)).items_center().child(div().flex_1().min_w_0().child(Input::new(&self.key))).child(ui::probed(ui::button("licence-activate").primary().label("Activate…").on_click(cx.listener(|p, _, window, cx| {
                        let key = p.key.read(cx).value().trim().to_string();
                        if key.is_empty() {
                            p.note = Some((Tone::Bad, "Enter the licence key first.".into()));
                            cx.notify();
                            return;
                        }
                        p.open_action(Act::Activate(key), window, cx);
                    })))),
                )
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap(crate::ui::button_gap())
                        .items_center()
                        .child(div().text_sm().text_color(theme.muted_foreground).child("Or import a licence key from a file:"))
                        .child(ui::probed(ui::button("licence-import").outline().icon(icon("file")).label("Import key file…").on_click(cx.listener(|p, _, window, cx| p.choose_key_file(window, cx))))),
                )
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap(crate::ui::button_gap())
                        .items_center()
                        .pt(px(12.))
                        .border_t_1()
                        .border_color(theme.border)
                        .child(div().text_sm().text_color(theme.muted_foreground).child("No key yet?"))
                        .child(ui::probed(ui::button("licence-demo").outline().icon(icon("mail")).label("Request a demo licence").on_click(|_, _, cx| ui::open_url(crate::licence::DEMO_URL, cx))))
                        .child(ui::probed(ui::button("licence-buy").outline().icon(icon("cart")).label("Buy a licence").on_click(|_, _, cx| ui::open_url(crate::licence::BUY_URL, cx)))),
                ),
        )
    }

    /// The over-limit warning SENTIENT shows, with the days left before it blocks.
    fn over_limit(&self, theme: &gpui_kit::component::Theme, r: &Reading) -> Option<Div> {
        let st = r.state.as_ref()?;
        if !st.is_over_limit {
            return None;
        }
        let what = st.over_limit_entities.first().cloned().or_else(|| r.usage.as_ref().and_then(|u| u.over_limit.first().cloned()))?;
        let noun = crate::licence::entity_noun(&what);
        let days = st.days_over_limit.unwrap_or(0);
        let lead = format!("Over the {noun} limit for {days} day{}.", if days == 1 { "" } else { "s" });
        let lkey = crate::licence::ALL_LIMITS.iter().find(|(_, _, u)| *u == Some(what.as_str())).map(|(_, k, _)| *k).unwrap_or("");
        let max = r.status.limits.as_ref().and_then(|l| l.get(lkey)).and_then(serde_json::Value::as_i64);
        let used = r.usage.as_ref().and_then(|u| u.usage.get(&what)).and_then(serde_json::Value::as_i64);
        let mut text = match (used, max) {
            (Some(u), Some(m)) => format!("{} {noun}s on a {}-{noun} licence. ", crate::licence::grouped(u), crate::licence::grouped(m)),
            _ => String::new(),
        };
        text += &match st.days_until_blocked {
            Some(0) => format!("SENTIENT blocks new {noun}s now, until {noun}s are removed or the licence is raised."),
            Some(d) => format!("SENTIENT is blocked in {d} day{} unless {noun}s are removed or the licence is raised.", if d == 1 { "" } else { "s" }),
            None => format!("Remove {noun}s or raise the licence."),
        };
        let buy = ui::button("licence-over-buy").outline().icon(icon("cart")).label("Buy a licence…").on_click(|_, _, cx| ui::open_url(crate::licence::BUY_URL, cx)).into_any_element();
        Some(banner(theme, Tone::Bad, Some(&lead), text, Some(buy)))
    }
}

impl Render for LicencePage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(act) = self.pending.take() {
            self.open_action(act, window, cx);
        }
        let theme = cx.theme().clone();
        let signed = match self.view {
            View::Ready(_) | View::Loading => crate::licence::peek_session(&self.target.key()),
            _ => None,
        };
        let action = self.title_action(signed.as_ref(), window, cx);
        // The content width, measured (the card shrinks on a narrow page).
        let width = self.width.clone();
        let probe = canvas(
            move |bounds, window, _| {
                let w = f32::from(bounds.size.width);
                if (width.get() - w).abs() > 0.5 {
                    width.set(w);
                    window.refresh();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .h(px(1.));
        // The probe spans the page's own box (not its content), less its padding.
        let avail = if self.width.get() > 0. { (self.width.get() - 2. * ui::PAGE_PAD).max(200.) } else { licence_card::W };
        // Room for the card's shadow at the sides when the page is narrow.
        let w = (avail - 40.).min(licence_card::W);
        let busy = self.job.read(cx).is_running();

        let mut page = ui::page_with(&theme, "Licence", "", action).relative().child(probe);
        if let Some((tone, msg)) = self.note.clone() {
            page = page.child(h_flex().gap_2().items_start().child(div().flex_1().min_w_0().child(ui::outcome(&theme, tone, msg))).child(ui::button("licence-note-x").ghost().icon(gpui_kit::component::IconName::Close).on_click(cx.listener(|p, _, _, cx| {
                p.note = None;
                cx.notify();
            }))));
        }
        let page = match self.view.clone() {
            View::Checking => page.child(centred(self.skeleton(&theme, false, w))),
            View::Loading => page.child(self.skeleton(&theme, true, w)),
            View::NotInstalled => match self.target {
                Target::Local(_) => page.child(ui::not_installed(&theme)),
                Target::Remote(_) => page.child(ui::notice(&theme, Tone::Neutral, format!("SENTIENT isn't installed on {}.\nInstall it from the Update page first; the licence is kept by SENTIENT itself.", self.name))),
            },
            View::Stopped => page.child(banner(
                &theme,
                Tone::Warn,
                Some("SENTIENT is stopped."),
                "The licence is kept by SENTIENT itself, so it has to run to show or change it.".into(),
                Some(ui::button("licence-start").outline().icon(Icon::empty().path("icons/power.svg")).label(if busy { "Starting…" } else { "Start SENTIENT" }).disabled(busy).on_click(cx.listener(|p, _, _, cx| p.start_sentient(cx))).into_any_element()),
            )),
            View::AgentOld => page.child(ui::notice_with(
                &theme,
                Tone::Warn,
                "The agent on this server is out of date.\nUpdate it to manage the licence from here. Nothing else on the server changes.",
                Some(ui::button("licence-agent-update").primary().label("Update agent").disabled(busy).on_click(cx.listener(|p, _, _, cx| p.update_agent(cx))).into_any_element()),
            )),
            View::Failed(e) => page.child(ui::notice_with(
                &theme,
                Tone::Bad,
                format!("The licence could not be read.\n{e}"),
                Some(ui::button("licence-retry").outline().label("Retry").on_click(cx.listener(|p, _, _, cx| p.reload(cx))).into_any_element()),
            )),
            View::SignIn => page.child(centred(self.sign_in_card(&theme, cx))),
            View::Ready(r) => {
                let page = match &r.status.binding_block_reason {
                    Some(why) => page.child(banner(&theme, Tone::Bad, Some("SENTIENT refuses new items."), why.clone(), None)),
                    None => page,
                };
                let page = page.children(self.over_limit(&theme, &r));
                if r.status.activated {
                    page.child(self.licensed(&theme, &r, w, cx))
                } else {
                    page.child(self.no_licence(&theme, avail, cx))
                }
            }
        };
        page
    }
}
