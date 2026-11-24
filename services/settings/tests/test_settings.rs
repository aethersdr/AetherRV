//! The Python settings tests (tests/test_settings.py: prefs and the automatic time zone; tests/test_core.py: the
//! fridge/freezer re-arm), plus the service on a private broker.

use aetherrv_alerts::Ctx;
use aetherrv_alerts::rules::r_cold;
use aetherrv_core::admin::AdminClient;
use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::command::ResultKind;
use aetherrv_core::config::Cfg;
use aetherrv_core::state::{StateStore, StateView};
use aetherrv_core::testutil::{Broker, eventually};
use aetherrv_settings::*;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// py: tests/test_settings.py::test_prefs_validate_persist_publish
#[test]
fn test_prefs_validate_persist_publish() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let st = Arc::new(StateStore::new());
    let mut p = Prefs::load(st.clone(), &path);
    assert_eq!(st.get("settings.screen_off_min"), Some(json!(10)));
    p.set("screen_off_min", &json!(30)).unwrap();
    assert_eq!(p.set("screen_off_min", &json!(999)), Err("screen_off_min must be 0–240".to_string()));
    assert_eq!(p.set("rm", &json!(1)), Err("unknown setting 'rm'".to_string()));
    assert_eq!(Prefs::load(Arc::new(StateStore::new()), &path).values["screen_off_min"], json!(30));
    // the file is Python's json.dumps(values, indent=1), keys in PREFS_DEFAULT order
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{\n \"screen_off_min\": 30,\n \"auto_timezone\": true,\n \"fridge_off\": false,\n \"freezer_off\": false\n}"
    );
    assert!(!dir.path().join("settings.tmp").exists());
}

/// A toggle must be a JSON boolean and a number a JSON number (no `bool("false") == True`); a float is truncated
/// like `int()`.
#[test]
fn prefs_refuse_the_wrong_json_type() {
    let dir = tempfile::tempdir().unwrap();
    let st = Arc::new(StateStore::new());
    let mut p = Prefs::load(st.clone(), dir.path().join("s.json"));
    assert_eq!(p.set("fridge_off", &json!("false")), Err(BAD_REQUEST.to_string()));
    assert_eq!(p.set("screen_off_min", &json!("30")), Err(BAD_REQUEST.to_string()));
    assert_eq!(p.set("screen_off_min", &json!(true)), Err(BAD_REQUEST.to_string()));
    assert_eq!(st.get("settings.fridge_off"), Some(json!(false)));
    p.set("screen_off_min", &json!(12.9)).unwrap();
    assert_eq!(st.get("settings.screen_off_min"), Some(json!(12)));
    p.set("fridge_off", &json!(true)).unwrap();
    assert_eq!(st.get("settings.fridge_off"), Some(json!(true)));
}

/// A fake admin helper on a Unix socket: records `(verb, args)` and answers `{"ok": true}`.
async fn fake_admin(dir: &Path) -> (AdminClient, Arc<Mutex<Vec<(String, Value)>>>) {
    let path = dir.join("admin.sock");
    let l = tokio::net::UnixListener::bind(&path).unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let c = calls.clone();
    tokio::spawn(async move {
        loop {
            let Ok((s, _)) = l.accept().await else { return };
            let (r, mut w) = s.into_split();
            let mut line = String::new();
            BufReader::new(r).read_line(&mut line).await.unwrap();
            let j: Value = serde_json::from_str(&line).unwrap();
            c.lock().unwrap().push((j["verb"].as_str().unwrap().to_string(), j["args"].clone()));
            w.write_all(b"{\"ok\": true}\n").await.unwrap();
        }
    });
    (AdminClient::new(path), calls)
}

