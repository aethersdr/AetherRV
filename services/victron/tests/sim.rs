//! The real `aetherrv-victron` binary against the simulated Cerbo (`aetherrv_sim::cerbo`: its own Mosquitto on a
//! loopback port), with a private AetherRV bus. Assertions on the Cerbo side come from what the simulator received.
//! Skipped when Mosquitto is missing.

use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::command::ResultKind;
use aetherrv_core::state::{StateExt, StateStore, StateView};
use aetherrv_core::testutil::{Broker, eventually, mosquitto_bin};
use aetherrv_sim::cerbo::{BrokerOptions, Cerbo, CerboConfig, PrivateBroker};
use serde_json::{Value, json};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

const S10: Duration = Duration::from_secs(10);

fn have_mosquitto() -> bool {
    Path::new(&mosquitto_bin()).exists()
}

struct Service {
    child: Child,
    db: std::path::PathBuf,
}

impl Drop for Service {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGTERM);
        }
        let _ = self.child.wait();
    }
}

fn start_service(dir: &Path, bus: &Path, port: u16, extra: &str, secrets: Option<&str>) -> Service {
    let sec = dir.join("secrets.toml");
    if let Some(s) = secrets {
        std::fs::write(&sec, s).unwrap();
    }
    let cfg = dir.join("aetherrv.toml");
    std::fs::write(
        &cfg,
        format!(
            r#"secrets_file = "{sec}"
[history]
path = "{d}/history.sqlite3"
[device.victron]
kind = "victron"
host = "127.0.0.1"
port = {port}
tx_enabled = true
cabin_temp = "IAT"
outside_temp = "oat"
fridge_temp = "FRG"
freezer_temp = "FRZ"
cabin_rear_temp = "RCT"
{extra}
"#,
            sec = sec.display(),
            d = dir.display()
        ),
    )
    .unwrap();
    let log = std::fs::File::create(dir.join("victron.log")).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_aetherrv-victron"))
        .arg("-c")
        .arg(&cfg)
        .env("AETHERRV_BUS", bus)
        .env("RUST_LOG", "info")
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .unwrap();
    Service { child, db: dir.join("history.sqlite3") }
}

async fn observer(sock: &Path) -> Bus {
    let bus = Bus::start(BusOptions::new("test-observer").socket(sock), Arc::new(StateStore::new()));
    bus.subscribe_state("victron", "#").await;
    assert!(eventually(S10, || bus.connected()).await);
    bus
}

