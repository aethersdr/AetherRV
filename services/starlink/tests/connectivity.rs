//! The Starlink half of `tests/test_connectivity.py`, plus the same command paths against the gRPC simulator.

use aetherrv_core::cmdlog::CommandLog;
use aetherrv_core::command::ResultKind;
use aetherrv_core::state::{StateExt, StateStore, StateView};
use aetherrv_core::txgate::TxGate;
use aetherrv_starlink::*;
use serde_json::{Map, Value, json};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap()
}

fn get(d: &[(String, Value)], k: &str) -> Value {
    d.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone()).unwrap_or_else(|| panic!("no {k}"))
}

fn b64decode(s: &str) -> Vec<u8> {
    let val = |c: u8| -> u32 {
        match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a' + 26) as u32,
            b'0'..=b'9' => (c - b'0' + 52) as u32,
            b'+' => 62,
            b'/' => 63,
            _ => panic!("bad base64"),
        }
    };
    let mut out = Vec::new();
    for ch in s.as_bytes().chunks(4) {
        let pad = ch.iter().filter(|&&c| c == b'=').count();
        let n = ch.iter().filter(|&&c| c != b'=').enumerate().fold(0u32, |n, (i, &c)| n | val(c) << (18 - 6 * i as u32));
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    out
}

/// py: tests/test_connectivity.py::test_starlink_normalize
#[test]
fn test_starlink_normalize() {
    let st = obj(json!({"state": "CONNECTED", "uptime": 1000, "software_version": "2026.11", "pop_ping_latency_ms": 31.24,
        "pop_ping_drop_rate": 0.0012, "downlink_throughput_bps": 85_400_000, "uplink_throughput_bps": 9_100_000,
        "fraction_obstructed": 0.0234, "currently_obstructed": false, "gps_sats": 12}));
    let alerts: Vec<(String, bool)> = [("alert_motors_stuck", false), ("alert_is_heating", true), ("alert_thermal_throttle", true), ("alert_brand_new_thing", true)]
        .map(|(a, b)| (a.to_string(), b))
        .into();
    let d = normalize(&st, &Map::new(), &alerts);
    assert!(get(&d, "starlink.online") == json!(true) && get(&d, "starlink.down_mbps") == json!(85.4) && get(&d, "starlink.obstructed_pct") == json!(2.34));
    let sev: Map<String, Value> = get(&d, "starlink.alerts").as_array().unwrap().iter().map(|a| (a["id"].as_str().unwrap().to_string(), a["severity"].clone())).collect();
    assert_eq!(Value::Object(sev), json!({"alert_is_heating": null, "alert_thermal_throttle": "warning", "alert_brand_new_thing": "warning"}));
    assert_eq!(get(&normalize(&obj(json!({"state": "OBSTRUCTED"})), &Map::new(), &[]), "starlink.online"), json!(false));
}

/// py: tests/test_connectivity.py::test_starlink_obstruction_map_encoding (defined twice in the Python file, identical;
/// pytest collects it once)
#[test]
fn test_starlink_obstruction_map_encoding() {
    let m = encode_obstruction_map(2, 3, &[-1.0, 0.0, 1.0, 0.5, 1.7, -0.2], Some(10.0), Some(80.0), Some(2)).unwrap();
    assert_eq!((m["rows"].clone(), m["cols"].clone(), m["frame"].clone(), m["min_el"].clone(), m["max_theta"].clone()), (json!(2), json!(3), json!("dish"), json!(10.0), json!(80.0)));
    assert_eq!(b64decode(m["data"].as_str().unwrap()), vec![255, 0, 254, 127, 254, 255]);
    assert_eq!(encode_obstruction_map(1, 1, &[1.0], None, None, Some(1)).unwrap()["frame"], "earth");
    assert!(encode_obstruction_map(2, 2, &[1.0], None, None, None).is_err());
}