/// py: tests/test_settings.py::test_auto_timezone_needs_fix_and_two_agreeing_lookups
#[tokio::test]
async fn test_auto_timezone_needs_fix_and_two_agreeing_lookups() {
    let dir = tempfile::tempdir().unwrap();
    let (adm, calls) = fake_admin(dir.path()).await;
    let st = Arc::new(StateStore::new());
    let prefs = Mutex::new(Prefs::load(st.clone(), dir.path().join("s.json")));
    let answers = Arc::new(Mutex::new(vec!["America/Denver"; 5]));
    let lookup: Lookup = Arc::new(move |_lat, _lon| {
        let a = answers.lock().unwrap().pop().map(str::to_string);
        Box::pin(async move { Ok(a) })
    });
    let mut tz = AutoTimezone::new(adm, lookup, Arc::new(|| Some("America/Los_Angeles".to_string())));
    st.update([("location.lat", json!(46.6)), ("location.lon", json!(-112.0)), ("location.fix", json!(false))]);
    let now = aetherrv_core::clock::now_s();
    tz.step(&*st, &prefs, &st, now).await;
    assert!(calls.lock().unwrap().is_empty(), "no fix → nothing");
    st.set("location.fix", json!(true));
    tz.step(&*st, &prefs, &st, now + 60.0).await;
    assert!(calls.lock().unwrap().is_empty(), "first answer only arms it");
    tz.step(&*st, &prefs, &st, now + 120.0).await;
    assert_eq!(*calls.lock().unwrap(), vec![("set_timezone".to_string(), json!({"tz": "America/Denver"}))]);
    assert_eq!(st.get("settings.timezone"), Some(json!("America/Denver")));
}

/// py: tests/test_settings.py::test_auto_timezone_offline_keeps_zone
#[tokio::test]
async fn test_auto_timezone_offline_keeps_zone() {
    let dir = tempfile::tempdir().unwrap();
    let (adm, calls) = fake_admin(dir.path()).await;
    let st = Arc::new(StateStore::new());
    let prefs = Mutex::new(Prefs::load(st.clone(), dir.path().join("s.json")));
    let lookup: Lookup = Arc::new(|_lat, _lon| Box::pin(async { Err("TimeoutError".to_string()) }));
    let mut tz = AutoTimezone::new(adm, lookup, Arc::new(system_zone));
    st.update([("location.lat", json!(46.6)), ("location.lon", json!(-112.0)), ("location.fix", json!(true))]);
    let now = aetherrv_core::clock::now_s();
    tz.step(&*st, &prefs, &st, now).await;
    tz.step(&*st, &prefs, &st, now + 60.0).await;
    assert!(calls.lock().unwrap().is_empty());
}

