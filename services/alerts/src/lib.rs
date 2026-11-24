//! `aetherrv-alerts`: the alert engine and its log (port of `src/aetherrv/alerts.py`).
//!
//! Evaluates the rules against every other service's state (the bus's remote view), debounces them, and keeps an
//! event log in `alert_log` (the shared SQLite file, `[history] path`). Severity follows the status palette:
//! warning < serious < critical. A rule must hold for its `for_s` before it raises, and must be false for `clear_s`
//! before it clears. Acknowledging only silences an alert; it stays active until its condition clears.
//!
//! Publishes `alerts.active` (every 1 s) and `alerts.log` (every 5 s); serves `ack` / `ack_all` on device
//! `alerts`. While the bus is down (and for a moment after it reconnects) evaluation pauses, so a broker restart
//! neither clears every alert nor raises a "module down" for every service (ARCHITECTURE.md §11).

pub mod py;
pub mod rules;

use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::command::{BoxFuture, CommandHandler, Reply};
use aetherrv_core::config::Cfg;
use aetherrv_core::num::{jf, round};
use aetherrv_core::state::{StateStore, StateView};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use rules::{default_rules, default_rules_base, service_rules};

/// The device name commands are served on.
pub const DEVICE: &str = "alerts";

pub const ALERT_LOG_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS alert_log (t REAL, event TEXT, id TEXT, severity TEXT, title TEXT, detail TEXT)";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Warning = 1,
    Serious = 2,
    Critical = 3,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Warning => "warning",
            Severity::Serious => "serious",
            Severity::Critical => "critical",
        }
    }
    pub fn parse(s: &str) -> Option<Severity> {
        Some(match s {
            "warning" => Severity::Warning,
            "serious" => Severity::Serious,
            "critical" => Severity::Critical,
            _ => return None,
        })
    }
    pub fn rank(&self) -> i32 {
        *self as i32
    }
}

/// One condition a rule currently sees as true.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub id: String,
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    pub source: String,
    /// An ack survives a restart while the condition lasts (stored fault memory).
    pub keep_ack: bool,
}

/// A rule that would have raised in Python (the text of the exception).
#[derive(Debug, Clone, PartialEq)]
pub struct RuleError(pub String);

impl std::fmt::Display for RuleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub type RuleResult = Result<Vec<Finding>, RuleError>;
pub type Check = fn(&dyn StateView, &Ctx) -> RuleResult;

/// Alert thresholds (`[alerts]`, Python `DEFAULTS`).
#[derive(Debug, Clone, PartialEq)]
pub struct AlertCfg {
    pub soc_warning: f64,
    pub soc_critical: f64,
    pub lfp_charge_min_c: f64,
    pub batt_cold_c: f64,
    pub data_warning_pct: f64,
    pub data_serious_pct: f64,
    pub starlink_offline_s: f64,
    pub bms_expected_modules: f64,
}

impl Default for AlertCfg {
    fn default() -> Self {
        AlertCfg {
            soc_warning: 20.0,
            soc_critical: 10.0,
            lfp_charge_min_c: 2.0,
            batt_cold_c: 0.0,
            data_warning_pct: 80.0,
            data_serious_pct: 95.0,
            starlink_offline_s: 180.0,
            bms_expected_modules: 0.0,
        }
    }
}

impl AlertCfg {
    /// `{**DEFAULTS, **cfg["alerts"]}`.
    pub fn from_cfg(a: &Cfg) -> AlertCfg {
        let d = AlertCfg::default();
        AlertCfg {
            soc_warning: a.f64("soc_warning", d.soc_warning),
            soc_critical: a.f64("soc_critical", d.soc_critical),
            lfp_charge_min_c: a.f64("lfp_charge_min_c", d.lfp_charge_min_c),
            batt_cold_c: a.f64("batt_cold_c", d.batt_cold_c),
            data_warning_pct: a.f64("data_warning_pct", d.data_warning_pct),
            data_serious_pct: a.f64("data_serious_pct", d.data_serious_pct),
            starlink_offline_s: a.f64("starlink_offline_s", d.starlink_offline_s),
            bms_expected_modules: a.f64("bms_expected_modules", d.bms_expected_modules),
        }
    }
}

