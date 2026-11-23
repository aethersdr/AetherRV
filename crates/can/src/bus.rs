//! `CanBus`: one reader per bus; fans frames out to handlers and an optional capture log; the only send path.
//!
//! The transport is a seam ([`Opener`] → [`Link`]): SocketCAN, socketcand, and [`fake`] for tests, which can
//! withhold the TX echo, refuse sends (ENOBUFS) or drop the link, because vcan always echoes and never loses an
//! ACK (ARCHITECTURE.md §12).

use crate::capture::CaptureWriter;
use crate::frame::CanFrame;
use aetherrv_core::txgate::{TxBlocked, TxGate};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Notify, mpsc};

pub type Handler = Arc<dyn Fn(&CanFrame) + Send + Sync>;
pub type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// The sending half of an open link (non-blocking: a full queue is an error).
pub trait Sender: Send + Sync {
    fn send(&self, f: &CanFrame) -> io::Result<()>;
}

/// An open link: received frames, and how to send.
pub struct Link {
    pub rx: mpsc::Receiver<io::Result<CanFrame>>,
    pub tx: Arc<dyn Sender>,
}

pub trait Opener: Send + Sync {
    fn open(&self) -> BoxFut<io::Result<Link>>;
    /// `socketcan` | `socketcand` | `fake`.
    fn kind(&self) -> &'static str;
}

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error(transparent)]
    Blocked(#[from] TxBlocked),
    #[error("[{0}] bus not open")]
    NotOpen(String),
    #[error("{0}")]
    Refused(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Debug, Clone, Default)]
pub struct BusStats {
    pub frames: u64,
    pub errors: u64,
    pub ids: HashMap<u32, u64>,
    pub last_rx: Option<f64>,
    /// Last frame another node sent (not an error frame, not our own echo).
    pub last_good: Option<f64>,
    window: VecDeque<f64>,
}

impl BusStats {
    pub fn note(&mut self, f: &CanFrame, now: f64, mono: f64) {
        self.frames += 1;
        if f.error {
            self.errors += 1;
        } else {
            *self.ids.entry(f.id).or_insert(0) += 1;
            if f.is_rx {
                self.last_good = Some(now);
            }
        }
        self.last_rx = Some(now);
        self.window.push_back(mono);
        while self.window.len() > 20_000 {
            self.window.pop_front();
        }
    }

    /// Frames per second over the last 5 s.
    pub fn rate(&mut self, mono: f64) -> f64 {
        while self.window.front().is_some_and(|t| *t < mono - 5.0) {
            self.window.pop_front();
        }
        self.window.len() as f64 / 5.0
    }
}

pub struct CanBus {
    pub name: String,
    pub channel: String,
    opener: Box<dyn Opener>,
    capture_dir: Option<PathBuf>,
    handlers: Mutex<Vec<Handler>>,
    stats: Mutex<BusStats>,
    link: Mutex<Option<Arc<dyn Sender>>>,
    connected: AtomicBool,
    kick: Notify,
    capture: Mutex<Option<CaptureWriter>>,
}

fn mono() -> f64 {
    aetherrv_core::clock::boottime_s()
}

impl CanBus {
    pub fn new(name: &str, channel: &str, opener: Box<dyn Opener>, capture_dir: Option<PathBuf>) -> Arc<Self> {
        Arc::new(CanBus {
            name: name.to_string(),
            channel: channel.to_string(),
            opener,
            capture_dir,
            handlers: Mutex::new(vec![]),
            stats: Mutex::new(BusStats::default()),
            link: Mutex::new(None),
            connected: AtomicBool::new(false),
            kick: Notify::new(),
            capture: Mutex::new(None),
        })
    }

    /// A SocketCAN bus (`own_echo`: receive our own frames once really sent).
    pub fn socketcan(name: &str, iface: &str, own_echo: bool, capture_dir: Option<PathBuf>) -> Arc<Self> {
        Self::new(name, iface, Box::new(crate::open::SocketCanOpener { iface: iface.into(), own_echo }), capture_dir)
    }

