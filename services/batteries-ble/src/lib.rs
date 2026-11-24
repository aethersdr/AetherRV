//! `aetherrv-batteries-ble`: the EPOCH batteries over Bluetooth LE (RoyPow BMS), READ-ONLY. Port of the connection
//! half of `src/aetherrv/adapters/epoch_ble.py` (`EpochBleAdapter`); the protocol (queries, frame assembly, decode,
//! the `batteries.ble*` keys) is `proto_roypow`.
//!
//! One persistent connection per battery, polled about once a second (owner choice 2026-11-21, like the phone
//! app; the modules take a single connection, so the app can't connect meanwhile):
//!
//! * **Connecting is serialized** (one connect lock for all batteries: BlueZ copes badly with simultaneous
//!   attempts).
//! * **First contact always fails** on these modules: bluetoothd reads the GAP "Peripheral Preferred Connection
//!   Parameters" (2a04), which they never answer (30 s ATT timeout). Within BlueZ's 30 s TemporaryTimeout those are
//!   cached, so an *immediate* reconnect works in ~2 s (INTEGRATION_PLAN.md §40). Hence: up to 3 attempts, 1 s
//!   apart, inside the connect lock, without rescanning between them.
//! * A missed reply (3 s) or a dropped link → disconnect and reconnect from scratch, after a backoff of 2 s
//!   doubling to 30 s; the device is rescanned (BlueZ forgets unconnected devices after 30 s).
//! * At start, links a previous process left up are dropped (the modules don't advertise while linked).
//!
//! BlueZ sits behind the [`Ble`] / [`Link`] traits: [`bluez::BlueZ`] is the real one (bluer, D-Bus), and the tests
//! drive the same logic with a mock. Only the three read queries are ever written.

pub mod bluez;

use aetherrv_core::config::{Cfg, truthy};
use aetherrv_core::pyfmt::py_str;
use aetherrv_core::state::StateStore;
use proto_roypow::{Battery, FrameAssembler, Record, decode, poll_cmds, query};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

/// Seconds to wait for a reply before dropping the link and reconnecting (Python `REPLY`).
pub const REPLY: Duration = Duration::from_secs(3);
/// Scan time for a battery that isn't cached (Python `find_device_by_address(addr, timeout=12)`).
pub const SCAN: Duration = Duration::from_secs(12);
/// Connect timeout (Python `BleakClient(dev, timeout=20)`).
pub const CONNECT: Duration = Duration::from_secs(20);
/// Settle time after connecting, before the notify subscription (Python 0.8 s).
pub const SETTLE: Duration = Duration::from_millis(800);
/// Pause between open attempts: short, to stay inside BlueZ's 30 s cache window (Python 1.0 s).
pub const RETRY: Duration = Duration::from_secs(1);
/// Open attempts per session (Python `range(3)`).
pub const ATTEMPTS: usize = 3;
/// Clearing a stale link may take this long (Python `wait_for(proc.wait(), 5)`).
pub const CLEAR: Duration = Duration::from_secs(5);

/// An error as Python shows it in a battery's `error`: `"<ExceptionName>: <message>"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BleError {
    /// Python's exception type name (`TimeoutError`, `ConnectionError`) or `BlueZError` for a D-Bus error.
    pub kind: &'static str,
    pub msg: String,
}

impl BleError {
    pub fn timeout(msg: impl Into<String>) -> Self {
        BleError { kind: "TimeoutError", msg: msg.into() }
    }
    pub fn connection(msg: impl Into<String>) -> Self {
        BleError { kind: "ConnectionError", msg: msg.into() }
    }
    pub fn bluez(msg: impl Into<String>) -> Self {
        BleError { kind: "BlueZError", msg: msg.into() }
    }
    /// The `error` field: `f"{type(e).__name__}: {e}"[:120]`.
    pub fn row_text(&self) -> String {
        format!("{}: {}", self.kind, self.msg).chars().take(120).collect()
    }
}

impl std::fmt::Display for BleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

