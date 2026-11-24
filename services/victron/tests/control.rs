//! Ported from `tests/test_victron_control.py`: the MultiPlus / MPPT command path against a fake Cerbo that answers
//! `R/` (republishes the current value) and applies `W/` (unless `apply` is off).

use aetherrv_core::cmdlog::CommandLog;
use aetherrv_core::command::ResultKind;
use aetherrv_core::state::{StateStore, StateView};
use aetherrv_core::txgate::TxGate;
use aetherrv_victron::model::{Want, roles};
use aetherrv_victron::{Link, Victron};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

/// Python `FakeCerbo`: answers R/ (republish the current value) and applies W/ (unless apply=False), 10 ms later.
struct FakeCerbo {
    a: Weak<Victron>,
    values: Mutex<HashMap<String, Value>>,
    apply: bool,
    writes: Mutex<Vec<(String, Value)>>,
    raw_writes: Mutex<Vec<Vec<u8>>>,
}

impl FakeCerbo {
    fn later(&self, path: String) {
        let (a, v) = (self.a.clone(), self.values.lock().unwrap().get(&path).cloned());
        if let Some(v) = v {
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                if let Some(a) = a.upgrade() {
                    a.on_control_path(&path, v);
                }
            });
        }
    }

    fn writes(&self) -> Vec<(String, Value)> {
        self.writes.lock().unwrap().clone()
    }
}

impl Link for FakeCerbo {
    fn subscribe(&self, _filter: &str) -> Result<(), String> {
        Ok(())
    }
    fn publish(&self, topic: &str, payload: &[u8]) -> Result<(), String> {
        let mut it = topic.splitn(3, '/');
        let (kind, _, path) = (it.next().unwrap(), it.next().unwrap(), it.next().unwrap().to_string());
        if kind == "W" {
            let v = serde_json::from_slice::<Value>(payload).unwrap()["value"].clone();
            self.writes.lock().unwrap().push((path.clone(), v.clone()));
            self.raw_writes.lock().unwrap().push(payload.to_vec());
            if self.apply {
                self.values.lock().unwrap().insert(path.clone(), v);
                self.later(path);
            }
        } else if kind == "R" && self.values.lock().unwrap().contains_key(&path) {
            self.later(path);
        }
        Ok(())
    }
}

fn base() -> Vec<(&'static str, Value)> {
    vec![
        ("vebus/276/Mode", json!(4)),
        ("vebus/276/ModeIsAdjustable", json!(1)),
        ("vebus/276/Ac/In/1/CurrentLimitIsAdjustable", json!(1)),
        ("vebus/276/Ac/In/1/CurrentLimit", json!(30.0)),
        ("solarcharger/277/Mode", json!(1)),
    ]
}

/// Python `make(values=BASE, apply=True, gate=True)`.
fn make(values: Vec<(&str, Value)>, apply: bool, gate: bool) -> (Arc<Victron>, Arc<FakeCerbo>) {
    let a = Victron::new(
        Arc::new(StateStore::new()),
        TxGate::new([("victron".to_string(), gate)]),
        CommandLog::memory("victron"),
        roles("cabin", Want::Auto, Want::Auto, Want::Auto, Want::Auto),
    );
    a.set_portal("p");
    for (k, v) in &values {
        a.on_control_path(k, v.clone());
    }
    let fake = Arc::new(FakeCerbo {
        a: Arc::downgrade(&a),
        values: Mutex::new(values.into_iter().map(|(k, v)| (k.to_string(), v)).collect()),
        apply,
        writes: Mutex::default(),
        raw_writes: Mutex::default(),
    });
    a.set_link(Some(fake.clone() as Arc<dyn Link>));
    (a, fake)
}

fn outcomes(a: &Victron) -> Vec<String> {
    a.log.rows().iter().map(|r| r.outcome.clone()).collect()
}

