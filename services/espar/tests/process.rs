//! The real `aetherrv-espar` binary on vcan against the simulated heater, with a private Mosquitto
//! (ARCHITECTURE.md §12). Assertions come from what the heater saw (its frame log), not from the service's word.
//! Skipped when the vcan interface or Mosquitto is missing.

use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::command::ResultKind;
use aetherrv_core::state::{StateExt, StateStore};
use aetherrv_core::testutil::{Broker, eventually};
use aetherrv_sim::heater::{Heater, HeaterConfig};
use serde_json::json;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

const S10: Duration = Duration::from_secs(10);

fn have(iface: &str) -> bool {
    aetherrv_can::raw::iface_up(iface) && Path::new(&aetherrv_core::testutil::mosquitto_bin()).exists()
}

struct Espar {
    child: Option<Child>,
    cfg: std::path::PathBuf,
    broker: std::path::PathBuf,
    log: std::path::PathBuf,
}

impl Espar {
    fn new(dir: &Path, iface: &str, broker: &Path, blind_off_s: f64) -> Espar {
        let cfg = dir.join("aetherrv.toml");
        std::fs::write(
            &cfg,
            format!(
                r#"capture = false
[history]
path = "{d}/history.sqlite3"
[web]
admin_socket = "{d}/admin.sock"
[bus.hat1]
interface = "socketcan"
channel = "{iface}"
[device.espar]
kind = "espar"
bus = "hat1"
tx_enabled = true
diag_tx = true
diag_interval_s = 1
run_state = "{d}/run/run.json"
lockout_state = "{d}/lockout.json"
blind_off_s = {blind_off_s}
"#,
                d = dir.display()
            ),
        )
        .unwrap();
        Espar { child: None, cfg, broker: broker.to_path_buf(), log: dir.join("espar.log") }
    }

    fn start(&mut self) {
        let log = std::fs::File::options().create(true).append(true).open(&self.log).unwrap();
        let c = Command::new(env!("CARGO_BIN_EXE_aetherrv-espar"))
            .arg("-c")
            .arg(&self.cfg)
            .env("AETHERRV_BUS", &self.broker)
            .env("RUST_LOG", "info")
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        self.child = Some(c);
    }

    fn signal(&mut self, sig: i32) {
        if let Some(mut c) = self.child.take() {
            unsafe {
                libc::kill(c.id() as i32, sig);
            }
            let _ = c.wait();
        }
    }
}

impl Drop for Espar {
    fn drop(&mut self) {
        self.signal(libc::SIGKILL);
    }
}

fn client(b: &Broker, name: &str) -> Bus {
    Bus::start(BusOptions::new(name).socket(&b.socket), Arc::new(StateStore::new()))
}

async fn heat(g: &Bus) {
    let r = g.request("espar", "set", json!({"mode": "heat", "setpoint_c": 21.0}), "kiosk", 3000, Duration::from_secs(15)).await;
    assert_eq!(r.result, ResultKind::Ok, "{r:?}");
}

async fn setup(iface: &str, blind_off_s: f64, loss: f64) -> (tempfile::TempDir, Broker, Heater, Espar, Bus, Bus) {
    let dir = tempfile::tempdir().unwrap();
    let b = Broker::start(false);
    let h = Heater::start(HeaterConfig {
        iface: iface.into(),
        loss_timeout_s: loss,
        after_run_s: 5.0,
        sleep_s: 5.0,
        log: Some(dir.path().join("heater.jsonl")),
    })
    .unwrap();
    let mut e = Espar::new(dir.path(), iface, &b.socket, blind_off_s);
    e.start();
    // a stand-in Victron service with the cabin Ruuvi
    let vic = client(&b, "victron");
    vic.state().update([("climate.cabin_front.temp", json!(18.0)), ("victron.connected", json!(true))]);
    let g = client(&b, "gateway");
    g.subscribe_all_state().await;
    assert!(eventually(S10, || g.remote().owner_ok("espar")).await, "espar never came up");
    (dir, b, h, e, g, vic)
}

fn log_rows(dir: &Path) -> Vec<(String, String, String)> {
    let db = rusqlite_open(&dir.join("history.sqlite3"));
    db
}

fn rusqlite_open(p: &Path) -> Vec<(String, String, String)> {
    // read command_log through the sqlite3 CLI-free route: the Rust crate via aetherrv-core's open_db
    let db = aetherrv_core::cmdlog::open_db(p).unwrap();
    let mut st = db.prepare("SELECT cmd, outcome, detail FROM command_log ORDER BY t").unwrap();
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(Result::unwrap).collect()
}

