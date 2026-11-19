// Copyright © 2016-2026 The SENTIENT Authors

//! Before a SENTIENT update (this computer and a server): whether the
//! instance's licence covers the version it goes to, and the dialogs that
//! protect the operator when it does not.
//!
//! The labels beside the offered versions come from the licence's allowed
//! versions, read without a sign-in from SENTIENT's database (here
//! crate::licence::read_versions; on a server its agent, in
//! `RemoteStatus::licence_versions`). When that cannot be read (an older
//! agent, native Linux without administrator rights), the labels say
//! "Licence not checked" and pressing Update signs in first: once, as
//! SENTIENT's system administrator, the email and password only in memory
//! for that one read (crate::licence::Credentials), never stored.
//!
//! The decision itself is pure: sentient_installer_core::licence_version
//! ([`decide`], SENTIENT's rule).

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Mutex;

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use sentient_installer_core::licence_version::{self, Coverage, Gate, Licence, LicenceRead, decide};

use crate::licence::{Credentials, Target};
use crate::ui::{self, Tone};

/// What a sign-in found, by instance (crate::licence::Target::key), for this
/// run of the Platform Manager: the allowed versions only, nothing of the
/// sign-in.
static CHECKED: Mutex<Option<HashMap<String, Licence>>> = Mutex::new(None);

/// What a sign-in found for `key` in this run.
pub fn remembered(key: &str) -> Option<Licence> {
    CHECKED.lock().ok()?.as_ref()?.get(key).cloned()
}

fn remember(key: &str, licence: &Licence) {
    if let Ok(mut g) = CHECKED.lock() {
        g.get_or_insert_with(HashMap::new).insert(key.to_string(), licence.clone());
    }
}

/// What is known for `key`: a sign-in in this run, else what was read
/// without one, else nothing yet.
pub fn known(key: &str, read_without_sign_in: Option<&Licence>) -> LicenceRead {
    // Screens: the licence as read without a sign-in,
    // SPM_DEBUG_LICENCE_VERSIONS=4.4,4.3 ("none": no licence, "empty": an
    // empty list).
    #[cfg(debug_assertions)]
    if let Ok(v) = std::env::var("SPM_DEBUG_LICENCE_VERSIONS") {
        return LicenceRead::Read(match v.as_str() {
            "none" => Licence::NotInstalled,
            "empty" => Licence::Allowed(Vec::new()),
            list => Licence::Allowed(list.split(',').map(|s| s.trim().to_string()).collect()),
        });
    }
    match remembered(key).or_else(|| read_without_sign_in.cloned()) {
        Some(l) => LicenceRead::Read(l),
        None => LicenceRead::NotRead,
    }
}

/// The label beside an offered version.
pub fn pill(theme: &gpui_kit::component::Theme, read: &LicenceRead, build: &str) -> Div {
    let c = licence_version::coverage(read, build);
    let tone = match c {
        Coverage::Covered => Tone::Good,
        Coverage::NeedsNew => Tone::Warn,
        Coverage::NoLicence | Coverage::NotChecked => Tone::Neutral,
    };
    ui::pill(theme, tone, c.label())
}

/// The label's words after a version in a menu ("4.5.0.1 · Needs a new licence").
pub fn menu_note(read: &LicenceRead, build: &str) -> &'static str {
    match licence_version::coverage(read, build) {
        Coverage::Covered => " · Covered by your licence",
        Coverage::NeedsNew => " · Needs a new licence",
        _ => "",
    }
}

/// One update the operator asked for.
pub struct Ask {
    pub target: Target,
    /// "this computer", or the server's name.
    pub place: String,
    /// The release, as shown ("4.5.0.1").
    pub version: String,
    /// Its build version, what the licence is compared with ("4.5.0").
    pub build: String,
    /// The normal confirmation (covered, or no licence installed).
    pub normal: Rc<dyn Fn(&mut Window, &mut App)>,
    /// "Update anyway", after the version was typed.
    pub anyway: Rc<dyn Fn(&mut Window, &mut App)>,
    /// What a sign-in found (the page relabels its versions).
    pub learned: Rc<dyn Fn(LicenceRead, &mut App)>,
}

