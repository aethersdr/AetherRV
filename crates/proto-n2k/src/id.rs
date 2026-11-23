//! The 29-bit NMEA 2000 / J1939 identifier: priority, PGN, source, destination.

/// A split identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct N2kId {
    pub priority: u8,
    pub pgn: u32,
    pub source: u8,
    /// 255 = global/broadcast.
    pub destination: u8,
}

/// Python `parse_id`: PDU1 (PF < 240) carries a destination in PS, which is not part of the PGN; PDU2 is broadcast.
pub fn parse_id(arbitration_id: u32) -> N2kId {
    let priority = ((arbitration_id >> 26) & 0x7) as u8;
    let pf = (arbitration_id >> 16) & 0xFF;
    let ps = ((arbitration_id >> 8) & 0xFF) as u8;
    let source = (arbitration_id & 0xFF) as u8;
    let mut pgn = (arbitration_id >> 8) & 0x3FFFF;
    let destination = if pf < 240 {
        pgn &= 0x3FF00;
        ps
    } else {
        255
    };
    N2kId { priority, pgn, source, destination }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::n2k_id;

    fn pdu2_and_pdu1(pgn: u32, src: u32) {
        let nid = parse_id(n2k_id(pgn, src, 3, 0x55));
        let pdu1 = ((pgn >> 8) & 0xFF) < 240;
        assert_eq!(
            (nid.pgn, u32::from(nid.source), nid.priority),
            (pgn & if pdu1 { 0x3FF00 } else { 0x3FFFF }, src, 3)
        );
        if pdu1 {
            assert_eq!(nid.destination, 0x55);
        } else {
            assert_eq!(nid.destination, 255);
        }
    }

    /// py: tests/test_core.py::test_n2k_parse_pdu2_and_pdu1[127501-35]
    #[test]
    fn test_n2k_parse_pdu2_and_pdu1_127501_35() {
        pdu2_and_pdu1(127501, 0x23);
    }

    /// py: tests/test_core.py::test_n2k_parse_pdu2_and_pdu1[130312-16]
    #[test]
    fn test_n2k_parse_pdu2_and_pdu1_130312_16() {
        pdu2_and_pdu1(130312, 0x10);
    }

    /// py: tests/test_core.py::test_n2k_parse_pdu2_and_pdu1[126208-1]
    #[test]
    fn test_n2k_parse_pdu2_and_pdu1_126208_1() {
        pdu2_and_pdu1(126208, 0x01);
    }

    /// py: tests/test_core.py::test_known_id_vector
    #[test]
    fn test_known_id_vector() {
        // 0x09F20123: priority 2, PGN 0x1F201 = 127489 (Engine Parameters, Dynamic), source 0x23
        let nid = parse_id(0x09F20123);
        assert_eq!((nid.priority, nid.pgn, nid.source, nid.destination), (2, 127489, 0x23, 255));
        // 0x18EAFF01: priority 6, PGN 59904 (ISO Request, PDU1) to global 0xFF from 0x01
        let nid = parse_id(0x18EAFF01);
        assert_eq!((nid.priority, nid.pgn, nid.source, nid.destination), (6, 59904, 0x01, 0xFF));
    }
}
