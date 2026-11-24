//! The radar recorder (Python `radar.py`): tests/test_core.py's radar tests ported, plus the recorder's poll
//! against a fake LibreWXR (never the internet) and the on-disk layout a Python rollback reads.

mod common;

use aetherrv_core::state::{StateExt, StateStore, StateView};
use aetherrv_weather::fetch::FetchError;
use aetherrv_weather::flow::{flow_pair, NEXRAD};
use aetherrv_weather::http::App;
use aetherrv_weather::radar::{BLOCK, CLEAR_PNG, PollError, Recorder, block};
use common::*;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

fn recorder(root: &Path, input: Arc<StateStore>, fetch: Arc<FakeFetch>) -> (Arc<Recorder>, Arc<StateStore>) {
    let out = Arc::new(StateStore::new());
    (Arc::new(Recorder::new(root, input as Arc<dyn StateView>, out.clone(), fetch)), out)
}

fn at_van() -> Arc<StateStore> {
    let st = Arc::new(StateStore::new());
    st.update([("location.lat", json!(48.26)), ("location.lon", json!(-116.72))]);
    st
}

fn now() -> i64 {
    aetherrv_core::clock::now_s() as i64
}

/// py: tests/test_core.py::test_radar_block_and_prune
#[test]
fn radar_block_and_prune() {
    let b = block(48.26, -116.72, 6, 3); // 3×3 centred on the van's tile
    assert!(b.len() == 9 && b.contains(&(10, 22)));
    assert!(block(48.26, -116.72, 4, 2).len() == 4 && block(89.9, 0.0, 3, 3).len() == 6); // clipped at the pole
    let tmp = tempfile::tempdir().unwrap();
    let (mut rec, _) = recorder(tmp.path(), Arc::new(StateStore::new()), FakeFetch::new(|_| status(500)));
    Arc::get_mut(&mut rec).unwrap().keep_s = 3600.0;
    for t in [1000, 5000, 9000] {
        std::fs::create_dir(tmp.path().join(t.to_string())).unwrap();
        std::fs::write(tmp.path().join(t.to_string()).join("done"), "1").unwrap();
    }
    std::fs::create_dir(tmp.path().join("9600")).unwrap(); // incomplete: not listed
    assert_eq!(rec.frames(), vec![1000, 5000, 9000]);
    rec.prune(9500.0);
    assert_eq!(rec.frames(), vec![9000]);
}

/// The Python block() values, including the antimeridian wrap and half-to-even rounding.
#[test]
fn radar_block_matches_python() {
    assert_eq!(block(48.26, -116.72, 6, 3), vec![(10, 21), (11, 21), (12, 21), (10, 22), (11, 22), (12, 22), (10, 23), (11, 23), (12, 23)]);
    assert_eq!(block(48.26, -116.72, 4, 2), vec![(2, 5), (3, 5), (2, 6), (3, 6)]);
    assert_eq!(block(89.9, 0.0, 3, 3), vec![(3, 0), (4, 0), (5, 0), (3, 1), (4, 1), (5, 1)]);
    assert_eq!(block(-33.9, 179.9, 5, 2), vec![(31, 18), (0, 18), (31, 19), (0, 19)]);
    assert_eq!(block(0.0, -180.0, 4, 3), vec![(15, 7), (0, 7), (1, 7), (15, 8), (0, 8), (1, 8), (15, 9), (0, 9), (1, 9)]);
}

