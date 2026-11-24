//! MultiPlus mode, shore current limit, MPPT on/off and the alarm acknowledge (owner-approved 2026-11-20 and
//! 2026-11-21): the validation half of Python's `VictronAdapter.plan()` / `_same()`, with no I/O.

use crate::model::{Model, eq_one, py_num};
use aetherrv_core::num::{jf, round};
use aetherrv_core::pyfmt::py_repr;
use serde_json::{Value, json};

/// MultiPlus modes: 1 charger only, 2 inverter only, 3 on, 4 off.
pub const VEBUS_MODES: [i64; 4] = [1, 2, 3, 4];
/// MultiPlus 12/3000/120-50: 50 A transfer switch.
pub const SHORE_MIN_A: f64 = 3.0;
pub const SHORE_MAX_A: f64 = 50.0;
/// The two unacknowledged counts `ack_alarms` reads and verifies.
pub const ACK_COUNT: [&str; 2] =
    ["platform/0/Notifications/NumberOfUnAcknowledgedAlarms", "platform/0/Notifications/NumberOfUnAcknowledgedWarnings"];
pub const ACK_ALL: &str = "platform/0/Notifications/AcknowledgeAll";

/// A mode must be a JSON integer. Python's `int(value)` also took `"3"`, `True` (= 1, charger only) and truncated
/// `3.7` to 3; those are refused here.
fn json_int(cmd: &str, v: &Value) -> Result<i64, String> {
    match v {
        Value::Number(n) if n.is_i64() || n.is_u64() => n.as_i64().ok_or_else(|| bad(cmd, "integer")),
        _ => Err(bad(cmd, "integer")),
    }
}

/// A current must be a JSON number. Python's `float(value)` also took `"15"` and `True`.
fn json_number(cmd: &str, v: &Value) -> Result<f64, String> {
    match v {
        Value::Number(n) => n.as_f64().filter(|f| f.is_finite()).ok_or_else(|| bad(cmd, "number")),
        _ => Err(bad(cmd, "number")),
    }
}

fn bad(cmd: &str, kind: &str) -> String {
    format!("bad request: {cmd} needs a JSON {kind}")
}

/// Validate against the allowlist + the Cerbo's own "IsAdjustable" flags → (path, value to write).
pub fn plan(m: &Model, cmd: &str, value: &Value) -> Result<(String, Value), String> {
    match cmd {
        "inverter_mode" => {
            let v = json_int(cmd, value)?;
            if !VEBUS_MODES.contains(&v) {
                return Err(format!("MultiPlus mode must be one of {}", py_repr(&json!(VEBUS_MODES))));
            }
            let vb = m.inst("vebus").ok_or("no MultiPlus (vebus) found on the Cerbo")?;
            if !eq_one(m.vals.get(&format!("vebus/{vb}/ModeIsAdjustable"))) {
                return Err("the Cerbo reports the MultiPlus mode is not adjustable (remote panel/switch?)".into());
            }
            Ok((format!("vebus/{vb}/Mode"), json!(v)))
        }
        "shore_limit" => {
            let a = round(json_number(cmd, value)?, 1);
            if !(SHORE_MIN_A..=SHORE_MAX_A).contains(&a) {
                return Err(format!("shore current limit must be {}-{} A", py_float(SHORE_MIN_A), py_float(SHORE_MAX_A)));
            }
            let vb = m.inst("vebus");
            let adjustable = vb.as_ref().is_some_and(|vb| eq_one(m.vals.get(&format!("vebus/{vb}/Ac/In/1/CurrentLimitIsAdjustable"))));
            match vb {
                Some(vb) if adjustable => Ok((format!("vebus/{vb}/Ac/In/1/CurrentLimit"), jf(a))),
                _ => Err("the Cerbo reports the shore current limit is not adjustable right now".into()),
            }
        }
        "mppt_mode" => {
            let v = json_int(cmd, value)?;
            if v != 1 && v != 4 {
                return Err("MPPT mode must be 1 (on) or 4 (off)".into());
            }
            let mp = m.inst("solarcharger").ok_or("no solar charger found on the Cerbo")?;
            Ok((format!("solarcharger/{mp}/Mode"), json!(v)))
        }
        _ => Err(format!("command {} is not allowed", py_repr(&json!(cmd)))),
    }
}

