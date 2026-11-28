//! The Cerbo topics → AetherRV state keys (Python `VictronAdapter._on_*`), with no I/O.
//!
//! Every value is the Cerbo's own JSON value, untouched: an integer stays an integer, a float a float, and objects
//! keep their insertion order (Python dict order). Where Python relies on its own semantics (truthiness, `== 1`
//! being true for `1`, `1.0` and `True`, dict lookups where `3.0` finds the key `3`) the helpers below reproduce
//! them.

use aetherrv_core::config::truthy;
use aetherrv_core::pyfmt::py_str;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap};

/// (topic suffix after `N/<portal>/`) → state key. Paths from Victron's dbus documentation.
pub const WATCH: &[(&str, &str)] = &[
    ("system/0/Dc/Battery/Soc", "victron.battery.soc"),
    ("system/0/Dc/Battery/Voltage", "victron.battery.voltage"),
    ("system/0/Dc/Battery/Current", "victron.battery.current"),
    ("system/0/Dc/Battery/Power", "victron.battery.power"),
    ("system/0/Dc/Battery/TimeToGo", "victron.battery.ttg"),
    ("system/0/Dc/Battery/Temperature", "victron.battery.temp"),
    ("system/0/Dc/Pv/Power", "victron.pv.power"),
    ("system/0/Dc/System/Power", "victron.dc.load"),
    ("system/0/Dc/Alternator/Power", "victron.alternator.power"),
    ("system/0/Dc/Vebus/Power", "victron.vebus.dc_w"),
    ("system/0/Ac/Consumption/L1/Power", "victron.ac.load"),
    ("system/0/Ac/Grid/L1/Power", "victron.ac.shore"),
    // the Energy tiles' amps (owner, 2026-11-27): the same system totals as the tiles' power
    ("system/0/Ac/Consumption/L1/Current", "victron.ac.load_a"),
    ("system/0/Ac/Grid/L1/Current", "victron.ac.shore_a"),
    ("system/0/SystemState/State", "victron.system.state"),
];

/// The control-relevant paths (subscribed and kept in [`Model::vals`]).
pub const CONTROL_PATHS: &[&str] = &[
    "vebus/+/Mode",
    "vebus/+/ModeIsAdjustable",
    "vebus/+/State",
    "vebus/+/Connected",
    "vebus/+/ProductName",
    "vebus/+/CustomName",
    "vebus/+/Ac/In/1/CurrentLimit",
    "vebus/+/Ac/In/1/CurrentLimitIsAdjustable",
    "vebus/+/Ac/Out/L1/P",
    "vebus/+/Ac/ActiveIn/Connected",
    "vebus/+/Ac/ActiveIn/L1/P",
    "solarcharger/+/Mode",
    "solarcharger/+/State",
    "solarcharger/+/ProductName",
    "solarcharger/+/CustomName",
    // display only (the Energy tiles' volts and amps), kept with the instance they belong to
    "vebus/+/Ac/ActiveIn/L1/V",
    "vebus/+/Ac/Out/L1/V",
    "solarcharger/+/Pv/V",
    "solarcharger/+/Yield/Power",
];

pub const BATT_FIELDS: &[&str] = &[
    "ProductName",
    "CustomName",
    "Soc",
    "Soh",
    "Dc/0/Voltage",
    "Dc/0/Current",
    "Dc/0/Power",
    "Dc/0/Temperature",
    "ConsumedAmphours",
    "TimeToGo",
    "Capacity",
    "InstalledCapacity",
    "System/NrOfModulesOnline",
    "System/NrOfModulesOffline",
    "System/NrOfModulesBlockingCharge",
    "System/NrOfModulesBlockingDischarge",
    "System/MinCellVoltage",
    "System/MaxCellVoltage",
    "System/MinVoltageCellId",
    "System/MaxVoltageCellId",
    "System/MinCellTemperature",
    "System/MaxCellTemperature",
    "Info/MaxChargeCurrent",
    "Info/MaxChargeVoltage",
    "Info/MaxDischargeCurrent",
    "Info/BatteryLowVoltage",
];

