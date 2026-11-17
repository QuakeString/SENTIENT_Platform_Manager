// Copyright © 2016-2026 The SENTIENT Authors

//! SENTIENT's licence, through SENTIENT's own licence functions (its REST
//! API, `/api/license/*`, the same calls its Licence page makes). Every call
//! is made as the SENTIENT system administrator: the page signs in with
//! SENTIENT's normal sign-in (`/api/auth/login`, lockout included). The
//! refresh token, and with "Remember me" the email and password, are kept
//! per instance in the OS secret store (a 0600 file where Linux has no
//! keyring), so a restart signs in again without asking.
//!
//! On this computer the calls go to SENTIENT's web port on 127.0.0.1; on a
//! remote instance the server's agent makes them there (`--job licence-api`,
//! the request in a file only its owner reads, removed once read), so
//! nothing but SSH has to reach the server.
//!
//! The instance tile's one-word state ([`TileLicence`]) is read from
//! SENTIENT's database instead, read-only and without the sign-in.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::installation::InstallationRecord;

/// Where SENTIENT's licence portal takes a demo request and a purchase
/// (SENTIENT's Licence page opens the same two).
pub const DEMO_URL: &str = "https://license.invenia.in/request-demo";
pub const BUY_URL: &str = "https://license.invenia.in/buy";

/// A session ends after this long without a call.

// ---------------------------------------------------------------------------
// What SENTIENT answers (camelCase, as its UI reads it)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Customer {
    #[serde(default)]
    pub company: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Integrity {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub release_label: Option<String>,
}

/// `GET /api/license/status`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    #[serde(default)]
    pub activated: bool,
    #[serde(default)]
    pub valid: bool,
    pub license_key: Option<String>,
    pub license_type: Option<String>,
    pub customer_details: Option<Customer>,
    /// Every limit, as SENTIENT names them (0 unlimited, -1 not allowed).
    #[serde(default)]
    pub limits: Option<serde_json::Map<String, Value>>,
    pub expiration_date: Option<String>,
    pub days_remaining: Option<i64>,
    pub binary_integrity: Option<Integrity>,
    pub binary_version: Option<String>,
    pub license_allowed_versions: Option<Vec<String>>,
    pub binding_block_reason: Option<String>,
}

/// `GET /api/license/usage`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    #[serde(default)]
    pub usage: serde_json::Map<String, Value>,
    #[serde(default)]
    pub over_limit: Vec<String>,
}

/// `GET /api/license/state` (the grace period).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct State {
    #[serde(default)]
    pub is_over_limit: bool,
    #[serde(default)]
    pub over_limit_entities: Vec<String>,
    pub days_over_limit: Option<i64>,
    pub days_until_blocked: Option<i64>,
}

/// Everything the page shows, read in one go.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Reading {
    pub status: Status,
    pub usage: Option<Usage>,
    pub state: Option<State>,
}

/// The four-character tier in the card's key (SENTIENT's tierAbbrev).
pub fn tier_abbrev(t: Option<&str>) -> &'static str {
    match t.unwrap_or("").to_ascii_uppercase().as_str() {
        "DEMO" => "DEMO",
        "BASIC" => "BASC",
        "ADVANCE" => "ADV ",
        "PROFESSIONAL" => "PROF",
        "ENTERPRISE" => "ENTR",
        _ => "····",
    }
}

/// The tier's name on the card ("Professional").
pub fn tier_label(t: Option<&str>) -> String {
    let t = t.unwrap_or("");
    let mut c = t.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
        None => "Licence".into(),
    }
}

/// The card's key, as SENTIENT shows it: "INVE  PROF  9F2C  ····  B71F".
pub fn card_key(key: Option<&str>, tier: Option<&str>) -> String {
    let abbrev = tier_abbrev(tier);
    let Some(key) = key else { return format!("INVE  {abbrev}  ····  ····  ····") };
    let clean: String = key.chars().filter(|c| !matches!(c, '-' | '.' | '·' | '…' | ' ')).collect();
    if clean.chars().count() < 4 {
        return key.to_string();
    }
    let first: String = clean.chars().take(4).collect::<String>().to_uppercase();
    let last: String = clean.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect::<String>().to_uppercase();
    format!("INVE  {abbrev}  {first}  ····  {last}")
}

