//! `FailoverBus`: one logical bus over two paths to the SAME physical bus: a primary (the Pi's own CAN port) and a
//! backup (e.g. the Cerbo's socketcand). Frames are forwarded from the active path only (the other path hears the
//! same frames) and sends go out the active path only, through TxGate as always.
//!
//! Failover: the primary goes silent (no frame from another node for `FRESH_S`) while the backup still hears the
//! bus. Both silent = the bus itself is quiet: stay put. Failback after `FAILBACK_S` of steady traffic on the
//! primary.
//!
//! A frame nobody ACKs is retried by the CAN chip forever (the MCP2515 has no one-shot mode; a node stays
//! error-passive, never bus-off, on ACK errors) and would go out whenever the wire came back. So on the primary:
//! refuse to send unless it heard the bus within `SEND_FRESH_S`, and expect each frame's TX echo within `echo_s`;
//! a missing echo resets the port at once (discarding the stuck frame) and fails over. A failed send is reported
//! as failed, never replayed on the other path.

use crate::bus::{BoxFut, CanBus, Handler, SendError};
use crate::frame::CanFrame;
use aetherrv_core::state::StateStore;
use aetherrv_core::txgate::TxGate;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

pub const FRESH_S: f64 = 2.0;
pub const SEND_FRESH_S: f64 = 1.0;
pub const FAILBACK_S: f64 = 30.0;
pub const ECHO_S: f64 = 0.25;

pub type ResetFn = Arc<dyn Fn(String) -> BoxFut<anyhow::Result<()>> + Send + Sync>;
pub type SwitchFn = Arc<dyn Fn(&str) + Send + Sync>;

struct Inner {
    primary_active: bool,
    pending: HashMap<(u32, Vec<u8>), f64>,
    good_since: Option<f64>,
    blocked_until: f64,
}

pub struct FailoverBus {
    pub name: String,
    pub primary: Arc<CanBus>,
    pub backup: Arc<CanBus>,
    inner: Mutex<Inner>,
    handlers: Mutex<Vec<Handler>>,
    on_switch: Mutex<Vec<SwitchFn>>,
    reset: Mutex<Option<ResetFn>>,
    state: Option<Arc<StateStore>>,
    echo_s: Mutex<f64>,
    me: Mutex<Weak<FailoverBus>>,
}

fn now_s() -> f64 {
    aetherrv_core::clock::now_s()
}

impl FailoverBus {
    /// Wire a failover bus over two paths. The primary must have been created with `own_echo` (SocketCAN).
    pub fn new(name: &str, primary: Arc<CanBus>, backup: Arc<CanBus>, state: Option<Arc<StateStore>>) -> Arc<Self> {
        let fb = Arc::new(FailoverBus {
            name: name.to_string(),
            primary: primary.clone(),
            backup: backup.clone(),
            inner: Mutex::new(Inner { primary_active: true, pending: HashMap::new(), good_since: None, blocked_until: 0.0 }),
            handlers: Mutex::new(vec![]),
            on_switch: Mutex::new(vec![]),
            reset: Mutex::new(None),
            state,
            echo_s: Mutex::new(ECHO_S),
            me: Mutex::new(Weak::new()),
        });
        *fb.me.lock().unwrap() = Arc::downgrade(&fb);
        let w = Arc::downgrade(&fb);
        primary.add_handler(Arc::new(move |f: &CanFrame| {
            if let Some(fb) = w.upgrade() {
                fb.rx(true, f)
            }
        }));
        let w = Arc::downgrade(&fb);
        backup.add_handler(Arc::new(move |f: &CanFrame| {
            if let Some(fb) = w.upgrade() {
                fb.rx(false, f)
            }
        }));
        fb
    }

    pub fn set_reset(&self, r: ResetFn) {
        *self.reset.lock().unwrap() = Some(r);
    }
    pub fn set_echo_s(&self, s: f64) {
        *self.echo_s.lock().unwrap() = s;
    }
    pub fn add_handler(&self, h: Handler) {
        self.handlers.lock().unwrap().push(h);
    }
    pub fn on_switch(&self, f: SwitchFn) {
        self.on_switch.lock().unwrap().push(f);
    }

    pub fn active(&self) -> Arc<CanBus> {
        if self.inner.lock().unwrap().primary_active { self.primary.clone() } else { self.backup.clone() }
    }
    pub fn on_primary(&self) -> bool {
        self.inner.lock().unwrap().primary_active
    }
    pub fn channel(&self) -> String {
        self.active().channel.clone()
    }
    pub fn link_up(&self) -> bool {
        self.active().link_up()
    }

    /// Heard another node within `within` seconds, and the link is up.
    pub fn fresh(bus: &CanBus, within: f64, now: f64) -> bool {
        bus.stats().last_good.is_some_and(|g| now - g < within) && bus.link_up()
    }

