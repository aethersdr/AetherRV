//! The configuration contract: `/etc/aetherrv/aetherrv.toml` and the secrets file it names.
//!
//! The Python services read the config with `dict.get(key, default)` everywhere, so a missing or extra key is never
//! an error. [`Cfg`] keeps that behaviour: it wraps the parsed document as JSON and every accessor takes a default.
//! Typed access is strict about *types* where safety depends on it (see [`Cfg::flag`]).

use serde_json::{Map, Value};
use std::path::Path;

/// A config node (the whole document or a section of it).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cfg(pub Value);

impl Cfg {
    pub fn empty() -> Self {
        Cfg(Value::Object(Map::new()))
    }

    /// Parse TOML text.
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let t: toml::Table = toml::from_str(text)?;
        Ok(Cfg(serde_json::to_value(t)?))
    }

    /// Load a TOML file.
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let p = path.as_ref();
        let text = std::fs::read_to_string(p).map_err(|e| anyhow::anyhow!("cannot read {}: {e}", p.display()))?;
        Self::parse(&text).map_err(|e| anyhow::anyhow!("{}: {e}", p.display()))
    }

    /// The node at a dotted path (`"device.espar"`), or an empty table. Each segment is one TOML key.
    pub fn at(&self, path: &str) -> Cfg {
        let mut cur = &self.0;
        for seg in path.split('.').filter(|s| !s.is_empty()) {
            match cur.get(seg) {
                Some(v) => cur = v,
                None => return Cfg::empty(),
            }
        }
        Cfg(cur.clone())
    }

    /// A direct child (one key, which may contain dots).
    pub fn child(&self, key: &str) -> Cfg {
        self.0.get(key).cloned().map(Cfg).unwrap_or_else(Cfg::empty)
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    pub fn has(&self, key: &str) -> bool {
        self.0.get(key).is_some_and(|v| !v.is_null())
    }

    pub fn is_empty(&self) -> bool {
        match &self.0 {
            Value::Object(m) => m.is_empty(),
            Value::Null => true,
            _ => false,
        }
    }

    /// String value, or the default. Numbers are not converted.
    pub fn str(&self, key: &str, default: &str) -> String {
        match self.0.get(key) {
            Some(Value::String(s)) => s.clone(),
            _ => default.to_string(),
        }
    }

    pub fn opt_str(&self, key: &str) -> Option<String> {
        match self.0.get(key) {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        }
    }

    /// Number (int or float), or the default (Python `float(d.get(k, default))`).
    pub fn f64(&self, key: &str, default: f64) -> f64 {
        self.0.get(key).and_then(Value::as_f64).unwrap_or(default)
    }

    pub fn opt_f64(&self, key: &str) -> Option<f64> {
        self.0.get(key).and_then(Value::as_f64)
    }

    /// Integer (a float is truncated, as Python's `int()`), or the default.
    pub fn i64(&self, key: &str, default: i64) -> i64 {
        match self.0.get(key) {
            Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).unwrap_or(default),
            _ => default,
        }
    }

    pub fn opt_i64(&self, key: &str) -> Option<i64> {
        match self.0.get(key) {
            Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
            _ => None,
        }
    }

    /// Python truthiness of `d.get(key, default)` for ordinary options (`enabled`, `capture`, ...).
    pub fn bool(&self, key: &str, default: bool) -> bool {
        match self.0.get(key) {
            None | Some(Value::Null) => default,
            Some(v) => truthy(v),
        }
    }

    /// A **safety flag** (`tx_enabled`, `diag_tx`): true only for a TOML boolean `true`. Python's
    /// `bool(v.get("tx_enabled"))` would also accept e.g. the string "false"; here anything but a real boolean is
    /// off (the safe direction), and `aetherrv-migrate` warns about it.
    pub fn flag(&self, key: &str) -> bool {
        matches!(self.0.get(key), Some(Value::Bool(true)))
    }

    /// An array of tables (`[[device.n2k.switch]]`), or empty.
    pub fn list(&self, key: &str) -> Vec<Cfg> {
        match self.0.get(key) {
            Some(Value::Array(a)) => a.iter().cloned().map(Cfg).collect(),
            _ => Vec::new(),
        }
    }

    /// An array of strings, or empty.
    pub fn str_list(&self, key: &str) -> Vec<String> {
        match self.0.get(key) {
            Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
            _ => Vec::new(),
        }
    }

    /// The (key, node) pairs of a table, in file order.
    pub fn entries(&self) -> Vec<(String, Cfg)> {
        match &self.0 {
            Value::Object(m) => m.iter().map(|(k, v)| (k.clone(), Cfg(v.clone()))).collect(),
            _ => Vec::new(),
        }
    }
}