/// The current on the panel side of the solar charger, which the Cerbo doesn't publish: its PV power (`Yield/Power`)
/// over the PV voltage (`Pv/V`), 2 decimals. No power is 0 A whatever the voltage (at night `Pv/V` reads a few
/// hundredths of a volt); with power but under 1 V, or a value missing, it is unknown.
pub fn panel_amps(power: Option<&Value>, volts: Option<&Value>) -> Value {
    match (power.and_then(Value::as_f64), volts.and_then(Value::as_f64)) {
        (Some(p), _) if p <= 0.0 => json!(0.0),
        (Some(p), Some(v)) if v >= 1.0 => json!(aetherrv_core::num::round(p / v, 2)),
        _ => Value::Null,
    }
}

/// Venus OS temperature service `TemperatureType` (dbus-adc / Ruuvi).
pub fn temp_type(v: &Value) -> &'static str {
    match py_int_key(v) {
        Some(0) => "battery",
        Some(1) => "fridge",
        Some(2) => "generic",
        Some(3) => "room",
        Some(4) => "outdoor",
        Some(5) => "water heater",
        Some(6) => "freezer",
        _ => "generic",
    }
}

/// Venus `FluidType`: 0 fuel, 1 fresh water, 2 waste water, 3 live well, 4 oil, 5 black water.
pub fn fluid(v: &Value) -> &'static str {
    match py_int_key(v) {
        Some(0) => "fuel",
        Some(1) => "fresh",
        Some(2) => "grey",
        Some(3) => "livewell",
        Some(4) => "oil",
        Some(5) => "black",
        _ => "fresh",
    }
}

/// The int a Python dict lookup `{0: …}.get(v)` would find: ints, integral floats and bools (`True == 1`).
pub fn py_int_key(v: &Value) -> Option<i64> {
    match v {
        Value::Bool(b) => Some(*b as i64),
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.fract() == 0.0 && f.abs() < 9e15).map(|f| f as i64)),
        _ => None,
    }
}

/// Python `isinstance(x, (int, float))` → the number (a bool counts: `True` is 1).
pub fn py_num(v: &Value) -> Option<f64> {
    match v {
        Value::Bool(b) => Some(*b as i64 as f64),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

/// Python `x == 1` for a JSON value (`1`, `1.0` and `True` are equal to 1).
pub fn eq_one(v: Option<&Value>) -> bool {
    v.and_then(py_num) == Some(1.0)
}

/// Python `a or b`.
fn or<'a>(a: Option<&'a Value>, b: Option<&'a Value>) -> Value {
    match a {
        Some(x) if truthy(x) => x.clone(),
        _ => b.cloned().unwrap_or(Value::Null),
    }
}

/// Python `int(s) if s.isdigit() else 0` (the battery and vebus instance sort keys).
fn digit_key(s: &str) -> u128 {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) { s.parse().unwrap_or(u128::MAX) } else { 0 }
}

/// Which sensor fills a temperature role (Python `cabin_temp=…`: `False`, `None`, or a name/instance).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Want {
    /// `False`: the role is owned by another source (cabin from the MCS7, or the average).
    Off,
    /// `None`: not configured: the first sensor of the role's type.
    Auto,
    /// A Custom name or a device instance, as Python's `str(want).strip().lower()`.
    Match(String),
}

impl Want {
    /// From a config value: missing → `Auto`, `false` → `Off`, anything else by its Python `str()`.
    pub fn from_value(v: Option<&Value>) -> Want {
        match v {
            None | Some(Value::Null) => Want::Auto,
            Some(Value::Bool(false)) => Want::Off,
            Some(other) => Want::Match(py_str(other).trim().to_lowercase()),
        }
    }
}

/// A temperature role: (state name, configured sensor, fallback `TemperatureType`).
#[derive(Debug, Clone)]
pub struct Role {
    pub name: String,
    pub want: Want,
    pub kind: &'static str,
}

