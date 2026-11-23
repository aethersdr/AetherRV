//! Derived energy figures for the UI: fill in what the Cerbo can't see (port of `energy.derive`).
//!
//! The Cerbo only knows an alternator if it appears as a DC source it recognises. A Wakespeed WS500 on NMEA 2000 is
//! not one, so the Cerbo reports no alternator power and books the charging current as NEGATIVE DC load (its
//! Dc/System/Power = known sources − battery power). When that happens and the N2K adapter has alternator readings,
//! use them for the alternator, and take the DC load as the gap between the battery shunt and the regulator's
//! current (owner's model, 2026-11-21):
//!     DC load = (battery current − regulator current) × battery voltage

use crate::pyfmt;
use serde_json::Value;

/// A Python number: ints stay ints (`round(-1808, 1)` is `-1808`), floats stay floats.
#[derive(Clone, Copy)]
enum Num {
    Int(i64),
    Float(f64),
}

impl Num {
    /// `None` for anything Python arithmetic would reject (bool counts as int, as in Python).
    fn of(v: &Value) -> Option<Num> {
        match v {
            Value::Bool(b) => Some(Num::Int(i64::from(*b))),
            Value::Number(n) => Some(n.as_i64().map_or_else(|| Num::Float(n.as_f64().unwrap_or(f64::NAN)), Num::Int)),
            _ => None,
        }
    }

    fn f(self) -> f64 {
        match self {
            Num::Int(i) => i as f64,
            Num::Float(f) => f,
        }
    }

    fn sub(self, o: Num) -> Num {
        match (self, o) {
            (Num::Int(a), Num::Int(b)) => a.checked_sub(b).map_or(Num::Float(a as f64 - b as f64), Num::Int),
            _ => Num::Float(self.f() - o.f()),
        }
    }

    fn mul(self, o: Num) -> Num {
        match (self, o) {
            (Num::Int(a), Num::Int(b)) => a.checked_mul(b).map_or(Num::Float(a as f64 * b as f64), Num::Int),
            _ => Num::Float(self.f() * o.f()),
        }
    }

    /// Python `max(0.0, x)`: `x` only if it is greater (NaN and ties give `0.0`).
    fn max0(self) -> Num {
        if self.f() > 0.0 { self } else { Num::Float(0.0) }
    }

    /// Python `round(x, 1)`.
    fn round1(self) -> Value {
        match self {
            Num::Int(i) => Value::from(i),
            Num::Float(f) => Value::from(pyfmt::round(f, 1)),
        }
    }
}

/// Python `energy.derive(state)`. `get` reads a state key (`None` or JSON null = Python `None`); the result is the
/// three keys to set, in Python's order: `energy.alternator.power`, `energy.alternator.source`, `energy.dc.load`.
pub fn derive(get: impl Fn(&str) -> Option<Value>) -> Vec<(String, Value)> {
    let val = |k: &str| get(k).filter(|v| !v.is_null());
    let cerbo_alt = val("victron.alternator.power");
    let n2k_alt = val("alternator.power");
    // The Cerbo's figure, passed through `round(dc, 1)` unless replaced below. `None`: unknown (or not a number).
    let mut dc: Option<Num> = val("victron.dc.load").and_then(|v| Num::of(&v));
    let (alt, src) = if let Some(a) = cerbo_alt {
        (a, Value::from("cerbo"))
    } else if let Some(a) = n2k_alt {
        let (i_batt, v_batt, i_alt) =
            (val("victron.battery.current"), val("victron.battery.voltage"), val("alternator.current"));
        dc = match (i_batt, v_batt, i_alt) {
            (Some(ib), Some(vb), Some(ia)) => match (Num::of(&ib), Num::of(&vb), Num::of(&ia)) {
                (Some(ib), Some(vb), Some(ia)) => Some(ib.sub(ia).mul(vb).max0()),
                _ => None, // Python raises TypeError here; unknown is the safe reading
            },
            _ => None, // the Cerbo's figure is wrong while charging; don't show it
        };
        (a, Value::from("n2k"))
    } else {
        (Value::Null, Value::Null)
    };
    vec![
        ("energy.alternator.power".into(), alt),
        ("energy.alternator.source".into(), src),
        ("energy.dc.load".into(), dc.map_or(Value::Null, Num::round1)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    struct State(BTreeMap<String, Value>);

    impl State {
        fn set(&mut self, k: &str, v: Value) {
            self.0.insert(k.into(), v);
        }
        fn get(&self, k: &str) -> Value {
            self.0.get(k).cloned().unwrap_or(Value::Null)
        }
        fn derive(&mut self) {
            for (k, v) in derive(|k| self.0.get(k).cloned()) {
                self.0.insert(k, v);
            }
        }
    }

    /// py: tests/test_n2k.py::test_energy_derive_fills_in_alternator_and_fixes_dc_load
    #[test]
    fn test_energy_derive_fills_in_alternator_and_fixes_dc_load() {
        let mut s = State(BTreeMap::new());
        s.set("victron.alternator.power", Value::Null);
        s.set("victron.dc.load", json!(-1808.0));
        s.set("alternator.power", json!(1781));
        s.set("victron.battery.current", json!(135.5));
        s.set("victron.battery.voltage", json!(13.67));
        s.set("alternator.current", json!(129.5));
        s.derive();
        assert!(s.get("energy.alternator.power") == json!(1781) && s.get("energy.alternator.source") == json!("n2k"));
        assert_eq!(s.get("energy.dc.load"), json!(pyfmt::round((135.5 - 129.5) * 13.67, 1))); // 82.0 W: shunt − regulator gap
        s.set("alternator.current", Value::Null); // no regulator current → unknown, not the Cerbo's −1808
        s.derive();
        assert_eq!(s.get("energy.dc.load"), Value::Null);
        s.set("alternator.current", json!(129.5));
        s.set("victron.alternator.power", json!(900)); // a Cerbo-known alternator wins, DC load untouched
        s.derive();
        assert!(s.get("energy.alternator.power") == json!(900) && s.get("energy.dc.load") == json!(-1808.0));
    }

    /// Python number types survive: an int DC load stays an int, a negative gap clamps to 0.0.
    #[test]
    fn python_number_types() {
        let st = |pairs: &[(&str, Value)]| {
            let m: BTreeMap<String, Value> = pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
            derive(|k| m.get(k).cloned())
        };
        let r = st(&[("victron.dc.load", json!(-1808))]);
        assert_eq!(r[2].1, json!(-1808));
        assert!(r[2].1.is_i64() && r[0].1.is_null() && r[1].1.is_null());
        let r = st(&[
            ("alternator.power", json!(5)),
            ("victron.battery.current", json!(135)),
            ("victron.battery.voltage", json!(13)),
            ("alternator.current", json!(129)),
        ]);
        assert!(r[2].1 == json!(78) && r[2].1.is_i64());
        let r = st(&[
            ("alternator.power", json!(5)),
            ("victron.battery.current", json!(100)),
            ("victron.battery.voltage", json!(13)),
            ("alternator.current", json!(129)),
        ]);
        assert!(r[2].1 == json!(0.0) && r[2].1.is_f64());
    }
}