/// "06 / 2027", or PERPETUAL for an enterprise licence without an end.
pub fn valid_through(date: Option<&str>, tier: Option<&str>) -> String {
    let parsed = date.and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok());
    let enterprise = tier.is_some_and(|t| t.eq_ignore_ascii_case("ENTERPRISE"));
    match parsed {
        Some(d) if enterprise && chrono::Datelike::year(&d) > 2099 => "PERPETUAL".into(),
        None if enterprise => "PERPETUAL".into(),
        Some(d) => d.format("%m / %Y").to_string(),
        None => "— —".into(),
    }
}

/// "30 Jun 2027".
pub fn day_label(date: Option<&str>) -> Option<String> {
    date.and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok()).map(|d| d.format("%-d %b %Y").to_string())
}

/// A number with thousands separators.
pub fn grouped(n: i64) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// The page's limits: label, the limit's key, the usage's key.
pub const MAIN_LIMITS: [(&str, &str, &str); 5] = [
    ("Devices", "maxDevices", "devices"),
    ("Users", "maxUsers", "users"),
    ("Dashboards", "maxDashboards", "dashboards"),
    ("Telemetry keys", "maxTelemetryKeys", "telemetryKeys"),
    ("Rule chains", "maxRuleChains", "ruleChains"),
];

/// Every limit SENTIENT has, in its Licence page's words and order.
pub const ALL_LIMITS: &[(&str, &str, Option<&str>)] = &[
    ("Tenants", "maxTenants", Some("tenants")),
    ("Users", "maxUsers", Some("users")),
    ("Devices", "maxDevices", Some("devices")),
    ("Assets", "maxAssets", Some("assets")),
    ("Customers", "maxCustomers", Some("customers")),
    ("Edges", "maxEdges", Some("edges")),
    ("Dashboards", "maxDashboards", Some("dashboards")),
    ("Report templates", "maxReportTemplates", Some("reportTemplates")),
    ("Report schedules", "maxReportSchedules", Some("reportSchedules")),
    ("Rule chains", "maxRuleChains", Some("ruleChains")),
    ("Integrations", "maxIntegrations", Some("integrations")),
    ("Telemetry keys", "maxTelemetryKeys", Some("telemetryKeys")),
    ("Attribute keys", "maxAttributeKeys", Some("attributeKeys")),
    ("Calculated fields", "maxCalculatedFields", None),
    ("Transport messages a month", "transportMsgLimit", None),
    ("Transport data points a month", "transportDataPointsLimit", None),
    ("Stored data points a month", "storageDataPointsLimit", None),
    ("Rule engine runs a month", "ruleEngineExecutionLimit", None),
    ("JavaScript runs a month", "jsExecutionLimit", None),
    ("TBEL runs a month", "tbelExecutionLimit", None),
    ("Emails a month", "emailLimit", None),
    ("SMS a month", "smsLimit", None),
    ("Alarms created a month", "createdAlarmsLimit", None),
    ("Data point storage days", "maxDpStorageDays", None),
    ("WebSocket sessions per tenant", "maxWsSessionsPerTenant", None),
    ("WebSocket sessions per customer", "maxWsSessionsPerCustomer", None),
    ("WebSocket sessions per user", "maxWsSessionsPerRegularUser", None),
    ("WebSocket sessions per public user", "maxWsSessionsPerPublicUser", None),
    ("WebSocket subscriptions per tenant", "maxWsSubscriptionsPerTenant", None),
    ("WebSocket subscriptions per customer", "maxWsSubscriptionsPerCustomer", None),
    ("WebSocket subscriptions per user", "maxWsSubscriptionsPerRegularUser", None),
    ("WebSocket subscriptions per public user", "maxWsSubscriptionsPerPublicUser", None),
    ("WebSocket queue per session", "wsMsgQueueLimitPerSession", None),
    ("Telemetry kept (days)", "defaultStorageTtlDays", None),
    ("Alarms kept (days)", "alarmsTtlDays", None),
    ("RPC kept (days)", "rpcTtlDays", None),
    ("Queue statistics kept (days)", "queueStatsTtlDays", None),
    ("Rule engine errors kept (days)", "ruleEngineExceptionsTtlDays", None),
    ("Resources", "maxResourcesInBytes", None),
    ("OTA packages", "maxOtaPackagesInBytes", None),
    ("Largest resource", "maxResourceSize", None),
    ("Transport", "transportEnabled", None),
    ("Database storage", "dbStorageEnabled", None),
    ("Rule engine", "ruleEngineEnabled", None),
    ("JavaScript", "jsExecutionEnabled", None),
    ("TBEL", "tbelExecutionEnabled", None),
    ("Email", "emailEnabled", None),
    ("SMS", "smsEnabled", None),
    ("Alarms", "alarmEnabled", None),
];

