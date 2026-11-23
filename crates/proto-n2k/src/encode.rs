//! What AetherRV sends on NMEA 2000 (the pure encoding half of `switching.py`).
//!
//! Wire format (verified against captures of N2KAnalyzer → CLMD16, firmware 1.0.3.66):
//! * Switch:  126208 Command for PGN 127501: priority byte 0xF8, pairs (1, bank instance), (N+1, 0|1).
//! * Dimmer:  126208 Command for PGN 127500: pairs (2, connection = channel-1), (6, duty 0..100).
//!
//! Every function refuses (an [`EncodeError`]) where Python raises; nothing is wrapped or truncated.

use std::fmt;

pub const PGN_ISO_REQUEST: u32 = 59904;
pub const PGN_ADDRESS_CLAIM: u32 = 60928;
pub const PGN_GROUP_FUNCTION: u32 = 126208;
pub const PGN_LOAD_CONTROLLER: u32 = 127500;
pub const PGN_SWITCH_STATUS: u32 = 127501;

/// A command that can't be encoded (Python `ValueError` / `OverflowError`). Nothing may be sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeError(pub String);

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EncodeError {}

/// 29-bit identifier. PDU1 PGNs (PF < 240) carry the destination in the PS byte.
pub fn can_id(priority: u8, pgn: u32, source: u8, dest: u8) -> u32 {
    let pf = (pgn >> 8) & 0xFF;
    let pgn = if pf < 240 { (pgn & 0x3FF00) | u32::from(dest) } else { pgn };
    ((u32::from(priority) & 7) << 26) | ((pgn & 0x3FFFF) << 8) | u32::from(source)
}

/// Split a fast-packet message into 8-byte frames (frame 0: seq|0, length, 6 bytes; then seq|n + 7 bytes),
/// padded with 0xFF. `seq` is masked to 3 bits, as in Python.
pub fn fast_packet_frames(payload: &[u8], seq: u8) -> Result<Vec<[u8; 8]>, EncodeError> {
    if payload.len() > 223 {
        return Err(EncodeError("fast-packet payload too long".into()));
    }
    let seq = (seq & 7) << 5;
    let frame = |head: &[u8], body: &[u8]| {
        let mut f = [0xFF; 8];
        f[..head.len()].copy_from_slice(head);
        f[head.len()..head.len() + body.len()].copy_from_slice(body);
        f
    };
    let n0 = payload.len().min(6);
    let mut out = vec![frame(&[seq, payload.len() as u8], &payload[..n0])];
    for (n, chunk) in payload[n0..].chunks(7).enumerate() {
        out.push(frame(&[seq | (n as u8 + 1)], chunk));
    }
    Ok(out)
}

/// 126208 Command (function 1): commanded PGN, priority setting 8 (= leave unchanged), N field pairs.
pub fn group_command(pgn: u32, pairs: &[(u8, &[u8])]) -> Result<Vec<u8>, EncodeError> {
    if pgn > 0xFF_FFFF {
        return Err(EncodeError("int too big to convert".into()));
    }
    let n = u8::try_from(pairs.len()).map_err(|_| EncodeError("bytes must be in range(0, 256)".into()))?;
    let mut body = vec![0x01];
    body.extend_from_slice(&pgn.to_le_bytes()[..3]);
    body.extend([0xF8, n]);
    for (fieldno, value) in pairs {
        body.push(*fieldno);
        body.extend_from_slice(value);
    }
    Ok(body)
}

/// Switch command: 126208 Command for 127501 setting `channel` (1..28) of `bank`.
/// Takes plain integers so an out-of-range value is refused here, as in Python, instead of being wrapped by a
/// caller's `as u8` into a valid-looking command.
pub fn switch_command(bank: i64, channel: i64, on: bool) -> Result<Vec<u8>, EncodeError> {
    if !(1..=28).contains(&channel) {
        return Err(EncodeError("channel must be 1..28".into()));
    }
    let bank = byte(bank)?;
    group_command(PGN_SWITCH_STATUS, &[(1, &[bank]), (channel as u8 + 1, &[u8::from(on)])])
}

/// Dimmer command: 126208 Command for 127500 setting connection `channel - 1` to duty `level` (0..100).
/// `channel` outside 1..256 is refused (Python `bytes([channel - 1])` raises). Plain integers, as [`switch_command`].
pub fn dim_command(channel: i64, level: i64) -> Result<Vec<u8>, EncodeError> {
    if !(0..=100).contains(&level) {
        return Err(EncodeError("level must be 0..100".into()));
    }
    let conn = byte(channel - 1)?;
    group_command(PGN_LOAD_CONTROLLER, &[(2, &[conn]), (6, &[level as u8])])
}

/// Python `bytes([v])`.
fn byte(v: i64) -> Result<u8, EncodeError> {
    u8::try_from(v).map_err(|_| EncodeError("bytes must be in range(0, 256)".into()))
}

/// `make_name` defaults: unregistered manufacturer code.
pub const NAME_MANUFACTURER: u16 = 2046;
/// `make_name` defaults: function 130 (PC gateway).
pub const NAME_FUNCTION: u8 = 130;
/// `make_name` defaults: class 25 (inter/intranetwork device).
pub const NAME_CLASS: u8 = 25;
/// `make_name` defaults: marine industry group.
pub const NAME_INDUSTRY: u8 = 4;
/// `N2kNode`'s default `unique`.
pub const NAME_UNIQUE: u32 = 0x0AE7;

