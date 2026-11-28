//! `aetherrv-victron`: the Victron Cerbo GX over its built-in MQTT broker (port of
//! `src/aetherrv/adapters/victron.py`).
//!
//! Topic layout (Victron's dbus-flashmq):
//! ```text
//! N/<portal id>/<service>/<instance>/<path>   payload {"value": ...}
//! R/<portal id>/keepalive                      a read request that keeps N/ topics publishing
//! R/<portal id>/<path>                         ask the Cerbo to republish one path (the fresh read)
//! W/<portal id>/<path>                         a write, only through `command()` and the TxGate
//! ```
//! EPOCH batteries reach us through here too: their BMS talks to the Cerbo's BMS-Can port, and the Cerbo republishes
//! them under `battery/<instance>/...` and `system/0/Dc/Battery/...`.
//!
//! Commands (`victron`: `inverter_mode`, `shore_limit`, `mppt_mode`, `ack_alarms`): TxGate → fresh `R/` read →
//! compare → one `W/` → verify → never retried, every attempt in `command_log`.

pub mod control;
pub mod model;
pub mod mqtt;

use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::cmdlog::CommandLog;
use aetherrv_core::command::{BoxFuture, CommandHandler, Reply};
use aetherrv_core::config::Cfg;
use aetherrv_core::pyfmt::{py_repr, py_str};
use aetherrv_core::state::StateStore;
use aetherrv_core::txgate::TxGate;
use model::{Effects, Io, Model, Role, Want};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The TxGate device and command device.
pub const DEVICE: &str = "victron";
/// How long a fresh read may take (Python: 4 s).
pub const FRESH_READ: Duration = Duration::from_secs(4);
/// How long a write may take to show (Python `verify_timeout=12.0`).
pub const VERIFY: Duration = Duration::from_secs(12);
/// How long the acknowledge may take to show (Python: 8 s).
pub const ACK_VERIFY: Duration = Duration::from_secs(8);

/// The connection to the Cerbo as `command()` and the message handler use it (Python's aiomqtt client): requests
/// are queued without waiting, and fail when the connection they belong to is gone.
pub trait Link: Send + Sync {
    fn subscribe(&self, filter: &str) -> Result<(), String>;
    fn publish(&self, topic: &str, payload: &[u8]) -> Result<(), String>;
}

/// The Cerbo connection settings from `[device.<name>]` (kind = "victron") and the secrets file.
#[derive(Clone)]
pub struct Settings {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub username: Option<String>,
    pub password: Option<String>,
    pub roles: Vec<Role>,
}

impl std::fmt::Debug for Settings {
    // never print the credentials
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settings")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("tls", &self.tls)
            .field("username", &self.username.as_ref().map(|_| "***"))
            .field("roles", &self.roles)
            .finish()
    }
}

/// The rear cabin Ruuvi (`climate.cabin_rear.*`, owner 2026-11-27), only when `cabin_rear_temp` names one (a Custom
/// name or an instance). Unlike the other roles there is no by-type stand-in: the first "room" sensor would be the
/// front one, counted twice in the cabin mean.
pub fn rear_role(v: Option<&Value>) -> Option<Role> {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => None,
        Some(_) => Some(Role { name: "cabin_rear".into(), want: Want::from_value(v), kind: "room" }),
    }
}

impl Settings {
    /// As Python's `build()`: the first `[device.*]` with `kind = "victron"` (`None` if there is none).
    /// `cabin_temp` is used only when `[climate] cabin_source` is "victron" (the default) or "average"; with
    /// "average" this service publishes `climate.cabin_front.*` and `aetherrv-derive` the mean.
    pub fn from_config(cfg: &Cfg, secrets: &Cfg) -> anyhow::Result<Option<Settings>> {
        let Some((name, d)) = cfg.at("device").entries().into_iter().find(|(_, d)| d.str("kind", "") == "victron") else {
            return Ok(None);
        };
        let host = d.opt_str("host").ok_or_else(|| anyhow::anyhow!("[device.{name}]: host is missing"))?;
        let port = d.i64("port", 1883);
        let port = u16::try_from(port).map_err(|_| anyhow::anyhow!("[device.{name}]: bad port {port}"))?;
        let source = cfg.at("climate").get("cabin_source").cloned().unwrap_or(json!("victron"));
        let (cabin, cabin_role) = match source.as_str() {
            Some("victron") => (Want::from_value(d.get("cabin_temp")), "cabin"),
            Some("average") => (Want::from_value(d.get("cabin_temp")), "cabin_front"),
            _ => (Want::Off, "cabin"),
        };
        let secret = |k: &str| secrets.opt_str(k).filter(|s| !s.is_empty());
        let mut roles = model::roles(
            cabin_role,
            cabin,
            Want::from_value(d.get("outside_temp")),
            Want::from_value(d.get("fridge_temp")),
            Want::from_value(d.get("freezer_temp")),
        );
        roles.extend(rear_role(d.get("cabin_rear_temp")));
        Ok(Some(Settings {
            host,
            port,
            tls: d.bool("tls", false),
            username: secret("victron_mqtt_user"),
            password: secret("victron_mqtt_password"),
            roles,
        }))
    }
}

