//! PGN decoders, from CANboat's canboat.json (v10.0.0) definitions.
//!
//! * 127501 Binary Switch Bank Status: single frame, 8 bytes. Byte 0 = bank instance, then 28 indicators of 2 bits
//!   each, packed LSB-first from bit 8. Values: 0 = Off, 1 = On (CANboat OFF_ON). 2 and 3 are unnamed in CANboat;
//!   by NMEA convention 2 = error and 3 = not available (unused channel).
//! * 126208 group function: header only (function code + commanded PGN); the parameter pairs stay raw hex.
//! * 60928 ISO Address Claim (the 64-bit NAME), 126996 Product Information.
//! * 127508 Battery Status, 127507 Charger Status (alternator regulators such as the Wakespeed WS500).
//! * 127500 Load Controller Connection State (CLMD16): [SID][connection][state 0 off/2 on/4 PWM][status][op][duty %].
//! * 127751 DC Voltage/Current: [SID][connection][V uint16 ×0.1][I int24 ×0.01 A, 0xFFFFFF = n/a].
//!   On the CLMD16, connections 0-15 are the outputs, 16-23 the wired inputs (voltage only).

use crate::pgns;
use crate::pyfmt::round;
use std::fmt;

/// A frame a decoder rejected (Python `ValueError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError(pub String);

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DecodeError {}

/// A 127501 indicator. "Not available" (3) is `None` in [`BankStates`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SwitchState {
    Off,
    On,
    Error,
}

impl SwitchState {
    /// The string Python uses (`"off"`, `"on"`, `"error"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
            Self::Error => "error",
        }
    }
}

/// The 28 channels of a 127501 bank; `None` = not available / unused channel.
pub type BankStates = [Option<SwitchState>; 28];

fn le(b: &[u8]) -> u64 {
    b.iter().rev().fold(0u64, |acc, &x| (acc << 8) | u64::from(x))
}

/// 127501 Binary Switch Bank Status → (bank instance, 28 channel states).
pub fn decode_127501(data: &[u8]) -> Result<(u8, BankStates), DecodeError> {
    if data.len() < 8 {
        return Err(DecodeError(format!("127501 needs 8 bytes, got {}", data.len())));
    }
    let bits = le(&data[1..8]);
    let mut states: BankStates = [None; 28];
    for (i, s) in states.iter_mut().enumerate() {
        *s = match (bits >> (2 * i)) & 0b11 {
            0 => Some(SwitchState::Off),
            1 => Some(SwitchState::On),
            2 => Some(SwitchState::Error),
            _ => None,
        };
    }
    Ok((data[0], states))
}

/// Python `GROUP_FUNCTION`.
pub fn group_function_name(code: u8) -> Option<&'static str> {
    Some(match code {
        0 => "Request",
        1 => "Command",
        2 => "Acknowledge",
        3 => "Read Fields",
        4 => "Read Fields Reply",
        5 => "Write Fields",
        6 => "Write Fields Reply",
        _ => return None,
    })
}

/// A 126208 group function header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupFunctionHeader {
    /// The function name, or `0xNN` for an unknown code.
    pub function: String,
    /// The commanded/requested PGN.
    pub pgn: u32,
    /// The rest of the message as lowercase hex pairs separated by spaces (`"f8 02 01 00"`).
    pub rest: String,
}

/// 126208 group function header: function code (8 bits) + commanded/requested PGN (24 bits LE).
pub fn decode_126208_header(data: &[u8]) -> Result<GroupFunctionHeader, DecodeError> {
    if data.len() < 4 {
        return Err(DecodeError("126208 header needs 4 bytes".into()));
    }
    let function = group_function_name(data[0]).map_or_else(|| format!("0x{:02X}", data[0]), str::to_string);
    let rest = data[4..].iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ");
    Ok(GroupFunctionHeader { function, pgn: le(&data[1..4]) as u32, rest })
}

/// A decoded ISO NAME (60928).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressClaim {
    /// The NAME as 16 uppercase hex digits.
    pub name: String,
    pub unique: u32,
    /// Manufacturer name, or `#<code>` when unknown.
    pub manufacturer: String,
    pub function: u8,
    /// Device class name, or `#<class>` when unknown.
    pub device_class: String,
    pub device_instance: u8,
}

/// 60928 ISO Address Claim: the 64-bit NAME (CANboat isoAddressClaim, bit offsets LSB-first).
pub fn decode_60928(data: &[u8]) -> Result<AddressClaim, DecodeError> {
    if data.len() < 8 {
        return Err(DecodeError("60928 needs 8 bytes".into()));
    }
    let n = le(&data[..8]);
    let mfr = ((n >> 21) & 0x7FF) as i64;
    let cls = ((n >> 49) & 0x7F) as i64;
    Ok(AddressClaim {
        name: format!("{n:016X}"),
        unique: (n & 0x1FFFFF) as u32,
        manufacturer: pgns::manufacturer(mfr).map_or_else(|| format!("#{mfr}"), str::to_string),
        function: ((n >> 40) & 0xFF) as u8,
        device_class: pgns::device_class(cls).map_or_else(|| format!("#{cls}"), str::to_string),
        device_instance: ((((n >> 35) & 0x1F) << 3) | ((n >> 32) & 0x7)) as u8,
    })
}

