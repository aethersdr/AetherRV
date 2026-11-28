//! `aetherrv-derive`: derived values (ARCHITECTURE.md §2).
//!
//! * energy (`src/aetherrv/energy.py`, every 1 s): `energy.alternator.power`, `energy.alternator.source`,
//!   `energy.dc.load` (the logic is `proto_n2k::energy::derive`).
//! * the cabin average (`aetherrv_core::climate`, every 2 s), only with `[climate] cabin_source = "average"`:
//!   `climate.cabin.temp`, `.humidity`, `.source`, `.front_temp` (front Ruuvi), `.rear_ruuvi_temp` (rear Ruuvi),
//!   `.rear_temp` (the MCS7's sensor by the bed).
//!
//! Inputs come from other services over the bus; an input whose owner is down reads as unknown (None), exactly
//! like a missing key in Python.

use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::climate::{self, Sensor};
use aetherrv_core::config::Cfg;
use aetherrv_core::num::jof;
use aetherrv_core::state::{StateStore, StateView};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// s. The Cerbo republishes a Ruuvi only when the value changes, so allow quiet spells.
pub const FRONT_STALE: f64 = climate::RUUVI_STALE;
/// s since the last successful MCS7 poll (`climate.rixens.updated`).
pub const REAR_STALE: f64 = climate::MCS7_STALE;

/// What the cabin average reads.
pub const CABIN_INPUTS: [&str; 9] = climate::INPUTS;

/// What the energy figures read.
pub const ENERGY_INPUTS: [&str; 6] = [
    "victron.alternator.power",
    "alternator.power",
    "victron.dc.load",
    "victron.battery.current",
    "victron.battery.voltage",
    "alternator.current",
];

/// Cabin temperature as the mean of the fresh sensors out of the front Ruuvi, the rear Ruuvi (when configured) and
/// the MCS7's own sensor at the back by the bed (owner, 2026-11-27); a missing or stale one is left out. With no
/// rear Ruuvi this is the front + MCS7 mean, as before.
pub fn derive_cabin(state: &dyn StateView, now: f64) -> Vec<(String, Value)> {
    let (temp, humidity, used) = climate::cabin(state, now);
    let source = match used.as_slice() {
        [] => Value::Null,
        [one] => json!(one.sensor.source()),
        _ => json!("average"),
    };
    let raw = |s: Sensor, k: &str| {
        if used.iter().any(|r| r.sensor == s) { state.get(k).unwrap_or(Value::Null) } else { Value::Null }
    };
    vec![
        ("climate.cabin.temp".into(), jof(temp)),
        ("climate.cabin.humidity".into(), jof(humidity)),
        ("climate.cabin.source".into(), source),
        ("climate.cabin.front_temp".into(), raw(Sensor::Front, "climate.cabin_front.temp")),
        ("climate.cabin.rear_temp".into(), raw(Sensor::Mcs7, "climate.rixens.cabin_c")),
        ("climate.cabin.rear_ruuvi_temp".into(), raw(Sensor::Rear, "climate.cabin_rear.temp")),
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
