// Copyright © 2016-2026 The SENTIENT Authors

//! Update in SENTIENT: when Apply on Gateway › Connection connected the
//! gateway, but SENTIENT then sent the connection it keeps for the gateway
//! (its gateway configuration › General) and the gateway switched to it,
//! the job ends with a note and this offer: save the typed connection in
//! SENTIENT instead.
//!
//! It signs in once (SENTIENT's normal sign-in, as the gateway's tenant
//! administrator). The password is never stored; the session token is kept
//! in memory for this run of the Platform Manager only. The gateway's
//! device is found by its access token's SHA-256 (the token never leaves
//! the gateway's machine), its `general_configuration` shared attribute gets
//! the host and port, SENTIENT's value is read back, and then the gateway's
//! own settings, until it uses them. Wrong password, no rights, SENTIENT
//! not answering: said, and nothing is changed.

use std::collections::HashMap;
use std::sync::Mutex;

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::{Value, json};
use sentient_installer_core::gateway_settings::{SENTIENT_CONNECTION_OFFER, SentientConnectionOffer, sha256_hex};

use crate::licence::{Credentials, Request, Target};
use crate::ui::{self, Tone};

/// This run's sign-ins, by SENTIENT (Target::key): the token only.
static SESSIONS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

fn session(key: &str) -> Option<String> {
    SESSIONS.lock().ok()?.as_ref()?.get(key).cloned()
}

fn keep_session(key: &str, token: Option<String>) {
    if let Ok(mut g) = SESSIONS.lock() {
        let m = g.get_or_insert_with(HashMap::new);
        match token {
            Some(t) => {
                m.insert(key.to_string(), t);
            }
            None => {
                m.remove(key);
            }
        }
    }
}

/// The offer a finished job carries (its Progress::Warning), if it is this one.
pub fn offer_of(warning: Option<&(SharedString, Option<String>)>) -> Option<SentientConnectionOffer> {
    let o = warning?.1.as_deref()?.strip_prefix(SENTIENT_CONNECTION_OFFER)?;
    serde_json::from_str(o).ok()
}

/// Where the gateway's own settings are read after the update.
#[derive(Clone)]
pub enum GatewayAt {
    Local,
    Remote(crate::remote::Instance),
}

impl GatewayAt {
    fn host_port(&self) -> Result<(Option<String>, Option<u16>), String> {
        let s = match self {
            GatewayAt::Local => sentient_installer_core::gateway_settings::read(&sentient_installer_core::gateway::status()),
            GatewayAt::Remote(inst) => crate::remote::gateway_settings(inst)?,
        };
        Ok((s.conn.host, s.conn.port))
    }
}

/// The "Update in SENTIENT" button for `offer`.
pub fn button(id: &'static str, target: Target, place: String, gateway: GatewayAt, offer: SentientConnectionOffer) -> AnyElement {
    ui::probed(ui::button(id).outline().label("Update in SENTIENT").on_click(move |_, window, cx| open(target.clone(), place.clone(), gateway.clone(), offer.clone(), window, cx))).into_any_element()
}

pub(crate) fn open(target: Target, place: String, gateway: GatewayAt, offer: SentientConnectionOffer, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| Body::new(target, place.clone(), gateway, offer, window, cx));
    // Screens: SPM_DEBUG_SC_STATE=wrong|norights|unreachable|done shows that
    // ending without signing in.
    #[cfg(debug_assertions)]
    if let Ok(state) = std::env::var("SPM_DEBUG_SC_STATE") {
        view.update(cx, |b, cx| {
            match state.as_str() {
                "wrong" => b.error = Some("SENTIENT refused this email and password. Nothing was changed.".into()),
                "norights" => b.error = Some(Failure::NoRights.words()),
                "unreachable" => b.error = Some(Failure::Unreachable("SENTIENT did not answer on port 8080. Is it running?".into()).words()),
                "done" => b.done = Some(format!("SENTIENT keeps {}:{} for plant-gateway now, and the gateway uses it.", b.offer.typed_host, b.offer.typed_port)),
                _ => {}
            }
            cx.notify();
        });
    }
    crate::modal::open(crate::modal::Modal { title: format!("Update the gateway's connection in SENTIENT on {place}").into(), body: view.into(), width: 520., icon: None, on_close: None }, cx);
}

