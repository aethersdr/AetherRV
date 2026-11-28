//! `aetherrv-ruuvi`: the Ruuvi tags read directly over Bluetooth LE (owner, 2026-11-27: "Direct only"), READ-ONLY:
//! a tag only broadcasts, and this service only listens (one BlueZ discovery session, Ruuvi advertisements only).
//!
//! **Observe-only for now.** It publishes its own `ruuvi.*` keys and nothing else, so it changes nothing that runs:
//! the cabin, outside, fridge and freezer values still come through the Cerbo. Its job at this stage is to show
//! whether the Pi hears every tag continuously alongside the battery links before anything depends on it.
//!
//! * `ruuvi.tags`: one row per configured tag (heard or not) and per other Ruuvi heard (at most
//!   [`MAX_UNASSIGNED`]): role, short id (the last four hex digits of its address, as Ruuvi names its tags), data
//!   format, temperature, humidity, battery voltage, RSSI, measurement sequence, when it was last heard, how many
//!   advertisements and new measurements, and the longest gaps between them since the service started.
//! * `ruuvi.scan`: the listener's state (`starting`, `scanning`, `error` with the reason, `off` when not configured).
//!
//! Config (the tags' addresses stay in the Pi's own config, never in the repo):
//! ```toml
//! [device.ruuvi]
//! kind = "ruuvi"
//! enabled = true
//! [device.ruuvi.tags]
//! cabin = "AA:BB:CC:DD:EE:01"
//! cabin_rear = "AA:BB:CC:DD:EE:02"
//! ```

pub mod bluez;

use aetherrv_core::clock::{boottime_s, now_s};
use aetherrv_core::config::Cfg;
use aetherrv_core::num::round;
use aetherrv_core::state::StateStore;
use proto_ruuvi::Reading;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Other Ruuvis heard (not in the config) kept at most: a campground can be full of them.
pub const MAX_UNASSIGNED: usize = 16;
/// A gap between advertisements counted in `gaps_over_60`.
pub const LONG_GAP_S: f64 = 60.0;

/// One advertisement as the listener hands it over.
#[derive(Debug, Clone, PartialEq)]
pub struct Advert {
    /// "AA:BB:CC:DD:EE:FF", upper case.
    pub addr: String,
    /// The manufacturer data for company 0x0499.
    pub data: Vec<u8>,
    pub rssi: Option<i16>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// address → role, from `[device.<name>.tags]`.
    pub tags: BTreeMap<String, String>,
}

/// "aa:bb:cc:dd:ee:ff" or "AABBCCDDEEFF" → "AA:BB:CC:DD:EE:FF"; anything else is `None`.
pub fn norm_addr(s: &str) -> Option<String> {
    let hex: String = s.chars().filter(|c| *c != ':' && *c != '-').collect();
    if hex.len() != 12 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let up = hex.to_ascii_uppercase();
    Some((0..6).map(|i| &up[i * 2..i * 2 + 2]).collect::<Vec<_>>().join(":"))
}

impl Settings {
    /// The first enabled `[device.*]` with `kind = "ruuvi"`; `None` when there is none. A tag whose address doesn't
    /// parse is an error (a typo must not silently drop a sensor).
    pub fn from_config(cfg: &Cfg) -> anyhow::Result<Option<Settings>> {
        let Some((name, d)) = cfg.at("device").entries().into_iter().find(|(_, d)| d.str("kind", "") == "ruuvi" && d.bool("enabled", true))
        else {
            return Ok(None);
        };
        let mut tags = BTreeMap::new();
        let empty = serde_json::Map::new();
        let table = match d.get("tags") {
            Some(Value::Object(m)) => m,
            None => &empty,
            Some(_) => anyhow::bail!("[device.{name}] tags must be a table of role = \"address\""),
        };
        for (role, v) in table {
            let raw = v.as_str().unwrap_or_default();
            let a = norm_addr(raw).ok_or_else(|| anyhow::anyhow!("[device.{name}.tags] {role} = {v} is not a Bluetooth address"))?;
            if let Some(other) = tags.insert(a.clone(), role.clone()) {
                anyhow::bail!("[device.{name}.tags]: {role} and {other} have the same address");
            }
        }
        Ok(Some(Settings { tags }))
    }
}

