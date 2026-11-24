//! `aetherrv-peplink`: the Peplink MAX BR1 Pro 5G over its local Router API (Python `adapters/peplink.py`,
//! documented for firmware 8.6.0).
//!
//! Auth, one of:
//! * client: an `api.read-only` client → POST /api/auth.token.grant → `?accessToken=…`
//! * user: the router's READ-ONLY web-admin user → POST /api/login → session cookie `bauth`. The router itself
//!   grants GET only to that account, so the Pi never holds admin rights for monitoring. A login that has POST
//!   permission is refused.
//!
//! Credentials live in the root:aetherrv 0640 secrets file.
//!
//! Endpoints read: `status.wan.connection` (per-WAN status, cellular carrier/RAT/band/signal, SIM, roaming),
//! `status.wan.connection.allowance` (per-SIM usage vs limit), `status.traffic` (live per-WAN bandwidth),
//! `info.location` (GPS).
//!
//! PRIVACY: the responses contain IMSI/ICCID/IMEI/APN credentials. [`normalize_wans`] builds every row from an
//! explicit whitelist of fields, so none of those ever reach state, logs or the UI.
//!
//! Control (TxGate device "peplink"): `set_wan_priority` / `set_wan_order`, through a SEPARATE read-write client
//! (`peplink_rw_*` secrets) used for nothing else: a fresh GET of `config.wan.connection.priority`, a strict payload
//! with only the changed connections, ONE POST (`instantActive`), never retried, then `status.wan.connection` is
//! polled until the change shows.

use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::clock::now_s;
use aetherrv_core::cmdlog::CommandLog;
use aetherrv_core::command::{BoxFuture, CommandHandler, Reply};
use aetherrv_core::config::{Cfg, truthy};
use aetherrv_core::num::{jf, round};
use aetherrv_core::pyfmt::{py_float, py_repr, py_str};
use aetherrv_core::state::StateStore;
use aetherrv_core::txgate::TxGate;
use serde_json::{Map, Value, json};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

pub const DEVICE: &str = "peplink";
pub const COMMANDS: [&str; 2] = ["set_wan_priority", "set_wan_order"];
pub const PRIORITY_FUNC: &str = "config.wan.connection.priority";

// ── errors ──────────────────────────────────────────────────────────────────────────────────────────────────────

/// `Api` is Python's `PeplinkError` (the router answered `stat != ok`, or a refused setup); `Json` a body that isn't
/// JSON (Python's `json.JSONDecodeError`, a `ValueError`); `Other` everything else (connection, TLS, timeout).
#[derive(Debug, Clone, PartialEq)]
pub enum PepError {
    Api(String),
    Json(String),
    Other(String),
}

impl std::fmt::Display for PepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PepError::Api(s) | PepError::Json(s) | PepError::Other(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for PepError {}

pub type PepFut<'a, T> = Pin<Box<dyn Future<Output = Result<T, PepError>> + Send + 'a>>;

/// The two router calls the service makes. [`PeplinkClient`] in the service; fakes in tests.
pub trait Router: Send + Sync + 'static {
    fn get<'a>(&'a self, func: &'a str) -> PepFut<'a, Value>;
    /// One POST, never repeated.
    fn post_once<'a>(&'a self, func: &'a str, body: &'a Value) -> PepFut<'a, Value>;
}

// ── small Python semantics helpers ──────────────────────────────────────────────────────────────────────────────

/// `d.get(k)` of a JSON value that may not be an object.
fn get<'a>(v: &'a Value, k: &str) -> Option<&'a Value> {
    v.as_object().and_then(|m| m.get(k))
}

/// `d.get(k) or default`.
fn or(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| truthy(x))
}

fn val(v: Option<&Value>) -> Value {
    v.cloned().unwrap_or(Value::Null)
}

/// `(d.get(k) or {})` as an object view.
fn obj_or_empty(v: Option<&Value>) -> Value {
    match or(v) {
        Some(x @ Value::Object(_)) => x.clone(),
        _ => Value::Object(Map::new()),
    }
}

/// `x or []` iterated.
fn list_or_empty(v: Option<&Value>) -> Vec<Value> {
    match or(v) {
        Some(Value::Array(a)) => a.clone(),
        _ => Vec::new(),
    }
}

/// A JSON integer (Python `isinstance(c, int)`; JSON booleans aren't numbers here).
fn is_int(v: &Value) -> bool {
    v.is_i64() || v.is_u64()
}

/// Python `json.dumps(value)` (default separators, `ensure_ascii`), for `command_log.value`.
pub fn json_dumps(v: &Value) -> String {
    fn s(out: &mut String, x: &str) {
        out.push('"');
        for c in x.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                    let mut buf = [0u16; 2];
                    for u in c.encode_utf16(&mut buf) {
                        out.push_str(&format!("\\u{u:04x}"));
                    }
                }
                c => out.push(c),
            }
        }
        out.push('"');
    }
    fn w(out: &mut String, v: &Value) {
        match v {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Number(n) => {
                if n.is_f64() {
                    out.push_str(&py_float(n.as_f64().unwrap_or(0.0)))
                } else {
                    out.push_str(&n.to_string())
                }
            }
            Value::String(x) => s(out, x),
            Value::Array(a) => {
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    w(out, x);
                }
                out.push(']');
            }
            Value::Object(m) => {
                out.push('{');
                for (i, (k, x)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    s(out, k);
                    out.push_str(": ");
                    w(out, x);
                }
                out.push('}');
            }
        }
    }
    let mut out = String::new();
    w(&mut out, v);
    out
}

// ── pure normalizers (the documented response shapes) ───────────────────────────────────────────────────────────