/// SENTIENT's answer as JSON, or what went wrong in plain words.
fn call(target: &Target, token: &str, method: &str, path: &str, body: Option<Value>) -> Result<Value, Failure> {
    let reply = target
        .api(&Request { method: method.into(), path: path.into(), token: Some(token.to_string()), body, file: None })
        .map_err(Failure::Unreachable)?;
    match reply.status {
        200..=299 => Ok(serde_json::from_str(&reply.body).unwrap_or(Value::Null)),
        401 => Err(Failure::SignedOut),
        403 => Err(Failure::NoRights),
        _ => Err(Failure::Other(reply.message())),
    }
}

#[derive(Debug, PartialEq)]
enum Failure {
    Unreachable(String),
    SignedOut,
    NoRights,
    Other(String),
}

impl Failure {
    fn words(&self) -> String {
        match self {
            Failure::Unreachable(e) => format!("{e} Nothing was changed."),
            Failure::SignedOut => "The sign-in has ended. Sign in again; nothing was changed.".into(),
            Failure::NoRights => "This sign-in may not change this gateway's configuration: sign in as the administrator of the tenant the gateway belongs to. Nothing was changed.".into(),
            Failure::Other(m) => format!("{m} Nothing was changed."),
        }
    }
}

/// The gateway's device: the tenant's gateways, the one whose access token
/// has `token_sha256` (or the only gateway when that is not known).
fn find_gateway(target: &Target, token: &str, token_sha256: Option<&str>) -> Result<(String, String), Failure> {
    let page = call(target, token, "GET", "/api/tenant/devices?pageSize=1000&page=0", None)?;
    let gateways: Vec<(String, String)> = page
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|d| d.pointer("/additionalInfo/gateway").and_then(Value::as_bool) == Some(true))
        .filter_map(|d| Some((d.pointer("/id/id")?.as_str()?.to_string(), d.get("name")?.as_str()?.to_string())))
        .collect();
    match token_sha256 {
        Some(want) => {
            for (id, name) in &gateways {
                let creds = call(target, token, "GET", &format!("/api/device/{id}/credentials"), None)?;
                if creds.get("credentialsId").and_then(Value::as_str).is_some_and(|c| sha256_hex(c) == want) {
                    return Ok((id.clone(), name.clone()));
                }
            }
            Err(Failure::Other("No gateway with this gateway's access token is in this tenant: sign in to the tenant the gateway belongs to.".into()))
        }
        None if gateways.len() == 1 => Ok(gateways[0].clone()),
        None => Err(Failure::Other("This tenant has more than one gateway, and this gateway's access token could not be read to tell which.".into())),
    }
}

/// The general configuration with `host` and `port` set (the rest kept).
pub fn with_connection(general: Option<&Value>, host: &str, port: u16) -> Value {
    let mut g = match general {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or_else(|_| json!({})),
        Some(v @ Value::Object(_)) => v.clone(),
        _ => json!({}),
    };
    if let Some(o) = g.as_object_mut() {
        o.insert("host".into(), json!(host));
        o.insert("port".into(), json!(port));
    }
    g
}

fn general_of(target: &Target, token: &str, id: &str) -> Result<Option<Value>, Failure> {
    let list = call(target, token, "GET", &format!("/api/plugins/telemetry/DEVICE/{id}/values/attributes/SHARED_SCOPE?keys=general_configuration"), None)?;
    Ok(list.as_array().into_iter().flatten().find(|a| a.get("key").and_then(Value::as_str) == Some("general_configuration")).and_then(|a| a.get("value").cloned()))
}

/// Save `offer`'s connection for the gateway in SENTIENT, read it back, and
/// wait for the gateway to use it. Ok: what happened, in words.
fn update(target: &Target, token: &str, gateway: &GatewayAt, offer: &SentientConnectionOffer) -> Result<String, Failure> {
    let (id, name) = find_gateway(target, token, offer.token_sha256.as_deref())?;
    let general = with_connection(general_of(target, token, &id)?.as_ref(), &offer.write_host, offer.typed_port);
    call(target, token, "POST", &format!("/api/plugins/telemetry/DEVICE/{id}/attributes/SHARED_SCOPE"), Some(json!({ "general_configuration": general })))?;
    // Read back from SENTIENT.
    let saved = g_raw(general_of(target, token, &id)?.as_ref());
    let saved_conn = saved.map(|v| (v.get("host").and_then(Value::as_str).map(str::to_string), v.get("port").and_then(Value::as_u64).map(|p| p as u16)));
    if saved_conn != Some((Some(offer.write_host.clone()), Some(offer.typed_port))) {
        return Err(Failure::Other(format!("SENTIENT did not keep {}:{} for {name} (it reads back otherwise).", offer.write_host, offer.typed_port)));
    }
    // The gateway takes it on its own (SENTIENT sends it): its settings show
    // it once it switched.
    let want = |h: &Option<String>, p: &Option<u16>| p == &Some(offer.typed_port) && h.as_deref().is_some_and(|h| h == offer.typed_host || h == offer.write_host);
    for _ in 0..12 {
        std::thread::sleep(std::time::Duration::from_secs(5));
        if let Ok((h, p)) = gateway.host_port() {
            if want(&h, &p) {
                return Ok(format!("SENTIENT keeps {}:{} for {name} now, and the gateway uses it.", offer.typed_host, offer.typed_port));
            }
        }
    }
    Ok(format!("SENTIENT keeps {}:{} for {name} now. The gateway has not switched to it yet: it does when it next hears from SENTIENT, or at its next restart.", offer.typed_host, offer.typed_port))
}

