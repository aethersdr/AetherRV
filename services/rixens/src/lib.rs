//! `aetherrv-rixens`: the Rixens MCS7 over its local HTTP API (Rixens "MCS Local API"). A port of
//! `adapters/rixens_http.py` (`RixensHttp` + `RixensHttpAdapter`).
//!
//! ```text
//! GET /status.xml                  full snapshot (polled every `interval` s; never < 1 s apart)
//! GET /interface.cgi?act=N&val=V   commands (integers only)
//! ```
//!
//! CONTROL (owner-approved): setpoint, fan, furnace/electric/engine, hot water, engine pre-heat, cabin heat
//! (thermostat). Every rule is in `command()`: the hard allowlist (`proto_mcs7::http::plan`), TxGate device
//! "rixens_http", a fresh status read immediately before acting, send only if different, send ONCE, poll to verify,
//! NEVER retry (the source acts are toggles: a retry could flip the state back), one lock + ≥1 s spacing shared by
//! polls and commands. Nothing is sent autonomously except the heat-lockout backstop (ARCHITECTURE.md §8): the
//! MCS7 stays the thermostat, and a dead Pi changes nothing. Wi-Fi secrets in status.xml are never parsed.

use aetherrv_core::cmdlog::CommandLog;
use aetherrv_core::command::{BoxFuture, CommandHandler, Reply};
use aetherrv_core::config::Cfg;
use aetherrv_core::state::{StateExt, StateStore, StateView};
use aetherrv_core::txgate::TxGate;
use aetherrv_power::LockoutTracker;
use proto_mcs7::http::{FuelAccount, PlanError, Status, parse_status, plan, publish_state};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const DEVICE: &str = "rixens_http";
/// The bus name of the commands (the gateway's device "rixens").
pub const CMD_DEVICE: &str = "rixens";
pub const GUARD_FRESH_S: f64 = 30.0;
/// Commands that start heat (refused while the house battery is locked out).
pub const HEAT_ON: [&str; 4] = ["furnace", "electric", "hot_water", "preheat"];

/// Serialized HTTP access: one lock, ≥ `min_gap` seconds between any two requests.
pub struct Mcs7Http {
    base: String,
    min_gap: Duration,
    client: reqwest::Client,
    last: tokio::sync::Mutex<Option<Instant>>,
}

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("{0}")]
    Http(String),
    #[error("{0}")]
    Parse(String),
}

impl Mcs7Http {
    pub fn new(host: &str, min_gap: f64) -> Self {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(8)).gzip(true).build().expect("http client");
        Mcs7Http { base: format!("http://{host}"), min_gap: Duration::from_secs_f64(min_gap), client, last: tokio::sync::Mutex::new(None) }
    }

    async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Vec<u8>, HttpError> {
        let mut last = self.last.lock().await;
        if let Some(t) = *last {
            let since = t.elapsed();
            if since < self.min_gap {
                tokio::time::sleep(self.min_gap - since).await;
            }
        }
        // the query values are integers and fixed names: no escaping needed
        let qs: Vec<String> = query.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let url = if qs.is_empty() { format!("{}{path}", self.base) } else { format!("{}{path}?{}", self.base, qs.join("&")) };
        let r = self.client.get(url).send().await;
        *last = Some(Instant::now());
        let r = r.map_err(|e| HttpError::Http(e.to_string()))?;
        let r = r.error_for_status().map_err(|e| HttpError::Http(e.to_string()))?;
        r.bytes().await.map(|b| b.to_vec()).map_err(|e| HttpError::Http(e.to_string()))
    }

    pub async fn status(&self) -> Result<Status, HttpError> {
        let b = self.get("/status.xml", &[]).await?;
        parse_status(&b).map_err(|e| HttpError::Parse(format!("{e:?}")))
    }

    pub async fn send(&self, act: i64, val: i64) -> Result<(), HttpError> {
        self.get("/interface.cgi", &[("act", act.to_string()), ("val", val.to_string())]).await.map(|_| ())
    }
}