/// py: tests/test_connectivity.py::test_starlink_signal_quality_to_snr
#[test]
fn test_starlink_signal_quality_to_snr() {
    let d = signal_fields(Some(&SignalRaw { signal_quality: Some(0.68), is_snr_above_noise_floor: Some(true), is_snr_persistently_low: Some(false) }));
    assert_eq!((get(&d, "starlink.signal_quality"), get(&d, "starlink.snr_db"), get(&d, "starlink.snr_capped")), (json!(0.68), json!(6.8), json!(false)));
    let d = signal_fields(Some(&SignalRaw { signal_quality: Some(1.0), is_snr_above_noise_floor: Some(true), is_snr_persistently_low: Some(false) }));
    assert!(get(&d, "starlink.snr_db") == json!(10.0) && get(&d, "starlink.snr_capped") == json!(true));
    assert_eq!(get(&signal_fields(None), "starlink.snr_db"), Value::Null);
}

/// py: tests/test_connectivity.py::test_starlink_history_fields_power_and_events
#[test]
fn test_starlink_history_fields_power_and_events() {
    // the test's enum tables (Python: Ev.DESCRIPTOR.fields_by_name[...].enum_type.values_by_number)
    let reasons = |n: i64| match n {
        8 => Some("EVENT_REASON_OUTAGE_NO_DOWNLINK".to_string()),
        22 => Some("EVENT_REASON_UT_ALERT_RAIN_SNR_PERSISTENTLY_LOW".to_string()),
        _ => None,
    };
    let sevs = |n: i64| (n == 1).then(|| "EVENT_SEVERITY_WARNING".to_string());
    let ev = |reason: i64, severity: i64, start: i64, dur: i64| HistEvent {
        reason,
        severity,
        reason_name: reasons(reason),
        severity_name: sevs(severity),
        start_timestamp_ns: start,
        duration_ns: dur,
    };
    // (device_id="SECRET" on the Python events: the Rust event type has no such field at all)
    let evs = vec![ev(8, 1, 1_000_000_000_000, 600_000_000), ev(22, 1, 2_000_000_000_000, 0)];
    let mut power = vec![10.0; 840];
    power.extend(vec![20.0; 60]);
    let h = History { power_in: power, current: 900, event_log: Some(EventLog { events: evs, start_timestamp_ns: 0, current_timestamp_ns: 3_600_000_000_000 }) };
    let d = history_fields(&h, 50);
    assert_eq!(get(&d, "starlink.power_w"), json!(20.0));
    let events = get(&d, "starlink.events");
    let what: Vec<&str> = events.as_array().unwrap().iter().map(|e| e["what"].as_str().unwrap()).collect();
    assert_eq!(what, ["Rain SNR persistently low", "No downlink"]);
    assert!(events[1]["outage"] == json!(true) && events[1]["dur_s"] == json!(0.6));
    assert!(!serde_json::to_string(&Value::Array(d.iter().map(|(_, v)| v.clone()).collect())).unwrap().contains("SECRET"));
    assert_eq!(get(&d, "starlink.events_span_s"), json!(3600));
}

// ── the command paths against a scripted dish (Python's FakeDish) ──────────────────────────────────────────────

/// Stand-in for DishOps: scripted status reads, counts reboot sends.
struct FakeDish {
    statuses: Mutex<Vec<Result<Map<String, Value>, String>>>,
    reboots: AtomicU32,
    reboot_exc: Option<String>,
    cfg: Mutex<Option<Value>>,
    sets: Mutex<Vec<Value>>,
    checks: AtomicU32,
}

impl FakeDish {
    fn new(statuses: Vec<Result<Value, &str>>, reboot_exc: Option<&str>) -> Arc<FakeDish> {
        Arc::new(FakeDish {
            statuses: Mutex::new(statuses.into_iter().map(|s| s.map(obj).map_err(str::to_string)).collect()),
            reboots: AtomicU32::new(0),
            reboot_exc: reboot_exc.map(str::to_string),
            cfg: Mutex::new(None),
            sets: Mutex::new(Vec::new()),
            checks: AtomicU32::new(0),
        })
    }
    fn reboots(&self) -> u32 {
        self.reboots.load(Ordering::SeqCst)
    }
    fn sets(&self) -> Vec<Value> {
        self.sets.lock().unwrap().clone()
    }
    fn ensure_cfg(&self) -> Value {
        let mut c = self.cfg.lock().unwrap();
        c.get_or_insert_with(|| json!({"heating": "auto", "sleep": {"enabled": false, "start_utc_min": 0, "duration_min": 0}})).clone()
    }
}

