//! The Unix-socket HTTP side: the traffic proxy test of tests/test_core.py ported, and every route's status codes,
//! bodies and caching headers as aiohttp gave them (captured from the Python app).

mod common;

use aetherrv_core::state::{StateStore, StateView};
use aetherrv_weather::fetch::{Fetch, FetchError, HttpFetch};
use aetherrv_weather::http::App;
use aetherrv_weather::radar::{CLEAR_PNG, Recorder};
use aetherrv_weather::traffic::{MAX_TILES, TrafficTiles};
use common::*;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

fn radar(root: &Path) -> Arc<Recorder> {
    Arc::new(Recorder::new(
        root,
        Arc::new(StateStore::new()) as Arc<dyn StateView>,
        Arc::new(StateStore::new()),
        FakeFetch::new(|_| status(500)),
    ))
}

/// py: tests/test_core.py::test_traffic_tiles_proxy_cache_and_cap
#[tokio::test]
async fn traffic_tiles_proxy_cache_and_cap() {
    let tmp = tempfile::tempdir().unwrap();
    let up = FakeFetch::new(|_| ok(b"PNG"));
    let t = TrafficTiles::new("KEY", 2, 270.0, up.clone());
    assert!(t.url("flow", "major", 5, 1, 2).contains("key=KEY") && t.url("flow", "major", 5, 1, 2).contains("relative-delay"));
    assert!(TrafficTiles::valid("incidents", "s0-dark", 7, 10, 20) && !TrafficTiles::valid("incidents", "s9", 7, 1, 1));
    assert!(!TrafficTiles::valid("flow", "major", 13, 0, 0) && !TrafficTiles::valid("flow", "major", 3, 8, 0));
    let c = start(App { radar: radar(tmp.path()), traffic: Some(Arc::new(t)) });
    let r = get(&c.sock, "/api/traffic/flow/major/5/1/2.png").await;
    assert!(r.status == 200 && r.body == b"PNG" && r.h("content-type") == Some("image/png"));
    get(&c.sock, "/api/traffic/flow/major/5/1/2.png?b=2").await; // cached: no second upstream call
    assert_eq!(up.calls().len(), 1);
    assert_eq!(get(&c.sock, "/api/traffic/incidents/s0/5/1/2.png").await.status, 200);
    assert_eq!(get(&c.sock, "/api/traffic/incidents/s0/5/1/3.png").await.status, 429); // daily cap reached
    assert_eq!(get(&c.sock, "/api/traffic/flow/bogus/5/1/2.png").await.status, 404);
    let c2 = start(App { radar: radar(tmp.path()), traffic: None }); // no key configured
    assert_eq!(get(&c2.sock, "/api/traffic/flow/major/5/1/2.png").await.status, 404);
}

#[tokio::test]
async fn traffic_routes_status_and_headers() {
    let tmp = tempfile::tempdir().unwrap();
    let up = FakeFetch::new(|url| if url.contains("/9.png") { status(500) } else { ok(b"TILE") });
    let t = Arc::new(TrafficTiles::new("KEY", 3, 270.0, up.clone()));
    let c = start(App { radar: radar(tmp.path()), traffic: Some(t.clone()) });
    let r = get(&c.sock, "/api/traffic/flow/major/5/1/2.png").await;
    assert_eq!((r.status, r.h("cache-control"), r.h("content-type")), (200, Some("max-age=240"), Some("image/png")));
    assert_eq!(r.body, b"TILE");
    let r = get(&c.sock, "/api/traffic/flow/major/5/1/9.png").await; // upstream error, nothing cached
    assert_eq!((r.status, r.h("content-type"), r.h("content-length"), r.body.len()), (502, None, Some("0"), 0));
    let r = get(&c.sock, "/api/traffic/flow/major/x/1/2.png").await;
    assert_eq!((r.status, r.body.as_slice()), (400, b"400: Bad Request".as_slice()));
    assert_eq!(r.h("content-type"), Some("text/plain; charset=utf-8"));
    let r = get(&c.sock, "/api/traffic/flow/major/99999999999999999999/1/2.png").await; // a valid int, not a tile
    assert_eq!((r.status, r.body.as_slice()), (404, b"404: Not Found".as_slice()));
    let c2 = start(App { radar: radar(tmp.path()), traffic: None });
    assert_eq!(get(&c2.sock, "/api/traffic/flow/major/x/1/2.png").await.status, 400); // ints first, as Python
    // the key never reaches a browser: not in any answer
    for p in ["/api/traffic/flow/major/5/1/2.png", "/api/traffic/flow/major/5/1/9.png", "/api/traffic/incidents/s0/5/1/4.png"] {
        let r = get(&c.sock, p).await;
        assert!(!String::from_utf8_lossy(&r.body).contains("KEY") && !format!("{:?}", r.headers).contains("KEY"));
    }
    assert!(up.calls().iter().all(|u| u.contains("key=KEY")));
}