/// The primary band: fw ≥ 8.1.2 nests bands under `rat`, older fw has a flat `band`. The strongest RSRP wins (the
/// first one on a tie, like Python's `max`).
fn best_band(cell: &Value) -> Vec<(&'static str, Value)> {
    let mut bands: Vec<(Value, Value)> = Vec::new();
    for rat in list_or_empty(get(cell, "rat")) {
        for b in list_or_empty(get(&rat, "band")) {
            bands.push((val(get(&rat, "name")), b));
        }
    }
    if bands.is_empty() {
        bands = list_or_empty(get(cell, "band")).into_iter().map(|b| (Value::Null, b)).collect();
    }
    if bands.is_empty() {
        return Vec::new();
    }
    let rsrp = |b: &Value| -> f64 {
        let sig = obj_or_empty(get(b, "signal"));
        get(&sig, "rsrp").and_then(Value::as_f64).unwrap_or(-999.0)
    };
    let mut best = 0;
    for (i, (_, b)) in bands.iter().enumerate() {
        if rsrp(b) > rsrp(&bands[best].1) {
            best = i;
        }
    }
    let (rat, b) = &bands[best];
    let sig = obj_or_empty(get(b, "signal"));
    vec![
        ("rat", rat.clone()),
        ("band", val(get(b, "name"))),
        ("rsrp", val(get(&sig, "rsrp"))),
        ("rsrq", val(get(&sig, "rsrq"))),
        ("sinr", val(get(&sig, "sinr"))),
        ("rssi", val(get(&sig, "rssi"))),
        ("bands", json!(bands.len())),
    ]
}

/// `status.wan.connection` → one row per WAN, every field from a whitelist (no IMSI/ICCID/IMEI/APN/BSSID/IP).
pub fn normalize_wans(resp: &Value) -> Vec<Value> {
    let order: Vec<Value> = match or(get(resp, "order")) {
        Some(Value::Array(a)) => a.clone(),
        Some(_) => Vec::new(),
        None => {
            let mut ids: Vec<i64> = resp
                .as_object()
                .map(|m| m.keys().filter(|k| !k.is_empty() && k.bytes().all(|c| c.is_ascii_digit())).filter_map(|k| k.parse().ok()).collect())
                .unwrap_or_default();
            ids.sort();
            ids.into_iter().map(Value::from).collect()
        }
    };
    let mut out = Vec::new();
    for cid in order {
        let w = obj_or_empty(get(resp, &py_str(&cid)));
        let name = match or(get(&w, "name")) {
            Some(Value::String(s)) => s.clone(),
            Some(other) => py_str(other),
            None => format!("WAN {}", py_str(&cid)),
        };
        let mut row = Map::new();
        row.insert("id".into(), cid.clone());
        row.insert("name".into(), json!(name));
        row.insert("type".into(), val(or(get(&w, "virtualType")).or(get(&w, "type"))));
        row.insert("message".into(), or(get(&w, "message")).cloned().unwrap_or(json!("")));
        row.insert("led".into(), or(get(&w, "statusLed")).cloned().unwrap_or(json!("gray")));
        row.insert("enabled".into(), json!(get(&w, "enable").is_some_and(truthy)));
        row.insert("priority".into(), val(get(&w, "priority")));
        row.insert("uptime".into(), val(get(&w, "uptime")));
        row.insert("connected".into(), json!(get(&w, "statusLed") == Some(&json!("green"))));
        row.insert("starlink".into(), json!(name.to_lowercase().contains("starlink")));
        if let Some(cell) = or(get(&w, "cellular")).or(or(get(&w, "gobi"))) {
            let sims = obj_or_empty(get(cell, "sim"));
            let active_sim = list_or_empty(get(&sims, "order"))
                .into_iter()
                .find(|sid| get(&obj_or_empty(get(&sims, &py_str(sid))), "active").is_some_and(truthy))
                .unwrap_or(Value::Null);
            let mut c = Map::new();
            c.insert("carrier".into(), val(get(&obj_or_empty(get(cell, "carrier")), "name")));
            c.insert("network".into(), val(or(get(cell, "mobileType")).or(get(cell, "network"))));
            c.insert("signal_level".into(), val(get(cell, "signalLevel")));
            c.insert("roaming".into(), val(get(&obj_or_empty(get(cell, "roamingStatus")), "message")));
            c.insert("ca".into(), val(get(cell, "carrierAggregation")));
            c.insert("sim".into(), active_sim);
            for (k, v) in best_band(cell) {
                c.insert(k.into(), v);
            }
            row.insert("cellular".into(), Value::Object(c));
        }
        // fw 8.6.1 reports "wifi"; older docs say "wireless"
        let wifi = or(get(&w, "wifi")).or(get(&w, "wireless"));
        let wifi_obj = wifi.filter(|x| x.is_object());
        if wifi_obj.is_some() || get(&w, "type") == Some(&json!("wifi")) {
            let wifi = wifi_obj.cloned().unwrap_or_else(|| json!({}));
            let sig = obj_or_empty(get(&wifi, "signal"));
            let ch = val(get(&wifi, "channel"));
            let band = match ch.as_i64() {
                Some(n) if is_int(&ch) => json!(if n <= 14 { "2.4 GHz" } else { "5 GHz" }),
                _ => Value::Null,
            };
            row.insert(
                "wifi".into(),
                json!({"ssid": val(get(&wifi, "ssid")), "strength": val(get(&sig, "strength")), "level": val(get(&sig, "level")),
                       "channel": ch, "band": band}),
            );
        }
        out.push(Value::Object(row));
    }
    out
}

/// `status.traffic` → [(wan id, {"down_mbps", "up_mbps"})] from the live per-WAN "overall" bandwidth.
pub fn normalize_bandwidth(resp: &Value) -> Vec<(i64, Value)> {
    let bw = obj_or_empty(get(resp, "bandwidth"));
    let unit = py_str(get(&bw, "unit").unwrap_or(&json!("kbps"))).to_lowercase();
    let scale = match unit.as_str() {
        "bps" => 1e-6,
        "kbps" => 1e-3,
        "mbps" => 1.0,
        _ => 1e-3,
    };
    let mut out = Vec::new();
    for (k, v) in bw.as_object().into_iter().flatten() {
        if k.is_empty() || !k.bytes().all(|c| c.is_ascii_digit()) || !v.is_object() {
            continue;
        }
        let o = obj_or_empty(get(v, "overall"));
        let (d, u) = (get(&o, "download").filter(|x| !x.is_null()), get(&o, "upload").filter(|x| !x.is_null()));
        if d.is_some() || u.is_some() {
            let n = |x: Option<&Value>| or(x).and_then(Value::as_f64).unwrap_or(0.0);
            let Ok(id) = k.parse::<i64>() else { continue };
            out.push((id, json!({"down_mbps": jf(round(n(d) * scale, 3)), "up_mbps": jf(round(n(u) * scale, 3))})));
        }
    }
    out
}

