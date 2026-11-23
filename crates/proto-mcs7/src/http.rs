//! The MCS7 Local HTTP API (port of the pure parts of `src/aetherrv/adapters/rixens_http.py`).
//!
//! ```text
//! GET /status.xml                     full snapshot (polled every `interval` s; never < 1 s apart)
//! GET /interface.cgi?act=N&val=V      commands (integers only)
//! ```
//!
//! Encodings, confirmed from the MCS7's own web UI JavaScript and the live unit (fw HW1W6-1.208):
//! temperatures are tenths of °C (currenttemp=72 → 7.2 °C); heater temps are hundredths of °C; humidity is
//! tenths of %RH; battv is tenths of V; fanspeed is an integer or "Auto"/"Off".
//!
//! CONTROL (owner-approved 2026-11-20): setpoint, fan speed, the three heat sources, and (approved later the
//! same day) hot water (constant heat, act 6) and engine pre-heat (act 7). The MCS7 web UI sends those two
//! exactly like the sources: act=N&val=<new state 0/1> (constantHeatDisable / preheatFunctionToggle). Cabin
//! heat on/off (thermostat, act 9, explicit 0/1) added 2026-11-22 at the owner's request.
//!
//! [`plan`] is the hard allowlist + integer range checks: nothing else is reachable from code (no
//! buttons.cgi / infra.cgi / floor / aux / override / Wi-Fi acts). The rest of the safety rules (TxGate
//! device `rixens_http`, the heat lockout, a fresh status read right before acting, send once, verify by
//! polling, NEVER auto-retry, one lock + ≥ 1 s spacing) live in the `aetherrv-rixens` service.
//!
//! Wi-Fi secrets in status.xml (key, ssid, infra_pass, infra_ssid, scan results) are never parsed: the
//! reader keeps the text of the whitelisted elements only (see [`crate::xml`]).

use serde_json::{Map, Value, json};

use crate::py;
use crate::xml::{self, XmlError};

pub const DEVICE: &str = "rixens_http";

pub const HEATER_STATE: [(i64, &str); 10] = [
    (0, "off"),
    (10, "cold start"),
    (12, "warm start"),
    (13, "cold blow"),
    (15, "flame stabilizing"),
    (20, "running"),
    (30, "after-run (shutdown)"),
    (31, "after-run (standby)"),
    (40, "pause"),
    (200, "error"),
];
pub const SRC_STATE: [(i64, &str); 3] = [(0, "off"), (1, "ready"), (2, "active")];
/// System fluid temperature indicator (the MCS7 touchscreen's "fluid temp" icon). Live value is the
/// top-level `<glycolstate>` (doc says 0/1, but the unit reports the 0-3 scale): 0 cold, 1 warming,
/// 2 ready (≥ 122 °F: cabin fan may run), 3 hot (≥ 158 °F: hot water ready).
pub const GLYCOL: [(i64, &str); 4] = [(0, "cold"), (1, "warming"), (2, "ready"), (3, "hot")];
/// `settings/glycol` is the separate "Comfort hot tank" status (0 unless hot-water mode is used).
pub const HOT_TANK: [(i64, &str); 4] = [(0, "off"), (1, "warming"), (2, "ready"), (3, "max")];
/// `settings/heatsources` bits, in Python's order (the order of `sources`).
pub const SOURCE_BITS: [(&str, i64); 3] = [("engine", 1), ("electric", 2), ("furnace", 4)];
pub const SOURCE_ACT: [(&str, i64); 3] = [("engine", 3), ("electric", 4), ("furnace", 5)];
/// act 9 = thermostat (cabin "Heat" destination), explicit 0/1, verified on settings/thermenabled:
/// owner-approved 2026-11-22 (make the Heat / Hot water / Pre-heat icons on the Climate card tappable).
pub const MODE_ACT: [(&str, i64); 3] = [("hot_water", 6), ("preheat", 7), ("thermostat", 9)];
/// The MCS7 UI's own limits (°C mode).
pub const SETPOINT_MIN_C: f64 = 1.0;
pub const SETPOINT_MAX_C: f64 = 33.0;
/// `heater_state_code`s that count as burning.
pub const BURNING: [i64; 4] = [10, 12, 15, 20];

