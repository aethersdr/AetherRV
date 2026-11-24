//! The whole switching command path on virtual CAN: the real N2K service (`run()`, address claim, 126208 fast
//! packets, verification from 127501) against the simulated CLMD16, commands over a private Mosquitto as the
//! gateway sends them. Assertions come from the simulator (its outputs and its log of received commands).
//! Skipped when vcan12 or Mosquitto is missing (harness/dev-setup.sh creates the vcans).

use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::command::ResultKind;
use aetherrv_core::config::Cfg;
use aetherrv_core::state::{StateExt, StateStore};
use aetherrv_core::testutil::{Broker, eventually};
use aetherrv_sim::clmd16::{Clmd16, Clmd16Config};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

const IFACE: &str = "vcan12";
const S10: Duration = Duration::from_secs(10);

fn have() -> bool {
    aetherrv_can::raw::iface_up(IFACE) && std::path::Path::new(&aetherrv_core::testutil::mosquitto_bin()).exists()
}

fn config(dir: &std::path::Path) -> Cfg {
    Cfg::parse(&format!(
        r#"capture = false
[history]
path = "{d}/history.sqlite3"
[web]
admin_socket = "{d}/admin.sock"
[bus.hat0]
interface = "socketcan"
channel = "{IFACE}"
[device.n2k]
kind = "n2k"
bus = "hat0"
tx_enabled = true
[device.n2k.switching]
controller = 0
bank = 32
vsw_bank = 33
press_s = 0.3
address = 150
[[device.n2k.switch]]
bank = 32
channel = 1
press = 1
name = "Spot lights"
requires_input = 2
[[device.n2k.switch]]
bank = 32
channel = 7
press = 7
name = "Water pump"
[[device.n2k.switch]]
bank = 32
channel = 15
press = 8
name = "Starlink"
[[device.n2k.switch]]
bank = 32
channel = 10
press = 10
on_with_input = 2
name = "Stereo"
[[device.n2k.input]]
input = 2
name = "Ignition"
debounce_s = 0.2
[[device.n2k.trigger]]
id = "drawer_locks"
name = "Drawer locks"
bank = 33
switch = 11
channel = 11
pulse_s = 0.5
expect_off_s = 13.0
"#,
        d = dir.display()
    ))
    .unwrap()
}

fn log_lines(p: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(p).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn switching_command_path_against_the_simulated_clmd16() {
    if !have() {
        eprintln!("skipped: {IFACE} or mosquitto missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let broker = Broker::start(false);
    let mut sc = Clmd16Config::van(IFACE);
    sc.log = Some(dir.path().join("clmd16.jsonl"));
    let sim = Clmd16::start(sc).unwrap();
    let cfg = config(dir.path());
    let sock = broker.socket.clone();
    let svc = tokio::spawn(async move { aetherrv_n2k::run(cfg, aetherrv_n2k::Options { bus_socket: Some(sock) }).await });
    let bus = Bus::start(BusOptions::new("test").socket(&broker.socket), Arc::new(StateStore::new()));
    bus.subscribe_all_state().await;
    assert!(eventually(S10, || bus.remote().owner_ok("n2k")).await, "n2k service up");
    assert!(eventually(S10, || bus.remote().list("switches.list").len() >= 4).await, "switch list published");
    tokio::time::sleep(Duration::from_secs(2)).await; // address claim settles, a status arrives

    // a plain ON: one press of virtual switch 33.7, verified from the status
    let r = bus.request("n2k", "set", json!({"channel": 7, "on": true}), "test", 3000, S10).await;
    assert_eq!(r.result, ResultKind::Ok, "{r:?}");
    assert!(sim.is_on(7));
    let r = bus.request("n2k", "set", json!({"channel": 7, "on": true}), "test", 3000, S10).await;
    assert_eq!((r.result, r.detail.as_str()), (ResultKind::Ok, "already on"));
    let r = bus.request("n2k", "set", json!({"channel": 7, "on": false}), "test", 3000, S10).await;
    assert_eq!(r.result, ResultKind::Ok, "{r:?}");
    assert!(!sim.is_on(7));

    // channel 15 is pressed through virtual switch 8 (press != channel)
    let r = bus.request("n2k", "set", json!({"channel": 15, "on": true}), "test", 3000, S10).await;
    assert_eq!(r.result, ResultKind::Ok, "{r:?}");
    assert!(sim.is_on(15));

    // interlock: the service refuses spot lights without ignition and sends nothing
    let before = log_lines(&dir.path().join("clmd16.jsonl")).len();
    let r = bus.request("n2k", "set", json!({"channel": 1, "on": true}), "test", 3000, S10).await;
    assert_eq!(r.result, ResultKind::Refused, "{r:?}");
    assert_eq!(log_lines(&dir.path().join("clmd16.jsonl")).len(), before, "nothing sent for a refused interlock");
    // with ignition on (debounced) it goes through
    sim.set_input(2, true);
    assert!(
        eventually(S10, || bus.remote().list("switching.inputs").iter().any(|i| i["input"] == 2 && i["on"] == true)).await,
        "ignition seen"
    );
    let r = bus.request("n2k", "set", json!({"channel": 1, "on": true}), "test", 3000, S10).await;
    assert_eq!(r.result, ResultKind::Ok, "{r:?}");
    assert!(sim.is_on(1));
    // the stereo is held by ignition: an OFF is refused by the service
    assert!(eventually(S10, || sim.is_on(10)).await);
    let r = bus.request("n2k", "set", json!({"channel": 10, "on": false}), "test", 3000, S10).await;
    assert_eq!(r.result, ResultKind::Refused, "{r:?}");

    // a controller that ignores commands: one press, unverified, never retried
    sim.set_ignore(true);
    let before = log_lines(&dir.path().join("clmd16.jsonl")).len();
    let r = bus.request("n2k", "set", json!({"channel": 7, "on": true}), "test", 3000, Duration::from_secs(20)).await;
    assert_eq!(r.result, ResultKind::Unverified, "{r:?}");
    let sent = log_lines(&dir.path().join("clmd16.jsonl"));
    assert_eq!(sent.len() - before, 2, "exactly one ON and one release: {:?}", &sent[before..]);
    sim.set_ignore(false);

    // the drawer-lock trigger: a pulse of virtual switch 11; the controller's own timer turns ch11 on
    let r = bus.request("n2k", "trigger", json!({"trigger": "drawer_locks"}), "test", 3000, S10).await;
    assert_eq!(r.result, ResultKind::Ok, "{r:?}");
    assert!(sim.is_on(11));

    // every command the controller received was a 126208 Command for 127501 on bank 33 (virtual switches)
    for l in log_lines(&dir.path().join("clmd16.jsonl")) {
        assert_eq!(l["pgn"], 127501, "{l}");
        assert_eq!(l["pairs"][0], json!([1, 33]), "only virtual-switch presses: {l}");
    }
    svc.abort();
}
