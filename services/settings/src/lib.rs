//! `aetherrv-settings`: preferences, the automatic time zone, the fridge/freezer re-arm and the config-derived UI
//! values (ports of `settings.py` `Prefs` / `AutoTimezone`, `climate.py` `ColdRearm`, and the `ui.*` keys that
//! Python's `__main__.py` set).
//!
//! * [`Prefs`]: AetherRV-only preferences in `/var/lib/aetherrv/settings.json` (`[web] settings_path`), published
//!   as `settings.*`; changed with the `pref` command on device `settings` (`{"key": …, "value": …}`).
//! * [`AutoTimezone`]: sets the system time zone from the van's GPS fix (Open-Meteo `timezone=auto`, two agreeing
//!   lookups, offline keeps the zone) through the admin helper's `set_timezone`; publishes `settings.timezone`.
//! * [`ColdRearm`]: clears `fridge_off` / `freezer_off` once that unit is back at or below its limit.
//! * `ui.bms_expected`, `ui.fridge_max_c`, `ui.freezer_max_c` from the config.

pub mod host;

use aetherrv_core::admin::AdminClient;
use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::command::{BoxFuture, CommandHandler, Reply};
use aetherrv_core::config::{Cfg, truthy};
use aetherrv_core::num::jf;
use aetherrv_core::pyfmt::{py_repr, py_str};
use aetherrv_core::state::{Merged, StateExt, StateStore, StateView};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const DEVICE: &str = "settings";

/// Python `PREFS_DEFAULT`, in order (`fridge_off` / `freezer_off` are set from a warm alert and cleared
/// automatically once cold).
pub fn prefs_default() -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("screen_off_min".into(), json!(10));
    m.insert("auto_timezone".into(), json!(true));
    m.insert("fridge_off".into(), json!(false));
    m.insert("freezer_off".into(), json!(false));
    m
}

/// Python `PREFS_LIMITS` (`screen_off_min` 0 = never).
pub const PREFS_LIMITS: &[(&str, (i64, i64))] = &[("screen_off_min", (0, 240))];

/// The text for a value of the wrong JSON type (a toggle must be a boolean, a number a number).
pub const BAD_REQUEST: &str = "bad request";

pub struct Prefs {
    pub path: PathBuf,
    pub values: Map<String, Value>,
    state: Arc<StateStore>,
}

impl Prefs {
    /// Python `Prefs(state, path)`: the defaults, overlaid with the known keys of the file, published.
    pub fn load(state: Arc<StateStore>, path: impl Into<PathBuf>) -> Prefs {
        let path = path.into();
        let mut values = prefs_default();
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(Value::Object(m)) = serde_json::from_str::<Value>(&text)
        {
            for (k, v) in m {
                if let Some(slot) = values.get_mut(&k) {
                    *slot = v;
                }
            }
        }
        let p = Prefs { path, values, state };
        p.publish();
        p
    }

    fn publish(&self) {
        self.state.update(self.values.iter().map(|(k, v)| (format!("settings.{k}"), v.clone())));
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }

    /// Python `Prefs.set`: validate, save atomically, publish. Errors are the texts the UI shows.
    pub fn set(&mut self, key: &str, value: &Value) -> Result<(), String> {
        let default = prefs_default();
        let Some(d) = default.get(key) else { return Err(format!("unknown setting {}", py_repr(&json!(key)))) };
        let value = if d.is_boolean() {
            match value {
                Value::Bool(b) => json!(b),
                _ => return Err(BAD_REQUEST.into()),
            }
        } else {
            let v = match value {
                Value::Number(n) => match n.as_i64() {
                    Some(i) => i,
                    None => n.as_f64().map(|f| f.trunc() as i64).ok_or(BAD_REQUEST)?, // int(): truncation
                },
                _ => return Err(BAD_REQUEST.into()),
            };
            let (lo, hi) = PREFS_LIMITS.iter().find(|(k, _)| *k == key).map_or((v, v), |(_, l)| *l);
            if !(lo..=hi).contains(&v) {
                return Err(format!("{key} must be {lo}–{hi}"));
            }
            json!(v)
        };
        self.values.insert(key.to_string(), value);
        if let Err(e) = self.save() {
            tracing::warn!("settings not saved: {e}");
        }
        self.publish();
        Ok(())
    }

