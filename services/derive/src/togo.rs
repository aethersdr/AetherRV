//! Time to go for the Energy page and Home (owner, 2026-11-27: "it should work for both charge and discharge"):
//! `energy.battery.to_go_s` (whole minutes, in seconds) and `energy.battery.to_go_dir` (`"empty"` while discharging,
//! `"full"` while charging, null at rest or when unknown).
//!
//! The Cerbo's own TimeToGo is no use here: Venus OS computes it only while discharging, and the van's Lynx Shunt has
//! no capacity set (its SoC has drifted from the batteries'). So it is computed from the batteries themselves:
//! * **Ah left**: the sum of each battery's own remaining Ah (Bluetooth, `batteries.ble`).
//! * **Ah to full**: each battery's remaining Ah to its own 100 % SoC, `remaining × (100 / SoC − 1)`. Their SoC says
//!   "full" at about 450 Ah, not the nominal 460, so a constant would overstate it and never reach 0.
//! * A battery not reporting is filled in at the average of those that are (as the Energy headline does).
//! * **Current**: the shunt's bank current (`victron.battery.current`, else the batteries' sum) averaged over
//!   [`AVG_S`] seconds, weighted by time, as a battery monitor's time-to-go does; the average restarts when the
//!   current changes direction, so a load switching on mid-charge doesn't show an absurd time.
//! * Within ±[`DEADBAND_A`] A nothing is shown; with no battery data, the Cerbo's value while discharging only.

use serde_json::{Value, json};

/// The averaging window (s), a battery monitor's usual time-to-go period.
pub const AVG_S: f64 = 180.0;
/// Below this average current (A) the bank is at rest: no time to go.
pub const DEADBAND_A: f64 = 1.0;
/// Under this much left to charge (Ah) the bank is full: nothing to count down.
pub const FULL_AH: f64 = 0.5;

/// (Ah left, Ah to full) of the whole bank, from the `batteries.ble` rows; `None` with no battery reporting.
pub fn bank_ah(rows: &[Value]) -> Option<(f64, f64)> {
    let mut used = 0usize;
    let (mut left, mut to_full) = (0.0, 0.0);
    for r in rows {
        let ok = r.get("ok").and_then(Value::as_bool) == Some(true);
        let (Some(rem), Some(soc)) = (r.get("remaining_ah").and_then(Value::as_f64), r.get("soc").and_then(Value::as_f64)) else {
            continue;
        };
        if !ok || soc <= 0.0 {
            continue;
        }
        used += 1;
        left += rem;
        to_full += if soc >= 100.0 { 0.0 } else { rem * (100.0 / soc - 1.0) };
    }
    (used > 0).then(|| {
        let scale = rows.len().max(used) as f64 / used as f64;
        (left * scale, to_full * scale)
    })
}

/// The time-weighted average of the bank current.
#[derive(Debug, Default, Clone)]
pub struct Avg {
    avg: Option<f64>,
    last_t: f64,
}

impl Avg {
    /// Add the current at `now` (s); returns the average. A change of direction (beyond the deadband) restarts it.
    pub fn add(&mut self, amps: f64, now: f64) -> f64 {
        let next = match self.avg {
            Some(a) if (a > DEADBAND_A && amps < -DEADBAND_A) || (a < -DEADBAND_A && amps > DEADBAND_A) => amps,
            Some(a) => {
                let dt = (now - self.last_t).clamp(0.0, AVG_S);
                a + (1.0 - (-dt / AVG_S).exp()) * (amps - a)
            }
            None => amps,
        };
        self.avg = Some(next);
        self.last_t = now;
        next
    }

    pub fn clear(&mut self) {
        self.avg = None;
    }
}

