//! `aetherrv-power-guard`: clean shutdown before the house battery runs out, and the heat lockout (owner,
//! 2026-11-23). A port of `lowbatt.py` (ARCHITECTURE.md §8).
//!
//! * Heat lockout at or below `heater_off_soc` for `hold_s` while not charging (`aetherrv_power::LockoutTracker`,
//!   latch persisted): publishes `power.heat_lockout`; a heater found heating is turned off through the heaters' own
//!   commands (`client = auto:low-battery`), at most once a minute; `power.heaters_off` lists what was done. The
//!   heater services enforce the lockout themselves too and only act on their own when this guard is gone.
//! * Power-off at or below `shutdown_soc` for `hold_s` while not charging: the heaters first (OFF, then wait up to
//!   `heater_wait_s` until both are idle, after-runs done), then the admin helper's `poweroff`. A heater whose
//!   service is down counts as busy while waiting (its state is unknown), so the wait runs its full length.
//! * No SoC reading at all never triggers anything.

use aetherrv_core::command::{BoxFuture, Reply};
use aetherrv_core::config::Cfg;
use aetherrv_core::state::{StateExt, StateStore, StateView};
use aetherrv_power::{LockoutTracker, bank_soc, charging};
use serde_json::{Value, json};
use std::sync::Arc;

/// How the guard reaches the heaters and the root helper (the bus and the admin client; fakes in tests).
pub trait Controls: Send + Sync {
    /// A heater command (`device` "espar" or "rixens").
    fn command(&self, device: &str, cmd: &str, value: Value, client: &str) -> BoxFuture<Option<Reply>>;
    /// The admin helper's `poweroff`.
    fn poweroff(&self) -> BoxFuture<Result<Value, String>>;
    /// Is this heater's service up (its state can be trusted)?
    fn heater_up(&self, device: &str) -> bool;
    /// Is this heater configured at all?
    fn has(&self, device: &str) -> bool;
}

pub struct Guard {
    pub tracker: LockoutTracker,
    pub heater_wait_s: f64,
    pub last_off: Option<f64>,
    pub off_wait_since: Option<f64>,
    pub warn_soc: f64,
    pub shutdown_soc: f64,
    pub hold_s: f64,
    pub low_since: Option<f64>,
    pub done: bool,
}

impl Guard {
    pub fn new(c: &Cfg, latch: Option<&std::path::Path>) -> Self {
        let heater_off = c.f64("heater_off_soc", 10.0);
        let hold = c.f64("hold_s", 120.0);
        Guard {
            tracker: match latch {
                Some(p) => LockoutTracker::persisted(heater_off, hold, p),
                None => LockoutTracker::new(heater_off, hold),
            },
            heater_wait_s: c.f64("heater_wait_s", 600.0),
            last_off: None,
            off_wait_since: None,
            warn_soc: c.f64("warn_soc", 8.0),
            shutdown_soc: c.f64("shutdown_soc", 5.0),
            hold_s: hold,
            low_since: None,
            done: false,
        }
    }

    /// `{"espar": heating, "mcs7": heating}` (Python `heating()`).
    pub fn heating(s: &dyn StateView) -> (bool, bool) {
        let espar = (s.truthy("climate.espar.ctl.active") && s.string("climate.espar.ctl.mode").as_deref() == Some("heat"))
            || matches!(s.string("climate.espar.state").as_deref(), Some("heating") | Some("starting"));
        let mcs7 = s.string("climate.rixens.state").as_deref() == Some("heating")
            || s.string("climate.rixens.electric").as_deref() == Some("active");
        (espar, mcs7)
    }

    /// A heater is burning or still in its after-run (power must stay on). A configured heater whose service is
    /// down counts as busy: its state is unknown.
    pub fn busy(s: &dyn StateView, ctl: &dyn Controls) -> bool {
        let (e, m) = Self::heating(s);
        if e || m {
            return true;
        }
        if s.string("climate.espar.state").as_deref() == Some("cooldown") {
            return true;
        }
        if s.num("climate.rixens.heater_state_code").is_some_and(|c| [10.0, 12.0, 13.0, 15.0, 20.0, 30.0, 31.0].contains(&c)) {
            return true;
        }
        (ctl.has("espar") && !ctl.heater_up("espar")) || (ctl.has("rixens") && !ctl.heater_up("rixens"))
    }