/// py: tests/test_victron_control.py::test_inverter_on_then_noop
#[tokio::test]
async fn test_inverter_on_then_noop() {
    let (a, cerbo) = make(base(), true, true);
    let r = a.command_with("inverter_mode", &json!(3), "kiosk", Duration::from_secs(1)).await;
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(cerbo.writes(), vec![("vebus/276/Mode".to_string(), json!(3))]);
    assert_eq!(a.state.get("victron.vebus.mode"), Some(json!(3)));
    let r = a.command("inverter_mode", &json!(3), "kiosk").await;
    assert!(r.is_ok() && r.detail == "already set");
    assert_eq!(cerbo.writes().len(), 1);
    assert_eq!(cerbo.raw_writes.lock().unwrap()[0], br#"{"value": 3}"#);
    assert_eq!(outcomes(&a), ["ok", "noop"]);
    assert_eq!(a.log.rows()[0].detail, "vebus/276/Mode=3 verified");
    assert_eq!(a.log.rows()[1].detail, "already in requested state");
}

/// py: tests/test_victron_control.py::test_unverified_is_sent_once
#[tokio::test]
async fn test_unverified_is_sent_once() {
    let (a, cerbo) = make(base(), false, true);
    let r = a.command_with("mppt_mode", &json!(4), "kiosk", Duration::from_millis(200)).await;
    assert!(!r.is_ok());
    assert_eq!(r.result, ResultKind::Unverified);
    assert_eq!(r.detail, "the Cerbo did not confirm (now 1; not retried)");
    assert_eq!(cerbo.writes(), vec![("solarcharger/277/Mode".to_string(), json!(4))]);
    assert_eq!(outcomes(&a), ["unverified"]);
    assert_eq!(a.log.rows()[0].detail, "solarcharger/277/Mode still 1");
}

/// py: tests/test_victron_control.py::test_guards
#[tokio::test]
async fn test_guards() {
    let mut v = base();
    v[1].1 = json!(0); // ModeIsAdjustable = 0
    let (a, _cerbo) = make(v, true, true);
    assert!(a.command("inverter_mode", &json!(3), "kiosk").await.detail.contains("not adjustable"));
    let (a, cerbo) = make(base(), true, true);
    for (cmd, v) in [
        ("inverter_mode", json!(5)),
        ("shore_limit", json!(80)),
        ("shore_limit", json!(1)),
        ("mppt_mode", json!(2)),
        ("relay", json!(1)),
    ] {
        let r = a.command(cmd, &v, "kiosk").await;
        assert!(!r.is_ok(), "{cmd}={v}");
        assert_eq!(r.result, ResultKind::Refused);
    }
    assert!(cerbo.writes().is_empty());
    assert_eq!(outcomes(&a), ["rejected"; 5]);
    let (a, cerbo) = make(base(), true, false);
    assert_eq!(a.command("inverter_mode", &json!(3), "kiosk").await.detail, "Victron control is disabled");
    assert!(cerbo.writes().is_empty());
    assert_eq!(a.log.rows()[0].outcome, "blocked");
    assert_eq!(a.log.rows()[0].detail, "tx_enabled false");
}

/// py: tests/test_victron_control.py::test_shore_limit_rounding
#[tokio::test]
async fn test_shore_limit_rounding() {
    let (a, cerbo) = make(base(), true, true);
    let r = a.command_with("shore_limit", &json!(15.04), "kiosk", Duration::from_secs(1)).await;
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(cerbo.writes(), vec![("vebus/276/Ac/In/1/CurrentLimit".to_string(), json!(15.0))]);
    assert_eq!(cerbo.raw_writes.lock().unwrap()[0], br#"{"value": 15.0}"#);
    assert_eq!(a.log.rows()[0].detail, "vebus/276/Ac/In/1/CurrentLimit=15.0 verified");
    assert_eq!(a.log.rows()[0].value, "15.04");
}

/// py: tests/test_victron_control.py::test_state_is_json_safe
#[tokio::test]
async fn test_state_is_json_safe() {
    let (a, _) = make(base(), true, true);
    let text = serde_json::to_string(&a.state.values()).unwrap();
    let back: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(back["victron.vebus.current_limit"], json!(30.0));
    assert_eq!(back["victron.vebus.mode_adjustable"], json!(true));
}

/// Values of the wrong JSON kind are refused before anything is sent (Python's `int("3")`, `int(True)` and
/// `int(3.7)` were accepted).
#[tokio::test]
async fn wrong_value_kinds_are_refused() {
    let (a, cerbo) = make(base(), true, true);
    for (cmd, v) in [("inverter_mode", json!("3")), ("inverter_mode", json!(true)), ("inverter_mode", json!(3.7)), ("shore_limit", json!("15"))]
    {
        let r = a.command(cmd, &v, "kiosk").await;
        assert_eq!(r.result, ResultKind::Refused, "{cmd}={v}");
        assert!(r.detail.starts_with("bad request"), "{}", r.detail);
    }
    assert!(cerbo.writes().is_empty());
    assert_eq!(outcomes(&a), ["rejected"; 4]);
}

/// No connection: refused, nothing logged (as Python). No fresh value: refused, `failed` logged, nothing written.
#[tokio::test]
async fn not_connected_and_no_fresh_read() {
    let (a, cerbo) = make(base(), true, true);
    a.set_link(None);
    let r = a.command("inverter_mode", &json!(3), "kiosk").await;
    assert_eq!((r.result, r.detail.as_str()), (ResultKind::Refused, "not connected to the Cerbo"));
    assert!(a.log.rows().is_empty());
    // a Cerbo that never answers the read
    cerbo.values.lock().unwrap().clear();
    a.set_link(Some(cerbo.clone() as Arc<dyn Link>));
    let t0 = tokio::time::Instant::now();
    let r = a.command("inverter_mode", &json!(3), "kiosk").await;
    assert!(t0.elapsed() >= Duration::from_secs(4));
    assert_eq!((r.result, r.detail.as_str()), (ResultKind::Refused, "the Cerbo did not report the current value"));
    assert_eq!(a.log.rows()[0].detail, "no fresh value for vebus/276/Mode");
    assert!(cerbo.writes().is_empty());
}

/// `ack_alarms`: fresh read of both counts → AcknowledgeAll = 1 once → both 0; nothing unacknowledged → noop.
#[tokio::test]
async fn ack_alarms_reads_writes_once_and_verifies() {
    let mut v = base();
    v.push(("platform/0/Notifications/NumberOfUnAcknowledgedAlarms", json!(1)));
    v.push(("platform/0/Notifications/NumberOfUnAcknowledgedWarnings", json!(0)));
    let (a, cerbo) = make(v.clone(), true, true);
    // the fake applies AcknowledgeAll as the Cerbo does: both counts drop to 0
    let r = {
        let (a2, c2) = (a.clone(), cerbo.clone());
        let h = tokio::spawn(async move { a2.command("ack_alarms", &json!(1), "kiosk").await });
        let w = loop {
            tokio::time::sleep(Duration::from_millis(5)).await;
            if let Some(w) = c2.writes().first().cloned() {
                break w;
            }
        };
        assert_eq!(w, ("platform/0/Notifications/AcknowledgeAll".to_string(), json!(1)));
        let p = "platform/0/Notifications/NumberOfUnAcknowledgedAlarms";
        c2.values.lock().unwrap().insert(p.to_string(), json!(0));
        a.on_control_path(p, json!(0));
        h.await.unwrap()
    };
    assert_eq!((r.result, r.detail.as_str()), (ResultKind::Ok, "acknowledged on the Cerbo (buzzer off)"));
    let r = a.command("ack_alarms", &json!(1), "kiosk").await;
    assert_eq!(r.detail, "nothing to acknowledge");
    assert_eq!(cerbo.writes().len(), 1, "AcknowledgeAll once, nothing for the noop");
    assert_eq!(outcomes(&a), ["ok", "noop"]);
    // a Cerbo that ignores the acknowledge: unverified, not retried
    let (a, cerbo) = make(v, false, true);
    let r = a.command("ack_alarms", &json!(1), "kiosk").await;
    assert_eq!((r.result, r.detail.as_str()), (ResultKind::Unverified, "the Cerbo did not confirm (not retried)"));
    assert_eq!(a.log.rows()[0].detail, "counts still [1, 0]");
    assert_eq!(cerbo.writes().len(), 1);
}
