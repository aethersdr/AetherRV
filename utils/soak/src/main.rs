//! `aetherrv-soak --hours 12 --units a,b,... [--ws ws://127.0.0.1:8080/ws] [--out DIR]`
//! Run as root on the BENCH with the whole system up against simulators (harness/bench/soak.sh does that).
//!
//! Activity, like a day in the van: a switch toggled over the bus every 5 min and over the gateway's WebSocket every
//! 10 min (the panel's path), the MCS7 setpoint nudged every 30 min, an Espar FAN → HEAT → OFF cycle every hour,
//! ignition flipped by the CLMD16 simulator. A WebSocket client stays connected the whole time, like the panel.
//!
//! Every 60 s: each unit's PID, RSS and CPU time, its restart count; every owner's health and the age of its newest
//! key; the heater simulator's report. At the end `summary.json` with the verdicts:
//! no restarts, no memory growth (last hour vs the hour after warm-up), no stuck state, every command ok, the
//! WebSocket never dropped, the heater never lost its controller. Exit code 1 if any verdict fails.

use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::command::ResultKind;
use aetherrv_core::state::{StateExt, StateStore};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn sh(args: &[&str]) -> String {
    std::process::Command::new(args[0]).args(&args[1..]).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
}

fn prop(unit: &str, p: &str) -> String {
    sh(&["systemctl", "show", "-p", p, "--value", unit])
}

/// (rss kB, cpu seconds) of a PID.
fn proc_stats(pid: u32) -> Option<(u64, f64)> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let rss = status.lines().find(|l| l.starts_with("VmRSS:"))?.split_whitespace().nth(1)?.parse().ok()?;
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 2..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    let tck = 100.0; // CLK_TCK on Linux/arm64
    let cpu = (f.get(11)?.parse::<f64>().ok()? + f.get(12)?.parse::<f64>().ok()?) / tck;
    Some((rss, cpu))
}

fn read_json(p: &str) -> Value {
    std::fs::read_to_string(p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null)
}

#[derive(Default)]
struct Cmds {
    sent: u64,
    failed: Vec<Value>,
}

