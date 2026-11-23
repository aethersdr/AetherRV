//! The MCS7 CAN decoder (port of `src/aetherrv/adapters/rixens.py`). READ-ONLY.
//!
//! Frame map source: the community project <https://github.com/mikegoubeaux/rixens-esphome> (MCS7.yaml,
//! Feb 2024). It has no license, so no code is copied; only the documented frame meanings are
//! reimplemented here. It is ONE community source and is UNVERIFIED on this van's MCS7 and firmware.
//! Rixens reportedly shares the official DBC on request, which would supersede this.
//!
//! Bus: 250 kbit/s, 11-bit identifiers. MCS7 board 4-pin CAN port (pins unlabeled; see the MCS7
//! Installation Manual p.10; CAN-H idles ~2.5-3.75 V, CAN-L ~1.25-2.5 V).
//!
//! ```text
//! 0x724  byte 6 bit flags: 0x08 furnace, 0x10 electric, 0x20 engine, 0x80 preheat (selected heat sources)
//! 0x725  byte 3 ×0.1 = burner fuel pump Hz; bytes 4-5 LE = burner fan RPM; bytes 6-7 LE ×0.1 = glow plug W
//! 0x726  bytes 2-3 LE ×0.01 = glycol outlet °C; bytes 6-7 LE ×0.1 = supply V
//! ```
//!
//! Commands TO the MCS7 (ID 0x788, byte 0 = property) are encoded below as pure functions for later use,
//! but NOTHING SENDS THEM. Sending requires TxGate approval (tx_enabled) AND lifting ListenOnly on that bus,
//! both after verification on this van.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::py;

pub const ID_SOURCES: u32 = 0x724;
pub const ID_DIAG: u32 = 0x725;
pub const ID_TEMPS: u32 = 0x726;
pub const ID_COMMAND: u32 = 0x788;

/// 0x724 byte 6: the selected heat sources, in Python's order.
pub const SOURCE_BITS: [(&str, u8); 4] = [("furnace", 0x08), ("electric", 0x10), ("engine", 0x20), ("preheat", 0x80)];

/// Seconds without a decoded frame after which the state reads "no data".
pub const STALE_AFTER: f64 = 10.0;

/// One decoded MCS7 frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Decoded {
    /// 0x724
    Sources { furnace: bool, electric: bool, engine: bool, preheat: bool },
    /// 0x725
    Diag { fuel_hz: f64, fan_rpm: u16, glow_w: f64 },
    /// 0x726
    Temps { glycol_c: f64, voltage: f64 },
}

impl Decoded {
    /// The Python `{field: value}` dict, in its key order.
    pub fn fields(&self) -> Vec<(&'static str, Value)> {
        match *self {
            Decoded::Sources { furnace, electric, engine, preheat } => vec![
                ("source.furnace", json!(furnace)),
                ("source.electric", json!(electric)),
                ("source.engine", json!(engine)),
                ("source.preheat", json!(preheat)),
            ],
            Decoded::Diag { fuel_hz, fan_rpm, glow_w } => {
                vec![("fuel_hz", json!(fuel_hz)), ("fan_rpm", json!(fan_rpm)), ("glow_w", json!(glow_w))]
            }
            Decoded::Temps { glycol_c, voltage } => {
                vec![("glycol_c", json!(glycol_c)), ("voltage", json!(voltage))]
            }
        }
    }

    pub fn to_map(&self) -> Map<String, Value> {
        self.fields().into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }
}

