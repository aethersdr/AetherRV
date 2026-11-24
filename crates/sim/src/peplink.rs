//! Peplink router API simulator: HTTPS (self-signed, as the router) or plain HTTP on loopback, implementing the
//! endpoints AetherRV uses with responses shaped like firmware 8.6.x, INCLUDING the private identifiers (IMSI,
//! ICCID, IMEI, APN password, BSSID, WAN IPs) that AetherRV's whitelist must drop. Every such value contains the
//! marker `SECRET`, so a test can grep everything published for it.
//!
//! Auth as the router: API clients (`auth.token.grant`, scopes `api.read-only` / `api`, `?accessToken=`), web users
//! (`login`, cookie `bauth`, permission GET or GET+POST). `config.wan.connection.priority` answers 401 to read-only
//! credentials; its POST (cookie sessions need `X-Requested-With`) changes the WAN priority/enable config, which
//! `status.wan.connection` then reports.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

/// A self-signed certificate for 127.0.0.1 / localhost (test only; generated with openssl, valid 100 years).
pub const CERT_DER: &[u8] = include_bytes!("../certs/cert.der");
pub const KEY_DER: &[u8] = include_bytes!("../certs/key.der");

#[derive(Debug, Clone)]
pub struct SimClient {
    pub id: String,
    pub secret: String,
    /// `api.read-only` or `api`.
    pub scope: String,
}

#[derive(Debug, Clone)]
pub struct SimUser {
    pub username: String,
    pub password: String,
    pub write: bool,
}

#[derive(Debug, Clone)]
pub struct PeplinkSimConfig {
    /// Loopback only (anything else is refused).
    pub bind: SocketAddr,
    pub tls: bool,
    /// Values don't move with time (uptimes, traffic).
    pub frozen: bool,
    pub clients: Vec<SimClient>,
    pub users: Vec<SimUser>,
    pub token_ttl_s: u64,
}

impl Default for PeplinkSimConfig {
    fn default() -> Self {
        let c = |id: &str, secret: &str, scope: &str| SimClient { id: id.into(), secret: secret.into(), scope: scope.into() };
        let u = |n: &str, p: &str, w: bool| SimUser { username: n.into(), password: p.into(), write: w };
        PeplinkSimConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            tls: true,
            frozen: false,
            clients: vec![c("sim-ro-client", "sim-ro-secret", "api.read-only"), c("sim-rw-client", "sim-rw-secret", "api")],
            users: vec![u("viewer", "ro-pass", false), u("admin", "admin-pass", true)],
            token_ttl_s: 172_800,
        }
    }
}

/// One WAN connection as the router keeps it.
#[derive(Debug, Clone)]
pub struct SimWan {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub enable: bool,
    pub priority: i64,
    pub group: Option<i64>,
    /// The link would be up if enabled.
    pub link_up: bool,
    pub uptime_base: i64,
    /// Extra status fields (the cellular / wifi blocks, IPs), merged into `status.wan.connection` as is.
    pub extra: Map<String, Value>,
    pub down_kbps: f64,
    pub up_kbps: f64,
}

#[derive(Debug, Clone)]
pub struct RouterState {
    pub wans: Vec<SimWan>,
    pub location: Value,
    pub allowance: Value,
    /// token → (scope, expires at)
    pub tokens: BTreeMap<String, (String, f64)>,
    /// bauth cookie → (user, write)
    pub sessions: BTreeMap<String, (String, bool)>,
    pub grants: u64,
    pub logins: u64,
    /// Every POST body to `config.wan.connection.priority`, accepted or not.
    pub posts: Vec<Value>,
    /// Requests per function.
    pub calls: BTreeMap<String, u64>,
    // faults
    /// Accept POSTs but don't apply them.
    pub ignore_posts: bool,
    /// Function → how many calls to answer with stat "fail" (code 500).
    pub fail: BTreeMap<String, u32>,
    serial: u64,
    started: std::time::Instant,
}