/// ISO 11783 NAME (arbitrary-address capable). Python's defaults are the `NAME_*` constants and instance 0; see
/// [`default_name`].
pub fn make_name(unique: u32, manufacturer: u16, function: u8, dev_class: u8, industry: u8, instance: u8) -> u64 {
    (u64::from(unique) & 0x1FFFFF)
        | ((u64::from(manufacturer) & 0x7FF) << 21)
        | (u64::from(instance) << 32)
        | (u64::from(function) << 40)
        | ((u64::from(dev_class) & 0x7F) << 49)
        | ((u64::from(industry) & 7) << 60)
        | (1 << 63)
}

/// Python `make_name(unique)` with every default.
pub fn default_name(unique: u32) -> u64 {
    make_name(unique, NAME_MANUFACTURER, NAME_FUNCTION, NAME_CLASS, NAME_INDUSTRY, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::hex;

    fn to_hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    // ── wire format, byte for byte against N2KAnalyzer → CLMD16 captures (fw 1.0.3.66, 2026-11-21) ──

    /// py: tests/test_switching.py::test_switch_on_matches_capture
    #[test]
    fn test_switch_on_matches_capture() {
        // 01ED0028#600A010DF201F802 / 01ED0028#6101200201FFFFFF : bank 32, channel 1 ON, seq 3
        let frames = fast_packet_frames(&switch_command(32, 1, true).unwrap(), 3).unwrap();
        let frames: Vec<Vec<u8>> = frames.iter().map(|f| f.to_vec()).collect();
        assert_eq!(frames, vec![hex("600A010DF201F802"), hex("6101200201FFFFFF")]);
    }

    /// py: tests/test_switching.py::test_switch_off_and_other_channels_match_capture
    #[test]
    fn test_switch_off_and_other_channels_match_capture() {
        assert_eq!(
            fast_packet_frames(&switch_command(32, 1, false).unwrap(), 1).unwrap()[1].to_vec(),
            hex("2101200200FFFFFF")
        );
        assert_eq!(to_hex(&switch_command(32, 2, true).unwrap()), "010df201f80201200301"); // field 3 = channel 2
        assert_eq!(to_hex(&switch_command(32, 8, true).unwrap()), "010df201f80201200901"); // field 9 = channel 8
    }

    /// py: tests/test_switching.py::test_dim_matches_capture
    #[test]
    fn test_dim_matches_capture() {
        // "f8 02 02 0b 06 64": connection 11 (= channel 12), duty 100 %
        assert_eq!(to_hex(&dim_command(12, 100).unwrap()), "010cf201f802020b0664");
        assert_eq!(*dim_command(12, 33).unwrap().last().unwrap(), 33);
        assert!(dim_command(12, 101).is_err());
    }

    /// py: tests/test_switching.py::test_identifiers
    #[test]
    fn test_identifiers() {
        assert_eq!(can_id(3, 126208, 150, 0), 0x0DED0096); // PDU1 + data page 1; = CLMD16's own ACK 0x0DED2800
        assert_eq!(can_id(6, 60928, 150, 255), 0x18EEFF96);
        assert_eq!(can_id(0, 126208, 0x28, 0), 0x01ED0028); // the IPG100's own frame
    }

    /// Where Python raises, Rust refuses; nothing is wrapped into a different (valid-looking) command.
    #[test]
    fn out_of_range_is_refused() {
        assert!(switch_command(32, 0, true).is_err());
        assert!(switch_command(32, 29, true).is_err());
        assert!(switch_command(32, 257, true).is_err()); // not wrapped to channel 1
        assert!(switch_command(256, 1, true).is_err() && switch_command(-1, 1, true).is_err());
        assert!(dim_command(0, 50).is_err());
        assert!(dim_command(257, 50).is_err());
        assert!(dim_command(12, 300).is_err() && dim_command(12, -1).is_err()); // not wrapped to 44 / 255
        assert_eq!(dim_command(256, 0).unwrap()[7], 255);
        assert!(group_command(0x100_0000, &[]).is_err());
        assert!(fast_packet_frames(&[0; 224], 0).is_err());
        assert_eq!(fast_packet_frames(&[0; 223], 0).unwrap().len(), 32);
        assert_eq!(fast_packet_frames(&[], 9).unwrap(), vec![[0x20, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]]);
    }

    /// `make_name` against an independent packing (values as `N2kNode` uses them).
    #[test]
    fn name_packing() {
        let n = default_name(NAME_UNIQUE);
        assert_eq!(n, 0x0AE7 | (2046 << 21) | (130 << 40) | (25 << 49) | (4 << 60) | (1 << 63));
        assert_eq!(
            make_name(0xFFFF_FFFF, 0xFFFF, 0, 0xFF, 0xFF, 0) & 0x7FFF_FFFF_FFFF_FFFF,
            0x1FFFFF | (0x7FF << 21) | (0x7F << 49) | (7 << 60)
        );
    }
}