/// A limit's value in words (SENTIENT's convention: 0 unlimited, below 0
/// not allowed).
pub fn limit_text(key: &str, v: &Value) -> String {
    match v {
        Value::Bool(b) => if *b { "On" } else { "Off" }.into(),
        Value::Number(n) => {
            let n = n.as_i64().unwrap_or(0);
            if n == 0 {
                "Unlimited".into()
            } else if n < 0 {
                "Not allowed".into()
            } else if key.ends_with("InBytes") || key == "maxResourceSize" {
                crate::dbstatus::bytes_label(n as u64)
            } else {
                grouped(n)
            }
        }
        Value::Null => "Not set".into(),
        other => other.to_string(),
    }
}

/// The usage key's plain name for the over-limit banner ("ruleChains" → "rule chain").
pub fn entity_noun(key: &str) -> String {
    MAIN_LIMITS
        .iter()
        .map(|(l, _, u)| (*l, *u))
        .chain(ALL_LIMITS.iter().filter_map(|(l, _, u)| u.map(|u| (*l, u))))
        .find(|(_, u)| *u == key)
        .map(|(l, _)| l.trim_end_matches('s').to_lowercase())
        .unwrap_or_else(|| key.to_string())
}

// ---------------------------------------------------------------------------
// One call to SENTIENT's REST API
// ---------------------------------------------------------------------------

/// A call, as the window hands it to the agent on a server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub body: Option<Value>,
    /// A key file for `/api/license/import` (name, base64 bytes).
    #[serde(default)]
    pub file: Option<(String, String)>,
}

/// SENTIENT's answer: the HTTP status and the body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reply {
    pub status: u16,
    pub body: String,
}

impl Reply {
    fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, String> {
        serde_json::from_str(&self.body).map_err(|e| format!("SENTIENT gave an answer the Platform Manager does not understand: {e}"))
    }

    /// SENTIENT's own message, or the status in words.
    fn message(&self) -> String {
        let msg = serde_json::from_str::<Value>(&self.body).ok().and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_string));
        msg.filter(|m| !m.trim().is_empty()).unwrap_or_else(|| match self.status {
            401 => "SENTIENT refused the sign-in.".into(),
            403 => "SENTIENT refused: this needs the system administrator.".into(),
            404 => "This SENTIENT has no licence functions (an older version).".into(),
            s => format!("SENTIENT answered with an error ({s})."),
        })
    }
}

/// The session ended (SENTIENT answered 401 to a call with a token).
pub const SESSION_ENDED: &str = "The sign-in has ended. Sign in again.";

/// Make the call against SENTIENT on `port` of this machine. Blocking.
pub fn call(port: u16, req: &Request) -> Result<Reply, String> {
    let url = format!("http://127.0.0.1:{port}{}", req.path);
    let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(5)).timeout(Duration::from_secs(90)).build();
    let mut r = agent.request(&req.method, &url).set("Accept", "application/json");
    if let Some(t) = &req.token {
        r = r.set("X-Authorization", &format!("Bearer {t}"));
    }
    let sent = if let Some((name, b64)) = &req.file {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| e.to_string())?;
        let boundary = format!("----spm{}", chrono::Utc::now().timestamp_millis());
        let mut body = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\nContent-Type: application/octet-stream\r\n\r\n", name.replace('"', "")).into_bytes();
        body.extend_from_slice(&bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        r.set("Content-Type", &format!("multipart/form-data; boundary={boundary}")).send_bytes(&body)
    } else if let Some(b) = &req.body {
        r.set("Content-Type", "application/json").send_string(&b.to_string())
    } else if req.method == "POST" {
        r.set("Content-Type", "application/json").send_string("{}")
    } else {
        r.call()
    };
    match sent {
        Ok(resp) => Ok(Reply { status: resp.status(), body: resp.into_string().unwrap_or_default() }),
        Err(ureq::Error::Status(code, resp)) => Ok(Reply { status: code, body: resp.into_string().unwrap_or_default() }),
        Err(ureq::Error::Transport(_)) => Err(format!("SENTIENT did not answer on port {port}. Is it running?")),
    }
}