    /// A socketcand bus (remote, e.g. the Cerbo GX).
    pub fn socketcand(name: &str, host: &str, port: u16, channel: &str, capture_dir: Option<PathBuf>) -> Arc<Self> {
        Self::new(
            name,
            channel,
            Box::new(crate::open::SocketcandOpener { host: host.into(), port, channel: channel.into() }),
            capture_dir,
        )
    }

    pub fn interface(&self) -> &'static str {
        self.opener.kind()
    }

    pub fn add_handler(&self, h: Handler) {
        self.handlers.lock().unwrap().push(h);
    }

    pub fn stats(&self) -> BusStats {
        self.stats.lock().unwrap().clone()
    }

    pub fn rate(&self) -> f64 {
        self.stats.lock().unwrap().rate(mono())
    }

    /// Fakes and tests: mark the link open without running the reader.
    pub fn set_connected(&self, v: bool) {
        self.connected.store(v, Ordering::SeqCst);
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    /// The honest "is the bus usable" signal: SocketCAN = open and `IFF_UP`; socketcand = open and heard within 30 s.
    pub fn link_up(&self) -> bool {
        let c = self.is_connected();
        match self.opener.kind() {
            "socketcan" => c && crate::raw::iface_up(&self.channel),
            "socketcand" => c && self.stats.lock().unwrap().last_rx.is_some_and(|t| aetherrv_core::clock::now_s() - t < 30.0),
            _ => c,
        }
    }

    /// Drop the socket and reopen (e.g. after a driver rebind).
    pub fn reopen(&self) {
        self.kick.notify_one();
    }

    /// Feed a frame through stats, capture and handlers (the reader calls this; tests may too).
    pub fn dispatch(&self, f: &CanFrame) {
        // the frame's receive time (set by the reader; tests set it to drive time)
        let now = if f.ts > 0.0 { f.ts } else { aetherrv_core::clock::now_s() };
        self.stats.lock().unwrap().note(f, now, mono());
        if let Some(w) = self.capture.lock().unwrap().as_mut()
            && let Err(e) = w.write(f)
        {
            tracing::warn!("[{}] capture write failed: {e}", self.name);
        }
        let hs = self.handlers.lock().unwrap().clone();
        for h in hs {
            // a decoder bug must never kill the bus reader
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| h(f))).is_err() {
                tracing::error!("[{}] handler panicked on {}", self.name, f.short());
            }
        }
    }

    /// The only transmit path. Refused unless `device` has `tx_enabled`.
    pub fn send(&self, device: &str, f: &CanFrame, gate: &TxGate, quiet: bool) -> Result<(), SendError> {
        gate.check(device, &format!("{} {}", self.name, f.short()), quiet)?;
        let link = self.link.lock().unwrap().clone();
        match link {
            None => Err(SendError::NotOpen(self.name.clone())),
            Some(l) => Ok(l.send(f)?),
        }
    }

    /// Read forever; reopen on failure (cable unplugged, bus-off, interface down) with 1 → 30 s backoff.
    /// Opening runs as its own task, so a slow open (socketcand to an unreachable host) never stalls anything.
    pub async fn run(self: Arc<Self>) {
        let mut backoff = Duration::from_secs(1);
        loop {
            match self.opener.open().await {
                Ok(mut link) => {
                    *self.link.lock().unwrap() = Some(link.tx.clone());
                    if let Some(d) = &self.capture_dir {
                        match CaptureWriter::open(d, &self.name) {
                            Ok(w) => *self.capture.lock().unwrap() = Some(w),
                            Err(e) => tracing::warn!("[{}] cannot capture: {e}", self.name),
                        }
                    }
                    self.connected.store(true, Ordering::SeqCst);
                    backoff = Duration::from_secs(1);
                    tracing::info!("[{}] open on {}:{}", self.name, self.opener.kind(), self.channel);
                    let mut flush = tokio::time::interval(Duration::from_secs(1));
                    let why = loop {
                        tokio::select! {
                            r = link.rx.recv() => match r {
                                Some(Ok(f)) => self.dispatch(&f),
                                Some(Err(e)) => break e.to_string(),
                                None => break "link closed".to_string(),
                            },
                            _ = self.kick.notified() => break "reopening (requested)".to_string(),
                            _ = flush.tick() => {
                                if let Some(w) = self.capture.lock().unwrap().as_mut() { let _ = w.flush(); }
                            }
                        }
                    };
                    tracing::warn!("[{}] bus error: {why}; retry in {:.0}s", self.name, backoff.as_secs_f64());
                }
                Err(e) => tracing::warn!("[{}] bus error: {e}; retry in {:.0}s", self.name, backoff.as_secs_f64()),
            }
            self.connected.store(false, Ordering::SeqCst);
            *self.link.lock().unwrap() = None;
            *self.capture.lock().unwrap() = None;
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    }
}

