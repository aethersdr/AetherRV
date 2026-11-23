//! The NMEA 2000 adapter: Maretron/CLMD16 switching status plus an inventory of everything on the backbone.
//! READ-ONLY (port of `adapters/n2k.py` `N2kAdapter`).
//!
//! Feed every frame to [`N2kAdapter::on_frame`]; publish [`N2kAdapter::snapshot`] once a second (Python's `run()`
//! loop). Switch control lives in the switching controller, behind the TxGate; it reads [`N2kAdapter::bank`] and
//! [`N2kAdapter::volts`] and hears every 127501 through [`N2kAdapter::add_bank_listener`].
//!
//! Devices are learned passively: we never send ISO Requests (listen-only), so names and products appear when a
//! device powers up or another device (IPG100, DSM) requests them.

use crate::Frame;
use crate::decode::{
    self, AddressClaim, BankStates, ChargerStatus, DC_TYPE_ALTERNATOR, DecodeError, ProductInfo, SwitchState,
};
use crate::fast_packet::FastPacketAssembler;
use crate::id::{N2kId, parse_id};
use crate::pgns;
use crate::pyfmt::{self, opt_f64, truthy};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Seconds without a 127501 from a bank → its channels show as stale, not on/off.
pub const STALE_AFTER: f64 = 30.0;
/// Seconds; NMEA 2000 heartbeats default to every 60 s.
pub const OFFLINE_AFTER: f64 = 90.0;
/// Seconds; the WS500 sends 127508 about once a second.
pub const ALT_STALE: f64 = 10.0;
/// Recent 126208 messages kept.
pub const COMMANDS_KEPT: usize = 20;

/// The latest 127501 of one bank (keyed by (source, instance): several devices may all use instance 0).
#[derive(Debug, Clone, PartialEq)]
pub struct Bank {
    pub source: u8,
    pub instance: u8,
    pub states: BankStates,
    /// When it was received (`now` of that frame).
    pub seen: f64,
}

/// The latest 127500 of one (source, connection).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Load {
    pub state: u8,
    pub status: u8,
    pub duty: u8,
    pub seen: f64,
}

/// The latest 127751 of one (source, connection).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Volts {
    pub volts: Option<f64>,
    pub amps: Option<f64>,
    pub seen: f64,
}

/// One device on the backbone, by source address (from any traffic, 60928, 126996 and 126993).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Device {
    pub source: u8,
    pub first: f64,
    /// Last frame from it (not updated for 254/255, which only enter via 60928/126996/126993).
    pub last: f64,
    pub claim: Option<AddressClaim>,
    pub product: Option<ProductInfo>,
    /// Last 126993 heartbeat.
    pub heartbeat: Option<f64>,
}

impl Device {
    fn new(source: u8, now: f64) -> Self {
        Self { source, first: now, last: now, ..Self::default() }
    }
}

/// A 126208 seen on the bus.
#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub function: String,
    pub pgn: u32,
    pub rest: String,
    pub source: u8,
    pub dest: u8,
    /// `round(now, 1)`.
    pub t: f64,
}

impl Command {
    fn to_json(&self) -> Value {
        json!({"function": self.function, "pgn": self.pgn, "rest": self.rest, "source": self.source,
               "dest": self.dest, "t": self.t})
    }
}

/// The first alternator regulator's readings (`alternator.*`).
#[derive(Debug, Clone, PartialEq)]
pub struct Alternator {
    pub source: u8,
    pub voltage: Option<f64>,
    pub current: Option<f64>,
    /// `round(voltage × current)` (an int, half-to-even).
    pub power: Option<i64>,
    pub state: Option<&'static str>,
    pub temp: Option<f64>,
    pub online: bool,
}

