//! The bus client (ARCHITECTURE.md §4): Mosquitto on a Unix socket, MQTT 5.
//!
//! ```text
//! aetherrv/v1/state/<service>/<key>   retained   {"v": <value>, "t": <ms>}
//! aetherrv/v1/svc/<service>/status    retained   {"up": bool, "pid", "version", "started", "t"}   (last will: up=false)
//! aetherrv/v1/svc/<service>/hb        retained   {"t", "seq"}   every 2 s
//! aetherrv/v1/cmd/<device>            request    (MQTT 5 response topic + correlation data)
//! aetherrv/v1/reply/<client-id>       reply
//! ```
//!
//! Device loops never wait on this module: state goes into the [`StateStore`] (non-blocking) and a background task
//! publishes it whenever the broker is reachable. A reconnect republishes everything with its original `t`.

use crate::command::{CommandHandler, LATE_DETAIL, Reply, Request, ResultKind};
use crate::cmdlog::CommandLog;
use crate::state::{Stamped, StateStore, StateView};
use bytes::Bytes;
use rumqttc::Transport;
use rumqttc::v5::mqttbytes::QoS;
use rumqttc::v5::mqttbytes::v5::{ConnectProperties, LastWill, Packet, Publish, PublishProperties};
use rumqttc::v5::{AsyncClient, Event, EventLoop, MqttOptions};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, oneshot};

pub const PREFIX: &str = "aetherrv/v1";
pub const DEFAULT_SOCKET: &str = "/run/aetherrv/bus/mqtt.sock";
/// An owner whose heartbeat is older than this is down (its keys are unknown).
pub const OWNER_STALE_MS: i64 = 6000;
pub const HEARTBEAT: Duration = Duration::from_secs(2);
/// Remember command ids this long (at-most-once).
const SEEN_MS: i64 = 600_000;

pub fn state_topic(service: &str, key: &str) -> String {
    format!("{PREFIX}/state/{service}/{key}")
}
pub fn cmd_topic(device: &str) -> String {
    format!("{PREFIX}/cmd/{device}")
}

#[derive(Clone, Debug)]
pub struct BusOptions {
    pub socket: PathBuf,
    pub service: String,
    pub version: String,
    /// After the first connect, clear our own retained keys that this process hasn't set within this long.
    pub clear_unset_after: Duration,
}

impl BusOptions {
    pub fn new(service: &str) -> Self {
        BusOptions {
            socket: std::env::var_os("AETHERRV_BUS").map(PathBuf::from).unwrap_or_else(|| DEFAULT_SOCKET.into()),
            service: service.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            clear_unset_after: Duration::from_secs(10),
        }
    }
    pub fn socket(mut self, p: impl Into<PathBuf>) -> Self {
        self.socket = p.into();
        self
    }
}

/// A change to the remote view (for consumers that react, e.g. the gateway).
#[derive(Clone, Debug)]
pub enum Update {
    State { owner: String, key: String, value: Option<Stamped> },
    Health { owner: String },
}

#[derive(Default)]
struct OwnerHealth {
    up: bool,
    hb_ms: i64,
    seen_status: bool,
}

/// What this service knows of other services' state and health.
#[derive(Default)]
pub struct RemoteState {
    keys: Mutex<HashMap<String, Vec<(String, Stamped)>>>,
    owners: Mutex<HashMap<String, OwnerHealth>>,
}

impl RemoteState {
    /// Up and heartbeat fresh.
    pub fn owner_ok(&self, owner: &str) -> bool {
        self.owner_ok_at(owner, crate::clock::now_ms())
    }

    pub fn owner_ok_at(&self, owner: &str, now_ms: i64) -> bool {
        self.owners.lock().unwrap().get(owner).is_some_and(|h| h.up && now_ms - h.hb_ms < OWNER_STALE_MS)
    }

