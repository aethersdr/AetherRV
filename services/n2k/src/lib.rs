//! `aetherrv-n2k`: NMEA 2000 (ARCHITECTURE.md §2). Owns `hat0` and the Cerbo's socketcand as one logical bus
//! (FailoverBus): decoding, the device list, the alternator, and the CLMD16 switching controller.

pub mod node;
pub mod switching;

use aetherrv_can::{CanBus, CanFrame, FailoverBus, SendError};
use aetherrv_core::admin::AdminClient;
use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::cmdlog::CommandLog;
use aetherrv_core::command::{BoxFuture, CommandHandler, Reply};
use aetherrv_core::config::Cfg;
use aetherrv_core::names::{N2K, VICTRON};
use aetherrv_core::state::{StateStore, StateView};
use aetherrv_core::txgate::TxGate;
use node::{N2kNode, N2kSink};
use proto_n2k::adapter::N2kAdapter;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use switching::{DEVICE, SwitchController};

/// The logical N2K bus: one port, or two paths to the same bus.
#[derive(Clone)]
pub enum N2kBus {
    Single(Arc<CanBus>),
    Failover(Arc<FailoverBus>),
}

pub struct BusSink {
    pub bus: N2kBus,
    pub gate: Arc<TxGate>,
}

impl N2kSink for BusSink {
    fn send(&self, f: &CanFrame, quiet: bool) -> Result<(), SendError> {
        match &self.bus {
            N2kBus::Single(b) => b.send(DEVICE, f, &self.gate, quiet),
            N2kBus::Failover(fb) => fb.send(DEVICE, f, &self.gate, quiet),
        }
    }
}

fn frame_of(f: &CanFrame) -> proto_n2k::Frame {
    proto_n2k::Frame { id: f.id, data: f.data.clone(), extended: f.extended, error: f.error, remote: f.remote }
}

struct Handler(Arc<SwitchController>);

impl CommandHandler for Handler {
    fn handle(&self, cmd: String, value: Value, client: String) -> BoxFuture<Reply> {
        let c = self.0.clone();
        Box::pin(async move { c.command(&cmd, &value, &client).await })
    }
    /// A hold release always runs (the safe direction; a late refusal would let the dimmer ramp to the cap).
    fn ttl_exempt(&self, cmd: &str, value: &Value) -> bool {
        cmd == "hold" && value.get("on") == Some(&Value::Bool(false))
    }
}

pub struct Options {
    pub bus_socket: Option<PathBuf>,
}

fn make_bus(cfg: &Cfg, name: &str, capture: Option<PathBuf>, own_echo: bool) -> anyhow::Result<Arc<CanBus>> {
    let b = cfg.at("bus").child(name);
    if b.is_empty() {
        anyhow::bail!("bus {name} is not configured");
    }
    let cap = capture.filter(|_| b.bool("capture", true));
    Ok(match b.str("interface", "socketcan").as_str() {
        "socketcan" => CanBus::socketcan(name, &b.str("channel", name), own_echo, cap),
        "socketcand" => CanBus::socketcand(name, &b.str("host", ""), b.i64("port", 29536) as u16, &b.str("channel", "can0"), cap),
        other => anyhow::bail!("bus {name}: unknown interface {other}"),
    })
}