/// A service's health as the bus sees it (`RemoteState::owners()`).
#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub name: String,
    /// Status up and heartbeat younger than 6 s.
    pub ok: bool,
    /// Seconds since its last heartbeat (`None`: never had one).
    pub age_s: Option<f64>,
}

/// What a rule sees besides the state: the thresholds, the time, and the modules' health.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub cfg: AlertCfg,
    /// Unix seconds (Python's `time.time()` in `r_starlink`).
    pub now: f64,
    pub modules: Vec<Module>,
}

impl Default for Ctx {
    fn default() -> Self {
        Ctx { cfg: AlertCfg::default(), now: aetherrv_core::clock::now_s(), modules: Vec::new() }
    }
}

impl Ctx {
    pub fn with_cfg(cfg: AlertCfg) -> Ctx {
        Ctx { cfg, ..Ctx::default() }
    }
}

pub struct Rule {
    pub name: String,
    pub check: Check,
    pub for_s: f64,
    pub clear_s: f64,
    /// Findings waiting out `for_s` (id, first seen), in order.
    pending: Vec<(String, f64)>,
}

impl Rule {
    pub fn new(name: &str, check: Check, for_s: f64, clear_s: f64) -> Rule {
        Rule { name: name.to_string(), check, for_s, clear_s, pending: Vec::new() }
    }
}

#[derive(Debug, Clone)]
pub struct Active {
    pub finding: Finding,
    pub since: f64,
    pub rule: String,
    pub acked: bool,
    pub false_since: Option<f64>,
}

pub struct Alerts {
    pub state: Arc<dyn StateView>,
    pub cfg: AlertCfg,
    pub rules: Vec<Rule>,
    /// Insertion-ordered (Python dict): ties in the snapshot sort keep this order.
    pub active: Vec<(String, Active)>,
    /// The modules' health, refreshed by the service before each evaluation.
    pub modules: Vec<Module>,
    db: Option<Connection>,
}

fn log_event(db: &Option<Connection>, event: &str, f: &Finding, now: f64) {
    if event == "raised" {
        tracing::warn!("alert {event}: [{}] {}", f.severity.as_str(), f.title);
    } else {
        tracing::info!("alert {event}: [{}] {}", f.severity.as_str(), f.title);
    }
    if let Some(db) = db
        && let Err(e) = db.execute(
            "INSERT INTO alert_log VALUES (?1,?2,?3,?4,?5,?6)",
            rusqlite::params![now, event, f.id, f.severity.as_str(), f.title, f.detail],
        )
    {
        tracing::error!("alert log write failed: {e}");
    }
}

/// True when the last logged event for this id is an ack (or a raise after an ack with no clear between): the
/// condition was acknowledged and never seen to clear, e.g. across a restart.
fn was_acked(db: &Option<Connection>, fid: &str) -> bool {
    let Some(db) = db else { return false };
    let r = db.query_row(
        "SELECT event FROM alert_log WHERE id = ?1 AND event IN ('acked', 'cleared') ORDER BY t DESC LIMIT 1",
        [fid],
        |r| r.get::<_, String>(0),
    );
    matches!(r, Ok(e) if e == "acked")
}

impl Alerts {
    /// Python `Alerts(state, db, cfg, rules)`; `rules = None` → [`default_rules`].
    pub fn new(state: Arc<dyn StateView>, db: Option<Connection>, cfg: AlertCfg, rules: Option<Vec<Rule>>) -> Alerts {
        if let Some(d) = &db
            && let Err(e) = d.execute(ALERT_LOG_SCHEMA, [])
        {
            tracing::error!("alert log table: {e}");
        }
        let rules = rules.unwrap_or_else(|| default_rules(&cfg));
        Alerts { state, cfg, rules, active: Vec::new(), modules: Vec::new(), db }
    }

    pub fn get(&self, id: &str) -> Option<&Active> {
        self.active.iter().find(|(k, _)| k == id).map(|(_, a)| a)
    }