/// (seconds, direction) for an average current, the bank's Ah and the Cerbo's TimeToGo (s).
pub fn to_go(avg_a: Option<f64>, ah: Option<(f64, f64)>, cerbo_ttg: Option<f64>) -> (Value, Value) {
    let none = (Value::Null, Value::Null);
    let Some(a) = avg_a else { return none };
    if a.abs() < DEADBAND_A {
        return none;
    }
    let secs = |h: f64| json!(((h * 3600.0) / 60.0).round() as i64 * 60);
    match (a > 0.0, ah) {
        (true, Some((_, to_full))) if to_full < FULL_AH => none,
        (true, Some((_, to_full))) => (secs(to_full / a), json!("full")),
        (false, Some((left, _))) => (secs(left / -a), json!("empty")),
        (false, None) => match cerbo_ttg {
            Some(t) if t > 0.0 => (json!((t / 60.0).round() as i64 * 60), json!("empty")),
            _ => none,
        },
        (true, None) => none,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(ok: bool, rem: f64, soc: f64) -> Value {
        json!({"ok": ok, "remaining_ah": rem, "soc": soc})
    }

    #[test]
    fn bank_ah_by_each_batterys_own_soc() {
        // the van at 21:30 on 11-27: 85 / 86 / 91 / 99 %
        let rows = [row(true, 382.2, 85.0), row(true, 384.5, 86.0), row(true, 409.1, 91.0), row(true, 448.0, 99.0)];
        let (left, to_full) = bank_ah(&rows).unwrap();
        assert_eq!(aetherrv_core::num::round(left, 1), 1623.8);
        // 67.4 + 62.6 + 40.5 + 4.5 Ah to each battery's own 100 %
        assert_eq!(aetherrv_core::num::round(to_full, 1), 175.0);
        // one battery not reporting: filled in at the others' average
        let rows3 = [row(true, 400.0, 88.0), row(false, 0.0, 0.0), row(true, 420.0, 92.0)];
        let (l3, f3) = bank_ah(&rows3).unwrap();
        assert_eq!((aetherrv_core::num::round(l3, 1), aetherrv_core::num::round(f3, 1)), (1230.0, 136.6));
        assert!(bank_ah(&[row(false, 1.0, 50.0)]).is_none() && bank_ah(&[]).is_none());
        assert_eq!(bank_ah(&[row(true, 451.0, 100.0)]).unwrap().1, 0.0);
    }

    #[test]
    fn charge_discharge_rest_and_full() {
        let ah = Some((1623.8, 175.0));
        // charging at 65 A: 175 Ah to full = 2 h 42 m (2.69 h)
        assert_eq!(to_go(Some(65.0), ah, None), (json!(9720), json!("full")));
        // discharging at 20 A: 1623.8 Ah = 81.2 h
        assert_eq!(to_go(Some(-20.0), ah, None), (json!(292_260), json!("empty")));
        // at rest, and full
        assert_eq!(to_go(Some(0.4), ah, None), (Value::Null, Value::Null));
        assert_eq!(to_go(Some(30.0), Some((1800.0, 0.2)), None), (Value::Null, Value::Null));
        // no battery data: the Cerbo's figure while discharging only, never a guess while charging
        assert_eq!(to_go(Some(-20.0), None, Some(86_420.0)), (json!(86_400), json!("empty")));
        assert_eq!(to_go(Some(20.0), None, Some(86_420.0)), (Value::Null, Value::Null));
        assert_eq!(to_go(None, ah, None), (Value::Null, Value::Null));
    }

    #[test]
    fn the_average_is_time_weighted_and_restarts_on_a_change_of_direction() {
        let mut a = Avg::default();
        assert_eq!(a.add(60.0, 0.0), 60.0, "seeded by the first reading");
        // a 1 s spike to 120 A barely moves a 3-minute average
        let s = a.add(120.0, 1.0);
        assert!(s > 60.0 && s < 61.0, "{s}");
        // a long steady change converges
        let mut v = s;
        for i in 2..1200 {
            v = a.add(30.0, i as f64);
        }
        assert!((v - 30.0).abs() < 0.1, "{v}");
        // the charger stops and a load starts: the average restarts at once, no "to full" from a stale charge
        assert_eq!(a.add(-15.0, 1200.0), -15.0);
        // within the deadband it averages normally (no restart)
        let w = a.add(0.5, 1201.0);
        assert!(w < -14.0, "{w}");
    }
}
