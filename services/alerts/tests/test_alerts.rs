//! The Python alert tests (tests/test_alerts.py), plus the alert-rule halves of tests ported elsewhere
//! (tests/test_espar.py, tests/test_lowbatt.py, tests/test_connectivity.py).

use aetherrv_alerts::rules::*;
use aetherrv_alerts::{AlertCfg, Alerts, Ctx, Finding, Rule, RuleError, RuleResult};
use aetherrv_core::state::{StateStore, StateView};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

fn mk(rules: Vec<Rule>, kv: &[(&str, Value)]) -> (Arc<StateStore>, Alerts) {
    let s = Arc::new(StateStore::new());
    for (k, v) in kv {
        s.set(k, v.clone());
    }
    let al = Alerts::new(s.clone(), None, AlertCfg::default(), Some(rules));
    (s, al)
}

fn state(kv: &[(&str, Value)]) -> StateStore {
    let s = StateStore::new();
    for (k, v) in kv {
        s.set(k, v.clone());
    }
    s
}

fn rows(al: &Alerts) -> Vec<Value> {
    al.snapshot().as_array().unwrap().clone()
}

fn ids(al: &Alerts) -> Vec<String> {
    rows(al).iter().map(|r| r["id"].as_str().unwrap().to_string()).collect()
}

fn sev_map(f: &[Finding]) -> BTreeMap<String, String> {
    f.iter().map(|x| (x.id.clone(), x.severity.as_str().to_string())).collect()
}

fn now() -> f64 {
    aetherrv_core::clock::now_s()
}

/// py: tests/test_alerts.py::test_debounce_raise_and_clear
#[test]
fn test_debounce_raise_and_clear() {
    let (s, mut al) = mk(vec![Rule::new("battery", r_battery, 30.0, 60.0)], &[("victron.battery.soc", json!(15.0))]);
    al.evaluate(0.0);
    assert!(rows(&al).is_empty(), "not yet: needs 30 s");
    al.evaluate(31.0);
    let r = rows(&al);
    assert_eq!(r.len(), 1);
    assert!(r[0]["id"] == "battery.soc" && r[0]["severity"] == "warning");
    s.set("victron.battery.soc", json!(50.0));
    al.evaluate(40.0);
    assert_eq!(rows(&al).len(), 1, "still held during clear_s");
    al.evaluate(101.0);
    assert!(rows(&al).is_empty());
}

/// py: tests/test_alerts.py::test_blip_shorter_than_for_s_never_raises
#[test]
fn test_blip_shorter_than_for_s_never_raises() {
    let (s, mut al) = mk(
        vec![Rule::new("cold", r_cold, 600.0, 120.0)],
        &[("climate.fridge.temp", json!(7.0)), ("ui.fridge_max_c", json!(5.0))],
    );
    al.evaluate(0.0);
    s.set("climate.fridge.temp", json!(4.0)); // door closed again
    al.evaluate(300.0);
    s.set("climate.fridge.temp", json!(7.0));
    al.evaluate(400.0);
    al.evaluate(900.0); // only 500 s since it went warm again
    assert!(rows(&al).is_empty());
    al.evaluate(1001.0);
    assert_eq!(rows(&al)[0]["id"], "cold.fridge");
}

/// py: tests/test_alerts.py::test_escalation_unacks_and_ack
#[test]
fn test_escalation_unacks_and_ack() {
    let (s, mut al) = mk(vec![Rule::new("battery", r_battery, 0.0, 0.0)], &[("victron.battery.soc", json!(18.0))]);
    al.evaluate(0.0);
    assert!(al.ack("battery.soc", now()) && rows(&al)[0]["acked"] == true);
    s.set("victron.battery.soc", json!(8.0));
    al.evaluate(1.0);
    let a = &rows(&al)[0];
    assert!(a["severity"] == "critical" && a["acked"] == false);
}

/// py: tests/test_alerts.py::test_per_device_offline_alerts_and_sorting
#[test]
fn test_per_device_offline_alerts_and_sorting() {
    let (s, mut al) = mk(
        vec![Rule::new("n2k", r_n2k_devices, 0.0, 0.0), Rule::new("battery", r_battery, 0.0, 0.0)],
        &[("victron.battery.soc", json!(5.0))],
    );
    s.set(
        "n2k.devices",
        json!([{"source": 16, "model": "IPG100", "online": false, "age": 120},
               {"source": 66, "model": "DCR100", "online": true, "age": 1}]),
    );
    al.evaluate(0.0);
    assert_eq!(ids(&al), ["battery.soc", "n2k.offline.16"], "critical first");
    s.set("n2k.devices", json!([{"source": 16, "model": "IPG100", "online": true, "age": 1}]));
    al.evaluate(1.0);
    assert_eq!(ids(&al), ["battery.soc"]);
}