pub struct Mcs7 {
    pub http: Mcs7Http,
    pub gate: TxGate,
    pub log: CommandLog,
    pub state: Arc<StateStore>,
    pub cabin_from_rixens: bool,
    pub interval: f64,
    pub stale_after: f64,
    pub fuel: Mutex<FuelAccount>,
    pub last: Mutex<Option<Status>>,
    pub last_ok: Mutex<f64>,
    db: Option<std::path::PathBuf>,
    cmd_lock: tokio::sync::Mutex<()>,
    pub lockout: Mutex<LockoutTracker>,
    pub lockout_enabled: bool,
    last_backstop: Mutex<Option<f64>>,
    /// Other services' keys (the battery readings and the guard's lockout).
    pub view: Mutex<Option<Arc<dyn StateView>>>,
}

fn now_s() -> f64 {
    aetherrv_core::clock::now_s()
}

impl Mcs7 {
    pub fn new(cfg: &Cfg, state: Arc<StateStore>, gate: TxGate, db: Option<std::path::PathBuf>) -> Arc<Mcs7> {
        let d = cfg.at("device").entries().into_iter().find(|(_, d)| d.str("kind", "") == "rixens_http").map(|x| x.1).unwrap_or_default();
        let cabin_source = cfg.at("climate").str("cabin_source", "victron");
        let mut fuel = FuelAccount::new(d.opt_f64("fuel_ml_per_pulse"), &aetherrv_core::clock::local_day(now_s()));
        if let Some(p) = &db
            && let Some((day, v)) = load_fuel(p)
        {
            fuel.restore(&day, v);
        }
        let p = cfg.at("power");
        let lockout = LockoutTracker::persisted(
            p.f64("heater_off_soc", 10.0),
            p.f64("hold_s", 120.0),
            p.str("rixens_lockout_state", "/var/lib/aetherrv/rixens/lockout.json"),
        );
        Arc::new(Mcs7 {
            http: Mcs7Http::new(&d.str("host", ""), d.f64("min_gap_s", 1.0)),
            gate,
            log: CommandLog::new(DEVICE, db.clone()),
            state,
            cabin_from_rixens: cabin_source == "rixens",
            interval: d.f64("interval", 5.0),
            stale_after: 15.0,
            fuel: Mutex::new(fuel),
            last: Mutex::new(None),
            last_ok: Mutex::new(0.0),
            db,
            cmd_lock: tokio::sync::Mutex::new(()),
            lockout: Mutex::new(lockout),
            lockout_enabled: p.bool("auto_shutdown", false),
            last_backstop: Mutex::new(None),
            view: Mutex::new(None),
        })
    }

    fn logc(&self, cmd: &str, value: &Value, outcome: &str, detail: &str, client: &str) {
        self.log.log(cmd, value, outcome, detail, client);
    }

    /// Python `_publish`: state, fuel accounting (persisted), reachable.
    pub fn publish(&self, d: &Status) {
        let now = now_s();
        let persist = self.fuel.lock().unwrap().account(d.fuel_hz, now, &aetherrv_core::clock::local_day(now));
        if persist
            && let Some(p) = &self.db
        {
            let f = self.fuel.lock().unwrap();
            save_fuel(p, f.day(), f.today_l());
        }
        let keys = publish_state(d, self.cabin_from_rixens, &self.fuel.lock().unwrap(), now);
        self.state.update(keys);
        *self.last.lock().unwrap() = Some(d.clone());
        *self.last_ok.lock().unwrap() = now;
    }

    pub async fn poll_once(&self) -> Result<(), HttpError> {
        let s = self.http.status().await?;
        self.publish(&s);
        Ok(())
    }

    pub async fn run_poll(self: Arc<Self>) {
        let mut backoff = self.interval;
        loop {
            match self.poll_once().await {
                Ok(()) => backoff = self.interval,
                Err(e) => {
                    tracing::debug!("rixens http poll failed: {e}");
                    self.state.set("climate.rixens.reachable", json!(false));
                    backoff = (backoff * 2.0).min(60.0);
                }
            }
            let stale = now_s() - *self.last_ok.lock().unwrap() > self.stale_after;
            self.state.set("climate.rixens.controls_ok", json!(self.gate.is_enabled(DEVICE) && !stale));
            if stale && self.last.lock().unwrap().is_some() {
                self.state.set("climate.rixens.state", json!("no data"));
            }
            tokio::time::sleep(Duration::from_secs_f64(backoff)).await;
        }
    }

    fn locked(&self) -> bool {
        let own = self.lockout_enabled && self.lockout.lock().unwrap().lockout;
        let guard = self.view.lock().unwrap().as_ref().is_some_and(|v| {
            v.is_true("power.heat_lockout") && v.age("power.heat_lockout").is_some_and(|a| a < GUARD_FRESH_S)
        });
        own || guard
    }