/// The connected WAN with the best (lowest) priority number. Peplink priority 1 = highest.
pub fn active_wan(wans: &[Value]) -> Option<&Value> {
    let mut best: Option<(&Value, f64)> = None;
    for w in wans {
        if w["enabled"] != json!(true) || w["connected"] != json!(true) {
            continue;
        }
        let Some(p) = w.get("priority").and_then(Value::as_f64) else { continue };
        if best.is_none_or(|(_, b)| p < b) {
            best = Some((w, p));
        }
    }
    best.map(|(w, _)| w)
}

/// `status.wan.connection.allowance` → the enabled allowances (per SIM for cellular WANs).
pub fn normalize_allowance(resp: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    for cid in list_or_empty(get(resp, "order")) {
        let conn = obj_or_empty(get(resp, &py_str(&cid)));
        let items: Vec<(Value, Value)> = if get(&conn, "usage").is_some() {
            vec![(Value::Null, conn.clone())] // non-cellular WAN: a single allowance object
        } else {
            list_or_empty(get(&conn, "order")).into_iter().map(|sid| {
                let a = obj_or_empty(get(&conn, &py_str(&sid)));
                (sid, a)
            }).collect()
        };
        for (sid, a) in items {
            if get(&a, "enable").is_some_and(truthy) {
                out.push(json!({"conn": cid, "sim": sid, "usage_mb": val(get(&a, "usage")), "limit_mb": val(get(&a, "limit")),
                                "percent": val(get(&a, "percent")), "start_day": val(get(&a, "start"))}));
            }
        }
    }
    out
}

/// `info.location` → `location.*` (fw 8.0.1+).
pub fn normalize_location(resp: &Value) -> Vec<(String, Value)> {
    let loc = obj_or_empty(get(resp, "location"));
    let fix = get(resp, "gps").is_some_and(truthy) && get(&loc, "latitude").is_some_and(|v| !v.is_null());
    let f = |k: &str| if fix { val(get(&loc, k)) } else { Value::Null };
    vec![
        ("location.fix".into(), json!(fix)),
        ("location.source".into(), json!("peplink")),
        ("location.lat".into(), f("latitude")),
        ("location.lon".into(), f("longitude")),
        ("location.alt_m".into(), f("altitude")),
        ("location.speed".into(), f("speed")), // units undocumented (likely km/h)
        ("location.heading".into(), f("heading")),
        ("location.hdop".into(), f("hdop")),
        ("location.time".into(), f("timestamp")),
    ]
}

// ── the strict WAN-priority builders (a ValueError in Python: nothing is sent) ──────────────────────────────────

fn shape_ok(current: &Value) -> Result<&Vec<Value>, String> {
    match get(current, "order") {
        Some(Value::Array(o)) if !o.is_empty() && o.iter().all(is_int) => Ok(o),
        _ => {
            let s: String = py_str(current).chars().take(200).collect();
            Err(format!("unexpected config.wan.connection.priority shape: {s}"))
        }
    }
}

/// Priority of one connection from GET config.wan.connection.priority, whose shape on this firmware is
/// {"order": [1, 2, …], "1": {"name", "group", "priority", "enable"}, …}. Strict.
pub fn current_priority(current: &Value, conn_id: i64) -> Result<Value, String> {
    let order = shape_ok(current)?;
    let e = get(current, &conn_id.to_string());
    let listed = order.iter().any(|c| c.as_i64() == Some(conn_id));
    match e {
        Some(Value::Object(m)) if listed && m.contains_key("priority") => {
            if !m.get("enable").is_some_and(truthy) {
                return Err(format!("connection {conn_id} is disabled on the router"));
            }
            Ok(val(m.get("priority")))
        }
        _ => Err(format!("connection {conn_id} is not in the router's priority list")),
    }
}

/// One connection's entry from GET config.wan.connection.priority (any enable state). Strict.
pub fn entry(current: &Value, conn_id: i64) -> Result<&Map<String, Value>, String> {
    let order = shape_ok(current)?;
    let listed = order.iter().any(|c| c.as_i64() == Some(conn_id));
    match get(current, &conn_id.to_string()) {
        Some(Value::Object(m)) if listed && m.contains_key("enable") => Ok(m),
        _ => Err(format!("connection {conn_id} is not in the router's priority list")),
    }
}

/// What a connection should become.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub enable: bool,
    pub priority: Option<i64>,
}

impl Target {
    pub fn on(p: i64) -> Target {
        Target { enable: true, priority: Some(p) }
    }
    pub const OFF: Target = Target { enable: false, priority: None };
}

/// Drag-to-reorder: `order` (enabled, top first) becomes P1, P2, P3, … (capped at P4); `disabled` is switched off.
/// Every enabled connection must be named exactly once; connections left out must already be disabled and stay so
/// (with `disabled` omitted, all currently disabled ones stay disabled). At least one stays enabled. Strict.
pub fn layout_targets(current: &Value, order: &[i64], disabled: Option<&[i64]>) -> Result<Vec<(i64, Target)>, String> {
    if order.is_empty() {
        return Err("at least one WAN must stay enabled".into());
    }
    let every: Vec<i64> = order.iter().chain(disabled.unwrap_or(&[])).copied().collect();
    let uniq: std::collections::BTreeSet<i64> = every.iter().copied().collect();
    if uniq.len() != every.len() {
        return Err("a connection is listed twice".into());
    }
    for &c in &every {
        entry(current, c)?;
    }
    let all: Vec<i64> = shape_ok(current)?.iter().filter_map(Value::as_i64).collect();
    let mut disabled: Vec<i64> = match disabled {
        Some(d) => d.to_vec(),
        None => {
            let mut d = Vec::new();
            for &c in &all {
                if !entry(current, c)?.get("enable").is_some_and(truthy) {
                    d.push(c);
                }
            }
            if d.iter().any(|c| order.contains(c)) {
                return Err("order includes a disabled connection".into());
            }
            d
        }
    };
    // Connections the kiosk doesn't show (e.g. "License Required") may be left out, but only if they're disabled now:
    // they simply stay disabled. Leaving out an enabled connection is an error.
    let missing: std::collections::BTreeSet<i64> = all.iter().copied().filter(|c| !order.contains(c) && !disabled.contains(c)).collect();
    for &c in &missing {
        if entry(current, c)?.get("enable").is_some_and(truthy) {
            return Err("order + disabled must list every enabled connection".into());
        }
    }
    disabled.extend(missing);
    let mut out: Vec<(i64, Target)> = order.iter().enumerate().map(|(i, &c)| (c, Target::on((i as i64 + 1).min(4)))).collect();
    out.extend(disabled.into_iter().map(|c| (c, Target::OFF)));
    Ok(out)
}

