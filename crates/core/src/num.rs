//! Python number semantics the state values depend on.
//!
//! `round(x, n)` in Python rounds the exact binary value half-to-even (`round(2.675, 2) == 2.67`,
//! `round(0.125, 2) == 0.12`). Rust's `{:.n}` formatting does the same, so rounding goes through it.

/// Python `round(x, n)` (float result).
pub fn round(x: f64, n: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    let r: f64 = format!("{x:.n$}").parse().unwrap_or(x);
    if r == 0.0 { 0.0_f64.copysign(r) } else { r }
}

pub fn round1(x: f64) -> f64 {
    round(x, 1)
}

/// Python `round(x)` (int result, half-to-even).
pub fn round_int(x: f64) -> i64 {
    format!("{x:.0}").parse::<f64>().map(|f| f as i64).unwrap_or(0)
}

/// JSON number for a value that Python would hold as `float` (always written with a decimal point).
pub fn jf(x: f64) -> serde_json::Value {
    serde_json::Number::from_f64(x).map(serde_json::Value::Number).unwrap_or(serde_json::Value::Null)
}

/// Optional float → JSON (None → null).
pub fn jof(x: Option<f64>) -> serde_json::Value {
    x.map(jf).unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_round() {
        assert_eq!(round(2.675, 2), 2.67);
        assert_eq!(round(0.125, 2), 0.12);
        assert_eq!(round(18.45, 1), 18.4);
        assert_eq!(round_int(2.5), 2);
        assert_eq!(round_int(3.5), 4);
        assert_eq!(round_int(-2.5), -2);
        assert!(round(-0.04, 1).is_sign_negative());
    }
}
