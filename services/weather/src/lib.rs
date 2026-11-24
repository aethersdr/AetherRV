//! `aetherrv-weather`: the radar recorder, its motion fields and the TomTom traffic proxy (Python `radar.py`,
//! `traffic.py` and their `web.py` routes; ARCHITECTURE.md §2, §5, §9).
//!
//! Reads `location.lat` / `location.lon` from the GPS service, publishes `radar.recorded`, `radar.flows`,
//! `radar.rec_mb_per_day` and `traffic.available`, and answers the gateway's `/api/radar/*` and `/api/traffic/*`
//! on a Unix socket (`[radar] socket`). Serving needs neither the bus nor a GPS fix nor a writable radar
//! directory: each of those only pauses the recorder.

pub mod fetch;
pub mod flow;
pub mod http;
pub mod radar;
pub mod traffic;

use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::config::{Cfg, load_secrets};
use aetherrv_core::names::{GPS, WEATHER};
use aetherrv_core::state::{StateStore, StateView};
use fetch::HttpFetch;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Keys this service reads from other services.
pub const FOLLOW: &[(&str, &str)] = &[(GPS, "location.lat"), (GPS, "location.lon")];

pub const DEFAULT_DIR: &str = "/var/lib/aetherrv/radar";
pub const DEFAULT_SOCKET: &str = "/run/aetherrv/weather/weather.sock"; // the unit's RuntimeDirectory (/run/aetherrv itself is root's)

pub struct Options {
    pub bus_socket: Option<PathBuf>,
    /// `[radar] dir`
    pub radar_dir: PathBuf,
    /// `[radar] socket`
    pub socket: PathBuf,
}

impl Options {
    pub fn from_config(cfg: &Cfg) -> Options {
        let r = cfg.at("radar");
        Options { bus_socket: None, radar_dir: r.str("dir", DEFAULT_DIR).into(), socket: r.str("socket", DEFAULT_SOCKET).into() }
    }
}

/// The traffic proxy, if the secrets file has a `tomtom_api_key`.
pub fn traffic_from_secrets(secrets: &Cfg) -> anyhow::Result<Option<Arc<traffic::TrafficTiles>>> {
    let key = secrets.opt_str("tomtom_api_key").unwrap_or_default();
    if key.is_empty() {
        return Ok(None);
    }
    let f = Arc::new(HttpFetch::new(None, Duration::from_secs(traffic::TIMEOUT_S))?);
    Ok(Some(Arc::new(traffic::TrafficTiles::new(key, traffic::DAILY_CAP, traffic::TTL_S, f))))
}

/// Run the service until SIGTERM/SIGINT.
pub async fn run(cfg: Cfg, opts: Options) -> anyhow::Result<()> {
    let state = Arc::new(StateStore::new());
    let mut bo = BusOptions::new(WEATHER);
    if let Some(s) = &opts.bus_socket {
        bo = bo.socket(s);
    }
    let bus = Bus::start(bo, state.clone());
    for (owner, key) in FOLLOW {
        bus.subscribe_state(owner, key).await;
    }
    let traffic = traffic_from_secrets(&load_secrets(&cfg))?;
    state.set("traffic.available", json!(traffic.is_some()));
    let fetch = Arc::new(HttpFetch::new(Some(radar::USER_AGENT), Duration::from_secs(radar::TIMEOUT_S))?);
    let view: Arc<dyn StateView> = bus.remote().clone();
    let rec = Arc::new(radar::Recorder::new(&opts.radar_dir, view, state.clone(), fetch));

    let listener = http::bind(&opts.socket)?;
    let app = Arc::new(http::App { radar: rec.clone(), traffic });
    let server = tokio::spawn(http::serve(listener, app));
    tokio::spawn(rec.run());
    aetherrv_core::notify::ready();
    aetherrv_core::notify::spawn_runtime_watchdog();
    tracing::info!("weather: serving {} (radar cache {})", opts.socket.display(), opts.radar_dir.display());

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = term.recv() => {},
        _ = tokio::signal::ctrl_c() => {},
    }
    aetherrv_core::notify::stopping();
    server.abort();
    let _ = std::fs::remove_file(&opts.socket);
    bus.shutdown().await;
    Ok(())
}
