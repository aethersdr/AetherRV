//! Shared by the weather tests: a fake upstream, the Unix-socket server and a client for it, and the synthetic
//! radar frames of the Python flow test.
#![allow(dead_code)]

use aetherrv_weather::fetch::{BoxFuture, Fetch, FetchError, Fetched};
use aetherrv_weather::flow::{Img, NEXRAD, gaussian_blur, ksize_for};
use aetherrv_weather::http::{App, bind, serve};
use http_body_util::{BodyExt, Empty};
use hyper::body::Bytes;
use hyper::header::HeaderMap;
use hyper_util::rt::TokioIo;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

type Answer = Box<dyn Fn(&str) -> Result<Fetched, FetchError> + Send + Sync>;

/// An upstream that answers from a closure and records every URL asked for.
pub struct FakeFetch {
    pub calls: Mutex<Vec<String>>,
    answer: Answer,
}

impl FakeFetch {
    pub fn new(answer: impl Fn(&str) -> Result<Fetched, FetchError> + Send + Sync + 'static) -> Arc<FakeFetch> {
        Arc::new(FakeFetch { calls: Mutex::new(Vec::new()), answer: Box::new(answer) })
    }
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

pub fn ok(body: &[u8]) -> Result<Fetched, FetchError> {
    Ok(Fetched { status: 200, body: Bytes::copy_from_slice(body) })
}

pub fn status(s: u16) -> Result<Fetched, FetchError> {
    Ok(Fetched { status: s, body: Bytes::new() })
}

impl Fetch for FakeFetch {
    fn get(&self, url: &str) -> BoxFuture<Result<Fetched, FetchError>> {
        self.calls.lock().unwrap().push(url.to_string());
        let r = (self.answer)(url);
        Box::pin(async move { r })
    }
}

/// The weather HTTP server on a Unix socket in a temporary directory.
pub struct Server {
    pub sock: PathBuf,
    _dir: tempfile::TempDir,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub fn start(app: App) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("weather.sock");
    let l = bind(&sock).unwrap();
    let task = tokio::spawn(serve(l, Arc::new(app)));
    Server { sock, _dir: dir, task }
}

pub struct Got {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Got {
    pub fn h(&self, k: &str) -> Option<&str> {
        self.headers.get(k).and_then(|v| v.to_str().ok())
    }
}

/// One request over the socket.
pub async fn request(sock: &Path, method: &str, path: &str, headers: &[(&str, &str)]) -> Got {
    let stream = tokio::net::UnixStream::connect(sock).await.unwrap();
    let (mut send, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await.unwrap();
    tokio::spawn(conn);
    let mut rq = hyper::Request::builder().method(method).uri(path).header("host", "weather");
    for (k, v) in headers {
        rq = rq.header(*k, *v);
    }
    let resp = send.send_request(rq.body(Empty::<Bytes>::new()).unwrap()).await.unwrap();
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let body = resp.into_body().collect().await.unwrap().to_bytes().to_vec();
    Got { status, headers, body }
}

pub async fn get(sock: &Path, path: &str) -> Got {
    request(sock, "GET", path, &[]).await
}

// ── synthetic radar (the Python flow test's generator) ──────────────────────────────────────────────────────────

/// SplitMix64: a small deterministic generator (the draws differ from numpy's, the construction is the same).
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// An integer in [lo, hi).
    pub fn integers(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next() % (hi - lo) as u64) as i64
    }
}

pub const SIDE: usize = 1536 + 64;

/// 3×3 tiles of 512 px plus room to shift: 60 blobby "echoes" of varying strength, blurred σ = 6.
pub fn blob_base(seed: u64) -> Img {
    let mut rng = Rng::new(seed);
    let mut base = Img::new(SIDE, SIDE);
    for _ in 0..60 {
        let (cx, cy) = (rng.integers(300, 1300), rng.integers(300, 1300));
        let (r, v) = (rng.integers(15, 60), rng.integers(1, 12) as f32);
        for y in (cy - r).max(0)..=(cy + r).min(SIDE as i64 - 1) {
            for x in (cx - r).max(0)..=(cx + r).min(SIDE as i64 - 1) {
                if (x - cx).pow(2) + (y - cy).pow(2) <= r * r {
                    base.d[y as usize * SIDE + x as usize] = v;
                }
            }
        }
    }
    gaussian_blur(&base, 6.0, ksize_for(6.0, false))
}

/// One RGBA tile as PNG bytes.
pub fn png_rgba(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut e = png::Encoder::new(&mut out, w, h);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        e.write_header().unwrap().write_image_data(rgba).unwrap();
    }
    out
}

/// The 512 px tile (i, j) of the 1536 px window at (x_off, y_off): rank r = clip(rint(v), 0, 15) → NEXRAD colour r,
/// rank 0 transparent.
pub fn tile_png(base: &Img, x_off: usize, y_off: usize, i: usize, j: usize) -> Vec<u8> {
    let mut rgba = vec![0u8; 512 * 512 * 4];
    for y in 0..512 {
        for x in 0..512 {
            let v = base.at(x_off + i * 512 + x, y_off + j * 512 + y);
            let r = v.round_ties_even().clamp(0.0, 15.0) as usize;
            if r >= 1 {
                let c = NEXRAD[r - 1];
                rgba[(y * 512 + x) * 4..][..4].copy_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
    }
    png_rgba(512, 512, &rgba)
}

/// Write the z6 block 6/(10+i)/(21+j).png of the window at (x_off, y_off) into frame dir `d`.
pub fn write_frame(d: &Path, base: &Img, x_off: usize, y_off: usize) {
    for i in 0..3 {
        for j in 0..3 {
            let p = d.join("6").join((10 + i).to_string()).join(format!("{}.png", 21 + j));
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, tile_png(base, x_off, y_off, i, j)).unwrap();
        }
    }
}

/// numpy's median.
pub fn median(v: &[f32]) -> f32 {
    let mut s = v.to_vec();
    s.sort_unstable_by(f32::total_cmp);
    let n = s.len();
    if n % 2 == 1 { s[n / 2] } else { 0.5 * (s[n / 2 - 1] + s[n / 2]) }
}

/// The centre 16×16 of a 32×32 grid (`reshape(32, 32)[8:24, 8:24]`).
pub fn centre(v: &[f32]) -> Vec<f32> {
    (8..24).flat_map(|j| (8..24).map(move |i| v[j * 32 + i])).collect()
}