impl Alternator {
    /// `(key suffix, value)` in Python's dict order.
    pub fn entries(&self) -> [(&'static str, Value); 7] {
        [
            ("source", Value::from(self.source)),
            ("voltage", opt_f64(self.voltage)),
            ("current", opt_f64(self.current)),
            ("power", self.power.map_or(Value::Null, Value::from)),
            ("state", self.state.map_or(Value::Null, Value::from)),
            ("temp", opt_f64(self.temp)),
            ("online", Value::from(self.online)),
        ]
    }
}

/// Called for every 127501: `(source, instance, states, now)`.
pub type BankListener = Box<dyn FnMut(u8, u8, &BankStates, f64) + Send>;

/// A `[[device.n2k.switch]]` entry without a usable `bank`/`channel`/`source` (Python raises at startup).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelError(pub String);

impl fmt::Display for LabelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LabelError {}

#[derive(Debug, Clone, Copy)]
struct DcStatus {
    voltage: Option<f64>,
    current: Option<f64>,
    temp: Option<f64>,
    seen: f64,
}

#[derive(Debug, Clone, Copy)]
struct Charger {
    state: Option<&'static str>,
    seen: f64,
}

struct PgnStat {
    count: u64,
    sources: BTreeSet<u8>,
    seen: f64,
}

/// (bank, channel 1-based, source if the label names one).
type LabelKey = (i64, i64, Option<i64>);

/// Python `N2kAdapter`.
pub struct N2kAdapter {
    show_unlabeled: bool,
    pgns: BTreeMap<u32, PgnStat>,
    fp: FastPacketAssembler,
    banks: BTreeMap<(u8, u8), Bank>,
    labels: HashMap<LabelKey, Map<String, Value>>,
    commands: Vec<Command>,
    bank_listeners: Vec<BankListener>,
    loads: HashMap<(u8, u8), Load>,
    volts: HashMap<(u8, u8), Volts>,
    devices: BTreeMap<u8, Device>,
    alt_inst: BTreeMap<u8, u8>,
    /// Insertion-ordered like the Python dict (the voltage fallback iterates in that order).
    dc_status: Vec<((u8, u8), DcStatus)>,
    charger: HashMap<(u8, u8), Charger>,
    names_from_cerbo: Value,
}

impl N2kAdapter {
    /// `switches`: the `[[device.n2k.switch]]` tables as JSON objects (bank, channel, optional source, name, icon,
    /// group, kind, blocked, home, requires_input, rating_a, ...). `show_unlabeled = false` lists only channels named
    /// in config (hides input indicators, keypads...).
    pub fn new(switches: &[Value], show_unlabeled: bool) -> Result<Self, LabelError> {
        let mut labels = HashMap::new();
        for (i, sw) in switches.iter().enumerate() {
            let m = sw.as_object().ok_or_else(|| LabelError(format!("switch #{}: not a table", i + 1)))?;
            let field = |k: &str| -> Result<i64, LabelError> {
                let v = m.get(k).ok_or_else(|| LabelError(format!("switch #{}: missing {k}", i + 1)))?;
                pyfmt::int(v).ok_or_else(|| LabelError(format!("switch #{}: {k} is not an integer: {v}", i + 1)))
            };
            let source = if m.contains_key("source") { Some(field("source")?) } else { None };
            labels.insert((field("bank")?, field("channel")?, source), m.clone());
        }
        Ok(Self {
            show_unlabeled,
            pgns: BTreeMap::new(),
            fp: FastPacketAssembler::new(),
            banks: BTreeMap::new(),
            labels,
            commands: Vec::new(),
            bank_listeners: Vec::new(),
            loads: HashMap::new(),
            volts: HashMap::new(),
            devices: BTreeMap::new(),
            alt_inst: BTreeMap::new(),
            dc_status: Vec::new(),
            charger: HashMap::new(),
            names_from_cerbo: Value::Null,
        })
    }

    /// Call `f` for every 127501 received (Python `bank_listeners.append`). A panicking listener is logged and
    /// skipped, as Python logs and swallows a listener's exception.
    pub fn add_bank_listener(&mut self, f: impl FnMut(u8, u8, &BankStates, f64) + Send + 'static) {
        self.bank_listeners.push(Box::new(f));
    }

    /// The latest value of state key `n2k.names_from_cerbo` (the Cerbo's VE.Can device list, by address), which
    /// names devices in [`Self::device_list`] before their own broadcasts arrive. `Value::Null` clears it.
    pub fn set_names_from_cerbo(&mut self, v: Value) {
        self.names_from_cerbo = v;
    }

    // ── accessors ──

    /// The latest 127501 from `source` for bank `instance`.
    pub fn bank(&self, source: u8, instance: u8) -> Option<&Bank> {
        self.banks.get(&(source, instance))
    }

    /// Every bank, sorted by (source, instance).
    pub fn banks(&self) -> impl Iterator<Item = &Bank> {
        self.banks.values()
    }

    /// The latest 127751 from `source` for `connection` (CLMD16: 0-15 outputs, 16-23 wired inputs).
    pub fn volts(&self, source: u8, connection: u8) -> Option<&Volts> {
        self.volts.get(&(source, connection))
    }

    /// The latest 127500 from `source` for `connection`.
    pub fn load(&self, source: u8, connection: u8) -> Option<&Load> {
        self.loads.get(&(source, connection))
    }

    /// Recent 126208 seen on the bus (newest last, at most [`COMMANDS_KEPT`]).
    pub fn commands(&self) -> &[Command] {
        &self.commands
    }

    /// Every device, sorted by source address.
    pub fn devices(&self) -> impl Iterator<Item = &Device> {
        self.devices.values()
    }

    /// Fast-packet messages dropped so far.
    pub fn fast_packet_dropped(&self) -> u64 {
        self.fp.dropped
    }

    // ── input ──

