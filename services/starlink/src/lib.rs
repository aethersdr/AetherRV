//! `aetherrv-starlink`: the Starlink dish over its local gRPC API (Python `adapters/starlink.py`).
//!
//! Polls status (and signal quality) every `interval`, the obstruction map, history (power, events) and the dish
//! config once a minute, and publishes `starlink.*`. The protobufs come from the dish by server reflection
//! ([`grpc`]); nothing is compiled in.
//!
//! Controls (owner-approved 2026-11-22; TxGate device "starlink", `tx_enabled`): each reads fresh state first,
//! sends exactly once on a brand-new channel, never retries, and verifies:
//!   * install_update: reboot ONLY while a downloaded update waits (software_update_state REBOOT_REQUIRED)
//!   * reboot:         plain reboot; verified by the dish going away or its uptime resetting
//!   * check_update:   `update` request WITHOUT schedule_reboot (the dish checks/downloads, never reboots)
//!   * set_heating:    snow-melt mode auto | on | off (dish_set_config)
//!   * set_sleep:      sleep schedule {enabled, start_utc_min, duration_min} (dish_set_config power_save_*)
//!
//! Not used: stow, GPS inhibit, factory reset, clear obstruction map, test modes. Location is not available on the
//! dish (removed from gRPC for most plans in May 2026); the Peplink GPS is used.

pub mod grpc;

use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::clock::now_s;
use aetherrv_core::cmdlog::CommandLog;
use aetherrv_core::command::{BoxFuture, CommandHandler, Reply};
use aetherrv_core::config::{Cfg, truthy};
use aetherrv_core::num::{jf, jof, round, round_int};
use aetherrv_core::pyfmt::py_str;
use aetherrv_core::state::StateStore;
use aetherrv_core::txgate::TxGate;
use serde_json::{Map, Value, json};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const DEVICE: &str = "starlink";
pub const COMMANDS: [&str; 5] = ["install_update", "reboot", "check_update", "set_heating", "set_sleep"];
/// Seconds between obstruction-map fetches (the map only accumulates slowly).
pub const MAP_EVERY: Duration = Duration::from_secs(60);

/// Dish alerts we surface, with the severity the alert center should use (`None`: informational). Unknown alert
/// names (newer firmware) still show in the list as "warning".
pub const ALERT_SEVERITY: &[(&str, Option<&str>)] = &[
    ("alert_motors_stuck", Some("serious")),
    ("alert_thermal_shutdown", Some("serious")),
    ("alert_thermal_throttle", Some("warning")),
    ("alert_mast_not_near_vertical", Some("warning")),
    ("alert_unexpected_location", Some("warning")),
    ("alert_slow_ethernet_speeds", Some("warning")),
    ("alert_power_supply_thermal_throttle", Some("warning")),
    ("alert_dish_water_detected", Some("serious")),
    ("alert_no_ethernet_link", Some("serious")),
    ("alert_install_pending", Some("warning")),
    ("alert_is_heating", None),         // snow melt / heating, not a fault
    ("alert_is_power_save_idle", None), // sleep schedule
];

/// `MapReferenceFrame`: FRAME_EARTH (fixed plans, top = north), FRAME_UT (mobile/roam: bottom edge = the direction
/// the dish faces, i.e. direction_azimuth).
pub fn frame_name(frame: i64) -> &'static str {
    match frame {
        1 => "earth",
        2 => "dish",
        _ => "unknown",
    }
}

/// `DishConfig.SnowMeltMode` AUTO / ALWAYS_ON / ALWAYS_OFF.
pub fn snow_melt(n: i64) -> Option<&'static str> {
    match n {
        0 => Some("auto"),
        1 => Some("on"),
        2 => Some("off"),
        _ => None,
    }
}

/// Python `str.capitalize()`.
fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c.flat_map(|x| x.to_lowercase())).collect(),
        None => String::new(),
    }
}

pub fn pretty_alert(name: &str) -> String {
    capitalize(&name.strip_prefix("alert_").unwrap_or(name).replace('_', " "))
}

/// `_r(v, n)`: `None` stays `None`, else `round(float(v), n)`.
fn r(v: Option<&Value>, n: usize) -> Value {
    match v.and_then(Value::as_f64) {
        Some(x) => jf(round(x, n)),
        None => Value::Null,
    }
}

