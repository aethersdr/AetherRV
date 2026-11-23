//! Test helpers ported from the Python tests: `tools/bench_traffic.py` (`n2k_id`, `encode_127501`, exposed to the
//! tests as `tools_bench`) and the helpers in `tests/test_n2k.py` (`frames`, `name_bytes`, `product_payload`).

use crate::Frame;

/// A fixed "now" for the tests (Python's tests use `time.time()`).
pub const T0: f64 = 1_700_000_000.0;

/// `tools_bench.n2k_id(pgn, source, priority, dest)` (unmasked, as written there).
pub fn n2k_id(pgn: u32, source: u32, priority: u32, dest: u32) -> u32 {
    let pf = (pgn >> 8) & 0xFF;
    let pgn = if pf < 240 { (pgn & 0x3FF00) | dest } else { pgn };
    (priority << 26) | (pgn << 8) | source
}

/// `tools_bench.encode_127501(instance, states)`: up to 28 values of 0 off / 1 on / 2 error / 3 n/a (missing → 3).
pub fn encode_127501(instance: u8, states: &[u8]) -> Vec<u8> {
    let mut bits: u64 = 0;
    for i in 0..28 {
        let v = u64::from(states.get(i).copied().unwrap_or(3) & 3);
        bits |= v << (2 * i);
    }
    let mut out = vec![instance];
    out.extend_from_slice(&bits.to_le_bytes()[..7]);
    out
}

/// test_n2k.py `frames(payload, seq=2)`: fast-packet frames padded with 0xFF.
pub fn frames(payload: &[u8], seq: u8) -> Vec<Vec<u8>> {
    let mut first = vec![seq << 5, payload.len() as u8];
    first.extend_from_slice(&payload[..payload.len().min(6)]);
    let mut out = vec![first];
    let mut rest = &payload[payload.len().min(6)..];
    let mut i = 1u8;
    while !rest.is_empty() {
        let n = rest.len().min(7);
        let mut f = vec![(seq << 5) | i];
        f.extend_from_slice(&rest[..n]);
        out.push(f);
        rest = &rest[n..];
        i += 1;
    }
    for f in &mut out {
        f.resize(f.len().max(8), 0xFF);
    }
    out
}

/// test_n2k.py `name_bytes(unique, mfr, inst_lo, inst_hi, function, dclass, sys_inst=0, industry=4, aac=1)`.
#[allow(clippy::too_many_arguments)]
pub fn name_bytes(
    unique: u64,
    mfr: u64,
    inst_lo: u64,
    inst_hi: u64,
    function: u64,
    dclass: u64,
    sys_inst: u64,
    industry: u64,
    aac: u64,
) -> [u8; 8] {
    let n = unique
        | mfr << 21
        | inst_lo << 32
        | inst_hi << 35
        | function << 40
        | dclass << 49
        | sys_inst << 56
        | industry << 60
        | aac << 63;
    n.to_le_bytes()
}

/// test_n2k.py `product_payload(model, sw, serial, len_=3)`.
pub fn product_payload(model: &str, sw: &str, serial: &str, len: u8) -> Vec<u8> {
    let f = |t: &str| {
        let mut b = t.as_bytes().to_vec();
        b.resize(b.len().max(32), 0xFF);
        b
    };
    let mut out = Vec::new();
    out.extend_from_slice(&2100u16.to_le_bytes());
    out.extend_from_slice(&1234u16.to_le_bytes());
    out.extend(f(model));
    out.extend(f(sw));
    out.extend(f("1"));
    out.extend(f(serial));
    out.extend([1, len]);
    out
}

/// `can.Message(arbitration_id=id, data=data, is_extended_id=True)`.
pub fn msg(id: u32, data: impl Into<Vec<u8>>) -> Frame {
    Frame::extended(id, data)
}

/// `bytes.fromhex`.
pub fn hex(s: &str) -> Vec<u8> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}
