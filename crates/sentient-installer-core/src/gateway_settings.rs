// Copyright © 2016-2026 The SENTIENT Authors

//! The gateway's settings as the Platform Manager shows and changes them
//! (the Gateway page's settings card, here and on a server).
//!
//! Reading:
//!
//! - A gateway that has `--status-json` (its `--help` lists it; capability,
//!   never a failed call) is asked for its state: connected or not, the
//!   page, the connectors. An older one is read from its files: the gateway
//!   file (`st_gateway.json`, else the Python `tb_gateway.json`), `logs.json`
//!   and the connector list in it, plus how its process runs (the unit's
//!   environment, the container's flags, the Windows service's).
//! - Secrets never leave this module: a token or a password is read only to
//!   say "set" or its last four characters ([`hint`]); a test of the
//!   connection reads only where the gateway connects (TCP, and a TLS
//!   handshake when it uses TLS), never its credentials.
//!
//! Who owns what (docs: the gateway control study, §2.2): SENTIENT pushes
//! the connectors, storage, logging levels, statistics, remote shell and
//! rate limits on every connection, so those are shown locked; the
//! connection is SENTIENT's while connected. The machine side is ours: the
//! local page, how the process restarts, serial ports, environment
//! overrides and the log level before SENTIENT connects. [`apply`] changes
//! those through the gateway's own command (`--install --ui …`,
//! `--ui-password -`), a systemd drop-in, the container recreated with the
//! same volumes, or the Windows service's settings, then reads back.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::gateway::Status;
use crate::progress::{Progress, ProgressFn};

/// What SENTIENT's gateway page calls the place to change a locked value.
pub const SENTIENT_PLACE: &str = "SENTIENT › Gateways › this gateway › Configuration";

/// The drop-in the Platform Manager writes for a native gateway's unit
/// (`/etc/systemd/system/<unit>.d/`): never the unit itself.
pub const DROPIN: &str = "50-platform-manager.conf";

/// One shown value: its name and the value in words.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fact {
    pub label: String,
    pub value: String,
}

fn fact(label: &str, value: impl Into<String>) -> Fact {
    Fact { label: label.into(), value: value.into() }
}

/// The connection to SENTIENT, without its secrets.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Connection {
    pub host: Option<String>,
    pub port: Option<u16>,
    /// accessToken, usernamePassword, tlsAccessToken or tlsPrivateKey.
    pub credential: String,
    /// The token or password: "set", or `••••` and its last four characters.
    pub secret_hint: Option<String>,
    pub username: Option<String>,
    pub client_id: Option<String>,
    pub tls: bool,
    /// Host, port or token come from the process's environment (a container
    /// started with `host`/`port`/`accessToken`).
    pub from_env: bool,
    /// Where the host the gateway uses comes from, in words (the config
    /// file and its path, the container's environment and the variable).
    pub host_source: Option<String>,
    /// How a host the person typed is reached when it is not used as
    /// typed (localhost in a container on its own network is reached as
    /// host.docker.internal): a note for under the field.
    pub via: Option<String>,
}

impl Connection {
    pub fn commissioned(&self) -> bool {
        self.host.is_some()
    }

    /// The credential in words, with its hint.
    pub fn credential_words(&self) -> String {
        let kind = match self.credential.as_str() {
            "usernamePassword" => "User name and password",
            "tlsAccessToken" => "Access token over TLS",
            "tlsPrivateKey" => "X.509 certificate",
            _ => "Access token",
        };
        match (&self.secret_hint, self.credential.as_str()) {
            (_, "tlsPrivateKey") => kind.to_string(),
            (Some(h), _) => format!("{kind} {h}"),
            (None, _) => format!("{kind}, not set"),
        }
    }
}

/// The gateway's local web page.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalUi {
    pub enabled: bool,
    pub listen: String,
    pub hostnames: Vec<String>,
    /// The administrator password is set (None: not known).
    pub password_set: Option<bool>,
    /// On, but not reachable as it runs (a container that does not publish
    /// its port): why, for the card.
    pub unreachable: Option<String>,
}

/// A connector as SENTIENT configured it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Connector {
    pub name: String,
    pub kind: String,
    pub enabled: Option<bool>,
    /// What the gateway reports (status-json only): running, failed, ….
    pub state: Option<String>,
    pub devices: Option<u64>,
}

/// An environment variable of the gateway's process.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EnvVar {
    pub name: String,
    /// None: a secret (kept as it is, never shown).
    pub value: Option<String>,
    /// Ours to change (the drop-in, the container's own, the service's).
    pub editable: bool,
    /// Where it comes from, for the read-only ones: the unit file, the image.
    pub origin: String,
}

/// Serial ports for the gateway.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Serial {
    /// "group": a native service, whose account joins the serial group;
    /// "devices": a container, given each chosen device; "": not offered.
    pub mode: String,
    /// The group that may open serial ports here (dialout, uucp on Arch).
    pub group: Option<String>,
    /// The service account; empty for root.
    pub account: Option<String>,
    pub in_group: bool,
    /// A container's devices now.
    pub chosen: Vec<String>,
    /// The serial adapters on this computer (stable names first).
    pub available: Vec<String>,
}

/// How the gateway's process runs: the settings that are ours.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Machine {
    /// docker, service, windows-service or process (Status::kind).
    pub kind: String,
    /// The Platform Manager can change these (None: why not).
    pub not_editable: Option<String>,
    /// Restarted after a failure; the policy in its own words.
    pub restart_always: Option<bool>,
    pub restart_word: Option<String>,
    pub serial: Serial,
    pub env: Vec<EnvVar>,
    /// RUST_LOG as set for the process (None: the gateway's own default).
    pub rust_log: Option<String>,
    /// ST_GW_UI / ST_GW_UI_LISTEN set in the environment (they win over the file).
    pub ui_env: bool,
    /// A container's network (host, bridge, a compose network): on its own
    /// network the web page's port is published; on the host's it is not.
    pub network: Option<String>,
}

impl Machine {
    /// How the gateway runs, in words, for the card: the web page is set up
    /// differently for each.
    pub fn kind_words(&self) -> String {
        match (self.kind.as_str(), self.network.as_deref()) {
            ("docker", Some("host")) => "In Docker, on the host network: the web page listens on this machine directly.".into(),
            ("docker", Some(n)) => format!("In Docker, on the {n} network: the web page's port is published on this machine."),
            ("docker", None) => "In Docker.".into(),
            ("service", _) => "As a systemd service: the web page listens on this machine directly (a firewall may need the port open).".into(),
            ("windows-service", _) => "As a Windows service: the web page listens on this machine directly (Windows Firewall may need the port open).".into(),
            ("process", _) => "As a program started by hand.".into(),
            _ => String::new(),
        }
    }
}

/// What the installed gateway's program can do (from its `--help`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Caps {
    pub status_json: bool,
    pub ui_password: bool,
    pub ui_listen: bool,
    /// `--install --ui-hostname`: extra host names (not in 4.16).
    pub ui_hostname: bool,
}

impl Caps {
    pub fn of_help(help: &str) -> Caps {
        Caps {
            status_json: help.contains("--status-json"),
            ui_password: help.contains("--ui-password"),
            ui_listen: help.contains("--ui-listen"),
            ui_hostname: help.contains("--ui-hostname"),
        }
    }
}

/// The gateway's settings, as shown. Serialised for a root job's and a
/// server agent's answer (`gateway-settings-json`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GwSettings {
    /// status-json, file, or none (nothing could be read: `note` says why).
    pub source: String,
    pub note: Option<String>,
    pub config_path: Option<String>,
    /// Connected to SENTIENT now (None: not known), and how that is known.
    pub connected: Option<bool>,
    pub connected_how: Option<String>,
    pub conn: Connection,
    pub ui: LocalUi,
    pub connectors: Vec<Connector>,
    pub storage: Vec<Fact>,
    pub logging: Vec<Fact>,
    pub log_files: Option<String>,
    pub statistics: Vec<Fact>,
    /// Remote shell, rate limits, payload size: SENTIENT's.
    pub platform: Vec<Fact>,
    pub machine: Machine,
    pub caps: Caps,
}

