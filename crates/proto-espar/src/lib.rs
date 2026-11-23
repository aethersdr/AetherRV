//! Espar (Eberspächer) Airtronic S2 D2L protocol, as captured on this van on 2026-11-22.
//!
//! Frame map: the public Airtronic S3 B2L work (TrooperDuper/espar-airtronic-esphome, docs/protocol-reference.md),
//! checked against this van's own captures. Facts only; no code taken from that project.
//!
//! * [`decoder`] reads the bus (never sends);
//! * [`uds`] builds read-only diagnostic requests and decodes the replies;
//! * this module holds the controller's frames (the EasyStart Pro's captured bytes).

pub mod decoder;
pub mod uds;

/// A received classic CAN frame (what the decoder needs).
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub id: u32,
    pub data: Vec<u8>,
    pub extended: bool,
    pub remote: bool,
    pub error: bool,
}

impl Frame {
    pub fn std(id: u32, data: &[u8]) -> Self {
        Frame { id, data: data.to_vec(), extended: false, remote: false, error: false }
    }
    pub fn hex(id: u32, h: &str) -> Self {
        Frame::std(id, &unhex(h))
    }
}

pub fn unhex(h: &str) -> Vec<u8> {
    let h: String = h.chars().filter(|c| !c.is_whitespace()).collect();
    (0..h.len() / 2).map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).expect("hex")).collect()
}

pub fn hex_spaced(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
}

pub fn hex_upper(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02X}")).collect()
}

/// `(°C + 50) × 10` little-endian; FE FF / FF FF = none.
pub fn temp_word(lo: u8, hi: u8) -> Option<f64> {
    let w = u16::from(lo) | u16::from(hi) << 8;
    if w == 0xFFFE || w == 0xFFFF {
        None
    } else {
        Some(format!("{:.1}", w as f64 / 10.0 - 50.0).parse().unwrap())
    }
}

/// The room word the controller sends; `None` → FE FF ("0 °C" to the heater while heating, i.e. full power, so the
/// controller never sends it while heating).
pub fn room_word(c: Option<f64>) -> [u8; 2] {
    match c {
        None => [0xFE, 0xFF],
        Some(c) => {
            let w = ((c.clamp(-40.0, 80.0) + 50.0) * 10.0).round_ties_even_py() as u16;
            w.to_le_bytes()
        }
    }
}

trait PyRound {
    fn round_ties_even_py(self) -> f64;
}
impl PyRound for f64 {
    /// Python `round(x)` (half to even).
    fn round_ties_even_py(self) -> f64 {
        format!("{self:.0}").parse().unwrap()
    }
}

/// Python `int(round(setpoint * 10))`.
pub fn setpoint_word(sp: f64) -> [u8; 2] {
    ((sp * 10.0).round_ties_even_py() as u16).to_le_bytes()
}

/// Our EasyStart Pro's own bytes (captured 2026-11-22), replayed so the heater sees the same controller settings.
pub const STARTUP_BURST: [(u32, &str); 18] = [
    (0x062, "12000000000001F1"),
    (0x066, "0000000000000000"),
    (0x10A, "100001201001"),
    (0x05C, "1E001E0000000000"),
    (0x05D, "000000000000FEFF"),
    (0x05E, "0000000000000000"),
    (0x05F, "1E001E0000000000"),
    (0x060, "000000000000FEFF"),
    (0x061, "0000000000000000"),
    (0x063, "3201010000000000"),
    (0x064, "3C000500C800FF03"),
    (0x067, "BE3E2700011E0000"),
    (0x068, "BE3E2700011E0000"),
    (0x069, "BE3E2700011E0000"),
    (0x06A, "BE3E2700011E0000"),
    (0x06B, "BE3E2700011E0000"),
    (0x06C, "BE3E2700011E0000"),
    (0x06D, "12000000000000F0"),
];

pub fn startup_burst() -> Vec<(u32, Vec<u8>)> {
    STARTUP_BURST.iter().map(|(i, h)| (*i, unhex(h))).collect()
}

pub const STATIC: [(u32, [u8; 8]); 3] = [
    (0x055, [0x10, 0x27, 0, 0, 0, 0, 0, 0]),
    (0x056, [0x10, 0x27, 0, 0, 0, 0, 0, 0]),
    (0x057, [0x00, 0x00, 0xFE, 0xFF, 0xFE, 0xFF, 0x00, 0x00]),
];

/// 60D as our Pro sends it (the heater doesn't check the ID).
pub const HEARTBEAT: [u8; 8] = [0x0D, 0x10, 0xFD, 0x1D, 0x02, 0x20, 0x70, 0x00];

pub fn mode_byte(mode: &str) -> Option<u8> {
    match mode {
        "off" => Some(0x00),
        "fan" => Some(0x02),
        "heat" => Some(0x05),
        _ => None,
    }
}

/// 054: `[flags | on][mode][setpoint LE or FE FF][room LE][00 00]`.
pub fn cmd_frame(flags: u8, mode: &str, setpoint_c: f64, room_c: Option<f64>) -> [u8; 8] {
    let on = matches!(mode, "heat" | "fan");
    let sp = if mode == "heat" { setpoint_word(setpoint_c) } else { [0xFE, 0xFF] };
    let r = room_word(room_c);
    [flags | if on { 0x01 } else { 0x00 }, mode_byte(mode).unwrap_or(0), sp[0], sp[1], r[0], r[1], 0, 0]
}

/// 065: room word, a 1-2-3 counter, five zero bytes.
pub fn room_frame(room_c: Option<f64>, counter: u8) -> [u8; 8] {
    let r = room_word(room_c);
    [r[0], r[1], counter, 0, 0, 0, 0, 0]
}