fn u16le(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

/// Decode one MCS7 frame, or `None` if the ID isn't mapped or the frame is shorter than 8 bytes.
pub fn decode(arbitration_id: u32, data: &[u8]) -> Option<Decoded> {
    if data.len() < 8 {
        return None;
    }
    match arbitration_id {
        ID_SOURCES => {
            let bit = |name: &str| SOURCE_BITS.iter().any(|(k, b)| *k == name && data[6] & b != 0);
            Some(Decoded::Sources {
                furnace: bit("furnace"),
                electric: bit("electric"),
                engine: bit("engine"),
                preheat: bit("preheat"),
            })
        }
        ID_DIAG => Some(Decoded::Diag {
            fuel_hz: py::round(data[3] as f64 * 0.1, 1),
            fan_rpm: u16le(data, 4),
            glow_w: py::round(u16le(data, 6) as f64 * 0.1, 1),
        }),
        // Signed on purpose: a cold-soaked loop can be below 0 °C. The source used unsigned.
        ID_TEMPS => Some(Decoded::Temps {
            glycol_c: py::round(i16::from_le_bytes([data[2], data[3]]) as f64 * 0.01, 2),
            voltage: py::round(u16le(data, 6) as f64 * 0.1, 1),
        }),
        _ => None,
    }
}

// ── Command encoders (NOT SENT; for the gated control phase) ─────────────────────────────────────────

/// Why a command could not be encoded (Python raises OverflowError / ValueError / KeyError).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    /// The value doesn't fit the 2-byte field (Python `int.to_bytes` OverflowError) or isn't finite.
    OutOfRange(String),
    /// Not one of furnace, electric, engine, hot_water, preheat (Python KeyError).
    UnknownSource(String),
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodeError::OutOfRange(v) => write!(f, "value {v} does not fit the command"),
            EncodeError::UnknownSource(n) => write!(f, "unknown heat source {n:?}"),
        }
    }
}

impl std::error::Error for EncodeError {}

fn cmd(prop: u8, value: i64, signed: bool) -> Result<[u8; 8], EncodeError> {
    let fits = if signed { (-32768..=32767).contains(&value) } else { (0..=65535).contains(&value) };
    if !fits {
        return Err(EncodeError::OutOfRange(value.to_string()));
    }
    let v = (value as u16).to_le_bytes();
    Ok([prop, v[0], v[1], 0, 0, 0, 0, 0])
}

/// Python `round(celsius * 10)` as an int, refusing what Python can't convert (NaN, ±inf).
fn tenths(celsius: f64) -> Result<i64, EncodeError> {
    let x = celsius * 10.0;
    if !x.is_finite() || x.abs() > 1e18 {
        return Err(EncodeError::OutOfRange(py::float_repr(celsius)));
    }
    Ok(py::round_int(x))
}

pub fn cmd_setpoint(celsius: f64) -> Result<[u8; 8], EncodeError> {
    cmd(0x01, tenths(celsius)?, true)
}

/// `percent` 0-100 (clamped), or `None` for AUTO (0xFF).
pub fn cmd_fan(percent: Option<i64>) -> [u8; 8] {
    let v = percent.map_or(0xFF, |p| p.clamp(0, 100));
    [0x02, v as u8, 0, 0, 0, 0, 0, 0]
}

pub fn cmd_source(name: &str, on: bool) -> Result<[u8; 8], EncodeError> {
    let prop = match name {
        "furnace" => 0x03,
        "electric" => 0x04,
        "engine" => 0x05,
        "hot_water" => 0x06,
        "preheat" => 0x07,
        _ => return Err(EncodeError::UnknownSource(name.to_string())),
    };
    cmd(prop, on as i64, false)
}

pub fn cmd_ambient(celsius: f64) -> Result<[u8; 8], EncodeError> {
    cmd(0x08, tenths(celsius)?, true)
}

pub fn cmd_engine_running(on: bool) -> [u8; 8] {
    [0x0B, on as u8, 0, 0, 0, 0, 0, 0]
}

// ── Adapter state ────────────────────────────────────────────────────────────────────────────────────

/// One received CAN frame, as the service's socket reports it.
#[derive(Debug, Clone, Copy)]
pub struct CanFrame<'a> {
    pub id: u32,
    pub data: &'a [u8],
    pub extended: bool,
    pub error: bool,
    pub remote: bool,
}

impl<'a> CanFrame<'a> {
    /// A plain 11-bit data frame.
    pub fn standard(id: u32, data: &'a [u8]) -> Self {
        Self { id, data, extended: false, error: false, remote: false }
    }
}

#[derive(Debug, Clone)]
struct IdEntry {
    count: u64,
    ext: bool,
    data: String,
    seen: f64,
}

/// The raw ID inventory (Python `CaptureOnlyAdapter`): which arbitration IDs are present, how often each
/// appears and its latest payload. Keyed by the numeric ID; `ext` is from the first sighting.
#[derive(Debug, Clone, Default)]
pub struct IdInventory {
    ids: BTreeMap<u32, IdEntry>,
}

