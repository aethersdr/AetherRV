//! The Python history tests (tests/test_core.py), plus the service on a private broker.

use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::config::Cfg;
use aetherrv_core::state::{StateStore, StateView};
use aetherrv_core::testutil::{Broker, eventually};
use aetherrv_history::{History, MEMORY, Options, open_readonly, query};
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

fn now() -> f64 {
    aetherrv_core::clock::now_s()
}

/// py: tests/test_core.py::test_history_samples_numeric_and_downsamples
#[test]
fn test_history_samples_numeric_and_downsamples() {
    let s = StateStore::new();
    let mut h = History::new(MEMORY, 1.0, 30.0);
    let now = now();
    for i in 0..50 {
        s.set("victron.battery.soc", json!(i as f64));
        s.set("victron.pv.power", json!(true)); // bools are not samples
        h.sample(&s, now - 50.0 + i as f64).unwrap();
    }
    assert_eq!(h.get("victron.battery.soc", 100.0, 1000, now).len(), 50);
    assert!(h.get("victron.pv.power", 86400.0, 480, now).is_empty());
    assert!(h.get("not.tracked", 86400.0, 480, now).is_empty());
    let ds = h.get("victron.battery.soc", 60.0, 10, now);
    assert!(ds.len() <= 10);
    let means: Vec<f64> = ds.iter().map(|p| p[1]).collect();
    let mut sorted = means.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(means, sorted, "bucket means stay ordered");
    assert!(ds[0][1] < 5.0 && ds[ds.len() - 1][1] > 45.0);
}

/// py: tests/test_core.py::test_history_persists_and_prunes
#[test]
fn test_history_persists_and_prunes() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("h.sqlite3");
    let db = db.to_str().unwrap();
    let s = StateStore::new();
    let mut h = History::new(db, 30.0, 1.0);
    let now = now();
    s.set("victron.battery.soc", json!(80.0));
    h.sample(&s, now - 2.0 * 86400.0).unwrap(); // older than the 1-day retention
    assert_eq!(h.count(), 1);
    drop(h);

    let mut h2 = History::new(db, 30.0, 1.0);
    assert_eq!(h2.count(), 1, "survived a restart");
    assert_eq!(h2.prune(now).unwrap(), 1, "and ages out");
    let s2 = StateStore::new();
    s2.set("victron.battery.soc", json!(81.0));
    h2.sample(&s2, now - 60.0).unwrap();
    drop(h2);
    let h3 = History::new(db, 30.0, 1.0);
    let pts: Vec<f64> = h3.get("victron.battery.soc", 3600.0, 480, now).iter().map(|p| p[1]).collect();
    assert_eq!(pts, [81.0]);
}

/// py: tests/test_core.py::test_history_falls_back_when_db_unwritable
#[test]
fn test_history_falls_back_when_db_unwritable() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("h.sqlite3");
    std::fs::write(&db, b"").unwrap();
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o444)).unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let r = std::panic::catch_unwind(|| {
        let mut h = History::new(db.to_str().unwrap(), 30.0, 30.0);
        assert!(h.degraded.is_some() && h.path == MEMORY);
        let s = StateStore::new();
        s.set("victron.battery.soc", json!(50.0));
        h.sample(&s, now()).unwrap();
        assert_eq!(h.count(), 1);
    });
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    r.unwrap();
}

/// The gateway's read-only query sees what the service wrote, and can't write.
#[test]
fn readonly_query_for_the_gateway() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("h.sqlite3");
    let s = StateStore::new();
    let mut h = History::new(db.to_str().unwrap(), 30.0, 30.0);
    let now = now();
    for i in 0..4 {
        s.set("wan.down_mbps", json!(10 + i));
        h.sample(&s, now - 120.0 + 30.0 * i as f64).unwrap();
    }
    let ro = open_readonly(&db).unwrap();
    let pts = query(&ro, "wan.down_mbps", 3600.0, 240, now, 30.0).unwrap();
    assert_eq!(pts.iter().map(|p| p[1]).collect::<Vec<_>>(), [10.0, 11.0, 12.0, 13.0]);
    assert_eq!(pts, h.get("wan.down_mbps", 3600.0, 240, now));
    assert!(query(&ro, "nope", 3600.0, 240, now, 30.0).unwrap().is_empty());
    assert!(ro.execute("DELETE FROM samples", []).is_err(), "read-only");
    // the history service restarting: the gateway's charts keep working
    drop(h);
    assert_eq!(query(&ro, "wan.down_mbps", 3600.0, 240, now, 30.0).unwrap().len(), 4);
    let ro2 = open_readonly(&db).unwrap();
    assert_eq!(query(&ro2, "wan.down_mbps", 3600.0, 240, now, 30.0).unwrap().len(), 4);
}

