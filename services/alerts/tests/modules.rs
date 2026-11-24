//! New (ARCHITECTURE.md §4.7): the "<module> down" rule, and the service on a private broker (ack / ack_all,
//! a module that stops).

use aetherrv_alerts::rules::{r_modules, service_rules};
use aetherrv_alerts::{AlertCfg, Alerts, Ctx, Module, Options};
use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::command::ResultKind;
use aetherrv_core::config::Cfg;
use aetherrv_core::state::{StateStore, StateView};
use aetherrv_core::testutil::{Broker, eventually};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

fn m(name: &str, ok: bool, age: Option<f64>) -> Module {
    Module { name: name.into(), ok, age_s: age }
}

#[test]
fn module_down_findings_severity_and_text() {
    let c = Ctx {
        modules: vec![
            m("espar", false, Some(1.0)),       // reported itself stopped
            m("gps", false, Some(40.0)),        // heartbeat stale
            m("netmon", true, Some(0.5)),       // fine
            m("alerts", false, Some(30.0)),     // never judges itself
            m("mosquitto-thing", false, None),  // not an AetherRV service
            m("can-aux-fd0", false, Some(9.0)), // config-dependent instance
            m("power-guard", false, None),
            m("rixens", false, Some(7.0)),
            m("n2k", false, Some(7.0)),
        ],
        ..Ctx::default()
    };
    let f = r_modules(&StateStore::new(), &c).unwrap();
    let got: Vec<(&str, &str)> = f.iter().map(|x| (x.id.as_str(), x.severity.as_str())).collect();
    assert_eq!(
        got,
        [
            ("svc.down.espar", "serious"),
            ("svc.down.gps", "warning"),
            ("svc.down.can-aux-fd0", "warning"),
            ("svc.down.power-guard", "serious"),
            ("svc.down.rixens", "serious"),
            ("svc.down.n2k", "serious"),
        ]
    );
    assert_eq!(f[0].title, "espar module down");
    assert!(f[0].detail.starts_with("No heartbeat from aetherrv-espar on the bus (stopped, crashed or hung)."), "{}", f[0].detail);
    assert!(f[1].detail.starts_with("No heartbeat from aetherrv-gps on the bus"), "{}", f[1].detail);
    assert!(f[2].detail.contains("aetherrv-can-aux@fd0"));
    assert!(f.iter().all(|x| x.source == "system"));
}

#[test]
fn service_rules_are_python_rules_plus_modules_with_unique_names() {
    let cfg = AlertCfg::default();
    let names: Vec<String> = service_rules(&cfg).into_iter().map(|r| r.name).collect();
    let set: BTreeSet<&String> = names.iter().collect();
    assert_eq!(set.len(), names.len(), "rules are matched to their alerts by name");
    assert_eq!(names.len(), 18);
    let modules = service_rules(&cfg).into_iter().find(|r| r.name == "modules").unwrap();
    assert_eq!((modules.for_s, modules.clear_s), (15.0, 5.0));
}

#[test]
fn module_down_raises_after_15s_and_a_restart_blip_never_does() {
    let s = Arc::new(StateStore::new());
    let cfg = AlertCfg::default();
    let mut al = Alerts::new(s.clone(), None, cfg.clone(), Some(service_rules(&cfg)));
    let ids = |al: &Alerts| -> Vec<String> {
        al.snapshot().as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap().to_string()).collect()
    };
    // a restart: down for 3 s, then up again
    al.modules = vec![m("espar", false, Some(0.5))];
    al.evaluate(0.0);
    al.evaluate(3.0);
    al.modules = vec![m("espar", true, Some(0.2))];
    al.evaluate(4.0);
    al.modules = vec![m("espar", false, Some(0.5))];
    al.evaluate(10.0);
    al.evaluate(24.0);
    assert!(ids(&al).is_empty(), "the 15 s restarts at every blip");
    al.evaluate(25.0);
    assert_eq!(ids(&al), ["svc.down.espar"]);
    assert_eq!(al.snapshot()[0]["severity"], "serious");
    // back: cleared after clear_s (5 s)
    al.modules = vec![m("espar", true, Some(0.1))];
    al.evaluate(26.0);
    assert_eq!(ids(&al), ["svc.down.espar"]);
    al.evaluate(31.0);
    assert!(ids(&al).is_empty());
}

fn bus(broker: &Broker, name: &str) -> (Arc<StateStore>, Bus) {
    let st = Arc::new(StateStore::new());
    let b = Bus::start(BusOptions::new(name).socket(&broker.socket), st.clone());
    (st, b)
}

fn active(view: &dyn StateView) -> Vec<Value> {
    view.get("alerts.active").and_then(|v| v.as_array().cloned()).unwrap_or_default()
}

fn find(view: &dyn StateView, id: &str) -> Option<Value> {
    active(view).into_iter().find(|r| r["id"] == id)
}