fn sample_wans() -> Vec<SimWan> {
    let obj = |v: Value| v.as_object().cloned().unwrap();
    vec![
        SimWan {
            id: 1,
            name: "Starlink".into(),
            kind: "ethernet".into(),
            enable: true,
            priority: 1,
            group: Some(1),
            link_up: true,
            uptime_base: 3600,
            extra: obj(json!({"ip": "SECRET-WAN-IP-100.64.0.2", "gateway": "SECRET-GW", "mac": "SECRET-MAC-1",
                              "dns": ["SECRET-DNS"], "mask": "255.192.0.0"})),
            down_kbps: 8423.0,
            up_kbps: 913.0,
        },
        SimWan {
            id: 2,
            name: "Cellular".into(),
            kind: "cellular".into(),
            enable: true,
            priority: 2,
            group: Some(1),
            link_up: true,
            uptime_base: 7200,
            extra: obj(json!({"ip": "SECRET-WAN-IP-10.0.0.2", "cellular": {
                "mobileType": "5G", "signalLevel": 4, "carrierAggregation": true,
                "carrier": {"name": "Verizon", "country": "US"},
                "roamingStatus": {"code": 1, "message": "home"},
                "imei": "SECRET-IMEI-350000000000001", "meid": "SECRET-MEID",
                "sim": {"order": [1, 2],
                        "1": {"status": "In Use", "active": true, "iccid": "SECRET-ICCID-8901", "imsi": "SECRET-IMSI-3114",
                              "apn": "SECRET-APN-NAME", "username": "SECRET-APN-USER", "password": "SECRET-APN-PASSWORD",
                              "mtu": 1428},
                        "2": {"status": "No SIM Card Detected", "active": false}},
                "rat": [{"name": "LTE", "band": [{"name": "B66", "channel": 66786, "signal": {"rsrp": -101, "rsrq": -11, "sinr": 9.5, "rssi": -70}}]},
                        {"name": "5G NR", "band": [{"name": "n77", "channel": 650000, "signal": {"rsrp": -95, "rsrq": -10, "sinr": 14.0}}]}]}})),
            down_kbps: 120.0,
            up_kbps: 14.0,
        },
        SimWan {
            id: 3,
            name: "WWAN on 2.4 GHz".into(),
            kind: "wifi".into(),
            enable: true,
            priority: 3,
            group: Some(1),
            link_up: false,
            uptime_base: 0,
            extra: Map::new(),
            down_kbps: 0.0,
            up_kbps: 0.0,
        },
        SimWan {
            id: 4,
            name: "WWAN on 5 GHz".into(),
            kind: "wifi".into(),
            enable: true,
            priority: 3,
            group: Some(1),
            link_up: true,
            uptime_base: 900,
            extra: obj(json!({"ip": "SECRET-WAN-IP-192.0.2.77", "wifi": {"signal": {"strength": 0, "level": 5}, "ssid": "PTP",
                              "bssid": "SECRET-BSSID-02:00:00:00:00:01", "channel": 149, "security": "WPA2", "password": "SECRET-WIFI-PSK"}})),
            down_kbps: 497.0,
            up_kbps: 38.0,
        },
        SimWan {
            id: 5,
            name: "VLAN WAN 1".into(),
            kind: "ethernet".into(),
            enable: false,
            priority: 1,
            group: None,
            link_up: false,
            uptime_base: 0,
            extra: Map::new(),
            down_kbps: 0.0,
            up_kbps: 0.0,
        },
    ]
}

impl RouterState {
    fn new() -> RouterState {
        RouterState {
            wans: sample_wans(),
            location: json!({"gps": true, "location": {"latitude": 45.5, "longitude": -122.6, "altitude": 55.1, "speed": 0.02,
                                                       "heading": 356.9, "hdop": 1, "timestamp": 1311972720}}),
            allowance: json!({"order": [1, 2],
                              "1": {"enable": true, "usage": 5, "limit": 1024, "percent": 0, "start": 1, "unit": "MB"},
                              "2": {"order": [1, 2], "1": {"enable": true, "usage": 41000, "limit": 50000, "percent": 82, "start": 1, "unit": "MB"},
                                    "2": {"enable": false}}}),
            tokens: BTreeMap::new(),
            sessions: BTreeMap::new(),
            grants: 0,
            logins: 0,
            posts: Vec::new(),
            calls: BTreeMap::new(),
            ignore_posts: false,
            fail: BTreeMap::new(),
            serial: 0,
            started: std::time::Instant::now(),
        }
    }

