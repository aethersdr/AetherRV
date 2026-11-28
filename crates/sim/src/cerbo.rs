//! A simulated Victron Cerbo GX (Venus OS dbus-flashmq) on a private Mosquitto.
//!
//! * [`PrivateBroker`] starts a Mosquitto of its own on a loopback TCP port (optionally TLS with a throw-away
//!   self-signed certificate, optionally a username/password). It never attaches to an existing listener, so it
//!   can't be the system broker or a real Cerbo; ports 1883 and 8883 are refused outright.
//! * [`Cerbo`] connects to it as a client and behaves like Venus OS:
//!   - `N/<portal>/system/0/Serial` is retained, so a client learns the portal id when it subscribes;
//!   - `R/<portal>/keepalive` republishes every value (a full dump) and keeps change notifications flowing for
//!     60 s;
//!   - `R/<portal>/<path>` republishes that one path (no answer for an unknown path);
//!   - `W/<portal>/<path>` `{"value": v}` changes the model when the path is writable (MultiPlus mode, shore
//!     current limit, MPPT mode, the alarm acknowledge) and publishes the new value(s), so a control verifies.
//!
//! Every `R/` and `W/` is logged with its topic and raw payload (and to a JSON-lines file when configured), so a
//! test can assert on exactly what a client sent.

use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use serde_json::{Map, Value, json};
use std::io::Write as _;
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The portal id of the simulated Cerbo (the one in the Python tests).
pub const PORTAL: &str = "abc123";

/// What the private broker should look like to the service.
#[derive(Debug, Clone, Default)]
pub struct BrokerOptions {
    /// The service-facing port (0 = a free one).
    pub port: u16,
    /// TLS on the service-facing port (a self-signed certificate made with `openssl`).
    pub tls: bool,
    /// Require this username/password (a Venus OS >= 3.80 token pairing).
    pub auth: Option<(String, String)>,
}

/// A Mosquitto of our own on 127.0.0.1. Killed on drop.
pub struct PrivateBroker {
    pub dir: tempfile::TempDir,
    /// The port the service connects to (TLS when `tls`).
    pub port: u16,
    /// A plain loopback port for the simulator itself (the same as `port` without TLS).
    pub sim_port: u16,
    pub tls: bool,
    pub auth: Option<(String, String)>,
    child: Option<Child>,
}

fn free_port() -> anyhow::Result<u16> {
    Ok(TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?.local_addr()?.port())
}

/// The standard MQTT ports: a system broker (or a real Cerbo, through a tunnel) may be there.
pub fn refuse_port(port: u16) -> anyhow::Result<()> {
    if port == 1883 || port == 8883 {
        anyhow::bail!("refusing port {port}: the Cerbo simulator never uses the standard MQTT ports");
    }
    Ok(())
}

impl PrivateBroker {
    pub fn start(opts: BrokerOptions) -> anyhow::Result<PrivateBroker> {
        refuse_port(opts.port)?;
        let dir = tempfile::tempdir()?;
        let port = if opts.port == 0 { free_port()? } else { opts.port };
        let sim_port = if opts.tls { free_port()? } else { port };
        let mut conf = String::from("per_listener_settings false\npersistence false\nlog_dest none\n");
        match &opts.auth {
            Some((user, pass)) => {
                let pw = dir.path().join("passwd");
                let st = Command::new("mosquitto_passwd")
                    .arg("-c")
                    .arg("-b")
                    .arg(&pw)
                    .arg(user)
                    .arg(pass)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()?;
                anyhow::ensure!(st.success(), "mosquitto_passwd failed");
                conf += &format!("allow_anonymous false\npassword_file {}\n", pw.display());
            }
            None => conf += "allow_anonymous true\n",
        }
        conf += &format!("listener {port} 127.0.0.1\n");
        if opts.tls {
            let (cert, key) = (dir.path().join("cert.pem"), dir.path().join("key.pem"));
            let st = Command::new("openssl")
                .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=cerbo-sim", "-keyout"])
                .arg(&key)
                .arg("-out")
                .arg(&cert)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?;
            anyhow::ensure!(st.success(), "openssl could not make a certificate");
            conf += &format!("certfile {}\nkeyfile {}\n", cert.display(), key.display());
            conf += &format!("listener {sim_port} 127.0.0.1\n");
        }
        let cfg = dir.path().join("mosquitto.conf");
        std::fs::write(&cfg, conf)?;
        let child = Command::new(mosquitto_bin()).arg("-c").arg(&cfg).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
        let mut b = PrivateBroker { dir, port, sim_port, tls: opts.tls, auth: opts.auth, child: Some(child) };
        b.wait_listening(Duration::from_secs(5))?;
        Ok(b)
    }

