//! Espar heater diagnostics: UDS (ISO 14229) over ISO-TP on the heater's CAN bus. READ requests only.
//!
//! Transport and data identifiers from the public Airtronic S3 B2L work (TrooperDuper/espar-airtronic-esphome,
//! docs/uds-diagnostics.md and docs/fault-codes.md); facts only, no code taken.
//!
//! ```text
//! request 0x7A0 (tester → heater), reply 0x73C (heater → tester), 8-byte frames padded with 0xAA
//! read:        03 22 <DID hi> <DID lo> AA AA AA AA     (default session, no unlock needed)
//! faults:      03 19 02 08 AA AA AA AA                  (reportDTCByStatusMask, confirmed DTCs)
//! flow control after a first frame: 30 00 14 AA AA AA AA AA   (clear to send, no block limit, 20 ms)
//! replies: 62 <DID> <data…> | 59 02 <mask> {3 code bytes + 1 status}… | 7F <service> <NRC>
//! multi-byte values on this channel are BIG-endian (the cyclic 2C4/054 frames are little-endian)
//! ```
//!
//! Never sent: writes (0x2E), routine control (0x31), ECU reset (0x11), session changes (0x10), security access
//! (0x27), clearing faults (0x14). [`request`] refuses any service other than 0x22 and 0x19; it is the only way a
//! request is built.

use serde_json::{Map, Value, json};

pub const REQ_ID: u32 = 0x7A0;
pub const RESP_ID: u32 = 0x73C;
pub const PAD: u8 = 0xAA;
pub const FLOW_CONTROL: [u8; 8] = [0x30, 0x00, 0x14, PAD, PAD, PAD, PAD, PAD];
pub const READ_ONLY_SERVICES: [u8; 2] = [0x22, 0x19];

#[derive(Debug, PartialEq, Eq)]
pub struct NotAllowed(pub String);

/// One single-frame read request. Only ReadDataByIdentifier (0x22) and ReadDTCInformation (0x19).
pub fn request(service: u8, args: &[u8]) -> Result<[u8; 8], NotAllowed> {
    if !READ_ONLY_SERVICES.contains(&service) {
        return Err(NotAllowed(format!("UDS service 0x{service:02x} is not allowed (read-only)")));
    }
    if args.len() + 1 > 7 {
        return Err(NotAllowed("request too long for a single frame".into()));
    }
    let mut out = [PAD; 8];
    out[0] = (args.len() + 1) as u8;
    out[1] = service;
    out[2..2 + args.len()].copy_from_slice(args);
    Ok(out)
}

pub fn read_did(did: u16) -> [u8; 8] {
    request(0x22, &[(did >> 8) as u8, did as u8]).expect("0x22 is read-only")
}

pub fn read_dtcs() -> [u8; 8] {
    request(0x19, &[0x02, 0x08]).expect("0x19 is read-only")
}

/// Combustion phases (FD10 byte 1). 0x0C seen on this S2 D2L (2026-11-22): glow plug on (~80 W), then fuel pump
/// 2 Hz, until the flame is detected (~2 min).
pub fn phase_name(b: u8) -> Option<&'static str> {
    Some(match b {
        0x00 => "off",
        0xD2 => "ventilation",
        0x04 | 0x0F | 0x10 => "startup",
        0x07 => "pre-glow",
        0x0C | 0x08 => "ignition",
        0x09 => "flame ramp-up",
        0x14 => "burning",
        0x28 => "control pause",
        0x1E => "cool-down",
        _ => return None,
    })
}

