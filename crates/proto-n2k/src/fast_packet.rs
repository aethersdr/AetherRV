//! Fast-packet reassembly (NMEA 2000 messages of up to 223 bytes over ≤ 32 frames).
//!
//! Frame byte 0 = sequence counter (bits 5-7) | frame index (bits 0-4).
//! Frame 0: byte 1 = total length, bytes 2-7 = first 6 data bytes. Frames 1..n: 7 data bytes.
//! Keyed by (source, PGN). A gap or sequence change drops the partial message.

use std::collections::HashMap;

struct Partial {
    seq: u8,
    next: u32,
    total: usize,
    buf: Vec<u8>,
    ts: f64,
}

/// Python `FastPacketAssembler`.
#[derive(Default)]
pub struct FastPacketAssembler {
    partial: HashMap<(u8, u32), Partial>,
    /// Partial messages dropped (gap, sequence change, timeout, or restarted before completion).
    pub dropped: u64,
}

/// `data[a:b]` with Python's clamping.
fn slice(data: &[u8], a: usize, b: usize) -> &[u8] {
    let b = b.min(data.len());
    if a >= b { &[] } else { &data[a..b] }
}

impl FastPacketAssembler {
    /// Seconds; a message's frames arrive back-to-back.
    pub const TIMEOUT: f64 = 0.75;

    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one frame's data; returns the whole message when its last frame arrives. `ts` is the frame's time
    /// (seconds, any epoch, only differences matter).
    pub fn feed(&mut self, source: u8, pgn: u32, data: &[u8], ts: f64) -> Option<Vec<u8>> {
        let first = *data.first()?;
        let (seq, idx) = (first >> 5, u32::from(first & 0x1F));
        let key = (source, pgn);
        if idx == 0 {
            let total = usize::from(*data.get(1)?);
            if self.partial.contains_key(&key) {
                self.dropped += 1;
            }
            let buf = slice(data, 2, 8).to_vec();
            if buf.len() >= total {
                self.partial.remove(&key);
                return Some(buf[..total].to_vec());
            }
            self.partial.insert(key, Partial { seq, next: 1, total, buf, ts });
            return None;
        }
        let p = self.partial.get_mut(&key)?; // None: joined mid-message
        if p.seq != seq || p.next != idx || ts - p.ts > Self::TIMEOUT {
            self.dropped += 1;
            self.partial.remove(&key);
            return None;
        }
        p.buf.extend_from_slice(slice(data, 1, 8));
        p.next += 1;
        p.ts = ts;
        if p.buf.len() >= p.total {
            let total = p.total;
            let mut p = self.partial.remove(&key)?;
            p.buf.truncate(total);
            return Some(p.buf);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::frames;

    /// py: tests/test_n2k.py::test_fast_packet_reassembly_and_interleave
    #[test]
    fn test_fast_packet_reassembly_and_interleave() {
        let mut fp = FastPacketAssembler::new();
        let a: Vec<u8> = (0..20).collect(); // 3 frames
        let b: Vec<u8> = (100..113).collect(); // 2 frames, different source
        let (fa, fb) = (frames(&a, 2), frames(&b, 5));
        assert_eq!(fp.feed(1, 126208, &fa[0], 0.0), None);
        assert_eq!(fp.feed(2, 126208, &fb[0], 0.0), None);
        assert_eq!(fp.feed(1, 126208, &fa[1], 0.01), None);
        assert_eq!(fp.feed(2, 126208, &fb[1], 0.01), Some(b));
        assert_eq!(fp.feed(1, 126208, &fa[2], 0.02), Some(a));
    }

    /// py: tests/test_n2k.py::test_fast_packet_drops_gaps_and_stale
    #[test]
    fn test_fast_packet_drops_gaps_and_stale() {
        let mut fp = FastPacketAssembler::new();
        let fa = frames(&(0..20).collect::<Vec<u8>>(), 2);
        fp.feed(1, 126208, &fa[0], 0.0);
        assert!(fp.feed(1, 126208, &fa[2], 0.01).is_none() && fp.dropped == 1); // skipped frame 1
        fp.feed(1, 126208, &fa[0], 1.0);
        assert!(fp.feed(1, 126208, &fa[1], 5.0).is_none() && fp.dropped == 2); // too slow
        assert!(fp.feed(9, 126208, &fa[1], 0.0).is_none()); // joined mid-message
    }

    /// Frame 0 with length 0 completes at once with an empty message (Python returns b"", not None).
    #[test]
    fn zero_length_message_is_empty_not_none() {
        let mut fp = FastPacketAssembler::new();
        assert_eq!(fp.feed(1, 126208, &[0x20, 0], 0.0), Some(vec![]));
        assert_eq!(fp.feed(1, 126208, &[0x20], 0.0), None);
        assert_eq!(fp.feed(1, 126208, &[], 0.0), None);
    }
}