/// What has been heard from one tag since the service started.
#[derive(Debug, Clone, Default)]
pub struct Tag {
    pub role: Option<String>,
    pub reading: Option<Reading>,
    pub rssi: Option<i16>,
    /// Wall clock (s), for display.
    pub last_heard: Option<f64>,
    pub adverts: u64,
    /// New measurements: the sequence number moved (format 5), or the data changed (format 3, which has none).
    pub measurements: u64,
    /// Longest gap between advertisements / between new measurements (s, boot clock).
    pub max_gap_s: f64,
    pub max_new_gap_s: f64,
    pub gaps_over_60: u64,
    pub errors: u64,
    pub last_error: Option<String>,
    last_mono: Option<f64>,
    last_new_mono: Option<f64>,
    last_data: Vec<u8>,
    /// Boot clock of the first advertisement (for the unassigned-tag cap).
    first_mono: f64,
}

pub struct Tracker {
    roles: BTreeMap<String, String>,
    pub tags: BTreeMap<String, Tag>,
}

/// Ruuvi's own short name for a tag: the last four hex digits of its address ("Ruuvi 123F").
pub fn short_id(addr: &str) -> String {
    let hex: String = addr.chars().filter(char::is_ascii_hexdigit).collect();
    hex[hex.len().saturating_sub(4)..].to_ascii_uppercase()
}

impl Tracker {
    pub fn new(s: &Settings) -> Tracker {
        let tags = s.tags.iter().map(|(a, r)| (a.clone(), Tag { role: Some(r.clone()), ..Default::default() })).collect();
        Tracker { roles: s.tags.clone(), tags }
    }

    /// One advertisement, at `wall` / `mono` (boot clock) seconds.
    pub fn on_advert(&mut self, ad: &Advert, wall: f64, mono: f64) {
        if !self.tags.contains_key(&ad.addr) {
            let unassigned: Vec<(String, f64)> =
                self.tags.iter().filter(|(_, t)| t.role.is_none()).map(|(a, t)| (a.clone(), t.first_mono)).collect();
            if unassigned.len() >= MAX_UNASSIGNED {
                // drop the oldest stranger to make room
                if let Some((oldest, _)) = unassigned.iter().min_by(|a, b| a.1.total_cmp(&b.1)) {
                    self.tags.remove(oldest);
                }
            }
            let role = self.roles.get(&ad.addr).cloned();
            self.tags.insert(ad.addr.clone(), Tag { role, first_mono: mono, ..Default::default() });
        }
        let t = self.tags.get_mut(&ad.addr).expect("just inserted");
        if t.first_mono == 0.0 {
            t.first_mono = mono;
        }
        if let Some(prev) = t.last_mono {
            let gap = mono - prev;
            t.max_gap_s = t.max_gap_s.max(gap);
            if gap > LONG_GAP_S {
                t.gaps_over_60 += 1;
            }
        }
        t.last_mono = Some(mono);
        t.last_heard = Some(wall);
        t.adverts += 1;
        if ad.rssi.is_some() {
            t.rssi = ad.rssi;
        }
        match proto_ruuvi::decode(&ad.data) {
            Ok(r) => {
                let new = match (&t.reading, r.sequence) {
                    (Some(old), Some(seq)) => old.sequence != Some(seq),
                    (Some(_), None) => t.last_data != ad.data,
                    (None, _) => true,
                };
                if new {
                    if let Some(prev) = t.last_new_mono {
                        t.max_new_gap_s = t.max_new_gap_s.max(mono - prev);
                    }
                    t.last_new_mono = Some(mono);
                    t.measurements += 1;
                }
                t.last_data = ad.data.clone();
                t.reading = Some(r);
            }
            Err(e) => {
                t.errors += 1;
                t.last_error = Some(e.to_string());
            }
        }
    }

