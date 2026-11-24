//! `aetherrv-history`: chart history (port of `src/aetherrv/history.py`).
//!
//! Samples the [`TRACKED`] numeric keys every `interval` seconds into `samples(key, t, v)` in the shared SQLite file
//! (`[history] path`, WAL). Rows older than `retention_days` are pruned hourly. Queries bucket-average on the SQL
//! side ([`query`]), so a 30-day chart is cheap; the gateway runs the same query on its own read-only connection
//! ([`open_readonly`]), so charts keep working while this service restarts.
//!
//! A broken or unwritable database must not take the console down: the service keeps sampling into memory and
//! publishes `history.degraded` (the only key it owns).

use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::config::Cfg;
use aetherrv_core::num::round;
use aetherrv_core::state::{StateExt, StateStore, StateView};
use rusqlite::Connection;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The keys the charts use, in Python's order.
pub const TRACKED: [&str; 28] = [
    "victron.battery.soc",
    "victron.battery.power",
    "victron.pv.power",
    "victron.dc.load",
    "victron.ac.load",
    "victron.ac.shore",
    "victron.alternator.power",
    "energy.alternator.power",
    "energy.dc.load",
    "alternator.current",
    "climate.cabin.temp",
    "climate.outside.temp",
    "climate.fridge.temp",
    "climate.freezer.temp",
    "climate.rixens.coolant",
    "climate.rixens.fuel_hz",
    "climate.rixens.inlet_c",
    "climate.rixens.outlet_c",
    "climate.rixens.flame_c",
    "climate.rixens.fan_rpm",
    "climate.rixens.fuel_gph",
    "starlink.down_mbps",
    "starlink.up_mbps",
    "starlink.latency_ms",
    "starlink.snr_db",
    "starlink.power_w",
    "wan.down_mbps", // the Peplink's active WAN (Network chart)
    "wan.up_mbps",
];

pub const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS samples (key TEXT NOT NULL, t REAL NOT NULL, v REAL NOT NULL);\n\
     CREATE INDEX IF NOT EXISTS samples_key_t ON samples (key, t);";

pub const MEMORY: &str = ":memory:";

pub struct History {
    pub interval: f64,
    /// Seconds.
    pub retention: f64,
    /// The database in use (`":memory:"` after a fallback).
    pub path: String,
    /// Why the file isn't used (`"<path>: <error>"`), or `None`.
    pub degraded: Option<String>,
    db: Connection,
    last_prune: f64,
}

fn open(path: &str) -> rusqlite::Result<Connection> {
    let db = if path == MEMORY { Connection::open_in_memory()? } else { aetherrv_core::cmdlog::open_db(Path::new(path))? };
    db.execute_batch(SCHEMA)?;
    db.execute("INSERT INTO samples (key, t, v) SELECT 'x', 0, 0 WHERE 0", [])?; // proves writability
    Ok(db)
}

impl History {
    /// Python `History(state, path, interval, retention_days)`.
    pub fn new(path: &str, interval: f64, retention_days: f64) -> History {
        let mut path = path.to_string();
        if path != MEMORY
            && let Some(dir) = Path::new(&path).parent()
        {
            let _ = std::fs::create_dir_all(dir);
        }
        let mut degraded = None;
        let db = match open(&path) {
            Ok(db) => db,
            Err(e) => {
                // A broken/unwritable history DB must not take the console down: keep running in memory.
                tracing::error!("history DB {path} unusable ({e}); falling back to in-memory history");
                degraded = Some(format!("{path}: {e}"));
                path = MEMORY.to_string();
                open(MEMORY).expect("in-memory SQLite")
            }
        };
        let h = History { interval, retention: retention_days * 86400.0, path, degraded, db, last_prune: 0.0 };
        tracing::info!("history: {} ({} rows, {:.0}-day retention)", h.path, h.count(), retention_days);
        h
    }

    pub fn count(&self) -> i64 {
        self.db.query_row("SELECT count(*) FROM samples", [], |r| r.get(0)).unwrap_or(0)
    }

    /// One sample of every tracked key that is a number (bools are not samples); prunes at most hourly.
    pub fn sample(&mut self, state: &dyn StateView, now: f64) -> rusqlite::Result<()> {
        let t = round(now, 1);
        let rows: Vec<(&str, f64)> = TRACKED.iter().filter_map(|k| state.num(k).map(|v| (*k, v))).collect();
        if !rows.is_empty() {
            let tx = self.db.unchecked_transaction()?;
            {
                let mut st = tx.prepare_cached("INSERT INTO samples (key, t, v) VALUES (?1, ?2, ?3)")?;
                for (k, v) in rows {
                    st.execute(rusqlite::params![k, t, v])?;
                }
            }
            tx.commit()?;
        }
        if now - self.last_prune > 3600.0 {
            self.prune(now)?;
        }
        Ok(())
    }

