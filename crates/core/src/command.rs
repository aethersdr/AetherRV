//! The command contract (ARCHITECTURE.md §4.6).
//!
//! A command is a request/reply over the bus with a correlation id and a TTL. Every result is explicit: `ok`,
//! `refused` (nothing was sent; the reason is shown to the user) or `unverified` (something may have been sent; not
//! retried). Python's command-log outcomes map onto these three ([`Outcome::result`]).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;

/// The three results the UI distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResultKind {
    Ok,
    Refused,
    Unverified,
}

/// A command's answer: the result and the text shown to the user (the Python `detail`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub result: ResultKind,
    pub detail: String,
}

impl Reply {
    pub fn ok(d: impl Into<String>) -> Self {
        Reply { result: ResultKind::Ok, detail: d.into() }
    }
    pub fn refused(d: impl Into<String>) -> Self {
        Reply { result: ResultKind::Refused, detail: d.into() }
    }
    pub fn unverified(d: impl Into<String>) -> Self {
        Reply { result: ResultKind::Unverified, detail: d.into() }
    }
    /// Python's `{"ok": ...}`.
    pub fn is_ok(&self) -> bool {
        self.result == ResultKind::Ok
    }
    /// The dict the Python `command()` returned (`ok`, `detail`), plus `result`.
    pub fn to_json(&self) -> Value {
        serde_json::json!({"ok": self.is_ok(), "detail": self.detail, "result": self.result})
    }
}

/// Python command-log outcomes (`command_log.outcome`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Noop,
    Rejected,
    Blocked,
    /// `failed` before anything was sent (no fresh status, device unreachable).
    FailedNotSent,
    /// `failed` on or after the send itself.
    FailedSent,
    Unverified,
    Error,
    /// Automatic actions and their own outcomes (`stopped`, `off`, `safety-off`, `resume`, ...).
    Other(&'static str),
}

impl Outcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Noop => "noop",
            Outcome::Rejected => "rejected",
            Outcome::Blocked => "blocked",
            Outcome::FailedNotSent | Outcome::FailedSent => "failed",
            Outcome::Unverified => "unverified",
            Outcome::Error => "error",
            Outcome::Other(s) => s,
        }
    }

    /// The mapping in ARCHITECTURE.md §4.6.
    pub fn result(&self) -> ResultKind {
        match self {
            Outcome::Ok | Outcome::Noop => ResultKind::Ok,
            Outcome::Rejected | Outcome::Blocked | Outcome::FailedNotSent => ResultKind::Refused,
            Outcome::FailedSent | Outcome::Unverified | Outcome::Error | Outcome::Other(_) => ResultKind::Unverified,
        }
    }
}

/// A request on `aetherrv/v1/cmd/<device>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    pub cmd: String,
    #[serde(default)]
    pub value: Value,
    /// Who asked: `kiosk`, `lan:<ip>`, `tailscale:<login>@<node>`, `auto:low-battery`, ...
    pub client: String,
    /// The requester's wall clock when it sent the request (ms since the epoch).
    pub sent_ms: i64,
    /// Refuse when older than this (ms).
    #[serde(default = "default_ttl")]
    pub ttl_ms: i64,
}

pub const DEFAULT_TTL_MS: i64 = 3000;

fn default_ttl() -> i64 {
    DEFAULT_TTL_MS
}

/// The text for a request that arrived after its TTL.
pub const LATE_DETAIL: &str = "the request arrived too late; nothing sent";

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// A device's command entry point (Python `adapter.command(cmd, value, client)`).
pub trait CommandHandler: Send + Sync + 'static {
    fn handle(&self, cmd: String, value: Value, client: String) -> BoxFuture<Reply>;

    /// Commands that run even when late (the safe direction, e.g. a dimmer `hold` release).
    fn ttl_exempt(&self, _cmd: &str, _value: &Value) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_mapping() {
        assert_eq!(Outcome::Noop.result(), ResultKind::Ok);
        assert_eq!(Outcome::Blocked.result(), ResultKind::Refused);
        assert_eq!(Outcome::FailedNotSent.result(), ResultKind::Refused);
        assert_eq!(Outcome::FailedSent.result(), ResultKind::Unverified);
        assert_eq!(Outcome::FailedSent.as_str(), "failed");
        let r = Reply::refused("x").to_json();
        assert_eq!(r["ok"], false);
        assert_eq!(r["result"], "refused");
    }

    #[test]
    fn request_defaults() {
        let r: Request = serde_json::from_str(r#"{"id":"a","cmd":"set","client":"kiosk","sent_ms":1}"#).unwrap();
        assert_eq!(r.ttl_ms, DEFAULT_TTL_MS);
        assert!(r.value.is_null());
    }
}