/// Python `==` between the router's priority value and an int.
fn same_priority(v: Option<&Value>, p: Option<i64>) -> bool {
    match (v.and_then(Value::as_f64), p) {
        (Some(a), Some(b)) => a == b as f64,
        _ => v.is_none_or(Value::is_null) && p.is_none(),
    }
}

/// Only what differs from the router now: enable flips, and priority moves of enabled connections.
pub fn layout_changes(current: &Value, targets: &[(i64, Target)]) -> Result<Vec<(i64, Target)>, String> {
    let mut out = Vec::new();
    for &(c, tg) in targets {
        let e = entry(current, c)?;
        let en_now = e.get("enable").is_some_and(truthy);
        if tg.enable != en_now || (tg.enable && !same_priority(e.get("priority"), tg.priority)) {
            out.push((c, tg));
        }
    }
    Ok(out)
}

/// ONE POST body (Peplink API: list of {connId, priority?, enable?}) for just the changed connections.
pub fn layout_payload(current: &Value, changes: &[(i64, Target)]) -> Result<Value, String> {
    if changes.is_empty() {
        return Err("nothing to change".into());
    }
    let mut list = Vec::new();
    for &(c, tg) in changes {
        let e = entry(current, c)?;
        let mut item = Map::new();
        item.insert("connId".into(), json!(c));
        if tg.enable {
            let p = tg.priority.unwrap_or(0);
            if !(1..=4).contains(&p) {
                return Err("priority must be 1–4".into());
            }
            item.insert("priority".into(), json!(p));
        }
        if tg.enable != e.get("enable").is_some_and(truthy) {
            item.insert("enable".into(), json!(tg.enable));
        }
        list.push(Value::Object(item));
    }
    Ok(json!({"instantActive": true, "list": list}))
}

/// Body for POST config.wan.connection.priority (Peplink API reference): ONLY the connection being changed, so no
/// other link's settings are touched; instantActive applies it without a separate 'apply changes'.
pub fn priority_payload(current: &Value, conn_id: i64, priority: i64) -> Result<Value, String> {
    current_priority(current, conn_id)?; // validates shape + connection
    if !(1..=4).contains(&priority) {
        return Err("priority must be 1–4".into());
    }
    Ok(json!({"instantActive": true, "list": [{"connId": conn_id, "priority": priority}]}))
}

// ── the HTTP client ─────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Client,
    User,
}

#[derive(Default)]
struct Auth {
    token: Option<String>,
    expires: f64,
    /// Cookies the router set (the `bauth` session), name → value.
    cookies: Vec<(String, String)>,
}

/// What a client is built from. `allow_write` only for the separate WAN-priority writer.
#[derive(Clone, Default)]
pub struct ClientConfig {
    pub host: String,
    pub client_id: String,
    pub client_secret: String,
    pub verify_tls: bool,
    pub scheme: String,
    pub username: String,
    pub password: String,
    pub scope: String,
    pub allow_write: bool,
}

impl ClientConfig {
    /// Python's defaults: https, scope `api.read-only`, read-only.
    pub fn new(host: &str) -> ClientConfig {
        ClientConfig { host: host.into(), scheme: "https".into(), scope: "api.read-only".into(), ..Default::default() }
    }
}

pub struct PeplinkClient {
    base: String,
    scope: String,
    allow_write: bool,
    client_id: String,
    client_secret: String,
    username: String,
    password: String,
    pub mode: Mode,
    http: reqwest::Client,
    auth: tokio::sync::Mutex<Auth>,
}

impl std::fmt::Debug for PeplinkClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // never the credentials or the token
        f.debug_struct("PeplinkClient").field("base", &self.base).field("mode", &self.mode).field("allow_write", &self.allow_write).finish()
    }
}

/// A reqwest error without its URL (the query carries the access token).
fn transport(e: reqwest::Error) -> PepError {
    let e = e.without_url();
    let mut s = e.to_string();
    let mut src = std::error::Error::source(&e);
    while let Some(x) = src {
        s += &format!(": {x}");
        src = x.source();
    }
    if e.is_timeout() { PepError::Other(String::new()) } else { PepError::Other(s) }
}

fn code_msg(body: &Value) -> String {
    let f = |k: &str| get(body, k).map(py_str).unwrap_or_else(|| "None".into());
    format!("{} {}", f("code"), f("message"))
}

