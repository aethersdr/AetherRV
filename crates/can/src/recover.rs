//! Link recovery: a local CAN port that stays down (`IFF_UP` clear) for `down_s` gets its SPI driver rebound by the
//! root helper (verb `can_rebind`), at most once per `every_s`, and the bus reopens. Seen once: after a reboot with
//! the FD HAT stacked, hat1's MCP2515 wouldn't enter config mode until its driver was rebound.

use crate::bus::{BoxFut, CanBus};
use aetherrv_core::state::StateStore;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// The ports recovery may rebind.
pub const LOCAL_PORTS: [&str; 4] = ["hat0", "hat1", "fd0", "fd1"];

pub trait Recoverable: Send + Sync {
    fn bus_name(&self) -> String;
    fn channel(&self) -> String;
    fn interface(&self) -> &'static str;
    fn link_up(&self) -> bool;
    fn reopen(&self);
}

impl Recoverable for CanBus {
    fn bus_name(&self) -> String {
        self.name.clone()
    }
    fn channel(&self) -> String {
        self.channel.clone()
    }
    fn interface(&self) -> &'static str {
        CanBus::interface(self)
    }
    fn link_up(&self) -> bool {
        CanBus::link_up(self)
    }
    fn reopen(&self) {
        CanBus::reopen(self)
    }
}

pub type RebindFn = Arc<dyn Fn(String) -> BoxFut<anyhow::Result<()>> + Send + Sync>;

pub struct Timing {
    pub down_s: f64,
    pub every_s: f64,
    pub poll_s: f64,
    pub settle_s: f64,
}

impl Default for Timing {
    fn default() -> Self {
        Timing { down_s: 30.0, every_s: 300.0, poll_s: 10.0, settle_s: 2.0 }
    }
}

pub async fn recover_links(buses: Vec<Arc<dyn Recoverable>>, rebind: RebindFn, state: Option<Arc<StateStore>>, t: Timing) {
    let mut down_since: HashMap<String, f64> = HashMap::new();
    let mut tried: HashMap<String, f64> = HashMap::new();
    loop {
        tokio::time::sleep(Duration::from_secs_f64(t.poll_s)).await;
        let now = aetherrv_core::clock::now_s();
        for b in &buses {
            if b.interface() != "socketcan" || !LOCAL_PORTS.contains(&b.channel().as_str()) {
                continue;
            }
            let name = b.bus_name();
            if b.link_up() {
                down_since.remove(&name);
                continue;
            }
            let since = *down_since.entry(name.clone()).or_insert(now);
            if now - since < t.down_s || now - tried.get(&name).copied().unwrap_or(0.0) < t.every_s {
                continue;
            }
            tried.insert(name.clone(), now);
            tracing::warn!("[{name}] {} down for {:.0} s: rebinding its driver", b.channel(), now - since);
            match rebind(b.channel()).await {
                Ok(()) => {
                    tokio::time::sleep(Duration::from_secs_f64(t.settle_s)).await;
                    b.reopen();
                    tracing::warn!("[{name}] driver rebound, reopening ({})", if b.link_up() { "up" } else { "still down" });
                }
                Err(e) => tracing::error!("[{name}] rebind failed: {e}"),
            }
            if let Some(s) = &state {
                s.update([(format!("bus.{name}.rebind_at"), json!(now)), (format!("bus.{name}.rebind_ok"), json!(b.link_up()))]);
            }
        }
    }
}