    /// One received frame. Never panics: a frame a decoder rejects is logged at debug and dropped.
    /// (Remote frames are not filtered, as in Python; their `data` is decoded like any other.)
    pub fn on_frame(&mut self, frame: &Frame, now: f64) {
        if frame.error || !frame.extended {
            return;
        }
        let nid = parse_id(frame.id);
        let e = self.pgns.entry(nid.pgn).or_insert_with(|| PgnStat { count: 0, sources: BTreeSet::new(), seen: now });
        e.count += 1;
        e.sources.insert(nid.source);
        e.seen = now;
        if nid.source < 254 {
            // 254 = cannot claim, 255 = global
            self.devices.entry(nid.source).or_insert_with(|| Device::new(nid.source, now)).last = now;
        }
        let assembled;
        let data: &[u8] = if pgns::is_fast_packet(nid.pgn) {
            // Python uses the frame's receive timestamp; the caller's `now` is that time here.
            match self.fp.feed(nid.source, nid.pgn, &frame.data, now) {
                Some(d) => {
                    assembled = d;
                    &assembled
                }
                None => return,
            }
        } else {
            &frame.data
        };
        if let Err(ex) = self.decode(nid, data, now) {
            tracing::debug!("n2k decode {} from {} failed: {}", nid.pgn, nid.source, ex);
        }
    }

    fn device(&mut self, source: u8, now: f64) -> &mut Device {
        self.devices.entry(source).or_insert_with(|| Device::new(source, now))
    }

