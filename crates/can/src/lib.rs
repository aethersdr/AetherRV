//! AetherRV CAN transport.
//!
//! One process owns each CAN port (ARCHITECTURE.md §2): decode, control, diagnostics and capture share one socket,
//! because SocketCAN loops a socket's frames back to every other socket on the interface.

pub mod bus;
pub mod capture;
pub mod failover;
pub mod frame;
pub mod open;
pub mod raw;
pub mod recover;
pub mod socketcand;

pub use bus::{BusStats, CanBus, Handler, SendError};
pub use failover::FailoverBus;
pub use frame::CanFrame;

use aetherrv_core::state::StateStore;
use serde_json::json;
use std::sync::Arc;

/// Publish `bus.<name>.*` once (Python `publish_bus_stats`, per bus).
pub fn publish_stats(b: &CanBus, state: &StateStore) {
    let st = b.stats();
    state.update([
        (format!("bus.{}.connected", b.name), json!(b.link_up())),
        (format!("bus.{}.channel", b.name), json!(b.channel)),
        (format!("bus.{}.frames", b.name), json!(st.frames)),
        (format!("bus.{}.errors", b.name), json!(st.errors)),
        (format!("bus.{}.rate", b.name), json!(aetherrv_core::num::round1(b.rate()))),
        (format!("bus.{}.ids", b.name), json!(st.ids.len())),
    ]);
}

/// Publish the stats of these buses every second.
pub async fn run_stats(buses: Vec<Arc<CanBus>>, state: Arc<StateStore>) {
    loop {
        for b in &buses {
            publish_stats(b, &state);
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}