fn py_float(f: f64) -> String {
    aetherrv_core::pyfmt::py_float(f)
}

/// Python `_same(a, b)`: numbers (bools included) within 0.05, anything else by equality.
pub fn same(a: Option<&Value>, b: &Value) -> bool {
    let a = a.unwrap_or(&Value::Null);
    match (py_num(a), py_num(b)) {
        (Some(x), Some(y)) => (x - y).abs() < 0.05,
        _ => a == b,
    }
}

/// The `W/` payload exactly as Python's `json.dumps({"value": v})` writes it (`{"value": 3}`, `{"value": 15.0}`).
pub fn write_payload(v: &Value) -> Vec<u8> {
    format!("{{\"value\": {}}}", serde_json::to_string(v).unwrap_or_else(|_| "null".into())).into_bytes()
}

/// Python `any(self.vals.get(p) for p in ACK_COUNT)`.
pub fn unacknowledged(m: &Model) -> bool {
    ACK_COUNT.iter().any(|p| m.vals.get(*p).is_some_and(aetherrv_core::config::truthy))
}

/// Python `[self.vals.get(p) for p in ACK_COUNT]`.
pub fn ack_counts(m: &Model) -> Value {
    Value::Array(ACK_COUNT.iter().map(|p| m.vals.get(*p).cloned().unwrap_or(Value::Null)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_payload_matches_python_json_dumps() {
        assert_eq!(write_payload(&json!(3)), br#"{"value": 3}"#);
        assert_eq!(write_payload(&jf(15.0)), br#"{"value": 15.0}"#);
    }

    #[test]
    fn values_must_be_json_numbers_of_the_right_kind() {
        let mut m = Model::default();
        m.vals.insert("vebus/276/ModeIsAdjustable".into(), json!(1));
        m.vals.insert("vebus/276/Ac/In/1/CurrentLimitIsAdjustable".into(), json!(1));
        m.vals.insert("solarcharger/277/Mode".into(), json!(1));
        for (cmd, v) in [
            ("inverter_mode", json!("3")),
            ("inverter_mode", json!(true)),
            ("inverter_mode", json!(3.7)),
            ("inverter_mode", json!(3.0)),
            ("inverter_mode", json!(null)),
            ("mppt_mode", json!("4")),
            ("shore_limit", json!("15")),
            ("shore_limit", json!(true)),
        ] {
            let e = plan(&m, cmd, &v).unwrap_err();
            assert!(e.starts_with("bad request"), "{cmd}={v}: {e}");
        }
        assert_eq!(plan(&m, "inverter_mode", &json!(3)).unwrap(), ("vebus/276/Mode".into(), json!(3)));
        assert_eq!(plan(&m, "shore_limit", &json!(16)).unwrap(), ("vebus/276/Ac/In/1/CurrentLimit".into(), jf(16.0)));
        assert_eq!(plan(&m, "inverter_mode", &json!(5)).unwrap_err(), "MultiPlus mode must be one of [1, 2, 3, 4]");
        assert_eq!(plan(&m, "shore_limit", &json!(80)).unwrap_err(), "shore current limit must be 3.0-50.0 A");
        assert_eq!(plan(&m, "relay", &json!(1)).unwrap_err(), "command 'relay' is not allowed");
    }

    #[test]
    fn same_is_python_same() {
        assert!(same(Some(&json!(30)), &jf(30.04)));
        assert!(same(Some(&json!(true)), &json!(1)));
        assert!(!same(None, &json!(3)));
        assert!(same(Some(&json!("x")), &json!("x")));
    }
}
