//! JSON values as Python's str(), repr() and json.dumps() print them. The harness's summaries and boot figures were
//! first written by Python scripts; their output stays the same byte for byte: objects in the order read (serde_json's
//! own map sorts its keys), `5` and `5.0` kept apart, `None`/`True`, and json.dumps's `ensure_ascii` escapes.

use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use std::fmt::Write;

#[derive(Clone, Debug, PartialEq)]
pub enum Py {
    None,
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
    List(Vec<Py>),
    Dict(Vec<(String, Py)>),
}

static NONE: Py = Py::None;

impl Py {
    pub fn parse(s: &str) -> serde_json::Result<Py> {
        serde_json::from_str(s)
    }
    /// `d.get(k)`, or None when this isn't a dict (Python's `(x or {}).get(k)`).
    pub fn at(&self, k: &str) -> &Py {
        match self {
            Py::Dict(v) => v.iter().find(|(key, _)| key == k).map(|(_, x)| x).unwrap_or(&NONE),
            _ => &NONE,
        }
    }
    pub fn has(&self, k: &str) -> bool {
        matches!(self, Py::Dict(v) if v.iter().any(|(key, _)| key == k))
    }
    pub fn idx(&self, i: usize) -> &Py {
        match self {
            Py::List(v) => v.get(i).unwrap_or(&NONE),
            _ => &NONE,
        }
    }
    pub fn list(&self) -> &[Py] {
        match self {
            Py::List(v) => v,
            _ => &[],
        }
    }
    pub fn values(&self) -> Vec<&Py> {
        match self {
            Py::Dict(v) => v.iter().map(|(_, x)| x).collect(),
            _ => vec![],
        }
    }
    pub fn is_none(&self) -> bool {
        matches!(self, Py::None)
    }
    /// A number (Python's bool is one too).
    pub fn num(&self) -> Option<f64> {
        match self {
            Py::Bool(b) => Some(*b as i32 as f64),
            Py::Int(i) => Some(*i as f64),
            Py::Float(f) => Some(*f),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Py::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn truthy(&self) -> bool {
        match self {
            Py::None => false,
            Py::Bool(b) => *b,
            Py::Int(i) => *i != 0,
            Py::Float(f) => *f != 0.0,
            Py::Str(s) => !s.is_empty(),
            Py::List(v) => !v.is_empty(),
            Py::Dict(v) => !v.is_empty(),
        }
    }
    /// `x or y`
    pub fn or<'a>(&'a self, y: &'a Py) -> &'a Py {
        if self.truthy() { self } else { y }
    }
    pub fn repr(&self) -> String {
        match self {
            Py::None => "None".into(),
            Py::Bool(b) => (if *b { "True" } else { "False" }).into(),
            Py::Int(i) => i.to_string(),
            Py::Float(f) => float_repr(*f),
            Py::Str(s) => str_repr(s),
            Py::List(v) => format!("[{}]", v.iter().map(Py::repr).collect::<Vec<_>>().join(", ")),
            Py::Dict(v) => format!("{{{}}}", v.iter().map(|(k, x)| format!("{}: {}", str_repr(k), x.repr())).collect::<Vec<_>>().join(", ")),
        }
    }
    pub fn str(&self) -> String {
        match self {
            Py::Str(s) => s.clone(),
            _ => self.repr(),
        }
    }
    /// json.dumps(self) (indent None) or json.dump(self, f, indent=n)
    pub fn dumps(&self, indent: Option<usize>) -> String {
        let mut s = String::new();
        self.dump_into(&mut s, indent, 0);
        s
    }
    fn dump_into(&self, out: &mut String, indent: Option<usize>, level: usize) {
        let items = |out: &mut String, open: char, close: char, n: usize, each: &dyn Fn(&mut String, usize)| {
            out.push(open);
            if n > 0 {
                for i in 0..n {
                    match indent {
                        Some(w) => {
                            out.push_str(if i > 0 { ",\n" } else { "\n" });
                            out.push_str(&" ".repeat(w * (level + 1)));
                        }
                        None if i > 0 => out.push_str(", "),
                        None => {}
                    }
                    each(out, i);
                }
                if let Some(w) = indent {
                    out.push('\n');
                    out.push_str(&" ".repeat(w * level));
                }
            }
            out.push(close);
        };
        match self {
            Py::None => out.push_str("null"),
            Py::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Py::Int(i) => out.push_str(&i.to_string()),
            Py::Float(f) if f.is_nan() => out.push_str("NaN"),
            Py::Float(f) if f.is_infinite() => out.push_str(if *f > 0.0 { "Infinity" } else { "-Infinity" }),
            Py::Float(f) => out.push_str(&float_repr(*f)),
            Py::Str(s) => json_str(out, s),
            Py::List(v) => items(out, '[', ']', v.len(), &|o, i| v[i].dump_into(o, indent, level + 1)),
            Py::Dict(v) => items(out, '{', '}', v.len(), &|o, i| {
                json_str(o, &v[i].0);
                o.push_str(": ");
                v[i].1.dump_into(o, indent, level + 1);
            }),
        }
    }
}

impl From<Option<f64>> for Py {
    fn from(x: Option<f64>) -> Py {
        x.map_or(Py::None, Py::Float)
    }
}

/// float.__repr__: the shortest digits that read back the same, positional from 1e-4 to below 1e16.
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return (if x > 0.0 { "inf" } else { "-inf" }).into();
    }
    // the fewest digits that read back the same ({:e}), then those digits rounded half to even as Python does
    // ({:e} alone rounds a decimal tie up: 3.0163650512695313 where Python prints ...312)
    let n = format!("{x:e}").split('e').next().unwrap().chars().filter(char::is_ascii_digit).count();
    let e = format!("{x:.p$e}", p = n - 1); // e.g. "-1.2345e2"
    let (mant, exp) = e.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let (sign, mant) = mant.strip_prefix('-').map_or(("", mant), |m| ("-", m));
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let n = digits.len() as i32;
    if (-4..16).contains(&exp) {
        if exp >= n - 1 {
            format!("{sign}{digits}{}.0", "0".repeat((exp - (n - 1)) as usize))
        } else if exp >= 0 {
            format!("{sign}{}.{}", &digits[..=exp as usize], &digits[exp as usize + 1..])
        } else {
            format!("{sign}0.{}{digits}", "0".repeat((-exp - 1) as usize))
        }
    } else {
        let frac = if n > 1 { format!(".{}", &digits[1..]) } else { String::new() };
        format!("{sign}{}{frac}e{}{:02}", &digits[..1], if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

/// Python's `"%.*f"`: as Rust's, but nan/inf spelled as Python spells them.
pub fn fixed(x: f64, d: usize) -> String {
    if x.is_nan() {
        "nan".into()
    } else if x.is_infinite() {
        (if x > 0.0 { "inf" } else { "-inf" }).into()
    } else {
        format!("{x:.d$}")
    }
}

fn str_repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut o = String::with_capacity(s.len() + 2);
    o.push(q);
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if c == q => {
                o.push('\\');
                o.push(c);
            }
            c if (c as u32) < 0x20 || (0x7f..0xa0).contains(&(c as u32)) => {
                let _ = write!(o, "\\x{:02x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push(q);
    o
}

fn json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            c => {
                let mut b = [0u16; 2];
                for u in c.encode_utf16(&mut b) {
                    let _ = write!(out, "\\u{u:04x}");
                }
            }
        }
    }
    out.push('"');
}

impl<'de> Deserialize<'de> for Py {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Py, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Py;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON")
            }
            fn visit_unit<E>(self) -> Result<Py, E> {
                Ok(Py::None)
            }
            fn visit_none<E>(self) -> Result<Py, E> {
                Ok(Py::None)
            }
            fn visit_bool<E>(self, b: bool) -> Result<Py, E> {
                Ok(Py::Bool(b))
            }
            fn visit_i64<E>(self, i: i64) -> Result<Py, E> {
                Ok(Py::Int(i as i128))
            }
            fn visit_u64<E>(self, i: u64) -> Result<Py, E> {
                Ok(Py::Int(i as i128))
            }
            fn visit_f64<E>(self, f: f64) -> Result<Py, E> {
                Ok(Py::Float(f))
            }
            fn visit_str<E>(self, s: &str) -> Result<Py, E> {
                Ok(Py::Str(s.to_string()))
            }
            fn visit_string<E>(self, s: String) -> Result<Py, E> {
                Ok(Py::Str(s))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Py, A::Error> {
                let mut v = vec![];
                while let Some(x) = a.next_element()? {
                    v.push(x);
                }
                Ok(Py::List(v))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Py, A::Error> {
                let mut v: Vec<(String, Py)> = vec![];
                while let Some((k, x)) = a.next_entry::<String, Py>()? {
                    // a repeated key keeps its first place and its last value, as in a Python dict
                    match v.iter_mut().find(|(key, _)| *key == k) {
                        Some(e) => e.1 = x,
                        None => v.push((k, x)),
                    }
                }
                Ok(Py::Dict(v))
            }
        }
        d.deserialize_any(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_reprs() {
        for (x, s) in [(0.1, "0.1"), (5.0, "5.0"), (-0.0, "-0.0"), (1e16, "1e+16"), (1.5e16, "1.5e+16"), (1e-5, "1e-05"),
                       (0.0001, "0.0001"), (123456789.125, "123456789.125"), (1e15, "1000000000000000.0"), (1.234e-7, "1.234e-07"),
                       (1712345678.9012345, "1712345678.9012344"), (3.01636505126953125, "3.0163650512695312"), (f64::NAN, "nan")] {
            assert_eq!(float_repr(x), s);
        }
        assert_eq!(fixed(2.5, 0), "2");
        assert_eq!(fixed(f64::NAN, 2), "nan");
    }

    #[test]
    fn repr_and_dumps() {
        let v = Py::parse(r#"{"b": 1, "a": [1.0, null, true, "it's", "–"], "c": {}}"#).unwrap();
        assert_eq!(v.repr(), r#"{'b': 1, 'a': [1.0, None, True, "it's", '–'], 'c': {}}"#);
        assert_eq!(v.dumps(None), r#"{"b": 1, "a": [1.0, null, true, "it's", "\u2013"], "c": {}}"#);
        assert_eq!(v.dumps(Some(1)), "{\n \"b\": 1,\n \"a\": [\n  1.0,\n  null,\n  true,\n  \"it's\",\n  \"\\u2013\"\n ],\n \"c\": {}\n}");
        assert!(!v.at("c").truthy() && v.at("missing").is_none() && v.at("a").idx(2).truthy());
    }
}