/// A secret in words: "set", or `••••` and its last four characters when
/// it is long enough that four say little. Empty: None.
pub fn hint(secret: &str) -> Option<String> {
    let s = secret.trim();
    if s.is_empty() {
        return None;
    }
    let n = s.chars().count();
    if n < 12 {
        return Some("set".into());
    }
    Some(format!("••••{}", s.chars().skip(n - 4).collect::<String>()))
}

/// Environment names that carry the connection (they stay as they are).
fn is_connection_env(name: &str) -> bool {
    let n = name.trim_start_matches("ST_GW_").trim_start_matches("TB_GW_").to_ascii_lowercase();
    matches!(n.as_str(), "host" | "port" | "accesstoken" | "access_token" | "type" | "security_type" | "username" | "password" | "clientid" | "client_id" | "cacert" | "ca_cert" | "cert" | "privatekey" | "private_key")
}

/// Names whose values are secrets.
pub fn is_secret_env(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["token", "password", "passwd", "secret", "privatekey", "private_key", "apikey", "api_key"].iter().any(|w| n.contains(w))
}

/// The variables the card edits with their own rows.
const DEDICATED_ENV: [&str; 3] = ["ST_GW_UI", "ST_GW_UI_LISTEN", "RUST_LOG"];

fn env_get<'a>(env: &'a [String], name: &str) -> Option<&'a str> {
    env.iter().find_map(|l| l.strip_prefix(name).and_then(|r| r.strip_prefix('=')))
}

fn env_any<'a>(env: &'a [String], names: &[&str]) -> Option<&'a str> {
    names.iter().find_map(|n| env_get(env, n)).map(str::trim).filter(|v| !v.is_empty())
}

fn words_bool(b: bool) -> &'static str {
    if b { "On" } else { "Off" }
}

fn value_words(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => words_bool(*b).into(),
        Value::Null => "not set".into(),
        other => other.to_string(),
    }
}

/// The settings in a gateway file and `logs.json`, the secrets reduced to
/// hints. `files`: the config folder's file names (connector files).
pub fn from_files(gateway: &str, logs: Option<&str>) -> GwSettings {
    let mut s = GwSettings { source: "file".into(), ..GwSettings::default() };
    let Ok(v) = serde_json::from_str::<Value>(gateway) else {
        s.source = "none".into();
        s.note = Some("The gateway's settings file could not be read as JSON.".into());
        return s;
    };
    read_config(&mut s, &v);
    if let Some(l) = logs.and_then(|t| serde_json::from_str::<Value>(t).ok()) {
        read_logs(&mut s, &l);
    }
    s
}

fn read_config(s: &mut GwSettings, v: &Value) {
    let p = v.get("sentient").or_else(|| v.get("thingsboard")).cloned().unwrap_or(Value::Null);
    let sec = p.get("security").cloned().unwrap_or(Value::Null);
    let str_of = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::trim).filter(|x| !x.is_empty()).map(str::to_string);
    let token = str_of(&sec, "accessToken");
    let password = str_of(&sec, "password");
    let credential = str_of(&sec, "type").unwrap_or_else(|| if token.is_some() || sec.get("username").is_none() { "accessToken".into() } else { "usernamePassword".into() });
    let credential = match credential.to_ascii_lowercase().as_str() {
        "usernamepassword" => "usernamePassword",
        "tlsaccesstoken" => "tlsAccessToken",
        "tlsprivatekey" | "x509certificate" | "x509" => "tlsPrivateKey",
        _ => "accessToken",
    }
    .to_string();
    s.conn = Connection {
        host: str_of(&p, "host"),
        port: p.get("port").and_then(Value::as_u64).map(|x| x as u16),
        secret_hint: if credential == "usernamePassword" { password.as_deref().and_then(hint) } else { token.as_deref().and_then(hint) },
        username: str_of(&sec, "username"),
        client_id: str_of(&sec, "clientId"),
        tls: credential.starts_with("tls") || sec.get("caCert").is_some_and(|c| c.as_str().is_some_and(|c| !c.is_empty())),
        credential,
        from_env: false,
        host_source: None,
        via: None,
    };
    // The page: on by default outside a container (resolved later).
    let ui = v.get("localUi").cloned().unwrap_or(Value::Null);
    s.ui.enabled = ui.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    s.ui.listen = str_of(&ui, "listen").unwrap_or_else(|| format!("127.0.0.1:{}", crate::gateway::UI_PORT));
    s.ui.hostnames = ui.get("hostnames").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).filter(|h| !h.trim().is_empty()).map(str::to_string).collect()).unwrap_or_default();
    // Connectors: the local list SENTIENT mirrored.
    s.connectors = v
        .get("connectors")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|c| Connector {
                    name: str_of(c, "name").unwrap_or_else(|| "unnamed".into()),
                    kind: str_of(c, "type").map(|t| connector_type_words(&t)).unwrap_or_default(),
                    enabled: c.get("enabled").and_then(Value::as_bool),
                    state: None,
                    devices: None,
                })
                .collect()
        })
        .unwrap_or_default();
    // Storage.
    let st = v.get("storage").cloned().unwrap_or(Value::Null);
    let kind = str_of(&st, "type").unwrap_or_else(|| "sqlite".into());
    let mut storage = vec![fact("Type", match kind.as_str() { "memory" => "In memory", "file" => "Files", _ => "SQLite" })];
    match kind.as_str() {
        "memory" => {
            if let Some(n) = st.get("max_records_count").and_then(Value::as_u64) {
                storage.push(fact("Keep at most", format!("{n} messages")));
            }
        }
        _ => {
            if let Some(path) = str_of(&st, "data_file_path").or_else(|| str_of(&st, "data_folder_path")) {
                storage.push(fact("Path", path));
            }
            if let Some(d) = st.get("messages_ttl_in_days").and_then(Value::as_u64) {
                storage.push(fact("Keep messages", if d == 0 { "Until sent".to_string() } else { format!("{d} days") }));
            }
            if let Some(mb) = st.get("size_limit").and_then(Value::as_f64) {
                storage.push(fact("Size limit", format!("{} MB per file", (mb * 1024.).round() as u64)));
            }
            if let Some(n) = st.get("max_db_amount").and_then(Value::as_u64) {
                storage.push(fact("Files", format!("at most {n}")));
            }
        }
    }
    s.storage = storage;
    // Statistics.
    let stat = p.get("statistics").cloned().unwrap_or(Value::Null);
    let on = stat.get("enable").and_then(Value::as_bool).unwrap_or(true);
    s.statistics = vec![
        fact("Statistics", words_bool(on)),
        fact("Every", format!("{} s", stat.get("statsSendPeriodInSeconds").and_then(Value::as_u64).unwrap_or(60))),
        fact("Custom statistics", words_bool(stat.get("enableCustom").and_then(Value::as_bool).unwrap_or(true))),
        fact("Commands", match stat.get("commands").and_then(Value::as_array).map(Vec::len).unwrap_or(0) {
            0 => "none".to_string(),
            n => format!("{n}"),
        }),
    ];
    // Remote features and limits.
    let mut platform = vec![fact("Remote shell", words_bool(p.get("remoteShell").and_then(Value::as_bool).unwrap_or(false)))];
    let limits: Vec<String> = ["rateLimits", "dpRateLimits", "messagesRateLimits"].iter().filter_map(|k| p.get(*k).and_then(Value::as_str)).map(str::to_string).collect();
    platform.push(fact("Rate limits", if limits.is_empty() || limits.iter().all(|l| l.starts_with("DEFAULT_")) { "SENTIENT's defaults".to_string() } else { limits.join(", ") }));
    if let Some(n) = p.get("maxPayloadSizeBytes").and_then(Value::as_u64) {
        platform.push(fact("Max payload", format!("{n} bytes")));
    }
    s.platform = platform;
}

