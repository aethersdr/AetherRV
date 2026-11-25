//! The bus contract against a private Mosquitto (ARCHITECTURE.md §4).

use aetherrv_core::bus::{Bus, BusOptions, state_topic};
use aetherrv_core::command::{BoxFuture, CommandHandler, LATE_DETAIL, Reply, ResultKind};
use aetherrv_core::state::{StateExt, StateStore, StateView};
use aetherrv_core::testutil::{Broker, eventually};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

fn bus(b: &Broker, svc: &str) -> Bus {
    let mut o = BusOptions::new(svc).socket(&b.socket);
    o.clear_unset_after = Duration::from_millis(500);
    Bus::start(o, Arc::new(StateStore::new()))
}

const S: Duration = Duration::from_secs(5);

struct Counter(AtomicU32);
impl CommandHandler for Counter {
    fn handle(&self, cmd: String, value: Value, client: String) -> BoxFuture<Reply> {
        let n = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move {
            if cmd == "slow" {
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
            Reply::ok(format!("{cmd} {value} from {client} #{n}"))
        })
    }
    fn ttl_exempt(&self, cmd: &str, value: &Value) -> bool {
        cmd == "hold" && value.get("on") == Some(&json!(false))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn state_flows_with_timestamps_and_owner_health() {
    let b = Broker::start(false);
    let a = bus(&b, "victron");
    let g = bus(&b, "gateway");
    g.subscribe_all_state().await;
    a.state().set("victron.battery.soc", json!(55.5));
    assert!(eventually(S, || g.remote().num("victron.battery.soc") == Some(55.5)).await);
    let (owner, st) = g.remote().stamped("victron.battery.soc", false).unwrap();
    assert_eq!(owner, "victron");
    assert!((aetherrv_core::clock::now_ms() - st.t) < 2000);
    assert!(g.remote().age("victron.battery.soc").unwrap() < 2.0);
    // the owner goes away cleanly: its keys become unknown (but stay visible raw)
    a.shutdown().await;
    assert!(eventually(S, || g.remote().get("victron.battery.soc").is_none()).await);
    assert!(g.remote().stamped("victron.battery.soc", true).is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn commands_reply_once_refuse_late_and_ignore_retained() {
    let b = Broker::start(false);
    let srv = bus(&b, "espar");
    let h = Arc::new(Counter(AtomicU32::new(0)));
    srv.serve("espar", h.clone(), None).await;
    let cli = bus(&b, "gateway");
    assert!(eventually(S, || cli.connected() && cli.remote().owner_ok("espar")).await);
    let r = cli.request("espar", "set", json!({"mode": "off"}), "kiosk", 3000, S).await;
    assert_eq!(r.result, ResultKind::Ok);
    assert!(r.detail.contains("from kiosk #1"), "{}", r.detail);
    // a late command is refused without running
    let r = cli.request("espar", "set", json!({}), "kiosk", -1, S).await;
    assert_eq!((r.result, r.detail.as_str()), (ResultKind::Refused, LATE_DETAIL));
    // ...except an exempt one (hold release)
    let r = cli.request("espar", "hold", json!({"on": false}), "kiosk", -1, S).await;
    assert_eq!(r.result, ResultKind::Ok);
    assert_eq!(h.0.load(Ordering::SeqCst), 2);
    // a retained command (published by someone misbehaving) is never acted on
    let raw = bus(&b, "rogue");
    raw.state().set("x", json!(1));
    let _ = raw; // (publishing a retained cmd needs a raw client; see below)
    let mut o = rumqttc::v5::MqttOptions::new("raw", b.socket.to_string_lossy(), 0);
    o.set_transport(rumqttc::Transport::unix());
    let (c, mut ev) = rumqttc::v5::AsyncClient::new(o, 10);
    tokio::spawn(async move { loop { if ev.poll().await.is_err() { tokio::time::sleep(Duration::from_millis(50)).await; } } });
    let req = json!({"id": "r1", "cmd": "set", "value": {}, "client": "x", "sent_ms": aetherrv_core::clock::now_ms(), "ttl_ms": 60000});
    c.publish("aetherrv/v1/cmd/espar", rumqttc::v5::mqttbytes::QoS::AtLeastOnce, true, serde_json::to_vec(&req).unwrap()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    // a new subscriber gets it as retained: serve on a fresh bus and check it never runs
    let srv2 = bus(&b, "espar2");
    let h2 = Arc::new(Counter(AtomicU32::new(0)));
    srv2.serve("espar", h2.clone(), None).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(h2.0.load(Ordering::SeqCst), 0, "retained command must be ignored");
    // a module that isn't up: refused at once, nothing sent
    let r = cli.request("nobody", "set", json!({}), "kiosk", 3000, S).await;
    assert_eq!(r.result, ResultKind::Refused);
}

#[tokio::test(flavor = "multi_thread")]
async fn broker_restart_republishes_with_original_t() {
    let mut b = Broker::start(false);
    let a = bus(&b, "victron");
    let g = bus(&b, "gateway");
    g.subscribe_all_state().await;
    a.state().set("victron.battery.soc", json!(42));
    assert!(eventually(S, || g.remote().num("victron.battery.soc") == Some(42.0)).await);
    let t0 = g.remote().stamped("victron.battery.soc", true).unwrap().1.t;
    b.kill(); // no persistence: the broker forgets everything
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!a.connected());
    a.state().set("victron.battery.voltage", json!(13.1)); // set while the bus is down
    b.restart();
    assert!(eventually(Duration::from_secs(10), || g.remote().num("victron.battery.voltage") == Some(13.1)).await);
    assert_eq!(g.remote().stamped("victron.battery.soc", true).unwrap().1.t, t0, "a republish keeps the original t");
    assert!(a.connects() >= 2);
    assert!(a.last_contact_boottime().is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_retained_keys_of_a_previous_process_are_cleared() {
    let b = Broker::start(false);
    {
        let old = bus(&b, "derive");
        old.state().set("climate.cabin.temp", json!(20.0));
        old.state().set("energy.dc.load", json!(5.0));
        tokio::time::sleep(Duration::from_millis(400)).await;
        old.shutdown().await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let new = bus(&b, "derive");
    new.state().set("energy.dc.load", json!(6.0)); // still published; climate.cabin.temp is not any more
    let g = bus(&b, "gateway");
    g.subscribe_all_state().await;
    assert!(eventually(S, || g.remote().stamped("climate.cabin.temp", true).is_none()
        && g.remote().num("energy.dc.load") == Some(6.0)).await);
    let _ = state_topic("derive", "x");
}

/// The production ACL (packaging/etc/mosquitto.acl): a service may publish only its own state (bug containment).
#[tokio::test(flavor = "multi_thread")]
async fn acl_keeps_each_service_in_its_own_namespace() {
    let acl = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/etc/mosquitto.acl");
    let b = Broker::with_acl(&acl);
    let g = bus(&b, "gateway");
    g.subscribe_all_state().await;
    let v = bus(&b, "victron");
    v.state().set("victron.battery.soc", json!(50));
    assert!(eventually(S, || g.remote().num("victron.battery.soc") == Some(50.0)).await, "own namespace works");
    // a buggy victron publishing into espar's namespace: dropped by the broker
    let mut o = rumqttc::v5::MqttOptions::new("victron2", b.socket.to_string_lossy(), 0);
    o.set_transport(rumqttc::Transport::unix());
    let (c, mut ev) = rumqttc::v5::AsyncClient::new(o, 10);
    tokio::spawn(async move { loop { if ev.poll().await.is_err() { tokio::time::sleep(Duration::from_millis(50)).await; } } });
    c.publish("aetherrv/v1/state/espar/climate.espar.state", rumqttc::v5::mqttbytes::QoS::AtLeastOnce, true,
              serde_json::to_vec(&json!({"v": "heating", "t": 1})).unwrap()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(g.remote().stamped("climate.espar.state", true).is_none(), "another service's namespace is refused");
    // commands and replies still flow
    let srv = bus(&b, "espar");
    srv.serve("espar", Arc::new(Counter(AtomicU32::new(0))), None).await;
    assert!(eventually(S, || g.remote().owner_ok("espar")).await);
    let r = g.request("espar", "set", json!({}), "kiosk", 3000, S).await;
    assert_eq!(r.result, ResultKind::Ok, "{r:?}");
}

/// A tool (`aetherrv-ctl`, the isolation and soak runners) sends commands and reads state like anything else, but
/// never shows as a module: no status, no heartbeat, nothing retained after it exits.
#[tokio::test(flavor = "multi_thread")]
async fn a_tool_is_never_listed_as_a_module() {
    let b = Broker::start(false);
    let g = bus(&b, "gateway");
    g.subscribe_all_state().await;
    let es = bus(&b, "espar");
    es.serve("espar", Arc::new(Counter(AtomicU32::new(0))), None).await;
    let t = Bus::start(BusOptions::new("ctl-4242").socket(&b.socket).tool(), Arc::new(StateStore::new()));
    assert!(eventually(S, || g.remote().owners().iter().any(|(n, up, _)| n == "espar" && *up)).await);
    assert!(eventually(S, || t.connected() && t.remote().owner_ok("espar")).await, "a tool sees the services' health");
    let r = t.request("espar", "set", json!({"mode": "off"}), "kiosk", 3000, S).await;
    assert_eq!(r.result, ResultKind::Ok, "a tool's command is answered");
    assert!(!g.remote().owners().iter().any(|(n, _, _)| n == "ctl-4242"), "not a module while it runs");
    t.shutdown().await;
    drop(t);
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(!g.remote().owners().iter().any(|(n, _, _)| n == "ctl-4242"), "nothing left behind once it has gone");
    let late = bus(&b, "late-observer");
    late.subscribe_all_state().await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(!late.remote().owners().iter().any(|(n, _, _)| n == "ctl-4242"), "no retained status for a newcomer either");
    assert!(late.remote().owners().iter().any(|(n, _, _)| n == "espar"));
}