    /// `json.dumps(values, indent=1)` to `<name>.tmp`, then renamed over the file.
    fn save(&self) -> std::io::Result<()> {
        let tmp = self.path.with_extension("tmp");
        let mut buf = Vec::new();
        let mut ser = serde_json::Serializer::with_formatter(&mut buf, serde_json::ser::PrettyFormatter::with_indent(b" "));
        serde::Serialize::serialize(&self.values, &mut ser).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, buf)?;
        std::fs::rename(&tmp, &self.path)
    }
}

/// Great-circle distance (km).
pub fn km(a: (f64, f64), b: (f64, f64)) -> f64 {
    let r = std::f64::consts::PI / 180.0;
    let h = ((b.0 - a.0) * r / 2.0).sin().powi(2) + (a.0 * r).cos() * (b.0 * r).cos() * ((b.1 - a.1) * r / 2.0).sin().powi(2);
    2.0 * 6371.0 * h.sqrt().asin()
}

/// The zone lookup: `Ok(Some(zone))`, `Ok(None)` (no valid zone), `Err` (offline, timeout, bad reply).
pub type Lookup = Arc<dyn Fn(f64, f64) -> BoxFuture<Result<Option<String>, String>> + Send + Sync>;
/// The system's current zone (`/etc/localtime` → `America/Denver`).
pub type Current = Arc<dyn Fn() -> Option<String> + Send + Sync>;

pub const OPEN_METEO: &str = "https://api.open-meteo.com/v1/forecast";

/// The Open-Meteo request (no key; `timezone=auto` returns the zone of the point).
pub fn lookup_url(lat: f64, lon: f64) -> String {
    format!("{OPEN_METEO}?latitude={lat:.3}&longitude={lon:.3}&timezone=auto&forecast_days=1&daily=temperature_2m_max")
}

/// The zone in an Open-Meteo reply: a non-empty string other than `GMT`, from a 200 reply.
pub fn parse_zone(status: u16, body: &[u8]) -> Result<Option<String>, String> {
    if status != 200 {
        return Ok(None);
    }
    let j: Value = serde_json::from_slice(body).map_err(|e| format!("ValueError: {e}"))?;
    Ok(match j.get("timezone") {
        Some(Value::String(tz)) if !tz.is_empty() && tz != "GMT" => Some(tz.clone()),
        _ => None,
    })
}

/// The real lookup (15 s total timeout). If the HTTP client can't be built (TLS setup), every lookup fails, which
/// keeps the current zone, and the rest of the service runs on.
pub fn open_meteo() -> Lookup {
    let client = match reqwest::Client::builder().timeout(Duration::from_secs(15)).build() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("auto time zone: no HTTP client ({e}); the time zone stays as it is");
            let why = e.to_string();
            return Arc::new(move |_, _| {
                let why = why.clone();
                Box::pin(async move { Err(why) })
            });
        }
    };
    Arc::new(move |lat, lon| {
        let client = client.clone();
        Box::pin(async move {
            let r = client.get(lookup_url(lat, lon)).send().await.map_err(|e| e.to_string())?;
            let status = r.status().as_u16();
            let body = r.bytes().await.map_err(|e| e.to_string())?;
            parse_zone(status, &body)
        })
    })
}

/// `os.path.realpath("/etc/localtime")` after `/zoneinfo/`.
pub fn system_zone() -> Option<String> {
    let p = std::fs::canonicalize("/etc/localtime").ok()?;
    p.to_string_lossy().split_once("/zoneinfo/").map(|(_, z)| z.to_string())
}

/// Keep the system time zone matching the van's GPS position. Looks up at most every 15 min, or sooner after a
/// move of more than 25 km; changes the zone only when the lookup returns a different, valid zone twice in a row
/// (no flapping at a border). Uses only the van's own fix (`location.fix`). Offline → keeps the current zone.
pub struct AutoTimezone {
    pub admin: AdminClient,
    pub lookup: Lookup,
    pub current: Current,
    last_pos: Option<(f64, f64)>,
    last_at: f64,
    pending: Option<String>,
}

impl AutoTimezone {
    pub fn new(admin: AdminClient, lookup: Lookup, current: Current) -> AutoTimezone {
        AutoTimezone { admin, lookup, current, last_pos: None, last_at: 0.0, pending: None }
    }