impl DishOps for FakeDish {
    fn status(&self) -> DishFut<'_, Map<String, Value>> {
        let s = {
            let mut l = self.statuses.lock().unwrap();
            if l.len() > 1 { l.remove(0) } else { l[0].clone() }
        };
        Box::pin(async move { s.map_err(DishError) })
    }
    fn reboot(&self) -> DishFut<'_, ()> {
        self.reboots.fetch_add(1, Ordering::SeqCst);
        let e = self.reboot_exc.clone();
        Box::pin(async move { e.map_or(Ok(()), |e| Err(DishError(e))) })
    }
    fn get_config(&self) -> DishFut<'_, Value> {
        let c = self.ensure_cfg();
        Box::pin(async move { Ok(c) })
    }
    fn set_config(&self, f: Map<String, Value>) -> DishFut<'_, ()> {
        self.ensure_cfg();
        self.sets.lock().unwrap().push(Value::Object(f.clone()));
        let mut g = self.cfg.lock().unwrap();
        let c = g.as_mut().unwrap();
        if let Some(m) = f.get("snow_melt_mode") {
            c["heating"] = json!(snow_melt(m.as_i64().unwrap()).unwrap());
        }
        if let Some(en) = f.get("power_save_mode") {
            c["sleep"]["enabled"] = en.clone();
            if let Some(s) = f.get("power_save_start_minutes") {
                c["sleep"]["start_utc_min"] = s.clone();
            }
            if let Some(d) = f.get("power_save_duration_minutes") {
                c["sleep"]["duration_min"] = d.clone();
            }
        }
        Box::pin(async { Ok(()) })
    }
    fn check_update(&self) -> DishFut<'_, ()> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}

fn sl(ops: Arc<FakeDish>, enabled: bool) -> Starlink {
    Starlink::new(Arc::new(StateStore::new()), Arc::new(TxGate::new([(DEVICE.to_string(), enabled)])), CommandLog::memory(DEVICE), ops)
}

fn pending() -> Value {
    json!({"software_update_state": "REBOOT_REQUIRED", "uptime": 5000})
}

/// py: tests/test_connectivity.py::test_starlink_install_blocked_without_tx_enabled
#[tokio::test]
async fn test_starlink_install_blocked_without_tx_enabled() {
    let ops = FakeDish::new(vec![Ok(pending())], None);
    let a = sl(ops.clone(), false);
    let r = a.command_with("install_update", &Value::Null, "test", 90.0, 0.0).await;
    assert!(!r.is_ok() && ops.reboots() == 0 && a.state.is_false("starlink.controls_ok"));
    assert_eq!(r.result, ResultKind::Refused);
    let row = &a.log.rows()[0];
    assert_eq!((row.outcome.as_str(), row.detail.as_str(), row.value.as_str()), ("blocked", "transmit to 'starlink' is disabled", "None"));
}

/// py: tests/test_connectivity.py::test_starlink_install_rejects_other_commands_and_no_pending_update
#[tokio::test]
async fn test_starlink_install_rejects_other_commands_and_no_pending_update() {
    let ops = FakeDish::new(vec![Ok(json!({"software_update_state": "IDLE", "uptime": 5000}))], None);
    let a = sl(ops.clone(), true);
    assert!(!a.command_with("dish_stow", &Value::Null, "test", 90.0, 0.0).await.is_ok()); // still not allowed
    let r = a.command_with("install_update", &Value::Null, "test", 90.0, 0.0).await;
    assert!(!r.is_ok() && r.detail.contains("no update") && ops.reboots() == 0);
    let rows = a.log.rows();
    assert_eq!((rows[0].outcome.as_str(), rows[0].detail.as_str()), ("rejected", "not an allowed command"));
    assert_eq!((rows[1].outcome.as_str(), rows[1].detail.as_str()), ("rejected", "software_update_state=IDLE"));
}

