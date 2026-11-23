//! `aetherrv-espar`: the Espar heater service (ARCHITECTURE.md §7).
//!
//! Owns `hat1` (one socket: decode, control, read-only UDS, capture). Its control loop depends on nothing outside
//! this process: the broker, gateway, UI and every other service may restart or die while a run continues. What it
//! takes from the bus (the cabin Ruuvi, the battery readings) has a local fallback or a safe default.

pub mod control;

use aetherrv_can::{CanBus, CanFrame, SendError};
use aetherrv_core::admin::AdminClient;
use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::clock::{boot_id, boottime_s, now_s};
use aetherrv_core::cmdlog::CommandLog;
use aetherrv_core::command::{BoxFuture, CommandHandler, Reply};
use aetherrv_core::config::Cfg;
use aetherrv_core::state::{StateExt, StateStore, StateView};
use aetherrv_core::txgate::TxGate;
use aetherrv_power::LockoutTracker;
use control::{Begin, Control, Inputs, RunState, SendErr, Sink, DEVICE};
use proto_espar::Frame;
use proto_espar::decoder::{Decoder, Event};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const DIAG: &str = "espar_diag";
/// A guard lockout key older than this isn't "fresh": the heater service's own backstop takes over (§8).
pub const GUARD_FRESH_S: f64 = 30.0;

pub struct Core {
    pub ctl: Control,
    pub dec: Decoder,
}

pub struct Espar {
    pub core: Arc<Mutex<Core>>,
    pub can: Arc<CanBus>,
    pub gate: Arc<TxGate>,
    pub state: Arc<StateStore>,
    pub bus: Option<Bus>,
    pub lockout: Mutex<LockoutTracker>,
    pub run_path: PathBuf,
    pub boot: String,
    pub admin: AdminClient,
    cmd_lock: tokio::sync::Mutex<()>,
    last_written: Mutex<Option<RunState>>,
    last_backstop: Mutex<Option<f64>>,
}

/// TxGate + the CAN bus as the controller's sink.
pub struct CanSink<'a> {
    pub can: &'a CanBus,
    pub gate: &'a TxGate,
}

impl Sink for CanSink<'_> {
    fn send(&mut self, id: u32, data: &[u8]) -> Result<(), SendErr> {
        match self.can.send(DEVICE, &CanFrame::std(id, data), self.gate, true) {
            Ok(()) => Ok(()),
            Err(SendError::Blocked(e)) => Err(SendErr::Blocked(e.to_string())),
            Err(e) => Err(SendErr::Io(e.to_string())),
        }
    }
}

pub struct Options {
    pub bus_socket: Option<PathBuf>,
    pub run_path: PathBuf,
    pub lockout_path: PathBuf,
    pub db_path: Option<PathBuf>,
    pub capture_dir: Option<PathBuf>,
    pub admin_socket: PathBuf,
}

impl Options {
    pub fn from_config(cfg: &Cfg) -> Options {
        let dev = cfg.at("device.espar");
        let capture = cfg.bool("capture", true);
        Options {
            bus_socket: None,
            run_path: dev.str("run_state", "/run/aetherrv/espar/run.json").into(),
            lockout_path: dev.str("lockout_state", "/var/lib/aetherrv/espar/lockout.json").into(),
            db_path: Some(cfg.at("history").str("path", "/var/lib/aetherrv/history.sqlite3").into()),
            capture_dir: if capture { Some(cfg.str("capture_dir", "captures").into()) } else { None },
            admin_socket: cfg.at("web").str("admin_socket", aetherrv_core::admin::SOCK).into(),
        }
    }
}

/// A handler panic caught by the CAN reader must not leave the control loop dead on a poisoned lock.
fn lock_core(m: &Mutex<Core>) -> std::sync::MutexGuard<'_, Core> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn frame_of(f: &CanFrame) -> Frame {
    Frame { id: f.id, data: f.data.clone(), extended: f.extended, remote: f.remote, error: f.error }
}