/// The service against a private broker: findings from another service's keys, ack / ack_all over the bus, the
/// alert log in the shared file, and a module that stops cleanly → "gps module down" after 15 s.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_on_the_bus_acks_and_module_down() {
    let broker = Broker::start(false);
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("history.sqlite3");
    // a GPS service that was up and then stopped cleanly (retained status up=false); its own runtime ends with it,
    // or its bus client would reconnect
    let sock = broker.socket.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let g = Bus::start(BusOptions::new("gps").socket(&sock), Arc::new(StateStore::new()));
            assert!(eventually(Duration::from_secs(5), || g.connected()).await);
            g.shutdown().await;
        });
        drop(rt);
    })
    .join()
    .unwrap();
    // the Cerbo has an unacknowledged alarm
    let (vst, vic) = bus(&broker, "victron");
    vst.set(
        "victron.notifications",
        json!([{"index": 3, "active": true, "type": 1, "description": "Low battery", "device": "", "acknowledged": false}]),
    );
    let opts = Options { bus_socket: Some(broker.socket.clone()), db_path: Some(db.clone()), settle: Duration::from_millis(300) };
    let svc = tokio::spawn(aetherrv_alerts::run(Cfg::empty(), opts));
    let (_ust, ui) = bus(&broker, "gateway");
    ui.subscribe_state("alerts", "alerts.active").await;
    ui.subscribe_state("alerts", "alerts.log").await;
    let view = ui.remote().clone();
    assert!(eventually(Duration::from_secs(10), || find(&*view, "cerbo.notif.3").is_some()).await, "cerbo alarm raised");
    assert!(eventually(Duration::from_secs(5), || view.owner_ok("alerts")).await);

    let r = ui.request("alerts", "ack", json!("cerbo.notif.3"), "kiosk", 3000, Duration::from_secs(5)).await;
    assert_eq!((r.result, r.detail.as_str()), (ResultKind::Ok, "acknowledged"));
    assert!(eventually(Duration::from_secs(3), || find(&*view, "cerbo.notif.3").is_some_and(|a| a["acked"] == true)).await);
    let r = ui.request("alerts", "ack", json!({"id": "cerbo.notif.3"}), "kiosk", 3000, Duration::from_secs(5)).await;
    assert_eq!((r.result, r.detail.as_str()), (ResultKind::Ok, "already acknowledged"));
    let r = ui.request("alerts", "ack", json!("nope"), "kiosk", 3000, Duration::from_secs(5)).await;
    assert_eq!((r.result, r.detail.as_str()), (ResultKind::Refused, "no active alert nope"));
    let r = ui.request("alerts", "ack_all", json!(null), "kiosk", 3000, Duration::from_secs(5)).await;
    assert_eq!((r.result, r.detail.as_str()), (ResultKind::Ok, "0 acknowledged"));

    // the stopped GPS module: raised after 15 s down (it was down before the engine started)
    assert!(find(&*view, "svc.down.gps").is_none(), "not before 15 s");
    assert!(eventually(Duration::from_secs(25), || find(&*view, "svc.down.gps").is_some()).await, "gps module down raised");
    let a = find(&*view, "svc.down.gps").unwrap();
    assert_eq!((a["severity"].clone(), a["title"].clone()), (json!("warning"), json!("gps module down")));
    assert!(find(&*view, "svc.down.victron").is_none(), "a healthy module isn't down");

    // the events reached the shared file
    assert!(eventually(Duration::from_secs(8), || {
        view.get("alerts.log").and_then(|v| v.as_array().cloned()).is_some_and(|l| l.iter().any(|e| e["event"] == "acked"))
    })
    .await);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let n: i64 = conn.query_row("SELECT count(*) FROM alert_log WHERE id = 'cerbo.notif.3'", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 2, "raised + acked");
    drop(vic);
    svc.abort();
}

/// A broker restart (ARCHITECTURE.md §11: alerts pause): an acked alert stays active and acked, nothing is logged
/// as cleared, and no service is reported down, with the production settle time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn broker_restart_neither_clears_nor_reports_modules_down() {
    let mut broker = Broker::start(false);
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("history.sqlite3");
    let (nst, _nm) = bus(&broker, "netmon");
    nst.set(
        "netmon.targets",
        json!([{"name": "IPG100", "host": "192.0.2.5", "tcp": 6544, "up": false, "rtt_ms": null, "tcp_ok": false,
                "reason": "no response", "severity": "serious", "hint": "", "down_for": 120}]),
    );
    let opts = Options { bus_socket: Some(broker.socket.clone()), db_path: Some(db.clone()), ..Options::from_config(&Cfg::empty()) };
    assert_eq!(opts.settle, Duration::from_secs(5));
    let svc = tokio::spawn(aetherrv_alerts::run(Cfg::empty(), opts));
    let (_ust, ui) = bus(&broker, "gateway");
    ui.subscribe_state("alerts", "alerts.active").await;
    let view = ui.remote().clone();
    assert!(eventually(Duration::from_secs(15), || find(&*view, "net.IPG100").is_some()).await, "netmon alert raised");
    assert!(eventually(Duration::from_secs(5), || view.owner_ok("alerts")).await);
    let r = ui.request("alerts", "ack", json!("net.IPG100"), "kiosk", 3000, Duration::from_secs(5)).await;
    assert_eq!(r.result, ResultKind::Ok);

    broker.kill();
    tokio::time::sleep(Duration::from_secs(8)).await; // past the 6 s heartbeat limit: every owner looks down
    broker.restart();
    assert!(eventually(Duration::from_secs(10), || view.owner_ok("alerts") && view.owner_ok("netmon")).await, "back on the bus");
    tokio::time::sleep(Duration::from_secs(7)).await; // past the settle: evaluating again
    let a = find(&*view, "net.IPG100").expect("still active");
    assert_eq!(a["acked"], true, "still acknowledged");
    let conn = rusqlite::Connection::open(&db).unwrap();
    let n = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(n("SELECT count(*) FROM alert_log WHERE event = 'cleared'"), 0);
    assert_eq!(n("SELECT count(*) FROM alert_log WHERE id LIKE 'svc.down.%'"), 0);
    assert_eq!(n("SELECT count(*) FROM alert_log WHERE id = 'net.IPG100'"), 2, "raised + acked, nothing else");
    svc.abort();
}