    /// Delete rows older than the retention; returns how many.
    pub fn prune(&mut self, now: f64) -> rusqlite::Result<usize> {
        self.last_prune = now;
        self.db.execute("DELETE FROM samples WHERE t < ?1", [now - self.retention])
    }

    /// Points for `key` over the last `since` seconds, averaged into at most `max_points` buckets.
    pub fn get(&self, key: &str, since: f64, max_points: i64, now: f64) -> Vec<[f64; 2]> {
        query(&self.db, key, since, max_points, now, self.interval).unwrap_or_else(|e| {
            tracing::error!("history query failed: {e}");
            Vec::new()
        })
    }

    pub fn db(&self) -> &Connection {
        &self.db
    }
}

/// The chart query (Python `History.get`), on any connection to the samples table: points `[t, mean]` for `key`
/// over the last `since` seconds, averaged into buckets of `max(interval, since / max_points)` seconds. Untracked
/// keys return nothing.
pub fn query(db: &Connection, key: &str, since: f64, max_points: i64, now: f64, interval: f64) -> rusqlite::Result<Vec<[f64; 2]>> {
    if !TRACKED.contains(&key) {
        return Ok(Vec::new());
    }
    let start = now - since;
    let bucket = interval.max(since / max_points.max(1) as f64);
    let mut st = db.prepare_cached(
        "SELECT MAX(t), AVG(v) FROM samples WHERE key = ?1 AND t >= ?2 GROUP BY CAST((t - ?3) / ?4 AS INTEGER) ORDER BY 1",
    )?;
    let rows = st.query_map(rusqlite::params![key, start, start, bucket], |r| Ok([r.get::<_, f64>(0)?, round(r.get::<_, f64>(1)?, 2)]))?;
    rows.collect()
}

/// A read-only connection to the history file (the gateway's `/api/history`), with a busy timeout.
pub fn open_readonly(path: &Path) -> rusqlite::Result<Connection> {
    let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    db.busy_timeout(Duration::from_secs(5))?;
    Ok(db)
}

pub struct Options {
    pub bus_socket: Option<PathBuf>,
    pub path: String,
    pub interval: f64,
    pub retention_days: f64,
}

impl Options {
    pub fn from_config(cfg: &Cfg) -> Options {
        let h = cfg.at("history");
        Options {
            bus_socket: None,
            path: h.str("path", "/var/lib/aetherrv/history.sqlite3"),
            interval: h.f64("interval", 30.0),
            retention_days: h.f64("retention_days", 30.0),
        }
    }
}

/// Run the service until SIGTERM/SIGINT.
pub async fn run(cfg: Cfg, opts: Options) -> anyhow::Result<()> {
    let state = Arc::new(StateStore::new());
    let mut bo = BusOptions::new(aetherrv_core::names::HISTORY);
    if let Some(s) = opts.bus_socket.clone() {
        bo = bo.socket(s);
    }
    let cabin_source = cfg.at("climate").str("cabin_source", "victron");
    let bus = Bus::start(bo, state.clone());
    // Every subscription and `serve` right after `Bus::start`, before the first CONNACK can arrive: core's
    // `subscribe_filter` loses a filter added while a connect is in progress (`on_connect` copies the filter list
    // before it sets `connected`), and this way the command topic is subscribed before our status says "up".
    for k in TRACKED {
        if let Some(owner) = aetherrv_core::names::owner_of(k, &cabin_source, &|_| None) {
            bus.subscribe_state(&owner, k).await;
        }
    }
    let (path, interval, days) = (opts.path.clone(), opts.interval, opts.retention_days);
    let h = tokio::task::spawn_blocking(move || History::new(&path, interval, days)).await?;
    if let Some(d) = &h.degraded {
        state.set("history.degraded", json!(d));
    }
    let h = Arc::new(Mutex::new(h));
    aetherrv_core::notify::ready();
    aetherrv_core::notify::spawn_runtime_watchdog();
    let task = {
        let (h, view) = (h.clone(), bus.remote().clone());
        tokio::spawn(async move {
            loop {
                let (h, view) = (h.clone(), view.clone());
                let r = tokio::task::spawn_blocking(move || {
                    h.lock().unwrap_or_else(|p| p.into_inner()).sample(&*view, aetherrv_core::clock::now_s())
                })
                .await;
                match r {
                    Ok(Err(e)) => tracing::error!("history sample failed: {e}"),
                    Err(e) => tracing::error!("history sample failed: {e}"),
                    Ok(Ok(())) => {}
                }
                tokio::time::sleep(Duration::from_secs_f64(interval.max(0.1))).await;
            }
        })
    };
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = term.recv() => {},
        _ = tokio::signal::ctrl_c() => {},
        r = task => tracing::error!("history: the sample loop ended ({r:?})"),
    }
    aetherrv_core::notify::stopping();
    bus.shutdown().await;
    Ok(())
}
