//! The bits of Python semantics the port relies on: `round()`, truthiness, `str()`, `int()` of JSON values.
//! (`aetherrv-core::pyfmt` has the same `str()`; this crate stays free of core's runtime dependencies.)

use serde_json::Value;

/// Python `round(x, ndigits)` on a float: the exact binary value rounded half-to-even to `ndigits` decimals, then
/// the nearest double (CPython does the same through dtoa). Rust's `{:.N}` formatting is exact and ties-to-even.
pub(crate) fn round(x: f64, ndigits: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.ndigits$}").parse().unwrap_or(x)
}

/// Python `round(x)` (no ndigits) on a finite float: an int, half-to-even.
pub(crate) fn round_int(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// A float as a JSON value (`None` → null).
pub(crate) fn opt_f64(v: Option<f64>) -> Value {
    v.map_or(Value::Null, Value::from)
}

/// Python truthiness of a JSON value.
pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

/// Python `int(v)` for the value types a TOML/JSON config can hold. `None` where Python raises.
pub(crate) fn int(v: &Value) -> Option<i64> {
    match v {
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f.trunc() as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// The integer a JSON value is equal to *as a Python dict key* (`d.get(v)` on an int-keyed dict: `176`, `176.0`
/// and `True`/`False` hit `176`/`1`/`0`). `None` for anything that can't equal an int.
pub(crate) fn int_key(v: &Value) -> Option<i64> {
    match v {
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().filter(|f| f.is_finite() && f.fract() == 0.0 && f.abs() < 9.0e18).map(|f| f as i64)),
        _ => None,
    }
}

/// Python `str(value)`.
pub(crate) fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// Python `repr(value)`.
fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                py_float(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => py_str_repr(s),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter().map(|(k, v)| format!("{}: {}", py_str_repr(k), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Python float repr: shortest round-trip digits, always with a decimal point or an exponent.
fn py_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let a = f.abs();
    if a != 0.0 && !(1e-4..1e16).contains(&a) {
        let s = format!("{f:e}");
        if let Some((mant, exp)) = s.split_once('e') {
            let e: i32 = exp.parse().unwrap_or(0);
            return format!("{mant}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs());
        }
        return s;
    }
    let s = format!("{f}");
    if s.contains('.') { s } else { format!("{s}.0") }
}

fn py_str_repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c)
            }
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Values checked against CPython 3.12 `round()`.
    #[test]
    fn round_matches_python() {
        for (x, n, want) in [
            (0.125, 2, 0.12),
            (0.375, 2, 0.38),
            (2.675, 2, 2.67),
            (0.05, 1, 0.1),
            (0.15, 1, 0.1),
            (0.25, 1, 0.2),
            (0.35, 1, 0.3),
            (2100.0 * 0.001, 3, 2.1),
            (6.0 * 13.67, 1, 82.0),
        ] {
            assert_eq!(round(x, n), want, "round({x}, {n})");
        }
        assert!(round(-0.04, 1).is_sign_negative() && round(-0.04, 1) == 0.0);
        assert_eq!(round_int(2.5), 2);
        assert_eq!(round_int(3.5), 4);
        assert_eq!(round_int(13.75 * 148.4), 2040);
    }

    #[test]
    fn str_and_truthiness() {
        assert_eq!(py_str(&json!(1540324)), "1540324");
        assert_eq!(py_str(&json!("1.0.3.66")), "1.0.3.66");
        assert_eq!(py_str(&json!(1.5)), "1.5");
        assert_eq!(py_str(&json!(true)), "True");
        assert!(!truthy(&json!(0)) && !truthy(&json!("")) && truthy(&json!("x")) && truthy(&json!(-1.0)));
        assert_eq!(int(&json!(" 32 ")), Some(32));
        assert_eq!(int(&json!(32.7)), Some(32));
        assert_eq!(int(&json!("32.0")), None);
        assert_eq!(int_key(&json!(176.0)), Some(176));
        assert_eq!(int_key(&json!("176")), None);
    }
}