    /// Espar OFF, then the MCS7's heat sources and modes that are on. Returns what was asked and the result.
    pub async fn heaters_off(s: &dyn StateView, ctl: &dyn Controls, soc: f64) -> Vec<String> {
        let mut done = vec![];
        if ctl.has("espar")
            && s.truthy("climate.espar.ctl.active")
            && !matches!(s.string("climate.espar.ctl.mode").as_deref(), None | Some("off"))
            && let Some(r) = ctl.command("espar", "set", json!({"mode": "off"}), "auto:low-battery").await
        {
            done.push(format!("Espar off: {}", r.detail));
        }
        if ctl.has("rixens") && s.truthy("climate.rixens.live") {
            let sources: Vec<String> = s.list("climate.rixens.sources").iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
            for (cmd, on) in [
                ("furnace", sources.iter().any(|x| x == "furnace")),
                ("electric", sources.iter().any(|x| x == "electric")),
                ("hot_water", s.truthy("climate.rixens.hot_water")),
                ("preheat", s.truthy("climate.rixens.preheat")),
            ] {
                if on && let Some(r) = ctl.command("rixens", cmd, json!(false), "auto:low-battery").await {
                    done.push(format!("MCS7 {cmd} off: {}", r.detail));
                }
            }
        }
        let what = if done.is_empty() { "nothing to switch off".to_string() } else { done.join("; ") };
        tracing::warn!("house battery at {soc:.0}% and a heater was heating: {what}");
        done
    }

    /// One poll (every 10 s). `state` receives the `power.*` keys.
    pub async fn tick(&mut self, s: &dyn StateView, out: &StateStore, ctl: &dyn Controls, now: f64) {
        let (soc, src) = bank_soc(s);
        let chg = charging(s);
        let before = self.tracker.lockout;
        self.tracker.tick(soc, chg, now);
        if before && !self.tracker.lockout {
            out.set("power.heaters_off", Value::Null);
        }
        let lockout = self.tracker.lockout;
        out.set("power.heat_lockout", json!(lockout));
        let (e, m) = Self::heating(s);
        if lockout && (e || m) && self.last_off.is_none_or(|t| now - t >= 60.0) {
            self.last_off = Some(now);
            let done = Self::heaters_off(s, ctl, soc.unwrap_or(0.0)).await;
            out.set("power.heaters_off", json!({"t": now.round() as i64, "soc": soc, "done": done}));
        }
        let low = soc.is_some_and(|x| x <= self.shutdown_soc) && !chg;
        if low {
            self.low_since.get_or_insert(now);
        } else {
            self.low_since = None;
        }
        let left = self.low_since.map(|t| (self.hold_s - (now - t)).max(0.0));
        let warning = soc.is_some_and(|x| x <= self.warn_soc) && !chg;
        let mut sd = json!({
            "soc": soc, "source": src, "charging": chg, "warn_soc": self.warn_soc, "shutdown_soc": self.shutdown_soc,
            "warning": warning, "in_s": left.map(|l| aetherrv_core::num::round_int(l)),
        });
        out.set("power.shutdown", sd.clone());
        if low && left == Some(0.0) && !self.done {
            // the heaters first: off, then wait until both are idle (after-runs done) before the Pi goes
            if Self::busy(s, ctl) {
                if self.off_wait_since.is_none() {
                    self.off_wait_since = Some(now);
                    Self::heaters_off(s, ctl, soc.unwrap_or(0.0)).await;
                }
                if now - self.off_wait_since.unwrap() < self.heater_wait_s {
                    sd["waiting_for_heaters"] = json!(true);
                    out.set("power.shutdown", sd);
                    return;
                }
                tracing::error!("heaters still busy after {:.0} s; powering off anyway", self.heater_wait_s);
            }
            self.done = true;
            tracing::warn!(
                "house battery at {:.1}% ({}) for {:.0} s and not charging: powering off cleanly",
                soc.unwrap_or(0.0),
                src.unwrap_or("?"),
                self.hold_s
            );
            match ctl.poweroff().await {
                Ok(r) => tracing::warn!("poweroff requested: {r}"),
                Err(e) => {
                    tracing::error!("poweroff failed: {e}");
                    self.done = false; // try again on a later tick
                }
            }
        }
    }
}

