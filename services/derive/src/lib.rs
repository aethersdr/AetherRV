//! `aetherrv-derive`: derived values (ARCHITECTURE.md §2).
//!
//! * energy (`src/aetherrv/energy.py`, every 1 s): `energy.alternator.power`, `energy.alternator.source`,
//!   `energy.dc.load` (the logic is `proto_n2k::energy::derive`).
//! * the cabin average (`src/aetherrv/climate.py` `derive_cabin`, every 2 s), only with `[climate] cabin_source =
//!   "average"`: `climate.cabin.temp`, `.humidity`, `.source`, `.front_temp`, `.rear_temp`.
//!
//! Inputs come from other services over the bus; an input whose owner is down reads as unknown (None), exactly
//! like a missing key in Python.

use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::config::Cfg;
use aetherrv_core::num::{jof, round};
use aetherrv_core::state::{StateExt, StateStore, StateView};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// s. The Cerbo republishes a Ruuvi only when the value changes, so allow quiet spells.
pub const FRONT_STALE: f64 = 15.0 * 60.0;
/// s since the last successful MCS7 poll (`climate.rixens.updated`).
pub const REAR_STALE: f64 = 60.0;

/// What the cabin average reads.
pub const CABIN_INPUTS: [&str; 7] = [
    "climate.cabin_front.temp",
    "climate.cabin_front.humidity",
    "victron.connected",
    "climate.rixens.cabin_c",
    "climate.rixens.cabin_rh",
    "climate.rixens.live",
    "climate.rixens.updated",
];

/// What the energy figures read.
pub const ENERGY_INPUTS: [&str; 6] = [
    "victron.alternator.power",
    "alternator.power",
    "victron.dc.load",
    "victron.battery.current",
    "victron.battery.voltage",
    "alternator.current",
];

fn mean(vals: &[Option<f64>]) -> Option<f64> {
    let v: Vec<f64> = vals.iter().flatten().copied().collect();
    if v.is_empty() { None } else { Some(round(v.iter().sum::<f64>() / v.len() as f64, 2)) }
}

/// Cabin temperature as the mean of the front Ruuvi (via the Cerbo) and the MCS7's own sensor at the back; if one
/// is missing or stale, the other alone. Display only: the heater regulates on its own sensor.
pub fn derive_cabin(state: &dyn StateView, now: f64) -> Vec<(String, Value)> {
    let age = state.age("climate.cabin_front.temp");
    let front_ok = state.val("climate.cabin_front.temp").is_some()
        && !state.is_false("victron.connected")
        && age.is_some_and(|a| a < FRONT_STALE);
    let upd = state.num("climate.rixens.updated");
    let rear_ok = state.val("climate.rixens.cabin_c").is_some()
        && !state.is_false("climate.rixens.live")
        && upd.is_some_and(|u| now - u < REAR_STALE);
    let (mut temps, mut hums, mut used) = (Vec::new(), Vec::new(), Vec::new());
    if front_ok {
        temps.push(state.num("climate.cabin_front.temp"));
        hums.push(state.num("climate.cabin_front.humidity"));
        used.push("front");
    }
    if rear_ok {
        temps.push(state.num("climate.rixens.cabin_c"));
        hums.push(state.num("climate.rixens.cabin_rh"));
        used.push("rear");
    }
    let source = match used.as_slice() {
        [_, _] => json!("average"),
        [one] => json!(one),
        _ => Value::Null,
    };
    let raw = |ok: bool, k: &str| if ok { state.get(k).unwrap_or(Value::Null) } else { Value::Null };
    vec![
        ("climate.cabin.temp".into(), jof(mean(&temps))),
        ("climate.cabin.humidity".into(), jof(mean(&hums))),
        ("climate.cabin.source".into(), source),
        ("climate.cabin.front_temp".into(), raw(front_ok, "climate.cabin_front.temp")),
        ("climate.cabin.rear_temp".into(), raw(rear_ok, "climate.rixens.cabin_c")),
    ]
}

/// The energy figures (Python `energy.derive`).
pub fn derive_energy(state: &dyn StateView) -> Vec<(String, Value)> {
    proto_n2k::energy::derive(|k| state.get(k))
}

pub struct Options {
    pub bus_socket: Option<PathBuf>,
    pub energy_period: Duration,
    pub cabin_period: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Options { bus_socket: None, energy_period: Duration::from_secs(1), cabin_period: Duration::from_secs(2) }
    }
}

fn owner(key: &str, cabin_source: &str) -> Option<String> {
    aetherrv_core::names::owner_of(key, cabin_source, &|_| None)
}

/// Run the service until SIGTERM/SIGINT.
pub async fn run(cfg: Cfg, opts: Options) -> anyhow::Result<()> {
    let state = Arc::new(StateStore::new());
    let mut bo = BusOptions::new(aetherrv_core::names::DERIVE);
    if let Some(s) = opts.bus_socket.clone() {
        bo = bo.socket(s);
    }
    let cabin_source = cfg.at("climate").str("cabin_source", "victron");
    let average = cabin_source == "average";
    let bus = Bus::start(bo, state.clone());
    // Every subscription and `serve` right after `Bus::start`, before the first CONNACK can arrive: core's
    // `subscribe_filter` loses a filter added while a connect is in progress (`on_connect` copies the filter list
    // before it sets `connected`), and this way the command topic is subscribed before our status says "up".
    let cabin: &[&str] = if average { &CABIN_INPUTS } else { &[] };
    for k in ENERGY_INPUTS.iter().chain(cabin) {
        if let Some(o) = owner(k, &cabin_source) {
            bus.subscribe_state(&o, k).await;
        }
    }
    let view: Arc<dyn StateView> = bus.remote().clone();
    aetherrv_core::notify::ready();
    aetherrv_core::notify::spawn_runtime_watchdog();
    {
        let (st, view) = (state.clone(), view.clone());
        tokio::spawn(async move {
            loop {
                st.update(derive_energy(&*view));
                tokio::time::sleep(opts.energy_period).await;
            }
        });
    }
    if average {
        tracing::info!("derive: cabin temperature = the average of the front and rear sensors");
        let (st, view) = (state.clone(), view.clone());
        tokio::spawn(async move {
            loop {
                st.update(derive_cabin(&*view, aetherrv_core::clock::now_s()));
                tokio::time::sleep(opts.cabin_period).await;
            }
        });
    }
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = term.recv() => {},
        _ = tokio::signal::ctrl_c() => {},
    }
    aetherrv_core::notify::stopping();
    bus.shutdown().await;
    Ok(())
}
