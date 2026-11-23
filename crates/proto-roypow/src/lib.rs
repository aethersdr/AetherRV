//! EPOCH lithium batteries over Bluetooth LE (RoyPow BMS protocol). READ-ONLY.
//!
//! Port of `src/aetherrv/adapters/epoch_ble.py` (the pure parts; the BlueZ connection code lives in the
//! `aetherrv-batteries-ble` service).
//!
//! The van's four EPOCH 12 V 460 Ah batteries advertise as " B12460A  2402000xxx " and carry a RoyPow
//! "smart BMS". Its BLE UART is GATT service ffe0, characteristic ffe1 (write + notify). Each read is a
//! query frame and a framed reply:
//!
//! ```text
//! query  EA D1 01 <len=04> FF <cmd> <xor of len..cmd> F5
//! reply  EA D1 01 <len> FF <cmd> <payload…> <xor> F5     (split across several notifications)
//!
//!   cmd 0x04: SoC, pack voltage, remaining Ah, cycles, runtime
//!   cmd 0x03: current (sign bit), problem code, temperatures, charge/discharge MOSFETs
//!   cmd 0x02: cell voltages
//! ```
//!
//! Field positions come from aiobmsble's RoyPow plugin (<https://pypi.org/p/aiobmsble/>, Apache-2.0,
//! `aiobmsble/bms/roypow_bms.py` and the `BaseBMS` helpers `_decode_data`, `_cell_voltages`,
//! `_temp_values`, version 0.33.0); its field map and decoding rules are reimplemented here. Its own
//! connection code is not used, because these modules reject the standard Battery Level subscription and
//! often fail the first ffe1 subscription, after which they drop the link.
//!
//! Only the three read queries are ever written; nothing that changes a setting.

use std::collections::{BTreeMap, HashMap};

use serde_json::{Map, Value, json};

mod py;

/// The BLE UART characteristic (write + notify).
pub const UART: &str = "0000ffe1-0000-1000-8000-00805f9b34fb";
pub const HEAD: [u8; 3] = [0xEA, 0xD1, 0x01];
pub const TAIL: u8 = 0xF5;
/// The three read queries, in the order a full read sends them.
pub const CMDS: [u8; 3] = [0x04, 0x03, 0x02];
/// The BLE module's own AT chatter, stripped from the start of a notification.
const BT_MODULE_MSG: &[u8] = b"AT+STAT\r\n";

/// Seconds without a good reply after which a battery counts as not reporting.
pub const STALE: f64 = 30.0;
/// Polls between cell-voltage (0x02) queries.
pub const CELLS_EVERY: u64 = 5;
/// Seconds to wait for a reply before dropping the link and reconnecting.
pub const REPLY: f64 = 3.0;

/// The query frame for one read command (only 0x04, 0x03 and 0x02 are ever sent).
pub fn query(cmd: u8) -> [u8; 8] {
    let data = [0x04, 0xFF, cmd];
    let x = data.iter().fold(0u8, |a, b| a ^ b);
    [HEAD[0], HEAD[1], HEAD[2], data[0], data[1], data[2], x, TAIL]
}

/// The poll period from the configured interval (`max(0.5, interval)`).
pub fn poll_period(interval: f64) -> f64 {
    0.5f64.max(interval)
}

/// The queries of poll number `n` of a session: the cell voltages only every [`CELLS_EVERY`] polls.
pub fn poll_cmds(n: u64) -> &'static [u8] {
    if n.is_multiple_of(CELLS_EVERY) { &[0x04, 0x03, 0x02] } else { &[0x04, 0x03] }
}

/// Collect notifications into complete, checked RoyPow frames keyed by command byte.
#[derive(Debug, Clone, Default)]
pub struct FrameAssembler {
    buf: Vec<u8>,
    /// The last good frame per command byte (whole frame, head to tail).
    pub msgs: BTreeMap<u8, Vec<u8>>,
}