/// Pressing Update: the dialog [`decide`] calls for.
pub fn guard(ask: Rc<Ask>, read: LicenceRead, window: &mut Window, cx: &mut App) {
    match decide(&read, &ask.build) {
        Gate::Confirm => (ask.normal)(window, cx),
        Gate::SignInFirst => sign_in(ask, window, cx),
        Gate::NotCovered { allowed } => {
            let red = licence_version::not_covered_text(&allowed, &ask.version, &ask.build);
            let hint = if allowed.is_empty() {
                "SENTIENT refuses a licence with an empty list of allowed versions.".to_string()
            } else {
                "Request the licence first under Licence, or update to a version your licence covers.".to_string()
            };
            typed(ask.clone(), format!("Update to {} without a licence for it?", ask.version), None, red, hint, false, window, cx);
        }
        Gate::Unchecked => {
            let why = match &read {
                LicenceRead::Failed(e) if !e.is_empty() && e != CANCELLED => format!("The Platform Manager could not read which versions this instance's licence covers: {e}"),
                _ => "The sign-in was cancelled, so the Platform Manager could not read which versions this instance's licence covers.".to_string(),
            };
            let red = format!("If the licence does not cover {}, SENTIENT stops working after the update until you have a licence for it.", ask.version);
            typed(ask, "Your licence could not be checked".into(), Some(why), red, String::new(), true, window, cx);
        }
    }
}

/// The normal confirmation of an update: SENTIENT `version` on `place`,
/// what the licence says about it, and Update.
pub fn confirm_update(place: &str, version: &str, installed: &str, read: &LicenceRead, build: &str, go: Rc<dyn Fn(&mut App)>, window: &mut Window, cx: &mut App) {
    let licence = match licence_version::coverage(read, build) {
        Coverage::Covered => "Your licence covers this version. ",
        Coverage::NoLicence => "No licence is installed. ",
        _ => "",
    };
    let text = format!(
        "{licence}SENTIENT on {place} stops for a few minutes while it updates to {version}, and goes back to {installed} by itself if the update fails."
    );
    let title = format!("Update SENTIENT to {version}?");
    crate::modal::alert(window, cx, move |dialog, _, _| {
        let go = go.clone();
        dialog.title(title.clone()).description(text.clone()).confirm().ok_text("Update").on_ok(move |_, _, cx| {
            go(cx);
            true
        })
    });
}

/// What a cancelled sign-in leaves.
const CANCELLED: &str = "cancelled";

/// Read the licence after a sign-in: this run's session of the Licence page
/// when there is one (no prompt), else the sign-in dialog.
fn sign_in(ask: Rc<Ask>, window: &mut Window, cx: &mut App) {
    let key = ask.target.key();
    if let Some(session) = crate::licence::peek_session(&key) {
        let target = ask.target.clone();
        window
            .spawn(cx, async move |cx| {
                let token = session.token;
                let got = cx.background_executor().spawn(async move { target.status(&token).and_then(|s| crate::licence::licence_of(&s)) }).await;
                cx.update(|window, cx| match got {
                    Ok(l) => after_read(ask, Ok(l), window, cx),
                    // The session ran out: ask.
                    Err(_) => open_sign_in(ask, window, cx),
                })
                .ok();
            })
            .detach();
        return;
    }
    open_sign_in(ask, window, cx);
}

/// What a sign-in (or its cancel) found: kept for the labels, then the
/// dialog it calls for.
fn after_read(ask: Rc<Ask>, got: Result<Licence, String>, window: &mut Window, cx: &mut App) {
    let read = match got {
        Ok(l) => {
            remember(&ask.target.key(), &l);
            LicenceRead::Read(l)
        }
        Err(e) => LicenceRead::Failed(e),
    };
    (ask.learned)(read.clone(), cx);
    guard(ask, read, window, cx);
}

fn open_sign_in(ask: Rc<Ask>, window: &mut Window, cx: &mut App) {
    let title = format!("Sign in to SENTIENT on {}", ask.place);
    let cancel_ask = ask.clone();
    let view = cx.new(|cx| SignInBody::new(ask, window, cx));
    crate::modal::open(
        crate::modal::Modal {
            title: title.into(),
            body: view.into(),
            width: 480.,
            icon: None,
            // Escape, the close button, the dim layer: cancelled.
            on_close: Some(Rc::new(move |window, cx| after_read(cancel_ask.clone(), Err(CANCELLED.into()), window, cx))),
        },
        cx,
    );
}

