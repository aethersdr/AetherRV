//! The Peplink half of `tests/test_connectivity.py`, plus the same paths against the router simulator.

use aetherrv_core::cmdlog::CommandLog;
use aetherrv_core::command::ResultKind;
use aetherrv_core::state::{StateExt, StateStore};
use aetherrv_core::txgate::TxGate;
use aetherrv_peplink::*;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// Shapes follow the Peplink Router API doc (fw 8.6.0) examples / object tables.
fn wan_resp() -> Value {
    json!({
        "1": {"name": "Starlink", "enable": true, "message": "Connected", "uptime": 3600, "type": "ethernet",
              "virtualType": "ethernet", "priority": 1, "ip": "100.64.0.2", "statusLed": "green"},
        "2": {"name": "Cellular", "enable": true, "message": "Connected", "uptime": 7200, "type": "cellular",
              "virtualType": "cellular", "priority": 2, "statusLed": "green",
              "cellular": {"mobileType": "5G", "signalLevel": 4, "carrierAggregation": true,
                           "carrier": {"name": "Verizon", "country": "US"},
                           "roamingStatus": {"code": 1, "message": "home"},
                           "imei": "SECRET-IMEI",
                           "sim": {"order": [1, 2], "1": {"status": "In Use", "active": true, "iccid": "SECRET-ICCID", "imsi": "SECRET-IMSI", "password": "SECRET-APN"},
                                   "2": {"status": "No SIM Card Detected", "active": false}},
                           "rat": [{"name": "LTE", "band": [{"name": "B66", "channel": 66786, "signal": {"rsrp": -101, "rsrq": -11, "sinr": 9.5, "rssi": -70}}]},
                                   {"name": "5G NR", "band": [{"name": "n77", "channel": 650000, "signal": {"rsrp": -95, "rsrq": -10, "sinr": 14.0}}]}]}},
        "3": {"name": "WAN2", "enable": true, "message": "No Cable Detected", "type": "ethernet", "statusLed": "red", "priority": 3},
        "order": [1, 2, 3],
    })
}

/// py: tests/test_connectivity.py::test_normalize_wans_and_active
#[test]
fn test_normalize_wans_and_active() {
    let mut wans = normalize_wans(&wan_resp());
    let names: Vec<&str> = wans.iter().map(|w| w["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Starlink", "Cellular", "WAN2"]);
    assert!(wans[0]["starlink"] == json!(true) && wans[0]["connected"] == json!(true) && wans[2]["connected"] == json!(false));
    let c = &wans[1]["cellular"];
    assert!(c["carrier"] == "Verizon" && c["network"] == "5G" && c["sim"] == 1 && c["roaming"] == "home");
    assert_eq!((c["rat"].clone(), c["band"].clone(), c["rsrp"].clone(), c["sinr"].clone(), c["bands"].clone()), (json!("5G NR"), json!("n77"), json!(-95), json!(14.0), json!(2))); // strongest band
    assert_eq!(active_wan(&wans).unwrap()["name"], "Starlink");
    wans[0]["connected"] = json!(false);
    assert_eq!(active_wan(&wans).unwrap()["name"], "Cellular");
}

/// py: tests/test_connectivity.py::test_no_identifiers_leak
#[test]
fn test_no_identifiers_leak() {
    let blob = serde_json::to_string(&normalize_wans(&wan_resp())).unwrap();
    assert!(!blob.contains("SECRET"));
    assert!(!blob.contains("100.64.0.2"), "the WAN IP isn't whitelisted either");
}

/// py: tests/test_connectivity.py::test_allowance_shapes
#[test]
fn test_allowance_shapes() {
    let resp = json!({"2": {"1": {"enable": true, "usage": 41000, "limit": 50000, "percent": 82, "start": 1, "unit": "MB"},
                            "2": {"enable": false}, "order": [1, 2]},
                      "1": {"enable": true, "usage": 5, "limit": 1024, "percent": 0, "start": 1, "unit": "MB"},
                      "order": [1, 2]});
    assert_eq!(
        normalize_allowance(&resp),
        vec![
            json!({"conn": 1, "sim": null, "usage_mb": 5, "limit_mb": 1024, "percent": 0, "start_day": 1}),
            json!({"conn": 2, "sim": 1, "usage_mb": 41000, "limit_mb": 50000, "percent": 82, "start_day": 1})
        ]
    );
}

fn kv(d: &[(String, Value)], k: &str) -> Value {
    d.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone()).unwrap()
}

