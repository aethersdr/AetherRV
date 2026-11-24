//! `aetherrv-netmon`: network health of van devices (port of `src/aetherrv/adapters/netmon.py`). Observation only.
//!
//! Each target is checked every `interval` seconds with:
//! * ICMP echo via an unprivileged ICMP datagram socket (needs `net.ipv4.ping_group_range` to include the
//!   service's group; no capabilities, so it works inside the hardened unit);
//! * optionally a TCP connect to `tcp` (e.g. the IPG100's N2KServer port 6544). The connection is closed at once:
//!   no handshake beyond TCP, no login, no data.
//!
//! A target is DOWN after `fail_after` consecutive failed checks. A host that answers ping but not its service port
//! is reported as "service not answering" (the IPG100's usual lockup mode). Publishes `netmon.targets`.

use aetherrv_core::bus::{Bus, BusOptions};
use aetherrv_core::config::Cfg;
use aetherrv_core::num::{jof, round, round_int};
use aetherrv_core::state::StateStore;
use serde_json::{Value, json};
use std::net::{Ipv4Addr, SocketAddrV4, ToSocketAddrs, UdpSocket};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub name: String,
    pub host: String,
    /// The configured port (`None` = ping only), published as given.
    pub tcp: Value,
    pub fail_after: i64,
    pub severity: Value,
    pub hint: Value,
    pub fails: i64,
    pub down_since: Option<f64>,
    pub last_ok: Option<f64>,
    pub rtt_ms: Option<f64>,
    pub tcp_ok: Option<bool>,
    pub up: Option<bool>,
}

impl Target {
    /// One `[[netmon.targets]]` entry (`name` and `host` are required).
    pub fn from_cfg(t: &Cfg) -> Option<Target> {
        let (Some(name), Some(host)) = (t.opt_str("name"), t.opt_str("host")) else { return None };
        Some(Target {
            name,
            host,
            tcp: t.get("tcp").cloned().unwrap_or(Value::Null),
            fail_after: t.i64("fail_after", 2),
            severity: t.get("severity").cloned().unwrap_or(json!("warning")),
            hint: t.get("hint").cloned().unwrap_or(json!("")),
            fails: 0,
            down_since: None,
            last_ok: None,
            rtt_ms: None,
            tcp_ok: None,
            up: None,
        })
    }

    /// The port to connect to (`int(t["tcp"]) if t["tcp"]`).
    pub fn port(&self) -> Option<u16> {
        if !aetherrv_core::config::truthy(&self.tcp) {
            return None;
        }
        self.tcp.as_f64().and_then(|p| u16::try_from(p as i64).ok())
    }
}

pub struct NetMon {
    pub targets: Vec<Target>,
    pub interval: f64,
    pub timeout: f64,
}

static ICMP_WARNED: AtomicBool = AtomicBool::new(false);

/// One echo request; the RTT in ms, or `None`. Blocking (run it on a worker thread).
pub fn icmp_ping(host: &str, timeout: f64) -> Option<f64> {
    let ip: Ipv4Addr = (host, 0).to_socket_addrs().ok()?.find_map(|a| match a {
        std::net::SocketAddr::V4(v) => Some(*v.ip()),
        _ => None,
    })?;
    let s = match socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, Some(socket2::Protocol::ICMPV4)) {
        Ok(s) => s,
        Err(e) => {
            if !ICMP_WARNED.swap(true, Ordering::SeqCst) {
                tracing::warn!("netmon: no ICMP socket ({e}); is net.ipv4.ping_group_range set for this group?");
            }
            return None;
        }
    };
    let s: UdpSocket = s.into();
    s.set_read_timeout(Some(Duration::from_secs_f64(timeout.max(0.001)))).ok()?;
    let seq = (std::process::id() & 0xFFFF) as u16;
    // type 8 (echo request), code 0, checksum 0, id 0, seq; the kernel fills id + checksum for DGRAM ICMP
    let mut pkt = vec![8u8, 0, 0, 0, 0, 0];
    pkt.extend_from_slice(&seq.to_be_bytes());
    pkt.extend_from_slice(b"aetherrv");
    let t0 = Instant::now();
    s.send_to(&pkt, SocketAddrV4::new(ip, 0)).ok()?;
    let mut buf = [0u8; 1024];
    loop {
        let (n, _) = s.recv_from(&mut buf).ok()?;
        if n > 0 && buf[0] == 0 {
            // echo reply
            return Some(round(t0.elapsed().as_secs_f64() * 1000.0, 1));
        }
    }
}

