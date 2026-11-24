//! `aetherrv-gps`: the NMEA 0183 GPS listener over UDP (the socket around `proto_nmea::NmeaGps`; port of
//! `src/aetherrv/adapters/nmea_gps.py` `NmeaGps.run`). Receive-only.
//!
//! The Peplink's "GPS Forwarding" pushes NMEA sentences over UDP. Broadcasts to the van LAN only reach hosts on that
//! LAN, so a Pi on the home LAN needs a unicast destination (its own IP). Either way this binds a UDP port
//! (`[gps] bind`, `port`; broadcasts allowed) and parses what arrives: `location.*` and `gps.sentences`. Every 5 s,
//! `location.fix = false` once the last fix is older than 30 s.

use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::config::Cfg;
use aetherrv_core::state::StateStore;
use proto_nmea::NmeaGps;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct Options {
    pub bus_socket: Option<PathBuf>,
    pub enabled: bool,
    pub bind: String,
    pub port: u16,
    pub source: String,
    pub stale_after: f64,
    pub stale_check: Duration,
    /// A socket bound already (tests); otherwise `bind:port` is bound at start.
    pub socket: Option<std::net::UdpSocket>,
}

impl Options {
    pub fn from_config(cfg: &Cfg) -> Options {
        let g = cfg.at("gps");
        Options {
            bus_socket: None,
            enabled: g.bool("enabled", false),
            bind: g.str("bind", "0.0.0.0"),
            port: u16::try_from(g.i64("port", 10110)).unwrap_or(10110),
            source: g.str("source", "peplink"),
            stale_after: 30.0,
            stale_check: Duration::from_secs(5),
            socket: None,
        }
    }
}

/// Run the service until SIGTERM/SIGINT. With `[gps] enabled = false` it stays up and publishes nothing (Python
/// didn't start the listener at all).
pub async fn run(_cfg: Cfg, opts: Options) -> anyhow::Result<()> {
    let state = Arc::new(StateStore::new());
    let mut bo = BusOptions::new(aetherrv_core::names::GPS);
    if let Some(s) = opts.bus_socket.clone() {
        bo = bo.socket(s);
    }
    let bus = Bus::start(bo, state.clone());
    if opts.enabled {
        let sock = match opts.socket {
            Some(s) => {
                s.set_nonblocking(true)?;
                tokio::net::UdpSocket::from_std(s)?
            }
            None => tokio::net::UdpSocket::bind((opts.bind.as_str(), opts.port)).await?,
        };
        sock.set_broadcast(true)?;
        tracing::info!("NMEA GPS listening on udp/{}:{}", opts.bind, sock.local_addr().map(|a| a.port()).unwrap_or(opts.port));
        let gps = Arc::new(Mutex::new(NmeaGps::new(&opts.source, opts.stale_after)));
        {
            let (gps, st) = (gps.clone(), state.clone());
            tokio::spawn(async move {
                let mut buf = vec![0u8; 65536];
                loop {
                    match sock.recv_from(&mut buf).await {
                        Ok((n, _)) => {
                            let up = gps.lock().unwrap().feed_datagram(&buf[..n], aetherrv_core::clock::now_s());
                            st.update(up);
                        }
                        Err(e) => {
                            tracing::warn!("gps: receive failed: {e}");
                            tokio::time::sleep(Duration::from_millis(200)).await;
                        }
                    }
                }
            });
        }
        {
            let (gps, st, every) = (gps.clone(), state.clone(), opts.stale_check);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(every).await;
                    if let Some((k, v)) = gps.lock().unwrap().stale(aetherrv_core::clock::now_s()) {
                        st.set(&k, v);
                    }
                }
            });
        }
    } else {
        tracing::info!("gps: disabled ([gps] enabled = false)");
    }
    aetherrv_core::notify::ready();
    aetherrv_core::notify::spawn_runtime_watchdog();
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = term.recv() => {},
        _ = tokio::signal::ctrl_c() => {},
    }
    aetherrv_core::notify::stopping();
    bus.shutdown().await;
    Ok(())
}
