//! `aetherrv-ctl` (run as the aetherrv user: the bus socket is the services' own)
//!
//!   aetherrv-ctl cmd <device> <cmd> '<json value>' [--client NAME] [--timeout S]   send one command, print the reply
//!   aetherrv-ctl get <key>...                                                     current values (and owner, age)
//!   aetherrv-ctl health                                                           every service's up/heartbeat age
//!
//! The client name defaults to "ctl:<user>" so command_log rows show where a command came from.

use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::state::{StateStore, StateView};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let flag = |n: &str| a.iter().position(|x| x == n).and_then(|i| a.get(i + 1)).cloned();
    let user = std::env::var("USER").unwrap_or_else(|_| "unknown".into());
    let bus = Bus::start(BusOptions::new(&format!("ctl-{}", std::process::id())).tool(), Arc::new(StateStore::new()));
    bus.subscribe_all_state().await;
    for _ in 0..50 {
        if bus.connected() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(400)).await; // retained state and health arrive
    match a.first().map(String::as_str) {
        Some("cmd") => {
            let (dev, cmd) = (a.get(1).cloned().unwrap_or_default(), a.get(2).cloned().unwrap_or_default());
            let value: Value = serde_json::from_str(a.get(3).map(String::as_str).unwrap_or("null"))?;
            let client = flag("--client").unwrap_or_else(|| format!("ctl:{user}"));
            let t: f64 = flag("--timeout").and_then(|s| s.parse().ok()).unwrap_or(30.0);
            let r = bus.request(&dev, &cmd, value, &client, 3000, Duration::from_secs_f64(t)).await;
            println!("{}", json!({"result": r.result, "detail": r.detail}));
        }
        Some("get") => {
            for k in &a[1..] {
                match bus.remote().stamped(k, true) {
                    Some((owner, s)) => println!("{k} = {} (owner {owner}, age {:.1} s, owner up {})", s.v,
                        bus.remote().age(k).unwrap_or(f64::NAN), bus.remote().owner_ok(&owner)),
                    None => println!("{k} = (none)"),
                }
            }
        }
        Some("health") => {
            for (n, up, age) in bus.remote().owners() {
                println!("{n:<16} {} hb {}", if up { "up  " } else { "DOWN" }, age.map(|x| format!("{x:.1}s")).unwrap_or("-".into()));
            }
        }
        _ => eprintln!("usage: aetherrv-ctl cmd <device> <cmd> '<json>' | get <key>... | health"),
    }
    Ok(())
}
