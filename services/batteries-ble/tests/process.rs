//! The `aetherrv-batteries-ble` binary on a private bus, with no enabled `epoch_ble` device: it comes up, reports
//! itself healthy, publishes nothing and stops cleanly on SIGTERM. (The enabled path opens the real Bluetooth
//! adapter, so it is never started here.) Skipped when Mosquitto is missing.

use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::state::{StateStore, StateView};
use aetherrv_core::testutil::{Broker, eventually, mosquitto_bin};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_service_comes_up_and_stops_cleanly() {
    if !std::path::Path::new(&mosquitto_bin()).exists() {
        eprintln!("skipped: mosquitto missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let broker = Broker::start(false);
    let cfg = dir.path().join("aetherrv.toml");
    std::fs::write(
        &cfg,
        "[device.epoch]\nkind = \"epoch_ble\"\nenabled = false\n[[device.epoch.battery]]\naddress = \"AA:00:00:00:00:01\"\n",
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_aetherrv-batteries-ble"))
        .arg("-c")
        .arg(&cfg)
        .env("AETHERRV_BUS", &broker.socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let bus = Bus::start(BusOptions::new("test-observer").socket(&broker.socket), Arc::new(StateStore::new()));
    bus.subscribe_state("batteries-ble", "#").await;
    assert!(eventually(Duration::from_secs(10), || bus.remote().owner_ok("batteries-ble")).await, "module up");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(bus.remote().keys().is_empty(), "disabled: no batteries.ble* keys");
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let st = child.wait().unwrap();
    assert!(st.success(), "{st}");
    assert!(eventually(Duration::from_secs(5), || !bus.remote().owner_ok("batteries-ble")).await, "marked down on exit");
}