    /// The command path (Python `command()`).
    pub async fn command(&self, cmd: &str, value: &Value, client: &str, verify_timeout: f64) -> Reply {
        let _g = self.cmd_lock.lock().await;
        // safety-relevant values must have the right JSON type (D11): never Python's bool("false") == True
        let typed = match cmd {
            "furnace" | "electric" | "engine" | "hot_water" | "preheat" | "thermostat" => value.is_boolean(),
            "setpoint" => value.is_number(),
            "fan" => value.as_str() == Some("auto") || value.is_i64() || value.is_u64(),
            _ => true,
        };
        if !typed {
            self.logc(cmd, value, "rejected", "bad value type", client);
            return Reply::refused("bad request");
        }
        if HEAT_ON.contains(&cmd) && value.as_bool() == Some(true) && self.locked() {
            // lowbatt.py: protect the burner's after-run
            self.logc(cmd, value, "rejected", "heat locked out: house battery low", client);
            return Reply::refused("Heat is locked out: the house battery is too low. Charge first.");
        }
        if let Err(e) = self.gate.check(DEVICE, &format!("{cmd}={}", aetherrv_core::pyfmt::py_str(value)), false) {
            self.logc(cmd, value, "blocked", &e.to_string(), client);
            return Reply::refused("control is disabled for the MCS7");
        }
        let cur = match self.http.status().await {
            Ok(s) => s, // fresh read, never the poll cache
            Err(e) => {
                self.logc(cmd, value, "failed", &format!("status read: {e}"), client);
                return Reply::refused("MCS7 not reachable");
            }
        };
        self.publish(&cur);
        let p = match plan(cmd, value, &cur) {
            Ok(p) => p,
            Err(PlanError::Rejected(e)) => {
                self.logc(cmd, value, "rejected", &e, client);
                return Reply::refused(e);
            }
            Err(PlanError::BadValue(e)) => {
                self.logc(cmd, value, "failed", &format!("status read: {e}"), client);
                return Reply::refused("MCS7 not reachable");
            }
        };
        let Some(p) = p else {
            self.logc(cmd, value, "noop", "already in requested state", client);
            return Reply::ok("already set");
        };
        if let Err(e) = self.http.send(p.act, p.val).await {
            // exactly once, never retried
            self.logc(cmd, value, "failed", &format!("send act={} val={}: {e}", p.act, p.val), client);
            return Reply::unverified("send failed (not retried)");
        }
        let deadline = Instant::now() + Duration::from_secs_f64(verify_timeout);
        while Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(1500).min(Duration::from_secs_f64(verify_timeout.max(0.01)))).await;
            if let Ok(s) = self.http.status().await {
                self.publish(&s);
                if p.verify.check(&s) {
                    self.logc(cmd, value, "ok", &format!("act={} val={} verified", p.act, p.val), client);
                    return Reply::ok("confirmed by the MCS7");
                }
            }
        }
        self.logc(cmd, value, "unverified", &format!("act={} val={}: state did not change within {:.0}s", p.act, p.val, verify_timeout), client);
        Reply::unverified("the MCS7 did not confirm the change (it may clamp or delay; not retried)")
    }

    /// The heat lockout inside this service (§8): refuse heat starts (in `command`), and turn heat off only when the
    /// guard isn't there.
    pub async fn lockout_tick(&self) {
        if !self.lockout_enabled {
            return;
        }
        let Some(view) = self.view.lock().unwrap().clone() else { return };
        let wall = now_s();
        self.lockout.lock().unwrap().tick_state(&*view, wall);
        let own = self.lockout.lock().unwrap().lockout;
        let guard_fresh = view.age("power.heat_lockout").is_some_and(|a| a < GUARD_FRESH_S);
        self.state.set("climate.rixens.lockout", json!(own || (guard_fresh && view.is_true("power.heat_lockout"))));
        let heating: Vec<&str> = {
            let l = self.last.lock().unwrap();
            let Some(s) = l.as_ref() else { return };
            let mut v = vec![];
            if s.sources.contains(&"furnace") {
                v.push("furnace");
            }
            if s.sources.contains(&"electric") {
                v.push("electric");
            }
            if s.hot_water {
                v.push("hot_water");
            }
            if s.preheat {
                v.push("preheat");
            }
            v
        };
        if own && !guard_fresh && !heating.is_empty() && self.last_backstop.lock().unwrap().is_none_or(|t| wall - t >= 60.0) {
            *self.last_backstop.lock().unwrap() = Some(wall);
            let mut done = vec![];
            for c in heating {
                let r = self.command(c, &json!(false), "auto:low-battery", 15.0).await;
                done.push(format!("MCS7 {c} off: {}", r.detail));
            }
            tracing::warn!("rixens: heat lockout and the power guard isn't answering: {}", done.join("; "));
            self.state.set("climate.rixens.lockout_off", json!({"t": wall.round(), "detail": format!("guard not answering; {}", done.join("; "))}));
        }
    }
}

