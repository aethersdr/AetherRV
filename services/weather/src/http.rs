//! The radar and traffic routes of Python's `web.py`, served on a Unix socket (the gateway proxies `/api/radar/*`
//! and `/api/traffic/*` here). Same status codes, bodies and caching headers as aiohttp gave them:
//!
//! | route | answer |
//! |---|---|
//! | `GET /api/radar/recorded` | `{"frames": […], "flows": […], "zoom": [4, 7]}` (Python `json.dumps` spacing) |
//! | `GET /api/radar/tile/{t}/{z}/{x}/{y}.png` | the saved tile (`max-age=86400`), else the 1×1 clear PNG (`max-age=300`) |
//! | `GET /api/radar/flow/{t}` | `flow.json` (`application/json`, `max-age=86400`), else 404 |
//! | `GET /api/traffic/{kind}/{style}/{z}/{x}/{y}.png` | the TomTom tile (`max-age=240`); 404 unknown layer or no key; 429 daily cap; 502 upstream error |
//!
//! A non-integer path number is 400 (`400: Bad Request`); other methods 405 with `Allow: GET,HEAD`. Files carry
//! aiohttp's `ETag` (`"<mtime ns hex>-<size hex>"`) and `Last-Modified`, and answer 304 to a matching
//! `If-None-Match` / `If-Modified-Since`.

use crate::radar::{CLEAR_PNG, Recorder};
use crate::traffic::{TrafficTiles, civil_from_days};
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::header::{HeaderMap, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::net::UnixListener;

pub type Resp = Response<Full<Bytes>>;

pub struct App {
    pub radar: Arc<Recorder>,
    /// None without a TomTom key: every traffic tile is 404.
    pub traffic: Option<Arc<TrafficTiles>>,
}

/// Bind the socket: its directory is created, a stale socket file replaced, a live one (another instance) refused.
pub fn bind(path: &Path) -> anyhow::Result<UnixListener> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    if std::os::unix::net::UnixStream::connect(path).is_ok() {
        anyhow::bail!("{} is already being served (another aetherrv-weather?)", path.display());
    }
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let l = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
    Ok(l)
}

/// Serve until the task is dropped.
pub async fn serve(listener: UnixListener, app: Arc<App>) {
    loop {
        let stream = match listener.accept().await {
            Ok((s, _)) => s,
            Err(e) => {
                tracing::warn!("weather http: accept failed: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
        };
        let app = app.clone();
        tokio::spawn(async move {
            let svc = hyper::service::service_fn(move |req: Request<Incoming>| {
                let app = app.clone();
                async move { Ok::<_, std::convert::Infallible>(handle(&app, req.method(), req.uri().path(), req.headers()).await) }
            });
            if let Err(e) = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), svc).await {
                tracing::debug!("weather http: connection: {e}");
            }
        });
    }
}

// ── routing ──────────────────────────────────────────────────────────────────────────────────────────────────────

