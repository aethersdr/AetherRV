//! Logging: journald under systemd, stderr otherwise. `RUST_LOG` overrides the level.

use tracing_subscriber::{EnvFilter, prelude::*};

pub fn init(service: &str, verbose: bool) {
    let default = if verbose { "debug" } else { "info" };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    let under_journal = std::env::var_os("JOURNAL_STREAM").is_some();
    let registry = tracing_subscriber::registry().with(filter);
    if under_journal && let Ok(j) = tracing_journald::layer() {
        let _ = registry.with(j.with_syslog_identifier(service.to_string())).try_init();
        return;
    }
    let _ = registry.with(tracing_subscriber::fmt::layer().with_target(false).with_writer(std::io::stderr)).try_init();
}