/// The service core: the Cerbo model, the state it publishes and the command path.
pub struct Victron {
    pub state: Arc<StateStore>,
    pub gate: TxGate,
    pub log: CommandLog,
    model: Mutex<Model>,
    /// Bumped on every control-path update (Python's `_changed` condition).
    changed: tokio::sync::watch::Sender<u64>,
    link: Mutex<Option<Arc<dyn Link>>>,
    cmd_lock: tokio::sync::Mutex<()>,
}

impl Victron {
    pub fn new(state: Arc<StateStore>, gate: TxGate, log: CommandLog, roles: Vec<Role>) -> Arc<Victron> {
        let enabled = gate.is_enabled(DEVICE);
        Arc::new(Victron {
            state,
            gate,
            log,
            model: Mutex::new(Model::new(roles, enabled)),
            changed: tokio::sync::watch::channel(0).0,
            link: Mutex::new(None),
            cmd_lock: tokio::sync::Mutex::new(()),
        })
    }

    fn m(&self) -> std::sync::MutexGuard<'_, Model> {
        self.model.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Read the model (tests, diagnostics).
    pub fn with_model<T>(&self, f: impl FnOnce(&Model) -> T) -> T {
        f(&self.m())
    }

    /// Set the portal id (tests: Python `a.portal = "p"`).
    pub fn set_portal(&self, portal: &str) {
        self.m().portal = Some(portal.to_string());
    }

    /// The current connection (Python `self._client`), `None` when disconnected.
    pub fn set_link(&self, link: Option<Arc<dyn Link>>) {
        let up = link.is_some();
        *self.link.lock().unwrap_or_else(|e| e.into_inner()) = link;
        self.m().link_up = up;
    }

    pub fn link(&self) -> Option<Arc<dyn Link>> {
        self.link.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// A new connection is up: clean session, so the data topics must be subscribed again.
    pub fn on_connect(&self, link: Arc<dyn Link>) {
        self.set_link(Some(link.clone()));
        self.set_subscribed(false);
        self.state.set("victron.connected", json!(true));
        if let Err(e) = link.subscribe("N/+/system/0/Serial") {
            tracing::warn!("victron: subscribe failed: {e}");
        }
    }

    /// The connection is gone (Python: `_client = None`, `victron.connected = False`).
    pub fn on_disconnect(&self) {
        self.set_link(None);
        self.state.set("victron.connected", json!(false));
    }

    /// What `run()` does on each connect (tests: Python `a._subscribed = False`).
    pub fn set_subscribed(&self, v: bool) {
        self.m().subscribed = v;
    }

    fn apply(&self, link: Option<&dyn Link>, fx: Effects) {
        self.state.update(fx.state);
        if let Some(link) = link {
            for io in fx.io {
                let r = match &io {
                    Io::Subscribe(f) => link.subscribe(f),
                    Io::Publish(t, p) => link.publish(t, p),
                };
                if let Err(e) = r {
                    tracing::warn!("victron: {io:?} failed: {e}");
                }
            }
        }
        if fx.control {
            self.changed.send_modify(|g| *g += 1);
        }
    }

    /// One MQTT message from the Cerbo (Python `_on_message(c, topic, payload)`).
    pub fn on_message(&self, link: &dyn Link, topic: &str, payload: &[u8]) {
        let fx = self.m().on_message(topic, payload);
        self.apply(Some(link), fx);
    }

    /// A control-relevant value (Python `_on_control_path`; the tests' FakeCerbo calls it).
    pub fn on_control_path(&self, rest: &str, value: Value) {
        let mut fx = Effects::default();
        self.m().on_control_path(rest, value, &mut fx);
        self.apply(None, fx);
    }

    /// Python `_on_temp(rest, value)`.
    pub fn on_temp(&self, rest: &str, value: Value) {
        let mut fx = Effects::default();
        self.m().on_temp(rest, value, &mut fx);
        self.apply(None, fx);
    }

    /// Python `_on_battery(rest, value)`.
    pub fn on_battery(&self, rest: &str, value: Value) {
        self.m().on_battery(rest, value);
    }

    pub fn batteries(&self) -> Vec<Value> {
        self.m().batteries()
    }

    /// One tick of the battery flush (every 1 s while connected).
    pub fn flush_batteries(&self) {
        let keys = self.m().flush_batteries();
        self.state.update(keys);
    }

    /// The keepalive request for the known portal (every 30 s while connected).
    pub fn keepalive(&self, link: &dyn Link) {
        let portal = self.m().portal.clone();
        if let Some(p) = portal
            && let Err(e) = link.publish(&format!("R/{p}/keepalive"), b"")
        {
            tracing::warn!("victron: keepalive not sent: {e}");
        }
    }

    /// `tx.enabled.victron` / `tx.blocked.victron` (ARCHITECTURE.md §4.5).
    pub fn publish_tx(&self) {
        let blocked = self.gate.blocked_count().get(DEVICE).copied().unwrap_or(0);
        self.state.update([
            (format!("tx.enabled.{DEVICE}"), json!(self.gate.is_enabled(DEVICE))),
            (format!("tx.blocked.{DEVICE}"), json!(blocked)),
        ]);
    }

    /// Wait until `pred` holds on the model, or the timeout (Python `_wait`).
    async fn wait(&self, pred: impl Fn(&Model) -> bool, timeout: Duration) -> bool {
        let mut rx = self.changed.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if pred(&self.m()) {
                return true;
            }
            match tokio::time::timeout_at(deadline, rx.changed()).await {
                Ok(Ok(())) => continue,
                _ => return false,
            }
        }
    }