/// py: tests/test_connectivity.py::test_location_fix_and_nofix
#[test]
fn test_location_fix_and_nofix() {
    let d = normalize_location(&json!({"gps": true, "location": {"latitude": 45.5, "longitude": -122.6, "altitude": 55.1,
                                                                 "speed": 0.02, "heading": 356.9, "hdop": 1, "timestamp": 1311972720}}));
    assert!(kv(&d, "location.fix") == json!(true) && kv(&d, "location.lat") == json!(45.5) && kv(&d, "location.source") == "peplink");
    assert_eq!(kv(&normalize_location(&json!({"gps": false, "location": {}})), "location.lat"), Value::Null);
}

/// py: tests/test_connectivity.py::test_peplink_bandwidth_per_wan
#[test]
fn test_peplink_bandwidth_per_wan() {
    let r = json!({"bandwidth": {"timestamp": 1, "unit": "kbps",
                                 "1": {"name": "Starlink", "overall": {"download": 8, "upload": 13}},
                                 "4": {"name": "WWAN on 5 GHz", "overall": {"download": 497, "upload": 38}}},
                   "lifetime": {"unit": "MB"}});
    assert_eq!(
        normalize_bandwidth(&r),
        vec![(1, json!({"down_mbps": 0.008, "up_mbps": 0.013})), (4, json!({"down_mbps": 0.497, "up_mbps": 0.038}))]
    );
    assert_eq!(normalize_bandwidth(&json!({})), vec![]);
}

/// py: tests/test_connectivity.py::test_peplink_wifi_wan_fields
#[test]
fn test_peplink_wifi_wan_fields() {
    let resp = json!({"order": [3, 4],
        "3": {"name": "WWAN on 2.4 GHz", "enable": true, "statusLed": "flash", "message": "Scanning...", "type": "wifi"},
        "4": {"name": "WWAN on 5 GHz", "enable": true, "statusLed": "green", "message": "Connected to PTP",
              "type": "wifi", "wifi": {"signal": {"strength": 0, "level": 5}, "ssid": "PTP", "bssid": "x", "channel": 149}}});
    let w = normalize_wans(&resp);
    let (w3, w4) = (&w[0], &w[1]);
    assert_eq!(w4["wifi"], json!({"ssid": "PTP", "strength": 0, "level": 5, "channel": 149, "band": "5 GHz"}));
    assert!(w4["wifi"].get("bssid").is_none());
    assert!(w3["wifi"]["ssid"].is_null() && w3["wifi"]["band"].is_null()); // scanning: still marked as Wi-Fi
}

fn router_prio() -> Value {
    // real router shape
    json!({"1": {"name": "Starlink", "group": 1, "priority": 3, "enable": true},
           "4": {"name": "WWAN on 5 GHz", "group": 1, "priority": 1, "enable": true},
           "5": {"name": "VLAN WAN 1", "priority": 1, "enable": false}, "order": [1, 4, 5]})
}

/// py: tests/test_connectivity.py::test_peplink_priority_payload_strict
#[test]
fn test_peplink_priority_payload_strict() {
    assert_eq!(priority_payload(&router_prio(), 1, 1).unwrap(), json!({"instantActive": true, "list": [{"connId": 1, "priority": 1}]}));
    for bad in [json!({}), json!({"order": []}), json!({"list": [{"connId": 1}]}), json!({"order": ["1"]}), json!([])] {
        assert!(priority_payload(&bad, 1, 1).is_err(), "{bad}");
    }
    for (conn, prio) in [(9, 1), (5, 2), (1, 5)] {
        // unknown, disabled on the router, out of range
        assert!(priority_payload(&router_prio(), conn, prio).is_err());
    }
}

/// py: tests/test_connectivity.py::test_peplink_layout_targets_strict
#[test]
fn test_peplink_layout_targets_strict() {
    let mut cur = json!({"order": [1, 2, 4, 5]});
    for (c, p) in [(1, 3), (2, 2), (4, 1), (5, 1)] {
        cur[c.to_string()] = json!({"name": format!("W{c}"), "priority": p, "enable": c != 5});
    }
    let t = layout_targets(&cur, &[4, 1, 2], None).unwrap(); // disabled omitted: 5 stays disabled
    assert_eq!(t, vec![(4, Target::on(1)), (1, Target::on(2)), (2, Target::on(3)), (5, Target::OFF)]);
    assert_eq!(
        layout_payload(&cur, &layout_changes(&cur, &t).unwrap()).unwrap(),
        json!({"instantActive": true, "list": [{"connId": 1, "priority": 2}, {"connId": 2, "priority": 3}]})
    );
    // enable 5 at P2, disable 2
    let t = layout_targets(&cur, &[4, 5, 1], Some(&[2])).unwrap();
    assert_eq!(
        layout_payload(&cur, &layout_changes(&cur, &t).unwrap()).unwrap()["list"],
        json!([{"connId": 5, "priority": 2, "enable": true}, {"connId": 2, "enable": false}]) // 1 is already P3: not sent
    );
    let bad: [(&[i64], Option<&[i64]>); 7] = [
        (&[4, 1], None),
        (&[4, 1, 2, 5], None),
        (&[4, 4, 1, 2], None),
        (&[], Some(&[1, 2, 4, 5])),
        (&[4, 1, 9], None),
        (&[4, 1], Some(&[5])),
        (&[4, 1, 2], Some(&[5, 2])),
    ];
    for (o, d) in bad {
        assert!(layout_targets(&cur, o, d).is_err(), "{o:?} {d:?}");
    }
    // a hidden, already-disabled connection (5) may be omitted: it stays disabled, nothing sent for it
    let t = layout_targets(&cur, &[4, 1], Some(&[2])).unwrap();
    assert!(t.contains(&(5, Target::OFF)) && !layout_changes(&cur, &t).unwrap().iter().any(|(c, _)| *c == 5));
}

