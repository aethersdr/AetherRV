//! Radar history beyond LibreWXR's 2 h (Python `radar.py`): we save each published frame's tiles around the van
//! and serve them once the source has dropped them (it answers 404 for anything older than its 2 h list).
//!
//! Same tiles as the Weather map asks for (512 px, colour scheme 6, no smoothing), at tile zooms 4-7 (map zooms
//! 5-8), a small block around the van per zoom: ~21 tiles ≈ 0.2 MB a frame, ~30 MB a day. Frames are kept 6 h.
//! The newest frame is skipped each poll (it may still be filling in) and saved on the next one, while it's still
//! in the source's 2 h list. Forecast (nowcast) frames are never saved.
//!
//! On-disk layout (unchanged from Python, so a rollback reads it): `<root>/<t>/<z>/<x>/<y>.png`, `<root>/<t>/done`
//! (the tile count, no newline) once every tile of the frame is there, `<root>/<t>/flow.json` (the motion field
//! from frame `t` to the next one).

use crate::fetch::{Fetch, FetchError};
use crate::flow;
use aetherrv_core::num::{jf, round, round_int};
use aetherrv_core::state::{StateStore, StateView};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const INDEX: &str = "https://api.librewxr.net/public/weather-maps.json";
/// The source rejects some default client agents.
pub const USER_AGENT: &str = "AetherRV/1.0";
pub const TIMEOUT_S: u64 = 30;
/// Tile zoom → block width/height around the van (6 = default view).
pub const BLOCK: [(u32, i64); 4] = [(4, 2), (5, 2), (6, 3), (7, 2)];
pub const KEEP_S: f64 = 6.0 * 3600.0;
pub const POLL_S: f64 = 300.0;
/// Retry this soon while there's no GPS position yet (just started).
pub const NO_POSITION_RETRY_S: f64 = 30.0;
/// At most this many tile downloads at once.
pub const PARALLEL: usize = 4;
/// Motion is estimated between frames at most this far apart.
pub const FLOW_MAX_GAP_S: i64 = 900;

/// Served for tiles outside the recorded block: a 1×1 transparent RGBA PNG, byte-identical to Python's
/// `CLEAR_PNG` (zlib level 6).
pub const CLEAR_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01,
    0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41,
    0x54, 0x78, 0x9c, 0x63, 0x60, 0x00, 0x02, 0x00, 0x00, 0x05, 0x00, 0x01, 0x7a, 0x5e, 0xab, 0x3f, 0x00, 0x00, 0x00, 0x00,
    0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

/// The block width at a tile zoom.
pub fn block_size(z: u32) -> Option<i64> {
    BLOCK.iter().find(|(bz, _)| *bz == z).map(|(_, s)| *s)
}

/// The size×size tiles nearest the point (odd: centred on its tile; even: the quadrant it's closest to). Rows
/// outside the map are dropped; columns wrap.
pub fn block(lat: f64, lon: f64, z: u32, size: i64) -> Vec<(i64, i64)> {
    let n = 1i64 << z;
    let nf = n as f64;
    let fx = (lon + 180.0) / 360.0 * nf;
    let r = lat.clamp(-85.0511, 85.0511).to_radians();
    let fy = (1.0 - (r.tan() + 1.0 / r.cos()).ln() / std::f64::consts::PI) / 2.0 * nf;
    let odd = size % 2 != 0;
    let x0 = (if odd { fx.floor() as i64 } else { round_int(fx) }) - size / 2;
    let y0 = (if odd { fy.floor() as i64 } else { round_int(fy) }) - size / 2;
    let mut out = Vec::new();
    for j in 0..size {
        for i in 0..size {
            if (0..n).contains(&(y0 + j)) {
                out.push(((x0 + i).rem_euclid(n), y0 + j));
            }
        }
    }
    out
}

/// A failed poll, as the Python log names it (`radar recorder: <class>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollError(pub &'static str);

impl std::fmt::Display for PollError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl From<FetchError> for PollError {
    fn from(e: FetchError) -> Self {
        PollError(e.0)
    }
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Write a file via a temporary next to it, so a reader never sees half of it.
pub fn write_atomic(p: &Path, body: &[u8]) -> std::io::Result<()> {
    let mut tmp = p.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, p)
}

struct Stats {
    bytes: u64,
    since: f64,
    /// The last position read from the bus: Python's `State` keeps the GPS keys while the GPS module is gone, the
    /// bus reads a down owner's keys as unknown, so the recorder holds on to it itself.
    last_pos: Option<(f64, f64)>,
}

