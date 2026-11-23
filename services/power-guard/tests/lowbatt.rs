//! The power guard (tests/test_lowbatt.py ported; the alert-rule assertions of the same tests are in
//! aetherrv-alerts, the Espar/MCS7 refusals in their services).

use aetherrv_core::command::{BoxFuture, Reply};
use aetherrv_core::config::Cfg;
use aetherrv_core::state::{StateExt, StateStore, StateView};
use aetherrv_power_guard::{Controls, Guard};
use serde_json::{Value, json};
use std::sync::Mutex;

#[derive(Default)]
struct Ctl {
    log: Mutex<Vec<(String, String, Value, String)>>,
    calls: Mutex<Vec<String>>,
    down: Mutex<Vec<String>>,
}

impl Controls for Ctl {
    fn command(&self, device: &str, cmd: &str, value: Value, client: &str) -> BoxFuture<Option<Reply>> {
        let name = if device == "rixens" { "mcs7" } else { device };
        self.log.lock().unwrap().push((name.into(), cmd.into(), value, client.into()));
        Box::pin(async { Some(Reply::ok("ok")) })
    }
    fn poweroff(&self) -> BoxFuture<Result<Value, String>> {
        self.calls.lock().unwrap().push("poweroff".into());
        Box::pin(async { Ok(json!({"powering_off": true})) })
    }
    fn heater_up(&self, device: &str) -> bool {
        !self.down.lock().unwrap().iter().any(|d| d == device)
    }
    fn has(&self, _device: &str) -> bool {
        true
    }
}

fn guard(c: Value) -> Guard {
    Guard::new(&Cfg(c), None)
}

async fn run(g: &mut Guard, s: &StateStore, ctl: &Ctl, now: f64) {
    g.tick(s, s, ctl, now).await;
}

/// py: tests/test_lowbatt.py::test_powers_off_after_hold_and_only_once
#[tokio::test]
async fn test_powers_off_after_hold_and_only_once() {
    let s = StateStore::new();
    s.update([("victron.battery.soc", json!(4.5)), ("victron.battery.current", json!(-6.0))]);
    let ctl = Ctl::default();
    let mut g = guard(json!({"shutdown_soc": 5, "hold_s": 120}));
    run(&mut g, &s, &ctl, 1000.0).await;
    run(&mut g, &s, &ctl, 1060.0).await;
    assert!(ctl.calls.lock().unwrap().is_empty());
    assert_eq!(s.get("power.shutdown").unwrap()["in_s"], 60);
    run(&mut g, &s, &ctl, 1120.0).await;
    run(&mut g, &s, &ctl, 1200.0).await;
    assert_eq!(*ctl.calls.lock().unwrap(), vec!["poweroff"]);
}

/// py: tests/test_lowbatt.py::test_charging_or_recovery_cancels_and_missing_soc_never_triggers
#[tokio::test]
async fn test_charging_or_recovery_cancels_and_missing_soc_never_triggers() {
    let s = StateStore::new();
    s.update([("victron.battery.soc", json!(4.0)), ("victron.battery.current", json!(-5.0))]);
    let ctl = Ctl::default();
    let mut g = guard(json!({"hold_s": 60}));
    run(&mut g, &s, &ctl, 0.0).await;
    s.set("victron.battery.current", json!(12.0)); // solar came in
    run(&mut g, &s, &ctl, 70.0).await;
    assert!(ctl.calls.lock().unwrap().is_empty() && g.low_since.is_none());
    assert_eq!(s.get("power.shutdown").unwrap()["warning"], false);
    let s2 = StateStore::new();
    let mut g2 = guard(json!({"hold_s": 0}));
    run(&mut g2, &s2, &ctl, 0.0).await;
    run(&mut g2, &s2, &ctl, 10.0).await;
    assert!(ctl.calls.lock().unwrap().is_empty());
    assert!(s2.get("power.shutdown").unwrap()["soc"].is_null());
}