/// The service samples other services' keys over the bus; an unusable file publishes `history.degraded`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_samples_from_the_bus_and_reports_degraded() {
    let broker = Broker::start(false);
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("history.sqlite3");
    let vst = Arc::new(StateStore::new());
    let _vic = Bus::start(BusOptions::new("victron").socket(&broker.socket), vst.clone());
    vst.set("victron.battery.soc", json!(77.0));
    vst.set("victron.pv.power", json!(120));
    let opts = Options { bus_socket: Some(broker.socket.clone()), path: db.to_str().unwrap().into(), interval: 0.2, retention_days: 30.0 };
    let svc = tokio::spawn(aetherrv_history::run(Cfg::empty(), opts));
    let count = || -> i64 {
        open_readonly(&db)
            .and_then(|c| c.query_row("SELECT count(*) FROM samples WHERE key = 'victron.battery.soc' AND v = 77.0", [], |r| r.get(0)))
            .unwrap_or(0)
    };
    assert!(eventually(Duration::from_secs(10), || count() >= 2).await, "samples written from bus values");
    svc.abort();

    // an unusable path: running in memory, and the alert engine can see why
    let bad = Options { bus_socket: Some(broker.socket.clone()), path: "/proc/nope/history.sqlite3".into(), interval: 0.2, retention_days: 30.0 };
    let svc = tokio::spawn(aetherrv_history::run(Cfg::empty(), bad));
    let ust = Arc::new(StateStore::new());
    let ui = Bus::start(BusOptions::new("gateway").socket(&broker.socket), ust);
    ui.subscribe_state("history", "history.degraded").await;
    let view = ui.remote().clone();
    assert!(
        eventually(Duration::from_secs(10), || view.get("history.degraded").and_then(|v| v.as_str().map(str::to_string)).is_some_and(
            |d| d.starts_with("/proc/nope/history.sqlite3: ")
        ))
        .await
    );
    svc.abort();
}

/// The Espar's diagnostics (owner, 2026-11-24): numbers, a field of an object, bools as 1/0, the state and the
/// phase as codes; while the heater sleeps only the state.
#[test]
fn espar_rows_flatten_and_code() {
    let s = StateStore::new();
    s.set("climate.espar.asleep", json!(false));
    s.set("climate.espar.state", json!("heating"));
    s.set("climate.espar.diag.phase", json!({"request": "heat", "phase": "burning"}));
    s.set("climate.espar.diag.power_pct", json!(100.0));
    s.set("climate.espar.diag.glow_w", json!(0.0));
    s.set("climate.espar.diag.blower", json!(4777));
    s.set("climate.espar.diag.flame", json!({"flame": true, "fuel_state": "pumping"}));
    s.set("climate.espar.diag.temps", json!({"flame_c": 167.7, "intake_c": 26.9, "outlet_c": 60.7}));
    s.set("climate.espar.diag.regulation", json!({"setpoint_c": 18.3, "regulated_c": 9.0}));
    s.set("climate.espar.ctl.active", json!(true));
    s.set("climate.espar.diag.ul_per_stroke", json!(null));
    let rows: std::collections::BTreeMap<_, _> = aetherrv_history::espar_rows(&s).into_iter().collect();
    assert_eq!(rows["climate.espar.state"], 2.0);
    assert_eq!(rows["climate.espar.diag.phase.phase"], 20.0); // 0x14 burning
    assert_eq!(rows["climate.espar.diag.power_pct"], 100.0);
    assert_eq!(rows["climate.espar.diag.blower"], 4777.0);
    assert_eq!(rows["climate.espar.diag.flame.flame"], 1.0);
    assert_eq!(rows["climate.espar.diag.temps.flame_c"], 167.7);
    assert_eq!(rows["climate.espar.diag.regulation.regulated_c"], 9.0);
    assert_eq!(rows["climate.espar.ctl.active"], 1.0);
    assert!(!rows.contains_key("climate.espar.diag.pump_hz"), "missing keys are skipped");
    assert_eq!(aetherrv_history::espar_phase_code("0x2a"), Some(42.0));
    assert_eq!(aetherrv_history::espar_phase_code("pre-glow"), Some(7.0));

    s.set("climate.espar.asleep", json!(true));
    s.set("climate.espar.state", json!("off"));
    let asleep = aetherrv_history::espar_rows(&s);
    assert_eq!(asleep, vec![("climate.espar.state", 0.0)], "asleep: the other values are stale, only the state");
}