    /// The router forgets every token and session (a reboot, an idle timeout).
    pub fn expire_all(&mut self) {
        self.tokens.clear();
        self.sessions.clear();
    }

    pub fn wan(&mut self, id: i64) -> Option<&mut SimWan> {
        self.wans.iter_mut().find(|w| w.id == id)
    }

    fn up(&self, w: &SimWan) -> bool {
        w.enable && w.link_up
    }

    fn priority_config(&self) -> Value {
        let mut m = Map::new();
        m.insert("order".into(), json!(self.wans.iter().map(|w| w.id).collect::<Vec<_>>()));
        for w in &self.wans {
            let mut e = Map::new();
            e.insert("name".into(), json!(w.name));
            if let Some(g) = w.group {
                e.insert("group".into(), json!(g));
            }
            e.insert("priority".into(), json!(w.priority));
            e.insert("enable".into(), json!(w.enable));
            m.insert(w.id.to_string(), Value::Object(e));
        }
        Value::Object(m)
    }

    fn wan_status(&self, frozen: bool) -> Value {
        let el = if frozen { 0 } else { self.started.elapsed().as_secs() as i64 };
        let mut m = Map::new();
        for w in &self.wans {
            let mut e = Map::new();
            e.insert("name".into(), json!(w.name));
            e.insert("enable".into(), json!(w.enable));
            let (led, msg) = match (w.enable, self.up(w), w.kind.as_str()) {
                (false, _, _) => ("gray", "Disabled".to_string()),
                (true, true, "wifi") => ("green", "Connected to PTP".to_string()),
                (true, true, _) => ("green", "Connected".to_string()),
                (true, false, "wifi") => ("flash", "Scanning...".to_string()),
                (true, false, _) => ("red", "No Cable Detected".to_string()),
            };
            e.insert("message".into(), json!(msg));
            if self.up(w) {
                e.insert("uptime".into(), json!(w.uptime_base + el));
            }
            e.insert("type".into(), json!(w.kind));
            e.insert("virtualType".into(), json!(w.kind));
            if w.enable {
                e.insert("priority".into(), json!(w.priority));
            }
            e.insert("statusLed".into(), json!(led));
            for (k, v) in &w.extra {
                e.insert(k.clone(), v.clone());
            }
            m.insert(w.id.to_string(), Value::Object(e));
        }
        m.insert("order".into(), json!(self.wans.iter().map(|w| w.id).collect::<Vec<_>>()));
        Value::Object(m)
    }

    fn traffic(&self, frozen: bool) -> Value {
        let t = if frozen { 0.0 } else { self.started.elapsed().as_secs_f64() };
        let mut bw = Map::new();
        bw.insert("timestamp".into(), json!(1_795_000_000 + t as i64));
        bw.insert("unit".into(), json!("kbps"));
        for w in &self.wans {
            if !self.up(w) {
                continue;
            }
            let k = if frozen { 1.0 } else { 1.0 + 0.2 * (t / 9.0).sin() };
            bw.insert(
                w.id.to_string(),
                json!({"name": w.name, "overall": {"download": (w.down_kbps * k).round() as i64, "upload": (w.up_kbps * k).round() as i64}}),
            );
        }
        json!({"bandwidth": bw, "lifetime": {"unit": "MB", "1": {"download": 123456, "upload": 2345}}})
    }

    /// POST config.wan.connection.priority: every item must name a known connection; priority 1–4.
    fn apply(&mut self, body: &Value) -> Result<(), String> {
        let list = body.get("list").and_then(Value::as_array).ok_or("no list")?;
        let mut changes = Vec::new();
        for it in list {
            let id = it.get("connId").and_then(Value::as_i64).ok_or("connId")?;
            if !self.wans.iter().any(|w| w.id == id) {
                return Err(format!("unknown connection {id}"));
            }
            let p = match it.get("priority") {
                None => None,
                Some(p) => Some(p.as_i64().filter(|p| (1..=4).contains(p)).ok_or("priority")?),
            };
            let en = match it.get("enable") {
                None => None,
                Some(e) => Some(e.as_bool().ok_or("enable")?),
            };
            changes.push((id, p, en));
        }
        if self.ignore_posts {
            return Ok(());
        }
        for (id, p, en) in changes {
            let w = self.wan(id).unwrap();
            if let Some(p) = p {
                w.priority = p;
            }
            if let Some(e) = en {
                w.enable = e;
            }
        }
        Ok(())
    }
}