fn connector_type_words(t: &str) -> String {
    match t.to_ascii_lowercase().as_str() {
        "modbus" => "Modbus".into(),
        "opcua" => "OPC UA".into(),
        "mqtt" => "MQTT".into(),
        "bacnet" => "BACnet".into(),
        "s7" | "siemens" => "S7".into(),
        "s7commplus" => "S7CommPlus".into(),
        "snmp" => "SNMP".into(),
        "request" | "rest" => "REST".into(),
        "can" => "CAN".into(),
        "ble" => "BLE".into(),
        "odbc" => "ODBC".into(),
        "ftp" => "FTP".into(),
        "socket" => "Socket".into(),
        "xmpp" => "XMPP".into(),
        "knx" => "KNX".into(),
        _ => t.to_string(),
    }
}

fn read_logs(s: &mut GwSettings, l: &Value) {
    let Some(loggers) = l.get("loggers").and_then(Value::as_object) else { return };
    let mut out = Vec::new();
    for (name, v) in loggers {
        let level = v.get("level").map(value_words).unwrap_or_default();
        let label = match name.as_str() {
            "service" => "Service".to_string(),
            "connector" => "Connectors".to_string(),
            "tb_connection" | "platform" => "Platform connection".to_string(),
            "storage" => "Storage".to_string(),
            "extension" => "Extensions".to_string(),
            "converter" => "Converters".to_string(),
            other => other.to_string(),
        };
        out.push(fact(&label, level));
    }
    if let Some(r) = l.get("remoteLoggingLevel").or_else(|| l.get("RemoteLoggingLevel")) {
        out.push(fact("Remote logging", value_words(r)));
    }
    s.logging = out;
}

/// The gateway's own state (`--status-json`, gateways that have it), over
/// what the files said: whether it is connected, its page and connectors.
/// Shaped leniently: `connected` (or `platform.connected`), `config` (the
/// gateway file's shape, its secrets reduced here), `localUi`
/// (`passwordSet`), `connectors` (`name`, `type`, `enabled`, `state`,
/// `devices`), `logs`.
pub fn merge_status_json(s: &mut GwSettings, text: &str) -> bool {
    let Some(v) = text.lines().rev().find_map(|l| serde_json::from_str::<Value>(l.trim()).ok().filter(Value::is_object)).or_else(|| serde_json::from_str::<Value>(text.trim()).ok()) else {
        return false;
    };
    if let Some(cfg) = v.get("config").filter(|c| c.is_object()) {
        read_config(s, cfg);
    }
    if let Some(l) = v.get("logs").filter(|c| c.is_object()) {
        read_logs(s, l);
    }
    if let Some(c) = v.get("connected").or_else(|| v.get("platform").and_then(|p| p.get("connected"))).and_then(Value::as_bool) {
        s.connected = Some(c);
        s.connected_how = Some("the gateway's own state".into());
    }
    if let Some(ui) = v.get("localUi") {
        if let Some(b) = ui.get("enabled").and_then(Value::as_bool) {
            s.ui.enabled = b;
        }
        if let Some(l) = ui.get("listen").and_then(Value::as_str) {
            s.ui.listen = l.to_string();
        }
        if let Some(b) = ui.get("passwordSet").and_then(Value::as_bool) {
            s.ui.password_set = Some(b);
        }
    }
    if let Some(a) = v.get("connectors").and_then(Value::as_array) {
        let str_of = |c: &Value, k: &str| c.get(k).and_then(Value::as_str).map(str::to_string);
        s.connectors = a
            .iter()
            .map(|c| Connector {
                name: str_of(c, "name").unwrap_or_else(|| "unnamed".into()),
                kind: str_of(c, "type").map(|t| connector_type_words(&t)).unwrap_or_default(),
                enabled: c.get("enabled").and_then(Value::as_bool),
                state: str_of(c, "state"),
                devices: c.get("devices").and_then(Value::as_u64),
            })
            .collect();
    }
    s.source = "status-json".into();
    true
}

/// The environment over the file, as the gateway resolves it: host, port
/// and token from `host`/`port`/`accessToken` (and their ST_GW_/TB_GW_
/// names), the page from ST_GW_UI and ST_GW_UI_LISTEN, RUST_LOG.
pub fn apply_env(s: &mut GwSettings, env: &[String], in_container: bool) {
    if let Some(h) = env_any(env, &["host", "ST_GW_HOST", "TB_GW_HOST"]) {
        s.conn.host = Some(h.to_string());
        s.conn.from_env = true;
        s.conn.host_source = Some(if in_container { "the container's environment".into() } else { "the service's environment".into() });
    }
    if let Some(p) = env_any(env, &["port", "ST_GW_PORT", "TB_GW_PORT"]).and_then(|p| p.parse().ok()) {
        s.conn.port = Some(p);
        s.conn.from_env = true;
    }
    if let Some(t) = env_any(env, &["accessToken", "ST_GW_ACCESS_TOKEN", "TB_GW_ACCESS_TOKEN"]) {
        // The image's placeholder is no token (the file's then counts).
        if !t.eq_ignore_ascii_case("YOUR_ACCESS_TOKEN") && !t.starts_with('<') {
            s.conn.secret_hint = hint(t);
            s.conn.from_env = true;
        }
    }
    let file_ui = s.ui.clone();
    if in_container && env_get(env, "ST_GW_UI").is_none() && s.source != "status-json" {
        // Off by default in a container, whatever the file's default says.
        s.ui.enabled = false;
        if file_ui.listen.starts_with("127.0.0.1:") {
            s.ui.listen = format!("0.0.0.0:{}", crate::gateway::UI_PORT);
        }
    }
    if let Some(v) = env_get(env, "ST_GW_UI") {
        match v.trim().to_ascii_lowercase().as_str() {
            "on" | "true" | "1" | "yes" => s.ui.enabled = true,
            "off" | "false" | "0" | "no" => s.ui.enabled = false,
            _ => {}
        }
        s.machine.ui_env = true;
    }
    if let Some(l) = env_any(env, &["ST_GW_UI_LISTEN"]) {
        s.ui.listen = l.to_string();
        s.machine.ui_env = true;
    }
    s.machine.rust_log = env_any(env, &["RUST_LOG"]).map(str::to_string);
}

/// The overrides of `env` the card lists: not the connection, not the
/// ones with their own rows, not `keep` (the image's own); secrets as
/// None. `editable`: whether the list is ours to change.
pub fn overrides(env: &[String], keep: &[String], editable: impl Fn(&str) -> bool, origin: &str) -> Vec<EnvVar> {
    env.iter()
        .filter_map(|l| l.split_once('='))
        .filter(|(n, _)| !n.is_empty() && !is_connection_env(n) && !DEDICATED_ENV.contains(n))
        .filter(|(n, v)| !keep.iter().any(|k| k.split_once('=').is_some_and(|(kn, kv)| kn == *n && kv == *v)))
        .map(|(n, v)| EnvVar { name: n.to_string(), value: (!is_secret_env(n)).then(|| v.to_string()), editable: editable(n), origin: origin.to_string() })
        .collect()
}

/// RUST_LOG in the card's words: errors, normal, detailed (None: other).
pub fn level_choice(rust_log: Option<&str>) -> Option<usize> {
    match rust_log.map(|l| l.trim().to_ascii_lowercase()) {
        None => Some(1),
        Some(l) if l.is_empty() || l == "info" => Some(1),
        Some(l) if l == "error" || l == "warn" => Some(0),
        Some(l) if l == "debug" || l == "trace" => Some(2),
        _ => None,
    }
}

pub const LEVELS: [&str; 3] = ["error", "info", "debug"];

// ---------------------------------------------------------------------------
// Connected or not, without the gateway's own state
// ---------------------------------------------------------------------------