/// The BlueZ side (Python: `BleakScanner` + `BleakClient` + `bluetoothctl`).
pub trait Ble: Send + Sync + 'static {
    type Link: Link;
    /// Scan until `addr` advertises; `false` when it isn't seen within `timeout`
    /// (`BleakScanner.find_device_by_address`).
    fn find(&self, addr: &str, timeout: Duration) -> impl Future<Output = Result<bool, BleError>> + Send;
    /// Connect, with the GATT services resolved (`BleakClient.connect()`).
    fn connect(&self, addr: &str, timeout: Duration) -> impl Future<Output = Result<Self::Link, BleError>> + Send;
    /// Drop whatever link BlueZ holds to `addr`, ignoring errors (`bluetoothctl disconnect`, and
    /// `client.disconnect()` after a failed connect).
    fn disconnect(&self, addr: &str) -> impl Future<Output = ()> + Send;
}

/// One connection to one battery.
pub trait Link: Send + 'static {
    /// StartNotify on the UART characteristic (`start_notify(..., use_start_notify=True)`): its notifications.
    fn start_notify(&mut self) -> impl Future<Output = Result<mpsc::UnboundedReceiver<Vec<u8>>, BleError>> + Send;
    /// Write without response (`write_gatt_char(UART, data, response=False)`).
    fn write(&mut self, data: &[u8]) -> impl Future<Output = Result<(), BleError>> + Send;
    /// Becomes `true` when the link drops (`disconnected_callback`).
    fn lost(&self) -> watch::Receiver<bool>;
    fn disconnect(&mut self) -> impl Future<Output = ()> + Send;
}

/// What the service knows about one battery: the published record plus the last good frame per command, kept
/// across sessions (Python `rec["msgs"]`).
#[derive(Debug, Clone, Default)]
pub struct Rec {
    pub record: Record,
    pub msgs: BTreeMap<u8, Vec<u8>>,
}

/// The configured batteries (`[device.<name>]` kind = "epoch_ble", enabled).
#[derive(Debug, Clone)]
pub struct Settings {
    pub batteries: Vec<Battery>,
    /// Seconds between polls (`max(0.5, interval)`).
    pub period: f64,
}

impl Settings {
    /// As Python's `build()`: the first enabled `kind = "epoch_ble"` device (`None` if there is none).
    pub fn from_config(cfg: &Cfg) -> anyhow::Result<Option<Settings>> {
        let Some((name, d)) =
            cfg.at("device").entries().into_iter().find(|(_, d)| d.str("kind", "") == "epoch_ble" && d.bool("enabled", false))
        else {
            return Ok(None);
        };
        let mut batteries = Vec::new();
        for b in d.list("battery") {
            let addr = b.get("address").map(py_str).ok_or_else(|| anyhow::anyhow!("[device.{name}] battery without an address"))?;
            // Python: str(b.get("name") or b["address"]), str(b.get("serial", ""))
            let bname = b.get("name").filter(|v| truthy(v)).map(py_str);
            let serial = b.get("serial").map(py_str);
            batteries.push(Battery::new(bname.as_deref(), &addr, serial.as_deref()));
        }
        Ok(Some(Settings { batteries, period: proto_roypow::poll_period(d.f64("interval", 1.0)) }))
    }
}

/// The service core, generic over the BlueZ layer.
pub struct Batteries<B: Ble> {
    pub ble: B,
    pub batteries: Vec<Battery>,
    pub period: Duration,
    pub state: Arc<StateStore>,
    recs: Mutex<HashMap<String, Rec>>,
    /// Batteries seen by a scan and not failed since (Python `_DEVICES`).
    cache: Mutex<HashSet<String>>,
    /// BlueZ copes badly with simultaneous connection attempts.
    connect_lock: tokio::sync::Mutex<()>,
    /// Wall clock (Python `time.time()`); tests drive it from tokio's paused clock.
    wall: Box<dyn Fn() -> f64 + Send + Sync>,
}

impl<B: Ble> Batteries<B> {
    pub fn new(ble: B, s: Settings, state: Arc<StateStore>) -> Arc<Self> {
        Self::with_clock(ble, s, state, Box::new(aetherrv_core::clock::now_s))
    }

    pub fn with_clock(ble: B, s: Settings, state: Arc<StateStore>, wall: Box<dyn Fn() -> f64 + Send + Sync>) -> Arc<Self> {
        Arc::new(Batteries {
            ble,
            batteries: s.batteries,
            period: Duration::from_secs_f64(s.period),
            state,
            recs: Mutex::new(HashMap::new()),
            cache: Mutex::new(HashSet::new()),
            connect_lock: tokio::sync::Mutex::new(()),
            wall,
        })
    }

