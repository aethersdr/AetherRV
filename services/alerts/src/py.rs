//! Python value semantics the rules depend on (`alerts.py` reads JSON-like dicts with `.get`, `[]`, `or`, f-strings).
//!
//! A rule that would raise in Python (a missing `d['key']`, formatting `None` with `:.0f`, `.get` on a list, …)
//! returns a [`RuleError`] here, and the engine skips that rule's whole pass, exactly like Python's `except`.

use crate::RuleError;
use aetherrv_core::pyfmt::{py_float, py_str};
use serde_json::{Map, Value};

pub type R<T> = Result<T, RuleError>;

pub fn type_error(what: &str) -> RuleError {
    RuleError(format!("TypeError: {what}"))
}

/// Python truthiness of `x.get(k)` (missing = `None`).
pub fn truthy(v: Option<&Value>) -> bool {
    v.is_some_and(aetherrv_core::config::truthy)
}

/// `x is None` (missing or JSON null).
pub fn is_none(v: Option<&Value>) -> bool {
    v.is_none_or(Value::is_null)
}

/// `x is False`.
pub fn is_false(v: Option<&Value>) -> bool {
    matches!(v, Some(Value::Bool(false)))
}

/// `str(x)` (missing = `"None"`).
pub fn s(v: Option<&Value>) -> String {
    py_str(v.unwrap_or(&Value::Null))
}

/// `x or "default"`, as text.
pub fn or_str(v: Option<&Value>, default: &str) -> String {
    if truthy(v) { s(v) } else { default.to_string() }
}

/// `_num(x)`: an int or float that isn't a bool.
pub fn num(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    }
}

/// A value used in arithmetic, a comparison or a `:.Nf` format: numbers and bools (Python's `bool` is an `int`);
/// anything else raises `TypeError`.
pub fn arith(v: Option<&Value>) -> R<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64().ok_or_else(|| type_error("not a number")),
        Some(Value::Bool(b)) => Ok(if *b { 1.0 } else { 0.0 }),
        Some(other) => Err(type_error(&format!("unsupported operand {other}"))),
        None => Err(type_error("unsupported operand None")),
    }
}

/// `f"{x:.Nf}"`.
pub fn f(v: Option<&Value>, prec: usize) -> R<String> {
    Ok(fx(arith(v)?, prec))
}

/// `f"{x:.Nf}"` of a float (Rust's `{:.N}` rounds the exact binary value half-to-even, as Python does).
pub fn fx(x: f64, prec: usize) -> String {
    format!("{x:.prec$}")
}

/// `int(x)` of a number (truncation toward zero).
pub fn int(v: Option<&Value>) -> R<i64> {
    Ok(arith(v)?.trunc() as i64)
}

/// Python `==` between JSON values: numbers (and bools) by value, everything else structurally.
pub fn eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(_) | Value::Bool(_), Value::Number(_) | Value::Bool(_)) => {
            matches!((arith(Some(a)), arith(Some(b))), (Ok(x), Ok(y)) if x == y)
        }
        _ => a == b,
    }
}

/// `for x in s.get(k) or []`: the items of a list; nothing when falsy. Other truthy values can't be iterated the
/// way the rules use them.
pub fn items(v: Option<Value>) -> R<Vec<Value>> {
    match v {
        Some(Value::Array(a)) => Ok(a),
        Some(v) if aetherrv_core::config::truthy(&v) => Err(type_error(&format!("cannot iterate {v}"))),
        _ => Ok(Vec::new()),
    }
}

/// A list item used as a dict (`.get` on anything else raises `AttributeError`).
pub fn obj(v: &Value) -> R<&Map<String, Value>> {
    v.as_object().ok_or_else(|| RuleError(format!("AttributeError: {v} has no attribute 'get'")))
}

/// `d[k]` (`KeyError` when missing; a stored null is `None`).
pub fn key<'a>(m: &'a Map<String, Value>, k: &str) -> R<&'a Value> {
    m.get(k).ok_or_else(|| RuleError(format!("KeyError: '{k}'")))
}

/// `x[0].upper() + x[1:]` of a string.
pub fn cap(v: &Value) -> R<String> {
    let Value::String(t) = v else { return Err(type_error(&format!("{v} is not subscriptable as text"))) };
    let mut c = t.chars();
    Ok(match c.next() {
        Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
        None => return Err(RuleError("IndexError: string index out of range".into())),
    })
}

/// Python's `str(x)` of a float (for computed values).
pub fn pyf(x: f64) -> String {
    py_float(x)
}
