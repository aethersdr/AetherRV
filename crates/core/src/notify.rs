//! systemd readiness and watchdog. The watchdog only covers a hung process; it is NOT a device-level failsafe.

use std::time::Duration;

/// Tell systemd we're ready (no-op outside systemd).
pub fn ready() {
    let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);
}

/// Tell systemd we're stopping.
pub fn stopping() {
    let _ = sd_notify::notify(&[sd_notify::NotifyState::Stopping]);
}

/// The watchdog interval systemd asked for, if any (`WatchdogSec=`).
pub fn watchdog_interval() -> Option<Duration> {
    let usec: u64 = std::env::var("WATCHDOG_USEC").ok()?.parse().ok()?;
    if usec == 0 { None } else { Some(Duration::from_micros(usec)) }
}

/// Feed the watchdog once.
pub fn kick() {
    let _ = sd_notify::notify(&[sd_notify::NotifyState::Watchdog]);
}

/// Feed the watchdog from the tokio runtime (services whose liveness is "the runtime runs"). The Espar service
/// feeds it from its control loop instead.
pub fn spawn_runtime_watchdog() {
    if let Some(iv) = watchdog_interval() {
        tokio::spawn(async move {
            let mut t = tokio::time::interval(iv / 3);
            loop {
                t.tick().await;
                kick();
            }
        });
    }
}