/// EasyStart Pro's choices: 1-240 by the minute, 270-720 every 30 min, or none (no limit).
pub fn run_minutes_ok(m: Option<i64>) -> bool {
    match m {
        None => true,
        Some(m) => (1..=240).contains(&m) || (m > 240 && m <= 720 && m % 30 == 0),
    }
}

fn cf(c: Option<f64>) -> String {
    match c {
        None => "n/a".into(),
        Some(c) => format!("{c:.1} °C ({:.0} °F)", c * 9.0 / 5.0 + 32.0),
    }
}

/// (what the frame is, its decoded value) for the Systems page's frame list (Python `_decode`).
pub fn describe(i: u32, d: &[u8]) -> (String, String) {
    let w = |a: usize| (d.len() > a + 1).then(|| u16::from(d[a]) | u16::from(d[a + 1]) << 8);
    let s = |a: &str, b: String| (a.to_string(), b);
    match i {
        0x054 if d.len() >= 6 => {
            let sp = w(2).unwrap();
            let mode = match d[1] {
                0x05 => "heat".to_string(),
                0x02 => "fan".to_string(),
                0x00 => "off".to_string(),
                x => format!("0x{x:x}"),
            };
            let set = if sp == 0xFFFE || sp == 0xFFFF { "n/a".to_string() } else { cf(Some(sp as f64 / 10.0)) };
            s("EasyStart Pro command", format!("{mode} · set {set} · room {} · flags {:02x}", cf(temp_word(d[4], d[5])), d[0]))
        }
        0x065 if d.len() >= 3 => s("EasyStart Pro room temperature", format!("{} · #{}", cf(temp_word(d[0], d[1])), d[2])),
        0x2C4 if d.len() >= 6 => {
            let f = d[2];
            let tag = if d[1] & 0x08 != 0 {
                " (after-run)"
            } else if d[1] & 0x04 != 0 {
                " (running)"
            } else {
                ""
            };
            let fault = if f != 0 {
                format!("fault 0x{f:x} {}", decoder::fault_text(f).unwrap_or(""))
            } else {
                "no fault".to_string()
            };
            s(
                "Heater status",
                format!("{} · flags {:02x}{tag} · {fault} · temp {}", decoder::state_name(d[0]), d[1], cf(temp_word(d[4], d[5]))),
            )
        }
        0x2C5 => s("Heater status 2", "reserved".into()),
        0x2C6 => s("Heater config ID", if d.len() >= 2 && d[..2] == [0xFE, 0xFF] { "n/a (starting)".into() } else { hex_spaced(d) }),
        0x625 if d.len() >= 3 => s("Heater heartbeat", format!("counter {:02x} · {}", d[2], if d[1] != 0 { "tick" } else { "tock" })),
        0x60D if d.len() >= 6 => s("EasyStart Pro heartbeat", format!("controller {:02X}{:02X}{:02X}", d[2], d[3], d[4])),
        0x055..=0x057 => s("EasyStart Pro (static)", "required with 054".into()),
        0x05C | 0x05F if !d.is_empty() => s("Preset run time", format!("{} min", d[0])),
        0x05D | 0x05E | 0x060 | 0x061 | 0x066 => s("EasyStart Pro settings", String::new()),
        0x062 => s("EasyStart Pro type", hex_spaced(d)),
        0x063 if d.len() >= 2 => s(
            "Room sensor settings",
            format!(
                "offset {:+.1} °C · sensor {}",
                (d[0] as f64 - 50.0) / 10.0,
                if d[1] == 1 { "control unit" } else { "none" }
            ),
        ),
        0x064 => s("Service interval", if d.len() >= 8 && d[6..8] == [0xFF, 0x03] { "off".into() } else { hex_spaced(d.get(6..8).unwrap_or(&[])) }),
        0x067..=0x06C => s(&format!("Timer {}", i - 0x066), hex_spaced(d)),
        0x06D => s("EasyStart Pro settings", String::new()),
        0x10A => s("Clock", hex_spaced(d)),
        0x7A0 => s("Diagnostic request", if d.first() == Some(&0x30) { "flow control".into() } else { hex_spaced(d) }),
        0x73C => s("Diagnostic reply", hex_spaced(d)),
        _ => (String::new(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_match_the_captured_easystart_pro() {
        assert_eq!(startup_burst().len(), 18);
        assert_eq!(cmd_frame(0x08, "heat", 21.5, Some(18.0)), [0x09, 0x05, 0xD7, 0x00, 0xA8, 0x02, 0, 0]);
        assert_eq!(cmd_frame(0x08, "off", 21.5, Some(18.0))[..4], [0x08, 0x00, 0xFE, 0xFF]);
        assert_eq!(cmd_frame(0x00, "fan", 21.0, Some(23.0))[..4], [0x01, 0x02, 0xFE, 0xFF]);
        assert_eq!(room_word(None), [0xFE, 0xFF]);
        assert_eq!(room_word(Some(-60.0)), 100u16.to_le_bytes(), "clamped to −40 °C");
        assert_eq!(temp_word(0xDA, 0x02), Some(23.0));
    }

    /// py: tests/test_espar_control.py::test_run_minutes_match_the_easystart_pro
    #[test]
    fn test_run_minutes_match_the_easystart_pro() {
        for m in [None, Some(1), Some(45), Some(240), Some(270), Some(300), Some(720)] {
            assert!(run_minutes_ok(m), "{m:?}");
        }
        for m in [0, 241, 250, 735, 721, -5] {
            assert!(!run_minutes_ok(Some(m)), "{m}");
        }
        // 1.5 is rejected before this point: a run time must be a JSON integer (see the controller's parser)
    }
}
