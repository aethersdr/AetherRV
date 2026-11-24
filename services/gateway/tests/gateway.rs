//! The gateway: HTTP behaviour, who may control, and the flat view (tests/test_core.py and tests/test_settings.py
//! ported, plus flat-view tests against a private broker).

use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::command::{BoxFuture, Reply};
use aetherrv_core::state::StateStore;
use aetherrv_core::testutil::{Broker, eventually};
use aetherrv_gateway::auth::{Admin, RemoteAuth};
use aetherrv_gateway::flat::Flat;
use aetherrv_gateway::{App, Commands, Conn, router};
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tower::ServiceExt;

#[derive(Default)]
struct FakeAdmin {
    calls: Mutex<Vec<(String, Value)>>,
}
impl Admin for FakeAdmin {
    fn call(&self, verb: &str, args: Value, _t: Duration) -> BoxFuture<Result<Value, String>> {
        self.calls.lock().unwrap().push((verb.to_string(), args));
        Box::pin(async { Ok(json!({"ok": true})) })
    }
}

#[derive(Default)]
struct FakeCmds {
    calls: Mutex<Vec<(String, String, Value, String)>>,
}
impl Commands for FakeCmds {
    fn request(&self, device: &str, cmd: &str, value: Value, client: &str, _t: Duration) -> BoxFuture<Reply> {
        self.calls.lock().unwrap().push((device.into(), cmd.into(), value, client.into()));
        Box::pin(async { Reply::ok("ok") })
    }
}

fn app(dir: &std::path::Path, admin: Arc<FakeAdmin>, cmds: Arc<FakeCmds>) -> Arc<App> {
    Arc::new(App {
        flat: Arc::new(Flat::new(json!({}))),
        auth: RemoteAuth::new(Box::new(admin.clone()), &[], "owner"),
        admin,
        cmds,
        static_dir: dir.to_path_buf(),
        history_db: dir.join("h.sqlite3"),
        weather_sock: dir.join("weather.sock"),
        coalesce: Duration::from_millis(33),
    })
}

fn req(method: &str, path: &str, from: &str, body: Option<Value>) -> Request<Body> {
    let conn = Conn { remote: format!("{from}:40000").parse().unwrap(), local: Some("127.0.0.1:8080".parse().unwrap()) };
    let b = Request::builder().method(method).uri(path).extension(ConnectInfo(conn));
    match body {
        Some(v) => b.header("content-type", "application/json").body(Body::from(v.to_string())).unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

async fn json_of(r: axum::response::Response) -> Value {
    let b = http_body_util::BodyExt::collect(r.into_body()).await.unwrap().to_bytes();
    serde_json::from_slice(&b).unwrap_or(Value::Null)
}

/// py: tests/test_core.py::test_ui_is_not_cached
#[tokio::test]
async fn test_ui_is_not_cached() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("index.html"), "<html></html>").unwrap();
    std::fs::write(d.path().join("app.js"), "//").unwrap();
    let a = app(d.path(), Arc::default(), Arc::default());
    for path in ["/", "/static/app.js"] {
        let r = router(a.clone()).oneshot(req("GET", path, "127.0.0.1", None)).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK, "{path}");
        assert_eq!(r.headers()["cache-control"], "no-cache");
    }
    let r = router(a).oneshot(req("GET", "/static/../secret", "127.0.0.1", None)).await.unwrap();
    assert_ne!(r.status(), StatusCode::OK, "no path traversal");
}

/// py: tests/test_settings.py::test_settings_actions_loopback_only
#[tokio::test]
async fn test_settings_actions_loopback_only() {
    let d = tempfile::tempdir().unwrap();
    let (adm, cmds) = (Arc::new(FakeAdmin::default()), Arc::new(FakeCmds::default()));
    let a = app(d.path(), adm.clone(), cmds.clone());
    let r = router(a.clone()).oneshot(req("POST", "/api/settings/action", "127.0.0.1", Some(json!({"verb": "reboot"})))).await.unwrap();
    assert_eq!(json_of(r).await["ok"], true);
    assert_eq!(adm.calls.lock().unwrap().last().unwrap(), &("reboot".to_string(), json!({})));
    let r = router(a.clone())
        .oneshot(req("POST", "/api/settings/action", "127.0.0.1", Some(json!({"verb": "set_timezone", "args": {"tz": "UTC"}}))))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN, "time zone is automatic only");
    let r = router(a.clone())
        .oneshot(req("POST", "/api/settings/action", "127.0.0.1", Some(json!({"verb": "pref", "args": {"key": "screen_off_min", "value": 5}}))))
        .await
        .unwrap();
    assert_eq!(json_of(r).await["ok"], true);
    assert_eq!(cmds.calls.lock().unwrap().last().unwrap(),
               &("settings".to_string(), "pref".to_string(), json!({"key": "screen_off_min", "value": 5}), "kiosk".to_string()),
               "the pref goes to the settings service");
    // a LAN viewer
    let n = adm.calls.lock().unwrap().len();
    let r = router(a).oneshot(req("POST", "/api/settings/action", "192.0.2.20", Some(json!({"verb": "reboot"})))).await.unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    assert_eq!(adm.calls.lock().unwrap().len(), n, "nothing reached the helper");
}