struct SignInBody {
    ask: Rc<Ask>,
    email: Entity<InputState>,
    password: Entity<InputState>,
    busy: bool,
    error: Option<String>,
    _enter: Vec<Subscription>,
}

impl SignInBody {
    fn new(ask: Rc<Ask>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let email = cx.new(|cx| InputState::new(window, cx).placeholder("admin@example.com"));
        let password = cx.new(|cx| InputState::new(window, cx).masked(true));
        // The administrator last signed in there, as a start (an email, not
        // a credential).
        let known = crate::licence::email_for(&ask.target.key());
        if let Some(e) = &known {
            email.update(cx, |i, cx| i.set_value(e.clone(), window, cx));
        }
        let first = if known.is_some() { password.clone() } else { email.clone() };
        first.update(cx, |i, cx| i.focus(window, cx));
        let mut me = Self { ask, email, password, busy: false, error: None, _enter: Vec::new() };
        me._enter = crate::ui::enter_moves_on(
            &[me.email.clone(), me.password.clone()],
            window,
            cx,
            |p: &Self, _| vec![p.email.clone(), p.password.clone()],
            |p: &mut Self, window, cx| {
                if p.busy {
                    return None;
                }
                if p.email.read(cx).value().trim().is_empty() {
                    return Some(p.email.clone());
                }
                if p.password.read(cx).value().is_empty() {
                    return Some(p.password.clone());
                }
                p.go(window, cx);
                None
            },
        );
        me
    }