/// The roles in Python's order: `{cabin_role: …, "outside": …, "fridge": …, "freezer": …}`.
pub fn roles(cabin_role: &str, cabin: Want, outside: Want, fridge: Want, freezer: Want) -> Vec<Role> {
    vec![
        Role { name: cabin_role.to_string(), want: cabin, kind: "room" },
        Role { name: "outside".into(), want: outside, kind: "outdoor" },
        Role { name: "fridge".into(), want: fridge, kind: "fridge" },
        Role { name: "freezer".into(), want: freezer, kind: "freezer" },
    ]
}

/// What a message asks the connection to do (Python's `await c.subscribe(...)` / `c.publish(...)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Io {
    Subscribe(String),
    Publish(String, Vec<u8>),
}

/// The effects of one message: state keys to set (in order) and connection requests.
#[derive(Debug, Default)]
pub struct Effects {
    pub state: Vec<(String, Value)>,
    pub io: Vec<Io>,
    /// A control-relevant path changed (wake the command waiters).
    pub control: bool,
}

/// The adapter's in-memory view of the Cerbo (Python `VictronAdapter` attributes).
#[derive(Debug, Default)]
pub struct Model {
    pub portal: Option<String>,
    /// Data topics subscribed on the CURRENT connection.
    pub subscribed: bool,
    pub tanks: BTreeMap<String, Map<String, Value>>,
    pub vecan: Map<String, Value>,
    /// Insertion-ordered (Python dict), sorted numerically when published.
    pub batts: Map<String, Value>,
    pub batts_dirty: bool,
    pub temps: BTreeMap<String, Map<String, Value>>,
    pub notif: BTreeMap<u128, Map<String, Value>>,
    /// raw `<svc>/<inst>/<path>` → value for the control-relevant paths.
    pub vals: HashMap<String, Value>,
    /// Per-path update counter (fresh-read detection).
    pub upd: HashMap<String, u64>,
    pub roles: Vec<Role>,
    /// `victron.controls_ok` inputs: the gate allows "victron", and a connection is up (Python `_client`).
    pub gate_enabled: bool,
    pub link_up: bool,
}

impl Model {
    pub fn new(roles: Vec<Role>, gate_enabled: bool) -> Model {
        Model { roles, gate_enabled, ..Default::default() }
    }

    pub fn upd(&self, path: &str) -> u64 {
        self.upd.get(path).copied().unwrap_or(0)
    }

    /// Python `_on_message(c, topic, payload)`.
    pub fn on_message(&mut self, topic: &str, payload: &[u8]) -> Effects {
        let mut fx = Effects::default();
        let mut parts = topic.splitn(3, '/');
        let (Some("N"), Some(portal), Some(rest)) = (parts.next(), parts.next(), parts.next()) else { return fx };
        // json.loads(payload).get("value"): not JSON or not an object → ignored; no "value" → None
        let value = match serde_json::from_slice::<Value>(payload) {
            Ok(Value::Object(mut m)) => m.remove("value").unwrap_or(Value::Null),
            _ => return fx,
        };
        if rest == "system/0/Serial" && !self.subscribed && self.portal.as_deref().is_none_or(|p| p == portal) {
            self.subscribed = true;
            if self.portal.is_none() {
                self.portal = Some(portal.to_string());
                fx.state.push(("victron.portal".into(), json!(portal)));
                tracing::info!("victron portal id {portal}");
            } else {
                tracing::info!("victron reconnected; re-subscribing");
            }
            for f in subscriptions(portal) {
                fx.io.push(Io::Subscribe(f));
            }
            fx.io.push(Io::Publish(format!("R/{portal}/keepalive"), Vec::new()));
            return fx;
        }
        if rest.starts_with("vecan/") {
            self.on_vecan(rest, value, &mut fx);
            return fx;
        }
        if rest.starts_with("platform/0/Notifications/") {
            self.on_notification(rest, &value, &mut fx);
            self.on_control_path(rest, value, &mut fx); // fresh-read / verify for ack_alarms
            return fx;
        }
        if rest.starts_with("battery/") {
            self.on_battery(rest, value.clone());
        }
        if matches!(rest.split('/').next(), Some("vebus" | "solarcharger")) {
            self.on_control_path(rest, value, &mut fx);
            return fx;
        }
        if rest.starts_with("tank/") {
            self.on_tank(rest, value, &mut fx);
            return fx;
        }
        if rest.starts_with("temperature/") {
            self.on_temp(rest, value, &mut fx);
            return fx;
        }
        if let Some((_, key)) = WATCH.iter().find(|(p, _)| *p == rest) {
            fx.state.push((key.to_string(), value));
        }
        fx
    }

