//! The few Python numeric semantics the reference relies on.

use serde_json::Value;

/// Python `round(x, n)` on a float: the exact binary value rounded half-to-even to `n` decimals.
/// Rust's exact formatting is correctly rounded with ties to even, so format + parse is the same
/// operation as CPython's `_Py_dg_dtoa(mode 3)` + `strtod`.
pub fn round(x: f64, n: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.n$}").parse().unwrap_or(x)
}

/// Python `round(x)` (no digits): the nearest integer, ties to even.
pub fn round_int(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// Python 3.12+ `sum()` of floats (start 0): Neumaier-compensated. `None` when there is nothing to add
/// (Python then returns the int 0).
pub fn sum(xs: impl IntoIterator<Item = f64>) -> Option<f64> {
    let mut it = xs.into_iter();
    let mut f = 0.0 + it.next()?; // int 0 + float
    let mut c = 0.0;
    for x in it {
        let t = f + x;
        if f.abs() >= x.abs() {
            c += (f - t) + x;
        } else {
            c += (x - t) + f;
        }
        f = t;
    }
    if c != 0.0 && c.is_finite() {
        f += c;
    }
    Some(f)
}

/// A float for the state (`None` → null; a non-finite float, which JSON can't carry, also → null).
pub fn num(x: Option<f64>) -> Value {
    x.and_then(serde_json::Number::from_f64).map_or(Value::Null, Value::Number)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sum_is_neumaier_like_python_312() {
        // Python 3.12: sum([0.1] * 10) == 1.0 and sum([1e100, 1.0, -1e100]) == 1.0
        assert_eq!(sum([0.1; 10]), Some(1.0));
        assert_eq!(sum([1e100, 1.0, -1e100]), Some(1.0));
        assert_eq!(sum([]), None);
    }

    #[test]
    fn round_matches_python() {
        assert_eq!(round(2.675, 2), 2.67); // binary 2.67499…
        assert_eq!(round(0.125, 2), 0.12); // exact tie → even
        assert_eq!(round(450.307, 1), 450.3);
        assert_eq!(round_int(2.5), 2);
        assert_eq!(round_int(3.5), 4);
    }
}