    fn wait_listening(&mut self, timeout: Duration) -> anyhow::Result<()> {
        let end = Instant::now() + timeout;
        loop {
            if let Some(c) = self.child.as_mut()
                && let Some(st) = c.try_wait()?
            {
                anyhow::bail!("mosquitto exited ({st}): port {} or {} taken?", self.port, self.sim_port);
            }
            let up = |p: u16| TcpStream::connect((Ipv4Addr::LOCALHOST, p)).is_ok();
            if up(self.port) && up(self.sim_port) {
                return Ok(());
            }
            if Instant::now() > end {
                anyhow::bail!("mosquitto did not start listening on 127.0.0.1:{}", self.port);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Kill the broker (the service sees its connection drop).
    pub fn kill(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Drop for PrivateBroker {
    fn drop(&mut self) {
        self.kill();
    }
}

fn mosquitto_bin() -> String {
    for p in ["/usr/bin/mosquitto", "/usr/sbin/mosquitto"] {
        if Path::new(p).exists() {
            return p.to_string();
        }
    }
    "mosquitto".to_string()
}

/// The simulated Cerbo's configuration.
#[derive(Debug, Clone)]
pub struct CerboConfig {
    pub portal: String,
    /// path (after `N/<portal>/`) → value, in publish order.
    pub values: Map<String, Value>,
    /// Apply `W/` writes to writable paths (false: a Cerbo that ignores writes, for "unverified").
    pub apply_writes: bool,
    /// Answer `R/<path>` reads (false: a Cerbo that never reports, for "no fresh value").
    pub answer_reads: bool,
    /// Paths a `W/` may change.
    pub writable: Vec<String>,
    /// How long change notifications keep flowing after a keepalive (Venus OS: 60 s).
    pub keepalive_s: f64,
    /// JSON-lines log of every `R/` and `W/`.
    pub log: Option<PathBuf>,
}

impl Default for CerboConfig {
    fn default() -> Self {
        Self::van()
    }
}

/// The van model (path → value, in publish order). All values are made up.
const VAN: &str = r#"{
    "system/0/Serial": "abc123",
    "system/0/Dc/Battery/Soc": 85.8,
    "system/0/Dc/Battery/Voltage": 13.28,
    "system/0/Dc/Battery/Current": -24.8,
    "system/0/Dc/Battery/Power": -329,
    "system/0/Dc/Battery/TimeToGo": 51840.0,
    "system/0/Dc/Battery/Temperature": 18.5,
    "system/0/Dc/Battery/ConsumedAmphours": -261.2,
    "system/0/Dc/Pv/Power": 312,
    "system/0/Dc/System/Power": 120,
    "system/0/Dc/Alternator/Power": null,
    "system/0/Dc/Vebus/Power": -45,
    "system/0/Ac/Consumption/L1/Power": 210,
    "system/0/Ac/Grid/L1/Power": 0,
    "system/0/SystemState/State": 9,
    "system/0/Batteries": [{"id": "com.victronenergy.battery.socketcan_can1", "soc": 85.8}],
    "battery/279/ProductName": "Lynx Shunt 1000A VE.Can",
    "battery/279/CustomName": "House bank",
    "battery/279/Soc": 85.8,
    "battery/279/Dc/0/Voltage": 13.28,
    "battery/279/Dc/0/Current": -24.8,
    "battery/279/Dc/0/Power": -329.3,
    "battery/279/ConsumedAmphours": -261.2,
    "battery/279/TimeToGo": 51840.0,
    "battery/279/Alarms/LowVoltage": 0,
    "battery/279/Mode": 3,
    "battery/512/ProductName": "Lithium BMS",
    "battery/512/Soc": 86,
    "battery/512/Soh": 100,
    "battery/512/Dc/0/Voltage": 13.3,
    "battery/512/Dc/0/Current": -6.2,
    "battery/512/Dc/0/Temperature": 17.0,
    "battery/512/InstalledCapacity": 1840.0,
    "battery/512/System/NrOfModulesOnline": 1,
    "battery/512/System/NrOfModulesOffline": 3,
    "battery/512/System/MinCellVoltage": 3.321,
    "battery/512/System/MaxCellVoltage": 3.325,
    "battery/512/Info/MaxChargeCurrent": 230.0,
    "battery/512/Info/MaxChargeVoltage": 14.2,
    "battery/512/Alarms/CellImbalance": 0,
    "battery/512/Alarms/LowVoltage": 1,
    "vebus/276/ProductName": "MultiPlus 12/3000/120-50",
    "vebus/276/CustomName": "",
    "vebus/276/Mode": 3,
    "vebus/276/ModeIsAdjustable": 1,
    "vebus/276/State": 9,
    "vebus/276/Connected": 1,
    "vebus/276/Ac/In/1/CurrentLimit": 30.0,
    "vebus/276/Ac/In/1/CurrentLimitIsAdjustable": 1,
    "vebus/276/Ac/Out/L1/P": 210,
    "vebus/276/Ac/ActiveIn/Connected": 0,
    "vebus/276/Ac/ActiveIn/L1/P": 0,
    "solarcharger/277/ProductName": "SmartSolar MPPT 100/50",
    "solarcharger/277/CustomName": "Roof",
    "solarcharger/277/Mode": 1,
    "solarcharger/277/State": 3,
    "solarcharger/277/Yield/Power": 312,
    "temperature/20/ProductName": "RuuviTag",
    "temperature/20/CustomName": "IAT",
    "temperature/20/TemperatureType": 2,
    "temperature/20/Temperature": 20.5,
    "temperature/20/Humidity": 44.0,
    "temperature/21/ProductName": "RuuviTag",
    "temperature/21/CustomName": "OAT",
    "temperature/21/TemperatureType": 2,
    "temperature/21/Temperature": -1.25,
    "temperature/21/Humidity": 81.5,
    "temperature/22/ProductName": "RuuviTag",
    "temperature/22/CustomName": "FRZ",
    "temperature/22/TemperatureType": 2,
    "temperature/22/Temperature": -18.0,
    "temperature/23/ProductName": "RuuviTag",
    "temperature/23/CustomName": "FRG",
    "temperature/23/TemperatureType": 2,
    "temperature/23/Temperature": 3.5,
    "temperature/23/Humidity": 60.0,
    "temperature/28/ProductName": "RuuviTag",
    "temperature/28/CustomName": "RCT",
    "temperature/28/TemperatureType": 2,
    "temperature/28/Temperature": 19.5,
    "temperature/28/Humidity": 40.0,
    "tank/40/CustomName": "Fresh",
    "tank/40/FluidType": 1,
    "tank/40/Level": 62.5,
    "platform/0/Notifications/0/Active": 1,
    "platform/0/Notifications/0/Type": 0,
    "platform/0/Notifications/0/Description": "Low battery voltage",
    "platform/0/Notifications/0/DeviceName": "Lithium BMS",
    "platform/0/Notifications/0/Acknowledged": 0,
    "platform/0/Notifications/0/Silenced": 0,
    "platform/0/Notifications/0/DateTime": 1795000000,
    "platform/0/Notifications/1/Active": 0,
    "platform/0/Notifications/1/Type": 1,
    "platform/0/Notifications/1/Description": "High temperature",
    "platform/0/Notifications/NumberOfUnAcknowledgedAlarms": 0,
    "platform/0/Notifications/NumberOfUnAcknowledgedWarnings": 1,
    "vecan/can0/Devices/0/Nad": 0,
    "vecan/can0/Devices/0/ModelName": "Cerbo GX",
    "vecan/can0/Devices/0/Manufacturer": 358,
    "vecan/can0/Devices/0/Serial": "SIM0000001",
    "vecan/can0/Devices/0/FirmwareVersion": "v3.70",
    "vecan/can0/Devices/0/CustomName": "",
    "vecan/can0/Devices/0/DeviceInstance": 0,
    "vecan/can0/Devices/1/Nad": 34,
    "vecan/can0/Devices/1/ModelName": "Lynx Shunt 1000A VE.Can",
    "vecan/can0/Devices/1/Manufacturer": 358,
    "vecan/can0/Devices/1/Serial": "SIM0000002",
    "vecan/can0/Devices/1/FirmwareVersion": "v1.16",
    "vecan/can0/Devices/1/CustomName": "House bank",
    "vecan/can0/Devices/1/DeviceInstance": 279
}"#;

impl CerboConfig {
    /// A van like the one AetherRV runs in: Lynx shunt + one BMS, MultiPlus, SmartSolar, five Ruuvi tags
    /// (IAT/OAT/FRZ/FRG/RCT, typed "generic" like Ruuvis on Venus OS), a fresh tank, two VE.Can devices and one active
    /// low-battery warning. All values are made up.
    pub fn van() -> CerboConfig {
        let v: Value = serde_json::from_str(VAN).expect("the built-in van model is valid JSON");
        CerboConfig {
            portal: PORTAL.to_string(),
            values: v.as_object().cloned().unwrap_or_default(),
            apply_writes: true,
            answer_reads: true,
            writable: vec![
                "vebus/276/Mode".into(),
                "vebus/276/Ac/In/1/CurrentLimit".into(),
                "solarcharger/277/Mode".into(),
                "platform/0/Notifications/AcknowledgeAll".into(),
            ],
            keepalive_s: 60.0,
            log: None,
        }
    }

    /// Apply a JSON configuration on top of [`CerboConfig::van`]:
    /// `{"portal", "replace": bool, "values": {path: value}, "remove": [path], "apply_writes", "answer_reads",
    /// "writable": [path], "keepalive_s"}`. `null` in `values` is a value (Venus publishes "no value" as null).
    pub fn from_json(j: &Value) -> anyhow::Result<CerboConfig> {
        let mut c = CerboConfig::van();
        let o = j.as_object().ok_or_else(|| anyhow::anyhow!("the Cerbo simulator config must be a JSON object"))?;
        if let Some(p) = o.get("portal").and_then(Value::as_str) {
            c.portal = p.to_string();
            c.values.insert("system/0/Serial".into(), json!(p));
        }
        if o.get("replace").and_then(Value::as_bool) == Some(true) {
            c.values.clear();
            c.values.insert("system/0/Serial".into(), json!(c.portal));
        }
        if let Some(v) = o.get("values").and_then(Value::as_object) {
            for (k, x) in v {
                c.values.insert(k.clone(), x.clone());
            }
        }
        if let Some(r) = o.get("remove").and_then(Value::as_array) {
            for k in r.iter().filter_map(Value::as_str) {
                c.values.shift_remove(k);
            }
        }
        if let Some(b) = o.get("apply_writes").and_then(Value::as_bool) {
            c.apply_writes = b;
        }
        if let Some(b) = o.get("answer_reads").and_then(Value::as_bool) {
            c.answer_reads = b;
        }
        if let Some(w) = o.get("writable").and_then(Value::as_array) {
            c.writable = w.iter().filter_map(Value::as_str).map(str::to_string).collect();
        }
        if let Some(k) = o.get("keepalive_s").and_then(Value::as_f64) {
            c.keepalive_s = k;
        }
        Ok(c)
    }
}

/// One `R/` or `W/` the simulator received.
#[derive(Debug, Clone, PartialEq)]
pub struct Logged {
    /// Seconds since the simulator started.
    pub t: f64,
    pub topic: String,
    /// The raw payload as text (lossy UTF-8).
    pub payload: String,
    /// For a `W/`: whether it changed the model.
    pub applied: Option<bool>,
}

struct Inner {
    cfg: CerboConfig,
    values: Mutex<Map<String, Value>>,
    alive_until: Mutex<Option<Instant>>,
    client: Mutex<Option<AsyncClient>>,
    log: Mutex<Vec<Logged>>,
    file: Option<Mutex<std::fs::File>>,
    connected: AtomicBool,
    t0: Instant,
}

/// The simulated Cerbo. Stops when dropped.
pub struct Cerbo {
    inner: Arc<Inner>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Cerbo {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A written value takes the kind the path already has (a dbus double stays a float, an int an int), as Venus
/// publishes the dbus type, not what the client sent.
fn coerce(old: Option<&Value>, new: Value) -> Value {
    match (old, &new) {
        (Some(Value::Number(o)), Value::Number(n)) if o.is_f64() && !n.is_f64() => {
            n.as_f64().and_then(serde_json::Number::from_f64).map(Value::Number).unwrap_or(new)
        }
        (Some(Value::Number(o)), Value::Number(n)) if !o.is_f64() && n.is_f64() => match n.as_f64() {
            Some(f) if f.fract() == 0.0 => json!(f as i64),
            _ => new,
        },
        _ => new,
    }
}

impl Cerbo {
    /// Connect to our own broker and start answering. Only a [`PrivateBroker`] is accepted.
    pub async fn start(cfg: CerboConfig, broker: &PrivateBroker) -> anyhow::Result<Cerbo> {
        refuse_port(broker.sim_port)?;
        let file = match &cfg.log {
            Some(p) => Some(Mutex::new(std::fs::OpenOptions::new().create(true).append(true).open(p)?)),
            None => None,
        };
        let inner = Arc::new(Inner {
            values: Mutex::new(cfg.values.clone()),
            cfg,
            alive_until: Mutex::new(None),
            client: Mutex::new(None),
            log: Mutex::new(Vec::new()),
            file,
            connected: AtomicBool::new(false),
            t0: Instant::now(),
        });
        let mut mo = MqttOptions::new("cerbo-sim", "127.0.0.1", broker.sim_port);
        mo.set_keep_alive(Duration::from_secs(10));
        mo.set_clean_session(true);
        mo.set_max_packet_size(1 << 20, 1 << 20);
        if let Some((u, p)) = &broker.auth {
            mo.set_credentials(u.clone(), p.clone());
        }
        let task = tokio::spawn(run(inner.clone(), mo));
        let c = Cerbo { inner, task };
        let end = Instant::now() + Duration::from_secs(5);
        while !c.inner.connected.load(Ordering::SeqCst) {
            anyhow::ensure!(Instant::now() < end, "the Cerbo simulator could not connect to its broker");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Ok(c)
    }

    pub fn portal(&self) -> &str {
        &self.inner.cfg.portal
    }

    /// Every `R/` and `W/` received so far.
    pub fn log(&self) -> Vec<Logged> {
        self.inner.log.lock().unwrap().clone()
    }

    /// The `W/` writes: (path, value).
    pub fn writes(&self) -> Vec<(String, Value)> {
        let pre = format!("W/{}/", self.inner.cfg.portal);
        self.log()
            .into_iter()
            .filter_map(|l| {
                let path = l.topic.strip_prefix(&pre)?.to_string();
                let v = serde_json::from_str::<Value>(&l.payload).ok().and_then(|j| j.get("value").cloned()).unwrap_or(Value::Null);
                Some((path, v))
            })
            .collect()
    }

    /// The `R/` paths read (keepalives included as "keepalive").
    pub fn reads(&self) -> Vec<String> {
        let pre = format!("R/{}/", self.inner.cfg.portal);
        self.log().into_iter().filter_map(|l| l.topic.strip_prefix(&pre).map(str::to_string)).collect()
    }

    pub fn value(&self, path: &str) -> Option<Value> {
        self.inner.values.lock().unwrap().get(path).cloned()
    }

    /// Back to the configured values (published if a keepalive is active); the request log is kept.
    pub fn reset(&self) {
        *self.inner.values.lock().unwrap() = self.inner.cfg.values.clone();
        if self.inner.alive() {
            self.inner.publish_all();
        }
    }

    /// Change a value as the device would (published if a keepalive is active).
    pub fn set(&self, path: &str, v: Value) {
        self.inner.values.lock().unwrap().insert(path.to_string(), v.clone());
        if self.inner.alive() {
            self.inner.publish(path, &v);
        }
    }
}

impl Inner {
    fn alive(&self) -> bool {
        self.alive_until.lock().unwrap().is_some_and(|t| Instant::now() < t)
    }

    fn publish(&self, path: &str, v: &Value) {
        let retain = path == "system/0/Serial";
        if let Some(c) = self.client.lock().unwrap().as_ref() {
            let body = serde_json::to_vec(&json!({"value": v})).unwrap();
            let _ = c.try_publish(format!("N/{}/{path}", self.cfg.portal), QoS::AtMostOnce, retain, body);
        }
    }

    fn publish_all(&self) {
        let all: Vec<(String, Value)> = self.values.lock().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        for (k, v) in &all {
            self.publish(k, v);
        }
        let t = crate_now();
        if let Some(c) = self.client.lock().unwrap().as_ref() {
            let body = serde_json::to_vec(&json!({"value": t})).unwrap();
            let _ = c.try_publish(format!("N/{}/full_publish_completed", self.cfg.portal), QoS::AtMostOnce, false, body);
        }
    }

    fn record(&self, topic: &str, payload: &[u8], applied: Option<bool>) {
        let l = Logged { t: self.t0.elapsed().as_secs_f64(), topic: topic.to_string(), payload: String::from_utf8_lossy(payload).into_owned(), applied };
        if let Some(f) = &self.file {
            let j = json!({"t": l.t, "topic": l.topic, "payload": l.payload, "applied": l.applied});
            let _ = writeln!(f.lock().unwrap(), "{j}");
        }
        self.log.lock().unwrap().push(l);
    }

    fn on_request(&self, topic: &str, payload: &[u8]) {
        let mut it = topic.splitn(3, '/');
        let (Some(kind), Some(portal), Some(path)) = (it.next(), it.next(), it.next()) else { return };
        if portal != self.cfg.portal {
            return;
        }
        match kind {
            "R" if path == "keepalive" => {
                self.record(topic, payload, None);
                *self.alive_until.lock().unwrap() = Some(Instant::now() + Duration::from_secs_f64(self.cfg.keepalive_s));
                self.publish_all();
            }
            "R" => {
                self.record(topic, payload, None);
                if self.cfg.answer_reads
                    && let Some(v) = self.values.lock().unwrap().get(path).cloned()
                {
                    self.publish(path, &v);
                }
            }
            "W" => {
                let value = serde_json::from_slice::<Value>(payload).ok().and_then(|j| j.get("value").cloned());
                let ok = self.cfg.apply_writes && self.cfg.writable.iter().any(|w| w == path) && value.is_some();
                self.record(topic, payload, Some(ok));
                if let (true, Some(v)) = (ok, value) {
                    for (p, v) in self.apply_write(path, v) {
                        self.publish(&p, &v);
                    }
                }
            }
            _ => {}
        }
    }

    /// The model's reaction to a write → the paths that changed.
    fn apply_write(&self, path: &str, v: Value) -> Vec<(String, Value)> {
        let mut vals = self.values.lock().unwrap();
        let mut changed = Vec::new();
        let mut put = |vals: &mut Map<String, Value>, p: &str, v: Value| {
            let v = coerce(vals.get(p), v);
            if vals.get(p) != Some(&v) {
                vals.insert(p.to_string(), v.clone());
                changed.push((p.to_string(), v));
            }
        };
        if path.ends_with("Notifications/AcknowledgeAll") {
            if aetherrv_core::config::truthy(&v) {
                let idx: Vec<String> = vals
                    .keys()
                    .filter_map(|k| k.strip_prefix("platform/0/Notifications/")?.strip_suffix("/Active").map(str::to_string))
                    .collect();
                for i in idx {
                    if vals.get(&format!("platform/0/Notifications/{i}/Active")).is_some_and(aetherrv_core::config::truthy) {
                        put(&mut vals, &format!("platform/0/Notifications/{i}/Acknowledged"), json!(1));
                    }
                }
                put(&mut vals, "platform/0/Notifications/NumberOfUnAcknowledgedAlarms", json!(0));
                put(&mut vals, "platform/0/Notifications/NumberOfUnAcknowledgedWarnings", json!(0));
            }
            return changed;
        }
        put(&mut vals, path, v.clone());
        // what follows a mode change on the real devices
        if let Some(inst) = path.strip_prefix("vebus/").and_then(|r| r.strip_suffix("/Mode")) {
            let state = if v == json!(4) { 0 } else { 9 }; // off / inverting (no shore in the model)
            put(&mut vals, &format!("vebus/{inst}/State"), json!(state));
        }
        if let Some(inst) = path.strip_prefix("solarcharger/").and_then(|r| r.strip_suffix("/Mode")) {
            let state = if v == json!(4) { 0 } else { 3 }; // off / bulk
            put(&mut vals, &format!("solarcharger/{inst}/State"), json!(state));
        }
        changed
    }
}

fn crate_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

async fn run(inner: Arc<Inner>, mo: MqttOptions) {
    loop {
        let (client, mut ev) = AsyncClient::new(mo.clone(), 8192);
        loop {
            match ev.poll().await {
                Ok(Event::Incoming(Packet::ConnAck(_))) => {
                    let p = &inner.cfg.portal;
                    let _ = client.try_subscribe(format!("R/{p}/#"), QoS::AtMostOnce);
                    let _ = client.try_subscribe(format!("W/{p}/#"), QoS::AtMostOnce);
                    *inner.client.lock().unwrap() = Some(client.clone());
                    let serial = inner.values.lock().unwrap().get("system/0/Serial").cloned().unwrap_or(json!(p));
                    inner.publish("system/0/Serial", &serial);
                    inner.connected.store(true, Ordering::SeqCst);
                }
                Ok(Event::Incoming(Packet::Publish(m))) => inner.on_request(&m.topic, &m.payload),
                Ok(_) => {}
                Err(e) => {
                    tracing::debug!("cerbo-sim: broker connection: {e}");
                    inner.connected.store(false, Ordering::SeqCst);
                    *inner.client.lock().unwrap() = None;
                    break;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_keep_the_dbus_number_kind() {
        assert_eq!(coerce(Some(&json!(30.0)), json!(15)), json!(15.0));
        assert_eq!(coerce(Some(&json!(3)), json!(4.0)), json!(4));
        assert_eq!(coerce(Some(&json!(3)), json!(4.5)), json!(4.5));
        assert_eq!(coerce(None, json!("x")), json!("x"));
    }

    #[test]
    fn standard_ports_are_refused() {
        assert!(refuse_port(1883).is_err() && refuse_port(8883).is_err() && refuse_port(18830).is_ok());
        assert!(PrivateBroker::start(BrokerOptions { port: 1883, ..Default::default() }).is_err());
    }

    #[test]
    fn json_config_overrides_the_van() {
        let c = CerboConfig::from_json(&json!({"portal": "p", "values": {"vebus/276/Mode": 4}, "remove": ["tank/40/Level"], "apply_writes": false}))
            .unwrap();
        assert_eq!(c.values["system/0/Serial"], json!("p"));
        assert_eq!(c.values["vebus/276/Mode"], json!(4));
        assert!(!c.values.contains_key("tank/40/Level") && !c.apply_writes);
    }
}