impl Espar {
    /// Build from the config (does not start anything).
    pub fn new(cfg: &Cfg, opts: Options) -> anyhow::Result<Arc<Espar>> {
        let dev = cfg.at("device.espar");
        let bus_name = dev.str("bus", "hat1");
        let b = cfg.at("bus").child(&bus_name);
        let channel = b.str("channel", &bus_name);
        let iface = b.str("interface", "socketcan");
        if iface != "socketcan" {
            anyhow::bail!("bus {bus_name}: the Espar needs a local SocketCAN port, not {iface}");
        }
        let capture = opts.capture_dir.clone().filter(|_| b.bool("capture", true));
        // own_echo off: this socket must never hear its own 054/60D (that is how another controller is detected)
        let can = CanBus::socketcan(&bus_name, &channel, false, capture);
        let gate = Arc::new(TxGate::from_config(cfg).only(&[DEVICE, DIAG]));
        let log = CommandLog::new(DEVICE, opts.db_path.clone());
        let mut ctl = Control::new(gate.is_enabled(DEVICE), log);
        ctl.t.blind_off_s = dev.f64("blind_off_s", 600.0);
        let dec = Decoder::new(DEVICE, dev.f64("diag_interval_s", 5.0), dev.bool("diag_scan", false));
        let p = cfg.at("power");
        let lockout = LockoutTracker::persisted(p.f64("heater_off_soc", 10.0), p.f64("hold_s", 120.0), &opts.lockout_path);
        Ok(Arc::new(Espar {
            core: Arc::new(Mutex::new(Core { ctl, dec })),
            can,
            gate,
            state: Arc::new(StateStore::new()),
            bus: None,
            lockout: Mutex::new(lockout),
            run_path: opts.run_path,
            boot: boot_id(),
            admin: AdminClient::new(opts.admin_socket),
            cmd_lock: tokio::sync::Mutex::new(()),
            last_written: Mutex::new(None),
            last_backstop: Mutex::new(None),
        }))
    }

    /// Wire the CAN handler (decoder, other-controller stop, flow control).
    pub fn attach_can(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        self.can.add_handler(Arc::new(move |f: &CanFrame| {
            let Some(me) = me.upgrade() else { return };
            let now = boottime_s();
            let ev = {
                let mut c = lock_core(&me.core);
                let ev = c.dec.on_frame(&frame_of(f), now);
                if ev == Event::OtherController {
                    c.ctl.on_other_controller();
                }
                ev
            };
            if let Event::SendDiag(fc) = ev {
                me.send_diag(&fc, "flow control");
            }
        }));
    }

    fn send_diag(&self, data: &[u8], what: &str) {
        if !self.gate.is_enabled(DIAG) {
            return;
        }
        let r = self.can.send(DIAG, &CanFrame::std(proto_espar::uds::REQ_ID, data), &self.gate, true);
        let mut c = lock_core(&self.core);
        match r {
            Ok(()) => c.dec.note_sent(),
            Err(e) => c.dec.note_send_error(what, &e.to_string()), // report, never retry
        }
    }

    /// Everything the loop needs from outside the heater bus.
    pub fn inputs(&self, view: &dyn StateView, now_boot: f64) -> Inputs {
        let cabin = view.num("climate.cabin_front.temp").zip(view.age("climate.cabin_front.temp"));
        let victron_connected = match view.get("victron.connected") {
            Some(Value::Bool(b)) => Some(b),
            _ => None,
        };
        let own = self.lockout.lock().unwrap().lockout;
        let guard = view.is_true("power.heat_lockout") && view.age("power.heat_lockout").is_some_and(|a| a < GUARD_FRESH_S);
        let contact = {
            let mut c = lock_core(&self.core);
            if let Some(b) = self.bus.as_ref().and_then(|b| b.last_contact_boottime()) {
                c.ctl.last_contact = Some(c.ctl.last_contact.map_or(b, |x| x.max(b)));
            }
            c.ctl.last_contact
        };
        Inputs { cabin, victron_connected, heat_lockout: own || guard, bus_contact_age: contact.map(|t| now_boot - t) }
    }

    pub fn remote_view(&self) -> Arc<dyn StateView> {
        match &self.bus {
            Some(b) => b.remote().clone(),
            None => Arc::new(StateStore::new()),
        }
    }