/// The Espar rows are logged like the chart keys: in the same sample, pruned by the same retention, queryable.
#[test]
fn espar_rows_are_logged_like_the_rest() {
    let s = StateStore::new();
    let mut h = History::new(MEMORY, 30.0, 30.0);
    let now = now();
    s.set("victron.battery.soc", json!(80.0));
    s.set("climate.espar.asleep", json!(false));
    s.set("climate.espar.state", json!("heating"));
    s.set("climate.espar.diag.power_pct", json!(29.2));
    let n = |h: &History, k: &str| -> i64 { h.db().query_row("SELECT count(*) FROM samples WHERE key = ?1", [k], |r| r.get(0)).unwrap() };
    h.sample(&s, now - 31.0 * 86400.0).unwrap(); // older than the 30-day retention
    assert_eq!((n(&h, "victron.battery.soc"), n(&h, "climate.espar.diag.power_pct"), n(&h, "climate.espar.state")), (1, 1, 1));
    s.set("climate.espar.diag.power_pct", json!(100.0));
    h.sample(&s, now - 60.0).unwrap(); // over an hour later: this sample runs the hourly prune
    assert_eq!(
        (n(&h, "victron.battery.soc"), n(&h, "climate.espar.diag.power_pct"), n(&h, "climate.espar.state")),
        (1, 1, 1),
        "the 31-day-old rows aged out together, chart key and Espar rows alike"
    );
    let pts: Vec<f64> = h.get("climate.espar.diag.power_pct", 3600.0, 480, now).iter().map(|p| p[1]).collect();
    assert_eq!(pts, [100.0], "queryable like the chart keys");
    s.set("climate.espar.asleep", json!(true));
    s.set("climate.espar.state", json!("off"));
    h.sample(&s, now).unwrap();
    assert_eq!(n(&h, "climate.espar.diag.power_pct"), 1, "asleep: its stale values aren't logged");
    assert_eq!(n(&h, "climate.espar.state"), 2, "but the state is");
}

/// The service samples the Espar's keys from the bus with the rest.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_samples_the_espar_keys_from_the_bus() {
    let broker = Broker::start(false);
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("history.sqlite3");
    let est = Arc::new(StateStore::new());
    let _esp = Bus::start(BusOptions::new("espar").socket(&broker.socket), est.clone());
    est.set("climate.espar.asleep", json!(false));
    est.set("climate.espar.state", json!("heating"));
    est.set("climate.espar.diag.temps", json!({"flame_c": 150.0, "intake_c": 20.0, "outlet_c": 55.0}));
    let opts = Options { bus_socket: Some(broker.socket.clone()), path: db.to_str().unwrap().into(), interval: 0.5, retention_days: 30.0 };
    let svc = tokio::spawn(aetherrv_history::run(Cfg::empty(), opts));
    let count = || -> i64 {
        open_readonly(&db)
            .and_then(|c| c.query_row("SELECT count(*) FROM samples WHERE key = 'climate.espar.diag.temps.flame_c' AND v = 150.0", [], |r| r.get(0)))
            .unwrap_or(0)
    };
    assert!(eventually(Duration::from_secs(10), || count() >= 2).await, "the flame temperature sampled with the rest");
    svc.abort();
}
