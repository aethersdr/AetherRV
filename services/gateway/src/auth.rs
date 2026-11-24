//! Who may control the van (Python `settings.RemoteAuth`): the touchscreen (loopback); the owner's own Tailscale
//! devices, or with `tailnet = "all"` every tailnet peer; and browsers in the configured `control_subnets`. Everyone
//! else is view-only. A Tailscale peer is identified by asking tailscaled (via the root helper's `tailscale_whois`)
//! who owns the source address, and the connection must have arrived on this console's own Tailscale address.
//! Results are cached 60 s.

use aetherrv_core::command::BoxFuture;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The root helper as the gateway uses it (a fake in tests).
pub trait Admin: Send + Sync {
    fn call(&self, verb: &str, args: Value, timeout: Duration) -> BoxFuture<Result<Value, String>>;
}

impl<T: Admin + ?Sized> Admin for std::sync::Arc<T> {
    fn call(&self, verb: &str, args: Value, timeout: Duration) -> BoxFuture<Result<Value, String>> {
        (**self).call(verb, args, timeout)
    }
}

impl Admin for aetherrv_core::admin::AdminClient {
    fn call(&self, verb: &str, args: Value, timeout: Duration) -> BoxFuture<Result<Value, String>> {
        let (c, verb) = (self.clone(), verb.to_string());
        Box::pin(async move { aetherrv_core::admin::AdminClient::call(&c, &verb, args, timeout).await.map_err(|e| e.to_string()) })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Net {
    addr: IpAddr,
    prefix: u8,
}

impl Net {
    pub fn parse(s: &str) -> Option<Net> {
        let (a, p) = s.split_once('/').unwrap_or((s, ""));
        let addr: IpAddr = a.trim().parse().ok()?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = if p.is_empty() { max } else { p.trim().parse().ok().filter(|x| *x <= max)? };
        Some(Net { addr, prefix })
    }

    /// strict=False: host bits are ignored (Python `ip_network(..., strict=False)`).
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip) {
            (IpAddr::V4(n), IpAddr::V4(a)) => {
                let m = if self.prefix == 0 { 0 } else { u32::MAX << (32 - self.prefix as u32) };
                u32::from(n) & m == u32::from(a) & m
            }
            (IpAddr::V6(n), IpAddr::V6(a)) => {
                let m = if self.prefix == 0 { 0 } else { u128::MAX << (128 - self.prefix as u32) };
                u128::from(n) & m == u128::from(a) & m
            }
            _ => false,
        }
    }
}

pub const TS_V4: &str = "100.64.0.0/10";
pub const TS_V6: &str = "fd7a:115c:a1e0::/48";

fn parse_ip(s: &str) -> Option<IpAddr> {
    let s = s.split('%').next().unwrap_or(s);
    let s = s.strip_prefix("::ffff:").filter(|x| x.contains('.')).unwrap_or(s);
    s.parse().ok()
}

type Cached = (Instant, Option<String>, Vec<String>);

pub struct RemoteAuth {
    pub admin: Box<dyn Admin>,
    pub ttl: Duration,
    pub subnets: Vec<Net>,
    pub tailnet_all: bool,
    cache: Mutex<HashMap<String, Cached>>,
}

impl RemoteAuth {
    pub fn new(admin: Box<dyn Admin>, control_subnets: &[String], tailnet: &str) -> Self {
        RemoteAuth {
            admin,
            ttl: Duration::from_secs(60),
            subnets: control_subnets.iter().filter_map(|s| Net::parse(s)).collect(),
            tailnet_all: tailnet.eq_ignore_ascii_case("all"),
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn is_ts(ip: IpAddr) -> bool {
        Net::parse(TS_V4).unwrap().contains(ip) || Net::parse(TS_V6).unwrap().contains(ip)
    }

    /// `kiosk`, `lan:<ip>`, `tailscale:<login>@<node>`, or `None` (view-only).
    pub async fn who(&self, remote: Option<&str>, local_ip: Option<&str>) -> Option<String> {
        let remote = remote?;
        let ip = parse_ip(remote)?;
        if ip.is_loopback() {
            return Some("kiosk".into());
        }
        if self.subnets.iter().any(|n| n.contains(ip)) {
            return Some(format!("lan:{ip}"));
        }
        if !Self::is_ts(ip) {
            return None;
        }
        let key = ip.to_string();
        let hit = self.cache.lock().unwrap().get(&key).cloned().filter(|(t, _, _)| t.elapsed() < self.ttl);
        let (who, self_ips) = match hit {
            Some((_, w, s)) => (w, s),
            None => {
                let r = match self.admin.call("tailscale_whois", json!({"ip": key}), Duration::from_secs(15)).await {
                    Ok(r) => r,
                    Err(_) => return None,
                };
                let ok = r.get("ok").and_then(Value::as_bool).unwrap_or(false);
                let known = r.get("known") != Some(&Value::Bool(false));
                let owner = r.get("owner").and_then(Value::as_bool).unwrap_or(false);
                let peer = ok && known && (owner || self.tailnet_all);
                let s = |k: &str| r.get(k).map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).unwrap_or_else(|| "None".into());
                let who = peer.then(|| format!("tailscale:{}@{}", s("login"), s("node")));
                let self_ips: Vec<String> =
                    r.get("self_ips").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
                self.cache.lock().unwrap().insert(key, (Instant::now(), who.clone(), self_ips.clone()));
                (who, self_ips)
            }
        };
        // must have come in on this console's own Tailscale address (not a LAN path with a 100.x source)
        if who.is_some()
            && let Some(l) = local_ip
            && !self_ips.is_empty()
        {
            let l = l.split('%').next().unwrap_or(l);
            let l = l.strip_prefix("::ffff:").filter(|x| x.contains('.')).unwrap_or(l);
            if !self_ips.iter().any(|s| s == l) {
                return None;
            }
        }
        who
    }
}