/// `(status.get(k) or 0)` as a float.
fn or0(v: Option<&Value>) -> f64 {
    v.filter(|x| truthy(x)).and_then(Value::as_f64).unwrap_or(0.0)
}

/// The signal fields of the raw dish status: `None` where this firmware's message has no such field.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SignalRaw {
    pub signal_quality: Option<f64>,
    pub is_snr_above_noise_floor: Option<bool>,
    pub is_snr_persistently_low: Option<bool>,
}

/// Signal quality from the raw dish status (newer firmware): signal_quality 0..1 = SNR/10 dB, clamped, so 1.0 means
/// SNR >= 10 dB. Absent on older firmware → None. (proto3 omits a 0.0 on the wire, so support is judged from the
/// reflected message definition, which [`grpc::status_data`] does.)
pub fn signal_fields(raw: Option<&SignalRaw>) -> Vec<(String, Value)> {
    let q = raw.and_then(|x| x.signal_quality);
    let ob = |b: Option<bool>| b.map(Value::Bool).unwrap_or(Value::Null);
    vec![
        ("starlink.signal_quality".into(), jof(q.map(|q| round(q, 3)))),
        ("starlink.snr_db".into(), jof(q.map(|q| round(q.clamp(0.0, 1.0) * 10.0, 1)))),
        ("starlink.snr_capped".into(), q.map(|q| Value::Bool(q >= 1.0)).unwrap_or(Value::Null)),
        ("starlink.snr_above_noise".into(), ob(raw.and_then(|x| x.is_snr_above_noise_floor))),
        ("starlink.snr_persistently_low".into(), ob(raw.and_then(|x| x.is_snr_persistently_low))),
    ]
}

/// Map starlink_grpc `status_data()` dicts to AetherRV state keys (pure). `alerts` in the dish's field order.
pub fn normalize(status: &Map<String, Value>, _obstruction: &Map<String, Value>, alerts: &[(String, bool)]) -> Vec<(String, Value)> {
    let g = |k: &str| status.get(k).filter(|v| !v.is_null());
    let state = match g("state") {
        Some(v) if truthy(v) => v.clone(),
        _ => json!("UNKNOWN"),
    };
    let alerts: Vec<Value> = alerts
        .iter()
        .filter(|(_, on)| *on)
        .map(|(a, _)| {
            let sev = match ALERT_SEVERITY.iter().find(|(n, _)| n == a) {
                Some((_, s)) => s.map(Value::from).unwrap_or(Value::Null),
                None => json!("warning"),
            };
            json!({"id": a, "label": pretty_alert(a), "severity": sev})
        })
        .collect();
    vec![
        ("starlink.reachable".into(), json!(true)),
        ("starlink.state".into(), state),
        ("starlink.online".into(), json!(g("state").and_then(Value::as_str) == Some("CONNECTED"))),
        ("starlink.uptime".into(), g("uptime").cloned().unwrap_or(Value::Null)),
        ("starlink.software".into(), g("software_version").cloned().unwrap_or(Value::Null)),
        ("starlink.hardware".into(), g("hardware_version").cloned().unwrap_or(Value::Null)),
        ("starlink.latency_ms".into(), r(g("pop_ping_latency_ms"), 1)),
        ("starlink.drop_rate".into(), r(g("pop_ping_drop_rate"), 4)),
        ("starlink.down_mbps".into(), jf(round(or0(g("downlink_throughput_bps")) / 1e6, 2))),
        ("starlink.up_mbps".into(), jf(round(or0(g("uplink_throughput_bps")) / 1e6, 2))),
        ("starlink.obstructed_pct".into(), jf(round(or0(g("fraction_obstructed")) * 100.0, 2))),
        ("starlink.currently_obstructed".into(), json!(g("currently_obstructed").is_some_and(truthy))),
        ("starlink.azimuth".into(), r(g("direction_azimuth"), 1)),
        ("starlink.elevation".into(), r(g("direction_elevation"), 1)),
        ("starlink.gps_sats".into(), g("gps_sats").cloned().unwrap_or(Value::Null)),
        ("starlink.update_state".into(), g("software_update_state").cloned().unwrap_or(Value::Null)),
        ("starlink.alerts".into(), Value::Array(alerts)),
    ]
}

