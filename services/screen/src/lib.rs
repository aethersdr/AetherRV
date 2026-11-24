//! `aetherrv-screen`: screen power for the kiosk, run by sway inside the kiosk session (a port of
//! `deploy/kiosk/screen_helper.py`).
//!
//! ```text
//! GET  http://127.0.0.1:8091/screen      → {"on": true|false}
//! POST http://127.0.0.1:8091/screen/off  → powers the HDMI output off (backlight off on the Waveshare panel)
//! POST http://127.0.0.1:8091/screen/on   → powers it back on
//! ```
//!
//! Loopback only; responses allow the console origin. The Waveshare panel has no DDC/CI or sysfs backlight, so HDMI
//! power is the only software switch. Wake on touch reads the touch panel's evdev node directly (read-only, no
//! grab), because sway doesn't deliver touches to the page while the output is off.

use serde_json::{Value, json};
use std::time::Duration;

pub const ORIGIN: &str = "http://127.0.0.1:8080";

fn sway(args: &[&str]) -> String {
    std::process::Command::new("swaymsg").args(args).output().map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default()
}

pub fn outputs() -> Vec<Value> {
    serde_json::from_str::<Value>(&sway(&["-t", "get_outputs", "-r"])).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default()
}

pub fn is_on() -> bool {
    let o = outputs();
    o.is_empty() || o.iter().any(|x| x.get("power").and_then(Value::as_bool).unwrap_or(true))
}

/// The kernel's view of the connector (sway can report success while the panel is still off).
pub fn drm_on(name: &str) -> Option<bool> {
    let pat = format!("-{name}");
    for e in std::fs::read_dir("/sys/class/drm").ok()?.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if n.starts_with("card") && n.ends_with(&pat)
            && let Ok(s) = std::fs::read_to_string(e.path().join("dpms"))
        {
            return Some(s.trim() == "On");
        }
    }
    None
}

/// Address outputs by name (sway 1.12's "output * power on" doesn't wake a powered-off output), and repeat "on"
/// until DRM agrees (the first power-on after an off is ignored on the Pi 5; max 4 tries).
pub fn set_power(on: bool) {
    for o in outputs() {
        let Some(name) = o.get("name").and_then(Value::as_str) else { continue };
        if !on {
            sway(&["output", name, "power", "off"]);
            continue;
        }
        for _ in 0..4 {
            sway(&["output", name, "power", "on"]);
            std::thread::sleep(Duration::from_millis(400));
            if drm_on(name) != Some(false) {
                break;
            }
        }
    }
}

/// One HTTP exchange (method, path) → (status, body).
pub fn route(method: &str, path: &str) -> (u16, Value) {
    match (method, path) {
        ("GET", "/screen") => (200, json!({"on": is_on()})),
        ("POST", "/screen/on") | ("POST", "/screen/off") => {
            set_power(path.ends_with("/on"));
            (200, json!({"on": is_on()}))
        }
        _ => (404, json!({"error": "not found"})),
    }
}

/// Wake at the first touch-down while the screen is off (one thread per input device, read-only).
pub fn wake_on_touch() {
    use std::io::Read;
    const EV_KEY: u16 = 0x01;
    const BTN_TOUCH: u16 = 0x14A;
    const EV_ABS: u16 = 0x03;
    let mut seen = std::collections::HashSet::new();
    for dir in ["/dev/input/by-id", "/dev/input/by-path"] {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            if !e.file_name().to_string_lossy().contains("-event") {
                continue;
            }
            let Ok(real) = std::fs::canonicalize(e.path()) else { continue };
            if !seen.insert(real.clone()) {
                continue;
            }
            std::thread::spawn(move || loop {
                if let Ok(mut f) = std::fs::File::open(&real) {
                    let mut buf = [0u8; 24 * 64];
                    while let Ok(n) = f.read(&mut buf) {
                        if n == 0 {
                            break;
                        }
                        let hit = buf[..n].chunks_exact(24).any(|ev| {
                            let typ = u16::from_ne_bytes([ev[16], ev[17]]);
                            let code = u16::from_ne_bytes([ev[18], ev[19]]);
                            let val = i32::from_ne_bytes([ev[20], ev[21], ev[22], ev[23]]);
                            (typ == EV_KEY && code == BTN_TOUCH && val == 1) || typ == EV_ABS
                        });
                        if hit && !is_on() {
                            set_power(true);
                        }
                    }
                }
                std::thread::sleep(Duration::from_secs(2)); // device gone/replugged: retry
            });
        }
    }
}

/// sway's children live in the login session scope, so a kiosk restart can orphan this helper: exit as soon as our
/// sway socket is gone or we are reparented.
pub fn exit_with_sway() {
    let sock = std::env::var("SWAYSOCK").unwrap_or_default();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(2));
        if unsafe { libc::getppid() } == 1 || (!sock.is_empty() && !std::path::Path::new(&sock).exists()) {
            std::process::exit(0);
        }
    });
}

/// A minimal HTTP/1.1 server on 127.0.0.1:8091 (one request per connection, like Python's).
pub fn serve(addr: &str) -> std::io::Result<()> {
    use std::io::{BufRead, BufReader, Write};
    let l = std::net::TcpListener::bind(addr)?;
    for s in l.incoming().flatten() {
        std::thread::spawn(move || {
            let mut r = BufReader::new(&s);
            let mut first = String::new();
            if r.read_line(&mut first).is_err() {
                return;
            }
            let mut line = String::new();
            while r.read_line(&mut line).is_ok() && line.trim() != "" {
                line.clear();
            }
            let mut it = first.split_whitespace();
            let (m, p) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
            let (code, body) = route(m, p);
            let b = body.to_string();
            let reason = if code == 200 { "OK" } else { "Not Found" };
            let _ = (&s).write_all(
                format!("HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: {ORIGIN}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}", b.len()).as_bytes(),
            );
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn routes() {
        assert_eq!(super::route("GET", "/nope").0, 404);
        assert_eq!(super::route("DELETE", "/screen").0, 404);
    }
}
