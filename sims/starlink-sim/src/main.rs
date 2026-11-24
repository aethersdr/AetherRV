//! `aetherrv-sim-starlink [--bind 127.0.0.1:9200] [--frozen] [--v1alpha-only] [--legacy-firmware]
//!                        [--update-available] [--down-s S] [--fetch-s S] [--outage CAUSE] [--alert NAME]...
//!                        [--state FILE]`
//!
//! A simulated Starlink dish: gRPC with server reflection and `SpaceX.API.Device.Device/Handle`. Loopback only.
//! `--outage NO_SCHEDULE|OBSTRUCTED|…` puts the dish in an outage (DishOutage.Cause name); `--alert motors_stuck`
//! (a DishAlerts field name, repeatable) raises an alert besides the default `roaming`.
//! `--state FILE` writes the dish's counters (reboots, update checks, config sets, uptime) there once a second.

use aetherrv_sim::starlink::{StarlinkSim, StarlinkSimConfig};
use std::time::Duration;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    let mut cfg = StarlinkSimConfig { bind: "127.0.0.1:9200".parse()?, ..Default::default() };
    let mut state_file = None;
    let (mut outage, mut alerts) = (None, Vec::new());
    let mut a = std::env::args().skip(1);
    while let Some(k) = a.next() {
        match k.as_str() {
            "--bind" => cfg.bind = a.next().unwrap_or_default().parse()?,
            "--frozen" => cfg.frozen = true,
            "--v1alpha-only" => cfg.v1alpha_only = true,
            "--legacy-firmware" => cfg.legacy_firmware = true,
            "--update-available" => cfg.update_available = true,
            "--down-s" => cfg.down_for = Duration::from_secs_f64(a.next().unwrap_or_default().parse()?),
            "--fetch-s" => cfg.fetch_for = Duration::from_secs_f64(a.next().unwrap_or_default().parse()?),
            "--state" => state_file = a.next(),
            "--outage" => {
                let name = a.next().unwrap_or_default();
                let pool = aetherrv_sim::starlink::pool();
                let cause = pool.get_enum_by_name("SpaceX.API.Device.DishOutage.Cause").and_then(|e| e.get_value_by_name(&name));
                outage = Some(cause.ok_or_else(|| anyhow::anyhow!("unknown outage cause {name}"))?.number());
            }
            "--alert" => alerts.push(a.next().unwrap_or_default()),
            "-h" | "--help" => {
                println!("see the source header for options");
                return Ok(());
            }
            _ => anyhow::bail!("unknown option {k}"),
        }
    }
    let frozen = cfg.frozen;
    let sim = StarlinkSim::start(cfg).await?;
    sim.with(|s| {
        s.outage_cause = outage;
        s.alerts.extend(alerts);
    });
    eprintln!("starlink sim listening on {}", sim.target());
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            _ = term.recv() => break,
            _ = tokio::signal::ctrl_c() => break,
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
        if let Some(p) = &state_file {
            let j = sim.with(|s| {
                serde_json::json!({"up": s.up, "uptime": s.uptime(frozen), "reboots": s.reboots,
                    "update_checks": s.update_checks, "set_configs": s.set_configs, "calls": s.calls,
                    "update_state": s.update_state, "software_version": s.software_version})
            });
            let _ = std::fs::write(p, j.to_string());
        }
    }
    sim.stop();
    Ok(())
}
