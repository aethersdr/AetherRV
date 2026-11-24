//! `aetherrv-sim-mcs7 [--port N] [--toggle] [--log FILE]`: a Rixens MCS7 local HTTP API on 127.0.0.1, serving the
//! real status.xml fixture (tests/fixtures/mcs7_status_hw1_1.208.xml) and applying `interface.cgi?act=N&val=V` to it
//! like the controller (setpoint, fan, the three sources, hot water, pre-heat, thermostat). Every command is logged
//! (JSON lines), so a test can assert what the MCS7 was sent. Loopback only.

use axum::extract::{Query, State};
use axum::routing::get;
use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex};

const FIXTURE: &str = include_str!("../../../tests/fixtures/mcs7_status_hw1_1.208.xml");

#[derive(Clone)]
struct Sim {
    xml: Arc<Mutex<String>>,
    toggle: bool,
    log: Arc<Mutex<Option<std::fs::File>>>,
}

fn tag_get(x: &str, t: &str) -> Option<String> {
    let (a, b) = (format!("<{t}>"), format!("</{t}>"));
    let i = x.find(&a)? + a.len();
    let j = i + x[i..].find(&b)?;
    Some(x[i..j].to_string())
}

fn tag_set(x: &str, t: &str, v: &str) -> String {
    let (a, b) = (format!("<{t}>"), format!("</{t}>"));
    match (x.find(&a), x.find(&b)) {
        (Some(i), Some(j)) if j > i => format!("{}{v}{}", &x[..i + a.len()], &x[j..]),
        _ => x.to_string(),
    }
}

async fn status(State(s): State<Sim>) -> ([(&'static str, &'static str); 1], String) {
    ([("content-type", "text/xml")], s.xml.lock().unwrap().clone())
}

async fn iface(State(s): State<Sim>, Query(q): Query<HashMap<String, String>>) -> &'static str {
    let act: i64 = q.get("act").and_then(|v| v.parse().ok()).unwrap_or(-1);
    let val: i64 = q.get("val").and_then(|v| v.parse().ok()).unwrap_or(-1);
    if let Some(f) = s.log.lock().unwrap().as_mut() {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
        let _ = writeln!(f, "{{\"t\":{t},\"act\":{act},\"val\":{val}}}");
        let _ = f.flush();
    }
    let mut x = s.xml.lock().unwrap();
    match act {
        1 => *x = tag_set(&x, "setpoint", &val.to_string()),
        2 => *x = tag_set(&x, "fanspeed", &if val == 999 { "Auto".into() } else { val.to_string() }),
        3..=5 => {
            let bit = [1, 2, 4][(act - 3) as usize];
            let hs: i64 = tag_get(&x, "heatsources").and_then(|v| v.parse().ok()).unwrap_or(0);
            let hs = if s.toggle { hs ^ bit } else if val != 0 { hs | bit } else { hs & !bit };
            *x = tag_set(&x, "heatsources", &hs.to_string());
        }
        6 => *x = tag_set(&x, "constantheat", &val.to_string()),
        7 => *x = tag_set(&x, "preheat", &val.to_string()),
        9 => *x = tag_set(&x, "thermenabled", &val.to_string()),
        _ => {}
    }
    "OK"
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let flag = |n: &str| a.iter().position(|x| x == n).and_then(|i| a.get(i + 1)).cloned();
    let port: u16 = flag("--port").and_then(|p| p.parse().ok()).unwrap_or(18111);
    let log = flag("--log").map(|p| std::fs::File::create(p)).transpose()?;
    let s = Sim { xml: Arc::new(Mutex::new(FIXTURE.to_string())), toggle: a.iter().any(|x| x == "--toggle"), log: Arc::new(Mutex::new(log)) };
    let app = axum::Router::new().route("/status.xml", get(status)).route("/interface.cgi", get(iface)).with_state(s);
    let l = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    axum::serve(l, app).await?;
    Ok(())
}
