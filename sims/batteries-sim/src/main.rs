//! `aetherrv-sim-batteries [-c /etc/aetherrv/aetherrv.toml] [--soc 86,85,87,84] [--current -6.2,-6.2,-6.2,-6.2]`
//!
//! Simulated EPOCH batteries for the bench, which has no Bluetooth batteries. It runs the real `aetherrv-batteries-ble`
//! core (`Batteries`: its connection rules, `proto_roypow`'s frame assembly, decode and `batteries.ble*` keys) on
//! the bus as the batteries service, with an in-process stand-in for BlueZ: every battery of the config's enabled
//! `kind = "epoch_ble"` device advertises, connects at once and answers the three read queries with RoyPow frames.
//! The frames are recorded EPOCH frames (`crates/proto-roypow` tests) with this battery's SoC and current written
//! in and the checksum redone. Nothing leaves the process: there is no radio and no BlueZ.
//!
//! It takes the batteries service's place on the bus (same client id), so stop `aetherrv-batteries-ble` first, and
//! run it as `aetherrv` (the bus socket). Used by the UI benchmark: the Energy page shows its 3D battery bank only
//! with per-battery data. `--soc` and `--current` give one value per battery (the last one repeats); the defaults
//! add up to the Cerbo sim's bank current (-24.8 A). Runs until SIGTERM / Ctrl-C.

use aetherrv_batteries_ble::{Batteries, Ble, BleError, Link, Settings};
use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::config::Cfg;
use aetherrv_core::state::StateStore;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

/// Recorded replies of one EPOCH module, one per read query (`crates/proto-roypow/tests`): 0x04 (status: SoC at
/// byte 7), 0x03 (current at bytes 6..9: sign in bit 0 of byte 6, then 0.01 A big-endian), 0x02 (cell voltages).
const REC: [(u8, &str); 3] = [
    (
        0x04,
        "ead10139ff0401640200270300070404e005000706223e07000608dffd0911980affff0b000500d40c2611040819470532\
         0cff0cfc0d14ce0080000ff5",
    ),
    (
        0x03,
        "ead1011aff0331024500000000043332373c0003000000128700000008f5",
    ),
    (0x02, "ead1010fff020404040cfd0cfc0cfc0cfff4f5"),
];

fn hex(s: &str) -> Vec<u8> {
    let s: Vec<u8> = s.bytes().filter(|c| c.is_ascii_hexdigit()).collect();
    s.chunks(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap_or("00"), 16).unwrap_or(0))
        .collect()
}

/// The checksum (XOR of everything between the head and the checksum byte) and the tail, redone after an edit.
fn seal(f: &mut [u8]) {
    let n = f.len();
    f[n - 2] = f[3..n - 2].iter().fold(0u8, |a, b| a ^ b);
    f[n - 1] = proto_roypow::TAIL;
}

/// The three replies for a battery at `soc` % carrying `amps` (negative = discharging).
fn replies(soc: u8, amps: f64) -> HashMap<u8, Vec<u8>> {
    REC.iter()
        .map(|&(cmd, h)| {
            let mut f = hex(h);
            match cmd {
                0x04 => f[7] = soc.min(100),
                0x03 => {
                    let m = ((amps.abs() * 100.0).round() as u64).min(0xFFFF) as u16;
                    f[6] = (f[6] & !1) | u8::from(amps < 0.0);
                    f[7..9].copy_from_slice(&m.to_be_bytes());
                }
                _ => {}
            }
            seal(&mut f);
            (cmd, f)
        })
        .collect()
}

/// BlueZ stand-in: every configured battery is in range and answers.
struct SimBle {
    replies: HashMap<String, HashMap<u8, Vec<u8>>>,
}

impl Ble for SimBle {
    type Link = SimLink;

    async fn find(&self, addr: &str, _timeout: Duration) -> Result<bool, BleError> {
        tokio::time::sleep(Duration::from_millis(200)).await;
        Ok(self.replies.contains_key(addr))
    }

    async fn connect(&self, addr: &str, _timeout: Duration) -> Result<SimLink, BleError> {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let replies = self
            .replies
            .get(addr)
            .cloned()
            .ok_or_else(|| BleError::connection("not in range"))?;
        let (lost_tx, lost) = watch::channel(false);
        Ok(SimLink {
            replies,
            tx: None,
            _lost_tx: lost_tx,
            lost,
        })
    }

    async fn disconnect(&self, _addr: &str) {}
}