    /// Collect every Cerbo battery service (EPOCH BMS, Lynx Shunt …); published at most 1/s.
    pub fn on_battery(&mut self, rest: &str, value: Value) {
        let parts: Vec<&str> = rest.splitn(3, '/').collect();
        let [_, inst, path] = parts[..] else { return };
        let b = self
            .batts
            .entry(inst.to_string())
            .or_insert_with(|| json!({"instance": inst, "alarms": {}}))
            .as_object_mut()
            .expect("battery entry is an object");
        if let Some(alarm) = path.strip_prefix("Alarms/") {
            b["alarms"].as_object_mut().expect("alarms is an object").insert(alarm.to_string(), value);
        } else if BATT_FIELDS.contains(&path) {
            b.insert(path.to_string(), value);
        } else {
            return;
        }
        self.batts_dirty = true;
    }

    /// Python `batteries()`: sorted by numeric instance (stable), with `kind` and `active_alarms`.
    pub fn batteries(&self) -> Vec<Value> {
        let mut items: Vec<(&String, &Value)> = self.batts.iter().collect();
        items.sort_by_key(|(inst, _)| digit_key(inst));
        items
            .into_iter()
            .map(|(_, b)| {
                let mut d = b.as_object().cloned().unwrap_or_default();
                let bms = d.keys().any(|k| k.starts_with("System/") || k.starts_with("Info/"));
                let active: Map<String, Value> = d
                    .get("alarms")
                    .and_then(Value::as_object)
                    .map(|a| a.iter().filter(|(_, v)| truthy(v)).map(|(k, v)| (k.clone(), v.clone())).collect())
                    .unwrap_or_default();
                d.insert("kind".into(), json!(if bms { "bms" } else { "monitor" }));
                d.insert("active_alarms".into(), Value::Object(active));
                Value::Object(d)
            })
            .collect()
    }

    /// One tick of Python's `_flush_batteries` loop (every 1 s while connected).
    pub fn flush_batteries(&mut self) -> Vec<(String, Value)> {
        if !self.batts_dirty {
            return Vec::new();
        }
        self.batts_dirty = false;
        let bl = self.batteries();
        let bms = bl.iter().find(|b| b.get("kind").and_then(Value::as_str) == Some("bms")).cloned();
        let mut out = vec![("victron.batteries".to_string(), Value::Array(bl))];
        if let Some(b) = bms {
            out.push(("victron.bms.modules_online".into(), b.get("System/NrOfModulesOnline").cloned().unwrap_or(Value::Null)));
        }
        out
    }

    /// The Cerbo's own NMEA 2000 device list (VE.Can): source address → model/maker/serial.
    fn on_vecan(&mut self, rest: &str, value: Value, fx: &mut Effects) {
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() != 5 || parts[2] != "Devices" {
            return;
        }
        let dev = self.vecan.entry(parts[3].to_string()).or_insert_with(|| json!({}));
        if let Some(d) = dev.as_object_mut() {
            d.insert(parts[4].to_string(), value);
        }
        let mut table = Map::new();
        for d in self.vecan.values() {
            // isinstance(nad, int): a JSON integer (or a bool, which Python also counts as int)
            let nad = match d.get("Nad") {
                Some(Value::Number(n)) if n.is_i64() || n.is_u64() => n.to_string(),
                Some(Value::Bool(b)) => if *b { "True" } else { "False" }.to_string(),
                _ => continue,
            };
            let mut row = Map::new();
            for k in ["ModelName", "Manufacturer", "Serial", "FirmwareVersion", "CustomName"] {
                row.insert(k.into(), d.get(k).cloned().unwrap_or(Value::Null));
            }
            table.insert(nad, Value::Object(row));
        }
        fx.state.push(("n2k.names_from_cerbo".into(), Value::Object(table)));
    }