    pub fn evaluate(&mut self, now: f64) {
        let ctx = Ctx { cfg: self.cfg.clone(), now, modules: self.modules.clone() };
        let Alerts { state, rules, active, db, .. } = self;
        for rule in rules.iter_mut() {
            let check = rule.check;
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(&**state, &ctx)));
            let list = match r {
                Ok(Ok(l)) => l,
                Ok(Err(e)) => {
                    // a broken rule must not take down the others
                    tracing::error!("alert rule {} failed: {e}", rule.name);
                    continue;
                }
                Err(_) => {
                    tracing::error!("alert rule {} failed: panicked", rule.name);
                    continue;
                }
            };
            // {f.id: f for f in ...}: first position, last value
            let mut findings: Vec<Finding> = Vec::new();
            for f in list {
                match findings.iter_mut().find(|x| x.id == f.id) {
                    Some(x) => *x = f,
                    None => findings.push(f),
                }
            }
            for f in &findings {
                if let Some((_, a)) = active.iter_mut().find(|(k, _)| *k == f.id) {
                    let escalated = f.severity.rank() > a.finding.severity.rank();
                    a.finding = f.clone();
                    a.false_since = None;
                    if escalated {
                        // e.g. warning -> critical needs fresh attention
                        a.acked = false;
                        log_event(db, "escalated", f, now);
                    }
                    continue;
                }
                let first = match rule.pending.iter().find(|(k, _)| *k == f.id) {
                    Some((_, t)) => *t,
                    None => {
                        rule.pending.push((f.id.clone(), now));
                        now
                    }
                };
                if now - first >= rule.for_s {
                    let acked = f.keep_ack && was_acked(db, &f.id);
                    active.push((f.id.clone(), Active { finding: f.clone(), since: now, rule: rule.name.clone(), acked, false_since: None }));
                    rule.pending.retain(|(k, _)| *k != f.id);
                    log_event(db, "raised", f, now);
                }
            }
            // conditions that went away before they were due
            rule.pending.retain(|(k, _)| findings.iter().any(|f| f.id == *k));
            let mut cleared = Vec::new();
            for (aid, a) in active.iter_mut() {
                if a.rule != rule.name || findings.iter().any(|f| f.id == *aid) {
                    continue;
                }
                // Python `a.false_since or now`
                let fs = match a.false_since {
                    Some(t) if t != 0.0 => t,
                    _ => now,
                };
                a.false_since = Some(fs);
                if now - fs >= rule.clear_s {
                    cleared.push(aid.clone());
                }
            }
            for aid in cleared {
                if let Some(i) = active.iter().position(|(k, _)| *k == aid) {
                    let (_, a) = active.remove(i);
                    log_event(db, "cleared", &a.finding, now);
                }
            }
        }
    }

    pub fn ack(&mut self, aid: &str, now: f64) -> bool {
        let Some((_, a)) = self.active.iter_mut().find(|(k, _)| k == aid) else { return false };
        if a.acked {
            return false;
        }
        a.acked = true;
        let f = a.finding.clone();
        log_event(&self.db, "acked", &f, now);
        true
    }

    pub fn ack_all(&mut self, now: f64) -> usize {
        let ids: Vec<String> = self.active.iter().map(|(k, _)| k.clone()).collect();
        ids.iter().filter(|id| self.ack(id, now)).count()
    }

    /// `alerts.active`: most severe first, then oldest first.
    pub fn snapshot(&self) -> Value {
        let mut rows: Vec<(Severity, f64, Value)> = self
            .active
            .iter()
            .map(|(_, a)| {
                let since = round(a.since, 1);
                let f = &a.finding;
                (
                    f.severity,
                    since,
                    json!({"id": f.id, "severity": f.severity.as_str(), "title": f.title, "detail": f.detail,
                           "source": f.source, "since": jf(since), "acked": a.acked}),
                )
            })
            .collect();
        rows.sort_by(|x, y| y.0.rank().cmp(&x.0.rank()).then(x.1.partial_cmp(&y.1).unwrap_or(std::cmp::Ordering::Equal)));
        Value::Array(rows.into_iter().map(|r| r.2).collect())
    }

    /// `alerts.log`: the newest `limit` events.
    pub fn history(&self, limit: i64) -> Value {
        let Some(db) = &self.db else { return json!([]) };
        let rows = db
            .prepare("SELECT t, event, id, severity, title FROM alert_log ORDER BY t DESC LIMIT ?1")
            .and_then(|mut st| {
                st.query_map([limit], |r| {
                    let t: f64 = r.get(0)?;
                    Ok(json!({"t": jf(round(t, 1)), "event": r.get::<_, Option<String>>(1)?, "id": r.get::<_, Option<String>>(2)?,
                              "severity": r.get::<_, Option<String>>(3)?, "title": r.get::<_, Option<String>>(4)?}))
                })?
                .collect::<rusqlite::Result<Vec<Value>>>()
            });
        match rows {
            Ok(v) => Value::Array(v),
            Err(e) => {
                tracing::error!("alert log read failed: {e}");
                json!([])
            }
        }
    }
}

