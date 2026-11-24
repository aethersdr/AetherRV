//! `aetherrv-can-aux --bus <name>`: one spare CAN port (ARCHITECTURE.md §2): capture to candump logs, the frame
//! inventory, and the MCS7 CAN decoder for a `kind = "rixens"` device on that bus (Python `CaptureOnlyAdapter` and
//! `RixensAdapter`). It has no send path at all.

use aetherrv_can::{CanBus, CanFrame};
use aetherrv_core::config::Cfg;
use aetherrv_core::state::StateStore;
use proto_mcs7::can::{CanFrame as McsFrame, IdInventory, Mcs7Can};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Buses no other service owns: everything in `[bus.*]` except the Espar's and the N2K paths.
pub fn aux_buses(cfg: &Cfg) -> Vec<String> {
    let dev = cfg.at("device");
    let mut owned = vec![];
    for (_, d) in dev.entries() {
        match d.str("kind", "").as_str() {
            "espar" | "n2k" => {
                owned.push(d.str("bus", ""));
                if let Some(b) = d.opt_str("backup_bus") {
                    owned.push(b);
                }
            }
            _ => {}
        }
    }
    cfg.at("bus").entries().into_iter().map(|(n, _)| n).filter(|n| !owned.contains(n)).collect()
}

enum Decoder {
    Capture(IdInventory),
    Mcs7(Box<Mcs7Can>),
}

pub async fn run(cfg: Cfg, bus_name: String, bus_socket: Option<std::path::PathBuf>) -> anyhow::Result<()> {
    let b = cfg.at("bus").child(&bus_name);
    if b.is_empty() {
        anyhow::bail!("bus {bus_name} is not configured");
    }
    let state = Arc::new(StateStore::new());
    let mut bo = aetherrv_core::bus::BusOptions::new(&aetherrv_core::names::can_aux(&bus_name));
    if let Some(s) = bus_socket {
        bo = bo.socket(s);
    }
    let bus = aetherrv_core::bus::Bus::start(bo, state.clone());
    let capture = (cfg.bool("capture", true) && b.bool("capture", true)).then(|| std::path::PathBuf::from(cfg.str("capture_dir", "captures")));
    let can = match b.str("interface", "socketcan").as_str() {
        "socketcand" => CanBus::socketcand(&bus_name, &b.str("host", ""), b.i64("port", 29536) as u16, &b.str("channel", "can0"), capture),
        _ => CanBus::socketcan(&bus_name, &b.str("channel", &bus_name), false, capture),
    };
    let rixens_http_on = cfg.at("device").entries().iter().any(|(_, d)| d.str("kind", "") == "rixens_http" && d.bool("enabled", false));
    let decoders: Arc<Mutex<Vec<(String, Decoder)>>> = Arc::new(Mutex::new(
        cfg.at("device")
            .entries()
            .into_iter()
            .filter(|(_, d)| d.str("bus", "") == bus_name)
            .filter_map(|(name, d)| match d.str("kind", "").as_str() {
                "capture" => Some((name, Decoder::Capture(IdInventory::default()))),
                // CAN = fallback/inventory when the HTTP adapter runs
                "rixens" => Some((name.clone(), Decoder::Mcs7(Box::new(Mcs7Can::new(&name, !rixens_http_on))))),
                _ => None,
            })
            .collect(),
    ));
    {
        let d = decoders.clone();
        can.add_handler(Arc::new(move |f: &CanFrame| {
            let now = if f.ts > 0.0 { f.ts } else { aetherrv_core::clock::now_s() };
            let mf = McsFrame { id: f.id, data: &f.data, extended: f.extended, remote: f.remote, error: f.error };
            for (_, dec) in d.lock().unwrap().iter_mut() {
                match dec {
                    Decoder::Capture(i) => i.on_frame(&mf, now),
                    Decoder::Mcs7(m) => m.on_frame(&mf, now),
                }
            }
        }));
    }
    tokio::spawn(can.clone().run());
    tokio::spawn(aetherrv_can::run_stats(vec![can.clone()], state.clone()));
    aetherrv_core::notify::ready();
    aetherrv_core::notify::spawn_runtime_watchdog();
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        let now = aetherrv_core::clock::now_s();
        let mut keys: Vec<(String, Value)> = vec![];
        for (name, dec) in decoders.lock().unwrap().iter() {
            match dec {
                Decoder::Capture(i) => keys.push((format!("{name}.ids"), i.summary(now))),
                Decoder::Mcs7(m) => keys.extend(m.publish(now)),
            }
        }
        state.update(keys);
        tokio::select! {
            _ = term.recv() => break,
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
    }
    bus.shutdown().await;
    Ok(())
}