    fn on_tank(&mut self, rest: &str, value: Value, fx: &mut Effects) {
        let parts: Vec<&str> = rest.splitn(3, '/').collect();
        let [_, inst, field] = parts[..] else { return };
        let t = self.tanks.entry(inst.to_string()).or_insert_with(|| {
            let mut m = Map::new();
            m.insert("name".into(), json!(format!("Tank {inst}")));
            m.insert("type".into(), json!("fresh"));
            m.insert("level".into(), Value::Null);
            m
        });
        match field {
            "Level" => {
                t.insert("level".into(), value);
            }
            "FluidType" => {
                t.insert("type".into(), json!(fluid(&value)));
            }
            "CustomName" if truthy(&value) => {
                t.insert("name".into(), value);
            }
            _ => {}
        }
        fx.state.push(("victron.tanks".into(), Value::Array(self.tanks.values().cloned().map(Value::Object).collect())));
    }

    /// A configured sensor (Custom name or instance) is used ONLY if it is present. The by-type fallback applies
    /// only when the role isn't configured; otherwise e.g. a freezer tag that the Cerbo types as "fridge" would be
    /// shown as the fridge while the real fridge tag is missing.
    fn pick(&self, want: &Want, kind: &str) -> Option<&Map<String, Value>> {
        match want {
            Want::Match(w) => self.temps.iter().find_map(|(inst, t)| {
                let name = t.get("name").map(py_str).unwrap_or_default();
                (inst == w || name.trim().to_lowercase() == *w).then_some(t)
            }),
            _ => self.temps.values().find(|t| t.get("type").and_then(Value::as_str) == Some(kind)),
        }
    }

    /// Python `_on_temp(rest, value)`.
    pub fn on_temp(&mut self, rest: &str, value: Value, fx: &mut Effects) {
        let parts: Vec<&str> = rest.splitn(3, '/').collect();
        let [_, inst, field] = parts[..] else { return };
        let t = self.temps.entry(inst.to_string()).or_insert_with(|| {
            let mut m = Map::new();
            m.insert("instance".into(), json!(inst));
            m.insert("name".into(), json!(format!("Sensor {inst}")));
            m.insert("type".into(), json!("generic"));
            m.insert("temp".into(), Value::Null);
            m
        });
        match field {
            "Temperature" => {
                t.insert("temp".into(), value);
            }
            "TemperatureType" => {
                t.insert("type".into(), json!(temp_type(&value)));
            }
            "CustomName" if truthy(&value) => {
                t.insert("name".into(), value);
            }
            "Humidity" => {
                t.insert("humidity".into(), value);
            }
            _ => {}
        }
        fx.state.push(("victron.temps".into(), Value::Array(self.temps.values().cloned().map(Value::Object).collect())));
        for role in &self.roles {
            if role.want == Want::Off {
                continue; // role owned by another source (e.g. cabin from the MCS7)
            }
            match self.pick(&role.want, role.kind) {
                Some(t) => {
                    fx.state.push((format!("climate.{}.temp", role.name), t.get("temp").cloned().unwrap_or(Value::Null)));
                    if let Some(h) = t.get("humidity").filter(|h| !h.is_null()) {
                        fx.state.push((format!("climate.{}.humidity", role.name), h.clone()));
                    }
                }
                // configured sensor not (or no longer) on the Cerbo: show "—", not a stand-in
                None if role.want != Want::Auto => fx.state.push((format!("climate.{}.temp", role.name), Value::Null)),
                None => {}
            }
        }
    }

