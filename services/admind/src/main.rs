//! `aetherrv-admind [socket-path]`: socket-activated by `aetherrv-admind.socket` (fd 3), or a manual run on a path
//! for testing. Exits after `AETHERRV_ADMIN_IDLE` seconds (default 600) without a connection when socket-activated.

use aetherrv_admind::{Helper, RealSys, serve_conn};
use std::os::fd::FromRawFd;
use std::os::unix::net::UnixListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    let activated = std::env::var("LISTEN_FDS").as_deref() == Ok("1");
    let listener = if activated {
        // SAFETY: systemd passes the listening socket as fd 3.
        unsafe { UnixListener::from_raw_fd(3) }
    } else {
        let path = std::env::args().nth(1).unwrap_or_else(|| "/run/aetherrv-admin.sock".into());
        let _ = std::fs::remove_file(&path);
        UnixListener::bind(&path)?
    };
    let idle: f64 = std::env::var("AETHERRV_ADMIN_IDLE").ok().and_then(|v| v.parse().ok()).unwrap_or(600.0);
    listener.set_nonblocking(true)?;
    let h = Arc::new(Helper::new(Box::new(RealSys)));
    let start = Instant::now();
    let last = Arc::new(AtomicU64::new(0));
    let busy = Arc::new(AtomicU64::new(0));
    loop {
        match listener.accept() {
            Ok((s, _)) => {
                let _ = s.set_nonblocking(false);
                last.store(start.elapsed().as_millis() as u64, Ordering::SeqCst);
                let (h, busy) = (h.clone(), busy.clone());
                busy.fetch_add(1, Ordering::SeqCst);
                std::thread::spawn(move || {
                    serve_conn(&h, s);
                    busy.fetch_sub(1, Ordering::SeqCst);
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
                let since = start.elapsed().as_secs_f64() - last.load(Ordering::SeqCst) as f64 / 1000.0;
                // socket-activated: exit when idle; systemd starts us again on the next connection
                if activated && since > idle && busy.load(Ordering::SeqCst) == 0 {
                    return Ok(());
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
}