    /// One step (every 60 s). `view` gives `location.*` (and this service's own keys), `out` is published.
    pub async fn step(&mut self, view: &dyn StateView, prefs: &Mutex<Prefs>, out: &StateStore, now: f64) {
        out.set("settings.timezone", (self.current)().map_or(Value::Null, Value::String));
        let auto = prefs.lock().unwrap_or_else(|p| p.into_inner()).get("auto_timezone").is_none_or(truthy);
        if !auto || !view.truthy("location.fix") {
            return;
        }
        let (Some(lat), Some(lon)) = (view.num("location.lat"), view.num("location.lon")) else { return };
        let pos = (lat, lon);
        let moved = self.last_pos.is_none_or(|p| km(p, pos) > 25.0);
        if !moved && now - self.last_at < 15.0 * 60.0 && self.pending.is_none() {
            return;
        }
        let tz = (self.lookup)(lat, lon).await.unwrap_or_else(|e| {
            tracing::debug!("auto time zone: lookup failed ({e}); keeping the zone");
            None // offline: keep the current zone
        });
        self.last_pos = Some(pos);
        self.last_at = now;
        let Some(tz) = tz.filter(|z| Some(z.as_str()) != (self.current)().as_deref()) else {
            self.pending = None;
            return;
        };
        if self.pending.as_deref() != Some(tz.as_str()) {
            // need the same answer twice before changing
            self.pending = Some(tz);
            self.last_at = 0.0; // re-check on the next step
            return;
        }
        let res = match self.admin.call("set_timezone", json!({"tz": tz}), Duration::from_secs(60)).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("auto time zone: {e}");
                return;
            }
        };
        if res.get("ok").is_some_and(truthy) {
            tracing::info!("time zone set from GPS: {tz}");
            out.set("settings.timezone", json!(tz));
        } else {
            let why = py_str(res.get("error").unwrap_or(&Value::Null));
            tracing::warn!("auto time zone refused: {why}");
        }
        self.pending = None;
    }
}

/// Fridge/freezer "off" is only ever set by the owner (the switch on a warm alert). It is cleared automatically
/// once that unit is back at or below its limit, so warm alerts resume by themselves. Nothing here ever decides a
/// unit is off.
pub struct ColdRearm;

impl ColdRearm {
    pub fn check(view: &dyn StateView, prefs: &Mutex<Prefs>) -> Vec<&'static str> {
        let mut rearmed = Vec::new();
        for role in ["fridge", "freezer"] {
            let (t, mx) = (view.num(&format!("climate.{role}.temp")), view.num(&format!("ui.{role}_max_c")));
            if view.truthy(&format!("settings.{role}_off"))
                && let (Some(t), Some(mx)) = (t, mx)
                && t <= mx
            {
                let r = prefs.lock().unwrap_or_else(|p| p.into_inner()).set(&format!("{role}_off"), &json!(false));
                if let Err(e) = r {
                    tracing::error!("{role} re-arm: {e}");
                    continue;
                }
                rearmed.push(role);
            }
        }
        rearmed
    }
}

/// The values Python's `__main__` set from the config.
pub fn ui_keys(cfg: &Cfg) -> Vec<(String, Value)> {
    let ui = cfg.at("ui");
    vec![
        ("ui.bms_expected".into(), json!(cfg.at("alerts").i64("bms_expected_modules", 0))),
        ("ui.fridge_max_c".into(), jf(ui.f64("fridge_max_c", 5.0))),
        ("ui.freezer_max_c".into(), jf(ui.f64("freezer_max_c", -12.0))),
    ]
}

/// The `pref` command: `{"key": …, "value": …}`.
pub struct PrefHandler {
    pub prefs: Arc<Mutex<Prefs>>,
}

impl PrefHandler {
    pub fn command(&self, cmd: &str, value: &Value, client: &str) -> Reply {
        if cmd != "pref" {
            return Reply::refused(format!("unknown command {cmd}"));
        }
        let empty = Map::new();
        let args = value.as_object().unwrap_or(&empty);
        let key = py_str(args.get("key").unwrap_or(&Value::Null));
        let v = args.get("value").cloned().unwrap_or(Value::Null);
        match self.prefs.lock().unwrap_or_else(|p| p.into_inner()).set(&key, &v) {
            Ok(()) => {
                tracing::info!("setting {key} = {} from {client}", py_str(&v));
                Reply::ok("")
            }
            Err(e) => Reply::refused(e),
        }
    }
}

struct Served(Arc<PrefHandler>);

impl CommandHandler for Served {
    fn handle(&self, cmd: String, value: Value, client: String) -> BoxFuture<Reply> {
        let h = self.0.clone();
        Box::pin(async move { h.command(&cmd, &value, &client) })
    }
}

pub struct Options {
    pub bus_socket: Option<PathBuf>,
    pub prefs_path: PathBuf,
    pub admin_socket: PathBuf,
    pub tz_interval: Duration,
    pub cold_period: Duration,
    /// `None`: Open-Meteo.
    pub lookup: Option<Lookup>,
}