/// Owners whose inputs change all the time: their newest key must stay this fresh (s).
const ACTIVE: [(&str, f64); 7] =
    [("n2k", 15.0), ("espar", 15.0), ("rixens", 30.0), ("starlink", 30.0), ("gps", 30.0), ("victron", 120.0), ("derive", 60.0)];

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let flag = |n: &str| a.iter().position(|x| x == n).and_then(|i| a.get(i + 1)).cloned();
    let hours: f64 = flag("--hours").and_then(|h| h.parse().ok()).unwrap_or(12.0);
    let units: Vec<String> = flag("--units").unwrap_or_default().split(',').filter(|s| !s.is_empty()).map(str::to_string).collect();
    let ws_url = flag("--ws").unwrap_or_else(|| "ws://127.0.0.1:8080/ws".into());
    let out = std::path::PathBuf::from(flag("--out").unwrap_or_else(|| "/var/tmp/soak".into()));
    let heater = flag("--heater-report").unwrap_or_else(|| "/run/aetherrv-sim-espar.json".into());
    std::fs::create_dir_all(&out)?;
    let services: Vec<String> = units.iter().filter_map(|u| u.strip_prefix("aetherrv-").map(|s| s.trim_end_matches(".service").to_string())).filter(|s| s != "bus").collect();

    let bus = Bus::start(BusOptions::new("soak"), Arc::new(StateStore::new()));
    bus.subscribe_all_state().await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let t0 = Instant::now();
    let end = t0 + Duration::from_secs_f64(hours * 3600.0);
    let cmds = Arc::new(Mutex::new(Cmds::default()));

    // the panel: one WebSocket for the whole soak
    let ws_msgs = Arc::new(AtomicU64::new(0));
    let ws_drops = Arc::new(AtomicU64::new(0));
    let ws_sent = Arc::new(AtomicU64::new(0));
    let ws_max_gap = Arc::new(Mutex::new(0f64));
    let (ws_tx, mut ws_rx) = tokio::sync::mpsc::channel::<Value>(8);
    let ws_results: Arc<Mutex<Vec<Value>>> = Arc::default();
    {
        let (msgs, drops, gap, results, cmds, url) = (ws_msgs.clone(), ws_drops.clone(), ws_max_gap.clone(), ws_results.clone(), cmds.clone(), ws_url.clone());
        tokio::spawn(async move {
            let mut first = true;
            loop {
                let Ok((mut ws, _)) = tokio_tungstenite::connect_async(&url).await else {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                };
                if !first {
                    drops.fetch_add(1, Ordering::SeqCst);
                }
                first = false;
                let mut last = Instant::now();
                loop {
                    tokio::select! {
                        m = ws.next() => {
                            let Some(Ok(m)) = m else { break };
                            let g = last.elapsed().as_secs_f64();
                            last = Instant::now();
                            msgs.fetch_add(1, Ordering::SeqCst);
                            {
                                let mut mg = gap.lock().unwrap();
                                if g > *mg { *mg = g; }
                            }
                            if let tokio_tungstenite::tungstenite::Message::Text(t) = m
                                && let Ok(v) = serde_json::from_str::<Value>(&t)
                                && v["type"] == "control_result"
                            {
                                if v["result"] != "ok" { cmds.lock().unwrap().failed.push(json!({"via": "ws", "r": v})); }
                                results.lock().unwrap().push(v);
                            }
                        }
                        c = ws_rx.recv() => {
                            let Some(c) = c else { return };
                            if ws.send(tokio_tungstenite::tungstenite::Message::Text(c.to_string().into())).await.is_err() { break; }
                        }
                    }
                }
            }
        });
    }

    // activity
    {
        let (bus, cmds, ws_sent) = (bus.clone(), cmds.clone(), ws_sent.clone());
        tokio::spawn(async move {
            let req = |dev: &'static str, cmd: &'static str, v: Value, to: u64| {
                let (bus, cmds) = (bus.clone(), cmds.clone());
                async move {
                    let r = bus.request(dev, cmd, v.clone(), "soak", 3000, Duration::from_secs(to)).await;
                    let mut c = cmds.lock().unwrap();
                    c.sent += 1;
                    if r.result != ResultKind::Ok {
                        c.failed.push(json!({"t": aetherrv_core::clock::now_s(), "dev": dev, "cmd": cmd, "value": v, "result": format!("{:?}", r.result), "detail": r.detail}));
                    }
                    r
                }
            };
            let mut minute = 0u64;
            let mut ws_id = 0u64;
            loop {
                let on7 = bus.remote().list("switches.list").iter().any(|s| s["channel"] == 7 && s["on"] == true);
                if minute.is_multiple_of(5) {
                    req("n2k", "set", json!({"channel": 7, "on": !on7}), 15).await;
                }
                if minute % 10 == 5 {
                    let on15 = bus.remote().list("switches.list").iter().any(|s| s["channel"] == 15 && s["on"] == true);
                    ws_id += 1;
                    cmds.lock().unwrap().sent += 1;
                    ws_sent.fetch_add(1, Ordering::SeqCst);
                    let _ = ws_tx.send(json!({"type": "control", "id": ws_id, "device": "n2k", "cmd": "set", "value": {"channel": 15, "on": !on15}})).await;
                }
                if minute % 30 == 15 {
                    let sp = if (minute / 30).is_multiple_of(2) { 20.5 } else { 21.0 };
                    req("rixens", "setpoint", json!(sp), 45).await;
                }
                match minute % 60 {
                    20 => {
                        req("espar", "set", json!({"mode": "fan"}), 30).await;
                    }
                    25 => {
                        req("espar", "set", json!({"mode": "heat", "setpoint_c": 21.0}), 30).await;
                    }
                    45 => {
                        req("espar", "set", json!({"mode": "off"}), 30).await;
                    }
                    _ => {}
                }
                minute += 1;
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
    }

    // sampling
    let mut samples = std::fs::File::create(out.join("samples.jsonl"))?;
    let pid0: BTreeMap<String, String> = units.iter().map(|u| (u.clone(), prop(u, "MainPID"))).collect();
    let restarts0: BTreeMap<String, String> = units.iter().map(|u| (u.clone(), prop(u, "NRestarts"))).collect();
    let mut rss: BTreeMap<String, Vec<(f64, u64)>> = BTreeMap::new();
    let mut owner_down: BTreeMap<String, u64> = BTreeMap::new();
    let mut stale: BTreeMap<String, (u64, f64)> = BTreeMap::new();
    let mut n = 0u64;
    while Instant::now() < end {
        tokio::time::sleep(Duration::from_secs(60)).await;
        n += 1;
        let el = t0.elapsed().as_secs_f64();
        let mut row = json!({"t": aetherrv_core::clock::now_s(), "elapsed_s": el.round()});
        let mut procs = serde_json::Map::new();
        for u in &units {
            let pid: u32 = prop(u, "MainPID").parse().unwrap_or(0);
            let st = proc_stats(pid);
            if let Some((r, _)) = st {
                rss.entry(u.clone()).or_default().push((el, r));
            }
            procs.insert(u.clone(), json!({"pid": pid, "rss_kb": st.map(|s| s.0), "cpu_s": st.map(|s| s.1), "restarts": prop(u, "NRestarts"), "state": prop(u, "ActiveState")}));
        }
        row["units"] = Value::Object(procs);
        let now_ms = aetherrv_core::clock::now_ms();
        let all = bus.remote().all();
        let mut ages = serde_json::Map::new();
        for s in &services {
            if !bus.remote().owner_ok(s) {
                *owner_down.entry(s.clone()).or_default() += 1;
            }
            let newest = all.iter().filter(|(_, o, _)| o == s).map(|(_, _, st)| st.t).max();
            let age = newest.map(|t| (now_ms - t) as f64 / 1000.0);
            ages.insert(s.clone(), json!(age));
            if let Some((_, lim)) = ACTIVE.iter().find(|(o, _)| o == s)
                && age.is_none_or(|a| a > *lim)
            {
                let e = stale.entry(s.clone()).or_insert((0, 0.0));
                e.0 += 1;
                e.1 = e.1.max(age.unwrap_or(f64::INFINITY));
            }
        }
        row["newest_key_age_s"] = Value::Object(ages);
        row["heater"] = read_json(&heater);
        row["ws_msgs"] = json!(ws_msgs.load(Ordering::SeqCst));
        row["commands"] = json!(cmds.lock().unwrap().sent);
        writeln!(samples, "{row}")?;
        samples.flush()?;
        if n.is_multiple_of(30) {
            println!("soak: {:.1} h, {} commands, {} failed", el / 3600.0, cmds.lock().unwrap().sent, cmds.lock().unwrap().failed.len());
        }
    }

    // verdicts
    let restarted: Vec<String> = units
        .iter()
        .filter(|u| prop(u, "MainPID") != pid0[*u] || prop(u, "NRestarts") != restarts0[*u] || prop(u, "ActiveState") != "active")
        .cloned()
        .collect();
    let median = |v: &mut Vec<u64>| -> u64 {
        v.sort();
        v.get(v.len() / 2).copied().unwrap_or(0)
    };
    let span = hours * 3600.0;
    let warm = (span * 0.1).clamp(60.0, 3600.0);
    let mut growth = serde_json::Map::new();
    let mut grew: Vec<String> = vec![];
    for (u, v) in &rss {
        let mut early: Vec<u64> = v.iter().filter(|(t, _)| *t >= warm && *t < warm + 3600.0f64.min(span * 0.2)).map(|x| x.1).collect();
        let mut late: Vec<u64> = v.iter().filter(|(t, _)| *t >= span - 3600.0f64.min(span * 0.2)).map(|x| x.1).collect();
        let (e, l) = (median(&mut early), median(&mut late));
        let d = l as i64 - e as i64;
        let bad = e > 0 && d > 4096 && (d as f64) > 0.10 * e as f64;
        if bad {
            grew.push(u.clone());
        }
        growth.insert(u.clone(), json!({"early_kb": e, "late_kb": l, "delta_kb": d, "max_kb": v.iter().map(|x| x.1).max()}));
    }
    let h = read_json(&heater);
    let c = cmds.lock().unwrap();
    let ws_res = ws_results.lock().unwrap().len();
    let checks = json!({
        "no_restarts": restarted.is_empty(),
        "no_memory_growth": grew.is_empty(),
        "owners_always_up": owner_down.is_empty(),
        "no_stuck_state": stale.is_empty(),
        // every WebSocket command answered (the last one may still be in flight at the end)
        "every_command_ok": c.failed.is_empty() && ws_res as u64 + 1 >= ws_sent.load(Ordering::SeqCst),
        "websocket_never_dropped": ws_drops.load(Ordering::SeqCst) == 0,
        "heater_never_lost_controller": h["controller_loss_faults"] == 0,
    });
    let pass = checks.as_object().unwrap().values().all(|v| v == true);
    let summary = json!({
        "pass": pass, "hours": hours, "checks": checks, "restarted": restarted, "memory": growth, "grew": grew,
        "owner_down_samples": owner_down, "stale": stale.iter().map(|(k, v)| (k.clone(), json!({"samples": v.0, "max_age_s": v.1}))).collect::<serde_json::Map<_, _>>(),
        "commands": c.sent, "failed": c.failed, "ws_results": ws_res, "ws_sent": ws_sent.load(Ordering::SeqCst), "ws_messages": ws_msgs.load(Ordering::SeqCst),
        "ws_drops": ws_drops.load(Ordering::SeqCst), "ws_max_gap_s": *ws_max_gap.lock().unwrap(), "heater": h,
        "version": std::fs::read_to_string("/opt/aetherrv-rs/VERSION").unwrap_or_default().trim(),
    });
    std::fs::write(out.join("summary.json"), serde_json::to_string_pretty(&summary)?)?;
    println!("{}", if pass { "SOAK: PASS" } else { "SOAK: FAIL" });
    std::process::exit(if pass { 0 } else { 1 });
}