pub async fn run(cfg: Cfg, opts: Options) -> anyhow::Result<()> {
    let dev = cfg.at("device.n2k");
    let state = Arc::new(StateStore::new());
    let mut bo = BusOptions::new(N2K);
    if let Some(s) = opts.bus_socket {
        bo = bo.socket(s);
    }
    let bus = Bus::start(bo, state.clone());
    let capture: Option<PathBuf> = cfg.bool("capture", true).then(|| cfg.str("capture_dir", "captures").into());
    let admin = AdminClient::new(cfg.at("web").str("admin_socket", aetherrv_core::admin::SOCK));
    let gate = Arc::new(TxGate::from_config(&cfg).only(&[DEVICE]));

    let primary_name = dev.str("bus", "hat0");
    let backup_name = dev.opt_str("backup_bus").filter(|b| !cfg.at("bus").child(b).is_empty());
    let primary = make_bus(&cfg, &primary_name, capture.clone(), backup_name.is_some())?;
    let mut physical = vec![primary.clone()];
    let n2kbus = match &backup_name {
        Some(b) => {
            let backup = make_bus(&cfg, b, capture.clone(), false)?;
            physical.push(backup.clone());
            let fb = FailoverBus::new(N2K, primary.clone(), backup, Some(state.clone()));
            let admin2 = admin.clone();
            fb.set_reset(Arc::new(move |iface: String| {
                let admin = admin2.clone();
                Box::pin(async move {
                    admin.call("can_reset", json!({"iface": iface}), Duration::from_secs(10)).await.map(|_| ()).map_err(anyhow::Error::from)
                })
            }));
            N2kBus::Failover(fb)
        }
        None => N2kBus::Single(primary.clone()),
    };

    let switches: Vec<Value> = dev.list("switch").into_iter().map(|c| c.0).collect();
    let show_unlabeled = dev.child("switching").bool("show_unlabeled", true);
    let adapter = Arc::new(Mutex::new(N2kAdapter::new(&switches, show_unlabeled).map_err(|e| anyhow::anyhow!("{e:?}"))?));
    let sink: Arc<dyn N2kSink> = Arc::new(BusSink { bus: n2kbus.clone(), gate: gate.clone() });
    let sw = dev.child("switching");
    let node = Arc::new(N2kNode::new(sink, sw.i64("address", 150) as u8, sw.i64("unique", 0x0AE7) as u32));

    // frames from the active path → the decoder and our node
    {
        let (adapter, node2) = (adapter.clone(), node.clone());
        let h: aetherrv_can::Handler = Arc::new(move |f: &CanFrame| {
            let now = if f.ts > 0.0 { f.ts } else { aetherrv_core::clock::now_s() };
            adapter.lock().unwrap_or_else(|e| e.into_inner()).on_frame(&frame_of(f), now);
            node2.on_frame(f);
        });
        match &n2kbus {
            N2kBus::Single(b) => b.add_handler(h),
            N2kBus::Failover(fb) => {
                fb.add_handler(h);
                let node = node.clone();
                fb.on_switch(Arc::new(move |_to: &str| node.reassert())); // same NAME + address on the new path
            }
        }
    }
    for b in &physical {
        tokio::spawn(b.clone().run());
    }
    if let N2kBus::Failover(fb) = &n2kbus {
        tokio::spawn(fb.clone().run());
    }
    tokio::spawn(aetherrv_can::run_stats(physical.clone(), state.clone()));
    {
        let admin = admin.clone();
        let st = state.clone();
        let local: Vec<Arc<dyn aetherrv_can::recover::Recoverable>> =
            physical.iter().map(|b| b.clone() as Arc<dyn aetherrv_can::recover::Recoverable>).collect();
        tokio::spawn(aetherrv_can::recover::recover_links(
            local,
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

    // the Cerbo's VE.Can device list names the devices (published by the Victron service)
    bus.subscribe_state(VICTRON, "n2k.names_from_cerbo").await;
    {
        let (adapter, state, bus2, gate) = (adapter.clone(), state.clone(), bus.clone(), gate.clone());
        tokio::spawn(async move {
            loop {
                let now = aetherrv_core::clock::now_s();
                let keys = {
                    let mut a = adapter.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(h) = bus2.remote().get("n2k.names_from_cerbo") {
                        a.set_names_from_cerbo(h);
                    }
                    a.snapshot(now)
                };
                state.update(keys);
                let blocked = gate.blocked_count();
                state.update([
                    (format!("tx.enabled.{DEVICE}"), json!(gate.is_enabled(DEVICE))),
                    (format!("tx.blocked.{DEVICE}"), json!(blocked.get(DEVICE).copied().unwrap_or(0))),
                ]);
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
    }

    // switching: control of the load controller (gated by device.n2k tx_enabled)
    if !dev.child("switching").is_empty() {
        let log = CommandLog::new(DEVICE, Some(cfg.at("history").str("path", "/var/lib/aetherrv/history.sqlite3").into()));
        let ctl = SwitchController::new(state.clone(), adapter.clone(), node.clone(), gate.clone(), &dev, log.clone());
        bus.serve(DEVICE, Arc::new(Handler(ctl.clone())), Some(log)).await;
        tokio::spawn(ctl.run());
    }

    aetherrv_core::notify::ready();
    aetherrv_core::notify::spawn_runtime_watchdog();
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = term.recv() => {},
        _ = tokio::signal::ctrl_c() => {},
    }
    aetherrv_core::notify::stopping();
    bus.shutdown().await;
    Ok(())
}

/// For tests: the view type used by the controller.
pub fn _assert_view(_: &dyn StateView) {}
