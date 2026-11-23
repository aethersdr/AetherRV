//! The Python string and number semantics the reference relies on (`str.strip`, `float()`, `int(_, 16)`,
//! `round`, slicing, `splitlines`, `strptime`).

use serde_json::Value;

/// Python `str.isspace()` (what `str.strip()` strips): Unicode White_Space plus U+001C..U+001F.
fn isspace(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// Python `str.strip()`.
pub fn strip(s: &str) -> &str {
    s.trim_matches(isspace)
}

/// Python `float(s)` for a str, `None` on ValueError. `int()`/`float()` strip `char::is_whitespace`
/// (not U+001C..U+001F) and allow single underscores between digits.
pub fn float(s: &str) -> Option<f64> {
    let s = s.trim_matches(char::is_whitespace);
    let s = without_underscores(s)?;
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.')) {
        return None;
    }
    s.parse().ok()
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

/// Python `int(s, 16)`, `None` on ValueError.
pub fn int_base16(s: &str) -> Option<i64> {
    let s = s.trim_matches(char::is_whitespace);
    let (neg, s) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let s = match s.get(..2) {
        Some("0x" | "0X") => s[2..].strip_prefix('_').unwrap_or(&s[2..]),
        _ => s,
    };
    let digits = without_underscores(s)?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = i64::from_str_radix(&digits, 16).ok()?;
    Some(if neg { -v } else { v })
}

/// Python `round(x, n)` on a float (exact value, ties to even): Rust's exact formatting is correctly
/// rounded with ties to even, the same operation as CPython's dtoa mode 3.
pub fn round(x: f64, n: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.n$}").parse().unwrap_or(x)
}

/// A float for the state (`None` → null; a non-finite float, which JSON can't carry, also → null).
pub fn num(x: Option<f64>) -> Value {
    x.and_then(serde_json::Number::from_f64).map_or(Value::Null, Value::Number)
}

/// A Python slice bound `i` (may be negative) on a sequence of `len` → the index it means.
pub fn slice_index(i: isize, len: usize) -> usize {
    let len = len as isize;
    (if i < 0 { (i + len).max(0) } else { i.min(len) }) as usize
}

/// Python `s[-n:]`.
pub fn last_chars(s: &str, n: usize) -> &str {
    let count = s.chars().count();
    let skip = count.saturating_sub(n);
    s.char_indices().nth(skip).map_or("", |(i, _)| &s[i..])
}

/// Python `str.splitlines()` (no line ends kept; no trailing empty line).
pub fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}') {
            out.push(&s[start..i]);
            let mut end = i + c.len_utf8();
            if c == '\r' && matches!(it.peek(), Some((_, '\n'))) {
                it.next();
                end += 1;
            }
            start = end;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

// ── strptime("%d%m%y%H%M%S") ─────────────────────────────────────────────────────────────────────────────

type Alt = &'static [(char, char)];
const DIG: (char, char) = ('0', '9');
// CPython _strptime's regexes, alternatives in their order (`\d` is ASCII here: the listener decodes
// datagrams as ASCII, so no other digits can reach it).
const D: &[Alt] = &[
    &[('3', '3'), ('0', '1')],
    &[('1', '2'), DIG],
    &[('0', '0'), ('1', '9')],
    &[('1', '9')],
    &[(' ', ' '), ('1', '9')],
];
const MO: &[Alt] = &[&[('1', '1'), ('0', '2')], &[('0', '0'), ('1', '9')], &[('1', '9')]];
const Y: &[Alt] = &[&[DIG, DIG]];
const H: &[Alt] = &[&[('2', '2'), ('0', '3')], &[('0', '1'), DIG], &[DIG]];
const MI: &[Alt] = &[&[('0', '5'), DIG], &[DIG]];
const S: &[Alt] = &[&[('6', '6'), ('0', '1')], &[('0', '5'), DIG], &[DIG]];
const FMT: [&[Alt]; 6] = [D, MO, Y, H, MI, S];