/// Python truthiness of a JSON value.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

/// The default config path.
pub const DEFAULT_CONFIG: &str = "/etc/aetherrv/aetherrv.toml";

/// Secrets live in a separate root:aetherrv 0640 file, never in the world-readable config. Missing or unreadable →
/// empty (logged), exactly as Python's `load_secrets`.
pub fn load_secrets(cfg: &Cfg) -> Cfg {
    let path = cfg.str("secrets_file", "/etc/aetherrv/secrets.toml");
    match std::fs::read_to_string(&path) {
        Ok(t) => Cfg::parse(&t).unwrap_or_else(|e| {
            tracing::warn!("cannot read secrets file {path}: {e}");
            Cfg::empty()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Cfg::empty(),
        Err(e) => {
            tracing::warn!("cannot read secrets file {path}: {e}");
            Cfg::empty()
        }
    }
}

/// Command-line handling shared by every service: `-c <config>` (default [`DEFAULT_CONFIG`]) and `-v`.
pub struct Args {
    pub config: String,
    pub verbose: bool,
    pub rest: Vec<String>,
}

impl Args {
    pub fn parse() -> Self {
        let mut config = DEFAULT_CONFIG.to_string();
        let mut verbose = false;
        let mut rest = Vec::new();
        let mut it = std::env::args().skip(1);
        while let Some(a) = it.next() {
            match a.as_str() {
                "-c" | "--config" => {
                    if let Some(v) = it.next() {
                        config = v;
                    }
                }
                "-v" | "--verbose" => verbose = true,
                _ => rest.push(a),
            }
        }
        Args { config, verbose, rest }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"
        capture = true
        [web]
        port = 8080
        control_subnets = ["192.0.2.0/24"]
        [device.espar]
        kind = "espar"
        tx_enabled = true
        diag_tx = "true"
        diag_interval_s = 1
        [[device.n2k.switch]]
        channel = 1
        name = "Cabin lights"
        [[device.n2k.switch]]
        channel = 2
    "#;

    #[test]
    fn lenient_access_like_python_get() {
        let c = Cfg::parse(DOC).unwrap();
        assert!(c.bool("capture", false));
        assert_eq!(c.at("web").i64("port", 0), 8080);
        assert_eq!(c.at("web").str_list("control_subnets"), vec!["192.0.2.0/24"]);
        assert_eq!(c.at("web").str("host", "0.0.0.0"), "0.0.0.0");
        assert_eq!(c.at("nope.deeper").f64("x", 2.5), 2.5);
        let e = c.at("device.espar");
        assert_eq!(e.f64("diag_interval_s", 5.0), 1.0);
        assert_eq!(c.at("device.n2k").list("switch").len(), 2);
        assert_eq!(c.at("device.n2k").list("switch")[0].str("name", ""), "Cabin lights");
        assert_eq!(c.at("device").entries().len(), 2);
    }

    #[test]
    fn safety_flags_need_a_real_boolean() {
        let c = Cfg::parse(DOC).unwrap();
        let e = c.at("device.espar");
        assert!(e.flag("tx_enabled"));
        assert!(!e.flag("diag_tx"), "a string is never a transmit permission");
        assert!(!e.flag("missing"));
    }
}