impl PeplinkClient {
    pub fn new(c: ClientConfig) -> PeplinkClient {
        let insecure = !c.verify_tls && c.scheme == "https"; // the router uses a self-signed cert on the van LAN
        // No request is ever repeated by the HTTP stack (aiohttp doesn't either): no redirects, no protocol-nack
        // retries. The writer also keeps no idle connection, so hyper's re-send of an unstarted request on a reused
        // connection can't apply to its one POST.
        let mut b = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .danger_accept_invalid_certs(insecure)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never());
        if c.allow_write {
            b = b.pool_max_idle_per_host(0);
        }
        let http = b.build().expect("http client");
        PeplinkClient {
            base: format!("{}://{}/api", c.scheme, c.host),
            scope: c.scope,
            allow_write: c.allow_write,
            mode: if c.username.is_empty() { Mode::Client } else { Mode::User },
            client_id: c.client_id,
            client_secret: c.client_secret,
            username: c.username,
            password: c.password,
            http,
            auth: tokio::sync::Mutex::new(Auth::default()),
        }
    }

    fn url(&self, func: &str, params: &[(&str, &str)]) -> Result<reqwest::Url, PepError> {
        let mut u = reqwest::Url::parse(&format!("{}/{func}", self.base)).map_err(|e| PepError::Other(e.to_string()))?;
        if !params.is_empty() {
            let mut q = u.query_pairs_mut();
            for (k, v) in params {
                q.append_pair(k, v);
            }
        }
        Ok(u)
    }

    fn cookie_header(a: &Auth) -> Option<String> {
        (!a.cookies.is_empty()).then(|| a.cookies.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; "))
    }

    fn take_cookies(a: &mut Auth, resp: &reqwest::Response) {
        for h in resp.headers().get_all(reqwest::header::SET_COOKIE) {
            let Ok(s) = h.to_str() else { continue };
            let mut parts = s.split(';');
            let Some((k, v)) = parts.next().and_then(|kv| kv.split_once('=')) else { continue };
            let (k, v) = (k.trim().to_string(), v.trim().trim_matches('"').to_string());
            let gone = v.is_empty() || parts.any(|p| p.trim().eq_ignore_ascii_case("max-age=0"));
            a.cookies.retain(|(n, _)| *n != k);
            if !gone {
                a.cookies.push((k, v));
            }
        }
    }

    async fn send(&self, a: &mut Auth, rb: reqwest::RequestBuilder) -> Result<Value, PepError> {
        let rb = match Self::cookie_header(a) {
            Some(c) => rb.header(reqwest::header::COOKIE, c),
            None => rb,
        };
        let resp = rb.send().await.map_err(transport)?;
        Self::take_cookies(a, &resp);
        let bytes = resp.bytes().await.map_err(transport)?;
        serde_json::from_slice(&bytes).map_err(|e| PepError::Json(e.to_string()))
    }

    async fn grant(&self, a: &mut Auth) -> Result<(), PepError> {
        if self.mode == Mode::User {
            return self.login(a).await;
        }
        let rb = self.http.post(self.url("auth.token.grant", &[])?).json(&json!({
            "clientId": self.client_id, "clientSecret": self.client_secret, "scope": self.scope}));
        let body = self.send(a, rb).await?;
        if get(&body, "stat") != Some(&json!("ok")) {
            return Err(PepError::Api(format!("token grant failed: {}", code_msg(&body))));
        }
        let resp = val(get(&body, "response"));
        let tok = match get(&resp, "accessToken") {
            Some(Value::String(t)) => t.clone(),
            Some(other) => py_str(other),
            None => return Err(PepError::Other("'accessToken'".into())),
        };
        let exp = match get(&resp, "expiresIn") {
            None => 3600.0,
            Some(Value::String(s)) => s.trim().parse().map_err(|_| PepError::Json(format!("could not convert string to float: '{s}'")))?,
            Some(v) => v.as_f64().ok_or_else(|| PepError::Other("float() argument must be a string or a real number".into()))?,
        };
        a.token = Some(tok);
        a.expires = now_s() + exp - 120.0;
        Ok(())
    }

    async fn login(&self, a: &mut Auth) -> Result<(), PepError> {
        let rb = self.http.post(self.url("login", &[])?).json(&json!({"username": self.username, "password": self.password}));
        let body = self.send(a, rb).await?;
        if get(&body, "stat") != Some(&json!("ok")) {
            return Err(PepError::Api(format!("token grant failed (login): {}", code_msg(&body))));
        }
        let perm = obj_or_empty(get(&obj_or_empty(get(&body, "response")), "permission"));
        if get(&perm, "POST").is_some_and(truthy) && !self.allow_write {
            // Refuse to run with write rights: monitoring must use the router's read-only user. Don't keep that session.
            a.cookies.clear();
            return Err(PepError::Api("login has POST (write) permission; configure the Peplink READ-ONLY user instead".into()));
        }
        a.token = Some("session".into());
        a.expires = now_s() + 3000.0; // re-login well before typical web-admin idle timeouts
        Ok(())
    }

    /// GET `func`: the `response` object. A 401/403 answer re-grants (or re-logs in) once.
    pub async fn get_with(&self, func: &str, params: &[(&str, &str)]) -> Result<Value, PepError> {
        let mut a = self.auth.lock().await;
        for attempt in 1..=2 {
            if a.token.is_none() || now_s() > a.expires {
                self.grant(&mut a).await?;
            }
            let mut q: Vec<(&str, &str)> = params.to_vec();
            let tok = a.token.clone().unwrap_or_default();
            if self.mode == Mode::Client {
                q.push(("accessToken", &tok));
            }
            let rb = self.http.get(self.url(func, &q)?);
            let body = self.send(&mut a, rb).await?;
            if !body.is_object() {
                return Err(PepError::Other("'list' object has no attribute 'get'".into()));
            }
            if get(&body, "stat") == Some(&json!("ok")) {
                return Ok(or(get(&body, "response")).cloned().unwrap_or_else(|| json!({})));
            }
            let code = get(&body, "code").and_then(Value::as_i64);
            if attempt == 1 && matches!(code, Some(401 | 403)) {
                a.token = None; // token expired/revoked → re-grant once
                continue;
            }
            return Err(PepError::Api(format!("{func}: {}", code_msg(&body))));
        }
        Err(PepError::Api(format!("{func}: unauthorized")))
    }

    /// One POST, never repeated (a token grant beforehand isn't a send). Only a client built to write may.
    pub async fn post_once_raw(&self, func: &str, body: &Value) -> Result<Value, PepError> {
        if !self.allow_write || (self.mode == Mode::Client && self.scope != "api") {
            return Err(PepError::Api("writes need the read-write API client or user".into()));
        }
        let mut a = self.auth.lock().await;
        if a.token.is_none() || now_s() > a.expires {
            self.grant(&mut a).await?;
        }
        let tok = a.token.clone().unwrap_or_default();
        let params: Vec<(&str, &str)> = if self.mode == Mode::Client { vec![("accessToken", &tok)] } else { vec![] };
        let mut rb = self.http.post(self.url(func, &params)?).json(body);
        if self.mode == Mode::User {
            // fw 8.6.1: a cookie-session POST is only accepted as a web-admin request with X-Requested-With
            // (otherwise it's treated as token auth → "401 Unauthorized"). Verified with a no-op POST, 2026-11-21.
            let origin = self.base.strip_suffix("/api").unwrap_or(&self.base).to_string();
            rb = rb.header("X-Requested-With", "XMLHttpRequest").header("Origin", &origin).header("Referer", format!("{origin}/"));
        }
        let resp = self.send(&mut a, rb).await?;
        if get(&resp, "stat") != Some(&json!("ok")) {
            return Err(PepError::Api(format!("{func}: {}", code_msg(&resp))));
        }
        Ok(or(get(&resp, "response")).cloned().unwrap_or_else(|| json!({})))
    }

    /// Tests: the router revoked or expired the token / session.
    #[doc(hidden)]
    pub async fn force_token(&self, t: Option<&str>) {
        self.auth.lock().await.token = t.map(str::to_string);
    }

    /// Tests: the session cookie is gone (Python `cookie_jar.clear()`).
    #[doc(hidden)]
    pub async fn clear_cookies(&self) {
        self.auth.lock().await.cookies.clear();
    }
}