/// py: tests/test_connectivity.py::test_peplink_write_needs_explicit_writer
#[tokio::test]
async fn test_peplink_write_needs_explicit_writer() {
    let ro = PeplinkClient::new(ClientConfig { username: "ro".into(), password: "x".into(), ..ClientConfig::new("192.0.2.1") });
    let e = ro.post_once_raw(PRIORITY_FUNC, &json!({})).await.unwrap_err(); // monitoring client can never write
    assert!(matches!(e, PepError::Api(_)));
    let api_ro = PeplinkClient::new(ClientConfig { client_id: "id".into(), client_secret: "sec".into(), allow_write: true, ..ClientConfig::new("192.0.2.1") }); // read-only scope
    assert!(matches!(api_ro.post_once_raw(PRIORITY_FUNC, &json!({})).await, Err(PepError::Api(_))));
}

// ── the client against Python's aiohttp fixtures (same handlers) ───────────────────────────────────────────────

mod fixtures {
    use super::*;
    use axum::Json;
    use axum::extract::{Query, State};
    use axum::http::{HeaderMap, header};
    use axum::response::IntoResponse;
    use std::collections::HashMap;

    #[derive(Default)]
    pub struct Calls {
        pub grant: AtomicU32,
        pub login: AtomicU32,
    }

    async fn serve(app: axum::Router) -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = l.local_addr().unwrap().to_string();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        host
    }

    /// Python `fake_router`: API-client grants (tok1, tok2, …) and a token-checked status.wan.connection.
    pub async fn fake_router() -> (String, Arc<Calls>) {
        let calls = Arc::new(Calls::default());
        async fn grant(State(c): State<Arc<Calls>>, Json(body): Json<Value>) -> Json<Value> {
            let n = c.grant.fetch_add(1, Ordering::SeqCst) + 1;
            if body["clientSecret"] != "s3cret" || body["scope"] != "api.read-only" {
                return Json(json!({"stat": "fail", "code": 401, "message": "Unauthorized"}));
            }
            Json(json!({"stat": "ok", "response": {"accessToken": format!("tok{n}"), "expiresIn": 172800}}))
        }
        async fn wan(State(c): State<Arc<Calls>>, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
            if q.get("accessToken").map(String::as_str) != Some(&format!("tok{}", c.grant.load(Ordering::SeqCst))) {
                return Json(json!({"stat": "fail", "code": 401, "message": "Unauthorized"}));
            }
            Json(json!({"stat": "ok", "response": wan_resp()}))
        }
        let app = axum::Router::new()
            .route("/api/auth.token.grant", axum::routing::post(grant))
            .route("/api/status.wan.connection", axum::routing::get(wan))
            .with_state(calls.clone());
        (serve(app).await, calls)
    }

    /// Python `fake_router_login`: web-user login setting the `bauth` cookie (admin has POST permission).
    pub async fn fake_router_login() -> (String, Arc<Calls>) {
        let calls = Arc::new(Calls::default());
        async fn login(State(c): State<Arc<Calls>>, Json(body): Json<Value>) -> axum::response::Response {
            let n = c.login.fetch_add(1, Ordering::SeqCst) + 1;
            if body["password"] != "ro-pass" {
                return Json(json!({"stat": "fail", "code": 401, "message": "Unauthorized"})).into_response();
            }
            let perm = if body["username"] == "admin" { json!({"GET": 1, "POST": 1}) } else { json!({"GET": 1}) };
            ([(header::SET_COOKIE, format!("bauth=s{n}; Path=/"))], Json(json!({"stat": "ok", "response": {"permission": perm}}))).into_response()
        }
        async fn wan(State(c): State<Arc<Calls>>, h: HeaderMap) -> Json<Value> {
            let want = format!("bauth=s{}", c.login.load(Ordering::SeqCst));
            let ok = h.get_all(header::COOKIE).iter().any(|v| v.to_str().unwrap_or("").split("; ").any(|p| p == want));
            if !ok {
                return Json(json!({"stat": "fail", "code": 401, "message": "Unauthorized"}));
            }
            Json(json!({"stat": "ok", "response": wan_resp()}))
        }
        let app = axum::Router::new()
            .route("/api/login", axum::routing::post(login))
            .route("/api/status.wan.connection", axum::routing::get(wan))
            .with_state(calls.clone());
        (serve(app).await, calls)
    }
}