struct WhoisAdmin {
    table: Value,
    calls: Mutex<Vec<String>>,
}
impl Admin for WhoisAdmin {
    fn call(&self, _verb: &str, args: Value, _t: Duration) -> BoxFuture<Result<Value, String>> {
        let ip = args["ip"].as_str().unwrap().to_string();
        self.calls.lock().unwrap().push(ip.clone());
        let mut r = json!({"ok": true, "self_ips": ["100.64.0.10", "fd7a:115c:a1e0::10"]});
        let entry = self.table.get(&ip).cloned().unwrap_or(json!({"ok": true, "known": false}));
        for (k, v) in entry.as_object().unwrap() {
            r[k] = v.clone();
        }
        Box::pin(async move { Ok(r) })
    }
}

/// py: tests/test_settings.py::test_remote_auth_owner_only
#[tokio::test]
async fn test_remote_auth_owner_only() {
    let adm = Arc::new(WhoisAdmin {
        table: json!({
            "100.101.1.1": {"known": true, "owner": true, "login": "me@x", "node": "macbook", "tagged": false},
            "100.101.1.2": {"known": true, "owner": false, "login": "friend@y", "node": "phone", "tagged": false},
            "100.101.1.3": {"known": true, "owner": false, "login": "tagged-devices", "node": "srv", "tagged": true}}),
        calls: Mutex::new(vec![]),
    });
    let a = RemoteAuth::new(Box::new(adm.clone()), &[], "owner");
    assert_eq!(a.who(Some("127.0.0.1"), None).await.as_deref(), Some("kiosk"));
    assert_eq!(a.who(Some("192.0.2.20"), None).await, None);
    assert!(adm.calls.lock().unwrap().is_empty(), "LAN: never even asks");
    assert_eq!(a.who(Some("100.101.1.1"), Some("100.64.0.10")).await.as_deref(), Some("tailscale:me@x@macbook"));
    assert_eq!(a.who(Some("100.101.1.2"), Some("100.64.0.10")).await, None, "shared-in user");
    assert_eq!(a.who(Some("100.101.1.3"), Some("100.64.0.10")).await, None, "tagged node");
    assert_eq!(a.who(Some("100.101.1.1"), Some("192.0.2.143")).await, None, "100.x source, not via tailscale0");
    let n = adm.calls.lock().unwrap().len();
    a.who(Some("100.101.1.1"), Some("100.64.0.10")).await;
    assert_eq!(adm.calls.lock().unwrap().len(), n, "cached");
}

/// py: tests/test_settings.py::test_remote_auth_trusted_lans_and_whole_tailnet
#[tokio::test]
async fn test_remote_auth_trusted_lans_and_whole_tailnet() {
    let adm = Arc::new(WhoisAdmin {
        table: json!({"100.101.1.2": {"known": true, "owner": false, "login": "friend@y", "node": "phone", "tagged": false},
                      "100.101.1.9": {"ok": true, "known": false}}),
        calls: Mutex::new(vec![]),
    });
    let a = RemoteAuth::new(Box::new(adm), &["10.10.110.0/24".into(), "10.20.50.0/24".into()], "all");
    assert_eq!(a.who(Some("10.10.110.170"), None).await.as_deref(), Some("lan:10.10.110.170"));
    assert_eq!(a.who(Some("10.20.50.20"), None).await.as_deref(), Some("lan:10.20.50.20"));
    assert_eq!(a.who(Some("192.168.1.5"), None).await, None, "other LANs stay view-only");
    assert_eq!(a.who(Some("100.101.1.2"), Some("100.64.0.10")).await.as_deref(), Some("tailscale:friend@y@phone"), "any tailnet peer");
    assert_eq!(a.who(Some("100.101.1.9"), Some("100.64.0.10")).await, None, "unknown to tailscaled");
    assert_eq!(a.who(Some("100.101.1.2"), Some("192.0.2.143")).await, None, "must arrive via tailscale0");
}

/// The flat view: today's keys from the services; a down owner's keys read null; tx dicts rebuilt (§4.4, §4.5).
#[tokio::test(flavor = "multi_thread")]
async fn flat_view_nulls_a_down_owner_and_rebuilds_tx_dicts() {
    let b = Broker::start(false);
    let gw = Bus::start(BusOptions::new("gateway").socket(&b.socket), Arc::new(StateStore::new()));
    gw.subscribe_all_state().await;
    let flat = Arc::new(Flat::new(json!({"espar": true, "n2k": false})));
    tokio::spawn(aetherrv_gateway::follow(gw.clone(), flat.clone()));
    let es = Bus::start(BusOptions::new("espar").socket(&b.socket), Arc::new(StateStore::new()));
    es.state().update([("climate.espar.state", json!("heating")), ("tx.blocked.espar", json!(2)), ("tx.blocked.espar_diag", json!(0))]);
    let s = Duration::from_secs(5);
    assert!(eventually(s, || flat.get("climate.espar.state") == Some(json!("heating"))).await);
    assert!(eventually(s, || flat.get("tx.blocked") == Some(json!({"espar": 2}))).await);
    assert_eq!(flat.get("tx.enabled"), Some(json!({"espar": true, "n2k": false})));
    assert!(eventually(s, || flat.get("svc.espar.up") == Some(json!(true))).await);
    es.shutdown().await; // the module goes down: its keys read null, never a stale "heating"
    assert!(eventually(s, || flat.get("climate.espar.state") == Some(Value::Null)).await);
    assert!(eventually(s, || flat.get("svc.espar.up") == Some(json!(false))).await);
}
