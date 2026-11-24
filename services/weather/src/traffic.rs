//! TomTom traffic tiles for the Weather map, fetched here so the API key never reaches a browser (Python
//! `traffic.py`).
//!
//! Two raster layers (TomTom Traffic API v4): flow with the 'relative-delay' style (only roads slower than
//! free-flow, i.e. slowdowns; free-flowing roads stay undrawn) and incidents (s0 / s0-dark). Tiles are cached for a
//! few minutes and shared by every screen; a daily cap keeps us inside the free tier and limits WAN data.
//!
//! The key is in every upstream URL, so a URL is never logged and nothing upstream (headers, error bodies) is
//! passed on: a browser only ever gets a 200 tile body, or an empty 429/502.

use crate::fetch::{Fetch, Fetched};
use hyper::body::Bytes;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

pub const BASE: &str = "https://api.tomtom.com/traffic/map/4/tile";
/// kind → allowed styles.
pub const STYLES: &[(&str, &[&str])] = &[("flow", &["major"]), ("incidents", &["s0", "s0-dark"])];
/// Traffic refreshes about once a minute; the map asks every 5 min.
pub const TTL_S: f64 = 270.0;
/// In-memory cache (512 px PNGs, mostly a few KB each).
pub const MAX_TILES: usize = 1500;
/// Upstream fetches per UTC day (free tier is far above this).
pub const DAILY_CAP: u64 = 20000;
pub const TIMEOUT_S: u64 = 15;

type Key = (String, String, i64, i64, i64);

struct Entry {
    t: f64,
    status: u16,
    body: Bytes,
    seq: u64,
}

#[derive(Default)]
struct Inner {
    cache: HashMap<Key, Entry>,
    /// LRU order: seq → key (oldest first), like Python's `OrderedDict` + `move_to_end`.
    order: BTreeMap<u64, Key>,
    seq: u64,
    day: String,
    fetched: u64,
    last_error: Option<String>,
}

impl Inner {
    fn touch(&mut self, k: &Key) {
        self.seq += 1;
        let seq = self.seq;
        if let Some(e) = self.cache.get_mut(k) {
            self.order.remove(&e.seq);
            e.seq = seq;
            self.order.insert(seq, k.clone());
        }
    }
}

pub struct TrafficTiles {
    key: String,
    pub cap: u64,
    pub ttl: f64,
    fetch: Arc<dyn Fetch>,
    inner: Mutex<Inner>,
}

/// UTC day of a Unix time (`time.strftime("%Y-%m-%d", time.gmtime(now))`).
pub fn utc_day(t: f64) -> String {
    let days = (t / 86400.0).floor() as i64;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days since 1970-01-01 → (year, month, day), proleptic Gregorian (H. Hinnant's algorithm).
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

impl TrafficTiles {
    pub fn new(key: impl Into<String>, daily_cap: u64, ttl_s: f64, fetch: Arc<dyn Fetch>) -> TrafficTiles {
        TrafficTiles { key: key.into(), cap: daily_cap, ttl: ttl_s, fetch, inner: Mutex::new(Inner::default()) }
    }

    pub fn valid(kind: &str, style: &str, z: i64, x: i64, y: i64) -> bool {
        let ok_style = STYLES.iter().any(|(k, s)| *k == kind && s.contains(&style));
        ok_style && (2..=12).contains(&z) && (0..1i64 << z).contains(&x) && (0..1i64 << z).contains(&y)
    }

    /// The upstream URL. Holds the key: never log it.
    pub fn url(&self, kind: &str, style: &str, z: i64, x: i64, y: i64) -> String {
        if kind == "flow" {
            // relative-delay: only roads slower than free-flow are drawn
            return format!("{BASE}/flow/relative-delay/{z}/{x}/{y}.png?key={}&tileSize=512&thickness=6", self.key);
        }
        format!("{BASE}/incidents/{style}/{z}/{x}/{y}.png?key={}&tileSize=512&t=-1", self.key)
    }

    /// Upstream fetches so far today.
    pub fn fetched_today(&self) -> u64 {
        self.inner.lock().unwrap().fetched
    }

    pub fn cached(&self) -> usize {
        self.inner.lock().unwrap().cache.len()
    }

    /// → (http status, PNG bytes). 429 when the daily cap is hit; upstream errors come back as 502.
    pub async fn get(&self, kind: &str, style: &str, z: i64, x: i64, y: i64) -> (u16, Bytes) {
        self.get_at(kind, style, z, x, y, aetherrv_core::clock::now_s()).await
    }

    pub async fn get_at(&self, kind: &str, style: &str, z: i64, x: i64, y: i64, now: f64) -> (u16, Bytes) {
        let k: Key = (kind.to_string(), style.to_string(), z, x, y);
        let hit = {
            let mut g = self.inner.lock().unwrap();
            let hit = g.cache.get(&k).map(|e| (e.t, e.status, e.body.clone()));
            if let Some((t, status, body)) = &hit
                && now - t < self.ttl
            {
                g.touch(&k);
                return (*status, body.clone());
            }
            let day = utc_day(now);
            if day != g.day {
                g.day = day;
                g.fetched = 0;
            }
            if g.fetched >= self.cap {
                return stale_or(hit, 429); // stale beats nothing
            }
            g.fetched += 1;
            hit
        };
        let r = self.fetch.get(&self.url(kind, style, z, x, y)).await;
        let mut g = self.inner.lock().unwrap();
        let body = match r {
            Ok(Fetched { status: 200, body }) => body,
            Ok(Fetched { status, .. }) => {
                let err = format!("HTTP {status}");
                if g.last_error.as_deref() != Some(err.as_str()) {
                    // once per change, never the URL (holds the key)
                    tracing::warn!("traffic tiles: TomTom answered {err}");
                }
                g.last_error = Some(err);
                return stale_or(hit, 502);
            }
            Err(e) => {
                if g.last_error.as_deref() != Some(e.0) {
                    tracing::warn!("traffic tiles: {e}");
                }
                g.last_error = Some(e.0.to_string());
                return stale_or(hit, 502);
            }
        };
        g.last_error = None;
        if let Some(old) = g.cache.remove(&k) {
            g.order.remove(&old.seq);
        }
        g.seq += 1;
        let seq = g.seq;
        g.cache.insert(k.clone(), Entry { t: now, status: 200, body: body.clone(), seq });
        g.order.insert(seq, k);
        while g.cache.len() > MAX_TILES {
            let Some((_, old)) = g.order.pop_first() else { break };
            g.cache.remove(&old);
        }
        (200, body)
    }
}

fn stale_or(hit: Option<(f64, u16, Bytes)>, status: u16) -> (u16, Bytes) {
    match hit {
        Some((_, s, b)) => (s, b),
        None => (status, Bytes::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_days() {
        assert_eq!(utc_day(0.0), "1970-01-01");
        assert_eq!(utc_day(86399.9), "1970-01-01");
        assert_eq!(utc_day(86400.0), "1970-01-02");
        assert_eq!(utc_day(951_782_400.0), "2000-02-29");
        assert_eq!(utc_day(1_795_996_342.0), "2026-11-29");
        assert_eq!(utc_day(1_798_761_599.0), "2026-12-31");
        assert_eq!(utc_day(1_798_761_600.0), "2027-01-01");
    }
}