fn http(c: ClientConfig) -> PeplinkClient {
    PeplinkClient::new(ClientConfig { scheme: "http".into(), ..c })
}

/// py: tests/test_connectivity.py::test_client_grants_and_regrants
#[tokio::test]
async fn test_client_grants_and_regrants() {
    let (host, calls) = fixtures::fake_router().await;
    let c = http(ClientConfig { client_id: "id".into(), client_secret: "s3cret".into(), ..ClientConfig::new(&host) });
    let r = c.get_with("status.wan.connection", &[]).await.unwrap();
    assert!(r["order"] == json!([1, 2, 3]) && calls.grant.load(Ordering::SeqCst) == 1);
    c.force_token(Some("stale")).await; // router revoked/expired the token
    c.get_with("status.wan.connection", &[]).await.unwrap();
    assert_eq!(calls.grant.load(Ordering::SeqCst), 2);
}

/// py: tests/test_connectivity.py::test_client_bad_secret
#[tokio::test]
async fn test_client_bad_secret() {
    let (host, _) = fixtures::fake_router().await;
    let c = http(ClientConfig { client_id: "id".into(), client_secret: "wrong".into(), ..ClientConfig::new(&host) });
    let e = c.get_with("status.wan.connection", &[]).await.unwrap_err();
    assert!(matches!(&e, PepError::Api(m) if m == "token grant failed: 401 Unauthorized"), "{e:?}");
}

/// py: tests/test_connectivity.py::test_readonly_user_login_and_relogin
#[tokio::test]
async fn test_readonly_user_login_and_relogin() {
    let (host, calls) = fixtures::fake_router_login().await;
    let c = http(ClientConfig { username: "viewer".into(), password: "ro-pass".into(), ..ClientConfig::new(&host) });
    assert!(c.get_with("status.wan.connection", &[]).await.unwrap()["order"] == json!([1, 2, 3]) && calls.login.load(Ordering::SeqCst) == 1);
    c.clear_cookies().await; // session expired on the router
    c.get_with("status.wan.connection", &[]).await.unwrap();
    assert_eq!(calls.login.load(Ordering::SeqCst), 2);
}

/// py: tests/test_connectivity.py::test_refuses_account_with_write_permission
#[tokio::test]
async fn test_refuses_account_with_write_permission() {
    let (host, _) = fixtures::fake_router_login().await;
    let c = http(ClientConfig { username: "admin".into(), password: "ro-pass".into(), ..ClientConfig::new(&host) });
    let e = c.get_with("status.wan.connection", &[]).await.unwrap_err();
    assert!(e.to_string().contains("write"), "{e}");
}

// ── the command paths against a scripted router (Python's _FakeWriter) ─────────────────────────────────────────

struct FakeWriter {
    prio: Mutex<Vec<(i64, i64)>>,
    en: Mutex<Vec<(i64, bool)>>,
    apply: bool,
    post_raises: Option<String>,
    posts: Mutex<Vec<Value>>,
    raw: Option<Value>,
}

impl FakeWriter {
    fn new(prio: &[(i64, i64)]) -> FakeWriter {
        FakeWriter::with(prio, true, None, None, &[])
    }
    fn with(prio: &[(i64, i64)], apply: bool, post_raises: Option<&str>, raw: Option<Value>, disabled: &[i64]) -> FakeWriter {
        FakeWriter {
            prio: Mutex::new(prio.to_vec()),
            en: Mutex::new(prio.iter().map(|(k, _)| (*k, !disabled.contains(k))).collect()),
            apply,
            post_raises: post_raises.map(str::to_string),
            posts: Mutex::new(Vec::new()),
            raw,
        }
    }
    fn cfg(&self) -> Value {
        let prio = self.prio.lock().unwrap();
        let en = self.en.lock().unwrap();
        let mut m = serde_json::Map::new();
        m.insert("order".into(), json!(prio.iter().map(|(k, _)| *k).collect::<Vec<_>>()));
        for (k, p) in prio.iter() {
            let e = en.iter().find(|(c, _)| c == k).unwrap().1;
            m.insert(k.to_string(), json!({"name": format!("W{k}"), "group": 1, "enable": e, "priority": p}));
        }
        Value::Object(m)
    }
    fn posts(&self) -> Vec<Value> {
        self.posts.lock().unwrap().clone()
    }
    fn enabled(&self, c: i64) -> bool {
        self.en.lock().unwrap().iter().find(|(k, _)| *k == c).unwrap().1
    }
}

