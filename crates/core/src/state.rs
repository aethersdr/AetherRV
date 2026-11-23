//! State: dotted keys → JSON values, each with the time it was last `set()`.
//!
//! [`StateStore`] is a service's own state (Python's `State`): `set()` an unchanged value refreshes its timestamp,
//! exactly like `State.set()` refreshes `age()`. What changed is published at once; an unchanged key whose
//! timestamp was refreshed is republished at most every `refresh_ms` (10 s), so a consumer's `age` is never more
//! than that too old (ARCHITECTURE.md §4.3).
//!
//! [`StateView`] is what service logic reads (Python's `state.get()` / `state.age()`), implemented by the local store
//! and by the bus's view of other services' keys.

use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;
use tokio::sync::Notify;

/// A value with the publisher's timestamp (ms since the epoch).
#[derive(Debug, Clone, PartialEq)]
pub struct Stamped {
    pub v: Value,
    pub t: i64,
}

/// Read access to state (local, remote or merged).
pub trait StateView: Send + Sync {
    /// The value, or `None` when the key is missing or unknown. A stored JSON `null` is `Some(Null)`.
    fn get(&self, key: &str) -> Option<Value>;
    /// Seconds since the key was last set (`None` when missing).
    fn age(&self, key: &str) -> Option<f64>;
    /// Every known key (for rules that enumerate, e.g. `bus.*`).
    fn keys(&self) -> Vec<String>;
}

/// Python-style helpers over a [`StateView`].
pub trait StateExt: StateView {
    /// `state.get(k)` treating JSON null as missing.
    fn val(&self, key: &str) -> Option<Value> {
        self.get(key).filter(|v| !v.is_null())
    }
    /// A number that isn't a bool (Python `_num`).
    fn num(&self, key: &str) -> Option<f64> {
        match self.get(key) {
            Some(Value::Number(n)) => n.as_f64(),
            _ => None,
        }
    }
    /// Python truthiness of `state.get(k)`.
    fn truthy(&self, key: &str) -> bool {
        self.get(key).as_ref().is_some_and(crate::config::truthy)
    }
    /// `state.get(k) is True`.
    fn is_true(&self, key: &str) -> bool {
        matches!(self.get(key), Some(Value::Bool(true)))
    }
    /// `state.get(k) is False`.
    fn is_false(&self, key: &str) -> bool {
        matches!(self.get(key), Some(Value::Bool(false)))
    }
    fn string(&self, key: &str) -> Option<String> {
        match self.get(key) {
            Some(Value::String(s)) => Some(s),
            _ => None,
        }
    }
    fn list(&self, key: &str) -> Vec<Value> {
        match self.get(key) {
            Some(Value::Array(a)) => a,
            _ => Vec::new(),
        }
    }
}

impl<T: StateView + ?Sized> StateExt for T {}

#[derive(Debug, Clone)]
struct Entry {
    v: Value,
    t: i64,
    /// The `t` that was last handed to the publisher (0 = never).
    published_t: i64,
}

/// A service's own state.
pub struct StateStore {
    inner: Mutex<Inner>,
    notify: Notify,
    refresh_ms: i64,
}

#[derive(Default)]
struct Inner {
    values: HashMap<String, Entry>,
    order: Vec<String>,
    dirty: BTreeSet<String>,
    removed: BTreeSet<String>,
}

pub const REFRESH_MS: i64 = 10_000;

impl Default for StateStore {
    fn default() -> Self {
        Self::new()
    }
}

impl StateStore {
    pub fn new() -> Self {
        Self::with_refresh(REFRESH_MS)
    }

    pub fn with_refresh(refresh_ms: i64) -> Self {
        StateStore { inner: Mutex::new(Inner::default()), notify: Notify::new(), refresh_ms }
    }

    pub fn set(&self, key: &str, value: Value) {
        self.set_at(key, value, crate::clock::now_ms());
    }

