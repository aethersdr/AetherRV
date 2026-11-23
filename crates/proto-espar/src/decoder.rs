//! The Espar decoder (Python `EsparAdapter` + `CaptureOnlyAdapter`): heater status, the controller's commands, and
//! the read-only UDS schedule. It never transmits: what it wants sent (flow control, a read request) it returns,
//! and the service sends it through TxGate device `<device>_diag`.
//!
//! ```text
//! 0x054 controller → heater, 200 ms   D1 flags (00 never run, 09 on, 08 after a run; bit 0 = on)
//!                                      D2 mode (05 heat, 02 fan, 00 off)   D3-D4 setpoint 0.1 °C LE (FE FF = none)
//!                                      D5-D6 room (°C + 50) × 10 LE (FE FF = none)
//! 0x065 controller, ~10 s            D1-D2 room word as above
//! 0x2C4 heater → controller, 200 ms   D1 state (00 init, 01 stopping, 02 startup, 03 idle, 08 init, 09 heating,
//!                                      0B fault, 21 fan)   D2 flags (04 running ~5 min in, 08 after-run)
//!                                      D3 fault code   D5-D6 temperature, (°C + 50) × 10 LE: while running a copy
//!                                      of the controller's room value; otherwise the heater's own sensor
//! 0x625 heater heartbeat 100 ms; 0x60D controller heartbeat 100 ms
//! ```
//!
//! The bus sleeps ~5-15 s after the heater goes idle: silence is "asleep", not a fault.
//!
//! Times (`now`) are seconds on one monotonic clock chosen by the caller (the service uses `CLOCK_BOOTTIME`).

use crate::uds::{self, IsoTpRx, Reply};
use crate::{Frame, hex_spaced, temp_word};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

pub const AFTER_RUN_S: f64 = 240.0;
pub const ASLEEP_S: f64 = 20.0;
pub const SCAN_GAP_S: f64 = 0.3;
pub const SCAN_WATCH_S: f64 = 1800.0;

pub fn state_name(code: u8) -> String {
    match code {
        0x00 | 0x08 => "init".into(),
        0x01 => "stopping".into(),
        0x02 => "startup".into(),
        0x03 => "idle".into(),
        0x09 => "heating".into(),
        0x0B => "fault".into(),
        0x21 => "fan".into(),
        x => format!("0x{x:02x}"),
    }
}

pub fn fault_text(code: u8) -> Option<&'static str> {
    match code {
        0x04 => Some("power-up lock (heater ignores commands until it gets a non-idle command then idle)"),
        0x20 => Some("flame loss / fuel starvation"),
        0x40 => Some("overtemperature / exhaust blocked"),
        _ => None,
    }
}

fn mode_name(b: u8) -> String {
    match b {
        0x05 => "heat".into(),
        0x02 => "fan".into(),
        0x00 => "off".into(),
        x => format!("0x{x:02x}"),
    }
}

/// The controller's last 054 (and 065 room) as seen on the bus; `None` = not in Python's dict.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Cmd {
    pub flags: Option<u8>,
    pub mode: Option<String>,
    pub setpoint_c: Option<f64>,
    pub room_c: Option<f64>,
}

/// The heater's last 2C4.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub code: u8,
    pub name: String,
    pub flags: u8,
    pub fault: u8,
    pub temp_c: Option<f64>,
}

/// What `on_frame` asks the caller to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    None,
    /// 054/60D heard from a controller that isn't us (we never receive our own frames).
    OtherController,
    /// A multi-frame UDS reply started: send this flow-control frame (device `<dev>_diag`).
    SendDiag([u8; 8]),
}

#[derive(Debug, Clone)]
struct IdEntry {
    count: u64,
    ext: bool,
    data: String,
    seen: f64,
}