/// py: tests/test_connectivity.py::test_starlink_install_sends_once_and_verifies_uptime_reset
#[tokio::test]
async fn test_starlink_install_sends_once_and_verifies_uptime_reset() {
    let ops = FakeDish::new(
        vec![Ok(pending()), Ok(json!({"software_update_state": "REBOOT_REQUIRED", "uptime": 5003})), Ok(json!({"software_update_state": "IDLE", "uptime": 12}))],
        None,
    );
    let a = sl(ops.clone(), true);
    let r = a.command_with("install_update", &Value::Null, "test", 90.0, 0.0).await;
    assert!(r.is_ok() && ops.reboots() == 1 && a.state.truthy("starlink.restart_requested"));
    assert_eq!(r.detail, "the dish restarted (installing the update)");
    assert_eq!(a.log.rows()[0].detail, "uptime reset 5000s → 12s");
}

/// py: tests/test_connectivity.py::test_starlink_install_send_error_is_not_retried
#[tokio::test]
async fn test_starlink_install_send_error_is_not_retried() {
    let ops = FakeDish::new(vec![Ok(pending()), Err("unreachable")], Some("deadline"));
    let a = sl(ops.clone(), true);
    let r = a.command_with("install_update", &Value::Null, "test", 1.0, 0.0).await;
    assert!(!r.is_ok() && ops.reboots() == 1);
    assert_eq!(r.result, ResultKind::Unverified);
    assert_eq!(a.log.rows()[0].detail, "send raised deadline; dish unreachable since");
}

/// py: tests/test_connectivity.py::test_starlink_install_unverified_when_dish_never_restarts
#[tokio::test]
async fn test_starlink_install_unverified_when_dish_never_restarts() {
    let ops = FakeDish::new(vec![Ok(pending())], None);
    let a = sl(ops.clone(), true);
    let r = a.command_with("install_update", &Value::Null, "test", 0.05, 0.01).await;
    assert!(!r.is_ok() && r.detail.contains("did not restart") && ops.reboots() == 1);
    assert_eq!(a.log.rows()[0].detail, "no restart seen within 0s");
}

/// py: tests/test_connectivity.py::test_starlink_plain_reboot_sends_once_and_verifies
#[tokio::test]
async fn test_starlink_plain_reboot_sends_once_and_verifies() {
    let ops = FakeDish::new(vec![Ok(json!({"software_update_state": "IDLE", "uptime": 5000})), Ok(json!({"software_update_state": "IDLE", "uptime": 9}))], None);
    let a = sl(ops.clone(), true);
    let r = a.command_with("reboot", &Value::Null, "test", 90.0, 0.0).await;
    assert!(r.is_ok() && ops.reboots() == 1);
}

/// py: tests/test_connectivity.py::test_starlink_heating_and_sleep_set_once_and_verify
#[tokio::test]
async fn test_starlink_heating_and_sleep_set_once_and_verify() {
    let ops = FakeDish::new(vec![Ok(json!({"software_update_state": "IDLE", "uptime": 5000}))], None);
    let a = sl(ops.clone(), true);
    let r = a.command_with("set_heating", &json!("on"), "test", 90.0, 0.0).await;
    assert!(r.is_ok() && ops.sets() == vec![json!({"snow_melt_mode": 1})] && a.state.get("starlink.heating") == Some(json!("on")));
    assert_eq!(r.detail, "heating on");
    let r = a.command_with("set_heating", &json!("on"), "test", 90.0, 0.0).await;
    assert!(r.detail == "already set" && ops.sets().len() == 1);
    let r = a.command_with("set_sleep", &json!({"enabled": true, "start_utc_min": 420, "duration_min": 480}), "test", 90.0, 0.0).await;
    assert!(r.is_ok() && *ops.sets().last().unwrap() == json!({"power_save_mode": true, "power_save_start_minutes": 420, "power_save_duration_minutes": 480}));
    let r = a.command_with("set_sleep", &json!({"enabled": false}), "test", 90.0, 0.0).await;
    assert!(r.is_ok() && *ops.sets().last().unwrap() == json!({"power_save_mode": false}));
    assert_eq!(r.detail, "sleep schedule off");
    assert!(!a.command_with("set_heating", &json!("sauna"), "test", 90.0, 0.0).await.is_ok());
    assert!(!a.command_with("set_sleep", &json!({"enabled": true, "start_utc_min": 2000, "duration_min": 5}), "test", 90.0, 0.0).await.is_ok());
    assert!(!sl(ops.clone(), false).command_with("set_heating", &json!("off"), "test", 90.0, 0.0).await.is_ok());
    let rows = a.log.rows();
    assert_eq!(rows[0].value, "on");
    assert_eq!(
        rows[0].detail,
        "{'heating': 'auto', 'sleep': {'enabled': False, 'start_utc_min': 0, 'duration_min': 0}} → {'heating': 'on', 'sleep': {'enabled': False, 'start_utc_min': 0, 'duration_min': 0}}"
    );
    assert_eq!((rows[4].outcome.as_str(), rows[4].detail.as_str()), ("rejected", "bad value: 'sauna'"));
    assert_eq!(rows[5].detail, "bad value: start 0–1439 min, duration 1–1439 min");
}