struct SimLink {
    replies: HashMap<u8, Vec<u8>>,
    tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
    _lost_tx: watch::Sender<bool>,
    lost: watch::Receiver<bool>,
}

impl Link for SimLink {
    async fn start_notify(&mut self) -> Result<mpsc::UnboundedReceiver<Vec<u8>>, BleError> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.tx = Some(tx);
        Ok(rx)
    }

    /// A read query: its reply arrives as 20-byte notifications, as from the module's BLE UART.
    async fn write(&mut self, data: &[u8]) -> Result<(), BleError> {
        let (Some(tx), Some(f)) = (&self.tx, data.get(5).and_then(|c| self.replies.get(c))) else {
            return Ok(());
        };
        for chunk in f.chunks(20) {
            let _ = tx.send(chunk.to_vec());
        }
        Ok(())
    }

    fn lost(&self) -> watch::Receiver<bool> {
        self.lost.clone()
    }

    async fn disconnect(&mut self) {
        self.tx = None;
    }
}

/// `"86,85"` → `[86.0, 85.0]`.
fn list(s: &str) -> anyhow::Result<Vec<f64>> {
    s.split(',')
        .map(|x| {
            x.trim()
                .parse::<f64>()
                .map_err(|e| anyhow::anyhow!("{x:?}: {e}"))
        })
        .collect()
}

fn main() -> anyhow::Result<()> {
    let mut a = std::env::args().skip(1);
    let (mut config, mut socs, mut amps) = (
        "/etc/aetherrv/aetherrv.toml".to_string(),
        vec![86.0, 85.0, 87.0, 84.0],
        vec![-6.2],
    );
    while let Some(k) = a.next() {
        match k.as_str() {
            "-c" | "--config" => config = a.next().unwrap_or_default(),
            "--soc" => socs = list(&a.next().unwrap_or_default())?,
            "--current" => amps = list(&a.next().unwrap_or_default())?,
            _ => anyhow::bail!(
                "unknown argument {k:?} (see the header of sims/batteries-sim/src/main.rs)"
            ),
        }
    }
    let cfg = Cfg::load(&config)?;
    let Some(s) = Settings::from_config(&cfg)?.filter(|s| !s.batteries.is_empty()) else {
        anyhow::bail!("{config}: no enabled [device.*] with kind = \"epoch_ble\" and batteries");
    };
    let pick = |v: &[f64], i: usize| v.get(i).or(v.last()).copied().unwrap_or(0.0);
    let replies = s
        .batteries
        .iter()
        .enumerate()
        .map(|(i, b)| {
            (
                b.address.clone(),
                replies(pick(&socs, i).clamp(0.0, 100.0) as u8, pick(&amps, i)),
            )
        })
        .collect();
    let n = s.batteries.len();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async move {
            let state = Arc::new(StateStore::new());
            let bus = Bus::start(
                BusOptions::new(aetherrv_core::names::BATTERIES),
                state.clone(),
            );
            let task = tokio::spawn(Batteries::new(SimBle { replies }, s, state).run());
            println!(
                "batteries-sim: {n} batteries on the bus as {}",
                aetherrv_core::names::BATTERIES
            );
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            tokio::select! {
                _ = term.recv() => {},
                _ = tokio::signal::ctrl_c() => {},
            }
            task.abort();
            bus.shutdown().await;
            Ok(())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto_roypow::{FrameAssembler, decode};

    /// The edited frames assemble (checksum and tail right) and decode to the SoC and current written in.
    #[test]
    fn frames_carry_soc_and_current() {
        for (soc, amps) in [(86u8, -6.2), (100, 12.34), (0, 0.0)] {
            let mut fa = FrameAssembler::new();
            for f in replies(soc, amps).values() {
                for c in f.chunks(20) {
                    fa.feed(c);
                }
            }
            assert_eq!(fa.msgs.len(), 3, "all three frames assembled");
            let r = decode(&fa.msgs);
            assert_eq!(r.soc, Some(u64::from(soc)));
            assert_eq!(r.current, Some(amps));
            assert_eq!(r.cells.len(), 4);
        }
    }

    /// Unedited, the recorded frames decode as in the codec's own tests (SoC 100 %, -5.81 A).
    #[test]
    fn recording_is_intact() {
        let mut fa = FrameAssembler::new();
        for (_, h) in REC {
            fa.feed(&hex(h));
        }
        let r = decode(&fa.msgs);
        assert_eq!((r.soc, r.current), (Some(100), Some(-5.81)));
    }
}