/// SIGTERM and SIGKILL restarts mid-run: the heater never loses its controller, never sees a burst or an OFF,
/// never sees FE FF with HEAT, and the run continues (owner, §3 + 2026-11-23).
#[tokio::test(flavor = "multi_thread")]
async fn restarts_mid_run_resume_without_interrupting_the_heater() {
    let iface = "vcan1";
    if !have(iface) {
        return eprintln!("{iface} or mosquitto missing: skipped");
    }
    let (dir, _b, h, mut e, g, _vic) = setup(iface, 600.0, 15.0).await;
    heat(&g).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(h.report().state, "heating");
    h.reset_stats();
    let t_reset = h.elapsed();
    // graceful restart (systemctl restart / an upgrade)
    e.signal(libc::SIGTERM);
    tokio::time::sleep(Duration::from_millis(100)).await; // RestartSec=100ms
    e.start();
    tokio::time::sleep(Duration::from_secs(3)).await;
    // a crash
    e.signal(libc::SIGKILL);
    tokio::time::sleep(Duration::from_millis(100)).await;
    e.start();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let r = h.report();
    assert_eq!(r.state, "heating", "the run continued");
    assert_eq!(r.controller_loss_faults, 0, "no controller-loss fault");
    assert!(r.max_054_gap_running < 3.0, "054 gap {:.2} s", r.max_054_gap_running);
    assert_eq!(r.full_power_frames, 0, "never FE FF with HEAT");
    assert!(r.burst_frames.iter().all(|(t, _)| *t < t_reset), "no wake burst after a resume: {:?}", r.burst_frames);
    assert!(r.modes.iter().filter(|(t, _)| *t > t_reset).all(|(_, m)| *m == 0x05), "no OFF frame after SIGTERM");
    let rows = log_rows(dir.path());
    let resumes: Vec<_> = rows.iter().filter(|r| r.0 == "resume").collect();
    assert_eq!(resumes.len(), 2, "{rows:?}");
    assert!(resumes.iter().all(|r| r.1 == "ok"));
    eprintln!("max 054 gap across SIGTERM + SIGKILL restarts: {:.2} s", r.max_054_gap_running);
    // and it can still be turned off normally
    let off = g.request("espar", "set", json!({"mode": "off"}), "kiosk", 3000, Duration::from_secs(15)).await;
    assert_eq!(off.result, ResultKind::Ok, "{off:?}");
    assert_eq!(h.report().state, "after-run");
}

/// The broker goes away: the run continues; after blind_off_s without contact the heater is turned off cleanly.
#[tokio::test(flavor = "multi_thread")]
async fn broker_down_keeps_heating_then_blind_running_turns_it_off() {
    let iface = "vcan2";
    if !have(iface) {
        return eprintln!("{iface} or mosquitto missing: skipped");
    }
    let (dir, mut b, h, _e, g, _vic) = setup(iface, 4.0, 15.0).await;
    heat(&g).await;
    h.reset_stats();
    b.kill();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let r = h.report();
    assert_eq!(r.state, "heating", "the broker going away doesn't touch the heater");
    assert!(r.max_054_gap_running < 0.5, "{}", r.max_054_gap_running);
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(h.report().state, "after-run", "blind for > blind_off_s: OFF");
    assert_eq!(h.report().controller_loss_faults, 0, "a clean OFF, not a lost controller");
    b.restart();
    assert!(eventually(S10, || g.connected() && g.remote().owner_ok("espar")).await, "espar reconnects");
    let rows = log_rows(dir.path());
    assert!(rows.iter().any(|r| r.0 == "auto_off" && r.2.contains("no contact")), "{rows:?}");
}

/// Another controller on the bus: stop transmitting at once. A silent heater: stop and report.
#[tokio::test(flavor = "multi_thread")]
async fn foreign_controller_and_silent_heater_stop_transmitting() {
    let iface = "vcan3";
    if !have(iface) {
        return eprintln!("{iface} or mosquitto missing: skipped");
    }
    let (_dir, _b, h, _e, g, _vic) = setup(iface, 600.0, 2.0).await;
    heat(&g).await;
    let pro = aetherrv_can::raw::RawSocket::open(iface, false).unwrap();
    pro.send(&aetherrv_can::CanFrame::std(0x60D, &[0x0D, 0x10, 0xFD, 0x1D, 0x02, 0x20, 0x70, 0x00])).unwrap();
    assert!(eventually(S10, || g.remote().is_false("climate.espar.ctl.active")).await);
    assert!(g.remote().string("climate.espar.ctl.problem").unwrap().contains("another controller"));
    let n = h.report().frames_054;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(h.report().frames_054, n, "nothing sent after another controller appeared");
    // the heater stops itself (its controller-loss timeout), as the real one does
    assert!(eventually(S10, || h.report().state != "heating").await);
    // a silent heater
    tokio::time::sleep(Duration::from_secs(4)).await; // the other-controller hold-off (3 s)
    let r = g.request("espar", "set", json!({"mode": "fan"}), "kiosk", 3000, Duration::from_secs(15)).await;
    assert_eq!(r.result, ResultKind::Ok, "{r:?}");
    h.pause(true);
    assert!(eventually(S10, || g.remote().is_false("climate.espar.ctl.active")).await);
    assert!(g.remote().string("climate.espar.ctl.problem").unwrap().contains("lost the heater"));
}

