//! AetherRV's own node on the NMEA 2000 bus: claims an address (ISO 11783-5) and defends it. Sending waits until
//! claimed. A port of `switching.N2kNode`.

use aetherrv_can::{CanFrame, SendError};
use proto_n2k::encode::{PGN_ADDRESS_CLAIM, can_id, default_name, fast_packet_frames};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Where N2K frames go: the logical bus (FailoverBus or CanBus) with TxGate device "n2k".
pub trait N2kSink: Send + Sync {
    fn send(&self, f: &CanFrame, quiet: bool) -> Result<(), SendError>;
}

/// s without a contest before the address counts as ours (ISO 11783-5).
pub const CLAIM_SETTLE: Duration = Duration::from_millis(250);

#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error("AetherRV has not claimed an NMEA 2000 address yet")]
    NotClaimed,
    #[error(transparent)]
    Send(#[from] SendError),
    #[error("{0}")]
    Encode(String),
}

impl NodeError {
    pub fn is_blocked(&self) -> bool {
        matches!(self, NodeError::Send(SendError::Blocked(_)) | NodeError::NotClaimed)
    }
}

struct Inner {
    address: u8,
    claimed_at: Option<Instant>,
    taken: BTreeSet<u8>,
    lost: bool,
}

pub struct N2kNode {
    sink: Arc<dyn N2kSink>,
    pub name: u64,
    inner: Mutex<Inner>,
}

impl N2kNode {
    pub fn new(sink: Arc<dyn N2kSink>, preferred: u8, unique: u32) -> Self {
        N2kNode {
            sink,
            name: default_name(unique),
            inner: Mutex::new(Inner { address: preferred, claimed_at: None, taken: BTreeSet::new(), lost: false }),
        }
    }

    pub fn address(&self) -> u8 {
        self.inner.lock().unwrap().address
    }

    pub fn claimed(&self) -> bool {
        self.inner.lock().unwrap().claimed_at.is_some()
    }

    pub fn ready(&self) -> bool {
        let g = self.inner.lock().unwrap();
        g.claimed_at.is_some_and(|t| t.elapsed() >= CLAIM_SETTLE) && !g.lost
    }

    /// Tests: pretend the claim settled long ago.
    pub fn settle_now(&self) {
        let mut g = self.inner.lock().unwrap();
        if g.claimed_at.is_some() {
            g.claimed_at = Some(Instant::now() - Duration::from_secs(1));
        }
    }

    /// reason "poll": answering a peer's ISO request for address claims. The Cerbo polls every 10 s and the IPG100
    /// broadcasts the same request, so those replies are logged at DEBUG. First claims, moves and contests stay at
    /// INFO/WARNING.
    fn send_claim(&self, reason: &str) -> Result<(), SendError> {
        let (addr, first) = {
            let g = self.inner.lock().unwrap();
            (g.address, g.claimed_at.is_none())
        };
        let poll = reason == "poll";
        let f = CanFrame::ext(can_id(6, PGN_ADDRESS_CLAIM, addr, 255), &self.name.to_le_bytes());
        self.sink.send(&f, poll)?;
        if !poll || first {
            tracing::info!("n2k: claiming address {addr} ({reason})");
        } else {
            tracing::debug!("n2k: address claim {addr} re-sent (answering a request)");
        }
        self.inner.lock().unwrap().claimed_at = Some(Instant::now());
        Ok(())
    }

    /// Re-send our claim (e.g. after the N2K path changed); quiet if transmit is off or the path is down.
    pub fn reassert(&self) {
        if !self.claimed() {
            return;
        }
        if let Err(e) = self.send_claim("path change") {
            tracing::info!("n2k: claim on the new path not sent ({e})");
        }
    }

    pub fn claim(&self) -> Result<(), SendError> {
        {
            let mut g = self.inner.lock().unwrap();
            let start = g.address;
            let free: Vec<u8> = (start..252).chain(0..start).filter(|a| !g.taken.contains(a)).collect();
            match free.first() {
                None => {
                    g.lost = true;
                    tracing::error!("n2k: no free address to claim");
                    return Ok(());
                }
                Some(&a) => {
                    g.address = a;
                    g.lost = false;
                }
            }
        }
        self.send_claim("claim")
    }

    pub fn on_frame(&self, f: &CanFrame) {
        if f.error || !f.extended {
            return;
        }
        let a = f.id;
        let (src, pf, ps) = ((a & 0xFF) as u8, ((a >> 16) & 0xFF) as u8, ((a >> 8) & 0xFF) as u8);
        let (addr, claimed) = {
            let g = self.inner.lock().unwrap();
            (g.address, g.claimed_at.is_some())
        };
        if pf == 0xEE && f.data.len() >= 8 {
            // someone's address claim
            if src == addr && claimed {
                let theirs = u64::from_le_bytes(f.data[..8].try_into().unwrap());
                if theirs == self.name {
                    return; // our own frame echoed back
                }
                if theirs < self.name {
                    // lower NAME wins: move
                    tracing::warn!("n2k: address {src} contested by a higher-priority NAME; moving");
                    {
                        let mut g = self.inner.lock().unwrap();
                        g.taken.insert(src);
                        g.claimed_at = None;
                    }
                    let _ = self.claim();
                } else {
                    tracing::warn!("n2k: address {src} contested by a lower-priority NAME; re-asserting");
                    let _ = self.send_claim("contest");
                }
            } else {
                self.inner.lock().unwrap().taken.insert(src);
            }
        } else if pf == 0xEA && (ps == 255 || ps == addr) && f.data.len() >= 3 {
            // ISO request
            let pgn = u32::from(f.data[0]) | u32::from(f.data[1]) << 8 | u32::from(f.data[2]) << 16;
            if pgn == PGN_ADDRESS_CLAIM && claimed {
                let _ = self.send_claim("poll");
            }
        } else if src < 252 {
            self.inner.lock().unwrap().taken.insert(src);
        }
    }

    pub fn send_fast(&self, pgn: u32, dest: u8, payload: &[u8], seq: u8, priority: u8) -> Result<(), NodeError> {
        if !self.ready() {
            return Err(NodeError::NotClaimed);
        }
        let addr = self.address();
        for frame in fast_packet_frames(payload, seq).map_err(|e| NodeError::Encode(e.to_string()))? {
            self.sink.send(&CanFrame::ext(can_id(priority, pgn, addr, dest), &frame), false)?;
        }
        Ok(())
    }
}