impl FrameAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one notification. A complete frame with a good XOR and tail lands in [`Self::msgs`]; a bad one
    /// is dropped. Bytes after the end of a frame in the same notification are discarded.
    pub fn feed(&mut self, data: &[u8]) {
        let data = data.strip_prefix(BT_MODULE_MSG).unwrap_or(data);
        if data.starts_with(&HEAD) {
            self.buf.clear();
        }
        self.buf.extend_from_slice(data);
        if !self.buf.starts_with(&HEAD) || self.buf.len() < 4 {
            if !self.buf.starts_with(&HEAD[..self.buf.len().min(HEAD.len())]) {
                self.buf.clear();
            }
            return;
        }
        let total = 4 + self.buf[3] as usize; // head(3) + len byte + len
        if self.buf.len() < total {
            return;
        }
        let mut frame = std::mem::take(&mut self.buf);
        frame.truncate(total);
        // frame[3:-2], empty when the frame is shorter than 5 bytes (as Python slices)
        let x = frame[3..total.saturating_sub(2).max(3)].iter().fold(0u8, |a, b| a ^ b);
        if frame[total - 1] != TAIL || frame[total - 2] != x {
            return; // bad frame
        }
        if let Some(&cmd) = frame.get(5) {
            self.msgs.insert(cmd, frame);
        }
    }
}

/// One decoded battery read: the Python `decode()` dict, field for field.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reading {
    pub soc: Option<u64>,
    pub voltage: Option<f64>,
    pub current: Option<f64>,
    pub remaining_ah: Option<f64>,
    pub cycles: Option<u64>,
    pub problem_code: Option<u64>,
    pub charge_fet: Option<bool>,
    pub discharge_fet: Option<bool>,
    pub runtime_h: Option<f64>,
    pub cells: Vec<f64>,
    pub cell_delta_mv: Option<i64>,
    pub temps: Vec<f64>,
}

impl Reading {
    /// The Python dict, in its key order (`None` → null).
    pub fn to_map(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("soc".into(), json!(self.soc));
        m.insert("voltage".into(), py::num(self.voltage));
        m.insert("current".into(), py::num(self.current));
        m.insert("remaining_ah".into(), py::num(self.remaining_ah));
        m.insert("cycles".into(), json!(self.cycles));
        m.insert("problem_code".into(), json!(self.problem_code));
        m.insert("charge_fet".into(), json!(self.charge_fet));
        m.insert("discharge_fet".into(), json!(self.discharge_fet));
        m.insert("runtime_h".into(), py::num(self.runtime_h));
        m.insert("cells".into(), Value::Array(self.cells.iter().map(|c| py::num(Some(*c))).collect()));
        m.insert("cell_delta_mv".into(), json!(self.cell_delta_mv));
        m.insert("temps".into(), Value::Array(self.temps.iter().map(|t| py::num(Some(*t))).collect()));
        m
    }
}

/// Big-endian unsigned field `size` bytes at `pos` of the frame for command `idx` (aiobmsble `_decode_data`:
/// a missing frame or a field past its end is skipped).
fn field(msgs: &BTreeMap<u8, Vec<u8>>, idx: u8, pos: usize, size: usize) -> Option<u64> {
    let msg = msgs.get(&idx)?;
    let b = msg.get(pos..pos + size)?;
    Some(b.iter().fold(0u64, |a, &x| (a << 8) | x as u64))
}

/// RoyPow frames → the battery reading (field map from aiobmsble).
pub fn decode(msgs: &BTreeMap<u8, Vec<u8>>) -> Reading {
    let battery_level = field(msgs, 0x4, 7, 1);
    let voltage = field(msgs, 0x4, 47, 2).map(|x| x as f64 / 100.0);
    let current = field(msgs, 0x3, 6, 3).map(|x| {
        let sign = if (x >> 16) & 0x1 != 0 { -1 } else { 1 };
        ((x & 0xFFFF) as i64 * sign) as f64 / 100.0
    });
    let problem_code = field(msgs, 0x3, 9, 3);
    let cycle_charge =
        field(msgs, 0x4, 24, 4).map(|x| ((x & 0xFFFF_0000) | ((x & 0xFF00) >> 8) | ((x & 0xFF) << 8)) as f64 / 1000.0);
    let runtime = field(msgs, 0x4, 30, 2).map(|x| x * 60);
    let temp_sensors = field(msgs, 0x3, 13, 1);
    let cycles = field(msgs, 0x4, 9, 2);
    let chrg_mosfet = field(msgs, 0x3, 24, 1).map(|x| x & 0x4 != 0);
    let dischrg_mosfet = field(msgs, 0x3, 24, 1).map(|x| x & 0x2 != 0);

    // aiobmsble _cell_voltages(start=9, size=2, divider=1000): a raw 0 is skipped.
    let cells: Vec<f64> = match msgs.get(&0x2) {
        Some(m) => {
            let n = m.len().saturating_sub(11) / 2;
            (0..n)
                .filter_map(|i| m.get(9 + 2 * i..9 + 2 * i + 2))
                .map(|b| u16::from_be_bytes([b[0], b[1]]))
                .filter(|&v| v != 0)
                .map(|v| v as f64 / 1000.0)
                .collect()
        }
        None => Vec::new(),
    };
    // aiobmsble _temp_values(start=14, size=1, unsigned, offset=40): a raw 0 (= -40 °C) is skipped.
    let temps: Vec<f64> = match msgs.get(&0x3) {
        Some(m) => (0..temp_sensors.unwrap_or(0) as usize)
            .filter_map(|i| m.get(14 + i).copied())
            .filter(|&v| v != 0)
            .map(|v| (v as i64 - 40) as f64 / 1.0)
            .collect(),
        None => Vec::new(),
    };

    let cell_delta_mv = if cells.is_empty() {
        None
    } else {
        let max = cells.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let min = cells.iter().copied().fold(f64::INFINITY, f64::min);
        Some(py::round_int((max - min) * 1000.0))
    };
    Reading {
        soc: battery_level,
        voltage,
        current,
        remaining_ah: cycle_charge.map(|c| py::round(c, 1)),
        cycles,
        problem_code,
        charge_fet: chrg_mosfet,
        discharge_fet: dischrg_mosfet,
        runtime_h: match runtime {
            None => None,
            Some(r) if r == 0xFFFF * 60 => None, // charging: no runtime
            Some(r) => Some(py::round(r as f64 / 3600.0, 1)),
        },
        cells: cells.iter().map(|c| py::round(*c, 3)).collect(),
        cell_delta_mv,
        temps,
    }
}