/// The dish's obstruction map as read (`get_obstruction_map`).
#[derive(Debug, Clone, PartialEq)]
pub struct ObstructionMap {
    pub num_rows: i64,
    pub num_cols: i64,
    pub snr: Vec<f64>,
    pub min_elevation_deg: Option<f64>,
    pub max_theta_deg: Option<f64>,
    pub map_reference_frame: Option<i64>,
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding (Python `base64.b64encode`).
pub fn b64encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for ch in data.chunks(3) {
        let n = (ch[0] as u32) << 16 | (*ch.get(1).unwrap_or(&0) as u32) << 8 | *ch.get(2).unwrap_or(&0) as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if ch.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if ch.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Pack the dish's sky map (rows×cols SNR quality, -1 = no data, 0 = obstructed … 1 = clear) into one byte per cell,
/// base64: 255 = no data, else round(snr × 254). 123×123 → ~20 KB of state, refreshed about once a minute.
pub fn encode_obstruction_map(
    rows: i64,
    cols: i64,
    snr: &[f64],
    min_el: Option<f64>,
    max_theta: Option<f64>,
    frame: Option<i64>,
) -> Result<Value, String> {
    if rows * cols != snr.len() as i64 {
        return Err(format!("obstruction map {rows}x{cols} but {} cells", snr.len()));
    }
    let raw: Vec<u8> = snr.iter().map(|&v| if v < 0.0 { 255 } else { round_int(f64::min(1.0, v) * 254.0) as u8 }).collect();
    Ok(json!({"rows": rows, "cols": cols, "min_el": jof(min_el), "max_theta": jof(max_theta),
              "frame": frame_name(frame.unwrap_or(0)), "data": b64encode(&raw)}))
}

/// EVENT_REASON_OUTAGE_NO_DOWNLINK → ("No downlink", true). Second item: is it an outage.
pub fn pretty_reason(name: &str) -> (String, bool) {
    let n = name.strip_prefix("EVENT_REASON_").unwrap_or(name);
    let outage = n.starts_with("OUTAGE_");
    let n = n.strip_prefix("OUTAGE_").unwrap_or(n);
    let n = n.strip_prefix("UT_ALERT_").unwrap_or(n);
    let n = n.strip_prefix("ROUTER_").unwrap_or(n);
    let mut s = capitalize(&n.replace('_', " "));
    for (a, b) in [
        (" snr", " SNR"),
        ("Snr", "SNR"),
        ("ipv4", "IPv4"),
        ("ipv6", "IPv6"),
        (" lan", " LAN"),
        (" wan", " WAN"),
        (" eth", " Ethernet"),
        ("Eth ", "Ethernet "),
        (" dhcp", " DHCP"),
        (" dns", " DNS"),
        (" pop", " PoP"),
        (" rf", " RF"),
        (" ut", " dish"),
    ] {
        s = s.replace(a, b);
    }
    (s, outage)
}

/// One `event_log` entry, with the names of its enum values as this firmware's descriptor has them.
#[derive(Debug, Clone, PartialEq)]
pub struct HistEvent {
    pub reason: i64,
    pub severity: i64,
    pub reason_name: Option<String>,
    pub severity_name: Option<String>,
    pub start_timestamp_ns: i64,
    pub duration_ns: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventLog {
    pub events: Vec<HistEvent>,
    pub start_timestamp_ns: i64,
    pub current_timestamp_ns: i64,
}

/// The parts of `get_history` AetherRV uses. `event_log` is `None` when the firmware has no such field.
#[derive(Debug, Clone, PartialEq)]
pub struct History {
    pub power_in: Vec<f64>,
    pub current: i64,
    pub event_log: Option<EventLog>,
}

/// From the dish's rolling history: average power over the last minute (power_in is 1 Hz, 900 s ring) and the
/// newest events/outages (event_log; device ids deliberately not copied).
pub fn history_fields(h: &History, max_events: usize) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    let pin = &h.power_in;
    if !pin.is_empty() {
        let (n, cur) = (pin.len() as i64, h.current);
        let last: Vec<f64> = if cur != 0 {
            (0..60.min(n).min(cur.max(0))).map(|i| pin[(cur - 1 - i).rem_euclid(n) as usize]).collect()
        } else {
            pin[pin.len().saturating_sub(60)..].to_vec()
        };
        let last: Vec<f64> = last.into_iter().filter(|&x| x != 0.0 && x > 0.0).collect();
        if !last.is_empty() {
            let sum: f64 = last.iter().sum();
            out.push(("starlink.power_w".into(), jf(round(sum / last.len() as f64, 1))));
        }
    }
    if let Some(log) = &h.event_log {
        let mut evs: Vec<(f64, Value)> = log
            .events
            .iter()
            .map(|e| {
                let (rname, sname) = match (&e.reason_name, &e.severity_name) {
                    (Some(r), Some(s)) => (r.clone(), s.clone()),
                    _ => (format!("EVENT_REASON_{}", e.reason), "EVENT_SEVERITY_UNKNOWN".to_string()),
                };
                let (label, outage) = pretty_reason(&rname);
                let t = round(e.start_timestamp_ns as f64 / 1e9, 1);
                let sev = sname.strip_prefix("EVENT_SEVERITY_").unwrap_or(&sname).to_lowercase();
                (t, json!({"t": jf(t), "dur_s": jf(round(e.duration_ns as f64 / 1e9, 1)), "what": label, "outage": outage, "sev": sev}))
            })
            .collect();
        evs.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)); // stable, newest first
        out.push(("starlink.events".into(), Value::Array(evs.into_iter().take(max_events).map(|(_, v)| v).collect())));
        let span = (log.current_timestamp_ns - log.start_timestamp_ns) as f64 / 1e9;
        out.push(("starlink.events_span_s".into(), if span > 0.0 { json!(round_int(span)) } else { Value::Null }));
    }
    out
}

