//! `aetherrv-sim-clmd16 <vcanN> [--log FILE] [--report FILE] [--inputs 2,4] [--toggle-input N --every S]`
//! The CLMD16 load controller with the van's button layout on a virtual CAN interface (vcan only). SIGUSR1 toggles
//! wired input 2 (ignition). `--toggle-input N --every S` flips input N every S seconds (the soak's activity).

use aetherrv_sim::clmd16::{Clmd16, Clmd16Config};
use std::time::Duration;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let flag = |n: &str| a.iter().position(|x| x == n).and_then(|i| a.get(i + 1)).cloned();
    let iface = a.get(1).cloned().unwrap_or_default();
    let mut cfg = Clmd16Config::van(&iface);
    cfg.log = flag("--log").map(Into::into);
    cfg.inputs_on = flag("--inputs").unwrap_or_default().split(',').filter_map(|s| s.trim().parse().ok()).collect();
    let report = flag("--report");
    let toggle: Option<(u8, f64)> = flag("--toggle-input").and_then(|n| n.parse().ok()).zip(flag("--every").and_then(|s| s.parse().ok()));
    let sim = Clmd16::start(cfg)?;
    let mut usr1 = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())?;
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut iv = tokio::time::interval(Duration::from_secs(1));
    let (mut n, every) = (0u64, toggle.map(|t| t.1.max(1.0) as u64).unwrap_or(0));
    loop {
        tokio::select! {
            _ = term.recv() => return Ok(()),
            _ = usr1.recv() => sim.set_input(2, !sim.input(2)),
            _ = iv.tick() => {
                n += 1;
                if let Some((inp, _)) = toggle && every > 0 && n % every == 0 {
                    sim.set_input(inp, !sim.input(inp));
                }
                if let Some(p) = &report {
                    let tmp = format!("{p}.tmp");
                    if std::fs::write(&tmp, serde_json::to_string(&sim.report())?).is_ok() {
                        let _ = std::fs::rename(&tmp, p);
                    }
                }
            }
        }
    }
}