    fn go(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let email = self.email.read(cx).value().trim().to_string();
        let password = self.password.read(cx).value().to_string();
        if email.is_empty() || password.is_empty() {
            self.error = Some("Enter the email and the password.".into());
            cx.notify();
            return;
        }
        // The field forgets it at once; the credentials live only in the
        // background read, which consumes and wipes them.
        self.password.update(cx, |i, cx| i.set_value("", window, cx));
        let creds = Credentials::new(email, password);
        self.busy = true;
        self.error = None;
        let target = self.ask.target.clone();
        cx.spawn_in(window, async move |this, cx| {
            let got = cx.background_executor().spawn(async move { target.check_licence(creds) }).await;
            this.update_in(cx, |p, window, cx| {
                p.busy = false;
                match got {
                    Ok(l) => {
                        crate::modal::close(cx);
                        after_read(p.ask.clone(), Ok(l), window, cx);
                    }
                    // A refused sign-in: said here, to try again.
                    Err(e) if e.contains("refused") || e.contains("second factor") => {
                        p.error = Some(e);
                        cx.notify();
                    }
                    // Signed in, but the licence could not be read.
                    Err(e) => {
                        crate::modal::close(cx);
                        after_read(p.ask.clone(), Err(e), window, cx);
                    }
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}

impl Render for SignInBody {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let field = |label: &'static str, input: AnyElement| v_flex().gap(px(6.)).child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(label)).child(input);
        let busy = self.busy;
        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .gap_1()
                    .items_start()
                    .child(div().flex_1().min_w_0().text_sm().text_color(theme.muted_foreground).child("To check which versions this instance's licence covers, the Platform Manager signs in once as the system administrator."))
                    .child(ui::info(
                        "lic-gate-signin-info".into(),
                        format!(
                            "Used once, never stored. The email and password go to SENTIENT on {} only to read the licence. They are not saved on this computer or on the server.",
                            self.ask.place
                        )
                        .into(),
                    )),
            )
            .child(field("System administrator email", ui::probed_input(Input::new(&self.email).disabled(busy)).into_any_element()))
            .child(field("Password", ui::probed_input(Input::new(&self.password).mask_toggle().disabled(busy)).into_any_element()))
            .when_some(self.error.clone(), |c, e| c.child(ui::outcome(&theme, Tone::Bad, e)))
            .child(
                h_flex()
                    .gap(ui::button_gap())
                    .justify_end()
                    .child(ui::probed(ui::button("lic-gate-cancel").outline().label("Cancel").disabled(busy).on_click(cx.listener(|p, _, window, cx| {
                        crate::modal::close(cx);
                        after_read(p.ask.clone(), Err(CANCELLED.into()), window, cx);
                    }))))
                    .child(ui::probed(ui::button("lic-gate-sign-in").primary().label(if busy { "Signing in" } else { "Sign in" }).disabled(busy).on_click(cx.listener(|p, _, window, cx| p.go(window, cx))))),
            )
    }
}

/// The typed confirmation: red `red`, the version typed to enable a red
/// "Update anyway"; Cancel is the default, focused button. `again`: also
/// "Sign in again".
#[allow(clippy::too_many_arguments)]
fn typed(ask: Rc<Ask>, title: String, lead: Option<String>, red: String, hint: String, again: bool, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| TypedBody::new(ask, lead, red, hint, again, window, cx));
    crate::modal::open(crate::modal::Modal { title: title.into(), body: view.into(), width: 520., icon: Some(Icon::new(IconName::TriangleAlert)), on_close: None }, cx);
}

struct TypedBody {
    ask: Rc<Ask>,
    lead: Option<String>,
    red: String,
    hint: String,
    again: bool,
    input: Entity<InputState>,
    /// Cancel's focus: the default button.
    cancel_focus: FocusHandle,
    focused: bool,
    _s: Subscription,
}

impl TypedBody {
    fn new(ask: Rc<Ask>, lead: Option<String>, red: String, hint: String, again: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(ask.version.clone()));
        let _s = cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify());
        #[cfg(debug_assertions)]
        if let Ok(t) = std::env::var("SPM_DEBUG_LICENCE_TYPED") {
            input.update(cx, |s, cx| s.set_value(t, window, cx));
        }
        Self { ask, lead, red, hint, again, input, cancel_focus: cx.focus_handle(), focused: false, _s }
    }

    fn ok(&self, cx: &App) -> bool {
        licence_version::typed_matches(&self.input.read(cx).value(), &self.ask.version)
    }
}

impl Render for TypedBody {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        // Cancel is the default: focused when the dialog opens.
        if !self.focused {
            self.focused = true;
            window.focus(&self.cancel_focus, cx);
        }
        let ok = self.ok(cx);
        let version = self.ask.version.clone();
        let cancel_focused = self.cancel_focus.is_focused(window);
        let anyway: AnyElement = if ok {
            ui::solid_button("lic-gate-anyway", "Update anyway".into(), theme.danger, gpui_kit::white(), cx.listener(|p, _, window, cx| {
                crate::modal::close(cx);
                (p.ask.anyway)(window, cx);
            }))
            .into_any_element()
        } else {
            ui::button("lic-gate-anyway").danger().label("Update anyway").disabled(true).into_any_element()
        };
        let cancel = div()
            .id("lic-gate-typed-cancel-focus")
            .relative()
            .flex_shrink_0()
            .track_focus(&self.cancel_focus)
            // Enter or Space on the default button cancels.
            .on_key_down(cx.listener(|_, e: &KeyDownEvent, _, cx| {
                if matches!(e.keystroke.key.as_str(), "enter" | "space") {
                    crate::modal::close(cx);
                }
            }))
            .child(ui::button("lic-gate-typed-cancel").outline().label("Cancel").on_click(|_, _, cx| crate::modal::close(cx)))
            // The focus ring, drawn around the button without moving it.
            .when(cancel_focused, |d| {
                d.child(div().absolute().top(px(-3.)).left(px(-3.)).right(px(-3.)).bottom(px(-3.)).rounded(px(crate::theme::BUTTON_RADIUS + 3.)).border_2().border_color(theme.ring))
            })
            .child(ui::layout_probe("button"));
        v_flex()
            .gap_4()
            .when_some(self.lead.clone(), |c, l| c.child(div().text_sm().child(l)))
            .child(div().text_sm().text_color(theme.danger).child(self.red.clone()))
            .when(!self.hint.is_empty(), |c| c.child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(self.hint.clone())))
            .child(
                v_flex()
                    .gap(px(6.))
                    .child(h_flex().flex_wrap().gap_1().text_sm().child("Type").child(div().font_weight(FontWeight::SEMIBOLD).child(version)).child("to confirm"))
                    .child(ui::probed_input(Input::new(&self.input))),
            )
            .child(
                h_flex()
                    .gap(ui::button_gap())
                    .justify_end()
                    .child(cancel)
                    .when(self.again, |r| {
                        r.child(ui::probed(ui::button("lic-gate-again").outline().label("Sign in again").on_click(cx.listener(|p, _, window, cx| {
                            crate::modal::close(cx);
                            sign_in(p.ask.clone(), window, cx);
                        }))))
                    })
                    .child(ui::probed(anyway)),
            )
    }
}