    /// Owners seen so far, with their health.
    pub fn owners(&self) -> Vec<(String, bool, Option<f64>)> {
        let now = crate::clock::now_ms();
        let g = self.owners.lock().unwrap();
        let mut v: Vec<_> = g
            .iter()
            .map(|(k, h)| {
                let age = if h.hb_ms > 0 { Some((now - h.hb_ms) as f64 / 1000.0) } else { None };
                (k.clone(), h.up && now - h.hb_ms < OWNER_STALE_MS, age)
            })
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// The value from a healthy owner (newest `t` if several), ignoring owner health when `raw`.
    pub fn stamped(&self, key: &str, raw: bool) -> Option<(String, Stamped)> {
        let now = crate::clock::now_ms();
        let g = self.keys.lock().unwrap();
        let list = g.get(key)?;
        list.iter()
            .filter(|(o, _)| raw || self.owner_ok_at(o, now))
            .max_by_key(|(_, s)| s.t)
            .cloned()
    }

    /// Keys published by more than one owner (a configuration error).
    pub fn conflicts(&self) -> Vec<(String, Vec<String>)> {
        let g = self.keys.lock().unwrap();
        let mut v: Vec<_> = g
            .iter()
            .filter(|(_, l)| l.len() > 1)
            .map(|(k, l)| (k.clone(), l.iter().map(|(o, _)| o.clone()).collect()))
            .collect();
        v.sort();
        v
    }

    /// Every key with its owner (raw, regardless of health).
    pub fn all(&self) -> Vec<(String, String, Stamped)> {
        let g = self.keys.lock().unwrap();
        let mut out = Vec::new();
        for (k, l) in g.iter() {
            for (o, s) in l {
                out.push((k.clone(), o.clone(), s.clone()));
            }
        }
        out
    }

    fn put(&self, owner: &str, key: &str, value: Option<Stamped>) {
        let mut g = self.keys.lock().unwrap();
        match value {
            Some(s) => {
                let l = g.entry(key.to_string()).or_default();
                match l.iter_mut().find(|(o, _)| o == owner) {
                    Some(e) => e.1 = s,
                    None => l.push((owner.to_string(), s)),
                }
            }
            None => {
                if let Some(l) = g.get_mut(key) {
                    l.retain(|(o, _)| o != owner);
                    if l.is_empty() {
                        g.remove(key);
                    }
                }
            }
        }
    }

    fn health(&self, owner: &str, f: impl FnOnce(&mut OwnerHealth)) {
        let mut g = self.owners.lock().unwrap();
        f(g.entry(owner.to_string()).or_default());
    }

    /// For tests: inject a value as if `owner` had published it (and mark the owner healthy).
    pub fn inject(&self, owner: &str, key: &str, v: Value, t_ms: i64) {
        self.health(owner, |h| {
            h.up = true;
            h.seen_status = true;
            h.hb_ms = crate::clock::now_ms();
        });
        self.put(owner, key, Some(Stamped { v, t: t_ms }));
    }

    /// For tests: mark an owner up/down and refresh its heartbeat.
    pub fn set_owner(&self, owner: &str, up: bool) {
        self.health(owner, |h| {
            h.up = up;
            h.seen_status = true;
            h.hb_ms = crate::clock::now_ms();
        });
    }
}

impl StateView for RemoteState {
    fn get(&self, key: &str) -> Option<Value> {
        self.stamped(key, false).map(|(_, s)| s.v)
    }
    fn age(&self, key: &str) -> Option<f64> {
        self.stamped(key, false).map(|(_, s)| (crate::clock::now_ms() - s.t) as f64 / 1000.0)
    }
    fn keys(&self) -> Vec<String> {
        let now = crate::clock::now_ms();
        let g = self.keys.lock().unwrap();
        let mut v: Vec<String> =
            g.iter().filter(|(_, l)| l.iter().any(|(o, _)| self.owner_ok_at(o, now))).map(|(k, _)| k.clone()).collect();
        v.sort();
        v
    }
}

struct Served {
    handler: Arc<dyn CommandHandler>,
    log: Option<CommandLog>,
}

struct Inner {
    opts: BusOptions,
    client: AsyncClient,
    state: Arc<StateStore>,
    remote: Arc<RemoteState>,
    filters: Mutex<Vec<String>>,
    served: Mutex<HashMap<String, Arc<Served>>>,
    seen: Mutex<VecDeque<(i64, String, Option<Reply>)>>,
    pending: Mutex<HashMap<String, oneshot::Sender<Reply>>>,
    connected: AtomicBool,
    /// `CLOCK_BOOTTIME` ms of the last packet from the broker.
    last_contact: AtomicI64,
    connects: AtomicU64,
    started_ms: i64,
    reply_topic: String,
    updates: broadcast::Sender<Update>,
    own_retained: Mutex<Option<Vec<String>>>,
}

/// A connection to the bus. Cheap to clone.
#[derive(Clone)]
pub struct Bus {
    inner: Arc<Inner>,
}

fn boot_ms() -> i64 {
    crate::clock::boottime().as_millis() as i64
}

impl Bus {
    /// Start the client (it connects, and keeps reconnecting, in the background).
    pub fn start(opts: BusOptions, state: Arc<StateStore>) -> Bus {
        let svc = opts.service.clone();
        let mut mo = MqttOptions::new(svc.clone(), opts.socket.to_string_lossy().to_string(), 0);
        mo.set_transport(Transport::unix());
        mo.set_keep_alive(Duration::from_secs(5));
        mo.set_clean_start(true);
        mo.set_connect_properties(ConnectProperties { max_packet_size: Some(16 << 20), ..Default::default() });
        let will = serde_json::to_vec(&json!({"up": false, "t": crate::clock::now_ms()})).unwrap();
        mo.set_last_will(LastWill::new(format!("{PREFIX}/svc/{svc}/status"), will, QoS::AtLeastOnce, true, None));
        let (client, eventloop) = AsyncClient::new(mo, 4096);
        let (updates, _) = broadcast::channel(8192);
        let inner = Arc::new(Inner {
            reply_topic: format!("{PREFIX}/reply/{svc}"),
            opts,
            client,
            state,
            remote: Arc::new(RemoteState::default()),
            filters: Mutex::new(vec![format!("{PREFIX}/svc/#")]),
            served: Mutex::new(HashMap::new()),
            seen: Mutex::new(VecDeque::new()),
            pending: Mutex::new(HashMap::new()),
            connected: AtomicBool::new(false),
            last_contact: AtomicI64::new(0),
            connects: AtomicU64::new(0),
            started_ms: crate::clock::now_ms(),
            updates,
            own_retained: Mutex::new(Some(Vec::new())),
        });
        let bus = Bus { inner };
        tokio::spawn(bus.clone().event_loop(eventloop));
        tokio::spawn(bus.clone().publisher());
        tokio::spawn(bus.clone().heartbeat());
        tokio::spawn(bus.clone().clear_unset());
        bus
    }