/// The agent's job on a server: read the request from `file` (and remove
/// it), make the call there, print the reply as one JSON line.
pub fn agent_job(file: &str) -> i32 {
    let text = std::fs::read_to_string(file);
    let _ = std::fs::remove_file(file);
    agent_job_text(text.map_err(|e| e.to_string()))
}

/// [`agent_job`] with the request given inline (`--request-b64`): one ssh
/// call per request instead of four (no request file to write and remove).
pub fn agent_job_b64(b64: &str) -> i32 {
    use base64::Engine as _;
    let text = base64::engine::general_purpose::STANDARD.decode(b64.trim()).map_err(|e| e.to_string()).and_then(|b| String::from_utf8(b).map_err(|e| e.to_string()));
    agent_job_text(text)
}

fn agent_job_text(text: Result<String, String>) -> i32 {
    let req: Request = match text.and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string())) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("The licence request could not be read: {e}");
            return 1;
        }
    };
    let Some(record) = InstallationRecord::load() else {
        eprintln!("No SENTIENT is added on this server.");
        return 1;
    };
    match call(record.stack.http_port, &req) {
        Ok(reply) => {
            println!("{}", serde_json::to_string(&reply).unwrap_or_default());
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

// ---------------------------------------------------------------------------
// The licence functions, for this computer or a server
// ---------------------------------------------------------------------------

/// Which SENTIENT.
#[derive(Clone)]
pub enum Target {
    Local(Option<InstallationRecord>),
    Remote(crate::remote::Instance),
}

impl Target {
    /// The session's key.
    pub fn key(&self) -> String {
        match self {
            Target::Local(_) => "@local".into(),
            Target::Remote(i) => i.id.clone(),
        }
    }

    fn send(&self, req: &Request) -> Result<Reply, String> {
        #[cfg(debug_assertions)]
        if let Some(r) = debug::answer(req) {
            return r;
        }
        match self {
            Target::Local(Some(r)) => call(r.stack.http_port, req),
            Target::Local(None) => Err("SENTIENT isn't installed on this computer.".into()),
            Target::Remote(inst) => crate::remote::licence_api(inst, req),
        }
    }

    /// Whether SENTIENT is installed and serving there. Blocking.
    pub fn serving(&self) -> Result<(bool, bool), String> {
        #[cfg(debug_assertions)]
        if let Some(c) = debug::canned() {
            debug::slow("check");
            return Ok((true, c.get("running").and_then(Value::as_bool).unwrap_or(true)));
        }
        match self {
            Target::Local(None) => Ok((false, false)),
            Target::Local(Some(r)) => {
                let snap = r.platform().snapshot();
                Ok((snap.installed, snap.serving))
            }
            Target::Remote(inst) => crate::remote::status(inst).map(|s| (s.installed, s.serving)),
        }
    }

    /// Sign in as `email`: SENTIENT's token and refresh token. Blocking.
    pub fn sign_in(&self, email: &str, password: &str) -> Result<(String, Option<String>), String> {
        let reply = self.send(&Request {
            method: "POST".into(),
            path: "/api/auth/login".into(),
            token: None,
            body: Some(serde_json::json!({ "username": email, "password": password })),
            file: None,
        })?;
        if reply.status == 401 {
            let m = reply.message();
            return Err(if m == "Invalid credentials" { "SENTIENT refused this email and password.".into() } else { m });
        }
        if !(200..300).contains(&reply.status) {
            return Err(reply.message());
        }
        let v: Value = reply.json()?;
        if v.get("scope").and_then(Value::as_str).is_some_and(|s| !s.is_empty()) {
            return Err("This administrator signs in with a second factor (2FA), which the Platform Manager cannot do. Use another system administrator, or SENTIENT's own Licence page.".into());
        }
        let token = v.get("token").and_then(Value::as_str).map(str::to_string).ok_or_else(|| "SENTIENT gave no sign-in token.".to_string())?;
        Ok((token, v.get("refreshToken").and_then(Value::as_str).map(str::to_string)))
    }

    /// A new token from a refresh token (SENTIENT's /api/auth/token).
    /// Blocking.
    pub fn renew(&self, refresh: &str) -> Result<(String, Option<String>), String> {
        let reply = self.send(&Request {
            method: "POST".into(),
            path: "/api/auth/token".into(),
            token: None,
            body: Some(serde_json::json!({ "refreshToken": refresh })),
            file: None,
        })?;
        if !(200..300).contains(&reply.status) {
            return Err(reply.message());
        }
        let v: Value = reply.json()?;
        let token = v.get("token").and_then(Value::as_str).map(str::to_string).ok_or_else(|| "SENTIENT gave no token.".to_string())?;
        Ok((token, v.get("refreshToken").and_then(Value::as_str).map(str::to_string)))
    }

    /// Sign in without asking, from what is kept for this instance: the
    /// refresh token first, then (Remember me) the email and password.
    /// The session is set; the reason when neither works. Blocking.
    pub fn silent_sign_in(&self) -> Result<Session, String> {
        let key = self.key();
        let Some(mut kept) = stored(&key) else { return Err(String::new()) };
        if let Some(r) = kept.refresh.clone() {
            if let Ok((token, refresh)) = self.renew(&r) {
                if refresh.is_some() {
                    kept.refresh = refresh;
                    store(&key, &kept);
                }
                set_session(&key, &kept.email, &token);
                return peek_session(&key).ok_or_else(String::new);
            }
        }
        match kept.password.clone() {
            Some(pw) => match self.sign_in(&kept.email, &pw) {
                Ok((token, refresh)) => {
                    kept.refresh = refresh;
                    store(&key, &kept);
                    set_session(&key, &kept.email, &token);
                    peek_session(&key).ok_or_else(String::new)
                }
                Err(e) => Err(format!("The saved password was refused: {e}")),
            },
            None => Err("Your sign-in has expired. Sign in again.".into()),
        }
    }

    fn get<T: serde::de::DeserializeOwned>(&self, token: &str, method: &str, path: &str, body: Option<Value>) -> Result<T, String> {
        let reply = self.send(&Request { method: method.into(), path: path.into(), token: Some(token.into()), body, file: None })?;
        Self::answer(reply)
    }

    fn answer<T: serde::de::DeserializeOwned>(reply: Reply) -> Result<T, String> {
        match reply.status {
            200..=299 => reply.json(),
            401 => Err(SESSION_ENDED.into()),
            _ => Err(reply.message()),
        }
    }

    /// Status, usage and the grace period. Blocking.
    pub fn read(&self, token: &str) -> Result<Reading, String> {
        #[cfg(debug_assertions)]
        debug::slow("read");
        // The three at once: on a server each is an ssh round trip.
        let (status, usage, state) = std::thread::scope(|sc| {
            let u = sc.spawn(|| self.get::<Usage>(token, "GET", "/api/license/usage", None).ok());
            let st = sc.spawn(|| self.get::<State>(token, "GET", "/api/license/state", None).ok());
            let status = self.get::<Status>(token, "GET", "/api/license/status", None);
            (status, u.join().ok().flatten(), st.join().ok().flatten())
        });
        Ok(Reading { status: status?, usage, state })
    }

    /// Refresh: SENTIENT asks the licence server again. Blocking.
    pub fn refresh(&self, token: &str) -> Result<(), String> {
        self.get::<Value>(token, "POST", "/api/license/refresh", None).map(|_| ())
    }

    pub fn activate(&self, token: &str, key: &str) -> Result<(), String> {
        self.get::<Value>(token, "POST", "/api/license/activate", Some(serde_json::json!({ "licenseKey": key }))).map(|_| ())
    }

    pub fn import(&self, token: &str, name: &str, bytes: &[u8]) -> Result<(), String> {
        use base64::Engine as _;
        let reply = self.send(&Request {
            method: "POST".into(),
            path: "/api/license/import".into(),
            token: Some(token.into()),
            body: None,
            file: Some((name.to_string(), base64::engine::general_purpose::STANDARD.encode(bytes))),
        })?;
        Self::answer::<Value>(reply).map(|_| ())
    }

    /// Deactivate (`force`: delete locally without the licence server).
    pub fn remove(&self, token: &str, force: bool) -> Result<String, String> {
        let v: Value = self.get(token, "DELETE", if force { "/api/license/force-delete" } else { "/api/license/deactivate" }, None)?;
        let msg = v.get("message").and_then(Value::as_str).unwrap_or("").to_string();
        if v.get("success").and_then(Value::as_bool) == Some(false) {
            return Err(if msg.is_empty() { "SENTIENT had no active licence to remove.".into() } else { msg });
        }
        Ok(msg)
    }
}

// ---------------------------------------------------------------------------
// The session, in memory only
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Session {
    pub email: String,
    pub token: String,
    pub last: Instant,
}

static SESSIONS: Mutex<Option<HashMap<String, Session>>> = Mutex::new(None);

/// The session for `key` (kept until Sign out or the app quits; the
/// next start signs in again from what is kept); a call marks it used.
pub fn session(key: &str) -> Option<Session> {
    let mut g = SESSIONS.lock().ok()?;
    let map = g.get_or_insert_with(HashMap::new);
    match map.get_mut(key) {
        Some(s) => {
            s.last = Instant::now();
            Some(s.clone())
        }
        None => None,
    }
}

/// The session for `key` without marking it used (drawing the page).
pub fn peek_session(key: &str) -> Option<Session> {
    let g = SESSIONS.lock().ok()?;
    g.as_ref()?.get(key).cloned()
}

/// The administrator last signed in for `key` (in memory), to fill in.
static EMAILS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

pub fn last_email(key: &str) -> Option<String> {
    EMAILS.lock().ok()?.as_ref()?.get(key).cloned()
}

pub fn set_session(key: &str, email: &str, token: &str) {
    if let Ok(mut g) = EMAILS.lock() {
        g.get_or_insert_with(HashMap::new).insert(key.to_string(), email.to_string());
    }
    if let Ok(mut g) = SESSIONS.lock() {
        g.get_or_insert_with(HashMap::new).insert(key.to_string(), Session { email: email.into(), token: token.into(), last: Instant::now() });
    }
}

/// What is kept for an instance's sign-in, in the OS secret store (the
/// keyring on Linux, DPAPI on Windows; crate::remote::set_secret): the
/// email, the refresh token, and with Remember me the password. Never in
/// session.json, logs or backups.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Stored {
    pub email: String,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub refresh: Option<String>,
}

fn stored_name(key: &str) -> String {
    format!("licence-{key}")
}

pub fn stored(key: &str) -> Option<Stored> {
    let text = crate::remote::secret(&stored_name(key)).or_else(|| file_store::read(&stored_name(key)))?;
    serde_json::from_str(&text).ok()
}

pub fn store(key: &str, kept: &Stored) {
    let Ok(t) = serde_json::to_string(kept) else { return };
    let name = stored_name(key);
    crate::remote::set_secret(&name, &t);
    // Linux without a keyring (no secret-tool, or no Secret Service): the
    // keyring call keeps it for this session only; a file only this account
    // can read keeps it across restarts.
    if cfg!(not(windows)) && !file_store::keyring_has(&name) {
        file_store::write(&name, &t);
    }
}

pub fn forget_stored(key: &str) {
    let name = stored_name(key);
    crate::remote::forget_named(&name);
    file_store::remove(&name);
}

/// The fallback for a Linux desktop without a keyring: one file per name,
/// mode 0600 in a 0700 folder of the Platform Manager's settings.
mod file_store {
    use std::path::PathBuf;

    fn dir() -> PathBuf {
        crate::updates::prefs_dir().join("secrets")
    }

    fn path(name: &str) -> PathBuf {
        dir().join(format!("{}.json", name.replace(['/', '\\', '.'], "_")))
    }

    pub fn keyring_has(name: &str) -> bool {
        #[cfg(not(windows))]
        {
            sentient_installer_core::host::host_command("secret-tool")
                .args(["lookup", "service", "sentient-platform-manager", "instance", name])
                .output()
                .is_ok_and(|o| o.status.success() && !o.stdout.is_empty())
        }
        #[cfg(windows)]
        {
            let _ = name;
            true
        }
    }

    pub fn write(name: &str, text: &str) {
        let d = dir();
        if std::fs::create_dir_all(&d).is_err() {
            return;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
            let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
            let p = path(name);
            let _ = std::fs::remove_file(&p);
            if let Ok(mut f) = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&p) {
                use std::io::Write as _;
                let _ = f.write_all(text.as_bytes());
            }
        }
        #[cfg(not(unix))]
        {
            let _ = text;
        }
    }

    pub fn read(name: &str) -> Option<String> {
        std::fs::read_to_string(path(name)).ok()
    }

    pub fn remove(name: &str) {
        let _ = std::fs::remove_file(path(name));
    }
}