pub struct Decoder {
    pub device: String,
    pub diag_interval: f64,
    isotp: IsoTpRx,
    pub diag: BTreeMap<String, (Value, f64)>,
    pub dtcs: Option<Vec<Value>>,
    pub diag_sent: u64,
    pub diag_err: Option<String>,
    pub cmd: Cmd,
    pub st: Option<Status>,
    pub last_rx: Option<f64>,
    pub last_heater: Option<f64>,
    pub last_ctrl: Option<f64>,
    pub heat_since: Option<f64>,
    pub heat_cmd_since: Option<f64>,
    pub last_other_ctrl: Option<f64>,
    /// Set by the controller while AetherRV drives the heater (our own control counts as "controller present").
    pub ctl_active: bool,
    pub after_since: Option<f64>,
    ids: BTreeMap<u32, IdEntry>,
    // the owner-approved read-only DID scan
    pub scan_dids: Vec<u16>,
    pub scan_todo: Vec<u16>,
    pub scan_res: BTreeMap<u16, String>,
    pub scan_wait: Option<u16>,
    pub scan_watch: Vec<u16>,
    pub scan_until: Option<f64>,
    // the diag schedule (Python run())
    once: Vec<u16>,
    live: usize,
    dtc_at: f64,
    next_req: f64,
    scan_at: f64,
    scan_tick: u64,
}

impl Decoder {
    pub fn new(device: &str, diag_interval: f64, scan: bool) -> Self {
        let scan_dids: Vec<u16> = (0xFD00..=0xFDFF).collect();
        Decoder {
            device: device.to_string(),
            diag_interval: diag_interval.max(1.0),
            isotp: IsoTpRx::new(),
            diag: BTreeMap::new(),
            dtcs: None,
            diag_sent: 0,
            diag_err: None,
            cmd: Cmd::default(),
            st: None,
            last_rx: None,
            last_heater: None,
            last_ctrl: None,
            heat_since: None,
            heat_cmd_since: None,
            last_other_ctrl: None,
            ctl_active: false,
            after_since: None,
            ids: BTreeMap::new(),
            scan_todo: if scan { scan_dids.clone() } else { vec![] },
            scan_dids,
            scan_res: BTreeMap::new(),
            scan_wait: None,
            scan_watch: vec![],
            scan_until: None,
            once: uds::ONCE_DIDS.to_vec(),
            live: 0,
            dtc_at: f64::NEG_INFINITY, // the first regular read is the fault memory (Python: time.time() − 0 > 600)
            next_req: 0.0,
            scan_at: 0.0,
            scan_tick: 0,
        }
    }

    pub fn st_name(&self) -> Option<&str> {
        self.st.as_ref().map(|s| s.name.as_str())
    }

    pub fn on_frame(&mut self, f: &Frame, now: f64) -> Event {
        // CaptureOnlyAdapter.on_frame
        if f.error || f.remote {
            return Event::None;
        }
        let e = self.ids.entry(f.id).or_insert(IdEntry { count: 0, ext: f.extended, data: String::new(), seen: now });
        e.count += 1;
        e.data = hex_spaced(&f.data);
        e.seen = now;
        if f.extended {
            return Event::None;
        }
        let (i, d) = (f.id, f.data.as_slice());
        if i == uds::RESP_ID {
            // a diagnostic reply (ours, or the EasyStart Pro's workshop menu)
            let (payload, need_fc) = self.isotp.feed(d, now);
            let mut ev = Event::None;
            if need_fc {
                ev = Event::SendDiag(uds::FLOW_CONTROL);
            }
            if let Some(p) = payload {
                self.scan_reply(&p);
                match uds::decode(&p) {
                    Reply::Did { key, value, .. } => {
                        self.diag.insert(key, (value, now));
                    }
                    Reply::Dtcs(d) => self.dtcs = Some(d),
                    Reply::Negative { service, nrc } => {
                        self.diag_err = Some(format!("negative reply {{'service': {service}, 'nrc': {nrc}}}"))
                    }
                    _ => {}
                }
            }
            return ev;
        }
        if i == uds::REQ_ID {
            return Event::None;
        }
        self.last_rx = Some(now);
        match i {
            0x054 if d.len() >= 6 => {
                let sp = u16::from(d[2]) | u16::from(d[3]) << 8;
                self.cmd = Cmd {
                    flags: Some(d[0]),
                    mode: Some(mode_name(d[1])),
                    setpoint_c: if sp == 0xFFFE || sp == 0xFFFF { None } else { Some(r1(sp as f64 / 10.0)) },
                    room_c: temp_word(d[4], d[5]),
                };
                if self.cmd.mode.as_deref() == Some("heat") {
                    self.heat_cmd_since.get_or_insert(now);
                } else {
                    self.heat_cmd_since = None;
                }
                self.last_ctrl = Some(now);
                self.last_other_ctrl = Some(now);
                return Event::OtherController;
            }
            0x065 if d.len() >= 2 => {
                if let Some(r) = temp_word(d[0], d[1]) {
                    self.cmd.room_c = Some(r);
                }
            }
            0x60D => {
                self.last_ctrl = Some(now);
                self.last_other_ctrl = Some(now);
                return Event::OtherController;
            }
            0x2C4 if d.len() >= 6 => {
                let (code, flags) = (d[0], d[1]);
                let name = state_name(code);
                if name == "heating" && self.heat_since.is_none() {
                    self.heat_since = Some(now);
                }
                if name != "heating" {
                    self.heat_since = None;
                }
                let after = name == "idle" && flags & 0x08 != 0;
                if after && self.after_since.is_none() {
                    self.after_since = Some(now);
                }
                if !after {
                    self.after_since = None;
                }
                self.st = Some(Status { code, name, flags, fault: d[2], temp_c: temp_word(d[4], d[5]) });
                self.last_heater = Some(now);
            }
            0x625 => self.last_heater = Some(now),
            _ => {}
        }
        Event::None
    }