enum Route<'a> {
    Recorded,
    Tile([&'a str; 4]),
    Flow(&'a str),
    Traffic(&'a str, &'a str, [&'a str; 3]),
}

/// aiohttp's `{name}` matches `[^{}/]+`.
fn seg(s: &str) -> Option<&str> {
    (!s.is_empty() && !s.contains(['{', '}'])).then_some(s)
}

fn png_seg(s: &str) -> Option<&str> {
    seg(s.strip_suffix(".png")?)
}

fn route(path: &str) -> Option<Route<'_>> {
    let segs: Vec<&str> = path.strip_prefix('/')?.split('/').collect();
    Some(match segs.as_slice() {
        ["api", "radar", "recorded"] => Route::Recorded,
        ["api", "radar", "tile", t, z, x, y] => Route::Tile([seg(t)?, seg(z)?, seg(x)?, png_seg(y)?]),
        ["api", "radar", "flow", t] => Route::Flow(seg(t)?),
        ["api", "traffic", kind, style, z, x, y] => Route::Traffic(seg(kind)?, seg(style)?, [seg(z)?, seg(x)?, png_seg(y)?]),
        _ => return None,
    })
}

/// A path number as Python's `int()` reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PyInt {
    Val(i64),
    /// A valid Python int beyond i64: names no tile, no frame, no valid traffic tile.
    Huge,
    /// `int()` raises ValueError → 400.
    Bad,
}

/// Python `int(s)` (optional sign, ASCII digits, single underscores between digits, surrounding whitespace).
pub fn py_int(s: &str) -> PyInt {
    let t = s.trim();
    let (neg, digits) = match t.as_bytes().first() {
        Some(b'+') => (false, &t[1..]),
        Some(b'-') => (true, &t[1..]),
        _ => (false, t),
    };
    let ok = !digits.is_empty()
        && !digits.starts_with('_')
        && !digits.ends_with('_')
        && !digits.contains("__")
        && digits.bytes().all(|b| b.is_ascii_digit() || b == b'_');
    if !ok {
        return PyInt::Bad;
    }
    let clean: String = digits.chars().filter(|c| *c != '_').collect();
    match clean.parse::<i64>() {
        Ok(v) => PyInt::Val(if neg { -v } else { v }),
        Err(_) => PyInt::Huge,
    }
}

/// A path number Python's `int()` refuses (→ 400).
struct BadInt;

/// Every number parsed; Err if any is bad, Ok(None) if any is beyond i64.
fn ints<const N: usize>(s: [&str; N]) -> Result<Option<[i64; N]>, BadInt> {
    let mut out = [0i64; N];
    let mut huge = false;
    for (o, v) in out.iter_mut().zip(s) {
        match py_int(v) {
            PyInt::Val(x) => *o = x,
            PyInt::Huge => huge = true,
            PyInt::Bad => return Err(BadInt),
        }
    }
    Ok((!huge).then_some(out))
}

pub async fn handle(app: &App, method: &Method, path: &str, headers: &HeaderMap) -> Resp {
    let Some(r) = route(path) else { return error(StatusCode::NOT_FOUND) };
    if method != Method::GET && method != Method::HEAD {
        let mut resp = error(StatusCode::METHOD_NOT_ALLOWED);
        resp.headers_mut().insert("allow", HeaderValue::from_static("GET,HEAD"));
        return resp;
    }
    match r {
        Route::Recorded => {
            let radar = app.radar.clone();
            let (frames, flows) = blocking(move || (radar.frames(), radar.flows())).await.unwrap_or_default();
            let list = |v: &[i64]| v.iter().map(i64::to_string).collect::<Vec<_>>().join(", ");
            let body = format!(r#"{{"frames": [{}], "flows": [{}], "zoom": [4, 7]}}"#, list(&frames), list(&flows));
            respond(StatusCode::OK, &[("content-type", "application/json; charset=utf-8".into())], body.into())
        }
        Route::Tile(s) => {
            let p = match ints(s) {
                Err(BadInt) => return error(StatusCode::BAD_REQUEST),
                Ok(None) => None,
                Ok(Some([t, z, x, y])) => {
                    let radar = app.radar.clone();
                    blocking(move || radar.tile(t, z, x, y)).await.flatten()
                }
            };
            match p {
                Some(p) => file_response(p, "image/png", headers).await,
                // outside the recorded block: clear, not an error tile
                None => respond(
                    StatusCode::OK,
                    &[("cache-control", "max-age=300".into()), ("content-type", "image/png".into())],
                    Bytes::from_static(CLEAR_PNG),
                ),
            }
        }
        Route::Flow(t) => {
            let p = match ints([t]) {
                Err(BadInt) => return error(StatusCode::BAD_REQUEST),
                Ok(None) => None,
                Ok(Some([t])) => {
                    let radar = app.radar.clone();
                    blocking(move || radar.flow(t)).await.flatten()
                }
            };
            match p {
                Some(p) => file_response(p, "application/json", headers).await,
                None => error(StatusCode::NOT_FOUND),
            }
        }
        Route::Traffic(kind, style, s) => {
            let Ok(zxy) = ints(s) else { return error(StatusCode::BAD_REQUEST) };
            let (Some(traffic), Some([z, x, y])) = (&app.traffic, zxy) else { return error(StatusCode::NOT_FOUND) };
            if !TrafficTiles::valid(kind, style, z, x, y) {
                return error(StatusCode::NOT_FOUND);
            }
            let (status, body) = traffic.get(kind, style, z, x, y).await;
            if status != 200 {
                return respond(StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY), &[], Bytes::new());
            }
            respond(StatusCode::OK, &[("cache-control", "max-age=240".into()), ("content-type", "image/png".into())], body)
        }
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(f).await.ok()
}

fn respond(status: StatusCode, headers: &[(&'static str, String)], body: Bytes) -> Resp {
    let mut r = Response::new(Full::new(body));
    *r.status_mut() = status;
    for (k, v) in headers {
        if let Ok(v) = HeaderValue::from_str(v) {
            r.headers_mut().insert(*k, v);
        }
    }
    r
}

/// aiohttp's error body: `"<code>: <reason>"`, text/plain.
pub fn error(status: StatusCode) -> Resp {
    let body = format!("{}: {}", status.as_u16(), status.canonical_reason().unwrap_or(""));
    respond(status, &[("content-type", "text/plain; charset=utf-8".into())], body.into())
}

// ── files (aiohttp FileResponse) ─────────────────────────────────────────────────────────────────────────────────

struct FileData {
    body: Vec<u8>,
    mtime_ns: i128,
    mtime: f64,
}

fn read_file(p: &Path) -> std::io::Result<FileData> {
    use std::os::unix::fs::MetadataExt;
    let body = std::fs::read(p)?;
    let m = std::fs::metadata(p)?;
    let mtime_ns = i128::from(m.mtime()) * 1_000_000_000 + i128::from(m.mtime_nsec());
    Ok(FileData { body, mtime_ns, mtime: m.mtime() as f64 + m.mtime_nsec() as f64 / 1e9 })
}

/// The quoted values of an `If-None-Match` list match ours (`*` matches anything; weak tags compare too).
fn etag_matches(header: &str, value: &str) -> bool {
    if header.trim() == "*" {
        return true;
    }
    header.split(',').any(|t| {
        let t = t.trim();
        let t = t.strip_prefix("W/").unwrap_or(t);
        t.strip_prefix('"').and_then(|t| t.strip_suffix('"')) == Some(value)
    })
}

async fn file_response(p: PathBuf, ctype: &'static str, headers: &HeaderMap) -> Resp {
    let Some(Ok(f)) = blocking(move || read_file(&p)).await else {
        return error(StatusCode::NOT_FOUND); // pruned since it was listed
    };
    let etag = format!("{:x}-{:x}", f.mtime_ns, f.body.len());
    let hdrs = [
        ("cache-control", "max-age=86400".to_string()),
        ("content-type", ctype.to_string()),
        ("etag", format!("\"{etag}\"")),
        ("last-modified", http_date(f.mtime.ceil() as i64)),
    ];
    let inm = headers.get("if-none-match").and_then(|v| v.to_str().ok()).filter(|v| !v.is_empty());
    let not_modified = match inm {
        Some(v) => etag_matches(v, &etag),
        None => headers
            .get("if-modified-since")
            .and_then(|v| v.to_str().ok())
            .and_then(parse_http_date)
            .is_some_and(|ims| f.mtime <= ims as f64),
    };
    if not_modified {
        return respond(StatusCode::NOT_MODIFIED, &hdrs, Bytes::new());
    }
    respond(StatusCode::OK, &hdrs, f.body.into())
}

// ── HTTP dates (IMF-fixdate) ─────────────────────────────────────────────────────────────────────────────────────

const WDAY: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"]; // 1970-01-01 was a Thursday
const MON: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// `Mon, 23 Nov 2026 23:52:22 GMT`.
pub fn http_date(t: i64) -> String {
    let (days, s) = (t.div_euclid(86400), t.rem_euclid(86400));
    let (y, m, d) = civil_from_days(days);
    format!(
        "{}, {d:02} {} {y:04} {:02}:{:02}:{:02} GMT",
        WDAY[days.rem_euclid(7) as usize],
        MON[m as usize - 1],
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

/// (year, month, day) → days since 1970-01-01.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// An IMF-fixdate → Unix seconds (other date forms are ignored, like an unparseable header).
pub fn parse_http_date(s: &str) -> Option<i64> {
    let p: Vec<&str> = s.split_whitespace().collect();
    let [_, d, mon, y, hms, "GMT"] = p.as_slice() else { return None };
    let d: u32 = d.parse().ok().filter(|d| (1..=31).contains(d))?;
    let m = MON.iter().position(|x| x == mon)? as u32 + 1;
    let y: i64 = y.parse().ok()?;
    let t: Vec<i64> = hms.split(':').map(|x| x.parse().ok()).collect::<Option<_>>()?;
    let [h, mi, se] = t.as_slice() else { return None };
    if !(0..24).contains(h) || !(0..60).contains(mi) || !(0..=60).contains(se) {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86400 + h * 3600 + mi * 60 + se)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_int() {
        assert_eq!(py_int("12"), PyInt::Val(12));
        assert_eq!(py_int("-5"), PyInt::Val(-5));
        assert_eq!(py_int("+7"), PyInt::Val(7));
        assert_eq!(py_int("1_000"), PyInt::Val(1000));
        assert_eq!(py_int("99999999999999999999"), PyInt::Huge);
        for bad in ["", "x", "1.5", "1__0", "_1", "1_", "0x10", "--1"] {
            assert_eq!(py_int(bad), PyInt::Bad, "{bad}");
        }
    }

    #[test]
    fn dates() {
        assert_eq!(http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(http_date(1_795_996_342), "Sun, 29 Nov 2026 23:52:22 GMT");
        assert_eq!(parse_http_date("Sun, 29 Nov 2026 23:52:22 GMT"), Some(1_795_996_342));
        assert_eq!(parse_http_date("Mon, 01 Jan 2001 00:00:00 GMT"), Some(978_307_200));
        assert_eq!(parse_http_date("garbage"), None);
        for t in [0, 951_782_400, 1_798_761_599, 4_102_444_800] {
            assert_eq!(parse_http_date(&http_date(t)), Some(t));
        }
    }

    #[test]
    fn etags() {
        assert!(etag_matches("\"ab-3\"", "ab-3"));
        assert!(etag_matches("W/\"x\", \"ab-3\"", "ab-3"));
        assert!(etag_matches("*", "ab-3"));
        assert!(!etag_matches("\"x\"", "ab-3"));
    }

    #[test]
    fn routes() {
        assert!(matches!(route("/api/radar/recorded"), Some(Route::Recorded)));
        assert!(route("/api/radar/recorded/").is_none());
        assert!(matches!(route("/api/radar/tile/1/6/10/22.png"), Some(Route::Tile(["1", "6", "10", "22"]))));
        assert!(route("/api/radar/tile/1/6/10/.png").is_none());
        assert!(route("/api/radar/tile/1/6/10/22").is_none());
        assert!(matches!(route("/api/radar/tile/1/6/10/2.x.png"), Some(Route::Tile([_, _, _, "2.x"]))));
        assert!(route("/api/radar/tile//6/10/22.png").is_none());
        assert!(matches!(route("/api/traffic/flow/major/5/1/2.png"), Some(Route::Traffic("flow", "major", _))));
    }
}