/// py: tests/test_core.py::test_radar_poll_records_gaps_not_newest
#[tokio::test]
async fn radar_poll_records_gaps_not_newest() {
    let tmp = tempfile::tempdir().unwrap();
    let now = now();
    let past = json!([{"time": now - 1200, "path": "/v2/radar/a"}, {"time": now - 600, "path": "/v2/radar/b"},
                      {"time": now, "path": "/v2/radar/c"}]);
    let index = json!({"host": "https://h", "radar": {"past": past, "nowcast": [{"time": now + 600, "path": "/v2/radar/n"}]}});
    let fetch = FakeFetch::new(move |url| {
        if url.ends_with("weather-maps.json") {
            return ok(index.to_string().as_bytes());
        }
        if url.contains("/b/") && url.contains("/7/") { status(404) } else { ok(b"PNG") } // frame b: one zoom fails
    });
    let (rec, out) = recorder(tmp.path(), at_van(), fetch.clone());
    assert_eq!(rec.poll().await, Ok(true));
    let tiles: i64 = BLOCK.iter().map(|(_, n)| n * n).sum();
    assert_eq!(rec.frames(), vec![now - 1200]); // a saved; b incomplete → dropped; c (newest) skipped
    let got = fetch.calls();
    assert!(!got.iter().any(|u| u.contains("/c/") || u.contains("/n/")));
    assert_eq!(got.iter().filter(|u| u.contains("/a/")).count() as i64, tiles);
    assert!(!tmp.path().join((now - 600).to_string()).exists());
    let srv = start(App { radar: rec.clone(), traffic: None });
    let r: Value = serde_json::from_slice(&get(&srv.sock, "/api/radar/recorded").await.body).unwrap();
    assert_eq!(r["frames"], json!([now - 1200]));
    let r = get(&srv.sock, &format!("/api/radar/tile/{}/6/10/22.png", now - 1200)).await;
    assert!(r.status == 200 && r.body == b"PNG");
    let r = get(&srv.sock, &format!("/api/radar/tile/{}/6/0/0.png", now - 1200)).await;
    assert!(r.status == 200 && r.body == CLEAR_PNG);
    // the keys, with Python's types
    assert_eq!(out.get("radar.recorded"), Some(json!(1)));
    assert_eq!(out.get("radar.flows"), Some(json!(0)));
    assert!(out.get("radar.rec_mb_per_day").is_some_and(|v| v.is_f64()));
    // the frame's files, exactly where Python puts them
    let d = tmp.path().join((now - 1200).to_string());
    assert_eq!(std::fs::read_to_string(d.join("done")).unwrap(), tiles.to_string());
    for (z, size) in BLOCK {
        for (x, y) in block(48.26, -116.72, z, size) {
            assert_eq!(std::fs::read(d.join(format!("{z}/{x}/{y}.png"))).unwrap(), b"PNG");
        }
    }
}

#[tokio::test]
async fn radar_waits_for_a_position_and_keeps_the_last_one() {
    let tmp = tempfile::tempdir().unwrap();
    let input = Arc::new(StateStore::new());
    let fetch = FakeFetch::new(|_| status(500));
    let (rec, out) = recorder(tmp.path(), input.clone(), fetch.clone());
    assert_eq!(rec.poll().await, Ok(false)); // no GPS position yet: nothing asked for
    assert!(fetch.calls().is_empty() && out.get("radar.recorded").is_none());
    input.update([("location.lat", json!(48.26)), ("location.lon", json!(-116.72))]);
    assert_eq!(rec.position(), Some((48.26, -116.72)));
    input.remove("location.lat"); // the GPS module went away: the bus reads its keys as unknown
    input.remove("location.lon");
    assert_eq!(rec.position(), Some((48.26, -116.72)));
    assert_eq!(rec.poll().await, Err(PollError("ClientResponseError")));
    assert_eq!(fetch.calls(), vec![aetherrv_weather::radar::INDEX.to_string()]);
}