impl Router for PeplinkClient {
    fn get<'a>(&'a self, func: &'a str) -> PepFut<'a, Value> {
        Box::pin(self.get_with(func, &[]))
    }
    fn post_once<'a>(&'a self, func: &'a str, body: &'a Value) -> PepFut<'a, Value> {
        Box::pin(self.post_once_raw(func, body))
    }
}

// ── the service ─────────────────────────────────────────────────────────────────────────────────────────────────

async fn within<T>(secs: f64, f: impl Future<Output = Result<T, PepError>>) -> Result<T, PepError> {
    match tokio::time::timeout(Duration::from_secs_f64(secs), f).await {
        Ok(r) => r,
        Err(_) => Err(PepError::Other(String::new())), // str(TimeoutError()) is empty
    }
}

/// A strict id/priority: a JSON integer (Python's `int()` would also take 1.5, "1" or True).
fn strict_int(v: Option<&Value>) -> Option<i64> {
    v.and_then(Value::as_i64)
}

fn strict_ints(v: Option<&Value>) -> Option<Vec<i64>> {
    v?.as_array()?.iter().map(|x| x.as_i64()).collect()
}

pub struct Peplink {
    pub state: Arc<StateStore>,
    /// Read-only: all monitoring.
    pub client: Arc<dyn Router>,
    /// Read-write: WAN priority only.
    pub writer: Option<Arc<dyn Router>>,
    pub gate: Arc<TxGate>,
    pub log: CommandLog,
    /// Publish `location.*` (off when the GPS service owns them, `[gps] enabled`).
    pub publish_location: bool,
    cmd_lock: tokio::sync::Mutex<()>,
}

impl Peplink {
    pub fn new(state: Arc<StateStore>, client: Arc<dyn Router>, writer: Option<Arc<dyn Router>>, gate: Arc<TxGate>, log: CommandLog) -> Peplink {
        state.set("peplink.controls_ok", json!(writer.is_some() && gate.is_enabled(DEVICE)));
        Peplink { state, client, writer, gate, log, publish_location: true, cmd_lock: tokio::sync::Mutex::new(()) }
    }

    /// Python `_log`: the value as `json.dumps(value)`.
    fn log(&self, cmd: &str, value: &Value, outcome: &str, detail: &str, client: &str) {
        self.log.log(cmd, &Value::String(json_dumps(value)), outcome, detail, client);
    }

    /// set_wan_priority {id, priority} | set_wan_order {order: [ids], disabled?: [ids]} (Python defaults: verify 20 s,
    /// poll every 2 s).
    pub async fn command(&self, cmd: &str, value: &Value, client: &str) -> Reply {
        self.command_with(cmd, value, client, 20.0, 2.0).await
    }