/// STRING_FIX field: padded with 0x00, 0xFF, '@' or spaces (Python `_fixstr`).
fn fixstr(b: &[u8]) -> String {
    let upto = b.iter().position(|&x| x == 0).map_or(b, |i| &b[..i]);
    let s: String =
        upto.iter().filter(|&&x| x != 0xFF).map(|&x| if x.is_ascii() { char::from(x) } else { '\u{FFFD}' }).collect();
    s.trim_end_matches([' ', '@']).to_string()
}

/// 126996 Product Information.
#[derive(Debug, Clone, PartialEq)]
pub struct ProductInfo {
    pub n2k_version: f64,
    pub product_code: u16,
    pub model: String,
    pub software: String,
    pub model_version: String,
    pub serial: String,
    pub len: u8,
}

/// 126996 Product Information (fast-packet, 134 bytes): model, software, serial, LEN.
pub fn decode_126996(data: &[u8]) -> Result<ProductInfo, DecodeError> {
    if data.len() < 134 {
        return Err(DecodeError(format!("126996 needs 134 bytes, got {}", data.len())));
    }
    Ok(ProductInfo {
        n2k_version: round(le(&data[0..2]) as f64 * 0.001, 3),
        product_code: le(&data[2..4]) as u16,
        model: fixstr(&data[4..36]),
        software: fixstr(&data[36..68]),
        model_version: fixstr(&data[68..100]),
        serial: fixstr(&data[100..132]),
        len: data[133],
    })
}

/// 127508 Battery Status.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BatteryStatus {
    pub instance: u8,
    /// Volts (0.01 V resolution), `None` = n/a.
    pub voltage: Option<f64>,
    /// Amps (0.1 A, signed), `None` = n/a.
    pub current: Option<f64>,
    /// °C (from 0.01 K), `None` = n/a.
    pub temp: Option<f64>,
}

/// 127508 Battery Status (single frame): instance, voltage 0.01 V, current 0.1 A (signed), temperature 0.01 K.
/// Needs 7 bytes (the adapter only calls it with ≥ 7; Python's `struct` would raise on fewer).
pub fn decode_127508(data: &[u8]) -> Result<BatteryStatus, DecodeError> {
    if data.len() < 7 {
        return Err(DecodeError(format!("127508 needs 7 bytes, got {}", data.len())));
    }
    let v = u16::from_le_bytes([data[1], data[2]]);
    let a = i16::from_le_bytes([data[3], data[4]]);
    let t = u16::from_le_bytes([data[5], data[6]]);
    Ok(BatteryStatus {
        instance: data[0],
        voltage: (v != 0xFFFF).then(|| round(f64::from(v) * 0.01, 2)),
        current: (a != 0x7FFF).then(|| round(f64::from(a) * 0.1, 1)),
        temp: (t != 0xFFFF).then(|| round(f64::from(t) * 0.01 - 273.15, 1)),
    })
}

/// Python `CHARGER_STATE`.
pub fn charger_state_name(code: u8) -> Option<&'static str> {
    Some(match code {
        0 => "not charging",
        1 => "bulk",
        2 => "absorption",
        3 => "overcharge",
        4 => "equalise",
        5 => "float",
        6 => "no float",
        7 => "constant VI",
        8 => "disabled",
        9 => "fault",
        _ => return None,
    })
}

/// 127506 DC Detailed Status DC type: alternator (how regulators such as the WS500 announce themselves).
pub const DC_TYPE_ALTERNATOR: u8 = 1;

/// 127507 Charger Status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChargerStatus {
    pub instance: u8,
    pub battery: u8,
    /// Operating state name; `None` for a code outside the table.
    pub state: Option<&'static str>,
    /// `None` = n/a (3).
    pub enabled: Option<bool>,
}

/// 127507 Charger Status: charger instance, battery instance, operating state (low nibble), enabled (2 bits).
/// Needs 4 bytes (the adapter only calls it with ≥ 4).
pub fn decode_127507(data: &[u8]) -> Result<ChargerStatus, DecodeError> {
    if data.len() < 4 {
        return Err(DecodeError(format!("127507 needs 4 bytes, got {}", data.len())));
    }
    let en = data[3] & 0x03;
    Ok(ChargerStatus {
        instance: data[0],
        battery: data[1],
        state: charger_state_name(data[2] & 0x0F),
        enabled: if en == 3 { None } else { Some(en != 0) },
    })
}

/// 127500 Load Controller Connection State: `(connection, state, status, duty)`; `None` below 6 bytes.
pub fn decode_127500(data: &[u8]) -> Option<(u8, u8, u8, u8)> {
    (data.len() >= 6).then(|| (data[1], data[2], data[3], data[5]))
}