    fn recs(&self) -> std::sync::MutexGuard<'_, HashMap<String, Rec>> {
        self.recs.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// What is known about one battery (tests, diagnostics).
    pub fn rec(&self, addr: &str) -> Option<Rec> {
        self.recs().get(addr).cloned()
    }

    /// The `batteries.ble*` keys (Python `publish()`).
    pub fn publish(&self) {
        let now = (self.wall)();
        let last: HashMap<String, Record> = self.recs().iter().map(|(k, r)| (k.clone(), r.record.clone())).collect();
        self.state.update(proto_roypow::publish(&self.batteries, &last, now));
    }

    /// Connect + subscribe (Python `_open`, lines 167-194): up to 3 attempts 1 s apart, the device scanned only if
    /// it isn't cached; not advertising ends the session at once.
    async fn open(&self, b: &Battery) -> Result<(B::Link, mpsc::UnboundedReceiver<Vec<u8>>), BleError> {
        let addr = &b.address;
        let mut last = None;
        for attempt in 0..ATTEMPTS {
            let cached = self.cache().contains(addr);
            if !cached {
                if !self.ble.find(addr, SCAN).await? {
                    return Err(BleError::connection("not advertising (another device connected?)"));
                }
                self.cache().insert(addr.clone());
            }
            match self.ble.connect(addr, CONNECT).await {
                Ok(mut link) => {
                    tokio::time::sleep(SETTLE).await;
                    match link.start_notify().await {
                        Ok(rx) => return Ok((link, rx)),
                        Err(e) => {
                            tracing::debug!("epoch {}: open attempt {}: {e}", b.name, attempt + 1);
                            link.disconnect().await;
                            last = Some(e);
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!("epoch {}: open attempt {}: {e}", b.name, attempt + 1);
                    self.ble.disconnect(addr).await;
                    last = Some(e);
                }
            }
            tokio::time::sleep(RETRY).await; // stay inside BlueZ's 30 s cache window
        }
        Err(last.unwrap_or_else(|| BleError::connection("connect failed")))
    }

    /// One connection's life (Python `_session`, lines 196-230): connect (serialized), then poll until a reply is
    /// missed or the link drops. Always ends with the error that ended it.
    pub async fn session(&self, b: &Battery) -> BleError {
        self.recs().entry(b.address.clone()).or_default();
        let opened = {
            let _g = self.connect_lock.lock().await;
            self.open(b).await
        };
        let (mut link, mut rx) = match opened {
            Ok(x) => x,
            Err(e) => return e,
        };
        self.set(&b.address, |r| r.record.connected = true);
        tracing::info!("epoch {}: connected", b.name);
        let err = self.poll(b, &mut link, &mut rx).await;
        self.set(&b.address, |r| r.record.connected = false);
        link.disconnect().await;
        err
    }

    fn set(&self, addr: &str, f: impl FnOnce(&mut Rec)) {
        f(self.recs().entry(addr.to_string()).or_default());
    }

    async fn poll(&self, b: &Battery, link: &mut B::Link, rx: &mut mpsc::UnboundedReceiver<Vec<u8>>) -> BleError {
        let mut fa = FrameAssembler::new();
        let mut lost = link.lost();
        let mut closed = false;
        let mut n = 0u64;
        while !*lost.borrow() {
            let started = Instant::now();
            for &cmd in poll_cmds(n) {
                drain(rx, &mut fa, &mut closed); // notifications that came in meanwhile are fed first, as Python's callback
                fa.msgs.remove(&cmd);
                if let Err(e) = link.write(&query(cmd)).await {
                    return e;
                }
                let end = Instant::now() + REPLY;
                loop {
                    drain(rx, &mut fa, &mut closed);
                    if fa.msgs.contains_key(&cmd) || *lost.borrow() || Instant::now() >= end {
                        break;
                    }
                    tokio::select! {
                        d = rx.recv(), if !closed => match d {
                            Some(d) => fa.feed(&d),
                            None => closed = true,
                        },
                        r = lost.changed() => if r.is_err() { break },
                        _ = tokio::time::sleep_until(end) => {}
                    }
                }
                if !fa.msgs.contains_key(&cmd) {
                    return BleError::timeout(format!("no reply to 0x{cmd:02x}"));
                }
            }
            let t = (self.wall)();
            self.set(&b.address, |r| {
                for (k, v) in &fa.msgs {
                    r.msgs.insert(*k, v.clone());
                }
                r.record.data = Some(decode(&r.msgs));
                r.record.t = Some(t);
                r.record.error = None;
            });
            n += 1;
            tokio::time::sleep(self.period.saturating_sub(started.elapsed())).await;
        }
        BleError::connection("link dropped")
    }

    /// One battery forever (Python `_battery`, lines 232-248). The backoff starts at 2 s and doubles to 30 s; as in
    /// Python it is never reset, because a session always ends with an error.
    pub async fn battery(self: Arc<Self>, i: usize) {
        let b = self.batteries[i].clone();
        let mut backoff = 2.0f64;
        loop {
            let e = self.session(&b).await;
            self.set(&b.address, |r| r.record.error = Some(e.row_text()));
            tracing::info!("epoch {}: {e}; reconnecting in {backoff:.0}s", b.name);
            self.cache().remove(&b.address); // BlueZ forgets unconnected devices after 30 s: rescan
            tokio::time::sleep(Duration::from_secs_f64(backoff)).await;
            backoff = (backoff * 2.0).min(30.0);
        }
    }

    /// A previous process may have left BlueZ links up; the modules don't advertise while linked.
    pub async fn clear_stale_links(&self) {
        for b in &self.batteries {
            let _ = tokio::time::timeout(CLEAR, self.ble.disconnect(&b.address)).await;
        }
    }

    /// Python `run()`: list the batteries at once, clear stale links, a task per battery, publish every second.
    pub async fn run(self: Arc<Self>) {
        self.publish(); // list the batteries ("connecting…") straight away
        self.clear_stale_links().await;
        let tasks: Vec<_> = (0..self.batteries.len()).map(|i| tokio::spawn(self.clone().battery(i))).collect();
        struct Abort(Vec<tokio::task::JoinHandle<()>>);
        impl Drop for Abort {
            fn drop(&mut self) {
                for t in &self.0 {
                    t.abort();
                }
            }
        }
        let _abort = Abort(tasks);
        loop {
            self.publish(); // ages tick even when nothing changed
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
}

fn drain(rx: &mut mpsc::UnboundedReceiver<Vec<u8>>, fa: &mut FrameAssembler, closed: &mut bool) {
    loop {
        match rx.try_recv() {
            Ok(d) => fa.feed(&d),
            Err(mpsc::error::TryRecvError::Empty) => return,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                *closed = true;
                return;
            }
        }
    }
}

pub struct Options {
    pub bus_socket: Option<PathBuf>,
}

/// Run the service until SIGTERM/SIGINT.
pub async fn run(cfg: Cfg, opts: Options) -> anyhow::Result<()> {
    use aetherrv_core::bus::{Bus, BusOptions};
    let settings = Settings::from_config(&cfg)?;
    let state = Arc::new(StateStore::new());
    let mut bo = BusOptions::new(aetherrv_core::names::BATTERIES);
    if let Some(s) = opts.bus_socket {
        bo = bo.socket(s);
    }
    let bus = Bus::start(bo, state.clone());
    let task = match settings {
        Some(s) if !s.batteries.is_empty() => {
            let ble = bluez::BlueZ::new();
            tracing::info!("epoch: {} batteries, polling every {:.1} s", s.batteries.len(), s.period);
            Some(tokio::spawn(Batteries::new(ble, s, state.clone()).run()))
        }
        _ => {
            tracing::warn!("epoch: no enabled [device.*] with kind = \"epoch_ble\" and batteries; idle");
            None
        }
    };
    aetherrv_core::notify::ready();
    aetherrv_core::notify::spawn_runtime_watchdog();
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = term.recv() => {},
        _ = tokio::signal::ctrl_c() => {},
    }
    aetherrv_core::notify::stopping();
    if let Some(t) = task {
        t.abort();
    }
    bus.shutdown().await;
    Ok(())
}