    /// Python `CaptureOnlyAdapter.summary()` with the Espar names (the Systems page's frame list).
    pub fn summary(&self, now: f64) -> Vec<Value> {
        self.ids
            .iter()
            .map(|(aid, e)| {
                let id = if e.ext { format!("{aid:08X}") } else { format!("{aid:03X}") };
                let (frame, decoded) = if e.ext {
                    ("wake-up (29-bit)".to_string(), String::new())
                } else {
                    let raw: Vec<u8> = e.data.split(' ').filter_map(|h| u8::from_str_radix(h, 16).ok()).collect();
                    crate::describe(*aid, &raw)
                };
                json!({"id": id, "frame": frame, "decoded": decoded, "count": e.count, "data": e.data,
                       "age": r1(now - e.seen)})
            })
            .collect()
    }

    pub fn asleep(&self, now: f64) -> bool {
        self.last_rx.is_none_or(|t| now - t > ASLEEP_S)
    }

    /// The `climate.espar.*` keys (Python `snapshot()`), in Python's order.
    pub fn snapshot(&self, now: f64) -> Vec<(String, Value)> {
        let asleep = self.asleep(now);
        let st = self.st.clone().unwrap_or_default();
        let st_name = self.st.as_ref().map(|s| s.name.as_str());
        // the EasyStart Pro's last command expires once it's gone
        let cmd_live = self.last_other_ctrl.is_some_and(|t| now - t <= 3.0);
        let cmd_mode = if cmd_live { self.cmd.mode.as_deref() } else { None };
        let state = if asleep {
            "off"
        } else if st_name == Some("fault") || (st.fault != 0 && st_name != Some("idle")) {
            "error"
        } else if st_name == Some("heating") {
            "heating"
        } else if st_name == Some("fan") {
            "fan"
        } else if matches!(st_name, Some("startup") | Some("init") | Some("stopping")) {
            if matches!(cmd_mode, Some("heat") | Some("fan")) { "starting" } else { "off" }
        } else if self.after_since.is_some() {
            "cooldown"
        } else {
            "off"
        };
        let fault = if self.st.is_some() { st.fault } else { 0 };
        let running = matches!(state, "heating" | "fan" | "starting" | "cooldown");
        let mut issue = Value::Null;
        if !asleep && self.heat_cmd_since.is_some_and(|t| now - t > 30.0) && st_name != Some("heating") {
            issue = json!(if fault == 0x04 {
                "power-up lock: the heater ignores HEAT after a power interruption. Run FAN for 5 s, then OFF, then HEAT"
            } else {
                "heat commanded for 30 s but the heater isn't heating"
            });
        }
        let p = "climate.espar.";
        let k = |s: &str| format!("{p}{s}");
        let opt = |v: Option<f64>| v.map(|x| json!(x)).unwrap_or(Value::Null);
        vec![
            (k("state"), json!(state)),
            (k("asleep"), json!(asleep)),
            (k("heard"), json!(self.last_rx.is_some())),
            (k("mode"), if asleep { Value::Null } else { cmd_mode.map(|m| json!(m)).unwrap_or(Value::Null) }),
            (k("setpoint_c"), if asleep || cmd_mode != Some("heat") { Value::Null } else { opt(self.cmd.setpoint_c) }),
            (k("room_c"), if asleep || !cmd_live { Value::Null } else { opt(self.cmd.room_c) }),
            // while running the heater's temperature field copies the room value; only report its own sensor
            (k("heater_c"), if asleep || running { Value::Null } else { opt(st.temp_c) }),
            (k("fault"), if fault != 0 { json!(format!("0x{fault:02x}")) } else { Value::Null }),
            (k("fault_text"), if fault != 0 { json!(fault_text(fault).unwrap_or("unknown fault code")) } else { Value::Null }),
            (k("run_s"), match self.heat_since {
                Some(t) if !asleep => json!(round_int(now - t)),
                _ => Value::Null,
            }),
            (k("cooldown_left_s"), match self.after_since {
                Some(t) if !asleep => json!(round_int(AFTER_RUN_S - (now - t)).max(0)),
                _ => Value::Null,
            }),
            (k("controller"), match self.last_ctrl {
                None => Value::Null,
                Some(t) => json!(now - t < ASLEEP_S),
            }),
            (k("issue"), issue),
            (k("last_seen"), opt(self.last_rx)),
        ]
    }