#[tokio::test]
async fn traffic_cache_ttl_stale_and_day() {
    let up = FakeFetch::new(|url| if url.contains("/7.png") && url.contains("incidents") { status(503) } else { ok(b"T") });
    let t = TrafficTiles::new("K", 3, 270.0, up.clone());
    let day0 = 1_795_996_342.0 - 1200.0; // 23:32:22 UTC
    assert_eq!(t.get_at("flow", "major", 5, 1, 7, day0).await.0, 200);
    assert_eq!(t.get_at("flow", "major", 5, 1, 7, day0 + 269.0).await.0, 200); // fresh: cached
    assert_eq!(up.calls().len(), 1);
    assert_eq!(t.get_at("flow", "major", 5, 1, 7, day0 + 271.0).await.0, 200); // expired: fetched again
    assert_eq!(up.calls().len(), 2);
    assert_eq!(t.get_at("incidents", "s0", 5, 1, 7, day0).await, (502, Default::default()));
    assert_eq!(t.fetched_today(), 3);
    // cap reached: an old copy beats nothing, otherwise 429
    assert_eq!(t.get_at("flow", "major", 5, 1, 7, day0 + 600.0).await, (200, hyper::body::Bytes::from_static(b"T")));
    assert_eq!(t.get_at("flow", "major", 5, 1, 8, day0 + 600.0).await.0, 429);
    assert_eq!(up.calls().len(), 3);
    // a new UTC day resets the count
    assert_eq!(t.get_at("flow", "major", 5, 1, 8, day0 + 1800.0).await.0, 200);
    assert_eq!(t.fetched_today(), 1);
}

#[tokio::test]
async fn traffic_upstream_error_serves_the_stale_copy() {
    let fail = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let f2 = fail.clone();
    let up = FakeFetch::new(move |_| {
        if f2.load(std::sync::atomic::Ordering::SeqCst) { Err(FetchError("TimeoutError")) } else { ok(b"OLD") }
    });
    let t = TrafficTiles::new("K", 100, 270.0, up);
    assert_eq!(t.get_at("flow", "major", 5, 1, 2, 1000.0).await.0, 200);
    fail.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(t.get_at("flow", "major", 5, 1, 2, 2000.0).await, (200, hyper::body::Bytes::from_static(b"OLD")));
    assert_eq!(t.get_at("flow", "major", 5, 1, 3, 2000.0).await.0, 502);
}

#[tokio::test]
async fn traffic_cache_is_bounded_lru() {
    let up = FakeFetch::new(|_| ok(b"T"));
    let t = TrafficTiles::new("K", 100_000, 270.0, up.clone());
    t.get_at("flow", "major", 12, 0, 0, 0.0).await;
    for i in 1..MAX_TILES as i64 {
        t.get_at("flow", "major", 12, i, 0, 0.0).await;
    }
    t.get_at("flow", "major", 12, 0, 0, 1.0).await; // used again: now the newest
    t.get_at("flow", "major", 12, 0, 1, 1.0).await; // one too many: the oldest (x = 1) goes
    assert_eq!(t.cached(), MAX_TILES);
    let n = up.calls().len();
    t.get_at("flow", "major", 12, 0, 0, 2.0).await;
    assert_eq!(up.calls().len(), n);
    t.get_at("flow", "major", 12, 1, 0, 2.0).await;
    assert_eq!(up.calls().len(), n + 1);
}