fn rows(db: &Path) -> Vec<(String, String, String, String)> {
    let c = rusqlite::Connection::open(db).unwrap();
    let mut q = c.prepare("SELECT cmd, value, outcome, detail FROM command_log ORDER BY rowid").unwrap();
    q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap().map(Result::unwrap).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn state_commands_and_reconnect_against_the_simulator() {
    if !have_mosquitto() {
        eprintln!("skipped: mosquitto missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let bus_broker = Broker::start(false);
    let mut cb = PrivateBroker::start(BrokerOptions::default()).unwrap();
    let cerbo = Cerbo::start(CerboConfig::van(), &cb).await.unwrap();
    let svc = start_service(dir.path(), &bus_broker.socket, cb.port, "", None);
    let bus = observer(&bus_broker.socket).await;
    let r = bus.remote().clone();

    // ── state: every family of topics reaches its key ──
    assert!(eventually(S10, || r.num("climate.freezer.temp") == Some(-18.0) && r.list("victron.batteries").len() == 2).await);
    assert_eq!(r.get("victron.connected"), Some(json!(true)));
    assert_eq!(r.get("victron.portal"), Some(json!("abc123")));
    assert_eq!(r.get("victron.battery.soc"), Some(json!(85.8)));
    assert_eq!(r.get("victron.battery.power"), Some(json!(-329)), "an integer stays an integer");
    assert_eq!(r.get("victron.alternator.power"), Some(Value::Null));
    assert_eq!(r.get("climate.cabin.temp"), Some(json!(20.5)));
    assert_eq!(r.get("climate.cabin.humidity"), Some(json!(44.0)));
    assert_eq!(r.get("climate.outside.temp"), Some(json!(-1.25)), "names match case-insensitively");
    assert_eq!(r.get("climate.fridge.temp"), Some(json!(3.5)));
    assert_eq!(r.get("climate.freezer.humidity"), None, "no humidity on that tag");
    assert_eq!(r.list("victron.temps").len(), 5);
    assert_eq!((r.get("climate.cabin_rear.temp"), r.get("climate.cabin_rear.humidity")), (Some(json!(19.5)), Some(json!(40.0))));
    // the Energy tiles' volts and amps: panel side of the solar charger (312 W / 38.5 V), shore, AC loads
    let va = |k: &str| r.get(k).unwrap_or(Value::Null);
    assert_eq!([va("victron.pv.panel_v"), va("victron.pv.panel_a")], [json!(38.5), json!(8.1)]);
    assert_eq!([va("victron.ac.shore_v"), va("victron.ac.shore_a"), va("victron.ac.load_v"), va("victron.ac.load_a")], [json!(0.0), json!(0.0), json!(120.1), json!(1.75)]);
    assert_eq!(r.get("victron.tanks"), Some(json!([{"name": "Fresh", "type": "fresh", "level": 62.5}])));
    assert_eq!(r.get("victron.bms.modules_online"), Some(json!(1)));
    let n = r.list("victron.notifications");
    assert_eq!(n.len(), 1);
    assert_eq!(n[0]["description"], "Low battery voltage");
    let names = r.get("n2k.names_from_cerbo").unwrap();
    assert_eq!(names["34"]["ModelName"], "Lynx Shunt 1000A VE.Can");
    assert_eq!(r.get("victron.vebus.mode"), Some(json!(3)));
    assert_eq!(r.get("victron.vebus.current_limit"), Some(json!(30.0)));
    assert_eq!(r.get("victron.mppt.name"), Some(json!("Roof")));
    assert_eq!(r.get("victron.vebus.name"), Some(json!("MultiPlus 12/3000/120-50")), "an empty CustomName falls back");
    assert_eq!(r.get("victron.controls_ok"), Some(json!(true)));
    assert_eq!(r.get("tx.enabled.victron"), Some(json!(true)));
    assert!(cerbo.reads().contains(&"keepalive".to_string()));

    // ── commands over the bus: fresh read, one write, verified ──
    let req = |cmd: &str, v: Value| {
        let (bus, cmd) = (bus.clone(), cmd.to_string());
        async move { bus.request("victron", &cmd, v, "kiosk", 3000, Duration::from_secs(20)).await }
    };
    let rep = req("inverter_mode", json!(4)).await;
    assert_eq!((rep.result, rep.detail.as_str()), (ResultKind::Ok, "confirmed by the Cerbo"));
    assert_eq!(cerbo.writes(), vec![("vebus/276/Mode".to_string(), json!(4))]);
    let log = cerbo.log();
    let w = log.iter().position(|l| l.topic == "W/abc123/vebus/276/Mode").unwrap();
    assert!(log[..w].iter().any(|l| l.topic == "R/abc123/vebus/276/Mode"), "the fresh read comes before the write");
    assert_eq!(log[w].payload, r#"{"value": 4}"#);
    assert!(eventually(S10, || r.get("victron.vebus.mode") == Some(json!(4))).await);
    let rep = req("inverter_mode", json!(4)).await;
    assert_eq!((rep.result, rep.detail.as_str()), (ResultKind::Ok, "already set"));
    let rep = req("shore_limit", json!(15.04)).await;
    assert!(rep.is_ok(), "{rep:?}");
    assert_eq!(cerbo.log().iter().rfind(|l| l.topic.starts_with("W/")).unwrap().payload, r#"{"value": 15.0}"#);
    let rep = req("mppt_mode", json!("4")).await;
    assert_eq!(rep.result, ResultKind::Refused);
    let rep = req("ack_alarms", json!(1)).await;
    assert_eq!((rep.result, rep.detail.as_str()), (ResultKind::Ok, "acknowledged on the Cerbo (buzzer off)"));
    assert_eq!(cerbo.writes().len(), 3, "mode, limit, acknowledge: nothing else written");
    let rows = rows(&svc.db);
    let outcomes: Vec<&str> = rows.iter().map(|r| r.2.as_str()).collect();
    assert_eq!(outcomes, ["ok", "noop", "ok", "rejected", "ok"]);
    assert_eq!(rows[0], ("inverter_mode".into(), "4".into(), "ok".into(), "vebus/276/Mode=4 verified".into()));
    assert_eq!(rows[2].3, "vebus/276/Ac/In/1/CurrentLimit=15.0 verified");
    assert_eq!(rows[4].1, "1");

    // ── the Cerbo's broker goes away and comes back: disconnected, then re-subscribed ──
    let port = cb.port;
    drop(cerbo);
    cb.kill();
    assert!(eventually(S10, || r.get("victron.connected") == Some(json!(false))).await);
    let cb2 = PrivateBroker::start(BrokerOptions { port, ..Default::default() }).unwrap();
    let mut van = CerboConfig::van();
    van.values.insert("system/0/Dc/Battery/Soc".into(), json!(77.0));
    let cerbo2 = Cerbo::start(van, &cb2).await.unwrap();
    assert!(eventually(Duration::from_secs(20), || r.get("victron.battery.soc") == Some(json!(77.0))).await, "data after the reconnect");
    assert_eq!(r.get("victron.connected"), Some(json!(true)));
    assert!(cerbo2.reads().contains(&"keepalive".to_string()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls_and_token_login_from_the_secrets_file() {
    if !have_mosquitto() || !Path::new("/usr/bin/openssl").exists() {
        eprintln!("skipped: mosquitto or openssl missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let bus_broker = Broker::start(false);
    let auth = Some(("token/sim".to_string(), "not-a-real-secret".to_string()));
    let cb = PrivateBroker::start(BrokerOptions { tls: true, auth, ..Default::default() }).unwrap();
    let _cerbo = Cerbo::start(CerboConfig::van(), &cb).await.unwrap();
    let secrets = "victron_mqtt_user = \"token/sim\"\nvictron_mqtt_password = \"not-a-real-secret\"\n";
    let _svc = start_service(dir.path(), &bus_broker.socket, cb.port, "tls = true", Some(secrets));
    let bus = observer(&bus_broker.socket).await;
    let r = bus.remote().clone();
    assert!(eventually(S10, || r.get("victron.battery.soc") == Some(json!(85.8))).await, "data over TLS with the token login");
    let log = std::fs::read_to_string(dir.path().join("victron.log")).unwrap();
    assert!(!log.contains("not-a-real-secret"), "the password never reaches the log");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_password_never_connects() {
    if !have_mosquitto() {
        eprintln!("skipped: mosquitto missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let bus_broker = Broker::start(false);
    let cb = PrivateBroker::start(BrokerOptions { auth: Some(("u".into(), "right".into())), ..Default::default() }).unwrap();
    let _cerbo = Cerbo::start(CerboConfig::van(), &cb).await.unwrap();
    let _svc = start_service(dir.path(), &bus_broker.socket, cb.port, "", Some("victron_mqtt_user = \"u\"\nvictron_mqtt_password = \"wrong\"\n"));
    let bus = observer(&bus_broker.socket).await;
    let r = bus.remote().clone();
    assert!(eventually(S10, || r.get("victron.connected") == Some(json!(false))).await);
    assert_eq!(r.get("victron.portal"), None);
    let rep = bus.request("victron", "inverter_mode", json!(4), "kiosk", 3000, Duration::from_secs(10)).await;
    assert_eq!((rep.result, rep.detail.as_str()), (ResultKind::Refused, "not connected to the Cerbo"));
}