pub struct Recorder {
    pub root: PathBuf,
    pub keep_s: f64,
    pub poll_s: f64,
    /// The LibreWXR index URL.
    pub index: String,
    /// `location.lat` / `location.lon` (the bus's view of the GPS service).
    input: Arc<dyn StateView>,
    /// Where `radar.*` is published.
    out: Arc<StateStore>,
    fetch: Arc<dyn Fetch>,
    stats: Mutex<Stats>,
}

impl Recorder {
    pub fn new(
        root: impl Into<PathBuf>,
        input: Arc<dyn StateView>,
        out: Arc<StateStore>,
        fetch: Arc<dyn Fetch>,
    ) -> Recorder {
        Recorder {
            root: root.into(),
            keep_s: KEEP_S,
            poll_s: POLL_S,
            index: INDEX.to_string(),
            input,
            out,
            fetch,
            stats: Mutex::new(Stats { bytes: 0, since: aetherrv_core::clock::now_s(), last_pos: None }),
        }
    }

    fn frame_dir(&self, t: i64) -> PathBuf {
        self.root.join(t.to_string())
    }

    /// Times of the complete frames on disk, oldest first.
    pub fn frames(&self) -> Vec<i64> {
        let Ok(rd) = std::fs::read_dir(&self.root) else { return Vec::new() };
        let mut v: Vec<i64> = rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                if !is_digits(&name) || !e.path().join("done").exists() {
                    return None;
                }
                name.parse().ok()
            })
            .collect();
        v.sort_unstable();
        v
    }

    /// Start times of the frame pairs (t → next frame) that have a motion field.
    pub fn flows(&self) -> Vec<i64> {
        self.frames().into_iter().filter(|t| self.frame_dir(*t).join("flow.json").exists()).collect()
    }

    pub fn flow(&self, t: i64) -> Option<PathBuf> {
        let p = self.frame_dir(t).join("flow.json");
        p.is_file().then_some(p)
    }

    pub fn tile(&self, t: i64, z: i64, x: i64, y: i64) -> Option<PathBuf> {
        let p = self.frame_dir(t).join(z.to_string()).join(x.to_string()).join(format!("{y}.png"));
        p.is_file().then_some(p)
    }

    /// Remove every frame directory (complete or not) older than `keep_s`.
    pub fn prune(&self, now: f64) {
        let cut = now - self.keep_s;
        let Ok(rd) = std::fs::read_dir(&self.root) else { return };
        for e in rd.flatten() {
            let Ok(name) = e.file_name().into_string() else { continue };
            // a name too long for i64 is a (huge) Python int: newer than any cut
            if is_digits(&name) && name.parse::<i64>().is_ok_and(|t| (t as f64) < cut) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }

    /// The van's position, or the last one seen.
    pub fn position(&self) -> Option<(f64, f64)> {
        let num = |k: &str| match self.input.get(k) {
            Some(Value::Number(n)) => n.as_f64(),
            _ => None,
        };
        let mut st = self.stats.lock().unwrap();
        if let (Some(lat), Some(lon)) = (num("location.lat"), num("location.lon")) {
            st.last_pos = Some((lat, lon));
        }
        st.last_pos
    }

    /// Bytes saved since start, and the start time.
    pub fn saved_bytes(&self) -> (u64, f64) {
        let st = self.stats.lock().unwrap();
        (st.bytes, st.since)
    }

    /// One poll: save every listed frame we don't have except the newest, prune, make the missing motion fields,
    /// publish `radar.*`. `Ok(false)`: no position yet (nothing fetched).
    pub async fn poll(&self) -> Result<bool, PollError> {
        let Some((lat, lon)) = self.position() else { return Ok(false) };
        let r = self.fetch.get(&self.index).await?;
        if r.status >= 400 {
            return Err(PollError("ClientResponseError")); // raise_for_status()
        }
        let j: Value = serde_json::from_slice(&r.body).map_err(|_| PollError("JSONDecodeError"))?;
        let host = match j.get("host") {
            Some(Value::String(s)) => s.clone(),
            Some(_) => return Err(PollError("TypeError")),
            None => return Err(PollError("KeyError")),
        };
        let past = match j.get("radar").and_then(|r| r.get("past")) {
            Some(Value::Array(a)) => a.clone(),
            Some(_) => return Err(PollError("TypeError")),
            None => return Err(PollError("KeyError")),
        };
        let have = self.frames();
        // every gap, oldest first; never the newest
        for f in past.iter().take(past.len().saturating_sub(1)) {
            let t = f.get("time").ok_or(PollError("KeyError"))?.as_i64().ok_or(PollError("TypeError"))?;
            if have.contains(&t) {
                continue;
            }
            let path = f.get("path").ok_or(PollError("KeyError"))?.as_str().ok_or(PollError("TypeError"))?;
            self.save(&host, t, path, lat, lon).await?;
        }
        self.prune(aetherrv_core::clock::now_s());
        self.make_flows().await;
        let (bytes, since) = self.saved_bytes();
        let now = aetherrv_core::clock::now_s();
        let per_day = bytes as f64 / 1e6 / (1.0 / 24.0f64).max((now - since) / 86400.0);
        self.out.update([
            ("radar.recorded", json!(self.frames().len())),
            ("radar.flows", json!(self.flows().len())),
            ("radar.rec_mb_per_day", jf(round(per_day, 1))),
        ]);
        Ok(true)
    }

    /// Save one frame's tiles; the frame counts (`done`) only when every tile came back 200, otherwise it's
    /// dropped and tried again on the next poll if the source still lists it.
    pub async fn save(&self, host: &str, t: i64, path: &str, lat: f64, lon: f64) -> Result<(), PollError> {
        let d = self.frame_dir(t);
        let jobs: Vec<(u32, i64, i64)> =
            BLOCK.iter().flat_map(|&(z, size)| block(lat, lon, z, size).into_iter().map(move |(x, y)| (z, x, y))).collect();
        let sem = Arc::new(tokio::sync::Semaphore::new(PARALLEL));
        let mut set = tokio::task::JoinSet::new();
        for &(z, x, y) in &jobs {
            let url = format!("{host}{path}/512/{z}/{x}/{y}/6/0_0.png");
            let p = d.join(z.to_string()).join(x.to_string()).join(format!("{y}.png"));
            let (fetch, sem) = (self.fetch.clone(), sem.clone());
            set.spawn(async move {
                let body = {
                    let _permit = sem.acquire_owned().await.ok()?;
                    let r = fetch.get(&url).await.ok()?;
                    if r.status != 200 {
                        return None;
                    }
                    r.body
                };
                let n = body.len() as u64;
                tokio::task::spawn_blocking(move || {
                    std::fs::create_dir_all(p.parent()?).ok()?;
                    std::fs::write(&p, &body).ok()
                })
                .await
                .ok()??;
                Some(n)
            });
        }
        let mut all = true;
        while let Some(r) = set.join_next().await {
            match r {
                Ok(Some(n)) => self.stats.lock().unwrap().bytes += n,
                _ => all = false,
            }
        }
        let n = jobs.len();
        tokio::task::spawn_blocking(move || {
            if all {
                std::fs::write(d.join("done"), n.to_string()).map_err(|_| PollError("OSError"))
            } else {
                let _ = std::fs::remove_dir_all(&d); // incomplete: dropped, retried next poll if still listed
                Ok(())
            }
        })
        .await
        .map_err(|_| PollError("OSError"))?
    }

    /// Motion between consecutive recorded frames (same z6 block, 10 min apart), off the async runtime.
    pub async fn make_flows(&self) {
        let fr = self.frames();
        for w in fr.windows(2) {
            let (a, b) = (w[0], w[1]);
            let (da, db) = (self.frame_dir(a), self.frame_dir(b));
            if b - a > FLOW_MAX_GAP_S || da.join("flow.json").exists() {
                continue;
            }
            let r = tokio::task::spawn_blocking(move || {
                let f = flow::flow_pair(&da, &db)?;
                if let Some(f) = &f {
                    write_atomic(&da.join("flow.json"), f.to_json(b - a).as_bytes())?;
                }
                anyhow::Ok(f.is_some())
            })
            .await;
            match r {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::info!("radar flow {a}: {e}"), // a bad pair must not stop the recorder
                Err(e) => tracing::info!("radar flow {a}: {e}"),
            }
        }
    }

    /// The recorder loop (Python `RadarRecorder.run`).
    pub async fn run(self: Arc<Self>) {
        if let Err(e) = std::fs::create_dir_all(&self.root) {
            // e.g. a dev machine without /var/lib/aetherrv
            tracing::warn!("radar recorder off: cannot create {} ({e})", self.root.display());
            return;
        }
        loop {
            let ran = match self.poll().await {
                Ok(r) => r,
                Err(e) => {
                    tracing::info!("radar recorder: {e}"); // offline etc.; try again next poll
                    true
                }
            };
            let s = if ran { self.poll_s } else { NO_POSITION_RETRY_S };
            tokio::time::sleep(Duration::from_secs_f64(s)).await;
        }
    }
}