/// Python would coerce these with bool()/int(); the Rust service refuses them (SERVICE-GUIDE: strict values).
#[tokio::test]
async fn set_sleep_values_must_have_json_types() {
    let ops = FakeDish::new(vec![Ok(json!({"software_update_state": "IDLE", "uptime": 5000}))], None);
    let a = sl(ops.clone(), true);
    for bad in [json!({"enabled": "false"}), json!({"enabled": 1}), json!({"enabled": true, "start_utc_min": "420", "duration_min": 60}),
                json!({"enabled": true, "start_utc_min": 420.5, "duration_min": 60}), json!(null), json!({"start_utc_min": 1}), json!([true])] {
        let r = a.command_with("set_sleep", &bad, "test", 90.0, 0.0).await;
        assert!(r.result == ResultKind::Refused && r.detail == "bad request", "{bad}");
    }
    assert!(ops.sets().is_empty());
    assert!(!a.command_with("set_heating", &json!(true), "test", 90.0, 0.0).await.is_ok());
}

/// py: tests/test_connectivity.py::test_starlink_check_update_never_reboots
#[tokio::test]
async fn test_starlink_check_update_never_reboots() {
    let ops = FakeDish::new(vec![Ok(json!({"software_update_state": "IDLE", "uptime": 5000}))], None);
    let a = sl(ops.clone(), true);
    let r = a.check_update(&Value::Null, "test", &json!("IDLE"), 0.0, 0.05).await;
    assert!(r.is_ok() && r.detail.contains("no update") && ops.reboots() == 0);
    assert_eq!(ops.checks.load(Ordering::SeqCst), 1);
    assert_eq!(a.log.rows()[0].detail, "IDLE → IDLE");
}

/// The check request fails but the dish's update state moves anyway: "unverified" with what the dish now reports
/// (owner decision Q-S1, ARCHITECTURE.md §13a D20; Python answered ok here). Not a Python test.
#[tokio::test]
async fn check_update_that_errors_while_the_state_moves_is_unverified() {
    struct FailingCheck(Arc<FakeDish>);
    impl DishOps for FailingCheck {
        fn status(&self) -> DishFut<'_, Map<String, Value>> {
            self.0.status()
        }
        fn reboot(&self) -> DishFut<'_, ()> {
            self.0.reboot()
        }
        fn get_config(&self) -> DishFut<'_, Value> {
            self.0.get_config()
        }
        fn set_config(&self, f: Map<String, Value>) -> DishFut<'_, ()> {
            self.0.set_config(f)
        }
        fn check_update(&self) -> DishFut<'_, ()> {
            self.0.checks.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err(DishError("deadline exceeded".into())) })
        }
    }
    let ops = FakeDish::new(vec![Ok(json!({"software_update_state": "FETCHING", "uptime": 5000}))], None);
    let a = Starlink::new(Arc::new(StateStore::new()), Arc::new(TxGate::new([(DEVICE.to_string(), true)])), CommandLog::memory(DEVICE), Arc::new(FailingCheck(ops.clone())));
    let r = a.check_update(&Value::Null, "test", &json!("IDLE"), 0.0, 0.05).await;
    assert_eq!(r.result, ResultKind::Unverified, "{r:?}");
    assert!(r.detail.contains("deadline exceeded") && r.detail.contains("fetching"), "{}", r.detail);
    assert_eq!((ops.checks.load(Ordering::SeqCst), ops.reboots()), (1, 0));
    assert_eq!(a.log.rows()[0].outcome, "error");
}

