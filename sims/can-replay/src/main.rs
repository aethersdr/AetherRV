//! `aetherrv-sim-replay <capture.log> <vcanN> [--loop] [--speed X]`: replays a candump capture's RECEIVED frames onto a
//! virtual CAN interface at their recorded pace (e.g. the van's N2K traffic for the isolation runs and the soak).
//! Refuses anything but a vcan.

use aetherrv_can::capture::read_file;
use aetherrv_can::raw::RawSocket;
use std::time::{Duration, Instant};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let (path, iface) = (a.get(1).cloned().unwrap_or_default(), a.get(2).cloned().unwrap_or_default());
    if !iface.starts_with("vcan") {
        anyhow::bail!("refusing {iface:?}: replay onto vcan only");
    }
    let looping = a.iter().any(|x| x == "--loop");
    let speed: f64 = a.iter().position(|x| x == "--speed").and_then(|i| a.get(i + 1)).and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let frames: Vec<_> = read_file(std::path::Path::new(&path))?.into_iter().filter(|f| f.is_rx && !f.error).collect();
    if frames.is_empty() {
        anyhow::bail!("no frames in {path}");
    }
    let sock = RawSocket::open(&iface, false)?;
    loop {
        let (t0, f0) = (Instant::now(), frames[0].ts);
        for f in &frames {
            let due = t0 + Duration::from_secs_f64(((f.ts - f0) / speed).max(0.0));
            let now = Instant::now();
            if due > now {
                tokio::time::sleep(due - now).await;
            }
            let _ = sock.send(f);
        }
        if !looping {
            return Ok(());
        }
    }
}