fn boom(_s: &dyn StateView, _c: &Ctx) -> RuleResult {
    Err(RuleError("RuntimeError: x".into()))
}

fn boom_panics(_s: &dyn StateView, _c: &Ctx) -> RuleResult {
    panic!("x")
}

/// py: tests/test_alerts.py::test_broken_rule_is_isolated (a rule that raises; in Rust also one that panics)
#[test]
fn test_broken_rule_is_isolated() {
    let (_s, mut al) = mk(
        vec![Rule::new("boom", boom, 0.0, 5.0), Rule::new("panics", boom_panics, 0.0, 5.0), Rule::new("battery", r_battery, 0.0, 5.0)],
        &[("victron.battery.soc", json!(5.0))],
    );
    al.evaluate(0.0);
    assert_eq!(rows(&al)[0]["id"], "battery.soc");
    al.evaluate(1.0); // and the engine keeps working after a panic
    assert_eq!(ids(&al), ["battery.soc"]);
}

/// py: tests/test_alerts.py::test_log_persists
#[test]
fn test_log_persists() {
    let dir = tempfile::tempdir().unwrap();
    let db = rusqlite::Connection::open(dir.path().join("a.sqlite3")).unwrap();
    let s = Arc::new(StateStore::new());
    s.set("victron.battery.soc", json!(5.0));
    let mut al = Alerts::new(s.clone(), Some(db), AlertCfg::default(), Some(vec![Rule::new("battery", r_battery, 0.0, 0.0)]));
    al.evaluate(0.0);
    al.ack("battery.soc", 1.0);
    s.set("victron.battery.soc", json!(90.0));
    al.evaluate(2.0);
    let ev: Vec<Value> = al.history(50).as_array().unwrap().iter().map(|h| h["event"].clone()).collect();
    assert_eq!(ev, [json!("cleared"), json!("acked"), json!("raised")]);
}

