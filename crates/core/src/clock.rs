//! Clocks. `now_s()` is Python's `time.time()`. `boottime()` is `CLOCK_BOOTTIME`: monotonic, counts suspend, and is
//! shared by every process of one boot, so a timer persisted by one process can be continued by the next one even
//! if the wall clock jumps (GPS time, NTP).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch (Python `time.time()`).
pub fn now_s() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// `CLOCK_BOOTTIME` since boot.
pub fn boottime() -> Duration {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: clock_gettime writes into the timespec we own; CLOCK_BOOTTIME exists on every Linux we run on.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) };
    if rc != 0 {
        return Duration::ZERO;
    }
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}

/// `CLOCK_BOOTTIME` in seconds (f64).
pub fn boottime_s() -> f64 {
    boottime().as_secs_f64()
}

/// The kernel's id of this boot (changes at every reboot).
pub fn boot_id() -> String {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id").map(|s| s.trim().to_string()).unwrap_or_default()
}

/// Local calendar day `YYYY-MM-DD` for a Unix time, using the process's time zone (Python
/// `time.strftime("%Y-%m-%d", time.localtime(t))`).
pub fn local_day(t: f64) -> String {
    let secs = t as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: localtime_r fills the tm we own.
    unsafe extern "C" {
        fn tzset();
    }
    unsafe {
        tzset();
        libc::localtime_r(&secs, &mut tm);
    }
    format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks_are_sane() {
        assert!(now_s() > 1.7e9);
        assert!((now_ms() as f64 / 1000.0 - now_s()).abs() < 1.0);
        let a = boottime();
        let b = boottime();
        assert!(b >= a && a > Duration::ZERO);
        assert_eq!(boot_id().len(), 36);
        assert_eq!(local_day(now_s()).len(), 10);
    }
}