/// The dish settings AetherRV shows (heating, sleep schedule) from DishConfig's fields.
pub fn config_fields(snow_melt_mode: i64, power_save_mode: bool, start_minutes: i64, duration_minutes: i64) -> Value {
    json!({"heating": snow_melt(snow_melt_mode).unwrap_or("auto"),
           "sleep": {"enabled": power_save_mode, "start_utc_min": start_minutes, "duration_min": duration_minutes}})
}

// ── the dish calls a command needs ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct DishError(pub String);

impl std::fmt::Display for DishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DishError {}

pub type DishFut<'a, T> = Pin<Box<dyn Future<Output = Result<T, DishError>> + Send + 'a>>;

/// Python `DishOps`: each call opens its own fresh channel and closes it, so a send can't be repeated behind our
/// back. Tests use a scripted fake.
pub trait DishOps: Send + Sync + 'static {
    /// starlink_grpc `status_data()`'s status dict (`software_update_state`, `uptime`, ...).
    fn status(&self) -> DishFut<'_, Map<String, Value>>;
    fn reboot(&self) -> DishFut<'_, ()>;
    /// [`config_fields`] of the dish config.
    fn get_config(&self) -> DishFut<'_, Value>;
    /// `dish_set_config` with each field and its `apply_<field>` flag.
    fn set_config(&self, fields: Map<String, Value>) -> DishFut<'_, ()>;
    /// The `update` request (no reboot schedule).
    fn check_update(&self) -> DishFut<'_, ()>;
}

/// Python `asyncio.wait_for(…, timeout)`: a timeout is an error whose text is empty (`str(TimeoutError())`).
async fn within<T>(secs: f64, f: impl Future<Output = Result<T, DishError>>) -> Result<T, DishError> {
    match tokio::time::timeout(Duration::from_secs_f64(secs), f).await {
        Ok(r) => r,
        Err(_) => Err(DishError(String::new())),
    }
}

async fn pause(poll: f64) {
    if poll > 0.0 {
        tokio::time::sleep(Duration::from_secs_f64(poll)).await;
    } else {
        tokio::task::yield_now().await;
    }
}

fn deadline(secs: f64) -> tokio::time::Instant {
    tokio::time::Instant::now() + Duration::from_secs_f64(secs)
}

