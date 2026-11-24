//! The flat state the UI and `/api/state` see: today's key names and values (ARCHITECTURE.md §4.2, §4.4, §4.5).
//!
//! * every key of every healthy service, as published;
//! * a key whose owner is down (status down or heartbeat older than 6 s) reads as `null`;
//! * `tx.enabled` and `tx.blocked` are rebuilt as Python's dicts (`tx.enabled` from the config, like Python's
//!   `__main__`; `tx.blocked` from the services' `tx.blocked.<device>` counters);
//! * `svc.<service>.up` / `svc.<service>.age_s`: module health for the UI.

use aetherrv_core::bus::RemoteState;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::sync::Mutex;
use tokio::sync::broadcast;

pub struct Flat {
    values: Mutex<BTreeMap<String, Value>>,
    tx_enabled: Value,
    changes: broadcast::Sender<Vec<(String, Value)>>,
}

impl Flat {
    pub fn new(tx_enabled: Value) -> Self {
        let (changes, _) = broadcast::channel(4096);
        let f = Flat { values: Mutex::new(BTreeMap::new()), tx_enabled: tx_enabled.clone(), changes };
        f.values.lock().unwrap().insert("tx.enabled".into(), tx_enabled);
        f
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Vec<(String, Value)>> {
        self.changes.subscribe()
    }

    pub fn snapshot(&self) -> Map<String, Value> {
        self.values.lock().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        self.values.lock().unwrap().get(key).cloned()
    }

    /// The flat value of one key: `Some(v)` (null for a down owner) or `None` (gone).
    fn value_of(remote: &RemoteState, key: &str) -> Option<Value> {
        match remote.stamped(key, false) {
            Some((_, s)) => Some(s.v),
            None => remote.stamped(key, true).map(|_| Value::Null),
        }
    }

    fn apply(&self, changes: Vec<(String, Option<Value>)>) {
        let mut out = vec![];
        {
            let mut g = self.values.lock().unwrap();
            for (k, v) in changes {
                match v {
                    Some(v) => {
                        if g.get(&k) != Some(&v) {
                            g.insert(k.clone(), v.clone());
                            out.push((k, v));
                        }
                    }
                    None => {
                        if g.remove(&k).is_some() {
                            out.push((k, Value::Null));
                        }
                    }
                }
            }
        }
        if !out.is_empty() {
            let _ = self.changes.send(out);
        }
    }

    /// Recompute a set of keys (after a state update).
    pub fn refresh_keys(&self, remote: &RemoteState, keys: &[String]) {
        let mut ch: Vec<(String, Option<Value>)> = keys.iter().map(|k| (k.clone(), Self::value_of(remote, k))).collect();
        if keys.iter().any(|k| k.starts_with("tx.blocked.")) {
            ch.push(("tx.blocked".into(), Some(Self::tx_blocked(remote))));
        }
        self.apply(ch);
    }

    fn tx_blocked(remote: &RemoteState) -> Value {
        let mut m = Map::new();
        let mut all: Vec<(String, String, aetherrv_core::state::Stamped)> = remote.all();
        all.sort_by(|a, b| a.0.cmp(&b.0));
        for (k, _, s) in all {
            if let Some(dev) = k.strip_prefix("tx.blocked.")
                && s.v.as_u64().unwrap_or(0) > 0
            {
                m.insert(dev.to_string(), s.v.clone());
            }
        }
        Value::Object(m)
    }

    /// Recompute everything (owner health may have changed).
    pub fn refresh_all(&self, remote: &RemoteState) {
        let mut ch: Vec<(String, Option<Value>)> = vec![];
        let have: Vec<String> = self.values.lock().unwrap().keys().cloned().collect();
        let mut seen = std::collections::HashSet::new();
        for (k, _, _) in remote.all() {
            if seen.insert(k.clone()) {
                ch.push((k.clone(), Self::value_of(remote, &k)));
            }
        }
        for k in have {
            if !seen.contains(&k) && !k.starts_with("svc.") && k != "tx.enabled" && k != "tx.blocked" {
                ch.push((k, None));
            }
        }
        for (name, up, age) in remote.owners() {
            ch.push((format!("svc.{name}.up"), Some(json!(up))));
            ch.push((format!("svc.{name}.age_s"), Some(age.map(|a| json!(aetherrv_core::num::round1(a))).unwrap_or(Value::Null))));
        }
        ch.push(("tx.blocked".into(), Some(Self::tx_blocked(remote))));
        ch.push(("tx.enabled".into(), Some(self.tx_enabled.clone())));
        self.apply(ch);
    }
}