    fn decode(&mut self, nid: N2kId, data: &[u8], now: f64) -> Result<(), DecodeError> {
        let src = nid.source;
        match nid.pgn {
            60928 => {
                let ident = decode::decode_60928(data)?;
                // same NAME moved to a new address
                self.devices.retain(|&sa, d| sa == src || d.claim.as_ref().is_none_or(|c| c.name != ident.name));
                self.device(src, now).claim = Some(ident);
            }
            126996 => {
                // Python evaluates `devices.setdefault(...)` before `decode_126996(data)`, so a rejected 126996 from
                // 254/255 still creates the (empty) device entry.
                self.device(src, now);
                let p = decode::decode_126996(data)?;
                self.device(src, now).product = Some(p);
            }
            126993 => self.device(src, now).heartbeat = Some(now),
            127506 if data.len() >= 3 => {
                if data[2] == DC_TYPE_ALTERNATOR {
                    self.alt_inst.insert(src, data[1]);
                }
            }
            127508 if data.len() >= 7 => {
                let r = decode::decode_127508(data)?;
                let rec = DcStatus { voltage: r.voltage, current: r.current, temp: r.temp, seen: now };
                let key = (src, r.instance);
                match self.dc_status.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, v)) => *v = rec,
                    None => self.dc_status.push((key, rec)),
                }
            }
            127507 if data.len() >= 4 => {
                let ChargerStatus { instance, state, .. } = decode::decode_127507(data)?;
                self.charger.insert((src, instance), Charger { state, seen: now });
            }
            127501 => {
                let (inst, states) = decode::decode_127501(data)?;
                self.banks.insert((src, inst), Bank { source: src, instance: inst, states, seen: now });
                for cb in &mut self.bank_listeners {
                    if catch_unwind(AssertUnwindSafe(|| cb(src, inst, &states, now))).is_err() {
                        tracing::error!("bank listener failed");
                    }
                }
            }
            127500 => {
                if let Some((conn, state, status, duty)) = decode::decode_127500(data) {
                    self.loads.insert((src, conn), Load { state, status, duty, seen: now });
                }
            }
            127751 => {
                if let Some((conn, volts, amps)) = decode::decode_127751(data) {
                    self.volts.insert((src, conn), Volts { volts, amps, seen: now });
                }
            }
            126208 => {
                let h = decode::decode_126208_header(data)?;
                tracing::info!(
                    "n2k 126208 {} for PGN {} from {} → {}: {}",
                    h.function,
                    h.pgn,
                    src,
                    nid.destination,
                    h.rest
                );
                self.commands.push(Command {
                    function: h.function,
                    pgn: h.pgn,
                    rest: h.rest,
                    source: src,
                    dest: nid.destination,
                    t: pyfmt::round(now, 1),
                });
                if self.commands.len() > COMMANDS_KEPT {
                    self.commands.drain(..self.commands.len() - COMMANDS_KEPT);
                }
            }
            _ => {}
        }
        Ok(())
    }

    // ── views ──

    /// Every available channel of every bank (`switches.list`); config labels name them and choose an icon and group.
    pub fn switch_list(&self, now: f64) -> Vec<Value> {
        let mut per_inst: HashMap<u8, usize> = HashMap::new();
        for &(_, inst) in self.banks.keys() {
            *per_inst.entry(inst).or_default() += 1;
        }
        let mut out = Vec::new();
        for (&(src, inst), b) in &self.banks {
            let stale = now - b.seen > STALE_AFTER;
            let shared = per_inst.get(&inst).copied().unwrap_or(0) > 1; // instance used by more than one source
            for (i, st) in b.states.iter().enumerate() {
                let ch = i as u8 + 1;
                let key = (i64::from(inst), i64::from(ch), shared.then_some(i64::from(src)));
                let lab = self.labels.get(&key);
                if lab.is_none() && (st.is_none() || !self.show_unlabeled) {
                    continue; // unused or unconfigured channel
                }
                let get = |k: &str| lab.and_then(|l| l.get(k));
                let or = |k: &str, d: String| get(k).cloned().unwrap_or(Value::String(d));
                let sid = format!("{inst}.{ch}") + &if shared { format!("@{src}") } else { String::new() };
                let name =
                    format!("Bank {inst} · Ch {ch}") + &if shared { format!(" (src {src})") } else { String::new() };
                let group = if lab.is_none() { "Unassigned" } else { "Switches" };
                let home = get("home");
                // home = <n> places it at position n on the Home grid (home = true: after those, by channel)
                let home_pos = match home {
                    Some(Value::Number(n)) if n.is_i64() || n.is_u64() => Value::Number(n.clone()),
                    _ => Value::Null,
                };
                let level = if get("kind").and_then(Value::as_str) == Some("dimmer") {
                    self.loads.get(&(src, ch - 1)).map_or(Value::Null, |l| Value::from(l.duty))
                } else {
                    Value::Null
                };
                let amps =
                    if ch <= 16 { opt_f64(self.volts.get(&(src, ch - 1)).and_then(|v| v.amps)) } else { Value::Null };
                let mut m = Map::new();
                m.insert("id".into(), Value::String(sid));
                m.insert("name".into(), or("name", name));
                m.insert("icon".into(), or("icon", "toggle".into()));
                m.insert("group".into(), or("group", group.into()));
                m.insert("on".into(), Value::Bool(*st == Some(SwitchState::On) && !stale));
                let state = if stale { "stale" } else { st.map_or("n/a", SwitchState::as_str) };
                m.insert("state".into(), Value::from(state));
                m.insert("source".into(), Value::from(src));
                m.insert("age".into(), Value::from(pyfmt::round(now - b.seen, 1)));
                m.insert("bank".into(), Value::from(inst));
                m.insert("channel".into(), Value::from(ch));
                m.insert("kind".into(), or("kind", "switch".into()));
                m.insert("blocked".into(), Value::Bool(get("blocked").is_some_and(truthy)));
                m.insert("home".into(), Value::Bool(home.is_some_and(truthy)));
                m.insert("home_pos".into(), home_pos);
                m.insert("requires_input".into(), get("requires_input").cloned().unwrap_or(Value::Null));
                m.insert("rating_a".into(), get("rating_a").cloned().unwrap_or(Value::Null));
                m.insert("level".into(), level);
                m.insert("amps".into(), amps);
                out.push(Value::Object(m));
            }
        }
        out
    }

    /// The device table (`n2k.devices`), named from the Cerbo's hints where a device hasn't sent 126996 yet.
    pub fn device_list(&self, now: f64) -> Vec<Value> {
        let hints = self.names_from_cerbo.as_object();
        let mut out = Vec::new();
        for (&sa, d) in &self.devices {
            let age = now - d.last;
            let h = hints.and_then(|m| m.get(&sa.to_string())).and_then(Value::as_object);
            let hint = |k: &str| h.and_then(|m| m.get(k)).filter(|v| truthy(v));
            let claim = d.claim.as_ref();
            let product = d.product.as_ref();
            let nonempty = |s: Option<&String>| s.filter(|s| !s.is_empty()).cloned();
            let mut model = nonempty(product.map(|p| &p.model)).map(Value::String);
            let mut manufacturer = nonempty(claim.map(|c| &c.manufacturer));
            let mut software = nonempty(product.map(|p| &p.software));
            let mut serial = nonempty(product.map(|p| &p.serial));
            // Python: `if not d.get("model") and h.get("ModelName")`, then `d.get(k) or str(h.get(K) or "")`
            if model.is_none()
                && let Some(m) = hint("ModelName")
            {
                model = Some(m.clone());
                serial = serial.or_else(|| hint("Serial").map(pyfmt::py_str));
                software = software.or_else(|| hint("FirmwareVersion").map(pyfmt::py_str));
                manufacturer = manufacturer.or_else(|| {
                    let code = h.and_then(|m| m.get("Manufacturer")).and_then(pyfmt::int_key);
                    code.and_then(pgns::manufacturer).map(str::to_string)
                });
            }
            let s = |v: Option<String>| Value::String(v.unwrap_or_default());
            let mut m = Map::new();
            m.insert("source".into(), Value::from(sa));
            m.insert("model".into(), model.filter(truthy).unwrap_or_else(|| Value::from("")));
            m.insert("manufacturer".into(), s(manufacturer));
            m.insert("class".into(), s(nonempty(claim.map(|c| &c.device_class))));
            m.insert("instance".into(), claim.map_or(Value::Null, |c| Value::from(c.device_instance)));
            m.insert("software".into(), s(software));
            m.insert("serial".into(), s(serial));
            m.insert("online".into(), Value::Bool(age <= OFFLINE_AFTER));
            m.insert("age".into(), Value::from(pyfmt::round(age, 1)));
            m.insert("heartbeat".into(), Value::Bool(d.heartbeat.is_some()));
            out.push(Value::Object(m));
        }
        out
    }

    /// Alternator readings from the first regulator seen (READ-ONLY, decoded from its broadcasts).
    ///
    /// The WS500 puts voltage + temperature on its alternator instance (127508, e.g. 49) but the current on
    /// instance 0, so the current is taken from any of that source's instances. NOTE: which current that is depends
    /// on where the regulator's shunt is wired (alternator output vs battery). It is used as alternator current; see
    /// INTEGRATION_PLAN. Once a regulator has been seen this always returns (`online: false` when nothing is fresh).
    pub fn alternator(&self, now: f64) -> Option<Alternator> {
        let (&src, &inst) = self.alt_inst.iter().next()?;
        // insertion order, like the Python dict comprehension
        let fresh: Vec<(u8, &DcStatus)> = self
            .dc_status
            .iter()
            .filter(|((s, _), r)| *s == src && now - r.seen <= ALT_STALE)
            .map(|((_, i), r)| (*i, r))
            .collect();
        let own = fresh.iter().find(|(i, _)| *i == inst).map(|(_, r)| *r);
        // `own.get("voltage") or next(...)`: a 0.0 reading falls through to the first non-None one
        let volts = match own.and_then(|r| r.voltage) {
            Some(v) if v != 0.0 => Some(v),
            _ => fresh.iter().find_map(|(_, r)| r.voltage),
        };
        let mut amps = own.and_then(|r| r.current);
        if amps.is_none() {
            let mut by_inst = fresh.clone();
            by_inst.sort_by_key(|(i, _)| *i);
            amps = by_inst.iter().find_map(|(_, r)| r.current);
        }
        let state = self.charger.get(&(src, inst)).filter(|c| now - c.seen <= ALT_STALE).and_then(|c| c.state);
        Some(Alternator {
            source: src,
            voltage: volts,
            current: amps,
            power: match (volts, amps) {
                (Some(v), Some(a)) => Some(pyfmt::round_int(v * a)),
                _ => None,
            },
            state,
            temp: own.and_then(|r| r.temp),
            online: !fresh.is_empty(),
        })
    }

    /// Every PGN seen (`n2k.pgns`): name, frame count, sources, age.
    pub fn summary(&self, now: f64) -> Vec<Value> {
        self.pgns
            .iter()
            .map(|(&pgn, e)| {
                json!({"pgn": pgn, "name": pgns::pgn_name(pgn).unwrap_or(""), "count": e.count,
                       "sources": e.sources.iter().collect::<Vec<_>>(), "age": pyfmt::round(now - e.seen, 1)})
            })
            .collect()
    }

    /// The state keys and values Python's `run()` publishes each second, in its order.
    pub fn snapshot(&self, now: f64) -> Vec<(String, Value)> {
        let rows = self.summary(now);
        let sources: BTreeSet<u8> = self.pgns.values().flat_map(|e| e.sources.iter().copied()).collect();
        let mut out = vec![
            ("n2k.pgns".to_string(), Value::Array(rows)),
            ("n2k.sources".to_string(), json!(sources.into_iter().collect::<Vec<_>>())),
            ("n2k.commands".to_string(), Value::Array(self.commands.iter().map(Command::to_json).collect())),
            ("n2k.devices".to_string(), Value::Array(self.device_list(now))),
        ];
        if let Some(alt) = self.alternator(now) {
            out.extend(alt.entries().into_iter().map(|(k, v)| (format!("alternator.{k}"), v)));
        }
        if !self.banks.is_empty() {
            out.push(("switches.source".to_string(), Value::from("n2k")));
            out.push(("switches.list".to_string(), Value::Array(self.switch_list(now))));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{T0, encode_127501, frames, hex, msg, n2k_id, name_bytes, product_payload};
    use std::collections::HashMap;

    fn by_id(list: Vec<Value>) -> HashMap<String, Value> {
        list.into_iter().map(|x| (x["id"].as_str().unwrap().to_string(), x)).collect()
    }

    fn by_source(list: Vec<Value>) -> HashMap<u64, Value> {
        list.into_iter().map(|x| (x["source"].as_u64().unwrap(), x)).collect()
    }

    /// py: tests/test_n2k.py::test_adapter_switch_list_with_labels
    #[test]
    fn test_adapter_switch_list_with_labels() {
        let mut a = N2kAdapter::new(
            &[
                json!({"bank": 0, "channel": 2, "name": "Galley", "icon": "light", "group": "Lights", "home": 1}),
                json!({"bank": 0, "channel": 12, "name": "Spare", "icon": "plug", "group": "Power", "home": true}),
            ],
            true,
        )
        .unwrap();
        a.on_frame(&msg(n2k_id(127501, 0x30, 3, 255), encode_127501(0, &[1, 0, 1])), T0);
        let lst = by_id(a.switch_list(T0));
        let mut ids: Vec<_> = lst.keys().cloned().collect();
        ids.sort();
        assert_eq!(ids, ["0.1", "0.12", "0.2", "0.3"]); // available channels + configured ones
        assert!(lst["0.2"]["name"] == "Galley" && lst["0.2"]["on"] == false && lst["0.2"]["group"] == "Lights");
        assert!(lst["0.1"]["group"] == "Unassigned" && lst["0.1"]["on"] == true);
        assert!(lst["0.12"]["state"] == "n/a" && lst["0.12"]["on"] == false);
        assert!(lst["0.2"]["home"] == true && lst["0.2"]["home_pos"] == 1); // home = 1: first on the Home grid
        assert!(lst["0.12"]["home"] == true && lst["0.12"]["home_pos"].is_null() && lst["0.1"]["home"] == false);
    }

    /// py: tests/test_n2k.py::test_adapter_reassembles_126208_command
    #[test]
    fn test_adapter_reassembles_126208_command() {
        let mut a = N2kAdapter::new(&[], true).unwrap();
        let payload = [1, 0x0D, 0xF2, 0x01, 0xF8, 0x02, 0x01, 0x00, 0x02, 0x01];
        for f in frames(&payload, 2) {
            a.on_frame(&msg(n2k_id(126208, 0x10, 3, 0x30), f), T0);
        }
        assert!(a.commands().len() == 1 && a.commands()[0].pgn == 127501 && a.commands()[0].dest == 0x30);
    }

    /// py: tests/test_n2k.py::test_adapter_marks_stale_banks
    #[test]
    fn test_adapter_marks_stale_banks() {
        let mut a = N2kAdapter::new(&[], true).unwrap();
        a.on_frame(&msg(n2k_id(127501, 0x30, 3, 255), encode_127501(0, &[1])), T0);
        let fresh = &a.switch_list(T0)[0];
        assert!(fresh["state"] == "on" && fresh["on"] == true);
        let old = &a.switch_list(T0 + 31.0)[0];
        assert!(old["state"] == "stale" && old["on"] == false);
    }

    /// py: tests/test_n2k.py::test_device_list_passive_and_offline
    #[test]
    fn test_device_list_passive_and_offline() {
        let mut a = N2kAdapter::new(&[], true).unwrap();
        let send = |a: &mut N2kAdapter, pgn: u32, src: u32, data: Vec<u8>| {
            let fs = if data.len() > 8 { frames(&data, 2) } else { vec![data] };
            for f in fs {
                a.on_frame(&msg(n2k_id(pgn, src, 6, 255), f), T0);
            }
        };
        send(&mut a, 60928, 0x23, name_bytes(777, 137, 0, 0, 130, 25, 0, 4, 1).to_vec());
        send(&mut a, 126996, 0x23, product_payload("IPG100", "1.6.2", "SN42", 3));
        send(&mut a, 126993, 0x23, vec![0x60, 0xEA, 1, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        send(&mut a, 127501, 0x30, encode_127501(0, &[1])); // a device we only see traffic from
        let devs = by_source(a.device_list(T0));
        assert!(
            devs[&0x23]["model"] == "IPG100"
                && devs[&0x23]["manufacturer"] == "Maretron"
                && devs[&0x23]["heartbeat"] == true
        );
        assert!(
            devs[&0x23]["class"] == "Internetwork device"
                && devs[&0x30]["model"] == ""
                && devs[&0x30]["online"] == true
        );
        assert!(by_source(a.device_list(T0 + 120.0))[&0x23]["online"] == false);
        // same NAME re-claims a new address -> old entry removed
        send(&mut a, 60928, 0x24, name_bytes(777, 137, 0, 0, 130, 25, 0, 4, 1).to_vec());
        assert!(!by_source(a.device_list(T0)).contains_key(&0x23));
    }

    /// py: tests/test_n2k.py::test_device_names_from_cerbo_hints
    #[test]
    fn test_device_names_from_cerbo_hints() {
        let mut a = N2kAdapter::new(&[], true).unwrap();
        a.on_frame(&msg(n2k_id(127501, 0, 3, 255), encode_127501(32, &[0])), T0);
        a.set_names_from_cerbo(
            json!({"0": {"ModelName": "CLMD16", "Manufacturer": 176, "Serial": 1540324, "FirmwareVersion": "1.0.3.66"}}),
        );
        let d = &a.device_list(T0)[0];
        assert!(d["model"] == "CLMD16" && d["serial"] == "1540324" && d["software"] == "1.0.3.66");
    }

    /// py: tests/test_n2k.py::test_same_instance_from_two_sources_does_not_collide
    #[test]
    fn test_same_instance_from_two_sources_does_not_collide() {
        let mut a = N2kAdapter::new(&[], true).unwrap();
        a.on_frame(&msg(n2k_id(127501, 225, 3, 255), encode_127501(0, &[1])), T0);
        a.on_frame(&msg(n2k_id(127501, 226, 3, 255), encode_127501(0, &[0, 1])), T0);
        let ids: HashMap<String, Value> = a
            .switch_list(T0)
            .into_iter()
            .map(|x| (x["id"].as_str().unwrap().to_string(), x["state"].clone()))
            .collect();
        let want: HashMap<String, Value> = [("0.1@225", "on"), ("0.1@226", "off"), ("0.2@226", "on")]
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v)))
            .collect();
        assert_eq!(ids, want);
    }

    /// test_n2k.py `_frame(pgn, src, data, prio=6)`.
    fn frame_raw(pgn: u32, src: u32, data: Vec<u8>, prio: u32) -> Frame {
        msg((prio << 26) | (pgn << 8) | src, data)
    }

    /// py: tests/test_n2k.py::test_ws500_alternator_from_real_frames
    ///
    /// Frames as the van's Wakespeed WS500 (src 128) sends them while charging (127506/127507 and the inst-49
    /// 127508 are captured verbatim; the inst-0 127508 is modelled on the captured ones).
    #[test]
    fn test_ws500_alternator_from_real_frames() {
        let mut a = N2kAdapter::new(&[], true).unwrap();
        for (pgn, hexes) in [
            (127506, vec!["C00B443101FFFFFF", "C1FFFFFFFFFFFFFF"]), // fast packet: inst 49, DC type 1 = alternator
            (127507, vec!["E006310011FDFFFF"]),                     // charger status (fast, 1 frame): bulk, enabled
            (127508, vec!["315F05FF7F0F6444"]),                     // inst 49: 13.75 V, current n/a, temp
            (127508, vec!["005F05CC05FFFF44"]),                     // inst 0: 13.75 V, 148.4 A
        ] {
            for h in hexes {
                a.on_frame(&frame_raw(pgn, 128, hex(h), 6), T0);
            }
        }
        let alt = a.alternator(T0).unwrap();
        assert!(alt.source == 128 && alt.voltage == Some(13.75) && alt.current == Some(148.4));
        assert!(alt.power == Some(pyfmt::round_int(13.75 * 148.4)) && alt.state == Some("bulk") && alt.online);
    }

    /// Never panics, on any id and any payload of 0..64 bytes, for every PGN the adapter decodes (single frames and
    /// fast-packet runs), and the views stay renderable.
    #[test]
    fn on_frame_never_panics() {
        let mut a = N2kAdapter::new(&[json!({"bank": 0, "channel": 1, "kind": "dimmer", "home": 2})], true).unwrap();
        a.add_bank_listener(|_, inst, _, _| {
            if inst == 7 {
                panic!("listener bug");
            }
        });
        a.set_names_from_cerbo(json!({"35": {"ModelName": 5, "Manufacturer": [1], "Serial": 1.5}, "36": "x"}));
        let pgns = [60928, 126996, 126993, 127506, 127508, 127507, 127501, 127500, 127751, 126208, 59904, 130312];
        let mut x: u64 = 0x2545_F491_4F6C_DD1D;
        let mut rnd = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let mut now = T0;
        for n in 0..60_000u32 {
            let r = rnd();
            let pgn = pgns[(r % pgns.len() as u64) as usize];
            let src = [0x23u32, 0x24, 0x30, 128, 254, 255][((r >> 8) % 6) as usize];
            let id =
                if n % 7 == 0 { (r >> 16) as u32 & 0x1FFF_FFFF } else { n2k_id(pgn, src, 6, (r >> 24) as u32 & 0xFF) };
            let len = if n % 13 == 0 { (r >> 32) as usize % 65 } else { (r >> 32) as usize % 9 };
            let data: Vec<u8> = (0..len).map(|_| rnd() as u8).collect();
            let mut f = msg(id, data);
            f.remote = n % 11 == 0;
            a.on_frame(&f, now);
            now += (r >> 40) as f64 % 1000.0 / 1000.0;
            if n % 1000 == 0 {
                let _ = a.snapshot(now);
            }
        }
        // fast-packet runs of every length up to the 223-byte limit
        for len in 0..=223usize {
            let payload: Vec<u8> = (0..len).map(|_| rnd() as u8).collect();
            for pgn in [126996, 126208, 127506, 127507] {
                for f in frames(&payload, (len % 8) as u8) {
                    a.on_frame(&msg(n2k_id(pgn, 0x23, 6, 255), f), now);
                }
            }
        }
        let _ = a.snapshot(now);
    }

    /// Python quirk kept: `devices.setdefault(...)` runs before `decode_126996`, so a rejected 126996 from 254
    /// still lists device 254; a rejected 60928 (decoded first) does not.
    #[test]
    fn rejected_126996_from_254_still_lists_the_device() {
        let mut a = N2kAdapter::new(&[], true).unwrap();
        a.on_frame(&msg(n2k_id(60928, 254, 6, 255), vec![1, 2, 3]), T0);
        assert_eq!(a.devices().count(), 0);
        for f in frames(&[0; 20], 1) {
            a.on_frame(&msg(n2k_id(126996, 254, 6, 255), f), T0);
        }
        let d: Vec<&Device> = a.devices().collect();
        assert!(d.len() == 1 && d[0].source == 254 && d[0].product.is_none() && d[0].claim.is_none());
        assert_eq!(a.device_list(T0)[0]["model"], "");
    }

    /// Every 127501 reaches the listeners with (source, instance, states, now), after the bank is stored.
    #[test]
    fn bank_listener_hears_every_127501() {
        use std::sync::{Arc, Mutex};
        let mut a = N2kAdapter::new(&[], true).unwrap();
        let heard = Arc::new(Mutex::new(Vec::new()));
        let h = heard.clone();
        a.add_bank_listener(move |src, inst, st, t| h.lock().unwrap().push((src, inst, *st, t)));
        a.on_frame(&msg(n2k_id(127501, 0x28, 3, 255), encode_127501(32, &[1, 0, 2])), T0);
        a.on_frame(&msg(n2k_id(127501, 0x28, 3, 255), vec![32, 1]), T0 + 1.0); // short: rejected, not heard
        let heard = heard.lock().unwrap();
        assert_eq!(heard.len(), 1);
        let (src, inst, st, t) = heard[0];
        assert!(src == 0x28 && inst == 32 && t == T0);
        assert_eq!(st[..4], [Some(SwitchState::On), Some(SwitchState::Off), Some(SwitchState::Error), None]);
        let b = a.bank(0x28, 32).unwrap();
        assert!(b.states == st && b.seen == T0);
    }

    /// The published keys, in Python's order, with Python's value types.
    #[test]
    fn snapshot_keys_and_order() {
        let mut a = N2kAdapter::new(&[], true).unwrap();
        let empty: Vec<String> = a.snapshot(T0).into_iter().map(|(k, _)| k).collect();
        assert_eq!(empty, ["n2k.pgns", "n2k.sources", "n2k.commands", "n2k.devices"]);
        a.on_frame(&frame_raw(127506, 128, hex("C00B443101FFFFFF"), 6), T0);
        a.on_frame(&frame_raw(127506, 128, hex("C1FFFFFFFFFFFFFF"), 6), T0);
        a.on_frame(&frame_raw(127508, 128, hex("315F05FF7F0F6444"), 6), T0);
        a.on_frame(&msg(n2k_id(127501, 0x30, 3, 255), encode_127501(0, &[1])), T0);
        let snap = a.snapshot(T0 + 0.25);
        let keys: Vec<&str> = snap.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            [
                "n2k.pgns",
                "n2k.sources",
                "n2k.commands",
                "n2k.devices",
                "alternator.source",
                "alternator.voltage",
                "alternator.current",
                "alternator.power",
                "alternator.state",
                "alternator.temp",
                "alternator.online",
                "switches.source",
                "switches.list"
            ]
        );
        assert_eq!(
            serde_json::to_string(&snap[0].1).unwrap(),
            r#"[{"pgn":127501,"name":"Binary Switch Bank Status","count":1,"sources":[48],"age":0.2},{"pgn":127506,"name":"DC Detailed Status","count":2,"sources":[128],"age":0.2},{"pgn":127508,"name":"Battery Status","count":1,"sources":[128],"age":0.2}]"#
        );
        assert_eq!(snap[1].1, json!([48, 128]));
        assert_eq!(snap[5].1, json!(13.75));
        assert!(snap[7].1.is_null() && snap[9].1 == json!(-17.0) && snap[10].1 == json!(true));
    }
}