/// Python `KeyError(k)` text.
fn key_err(k: &str) -> String {
    format!("'{k}'")
}

/// A strict integer (a JSON integer; Python's `int()` would also take 420.5, "420" or True).
fn strict_int(v: &Value) -> Option<i64> {
    v.as_i64()
}

pub struct Starlink {
    pub state: Arc<StateStore>,
    pub gate: Arc<TxGate>,
    pub log: CommandLog,
    pub ops: Arc<dyn DishOps>,
    /// How long a heating / sleep change is watched for (Python `_set_config(verify_timeout=15)`).
    pub config_verify_s: f64,
    cmd_lock: tokio::sync::Mutex<()>,
}

impl Starlink {
    pub fn new(state: Arc<StateStore>, gate: Arc<TxGate>, log: CommandLog, ops: Arc<dyn DishOps>) -> Starlink {
        state.set("starlink.controls_ok", json!(gate.is_enabled(DEVICE)));
        Starlink { state, gate, log, ops, config_verify_s: 15.0, cmd_lock: tokio::sync::Mutex::new(()) }
    }

    fn log(&self, cmd: &str, value: &Value, outcome: &str, detail: &str, client: &str) {
        self.log.log(cmd, value, outcome, detail, client);
    }

    /// Fresh read → send once (never retried) → verify (Python defaults: verify 90 s, poll every 3 s).
    pub async fn command(&self, cmd: &str, value: &Value, client: &str) -> Reply {
        self.command_with(cmd, value, client, 90.0, 3.0).await
    }

    pub async fn command_with(&self, cmd: &str, value: &Value, client: &str, verify_timeout: f64, poll: f64) -> Reply {
        let _g = self.cmd_lock.lock().await;
        if let Err(e) = self.gate.check(DEVICE, cmd, false) {
            self.log(cmd, value, "blocked", &e.to_string(), client);
            return Reply::refused("control is disabled for the Starlink dish");
        }
        if !COMMANDS.contains(&cmd) {
            self.log(cmd, value, "rejected", "not an allowed command", client);
            return Reply::refused(format!("command {} is not allowed", aetherrv_core::pyfmt::py_repr(&json!(cmd))));
        }
        if cmd == "set_heating" || cmd == "set_sleep" {
            return self.set_config(cmd, value, client, poll, self.config_verify_s).await;
        }
        let st = match within(15.0, self.ops.status()).await {
            // fresh, never the poll cache
            Ok(s) => s,
            Err(e) => {
                self.log(cmd, value, "failed", &format!("status read: {e}"), client);
                return Reply::refused("dish not reachable; nothing sent");
            }
        };
        let upd = st.get("software_update_state").cloned().unwrap_or(Value::Null);
        if cmd == "check_update" {
            return self.check_update(value, client, &upd, poll, 30.0).await;
        }
        if cmd == "install_update" && upd.as_str() != Some("REBOOT_REQUIRED") {
            self.log(cmd, value, "rejected", &format!("software_update_state={}", py_str(&upd)), client);
            return Reply::refused("no update is waiting to install");
        }
        self.reboot_verify(cmd, value, client, &st, verify_timeout, poll).await
    }

    async fn reboot_verify(&self, cmd: &str, value: &Value, client: &str, st: &Map<String, Value>, verify_timeout: f64, poll: f64) -> Reply {
        let before = st.get("uptime").cloned().unwrap_or(Value::Null);
        let mut send_err = None;
        if let Err(e) = within(20.0, self.ops.reboot()).await {
            // exactly once. The dish may have received it (e.g. a deadline expiring as it goes down): verify, never resend.
            tracing::info!("starlink reboot call raised {e}; verifying instead of retrying");
            send_err = Some(e);
        }
        self.state.set("starlink.restart_requested", jf(round(now_s(), 1)));
        let what = if cmd == "install_update" { "installing the update" } else { "rebooting" };
        let end = deadline(verify_timeout);
        while tokio::time::Instant::now() < end {
            pause(poll).await;
            let now = match within(8.0, self.ops.status()).await {
                Ok(s) => s,
                Err(_) => {
                    if let Some(e) = &send_err {
                        // unreachable after a failed send: can't tell what happened
                        self.log(cmd, value, "unverified", &format!("send raised {e}; dish unreachable since"), client);
                        return Reply::unverified("the request may not have reached the dish (not retried)");
                    }
                    self.log(cmd, value, "ok", &format!("dish went offline after the reboot request (uptime was {}s)", py_str(&before)), client);
                    return Reply::ok(format!("the dish is {what} (offline a few minutes)"));
                }
            };
            let up = now.get("uptime").cloned().unwrap_or(Value::Null);
            if let (Some(b), Some(u)) = (before.as_f64(), up.as_f64())
                && u < b
            {
                self.log(cmd, value, "ok", &format!("uptime reset {}s → {}s", py_str(&before), py_str(&up)), client);
                return Reply::ok(format!("the dish restarted ({what})"));
            }
        }
        self.log(cmd, value, "unverified", &format!("no restart seen within {verify_timeout:.0}s"), client);
        Reply::unverified("the dish did not restart (not retried)")
    }