    /// `set()` at an explicit time (tests drive time this way).
    pub fn set_at(&self, key: &str, value: Value, now_ms: i64) {
        let mut g = self.inner.lock().unwrap();
        let mut wake = false;
        match g.values.get_mut(key) {
            Some(e) if e.v == value => {
                e.t = now_ms;
                if now_ms - e.published_t >= self.refresh_ms {
                    wake = true;
                }
            }
            Some(e) => {
                e.v = value;
                e.t = now_ms;
                wake = true;
            }
            None => {
                g.values.insert(key.to_string(), Entry { v: value, t: now_ms, published_t: 0 });
                g.order.push(key.to_string());
                g.removed.remove(key);
                wake = true;
            }
        }
        if wake {
            g.dirty.insert(key.to_string());
            drop(g);
            self.notify.notify_one();
        }
    }

    pub fn update<I, K>(&self, values: I)
    where
        I: IntoIterator<Item = (K, Value)>,
        K: AsRef<str>,
    {
        let now = crate::clock::now_ms();
        for (k, v) in values {
            self.set_at(k.as_ref(), v, now);
        }
    }

    /// Forget a key entirely (its retained topic is cleared).
    pub fn remove(&self, key: &str) {
        let mut g = self.inner.lock().unwrap();
        if g.values.remove(key).is_some() {
            g.order.retain(|k| k != key);
            g.dirty.remove(key);
            g.removed.insert(key.to_string());
            drop(g);
            self.notify.notify_one();
        }
    }

    pub fn get_stamped(&self, key: &str) -> Option<Stamped> {
        self.inner.lock().unwrap().values.get(key).map(|e| Stamped { v: e.v.clone(), t: e.t })
    }

    /// Everything, in first-set order (Python dict order).
    pub fn snapshot(&self) -> Vec<(String, Stamped)> {
        let g = self.inner.lock().unwrap();
        g.order
            .iter()
            .filter_map(|k| g.values.get(k).map(|e| (k.clone(), Stamped { v: e.v.clone(), t: e.t })))
            .collect()
    }

    /// Keys to publish (and keys to clear), marking them as handed over.
    pub fn take_dirty(&self) -> (Vec<(String, Stamped)>, Vec<String>) {
        let mut g = self.inner.lock().unwrap();
        let dirty: Vec<String> = std::mem::take(&mut g.dirty).into_iter().collect();
        let removed: Vec<String> = std::mem::take(&mut g.removed).into_iter().collect();
        let mut out = Vec::with_capacity(dirty.len());
        for k in dirty {
            if let Some(e) = g.values.get_mut(&k) {
                e.published_t = e.t;
                out.push((k, Stamped { v: e.v.clone(), t: e.t }));
            }
        }
        (out, removed)
    }

    /// After a reconnect: everything is published again, with its original `t`.
    pub fn mark_all_dirty(&self) {
        let mut g = self.inner.lock().unwrap();
        let keys: Vec<String> = g.values.keys().cloned().collect();
        g.dirty.extend(keys);
        drop(g);
        self.notify.notify_one();
    }

    pub async fn changed(&self) {
        self.notify.notified().await
    }

    pub fn has_dirty(&self) -> bool {
        let g = self.inner.lock().unwrap();
        !g.dirty.is_empty() || !g.removed.is_empty()
    }

    pub fn contains(&self, key: &str) -> bool {
        self.inner.lock().unwrap().values.contains_key(key)
    }

    /// Plain values (Python `State.snapshot()`).
    pub fn values(&self) -> serde_json::Map<String, Value> {
        self.snapshot().into_iter().map(|(k, s)| (k, s.v)).collect()
    }
}

