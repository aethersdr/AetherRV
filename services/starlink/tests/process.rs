//! The whole service: `run()` on a private broker against the gRPC simulator. State topics, a command over the bus,
//! the command_log row, and no dish identifier in anything published.

use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::command::ResultKind;
use aetherrv_core::config::Cfg;
use aetherrv_core::state::{StateExt, StateStore, StateView};
use aetherrv_core::testutil::{Broker, eventually};
use aetherrv_sim::starlink::{StarlinkSim, StarlinkSimConfig};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_on_the_bus_against_the_simulator() {
    let broker = Broker::start(false);
    let sim = StarlinkSim::start(StarlinkSimConfig { frozen: true, ..Default::default() }).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("history.sqlite3");
    let cfg = Cfg::parse(&format!(
        "[history]\npath = \"{}\"\n[device.starlink]\nkind = \"starlink\"\nenabled = true\ntarget = \"{}\"\ninterval = 0.5\ntx_enabled = true\n",
        db.display(),
        sim.target()
    ))
    .unwrap();
    let opts = aetherrv_starlink::Options { bus_socket: Some(broker.socket.clone()), db_path: Some(db.clone()) };
    let svc = tokio::spawn(aetherrv_starlink::run(cfg, opts));

    let client = Bus::start(BusOptions::new("test-client").socket(&broker.socket), Arc::new(StateStore::new()));
    client.subscribe_all_state().await;
    let remote = client.remote().clone();
    assert!(eventually(Duration::from_secs(10), || remote.is_true("starlink.reachable") && remote.get("starlink.sleep").is_some()).await);
    assert!(remote.is_true("starlink.controls_ok"));
    assert_eq!(remote.get("starlink.uptime"), Some(json!(86400)));
    assert!(eventually(Duration::from_secs(5), || remote.get("tx.enabled.starlink") == Some(json!(true))).await);
    for k in remote.keys() {
        let v = serde_json::to_string(&remote.get(&k)).unwrap();
        assert!(!v.contains("ut-sim-") && !v.contains("router-sim-"), "{k} carries a device id");
    }

    let r = client.request("starlink", "set_heating", json!("on"), "test", 3000, Duration::from_secs(30)).await;
    assert_eq!((r.result, r.detail.as_str()), (ResultKind::Ok, "heating on"));
    assert_eq!(sim.with(|s| s.set_configs.clone()), vec![json!({"snow_melt_mode": 1})]);
    assert!(eventually(Duration::from_secs(5), || remote.get("starlink.heating") == Some(json!("on"))).await);
    let r = client.request("starlink", "dish_stow", json!(null), "test", 3000, Duration::from_secs(30)).await;
    assert_eq!((r.result, r.detail.as_str()), (ResultKind::Refused, "command 'dish_stow' is not allowed"));

    // the rows, in the Python table, written by the service itself
    let rows = (0..50)
        .find_map(|_| {
            std::thread::sleep(Duration::from_millis(100));
            let c = rusqlite::Connection::open(&db).ok()?;
            let rows: Vec<(String, String, String, String)> = c
                .prepare("SELECT cmd, value, outcome, client FROM command_log ORDER BY t")
                .ok()?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .ok()?
                .filter_map(Result::ok)
                .collect();
            (rows.len() == 2).then_some(rows)
        })
        .expect("command_log rows");
    assert_eq!(rows[0], ("set_heating".into(), "on".into(), "ok".into(), "test".into()));
    assert_eq!(rows[1], ("dish_stow".into(), "None".into(), "rejected".into(), "test".into()));

    // the dish going away: reachable false
    sim.stop();
    assert!(eventually(Duration::from_secs(10), || remote.is_false("starlink.reachable")).await);
    svc.abort();
}