    /// Python `_check_update(value, client, before, poll, watch=30)`: the `update` request once (no reboot), then
    /// watch `software_update_state`.
    pub async fn check_update(&self, value: &Value, client: &str, before: &Value, poll: f64, watch: f64) -> Reply {
        let send_err = within(15.0, self.ops.check_update()).await.err(); // once, no reboot
        let mut seen = before.clone();
        let end = deadline(watch);
        while tokio::time::Instant::now() < end {
            pause(poll).await;
            match within(8.0, self.ops.status()).await {
                Ok(s) => seen = s.get("software_update_state").cloned().unwrap_or(Value::Null),
                Err(_) => continue,
            }
            if seen != *before || seen.as_str() == Some("REBOOT_REQUIRED") {
                break;
            }
        }
        self.state.set("starlink.update_state", seen.clone());
        self.state.set("starlink.update_checked", jf(round(now_s(), 1)));
        let detail = match seen.as_str() {
            Some("REBOOT_REQUIRED") => "an update is downloaded and ready to install".to_string(),
            Some("IDLE") => "no update available (up to date)".to_string(),
            _ => format!("update state: {}", py_str(&seen).replace('_', " ").to_lowercase()),
        };
        let outcome = if send_err.is_some() { "error" } else { "ok" };
        let mut d = format!("{} → {}", py_str(before), py_str(&seen));
        if let Some(e) = &send_err {
            d += &format!("; send raised {e}");
        }
        self.log("check_update", value, outcome, &d, client);
        if let Some(e) = &send_err
            && seen == *before
        {
            return Reply::unverified(format!("the dish didn't accept the check ({e}); {detail}"));
        }
        // Python answers ok=True here even when the send raised (the state moved, so the check happened)
        Reply::ok(detail)
    }