/// The modules' health from the bus.
pub fn modules_of(bus: &Bus) -> Vec<Module> {
    bus.remote().owners().into_iter().map(|(name, ok, age_s)| Module { name, ok, age_s }).collect()
}

/// Open `alert_log` in the shared file; if the file is unusable, keep the log in memory (as Python did through the
/// history module's in-memory fallback), so acks and `alerts.log` still work for this process.
pub fn open_db(path: Option<&std::path::Path>) -> Option<Connection> {
    let path = path?;
    match aetherrv_core::cmdlog::open_db(path).and_then(|d| d.execute(ALERT_LOG_SCHEMA, []).map(|_| d)) {
        Ok(d) => Some(d),
        Err(e) => {
            tracing::error!("alert log {} unusable ({e}); keeping the alert log in memory", path.display());
            Connection::open_in_memory().ok()
        }
    }
}

/// The `ack` / `ack_all` commands. Acknowledging only changes AetherRV's own alert state, never a device.
pub struct Handler {
    pub engine: Arc<Mutex<Alerts>>,
    pub state: Arc<StateStore>,
}

/// The id in an `ack` request: the value itself (a string) or `{"id": ...}`.
pub fn ack_id(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Object(m) => aetherrv_core::pyfmt::py_str(m.get("id").unwrap_or(&json!(""))),
        other => aetherrv_core::pyfmt::py_str(other),
    }
}

impl Handler {
    pub fn command(&self, cmd: &str, value: &Value, client: &str) -> Reply {
        let now = aetherrv_core::clock::now_s();
        let mut e = self.engine.lock().unwrap_or_else(|p| p.into_inner());
        let reply = match cmd {
            "ack" => {
                let id = ack_id(value);
                match e.get(&id).map(|a| a.acked) {
                    None => Reply::refused(format!("no active alert {id}")),
                    Some(true) => Reply::ok("already acknowledged"),
                    Some(false) => {
                        e.ack(&id, now);
                        tracing::info!("alert {id} acknowledged by {client}");
                        Reply::ok("acknowledged")
                    }
                }
            }
            "ack_all" => {
                let n = e.ack_all(now);
                tracing::info!("{n} alert(s) acknowledged by {client}");
                Reply::ok(format!("{n} acknowledged"))
            }
            other => return Reply::refused(format!("unknown command {other}")),
        };
        self.state.set("alerts.active", e.snapshot());
        reply
    }
}

struct Served(Arc<Handler>);

impl CommandHandler for Served {
    fn handle(&self, cmd: String, value: Value, client: String) -> BoxFuture<Reply> {
        let h = self.0.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || h.command(&cmd, &value, &client))
                .await
                .unwrap_or_else(|_| Reply::unverified("the alert engine failed"))
        })
    }

    /// An ack never touches a device; a late one is still wanted (Python had no TTL here).
    fn ttl_exempt(&self, cmd: &str, _value: &Value) -> bool {
        cmd == "ack" || cmd == "ack_all"
    }
}