    /// The `ruuvi.tags` rows: configured tags first (by role), then the others (by id).
    pub fn rows(&self, wall: f64) -> Value {
        let mut rows: Vec<(bool, String, Value)> = self
            .tags
            .iter()
            .map(|(addr, t)| {
                let r = t.reading.as_ref();
                let row = json!({
                    "role": t.role,
                    "id": short_id(addr),
                    "heard": t.last_heard.is_some(),
                    "format": r.map(|r| r.format),
                    "temp_c": r.and_then(|r| r.temp_c),
                    "humidity": r.and_then(|r| r.humidity),
                    "battery_v": r.and_then(|r| r.battery_v),
                    "rssi": t.rssi,
                    "sequence": r.and_then(|r| r.sequence),
                    "last_heard": t.last_heard.map(|x| round(x, 1)),
                    "age_s": t.last_heard.map(|x| round(wall - x, 1)),
                    "adverts": t.adverts,
                    "measurements": t.measurements,
                    "max_gap_s": round(t.max_gap_s, 1),
                    "max_new_gap_s": round(t.max_new_gap_s, 1),
                    "gaps_over_60": t.gaps_over_60,
                    "errors": t.errors,
                    "last_error": t.last_error,
                });
                (t.role.is_none(), t.role.clone().unwrap_or_else(|| short_id(addr)), row)
            })
            .collect();
        rows.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        Value::Array(rows.into_iter().map(|(_, _, v)| v).collect())
    }
}

/// The listener's state for `ruuvi.scan`.
#[derive(Debug, Clone, PartialEq)]
pub enum Scan {
    Off,
    Starting,
    Scanning,
    Error(String),
}

impl Scan {
    pub fn to_json(&self, since: f64) -> Value {
        let (state, error) = match self {
            Scan::Off => ("off", None),
            Scan::Starting => ("starting", None),
            Scan::Scanning => ("scanning", None),
            Scan::Error(e) => ("error", Some(e.clone())),
        };
        json!({"state": state, "error": error, "since": round(since, 1)})
    }
}

pub struct Options {
    pub bus_socket: Option<PathBuf>,
}

/// Feed adverts from `rx` into the tracker and publish `ruuvi.tags` / `ruuvi.scan` every second.
pub async fn publish_loop(
    state: Arc<StateStore>,
    tracker: Arc<Mutex<Tracker>>,
    mut rx: tokio::sync::mpsc::Receiver<Advert>,
    scan: tokio::sync::watch::Receiver<(Scan, f64)>,
) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            ad = rx.recv() => match ad {
                Some(ad) => tracker.lock().unwrap().on_advert(&ad, now_s(), boottime_s()),
                None => return,
            },
            _ = tick.tick() => {
                let rows = tracker.lock().unwrap().rows(now_s());
                let (s, since) = scan.borrow().clone();
                state.update([("ruuvi.tags", rows), ("ruuvi.scan", s.to_json(since))]);
            }
        }
    }
}

/// Run the service until SIGTERM/SIGINT.
pub async fn run(cfg: Cfg, opts: Options) -> anyhow::Result<()> {
    use aetherrv_core::bus::{Bus, BusOptions};
    let settings = Settings::from_config(&cfg)?;
    let state = Arc::new(StateStore::new());
    let mut bo = BusOptions::new(aetherrv_core::names::RUUVI);
    if let Some(s) = opts.bus_socket {
        bo = bo.socket(s);
    }
    let bus = Bus::start(bo, state.clone());
    let mut tasks = Vec::new();
    match settings {
        Some(s) => {
            tracing::info!("ruuvi: listening for {} configured tags (observe-only: ruuvi.* keys only)", s.tags.len());
            let tracker = Arc::new(Mutex::new(Tracker::new(&s)));
            let (tx, rx) = tokio::sync::mpsc::channel(256);
            let (scan_tx, scan_rx) = tokio::sync::watch::channel((Scan::Starting, now_s()));
            tasks.push(tokio::spawn(bluez::listen(tx, scan_tx)));
            tasks.push(tokio::spawn(publish_loop(state.clone(), tracker, rx, scan_rx)));
        }
        None => {
            tracing::warn!("ruuvi: no enabled [device.*] with kind = \"ruuvi\"; idle");
            state.set("ruuvi.scan", Scan::Off.to_json(now_s()));
        }
    }
    aetherrv_core::notify::ready();
    aetherrv_core::notify::spawn_runtime_watchdog();
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = term.recv() => {},
        _ = tokio::signal::ctrl_c() => {},
    }
    aetherrv_core::notify::stopping();
    for t in tasks {
        t.abort();
    }
    bus.shutdown().await;
    Ok(())
}