/// py: tests/test_alerts.py::test_default_rules_cover_all_sources
#[test]
fn test_default_rules_cover_all_sources() {
    let names: BTreeSet<String> = default_rules(&AlertCfg::default()).into_iter().map(|r| r.name).collect();
    let want: BTreeSet<String> = [
        "battery", "cold", "n2k", "buses", "victron", "rixens", "starlink", "peplink", "netmon", "system", "bms",
        "switch_trip", "cerbo", "trigger", "epoch", "espar", "power_shutdown",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(names, want);
}

/// py: tests/test_alerts.py::test_connectivity_rules
#[test]
fn test_connectivity_rules() {
    let s = StateStore::new();
    let c = Ctx::default(); // DEFAULTS
    assert!(r_starlink(&s, &c).unwrap().is_empty() && r_peplink(&s, &c).unwrap().is_empty(), "not enabled → silent");
    s.set("starlink.reachable", json!(true));
    s.set("starlink.online", json!(false));
    s.set("starlink.state", json!("OBSTRUCTED"));
    s.set(
        "starlink.alerts",
        json!([{"id": "alert_motors_stuck", "label": "Motors stuck", "severity": "serious"},
               {"id": "alert_is_heating", "label": "Is heating", "severity": null}]),
    );
    let m = sev_map(&r_starlink(&s, &c).unwrap());
    assert_eq!(m, BTreeMap::from([("starlink.offline".into(), "warning".into()), ("starlink.alert_motors_stuck".into(), "serious".into())]));
    s.set("peplink.reachable", json!(true));
    s.set(
        "peplink.allowance",
        json!([{"conn": 2, "sim": 1, "percent": 96, "usage_mb": 48000, "limit_mb": 50000},
               {"conn": 2, "sim": 2, "percent": 81, "usage_mb": 1, "limit_mb": 1}]),
    );
    let sev: Vec<&str> = r_peplink(&s, &c).unwrap().iter().map(|f| f.severity.as_str()).collect();
    assert_eq!(sev, ["serious", "warning"]);
}

/// py: tests/test_alerts.py::test_bms_rules
#[test]
fn test_bms_rules() {
    let s = StateStore::new();
    s.set(
        "victron.batteries",
        json!([
            {"instance": "0", "kind": "monitor", "ProductName": "Lynx Shunt", "active_alarms": {}},
            {"instance": "512", "kind": "bms", "CustomName": "Epoch 460", "System/NrOfModulesOnline": 1,
             "System/NrOfModulesBlockingCharge": 0, "System/NrOfModulesBlockingDischarge": 1,
             "active_alarms": {"CellImbalance": 1, "HighTemperature": 2}}]),
    );
    let c4 = Ctx::with_cfg(AlertCfg { bms_expected_modules: 4.0, ..AlertCfg::default() });
    let f: BTreeMap<String, (String, String)> =
        r_bms(&s, &c4).unwrap().into_iter().map(|x| (x.id, (x.severity.as_str().to_string(), x.title))).collect();
    assert_eq!(f["bms.modules.512"], ("serious".to_string(), "Only 1 of 4 batteries reporting".to_string()));
    assert!(f["bms.alarm.512.CellImbalance"].0 == "warning" && f["bms.alarm.512.HighTemperature"].0 == "serious");
    assert!(f["bms.block.512.discharging"].0 == "serious" && !f.contains_key("bms.block.512.charging"));
    let c0 = Ctx::with_cfg(AlertCfg { bms_expected_modules: 0.0, ..AlertCfg::default() });
    assert!(!r_bms(&s, &c0).unwrap().iter().any(|x| x.id == "bms.modules.512"));
}

/// py: tests/test_alerts.py::test_switch_trip_and_cerbo_notification_alerts
#[test]
fn test_switch_trip_and_cerbo_notification_alerts() {
    let s = StateStore::new();
    let c = Ctx::default();
    s.set(
        "switches.list",
        json!([{"bank": 32, "channel": 4, "name": "Reverse lights", "state": "error"},
               {"bank": 32, "channel": 1, "name": "Spot", "state": "on"}]),
    );
    let t: Vec<(String, &str)> = r_switch_trip(&s, &c).unwrap().into_iter().map(|f| (f.id, f.severity.as_str())).collect();
    assert_eq!(t, [("switch.trip.32.4".to_string(), "serious")]);
    s.set(
        "victron.notifications",
        json!([{"index": 0, "active": true, "type": 1, "description": "Breaker Tripped", "device": "CLMD16", "acknowledged": false},
               {"index": 1, "active": true, "type": 2, "description": "info only", "device": "", "acknowledged": false}]),
    );
    let f: Vec<(String, &str, String)> =
        r_cerbo_notifications(&s, &c).unwrap().into_iter().map(|x| (x.id, x.severity.as_str(), x.title)).collect();
    assert_eq!(f, [("cerbo.notif.0".to_string(), "serious", "Cerbo: Breaker Tripped (CLMD16)".to_string())]);
}

/// py: tests/test_alerts.py::test_epoch_alerts_and_bms_count_downgrade
#[test]
fn test_epoch_alerts_and_bms_count_downgrade() {
    let s = StateStore::new();
    s.set(
        "batteries.ble",
        json!([
            {"name": "296", "ok": true, "problem_code": 0, "charge_fet": true, "discharge_fet": true, "cell_delta_mv": 4, "temps": [12]},
            {"name": "337", "ok": true, "problem_code": 0, "charge_fet": false, "discharge_fet": true, "cell_delta_mv": 80, "temps": [12]},
            {"name": "354", "ok": false, "age": 900, "error": "TimeoutError"}]),
    );
    let m = sev_map(&r_epoch(&s, &Ctx::default()).unwrap());
    assert_eq!(
        m,
        BTreeMap::from([
            ("epoch.337.charge_fet".into(), "serious".into()),
            ("epoch.337.cells".into(), "warning".into()),
            ("epoch.354.silent".into(), "warning".into()),
        ])
    );
    s.set("victron.batteries", json!([{"kind": "bms", "instance": 512, "System/NrOfModulesOnline": 1}]));
    s.set("batteries.ble.reporting", json!(4)); // read over Bluetooth: the BMS-Can count is no fault
    let c4 = Ctx::with_cfg(AlertCfg { bms_expected_modules: 4.0, ..AlertCfg::default() });
    assert!(r_bms(&s, &c4).unwrap().iter().all(|x| !x.id.starts_with("bms.modules")));
}

/// py: tests/test_alerts.py::test_epoch_cold_cells
#[test]
fn test_epoch_cold_cells() {
    let s = StateStore::new();
    s.set(
        "batteries.ble",
        json!([
            {"name": "296", "ok": true, "temps": [-1.0, 3.0, 20.0, 25.0]},  // one cell probe below 32 °F
            {"name": "337", "ok": true, "temps": [0.5, 1.0, -5.0, -5.0]},   // cold ambient/MOS only: no alert
            {"name": "354", "ok": true, "temps": [null, 2.0]}]),
    );
    let f = r_epoch(&s, &Ctx::with_cfg(AlertCfg { batt_cold_c: 0.0, ..AlertCfg::default() })).unwrap();
    assert_eq!(f.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), ["epoch.296.cold"]);
    assert!(f[0].severity.as_str() == "warning" && f[0].title.contains("30 °F"));
}

