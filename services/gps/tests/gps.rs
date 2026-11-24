//! The listener on a private broker: NMEA datagrams on loopback → `location.*`, and the fix goes stale. (The
//! parsing tests, tests/test_nmea.py, are ported in crates/proto-nmea.)

use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::config::Cfg;
use aetherrv_core::state::{StateStore, StateView};
use aetherrv_core::testutil::{Broker, eventually};
use aetherrv_gps::Options;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

const GGA: &str = "$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47";
const RMC: &str = "$GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W*6A";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn udp_sentences_become_location_keys_and_the_fix_goes_stale() {
    let broker = Broker::start(false);
    let sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = sock.local_addr().unwrap().port();
    let opts = Options {
        bus_socket: Some(broker.socket.clone()),
        enabled: true,
        bind: "127.0.0.1".into(),
        port,
        source: "peplink".into(),
        stale_after: 1.0,
        stale_check: Duration::from_millis(100),
        socket: Some(sock),
    };
    let svc = tokio::spawn(aetherrv_gps::run(Cfg::empty(), opts));
    let ui = Bus::start(BusOptions::new("gateway").socket(&broker.socket), Arc::new(StateStore::new()));
    ui.subscribe_state("gps", "#").await;
    let view = ui.remote().clone();
    let tx = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    // one datagram, two lines (CRLF), as the Peplink sends them
    tx.send_to(format!("{RMC}\r\n{GGA}\r\n").as_bytes(), ("127.0.0.1", port)).unwrap();
    assert!(eventually(Duration::from_secs(10), || view.get("location.sats") == Some(json!(8))).await);
    assert_eq!(view.get("location.fix"), Some(json!(true)));
    assert_eq!(view.get("location.lat"), Some(json!(48.1173)));
    assert_eq!(view.get("location.lon"), Some(json!(11.5166667)));
    assert_eq!(view.get("location.speed"), Some(json!(41.5)));
    assert_eq!(view.get("location.alt_m"), Some(json!(545.4)));
    assert_eq!(view.get("location.source"), Some(json!("peplink NMEA")));
    assert_eq!(view.get("gps.sentences"), Some(json!(2)));
    // no more fixes: stale after stale_after
    assert!(eventually(Duration::from_secs(5), || view.get("location.fix") == Some(json!(false))).await);
    svc.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_stays_up_and_publishes_nothing() {
    let broker = Broker::start(false);
    let opts = Options { enabled: false, bus_socket: Some(broker.socket.clone()), ..Options::from_config(&Cfg::empty()) };
    let svc = tokio::spawn(aetherrv_gps::run(Cfg::empty(), opts));
    let ui = Bus::start(BusOptions::new("gateway").socket(&broker.socket), Arc::new(StateStore::new()));
    ui.subscribe_state("gps", "#").await;
    let view = ui.remote().clone();
    assert!(eventually(Duration::from_secs(10), || view.owner_ok("gps")).await, "up, so no 'gps module down'");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(view.keys().is_empty());
    assert!(!svc.is_finished());
    svc.abort();
}
