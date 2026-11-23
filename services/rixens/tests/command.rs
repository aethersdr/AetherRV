//! The MCS7 command path against a fake MCS7 HTTP server serving the real status.xml fixture
//! (tests/test_rixens_http.py's command tests ported, plus the lockout refusal of test_lowbatt.py).

use aetherrv_core::command::ResultKind;
use aetherrv_core::config::Cfg;
use aetherrv_core::state::StateStore;
use aetherrv_core::txgate::TxGate;
use aetherrv_rixens::Mcs7;
use axum::extract::{Query, State};
use axum::routing::get;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const FIXTURE: &str = include_str!("../../../tests/fixtures/mcs7_status_hw1_1.208.xml");

#[derive(Clone)]
struct Fake {
    xml: Arc<Mutex<String>>,
    toggle: bool,
    apply: bool,
    sent: Arc<Mutex<Vec<(i64, i64)>>>,
}

fn tag_replace(xml: &str, tag: &str, val: &str) -> String {
    let (a, b) = (format!("<{tag}>"), format!("</{tag}>"));
    let i = xml.find(&a).unwrap() + a.len();
    let j = i + xml[i..].find(&b).unwrap();
    format!("{}{val}{}", &xml[..i], &xml[j..])
}

fn tag_get(xml: &str, tag: &str) -> String {
    let (a, b) = (format!("<{tag}>"), format!("</{tag}>"));
    let i = xml.find(&a).unwrap() + a.len();
    let j = i + xml[i..].find(&b).unwrap();
    xml[i..j].to_string()
}

async fn status(State(f): State<Fake>) -> ([(&'static str, &'static str); 1], String) {
    ([("content-type", "text/xml")], f.xml.lock().unwrap().clone())
}

async fn iface(State(f): State<Fake>, Query(q): Query<HashMap<String, String>>) -> &'static str {
    let (act, val): (i64, i64) = (q["act"].parse().unwrap(), q["val"].parse().unwrap());
    f.sent.lock().unwrap().push((act, val));
    if f.apply {
        let mut x = f.xml.lock().unwrap();
        match act {
            1 => *x = tag_replace(&x, "setpoint", &val.to_string()),
            2 => *x = tag_replace(&x, "fanspeed", &if val == 999 { "Auto".to_string() } else { val.to_string() }),
            3..=5 => {
                let bit = match act {
                    3 => 1,
                    4 => 2,
                    _ => 4,
                };
                let hs: i64 = tag_get(&x, "heatsources").parse().unwrap();
                let hs = if f.toggle { hs ^ bit } else if val != 0 { hs | bit } else { hs & !bit };
                *x = tag_replace(&x, "heatsources", &hs.to_string());
            }
            _ => {}
        }
    }
    "OK"
}

async fn make(toggle: bool, apply: bool, enabled: bool) -> (Arc<Mcs7>, Fake) {
    let f = Fake { xml: Arc::new(Mutex::new(FIXTURE.to_string())), toggle, apply, sent: Arc::new(Mutex::new(vec![])) };
    let app = axum::Router::new().route("/status.xml", get(status)).route("/interface.cgi", get(iface)).with_state(f.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    let cfg = Cfg(json!({"device": {"rixens_http": {"kind": "rixens_http", "enabled": true, "host": addr.to_string(), "min_gap_s": 0.01}}}));
    let gate = TxGate::new([("rixens_http".to_string(), enabled)]);
    (Mcs7::new(&cfg, Arc::new(StateStore::new()), gate, None), f)
}

fn sources(m: &Mcs7) -> Vec<&'static str> {
    m.last.lock().unwrap().as_ref().unwrap().sources.clone()
}

async fn heat_source_sends_once_and_verifies(toggle: bool) {
    let (m, f) = make(toggle, true, true).await;
    let r = m.command("electric", &json!(true), "kiosk", 5.0).await;
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(*f.sent.lock().unwrap(), vec![(4, 1)]);
    assert!(sources(&m).contains(&"electric"));
    let r = m.command("electric", &json!(true), "kiosk", 5.0).await; // already on → nothing sent (toggle-safe)
    assert!(r.is_ok() && *f.sent.lock().unwrap() == vec![(4, 1)]);
    let r = m.command("furnace", &json!(false), "kiosk", 5.0).await;
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(*f.sent.lock().unwrap().last().unwrap(), (5, 0));
    assert!(!sources(&m).contains(&"furnace"));
}

/// py: tests/test_rixens_http.py::test_heat_source_sends_once_and_verifies[set]
#[tokio::test]
async fn test_heat_source_sends_once_and_verifies_set() {
    heat_source_sends_once_and_verifies(false).await;
}

/// py: tests/test_rixens_http.py::test_heat_source_sends_once_and_verifies[toggle]
#[tokio::test]
async fn test_heat_source_sends_once_and_verifies_toggle() {
    heat_source_sends_once_and_verifies(true).await;
}

/// py: tests/test_rixens_http.py::test_unverified_is_not_retried
#[tokio::test]
async fn test_unverified_is_not_retried() {
    let (m, f) = make(false, false, true).await; // the controller ignores/clamps the request
    let r = m.command("setpoint", &json!(20.0), "kiosk", 0.2).await;
    assert_eq!(r.result, ResultKind::Unverified);
    assert_eq!(*f.sent.lock().unwrap(), vec![(1, 200)], "exactly one send");
}

/// py: tests/test_rixens_http.py::test_gate_blocks_without_sending
#[tokio::test]
async fn test_gate_blocks_without_sending() {
    let (m, f) = make(false, true, false).await;
    let r = m.command("setpoint", &json!(20.0), "kiosk", 5.0).await;
    assert!(!r.is_ok() && f.sent.lock().unwrap().is_empty());
}

/// py: tests/test_rixens_http.py::test_rejected_command_never_sent
#[tokio::test]
async fn test_rejected_command_never_sent() {
    let (m, f) = make(false, true, true).await;
    let r = m.command("setpoint", &json!(45.0), "kiosk", 5.0).await;
    assert!(!r.is_ok() && r.detail.contains("outside") && f.sent.lock().unwrap().is_empty(), "{r:?}");
}

/// py: tests/test_lowbatt.py::test_heat_starts_refused_during_lockout (the MCS7 half)
#[tokio::test]
async fn test_heat_starts_refused_during_lockout() {
    let (m, f) = make(false, true, true).await;
    let view = Arc::new(StateStore::new());
    view.set("power.heat_lockout", json!(true));
    *m.view.lock().unwrap() = Some(view);
    let r = m.command("furnace", &json!(true), "kiosk", 5.0).await;
    assert!(!r.is_ok() && r.detail.contains("locked out") && f.sent.lock().unwrap().is_empty(), "{r:?}");
    // turning heat OFF is never locked out
    let r = m.command("furnace", &json!(false), "kiosk", 5.0).await;
    assert!(r.is_ok(), "{r:?}");
}

/// D11: on/off values must be JSON booleans (Python's bool("false") is True); nothing sent otherwise.
#[tokio::test]
async fn non_boolean_on_off_is_refused() {
    let (m, f) = make(false, true, true).await;
    for v in [json!("false"), json!(0), json!("0"), Value::Null] {
        let r = m.command("electric", &v, "kiosk", 5.0).await;
        assert!(!r.is_ok() && r.detail == "bad request", "{v}: {r:?}");
    }
    let r = m.command("setpoint", &json!("20"), "kiosk", 5.0).await;
    assert!(!r.is_ok());
    assert!(f.sent.lock().unwrap().is_empty());
}