    fn rx(&self, from_primary: bool, f: &CanFrame) {
        if !f.is_rx {
            // our own frame, confirmed sent (SocketCAN echo)
            self.inner.lock().unwrap().pending.remove(&(f.id, f.data.clone()));
            return;
        }
        if from_primary != self.on_primary() {
            return;
        }
        let hs = self.handlers.lock().unwrap().clone();
        for h in hs {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| h(f))).is_err() {
                tracing::error!("[{}] handler panicked on {}", self.name, f.short());
            }
        }
    }

    pub fn send(&self, device: &str, f: &CanFrame, gate: &TxGate, quiet: bool) -> Result<(), SendError> {
        self.send_at(device, f, gate, quiet, now_s())
    }

    pub fn send_at(&self, device: &str, f: &CanFrame, gate: &TxGate, quiet: bool, now: f64) -> Result<(), SendError> {
        let primary = self.on_primary();
        let bus = if primary { &self.primary } else { &self.backup };
        if !Self::fresh(bus, if primary { SEND_FRESH_S } else { FRESH_S }, now) {
            return Err(SendError::Refused(format!("[{}] {} isn't hearing the bus; not sent", self.name, bus.name)));
        }
        bus.send(device, f, gate, quiet)?;
        if primary {
            let key = (f.id, f.data.clone());
            self.inner.lock().unwrap().pending.insert(key.clone(), now);
            let me = self.me.lock().unwrap().upgrade();
            let wait = *self.echo_s.lock().unwrap();
            if let (Some(me), Ok(rt)) = (me, tokio::runtime::Handle::try_current()) {
                rt.spawn(async move {
                    tokio::time::sleep(Duration::from_secs_f64(wait)).await;
                    me.check_echo(&key);
                });
            }
        }
        Ok(())
    }

    fn check_echo(&self, key: &(u32, Vec<u8>)) {
        {
            let mut g = self.inner.lock().unwrap();
            if g.pending.remove(key).is_none() || !g.primary_active {
                return;
            }
            g.pending.clear();
            g.blocked_until = now_s() + FAILBACK_S;
        }
        tracing::error!(
            "[{}] {}: frame {:X} not sent within {:.0} ms (no ACK: wiring?); resetting the port, switching to {}",
            self.name,
            self.primary.name,
            key.0,
            *self.echo_s.lock().unwrap() * 1000.0,
            self.backup.name
        );
        if let Some(r) = self.reset.lock().unwrap().clone() {
            let (name, pname, ch) = (self.name.clone(), self.primary.name.clone(), self.primary.channel.clone());
            tokio::spawn(async move {
                match r(ch).await {
                    Ok(()) => tracing::warn!("[{name}] {pname} reset (pending frame discarded)"),
                    Err(e) => tracing::error!("[{name}] {pname} reset failed: {e}"),
                }
            });
        }
        self.switch(false, "a frame was not acknowledged");
    }

    fn switch(&self, to_primary: bool, why: &str) {
        {
            let mut g = self.inner.lock().unwrap();
            if g.primary_active == to_primary {
                return;
            }
            g.primary_active = to_primary;
            g.good_since = None;
        }
        let to = if to_primary { &self.primary } else { &self.backup };
        if to_primary {
            tracing::info!("[{}] now via {} ({why})", self.name, to.name);
        } else {
            tracing::warn!("[{}] now via {} ({why})", self.name, to.name);
        }
        self.publish();
        let cbs = self.on_switch.lock().unwrap().clone();
        for cb in cbs {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cb(&to.name))).is_err() {
                tracing::error!("[{}] switch callback failed", self.name);
            }
        }
    }

    pub fn publish(&self) {
        if let Some(s) = &self.state {
            let backup = !self.on_primary();
            s.update([
                (format!("{}.path", self.name), json!(self.active().name)),
                (format!("{}.path_backup", self.name), json!(backup)),
            ]);
        }
    }

    pub fn tick(&self, now: f64) {
        let p_ok = Self::fresh(&self.primary, FRESH_S, now);
        let b_ok = Self::fresh(&self.backup, FRESH_S, now);
        if self.on_primary() {
            if !p_ok && b_ok {
                self.switch(false, &format!("{} silent", self.primary.name));
            }
            return;
        }
        let back = {
            let mut g = self.inner.lock().unwrap();
            if p_ok && now >= g.blocked_until {
                let since = *g.good_since.get_or_insert(now);
                now - since >= FAILBACK_S
            } else {
                g.good_since = None;
                false
            }
        };
        if back {
            self.switch(true, &format!("{} steady again", self.primary.name));
        }
    }

    pub async fn run(self: Arc<Self>) {
        self.publish();
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            self.tick(now_s());
        }
    }
}
