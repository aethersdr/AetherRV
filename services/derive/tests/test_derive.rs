//! The Python cabin-average test (tests/test_core.py), plus the service on a private broker.

use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::config::Cfg;
use aetherrv_core::state::{StateStore, StateView};
use aetherrv_core::testutil::{Broker, eventually};
use aetherrv_derive::{Options, derive_cabin};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

/// py: tests/test_core.py::test_cabin_average_of_front_ruuvi_and_rear_mcs7
#[test]
fn test_cabin_average_of_front_ruuvi_and_rear_mcs7() {
    let s = StateStore::new();
    let now = aetherrv_core::clock::now_s();
    s.update([
        ("climate.cabin_front.temp", json!(21.2)),
        ("climate.cabin_front.humidity", json!(33.3)),
        ("victron.connected", json!(true)),
        ("climate.rixens.cabin_c", json!(15.6)),
        ("climate.rixens.cabin_rh", json!(42.5)),
        ("climate.rixens.live", json!(true)),
        ("climate.rixens.updated", json!(now)),
    ]);
    s.update(derive_cabin(&s, now));
    assert!(s.get("climate.cabin.temp") == Some(json!(18.4)) && s.get("climate.cabin.humidity") == Some(json!(37.9)));
    assert_eq!(s.get("climate.cabin.source"), Some(json!("average")));
    assert_eq!((s.get("climate.cabin.front_temp"), s.get("climate.cabin.rear_temp")), (Some(json!(21.2)), Some(json!(15.6))));
    // MCS7 stale → front alone
    s.update(derive_cabin(&s, now + 120.0));
    assert!(s.get("climate.cabin.temp") == Some(json!(21.2)) && s.get("climate.cabin.source") == Some(json!("front")));
    // Cerbo gone too → nothing
    s.set("victron.connected", json!(false));
    s.update(derive_cabin(&s, now + 120.0));
    assert!(s.get("climate.cabin.temp") == Some(Value::Null) && s.get("climate.cabin.source") == Some(Value::Null));
}

/// The front value's age is the publisher's: a Ruuvi reading older than 15 min is stale even though it was just
/// received over the bus (ARCHITECTURE.md §4.3).
#[test]
fn front_sensor_older_than_15_min_is_not_used() {
    let s = StateStore::new();
    let now = aetherrv_core::clock::now_s();
    s.set_at("climate.cabin_front.temp", json!(21.2), ((now - 16.0 * 60.0) * 1000.0) as i64);
    s.update([("climate.rixens.cabin_c", json!(15.6)), ("climate.rixens.updated", json!(now))]);
    let out: std::collections::HashMap<String, Value> = derive_cabin(&s, now).into_iter().collect();
    assert_eq!((out["climate.cabin.temp"].clone(), out["climate.cabin.source"].clone()), (json!(15.6), json!("rear")));
    assert_eq!(out["climate.cabin.front_temp"], Value::Null);
}

fn publisher(broker: &Broker, name: &str) -> (Arc<StateStore>, Bus) {
    let st = Arc::new(StateStore::new());
    let b = Bus::start(BusOptions::new(name).socket(&broker.socket), st.clone());
    (st, b)
}

/// The service reads the Cerbo, the MCS7 and the N2K alternator over the bus and publishes the derived keys; a
/// down input owner reads as unknown.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_derives_from_the_bus() {
    let broker = Broker::start(false);
    let (vst, _v) = publisher(&broker, "victron");
    let (rst, _r) = publisher(&broker, "rixens");
    let (nst, _n) = publisher(&broker, "n2k");
    let now = aetherrv_core::clock::now_s();
    vst.update([
        ("climate.cabin_front.temp", json!(21.2)),
        ("climate.cabin_front.humidity", json!(33.3)),
        ("victron.connected", json!(true)),
        ("victron.dc.load", json!(-480.0)),
        ("victron.battery.current", json!(30.0)),
        ("victron.battery.voltage", json!(13.6)),
    ]);
    rst.update([
        ("climate.rixens.cabin_c", json!(15.6)),
        ("climate.rixens.cabin_rh", json!(42.5)),
        ("climate.rixens.live", json!(true)),
        ("climate.rixens.updated", json!(now + 30.0)),
    ]);
    nst.update([("alternator.power", json!(540.0)), ("alternator.current", json!(40.0))]);
    let cfg = Cfg::parse("[climate]\ncabin_source = \"average\"\n").unwrap();
    let opts = Options { bus_socket: Some(broker.socket.clone()), energy_period: Duration::from_millis(100), cabin_period: Duration::from_millis(100) };
    let svc = tokio::spawn(aetherrv_derive::run(cfg, opts));
    let (_ust, ui) = publisher(&broker, "gateway");
    ui.subscribe_state("derive", "#").await;
    let view = ui.remote().clone();
    let is = |k: &str, v: Value| view.get(k) == Some(v);
    assert!(eventually(Duration::from_secs(10), || is("climate.cabin.temp", json!(18.4)) && is("climate.cabin.source", json!("average"))).await);
    assert!(eventually(Duration::from_secs(5), || is("energy.alternator.power", json!(540.0)) && is("energy.alternator.source", json!("n2k"))).await);
    assert!(is("energy.dc.load", json!(0.0)), "max(0, (30 - 40) × 13.6)");
    nst.set("alternator.current", json!(20.0));
    assert!(eventually(Duration::from_secs(5), || is("energy.dc.load", json!(136.0))).await);
    svc.abort();
}