    /// Heater and controller both talking within the last 2 s: only then do we ask (we never wake the bus).
    pub fn awake(&self, now: f64) -> bool {
        let ctrl = self.ctl_active || self.last_ctrl.is_some_and(|t| now - t < 2.0);
        self.last_heater.is_some_and(|t| now - t < 2.0) && ctrl
    }

    /// The `climate.espar.diag.*` keys. `enabled`: TxGate `<device>_diag`.
    pub fn diag_snapshot(&self, now: f64, enabled: bool) -> Vec<(String, Value)> {
        let p = "climate.espar.diag.";
        let k = |s: &str| format!("{p}{s}");
        let mut out = vec![
            (k("enabled"), json!(enabled)),
            (k("sent"), json!(self.diag_sent)),
            (k("error"), self.diag_err.clone().map(Value::String).unwrap_or(Value::Null)),
            (k("dtcs"), self.dtcs.clone().map(Value::Array).unwrap_or(Value::Null)),
        ];
        let keep = (self.diag_interval * (uds::live_dids().len() as f64 + 5.0)).max(30.0);
        for (key, (v, t)) in &self.diag {
            let sticky = matches!(key.as_str(), "software" | "hardware" | "hours" | "event_count" | "pump_strokes" | "air_hpa");
            out.push((k(key), if now - t < keep || sticky { v.clone() } else { Value::Null }));
        }
        // the heater's own fuel per pump stroke: lifetime fuel ÷ lifetime strokes (µl)
        let fuel = self.diag.get("fuel").map(|(v, _)| v).filter(|v| v.as_object().is_some_and(|m| !m.is_empty()));
        let strokes = self.diag.get("pump_strokes").and_then(|(v, _)| v.as_f64()).unwrap_or(0.0);
        out.push((
            k("ul_per_stroke"),
            match fuel.and_then(|f| f.get("total_l")).and_then(Value::as_f64) {
                Some(f) if strokes != 0.0 => json!(r1(f * 1e6 / strokes)),
                _ => Value::Null,
            },
        ));
        if !self.scan_res.is_empty() {
            let m: Map<String, Value> = self
                .scan_res
                .iter()
                .filter(|(_, v)| !v.starts_with("NRC"))
                .map(|(d, v)| (format!("{d:04X}"), json!(v)))
                .collect();
            out.push((k("scan"), Value::Object(m)));
        }
        let lock = self.dtcs.as_ref().and_then(|d| {
            d.iter().find(|x| x.get("lockout") == Some(&json!(true)) && x.get("active") == Some(&json!(true))).cloned()
        });
        out.push((
            k("lockout"),
            match lock {
                Some(l) => {
                    let code = l["code"].as_str().unwrap_or_default().to_string();
                    json!({"code": code, "why": uds::lockout_reason(&code).unwrap_or(""), "help": uds::LOCKOUT_HELP})
                }
                None => Value::Null,
            },
        ));
        out
    }