    pub fn service(&self) -> &str {
        &self.inner.opts.service
    }
    pub fn state(&self) -> &Arc<StateStore> {
        &self.inner.state
    }
    pub fn remote(&self) -> &Arc<RemoteState> {
        &self.inner.remote
    }
    pub fn connected(&self) -> bool {
        self.inner.connected.load(Ordering::SeqCst)
    }
    pub fn connects(&self) -> u64 {
        self.inner.connects.load(Ordering::SeqCst)
    }
    /// `CLOCK_BOOTTIME` seconds of the last packet from the broker (`None` = never).
    pub fn last_contact_boottime(&self) -> Option<f64> {
        let v = self.inner.last_contact.load(Ordering::SeqCst);
        if v == 0 { None } else { Some(v as f64 / 1000.0) }
    }
    pub fn updates(&self) -> broadcast::Receiver<Update> {
        self.inner.updates.subscribe()
    }

    /// Follow another service's key (or `#`-suffixed prefix): `subscribe_state("victron", "climate.cabin_front.temp")`.
    pub async fn subscribe_state(&self, owner: &str, key: &str) {
        self.subscribe_filter(state_topic(owner, key)).await
    }

    /// Follow every key of every service (the gateway, the alert engine).
    pub async fn subscribe_all_state(&self) {
        self.subscribe_filter(format!("{PREFIX}/state/#")).await
    }

