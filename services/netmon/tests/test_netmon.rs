//! The Python netmon tests (tests/test_netmon.py), plus a loopback ping and the service on a private broker.
//! Every check here goes to 127.0.0.1 only.

use aetherrv_alerts::rules::{default_rules, r_netmon};
use aetherrv_alerts::{AlertCfg, Ctx};
use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::config::Cfg;
use aetherrv_core::state::{StateStore, StateView};
use aetherrv_core::testutil::{Broker, eventually};
use aetherrv_netmon::{NetMon, Options, Target, icmp_ping, tcp_ok};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

fn mon() -> NetMon {
    let cfg = Cfg::parse(
        r#"[[netmon.targets]]
name = "IPG100"
host = "192.0.2.5"
tcp = 6544
fail_after = 2
severity = "serious"
hint = "Power-cycle the IPG100 to recover it."
"#,
    )
    .unwrap();
    NetMon::from_config(&cfg)
}

fn t(m: &mut NetMon) -> &mut Target {
    &mut m.targets[0]
}

fn snap(m: &NetMon, now: f64) -> Value {
    m.snapshot(now)[0].clone()
}

/// py: tests/test_netmon.py::test_debounce_and_reasons
#[test]
fn test_debounce_and_reasons() {
    let mut m = mon();
    NetMon::apply(t(&mut m), Some(5.0), Some(true), 0.0);
    assert_eq!(t(&mut m).up, Some(true));
    NetMon::apply(t(&mut m), Some(5.1), Some(false), 60.0); // ping ok, service dead: 1st failure, not yet down
    assert_eq!(t(&mut m).up, Some(true));
    NetMon::apply(t(&mut m), Some(5.2), Some(false), 120.0);
    let s = snap(&m, 300.0);
    assert!(s["up"] == false && s["reason"] == "service not answering" && s["down_for"] == 300);
    NetMon::apply(t(&mut m), None, None, 360.0);
    assert_eq!(snap(&m, 360.0)["reason"], "no response");
    NetMon::apply(t(&mut m), Some(4.9), Some(true), 420.0);
    assert!(snap(&m, 420.0)["up"] == true && snap(&m, 420.0)["down_for"].is_null());
    // the published shape, in Python's key order
    let row = snap(&m, 420.0);
    let keys: Vec<&String> = row.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["name", "host", "tcp", "up", "rtt_ms", "tcp_ok", "reason", "severity", "hint", "down_for"]);
}

/// py: tests/test_netmon.py::test_alert_text
#[test]
fn test_alert_text() {
    let mut m = mon();
    NetMon::apply(t(&mut m), Some(5.0), Some(true), 0.0);
    NetMon::apply(t(&mut m), Some(5.0), Some(false), 60.0);
    NetMon::apply(t(&mut m), Some(5.0), Some(false), 120.0);
    let s = StateStore::new();
    s.set("netmon.targets", m.snapshot(600.0));
    let f = r_netmon(&s, &Ctx::default()).unwrap();
    assert_eq!(f.len(), 1);
    assert!(f[0].severity.as_str() == "serious" && f[0].title == "IPG100 service not answering");
    assert!(f[0].detail.contains("10 min") && f[0].detail.contains("Power-cycle"));
    assert_eq!(f[0].detail, "192.0.2.5:6544 service not answering for 10 min. Power-cycle the IPG100 to recover it.");
    assert!(default_rules(&AlertCfg::default()).iter().any(|r| r.name == "netmon"));
}

/// py: tests/test_netmon.py::test_tcp_check_real_socket
#[tokio::test]
async fn test_tcp_check_real_socket() {
    let srv = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = srv.local_addr().unwrap().port();
    let acc = tokio::spawn(async move {
        loop {
            let Ok((s, _)) = srv.accept().await else { return };
            drop(s);
        }
    });
    assert!(tcp_ok("127.0.0.1", port, 1.0).await);
    acc.abort();
    let _ = acc.await; // the listener is dropped with the task
    assert!(!tcp_ok("127.0.0.1", port, 1.0).await);
}

fn icmp_allowed() -> bool {
    socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, Some(socket2::Protocol::ICMPV4)).is_ok()
}

/// The unprivileged ICMP echo against loopback (skipped where `ping_group_range` excludes this user).
#[test]
fn icmp_echo_to_loopback() {
    if !icmp_allowed() {
        eprintln!("skipped: no unprivileged ICMP socket for this user");
        return;
    }
    let rtt = icmp_ping("127.0.0.1", 1.0).expect("loopback answers");
    assert!((0.0..1000.0).contains(&rtt));
}

/// The service on a private broker: a loopback target whose TCP port goes away is "service not answering" after
/// `fail_after` checks.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_publishes_targets_over_the_bus() {
    if !icmp_allowed() {
        eprintln!("skipped: no unprivileged ICMP socket for this user");
        return;
    }
    let broker = Broker::start(false);
    let srv = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = srv.local_addr().unwrap().port();
    let cfg = Cfg::parse(&format!(
        "[netmon]\ninterval = 0.2\n[[netmon.targets]]\nname = \"Loop\"\nhost = \"127.0.0.1\"\ntcp = {port}\nfail_after = 2\n"
    ))
    .unwrap();
    let svc = tokio::spawn(aetherrv_netmon::run(cfg, Options { bus_socket: Some(broker.socket.clone()) }));
    let ui = Bus::start(BusOptions::new("gateway").socket(&broker.socket), Arc::new(StateStore::new()));
    ui.subscribe_state("netmon", "netmon.targets").await;
    let view = ui.remote().clone();
    let target = || view.get("netmon.targets").and_then(|v| v.get(0).cloned());
    assert!(eventually(Duration::from_secs(10), || target().is_some_and(|t| t["up"] == true && t["rtt_ms"].is_f64())).await);
    assert_eq!(target().unwrap()["severity"], "warning");
    drop(srv);
    assert!(
        eventually(Duration::from_secs(10), || target().is_some_and(|t| t["up"] == false && t["reason"] == json!("service not answering")))
            .await
    );
    svc.abort();
}