    fn scan_reply(&mut self, p: &[u8]) {
        let mut did = if p.len() >= 3 && p[0] == 0x62 { Some(u16::from(p[1]) << 8 | u16::from(p[2])) } else { None };
        let val;
        if did.is_none() && p.len() >= 3 && p[0] == 0x7F && p[1] == 0x22 && self.scan_wait.is_some() {
            did = self.scan_wait;
            val = format!("NRC 0x{:02x}", p[2]);
        } else if let Some(d) = did
            && (Some(d) == self.scan_wait || self.scan_res.contains_key(&d))
        {
            val = hex_spaced(&p[3..]);
        } else {
            return;
        }
        let did = did.unwrap();
        if Some(did) == self.scan_wait {
            self.scan_wait = None;
        }
        if self.scan_res.get(&did) != Some(&val) {
            self.scan_res.insert(did, val);
        }
    }

    /// One scan request if one is due (read service 0x22 only). Returns the frame to send.
    pub fn scan_step(&mut self, now: f64) -> Option<[u8; 8]> {
        let did = if !self.scan_todo.is_empty() {
            self.scan_todo.remove(0)
        } else if self.scan_until.is_none() && !self.scan_res.is_empty() {
            // pass 1 done → pass 2 on the undecoded responders
            self.scan_watch = self
                .scan_res
                .iter()
                .filter(|(d, v)| !v.starts_with("NRC") && uds::did_key(**d).is_none())
                .map(|(d, _)| *d)
                .collect();
            self.scan_until = Some(now + SCAN_WATCH_S);
            return None;
        } else if !self.scan_watch.is_empty() && self.scan_until.is_some_and(|u| now < u) {
            let d = self.scan_watch.remove(0);
            self.scan_watch.push(d);
            d
        } else {
            return None;
        };
        self.scan_wait = Some(did);
        Some(uds::read_did(did))
    }

    fn scanning(&self, now: f64) -> bool {
        !self.scan_todo.is_empty()
            || (!self.scan_res.is_empty() && self.scan_until.is_none())
            || self.scan_until.is_some_and(|u| now < u)
    }

    /// The diag schedule (Python `run()`'s body): the next read request to send now, if any. Call every ~0.25 s
    /// while diagnostics are enabled; report each send with [`Decoder::note_sent`].
    pub fn diag_due(&mut self, now: f64, enabled: bool) -> Option<[u8; 8]> {
        if !enabled || !self.awake(now) {
            return None;
        }
        if self.scanning(now) && now >= self.scan_at && self.once.is_empty() {
            self.scan_tick += 1;
            if !self.scan_todo.is_empty() || self.scan_tick % 2 == 1 {
                let r = self.scan_step(now);
                self.scan_at = if !self.scan_todo.is_empty() { now + SCAN_GAP_S } else { now + 1.0 };
                return r;
            }
            self.scan_at = now + 1.0;
        }
        if now >= self.next_req {
            self.next_req = now + self.diag_interval;
            if !self.once.is_empty() {
                return Some(uds::read_did(self.once.remove(0)));
            } else if now - self.dtc_at > 600.0 {
                self.dtc_at = now;
                return Some(uds::read_dtcs());
            } else {
                let l = uds::live_dids();
                let d = l[self.live % l.len()];
                self.live += 1;
                return Some(uds::read_did(d));
            }
        }
        None
    }

    pub fn note_sent(&mut self) {
        self.diag_sent += 1;
    }

    pub fn note_send_error(&mut self, what: &str, err: &str) {
        self.diag_err = Some(format!("{what}: {err}"));
    }

    /// The intake reading the controller may use as room temperature (the adapter blanks it when old).
    pub fn intake_c(&self, now: f64) -> Option<f64> {
        let keep = (self.diag_interval * (uds::live_dids().len() as f64 + 5.0)).max(30.0);
        self.diag.get("temps").filter(|(_, t)| now - t < keep).and_then(|(v, _)| v.get("intake_c")).and_then(Value::as_f64)
    }
}

fn r1(x: f64) -> f64 {
    format!("{x:.1}").parse().unwrap()
}

fn round_int(x: f64) -> i64 {
    format!("{x:.0}").parse::<f64>().unwrap() as i64
}