    /// The command path with the default verify window.
    pub async fn command(&self, cmd: &str, value: &Value, client: &str) -> Reply {
        self.command_with(cmd, value, client, VERIFY).await
    }

    /// Python `command(cmd, value, client, verify_timeout)`: serialized, fresh read, once, verified, never retried.
    pub async fn command_with(&self, cmd: &str, value: &Value, client: &str, verify: Duration) -> Reply {
        let _g = self.cmd_lock.lock().await;
        if self.gate.check(DEVICE, &format!("{cmd}={}", py_str(value)), false).is_err() {
            self.log.log(cmd, value, "blocked", "tx_enabled false", client);
            return Reply::refused("Victron control is disabled");
        }
        let portal = self.m().portal.clone();
        let (Some(link), Some(portal)) = (self.link(), portal) else {
            return Reply::refused("not connected to the Cerbo");
        };
        if cmd == "ack_alarms" {
            // owner-requested 2026-11-21: acknowledge (silence) Cerbo alarms; the value is not used
            return self.ack_alarms(&*link, &portal, client).await;
        }
        let planned = control::plan(&self.m(), cmd, value);
        let (path, target) = match planned {
            Ok(p) => p,
            Err(e) => {
                self.log.log(cmd, value, "rejected", &e, client);
                return Reply::refused(e);
            }
        };
        // fresh read: ask the Cerbo to republish this path, then compare
        let n0 = self.m().upd(&path);
        if let Err(e) = link.publish(&format!("R/{portal}/{path}"), b"") {
            self.log.log(cmd, value, "failed", &format!("read request for {path} not sent: {e}"), client);
            return Reply::refused("not connected to the Cerbo");
        }
        if !self.wait(|m| m.upd(&path) > n0, FRESH_READ).await {
            self.log.log(cmd, value, "failed", &format!("no fresh value for {path}"), client);
            return Reply::refused("the Cerbo did not report the current value");
        }
        if control::same(self.m().vals.get(&path), &target) {
            self.log.log(cmd, value, "noop", "already in requested state", client);
            return Reply::ok("already set");
        }
        // exactly once, never retried (QoS 0 on a connection of its own: nothing is resent after a reconnect)
        if let Err(e) = link.publish(&format!("W/{portal}/{path}"), &control::write_payload(&target)) {
            self.log.log(cmd, value, "failed", &format!("write to {path} not sent: {e}"), client);
            return Reply::refused("not connected to the Cerbo; nothing sent");
        }
        if self.wait(|m| control::same(m.vals.get(&path), &target), verify).await {
            self.log.log(cmd, value, "ok", &format!("{path}={} verified", py_str(&target)), client);
            return Reply::ok("confirmed by the Cerbo");
        }
        let now = py_repr(&self.m().vals.get(&path).cloned().unwrap_or(Value::Null));
        self.log.log(cmd, value, "unverified", &format!("{path} still {now}"), client);
        Reply::unverified(format!("the Cerbo did not confirm (now {now}; not retried)"))
    }