/// The general configuration as an object (SENTIENT may keep it as a JSON string).
fn g_raw(v: Option<&Value>) -> Option<Value> {
    match v? {
        Value::String(s) => serde_json::from_str(s).ok(),
        o @ Value::Object(_) => Some(o.clone()),
        _ => None,
    }
}

struct Body {
    target: Target,
    place: String,
    gateway: GatewayAt,
    offer: SentientConnectionOffer,
    email: Entity<InputState>,
    password: Entity<InputState>,
    signed_in: bool,
    busy: bool,
    error: Option<String>,
    done: Option<String>,
}

impl Body {
    fn new(target: Target, place: String, gateway: GatewayAt, offer: SentientConnectionOffer, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let email = cx.new(|cx| InputState::new(window, cx).placeholder("tenant-admin@example.com"));
        let password = cx.new(|cx| InputState::new(window, cx).masked(true));
        let signed_in = session(&target.key()).is_some();
        if !signed_in {
            email.update(cx, |i, cx| i.focus(window, cx));
        }
        Self { target, place, gateway, offer, email, password, signed_in, busy: false, error: None, done: None }
    }

    fn go(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.target.key();
        let creds = if self.signed_in {
            None
        } else {
            let email = self.email.read(cx).value().trim().to_string();
            let password = self.password.read(cx).value().to_string();
            if email.is_empty() || password.is_empty() {
                self.error = Some("Enter the email and the password.".into());
                cx.notify();
                return;
            }
            // The field forgets it at once.
            self.password.update(cx, |i, cx| i.set_value("", window, cx));
            Some(Credentials::new(email, password))
        };
        self.busy = true;
        self.error = None;
        let (target, gateway, offer) = (self.target.clone(), self.gateway.clone(), self.offer.clone());
        cx.spawn_in(window, async move |this, cx| {
            let got = cx
                .background_executor()
                .spawn(async move {
                    let token = match creds {
                        Some(c) => {
                            let t = target.sign_in_with(c).map_err(|e| (false, format!("{e} Nothing was changed.")))?;
                            keep_session(&target.key(), Some(t.clone()));
                            t
                        }
                        None => session(&target.key()).ok_or((true, Failure::SignedOut.words()))?,
                    };
                    update(&target, &token, &gateway, &offer).map_err(|f| {
                        let out = f == Failure::SignedOut;
                        if out {
                            keep_session(&target.key(), None);
                        }
                        (out, f.words())
                    })
                })
                .await;
            this.update_in(cx, |p, _, cx| {
                p.busy = false;
                match got {
                    Ok(m) => p.done = Some(m),
                    Err((signed_out, e)) => {
                        if signed_out {
                            p.signed_in = false;
                        }
                        p.error = Some(e);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        let _ = key;
        cx.notify();
    }
}

impl Render for Body {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let field = |label: &'static str, input: AnyElement| v_flex().gap(px(6.)).child(div().text_size(px(13.)).text_color(theme.muted_foreground).child(label)).child(input);
        let busy = self.busy;
        let o = &self.offer;
        let what = if o.write_host == o.typed_host {
            format!("SENTIENT keeps {}:{} for this gateway. Save {}:{} there instead, the connection set on this page.", o.sentient_host, o.sentient_port, o.typed_host, o.typed_port)
        } else {
            format!(
                "SENTIENT keeps {}:{} for this gateway. Save {}:{} there instead, the connection set on this page (given to SENTIENT as {}:{}: this gateway's container reaches this machine by that name).",
                o.sentient_host, o.sentient_port, o.typed_host, o.typed_port, o.write_host, o.typed_port
            )
        };
        let done = self.done.clone();
        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .gap_1()
                    .items_start()
                    .child(div().flex_1().min_w_0().text_sm().text_color(theme.muted_foreground).child(what))
                    .child(ui::info(
                        "sc-info".into(),
                        format!("Signs in to SENTIENT on {} as the gateway's tenant administrator. The password is used once and never stored; the sign-in lasts until the Platform Manager closes.", self.place).into(),
                    )),
            )
            .when(!self.signed_in && done.is_none(), |c| {
                c.child(field("Tenant administrator email", ui::probed_input(Input::new(&self.email).disabled(busy)).into_any_element()))
                    .child(field("Password", ui::probed_input(Input::new(&self.password).mask_toggle().disabled(busy)).into_any_element()))
            })
            .when_some(self.error.clone(), |c, e| c.child(ui::outcome(&theme, Tone::Bad, e)))
            .when_some(done.clone(), |c, m| c.child(ui::outcome(&theme, Tone::Good, m)))
            .child(
                h_flex()
                    .gap(ui::button_gap())
                    .justify_end()
                    .when(done.is_some(), |r| r.child(ui::probed(ui::button("sc-close").primary().label("Close").on_click(|_, _, cx| crate::modal::close(cx)))))
                    .when(done.is_none(), |r| {
                        r.child(ui::probed(ui::button("sc-cancel").outline().label("Cancel").disabled(busy).on_click(|_, _, cx| crate::modal::close(cx))))
                            .child(ui::probed(ui::button("sc-update").primary().label(if busy { "Updating" } else if self.signed_in { "Update" } else { "Sign in and update" }).disabled(busy).on_click(cx.listener(|p, _, window, cx| p.go(window, cx)))))
                    }),
            )
    }
}

/// Debug builds: the whole update without the window, against SENTIENT on
/// this computer (SPM_SC_EMAIL, SPM_SC_PASSWORD, SPM_SC_PORT; the offer as
/// JSON in `offer`). Prints what the dialog would say.
#[cfg(debug_assertions)]
pub fn debug_run(offer: &str) -> i32 {
    let offer: SentientConnectionOffer = match serde_json::from_str(offer) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("offer: {e}");
            return 2;
        }
    };
    let port: u16 = std::env::var("SPM_SC_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8080);
    let Some(mut rec) = crate::installation::InstallationRecord::load() else {
        eprintln!("no SENTIENT installed here");
        return 2;
    };
    rec.stack.http_port = port;
    let target = Target::Local(Some(rec));
    let creds = Credentials::new(std::env::var("SPM_SC_EMAIL").unwrap_or_default(), std::env::var("SPM_SC_PASSWORD").unwrap_or_default());
    let got = target.sign_in_with(creds).map_err(|e| format!("{e} Nothing was changed.")).and_then(|t| update(&target, &t, &GatewayAt::Local, &offer).map_err(|f| f.words()));
    match got {
        Ok(m) => {
            println!("OK {m}");
            0
        }
        Err(e) => {
            println!("ERR {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SENTIENT_CONNECTION_OFFER, SentientConnectionOffer, SharedString, Value, offer_of, with_connection};

    #[test]
    fn the_connection_is_set_and_the_rest_kept() {
        let g = serde_json::json!({"host": "192.168.0.20", "port": 1883, "remoteShell": false});
        assert_eq!(with_connection(Some(&g), "127.0.0.1", 1883), serde_json::json!({"host": "127.0.0.1", "port": 1883, "remoteShell": false}));
        let s = Value::String(r#"{"host":"a","port":1,"x":2}"#.into());
        assert_eq!(with_connection(Some(&s), "b", 2), serde_json::json!({"host": "b", "port": 2, "x": 2}));
        assert_eq!(with_connection(None, "h", 3), serde_json::json!({"host": "h", "port": 3}));
    }

    #[test]
    fn the_offer_is_read_from_the_jobs_note() {
        let o = SentientConnectionOffer { typed_host: "127.0.0.1".into(), typed_port: 1883, write_host: "host.docker.internal".into(), sentient_host: "192.168.0.20".into(), sentient_port: 1883, token_sha256: Some("ab".into()) };
        let w: (SharedString, Option<String>) = ("note".into(), Some(format!("{SENTIENT_CONNECTION_OFFER}{}", serde_json::to_string(&o).unwrap())));
        assert_eq!(offer_of(Some(&w)), Some(o));
        assert_eq!(offer_of(Some(&("note".into(), Some("other:{}".into())))), None);
        assert_eq!(offer_of(None), None);
    }
}