/// Socket inodes in an `ls -l /proc/<pid>/fd` listing (`… -> socket:[123]`).
pub fn socket_inodes(fd_listing: &str) -> Vec<u64> {
    fd_listing.lines().filter_map(|l| l.split("socket:[").nth(1)).filter_map(|r| r.split(']').next()).filter_map(|n| n.parse().ok()).collect()
}

/// Whether one of `inodes` is an established TCP connection to `port` in
/// `/proc/net/tcp` and `tcp6` tables.
pub fn established_to(tables: &str, inodes: &[u64], port: u16) -> bool {
    tables.lines().any(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 10 || f[3] != "01" {
            return false;
        }
        let remote_port = f[2].rsplit(':').next().and_then(|p| u16::from_str_radix(p, 16).ok());
        let inode = f[9].parse::<u64>().ok();
        remote_port == Some(port) && inode.is_some_and(|i| inodes.contains(&i))
    })
}

/// Windows: `netstat -ano -p TCP` has an established connection of `pid`
/// to `port`.
pub fn netstat_established(out: &str, pid: u32, port: u16) -> bool {
    out.lines().any(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        f.len() >= 5 && f[0].eq_ignore_ascii_case("TCP") && f[3].eq_ignore_ascii_case("ESTABLISHED") && f[4].parse::<u32>().ok() == Some(pid) && f[2].rsplit(':').next().and_then(|p| p.parse::<u16>().ok()) == Some(port)
    })
}

// ---------------------------------------------------------------------------
// Reading on this computer
// ---------------------------------------------------------------------------

/// The read script for a gateway's config folder (native, as root, and in
/// its container): the gateway file, `logs.json`, whether the page has its
/// password, and the process's sockets. `$D` the folder, `$P` the pid.
const READ_SCRIPT: &str = r#"f="$D/st_gateway.json"; [ -f "$f" ] || { [ -f "$D/tb_gateway.json" ] && f="$D/tb_gateway.json"; }; echo "@@PATH $f"; cat "$f" 2>/dev/null; echo; echo "@@LOGS"; cat "$D/logs.json" 2>/dev/null; echo; echo "@@PW"; if [ -f "$D/local-ui/admin.argon2" ]; then echo yes; else echo no; fi; echo "@@FD"; ls -l /proc/$P/fd 2>/dev/null; echo "@@TCP"; cat /proc/net/tcp /proc/net/tcp6 2>/dev/null; echo "@@END""#;

/// The read script, for a folder `$D` and a process `$P` set before it.
pub fn read_script() -> &'static str {
    READ_SCRIPT
}

/// The read script's sections by name (PATH's own line, the file's path,
/// as FILE).
pub fn sections(out: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let mut cur: Option<String> = None;
    let mut buf = String::new();
    for l in out.lines() {
        if let Some(rest) = l.strip_prefix("@@") {
            if let Some(c) = cur.take() {
                map.insert(c, std::mem::take(&mut buf));
            }
            let (name, val) = rest.split_once(' ').unwrap_or((rest, ""));
            if name == "PATH" {
                map.insert("FILE".into(), val.to_string());
            }
            cur = Some(name.to_string());
            continue;
        }
        buf.push_str(l);
        buf.push('\n');
    }
    if let Some(c) = cur {
        map.insert(c, buf);
    }
    map
}

/// What the read script found: the settings from the files, the page's
/// password and, by the process's own sockets, whether it is connected.
pub fn from_read(out: &str) -> GwSettings {
    let sec = sections(out);
    let file = sec.get("FILE").cloned().unwrap_or_default();
    let body = sec.get("PATH");
    let mut s = match body.map(|b| b.trim()).filter(|b| !b.is_empty()) {
        Some(b) => from_files(b, sec.get("LOGS").map(String::as_str).filter(|l| !l.trim().is_empty())),
        None => {
            // No file (a container commissioned by its environment): the
            // gateway's own defaults, the environment over them later.
            let mut s = GwSettings { source: "none".into(), note: Some("The gateway has no settings file: it runs on its defaults and its environment.".into()), ..GwSettings::default() };
            s.conn.credential = "accessToken".into();
            s.ui.enabled = true;
            s.ui.listen = format!("127.0.0.1:{}", crate::gateway::UI_PORT);
            s
        }
    };
    if !file.is_empty() {
        if s.conn.host.is_some() {
            s.conn.host_source = Some("the gateway's config file".into());
        }
        s.config_path = Some(file);
    }
    s.ui.password_set = sec.get("PW").map(|p| p.trim() == "yes");
    connected_from_read(&mut s, out);
    s
}

/// Connected or not by the process's own sockets in the read script's
/// answer, to the port known now (the environment may name it after the
/// file): only when nothing better said it.
pub fn connected_from_read(s: &mut GwSettings, out: &str) {
    // Said by the gateway itself, or stopped: kept. By the sockets: again,
    // to the port known now.
    if s.connected.is_some() && s.connected_how.as_deref() != Some(BY_SOCKETS) {
        return;
    }
    s.connected = None;
    let sec = sections(out);
    let inodes = socket_inodes(sec.get("FD").map(String::as_str).unwrap_or_default());
    if let (Some(port), false) = (s.conn.port, inodes.is_empty()) {
        s.connected = Some(established_to(sec.get("TCP").map(String::as_str).unwrap_or_default(), &inodes, port));
        s.connected_how = Some(BY_SOCKETS.into());
    }
}

const BY_SOCKETS: &str = "the gateway's open connections";

/// The gateway found as `status`, read on this computer (as root for a
/// native one: the root job `gateway-settings-json`).
pub fn read(status: &Status) -> GwSettings {
    if !status.installed {
        return GwSettings { source: "none".into(), note: Some("There is no gateway.".into()), ..GwSettings::default() };
    }
    let mut s = match status.kind() {
        "docker" => crate::distro::gateway_docker::read_settings(status),
        #[cfg(not(windows))]
        "service" | "process" => crate::distro::gateway_native::gw_settings(status),
        #[cfg(windows)]
        "windows-service" => crate::gateway::windows_read_settings(),
        _ => GwSettings { source: "none".into(), note: Some("This kind of gateway is not read here.".into()), ..GwSettings::default() },
    };
    s.machine.kind = status.kind().to_string();
    s
}

// ---------------------------------------------------------------------------
// Applying
// ---------------------------------------------------------------------------

/// What Apply changes: only the fields set. Carries no secret (the page's
/// password goes its own way, [`set_password`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ApplyPlan {
    pub ui_enabled: Option<bool>,
    pub ui_listen: Option<String>,
    pub restart_always: Option<bool>,
    /// Native: the service account in the serial group.
    pub serial_group: Option<bool>,
    /// A container: the serial devices it is given.
    pub serial_devices: Option<Vec<String>>,
    /// The editable overrides, all of them (secrets kept as they are).
    pub env: Option<Vec<(String, String)>>,
    /// RUST_LOG: "" removes it.
    pub rust_log: Option<String>,
    /// The platform connection, written to the gateway's config file (a
    /// container's environment override of it is removed): host, port.
    pub conn_host: Option<String>,
    pub conn_port: Option<u16>,
}

impl ApplyPlan {
    pub fn is_empty(&self) -> bool {
        *self == ApplyPlan::default()
    }

    /// Whether applying it restarts the gateway (once) where it runs as
    /// `kind`: a Windows service's restart policy alone does not.
    pub fn restarts(&self, kind: &str) -> bool {
        if kind == "windows-service" {
            return self.ui_enabled.is_some() || self.ui_listen.is_some() || self.env.is_some() || self.rust_log.is_some() || self.conn_host.is_some() || self.conn_port.is_some();
        }
        !self.is_empty()
    }

