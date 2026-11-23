//! The house bank as `lowbatt.py` sees it, and the heat lockout (owner, 2026-11-23).
//!
//! SoC comes from the Victron shunt (`victron.battery.soc`) while it's fresh, else the mean of the batteries read over
//! Bluetooth. Charging = bank current above +1 A. No SoC reading at all never triggers anything.
//!
//! Heat lockout: once the SoC has been at or below `heater_off_soc` for `hold_s` while not charging, heat is locked
//! out; it lifts when charging or the SoC is 2 % above the threshold. The tracker is used by the power guard AND
//! inside both heater services (ARCHITECTURE.md §8), and its latch can be persisted so a restart or reboot at 9 %
//! stays locked until fresh readings show recovery.

use aetherrv_core::state::{StateExt, StateView};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A SoC or current older than this is ignored.
pub const FRESH_S: f64 = 120.0;
pub const CHARGING_A: f64 = 1.0;

/// `(soc, source)`: `"shunt"` or `"batteries"`, or `(None, None)`.
pub fn bank_soc(s: &dyn StateView) -> (Option<f64>, Option<&'static str>) {
    if let (Some(soc), Some(age)) = (s.num("victron.battery.soc"), s.age("victron.battery.soc"))
        && age < FRESH_S
    {
        return (Some(soc), Some("shunt"));
    }
    let vals: Vec<f64> = s
        .list("batteries.ble")
        .iter()
        .filter(|r| aetherrv_core::config::truthy(r.get("ok").unwrap_or(&serde_json::Value::Null)))
        .filter_map(|r| r.get("soc").and_then(|v| v.as_f64()))
        .collect();
    if vals.is_empty() {
        (None, None)
    } else {
        (Some(aetherrv_core::num::round(vals.iter().sum::<f64>() / vals.len() as f64, 1)), Some("batteries"))
    }
}

pub fn charging(s: &dyn StateView) -> bool {
    if let (Some(cur), Some(age)) = (s.num("victron.battery.current"), s.age("victron.battery.current"))
        && age < FRESH_S
    {
        return cur > CHARGING_A;
    }
    s.num("batteries.ble.current").is_some_and(|c| c > CHARGING_A)
}

/// What changed on a tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    None,
    Locked,
    Lifted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Latch {
    pub lockout: bool,
    /// Unix time the latch last changed.
    pub t: f64,
}

pub struct LockoutTracker {
    pub heater_off_soc: f64,
    pub hold_s: f64,
    pub heat_low_since: Option<f64>,
    pub lockout: bool,
    path: Option<PathBuf>,
}

impl LockoutTracker {
    pub fn new(heater_off_soc: f64, hold_s: f64) -> Self {
        LockoutTracker { heater_off_soc, hold_s, heat_low_since: None, lockout: false, path: None }
    }

    /// With the latch persisted in `path` (loaded now; a missing or bad file = not locked).
    pub fn persisted(heater_off_soc: f64, hold_s: f64, path: impl AsRef<Path>) -> Self {
        let p = path.as_ref().to_path_buf();
        let lockout = std::fs::read_to_string(&p)
            .ok()
            .and_then(|t| serde_json::from_str::<Latch>(&t).ok())
            .is_some_and(|l| l.lockout);
        if lockout {
            tracing::warn!("heat lockout latched from before the restart ({})", p.display());
        }
        LockoutTracker { heater_off_soc, hold_s, heat_low_since: None, lockout, path: Some(p) }
    }

    pub fn tick(&mut self, soc: Option<f64>, chg: bool, now: f64) -> Change {
        let heat_low = soc.is_some_and(|s| s <= self.heater_off_soc) && !chg;
        let mut ch = Change::None;
        if heat_low {
            let since = *self.heat_low_since.get_or_insert(now);
            if now - since >= self.hold_s && !self.lockout {
                self.lockout = true;
                tracing::warn!("house battery at {:.1}%: heat locked out", soc.unwrap_or(0.0));
                ch = Change::Locked;
            }
        } else {
            self.heat_low_since = None;
            if self.lockout && (chg || soc.is_some_and(|s| s > self.heater_off_soc + 2.0)) {
                self.lockout = false;
                tracing::warn!("house battery recovered ({soc:?}%, charging={chg}): heat lockout lifted");
                ch = Change::Lifted;
            }
        }
        if ch != Change::None {
            self.save(now);
        }
        ch
    }

    /// Tick from a state view (bank_soc + charging).
    pub fn tick_state(&mut self, s: &dyn StateView, now: f64) -> Change {
        let (soc, _) = bank_soc(s);
        let chg = charging(s);
        self.tick(soc, chg, now)
    }

    fn save(&self, now: f64) {
        if let Some(p) = &self.path {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let tmp = p.with_extension("tmp");
            let body = serde_json::to_string(&Latch { lockout: self.lockout, t: now }).unwrap();
            if let Err(e) = std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, p)) {
                tracing::error!("heat lockout latch not saved ({}): {e}", p.display());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aetherrv_core::state::StateStore;
    use serde_json::json;

    #[test]
    fn shunt_then_bluetooth_and_charging() {
        let s = StateStore::new();
        assert_eq!(bank_soc(&s), (None, None));
        s.update([("batteries.ble", json!([{"ok": true, "soc": 7.0}, {"ok": true, "soc": 8.0}, {"ok": false, "soc": 1.0}])),
                  ("batteries.ble.current", json!(-4.0))]);
        assert_eq!(bank_soc(&s), (Some(7.5), Some("batteries")));
        assert!(!charging(&s));
        s.update([("victron.battery.soc", json!(55.0)), ("victron.battery.current", json!(3.0))]);
        assert_eq!(bank_soc(&s), (Some(55.0), Some("shunt")));
        assert!(charging(&s));
        // a stale shunt is ignored
        s.set_at("victron.battery.soc", json!(55.0), aetherrv_core::clock::now_ms() - 121_000);
        assert_eq!(bank_soc(&s).1, Some("batteries"));
    }

    #[test]
    fn lockout_holds_lifts_and_survives_a_restart() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("lockout.json");
        let mut t = LockoutTracker::persisted(10.0, 120.0, &p);
        assert_eq!(t.tick(Some(9.5), false, 0.0), Change::None);
        assert_eq!(t.tick(Some(9.5), false, 119.0), Change::None);
        assert_eq!(t.tick(Some(9.5), false, 121.0), Change::Locked);
        // restart: still locked, and an unknown SoC never lifts it
        let mut t2 = LockoutTracker::persisted(10.0, 120.0, &p);
        assert!(t2.lockout);
        assert_eq!(t2.tick(None, false, 200.0), Change::None);
        assert!(t2.lockout);
        assert_eq!(t2.tick(Some(11.0), false, 210.0), Change::None, "needs +2 % (or charging)");
        assert_eq!(t2.tick(Some(12.5), false, 220.0), Change::Lifted);
        assert!(!LockoutTracker::persisted(10.0, 120.0, &p).lockout);
    }
}