    /// Python `_set_config(cmd, value, client, poll, verify_timeout=15)`.
    async fn set_config(&self, cmd: &str, value: &Value, client: &str, poll: f64, verify_timeout: f64) -> Reply {
        let parsed: Result<(Value, Map<String, Value>), String> = (|| {
            if cmd == "set_heating" {
                let s = value.as_str().ok_or_else(|| key_err(&py_str(value)))?;
                let mode = match s {
                    "auto" => 0,
                    "on" => 1,
                    "off" => 2,
                    _ => return Err(key_err(s)),
                };
                let mut f = Map::new();
                f.insert("snow_melt_mode".into(), json!(mode));
                Ok((json!({"heating": s}), f))
            } else {
                let o = value.as_object().ok_or_else(|| format!("'{}' object is not subscriptable", py_type(value)))?;
                let en = match o.get("enabled") {
                    None => return Err(key_err("enabled")),
                    Some(Value::Bool(b)) => *b,
                    Some(_) => return Err("enabled must be true or false".into()),
                };
                let int = |k: &str| match o.get(k) {
                    None => Ok(0),
                    Some(v) => strict_int(v).ok_or_else(|| format!("{k} must be a whole number")),
                };
                let (start, dur) = (int("start_utc_min")?, int("duration_min")?);
                if en && !((0..1440).contains(&start) && (1..=1439).contains(&dur)) {
                    return Err("start 0–1439 min, duration 1–1439 min".into());
                }
                let mut f = Map::new();
                f.insert("power_save_mode".into(), json!(en));
                let want = if en {
                    f.insert("power_save_start_minutes".into(), json!(start));
                    f.insert("power_save_duration_minutes".into(), json!(dur));
                    json!({"sleep": {"enabled": en, "start_utc_min": start, "duration_min": dur}})
                } else {
                    json!({"sleep": {"enabled": en}})
                };
                Ok((want, f))
            }
        })();
        let (want, fields) = match parsed {
            Ok(x) => x,
            Err(e) => {
                self.log(cmd, value, "rejected", &format!("bad value: {e}"), client);
                return Reply::refused("bad request");
            }
        };
        let matches = |c: &Value| -> bool {
            if let Some(h) = want.get("heating") {
                return c.get("heating") == Some(h);
            }
            want["sleep"].as_object().unwrap().iter().all(|(k, v)| c["sleep"].get(k) == Some(v))
        };
        let cur = match within(15.0, self.ops.get_config()).await {
            // fresh
            Ok(c) => c,
            Err(e) => {
                self.log(cmd, value, "failed", &format!("config read: {e}"), client);
                return Reply::refused("dish not reachable; nothing sent");
            }
        };
        if matches(&cur) {
            self.log(cmd, value, "noop", "already set", client);
            return Reply::ok("already set");
        }
        // once; it may still have applied when it raised: verify, never resend
        let send_err = within(15.0, self.ops.set_config(fields)).await.err();
        let end = deadline(verify_timeout);
        while tokio::time::Instant::now() < end {
            pause(poll).await;
            let Ok(now) = within(8.0, self.ops.get_config()).await else { continue };
            if matches(&now) {
                self.state.update([("starlink.heating", now["heating"].clone()), ("starlink.sleep", now["sleep"].clone())]);
                let mut d = format!("{} → {}", py_str(&cur), py_str(&now));
                if let Some(e) = &send_err {
                    d += &format!(" (send raised {e})");
                }
                self.log(cmd, value, "ok", &d, client);
                return Reply::ok(if cmd == "set_heating" {
                    format!("heating {}", now["heating"].as_str().unwrap_or_default())
                } else if now["sleep"]["enabled"] == json!(true) {
                    "sleep schedule on".to_string()
                } else {
                    "sleep schedule off".to_string()
                });
            }
        }
        let send = match &send_err {
            Some(e) => format!("error: {e}"),
            None => "ok".into(),
        };
        self.log(cmd, value, "unverified", &format!("send={send}; not seen"), client);
        Reply::unverified("the dish didn't confirm the change (not retried)")
    }
}

fn py_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

// ── the poll ────────────────────────────────────────────────────────────────────────────────────────────────────

/// Python `_poll()`: status every call; map, history and config once a minute (each may fail on its own).
pub struct StatusPoll {
    pub reader: grpc::Poller,
    pub ops: Arc<dyn DishOps>,
    map_at: Option<Instant>,
}

impl StatusPoll {
    pub fn new(reader: grpc::Poller, ops: Arc<dyn DishOps>) -> StatusPoll {
        StatusPoll { reader, ops, map_at: None }
    }

