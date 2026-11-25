//! `aetherrv-isolation --services espar,n2k,... [--heater-report F] [--mcs7-log F] [--out report.json]`
//! Run as root on the BENCH with aetherrv.target and the simulators up (harness/bench/isolation.sh does that).
//!
//! 1. Starts an Espar HEAT run and MCS7 furnace heat through the bus (as the touchscreen would).
//! 2. For every service, then for the broker: `kill -9` (and for the broker also a clean restart) under systemd and
//!    watch at 4 Hz that every OTHER service stays up the whole time, that the victim comes back and republishes its
//!    state (any key it owns restamped after the kill; for the broker: every service up and connected again),
//!    that the heater simulator never loses its controller (054 gap, controller-loss faults) and that the MCS7
//!    simulator was sent nothing.
//!    Every OTHER unit's MainPID and NRestarts must be unchanged after each case (no collateral restart), the gateway
//!    must answer /api/state during every case it isn't the victim of, and the admin helper (socket-activated, not
//!    on the bus) must answer a Settings call after its kill. `can-aux@<bus>` victims are on the bus as
//!    `can-aux-<bus>`. At the end the history database must have no sample gap longer than two intervals.
//! 3. Writes a JSON report; exit code 1 if anything failed.

use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::state::{StateExt, StateStore};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn sh(args: &[&str]) -> String {
    std::process::Command::new(args[0]).args(&args[1..]).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
}

fn read_json(p: &str) -> Value {
    std::fs::read_to_string(p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null)
}

fn lines(p: &str) -> usize {
    std::fs::read_to_string(p).map(|s| s.lines().count()).unwrap_or(0)
}

/// A heartbeat published after `t_ms` (heartbeats carry the publisher's own time): proof the process that sent it
/// was alive and connected after the kill.
fn hb_after(bus: &Bus, owner: &str, t_ms: i64) -> bool {
    let now = aetherrv_core::clock::now_ms();
    bus.remote().owners().into_iter().any(|(n, _, age)| n == owner && age.is_some_and(|a| now - (a * 1000.0) as i64 > t_ms))
}

/// Keys the service owns, and whether one of them was published after `t_ms` (a new process restamps every key).
fn owned(bus: &Bus, owner: &str, t_ms: i64) -> (usize, bool) {
    let mine: Vec<_> = bus.remote().all().into_iter().filter(|(_, o, _)| o == owner).collect();
    let fresh = mine.iter().any(|(_, _, s)| s.t > t_ms);
    (mine.len(), fresh)
}

/// The name a victim has on the bus (`None`: not a bus client).
fn owner_of(victim: &str) -> Option<String> {
    match victim {
        "admind" => None,
        v if v.starts_with("can-aux@") => Some(format!("can-aux-{}", &v["can-aux@".len()..])),
        v => Some(v.to_string()),
    }
}

/// (MainPID, NRestarts) of every unit.
fn unit_snapshot(units: &[String]) -> Vec<(String, String, String)> {
    units
        .iter()
        .map(|u| (u.clone(), sh(&["systemctl", "show", "-p", "MainPID", "--value", u]), sh(&["systemctl", "show", "-p", "NRestarts", "--value", u])))
        .collect()
}

