//! The Python semantics the reference relies on: `str.strip`, `int()`, `float()`, `round`, `bool()`,
//! `repr()` of a float or a str, and the exception texts of `int()`/`float()` on a request value.

use serde_json::Value;

/// Python `str.isspace()` (what `str.strip()` strips): Unicode White_Space plus U+001C..U+001F.
fn isspace(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// Python `str.strip()`.
pub fn strip(s: &str) -> &str {
    s.trim_matches(isspace)
}

/// Remove digit-group underscores as CPython does: each `_` must follow and precede an ASCII digit.
fn without_underscores(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    let mut prev = '\0';
    for c in s.chars() {
        if c == '_' {
            if !prev.is_ascii_digit() {
                return None;
            }
        } else {
            if prev == '_' && !c.is_ascii_digit() {
                return None;
            }
            out.push(c);
        }
        prev = c;
    }
    (prev != '_').then_some(out)
}

/// Python `int(s)` (base 10) for a str: `None` on ValueError. A value beyond i128 saturates (Python would
/// keep the exact big int; every caller only range-checks or publishes values far inside i64).
pub fn int_str(s: &str) -> Option<i128> {
    let s = s.trim_matches(char::is_whitespace);
    let (neg, s) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let digits = without_underscores(s)?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let v = digits.parse::<i128>().unwrap_or(i128::MAX);
    Some(if neg { -v } else { v })
}

/// Python `_int(v)` of the reference: `int(str(v).strip())`, `None` on failure, as an i64 (a status.xml
/// number beyond i64 reads as `None`).
pub fn int(s: &str) -> Option<i64> {
    int_str(strip(s)).and_then(|v| i64::try_from(v).ok())
}

/// Python `float(s)` for a str, `None` on ValueError. `float()` strips `char::is_whitespace` (not
/// U+001C..U+001F) and allows single underscores between digits; the rest is Rust's grammar, which accepts
/// the same strings as CPython's (`inf`, `infinity`, `nan` in any case, an optional sign, `.5`, `5.`, `1e5`).
pub fn float_str(s: &str) -> Option<f64> {
    let s = s.trim_matches(char::is_whitespace);
    let s = without_underscores(s)?;
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.')) {
        return None;
    }
    s.parse().ok()
}

/// Python `round(x, n)` on a float (exact value, ties to even): Rust's exact formatting is correctly
/// rounded with ties to even, the same operation as CPython's dtoa mode 3.
pub fn round(x: f64, n: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.n$}").parse().unwrap_or(x)
}

/// Python `round(x)` (no digits) of a finite float: the nearest integer, ties to even.
pub fn round_int(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// A float for the state (`None` → null; a non-finite float, which JSON can't carry, also → null).
pub fn num(x: Option<f64>) -> Value {
    x.and_then(serde_json::Number::from_f64).map_or(Value::Null, Value::Number)
}

/// Python `bool(value)` of a JSON value.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// Python `float(value)` of a JSON value; `Err` carries the exception text Python would raise.
pub fn float_of(v: &Value) -> Result<f64, String> {
    match v {
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => Ok(n.as_f64().unwrap_or(f64::NAN)),
        Value::String(s) => float_str(s).ok_or_else(|| format!("could not convert string to float: {}", str_repr(s))),
        other => Err(format!("float() argument must be a string or a real number, not '{}'", type_name(other))),
    }
}

/// Python `int(value)` of a JSON value (a float truncates; saturating beyond i128); `Err` carries the
/// exception text Python would raise.
pub fn int_of(v: &Value) -> Result<i128, String> {
    match v {
        Value::Bool(b) => Ok(*b as i128),
        Value::Number(n) => Ok(match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => i as i128,
            (None, Some(u)) => u as i128,
            _ => n.as_f64().unwrap_or(0.0).trunc() as i128,
        }),
        Value::String(s) => {
            int_str(s).ok_or_else(|| format!("invalid literal for int() with base 10: {}", str_repr(s)))
        }
        other => Err(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            type_name(other)
        )),
    }
}

/// Python `repr(float)`: shortest round-trip digits, always with a decimal point or an exponent
/// (`40.0`, `1e+20`, `0.30000000000000004`).
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let a = f.abs();
    if a != 0.0 && !(1e-4..1e16).contains(&a) {
        let s = format!("{f:e}");
        let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
        let e: i32 = exp.parse().unwrap_or(0);
        return format!("{mant}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs());
    }
    let s = format!("{f}");
    if s.contains('.') { s } else { format!("{s}.0") }
}

/// Python `repr(str)`.
pub fn str_repr(s: &str) -> String {
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
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
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
    fn exception_texts_match_python() {
        // As printed by Python 3.12 for float(v) / int(v)
        assert_eq!(
            float_of(&json!(null)).unwrap_err(),
            "float() argument must be a string or a real number, not 'NoneType'"
        );
        assert_eq!(
            float_of(&json!([1])).unwrap_err(),
            "float() argument must be a string or a real number, not 'list'"
        );
        assert_eq!(float_of(&json!("abc")).unwrap_err(), "could not convert string to float: 'abc'");
        assert_eq!(float_of(&json!(" 4 0 ")).unwrap_err(), "could not convert string to float: ' 4 0 '");
        assert_eq!(float_of(&json!("it's")).unwrap_err(), "could not convert string to float: \"it's\"");
        assert_eq!(float_of(&json!(true)), Ok(1.0));
        assert_eq!(float_of(&json!("1e3")), Ok(1000.0));
        assert_eq!(
            int_of(&json!({"a": 1})).unwrap_err(),
            "int() argument must be a string, a bytes-like object or a real number, not 'dict'"
        );
        assert_eq!(int_of(&json!("40.0")).unwrap_err(), "invalid literal for int() with base 10: '40.0'");
        assert_eq!(int_of(&json!(40.7)), Ok(40));
        assert_eq!(int_of(&json!(true)), Ok(1));
        assert_eq!(int_of(&json!(" 1_000 ")), Ok(1000));
        assert_eq!(float_repr(40.0), "40.0");
        assert_eq!(float_repr(1e20), "1e+20");
        assert_eq!(float_repr(1e16), "1e+16");
        assert_eq!(float_repr(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(str_repr("floor"), "'floor'");
        assert_eq!(str_repr("it's"), "\"it's\"");
    }
}