/// A TCP connect within `timeout`, closed at once.
pub async fn tcp_ok(host: &str, port: u16, timeout: f64) -> bool {
    matches!(
        tokio::time::timeout(Duration::from_secs_f64(timeout), tokio::net::TcpStream::connect((host, port))).await,
        Ok(Ok(_))
    )
}

impl NetMon {
    pub fn new(targets: Vec<Target>, interval: f64, timeout: f64) -> NetMon {
        NetMon { targets, interval, timeout }
    }

    pub fn from_config(cfg: &Cfg) -> NetMon {
        let nm = cfg.at("netmon");
        let mut targets = Vec::new();
        for t in nm.list("targets") {
            match Target::from_cfg(&t) {
                Some(x) => targets.push(x),
                None => tracing::warn!("netmon: a target without name/host; skipping"),
            }
        }
        NetMon::new(targets, nm.f64("interval", 60.0), 3.0)
    }

    /// Fold one check result into a target (Python `NetMon.apply`).
    pub fn apply(t: &mut Target, rtt: Option<f64>, tcp: Option<bool>, now: f64) {
        let ok = rtt.is_some() && tcp != Some(false);
        t.rtt_ms = rtt;
        t.tcp_ok = tcp;
        if ok {
            t.fails = 0;
            t.up = Some(true);
            t.down_since = None;
            t.last_ok = Some(now);
            return;
        }
        t.fails += 1;
        if t.fails >= t.fail_after {
            if t.up != Some(false) {
                t.down_since = Some(t.last_ok.unwrap_or(now));
            }
            t.up = Some(false);
        }
    }

    /// `netmon.targets`.
    pub fn snapshot(&self, now: f64) -> Value {
        Value::Array(
            self.targets
                .iter()
                .map(|t| {
                    let reason = match t.up {
                        Some(false) if t.rtt_ms.is_some() && t.tcp_ok == Some(false) => json!("service not answering"),
                        Some(false) => json!("no response"),
                        _ => Value::Null,
                    };
                    json!({"name": t.name, "host": t.host, "tcp": t.tcp, "up": t.up, "rtt_ms": jof(t.rtt_ms),
                           "tcp_ok": t.tcp_ok, "reason": reason, "severity": t.severity, "hint": t.hint,
                           "down_for": t.down_since.map(|d| round_int(now - d))})
                })
                .collect(),
        )
    }

    /// Check one target: ping, then the TCP port (if any).
    pub async fn check(host: String, port: Option<u16>, timeout: f64) -> (Option<f64>, Option<bool>) {
        let h = host.clone();
        let rtt = tokio::task::spawn_blocking(move || icmp_ping(&h, timeout)).await.ok().flatten();
        let tcp = match port {
            Some(p) => Some(tcp_ok(&host, p, timeout).await),
            None => None,
        };
        (rtt, tcp)
    }

    /// Check every target concurrently and fold the results in.
    pub async fn check_all(&mut self) {
        let mut set = tokio::task::JoinSet::new();
        for (i, t) in self.targets.iter().enumerate() {
            let (host, port, timeout) = (t.host.clone(), t.port(), self.timeout);
            set.spawn(async move { (i, NetMon::check(host, port, timeout).await) });
        }
        while let Some(r) = set.join_next().await {
            if let Ok((i, (rtt, tcp))) = r {
                NetMon::apply(&mut self.targets[i], rtt, tcp, aetherrv_core::clock::now_s());
            }
        }
    }
}

pub struct Options {
    pub bus_socket: Option<PathBuf>,
}

/// Run the service until SIGTERM/SIGINT. With no targets configured it stays up and publishes nothing (Python
/// didn't start the monitor at all).
pub async fn run(cfg: Cfg, opts: Options) -> anyhow::Result<()> {
    let state = Arc::new(StateStore::new());
    let mut bo = BusOptions::new(aetherrv_core::names::NETMON);
    if let Some(s) = opts.bus_socket {
        bo = bo.socket(s);
    }
    let bus = Bus::start(bo, state.clone());
    let mut nm = NetMon::from_config(&cfg);
    aetherrv_core::notify::ready();
    aetherrv_core::notify::spawn_runtime_watchdog();
    if nm.targets.is_empty() {
        tracing::info!("netmon: no targets configured");
    } else {
        let st = state.clone();
        tokio::spawn(async move {
            loop {
                nm.check_all().await;
                st.set("netmon.targets", nm.snapshot(aetherrv_core::clock::now_s()));
                tokio::time::sleep(Duration::from_secs_f64(nm.interval.max(0.05))).await;
            }
        });
    }
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = term.recv() => {},
        _ = tokio::signal::ctrl_c() => {},
    }
    aetherrv_core::notify::stopping();
    bus.shutdown().await;
    Ok(())
}