#[tokio::test]
async fn radar_poll_errors_name_their_class() {
    let tmp = tempfile::tempdir().unwrap();
    for (answer, class) in [
        (Err(FetchError("TimeoutError")), "TimeoutError"),
        (ok(b"not json"), "JSONDecodeError"),
        (ok(br#"{"radar": {"past": []}}"#), "KeyError"),
        (ok(br#"{"host": "https://h"}"#), "KeyError"),
        (ok(br#"{"host": "https://h", "radar": {"past": [{"path": "/x"}, {"time": 1, "path": "/y"}]}}"#), "KeyError"),
    ] {
        let (rec, _) = recorder(tmp.path(), at_van(), FakeFetch::new(move |_| answer.clone()));
        assert_eq!(rec.poll().await, Err(PollError(class)));
    }
    // an empty list and a list of one (the newest, never saved) are fine
    for past in ["[]", r#"[{"time": 5, "path": "/n"}]"#] {
        let body = format!(r#"{{"host": "https://h", "radar": {{"past": {past}}}}}"#);
        let fetch = FakeFetch::new(move |_| ok(body.as_bytes()));
        let (rec, out) = recorder(tmp.path(), at_van(), fetch.clone());
        assert_eq!(rec.poll().await, Ok(true));
        assert_eq!(fetch.calls().len(), 1);
        assert_eq!(out.num("radar.recorded"), Some(0.0));
    }
}

/// Frames a (t0) and b (t0 + 600) of the synthetic radar served by a fake LibreWXR: one poll saves both, writes the
/// motion field a → b with Python's keys and `dt`, and leaves the layout a Python rollback reads. With
/// `AETHERRV_WEATHER_KEEP=<dir>` the cache is copied there (for the Python check).
#[tokio::test]
async fn radar_poll_saves_frames_and_their_motion() {
    let tmp = tempfile::tempdir().unwrap();
    let base = Arc::new(blob_base(1));
    let t0 = now() - 1800;
    let past = json!([{"time": t0, "path": "/v2/radar/a"}, {"time": t0 + 600, "path": "/v2/radar/b"},
                      {"time": t0 + 1200, "path": "/v2/radar/c"}]);
    let index = json!({"host": "https://h", "radar": {"past": past}});
    let tiles: Arc<std::collections::HashMap<String, Vec<u8>>> = Arc::new(
        [("a", 32, 32), ("b", 26, 29)] // B(p) = A(p - (6, 3))
            .iter()
            .flat_map(|&(f, xo, yo)| {
                let base = base.clone();
                (0..9).map(move |k| (format!("/{f}/512/6/{}/{}/", 10 + k % 3, 21 + k / 3), tile_png(&base, xo, yo, k % 3, k / 3)))
            })
            .collect(),
    );
    let other = png_rgba(1, 1, &[0, 0, 0, 0]);
    let fetch = FakeFetch::new(move |url| {
        if url.ends_with("weather-maps.json") {
            return ok(index.to_string().as_bytes());
        }
        match tiles.iter().find(|(k, _)| url.contains(k.as_str())) {
            Some((_, png)) => ok(png),
            None => ok(&other),
        }
    });
    let (rec, out) = recorder(tmp.path(), at_van(), fetch);
    assert_eq!(rec.poll().await, Ok(true));
    assert_eq!(rec.frames(), vec![t0, t0 + 600]);
    assert_eq!(rec.flows(), vec![t0]);
    assert_eq!(out.get("radar.flows"), Some(json!(1)));
    let text = std::fs::read_to_string(rec.flow(t0).unwrap()).unwrap();
    assert!(!text.contains(' ') && text.starts_with(r#"{"z":6,"x0":10,"y0":21,"w":3,"n":32,"dx":["#), "{}", &text[..60]);
    let f: Value = serde_json::from_str(&text).unwrap();
    let keys: Vec<&str> = f.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["z", "x0", "y0", "w", "n", "dx", "dy", "dt"]);
    assert_eq!(f["dt"], json!(600));
    let col = |k: &str| -> Vec<f32> { f[k].as_array().unwrap().iter().map(|v| v.as_f64().unwrap() as f32).collect() };
    let (dx, dy) = (median(&centre(&col("dx"))), median(&centre(&col("dy"))));
    assert!((dx - 6.0).abs() < 1.5 && (dy - 3.0).abs() < 1.5, "{dx} {dy}");
    assert!(!tmp.path().join(t0.to_string()).join("flow.json.tmp").exists());
    // a second poll finds nothing new to do
    assert_eq!(rec.poll().await, Ok(true));
    assert_eq!(std::fs::read_to_string(rec.flow(t0).unwrap()).unwrap(), text);
    if let Some(keep) = std::env::var_os("AETHERRV_WEATHER_KEEP") {
        let st = std::process::Command::new("cp").arg("-a").arg(tmp.path()).arg(&keep).status().unwrap();
        assert!(st.success());
    }
}

/// py: tests/test_core.py::test_radar_flow_pair_finds_known_shift
#[test]
fn radar_flow_pair_finds_known_shift() {
    let tmp = tempfile::tempdir().unwrap();
    let base = blob_base(1);
    write_frame(&tmp.path().join("a"), &base, 32, 32);
    write_frame(&tmp.path().join("b"), &base, 26, 29); // B(p) = A(p - (6, 3)): moved right/down
    let f = flow_pair(&tmp.path().join("a"), &tmp.path().join("b")).unwrap().unwrap();
    let (dx, dy) = (median(&centre(&f.dx)), median(&centre(&f.dy)));
    assert_eq!((f.x0, f.y0, f.w, f.n), (10, 21, 3, 32));
    assert!((dx - 6.0).abs() < 1.5 && (dy - 3.0).abs() < 1.5, "{dx} {dy}");
    let c = tmp.path().join("c").join("6").join("11").join("21.png"); // different block → no flow
    std::fs::create_dir_all(c.parent().unwrap()).unwrap();
    std::fs::write(&c, b"").unwrap();
    assert!(flow_pair(&tmp.path().join("a"), &tmp.path().join("c")).unwrap().is_none());
}

/// What OpenCV's IMREAD_UNCHANGED makes 4 channels is read; the rest is "unreadable tile", like Python.
#[test]
fn radar_tile_colour_types() {
    use aetherrv_weather::flow::tile_ranks;
    fn enc(ct: png::ColorType, depth: png::BitDepth, pal: Option<&[u8]>, trns: Option<&[u8]>, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut e = png::Encoder::new(&mut out, 2, 1);
            e.set_color(ct);
            e.set_depth(depth);
            if let Some(p) = pal {
                e.set_palette(p.to_vec());
            }
            if let Some(t) = trns {
                e.set_trns(t.to_vec());
            }
            e.write_header().unwrap().write_image_data(data).unwrap();
        }
        out
    }
    use png::{BitDepth::*, ColorType::*};
    let red = NEXRAD[9];
    let pal = [red[0], red[1], red[2], 4, 233, 231];
    // palette + tRNS: index 0 transparent, index 1 = NEXRAD[0]
    assert_eq!(tile_ranks(&enc(Indexed, Eight, Some(&pal), Some(&[0, 255]), &[0, 1])).unwrap().2, vec![0, 1]);
    assert_eq!(tile_ranks(&enc(Indexed, Eight, Some(&pal), Some(&[255]), &[0, 1])).unwrap().2, vec![10, 1]);
    assert_eq!(tile_ranks(&enc(Rgba, Eight, None, None, &[253, 0, 0, 255, 9, 9, 9, 0])).unwrap().2, vec![10, 0]);
    assert_eq!(tile_ranks(&enc(Rgb, Eight, None, Some(&[0, 253, 0, 0, 0, 0]), &[253, 0, 0, 1, 1, 1])).unwrap().2, vec![0, 6]);
    assert_eq!(tile_ranks(&enc(GrayscaleAlpha, Eight, None, None, &[253, 255, 0, 0])).unwrap().2, vec![15, 0]);
    for bad in [
        enc(Indexed, Eight, Some(&pal), None, &[0, 1]),
        enc(Rgb, Eight, None, None, &[253, 0, 0, 1, 1, 1]),
        enc(Grayscale, Eight, None, None, &[1, 2]),
        enc(Grayscale, Eight, None, Some(&[0, 1]), &[1, 2]),
        b"not a png".to_vec(),
    ] {
        assert_eq!(tile_ranks(&bad).unwrap_err().to_string(), "unreadable tile");
    }
}

#[test]
fn radar_flow_pair_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    assert!(flow_pair(&a, &b).unwrap().is_none()); // no z6 block at all
    let base = blob_base(2);
    write_frame(&a, &base, 32, 32);
    write_frame(&b, &base, 32, 32);
    std::fs::write(b.join("6/11/22.png"), b"garbage").unwrap();
    assert_eq!(flow_pair(&a, &b).unwrap_err().to_string(), "unreadable tile");
    std::fs::create_dir_all(b.join("6/x")).unwrap();
    assert!(flow_pair(&a, &b).unwrap_err().to_string().starts_with("invalid literal for int()"));
}