/// py: tests/test_alerts.py::test_stored_faults_alert_and_ack_survives_restart (the restart is a new connection to
/// the same database file)
#[test]
fn test_stored_faults_alert_and_ack_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("h.sqlite3");
    let st = Arc::new(state(&[
        (
            "climate.espar.diag.dtcs",
            json!([{"code": "P000129", "text": "flame cut-out at 75-100 % power", "active": false, "severity": 1, "lockout": false}]),
        ),
        ("climate.rixens.fault_history", json!([17, 17])),
        ("climate.rixens.fault_active", json!(null)),
    ]));
    let mut al = Alerts::new(st.clone(), Some(rusqlite::Connection::open(&path).unwrap()), AlertCfg::default(), None);
    al.evaluate(0.0);
    al.evaluate(100.0);
    let by_id = |al: &Alerts| -> BTreeMap<String, Value> {
        rows(al).into_iter().map(|r| (r["id"].as_str().unwrap().to_string(), r)).collect()
    };
    let ids1 = by_id(&al);
    assert!(ids1.contains_key("espar.stored.P000129") && ids1.contains_key("rixens.stored.17"));
    assert!(ids1["espar.stored.P000129"]["severity"] == "warning" && ids1["espar.stored.P000129"]["acked"] == false);
    assert!(al.ack("espar.stored.P000129", now()));
    drop(al);
    // restart: the stored fault is still there
    let mut al2 = Alerts::new(st.clone(), Some(rusqlite::Connection::open(&path).unwrap()), AlertCfg::default(), None);
    al2.evaluate(200.0);
    al2.evaluate(300.0);
    let ids2 = by_id(&al2);
    assert!(ids2["espar.stored.P000129"]["acked"] == true && ids2["rixens.stored.17"]["acked"] == false);
}

// ── rule halves of tests ported elsewhere ────────────────────────────────────────────────────────────────────────

fn espar_frame(id: u32, hex: &str) -> proto_espar::Frame {
    proto_espar::Frame::hex(id, hex)
}

/// py: tests/test_espar.py::test_espar_fault_alert (the alert-rule half; the decoder half is in
/// crates/proto-espar/tests/espar.rs)
#[test]
fn test_espar_fault_alert() {
    let st = StateStore::new();
    let mut a = proto_espar::decoder::Decoder::new("espar", 5.0, false);
    a.on_frame(&espar_frame(0x054, "09050B01DA020000"), 50.0);
    a.on_frame(&espar_frame(0x2C4, "0B082000DA020000"), 50.0); // fault: flame loss
    st.update(a.snapshot(50.0));
    assert!(st.get("climate.espar.state") == Some(json!("error")) && st.get("climate.espar.fault") == Some(json!("0x20")));
    let f = r_espar(&st, &Ctx::default()).unwrap();
    assert_eq!(f.len(), 1);
    assert!(f[0].severity.as_str() == "serious" && f[0].detail.contains("flame loss"));
}

/// py: tests/test_espar.py::test_espar_no_heat_and_power_up_lock_and_lockout_alerts (the alert-rule half)
#[test]
fn test_espar_no_heat_and_power_up_lock_and_lockout_alerts() {
    let st = StateStore::new();
    let mut now = 10.0;
    let mut a = proto_espar::decoder::Decoder::new("espar", 5.0, false);
    a.on_frame(&espar_frame(0x054, "09050B01DA020000"), now);
    a.on_frame(&espar_frame(0x2C4, "030004001B020000"), now); // HEAT, but idle + 0x04
    now += 31.0;
    a.on_frame(&espar_frame(0x054, "09050B01DA020000"), now);
    a.on_frame(&espar_frame(0x2C4, "030004001B020000"), now);
    st.update(a.snapshot(now));
    assert!(st.get("climate.espar.issue").unwrap().as_str().unwrap().contains("power-up lock"));
    a.dtcs = Some(vec![json!({"code": "P00012B", "text": "lockout", "active": true, "severity": 1, "lockout": true})]);
    st.update(a.diag_snapshot(now, false));
    let m = sev_map(&r_espar(&st, &Ctx::default()).unwrap());
    assert!(m["espar.lockout"] == "serious" && m.contains_key("espar.issue"));
}