/// The bus + admin helper as the guard's controls.
pub struct BusControls {
    pub bus: aetherrv_core::bus::Bus,
    pub admin: aetherrv_core::admin::AdminClient,
    pub espar: bool,
    pub rixens: bool,
}

impl Controls for BusControls {
    fn command(&self, device: &str, cmd: &str, value: Value, client: &str) -> BoxFuture<Option<Reply>> {
        let (bus, device, cmd, client) = (self.bus.clone(), device.to_string(), cmd.to_string(), client.to_string());
        let timeout = std::time::Duration::from_secs(if device == "espar" { 15 } else { 25 });
        Box::pin(async move { Some(bus.request(&device, &cmd, value, &client, 3000, timeout).await) })
    }
    fn poweroff(&self) -> BoxFuture<Result<Value, String>> {
        let admin = self.admin.clone();
        Box::pin(async move { admin.call("poweroff", json!({}), std::time::Duration::from_secs(60)).await.map_err(|e| e.to_string()) })
    }
    fn heater_up(&self, device: &str) -> bool {
        self.bus.remote().owner_ok(device)
    }
    fn has(&self, device: &str) -> bool {
        match device {
            "espar" => self.espar,
            "rixens" => self.rixens,
            _ => false,
        }
    }
}

pub const FOLLOW: &[(&str, &str)] = &[
    ("victron", "victron.battery.soc"),
    ("victron", "victron.battery.current"),
    ("batteries-ble", "batteries.ble"),
    ("batteries-ble", "batteries.ble.current"),
    // a key is one topic level, so a service's keys are followed as a whole
    ("espar", "#"),
    ("rixens", "#"),
];

pub async fn run(cfg: Cfg, bus_socket: Option<std::path::PathBuf>) -> anyhow::Result<()> {
    let pcfg = cfg.at("power");
    let state = Arc::new(StateStore::new());
    let mut bo = aetherrv_core::bus::BusOptions::new(aetherrv_core::names::POWER_GUARD);
    if let Some(s) = bus_socket {
        bo = bo.socket(s);
    }
    let bus = aetherrv_core::bus::Bus::start(bo, state.clone());
    for (o, k) in FOLLOW {
        bus.subscribe_state(o, k).await;
    }
    let dev = cfg.at("device");
    let enabled = |kind: &str| dev.entries().iter().any(|(_, d)| d.str("kind", "") == kind && (kind == "espar" || d.bool("enabled", false)));
    let ctl = BusControls {
        bus: bus.clone(),
        admin: aetherrv_core::admin::AdminClient::new(cfg.at("web").str("admin_socket", aetherrv_core::admin::SOCK)),
        espar: enabled("espar"),
        rixens: enabled("rixens_http"),
    };
    let latch = std::path::PathBuf::from(pcfg.str("lockout_state", "/var/lib/aetherrv/power-guard/lockout.json"));
    let mut g = Guard::new(&pcfg, Some(&latch));
    let auto = pcfg.bool("auto_shutdown", false);
    aetherrv_core::notify::ready();
    aetherrv_core::notify::spawn_runtime_watchdog();
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        if auto {
            g.tick(&**bus.remote(), &state, &ctl, aetherrv_core::clock::now_s()).await;
        }
        tokio::select! {
            _ = term.recv() => break,
            _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {}
        }
    }
    bus.shutdown().await;
    Ok(())
}