impl Router for FakeWriter {
    fn get<'a>(&'a self, func: &'a str) -> PepFut<'a, Value> {
        let v = if func == PRIORITY_FUNC { self.raw.clone().unwrap_or_else(|| self.cfg()) } else { self.cfg() };
        Box::pin(async move { Ok(v) })
    }
    fn post_once<'a>(&'a self, _func: &'a str, body: &'a Value) -> PepFut<'a, Value> {
        self.posts.lock().unwrap().push(body.clone());
        if self.apply {
            for e in body["list"].as_array().unwrap() {
                let c = e["connId"].as_i64().unwrap();
                if let Some(p) = e.get("priority") {
                    self.prio.lock().unwrap().iter_mut().find(|(k, _)| *k == c).unwrap().1 = p.as_i64().unwrap();
                }
                if let Some(en) = e.get("enable") {
                    self.en.lock().unwrap().iter_mut().find(|(k, _)| *k == c).unwrap().1 = en.as_bool().unwrap();
                }
            }
        }
        let r = self.post_raises.clone();
        Box::pin(async move { r.map_or(Ok(json!({})), |e| Err(PepError::Other(e))) })
    }
}

/// The monitoring client of the Python test (`RO`): reads through the writer.
struct Ro(Arc<FakeWriter>);

impl Router for Ro {
    fn get<'a>(&'a self, func: &'a str) -> PepFut<'a, Value> {
        self.0.get(func)
    }
    fn post_once<'a>(&'a self, _func: &'a str, _body: &'a Value) -> PepFut<'a, Value> {
        Box::pin(async { Err(PepError::Api("read-only".into())) })
    }
}

fn pep(w: &Arc<FakeWriter>, enabled: bool) -> Peplink {
    Peplink::new(
        Arc::new(StateStore::new()),
        Arc::new(Ro(w.clone())),
        Some(w.clone() as Arc<dyn Router>),
        Arc::new(TxGate::new([(DEVICE.to_string(), enabled)])),
        CommandLog::memory(DEVICE),
    )
}