struct Handler(Arc<Mcs7>);

impl CommandHandler for Handler {
    fn handle(&self, cmd: String, value: Value, client: String) -> BoxFuture<Reply> {
        let m = self.0.clone();
        Box::pin(async move { m.command(&cmd, &value, &client, 15.0).await })
    }
    /// A late heat OFF still runs: the safe direction (owner, 2026-11-23, Q-T1).
    fn ttl_exempt(&self, cmd: &str, value: &Value) -> bool {
        HEAT_ON.contains(&cmd) && value == &Value::Bool(false)
    }
}

fn load_fuel(p: &std::path::Path) -> Option<(String, f64)> {
    let db = aetherrv_core::cmdlog::open_db(p).ok()?;
    db.execute("CREATE TABLE IF NOT EXISTS counters (key TEXT PRIMARY KEY, day TEXT, value REAL)", []).ok()?;
    db.query_row("SELECT day, value FROM counters WHERE key='rixens.fuel_l'", [], |r| Ok((r.get(0)?, r.get(1)?))).ok()
}

fn save_fuel(p: &std::path::Path, day: &str, v: f64) {
    let r = aetherrv_core::cmdlog::open_db(p).and_then(|db| {
        db.execute("CREATE TABLE IF NOT EXISTS counters (key TEXT PRIMARY KEY, day TEXT, value REAL)", [])?;
        db.execute(
            "INSERT INTO counters VALUES ('rixens.fuel_l', ?1, ?2) ON CONFLICT(key) DO UPDATE SET day=excluded.day, value=excluded.value",
            rusqlite::params![day, v],
        )
    });
    if let Err(e) = r {
        tracing::error!("fuel counter write failed: {e}");
    }
}

pub const FOLLOW: &[(&str, &str)] = &[
    ("victron", "victron.battery.soc"),
    ("victron", "victron.battery.current"),
    ("batteries-ble", "batteries.ble"),
    ("batteries-ble", "batteries.ble.current"),
    ("power-guard", "power.heat_lockout"),
];

pub async fn run(cfg: Cfg, bus_socket: Option<std::path::PathBuf>) -> anyhow::Result<()> {
    let state = Arc::new(StateStore::new());
    let mut bo = aetherrv_core::bus::BusOptions::new(aetherrv_core::names::RIXENS);
    if let Some(s) = bus_socket {
        bo = bo.socket(s);
    }
    let bus = aetherrv_core::bus::Bus::start(bo, state.clone());
    for (o, k) in FOLLOW {
        bus.subscribe_state(o, k).await;
    }
    let db = Some(std::path::PathBuf::from(cfg.at("history").str("path", "/var/lib/aetherrv/history.sqlite3")));
    let gate = TxGate::from_config(&cfg).only(&[DEVICE]);
    let m = Mcs7::new(&cfg, state.clone(), gate, db);
    *m.view.lock().unwrap() = Some(bus.remote().clone());
    bus.serve(CMD_DEVICE, Arc::new(Handler(m.clone())), Some(m.log.clone())).await;
    tokio::spawn(m.clone().run_poll());
    {
        let m = m.clone();
        tokio::spawn(async move {
            loop {
                m.lockout_tick().await;
                let blocked = m.gate.blocked_count();
                m.state.update([
                    (format!("tx.enabled.{DEVICE}"), json!(m.gate.is_enabled(DEVICE))),
                    (format!("tx.blocked.{DEVICE}"), json!(blocked.get(DEVICE).copied().unwrap_or(0))),
                ]);
                tokio::time::sleep(Duration::from_secs(2)).await;
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
    bus.shutdown().await;
    Ok(())
}