pub const P_CODES: &[(&str, &str)] = &[
    ("P0001A", "lockout: too many overheating events"),
    ("P00010", "overheat/outlet sensor open"),
    ("P000101", "overheat/outlet sensor short"),
    ("P000110", "air inlet sensor open"),
    ("P000111", "air inlet sensor short"),
    ("P00010A", "cold-blowing timeout"),
    ("P000114", "possible overheating (implausible signal)"),
    ("P000115", "overheating (software threshold)"),
    ("P000116", "overheating (hardware threshold, >150 °C)"),
    ("P00011A", "lockout: too many overheating events"),
    ("P000120", "flame sensor open"),
    ("P000121", "flame sensor short"),
    ("P000125", "flame cut-out during start"),
    ("P000126", "flame cut-out at 0-25 % power"),
    ("P000127", "flame cut-out at 25-50 % power"),
    ("P000128", "flame cut-out at 50-75 % power"),
    ("P000129", "flame cut-out at 75-100 % power"),
    ("P00012A", "start failed"),
    ("P00012B", "lockout: 10 failed starts in a row"),
    ("P000143", "air pressure sensor implausible"),
    ("P000200", "fuel pump open circuit"),
    ("P000201", "fuel pump short"),
    ("P000202", "fuel pump shorted to + / driver fault"),
    ("P000210", "glow plug open circuit"),
    ("P000211", "glow plug short"),
    ("P000212", "glow plug shorted to +"),
    ("P000213", "glow plug: ignition energy too low"),
    ("P000223", "burner motor blocked"),
    ("P000224", "burner motor current too high"),
    ("P000301", "watchdog reset"),
    ("P000302", "too many watchdog resets"),
    ("P000304", "too many resets (loose contact?)"),
    ("P000307", "CAN communication error in the control unit"),
    ("P00030A", "CAN communication error"),
    ("P000310", "over-voltage cut-out"),
    ("P000312", "under-voltage cut-out"),
    ("P000313", "under-voltage (>20 s)?"),
    ("P000342", "too many CAN controllers on the bus"),
    ("P000440", "timeout talking to the controller"),
    ("P000450", "LIN communication error"),
];

pub fn p_code_text(code: &str) -> Option<&'static str> {
    P_CODES.iter().find(|(c, _)| *c == code).map(|(_, t)| *t)
}

/// Severity class per P-code (Airtronic S3 service manual via the S3 project): 1 service, 2 under-voltage,
/// 3 over-voltage, 4 fuel supply (heat stops), 5 air system (immediate stop), 6 overheating lockout, 7 limp/log.
pub fn severity(code: &str) -> i64 {
    match code {
        "P0001A" | "P00011A" => 6,
        "P000115" | "P000116" => 5,
        "P00012A" | "P000200" | "P000201" | "P000202" => 4,
        "P000310" => 3,
        "P000312" => 2,
        "P000143" | "P000130" | "P000131" => 7,
        _ => 1,
    }
}

pub fn lockout_reason(code: &str) -> Option<&'static str> {
    match code {
        "P00012B" => Some("10 failed starts in a row"),
        "P00011A" | "P0001A" => Some("too many overheating events"),
        _ => None,
    }
}

pub const LOCKOUT_HELP: &str = "Locked out by the heater. Reset: switch the heater on, pull the heater's fuse within 20 s, \
     wait ~5 s, refit it. Find and fix the cause first (fuel, exhaust, air path).";

fn u16be(d: &[u8], i: usize) -> Option<u32> {
    (d.len() >= i + 2).then(|| u32::from(d[i]) << 8 | u32::from(d[i + 1]))
}

fn u32be(d: &[u8], i: usize) -> Option<u64> {
    (d.len() >= i + 4).then(|| u64::from(u32::from_be_bytes([d[i], d[i + 1], d[i + 2], d[i + 3]])))
}

fn r1(x: f64) -> f64 {
    format!("{x:.1}").parse().unwrap()
}
fn r3(x: f64) -> f64 {
    format!("{x:.3}").parse().unwrap()
}

/// FD01 style: raw / 100 − 50 °C, FFFF = not fitted.
fn temp100(w: Option<u32>) -> Value {
    match w {
        None | Some(0xFFFF) => Value::Null,
        Some(w) => json!(r1(w as f64 / 100.0 - 50.0)),
    }
}

/// FD45 style: (°C + 50) × 10.
fn temp10(w: Option<u32>) -> Value {
    match w {
        None | Some(0xFFFE) | Some(0xFFFF) => Value::Null,
        Some(w) => json!(r1(w as f64 / 10.0 - 50.0)),
    }
}

fn ascii(d: &[u8]) -> Value {
    let s = String::from_utf8_lossy(d);
    json!(s.trim_matches(|c| c == '\0' || c == ' '))
}