/// A real fetch error is reported as a class, never with the URL (which holds the TomTom key). Loopback only.
#[tokio::test]
async fn fetch_errors_never_carry_the_url() {
    let f = HttpFetch::new(None, Duration::from_secs(2)).unwrap();
    let e = f.get("http://127.0.0.1:1/tile.png?key=SECRET").await.unwrap_err();
    assert_eq!(e, FetchError("ClientConnectorError"));
    assert!(!format!("{e} {e:?}").contains("SECRET"));
}

#[tokio::test]
async fn radar_routes_status_and_headers() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("1000");
    std::fs::create_dir_all(d.join("6/10")).unwrap();
    std::fs::write(d.join("6/10/22.png"), b"PNG").unwrap();
    std::fs::write(d.join("done"), "21").unwrap();
    std::fs::write(d.join("flow.json"), r#"{"z":6}"#).unwrap();
    std::fs::create_dir_all(tmp.path().join("2000")).unwrap();
    std::fs::write(tmp.path().join("2000/done"), "21").unwrap();
    std::fs::create_dir_all(tmp.path().join("3000")).unwrap(); // incomplete
    let c = start(App { radar: radar(tmp.path()), traffic: None });
    let s = &c.sock;

    let r = get(s, "/api/radar/recorded").await;
    assert_eq!(r.status, 200);
    assert_eq!(String::from_utf8(r.body.clone()).unwrap(), r#"{"frames": [1000, 2000], "flows": [1000], "zoom": [4, 7]}"#);
    assert_eq!(r.h("content-type"), Some("application/json; charset=utf-8"));
    assert_eq!(r.h("cache-control"), None);

    let r = get(s, "/api/radar/tile/1000/6/10/22.png").await;
    assert_eq!((r.status, r.body.as_slice()), (200, b"PNG".as_slice()));
    assert_eq!((r.h("cache-control"), r.h("content-type"), r.h("content-length")), (Some("max-age=86400"), Some("image/png"), Some("3")));
    let (etag, lm) = (r.h("etag").unwrap().to_string(), r.h("last-modified").unwrap().to_string());
    {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(d.join("6/10/22.png")).unwrap();
        let ns = i128::from(m.mtime()) * 1_000_000_000 + i128::from(m.mtime_nsec());
        assert_eq!(etag, format!("\"{ns:x}-3\"")); // aiohttp: "<st_mtime_ns hex>-<size hex>"
        let ceil = m.mtime() + i64::from(m.mtime_nsec() > 0);
        assert_eq!(lm, aetherrv_weather::http::http_date(ceil)); // aiohttp rounds the mtime up
    }
    for h in [("if-none-match", etag.as_str()), ("if-none-match", "*"), ("if-modified-since", lm.as_str())] {
        let r = request(s, "GET", "/api/radar/tile/1000/6/10/22.png", &[h]).await;
        assert_eq!((r.status, r.body.len()), (304, 0), "{h:?}");
        assert_eq!((r.h("etag"), r.h("cache-control")), (Some(etag.as_str()), Some("max-age=86400")));
    }
    for h in [("if-none-match", "\"x\""), ("if-modified-since", "Mon, 01 Jan 2001 00:00:00 GMT"), ("if-modified-since", "garbage")] {
        assert_eq!(request(s, "GET", "/api/radar/tile/1000/6/10/22.png", &[h]).await.status, 200, "{h:?}");
    }
    // If-None-Match decides alone when both are sent
    let r = request(s, "GET", "/api/radar/tile/1000/6/10/22.png", &[("if-none-match", "\"x\""), ("if-modified-since", &lm)]).await;
    assert_eq!(r.status, 200);

    let r = request(s, "HEAD", "/api/radar/tile/1000/6/10/22.png", &[]).await;
    assert_eq!((r.status, r.h("content-length"), r.body.len()), (200, Some("3"), 0));
    assert_eq!(r.h("cache-control"), Some("max-age=86400"));

    for p in ["/api/radar/tile/1000/6/0/0.png", "/api/radar/tile/5/6/10/22.png", "/api/radar/tile/99999999999999999999/6/10/22.png"] {
        let r = get(s, p).await; // outside the recorded block: clear, not an error tile
        assert_eq!((r.status, r.body.as_slice()), (200, CLEAR_PNG), "{p}");
        assert_eq!((r.h("cache-control"), r.h("content-type")), (Some("max-age=300"), Some("image/png")));
    }
    for p in ["/api/radar/tile/x/6/0/0.png", "/api/radar/tile/1000/6/0/0.x.png", "/api/radar/flow/x", "/api/radar/flow/1.5"] {
        let r = get(s, p).await;
        assert_eq!((r.status, r.body.as_slice()), (400, b"400: Bad Request".as_slice()), "{p}");
        assert_eq!(r.h("content-type"), Some("text/plain; charset=utf-8"));
    }

    let r = get(s, "/api/radar/flow/1000").await;
    assert_eq!((r.status, r.body.as_slice()), (200, br#"{"z":6}"#.as_slice()));
    assert_eq!((r.h("cache-control"), r.h("content-type")), (Some("max-age=86400"), Some("application/json")));
    assert!(r.h("etag").is_some() && r.h("last-modified").is_some());
    for p in ["/api/radar/flow/2000", "/api/radar/flow/99999999999999999999", "/api/radar/nothing", "/api/radar/recorded/", "/"] {
        let r = get(s, p).await;
        assert_eq!((r.status, r.body.as_slice()), (404, b"404: Not Found".as_slice()), "{p}");
        assert_eq!(r.h("content-type"), Some("text/plain; charset=utf-8"));
    }

    let r = request(s, "POST", "/api/radar/recorded", &[]).await;
    assert_eq!((r.status, r.body.as_slice(), r.h("allow")), (405, b"405: Method Not Allowed".as_slice(), Some("GET,HEAD")));
    assert_eq!(request(s, "DELETE", "/api/traffic/flow/major/5/1/2.png", &[]).await.status, 405);
    assert_eq!(request(s, "POST", "/api/nothing", &[]).await.status, 404);
}

#[tokio::test]
async fn serving_without_a_radar_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let c = start(App { radar: radar(&tmp.path().join("missing")), traffic: None });
    let r = get(&c.sock, "/api/radar/recorded").await;
    assert_eq!(String::from_utf8(r.body).unwrap(), r#"{"frames": [], "flows": [], "zoom": [4, 7]}"#);
    assert_eq!(get(&c.sock, "/api/radar/tile/1/6/10/22.png").await.body, CLEAR_PNG);
}

#[tokio::test]
async fn socket_bind_replaces_a_stale_file_and_refuses_a_live_one() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("run/weather.sock");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, b"").unwrap(); // left behind by a crash
    let l = aetherrv_weather::http::bind(&p).unwrap();
    assert!(aetherrv_weather::http::bind(&p).is_err()); // being served
    drop(l);
    assert!(aetherrv_weather::http::bind(&p).is_ok());
}

/// The clear tile is a valid 1×1 fully transparent RGBA PNG.
#[test]
fn clear_png_decodes() {
    let mut d = png::Decoder::new(std::io::Cursor::new(CLEAR_PNG));
    d.set_transformations(png::Transformations::IDENTITY);
    let mut r = d.read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size().unwrap()];
    let fi = r.next_frame(&mut buf).unwrap();
    assert_eq!((fi.width, fi.height, fi.color_type, fi.bit_depth), (1, 1, png::ColorType::Rgba, png::BitDepth::Eight));
    assert_eq!(&buf[..4], &[0, 0, 0, 0]);
}