    /// What Apply does to the running gateway, in one sentence (the Apply
    /// bar's line and the job's step).
    pub fn restart_words(&self, kind: &str) -> &'static str {
        if !self.restarts(kind) {
            return "Applied without a restart.";
        }
        match kind {
            "docker" => "The container is created again once, with the same volumes; its buffer keeps the data meanwhile.",
            _ => "The gateway restarts once to apply this; its buffer keeps the data meanwhile.",
        }
    }

    /// What changes, in words, for the job's steps and its last line.
    pub fn names(&self) -> Vec<&'static str> {
        let mut n = Vec::new();
        if self.conn_host.is_some() || self.conn_port.is_some() {
            n.push("Connection to SENTIENT");
        }
        if self.ui_enabled.is_some() {
            n.push("Web page");
        }
        if self.ui_listen.is_some() {
            n.push("Listen on");
        }
        if self.restart_always.is_some() {
            n.push("Restart after a failure");
        }
        if self.serial_group.is_some() || self.serial_devices.is_some() {
            n.push("Serial ports");
        }
        if self.env.is_some() {
            n.push("Environment overrides");
        }
        if self.rust_log.is_some() {
            n.push("Level before SENTIENT connects");
        }
        n
    }
}

/// What a read-back shows that the plan asked for otherwise ([] when all
/// is as asked).
pub fn mismatches(plan: &ApplyPlan, after: &GwSettings) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(h) = &plan.conn_host {
        if after.conn.host.as_deref() != Some(h.as_str()) {
            let from = after.conn.host_source.as_deref().map(|f| format!(" (from {f})")).unwrap_or_default();
            out.push(format!("the host {}{from}, not {h}", after.conn.host.clone().unwrap_or_else(|| "none".into())));
        }
    }
    if let Some(p) = plan.conn_port {
        if after.conn.port != Some(p) {
            out.push(format!("the MQTT port {}, not {p}", after.conn.port.map(|p| p.to_string()).unwrap_or_else(|| "not set".into())));
        }
    }

    if let Some(on) = plan.ui_enabled {
        if after.ui.enabled != on {
            out.push(format!("the web page is {}", if after.ui.enabled { "on" } else { "off" }));
        }
    }
    if let Some(l) = &plan.ui_listen {
        if plan.ui_enabled != Some(false) && &after.ui.listen != l {
            out.push(format!("the page listens on {}", after.ui.listen));
        }
    }
    if let Some(r) = plan.restart_always {
        if after.machine.restart_always.is_some_and(|a| a != r) {
            out.push(format!("its restart policy is {}", after.machine.restart_word.clone().unwrap_or_default()));
        }
    }
    if let Some(g) = plan.serial_group {
        if after.machine.serial.in_group != g {
            out.push("the serial group membership did not change".into());
        }
    }
    if let Some(d) = &plan.serial_devices {
        let mut want = d.clone();
        want.sort();
        let mut got = after.machine.serial.chosen.clone();
        got.sort();
        if want != got {
            out.push("the container's serial devices differ".into());
        }
    }
    if let Some(l) = &plan.rust_log {
        let got = after.machine.rust_log.clone().unwrap_or_default();
        // Removed: the gateway's own default comes back (a container's
        // image sets RUST_LOG=info).
        let same = if l.is_empty() { got.is_empty() || got == "info" } else { &got == l };
        if !same {
            out.push(format!("RUST_LOG is {}", if got.is_empty() { "not set" } else { &got }));
        }
    }
    if let Some(env) = &plan.env {
        for (n, v) in env {
            if !after.machine.env.iter().any(|e| &e.name == n && e.value.as_ref().is_none_or(|x| x == v)) {
                out.push(format!("{n} is not set"));
            }
        }
    }
    out
}

/// Apply `plan` to the gateway found as `status` on this computer (as root
/// for a native one), then read it back.
pub fn apply(sink: &ProgressFn, status: &Status, plan: &ApplyPlan) -> Result<(), String> {
    if plan.is_empty() {
        sink(Progress::Done { message: "Nothing to change.".into() });
        return Ok(());
    }
    let before = read(status);
    if let Some(why) = &before.machine.not_editable {
        return Err(why.clone());
    }
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    apply_kind(sink, status, &before, plan)?;
    // A new connection counts only once the gateway is connected with it,
    // else the previous one comes back (a container does this itself).
    let conn = plan.conn_host.is_some() || plan.conn_port.is_some();
    if conn && status.kind() != "docker" {
        let target = format!("{}:{}", plan.conn_host.clone().or_else(|| before.conn.host.clone()).unwrap_or_default(), plan.conn_port.or(before.conn.port).unwrap_or(1883));
        sink(Progress::Step { name: format!("Checking the gateway connects to SENTIENT at {target}") });
        let ok = wait_connected(status, since, 60);
        if ok.is_none() {
            sink(Progress::Log { line: "This gateway does not say whether it is connected, so the connection could not be checked: see Connected on the card.".into() });
        }
        if ok == Some(false) {
            sink(Progress::Log { line: "Not connected within 60 seconds.".into() });
            sink(Progress::Step { name: "Putting the previous connection back".into() });
            let back = ApplyPlan {
                conn_host: before.conn.host.clone().filter(|_| plan.conn_host.is_some()),
                conn_port: before.conn.port.filter(|_| plan.conn_port.is_some()),
                ..ApplyPlan::default()
            };
            if !back.is_empty() {
                let now = crate::gateway::status();
                let current = read(&now);
                if let Err(e) = apply_kind(sink, &now, &current, &back) {
                    sink(Progress::Log { line: format!("The previous connection could not be put back: {e}") });
                }
            }
            return Err(crate::distro::gateway_docker::no_connect_words(&target, false));
        }
        if ok == Some(true) {
            sink(Progress::Log { line: format!("Connected to SENTIENT at {target}.") });
        }
    }
    // Connected, the gateway takes the connection SENTIENT keeps for it
    // (its gateway configuration) and writes it to its file: the one typed
    // here is then not kept, and the reason is SENTIENT, not this Apply.
    if conn {
        if let Some((h, p)) = platform_took_connection(status, since, 20) {
            let target = format!("{}:{}", plan.conn_host.clone().or_else(|| before.conn.host.clone()).unwrap_or_default(), plan.conn_port.or(before.conn.port).unwrap_or(1883));
            let via = status.kind() == "docker" && plan.conn_host.as_deref().is_some_and(crate::distro::gateway_docker::is_loopback);
            return Err(platform_owns_words(&target, &h, p, via));
        }
    }
    sink(Progress::Step { name: "Reading the settings back".into() });
    let status = crate::gateway::status();
    let after = read(&status);
    let wrong = mismatches(plan, &after);
    if !wrong.is_empty() {
        return Err(format!("Applied, but the gateway reads back {}.", wrong.join("; ")));
    }
    sink(Progress::Done { message: format!("Applied and read back: {}.", plan.names().join(", ")) });
    Ok(())
}

/// Why a connection set here was not kept: SENTIENT sent the one it keeps
/// for this gateway once it connected (`via`: typed as localhost for a
/// container, which SENTIENT must name as host.docker.internal).
pub fn platform_owns_words(target: &str, host: &str, port: u16, via: bool) -> String {
    let how = if via { " (there, this machine is host.docker.internal: localhost would be the gateway's own container)" } else { "" };
    format!(
        "The gateway connected to SENTIENT at {target}, then SENTIENT sent the connection it keeps for this gateway, {host}:{port}, and the gateway switched to it. While the gateway is connected, SENTIENT's gateway configuration decides its connection. To use {target}, change it in SENTIENT › Gateways › this gateway › Configuration › General{how}."
    )
}