/// One configured battery (`[[device.epoch_ble.battery]]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Battery {
    pub name: String,
    pub address: String,
    pub serial: String,
}

impl Battery {
    /// As Python's adapter: an empty or missing name falls back to the address; a missing serial is "".
    pub fn new(name: Option<&str>, address: &str, serial: Option<&str>) -> Self {
        let name = name.filter(|n| !n.is_empty()).unwrap_or(address);
        Self { name: name.to_string(), address: address.to_string(), serial: serial.unwrap_or("").to_string() }
    }
}

/// What the service knows about one battery (Python `EpochBleAdapter.last[address]`).
#[derive(Debug, Clone, Default)]
pub struct Record {
    /// Wall time (Unix s) of the last good read. Python treats a 0 the same as none.
    pub t: Option<f64>,
    /// The last good reading.
    pub data: Option<Reading>,
    pub connected: bool,
    /// The last error text (`"<ExceptionName>: <message>"`, at most 120 characters), cleared by a good read.
    pub error: Option<String>,
}

/// The `batteries.ble*` state keys, in Python's order, from the configured batteries and what is known about
/// each (keyed by address). A battery counts as fresh (`ok`) when its last good read is at most [`STALE`] old.
pub fn publish(batteries: &[Battery], last: &HashMap<String, Record>, now: f64) -> Vec<(String, Value)> {
    let mut rows = Vec::with_capacity(batteries.len());
    let mut good_currents: Vec<Option<f64>> = Vec::new();
    let mut good = 0usize;
    for b in batteries {
        let empty = Record::default();
        let d = last.get(&b.address).unwrap_or(&empty);
        let t = d.t.filter(|t| *t != 0.0); // Python: bool(d.get("t"))
        let ok = t.is_some_and(|t| now - t <= STALE);
        let mut row = Map::new();
        row.insert("name".into(), json!(b.name));
        row.insert("address".into(), json!(b.address));
        row.insert("serial".into(), json!(b.serial));
        if let Some(data) = &d.data {
            row.extend(data.to_map());
        }
        row.insert("ok".into(), json!(ok));
        row.insert("age".into(), py::num(t.map(|t| py::round(now - t, 1))));
        row.insert("connected".into(), json!(d.connected));
        row.insert("error".into(), json!(d.error));
        if ok {
            good += 1;
            good_currents.push(d.data.as_ref().and_then(|r| r.current));
        }
        rows.push(Value::Object(row));
    }
    let current = if good == 0 {
        Value::Null
    } else {
        match py::sum(good_currents.into_iter().flatten()) {
            Some(s) => py::num(Some(py::round(s, 2))),
            None => json!(0), // Python: round(sum([]), 2) is the int 0
        }
    };
    let expected = rows.len();
    vec![
        ("batteries.ble".to_string(), Value::Array(rows)),
        ("batteries.ble.reporting".to_string(), json!(good)),
        ("batteries.ble.expected".to_string(), json!(expected)),
        ("batteries.ble.current".to_string(), current),
    ]
}
