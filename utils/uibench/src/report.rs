//! Reports from the page (`ui/src/bench/` with `?bench=1`, or the reference UI's injected script): a tiny HTTP
//! listener on 127.0.0.1 that accepts `navigator.sendBeacon` POSTs (text/plain, no CORS preflight) and keeps each
//! JSON body with its arrival time. Geometry (where the rail and the controls are), page marks and cross-check
//! timings arrive this way; nothing here is used as a frame time.

use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct Report {
    pub recv: u64,
    pub body: Value,
}

#[derive(Clone, Default)]
pub struct Reports(pub Arc<Mutex<Vec<Report>>>);

impl Reports {
    pub fn listen(port: u16) -> std::io::Result<Reports> {
        let l = std::net::TcpListener::bind(("127.0.0.1", port))?;
        let r = Reports::default();
        let r2 = r.clone();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let r3 = r2.clone();
                std::thread::spawn(move || handle(s, r3));
            }
        });
        Ok(r)
    }
    pub fn since(&self, t: u64) -> Vec<Report> {
        self.0.lock().unwrap().iter().filter(|r| r.recv >= t).cloned().collect()
    }
    /// The newest report (received at/after `t`) matching `pred`, waiting up to `ms`.
    pub fn wait(&self, t: u64, ms: u64, pred: impl Fn(&Value) -> bool) -> Option<Report> {
        let end = crate::mono_ns() + ms * 1_000_000;
        loop {
            if let Some(r) = self.0.lock().unwrap().iter().rev().find(|r| r.recv >= t && pred(&r.body)) {
                return Some(r.clone());
            }
            if crate::mono_ns() > end {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}

fn handle(s: std::net::TcpStream, r: Reports) {
    let recv = crate::mono_ns();
    let mut rd = BufReader::new(&s);
    let mut first = String::new();
    if rd.read_line(&mut first).is_err() {
        return;
    }
    let mut len = 0usize;
    let mut line = String::new();
    loop {
        line.clear();
        if rd.read_line(&mut line).is_err() || line.trim().is_empty() {
            break;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; len.min(4 << 20)];
    let _ = rd.read_exact(&mut body);
    if first.starts_with("POST")
        && let Ok(v) = serde_json::from_slice::<Value>(&body)
    {
        r.0.lock().unwrap().push(Report { recv, body: v });
    }
    let _ = (&s).write_all(
        b"HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: *\r\nAccess-Control-Allow-Methods: POST, OPTIONS\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    );
}