    pub async fn poll_once(&mut self) -> Result<Vec<(String, Value)>, DishError> {
        let resp = self.reader.read("get_status").await?;
        let (status, alerts, raw) = grpc::status_data(&grpc::sub(&resp, "dish_get_status")?);
        let mut out = normalize(&status, &Map::new(), &alerts);
        out.extend(signal_fields(Some(&raw)));
        if self.map_at.is_none_or(|t| t.elapsed() >= MAP_EVERY) {
            self.map_at = Some(Instant::now());
            // older firmware / reflection mismatch: the rest of the card still works
            let m = async {
                let resp = self.reader.read("dish_get_obstruction_map").await?;
                let m = grpc::obstruction_map(&grpc::sub(&resp, "dish_get_obstruction_map")?)?;
                encode_obstruction_map(m.num_rows, m.num_cols, &m.snr, m.min_elevation_deg, m.max_theta_deg, m.map_reference_frame)
                    .map_err(DishError)
            }
            .await;
            match m {
                Ok(v) => out.push(("starlink.obstruction_map".into(), v)),
                Err(e) => tracing::debug!("starlink obstruction map failed: {e}"),
            }
            // power (graph) + events/outages for Settings → Starlink
            let h = async {
                let resp = self.reader.read("get_history").await?;
                grpc::history(&grpc::sub(&resp, "dish_get_history")?)
            }
            .await;
            match h {
                Ok(h) => out.extend(history_fields(&h, 50)),
                Err(e) => tracing::debug!("starlink history read failed: {e}"),
            }
            // heating + sleep schedule shown in Settings → Starlink (a fresh channel, as Python's ops.get_config())
            match self.ops.get_config().await {
                Ok(c) => {
                    out.push(("starlink.heating".into(), c["heating"].clone()));
                    out.push(("starlink.sleep".into(), c["sleep"].clone()));
                }
                Err(e) => tracing::debug!("starlink config read failed: {e}"),
            }
        }
        Ok(out)
    }
}

/// Python `run()`: poll every `interval`; on failure mark the dish unreachable and back off (doubling, 60 s max).
pub async fn poll_loop(state: Arc<StateStore>, mut poll: StatusPoll, interval: f64) {
    let mut backoff = interval;
    loop {
        match tokio::time::timeout(Duration::from_secs(15), poll.poll_once()).await {
            Ok(Ok(mut data)) => {
                data.push(("starlink.updated".into(), jf(round(now_s(), 1))));
                state.update(data);
                backoff = interval;
            }
            r => {
                let e = match r {
                    Ok(Err(e)) => e.to_string(),
                    _ => "timed out".into(),
                };
                tracing::debug!("starlink poll failed: {e}");
                state.update([("starlink.reachable", json!(false)), ("starlink.online", json!(false))]);
                poll.reader.close();
                backoff = (backoff * 2.0).min(60.0);
            }
        }
        tokio::time::sleep(Duration::from_secs_f64(backoff)).await;
    }
}

// ── the service ─────────────────────────────────────────────────────────────────────────────────────────────────

struct Handler(Arc<Starlink>);

impl CommandHandler for Handler {
    fn handle(&self, cmd: String, value: Value, client: String) -> BoxFuture<Reply> {
        let me = self.0.clone();
        Box::pin(async move { me.command(&cmd, &value, &client).await })
    }
}

pub struct Options {
    pub bus_socket: Option<PathBuf>,
    pub db_path: Option<PathBuf>,
}

impl Options {
    pub fn from_config(cfg: &Cfg) -> Options {
        Options { bus_socket: None, db_path: Some(cfg.at("history").str("path", "/var/lib/aetherrv/history.sqlite3").into()) }
    }
}

/// Run until SIGTERM/SIGINT. With `[device.starlink] enabled = false` it stays up and polls nothing (Python didn't
/// create the adapter); `tx.enabled.starlink` is published either way (Python's `tx.enabled` listed every device).
pub async fn run(cfg: Cfg, opts: Options) -> anyhow::Result<()> {
    let dev = cfg.at("device.starlink");
    let state = Arc::new(StateStore::new());
    let mut bo = BusOptions::new(aetherrv_core::names::STARLINK);
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
    if dev.bool("enabled", false) {
        let target = dev.str("target", "192.168.100.1:9200");
        let interval = dev.f64("interval", 5.0);
        let log = CommandLog::new(DEVICE, opts.db_path);
        let cache: grpc::ReflectCache = Default::default();
        let ops: Arc<dyn DishOps> = Arc::new(grpc::GrpcDish::new(&target, cache.clone()));
        let me = Arc::new(Starlink::new(state.clone(), gate.clone(), log.clone(), ops.clone()));
        bus.serve(DEVICE, Arc::new(Handler(me.clone())), Some(log)).await;
        tokio::spawn(poll_loop(state.clone(), StatusPoll::new(grpc::Poller::new(&target, cache), ops), interval));
        tracing::info!("starlink: polling {target} every {interval} s (controls {})", if gate.is_enabled(DEVICE) { "on" } else { "off" });
    } else {
        tracing::info!("starlink: disabled ([device.starlink] enabled = false); idle");
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
