//! NMEA 2000 PGN metadata from CANboat (canboat.json): which PGNs are fast-packet, their names, and the manufacturer
//! and device-class names. The tables are generated into `pgns/tables.rs` by tools/gen_n2k_pgns.sh (CANboat
//! (C) 2009-2026 Kees Verruijt and contributors, https://github.com/canboat/canboat, Apache License 2.0), sorted by
//! key for binary search.

mod tables;
pub use tables::{DEVICE_CLASSES, FAST_PACKET_PGNS, MANUFACTURERS, PGN_NAMES};

fn lookup<K: Ord + Copy>(t: &'static [(K, &'static str)], k: K) -> Option<&'static str> {
    t.binary_search_by(|(x, _)| x.cmp(&k)).ok().map(|i| t[i].1)
}

/// Python `pgn in FAST_PACKET_PGNS`.
pub fn is_fast_packet(pgn: u32) -> bool {
    FAST_PACKET_PGNS.binary_search(&pgn).is_ok()
}

/// Python `PGN_NAMES.get(pgn)`.
pub fn pgn_name(pgn: u32) -> Option<&'static str> {
    lookup(PGN_NAMES, pgn)
}

/// Python `MANUFACTURERS.get(code)`.
pub fn manufacturer(code: i64) -> Option<&'static str> {
    u16::try_from(code).ok().and_then(|c| lookup(MANUFACTURERS, c))
}

/// Python `DEVICE_CLASSES.get(class)`.
pub fn device_class(class: i64) -> Option<&'static str> {
    u8::try_from(class).ok().and_then(|c| lookup(DEVICE_CLASSES, c))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tables are sorted (binary search) and as long as the Python ones.
    #[test]
    fn tables_sorted_and_complete() {
        assert!(FAST_PACKET_PGNS.windows(2).all(|w| w[0] < w[1]));
        assert!(PGN_NAMES.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(MANUFACTURERS.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(DEVICE_CLASSES.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(FAST_PACKET_PGNS.len(), 181);
        assert_eq!(PGN_NAMES.len(), 346);
        assert_eq!(MANUFACTURERS.len(), 283);
        assert_eq!(DEVICE_CLASSES.len(), 18);
        assert_eq!(manufacturer(137), Some("Maretron"));
        assert_eq!(device_class(30), Some("Electrical Distribution"));
        assert!(is_fast_packet(126208) && !is_fast_packet(127501));
    }
}