/// A transport for tests: inject received frames, decide what happens to sends.
pub mod fake {
    use super::*;

    #[derive(Default)]
    pub struct Control {
        pub sent: Mutex<Vec<CanFrame>>,
        /// Echo our own sends back (`is_rx = false`), like SocketCAN with `own_echo`.
        pub echo: AtomicBool,
        /// Fail every send with ENOBUFS.
        pub fail_sends: AtomicBool,
        /// Refuse to open.
        pub fail_open: AtomicBool,
        /// Opens so far.
        pub opens: Mutex<u32>,
        inject: Mutex<Option<mpsc::Sender<io::Result<CanFrame>>>>,
    }

    impl Control {
        pub fn new() -> Arc<Self> {
            Arc::new(Control::default())
        }
        /// Deliver a frame as if another node sent it.
        pub fn hear(&self, f: CanFrame) -> bool {
            let tx = self.inject.lock().unwrap().clone();
            tx.is_some_and(|t| t.try_send(Ok(f)).is_ok())
        }
        /// Break the link (the bus reopens).
        pub fn drop_link(&self) {
            if let Some(t) = self.inject.lock().unwrap().take() {
                let _ = t.try_send(Err(io::Error::new(io::ErrorKind::BrokenPipe, "link dropped (test)")));
            }
        }
        pub fn sent(&self) -> Vec<CanFrame> {
            self.sent.lock().unwrap().clone()
        }
    }

    pub struct FakeOpener(pub Arc<Control>);

    struct FakeSender(Arc<Control>);

    impl Sender for FakeSender {
        fn send(&self, f: &CanFrame) -> io::Result<()> {
            if self.0.fail_sends.load(Ordering::SeqCst) {
                return Err(io::Error::from_raw_os_error(libc::ENOBUFS));
            }
            self.0.sent.lock().unwrap().push(f.clone());
            if self.0.echo.load(Ordering::SeqCst) {
                let mut e = f.clone().tx();
                e.ts = aetherrv_core::clock::now_s();
                self.0.hear(e);
            }
            Ok(())
        }
    }

    impl Opener for FakeOpener {
        fn open(&self) -> BoxFut<io::Result<Link>> {
            let c = self.0.clone();
            Box::pin(async move {
                *c.opens.lock().unwrap() += 1;
                if c.fail_open.load(Ordering::SeqCst) {
                    return Err(io::Error::new(io::ErrorKind::NotFound, "fake: open refused"));
                }
                let (tx, rx) = mpsc::channel(4096);
                *c.inject.lock().unwrap() = Some(tx);
                Ok(Link { rx, tx: Arc::new(FakeSender(c.clone())) })
            })
        }
        fn kind(&self) -> &'static str {
            "fake"
        }
    }

    /// A bus on the fake transport.
    pub fn bus(name: &str, capture_dir: Option<PathBuf>) -> (Arc<CanBus>, Arc<Control>) {
        let c = Control::new();
        (CanBus::new(name, name, Box::new(FakeOpener(c.clone())), capture_dir), c)
    }
}