/// (host, port) when the gateway's log since `since` says SENTIENT changed
/// its connection ("the platform changed this gateway's connection"),
/// watched for `secs` seconds.
fn platform_took_connection(status: &Status, since: u64, secs: u64) -> Option<(String, u16)> {
    for _ in 0..secs / 4 {
        std::thread::sleep(std::time::Duration::from_secs(4));
        let log: Option<String> = match status.kind() {
            #[cfg(not(windows))]
            "docker" => status.container.as_deref().and_then(|n| crate::distro::gateway_docker::logs_since(n, since)),
            #[cfg(not(windows))]
            "service" => status.service.as_deref().and_then(|u| {
                crate::sys::host_command("journalctl").args(["-u", u, "--since", &format!("@{since}"), "-o", "cat", "--no-pager"]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            }),
            _ => None,
        };
        if let Some(found) = log.as_deref().and_then(platform_change_in) {
            return Some(found);
        }
    }
    None
}

/// The last "the platform changed this gateway's connection" line's host
/// and port.
pub fn platform_change_in(log: &str) -> Option<(String, u16)> {
    let line = log.lines().rev().find(|l| l.contains("the platform changed this gateway's connection"))?;
    let field = |k: &str| line.split_whitespace().find_map(|w| w.strip_prefix(&format!("{k}=")).map(|v| v.trim_matches('"').to_string()));
    Some((field("host")?, field("port")?.parse().ok()?))
}

/// The settings applied by the way the gateway runs.
fn apply_kind(sink: &ProgressFn, status: &Status, before: &GwSettings, plan: &ApplyPlan) -> Result<(), String> {
    match status.kind() {
        "docker" => crate::distro::gateway_docker::apply_settings(sink, status, before, plan),
        #[cfg(not(windows))]
        "service" => crate::distro::gateway_native::apply_settings(sink, status, before, plan),
        #[cfg(windows)]
        "windows-service" => crate::gateway::windows_apply_settings(sink, before, plan),
        _ => Err("This gateway's settings cannot be changed from here.".into()),
    }
}

/// Whether the service gateway connects within `secs` seconds of `since`
/// (Unix seconds): its log since then says so (systemd), or its own open
/// connections do. None: it cannot be known (no log here, and the gateway
/// does not say).
fn wait_connected(status: &Status, since: u64, secs: u64) -> Option<bool> {
    let journal = cfg!(not(windows)) && status.service.is_some();
    for _ in 0..secs / 3 {
        std::thread::sleep(std::time::Duration::from_secs(3));
        #[cfg(not(windows))]
        if let Some(unit) = &status.service {
            let at = format!("@{since}");
            if let Ok(o) = crate::sys::host_command("journalctl").args(["-u", unit, "--since", &at, "-o", "cat", "--no-pager"]).output() {
                if crate::distro::gateway_docker::connected_in_log(&crate::distro::gateway_docker::strip_ansi(&String::from_utf8_lossy(&o.stdout))) {
                    return Some(true);
                }
            }
        }
        #[cfg(windows)]
        let _ = since;
        match read(&crate::gateway::status()).connected {
            Some(true) => return Some(true),
            None if !journal => return None,
            _ => {}
        }
    }
    Some(false)
}

/// Set the local page's administrator password through the gateway's own
/// command (`--ui-password -`, the password on its standard input; never
/// in a command line or a log). Every browser signed in is signed out.
pub fn set_password(status: &Status, password: &str) -> Result<String, String> {
    if password.chars().count() < 8 {
        return Err("The password needs 8 characters or more.".into());
    }
    match status.kind() {
        "docker" => crate::distro::gateway_docker::set_ui_password(status, password)?,
        #[cfg(not(windows))]
        "service" | "process" => run_with_stdin(status.program.as_deref().unwrap_or(crate::distro::gateway_native::PROGRAM), &["--ui-password", "-"], password)?,
        #[cfg(windows)]
        "windows-service" => run_with_stdin(&crate::gateway::windows_program(), &["--ui-password", "-"], password)?,
        _ => return Err("There is no gateway here whose password can be set.".into()),
    };
    Ok("The web page's password is changed; every browser signed in to it is signed out.".into())
}

/// Run `program args` with `input` as one line on its standard input:
/// (its output, else its error).
pub(crate) fn run_with_stdin(program: &str, args: &[&str], input: &str) -> Result<String, String> {
    use std::io::Write as _;
    use std::process::Stdio;
    let mut c = crate::sys::host_command(program);
    c.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("start {program}: {e}"))?;
    if let Some(mut s) = child.stdin.take() {
        let _ = writeln!(s, "{input}");
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let e = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if e.is_empty() { format!("{program} {} failed", args.join(" ")) } else { e })
    }
}

/// The page's listen address from the card's three choices.
pub fn listen_of(mode: usize, address: &str, port: u16) -> String {
    match mode {
        0 => format!("127.0.0.1:{port}"),
        1 => format!("0.0.0.0:{port}"),
        _ => {
            let a = address.trim().trim_start_matches('[').trim_end_matches(']');
            if a.contains(':') { format!("[{a}]:{port}") } else { format!("{a}:{port}") }
        }
    }
}

/// A listen address as the card's choice: (mode, address, port).
pub fn listen_parts(listen: &str) -> (usize, String, u16) {
    let (h, p) = match listen.rsplit_once(':') {
        Some((h, p)) => (h.trim_start_matches('[').trim_end_matches(']').to_string(), p.parse().unwrap_or(crate::gateway::UI_PORT)),
        None => (listen.to_string(), crate::gateway::UI_PORT),
    };
    let mode = if h == "127.0.0.1" || h == "localhost" || h == "::1" {
        0
    } else if h == "0.0.0.0" || h == "::" || h.is_empty() {
        1
    } else {
        2
    };
    (mode, if mode == 2 { h } else { String::new() }, p)
}

// ---------------------------------------------------------------------------
// Test connection
// ---------------------------------------------------------------------------

/// What a test of the connection is told: where SENTIENT's MQTT is and
/// whether it takes TLS. No credential is read for it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestCreds {
    pub host: String,
    pub port: u16,
    pub tls: bool,
}

/// The test's closing words: the gateway's credentials are not tried.
pub const NOT_SIGNED_IN: &str = "Not yet tested with the gateway's credentials.";

/// Where the gateway connects, from its file, the environment over it.
pub fn creds_of(file: Option<&str>, env: &[String]) -> Option<TestCreds> {
    let v = file.and_then(|t| serde_json::from_str::<Value>(t).ok()).unwrap_or(Value::Null);
    let p = v.get("sentient").or_else(|| v.get("thingsboard")).cloned().unwrap_or(Value::Null);
    let sec = p.get("security").cloned().unwrap_or(Value::Null);
    let str_of = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::trim).filter(|x| !x.is_empty()).map(str::to_string);
    let host = env_any(env, &["host", "ST_GW_HOST", "TB_GW_HOST"]).map(str::to_string).or_else(|| str_of(&p, "host"))?;
    let port = env_any(env, &["port", "ST_GW_PORT", "TB_GW_PORT"]).and_then(|p| p.parse().ok()).or_else(|| p.get("port").and_then(Value::as_u64).map(|x| x as u16)).unwrap_or(1883);
    let kind = str_of(&sec, "type").unwrap_or_default().to_ascii_lowercase();
    let tls = kind.starts_with("tls") || kind.starts_with("x509") || str_of(&sec, "caCert").is_some();
    Some(TestCreds { host, port, tls })
}