impl Options {
    pub fn from_config(cfg: &Cfg) -> Options {
        let web = cfg.at("web");
        Options {
            bus_socket: None,
            prefs_path: web.str("settings_path", "/var/lib/aetherrv/settings.json").into(),
            admin_socket: web.str("admin_socket", aetherrv_core::admin::SOCK).into(),
            tz_interval: Duration::from_secs(60),
            cold_period: Duration::from_secs(10),
            lookup: None,
        }
    }
}

/// Keys this service reads from other services.
pub const FOLLOW: &[&str] = &["location.fix", "location.lat", "location.lon", "climate.fridge.temp", "climate.freezer.temp"];

/// Run the service until SIGTERM/SIGINT.
pub async fn run(cfg: Cfg, opts: Options) -> anyhow::Result<()> {
    let state = Arc::new(StateStore::new());
    state.update(ui_keys(&cfg));
    if let Some(d) = opts.prefs_path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let prefs = Arc::new(Mutex::new(Prefs::load(state.clone(), &opts.prefs_path)));
    let handler = Arc::new(Served(Arc::new(PrefHandler { prefs: prefs.clone() })));
    let cabin_source = cfg.at("climate").str("cabin_source", "victron");
    let mut bo = BusOptions::new(aetherrv_core::names::SETTINGS);
    if let Some(s) = opts.bus_socket.clone() {
        bo = bo.socket(s);
    }
    let bus = Bus::start(bo, state.clone());
    // Every subscription and `serve` right after `Bus::start`, before the first CONNACK can arrive: core's
    // `subscribe_filter` loses a filter added while a connect is in progress (`on_connect` copies the filter list
    // before it sets `connected`), and this way the command topic is subscribed before our status says "up".
    bus.serve(DEVICE, handler, None).await;
    for k in FOLLOW {
        if let Some(o) = aetherrv_core::names::owner_of(k, &cabin_source, &|_| None) {
            bus.subscribe_state(&o, k).await;
        }
    }
    aetherrv_core::notify::ready();
    aetherrv_core::notify::spawn_runtime_watchdog();
    {
        // the Pi's own load (Settings → Device): published once per history interval, sampled every host::SAMPLE
        let state = state.clone();
        let every = Duration::from_secs_f64(cfg.at("history").f64("interval", 30.0).max(5.0));
        tokio::spawn(async move {
            let paths = host::Paths::discover();
            tracing::info!("host: SoC {:?}, RP1 {:?}, NVMe {:?}, GPU stats {:?}", paths.soc_temp, paths.rp1_temp, paths.nvme_temp, paths.gpu_stats);
            let mut s = host::Sampler::new(paths);
            let mut next = tokio::time::Instant::now() + every;
            loop {
                tokio::time::sleep(host::SAMPLE.min(next.saturating_duration_since(tokio::time::Instant::now()))).await;
                if tokio::time::Instant::now() >= next {
                    state.update(s.publish());
                    next += every;
                    if next <= tokio::time::Instant::now() {
                        next = tokio::time::Instant::now() + every; // fell behind (a stall): no burst of catch-up publishes
                    }
                } else {
                    s.sample();
                }
            }
        });
    }
    {
        let (state, prefs, remote) = (state.clone(), prefs.clone(), bus.remote().clone());
        let lookup = opts.lookup.clone().unwrap_or_else(open_meteo);
        let mut tz = AutoTimezone::new(AdminClient::new(&opts.admin_socket), lookup, Arc::new(system_zone));
        let every = opts.tz_interval;
        tokio::spawn(async move {
            loop {
                let view = Merged { local: &*state, remote: &*remote };
                tz.step(&view, &prefs, &state, aetherrv_core::clock::now_s()).await;
                tokio::time::sleep(every).await;
            }
        });
    }
    {
        let (state, prefs, remote) = (state.clone(), prefs.clone(), bus.remote().clone());
        let every = opts.cold_period;
        tokio::spawn(async move {
            loop {
                let view = Merged { local: &*state, remote: &*remote };
                for role in ColdRearm::check(&view, &prefs) {
                    tracing::info!("{role} is cold again: warm alerts re-armed");
                }
                tokio::time::sleep(every).await;
            }
        });
    }
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = term.recv() => {},
        _ = tokio::signal::ctrl_c() => {},
    }
    aetherrv_core::notify::stopping();
    bus.shutdown().await;
    Ok(())
}

/// For tests and tools: the file a [`Prefs`] writes.
pub fn read_prefs_file(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}