/// py: tests/test_lowbatt.py::test_bluetooth_fallback_and_warning
#[tokio::test]
async fn test_bluetooth_fallback_and_warning() {
    let s = StateStore::new();
    s.update([
        ("batteries.ble", json!([{"ok": true, "soc": 7.0}, {"ok": true, "soc": 8.0}, {"ok": false, "soc": 1.0}])),
        ("batteries.ble.current", json!(-4.0)),
    ]);
    let mut g = guard(json!({"warn_soc": 8, "shutdown_soc": 5}));
    run(&mut g, &s, &Ctl::default(), 0.0).await;
    let p = s.get("power.shutdown").unwrap();
    assert_eq!((p["soc"].clone(), p["source"].clone(), p["warning"].clone()), (json!(7.5), json!("batteries"), json!(true)));
    assert!(p["in_s"].is_null());
}

/// py: tests/test_lowbatt.py::test_heat_lockout_turns_both_off_and_keeps_them_off_until_charging
#[tokio::test]
async fn test_heat_lockout_turns_both_off_and_keeps_them_off_until_charging() {
    let s = StateStore::new();
    s.update([
        ("victron.battery.soc", json!(9.5)),
        ("victron.battery.current", json!(-20.0)),
        ("climate.espar.ctl.active", json!(true)),
        ("climate.espar.ctl.mode", json!("heat")),
        ("climate.rixens.live", json!(true)),
        ("climate.rixens.state", json!("standby")),
        ("climate.rixens.sources", json!(["furnace"])),
        ("climate.rixens.hot_water", json!(true)),
        ("climate.rixens.preheat", json!(false)),
    ]);
    let ctl = Ctl::default();
    let mut g = guard(json!({"heater_off_soc": 10, "hold_s": 120}));
    run(&mut g, &s, &ctl, 0.0).await;
    run(&mut g, &s, &ctl, 60.0).await;
    assert!(ctl.log.lock().unwrap().is_empty() && s.is_false("power.heat_lockout"), "still inside the hold");
    run(&mut g, &s, &ctl, 121.0).await;
    assert!(s.is_true("power.heat_lockout"));
    let want = vec![
        ("espar".to_string(), "set".to_string(), json!({"mode": "off"}), "auto:low-battery".to_string()),
        ("mcs7".to_string(), "furnace".to_string(), json!(false), "auto:low-battery".to_string()),
        ("mcs7".to_string(), "hot_water".to_string(), json!(false), "auto:low-battery".to_string()),
    ];
    assert_eq!(*ctl.log.lock().unwrap(), want);
    assert!(s.get("power.heaters_off").is_some_and(|v| !v.is_null()));
    run(&mut g, &s, &ctl, 150.0).await;
    assert_eq!(ctl.log.lock().unwrap().len(), 3, "at most once a minute");
    s.update([("climate.espar.ctl.mode", json!("off")), ("climate.rixens.sources", json!([])), ("climate.rixens.hot_water", json!(false))]);
    run(&mut g, &s, &ctl, 300.0).await;
    assert_eq!(ctl.log.lock().unwrap().len(), 3, "nothing heating: nothing sent");
    s.set("climate.espar.ctl.mode", json!("heat")); // someone got it going again: off again
    run(&mut g, &s, &ctl, 400.0).await;
    assert_eq!(ctl.log.lock().unwrap().last().unwrap().0, "espar");
    s.set("victron.battery.current", json!(15.0)); // charging lifts the lockout and clears the list
    run(&mut g, &s, &ctl, 410.0).await;
    assert!(s.is_false("power.heat_lockout"));
    assert!(s.get("power.heaters_off").unwrap().is_null());
}