/// A TLS check that accepts any certificate: it only shows that the
/// address speaks TLS. Nothing is sent over it.
#[derive(Debug)]
struct AnyCert(std::sync::Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AnyCert {
    fn verify_server_cert(&self, _: &rustls::pki_types::CertificateDer<'_>, _: &[rustls::pki_types::CertificateDer<'_>], _: &rustls::pki_types::ServerName<'_>, _: &[u8], _: rustls::pki_types::UnixTime) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, m: &[u8], c: &rustls::pki_types::CertificateDer<'_>, d: &rustls::DigitallySignedStruct) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &rustls::pki_types::CertificateDer<'_>, d: &rustls::DigitallySignedStruct) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn tls_handshake(tcp: &mut std::net::TcpStream, host: &str) -> Result<(), String> {
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(AnyCert(provider)))
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(host.to_string()).map_err(|e| e.to_string())?;
    let mut conn = rustls::ClientConnection::new(std::sync::Arc::new(config), name).map_err(|e| e.to_string())?;
    while conn.is_handshaking() {
        conn.complete_io(tcp).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Test that SENTIENT's MQTT answers where the gateway connects: a TCP
/// connection, and a TLS handshake when it takes TLS. The gateway's
/// credentials are not tried (its own session is never touched).
pub fn test_with(c: &TestCreds) -> Result<String, String> {
    use std::net::ToSocketAddrs as _;
    let addr = (c.host.as_str(), c.port).to_socket_addrs().map_err(|e| format!("{}: the name is not known here ({e}).", c.host))?.next().ok_or_else(|| format!("{} has no address.", c.host))?;
    let mut tcp = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(5)).map_err(|e| format!("{}:{} does not answer: {e}.", c.host, c.port))?;
    if c.tls {
        let _ = tcp.set_read_timeout(Some(std::time::Duration::from_secs(8)));
        let _ = tcp.set_write_timeout(Some(std::time::Duration::from_secs(8)));
        tls_handshake(&mut tcp, &c.host).map_err(|e| format!("{}:{} answers, but its TLS handshake failed: {e}.", c.host, c.port))?;
        return Ok(format!("{}:{} answers and completes a TLS handshake. {NOT_SIGNED_IN}", c.host, c.port));
    }
    Ok(format!("{}:{} answers. {NOT_SIGNED_IN}", c.host, c.port))
}

/// Test where the gateway found as `status` on this computer connects (as
/// root for a native one: its settings folder is closed).
pub fn test_connection(status: &Status) -> Result<String, String> {
    let creds = match status.kind() {
        "docker" => crate::distro::gateway_docker::test_creds(status),
        #[cfg(not(windows))]
        "service" | "process" => crate::distro::gateway_native::test_creds(),
        #[cfg(windows)]
        "windows-service" => crate::gateway::windows_test_creds(),
        _ => None,
    };
    let creds = creds.ok_or("The gateway is not commissioned: it has no platform host to test.")?;
    test_with(&creds)
}

#[cfg(test)]
mod tests {
    /// SENTIENT's push of its own connection is found in the gateway's log,
    /// and said as SENTIENT's, with host.docker.internal for a container.
    #[test]
    fn sentients_own_connection_is_found_and_said() {
        let log = "2026-11-21T17:23:38Z  INFO gateway_core::remote_config: the platform changed this gateway's connection host=192.168.0.20 port=1883";
        assert_eq!(super::platform_change_in(log), Some(("192.168.0.20".to_string(), 1883)));
        assert_eq!(super::platform_change_in("INFO connected to platform"), None);
        let w = super::platform_owns_words("127.0.0.1:1883", "192.168.0.20", 1883, true);
        assert!(w.contains("192.168.0.20:1883") && w.contains("Configuration › General") && w.contains("host.docker.internal"), "{w}");
    }

    use super::*;