    async fn subscribe_filter(&self, f: String) {
        let new = {
            let mut g = self.inner.filters.lock().unwrap();
            if g.contains(&f) {
                false
            } else {
                g.push(f.clone());
                true
            }
        };
        if new && self.connected() {
            let _ = self.inner.client.subscribe(f, QoS::AtLeastOnce).await;
        }
    }

    /// Serve commands for `device` (`aetherrv/v1/cmd/<device>`); `log` records late/duplicate refusals.
    pub async fn serve(&self, device: &str, handler: Arc<dyn CommandHandler>, log: Option<CommandLog>) {
        self.inner.served.lock().unwrap().insert(device.to_string(), Arc::new(Served { handler, log }));
        self.subscribe_filter(cmd_topic(device)).await;
    }

    /// Send a command and wait for its reply (the gateway, the power guard). Never retried.
    pub async fn request(&self, device: &str, cmd: &str, value: Value, client: &str, ttl_ms: i64, timeout: Duration) -> Reply {
        if !self.connected() {
            return Reply::refused("the bus is down; nothing sent");
        }
        if !self.inner.remote.owner_ok(device) {
            return Reply::refused(format!("the {device} module is down; nothing sent"));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let req = Request {
            id: id.clone(),
            cmd: cmd.to_string(),
            value,
            client: client.to_string(),
            sent_ms: crate::clock::now_ms(),
            ttl_ms,
        };
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().unwrap().insert(id.clone(), tx);
        let props = PublishProperties {
            response_topic: Some(self.inner.reply_topic.clone()),
            correlation_data: Some(Bytes::from(id.clone())),
            message_expiry_interval: Some(((ttl_ms + 999) / 1000).max(1) as u32),
            ..Default::default()
        };
        let body = serde_json::to_vec(&req).unwrap();
        if let Err(e) = self.inner.client.publish_with_properties(cmd_topic(device), QoS::AtLeastOnce, false, body, props).await {
            self.inner.pending.lock().unwrap().remove(&id);
            return Reply::refused(format!("the bus refused the request ({e}); nothing sent"));
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(r)) => r,
            _ => {
                self.inner.pending.lock().unwrap().remove(&id);
                Reply::unverified(format!("no answer from the {device} module; it may have acted (not retried)"))
            }
        }
    }

    /// Publish a one-shot event (`aetherrv/v1/event/<service>/<name>`).
    pub async fn event(&self, name: &str, payload: &Value) {
        let t = format!("{PREFIX}/event/{}/{name}", self.service());
        let _ = self.inner.client.publish(t, QoS::AtMostOnce, false, serde_json::to_vec(payload).unwrap()).await;
    }

    /// Clean shutdown: mark this service down and disconnect.
    pub async fn shutdown(&self) {
        let body = serde_json::to_vec(&json!({"up": false, "t": crate::clock::now_ms()})).unwrap();
        let _ = self.inner.client.publish(format!("{PREFIX}/svc/{}/status", self.service()), QoS::AtLeastOnce, true, body).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = self.inner.client.disconnect().await;
    }

    // ── background tasks ─────────────────────────────────────────────────────────────────────────────────────────

    async fn event_loop(self, mut ev: EventLoop) {
        let mut backoff = Duration::from_millis(100);
        loop {
            match ev.poll().await {
                Ok(event) => {
                    backoff = Duration::from_millis(100);
                    if let Event::Incoming(p) = event {
                        self.inner.last_contact.store(boot_ms(), Ordering::SeqCst);
                        match p {
                            // the subscribe/publish requests go through a bounded queue this loop drains:
                            // never await them here (a full queue at reconnect would deadlock)
                            Packet::ConnAck(_) => {
                                tokio::spawn(self.clone().on_connect());
                            }
                            Packet::Publish(p) => self.on_publish(p),
                            _ => {}
                        }
                    }
                }
                Err(e) => {
                    if self.inner.connected.swap(false, Ordering::SeqCst) {
                        tracing::warn!("bus: disconnected ({e}); reconnecting");
                    } else {
                        tracing::debug!("bus: connect failed ({e})");
                    }
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(2));
                }
            }
        }
    }