    /// Venus OS notifications (`platform/0/Notifications/<n>/<field>`): the alarms behind the buzzer.
    fn on_notification(&mut self, rest: &str, value: &Value, fx: &mut Effects) {
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() != 5 || parts[3].is_empty() || !parts[3].bytes().all(|b| b.is_ascii_digit()) {
            return;
        }
        self.notif.entry(digit_key(parts[3])).or_default().insert(parts[4].to_string(), value.clone());
        let list: Vec<Value> = self
            .notif
            .iter()
            .filter(|(_, n)| n.get("Active").is_some_and(truthy))
            .map(|(i, n)| {
                let b = |k: &str| n.get(k).is_some_and(truthy);
                let or_empty = |k: &str| match n.get(k) {
                    Some(v) if truthy(v) => v.clone(),
                    _ => json!(""),
                };
                let mut m = Map::new();
                m.insert("index".into(), json!(*i as u64));
                m.insert("active".into(), json!(b("Active")));
                m.insert("type".into(), n.get("Type").cloned().unwrap_or(Value::Null));
                m.insert("description".into(), or_empty("Description"));
                m.insert("device".into(), or_empty("DeviceName"));
                m.insert("acknowledged".into(), json!(b("Acknowledged")));
                m.insert("silenced".into(), json!(b("Silenced")));
                m.insert("since".into(), n.get("DateTime").cloned().unwrap_or(Value::Null));
                Value::Object(m)
            })
            .collect();
        fx.state.push(("victron.notifications".into(), Value::Array(list)));
    }

    /// Python `_on_control_path(rest, value)` (without the condition notify, which the caller does).
    pub fn on_control_path(&mut self, rest: &str, value: Value, fx: &mut Effects) {
        self.vals.insert(rest.to_string(), value);
        *self.upd.entry(rest.to_string()).or_insert(0) += 1;
        fx.state.extend(self.control_state());
        fx.control = true;
    }

    /// The lowest instance of a service among the control paths (Python `_inst`).
    pub fn inst(&self, svc: &str) -> Option<String> {
        let pre = format!("{svc}/");
        let mut insts: Vec<String> = self
            .vals
            .keys()
            .filter(|k| k.starts_with(&pre))
            .filter_map(|k| k.split('/').nth(1).map(str::to_string))
            .collect();
        insts.sort_by(|a, b| digit_key(a).cmp(&digit_key(b)).then_with(|| a.cmp(b)));
        insts.dedup();
        insts.into_iter().next()
    }

    fn g(&self, svc: &str, inst: &Option<String>, path: &str) -> Option<&Value> {
        // Python `if inst`: an empty instance string is falsy
        inst.as_deref().filter(|i| !i.is_empty()).and_then(|i| self.vals.get(&format!("{svc}/{i}/{path}")))
    }

    /// Python `_publish_control_state()`.
    pub fn control_state(&self) -> Vec<(String, Value)> {
        let (vb, mp) = (self.inst("vebus"), self.inst("solarcharger"));
        let g = |svc: &str, inst: &Option<String>, path: &str| self.g(svc, inst, path).cloned().unwrap_or(Value::Null);
        let one = |svc: &str, inst: &Option<String>, path: &str| json!(eq_one(self.g(svc, inst, path)));
        vec![
            ("victron.vebus.instance".into(), json!(vb)),
            ("victron.vebus.name".into(), or(self.g("vebus", &vb, "CustomName"), self.g("vebus", &vb, "ProductName"))),
            ("victron.vebus.product".into(), g("vebus", &vb, "ProductName")),
            ("victron.vebus.mode".into(), g("vebus", &vb, "Mode")),
            ("victron.vebus.mode_adjustable".into(), one("vebus", &vb, "ModeIsAdjustable")),
            ("victron.vebus.state".into(), g("vebus", &vb, "State")),
            ("victron.vebus.connected".into(), one("vebus", &vb, "Connected")),
            ("victron.vebus.current_limit".into(), g("vebus", &vb, "Ac/In/1/CurrentLimit")),
            ("victron.vebus.limit_adjustable".into(), one("vebus", &vb, "Ac/In/1/CurrentLimitIsAdjustable")),
            ("victron.vebus.ac_out_w".into(), g("vebus", &vb, "Ac/Out/L1/P")),
            ("victron.vebus.ac_in_w".into(), g("vebus", &vb, "Ac/ActiveIn/L1/P")),
            ("victron.vebus.shore_connected".into(), one("vebus", &vb, "Ac/ActiveIn/Connected")),
            ("victron.mppt.instance".into(), json!(mp)),
            ("victron.mppt.name".into(), or(self.g("solarcharger", &mp, "CustomName"), self.g("solarcharger", &mp, "ProductName"))),
            ("victron.mppt.mode".into(), g("solarcharger", &mp, "Mode")),
            ("victron.mppt.state".into(), g("solarcharger", &mp, "State")),
            ("victron.ac.shore_v".into(), g("vebus", &vb, "Ac/ActiveIn/L1/V")),
            ("victron.ac.load_v".into(), g("vebus", &vb, "Ac/Out/L1/V")),
            ("victron.pv.panel_v".into(), g("solarcharger", &mp, "Pv/V")),
            ("victron.pv.panel_a".into(), panel_amps(self.g("solarcharger", &mp, "Yield/Power"), self.g("solarcharger", &mp, "Pv/V"))),
            ("victron.controls_ok".into(), json!(self.gate_enabled && self.link_up)),
        ]
    }
}