/// The email to fill in: this session's, else the kept one.
pub fn email_for(key: &str) -> Option<String> {
    last_email(key).or_else(|| stored(key).map(|k| k.email).filter(|e| !e.is_empty()))
}

pub fn end_session(key: &str) {
    if let Ok(mut g) = SESSIONS.lock() {
        if let Some(m) = g.as_mut() {
            m.remove(key);
        }
    }
}

// ---------------------------------------------------------------------------
// The instance tile: read-only from the database
// ---------------------------------------------------------------------------

/// The tile's one-word licence state.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "days", rename_all = "snake_case")]
pub enum TileLicence {
    Active,
    /// Ends within 30 days (SENTIENT's warning threshold).
    EndsIn(i64),
    Expired,
    OverLimit,
    None,
}

impl TileLicence {
    pub fn label(self) -> String {
        match self {
            TileLicence::Active => "Active".into(),
            TileLicence::EndsIn(0) => "Ends today".into(),
            TileLicence::EndsIn(1) => "Ends in 1 day".into(),
            TileLicence::EndsIn(d) => format!("Ends in {d} days"),
            TileLicence::Expired => "Expired".into(),
            TileLicence::OverLimit => "Over a limit".into(),
            TileLicence::None => "None".into(),
        }
    }

    /// From the stored row: (licence type, expiry ms) of the active licence,
    /// and whether the grace timer runs.
    pub fn from_row(active: Option<(String, Option<i64>)>, over_limit: bool, now_ms: i64) -> TileLicence {
        let Some((_, expires)) = active else { return TileLicence::None };
        if let Some(e) = expires {
            if e <= now_ms {
                return TileLicence::Expired;
            }
            if over_limit {
                return TileLicence::OverLimit;
            }
            let days = (e - now_ms) / 86_400_000;
            if days <= 30 {
                return TileLicence::EndsIn(days);
            }
        } else if over_limit {
            return TileLicence::OverLimit;
        }
        TileLicence::Active
    }
}