    async fn on_connect(self) {
        let n = self.inner.connects.fetch_add(1, Ordering::SeqCst) + 1;
        tracing::info!("bus: connected to {} (connect #{n})", self.inner.opts.socket.display());
        let c = &self.inner.client;
        let mut filters = self.inner.filters.lock().unwrap().clone();
        filters.push(self.inner.reply_topic.clone());
        if n == 1 {
            filters.push(format!("{PREFIX}/state/{}/#", self.service()));
        }
        for f in filters {
            let _ = c.subscribe(f, QoS::AtLeastOnce).await;
        }
        let status = json!({"up": true, "pid": std::process::id(), "version": self.inner.opts.version,
                            "started": self.inner.started_ms, "t": crate::clock::now_ms()});
        let _ = c
            .publish(format!("{PREFIX}/svc/{}/status", self.service()), QoS::AtLeastOnce, true, serde_json::to_vec(&status).unwrap())
            .await;
        self.inner.connected.store(true, Ordering::SeqCst);
        self.inner.state.mark_all_dirty();
        self.beat(0).await;
    }

    fn on_publish(&self, p: Publish) {
        let topic = String::from_utf8_lossy(&p.topic).to_string();
        let Some(rest) = topic.strip_prefix(PREFIX).and_then(|r| r.strip_prefix('/')) else { return };
        if let Some(r) = rest.strip_prefix("state/") {
            let Some((owner, key)) = r.split_once('/') else { return };
            if owner == self.service() {
                if p.retain
                    && let Some(list) = self.inner.own_retained.lock().unwrap().as_mut()
                {
                    list.push(key.to_string());
                }
                return;
            }
            let value = if p.payload.is_empty() {
                None
            } else {
                match serde_json::from_slice::<Value>(&p.payload) {
                    Ok(j) => Some(Stamped { v: j.get("v").cloned().unwrap_or(Value::Null), t: j.get("t").and_then(Value::as_i64).unwrap_or(0) }),
                    Err(_) => return,
                }
            };
            self.inner.remote.put(owner, key, value.clone());
            let _ = self.inner.updates.send(Update::State { owner: owner.to_string(), key: key.to_string(), value });
        } else if let Some(r) = rest.strip_prefix("svc/") {
            let Some((owner, what)) = r.split_once('/') else { return };
            let j: Value = serde_json::from_slice(&p.payload).unwrap_or(Value::Null);
            match what {
                "status" => self.inner.remote.health(owner, |h| {
                    h.up = j.get("up").and_then(Value::as_bool).unwrap_or(false);
                    h.seen_status = true;
                    if h.up {
                        h.hb_ms = h.hb_ms.max(j.get("t").and_then(Value::as_i64).unwrap_or(0));
                    }
                }),
                "hb" => self.inner.remote.health(owner, |h| h.hb_ms = j.get("t").and_then(Value::as_i64).unwrap_or(0)),
                _ => return,
            }
            let _ = self.inner.updates.send(Update::Health { owner: owner.to_string() });
        } else if let Some(dev) = rest.strip_prefix("cmd/") {
            self.on_command(dev.to_string(), p);
        } else if rest.starts_with("reply/") {
            let Ok(j) = serde_json::from_slice::<Value>(&p.payload) else { return };
            let id = j.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
            let reply = Reply {
                result: serde_json::from_value(j.get("result").cloned().unwrap_or(Value::Null)).unwrap_or(ResultKind::Unverified),
                detail: j.get("detail").and_then(Value::as_str).unwrap_or_default().to_string(),
            };
            if let Some(tx) = self.inner.pending.lock().unwrap().remove(&id) {
                let _ = tx.send(reply);
            }
        }
    }

