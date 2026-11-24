//! Device simulators: wire-level stand-ins for the van's devices, run as plain processes (no containers) or in
//! tests. Never pointed at a real device.

pub mod cerbo;
pub mod heater;
pub mod peplink;
pub mod starlink;