/// Leftmost-first regex matching of the six groups (the first match found, as `re.match`, which need not
/// reach the end of the string).
fn search(c: &[char], pos: usize, k: usize, caps: &mut [(usize, usize); 6]) -> Option<usize> {
    if k == FMT.len() {
        return Some(pos);
    }
    for alt in FMT[k] {
        let end = pos + alt.len();
        if end <= c.len() && alt.iter().zip(&c[pos..end]).all(|(&(lo, hi), &ch)| lo <= ch && ch <= hi) {
            caps[k] = (pos, end);
            if let Some(e) = search(c, end, k + 1, caps) {
                return Some(e);
            }
        }
    }
    None
}

/// Python `datetime.strptime(s, "%d%m%y%H%M%S").replace(tzinfo=timezone.utc).timestamp()`, `None` where
/// Python raises ValueError (no match, unconverted data left, an invalid date, second 60/61).
pub fn strptime_dmyhms_utc(s: &str) -> Option<f64> {
    let c: Vec<char> = s.chars().collect();
    let mut caps = [(0, 0); 6];
    if search(&c, 0, 0, &mut caps)? != c.len() {
        return None; // "unconverted data remains"
    }
    let v: Vec<i64> = caps
        .iter()
        .map(|&(a, b)| c[a..b].iter().collect::<String>().trim_start().parse::<i64>().unwrap_or(0))
        .collect();
    let (day, month, yy, hour, minute, second) = (v[0], v[1], v[2], v[3], v[4], v[5]);
    let year = if yy <= 68 { yy + 2000 } else { yy + 1900 };
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let dim = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][(month - 1) as usize];
    if day > dim || second > 59 {
        return None; // datetime() refuses it
    }
    Some((days_from_civil(year, month, day) * 86400 + hour * 3600 + minute * 60 + second) as f64)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_parsing_edges() {
        assert_eq!(float("1_0"), Some(10.0));
        assert_eq!(float(" 5 "), Some(5.0));
        assert_eq!(float("+.5"), Some(0.5));
        assert_eq!(float("5."), Some(5.0));
        assert_eq!(float("1e1_0"), Some(1e10));
        for bad in ["_1", "1_", "1__0", "1_.5", "1._5", "1e", ".", "0x10", "\x1c5\x1f", ""] {
            assert_eq!(float(bad), None, "{bad:?}");
        }
        assert_eq!(int_base16("-0"), Some(0));
        assert_eq!(int_base16(" 4"), Some(4));
        assert_eq!(int_base16("4 "), Some(4));
        assert_eq!(int_base16("+f"), Some(15));
        assert_eq!(int_base16("0x"), None);
        assert_eq!(int_base16("\x1c4"), None);
        assert_eq!(strip(" \x1c a \x1f"), "a");
        assert_eq!(splitlines("a\r\nb\n\nc\r"), vec!["a", "b", "", "c"]);
        assert_eq!(splitlines("\n"), vec![""]);
        assert_eq!(slice_index(-1, 3), 2);
        assert_eq!(slice_index(-5, 3), 0);
        assert_eq!(last_chars("GPRMC", 3), "RMC");
        assert_eq!(last_chars("MC", 3), "MC");
    }

    #[test]
    fn strptime_like_python() {
        // Expected values printed by Python 3.12:
        // datetime.strptime(s, "%d%m%y%H%M%S").replace(tzinfo=timezone.utc).timestamp()
        let cases: [(&str, Option<f64>); 13] = [
            ("201126010203", Some(1795136523.0)),
            ("230394123519", Some(764426119.0)),
            ("2011261234", Some(1795176184.0)), // backtracking: 12:03:04
            ("2011260102", Some(1795136402.0)), // 01:00:02
            ("2011261234567", None),            // unconverted data remains: 7
            ("311126010203", None),             // day 31 in November
            ("201126010260", None),             // second 60
            ("29022", None),                    // does not match
            ("290228000000", Some(1835395200.0)),
            ("290227000000", None), // 29 Feb 2027
            ("0101690000", Some(-31536000.0)),
            ("010168000000", Some(3092601600.0)),
            (" 51126010203", Some(1793840523.0)),
        ];
        for (s, want) in cases {
            assert_eq!(strptime_dmyhms_utc(s), want, "{s:?}");
        }
        assert_eq!(strptime_dmyhms_utc("5112601023"), Some(1793840523.0));
    }
}
