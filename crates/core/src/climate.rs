//! The cabin temperature from up to three sensors (owner, 2026-11-27): the front Ruuvi (`climate.cabin_front.*`)
//! and the rear Ruuvi (`climate.cabin_rear.*`, published only when `[device.victron] cabin_rear_temp` names one),
//! both via the Cerbo, and the MCS7's own sensor at the back by the bed (`climate.rixens.cabin_*`).
//!
//! The cabin value is the mean of the sensors that are fresh; a missing or stale one is left out. One function
//! for every reader, so they agree on which sensors count. `aetherrv-derive` publishes it as `climate.cabin.*` with
//! `[climate] cabin_source = "average"`.

use crate::num::round;
use crate::state::{StateExt, StateView};

/// s. The Cerbo republishes a Ruuvi only when the value changes, so allow quiet spells.
pub const RUUVI_STALE: f64 = 15.0 * 60.0;
/// s since the last successful MCS7 poll (`climate.rixens.updated`).
pub const MCS7_STALE: f64 = 60.0;

/// Every key the cabin mean reads (from the Victron and Rixens services).
pub const INPUTS: [&str; 9] = [
    "climate.cabin_front.temp",
    "climate.cabin_front.humidity",
    "climate.cabin_rear.temp",
    "climate.cabin_rear.humidity",
    "victron.connected",
    "climate.rixens.cabin_c",
    "climate.rixens.cabin_rh",
    "climate.rixens.live",
    "climate.rixens.updated",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sensor {
    /// The front Ruuvi.
    Front,
    /// The rear Ruuvi.
    Rear,
    /// The MCS7's sensor at the back by the bed.
    Mcs7,
}

impl Sensor {
    /// Its name in `climate.cabin.source`. The MCS7 sensor was "rear" before the rear Ruuvi existed and keeps it.
    pub fn source(self) -> &'static str {
        match self {
            Sensor::Front => "front",
            Sensor::Rear => "rear_ruuvi",
            Sensor::Mcs7 => "rear",
        }
    }
}

/// One fresh sensor. `temp` is `None` only for a value that isn't a number (it still counts as present, as in
/// Python's `climate.py`).
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub sensor: Sensor,
    pub temp: Option<f64>,
    pub humidity: Option<f64>,
}

/// A Ruuvi counts while the Cerbo is connected and its value is younger than [`RUUVI_STALE`] (the publisher's age).
fn ruuvi_fresh(state: &dyn StateView, key: &str) -> bool {
    state.val(key).is_some() && !state.is_false("victron.connected") && state.age(key).is_some_and(|a| a < RUUVI_STALE)
}

/// The fresh sensors, in the order front Ruuvi, rear Ruuvi, MCS7. `now` is the wall clock (s), against which the
/// MCS7's `climate.rixens.updated` is compared.
pub fn fresh(state: &dyn StateView, now: f64) -> Vec<Reading> {
    let mut out = Vec::new();
    for (sensor, p) in [(Sensor::Front, "climate.cabin_front"), (Sensor::Rear, "climate.cabin_rear")] {
        let t = format!("{p}.temp");
        if ruuvi_fresh(state, &t) {
            out.push(Reading { sensor, temp: state.num(&t), humidity: state.num(&format!("{p}.humidity")) });
        }
    }
    let upd = state.num("climate.rixens.updated");
    if state.val("climate.rixens.cabin_c").is_some()
        && !state.is_false("climate.rixens.live")
        && upd.is_some_and(|u| now - u < MCS7_STALE)
    {
        out.push(Reading {
            sensor: Sensor::Mcs7,
            temp: state.num("climate.rixens.cabin_c"),
            humidity: state.num("climate.rixens.cabin_rh"),
        });
    }
    out
}

/// The mean of the values present, to 2 decimals; `None` when there are none.
pub fn mean(vals: impl IntoIterator<Item = Option<f64>>) -> Option<f64> {
    let v: Vec<f64> = vals.into_iter().flatten().collect();
    if v.is_empty() { None } else { Some(round(v.iter().sum::<f64>() / v.len() as f64, 2)) }
}

/// The cabin mean: (temperature, humidity, the sensors used).
pub fn cabin(state: &dyn StateView, now: f64) -> (Option<f64>, Option<f64>, Vec<Reading>) {
    let used = fresh(state, now);
    (mean(used.iter().map(|r| r.temp)), mean(used.iter().map(|r| r.humidity)), used)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::StateStore;
    use serde_json::json;

    fn van(now: f64) -> StateStore {
        let s = StateStore::new();
        s.update([
            ("climate.cabin_front.temp", json!(5.25)),
            ("climate.cabin_front.humidity", json!(51.5)),
            ("climate.cabin_rear.temp", json!(8.71)),
            ("climate.cabin_rear.humidity", json!(41.6)),
            ("victron.connected", json!(true)),
            ("climate.rixens.cabin_c", json!(5.61)),
            ("climate.rixens.cabin_rh", json!(49.0)),
            ("climate.rixens.live", json!(true)),
            ("climate.rixens.updated", json!(now)),
        ]);
        s
    }

    #[test]
    fn the_mean_of_all_three() {
        let now = crate::clock::now_s();
        let (t, h, used) = cabin(&van(now), now);
        assert_eq!((t, h), (Some(6.52), Some(47.37)));
        assert_eq!(used.iter().map(|r| r.sensor).collect::<Vec<_>>(), [Sensor::Front, Sensor::Rear, Sensor::Mcs7]);
    }

    #[test]
    fn a_stale_or_missing_sensor_is_left_out() {
        let now = crate::clock::now_s();
        let s = van(now);
        // the MCS7 not polled for over a minute → the two Ruuvis
        assert_eq!(cabin(&s, now + 61.0).0, Some(6.98));
        // the rear Ruuvi older than 15 min (the publisher's age) → front + MCS7, exactly as before it existed
        s.set_at("climate.cabin_rear.temp", json!(8.71), ((now - 16.0 * 60.0) * 1000.0) as i64);
        assert_eq!(cabin(&s, now).0, Some(5.43));
        // a configured rear tag the Cerbo no longer has (null) → left out
        s.set("climate.cabin_rear.temp", serde_json::Value::Null);
        assert_eq!(cabin(&s, now).0, Some(5.43));
        // the Cerbo disconnected → both Ruuvis out, the MCS7 alone
        s.set("climate.cabin_rear.temp", json!(8.71));
        s.set("victron.connected", json!(false));
        let (t, _, used) = cabin(&s, now);
        assert_eq!((t, used.len()), (Some(5.61), 1));
        // nothing fresh → nothing
        assert_eq!(cabin(&s, now + 61.0), (None, None, vec![]));
    }
}