impl IdInventory {
    pub fn on_frame(&mut self, f: &CanFrame<'_>, now: f64) {
        if f.error || f.remote {
            return;
        }
        let e = self.ids.entry(f.id).or_insert(IdEntry { count: 0, ext: f.extended, data: String::new(), seen: now });
        e.count += 1;
        e.data = f.data.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ");
        e.seen = now;
    }

    /// The `<device>.ids` rows, sorted by ID: `{"id", "count", "data", "age"}`.
    pub fn summary(&self, now: f64) -> Value {
        Value::Array(
            self.ids
                .iter()
                .map(|(aid, e)| {
                    let id = if e.ext { format!("{aid:08X}") } else { format!("{aid:03X}") };
                    let age = py::num(Some(py::round(now - e.seen, 1)));
                    json!({"id": id, "count": e.count, "data": e.data, "age": age})
                })
                .collect(),
        )
    }
}

/// The MCS7 CAN adapter's state (Python `RixensAdapter`): decodes the known frames into `climate.rixens.*`
/// and keeps the raw ID inventory too.
#[derive(Debug, Clone)]
pub struct Mcs7Can {
    pub device: String,
    /// False when the HTTP adapter owns `climate.rixens.*` (the CAN bus is then fallback/inventory only).
    pub publish_climate: bool,
    pub inventory: IdInventory,
    values: Map<String, Value>,
    last_decoded: Option<f64>,
}

impl Mcs7Can {
    pub fn new(device: &str, publish_climate: bool) -> Self {
        Self {
            device: device.to_string(),
            publish_climate,
            inventory: IdInventory::default(),
            values: Map::new(),
            last_decoded: None,
        }
    }

    pub fn last_decoded(&self) -> Option<f64> {
        self.last_decoded
    }

    pub fn on_frame(&mut self, f: &CanFrame<'_>, now: f64) {
        self.inventory.on_frame(f, now);
        if f.error || f.remote || f.extended {
            return;
        }
        if let Some(d) = decode(f.id, f.data) {
            for (k, v) in d.fields() {
                self.values.insert(k.to_string(), v);
            }
            self.last_decoded = Some(now);
        }
    }

    pub fn summary(&self, now: f64) -> Value {
        self.inventory.summary(now)
    }

    /// The `climate.rixens.*` keys, in Python's order. Never heard from → `state: null`; silent for more
    /// than [`STALE_AFTER`] → only `state: "no data"` (the other keys keep their last values).
    pub fn derived(&self, now: f64) -> Vec<(String, Value)> {
        match self.last_decoded {
            None => return vec![("climate.rixens.state".into(), Value::Null)],
            Some(t) if now - t > STALE_AFTER => {
                return vec![("climate.rixens.state".into(), json!("no data"))];
            }
            Some(_) => {}
        }
        let v = &self.values;
        let burning = v.get("fuel_hz").and_then(Value::as_f64).unwrap_or(0.0) > 0.0;
        let sources: Vec<&str> = SOURCE_BITS
            .iter()
            .map(|(k, _)| *k)
            .filter(|k| v.get(&format!("source.{k}")).is_some_and(py::truthy))
            .collect();
        let mut out: Vec<(String, Value)> =
            v.iter().map(|(k, val)| (format!("climate.rixens.{}", k.replace('.', "_")), val.clone())).collect();
        // "heating" only when the burner is actually firing; selected sources alone = standby.
        let state = if burning {
            "heating"
        } else if !sources.is_empty() {
            "standby"
        } else {
            "off"
        };
        out.extend([
            ("climate.rixens.coolant".into(), v.get("glycol_c").cloned().unwrap_or(Value::Null)),
            ("climate.rixens.burner".into(), json!(burning)),
            ("climate.rixens.sources".into(), json!(sources)),
            ("climate.rixens.state".into(), json!(state)),
            ("climate.rixens.live".into(), json!(true)),
        ]);
        out
    }

    /// One tick of the Python `run()` loop (every 1 s): `<device>.ids`, then the derived keys when this
    /// adapter publishes climate and has ever decoded a frame.
    pub fn publish(&self, now: f64) -> Vec<(String, Value)> {
        let mut out = vec![(format!("{}.ids", self.device), self.summary(now))];
        if self.publish_climate && self.last_decoded.is_some() {
            out.extend(self.derived(now));
        }
        out
    }
}