    /// Fresh read of the router's priority list → one POST with just the changed connections → verify.
    pub async fn command_with(&self, cmd: &str, value: &Value, client: &str, verify_timeout: f64, poll: f64) -> Reply {
        let _g = self.cmd_lock.lock().await;
        if let Err(e) = self.gate.check(DEVICE, cmd, false) {
            self.log(cmd, value, "blocked", &e.to_string(), client);
            return Reply::refused("control is disabled for the Peplink router");
        }
        if !COMMANDS.contains(&cmd) {
            self.log(cmd, value, "rejected", "not an allowed command", client);
            return Reply::refused(format!("command {} is not allowed", py_repr(&json!(cmd))));
        }
        let Some(writer) = self.writer.clone() else {
            self.log(cmd, value, "rejected", "no read-write API client", client);
            return Reply::refused("WAN control isn't set up (no read-write API client)");
        };
        enum Req {
            Priority(i64, i64),
            Order(Vec<i64>, Option<Vec<i64>>),
        }
        let req = if cmd == "set_wan_priority" {
            strict_int(get(value, "id")).zip(strict_int(get(value, "priority"))).map(|(c, p)| Req::Priority(c, p))
        } else if value.is_object() {
            let dis = get(value, "disabled").filter(|d| !d.is_null());
            match (strict_ints(get(value, "order")), dis) {
                (Some(o), None) => Some(Req::Order(o, None)),
                (Some(o), Some(d)) => strict_ints(Some(d)).map(|d| Req::Order(o, Some(d))),
                _ => None,
            }
        } else {
            None
        };
        let Some(req) = req else {
            self.log(cmd, value, "rejected", "bad value", client);
            return Reply::refused("bad request");
        };
        let cur = match within(10.0, writer.get(PRIORITY_FUNC)).await {
            // fresh
            Ok(c) => c,
            Err(PepError::Json(e)) => {
                // Python: a JSONDecodeError is a ValueError
                self.log(cmd, value, "rejected", &e, client);
                return Reply::refused(format!("not sent: {e}"));
            }
            Err(e) => {
                self.log(cmd, value, "failed", &format!("read: {e}"), client);
                return Reply::refused("router not reachable; nothing sent");
            }
        };
        type Plan = (Vec<(i64, Target)>, Vec<(i64, Target)>, Option<Value>);
        let planned: Result<Plan, String> = (|| {
            let targets = match &req {
                Req::Priority(c, p) => {
                    current_priority(&cur, *c)?; // must exist and be enabled
                    if !(1..=4).contains(p) {
                        return Err("priority must be 1–4".into());
                    }
                    vec![(*c, Target::on(*p))]
                }
                Req::Order(o, d) => layout_targets(&cur, o, d.as_deref())?,
            };
            let changed = layout_changes(&cur, &targets)?;
            let body = if changed.is_empty() { None } else { Some(layout_payload(&cur, &changed)?) };
            Ok((targets, changed, body))
        })();
        let (targets, changed, body) = match planned {
            Ok(x) => x,
            Err(e) => {
                self.log(cmd, value, "rejected", &e, client);
                return Reply::refused(format!("not sent: {e}"));
            }
        };
        let name = |c: i64| -> String {
            match or(get(&obj_or_empty(get(&cur, &c.to_string())), "name")) {
                Some(Value::String(s)) => s.clone(),
                Some(o) => py_str(o),
                None => format!("WAN {c}"),
            }
        };
        let Some(body) = body else {
            self.log(cmd, value, "noop", "already set", client);
            return Reply::ok(if cmd == "set_wan_order" {
                "already in that order".to_string()
            } else {
                format!("already P{}", targets[0].1.priority.unwrap_or(0))
            });
        };
        let desc = |c: i64, tg: Target| -> String {
            let e = obj_or_empty(get(&cur, &c.to_string()));
            if !tg.enable {
                return format!("{} disabled", name(c));
            }
            let from = if !get(&e, "enable").is_some_and(truthy) {
                "enabled at ".to_string()
            } else {
                format!("P{}→", py_str(&val(get(&e, "priority"))))
            };
            format!("{} {from}P{}", name(c), tg.priority.unwrap_or(0))
        };
        // once; it may still have applied when it raised: verify, never resend
        let send_err = within(10.0, writer.post_once(PRIORITY_FUNC, &body)).await.err();
        let what = changed.iter().map(|&(c, tg)| desc(c, tg)).collect::<Vec<_>>().join(", ");
        let end = tokio::time::Instant::now() + Duration::from_secs_f64(verify_timeout);
        while tokio::time::Instant::now() < end {
            if poll > 0.0 {
                tokio::time::sleep(Duration::from_secs_f64(poll)).await;
            } else {
                tokio::task::yield_now().await;
            }
            let Ok(resp) = within(10.0, writer.get("status.wan.connection")).await else { continue }; // busy switching links
            let wans = normalize_wans(&resp);
            let seen = changed.iter().all(|&(c, tg)| {
                wans.iter().find(|w| w["id"].as_i64() == Some(c) && is_int(&w["id"])).is_some_and(|w| {
                    w["enabled"] == json!(tg.enable) && (!tg.enable || same_priority(w.get("priority"), tg.priority))
                })
            });
            if seen {
                let _ = self.wan().await;
                let mut d = what.clone();
                if let Some(e) = &send_err {
                    d += &format!(" (send raised {e})");
                }
                self.log(cmd, value, "ok", &d, client);
                if cmd == "set_wan_priority" {
                    let (c, tg) = changed[0];
                    return Reply::ok(format!("{} is now P{}", name(c), tg.priority.unwrap_or(0)));
                }
                return Reply::ok(what);
            }
        }
        let send = match &send_err {
            Some(e) => format!("error: {e}"),
            None => "ok".into(),
        };
        self.log(cmd, value, "unverified", &format!("{what}; send={send}; not seen"), client);
        Reply::unverified("the router didn't confirm the change (not retried)")
    }

    /// The WAN list, the active WAN and its live throughput (one continuous history series, whichever WAN carries
    /// traffic).
    pub async fn wan(&self) -> Result<(), PepError> {
        let wans = normalize_wans(&self.client.get("status.wan.connection").await?);
        let act = active_wan(&wans).cloned();
        let mut up: Vec<(String, Value)> = vec![
            ("peplink.reachable".into(), json!(true)),
            ("peplink.wans".into(), Value::Array(wans.clone())),
            ("peplink.active".into(), act.as_ref().map(|a| a["name"].clone()).unwrap_or(Value::Null)),
            (
                "peplink.active_type".into(),
                act.as_ref().map(|a| if a["starlink"] == json!(true) { json!("starlink") } else { a["type"].clone() }).unwrap_or(Value::Null),
            ),
            ("peplink.updated".into(), jf(round(now_s(), 1))),
        ];
        let bw = match self.client.get("status.traffic").await {
            Ok(r) => normalize_bandwidth(&r),
            Err(PepError::Api(e)) => {
                tracing::debug!("peplink status.traffic failed: {e}");
                Vec::new()
            }
            Err(e) => return Err(e),
        };
        let cur = act.as_ref().and_then(|a| a["id"].as_i64().filter(|_| is_int(&a["id"]))).and_then(|id| bw.iter().find(|(k, _)| *k == id)).map(|(_, v)| v.clone());
        up.push(("wan.down_mbps".into(), cur.as_ref().map(|c| c["down_mbps"].clone()).unwrap_or(Value::Null)));
        up.push(("wan.up_mbps".into(), cur.as_ref().map(|c| c["up_mbps"].clone()).unwrap_or(Value::Null)));
        self.state.update(up);
        Ok(())
    }

