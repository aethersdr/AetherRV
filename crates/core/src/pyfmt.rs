//! Python `str()` of JSON-like values, so log rows (`command_log.value`, log lines) read exactly as the Python
//! reference wrote them: `{'mode': 'off'}`, `True`, `None`, `[1, 2]`, `21.0`.

use serde_json::Value;

/// Python `str(value)`.
pub fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// Python `repr(value)`.
pub fn py_repr(v: &Value) -> String {
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

/// Python float repr: shortest round-trip digits, always with a decimal point (`21.0`, `0.1`, `1e-05`).
pub fn py_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let a = f.abs();
    if a != 0.0 && !(1e-4..1e16).contains(&a) {
        // Python switches to exponent notation outside [1e-4, 1e16).
        let s = format!("{f:e}"); // e.g. 1e-5, 1.5e20
        let (mant, exp) = s.split_once('e').unwrap();
        let e: i32 = exp.parse().unwrap();
        return format!("{mant}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs());
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

    #[test]
    fn matches_python_str() {
        assert_eq!(py_str(&json!({"mode": "off"})), "{'mode': 'off'}");
        assert_eq!(py_str(&json!({"channel": 12, "on": true})), "{'channel': 12, 'on': True}");
        assert_eq!(py_str(&json!(null)), "None");
        assert_eq!(py_str(&json!(false)), "False");
        assert_eq!(py_str(&json!(21.0)), "21.0");
        assert_eq!(py_str(&json!(0.1)), "0.1");
        assert_eq!(py_str(&json!(1e-5)), "1e-05");
        assert_eq!(py_str(&json!("auto")), "auto");
        assert_eq!(py_str(&json!(["a", 1])), "['a', 1]");
        assert_eq!(py_str(&json!("it's")), "it's");
        assert_eq!(py_repr(&json!("it's")), "\"it's\"");
    }
}