/// Fuel flow = dosing-pump Hz × volume per stroke. Calibrated 2026-11-20: during a cold-start warm-up (full
/// output) the pump held 5.5 Hz; Eberspächer Hydronic D5 SC full output = 5.0 kW = 0.62 l/h (spec sheet) →
/// 0.62 l/h ÷ (5.5 × 3600) = 0.0313 ml/stroke. Cross-check: the D5 W SC fuel test (9.0 cm³ / 90 s nominal)
/// implies ~3.2 Hz at that volume. Override with `fuel_ml_per_pulse`.
pub const DEFAULT_ML_PER_PULSE: f64 = 0.0313;
pub const L_PER_GAL: f64 = 3.785411784;

fn lookup(table: &[(i64, &'static str)], code: Option<i64>) -> Option<&'static str> {
    code.and_then(|c| table.iter().find(|(k, _)| *k == c).map(|(_, v)| *v))
}

/// `settings/fanspeed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanSpeed {
    Auto,
    Off,
    Pct(i64),
}

impl FanSpeed {
    pub fn to_json(self) -> Value {
        match self {
            FanSpeed::Auto => json!("auto"),
            FanSpeed::Off => json!("off"),
            FanSpeed::Pct(n) => json!(n),
        }
    }
}

/// The optional heat sources and functions the MCS7 has enabled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Available {
    pub electric: bool,
    pub engine: bool,
    pub preheat: bool,
    pub floor: bool,
    pub aux: bool,
}

impl Available {
    pub fn to_json(self) -> Value {
        let Available { electric, engine, preheat, floor, aux } = self;
        json!({"electric": electric, "engine": engine, "preheat": preheat, "floor": floor, "aux": aux})
    }
}

/// One normalized `status.xml` (the Python `parse_status` dict, field for field).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub version: String,
    pub heat_version: String,
    pub uptime: Option<i64>,
    pub system_heat: bool,
    pub heater_state_code: Option<i64>,
    pub heater_state: Option<String>,
    pub cabin_c: Option<f64>,
    pub cabin_rh: Option<f64>,
    pub setpoint_c: Option<f64>,
    pub thermostat: bool,
    pub fan_enabled: bool,
    pub fan_running: bool,
    pub fan_speed: Option<FanSpeed>,
    /// In Auto the MCS7 runs the cabin fan from a PID loop (`<autofan>` fanset 0 = 999 → PID enabled);
    /// heater1/pidspeed is that loop's live output on the same 0–100 % scale as manual fan speeds.
    pub fan_auto_pct: Option<i64>,
    pub pump_running: bool,
    /// Selected heat sources, in [`SOURCE_BITS`] order.
    pub sources: Vec<&'static str>,
    pub furnace: Option<&'static str>,
    pub electric: Option<&'static str>,
    pub engine: Option<&'static str>,
    pub floor: Option<&'static str>,
    pub aux: Option<&'static str>,
    pub hot_water: bool,
    pub hot_water_timer_s: Option<i64>,
    pub preheat: bool,
    pub glycol: Option<&'static str>,
    pub hot_tank: Option<&'static str>,
    pub available: Available,
    pub battery_v: Option<f64>,
    pub runtime_h: Option<f64>,
    pub flame_c: Option<f64>,
    pub inlet_c: Option<f64>,
    pub outlet_c: Option<f64>,
    pub altitude_m: Option<f64>,
    /// "Fan-Glow-Fuel" exactly as the MCS7's own diagnostics page scales it (web UI JS):
    /// burnermotor = rpm, glowpin ÷ 10 = W, dosingpump ÷ 10 = Hz.
    pub fan_rpm: Option<i64>,
    pub glow_w: Option<f64>,
    pub fuel_hz: Option<f64>,
    /// AF: the live fault (0 and 255, "none" in the doc example, read as no fault).
    pub fault_active: Option<i64>,
    pub fault_history: Vec<i64>,
}