/// The SQL: plain columns SENTIENT keeps unencrypted beside the licence,
/// read in a read-only transaction. Tables an older SENTIENT lacks read as
/// "no licence".
const TILE_SQL: &str = "SELECT json_build_object(\
 'has_license', to_regclass('public.license') IS NOT NULL,\
 'has_state', to_regclass('public.license_state') IS NOT NULL)::text";
const TILE_LICENCE_SQL: &str = "SELECT json_build_object('type', license_type, 'expires', expiration_date)::text FROM license WHERE active = true LIMIT 1";
const TILE_STATE_SQL: &str = "SELECT json_build_object('since', over_limit_since)::text FROM license_state LIMIT 1";

/// Read the tile's state. Blocking; None when the database cannot be read.
pub fn read_tile(record: &InstallationRecord) -> Option<TileLicence> {
    let password = crate::dbstatus::db_password(record).ok()?;
    let st = &record.stack;
    let conn = sentient_backup_core::db::ConnConfig { host: "127.0.0.1".into(), port: st.pg_port, dbname: st.db_name.clone(), user: st.db_user.clone(), password };
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;
    rt.block_on(async {
        let db = tokio::time::timeout(Duration::from_secs(5), sentient_backup_core::db::DbInspector::connect(&conn)).await.ok()?.ok()?;
        // Read-only: this session cannot write, whatever is run on it.
        db.batch("SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY; SET statement_timeout = 3000").await.ok()?;
        let has: Value = serde_json::from_str(&db.text(TILE_SQL).await.ok()??).ok()?;
        if has.get("has_license").and_then(Value::as_bool) != Some(true) {
            return Some(TileLicence::None);
        }
        let row = db.text(TILE_LICENCE_SQL).await.ok()?.and_then(|t| serde_json::from_str::<Value>(&t).ok());
        let active = row.map(|v| (v.get("type").and_then(Value::as_str).unwrap_or("").to_string(), v.get("expires").and_then(Value::as_i64)));
        let over = if has.get("has_state").and_then(Value::as_bool) == Some(true) {
            db.text(TILE_STATE_SQL).await.ok().flatten().and_then(|t| serde_json::from_str::<Value>(&t).ok()).is_some_and(|v| v.get("since").is_some_and(|s| !s.is_null()))
        } else {
            false
        };
        Some(TileLicence::from_row(active, over, chrono::Utc::now().timestamp_millis()))
    })
}