/// 127751 DC Voltage/Current: `(connection, volts, amps)`; `None` below 7 bytes.
pub fn decode_127751(data: &[u8]) -> Option<(u8, Option<f64>, Option<f64>)> {
    if data.len() < 7 {
        return None;
    }
    let v = u16::from_le_bytes([data[2], data[3]]);
    let raw = le(&data[4..7]) as i32;
    let i = (raw << 8) >> 8; // sign-extend 24 bits
    Some((
        data[1],
        (v != 0xFFFF).then(|| round(f64::from(v) * 0.1, 1)),
        (i != 0x7FFFFF && i != -1).then(|| round(f64::from(i) * 0.01, 2)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{encode_127501, name_bytes, product_payload};
    use SwitchState::{Error, Off, On};

    /// py: tests/test_n2k.py::test_127501_independent_vector
    #[test]
    fn test_127501_independent_vector() {
        // ch1 on (01), ch2 off (00), ch3 on (01), ch4 error (10) -> byte1 = 0b10_01_00_01 = 0x91,
        // every other channel "not available" (11) -> 0xFF
        let data = [3, 0x91, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
        let (inst, st) = decode_127501(&data).unwrap();
        assert_eq!(inst, 3);
        assert_eq!(st[..5], [Some(On), Some(Off), Some(On), Some(Error), None]);
        assert!(st[4..] == [None; 24] && st.len() == 28);
    }

    /// py: tests/test_n2k.py::test_127501_roundtrip_with_bench_encoder
    #[test]
    fn test_127501_roundtrip_with_bench_encoder() {
        let states = [1, 0, 1, 1, 0, 0, 1, 0];
        let (inst, st) = decode_127501(&encode_127501(7, &states)).unwrap();
        let want: Vec<_> = states.iter().map(|&s| Some(if s == 1 { On } else { Off })).collect();
        assert!(inst == 7 && st[..8] == want[..] && st[8].is_none());
    }

    /// py: tests/test_n2k.py::test_127501_short_frame_rejected
    #[test]
    fn test_127501_short_frame_rejected() {
        assert!(decode_127501(b"\x00\x01").is_err());
    }

    /// py: tests/test_n2k.py::test_126208_header
    #[test]
    fn test_126208_header() {
        // Command (1) for PGN 127501 = 0x01F20D little-endian, then raw parameters
        let h = decode_126208_header(&[1, 0x0D, 0xF2, 0x01, 0xF8, 0x02, 0x01, 0x00]).unwrap();
        assert!(h.function == "Command" && h.pgn == 127501 && h.rest == "f8 02 01 00");
    }

    /// py: tests/test_n2k.py::test_60928_vector
    #[test]
    fn test_60928_vector() {
        // Hand-packed NAME: unique 12345, Maretron (137), instance 0b00011_010 = 26, function 140, class 30
        let d = decode_60928(&name_bytes(12345, 137, 0b010, 0b00011, 140, 30, 0, 4, 1)).unwrap();
        assert!(d.unique == 12345 && d.manufacturer == "Maretron");
        assert!(d.device_instance == 26 && d.function == 140 && d.device_class == "Electrical Distribution");
        // independent byte check: unique 12345 = 0x003039, mfr 137 << 21 sets byte 2 high bits 0b001 << 5 and byte 3 = 0x11
        let raw = name_bytes(12345, 137, 0, 0, 0, 0, 0, 0, 0);
        assert_eq!(raw[..4], [0x39, 0x30, 0x20, 0x11]);
    }

    /// py: tests/test_n2k.py::test_126996_vector
    #[test]
    fn test_126996_vector() {
        let d = decode_126996(&product_payload("IPG100", "1.6.2", "1234567", 3)).unwrap();
        assert_eq!(
            d,
            ProductInfo {
                n2k_version: 2.1,
                product_code: 1234,
                model: "IPG100".into(),
                software: "1.6.2".into(),
                model_version: "1".into(),
                serial: "1234567".into(),
                len: 3,
            }
        );
    }

    /// STRING_FIX: cut at NUL, 0xFF removed, non-ASCII replaced, trailing spaces and '@' stripped.
    #[test]
    fn fixstr_like_python() {
        assert_eq!(fixstr(b"AB\xffC @@ \x00junk"), "ABC");
        assert_eq!(fixstr(b"\xe9x"), "\u{FFFD}x");
        assert_eq!(fixstr(b"@@@"), "");
    }

    /// 127751 current is a signed 24-bit field; 0x7FFFFF and 0xFFFFFF (-1) are n/a.
    #[test]
    fn dc_current_sign_and_na() {
        assert_eq!(decode_127751(&[0, 3, 0x8A, 0x00, 0x18, 0xFC, 0xFF]), Some((3, Some(13.8), Some(-10.0))));
        assert_eq!(decode_127751(&[0, 3, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]), Some((3, None, None)));
        assert_eq!(decode_127751(&[0, 3, 0, 0, 0xFF, 0xFF, 0x7F]), Some((3, Some(0.0), None)));
        assert_eq!(decode_127751(&[0, 3, 0, 0, 0, 0]), None);
    }
}