/// The Open-Meteo request and reply handling (never fetched in tests).
#[test]
fn open_meteo_url_and_reply() {
    assert_eq!(
        lookup_url(46.6, -112.0),
        "https://api.open-meteo.com/v1/forecast?latitude=46.600&longitude=-112.000&timezone=auto&forecast_days=1&daily=temperature_2m_max"
    );
    assert_eq!(parse_zone(200, br#"{"timezone": "America/Denver"}"#), Ok(Some("America/Denver".into())));
    assert_eq!(parse_zone(200, br#"{"timezone": "GMT"}"#), Ok(None));
    assert_eq!(parse_zone(200, br#"{"timezone": ""}"#), Ok(None));
    assert_eq!(parse_zone(503, b"busy"), Ok(None));
    assert!(parse_zone(200, b"<html>").is_err(), "a bad reply is offline: the zone is kept");
    assert!((km((46.6, -112.0), (46.6, -111.0)) - 76.4).abs() < 0.5);
}

/// The real lookup's HTTP client builds (TLS setup included); nothing is sent.
#[tokio::test]
async fn open_meteo_client_builds_without_sending() {
    let lookup = open_meteo();
    drop(lookup);
}

/// py: tests/test_core.py::test_fridge_off_silences_warm_alert_and_rearms_when_cold
#[test]
fn test_fridge_off_silences_warm_alert_and_rearms_when_cold() {
    let dir = tempfile::tempdir().unwrap();
    let s = Arc::new(StateStore::new());
    let prefs = Mutex::new(Prefs::load(s.clone(), dir.path().join("settings.json")));
    s.update([
        ("ui.fridge_max_c", json!(5.0)),
        ("ui.freezer_max_c", json!(-12.0)),
        ("climate.fridge.temp", json!(21.0)),
        ("climate.freezer.temp", json!(21.0)),
    ]);
    let cold = |s: &dyn StateView| -> BTreeSet<String> { r_cold(s, &Ctx::default()).unwrap().into_iter().map(|f| f.id).collect() };
    assert_eq!(cold(&*s), BTreeSet::from(["cold.fridge".to_string(), "cold.freezer".to_string()]), "warm → alerts, never guessed off");
    prefs.lock().unwrap().set("fridge_off", &json!(true)).unwrap(); // owner: "fridge is off"
    assert_eq!(cold(&*s), BTreeSet::from(["cold.freezer".to_string()]));
    assert!(ColdRearm::check(&*s, &prefs).is_empty() && s.get("settings.fridge_off") == Some(json!(true)), "still warm: stays off");
    s.set("climate.fridge.temp", json!(4.5));
    assert_eq!(ColdRearm::check(&*s, &prefs), ["fridge"]);
    assert_eq!(s.get("settings.fridge_off"), Some(json!(false)), "cold again: re-armed");
    s.set("climate.fridge.temp", json!(8.0));
    assert!(cold(&*s).contains("cold.fridge"));
}

#[test]
fn ui_keys_from_the_config() {
    let cfg = Cfg::parse("[alerts]\nbms_expected_modules = 4\n[ui]\nfridge_max_c = 4\n").unwrap();
    assert_eq!(
        ui_keys(&cfg),
        vec![
            ("ui.bms_expected".to_string(), json!(4)),
            ("ui.fridge_max_c".to_string(), json!(4.0)),
            ("ui.freezer_max_c".to_string(), json!(-12.0)),
        ]
    );
}

/// The service on a private broker: `pref` over the bus with Python's error texts, the `ui.*` keys, and the
/// re-arm from the Cerbo's fridge temperature.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_pref_command_and_rearm_over_the_bus() {
    let broker = Broker::start(false);
    let dir = tempfile::tempdir().unwrap();
    let vst = Arc::new(StateStore::new());
    let _vic = Bus::start(BusOptions::new("victron").socket(&broker.socket), vst.clone());
    vst.set("climate.fridge.temp", json!(9.0));
    let no_internet: Lookup = Arc::new(|_, _| Box::pin(async { Err("no network in tests".to_string()) }));
    let opts = Options {
        bus_socket: Some(broker.socket.clone()),
        prefs_path: dir.path().join("settings.json"),
        admin_socket: dir.path().join("no-admin.sock"),
        tz_interval: Duration::from_secs(60),
        cold_period: Duration::from_millis(100),
        lookup: Some(no_internet),
    };
    let svc = tokio::spawn(aetherrv_settings::run(Cfg::parse("[ui]\nfridge_max_c = 5.0\n").unwrap(), opts));
    let ui = Bus::start(BusOptions::new("gateway").socket(&broker.socket), Arc::new(StateStore::new()));
    ui.subscribe_state("settings", "#").await;
    let view = ui.remote().clone();
    assert!(eventually(Duration::from_secs(10), || view.owner_ok("settings") && view.get("ui.fridge_max_c") == Some(json!(5.0))).await);
    assert_eq!(view.get("settings.screen_off_min"), Some(json!(10)));
    let pref = |key: &str, value: Value| {
        let ui = ui.clone();
        let v = json!({"key": key, "value": value});
        async move { ui.request("settings", "pref", v, "kiosk", 3000, Duration::from_secs(5)).await }
    };
    let r = pref("screen_off_min", json!(5)).await;
    assert_eq!(r.result, ResultKind::Ok);
    assert!(eventually(Duration::from_secs(3), || view.get("settings.screen_off_min") == Some(json!(5))).await);
    let r = pref("screen_off_min", json!(999)).await;
    assert_eq!((r.result, r.detail.as_str()), (ResultKind::Refused, "screen_off_min must be 0–240"));
    let r = pref("rm", json!(1)).await;
    assert_eq!((r.result, r.detail.as_str()), (ResultKind::Refused, "unknown setting 'rm'"));
    // the owner marks the fridge off; it stays off while warm, and re-arms once the Cerbo reports it cold
    assert_eq!(pref("fridge_off", json!(true)).await.result, ResultKind::Ok);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(view.get("settings.fridge_off"), Some(json!(true)));
    vst.set("climate.fridge.temp", json!(4.0));
    assert!(eventually(Duration::from_secs(5), || view.get("settings.fridge_off") == Some(json!(false))).await);
    assert_eq!(read_prefs_file(&dir.path().join("settings.json")).unwrap()["fridge_off"], json!(false));
    svc.abort();
}
