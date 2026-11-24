//! The whole service: `run()` on a private broker against the HTTPS router simulator, with the credentials in a
//! secrets file. State topics, a command over the bus, the command_log row, and no private identifier, credential,
//! session cookie or access token in anything published or logged.

use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::command::ResultKind;
use aetherrv_core::config::Cfg;
use aetherrv_core::state::{StateExt, StateStore, StateView};
use aetherrv_core::testutil::{Broker, eventually};
use aetherrv_sim::peplink::{PeplinkSim, PeplinkSimConfig};
use serde_json::json;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_on_the_bus_against_the_simulator() {
    let logs = Captured::default();
    let w = logs.clone();
    tracing_subscriber::fmt().with_max_level(tracing::Level::TRACE).with_writer(move || w.clone()).with_ansi(false).init();

    let broker = Broker::start(false);
    let sim = PeplinkSim::start(PeplinkSimConfig { frozen: true, ..Default::default() }).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("history.sqlite3");
    let secrets = dir.path().join("secrets.toml");
    std::fs::write(
        &secrets,
        "peplink_ro_user = \"viewer\"\npeplink_ro_password = \"ro-pass\"\npeplink_rw_client_id = \"sim-rw-client\"\npeplink_rw_client_secret = \"sim-rw-secret\"\n",
    )
    .unwrap();
    let cfg = Cfg::parse(&format!(
        "secrets_file = \"{}\"\n[history]\npath = \"{}\"\n[device.peplink]\nkind = \"peplink\"\nenabled = true\nhost = \"{}\"\nverify_tls = false\ntx_enabled = true\n",
        secrets.display(),
        db.display(),
        sim.host()
    ))
    .unwrap();
    let opts = aetherrv_peplink::Options { bus_socket: Some(broker.socket.clone()), db_path: Some(db.clone()), every: Duration::from_millis(300) };
    let svc = tokio::spawn(aetherrv_peplink::run(cfg, opts));

    let client = Bus::start(BusOptions::new("test-client").socket(&broker.socket), Arc::new(StateStore::new()));
    client.subscribe_all_state().await;
    let remote = client.remote().clone();
    assert!(eventually(Duration::from_secs(10), || remote.is_true("peplink.reachable") && remote.get("peplink.allowance").is_some()).await);
    assert!(remote.is_true("peplink.controls_ok"));
    assert_eq!(remote.get("peplink.active"), Some(json!("Starlink")));
    assert_eq!(remote.get("location.lat"), Some(json!(45.5)));
    assert!(eventually(Duration::from_secs(5), || remote.get("tx.enabled.peplink") == Some(json!(true))).await);

    let r = client.request("peplink", "set_wan_priority", json!({"id": 4, "priority": 1}), "test", 3000, Duration::from_secs(30)).await;
    assert_eq!((r.result, r.detail.as_str()), (ResultKind::Ok, "WWAN on 5 GHz is now P1"));
    assert_eq!(sim.with(|s| s.posts.len()), 1);
    let rows = (0..50)
        .find_map(|_| {
            std::thread::sleep(Duration::from_millis(100));
            let c = rusqlite::Connection::open(&db).ok()?;
            let r: (String, String, String, String) = c
                .query_row("SELECT cmd, value, outcome, detail FROM command_log", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .ok()?;
            Some(r)
        })
        .expect("command_log row");
    assert_eq!(rows, ("set_wan_priority".into(), r#"{"id": 4, "priority": 1}"#.into(), "ok".into(), "WWAN on 5 GHz P3→P1".into()));

    // nothing private anywhere: state topics, and every log line (TRACE, all crates) of this run
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut published = String::new();
    for k in remote.keys() {
        published += &format!("{k}={}\n", serde_json::to_string(&remote.get(&k)).unwrap());
    }
    let logged = String::from_utf8_lossy(&logs.0.lock().unwrap()).into_owned();
    for (what, text) in [("state", &published), ("logs", &logged)] {
        for s in ["SECRET", "ro-pass", "sim-rw-secret", "simsess", "simtok", "accessToken="] {
            assert!(!text.contains(s), "{s} in the {what}");
        }
    }
    assert!(logged.contains("TX allowed  peplink: set_wan_priority"), "the capture works");
    svc.abort();
}