/// Every data topic, subscribed once the portal id is known (Python's list, in its order).
pub fn subscriptions(portal: &str) -> Vec<String> {
    let mut v = vec![
        format!("N/{portal}/system/0/#"),
        format!("N/{portal}/battery/#"),
        format!("N/{portal}/tank/+/Level"),
        format!("N/{portal}/tank/+/FluidType"),
        format!("N/{portal}/tank/+/CustomName"),
    ];
    for f in ["Temperature", "TemperatureType", "CustomName", "Humidity"] {
        v.push(format!("N/{portal}/temperature/+/{f}"));
    }
    for p in CONTROL_PATHS {
        v.push(format!("N/{portal}/{p}"));
    }
    v.push(format!("N/{portal}/platform/0/Notifications/#"));
    for f in ["Nad", "ModelName", "Manufacturer", "Serial", "FirmwareVersion", "CustomName", "DeviceInstance"] {
        v.push(format!("N/{portal}/vecan/+/Devices/+/{f}"));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_semantics() {
        assert!(eq_one(Some(&json!(1))) && eq_one(Some(&json!(1.0))) && eq_one(Some(&json!(true))));
        assert!(!eq_one(Some(&json!("1"))) && !eq_one(None) && !eq_one(Some(&json!(0))));
        assert_eq!(temp_type(&json!(3.0)), "room");
        assert_eq!(temp_type(&json!(true)), "fridge");
        assert_eq!(temp_type(&json!("3")), "generic");
        assert_eq!(Want::from_value(Some(&json!(30))), Want::Match("30".into()));
        assert_eq!(Want::from_value(Some(&json!(" Galley "))), Want::Match("galley".into()));
        assert_eq!(Want::from_value(Some(&json!(false))), Want::Off);
        assert_eq!(Want::from_value(None), Want::Auto);
    }

    #[test]
    fn panel_amps_from_pv_power_over_pv_volts() {
        let a = |p: Value, v: Value| panel_amps(Some(&p), Some(&v));
        assert_eq!(a(json!(312), json!(38.5)), json!(8.1));
        assert_eq!(a(json!(0.0), json!(0.07)), json!(0.0), "night: no power is 0 A");
        assert_eq!(a(json!(100), json!(0.5)), Value::Null, "power but no real voltage: unknown");
        assert_eq!(panel_amps(None, Some(&json!(38.0))), Value::Null);
        assert_eq!(a(json!(250), Value::Null), Value::Null);
    }

    #[test]
    fn payload_without_value_is_none_and_garbage_is_ignored() {
        let mut m = Model::new(roles("cabin", Want::Auto, Want::Auto, Want::Auto, Want::Auto), false);
        let fx = m.on_message("N/p/system/0/Dc/Pv/Power", b"{}");
        assert_eq!(fx.state, vec![("victron.pv.power".to_string(), Value::Null)]);
        assert!(m.on_message("N/p/system/0/Dc/Pv/Power", b"").state.is_empty());
        assert!(m.on_message("N/p/system/0/Dc/Pv/Power", b"[1]").state.is_empty());
        assert!(m.on_message("W/p/system/0/Dc/Pv/Power", b"{\"value\": 1}").state.is_empty());
    }
}
