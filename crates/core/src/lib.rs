//! AetherRV shared library.
//!
//! * [`config`]   the TOML configuration contract (`/etc/aetherrv/aetherrv.toml`) and the secrets file
//! * [`txgate`]   the single choke point for anything that changes a device (deny by default)
//! * [`state`]    the state publisher: dotted keys → JSON values with the publisher's timestamp
//! * [`bus`]      the MQTT bus client (Mosquitto on a Unix socket): state, module health, commands
//! * [`command`]  the command contract (request/reply, TTL, at-most-once, explicit results)
//! * [`cmdlog`]   the `command_log` table writer (works with the bus down)
//! * [`clock`]    wall clock and `CLOCK_BOOTTIME`
//! * [`notify`]   systemd readiness and watchdog
//! * [`pyfmt`]    Python `str()` / `repr()` formatting of JSON values, for log rows identical to the reference

pub mod admin;
pub mod bus;
pub mod clock;
pub mod cmdlog;
pub mod command;
pub mod config;
pub mod logging;
pub mod notify;
pub mod num;
pub mod pyfmt;
pub mod state;
pub mod txgate;

#[cfg(any(test, feature = "testutil"))]
pub mod testutil;

pub use serde_json::{Value, json};