pub struct Options {
    pub bus_socket: Option<PathBuf>,
    pub db_path: Option<PathBuf>,
    /// After a (re)connect, wait this long for the other services and their retained state before evaluating again.
    pub settle: Duration,
}

impl Options {
    pub fn from_config(cfg: &Cfg) -> Options {
        Options {
            bus_socket: None,
            db_path: Some(cfg.at("history").str("path", "/var/lib/aetherrv/history.sqlite3").into()),
            // longer than the bus client's reconnect backoff (2 s), so every service is back before we judge
            settle: Duration::from_secs(5),
        }
    }
}

/// Run the service until SIGTERM/SIGINT.
pub async fn run(cfg: Cfg, opts: Options) -> anyhow::Result<()> {
    let state = Arc::new(StateStore::new());
    let acfg = AlertCfg::from_cfg(&cfg.at("alerts"));
    let path = opts.db_path.clone();
    let db = tokio::task::spawn_blocking(move || open_db(path.as_deref())).await?;
    // the engine reads the bus's view once the bus exists (below)
    let engine = Arc::new(Mutex::new(Alerts::new(Arc::new(StateStore::new()), db, acfg.clone(), Some(service_rules(&acfg)))));
    let handler = Arc::new(Served(Arc::new(Handler { engine: engine.clone(), state: state.clone() })));
    let mut bo = BusOptions::new(aetherrv_core::names::ALERTS);
    if let Some(s) = opts.bus_socket.clone() {
        bo = bo.socket(s);
    }
    let bus = Bus::start(bo, state.clone());
    // Every subscription and `serve` right after `Bus::start`, before the first CONNACK can arrive: core's
    // `subscribe_filter` loses a filter added while a connect is in progress (`on_connect` copies the filter list
    // before it sets `connected`), and this way the command topic is subscribed before our status says "up".
    bus.subscribe_all_state().await;
    bus.serve(DEVICE, handler, None).await;
    // last known values while an owner is down: its outage is the "module down" alert, not a churn of its alerts
    let view: Arc<dyn StateView> = Arc::new(aetherrv_core::bus::RawView(bus.remote().clone()));
    engine.lock().unwrap().state = view;
    aetherrv_core::notify::ready();
    aetherrv_core::notify::spawn_runtime_watchdog();
    tracing::info!("alerts: {} rules", engine.lock().unwrap().rules.len());

    let task = {
        let (bus, state, engine) = (bus.clone(), state.clone(), engine.clone());
        tokio::spawn(async move {
            let mut tick: u64 = 0;
            let mut seen_connects = 0u64;
            let mut settled_at: Option<tokio::time::Instant> = None;
            let mut iv = tokio::time::interval(Duration::from_secs(1));
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                iv.tick().await;
                let n = bus.connects();
                if n != seen_connects {
                    seen_connects = n;
                    settled_at = Some(tokio::time::Instant::now() + opts.settle);
                }
                // pause while the bus is down and until the retained state is back after a (re)connect
                let live = bus.connected() && settled_at.is_some_and(|t| tokio::time::Instant::now() >= t);
                let modules = modules_of(&bus);
                let with_log = tick.is_multiple_of(5);
                let e = engine.clone();
                let r = tokio::task::spawn_blocking(move || {
                    let mut e = e.lock().unwrap_or_else(|p| p.into_inner());
                    if live {
                        e.modules = modules;
                        e.evaluate(aetherrv_core::clock::now_s());
                    }
                    (e.snapshot(), with_log.then(|| e.history(40)))
                })
                .await;
                if let Ok((snap, log)) = r {
                    state.set("alerts.active", snap);
                    if let Some(l) = log {
                        state.set("alerts.log", l);
                    }
                }
                tick += 1;
            }
        })
    };

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = term.recv() => {},
        _ = tokio::signal::ctrl_c() => {},
        r = task => tracing::error!("alerts: the evaluation loop ended ({r:?})"),
    }
    aetherrv_core::notify::stopping();
    bus.shutdown().await;
    Ok(())
}