// ── against the gRPC simulator ──────────────────────────────────────────────────────────────────────────────────

mod sim {
    use super::*;
    use aetherrv_sim::starlink::{SW_REBOOT_REQUIRED, StarlinkSim, StarlinkSimConfig};
    use std::time::Duration;

    fn grpc_sl(target: &str, enabled: bool) -> (Starlink, StatusPoll) {
        let cache: grpc::ReflectCache = Default::default();
        let ops: Arc<dyn DishOps> = Arc::new(grpc::GrpcDish::new(target, cache.clone()));
        let s = Starlink::new(Arc::new(StateStore::new()), Arc::new(TxGate::new([(DEVICE.to_string(), enabled)])), CommandLog::memory(DEVICE), ops.clone());
        (s, StatusPoll::new(grpc::Poller::new(target, cache), ops))
    }

    fn quick() -> StarlinkSimConfig {
        StarlinkSimConfig {
            reboot_delay: Duration::from_millis(100),
            down_for: Duration::from_millis(800),
            fetch_for: Duration::from_millis(300),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn poll_reads_every_key_and_copies_no_device_id() {
        let sim = StarlinkSim::start(StarlinkSimConfig { frozen: true, ..Default::default() }).await.unwrap();
        let (_s, mut p) = grpc_sl(&sim.target(), false);
        let d = p.poll_once().await.unwrap();
        let keys: Vec<&str> = d.iter().map(|(k, _)| k.as_str()).collect();
        for k in ["starlink.uptime", "starlink.signal_quality", "starlink.obstruction_map", "starlink.power_w", "starlink.events", "starlink.events_span_s", "starlink.heating", "starlink.sleep"] {
            assert!(keys.contains(&k), "{k} missing");
        }
        assert_eq!(get(&d, "starlink.state"), json!("CONNECTED"));
        assert_eq!(get(&d, "starlink.uptime"), json!(86400));
        assert_eq!(get(&d, "starlink.signal_quality"), json!(0.68));
        assert_eq!(get(&d, "starlink.alerts"), json!([{"id": "alert_roaming", "label": "Roaming", "severity": "warning"}]));
        let blob = serde_json::to_string(&d.iter().map(|(_, v)| v.clone()).collect::<Vec<_>>()).unwrap();
        assert!(!blob.contains("ut-sim-") && !blob.contains("router-sim-"), "a device id leaked");
        // the second poll inside a minute reads status only
        let d2 = p.poll_once().await.unwrap();
        assert!(d2.iter().all(|(k, _)| k != "starlink.obstruction_map"));
        assert_eq!(sim.with(|s| s.calls.get("get_history").copied()), Some(1));
    }

    #[tokio::test]
    async fn older_firmware_without_signal_quality_and_v1alpha_reflection() {
        let sim = StarlinkSim::start(StarlinkSimConfig { frozen: true, legacy_firmware: true, v1alpha_only: true, ..Default::default() }).await.unwrap();
        let (_s, mut p) = grpc_sl(&sim.target(), false);
        let d = p.poll_once().await.unwrap();
        assert_eq!(get(&d, "starlink.signal_quality"), Value::Null);
        assert_eq!(get(&d, "starlink.snr_db"), Value::Null);
        assert_eq!(get(&d, "starlink.snr_above_noise"), json!(true));
        assert_eq!(get(&d, "starlink.reachable"), json!(true));
    }

    #[tokio::test]
    async fn outage_states() {
        let sim = StarlinkSim::start(StarlinkSimConfig { frozen: true, ..Default::default() }).await.unwrap();
        let (_s, mut p) = grpc_sl(&sim.target(), false);
        sim.with(|s| s.outage_cause = Some(aetherrv_sim::starlink::OUTAGE_NO_SCHEDULE));
        let d = p.poll_once().await.unwrap();
        assert_eq!((get(&d, "starlink.state"), get(&d, "starlink.online")), (json!("SEARCHING"), json!(false)));
        sim.with(|s| s.outage_cause = Some(aetherrv_sim::starlink::OUTAGE_OBSTRUCTED));
        assert_eq!(get(&p.poll_once().await.unwrap(), "starlink.state"), json!("OBSTRUCTED"));
    }

    /// py: tests/test_connectivity.py::test_starlink_alerts_quiet_after_requested_restart (an alert-rule test: only
    /// the Starlink side is ported here, the rule `r_starlink` belongs to the alerts service): the dish going away
    /// publishes `reachable: false`, and a requested restart stamps `starlink.restart_requested` with the wall time
    /// the rule compares against.
    #[tokio::test]
    async fn starlink_side_of_alerts_quiet_after_requested_restart() {
        let sim = StarlinkSim::start(quick()).await.unwrap();
        let (s, p) = grpc_sl(&sim.target(), true);
        let state = s.state.clone();
        let poll = tokio::spawn(poll_loop(state.clone(), p, 0.2));
        assert!(aetherrv_core::testutil::eventually(Duration::from_secs(5), || state.is_true("starlink.reachable")).await);
        let r = s.command_with("reboot", &Value::Null, "test", 10.0, 0.1).await;
        assert!(r.is_ok(), "{r:?}");
        let t = state.num("starlink.restart_requested").unwrap();
        assert!((aetherrv_core::clock::now_s() - t).abs() < 5.0);
        assert!(aetherrv_core::testutil::eventually(Duration::from_secs(5), || state.is_false("starlink.reachable")).await);
        assert!(state.is_false("starlink.online"));
        poll.abort();
    }

    #[tokio::test]
    async fn check_then_install_update_against_the_sim() {
        let sim = StarlinkSim::start(StarlinkSimConfig { update_available: true, ..quick() }).await.unwrap();
        let (s, _p) = grpc_sl(&sim.target(), true);
        // nothing waiting yet: install refused, nothing sent
        let r = s.command_with("install_update", &Value::Null, "kiosk", 10.0, 0.1).await;
        assert!(r.result == ResultKind::Refused && r.detail == "no update is waiting to install");
        let r = s.command_with("check_update", &Value::Null, "kiosk", 10.0, 0.1).await;
        // the watch ends at the first change (IDLE → FETCHING), as in Python
        assert!(r.is_ok() && r.detail == "update state: fetching", "{r:?}");
        assert!(aetherrv_core::testutil::eventually(Duration::from_secs(5), || sim.with(|x| x.update_state == SW_REBOOT_REQUIRED)).await);
        let r = s.command_with("check_update", &Value::Null, "kiosk", 10.0, 0.1).await;
        assert!(r.is_ok() && r.detail == "an update is downloaded and ready to install", "{r:?}");
        assert_eq!(sim.with(|x| (x.update_checks, x.reboots, x.update_state)), (2, 0, SW_REBOOT_REQUIRED));
        let old = sim.with(|x| x.software_version.clone());
        let r = s.command_with("install_update", &Value::Null, "kiosk", 10.0, 0.1).await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(sim.with(|x| x.reboots), 1);
        assert!(aetherrv_core::testutil::eventually(Duration::from_secs(5), || sim.is_up()).await);
        assert_ne!(sim.with(|x| x.software_version.clone()), old);
        let rows = s.log.rows();
        assert_eq!(rows.iter().map(|r| r.outcome.as_str()).collect::<Vec<_>>(), ["rejected", "ok", "ok", "ok"]);
        assert_eq!(rows[1].detail, "IDLE → FETCHING");
        assert_eq!(rows[2].detail, "REBOOT_REQUIRED → REBOOT_REQUIRED");
    }

    #[tokio::test]
    async fn reboot_verified_by_uptime_reset_when_the_poll_misses_the_outage() {
        // a slow verify poll (longer than the outage) sees the uptime reset instead of the dish being away
        let sim = StarlinkSim::start(StarlinkSimConfig { down_for: Duration::from_millis(200), ..quick() }).await.unwrap();
        let (s, _p) = grpc_sl(&sim.target(), true);
        let r = s.command_with("reboot", &Value::Null, "kiosk", 10.0, 0.8).await;
        assert!(r.is_ok() && r.detail == "the dish restarted (rebooting)", "{r:?}");
        assert!(s.log.rows()[0].detail.starts_with("uptime reset "), "{:?}", s.log.rows());
        assert_eq!(sim.with(|x| x.reboots), 1);
    }

    #[tokio::test]
    async fn reboot_whose_reply_is_lost_is_sent_once_and_unverified() {
        let sim = StarlinkSim::start(quick()).await.unwrap();
        sim.with(|x| x.reboot_reply_error = true);
        let (s, _p) = grpc_sl(&sim.target(), true);
        let r = s.command_with("reboot", &Value::Null, "kiosk", 10.0, 0.3).await;
        assert_eq!(r.result, ResultKind::Unverified);
        assert_eq!(r.detail, "the request may not have reached the dish (not retried)");
        assert_eq!(sim.with(|x| x.reboots), 1);
    }

    #[tokio::test]
    async fn heating_and_sleep_against_the_sim_are_sent_once() {
        let sim = StarlinkSim::start(quick()).await.unwrap();
        let (mut s, _p) = grpc_sl(&sim.target(), true);
        s.config_verify_s = 1.0;
        let r = s.command_with("set_heating", &json!("off"), "kiosk", 10.0, 0.05).await;
        assert!(r.is_ok() && r.detail == "heating off", "{r:?}");
        let r = s.command_with("set_sleep", &json!({"enabled": true, "start_utc_min": 120, "duration_min": 300}), "kiosk", 10.0, 0.05).await;
        assert!(r.is_ok() && r.detail == "sleep schedule on", "{r:?}");
        let r = s.command_with("set_sleep", &json!({"enabled": true, "start_utc_min": 120, "duration_min": 300}), "kiosk", 10.0, 0.05).await;
        assert_eq!(r.detail, "already set");
        assert_eq!(
            sim.with(|x| x.set_configs.clone()),
            vec![json!({"snow_melt_mode": 2}), json!({"power_save_start_minutes": 120, "power_save_duration_minutes": 300, "power_save_mode": true})]
        );
        // a dish that never applies: one send, then unverified
        sim.with(|x| x.ignore_set_config = true);
        let r = s.command_with("set_heating", &json!("on"), "kiosk", 10.0, 0.05).await;
        assert_eq!(r.result, ResultKind::Unverified);
        assert_eq!(sim.with(|x| x.set_configs.len()), 3);
    }

    #[tokio::test]
    async fn unreachable_dish_refuses_and_sends_nothing() {
        let sim = StarlinkSim::start(quick()).await.unwrap();
        let target = sim.target();
        sim.stop();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (s, mut p) = grpc_sl(&target, true);
        let r = s.command_with("reboot", &Value::Null, "kiosk", 1.0, 0.1).await;
        assert!(r.result == ResultKind::Refused && r.detail == "dish not reachable; nothing sent");
        assert!(p.poll_once().await.is_err());
    }

    /// The poll reader's one retry on a reused channel (reads only).
    #[tokio::test]
    async fn poll_read_retries_once_on_a_reused_channel() {
        let sim = StarlinkSim::start(StarlinkSimConfig { frozen: true, ..Default::default() }).await.unwrap();
        let (_s, mut p) = grpc_sl(&sim.target(), false);
        p.poll_once().await.unwrap();
        sim.with(|x| x.fail.insert("get_status".into(), 1));
        p.poll_once().await.unwrap(); // first read fails on the kept channel, the retry on a new one succeeds
        assert_eq!(sim.with(|x| x.calls["get_status"]), 3);
        sim.with(|x| x.fail.insert("get_status".into(), 2));
        assert!(p.poll_once().await.is_err());
    }
}