/// The DIDs with a name: `(DID, state key)`.
pub const DIDS: &[(u16, &str)] = &[
    (0xFD04, "voltage_v"),
    (0xFD08, "blower"),
    (0xFD0A, "glow_w"),
    (0xFD0D, "pump_hz"),
    (0xFD0E, "power_pct"),
    (0xFD0F, "flame"),
    (0xFD10, "phase"),
    (0xFD11, "fuel"),
    (0xFD01, "temps"),
    (0xFD45, "regulation"),
    (0xFD2A, "burn_s"),
    (0xFD17, "hours"),
    (0xFD36, "air_hpa"),
    (0xFD6A, "pump_strokes"),
    (0xFD2B, "ignition_s"),
    (0xFD35, "unit_c"),
    (0xFD06, "event_count"),
    (0xF195, "software"),
    (0xF193, "hardware"),
    (0xFD58, "variant"),
];

pub fn did_key(did: u16) -> Option<&'static str> {
    DIDS.iter().find(|(d, _)| *d == did).map(|(_, k)| *k)
}

/// Decode a DID's data (the Python lambdas of `DIDS`).
pub fn decode_did(did: u16, d: &[u8]) -> Value {
    let u = |i| u16be(d, i);
    match did {
        0xFD04 => u(0).map(|v| json!(r1(v as f64 / 10.0))).unwrap_or(Value::Null),
        0xFD08 => u(0).map(|v| json!((v as i64 - 20000).max(0))).unwrap_or(Value::Null),
        0xFD0A => u(0).map(|v| json!(r1(v as f64 / 10.0))).unwrap_or(Value::Null),
        0xFD0D => d.first().map(|b| json!(r1(*b as f64 / 10.0))).unwrap_or(Value::Null),
        0xFD0E => u(0).map(|v| json!(r1(v as f64 / 100.0))).unwrap_or(Value::Null),
        0xFD0F => {
            if d.len() >= 2 {
                let fuel = match d[1] {
                    1 => json!("no fuel"),
                    2 => json!("pumping"),
                    x => json!(x),
                };
                json!({"flame": d[0] != 0, "fuel_state": fuel})
            } else {
                Value::Null
            }
        }
        0xFD10 => {
            if d.len() >= 2 {
                let req = match d[0] {
                    0 => json!("off"),
                    1 => json!("heat"),
                    3 => json!("fan"),
                    x => json!(x),
                };
                let ph = phase_name(d[1]).map(|s| json!(s)).unwrap_or_else(|| json!(format!("0x{:02x}", d[1])));
                json!({"request": req, "phase": ph})
            } else {
                Value::Null
            }
        }
        0xFD11 => {
            if d.len() >= 10 {
                let whole = u32be(d, 2).unwrap() as f64;
                let frac = u32be(d, 6).unwrap() as f64 / 1e9;
                json!({"rate_ul_s": d[1], "total_l": r3(whole + frac)})
            } else {
                Value::Null
            }
        }
        // 4 words on the S2 D2L: flame, intake, outlet (found by the 2026-11-22 scan), and a 4th that reads 0 °C
        0xFD01 => {
            if d.len() >= 4 {
                json!({"flame_c": temp100(u(0)), "intake_c": temp100(u(2)), "outlet_c": temp100(u(4))})
            } else {
                Value::Null
            }
        }
        0xFD45 => {
            if d.len() >= 4 {
                json!({"setpoint_c": temp10(u(0)), "regulated_c": temp10(u(2))})
            } else {
                Value::Null
            }
        }
        0xFD2A | 0xFD2B => u(0).map(|v| json!(r1(v as f64 / 100.0))).unwrap_or(Value::Null),
        0xFD17 => {
            let mut m = Map::new();
            for (i, k) in ["operating", "p0_25", "p25_50", "p50_75", "p75_100", "pause", "altitude"].iter().enumerate() {
                if let Some(v) = u32be(d, i * 4) {
                    m.insert(k.to_string(), json!(r1(v as f64 / 60.0)));
                }
            }
            Value::Object(m)
        }
        0xFD36 => u(0).map(|v| json!(r1(v as f64 / 10.0))).unwrap_or(Value::Null),
        0xFD6A => u32be(d, 0).map(|v| json!(v)).unwrap_or(Value::Null),
        0xFD35 => temp100(u(0)),
        0xFD06 => u(0).map(|v| json!(v)).unwrap_or(Value::Null),
        0xF195 | 0xF193 | 0xFD58 => ascii(d),
        _ => json!(crate::hex_spaced(d)),
    }
}

