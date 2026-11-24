//! The real `aetherrv-weather` binary: it serves its routes with the bus down, publishes `traffic.available` on a
//! private Mosquitto, never logs the TomTom key, and removes its socket on SIGTERM. No GPS position is ever
//! published here, so the recorder never fetches anything (no internet).

mod common;

use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::state::{StateExt, StateStore};
use aetherrv_core::testutil::{Broker, eventually, mosquitto_bin};
use common::get;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

const S10: Duration = Duration::from_secs(10);

struct Proc {
    child: Child,
    sock: PathBuf,
    log: PathBuf,
}

impl Proc {
    fn start(dir: &Path, key: Option<&str>, bus: &Path) -> Proc {
        let secrets = dir.join("secrets.toml");
        std::fs::write(&secrets, key.map(|k| format!("tomtom_api_key = \"{k}\"\n")).unwrap_or_default()).unwrap();
        let sock = dir.join("run/weather.sock");
        let cfg = dir.join("aetherrv.toml");
        std::fs::write(
            &cfg,
            format!(
                "secrets_file = \"{}\"\n[radar]\ndir = \"{}\"\nsocket = \"{}\"\n",
                secrets.display(),
                dir.join("radar").display(),
                sock.display()
            ),
        )
        .unwrap();
        let log = dir.join("weather.log");
        let child = Command::new(env!("CARGO_BIN_EXE_aetherrv-weather"))
            .arg("-c")
            .arg(&cfg)
            .env("AETHERRV_BUS", bus)
            .env("RUST_LOG", "debug")
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap();
        Proc { child, sock, log }
    }

    async fn serving(&self) -> bool {
        eventually(S10, || std::os::unix::net::UnixStream::connect(&self.sock).is_ok()).await
    }

    fn term(&mut self) -> std::process::ExitStatus {
        let st = Command::new("kill").arg("-TERM").arg(self.child.id().to_string()).status().unwrap();
        assert!(st.success());
        self.child.wait().unwrap()
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test]
async fn process_serves_with_the_bus_down() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = Proc::start(dir.path(), None, &dir.path().join("no-broker.sock"));
    assert!(p.serving().await, "never served: {}", p.log());
    let r = get(&p.sock, "/api/radar/recorded").await;
    assert_eq!(String::from_utf8(r.body).unwrap(), r#"{"frames": [], "flows": [], "zoom": [4, 7]}"#);
    assert_eq!(get(&p.sock, "/api/traffic/flow/major/5/1/2.png").await.status, 404); // no key
    assert!(eventually(S10, || dir.path().join("radar").is_dir()).await); // the recorder made its directory
    assert!(p.term().success());
    assert!(!p.sock.exists(), "socket left behind");
}

#[tokio::test]
async fn process_publishes_traffic_available_and_keeps_the_key() {
    if !Path::new(&mosquitto_bin()).exists() {
        eprintln!("skipped: no mosquitto");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let b = Broker::start(false);
    let mut p = Proc::start(dir.path(), Some("SECRET-TOMTOM-KEY"), &b.socket);
    let g = Bus::start(BusOptions::new("gateway").socket(&b.socket), Arc::new(StateStore::new()));
    g.subscribe_state("weather", "traffic.available").await;
    assert!(p.serving().await, "never served: {}", p.log());
    assert!(eventually(S10, || g.remote().is_true("traffic.available")).await, "{}", p.log());
    assert!(g.remote().owner_ok("weather"));
    // an unknown layer never reaches upstream
    assert_eq!(get(&p.sock, "/api/traffic/flow/bogus/5/1/2.png").await.status, 404);
    assert!(p.term().success());
    assert!(eventually(S10, || !g.remote().owner_ok("weather")).await, "marked down on stop");
    assert!(!p.log().contains("SECRET-TOMTOM-KEY"));
    g.shutdown().await;
}