struct Inner {
    cfg: PeplinkSimConfig,
    state: Mutex<RouterState>,
}

pub struct PeplinkSim {
    pub addr: SocketAddr,
    inner: Arc<Inner>,
    task: tokio::task::JoinHandle<()>,
}

impl PeplinkSim {
    pub async fn start(cfg: PeplinkSimConfig) -> anyhow::Result<PeplinkSim> {
        if !cfg.bind.ip().is_loopback() {
            anyhow::bail!("refusing to listen on {}: the Peplink simulator is loopback only", cfg.bind);
        }
        let listener = TcpListener::bind(cfg.bind).await?;
        let addr = listener.local_addr()?;
        let tls = cfg.tls;
        let inner = Arc::new(Inner { cfg, state: Mutex::new(RouterState::new()) });
        let app = axum::Router::new().fallback(api).with_state(inner.clone());
        let task = if tls {
            let acceptor = tls_acceptor()?;
            tokio::spawn(serve_tls(listener, acceptor, app))
        } else {
            tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            })
        };
        Ok(PeplinkSim { addr, inner, task })
    }

    /// `host:port`, as the Peplink config's `host`.
    pub fn host(&self) -> String {
        self.addr.to_string()
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut RouterState) -> R) -> R {
        f(&mut self.inner.state.lock().unwrap())
    }

    pub fn stop(self) {
        self.task.abort();
    }
}

fn tls_acceptor() -> anyhow::Result<tokio_rustls::TlsAcceptor> {
    use tokio_rustls::rustls;
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let cert = rustls::pki_types::CertificateDer::from(CERT_DER.to_vec());
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(KEY_DER.to_vec().into());
    let cfg = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)?;
    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(cfg)))
}

async fn serve_tls(listener: TcpListener, acceptor: tokio_rustls::TlsAcceptor, app: axum::Router) {
    loop {
        let Ok((tcp, _)) = listener.accept().await else { continue };
        let (acceptor, app) = (acceptor.clone(), app.clone());
        tokio::spawn(async move {
            let Ok(tls) = acceptor.accept(tcp).await else { return };
            let svc = hyper_util::service::TowerToHyperService::new(app);
            let _ = hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
                .serve_connection(hyper_util::rt::TokioIo::new(tls), svc)
                .await;
        });
    }
}

fn reply(v: Value) -> Response {
    (StatusCode::OK, [(header::CONTENT_TYPE, "application/json")], v.to_string()).into_response()
}

fn fail(code: i64, msg: &str) -> Response {
    reply(json!({"stat": "fail", "code": code, "message": msg}))
}

fn query(uri: &Uri) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for kv in uri.query().unwrap_or("").split('&').filter(|s| !s.is_empty()) {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        out.insert(unescape(k), unescape(v));
    }
    out
}

fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let h = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|x| u8::from_str_radix(x, 16).ok());
                match h {
                    Some(c) => {
                        out.push(c);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn cookie(h: &HeaderMap, name: &str) -> Option<String> {
    for v in h.get_all(header::COOKIE) {
        for part in v.to_str().ok()?.split(';') {
            if let Some((k, v)) = part.trim().split_once('=')
                && k == name
            {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64()
}

/// What the request may do: `Some(write)` when authenticated.
fn authorized(st: &RouterState, q: &BTreeMap<String, String>, h: &HeaderMap) -> Option<bool> {
    if let Some(t) = q.get("accessToken") {
        return st.tokens.get(t).filter(|(_, exp)| *exp > now()).map(|(scope, _)| scope == "api");
    }
    let s = cookie(h, "bauth")?;
    st.sessions.get(&s).map(|(_, w)| *w)
}

async fn api(State(inner): State<Arc<Inner>>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    let Some(func) = uri.path().strip_prefix("/api/").map(str::to_string) else { return fail(404, "Not Found") };
    let q = query(&uri);
    let cfg = &inner.cfg;
    let mut st = inner.state.lock().unwrap();
    *st.calls.entry(format!("{method} {func}")).or_insert(0) += 1;
    if let Some(n) = st.fail.get_mut(&func)
        && *n > 0
    {
        *n -= 1;
        return fail(500, "Simulated failure");
    }
    let json_body = || serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null);
    match (method.as_str(), func.as_str()) {
        ("POST", "auth.token.grant") => {
            st.grants += 1;
            let b = json_body();
            let s = |k: &str| b.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let Some(c) = cfg.clients.iter().find(|c| c.id == s("clientId") && c.secret == s("clientSecret")) else {
                return fail(401, "Unauthorized");
            };
            let want = s("scope");
            if !(want == "api.read-only" || (want == "api" && c.scope == "api")) {
                return fail(401, "Unauthorized");
            }
            st.serial += 1;
            let tok = format!("simtok{:08}", st.serial);
            st.tokens.insert(tok.clone(), (want, now() + cfg.token_ttl_s as f64));
            reply(json!({"stat": "ok", "response": {"accessToken": tok, "expiresIn": cfg.token_ttl_s}}))
        }
        ("POST", "login") => {
            st.logins += 1;
            let b = json_body();
            let s = |k: &str| b.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let Some(u) = cfg.users.iter().find(|u| u.username == s("username") && u.password == s("password")) else {
                return fail(401, "Unauthorized");
            };
            st.serial += 1;
            let sess = format!("simsess{:08}", st.serial);
            st.sessions.insert(sess.clone(), (u.username.clone(), u.write));
            let perm = if u.write { json!({"GET": 1, "POST": 1}) } else { json!({"GET": 1}) };
            let mut r = reply(json!({"stat": "ok", "response": {"permission": perm}}));
            let c = format!("bauth={sess}; Path=/; HttpOnly{}", if cfg.tls { "; Secure" } else { "" });
            r.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_str(&c).unwrap());
            r
        }
        ("POST", "config.wan.connection.priority") => {
            let write = authorized(&st, &q, &headers);
            let via_cookie = !q.contains_key("accessToken");
            // fw 8.6.1: a cookie-session POST counts only as a web-admin request (X-Requested-With)
            let xrw = headers.get("x-requested-with").and_then(|v| v.to_str().ok()) == Some("XMLHttpRequest");
            let b = json_body();
            st.posts.push(b.clone());
            if write != Some(true) || (via_cookie && !xrw) {
                return fail(401, "Unauthorized");
            }
            match st.apply(&b) {
                Ok(()) => reply(json!({"stat": "ok", "response": {}})),
                Err(e) => fail(400, &format!("Invalid request: {e}")),
            }
        }
        ("GET", f) => {
            let Some(write) = authorized(&st, &q, &headers) else { return fail(401, "Unauthorized") };
            let resp = match f {
                "status.wan.connection" => st.wan_status(cfg.frozen),
                "status.wan.connection.allowance" => st.allowance.clone(),
                "status.traffic" => st.traffic(cfg.frozen),
                "info.location" => st.location.clone(),
                "config.wan.connection.priority" if write => st.priority_config(),
                "config.wan.connection.priority" => return fail(401, "Unauthorized"),
                _ => return fail(999, "Unknown function"),
            };
            reply(json!({"stat": "ok", "response": resp}))
        }
        _ => fail(999, "Unknown function"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn loopback_only() {
        let cfg = PeplinkSimConfig { bind: "0.0.0.0:0".parse().unwrap(), ..Default::default() };
        assert!(PeplinkSim::start(cfg).await.is_err());
    }

    #[test]
    fn the_test_certificate_loads() {
        tls_acceptor().unwrap();
    }

    #[test]
    fn priority_post_applies_and_validates() {
        let mut st = RouterState::new();
        st.apply(&json!({"instantActive": true, "list": [{"connId": 5, "priority": 2, "enable": true}, {"connId": 1, "priority": 4}]})).unwrap();
        assert!(st.wan(5).unwrap().enable && st.wan(5).unwrap().priority == 2 && st.wan(1).unwrap().priority == 4);
        assert!(st.apply(&json!({"list": [{"connId": 9, "priority": 1}]})).is_err());
        assert!(st.apply(&json!({"list": [{"connId": 1, "priority": 5}]})).is_err());
        let p = st.priority_config();
        assert_eq!(p["order"], json!([1, 2, 3, 4, 5]));
        assert_eq!(p["5"], json!({"name": "VLAN WAN 1", "priority": 2, "enable": true}));
    }
}