/// py: tests/test_lowbatt.py::test_poweroff_waits_for_heater_after_run
#[tokio::test]
async fn test_poweroff_waits_for_heater_after_run() {
    let s = StateStore::new();
    s.update([("victron.battery.soc", json!(4.0)), ("victron.battery.current", json!(-5.0)), ("climate.espar.state", json!("cooldown"))]);
    let ctl = Ctl::default();
    let mut g = guard(json!({"hold_s": 10, "heater_wait_s": 600}));
    run(&mut g, &s, &ctl, 0.0).await;
    run(&mut g, &s, &ctl, 20.0).await;
    assert!(ctl.calls.lock().unwrap().is_empty());
    assert_eq!(s.get("power.shutdown").unwrap()["waiting_for_heaters"], true);
    s.set("climate.espar.state", json!("off")); // after-run finished
    run(&mut g, &s, &ctl, 300.0).await;
    assert_eq!(*ctl.calls.lock().unwrap(), vec!["poweroff"]);
}

/// py: tests/test_lowbatt.py::test_poweroff_waits_at_most_heater_wait_s
#[tokio::test]
async fn test_poweroff_waits_at_most_heater_wait_s() {
    let s = StateStore::new();
    s.update([("victron.battery.soc", json!(4.0)), ("victron.battery.current", json!(-5.0)), ("climate.rixens.heater_state_code", json!(30))]);
    let ctl = Ctl::default();
    let mut g = guard(json!({"hold_s": 10, "heater_wait_s": 300}));
    run(&mut g, &s, &ctl, 0.0).await;
    run(&mut g, &s, &ctl, 20.0).await;
    run(&mut g, &s, &ctl, 200.0).await;
    assert!(ctl.calls.lock().unwrap().is_empty());
    run(&mut g, &s, &ctl, 330.0).await;
    assert_eq!(*ctl.calls.lock().unwrap(), vec!["poweroff"]);
}

/// py: tests/test_lowbatt.py::test_heaters_left_alone_when_not_heating_or_above_threshold
#[tokio::test]
async fn test_heaters_left_alone_when_not_heating_or_above_threshold() {
    let s = StateStore::new();
    s.update([
        ("victron.battery.soc", json!(9.0)),
        ("victron.battery.current", json!(-3.0)),
        ("climate.espar.ctl.active", json!(false)),
        ("climate.rixens.state", json!("standby")),
        ("climate.rixens.sources", json!(["furnace"])),
    ]);
    let ctl = Ctl::default();
    let mut g = guard(json!({"hold_s": 10}));
    run(&mut g, &s, &ctl, 0.0).await;
    run(&mut g, &s, &ctl, 60.0).await;
    assert!(ctl.log.lock().unwrap().is_empty(), "nothing heating: nothing sent");
    s.update([("victron.battery.soc", json!(30.0)), ("climate.rixens.state", json!("heating"))]);
    run(&mut g, &s, &ctl, 70.0).await;
    run(&mut g, &s, &ctl, 200.0).await;
    assert!(ctl.log.lock().unwrap().is_empty());
}

/// New (ARCHITECTURE.md §8): while waiting to power off, a heater whose service is down counts as busy, so the wait
/// runs its full length instead of powering off early.
#[tokio::test]
async fn a_heater_service_that_is_down_counts_as_busy() {
    let s = StateStore::new();
    s.update([("victron.battery.soc", json!(4.0)), ("victron.battery.current", json!(-5.0))]);
    let ctl = Ctl::default();
    ctl.down.lock().unwrap().push("espar".into());
    let mut g = guard(json!({"hold_s": 10, "heater_wait_s": 300}));
    run(&mut g, &s, &ctl, 0.0).await;
    run(&mut g, &s, &ctl, 20.0).await;
    run(&mut g, &s, &ctl, 200.0).await;
    assert!(ctl.calls.lock().unwrap().is_empty());
    run(&mut g, &s, &ctl, 330.0).await;
    assert_eq!(*ctl.calls.lock().unwrap(), vec!["poweroff"], "after heater_wait_s it powers off anyway");
    let _ = s.keys();
}