    /// One control-loop iteration (100 ms).
    pub fn control_tick(&self, view: &dyn StateView) {
        let now = boottime_s();
        let inp = self.inputs(view, now);
        let mut c = lock_core(&self.core);
        let Core { ctl, dec } = &mut *c;
        ctl.step(now, dec, &inp, &mut CanSink { can: &self.can, gate: &self.gate });
        let reset = std::mem::take(&mut ctl.reset_requested);
        let keys = ctl.publish(dec, now);
        let rs = ctl.run_state(&self.boot, now);
        drop(c);
        self.state.update(keys);
        if reset {
            let (admin, ch) = (self.admin.clone(), self.can.channel.clone());
            tokio::spawn(async move {
                if let Err(e) = admin.call("can_reset", json!({"iface": ch}), Duration::from_secs(10)).await {
                    tracing::error!("espar control: {ch} reset failed: {e}");
                }
            });
        }
        self.persist(rs);
    }

    /// Write the run file on every change, and at least every second (the liveness stamp).
    pub fn persist(&self, rs: RunState) {
        let mut last = self.last_written.lock().unwrap();
        let due = match last.as_ref() {
            None => true,
            Some(l) => !rs.same_run(l) || rs.alive - l.alive >= 1.0,
        };
        if !due {
            return;
        }
        if let Some(d) = self.run_path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let tmp = self.run_path.with_extension("tmp");
        let body = serde_json::to_vec(&rs).unwrap();
        match std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, &self.run_path)) {
            Ok(()) => *last = Some(rs),
            Err(e) => tracing::error!("espar: run state not saved ({}): {e}", self.run_path.display()),
        }
    }

    pub fn load_saved(&self) -> Option<RunState> {
        let t = std::fs::read_to_string(&self.run_path).ok()?;
        match serde_json::from_str(&t) {
            Ok(r) => Some(r),
            Err(e) => {
                tracing::warn!("espar: saved run state unreadable ({e}); starting OFF");
                None
            }
        }
    }

    /// At start: listen, then continue the saved run if the heater is still running it (§7.2).
    pub async fn try_resume(&self, listen_s: f64) -> bool {
        let Some(saved) = self.load_saved() else { return false };
        if !saved.active {
            return false;
        }
        let t0 = boottime_s();
        // decide as soon as the heater has reported and another controller would have shown itself (60D is 100 ms)
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let now = boottime_s();
            let heard = lock_core(&self.core).dec.st.is_some();
            if (heard && now - t0 >= 0.3) || now - t0 >= listen_s {
                break;
            }
        }
        let now = boottime_s();
        let mut c = lock_core(&self.core);
        let Core { ctl, dec } = &mut *c;
        match ctl.resume_check(&saved, &self.boot, dec, now) {
            Ok(()) => {
                ctl.resume(&saved, dec, now);
                let rs = ctl.run_state(&self.boot, now);
                drop(c);
                *self.last_written.lock().unwrap() = None;
                self.persist(rs); // the resume count must survive an immediate crash
                tracing::warn!("espar: resumed the {} run after a {:.1} s gap", saved.target.as_deref().unwrap_or(&saved.mode), now - saved.alive);
                true
            }
            Err(why) => {
                ctl.note_not_resumed(&saved, &why, now);
                tracing::warn!("espar: not resuming ({why}); OFF and silent");
                false
            }
        }
    }

    /// Diagnostics schedule (50 ms).
    pub fn diag_tick(&self) {
        let en = self.gate.is_enabled(DIAG);
        let req = lock_core(&self.core).dec.diag_due(boottime_s(), en);
        if let Some(r) = req {
            self.send_diag(&r, "read");
        }
    }

    /// The decoder's keys (250 ms), with `last_seen` as wall time.
    pub fn publish_decoder(&self, with_ids: bool) {
        let now = boottime_s();
        let wall = now_s();
        let en = self.gate.is_enabled(DIAG);
        let c = lock_core(&self.core);
        let mut keys = c.dec.snapshot(now);
        for (k, v) in keys.iter_mut() {
            if k == "climate.espar.last_seen"
                && let Some(t) = v.as_f64()
            {
                *v = json!(wall - (now - t));
            }
        }
        keys.extend(c.dec.diag_snapshot(now, en));
        if with_ids {
            keys.push((format!("{DEVICE}.ids"), Value::Array(c.dec.summary(now))));
        }
        drop(c);
        self.state.update(keys);
        let blocked = self.gate.blocked_count();
        self.state.update([
            (format!("tx.enabled.{DEVICE}"), json!(self.gate.is_enabled(DEVICE))),
            (format!("tx.enabled.{DIAG}"), json!(en)),
            (format!("tx.blocked.{DEVICE}"), json!(blocked.get(DEVICE).copied().unwrap_or(0))),
            (format!("tx.blocked.{DIAG}"), json!(blocked.get(DIAG).copied().unwrap_or(0))),
        ]);
    }

    /// The heat lockout inside this service (§8): track it, and turn a heat run off only when the guard isn't there.
    pub async fn lockout_tick(self: &Arc<Self>, view: &dyn StateView) {
        let wall = now_s();
        self.lockout.lock().unwrap().tick_state(view, wall);
        let own = self.lockout.lock().unwrap().lockout;
        let guard_fresh = view.age("power.heat_lockout").is_some_and(|a| a < GUARD_FRESH_S);
        let guard_locked = guard_fresh && view.is_true("power.heat_lockout");
        self.state.set("climate.espar.lockout", json!(own || guard_locked));
        let heating = {
            let c = lock_core(&self.core);
            c.ctl.active && (c.ctl.mode == "heat" || c.ctl.target.as_deref() == Some("heat"))
        };
        if own && !guard_fresh && heating {
            let due = self.last_backstop.lock().unwrap().is_none_or(|t| wall - t >= 60.0);
            if due {
                *self.last_backstop.lock().unwrap() = Some(wall);
                let r = self.command("set".into(), json!({"mode": "off"}), "auto:low-battery".into()).await;
                tracing::warn!("espar: heat lockout and the power guard isn't answering: turned the heater off ({})", r.detail);
                self.state.set("climate.espar.lockout_off", json!({"t": wall.round(), "detail": format!("guard not answering; {}", r.detail)}));
            }
        }
    }

    /// The command path (Python `EsparControl.command`): serialized, verified, never retried.
    pub async fn command(self: &Arc<Self>, cmd: String, value: Value, client: String) -> Reply {
        let _g = self.cmd_lock.lock().await;
        let view = self.remote_view();
        let now = boottime_s();
        let inp = self.inputs(&*view, now);
        let gate_ok = cmd != "set" || self.gate.check(DEVICE, &control::request_text(&value), false).is_ok();
        let begin = {
            let mut c = lock_core(&self.core);
            let Core { ctl, dec } = &mut *c;
            ctl.begin(&cmd, &value, &client, now, dec, &inp, gate_ok)
        };
        let p = match begin {
            Begin::Done(r) => return r,
            Begin::Wait(p) => p,
        };
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let mut c = lock_core(&self.core);
            let Core { ctl, dec } = &mut *c;
            if let Some(r) = ctl.check(&p, dec, boottime_s()) {
                return r;
            }
        }
    }

    /// SIGTERM: stop transmitting now, no OFF, keep the run for the next process.
    pub fn suspend(&self) {
        let now = boottime_s();
        let rs = {
            let mut c = lock_core(&self.core);
            c.ctl.suspended = true;
            c.ctl.run_state(&self.boot, now)
        };
        *self.last_written.lock().unwrap() = None;
        self.persist(rs);
    }
}

