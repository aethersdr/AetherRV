//! `aetherrv-sim-espar <vcan> [--loss-timeout S] [--after-run S] [--sleep S] [--log FILE] [--report FILE]`
//! Refuses any interface that isn't a vcan (never a real heater bus).

use aetherrv_sim::heater::{Heater, HeaterConfig};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let mut a = std::env::args().skip(1);
    let mut cfg = HeaterConfig { iface: a.next().unwrap_or_else(|| "vcan0".into()), ..Default::default() };
    if !cfg.iface.starts_with("vcan") {
        anyhow::bail!("refusing {}: the simulator runs on vcan only", cfg.iface);
    }
    let mut report = None;
    while let Some(k) = a.next() {
        let v = a.next().unwrap_or_default();
        match k.as_str() {
            "--loss-timeout" => cfg.loss_timeout_s = v.parse()?,
            "--after-run" => cfg.after_run_s = v.parse()?,
            "--sleep" => cfg.sleep_s = v.parse()?,
            "--log" => cfg.log = Some(v.into()),
            "--report" => report = Some(v),
            _ => anyhow::bail!("unknown option {k}"),
        }
    }
    let h = Heater::start(cfg)?;
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            _ = term.recv() => break,
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
        }
        if let Some(p) = &report {
            let r = h.report();
            let j = serde_json::json!({"state": r.state, "max_054_gap_running": r.max_054_gap_running,
                "controller_loss_faults": r.controller_loss_faults, "full_power_frames": r.full_power_frames,
                "frames_054": r.frames_054, "dtcs": r.dtcs, "heat_runs": r.heat_runs});
            let _ = std::fs::write(p, j.to_string());
        }
    }
    Ok(())
}
