//! `aetherrv-gateway`: HTTP + WebSocket for the UI (ARCHITECTURE.md §5). A port of `web.py` on top of the bus.
//!
//! The gateway holds no device state of its own: restarting it costs the UI a reconnect and nothing else. Who may
//! control is decided here (`auth::RemoteAuth`), never in the UI alone; the services trust the `client` it sends.

pub mod auth;
pub mod flat;

use aetherrv_core::bus::{Bus, Update};
use aetherrv_core::command::BoxFuture;
use aetherrv_core::config::Cfg;
use aetherrv_core::state::StateStore;
use auth::{Admin, RemoteAuth};
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path as AxPath, Query, State};
use axum::http::{HeaderValue, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use flat::Flat;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Settings verbs the touchscreen may ask the root helper for (the helper enforces its own list too).
pub const SETTINGS_VERBS: [&str; 13] = [
    "set_hostname", "reboot", "poweroff", "restart_kiosk", "wifi_scan", "wifi_power", "wifi_connect", "wifi_forget",
    "tailscale_install", "tailscale_login", "tailscale_logout", "update_check", "update_start",
];

fn long_verb(v: &str) -> Duration {
    Duration::from_secs(match v {
        "tailscale_install" => 660,
        "update_check" => 330,
        _ => 120,
    })
}

/// History keys the UI may chart (history.py's TRACKED).
pub const TRACKED: [&str; 28] = [
    "victron.battery.soc", "victron.battery.power", "victron.pv.power", "victron.dc.load", "victron.ac.load",
    "victron.ac.shore", "victron.alternator.power", "energy.alternator.power", "energy.dc.load", "alternator.current",
    "climate.cabin.temp", "climate.outside.temp", "climate.fridge.temp", "climate.freezer.temp", "climate.rixens.coolant",
    "climate.rixens.fuel_hz", "climate.rixens.inlet_c", "climate.rixens.outlet_c", "climate.rixens.flame_c",
    "climate.rixens.fan_rpm", "climate.rixens.fuel_gph", "starlink.down_mbps", "starlink.up_mbps", "starlink.latency_ms",
    "starlink.snr_db", "starlink.power_w", "wan.down_mbps", "wan.up_mbps",
];

/// How the gateway reaches the services (the bus; a fake in tests).
pub trait Commands: Send + Sync {
    fn request(&self, device: &str, cmd: &str, value: Value, client: &str, timeout: Duration) -> BoxFuture<aetherrv_core::command::Reply>;
}

impl Commands for Bus {
    fn request(&self, device: &str, cmd: &str, value: Value, client: &str, timeout: Duration) -> BoxFuture<aetherrv_core::command::Reply> {
        let (b, d, c, cl) = (self.clone(), device.to_string(), cmd.to_string(), client.to_string());
        Box::pin(async move { Bus::request(&b, &d, &c, value, &cl, 3000, timeout).await })
    }
}

/// Per-device reply timeouts (a service's own verify window plus a queued command).
pub fn device_timeout(device: &str) -> Duration {
    Duration::from_secs(match device {
        "espar" | "n2k" => 15,
        "rixens" | "victron" => 40,
        // worst cases: Starlink install/reboot 15 + 20 + 90 + 8 s; Peplink grant + POST + verify ~55 s
        "peplink" => 70,
        "starlink" => 150,
        _ => 10,
    })
}

pub struct App {
    pub flat: Arc<Flat>,
    pub auth: RemoteAuth,
    pub admin: Arc<dyn Admin>,
    pub cmds: Arc<dyn Commands>,
    pub static_dir: PathBuf,
    pub history_db: PathBuf,
    pub weather_sock: PathBuf,
    pub coalesce: Duration,
}

/// Remote and local address of a connection (for `RemoteAuth`).
#[derive(Clone, Copy, Debug)]
pub struct Conn {
    pub remote: SocketAddr,
    pub local: Option<SocketAddr>,
}

impl axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, tokio::net::TcpListener>> for Conn {
    fn connect_info(s: axum::serve::IncomingStream<'_, tokio::net::TcpListener>) -> Self {
        Conn { remote: *s.remote_addr(), local: s.io().local_addr().ok() }
    }
}

impl App {
    async fn who(&self, c: &Conn) -> Option<String> {
        let r = c.remote.ip().to_string();
        let l = c.local.map(|l| l.ip().to_string());
        self.auth.who(Some(&r), l.as_deref()).await
    }
}

async fn file(path: PathBuf, cache: &'static str) -> Response {
    match tokio::fs::read(&path).await {
        Ok(b) => {
            let ct = match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
                "html" => "text/html; charset=utf-8",
                "js" | "mjs" => "text/javascript; charset=utf-8",
                "css" => "text/css; charset=utf-8",
                "json" => "application/json",
                "png" => "image/png",
                "svg" => "image/svg+xml",
                "woff2" => "font/woff2",
                "wasm" => "application/wasm",
                "ico" => "image/x-icon",
                _ => "application/octet-stream",
            };
            let mut r = Response::new(Body::from(b));
            r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(ct));
            r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
            r
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

fn safe_rel(p: &str) -> Option<PathBuf> {
    let pb = PathBuf::from(p);
    if pb.components().all(|c| matches!(c, std::path::Component::Normal(_))) { Some(pb) } else { None }
}

async fn index(State(a): State<Arc<App>>) -> Response {
    file(a.static_dir.join("index.html"), "no-cache").await
}

/// The Python UI's files (`/static/…`): always revalidate (the kiosk otherwise keeps an old UI after deploys).
async fn static_file(State(a): State<Arc<App>>, AxPath(p): AxPath<String>) -> Response {
    match safe_rel(&p) {
        Some(rel) => file(a.static_dir.join(rel), "no-cache").await,
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// The React bundle's hashed assets (`/assets/…`): immutable.
async fn asset(State(a): State<Arc<App>>, AxPath(p): AxPath<String>) -> Response {
    match safe_rel(&p) {
        Some(rel) => file(a.static_dir.join("assets").join(rel), "public, max-age=31536000, immutable").await,
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn api_state(State(a): State<Arc<App>>) -> Json<Value> {
    Json(Value::Object(a.flat.snapshot()))
}

/// Points for `key` over the last `since` seconds, averaged into at most `max_points` buckets (history.py `get`).
pub fn history_get(db: &rusqlite::Connection, key: &str, since: f64, max_points: i64, interval: f64, now: f64) -> Vec<Value> {
    if !TRACKED.contains(&key) {
        return vec![];
    }
    let start = now - since;
    let bucket = interval.max(since / max_points.max(1) as f64);
    let mut st = match db.prepare(
        "SELECT MAX(t), AVG(v) FROM samples WHERE key = ?1 AND t >= ?2 GROUP BY CAST((t - ?2) / ?3 AS INTEGER) ORDER BY 1",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let rows = st.query_map(rusqlite::params![key, start, bucket], |r| Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?)));
    match rows {
        Ok(it) => it.filter_map(Result::ok).map(|(t, v)| json!([t, aetherrv_core::num::round(v, 2)])).collect(),
        Err(_) => vec![],
    }
}

async fn api_history(State(a): State<Arc<App>>, Query(q): Query<HashMap<String, String>>) -> Response {
    let keys: Vec<String> = q.get("keys").map(|k| k.split(',').filter(|x| !x.is_empty()).map(str::to_string).collect()).unwrap_or_default();
    let since = match q.get("since").map(|s| s.parse::<f64>()).unwrap_or(Ok(86400.0)) {
        Ok(s) => s.min(31.0 * 86400.0),
        Err(_) => return (StatusCode::BAD_REQUEST, "bad since/points").into_response(),
    };
    let points = match q.get("points").map(|s| s.parse::<i64>()).unwrap_or(Ok(240)) {
        Ok(p) => p.clamp(10, 2000),
        Err(_) => return (StatusCode::BAD_REQUEST, "bad since/points").into_response(),
    };
    let path = a.history_db.clone();
    let out = tokio::task::spawn_blocking(move || {
        let mut m = Map::new();
        let db = rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY);
        for k in keys {
            let pts = match &db {
                Ok(db) => history_get(db, &k, since, points, 30.0, aetherrv_core::clock::now_s()),
                Err(_) => vec![],
            };
            m.insert(k, Value::Array(pts));
        }
        Value::Object(m)
    })
    .await
    .unwrap_or_else(|_| json!({}));
    Json(out).into_response()
}

const PREF_KEYS: [&str; 4] = ["screen_off_min", "auto_timezone", "fridge_off", "freezer_off"];

async fn api_settings(State(a): State<Arc<App>>, ConnectInfo(c): ConnectInfo<Conn>) -> Json<Value> {
    let version = std::fs::read_to_string(a.static_dir.parent().map(|p| p.join("VERSION")).unwrap_or_default()).ok().map(|s| s.trim().to_string());
    let prefs: Map<String, Value> = PREF_KEYS.iter().filter_map(|k| a.flat.get(&format!("settings.{k}")).map(|v| (k.to_string(), v))).collect();
    let call = |verb: &'static str| {
        let adm = a.admin.clone();
        async move { adm.call(verb, json!({}), Duration::from_secs(20)).await.unwrap_or_else(|e| json!({"ok": false, "error": e})) }
    };
    let (st, wifi, ts, upd) = tokio::join!(call("status"), call("wifi_status"), call("tailscale_status"), call("update_status"));
    let mut out = json!({"prefs": prefs, "admin": st, "version": version, "wifi": wifi, "tailscale": ts, "update": upd});
    if a.who(&c).await.is_none()
        && let Some(t) = out["tailscale"].as_object_mut()
    {
        t.remove("auth_url"); // a pending sign-in link is for the touchscreen only
    }
    Json(out)
}

async fn api_settings_action(State(a): State<Arc<App>>, ConnectInfo(c): ConnectInfo<Conn>, body: axum::body::Bytes) -> Response {
    let Some(who) = a.who(&c).await else {
        return (StatusCode::FORBIDDEN, Json(json!({"ok": false, "error": "view only: settings change from the touchscreen or your Tailscale devices"}))).into_response();
    };
    let Ok(b) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": "bad request"}))).into_response();
    };
    let verb = b.get("verb").and_then(Value::as_str).unwrap_or("").to_string();
    let args = b.get("args").cloned().filter(|v| !v.is_null()).unwrap_or_else(|| json!({}));
    if verb == "pref" {
        let r = a.cmds.request("settings", "pref", json!({"key": args.get("key"), "value": args.get("value")}), &who, Duration::from_secs(10)).await;
        return Json(if r.is_ok() { json!({"ok": true}) } else { json!({"ok": false, "error": r.detail}) }).into_response();
    }
    if !SETTINGS_VERBS.contains(&verb.as_str()) || !args.is_object() {
        return (StatusCode::FORBIDDEN, Json(json!({"ok": false, "error": "not allowed"}))).into_response();
    }
    tracing::info!("settings action from {who}: {verb}"); // args never logged (may hold secrets)
    match a.admin.call(&verb, args, long_verb(&verb)).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => Json(json!({"ok": false, "error": e})).into_response(),
    }
}

async fn api_client_info(State(a): State<Arc<App>>, ConnectInfo(c): ConnectInfo<Conn>, body: axum::body::Bytes) -> Response {
    if a.who(&c).await.as_deref() != Some("kiosk") {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok(v) = serde_json::from_slice::<Value>(&body) else { return StatusCode::BAD_REQUEST.into_response() };
    let s = v.to_string();
    tracing::info!("kiosk page: {}", &s[..s.len().min(400)]);
    Json(json!({"ok": true})).into_response()
}

/// `/api/radar/*` and `/api/traffic/*` → the weather service's Unix socket.
async fn proxy_weather(State(a): State<Arc<App>>, req: Request<Body>) -> Response {
    let pq = req.uri().path_and_query().map(|p| p.as_str().to_string()).unwrap_or_default();
    let r: anyhow::Result<Response> = async {
        let stream = tokio::net::UnixStream::connect(&a.weather_sock).await?;
        let io = hyper_util::rt::TokioIo::new(stream);
        let (mut tx, conn) = hyper::client::conn::http1::handshake(io).await?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let up = Request::builder().uri(&pq).header(header::HOST, "weather").body(http_body_util::Empty::<bytes::Bytes>::new())?;
        let resp = tx.send_request(up).await?;
        let (parts, body) = resp.into_parts();
        let bytes = http_body_util::BodyExt::collect(body).await?.to_bytes();
        Ok(Response::from_parts(parts, Body::from(bytes)))
    }
    .await;
    r.unwrap_or_else(|e| {
        tracing::debug!("weather proxy {pq}: {e}");
        StatusCode::BAD_GATEWAY.into_response()
    })
}

async fn ws(State(a): State<Arc<App>>, ConnectInfo(c): ConnectInfo<Conn>, up: WebSocketUpgrade) -> Response {
    let who = a.who(&c).await;
    up.on_upgrade(move |sock| ws_session(a, sock, who))
}

async fn ws_session(a: Arc<App>, mut sock: WebSocket, who: Option<String>) {
    let hello = json!({"type": "hello", "control": who.is_some(),
                       "who": match who.as_deref() { Some("kiosk") => "touchscreen".to_string(), Some(w) => w.to_string(), None => "viewer".into() }});
    let mut rx = a.flat.subscribe();
    if sock.send(Message::Text(hello.to_string().into())).await.is_err() {
        return;
    }
    if sock.send(Message::Text(json!({"type": "snapshot", "data": a.flat.snapshot()}).to_string().into())).await.is_err() {
        return;
    }
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<String>(64);
    let mut pending: Map<String, Value> = Map::new();
    let mut tick = tokio::time::interval(a.coalesce);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut ping = tokio::time::interval(Duration::from_secs(15));
    loop {
        tokio::select! {
            ch = rx.recv() => match ch {
                Ok(v) => for (k, x) in v { pending.insert(k, x); },
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // a slow client resyncs from a full snapshot
                    pending.clear();
                    let s = json!({"type": "snapshot", "data": a.flat.snapshot()}).to_string();
                    if sock.send(Message::Text(s.into())).await.is_err() { return; }
                }
                Err(_) => return,
            },
            _ = tick.tick() => {
                if !pending.is_empty() {
                    let d = json!({"type": "delta", "data": std::mem::take(&mut pending)}).to_string();
                    if sock.send(Message::Text(d.into())).await.is_err() { return; }
                }
            }
            _ = ping.tick() => {
                if sock.send(Message::Ping(Vec::new().into())).await.is_err() { return; }
            }
            m = out_rx.recv() => if let Some(m) = m {
                if sock.send(Message::Text(m.into())).await.is_err() { return; }
            },
            msg = sock.recv() => {
                let Some(Ok(msg)) = msg else { return };
                let Message::Text(t) = msg else { continue };
                let Ok(req) = serde_json::from_str::<Value>(&t) else { continue };
                handle_ws_msg(&a, &who, req, out_tx.clone()).await;
            }
        }
    }
}

async fn handle_ws_msg(a: &Arc<App>, who: &Option<String>, req: Value, out: tokio::sync::mpsc::Sender<String>) {
    let ty = req.get("type").and_then(Value::as_str).unwrap_or("").to_string();
    match ty.as_str() {
        // acknowledging an alert only changes AetherRV's own UI state, never a device
        "ack" | "ack_all" => {
            let (cmds, client) = (a.cmds.clone(), who.clone().unwrap_or_else(|| "viewer".into()));
            let id = req.get("id").cloned().unwrap_or(Value::Null);
            tokio::spawn(async move {
                let v = if ty == "ack" { json!({"id": id.as_str().map(str::to_string).unwrap_or_else(|| id.to_string())}) } else { json!({}) };
                cmds.request("alerts", &ty, v, &client, Duration::from_secs(5)).await;
            });
        }
        "control" => {
            let id = req.get("id").cloned().unwrap_or(Value::Null);
            let device = req.get("device").and_then(Value::as_str).unwrap_or("").to_string();
            const DEVICES: [&str; 6] = ["rixens", "victron", "starlink", "n2k", "peplink", "espar"];
            let Some(client) = who.clone() else {
                let _ = out.send(json!({"type": "control_result", "id": id, "ok": false, "result": "refused",
                    "detail": "view only: control from the touchscreen or your Tailscale devices"}).to_string()).await;
                return;
            };
            if !DEVICES.contains(&device.as_str()) {
                let _ = out.send(json!({"type": "control_result", "id": id, "ok": false, "result": "refused", "detail": "device not available"}).to_string()).await;
                return;
            }
            let cmd = req.get("cmd").map(|c| c.as_str().map(str::to_string).unwrap_or_else(|| c.to_string())).unwrap_or_default();
            let value = req.get("value").cloned().unwrap_or(Value::Null);
            let cmds = a.cmds.clone();
            tokio::spawn(async move {
                let r = cmds.request(&device, &cmd, value, &client, device_timeout(&device)).await;
                let _ = out.send(json!({"type": "control_result", "id": id, "ok": r.is_ok(), "result": r.result, "detail": r.detail}).to_string()).await;
            });
        }
        _ => {
            let _ = out.send(json!({"type": "error", "error": "control is disabled"}).to_string()).await;
        }
    }
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/static/{*p}", get(static_file))
        .route("/assets/{*p}", get(asset))
        .route("/api/state", get(api_state))
        .route("/api/history", get(api_history))
        .route("/api/settings", get(api_settings))
        .route("/api/settings/action", post(api_settings_action))
        .route("/api/client-info", post(api_client_info))
        .route("/api/radar/{*p}", get(proxy_weather))
        .route("/api/traffic/{*p}", get(proxy_weather))
        .route("/ws", get(ws))
        .with_state(app)
}

/// Keep the flat view current from the bus.
pub async fn follow(bus: Bus, flat: Arc<Flat>) {
    let mut rx = bus.updates();
    let remote = bus.remote().clone();
    let mut health = tokio::time::interval(Duration::from_secs(1));
    flat.refresh_all(&remote);
    loop {
        tokio::select! {
            u = rx.recv() => match u {
                Ok(Update::State { key, .. }) => flat.refresh_keys(&remote, &[key]),
                Ok(Update::Health { .. }) => flat.refresh_all(&remote),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => flat.refresh_all(&remote),
                Err(_) => return,
            },
            // heartbeats go stale without an update event
            _ = health.tick() => flat.refresh_all(&remote),
        }
    }
}

pub async fn run(cfg: Cfg, bus_socket: Option<PathBuf>) -> anyhow::Result<()> {
    let web = cfg.at("web");
    let state = Arc::new(StateStore::new());
    let mut bo = aetherrv_core::bus::BusOptions::new(aetherrv_core::names::GATEWAY);
    if let Some(s) = bus_socket {
        bo = bo.socket(s);
    }
    let bus = Bus::start(bo, state);
    bus.subscribe_all_state().await;
    let gate = aetherrv_core::txgate::TxGate::from_config(&cfg);
    let tx_enabled: Map<String, Value> = cfg.at("device").entries().into_iter().map(|(d, _)| (d.clone(), json!(gate.is_enabled(&d)))).collect();
    let flat = Arc::new(Flat::new(Value::Object(tx_enabled)));
    tokio::spawn(follow(bus.clone(), flat.clone()));
    let admin_client = aetherrv_core::admin::AdminClient::new(web.str("admin_socket", aetherrv_core::admin::SOCK));
    let app = Arc::new(App {
        flat,
        auth: RemoteAuth::new(Box::new(admin_client.clone()), &web.str_list("control_subnets"), &web.str("control_tailnet", "owner")),
        admin: Arc::new(admin_client),
        cmds: Arc::new(bus.clone()),
        static_dir: web.str("static_dir", "/opt/aetherrv-rs/ui").into(),
        history_db: cfg.at("history").str("path", "/var/lib/aetherrv/history.sqlite3").into(),
        weather_sock: cfg.at("radar").str("socket", "/run/aetherrv/weather/weather.sock").into(),
        coalesce: Duration::from_millis(web.i64("coalesce_ms", 33).max(5) as u64),
    });
    let addr: SocketAddr = format!("{}:{}", web.str("host", "0.0.0.0"), web.i64("port", 8080)).parse()?;
    let l = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("web UI on http://{addr}");
    aetherrv_core::notify::ready();
    aetherrv_core::notify::spawn_runtime_watchdog();
    let server = axum::serve(l, router(app).into_make_service_with_connect_info::<Conn>());
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        r = server => r?,
        _ = term.recv() => {},
    }
    bus.shutdown().await;
    Ok(())
}