    const FILE: &str = r#"{"sentient":{"host":"sentient.example.local","port":1883,"remoteShell":false,
        "security":{"type":"accessToken","accessToken":"Q1w2E3r4T5y6U7i8O9p0"},
        "statistics":{"enable":true,"statsSendPeriodInSeconds":3600,"enableCustom":false,"commands":[]},
        "rateLimits":"DEFAULT_TELEMETRY_RATE_LIMIT","maxPayloadSizeBytes":8196},
        "storage":{"type":"sqlite","data_file_path":"./data/","messages_ttl_in_days":7,"size_limit":1.0,"max_db_amount":10},
        "localUi":{"enabled":true,"listen":"0.0.0.0:8642","hostnames":["gw.example.local"]},
        "connectors":[{"name":"line-1-modbus","type":"modbus","configuration":"modbus.json"},{"name":"kiln-opc","type":"opcua","configuration":"opcua.json","enabled":false}]}"#;

    #[test]
    fn secrets_become_hints() {
        assert_eq!(hint("Q1w2E3r4T5y6U7i8O9p0").as_deref(), Some("••••O9p0"));
        assert_eq!(hint("short").as_deref(), Some("set"));
        assert_eq!(hint("  "), None);
        let s = from_files(FILE, None);
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("Q1w2E3r4T5y6U7i8O9p0"), "{json}");
        assert_eq!(s.conn.credential_words(), "Access token ••••O9p0");
    }

    #[test]
    fn the_file_gives_each_tab() {
        let s = from_files(FILE, Some(r#"{"loggers":{"service":{"level":"INFO"},"connector":{"level":"WARNING"}}}"#));
        assert_eq!((s.conn.host.as_deref(), s.conn.port), (Some("sentient.example.local"), Some(1883)));
        assert!(s.ui.enabled && s.ui.listen == "0.0.0.0:8642" && s.ui.hostnames == ["gw.example.local"]);
        assert_eq!(s.connectors.len(), 2);
        assert_eq!((s.connectors[1].kind.as_str(), s.connectors[1].enabled), ("OPC UA", Some(false)));
        assert!(s.storage.iter().any(|f| f.label == "Keep messages" && f.value == "7 days"));
        assert!(s.statistics.iter().any(|f| f.label == "Every" && f.value == "3600 s"));
        assert!(s.logging.iter().any(|f| f.label == "Connectors" && f.value == "WARNING"));
        assert!(s.platform.iter().any(|f| f.label == "Rate limits" && f.value == "SENTIENT's defaults"));
    }

    #[test]
    fn the_environment_wins_and_hides_its_secrets() {
        let mut s = from_files(FILE, None);
        let env: Vec<String> = ["host=10.0.4.12", "port=1884", "accessToken=AbCdEfGh12345678wxyz", "ST_GW_UI=off", "RUST_LOG=debug", "TB_GW_STORAGE_PASSWORD=x", "PATH=/usr/bin", "ST_GW_EXTRA=1"].iter().map(|s| s.to_string()).collect();
        apply_env(&mut s, &env, true);
        assert_eq!((s.conn.host.as_deref(), s.conn.port, s.conn.secret_hint.as_deref()), (Some("10.0.4.12"), Some(1884), Some("••••wxyz")));
        assert!(!s.ui.enabled && s.machine.ui_env);
        assert_eq!(s.machine.rust_log.as_deref(), Some("debug"));
        let o = overrides(&env, &["PATH=/usr/bin".to_string()], |_| true, "container");
        let names: Vec<&str> = o.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["TB_GW_STORAGE_PASSWORD", "ST_GW_EXTRA"]);
        assert_eq!(o[0].value, None);
        assert_eq!(o[1].value.as_deref(), Some("1"));
    }

    #[test]
    fn a_container_without_a_file_runs_on_its_environment() {
        let out = "@@PATH /sentient_gateway/config/tb_gateway.json\n\n@@LOGS\n@@PW\nno\n@@FD\nlrwx 7 -> socket:[9]\n@@TCP\n 0: 0100007F:D2B4 0100007F:075B 01 0:0 0:0 0 0 0 9 1\n@@END\n";
        let mut s = from_read(out);
        assert_eq!(s.connected, None);
        apply_env(&mut s, &["host=127.0.0.1".into(), "port=1883".into(), "accessToken=Ex4mpleT0kenAbcd3f9a".into()], true);
        connected_from_read(&mut s, out);
        assert_eq!((s.connected, s.conn.credential.as_str()), (Some(true), "accessToken"));
        assert_eq!((s.ui.enabled, s.ui.listen.as_str()), (false, "0.0.0.0:8642"));
    }

    #[test]
    fn a_container_page_is_off_by_default() {
        let mut s = from_files(r#"{"sentient":{"host":"h"}}"#, None);
        apply_env(&mut s, &[], true);
        assert!(!s.ui.enabled);
        assert_eq!(s.ui.listen, "0.0.0.0:8642");
    }

    #[test]
    fn status_json_says_connected_and_its_connectors() {
        let mut s = from_files(FILE, None);
        assert!(merge_status_json(&mut s, r#"{"connected":false,"localUi":{"passwordSet":true},"connectors":[{"name":"a","type":"s7commplus","state":"running","devices":3}],"config":{"sentient":{"host":"h2","port":1883,"security":{"accessToken":"ZZZZZZZZZZZZZZZZ1234"}}}}"#));
        assert_eq!((s.connected, s.ui.password_set, s.source.as_str()), (Some(false), Some(true), "status-json"));
        assert_eq!(s.connectors[0].devices, Some(3));
        assert_eq!(s.conn.secret_hint.as_deref(), Some("••••1234"));
        assert!(!serde_json::to_string(&s).unwrap().contains("ZZZZ"));
        assert!(!merge_status_json(&mut s, "not json"));
    }

    /// The gateway's own --status-json (sentient-gateway-rs, branch
    /// claude/project-thread-40rbqc), as it prints it.
    #[test]
    fn the_gateways_status_json_as_printed() {
        let line = r#"{"version":"4.16.3","running":true,"connected":true,"stateAgeMs":3012,"phase":"running","configFile":"/etc/sentient-gateway/st_gateway.json","config":{"sentient":{"host":"sentient.example.local","port":1883,"security":{"type":"accessToken","accessToken":"set"}}},"localUi":{"enabled":true,"listen":"127.0.0.1:8642","hostnames":[],"passwordSet":true},"connectors":[{"name":"line-1-modbus","type":"modbus","enabled":true,"state":"running","devices":3}],"logs":{"loggers":{"service":{"level":"INFO"}}}}"#;
        let mut s = GwSettings::default();
        assert!(merge_status_json(&mut s, line));
        assert_eq!((s.connected, s.ui.password_set, s.conn.secret_hint.as_deref()), (Some(true), Some(true), Some("set")));
        assert_eq!((s.connectors[0].state.as_deref(), s.connectors[0].devices), (Some("running"), Some(3)));
        assert!(s.logging.iter().any(|f| f.label == "Service" && f.value == "INFO"));
        // Not known (an old state file of a live gateway): left to the sockets.
        let mut u = GwSettings { connected: Some(false), ..GwSettings::default() };
        assert!(merge_status_json(&mut u, &line.replace("\"connected\":true", "\"connected\":null")));
        assert_eq!(u.connected, Some(false));
    }

    #[test]
    fn connected_by_the_process_sockets() {
        let fd = "lrwx------ 1 sg sg 64 Nov 18 07:00 7 -> socket:[48123]\nlrwx------ 1 sg sg 64 Nov 18 07:00 8 -> /dev/null\n";
        assert_eq!(socket_inodes(fd), vec![48123]);
        let tcp = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0100007F:D2B4 0100007F:075B 01 00000000:00000000 00:00000000 00000000  1000        0 48123 1 0000000000000000 20 4 30 10 -1\n";
        assert!(established_to(tcp, &[48123], 1883));
        assert!(!established_to(tcp, &[1], 1883));
        assert!(!established_to(tcp, &[48123], 8883));
        let netstat = "  Proto  Local Address          Foreign Address        State           PID\n  TCP    10.0.0.2:50122         10.0.4.12:1883         ESTABLISHED     4242\n";
        assert!(netstat_established(netstat, 4242, 1883));
        assert!(!netstat_established(netstat, 4243, 1883));
    }

    #[test]
    fn the_read_script_answer() {
        let out = format!("@@PATH /etc/sentient-gateway/st_gateway.json\n{FILE}\n@@LOGS\n\n@@PW\nyes\n@@FD\nlrwx 7 -> socket:[9]\n@@TCP\n 0: 0100007F:D2B4 0100007F:075B 01 0:0 0:0 0 0 0 9 1\n@@END\n");
        let s = from_read(&out);
        assert_eq!(s.config_path.as_deref(), Some("/etc/sentient-gateway/st_gateway.json"));
        assert_eq!((s.ui.password_set, s.connected), (Some(true), Some(true)));
        assert_eq!(s.connectors.len(), 2);
        let none = from_read("@@PATH /etc/sentient-gateway/st_gateway.json\n\n@@LOGS\n@@PW\nno\n@@FD\n@@TCP\n@@END\n");
        assert_eq!((none.source.as_str(), none.connected), ("none", None));
    }

    #[test]
    fn listen_choices_round_trip() {
        assert_eq!(listen_parts("127.0.0.1:8642"), (0, String::new(), 8642));
        assert_eq!(listen_parts("0.0.0.0:9443"), (1, String::new(), 9443));
        assert_eq!(listen_parts("192.168.1.20:8642"), (2, "192.168.1.20".into(), 8642));
        assert_eq!(listen_of(2, "192.168.1.20", 8642), "192.168.1.20:8642");
        assert_eq!(listen_of(2, "fe80::1", 8642), "[fe80::1]:8642");
        assert_eq!(listen_of(1, "", 8000), "0.0.0.0:8000");
    }

    #[test]
    fn the_test_reads_no_credential() {
        let c = creds_of(Some(FILE), &[]).unwrap();
        assert_eq!(c, TestCreds { host: "sentient.example.local".into(), port: 1883, tls: false });
        assert!(!format!("{c:?}").contains("Q1w2"));
        let tls = creds_of(Some(r#"{"sentient":{"host":"h","port":8883,"security":{"type":"tlsAccessToken","accessToken":"x"}}}"#), &["port=8884".into()]).unwrap();
        assert_eq!((tls.port, tls.tls), (8884, true));
        assert!(creds_of(Some("{}"), &[]).is_none());
    }

    #[test]
    fn the_test_says_it_did_not_sign_in() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let m = test_with(&TestCreds { host: "127.0.0.1".into(), port, tls: false }).unwrap();
        assert!(m.ends_with(NOT_SIGNED_IN), "{m}");
        drop(l);
        // Nothing listening now: says so.
        assert!(test_with(&TestCreds { host: "127.0.0.1".into(), port, tls: false }).unwrap_err().contains("does not answer"));
    }

    #[test]
    fn a_read_back_names_what_differs() {
        let plan = ApplyPlan { ui_enabled: Some(true), ui_listen: Some("0.0.0.0:8642".into()), rust_log: Some("debug".into()), ..ApplyPlan::default() };
        let mut after = GwSettings::default();
        after.ui.enabled = true;
        after.ui.listen = "0.0.0.0:8642".into();
        after.machine.rust_log = Some("debug".into());
        assert!(mismatches(&plan, &after).is_empty());
        after.ui.listen = "127.0.0.1:8642".into();
        assert_eq!(mismatches(&plan, &after), vec!["the page listens on 127.0.0.1:8642"]);
        assert_eq!(plan.names(), vec!["Web page", "Listen on", "Level before SENTIENT connects"]);
    }

    #[test]
    fn what_restarts_where() {
        let policy = ApplyPlan { restart_always: Some(true), ..ApplyPlan::default() };
        assert!(!policy.restarts("windows-service") && policy.restarts("service") && policy.restarts("docker"));
        assert_eq!(policy.restart_words("windows-service"), "Applied without a restart.");
        assert!(policy.restart_words("docker").contains("created again once"));
        let level = ApplyPlan { rust_log: Some("debug".into()), ..ApplyPlan::default() };
        assert!(level.restarts("windows-service") && level.restart_words("service").contains("restarts once"));
        assert!(!ApplyPlan::default().restarts("service"));
    }

    #[test]
    fn levels_in_words() {
        assert_eq!(level_choice(None), Some(1));
        assert_eq!(level_choice(Some("warn")), Some(0));
        assert_eq!(level_choice(Some("debug")), Some(2));
        assert_eq!(level_choice(Some("gateway_core=trace")), None);
    }

    #[test]
    fn capabilities_from_the_help() {
        let c = Caps::of_help("  --ui-password -  …\n  --ui-listen ADDR\n");
        assert!(c.ui_password && c.ui_listen && !c.status_json && !c.ui_hostname);
        assert!(Caps::of_help("sentient-gateway --status-json").status_json);
    }
}
