//! `aetherrv-sim-peplink [--bind 127.0.0.1:8443] [--http] [--frozen] [--state FILE]`
//!
//! A simulated Peplink router API (HTTPS with a self-signed certificate unless `--http`). Loopback only.
//! Credentials: API clients `sim-ro-client`/`sim-ro-secret` (api.read-only) and `sim-rw-client`/`sim-rw-secret`
//! (api); web users `viewer`/`ro-pass` (GET) and `admin`/`admin-pass` (GET+POST).
//! `--state FILE` writes the router's counters and WAN priority config there once a second.

use aetherrv_sim::peplink::{PeplinkSim, PeplinkSimConfig};
use std::time::Duration;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    let mut cfg = PeplinkSimConfig { bind: "127.0.0.1:8443".parse()?, ..Default::default() };
    let mut state_file = None;
    let mut a = std::env::args().skip(1);
    while let Some(k) = a.next() {
        match k.as_str() {
            "--bind" => cfg.bind = a.next().unwrap_or_default().parse()?,
            "--http" => cfg.tls = false,
            "--frozen" => cfg.frozen = true,
            "--state" => state_file = a.next(),
            "-h" | "--help" => {
                println!("see the source header for options");
                return Ok(());
            }
            _ => anyhow::bail!("unknown option {k}"),
        }
    }
    let tls = cfg.tls;
    let sim = PeplinkSim::start(cfg).await?;
    eprintln!("peplink sim listening on {}://{}", if tls { "https" } else { "http" }, sim.host());
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            _ = term.recv() => break,
            _ = tokio::signal::ctrl_c() => break,
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
        if let Some(p) = &state_file {
            let j = sim.with(|s| {
                let wans: Vec<_> = s.wans.iter().map(|w| serde_json::json!({"id": w.id, "enable": w.enable, "priority": w.priority})).collect();
                serde_json::json!({"posts": s.posts, "grants": s.grants, "logins": s.logins, "calls": s.calls, "wans": wans})
            });
            let _ = std::fs::write(p, j.to_string());
        }
    }
    sim.stop();
    Ok(())
}