/// What the Climate card shows is read every cycle; the slower values one per cycle, in turn.
pub const FAST_DIDS: [u16; 8] = [0xFD10, 0xFD01, 0xFD08, 0xFD0A, 0xFD0D, 0xFD0E, 0xFD0F, 0xFD11];
pub const SLOW_DIDS: [u16; 6] = [0xFD04, 0xFD45, 0xFD2A, 0xFD2B, 0xFD36, 0xFD35];
pub const ONCE_DIDS: [u16; 5] = [0xF195, 0xF193, 0xFD17, 0xFD06, 0xFD6A];

/// `[*FAST, slow1, *FAST, slow2, ...]`
pub fn live_dids() -> Vec<u16> {
    let mut v = Vec::new();
    for s in SLOW_DIDS {
        v.extend_from_slice(&FAST_DIDS);
        v.push(s);
    }
    v
}

/// Reassembles replies on 0x73C.
#[derive(Debug, Default)]
pub struct IsoTpRx {
    buf: Vec<u8>,
    want: usize,
    next_sn: u8,
    t: f64,
}

impl IsoTpRx {
    pub fn new() -> Self {
        IsoTpRx { buf: vec![], want: 0, next_sn: 1, t: 0.0 }
    }

    /// → (complete payload, need flow control)
    pub fn feed(&mut self, d: &[u8], now: f64) -> (Option<Vec<u8>>, bool) {
        if d.is_empty() {
            return (None, false);
        }
        match d[0] >> 4 {
            0 => {
                // single frame: length in the low nibble; extra bytes are junk
                let n = (d[0] & 0x0F) as usize;
                self.want = 0;
                ((n > 0 && n <= 7).then(|| d[1..(1 + n).min(d.len())].to_vec()), false)
            }
            1 => {
                if d.len() < 2 {
                    return (None, false);
                }
                self.want = ((d[0] as usize & 0x0F) << 8) | d[1] as usize;
                self.buf = d[2..d.len().min(8)].to_vec();
                self.next_sn = 1;
                self.t = now;
                (None, true)
            }
            2 if self.want > 0 => {
                if d[0] & 0x0F != self.next_sn || now - self.t > 1.0 {
                    self.want = 0; // out of order or stale: drop it
                    return (None, false);
                }
                self.buf.extend_from_slice(&d[1..d.len().min(8)]);
                self.next_sn = (self.next_sn + 1) & 0x0F;
                self.t = now;
                if self.buf.len() >= self.want {
                    let out = self.buf[..self.want].to_vec();
                    self.want = 0;
                    return (Some(out), false);
                }
                (None, false)
            }
            _ => (None, false),
        }
    }
}

/// A decoded UDS reply.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Did { did: u16, key: String, value: Value },
    Dtcs(Vec<Value>),
    Negative { service: u8, nrc: u8 },
    Raw(String),
    Empty,
}

impl Reply {
    /// The Python dict form (for tests and diagnostics).
    pub fn to_json(&self) -> Value {
        match self {
            Reply::Did { did, key, value } => json!({"did": did, "key": key, "value": value}),
            Reply::Dtcs(d) => json!({"dtcs": d}),
            Reply::Negative { service, nrc } => json!({"negative": {"service": service, "nrc": nrc}}),
            Reply::Raw(r) => json!({"raw": r}),
            Reply::Empty => json!({}),
        }
    }
}

pub fn decode(p: &[u8]) -> Reply {
    if p.is_empty() {
        return Reply::Empty;
    }
    match p[0] {
        0x62 if p.len() >= 3 => {
            let did = u16::from(p[1]) << 8 | u16::from(p[2]);
            let key = did_key(did).map(str::to_string).unwrap_or_else(|| format!("did_{did:04x}"));
            Reply::Did { did, key, value: decode_did(did, &p[3..]) }
        }
        0x59 if p.len() >= 3 && p[1] == 0x02 => {
            let mut dtcs = vec![];
            let mut i = 3;
            // Python: range(3, len(payload) - 3, 4)
            while i + 3 < p.len() {
                let code = format!("P{}", crate::hex_upper(&p[i..i + 3]));
                dtcs.push(json!({
                    "code": code,
                    "text": p_code_text(&code).unwrap_or("unknown"),
                    "active": p[i + 3] & 0x01 != 0,
                    "severity": severity(&code),
                    "lockout": lockout_reason(&code).is_some(),
                }));
                i += 4;
            }
            Reply::Dtcs(dtcs)
        }
        0x7F if p.len() >= 3 => Reply::Negative { service: p[1], nrc: p[2] },
        _ => Reply::Raw(crate::hex_spaced(p)),
    }
}