impl Status {
    /// The Python dict, in its key order.
    pub fn to_map(&self) -> Map<String, Value> {
        let n = |x: Option<f64>| py::num(x);
        let mut m = Map::new();
        let mut put = |k: &str, v: Value| {
            m.insert(k.to_string(), v);
        };
        put("version", json!(self.version));
        put("heat_version", json!(self.heat_version));
        put("uptime", json!(self.uptime));
        put("system_heat", json!(self.system_heat));
        put("heater_state_code", json!(self.heater_state_code));
        put("heater_state", json!(self.heater_state));
        put("cabin_c", n(self.cabin_c));
        put("cabin_rh", n(self.cabin_rh));
        put("setpoint_c", n(self.setpoint_c));
        put("thermostat", json!(self.thermostat));
        put("fan_enabled", json!(self.fan_enabled));
        put("fan_running", json!(self.fan_running));
        put("fan_speed", self.fan_speed.map_or(Value::Null, FanSpeed::to_json));
        put("fan_auto_pct", json!(self.fan_auto_pct));
        put("pump_running", json!(self.pump_running));
        put("sources", json!(self.sources));
        put("furnace", json!(self.furnace));
        put("electric", json!(self.electric));
        put("engine", json!(self.engine));
        put("floor", json!(self.floor));
        put("aux", json!(self.aux));
        put("hot_water", json!(self.hot_water));
        put("hot_water_timer_s", json!(self.hot_water_timer_s));
        put("preheat", json!(self.preheat));
        put("glycol", json!(self.glycol));
        put("hot_tank", json!(self.hot_tank));
        put("available", self.available.to_json());
        put("battery_v", n(self.battery_v));
        put("runtime_h", n(self.runtime_h));
        put("flame_c", n(self.flame_c));
        put("inlet_c", n(self.inlet_c));
        put("outlet_c", n(self.outlet_c));
        put("altitude_m", n(self.altitude_m));
        put("fan_rpm", json!(self.fan_rpm));
        put("glow_w", n(self.glow_w));
        put("fuel_hz", n(self.fuel_hz));
        put("fault_active", json!(self.fault_active));
        put("fault_history", json!(self.fault_history));
        m
    }

    fn has_source(&self, name: &str) -> bool {
        self.sources.contains(&name)
    }

    /// `bool(d.get(mode))` for the three MODE_ACT keys.
    fn mode(&self, name: &str) -> bool {
        match name {
            "hot_water" => self.hot_water,
            "preheat" => self.preheat,
            "thermostat" => self.thermostat,
            _ => false,
        }
    }
}

/// Why a `status.xml` could not be read (Python raises out of `parse_status`, so the poll fails and the
/// service sets `climate.rixens.reachable = false`).
#[derive(Debug, Clone, PartialEq)]
pub enum StatusError {
    /// Not well-formed XML (Python `ET.ParseError`).
    Xml(XmlError),
    /// `heater1/altitude` is present but not a number (Python `float()` ValueError).
    Altitude(String),
}

impl std::fmt::Display for StatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StatusError::Xml(e) => e.fmt(f),
            StatusError::Altitude(s) => {
                write!(f, "could not convert string to float: {}", py::str_repr(s))
            }
        }
    }
}

impl std::error::Error for StatusError {}

/// `_scaled(v, div)`: tenths/hundredths → a float, `None` for missing or a sentinel ≤ -999.
fn scaled(v: &str, div: f64) -> Option<f64> {
    py::int(v).filter(|n| *n > -999).map(|n| py::round(n as f64 / div, 2))
}