    /// One poll tick (Python `run()` body): WANs every tick, location every 4th (~60 s), allowance every 20th (~5 min).
    pub async fn tick(&self, tick: u64) {
        let r: Result<(), PepError> = async {
            self.wan().await?;
            if tick.is_multiple_of(4) {
                let loc = normalize_location(&self.client.get("info.location").await?);
                if self.publish_location {
                    self.state.update(loc);
                }
            }
            if tick.is_multiple_of(20) {
                let a = normalize_allowance(&self.client.get("status.wan.connection.allowance").await?);
                self.state.set("peplink.allowance", Value::Array(a));
            }
            Ok(())
        }
        .await;
        if let Err(e) = r {
            tracing::debug!("peplink poll failed: {e}");
            self.state.set("peplink.reachable", json!(false));
            if matches!(&e, PepError::Api(s) if s.contains("token grant")) {
                tracing::warn!("peplink: {e} (check the read-only client ID/secret)");
            }
        }
    }

    pub async fn run_loop(self: Arc<Self>, every: Duration) {
        let mut tick = 0u64;
        loop {
            self.tick(tick).await;
            tick += 1;
            tokio::time::sleep(every).await;
        }
    }
}

struct Handler(Arc<Peplink>);

impl CommandHandler for Handler {
    fn handle(&self, cmd: String, value: Value, client: String) -> BoxFuture<Reply> {
        let me = self.0.clone();
        Box::pin(async move { me.command(&cmd, &value, &client).await })
    }
}

pub struct Options {
    pub bus_socket: Option<PathBuf>,
    pub db_path: Option<PathBuf>,
    /// Python polls every 15 s.
    pub every: Duration,
}

impl Options {
    pub fn from_config(cfg: &Cfg) -> Options {
        Options {
            bus_socket: None,
            db_path: Some(cfg.at("history").str("path", "/var/lib/aetherrv/history.sqlite3").into()),
            every: Duration::from_secs(15),
        }
    }
}

/// The monitoring client and the optional writer from the config and the secrets file (Python `__main__`).
/// `None` when the host or the read-only credentials are missing.
pub fn clients_from(dev: &Cfg, sec: &Cfg) -> Option<(PeplinkClient, Option<PeplinkClient>)> {
    let s = |k: &str| sec.str(k, "");
    let host = dev.str("host", "");
    let (cid, csec, user, pw) = (s("peplink_client_id"), s("peplink_client_secret"), s("peplink_ro_user"), s("peplink_ro_password"));
    if host.is_empty() || !((!user.is_empty() && !pw.is_empty()) || (!cid.is_empty() && !csec.is_empty())) {
        return None;
    }
    let vt = dev.bool("verify_tls", false);
    // WAN priority control uses a SEPARATE read-write client; monitoring stays read-only. Either a read-write API
    // client (scope "api"), or (firmware that won't create clients) a read-write web user.
    let (rw_id, rw_sec, rw_user, rw_pw) = (s("peplink_rw_client_id"), s("peplink_rw_client_secret"), s("peplink_rw_user"), s("peplink_rw_password"));
    let writer = if !rw_id.is_empty() && !rw_sec.is_empty() {
        Some(PeplinkClient::new(ClientConfig { client_id: rw_id, client_secret: rw_sec, verify_tls: vt, scope: "api".into(), allow_write: true, ..ClientConfig::new(&host) }))
    } else if !rw_user.is_empty() && !rw_pw.is_empty() {
        Some(PeplinkClient::new(ClientConfig { verify_tls: vt, username: rw_user, password: rw_pw, allow_write: true, ..ClientConfig::new(&host) }))
    } else {
        None
    };
    let client = PeplinkClient::new(ClientConfig { client_id: cid, client_secret: csec, verify_tls: vt, username: user, password: pw, ..ClientConfig::new(&host) });
    Some((client, writer))
}

/// Run until SIGTERM/SIGINT. Disabled, or without host / credentials, it stays up and polls nothing (Python didn't
/// create the adapter); `tx.enabled.peplink` is published either way (Python's `tx.enabled` listed every device).
pub async fn run(cfg: Cfg, opts: Options) -> anyhow::Result<()> {
    let dev = cfg.at("device.peplink");
    let state = Arc::new(StateStore::new());
    let mut bo = BusOptions::new(aetherrv_core::names::PEPLINK);
    if let Some(s) = opts.bus_socket {
        bo = bo.socket(s);
    }
    let bus = Bus::start(bo, state.clone());
    let gate = Arc::new(TxGate::from_config(&cfg).only(&[DEVICE]));
    {
        let (state, gate) = (state.clone(), gate.clone());
        tokio::spawn(async move {
            loop {
                let blocked = gate.blocked_count();
                state.update([
                    (format!("tx.enabled.{DEVICE}"), json!(gate.is_enabled(DEVICE))),
                    (format!("tx.blocked.{DEVICE}"), json!(blocked.get(DEVICE).copied().unwrap_or(0))),
                ]);
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
    }
    let clients = if dev.bool("enabled", false) {
        let sec = aetherrv_core::config::load_secrets(&cfg);
        let c = clients_from(&dev, &sec);
        if c.is_none() {
            tracing::warn!("peplink enabled but host / credentials missing; skipping");
        }
        c
    } else {
        tracing::info!("peplink: disabled ([device.peplink] enabled = false); idle");
        None
    };
    if let Some((client, writer)) = clients {
        let log = CommandLog::new(DEVICE, opts.db_path);
        let writer: Option<Arc<dyn Router>> = writer.map(|w| Arc::new(w) as Arc<dyn Router>);
        let mut pep = Peplink::new(state.clone(), Arc::new(client), writer, gate.clone(), log.clone());
        // location.* belongs to the GPS service when its NMEA receiver is on (names.rs); Python wrote it from both
        pep.publish_location = !cfg.at("gps").bool("enabled", false);
        let me = Arc::new(pep);
        bus.serve(DEVICE, Arc::new(Handler(me.clone())), Some(log)).await;
        tokio::spawn(me.clone().run_loop(opts.every));
        tracing::info!(
            "peplink: polling the router API (controls {})",
            if me.writer.is_some() && gate.is_enabled(DEVICE) { "on" } else { "off" }
        );
    }
    aetherrv_core::notify::ready();
    aetherrv_core::notify::spawn_runtime_watchdog();
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = term.recv() => {},
        _ = tokio::signal::ctrl_c() => {},
    }
    aetherrv_core::notify::stopping();
    bus.shutdown().await;
    Ok(())
}