    /// Fresh read of the unacknowledged counts → `AcknowledgeAll = 1` once → verify both reach 0.
    async fn ack_alarms(&self, link: &dyn Link, portal: &str, client: &str) -> Reply {
        let one = json!(1);
        let n0: Vec<u64> = control::ACK_COUNT.iter().map(|p| self.m().upd(p)).collect();
        for p in control::ACK_COUNT {
            if let Err(e) = link.publish(&format!("R/{portal}/{p}"), b"") {
                self.log.log("ack_alarms", &one, "failed", &format!("read request not sent: {e}"), client);
                return Reply::refused("not connected to the Cerbo");
            }
        }
        let fresh = |m: &Model| control::ACK_COUNT.iter().zip(&n0).all(|(p, n)| m.upd(p) > *n);
        if !self.wait(fresh, FRESH_READ).await {
            self.log.log("ack_alarms", &one, "failed", "no fresh notification counts", client);
            return Reply::refused("the Cerbo did not report its alarm counts");
        }
        if !control::unacknowledged(&self.m()) {
            self.log.log("ack_alarms", &one, "noop", "nothing unacknowledged", client);
            return Reply::ok("nothing to acknowledge");
        }
        if let Err(e) = link.publish(&format!("W/{portal}/{}", control::ACK_ALL), &control::write_payload(&one)) {
            self.log.log("ack_alarms", &one, "failed", &format!("write not sent: {e}"), client);
            return Reply::refused("not connected to the Cerbo; nothing sent");
        }
        if self.wait(|m| !control::unacknowledged(m), ACK_VERIFY).await {
            self.log.log("ack_alarms", &one, "ok", "unacknowledged alarms/warnings now 0", client);
            return Reply::ok("acknowledged on the Cerbo (buzzer off)");
        }
        let counts = py_repr(&control::ack_counts(&self.m()));
        self.log.log("ack_alarms", &one, "unverified", &format!("counts still {counts}"), client);
        Reply::unverified("the Cerbo did not confirm (not retried)")
    }
}

struct Handler(Arc<Victron>);

impl CommandHandler for Handler {
    fn handle(&self, cmd: String, value: Value, client: String) -> BoxFuture<Reply> {
        let me = self.0.clone();
        Box::pin(async move { me.command(&cmd, &value, &client).await })
    }
}

pub struct Options {
    pub bus_socket: Option<PathBuf>,
    pub db_path: Option<PathBuf>,
}

impl Options {
    pub fn from_config(cfg: &Cfg) -> Options {
        Options { bus_socket: None, db_path: Some(cfg.at("history").str("path", "/var/lib/aetherrv/history.sqlite3").into()) }
    }
}

/// Run the service until SIGTERM/SIGINT.
pub async fn run(cfg: Cfg, opts: Options) -> anyhow::Result<()> {
    let settings = Settings::from_config(&cfg, &aetherrv_core::config::load_secrets(&cfg))?;
    let state = Arc::new(StateStore::new());
    let mut bo = BusOptions::new(aetherrv_core::names::VICTRON);
    if let Some(s) = opts.bus_socket {
        bo = bo.socket(s);
    }
    let bus = Bus::start(bo, state.clone());
    let gate = TxGate::from_config(&cfg).only(&[DEVICE]);
    let log = CommandLog::new(DEVICE, opts.db_path);
    let roles = settings.as_ref().map(|s| s.roles.clone()).unwrap_or_default();
    let v = Victron::new(state, gate, log.clone(), roles);
    match settings {
        Some(s) => {
            bus.serve(DEVICE, Arc::new(Handler(v.clone())), Some(log)).await;
            tracing::info!("victron: Cerbo at {}:{}{}", s.host, s.port, if s.tls { " (TLS)" } else { "" });
            tokio::spawn(mqtt::connection_loop(v.clone(), s));
        }
        None => tracing::warn!("victron: no [device.*] with kind = \"victron\" in the config; idle"),
    }
    {
        let v = v.clone();
        tokio::spawn(async move {
            loop {
                v.publish_tx();
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
    }
    aetherrv_core::notify::ready();
    aetherrv_core::notify::spawn_runtime_watchdog();
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = term.recv() => {},
        _ = tokio::signal::ctrl_c() => {},
    }
    aetherrv_core::notify::stopping();
    bus.shutdown().await;
    Ok(())
}