struct Handler(Arc<Espar>);

impl CommandHandler for Handler {
    fn handle(&self, cmd: String, value: Value, client: String) -> BoxFuture<Reply> {
        let me = self.0.clone();
        Box::pin(async move { me.command(cmd, value, client).await })
    }

    /// A late OFF still runs: it's the safe direction (owner, 2026-11-23, Q-T1).
    fn ttl_exempt(&self, cmd: &str, value: &Value) -> bool {
        cmd == "set" && value.get("mode").and_then(Value::as_str) == Some("off")
    }
}

/// Keys this service reads from other services.
use aetherrv_core::names::{BATTERIES, POWER_GUARD, VICTRON};
pub const FOLLOW: &[(&str, &str)] = &[
    (VICTRON, "climate.cabin_front.temp"),
    (VICTRON, "victron.connected"),
    (VICTRON, "victron.battery.soc"),
    (VICTRON, "victron.battery.current"),
    (BATTERIES, "batteries.ble"),
    (BATTERIES, "batteries.ble.current"),
    (POWER_GUARD, "power.heat_lockout"),
];

/// Run the service until SIGTERM/SIGINT.
pub async fn run(cfg: Cfg, mut opts: Options) -> anyhow::Result<()> {
    let socket = opts.bus_socket.take();
    let me = Espar::new(&cfg, opts)?;
    let me = {
        // attach the bus client (the Arc is still unique here)
        let mut e = Arc::try_unwrap(me).map_err(|_| anyhow::anyhow!("espar: unexpected shared state"))?;
        let mut bo = BusOptions::new(aetherrv_core::names::ESPAR);
        if let Some(s) = socket {
            bo = bo.socket(s);
        }
        e.bus = Some(Bus::start(bo, e.state.clone()));
        Arc::new(e)
    };
    me.attach_can();
    tokio::spawn(me.can.clone().run());
    let bus = me.bus.clone().unwrap();
    for (owner, key) in FOLLOW {
        bus.subscribe_state(owner, key).await;
    }
    // Resume before the first frame goes out, and before any command is acted on: a command that arrives during
    // the listen window waits for the decision (its TTL was checked on arrival).
    let held = me.cmd_lock.lock().await;
    bus.serve(DEVICE, Arc::new(Handler(me.clone())), Some(lock_core(&me.core).ctl.log.clone())).await;
    let listen = cfg.at("device.espar").f64("resume_listen_s", 1.0);
    me.try_resume(listen).await;
    drop(held);

    aetherrv_core::notify::ready();
    let wd = aetherrv_core::notify::watchdog_interval();
    // the 100 ms control loop: feeds the watchdog itself, so a stalled loop is restarted (and then resumes)
    let control_task = {
        let me = me.clone();
        tokio::spawn(async move {
            let view = me.remote_view();
            let mut iv = tokio::time::interval(Duration::from_millis(100));
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                iv.tick().await;
                me.control_tick(&*view);
                if wd.is_some() {
                    aetherrv_core::notify::kick();
                }
            }
        })
    };
    {
        let me = me.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(50));
            loop {
                iv.tick().await;
                me.diag_tick();
            }
        });
    }
    {
        let me = me.clone();
        tokio::spawn(async move {
            let mut n = 0u64;
            loop {
                me.publish_decoder(n % 4 == 0);
                n += 1;
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        });
    }
    {
        let me = me.clone();
        tokio::spawn(async move {
            let view = me.remote_view();
            loop {
                me.lockout_tick(&*view).await;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });
    }
    tokio::spawn(aetherrv_can::run_stats(vec![me.can.clone()], me.state.clone()));
    {
        let (can, admin, st) = (me.can.clone(), me.admin.clone(), me.state.clone());
        tokio::spawn(aetherrv_can::recover::recover_links(
            vec![can as Arc<dyn aetherrv_can::recover::Recoverable>],
            Arc::new(move |iface: String| {
                let admin = admin.clone();
                Box::pin(async move {
                    admin.call("can_rebind", json!({"iface": iface}), Duration::from_secs(30)).await.map(|_| ()).map_err(anyhow::Error::from)
                })
            }),
            Some(st),
            Default::default(),
        ));
    }

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = term.recv() => {},
        _ = tokio::signal::ctrl_c() => {},
        r = control_task => {
            // never expected: exit so systemd restarts us (and the next process resumes the run)
            tracing::error!("espar: the control loop ended ({r:?}); exiting without sending OFF");
            me.suspend();
            std::process::exit(70);
        }
    }
    aetherrv_core::notify::stopping();
    me.suspend();
    tracing::info!("espar: stopping (no OFF sent; the run file is kept for the next process)");
    bus.shutdown().await;
    Ok(())
}