fn sevs(f: &[Finding]) -> Vec<&'static str> {
    f.iter().map(|x| x.severity.as_str()).collect()
}

/// py: tests/test_lowbatt.py::test_powers_off_after_hold_and_only_once (the alert rule; the guard itself is
/// ported in services/power-guard). The state is what the guard publishes 60 s into the 120 s hold.
#[test]
fn test_powers_off_after_hold_and_only_once() {
    let s = state(&[
        ("power.heat_lockout", json!(false)),
        (
            "power.shutdown",
            json!({"soc": 4.5, "source": "victron", "charging": false, "warn_soc": 8.0, "shutdown_soc": 5.0,
                   "warning": true, "in_s": 60}),
        ),
    ]);
    let f = r_power_shutdown(&s, &Ctx::default()).unwrap();
    assert_eq!(sevs(&f), ["critical"]);
    assert_eq!(f[0].title, "Console shutting down in 60 s");
    assert!(f[0].detail.starts_with("House battery at 4% and not charging. AetherRV powers the Pi off cleanly at 5%"));
}

/// py: tests/test_lowbatt.py::test_bluetooth_fallback_and_warning (the alert rule)
#[test]
fn test_bluetooth_fallback_and_warning() {
    let s = state(&[
        ("power.heat_lockout", json!(false)),
        (
            "power.shutdown",
            json!({"soc": 7.5, "source": "batteries", "charging": false, "warn_soc": 8.0, "shutdown_soc": 5.0,
                   "warning": true, "in_s": null}),
        ),
    ]);
    let f = r_power_shutdown(&s, &Ctx::default()).unwrap();
    assert_eq!(sevs(&f), ["serious"]);
    assert_eq!((f[0].title.as_str(), f[0].detail.as_str()), ("Console will shut down at 5%", "House battery at 8% and not charging."));
}

/// py: tests/test_lowbatt.py::test_heat_lockout_turns_both_off_and_keeps_them_off_until_charging (the alert rule)
#[test]
fn test_heat_lockout_turns_both_off_and_keeps_them_off_until_charging() {
    let shutdown = json!({"soc": 9.5, "source": "victron", "charging": false, "warn_soc": 8.0, "shutdown_soc": 5.0,
                          "warning": false, "in_s": null});
    let s = state(&[
        ("power.heat_lockout", json!(true)),
        ("power.heaters_off", json!({"t": 121, "soc": 9.5, "done": ["Espar off: ok", "MCS7 furnace off: ok", "MCS7 hot_water off: ok"]})),
        ("power.shutdown", shutdown.clone()),
    ]);
    let f = r_power_shutdown(&s, &Ctx::default()).unwrap();
    let ids: Vec<&str> = f.iter().map(|x| x.id.as_str()).collect();
    assert!(ids.contains(&"power.heat_lockout") && ids.contains(&"power.heaters_off"));
    let off = f.iter().find(|x| x.id == "power.heaters_off").unwrap();
    assert_eq!(off.title, "Heaters turned off: house battery at 10%");
    assert_eq!(off.detail, "Espar off: ok; MCS7 furnace off: ok; MCS7 hot_water off: ok");
    // charging lifts the lockout and clears the alerts
    s.set("power.heat_lockout", json!(false));
    s.set("power.heaters_off", json!(null));
    s.set("power.shutdown", json!({"soc": 9.5, "source": "victron", "charging": true, "warn_soc": 8.0, "shutdown_soc": 5.0,
                                   "warning": false, "in_s": null}));
    assert!(r_power_shutdown(&s, &Ctx::default()).unwrap().is_empty());
}

/// py: tests/test_connectivity.py::test_starlink_alerts_quiet_after_requested_restart
#[test]
fn test_starlink_alerts_quiet_after_requested_restart() {
    let s = StateStore::new();
    s.set("starlink.reachable", json!(false));
    assert!(!r_starlink(&s, &Ctx::default()).unwrap().is_empty());
    s.set("starlink.restart_requested", json!(now()));
    assert!(r_starlink(&s, &Ctx::default()).unwrap().is_empty());
}