// ---------------------------------------------------------------------------
// Screens and click tests: canned answers (debug builds only)
// ---------------------------------------------------------------------------

#[cfg(debug_assertions)]
mod debug {
    use super::*;

    /// SPM_DEBUG_LICENCE=<json file>: { "running": bool, "status": {…},
    /// "usage": {…}, "state": {…} } (SENTIENT's answers). Password "wrong"
    /// is refused.
    pub fn canned() -> Option<Value> {
        let p = std::env::var_os("SPM_DEBUG_LICENCE")?;
        serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
    }

    /// SPM_DEBUG_REMOTE_SLOW=<secs> holds the answers back; with
    /// SPM_DEBUG_LICENCE_SLOW=check or read, only that stage.
    pub fn slow(stage: &str) {
        if std::env::var("SPM_DEBUG_LICENCE_SLOW").is_ok_and(|s| s != stage) {
            return;
        }
        if let Some(secs) = std::env::var("SPM_DEBUG_REMOTE_SLOW").ok().and_then(|s| s.parse::<u64>().ok()) {
            std::thread::sleep(Duration::from_secs(secs));
        }
    }

    pub fn answer(req: &Request) -> Option<Result<Reply, String>> {
        let c = canned()?;
        let ok = |v: Option<&Value>| Ok(Reply { status: 200, body: v.cloned().unwrap_or(Value::Null).to_string() });
        Some(match req.path.as_str() {
            "/api/auth/token" => {
                let r = req.body.as_ref().and_then(|b| b.get("refreshToken")).and_then(Value::as_str).unwrap_or("");
                if r == "revoked" {
                    Ok(Reply { status: 401, body: r#"{"message":"Invalid refresh token"}"#.into() })
                } else {
                    Ok(Reply { status: 200, body: r#"{"token":"debug","refreshToken":"debug"}"#.into() })
                }
            }
            "/api/auth/login" => {
                let pw = req.body.as_ref().and_then(|b| b.get("password")).and_then(Value::as_str).unwrap_or("");
                if pw == "wrong" {
                    Ok(Reply { status: 401, body: r#"{"message":"Invalid credentials"}"#.into() })
                } else {
                    Ok(Reply { status: 200, body: r#"{"token":"debug","refreshToken":"debug"}"#.into() })
                }
            }
            "/api/license/status" => ok(c.get("status")),
            "/api/license/usage" => ok(c.get("usage")),
            "/api/license/state" => ok(c.get("state")),
            "/api/license/deactivate" | "/api/license/force-delete" => Ok(Reply { status: 200, body: r#"{"success":true,"message":"License deactivated successfully"}"#.into() }),
            _ => Ok(Reply { status: 200, body: "{}".into() }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_key_is_sentients_five_blocks() {
        assert_eq!(card_key(Some("9f2c...b71f"), Some("PROFESSIONAL")), "INVE  PROF  9F2C  ····  B71F");
        assert_eq!(card_key(None, Some("DEMO")), "INVE  DEMO  ····  ····  ····");
    }

    #[test]
    fn valid_through_and_tier() {
        assert_eq!(valid_through(Some("2027-06-30T00:00:00Z"), Some("PROFESSIONAL")), "06 / 2027");
        assert_eq!(valid_through(None, Some("ENTERPRISE")), "PERPETUAL");
        assert_eq!(tier_label(Some("PROFESSIONAL")), "Professional");
    }

    #[test]
    fn tile_state_from_the_row() {
        let now = 1_000_000_000_000;
        let day = 86_400_000;
        assert_eq!(TileLicence::from_row(None, false, now), TileLicence::None);
        assert_eq!(TileLicence::from_row(Some(("PROFESSIONAL".into(), Some(now + 200 * day))), false, now), TileLicence::Active);
        assert_eq!(TileLicence::from_row(Some(("DEMO".into(), Some(now + 12 * day))), false, now), TileLicence::EndsIn(12));
        assert_eq!(TileLicence::from_row(Some(("DEMO".into(), Some(now - day))), true, now), TileLicence::Expired);
        assert_eq!(TileLicence::from_row(Some(("BASIC".into(), Some(now + 200 * day))), true, now), TileLicence::OverLimit);
        assert_eq!(serde_json::to_string(&TileLicence::EndsIn(3)).unwrap(), r#"{"kind":"ends_in","days":3}"#);
    }

    #[test]
    fn numbers_and_limits() {
        assert_eq!(grouped(1840), "1,840");
        assert_eq!(grouped(500), "500");
        assert_eq!(limit_text("maxDevices", &serde_json::json!(0)), "Unlimited");
        assert_eq!(limit_text("maxDevices", &serde_json::json!(-1)), "Not allowed");
        assert_eq!(entity_noun("ruleChains"), "rule chain");
    }
}