fn gateway_answers() -> bool {
    sh(&["curl", "-sS", "--max-time", "2", "-o", "/dev/null", "-w", "%{http_code}", "http://127.0.0.1:8080/api/state"]) == "200"
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let flag = |n: &str| a.iter().position(|x| x == n).and_then(|i| a.get(i + 1)).cloned();
    let services: Vec<String> = flag("--services").unwrap_or_default().split(',').filter(|s| !s.is_empty()).map(str::to_string).collect();
    let heater = flag("--heater-report").unwrap_or_else(|| "/run/aetherrv-sim-espar.json".into());
    let mcs7 = flag("--mcs7-log").unwrap_or_else(|| "/var/tmp/sim-mcs7.jsonl".into());
    let out = flag("--out").unwrap_or_else(|| "/var/tmp/isolation-report.json".into());
    let bus = Bus::start(BusOptions::new("isolation").tool(), Arc::new(StateStore::new()));
    bus.subscribe_all_state().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let s30 = Duration::from_secs(30);
    // 1. heat on both heaters
    let r = bus.request("espar", "set", json!({"mode": "fan"}), "isolation", 3000, s30).await;
    println!("espar fan: {:?}", r);
    for _ in 0..60 {
        if bus.remote().string("climate.espar.ctl.room_src").is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let r = bus.request("espar", "set", json!({"mode": "heat", "setpoint_c": 21.0}), "isolation", 3000, s30).await;
    println!("espar heat: {:?}", r);
    let r = bus.request("rixens", "furnace", json!(true), "isolation", 3000, s30).await;
    println!("mcs7 furnace: {:?}", r);
    tokio::time::sleep(Duration::from_secs(3)).await;
    let mut results = vec![];
    let mut ok_all = true;
    let mut cases: Vec<(String, &'static str)> = services.iter().map(|s| (s.clone(), "kill")).collect();
    cases.push(("bus".into(), "restart"));
    cases.push(("bus".into(), "kill"));
    let owners: Vec<String> = services.iter().filter_map(|s| owner_of(s)).collect();
    // every long-running unit (the socket-activated admin helper comes and goes by design: judged by its own case)
    let mut all_units: Vec<String> = services.iter().filter(|s| *s != "admind").map(|s| format!("aetherrv-{s}.service")).collect();
    all_units.push("aetherrv-bus.service".into());
    let run_t0 = aetherrv_core::clock::now_s();
    for (victim, how) in cases {
        let before = unit_snapshot(&all_units);
        if victim == "admind" {
            // the helper is socket-activated: make sure it is running, kill it, then a Settings call must still work
            let _ = sh(&["curl", "-sS", "--max-time", "25", "-o", "/dev/null", "http://127.0.0.1:8080/api/settings"]);
            let active0 = sh(&["systemctl", "is-active", "aetherrv-admind.service"]);
            let pid0 = sh(&["systemctl", "show", "-p", "MainPID", "--value", "aetherrv-admind.service"]);
            sh(&["systemctl", "kill", "-s", "KILL", "aetherrv-admind.service"]);
            tokio::time::sleep(Duration::from_millis(500)).await;
            let body = sh(&["curl", "-sS", "--max-time", "25", "http://127.0.0.1:8080/api/settings"]);
            let admin_ok = serde_json::from_str::<Value>(&body).ok().is_some_and(|j| j["admin"]["ok"] == true || j["admin"].get("error").is_none() && j["admin"].is_object());
            let pid1 = sh(&["systemctl", "show", "-p", "MainPID", "--value", "aetherrv-admind.service"]);
            let after = unit_snapshot(&all_units);
            let collateral: Vec<String> = before.iter().zip(&after).filter(|(b, a)| b != a).map(|(b, _)| b.0.clone()).collect();
            let pass = admin_ok && pid1 != pid0 && collateral.is_empty();
            ok_all &= pass;
            let row = json!({"victim": "admind", "how": "kill", "pass": pass, "was_active": active0, "pid_before": pid0, "pid_after": pid1,
                             "settings_answered": admin_ok, "collateral_restarts": collateral});
            println!("{row}");
            results.push(row);
            continue;
        }
        let h0 = read_json(&heater);
        let m0 = lines(&mcs7);
        let gap0 = h0["max_054_gap_running"].as_f64().unwrap_or(0.0);
        let unit = format!("aetherrv-{victim}.service");
        let me = owner_of(&victim).unwrap_or_default();
        let pid0 = sh(&["systemctl", "show", "-p", "MainPID", "--value", &unit]);
        let t_kill = aetherrv_core::clock::now_ms();
        let (keys_before, _) = owned(&bus, &me, t_kill);
        if how == "kill" {
            sh(&["systemctl", "kill", "-s", "KILL", &unit]);
        } else {
            sh(&["systemctl", "restart", &unit]);
        }
        // while the victim is down, the switching command path must still work end to end (not via the broker
        // when the broker is the victim)
        let switch_probe = if victim != "n2k" && victim != "bus" {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let on = !bus.remote().list("switches.list").iter().any(|s| s["channel"] == 7 && s["on"] == true);
            let r = bus.request("n2k", "set", json!({"channel": 7, "on": on}), "isolation", 3000, Duration::from_secs(15)).await;
            Some(r.result == aetherrv_core::command::ResultKind::Ok)
        } else {
            None
        };
        let start = Instant::now();
        let mut others_down: Vec<String> = vec![];
        let mut back_at: Option<f64> = None;
        let mut fresh_at: Option<f64> = None;
        let mut gateway_seen = victim == "gateway";
        while start.elapsed() < Duration::from_secs(20) {
            tokio::time::sleep(Duration::from_millis(250)).await;
            if !gateway_seen && gateway_answers() {
                gateway_seen = true;
            }
            if victim != "bus" {
                for (n, up, _) in bus.remote().owners() {
                    if n != me && owners.contains(&n) && !up && !others_down.contains(&n) {
                        others_down.push(n);
                    }
                }
            }
            // back = a new process (systemd's MainPID changed) that is on the bus again (a heartbeat stamped after
            // the kill); for the broker: every service has published a heartbeat since it came back
            let up = if victim == "bus" {
                bus.connected() && owners.iter().all(|s| hb_after(&bus, s, t_kill))
            } else {
                let pid = sh(&["systemctl", "show", "-p", "MainPID", "--value", &unit]);
                pid != pid0 && pid != "0" && hb_after(&bus, &me, t_kill) && bus.remote().owner_ok(&me)
            };
            if up && back_at.is_none() {
                back_at = Some(start.elapsed().as_secs_f64());
            }
            let needs_fresh = victim != "bus" && keys_before > 0;
            if fresh_at.is_none() && up && (!needs_fresh || owned(&bus, &me, t_kill).1) {
                fresh_at = Some(start.elapsed().as_secs_f64());
            }
            if back_at.is_some() && fresh_at.is_some() && gateway_seen && start.elapsed() > Duration::from_secs(6) {
                break;
            }
        }
        let h1 = read_json(&heater);
        let gap = h1["max_054_gap_running"].as_f64().unwrap_or(0.0);
        let mcs7_new = lines(&mcs7).saturating_sub(m0);
        let heater_ok = h1["state"] == "heating" && h1["controller_loss_faults"] == h0["controller_loss_faults"];
        let furnace = bus.remote().list("climate.rixens.sources").iter().any(|v| v == "furnace");
        // no OTHER unit restarted (the victim's own PID/restart count is expected to change)
        let after = unit_snapshot(&all_units);
        let collateral: Vec<String> =
            before.iter().zip(&after).filter(|(b, a)| b != a && b.0 != unit).map(|(b, _)| b.0.clone()).collect();
        let pass = others_down.is_empty() && back_at.is_some() && fresh_at.is_some() && heater_ok && mcs7_new == 0 && furnace
            && switch_probe != Some(false) && collateral.is_empty() && gateway_seen;
        ok_all &= pass;
        let row = json!({"victim": victim, "how": how, "pass": pass, "back_s": back_at, "fresh_state_s": fresh_at,
                         "others_down": others_down, "keys_owned": keys_before, "heater_state": h1["state"], "max_054_gap_s": gap.max(gap0),
                         "controller_loss_faults": h1["controller_loss_faults"], "mcs7_commands_sent": mcs7_new,
                         "mcs7_furnace_on": furnace, "switch_probe_ok": switch_probe,
                         "collateral_restarts": collateral, "gateway_answered": gateway_seen});
        println!("{row}");
        results.push(row);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let r = bus.request("espar", "set", json!({"mode": "off"}), "isolation", 3000, s30).await;
    println!("espar off: {:?}", r);
    let r = bus.request("rixens", "furnace", json!(false), "isolation", 3000, s30).await;
    println!("mcs7 furnace off: {:?}", r);
    // history kept writing through every case (its own restart included): no gap over two intervals
    let interval: f64 = flag("--history-interval").and_then(|s| s.parse().ok()).unwrap_or(30.0);
    let db = flag("--history-db").unwrap_or_else(|| "/var/lib/aetherrv/history.sqlite3".into());
    let q = format!("SELECT DISTINCT round(t) FROM samples WHERE t >= {run_t0} ORDER BY 1");
    let ts: Vec<f64> = sh(&["sqlite3", &db, &q]).lines().filter_map(|l| l.trim().parse().ok()).collect();
    let max_gap = ts.windows(2).map(|w| w[1] - w[0]).fold(0.0, f64::max);
    let run_s = aetherrv_core::clock::now_s() - run_t0;
    let history_ok = ts.len() as f64 >= (run_s / interval).floor() - 1.0 && max_gap <= 2.0 * interval + 1.0;
    ok_all &= history_ok;
    let history = json!({"pass": history_ok, "samples_times": ts.len(), "max_gap_s": max_gap, "run_s": run_s.round(), "interval_s": interval});
    println!("history: {history}");
    let report = json!({"pass": ok_all, "results": results, "history": history,
                        "version": std::fs::read_to_string("/opt/aetherrv-rs/VERSION").unwrap_or_default().trim()});
    std::fs::write(&out, serde_json::to_string_pretty(&report)?)?;
    println!("{}", if ok_all { "ISOLATION: PASS" } else { "ISOLATION: FAIL" });
    std::process::exit(if ok_all { 0 } else { 1 });
}