/// An OFF sent while the restarted service is still deciding whether to resume waits for the decision and then
/// turns the resumed run off (it is never answered "already off" while the run resumes).
#[tokio::test(flavor = "multi_thread")]
async fn an_off_during_the_resume_window_turns_the_resumed_run_off() {
    let iface = "vcan4";
    if !have(iface) {
        return eprintln!("{iface} or mosquitto missing: skipped");
    }
    let (_dir, _b, h, mut e, g, _vic) = setup(iface, 600.0, 15.0).await;
    heat(&g).await;
    e.signal(libc::SIGKILL);
    assert!(eventually(S10, || !g.remote().owner_ok("espar")).await, "the last will marks it down");
    e.start();
    assert!(eventually(S10, || g.remote().owner_ok("espar")).await);
    let off = g.request("espar", "set", json!({"mode": "off"}), "kiosk", 3000, Duration::from_secs(15)).await;
    assert_eq!(off.result, ResultKind::Ok, "{off:?}");
    assert_ne!(off.detail, "already off", "the OFF must apply to the resumed run");
    assert!(eventually(S10, || h.report().state == "after-run").await, "{}", h.report().state);
    assert_eq!(h.report().controller_loss_faults, 0);
}

/// A crash while the broker is down: no Ruuvi on restart, so the run continues on the room value saved before the
/// restart (then the heater's intake sensor), never FE FF.
#[tokio::test(flavor = "multi_thread")]
async fn a_crash_with_the_broker_down_resumes_on_the_saved_room_value() {
    let iface = "vcan0";
    if !have(iface) {
        return eprintln!("{iface} or mosquitto missing: skipped");
    }
    let (dir, mut b, h, mut e, g, _vic) = setup(iface, 600.0, 15.0).await;
    heat(&g).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    b.kill();
    h.reset_stats();
    e.signal(libc::SIGKILL);
    tokio::time::sleep(Duration::from_millis(100)).await;
    e.start();
    tokio::time::sleep(Duration::from_secs(4)).await;
    let r = h.report();
    assert_eq!(r.state, "heating");
    assert_eq!(r.full_power_frames, 0, "never FE FF with HEAT");
    assert_eq!(r.controller_loss_faults, 0);
    assert!(r.max_054_gap_running < 3.0, "{}", r.max_054_gap_running);
    assert!(log_rows(dir.path()).iter().any(|r| r.0 == "resume" && r.1 == "ok"));
    b.restart();
}

/// A late HEAT is refused (nothing sent); a late OFF still runs (the safe direction, owner Q-T1).
#[tokio::test(flavor = "multi_thread")]
async fn a_late_heat_is_refused_but_a_late_off_runs() {
    let iface = "vcan5";
    if !have(iface) {
        return eprintln!("{iface} or mosquitto missing: skipped");
    }
    let (_dir, _b, h, _e, g, _vic) = setup(iface, 600.0, 15.0).await;
    let late = g.request("espar", "set", json!({"mode": "heat", "setpoint_c": 21.0}), "kiosk", -1, Duration::from_secs(15)).await;
    assert_eq!(late.result, ResultKind::Refused, "{late:?}");
    assert_eq!(h.report().frames_054, 0, "nothing sent");
    heat(&g).await;
    let off = g.request("espar", "set", json!({"mode": "off"}), "kiosk", -1, Duration::from_secs(15)).await;
    assert_eq!(off.result, ResultKind::Ok, "{off:?}");
    assert_eq!(h.report().state, "after-run");
}

/// No cabin Ruuvi on the bus (no Victron): FAN wakes the heater, the read-only diagnostics read its intake sensor,
/// and HEAT then regulates on that value (Python's fallback), never on FE FF.
#[tokio::test(flavor = "multi_thread")]
async fn without_the_ruuvi_heat_uses_the_heater_intake_sensor() {
    let iface = "vcan9";
    if !have(iface) {
        return eprintln!("{iface} or mosquitto missing: skipped");
    }
    let dir = tempfile::tempdir().unwrap();
    let b = Broker::start(false);
    let h = Heater::start(HeaterConfig { iface: iface.into(), loss_timeout_s: 15.0, after_run_s: 5.0, sleep_s: 5.0, log: None }).unwrap();
    let mut e = Espar::new(dir.path(), iface, &b.socket, 600.0);
    e.start();
    let g = client(&b, "gateway");
    g.subscribe_all_state().await;
    assert!(eventually(S10, || g.remote().owner_ok("espar")).await);
    let r = g.request("espar", "set", json!({"mode": "heat", "setpoint_c": 21.0}), "kiosk", 3000, Duration::from_secs(15)).await;
    assert_eq!(r.result, ResultKind::Refused, "no room temperature yet: {r:?}");
    let r = g.request("espar", "set", json!({"mode": "fan"}), "kiosk", 3000, Duration::from_secs(15)).await;
    assert_eq!(r.result, ResultKind::Ok, "{r:?}");
    assert!(eventually(Duration::from_secs(30), || g.remote().string("climate.espar.ctl.room_src").as_deref() == Some("heater intake sensor")).await,
            "the diagnostics read the intake sensor");
    heat(&g).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let rep = h.report();
    assert_eq!(rep.state, "heating");
    assert_eq!(rep.full_power_frames, 0);
}