/// status.xml → the normalized status. Whitelisted fields only (no Wi-Fi secrets).
pub fn parse_status(xml: impl AsRef<[u8]>) -> Result<Status, StatusError> {
    let x = xml::extract(xml.as_ref()).map_err(StatusError::Xml)?;
    let t = |parent: &str, name: &str| py::strip(x.findtext(parent, name).unwrap_or(""));
    let top = |name: &str| t("", name);
    let s = |name: &str| t("settings", name);
    let h = |name: &str| t("heater1", name);

    let fan_raw = s("fanspeed");
    let fan = if fan_raw.is_empty() {
        None
    } else {
        match fan_raw.to_lowercase().as_str() {
            "auto" => Some(FanSpeed::Auto),
            "off" => Some(FanSpeed::Off),
            _ => None,
        }
    };
    let fan = fan.or_else(|| match py::int(fan_raw) {
        Some(999) => Some(FanSpeed::Auto),
        Some(n) => Some(FanSpeed::Pct(n)),
        None => None,
    });
    let hs = py::int(s("heatsources")).unwrap_or(0);

    // dict semantics: a repeated fault name keeps its first position and takes the last value
    let mut faults: Vec<(String, Option<i64>)> = Vec::new();
    for f in &x.faults {
        let name = py::strip(f.name.as_deref().unwrap_or("")).to_string();
        let value = f.value.as_deref().and_then(py::int);
        match faults.iter_mut().find(|(k, _)| *k == name) {
            Some(e) => e.1 = value,
            None => faults.push((name, value)),
        }
    }
    let af = faults.iter().position(|(k, _)| k == "AF").and_then(|i| faults.remove(i).1);
    faults.sort_by(|a, b| a.0.cmp(&b.0));

    let hstate = py::int(top("heaterstate"));
    let src = |name: &str| lookup(&SRC_STATE, py::int(s(name)));
    let timer = py::int(s("cnstheattimerval"));
    let runtime = py::int(h("runtime"));
    let altitude = h("altitude");
    let altitude_m = if altitude.is_empty() {
        None
    } else {
        Some(py::round(py::float_str(altitude).ok_or_else(|| StatusError::Altitude(altitude.to_string()))?, 0))
    };
    Ok(Status {
        version: top("version").to_string(),
        heat_version: top("heatversion").to_string(),
        uptime: py::int(top("uptime")),
        system_heat: top("systemheat") == "1",
        heater_state_code: hstate,
        heater_state: hstate
            .map(|c| lookup(&HEATER_STATE, Some(c)).map_or_else(|| format!("state {c}"), str::to_string)),
        cabin_c: scaled(top("currenttemp"), 10.0),
        cabin_rh: scaled(top("currenthumidity"), 10.0),
        setpoint_c: scaled(s("setpoint"), 10.0),
        thermostat: s("thermenabled") == "1",
        fan_enabled: s("fanenabled") == "1",
        fan_running: s("fanstate") == "1",
        fan_speed: fan,
        fan_auto_pct: if fan == Some(FanSpeed::Auto) { py::int(h("pidspeed")) } else { None },
        pump_running: s("pumpstate") == "1",
        sources: SOURCE_BITS.iter().filter(|(_, bit)| hs & bit != 0).map(|(k, _)| *k).collect(),
        furnace: src("furnacesrc"),
        electric: src("electricsrc"),
        engine: src("enginesrc"),
        floor: src("floorsrc"),
        aux: src("auxsrc"),
        hot_water: s("cnstheat") == "1",
        // Python: `_int(v) if (_int(v) or -1) >= 0 else None`, so a timer of 0 also reads as None
        hot_water_timer_s: timer.filter(|v| *v > 0),
        preheat: s("preheatsrc") == "1",
        glycol: lookup(&GLYCOL, py::int(top("glycolstate"))),
        hot_tank: lookup(&HOT_TANK, py::int(s("glycol"))),
        available: Available {
            electric: s("electricenable") == "1",
            engine: s("engineenable") == "1",
            preheat: s("preheatenable") == "1",
            floor: s("floorenable") == "1",
            aux: s("auxenable") == "1",
        },
        battery_v: scaled(h("battv"), 10.0),
        // Python: a runtime of 0 also reads as None
        runtime_h: runtime.filter(|r| *r != 0).map(|r| py::round(r as f64 / 60.0, 1)),
        flame_c: scaled(h("flametemp"), 100.0),
        inlet_c: scaled(h("inlettemp"), 100.0),
        outlet_c: scaled(h("outlettemp"), 100.0),
        altitude_m,
        fan_rpm: py::int(h("burnermotor")),
        glow_w: scaled(h("glowpin"), 10.0),
        fuel_hz: scaled(h("dosingpump"), 10.0),
        fault_active: af.filter(|v| *v != 0 && *v != 255),
        fault_history: faults.iter().filter_map(|(_, v)| v.filter(|v| *v != 0)).collect(),
    })
}

/// The `climate.rixens.*` (and, with `cabin_from_rixens`, `climate.cabin.*`) keys of one status, in Python's
/// order. `now` is the wall time (Unix s) of `climate.rixens.updated`.
pub fn to_state(d: &Status, cabin_from_rixens: bool, now: f64) -> Vec<(String, Value)> {
    let mut out: Vec<(String, Value)> = d
        .to_map()
        .into_iter()
        .filter(|(k, _)| k != "available")
        .map(|(k, v)| (format!("climate.rixens.{k}"), v))
        .collect();
    let burning = d.heater_state_code.is_some_and(|c| BURNING.contains(&c));
    let state = if burning {
        "heating"
    } else if d.heater_state_code == Some(200) {
        "error"
    } else if !d.sources.is_empty() {
        "standby"
    } else {
        "off"
    };
    out.extend([
        ("climate.rixens.available".into(), d.available.to_json()),
        ("climate.rixens.live".into(), json!(true)),
        ("climate.rixens.coolant".into(), py::num(d.outlet_c)),
        ("climate.rixens.burner".into(), json!(burning)),
        ("climate.rixens.state".into(), json!(state)),
        ("climate.cabin.setpoint".into(), py::num(d.setpoint_c)),
        ("climate.rixens.updated".into(), py::num(Some(py::round(now, 1)))),
    ]);
    if cabin_from_rixens {
        out.extend([
            ("climate.cabin.temp".into(), py::num(d.cabin_c)),
            ("climate.cabin.humidity".into(), py::num(d.cabin_rh)),
            ("climate.cabin.source".into(), json!("rixens")),
        ]);
    }
    out
}