/// py: tests/test_connectivity.py::test_peplink_set_priority_sends_once_and_verifies
#[tokio::test]
async fn test_peplink_set_priority_sends_once_and_verifies() {
    let w = Arc::new(FakeWriter::new(&[(1, 3), (4, 1)]));
    let a = pep(&w, true);
    assert!(a.state.is_true("peplink.controls_ok"));
    let r = a.command_with("set_wan_priority", &json!({"id": 1, "priority": 1}), "kiosk", 1.0, 0.01).await;
    assert!(r.is_ok() && w.posts() == vec![json!({"instantActive": true, "list": [{"connId": 1, "priority": 1}]})] && r.detail.contains("P1"));
    assert_eq!(r.detail, "W1 is now P1");
    let row = &a.log.rows()[0];
    assert_eq!((row.value.as_str(), row.outcome.as_str(), row.detail.as_str()), (r#"{"id": 1, "priority": 1}"#, "ok", "W1 P3→P1"));
    assert!(a.state.is_true("peplink.reachable"), "the WAN keys are refreshed after a verified change");
}

/// py: tests/test_connectivity.py::test_peplink_set_priority_refusals_send_nothing
#[tokio::test]
async fn test_peplink_set_priority_refusals_send_nothing() {
    let w = Arc::new(FakeWriter::new(&[(1, 3)]));
    assert!(!pep(&w, false).command("set_wan_priority", &json!({"id": 1, "priority": 1}), "kiosk").await.is_ok());
    let r = pep(&w, true).command("reboot", &Value::Null, "kiosk").await;
    assert!(!r.is_ok() && r.detail == "command 'reboot' is not allowed");
    assert!(!pep(&w, true).command("set_wan_priority", &json!({"id": 1}), "kiosk").await.is_ok());
    let weird = Arc::new(FakeWriter::with(&[(1, 3)], true, None, Some(json!({"weird": 1, "list": "x"})), &[]));
    let r = pep(&weird, true).command("set_wan_priority", &json!({"id": 1, "priority": 1}), "kiosk").await;
    assert!(!r.is_ok() && r.detail.starts_with("not sent: unexpected config.wan.connection.priority shape: {'weird': 1, 'list': 'x'}"), "{r:?}");
    assert_eq!(pep(&w, true).command("set_wan_priority", &json!({"id": 1, "priority": 3}), "kiosk").await.detail, "already P3");
    assert!(w.posts().is_empty() && weird.posts().is_empty());
}

/// Python's int() would take these; the Rust service refuses them (SERVICE-GUIDE: strict values).
#[tokio::test]
async fn set_priority_values_must_be_json_integers() {
    let w = Arc::new(FakeWriter::new(&[(1, 3), (4, 1)]));
    let a = pep(&w, true);
    for bad in [json!({"id": "1", "priority": 1}), json!({"id": 1, "priority": 1.5}), json!({"id": true, "priority": 1}), json!([1, 1]), json!(null)] {
        let r = a.command("set_wan_priority", &bad, "kiosk").await;
        assert!(r.result == ResultKind::Refused && r.detail == "bad request", "{bad}");
    }
    for bad in [json!({"order": "41"}), json!({"order": [4, "1"]}), json!({"order": [4, 1], "disabled": 5}), json!({"disabled": [5]})] {
        assert_eq!(a.command("set_wan_order", &bad, "kiosk").await.detail, "bad request", "{bad}");
    }
    assert!(w.posts().is_empty());
    assert_eq!(a.log.rows()[0].value, r#"{"id": "1", "priority": 1}"#);
}

/// py: tests/test_connectivity.py::test_peplink_set_priority_unconfirmed_is_not_retried
#[tokio::test]
async fn test_peplink_set_priority_unconfirmed_is_not_retried() {
    let w = Arc::new(FakeWriter::with(&[(1, 3)], false, Some("timeout"), None, &[]));
    let a = pep(&w, true);
    let r = a.command_with("set_wan_priority", &json!({"id": 1, "priority": 1}), "kiosk", 0.05, 0.01).await;
    assert!(!r.is_ok() && w.posts().len() == 1);
    assert_eq!(r.result, ResultKind::Unverified);
    assert_eq!(a.log.rows()[0].detail, "W1 P3→P1; send=error: timeout; not seen");
}

/// py: tests/test_connectivity.py::test_peplink_set_order_one_post_only_changed
#[tokio::test]
async fn test_peplink_set_order_one_post_only_changed() {
    let w = Arc::new(FakeWriter::new(&[(1, 3), (2, 2), (4, 1)]));
    let r = pep(&w, true).command_with("set_wan_order", &json!({"order": [4, 1, 2]}), "kiosk", 1.0, 0.01).await;
    assert!(r.is_ok() && w.posts().len() == 1);
    assert_eq!(w.posts()[0], json!({"instantActive": true, "list": [{"connId": 1, "priority": 2}, {"connId": 2, "priority": 3}]}));
    assert_eq!(r.detail, "W1 P3→P2, W2 P2→P3");
    assert_eq!(pep(&w, true).command("set_wan_order", &json!({"order": [4, 1, 2]}), "kiosk").await.detail, "already in that order");
    assert_eq!(w.posts().len(), 1);
}

/// py: tests/test_connectivity.py::test_peplink_drag_disable_and_enable
#[tokio::test]
async fn test_peplink_drag_disable_and_enable() {
    let w = Arc::new(FakeWriter::with(&[(1, 3), (2, 2), (4, 1), (5, 1)], true, None, None, &[5]));
    let r = pep(&w, true).command_with("set_wan_order", &json!({"order": [4, 2], "disabled": [1, 5]}), "kiosk", 1.0, 0.01).await;
    assert!(r.is_ok() && w.posts().last().unwrap()["list"] == json!([{"connId": 1, "enable": false}]) && !w.enabled(1));
    assert_eq!(r.detail, "W1 disabled");
    let r = pep(&w, true).command_with("set_wan_order", &json!({"order": [4, 1, 2], "disabled": [5]}), "kiosk", 1.0, 0.01).await;
    assert!(r.is_ok() && w.posts().last().unwrap()["list"] == json!([{"connId": 1, "priority": 2, "enable": true}, {"connId": 2, "priority": 3}]));
    assert_eq!(r.detail, "W1 enabled at P2, W2 P2→P3");
    let n = w.posts().len();
    let r = pep(&w, true).command("set_wan_order", &json!({"order": [], "disabled": [1, 2, 4, 5]}), "kiosk").await;
    assert!(!r.is_ok() && w.posts().len() == n); // never disable everything
    assert_eq!(r.detail, "not sent: at least one WAN must stay enabled");
}

#[tokio::test]
async fn no_writer_means_no_control() {
    let w = Arc::new(FakeWriter::new(&[(1, 3)]));
    let a = Peplink::new(Arc::new(StateStore::new()), Arc::new(Ro(w.clone())), None, Arc::new(TxGate::new([(DEVICE.to_string(), true)])), CommandLog::memory(DEVICE));
    assert!(a.state.is_false("peplink.controls_ok"));
    let r = a.command("set_wan_priority", &json!({"id": 1, "priority": 1}), "kiosk").await;
    assert!(r.result == ResultKind::Refused && r.detail == "WAN control isn't set up (no read-write API client)");
    assert!(w.posts().is_empty());
}

#[test]
fn json_dumps_is_pythons() {
    assert_eq!(json_dumps(&json!({"order": [4, 1], "disabled": null, "x": 1.0, "s": "\u{e9}\"\n"})), "{\"order\": [4, 1], \"disabled\": null, \"x\": 1.0, \"s\": \"\\u00e9\\\"\\n\"}");
    assert_eq!(json_dumps(&Value::Null), "null");
}

// ── against the router simulator (HTTPS, self-signed) ──────────────────────────────────────────────────────────

mod sim {
    use super::*;
    use aetherrv_sim::peplink::{PeplinkSim, PeplinkSimConfig};
    use std::time::Duration;

    fn ro_user(host: &str) -> PeplinkClient {
        PeplinkClient::new(ClientConfig { username: "viewer".into(), password: "ro-pass".into(), ..ClientConfig::new(host) })
    }
    fn rw_user(host: &str) -> PeplinkClient {
        PeplinkClient::new(ClientConfig { username: "admin".into(), password: "admin-pass".into(), allow_write: true, ..ClientConfig::new(host) })
    }
    fn rw_client(host: &str) -> PeplinkClient {
        PeplinkClient::new(ClientConfig {
            client_id: "sim-rw-client".into(),
            client_secret: "sim-rw-secret".into(),
            scope: "api".into(),
            allow_write: true,
            ..ClientConfig::new(host)
        })
    }
    fn service(host: &str, writer: Option<PeplinkClient>) -> Peplink {
        Peplink::new(
            Arc::new(StateStore::new()),
            Arc::new(ro_user(host)),
            writer.map(|w| Arc::new(w) as Arc<dyn Router>),
            Arc::new(TxGate::new([(DEVICE.to_string(), true)])),
            CommandLog::memory(DEVICE),
        )
    }

    #[tokio::test]
    async fn monitoring_publishes_no_private_identifiers() {
        let sim = PeplinkSim::start(PeplinkSimConfig { frozen: true, ..Default::default() }).await.unwrap();
        let p = service(&sim.host(), None);
        p.tick(0).await;
        let v = p.state.values();
        assert_eq!(v["peplink.reachable"], json!(true));
        assert_eq!(v["peplink.active"], json!("Starlink"));
        assert_eq!(v["peplink.active_type"], json!("starlink"));
        assert_eq!((v["wan.down_mbps"].clone(), v["wan.up_mbps"].clone()), (json!(8.423), json!(0.913)));
        assert_eq!(v["location.lat"], json!(45.5));
        assert_eq!(v["peplink.allowance"].as_array().unwrap().len(), 2);
        let wans = v["peplink.wans"].as_array().unwrap();
        assert_eq!(wans[1]["cellular"]["band"], json!("n77"));
        assert_eq!(wans[3]["wifi"]["band"], json!("5 GHz"));
        let blob = serde_json::to_string(&v).unwrap();
        assert!(!blob.contains("SECRET"), "a private identifier reached state: {blob}");
        // ticks 1–3 skip location and allowance
        p.tick(1).await;
        assert_eq!(sim.with(|s| s.calls.get("GET info.location").copied()), Some(1));
    }

    #[tokio::test]
    async fn readonly_user_cannot_read_the_priority_config_and_an_admin_login_is_refused_for_monitoring() {
        let sim = PeplinkSim::start(PeplinkSimConfig::default()).await.unwrap();
        let ro = ro_user(&sim.host());
        assert!(matches!(ro.get_with(PRIORITY_FUNC, &[]).await, Err(PepError::Api(m)) if m == "config.wan.connection.priority: 401 Unauthorized"));
        let admin_ro = PeplinkClient::new(ClientConfig { username: "admin".into(), password: "admin-pass".into(), ..ClientConfig::new(&sim.host()) });
        assert!(admin_ro.get_with("status.wan.connection", &[]).await.unwrap_err().to_string().contains("write"));
    }

    #[tokio::test]
    async fn sessions_and_tokens_are_renewed_after_the_router_forgets_them() {
        let sim = PeplinkSim::start(PeplinkSimConfig::default()).await.unwrap();
        let (u, c) = (ro_user(&sim.host()), rw_client(&sim.host()));
        u.get_with("status.wan.connection", &[]).await.unwrap();
        c.get_with("status.wan.connection", &[]).await.unwrap();
        sim.with(|s| s.expire_all());
        u.get_with("status.wan.connection", &[]).await.unwrap();
        c.get_with("status.wan.connection", &[]).await.unwrap();
        assert_eq!(sim.with(|s| (s.logins, s.grants)), (2, 2));
    }

    #[tokio::test]
    async fn set_priority_through_the_web_user_writer_one_post_verified() {
        let sim = PeplinkSim::start(PeplinkSimConfig::default()).await.unwrap();
        let p = service(&sim.host(), Some(rw_user(&sim.host())));
        let r = p.command_with("set_wan_priority", &json!({"id": 4, "priority": 1}), "kiosk", 5.0, 0.05).await;
        assert!(r.is_ok() && r.detail == "WWAN on 5 GHz is now P1", "{r:?}");
        assert_eq!(sim.with(|s| s.posts.clone()), vec![json!({"instantActive": true, "list": [{"connId": 4, "priority": 1}]})]);
        assert_eq!(sim.with(|s| s.wan(4).unwrap().priority), 1);
    }

    #[tokio::test]
    async fn set_order_through_the_api_client_writer() {
        let sim = PeplinkSim::start(PeplinkSimConfig::default()).await.unwrap();
        let p = service(&sim.host(), Some(rw_client(&sim.host())));
        // enable 5 at P2, disable 3; 3 stays out of the order
        let r = p.command_with("set_wan_order", &json!({"order": [1, 5, 2, 4], "disabled": [3]}), "kiosk", 5.0, 0.05).await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(
            sim.with(|s| s.posts.clone()),
            vec![json!({"instantActive": true, "list": [{"connId": 5, "priority": 2, "enable": true}, {"connId": 2, "priority": 3},
                                                       {"connId": 4, "priority": 4}, {"connId": 3, "enable": false}]})]
        );
        assert_eq!(r.detail, "VLAN WAN 1 enabled at P2, Cellular P2→P3, WWAN on 5 GHz P3→P4, WWAN on 2.4 GHz disabled");
    }

    #[tokio::test]
    async fn a_router_that_never_applies_is_posted_once_and_unverified() {
        let sim = PeplinkSim::start(PeplinkSimConfig::default()).await.unwrap();
        sim.with(|s| s.ignore_posts = true);
        let p = service(&sim.host(), Some(rw_user(&sim.host())));
        let r = p.command_with("set_wan_priority", &json!({"id": 2, "priority": 1}), "kiosk", 0.5, 0.05).await;
        assert_eq!(r.result, ResultKind::Unverified);
        assert_eq!(sim.with(|s| s.posts.len()), 1);
    }

    #[tokio::test]
    async fn a_writer_without_write_rights_is_refused_by_the_router_once() {
        // a read-only API client configured as the writer: the GET of the priority list is refused → nothing sent
        let sim = PeplinkSim::start(PeplinkSimConfig::default()).await.unwrap();
        let w = PeplinkClient::new(ClientConfig {
            client_id: "sim-ro-client".into(),
            client_secret: "sim-ro-secret".into(),
            scope: "api".into(),
            allow_write: true,
            ..ClientConfig::new(&sim.host())
        });
        let p = service(&sim.host(), Some(w));
        let r = p.command_with("set_wan_priority", &json!({"id": 2, "priority": 1}), "kiosk", 0.5, 0.05).await;
        assert!(r.result == ResultKind::Refused && r.detail == "router not reachable; nothing sent", "{r:?}");
        assert!(sim.with(|s| s.posts.is_empty()));
        assert_eq!(p.log.rows()[0].detail, "read: token grant failed: 401 Unauthorized");
    }

    #[tokio::test]
    async fn router_unreachable() {
        let sim = PeplinkSim::start(PeplinkSimConfig::default()).await.unwrap();
        let host = sim.host();
        sim.stop();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let p = service(&host, Some(rw_user(&host)));
        p.tick(0).await;
        assert!(p.state.is_false("peplink.reachable"));
        let r = p.command("set_wan_priority", &json!({"id": 2, "priority": 1}), "kiosk").await;
        assert!(r.result == ResultKind::Refused && r.detail == "router not reachable; nothing sent");
    }
}