    fn on_command(&self, device: String, p: Publish) {
        let Some(served) = self.inner.served.lock().unwrap().get(&device).cloned() else { return };
        if p.retain {
            tracing::warn!("bus: ignoring a RETAINED command on {device} (commands are never retained)");
            return;
        }
        let Ok(req) = serde_json::from_slice::<Request>(&p.payload) else {
            tracing::warn!("bus: malformed command on {device}");
            return;
        };
        let props = p.properties.unwrap_or_default();
        let resp_topic = props.response_topic.clone();
        let corr = props.correlation_data.clone();
        let now = crate::clock::now_ms();
        // at most once: a repeat gets the first answer (or nothing while it is still running)
        {
            let mut seen = self.inner.seen.lock().unwrap();
            while seen.front().is_some_and(|(t, _, _)| now - t > SEEN_MS) {
                seen.pop_front();
            }
            if let Some((_, _, r)) = seen.iter().find(|(_, id, _)| *id == req.id) {
                if let Some(r) = r.clone() {
                    let bus = self.clone();
                    let id = req.id.clone();
                    tokio::spawn(async move { bus.reply(resp_topic, corr, &id, &r).await });
                }
                return;
            }
            seen.push_back((now, req.id.clone(), None));
        }
        let bus = self.clone();
        tokio::spawn(async move {
            let late = now - req.sent_ms > req.ttl_ms;
            let reply = if late && !served.handler.ttl_exempt(&req.cmd, &req.value) {
                if let Some(l) = &served.log {
                    l.log(&req.cmd, &req.value, "rejected", &format!("arrived {} ms after it was sent", now - req.sent_ms), &req.client);
                }
                Reply::refused(LATE_DETAIL)
            } else {
                served.handler.handle(req.cmd.clone(), req.value.clone(), req.client.clone()).await
            };
            if let Some(e) = bus.inner.seen.lock().unwrap().iter_mut().find(|(_, id, _)| *id == req.id) {
                e.2 = Some(reply.clone());
            }
            bus.reply(resp_topic, corr, &req.id, &reply).await;
        });
    }

    async fn reply(&self, topic: Option<String>, corr: Option<Bytes>, id: &str, r: &Reply) {
        let Some(topic) = topic else { return };
        let body = serde_json::to_vec(&json!({"id": id, "result": r.result, "detail": r.detail})).unwrap();
        let props = PublishProperties { correlation_data: corr, ..Default::default() };
        let _ = self.inner.client.publish_with_properties(topic, QoS::AtLeastOnce, false, body, props).await;
    }

    async fn publisher(self) {
        loop {
            if self.connected() && self.inner.state.has_dirty() {
                let (dirty, removed) = self.inner.state.take_dirty();
                for (k, s) in dirty {
                    let body = serde_json::to_vec(&json!({"v": s.v, "t": s.t})).unwrap();
                    if self.inner.client.publish(state_topic(self.service(), &k), QoS::AtMostOnce, true, body).await.is_err() {
                        break;
                    }
                }
                for k in removed {
                    let _ = self.inner.client.publish(state_topic(self.service(), &k), QoS::AtMostOnce, true, Vec::<u8>::new()).await;
                }
            }
            // wake on a change, or at least every 250 ms (a reconnect marks everything dirty)
            let _ = tokio::time::timeout(Duration::from_millis(250), self.inner.state.changed()).await;
            // coalesce bursts: a loop that sets 40 keys publishes them together
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    async fn beat(&self, seq: u64) {
        let body = serde_json::to_vec(&json!({"t": crate::clock::now_ms(), "seq": seq})).unwrap();
        let _ = self.inner.client.publish(format!("{PREFIX}/svc/{}/hb", self.service()), QoS::AtLeastOnce, true, body).await;
    }

    async fn heartbeat(self) {
        let mut seq = 0u64;
        let mut t = tokio::time::interval(HEARTBEAT);
        loop {
            t.tick().await;
            seq += 1;
            if self.connected() {
                self.beat(seq).await;
            }
        }
    }

    async fn clear_unset(self) {
        while self.connects() == 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        tokio::time::sleep(self.inner.opts.clear_unset_after).await;
        let old = self.inner.own_retained.lock().unwrap().take().unwrap_or_default();
        let _ = self.inner.client.unsubscribe(format!("{PREFIX}/state/{}/#", self.service())).await;
        for k in old {
            if !self.inner.state.contains(&k) {
                tracing::info!("bus: clearing stale retained key {k} (no longer published)");
                let _ = self.inner.client.publish(state_topic(self.service(), &k), QoS::AtMostOnce, true, Vec::<u8>::new()).await;
            }
        }
    }
}