/// What a sent command must change, checked on a later status (Python's `verify(status) -> bool`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verify {
    /// `round(setpoint_c * 10) == tenths`
    Setpoint(i64),
    /// `fan_speed == "auto"`
    FanAuto,
    /// `fan_speed == pct`
    FanPct(i64),
    /// `(source in sources) == on`
    Source { source: &'static str, on: bool },
    /// `bool(status[mode]) == on` (hot_water, preheat, thermostat)
    Mode { mode: &'static str, on: bool },
}

impl Verify {
    pub fn check(&self, s: &Status) -> bool {
        match *self {
            Verify::Setpoint(val) => s.setpoint_c.is_some_and(|c| py::round_int(c * 10.0) == val),
            Verify::FanAuto => s.fan_speed == Some(FanSpeed::Auto),
            Verify::FanPct(pct) => s.fan_speed == Some(FanSpeed::Pct(pct)),
            Verify::Source { source, on } => s.has_source(source) == on,
            Verify::Mode { mode, on } => s.mode(mode) == on,
        }
    }
}

/// One allowed request: send `interface.cgi?act=<act>&val=<val>` once, then poll until `verify` holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    pub act: i64,
    pub val: i64,
    pub verify: Verify,
}

/// Why a request is refused before anything is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// Python `CommandError`: the service logs outcome "rejected" with this text and replies with it.
    Rejected(String),
    /// The request value isn't a number where one is needed (Python's `float()`/`int()` raise ValueError or
    /// TypeError, with this text, out of `plan()`). Python's `command()` catches those in its generic handler:
    /// outcome "failed", detail `"status read: <text>"`, reply "MCS7 not reachable". Nothing is sent.
    BadValue(String),
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanError::Rejected(s) | PlanError::BadValue(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for PlanError {}

/// Validate a request against the allowlist → the act to send, or `None` if no change is needed (the
/// current status already matches). `value` is the command's JSON value, read as Python does: `float()`
/// for the setpoint, `"auto"` or `int()` for the fan, and truthiness (`bool()`) for the on/off commands.
pub fn plan(cmd: &str, value: &Value, cur: &Status) -> Result<Option<Plan>, PlanError> {
    let tenths = |c: f64| py::round_int(c * 10.0);
    if cmd == "setpoint" {
        let c = py::float_of(value).map_err(PlanError::BadValue)?;
        if !(SETPOINT_MIN_C..=SETPOINT_MAX_C).contains(&c) {
            return Err(PlanError::Rejected(format!(
                "setpoint {} °C outside {}–{} °C",
                py::float_repr(c),
                py::float_repr(SETPOINT_MIN_C),
                py::float_repr(SETPOINT_MAX_C)
            )));
        }
        let val = tenths(c);
        if cur.setpoint_c.is_some_and(|s| tenths(s) == val) {
            return Ok(None);
        }
        return Ok(Some(Plan { act: 1, val, verify: Verify::Setpoint(val) }));
    }
    if cmd == "fan" {
        if value.as_str() == Some("auto") {
            if cur.fan_speed == Some(FanSpeed::Auto) {
                return Ok(None);
            }
            return Ok(Some(Plan { act: 2, val: 999, verify: Verify::FanAuto }));
        }
        let pct = py::int_of(value).map_err(PlanError::BadValue)?;
        if !(0..=100).contains(&pct) {
            return Err(PlanError::Rejected("fan speed must be 0–100 or 'auto'".into()));
        }
        let pct = pct as i64;
        if cur.fan_speed == Some(FanSpeed::Pct(pct)) {
            return Ok(None);
        }
        return Ok(Some(Plan { act: 2, val: pct, verify: Verify::FanPct(pct) }));
    }
    if let Some(&(source, act)) = SOURCE_ACT.iter().find(|(k, _)| *k == cmd) {
        let on = py::truthy(value);
        let available = match source {
            "electric" => cur.available.electric,
            "engine" => cur.available.engine,
            _ => true, // furnace: always installed
        };
        if !available {
            return Err(PlanError::Rejected(format!("{cmd} heat is not installed/enabled on this MCS7")));
        }
        if cur.has_source(source) == on {
            return Ok(None);
        }
        return Ok(Some(Plan { act, val: on as i64, verify: Verify::Source { source, on } }));
    }
    if let Some(&(mode, act)) = MODE_ACT.iter().find(|(k, _)| *k == cmd) {
        let on = py::truthy(value);
        if mode == "preheat" && !cur.available.preheat {
            return Err(PlanError::Rejected("engine pre-heat is not enabled on this MCS7".into()));
        }
        if cur.mode(mode) == on {
            return Ok(None);
        }
        return Ok(Some(Plan { act, val: on as i64, verify: Verify::Mode { mode, on } }));
    }
    Err(PlanError::Rejected(format!("command {} is not allowed", py::str_repr(cmd))))
}

/// Today's burner fuel (Python `RixensHttpAdapter._account_fuel` and `fuel_lph`). Persistence is the
/// caller's: restore the saved row with [`FuelAccount::restore`], save `(day, today_l)` whenever
/// [`FuelAccount::account`] returns true.
#[derive(Debug, Clone)]
pub struct FuelAccount {
    pub ml_per_pulse: f64,
    day: String,
    today_l: f64,
    last_t: Option<f64>,
}

impl FuelAccount {
    /// `ml_per_pulse`: the configured `fuel_ml_per_pulse` (missing or 0 → [`DEFAULT_ML_PER_PULSE`]);
    /// `today`: the local day (`%Y-%m-%d`) at start.
    pub fn new(ml_per_pulse: Option<f64>, today: &str) -> Self {
        Self {
            ml_per_pulse: ml_per_pulse.filter(|v| *v != 0.0).unwrap_or(DEFAULT_ML_PER_PULSE),
            day: today.to_string(),
            today_l: 0.0,
            last_t: None,
        }
    }

    /// The saved counter row (`counters` key `rixens.fuel_l`): used only when it is from the current day.
    pub fn restore(&mut self, day: &str, value: f64) {
        if day == self.day {
            self.today_l = value;
        }
    }

    pub fn day(&self) -> &str {
        &self.day
    }

    pub fn today_l(&self) -> f64 {
        self.today_l
    }

    /// Dosing-pump Hz → l/h (ml/s × 3600 / 1000), 3 decimals.
    pub fn lph(&self, hz: Option<f64>) -> Option<f64> {
        hz.map(|hz| py::round(hz * self.ml_per_pulse * 3.6, 3))
    }

    /// Integrate the fuel flow since the previous status into today's total; it resets when `local_day`
    /// (`%Y-%m-%d` of `now` in local time) changes. A gap of 120 s or more (or a clock going back) is not
    /// integrated. Returns true when the total changed (save it).
    pub fn account(&mut self, fuel_hz: Option<f64>, now: f64, local_day: &str) -> bool {
        if local_day != self.day {
            self.day = local_day.to_string();
            self.today_l = 0.0;
        }
        let lph = self.lph(fuel_hz).unwrap_or(0.0);
        let mut changed = false;
        if let Some(t) = self.last_t
            && 0.0 < now - t
            && now - t < 120.0
            && lph > 0.0
        {
            self.today_l += lph * (now - t) / 3600.0;
            changed = true;
        }
        self.last_t = Some(now);
        changed
    }

    /// The five fuel keys of one status, in Python's order.
    pub fn state(&self, fuel_hz: Option<f64>) -> Vec<(String, Value)> {
        let lph = self.lph(fuel_hz);
        vec![
            ("climate.rixens.fuel_lph".into(), py::num(lph)),
            ("climate.rixens.fuel_gph".into(), py::num(lph.map(|l| py::round(l / L_PER_GAL, 4)))),
            ("climate.rixens.fuel_today_l".into(), py::num(Some(py::round(self.today_l, 3)))),
            ("climate.rixens.fuel_today_gal".into(), py::num(Some(py::round(self.today_l / L_PER_GAL, 4)))),
            ("climate.rixens.fuel_ml_per_pulse".into(), py::num(Some(self.ml_per_pulse))),
        ]
    }
}

/// Everything Python's `_publish(status)` sets, in order: [`to_state`], the fuel keys, then
/// `climate.rixens.reachable = true`. Call [`FuelAccount::account`] for this status first, as Python does.
pub fn publish_state(d: &Status, cabin_from_rixens: bool, fuel: &FuelAccount, now: f64) -> Vec<(String, Value)> {
    let mut out = to_state(d, cabin_from_rixens, now);
    out.extend(fuel.state(d.fuel_hz));
    out.push(("climate.rixens.reachable".into(), json!(true)));
    out
}
