//! Rixens MCS7 hydronic heat: the controller's Local HTTP API and its CAN frames.
//!
//! * [`http`]: `status.xml` → [`http::Status`] (whitelisted fields only; Wi-Fi secrets are never parsed),
//!   the `climate.rixens.*` state, the command allowlist ([`http::plan`]) and the daily fuel total
//!   (port of `src/aetherrv/adapters/rixens_http.py`, without the HTTP client and the command flow).
//! * [`can`]: the MCS7 CAN decoder, the (never sent) command encoders and the CAN adapter's state
//!   (port of `src/aetherrv/adapters/rixens.py`).
//!
//! Pure: no I/O, no clock. Time comes in as `now` (Unix seconds, Python `time.time()`); the local day for
//! the fuel total comes in as a `%Y-%m-%d` string.

pub mod can;
pub mod http;
mod py;
pub mod xml;

pub use http::{FanSpeed, FuelAccount, Plan, PlanError, Status, StatusError, Verify, parse_status, plan, to_state};