impl StateView for StateStore {
    fn get(&self, key: &str) -> Option<Value> {
        self.inner.lock().unwrap().values.get(key).map(|e| e.v.clone())
    }
    fn age(&self, key: &str) -> Option<f64> {
        let t = self.inner.lock().unwrap().values.get(key).map(|e| e.t)?;
        Some((crate::clock::now_ms() - t) as f64 / 1000.0)
    }
    fn keys(&self) -> Vec<String> {
        self.inner.lock().unwrap().order.clone()
    }
}

/// A local store over a remote view: keys this service owns come from `local`, everything else from `remote`.
pub struct Merged<'a> {
    pub local: &'a dyn StateView,
    pub remote: &'a dyn StateView,
}

impl StateView for Merged<'_> {
    fn get(&self, key: &str) -> Option<Value> {
        self.local.get(key).or_else(|| self.remote.get(key))
    }
    fn age(&self, key: &str) -> Option<f64> {
        self.local.age(key).or_else(|| self.remote.age(key))
    }
    fn keys(&self) -> Vec<String> {
        let mut k = self.local.keys();
        let have: std::collections::HashSet<String> = k.iter().cloned().collect();
        k.extend(self.remote.keys().into_iter().filter(|x| !have.contains(x)));
        k
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// py: tests/test_core.py::test_state_pubsub (a set of an unchanged value isn't a change; changes come out once,
    /// in order)
    #[test]
    fn test_state_pubsub() {
        let s = StateStore::new();
        s.set_at("a", json!(1), 1000);
        s.set_at("a", json!(1), 1001);
        assert_eq!(s.take_dirty().0, vec![("a".to_string(), Stamped { v: json!(1), t: 1001 })]);
        s.set_at("a", json!(2), 1002);
        assert_eq!(s.take_dirty().0, vec![("a".to_string(), Stamped { v: json!(2), t: 1002 })]);
        assert!(!s.has_dirty());
    }

    #[test]
    fn changes_publish_at_once_unchanged_values_refresh_at_most_every_10s() {
        let s = StateStore::new();
        s.set_at("a", json!(1), 1000);
        let (d, _) = s.take_dirty();
        assert_eq!(d, vec![("a".to_string(), Stamped { v: json!(1), t: 1000 })]);
        s.set_at("a", json!(1), 5000); // unchanged, 4 s later: age refreshed locally, not published
        assert!(!s.has_dirty());
        assert_eq!(s.get_stamped("a").unwrap().t, 5000);
        s.set_at("a", json!(1), 11_000); // 10 s after the last publish: republished with the new t
        let (d, _) = s.take_dirty();
        assert_eq!(d[0].1.t, 11_000);
        s.set_at("a", json!(2), 11_100); // a change goes out at once
        assert_eq!(s.take_dirty().0[0].1.v, json!(2));
    }

    #[test]
    fn reconnect_republishes_with_original_t_and_removals_clear() {
        let s = StateStore::new();
        s.set_at("a", json!("x"), 1000);
        s.set_at("b", json!(null), 2000);
        s.take_dirty();
        s.mark_all_dirty();
        let (mut d, _) = s.take_dirty();
        d.sort_by(|x, y| x.0.cmp(&y.0));
        assert_eq!(d[0].1.t, 1000);
        assert_eq!(d[1].1, Stamped { v: json!(null), t: 2000 });
        s.remove("a");
        assert_eq!(s.take_dirty().1, vec!["a".to_string()]);
        assert_eq!(s.keys(), vec!["b".to_string()]);
    }

    #[test]
    fn python_style_reads() {
        let s = StateStore::new();
        s.update([("n", json!(3.5)), ("b", json!(true)), ("z", json!(null)), ("l", json!([1]))]);
        assert_eq!(s.num("n"), Some(3.5));
        assert_eq!(s.num("b"), None, "a bool is not a number (Python _num)");
        assert!(s.is_true("b") && s.truthy("l") && !s.truthy("z"));
        assert_eq!(s.val("z"), None);
        assert!(s.age("n").unwrap() < 1.0);
        assert_eq!(s.keys(), vec!["n", "b", "z", "l"]);
    }
}
