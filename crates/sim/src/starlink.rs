//! Starlink dish simulator: gRPC on loopback with server reflection (v1 and v1alpha) and
//! `SpaceX.API.Device.Device/Handle`, built from the hand-written `proto/aetherrv_sim/*.proto` (only what AetherRV
//! uses; see the header of `starlink_dish.proto` for what is a fact and what is unverified).
//!
//! The model: uptime, reboot (the dish goes away, then comes back with its uptime reset and a waiting update
//! installed), `software_update_state` (a check fetches an available update, then REBOOT_REQUIRED), the dish config
//! (snow melt, sleep schedule), alerts, the obstruction map and the history (power, event log with device ids that
//! AetherRV must not copy). `frozen` keeps every value constant (except what a command changes), so two clients
//! reading it at different times see the same values (the parity runs).

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor, ReflectMessage, Value as PValue};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tonic::Status;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};

/// `protoc --include_imports --descriptor_set_out=starlink.fds aetherrv_sim/starlink_device.proto`
pub const FDS: &[u8] = include_bytes!("../proto/starlink.fds");
pub const SERVICE: &str = "SpaceX.API.Device.Device";

// SoftwareUpdateState numbers (this proto's; unverified against a dish)
pub const SW_IDLE: i32 = 1;
pub const SW_FETCHING: i32 = 2;
pub const SW_REBOOT_REQUIRED: i32 = 6;
// DishOutage.Cause
pub const OUTAGE_NO_SCHEDULE: i32 = 4;
pub const OUTAGE_OBSTRUCTED: i32 = 6;

#[derive(Debug, Clone)]
pub struct StarlinkSimConfig {
    /// Loopback only (anything else is refused).
    pub bind: SocketAddr,
    /// Values don't move with time.
    pub frozen: bool,
    /// Serve only the v1alpha reflection service (the v1 one answers UNIMPLEMENTED).
    pub v1alpha_only: bool,
    /// Older firmware: `signal_quality` is not in the reflected `DishGetStatusResponse`.
    pub legacy_firmware: bool,
    /// After a reboot request: answer, wait this long, then go away.
    pub reboot_delay: Duration,
    /// How long a reboot keeps the dish away.
    pub down_for: Duration,
    /// How long a check takes to download an available update.
    pub fetch_for: Duration,
    /// An update is available for download.
    pub update_available: bool,
}

impl Default for StarlinkSimConfig {
    fn default() -> Self {
        StarlinkSimConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            frozen: false,
            v1alpha_only: false,
            legacy_firmware: false,
            reboot_delay: Duration::from_millis(300),
            down_for: Duration::from_secs(3),
            fetch_for: Duration::from_secs(2),
            update_available: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SimEvent {
    pub reason: i32,
    pub severity: i32,
    pub start_ns: i64,
    pub duration_ns: i64,
    pub device_id: String,
}

/// The dish. Tests read and change it through [`StarlinkSim::with`].
#[derive(Debug, Clone)]
pub struct DishState {
    pub up: bool,
    pub uptime_base_s: u64,
    pub boot: Instant,
    pub device_id: String,
    pub hardware_version: String,
    pub software_version: String,
    pub next_software_version: String,
    pub update_state: i32,
    pub fetch_until: Option<Instant>,
    pub update_available: bool,
    pub outage_cause: Option<i32>,
    pub latency_ms: f32,
    pub drop_rate: f32,
    pub down_bps: f32,
    pub up_bps: f32,
    pub fraction_obstructed: f32,
    pub currently_obstructed: bool,
    pub azimuth: f32,
    pub elevation: f32,
    pub gps_valid: bool,
    pub gps_sats: u32,
    pub snr_above_noise: bool,
    pub snr_persistently_low: bool,
    pub signal_quality: f32,
    /// `DishAlerts` field names that are set.
    pub alerts: Vec<String>,
    pub snow_melt_mode: i32,
    pub power_save_mode: bool,
    pub power_save_start: u32,
    pub power_save_duration: u32,
    pub map_rows: u32,
    pub map_cols: u32,
    pub map_snr: Vec<f32>,
    pub map_min_el: f32,
    pub map_max_theta: f32,
    pub map_frame: i32,
    pub history_current_base: u64,
    pub events: Vec<SimEvent>,
    pub events_start_ns: i64,
    pub events_now_ns: i64,
    // what was asked
    pub calls: BTreeMap<String, u64>,
    pub reboots: u64,
    pub update_checks: u64,
    /// Every applied `dish_set_config`, as {field: value} of the fields with their apply_ flag (the Python test's
    /// `FakeDish.sets`).
    pub set_configs: Vec<serde_json::Value>,
    // faults
    /// Request kind (`get_status`, `reboot`, ...) → how many calls to fail with UNAVAILABLE.
    pub fail: BTreeMap<String, u32>,
    /// A reboot is acted on but its reply is an error (a deadline expiring as the dish goes down).
    pub reboot_reply_error: bool,
    /// A reboot is answered but the dish never restarts.
    pub ignore_reboot: bool,
    /// A config change is answered but not applied.
    pub ignore_set_config: bool,
}

impl DishState {
    fn new(cfg: &StarlinkSimConfig) -> DishState {
        let (rows, cols) = (61u32, 61u32);
        DishState {
            up: true,
            uptime_base_s: 86_400,
            boot: Instant::now(),
            device_id: "ut-sim-00000000-00000000-00000001".into(),
            hardware_version: "rev4_prod2".into(),
            software_version: "2026.11.1.mr00001".into(),
            next_software_version: "2026.11.2.mr00002".into(),
            update_state: SW_IDLE,
            fetch_until: None,
            update_available: cfg.update_available,
            outage_cause: None,
            latency_ms: 31.25,
            drop_rate: 0.0012,
            down_bps: 85_400_000.0,
            up_bps: 9_100_000.0,
            fraction_obstructed: 0.0234,
            currently_obstructed: false,
            azimuth: 12.5,
            elevation: 64.75,
            gps_valid: true,
            gps_sats: 12,
            snr_above_noise: true,
            snr_persistently_low: false,
            signal_quality: 0.68,
            alerts: vec!["roaming".into()],
            snow_melt_mode: 0,
            power_save_mode: false,
            power_save_start: 0,
            power_save_duration: 0,
            map_rows: rows,
            map_cols: cols,
            map_snr: sky_map(rows, cols),
            map_min_el: 25.0,
            map_max_theta: 65.0,
            map_frame: 2,
            history_current_base: 4_000,
            events: vec![
                SimEvent { reason: 8, severity: 1, start_ns: 1_763_900_000_000_000_000, duration_ns: 600_000_000, device_id: "ut-sim-00000000-00000000-00000001".into() },
                SimEvent { reason: 22, severity: 1, start_ns: 1_763_903_600_000_000_000, duration_ns: 0, device_id: "router-sim-0000000000000001".into() },
                SimEvent { reason: 1, severity: 2, start_ns: 1_763_901_800_500_000_000, duration_ns: 12_300_000_000, device_id: "ut-sim-00000000-00000000-00000001".into() },
                SimEvent { reason: 12, severity: 1, start_ns: 1_763_902_000_000_000_000, duration_ns: 4_000_000_000, device_id: "router-sim-0000000000000001".into() },
            ],
            events_start_ns: 1_763_870_000_000_000_000,
            events_now_ns: 1_763_906_000_000_000_000,
            calls: BTreeMap::new(),
            reboots: 0,
            update_checks: 0,
            set_configs: Vec::new(),
            fail: BTreeMap::new(),
            reboot_reply_error: false,
            ignore_reboot: false,
            ignore_set_config: false,
        }
    }

    fn elapsed(&self, frozen: bool) -> u64 {
        if frozen { 0 } else { self.boot.elapsed().as_secs() }
    }

    pub fn uptime(&self, frozen: bool) -> u64 {
        self.uptime_base_s + self.elapsed(frozen)
    }

    fn tick(&mut self) {
        if self.update_state == SW_FETCHING && self.fetch_until.is_some_and(|t| Instant::now() >= t) {
            self.update_state = SW_REBOOT_REQUIRED;
            self.fetch_until = None;
            if !self.alerts.iter().any(|a| a == "install_pending") {
                self.alerts.push("install_pending".into());
            }
        }
    }

    /// The dish after a restart: uptime reset, a waiting update installed.
    fn boot_now(&mut self) {
        self.uptime_base_s = 0;
        self.boot = Instant::now();
        if self.update_state == SW_REBOOT_REQUIRED {
            self.software_version = self.next_software_version.clone();
            self.update_available = false;
            self.alerts.retain(|a| a != "install_pending");
        }
        self.update_state = SW_IDLE;
        self.fetch_until = None;
    }
}

/// A sky map: -1 outside the visible cone, clear sky, an obstructed wedge low in the east (exact binary values).
fn sky_map(rows: u32, cols: u32) -> Vec<f32> {
    let (cr, cc) = ((rows as f32 - 1.0) / 2.0, (cols as f32 - 1.0) / 2.0);
    let mut v = Vec::with_capacity((rows * cols) as usize);
    for r in 0..rows {
        for c in 0..cols {
            let (dy, dx) = (r as f32 - cr, c as f32 - cc);
            let d = (dx * dx + dy * dy).sqrt() / cr;
            let ang = dy.atan2(dx).to_degrees();
            v.push(if d > 1.0 {
                -1.0
            } else if d > 0.7 && (20.0..70.0).contains(&ang) {
                if d > 0.85 { 0.0 } else { 0.25 }
            } else if d > 0.9 && (r + c) % 7 == 0 {
                -1.0
            } else if (r * 3 + c) % 11 == 0 {
                0.75
            } else {
                1.0
            });
        }
    }
    v
}

/// The descriptor set this dish serves by reflection (`legacy`: without `signal_quality`).
pub fn served_fds(legacy: bool) -> Vec<u8> {
    let mut set = prost_types::FileDescriptorSet::decode(FDS).expect("starlink.fds");
    if legacy {
        for f in &mut set.file {
            for m in &mut f.message_type {
                if m.name() == "DishGetStatusResponse" {
                    m.field.retain(|x| x.name() != "signal_quality");
                }
            }
        }
    }
    set.encode_to_vec()
}

pub fn pool() -> DescriptorPool {
    DescriptorPool::decode(FDS).expect("starlink.fds")
}

// ── a codec for dynamic messages ────────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct DynCodec(pub MessageDescriptor);
pub struct DynEncoder;
pub struct DynDecoder(MessageDescriptor);

impl Codec for DynCodec {
    type Encode = DynamicMessage;
    type Decode = DynamicMessage;
    type Encoder = DynEncoder;
    type Decoder = DynDecoder;
    fn encoder(&mut self) -> DynEncoder {
        DynEncoder
    }
    fn decoder(&mut self) -> DynDecoder {
        DynDecoder(self.0.clone())
    }
}

impl Encoder for DynEncoder {
    type Item = DynamicMessage;
    type Error = Status;
    fn encode(&mut self, item: DynamicMessage, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        item.encode(dst).map_err(|e| Status::internal(e.to_string()))
    }
}

impl Decoder for DynDecoder {
    type Item = DynamicMessage;
    type Error = Status;
    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<DynamicMessage>, Status> {
        DynamicMessage::decode(self.0.clone(), src).map(Some).map_err(|e| Status::internal(e.to_string()))
    }
}

// ── the server ──────────────────────────────────────────────────────────────────────────────────────────────────

struct Inner {
    cfg: StarlinkSimConfig,
    pool: DescriptorPool,
    state: Mutex<DishState>,
    shutdown: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    back_up: tokio::sync::Notify,
}

pub struct StarlinkSim {
    pub addr: SocketAddr,
    inner: Arc<Inner>,
    task: tokio::task::JoinHandle<()>,
}

impl StarlinkSim {
    /// Start listening (refuses anything but a loopback address).
    pub async fn start(cfg: StarlinkSimConfig) -> anyhow::Result<StarlinkSim> {
        if !cfg.bind.ip().is_loopback() {
            anyhow::bail!("refusing to listen on {}: the Starlink simulator is loopback only", cfg.bind);
        }
        let listener = TcpListener::bind(cfg.bind).await?;
        let addr = listener.local_addr()?;
        let inner = Arc::new(Inner {
            state: Mutex::new(DishState::new(&cfg)),
            cfg,
            pool: pool(),
            shutdown: Mutex::new(None),
            back_up: tokio::sync::Notify::new(),
        });
        let task = tokio::spawn(serve_loop(inner.clone(), addr, listener));
        Ok(StarlinkSim { addr, inner, task })
    }

    /// `host:port` for a client.
    pub fn target(&self) -> String {
        self.addr.to_string()
    }

    /// Read or change the dish.
    pub fn with<R>(&self, f: impl FnOnce(&mut DishState) -> R) -> R {
        let mut st = self.inner.state.lock().unwrap();
        st.tick();
        f(&mut st)
    }

    pub fn is_up(&self) -> bool {
        self.with(|s| s.up)
    }

    pub fn stop(self) {
        self.task.abort();
        if let Some(tx) = self.inner.shutdown.lock().unwrap().take() {
            let _ = tx.send(());
        }
    }
}

async fn serve_loop(inner: Arc<Inner>, addr: SocketAddr, first: TcpListener) {
    let mut listener = Some(first);
    loop {
        let l = match listener.take() {
            Some(l) => l,
            None => loop {
                match TcpListener::bind(addr).await {
                    Ok(l) => break l,
                    Err(e) => {
                        tracing::warn!("starlink sim: rebind {addr}: {e}");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            },
        };
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        *inner.shutdown.lock().unwrap() = Some(tx);
        let fds = served_fds(inner.cfg.legacy_firmware);
        let mut b = tonic::transport::Server::builder();
        let v1a = tonic_reflection::server::Builder::configure()
            .register_encoded_file_descriptor_set(&fds)
            .build_v1alpha()
            .expect("reflection v1alpha");
        let mut router = b.add_service(v1a).add_service(DeviceServer(inner.clone()));
        if !inner.cfg.v1alpha_only {
            let v1 = tonic_reflection::server::Builder::configure()
                .register_encoded_file_descriptor_set(&fds)
                .build_v1()
                .expect("reflection v1");
            router = router.add_service(v1);
        }
        let incoming = tonic::transport::server::TcpIncoming::from(l);
        let serve = router.serve_with_incoming_shutdown(incoming, async {
            let _ = rx.await;
        });
        if let Err(e) = serve.await {
            tracing::warn!("starlink sim: server ended: {e}");
        }
        // down (rebooting): wait until the reboot task brings the dish back
        while !inner.state.lock().unwrap().up {
            let _ = tokio::time::timeout(Duration::from_millis(200), inner.back_up.notified()).await;
        }
    }
}

#[derive(Clone)]
struct DeviceServer(Arc<Inner>);

impl tonic::server::NamedService for DeviceServer {
    const NAME: &'static str = SERVICE;
}

type BoxFut<T, E> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, E>> + Send + 'static>>;

impl<B> tonic::codegen::Service<tonic::codegen::http::Request<B>> for DeviceServer
where
    B: tonic::codegen::Body + Send + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>> + Send + 'static,
{
    type Response = tonic::codegen::http::Response<tonic::body::Body>;
    type Error = std::convert::Infallible;
    type Future = BoxFut<Self::Response, Self::Error>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: tonic::codegen::http::Request<B>) -> Self::Future {
        let inner = self.0.clone();
        if req.uri().path() != "/SpaceX.API.Device.Device/Handle" {
            return Box::pin(async move { Ok(Status::unimplemented("").into_http()) });
        }
        Box::pin(async move {
            let req_desc = inner.pool.get_message_by_name("SpaceX.API.Device.Request").unwrap();
            let resp_desc = inner.pool.get_message_by_name("SpaceX.API.Device.Response").unwrap();
            let mut grpc = tonic::server::Grpc::new(DynCodec(req_desc)).max_decoding_message_size(4 << 20);
            let _ = resp_desc;
            Ok(grpc.unary(HandleSvc(inner), req).await)
        })
    }
}

struct HandleSvc(Arc<Inner>);

impl tonic::server::UnaryService<DynamicMessage> for HandleSvc {
    type Response = DynamicMessage;
    type Future = BoxFut<tonic::Response<DynamicMessage>, Status>;
    fn call(&mut self, request: tonic::Request<DynamicMessage>) -> Self::Future {
        let inner = self.0.clone();
        Box::pin(async move { handle(&inner, request.into_inner()).map(tonic::Response::new) })
    }
}

fn msg(pool: &DescriptorPool, name: &str) -> DynamicMessage {
    DynamicMessage::new(pool.get_message_by_name(&format!("SpaceX.API.Device.{name}")).unwrap())
}

fn set(m: &mut DynamicMessage, field: &str, v: PValue) {
    m.set_field_by_name(field, v);
}

fn handle(inner: &Arc<Inner>, req: DynamicMessage) -> Result<DynamicMessage, Status> {
    let kind = req
        .descriptor()
        .oneofs()
        .find(|o| o.name() == "request")
        .and_then(|o| o.fields().find(|f| req.has_field(f)))
        .map(|f| f.name().to_string())
        .ok_or_else(|| Status::invalid_argument("empty request"))?;
    let frozen = inner.cfg.frozen;
    let p = &inner.pool;
    let mut st = inner.state.lock().unwrap();
    if !st.up {
        return Err(Status::unavailable("dish is rebooting"));
    }
    *st.calls.entry(kind.clone()).or_insert(0) += 1;
    if let Some(n) = st.fail.get_mut(&kind)
        && *n > 0
    {
        *n -= 1;
        return Err(Status::unavailable("simulated failure"));
    }
    st.tick();
    let mut resp = msg(p, "Response");
    match kind.as_str() {
        "get_status" => set(&mut resp, "dish_get_status", PValue::Message(status_msg(p, &st, frozen))),
        "get_history" => set(&mut resp, "dish_get_history", PValue::Message(history_msg(p, &st, frozen))),
        "dish_get_obstruction_map" => {
            let mut m = msg(p, "DishGetObstructionMapResponse");
            set(&mut m, "num_rows", PValue::U32(st.map_rows));
            set(&mut m, "num_cols", PValue::U32(st.map_cols));
            set(&mut m, "snr", PValue::List(st.map_snr.iter().map(|x| PValue::F32(*x)).collect()));
            set(&mut m, "min_elevation_deg", PValue::F32(st.map_min_el));
            set(&mut m, "max_theta_deg", PValue::F32(st.map_max_theta));
            set(&mut m, "map_reference_frame", PValue::EnumNumber(st.map_frame));
            set(&mut resp, "dish_get_obstruction_map", PValue::Message(m));
        }
        "dish_get_config" => {
            let mut m = msg(p, "DishGetConfigResponse");
            set(&mut m, "dish_config", PValue::Message(config_msg(p, &st)));
            set(&mut resp, "dish_get_config", PValue::Message(m));
        }
        "dish_set_config" => {
            let c = req
                .get_field_by_name("dish_set_config")
                .and_then(|v| v.as_message().cloned())
                .and_then(|m| m.get_field_by_name("dish_config").and_then(|v| v.as_message().cloned()))
                .ok_or_else(|| Status::invalid_argument("no dish_config"))?;
            let on = |f: &str| c.get_field_by_name(&format!("apply_{f}")).and_then(|v| v.as_bool()).unwrap_or(false);
            let mut applied = serde_json::Map::new();
            for f in ["snow_melt_mode", "power_save_start_minutes", "power_save_duration_minutes", "power_save_mode"] {
                if !on(f) {
                    continue;
                }
                let v = c.get_field_by_name(f).map(|v| v.into_owned()).unwrap_or(PValue::Bool(false));
                match (f, &v) {
                    ("snow_melt_mode", PValue::EnumNumber(n)) => {
                        applied.insert(f.into(), (*n).into());
                        if !st.ignore_set_config {
                            st.snow_melt_mode = *n;
                        }
                    }
                    ("power_save_mode", PValue::Bool(b)) => {
                        applied.insert(f.into(), (*b).into());
                        if !st.ignore_set_config {
                            st.power_save_mode = *b;
                        }
                    }
                    (_, PValue::U32(n)) => {
                        applied.insert(f.into(), (*n).into());
                        if !st.ignore_set_config {
                            if f == "power_save_start_minutes" {
                                st.power_save_start = *n;
                            } else {
                                st.power_save_duration = *n;
                            }
                        }
                    }
                    _ => return Err(Status::invalid_argument(format!("bad {f}"))),
                }
            }
            st.set_configs.push(serde_json::Value::Object(applied));
            let mut m = msg(p, "DishSetConfigResponse");
            set(&mut m, "updated_dish_config", PValue::Message(config_msg(p, &st)));
            set(&mut resp, "dish_set_config", PValue::Message(m));
        }
        "update" => {
            st.update_checks += 1;
            if st.update_state == SW_IDLE && st.update_available {
                st.update_state = SW_FETCHING;
                st.fetch_until = Some(Instant::now() + inner.cfg.fetch_for);
            }
            set(&mut resp, "update", PValue::Message(msg(p, "UpdateResponse")));
        }
        "reboot" => {
            st.reboots += 1;
            if !st.ignore_reboot {
                let inner2 = inner.clone();
                tokio::spawn(async move { reboot(inner2).await });
            }
            if st.reboot_reply_error {
                return Err(Status::deadline_exceeded("Deadline Exceeded"));
            }
            set(&mut resp, "reboot", PValue::Message(msg(p, "RebootResponse")));
        }
        other => return Err(Status::unimplemented(format!("{other} is not simulated"))),
    }
    Ok(resp)
}

async fn reboot(inner: Arc<Inner>) {
    tokio::time::sleep(inner.cfg.reboot_delay).await;
    inner.state.lock().unwrap().up = false;
    if let Some(tx) = inner.shutdown.lock().unwrap().take() {
        let _ = tx.send(());
    }
    tokio::time::sleep(inner.cfg.down_for).await;
    {
        let mut st = inner.state.lock().unwrap();
        st.boot_now();
        st.up = true;
    }
    inner.back_up.notify_waiters();
}

fn config_msg(p: &DescriptorPool, st: &DishState) -> DynamicMessage {
    let mut c = msg(p, "DishConfig");
    set(&mut c, "snow_melt_mode", PValue::EnumNumber(st.snow_melt_mode));
    set(&mut c, "power_save_mode", PValue::Bool(st.power_save_mode));
    set(&mut c, "power_save_start_minutes", PValue::U32(st.power_save_start));
    set(&mut c, "power_save_duration_minutes", PValue::U32(st.power_save_duration));
    c
}

fn status_msg(p: &DescriptorPool, st: &DishState, frozen: bool) -> DynamicMessage {
    let wobble = |base: f32, amp: f32, period: f32| -> f32 {
        if frozen {
            base
        } else {
            let t = st.boot.elapsed().as_secs_f32();
            base + amp * (t / period).sin()
        }
    };
    let mut s = msg(p, "DishGetStatusResponse");
    let mut di = msg(p, "DeviceInfo");
    set(&mut di, "id", PValue::String(st.device_id.clone()));
    set(&mut di, "hardware_version", PValue::String(st.hardware_version.clone()));
    set(&mut di, "software_version", PValue::String(st.software_version.clone()));
    set(&mut s, "device_info", PValue::Message(di));
    let mut ds = msg(p, "DeviceState");
    set(&mut ds, "uptime_s", PValue::U64(st.uptime(frozen)));
    set(&mut s, "device_state", PValue::Message(ds));
    set(&mut s, "pop_ping_drop_rate", PValue::F32(st.drop_rate));
    set(&mut s, "pop_ping_latency_ms", PValue::F32(wobble(st.latency_ms, 4.0, 7.0)));
    set(&mut s, "downlink_throughput_bps", PValue::F32(wobble(st.down_bps, 20_000_000.0, 11.0).max(0.0)));
    set(&mut s, "uplink_throughput_bps", PValue::F32(wobble(st.up_bps, 2_000_000.0, 13.0).max(0.0)));
    set(&mut s, "boresight_azimuth_deg", PValue::F32(st.azimuth));
    set(&mut s, "boresight_elevation_deg", PValue::F32(st.elevation));
    let mut ob = msg(p, "DishObstructionStats");
    set(&mut ob, "fraction_obstructed", PValue::F32(st.fraction_obstructed));
    set(&mut ob, "currently_obstructed", PValue::Bool(st.currently_obstructed));
    set(&mut ob, "valid_s", PValue::F32(43_200.0));
    set(&mut s, "obstruction_stats", PValue::Message(ob));
    let mut al = msg(p, "DishAlerts");
    for a in &st.alerts {
        if al.descriptor().get_field_by_name(a).is_some() {
            set(&mut al, a, PValue::Bool(true));
        }
    }
    set(&mut s, "alerts", PValue::Message(al));
    if let Some(c) = st.outage_cause {
        let mut o = msg(p, "DishOutage");
        set(&mut o, "cause", PValue::EnumNumber(c));
        set(&mut s, "outage", PValue::Message(o));
    }
    let mut gps = msg(p, "DishGpsStats");
    set(&mut gps, "gps_valid", PValue::Bool(st.gps_valid));
    set(&mut gps, "gps_sats", PValue::U32(st.gps_sats));
    set(&mut s, "gps_stats", PValue::Message(gps));
    set(&mut s, "is_snr_above_noise_floor", PValue::Bool(st.snr_above_noise));
    set(&mut s, "is_snr_persistently_low", PValue::Bool(st.snr_persistently_low));
    set(&mut s, "software_update_state", PValue::EnumNumber(st.update_state));
    set(&mut s, "swupdate_reboot_ready", PValue::Bool(st.update_state == SW_REBOOT_REQUIRED));
    set(&mut s, "signal_quality", PValue::F32(st.signal_quality));
    s
}

fn history_msg(p: &DescriptorPool, st: &DishState, frozen: bool) -> DynamicMessage {
    let current = st.history_current_base + st.elapsed(frozen);
    let ring = 900u64;
    let sample = |k: u64| -> f32 { 38.0 + ((k % 13) as f32) * 0.5 };
    let mut power = Vec::with_capacity(ring as usize);
    for slot in 0..ring {
        if current <= slot {
            power.push(PValue::F32(0.0));
            continue;
        }
        let k = current - 1 - ((current - 1 - slot) % ring);
        power.push(PValue::F32(sample(k)));
    }
    let mut h = msg(p, "DishGetHistoryResponse");
    set(&mut h, "current", PValue::U64(current));
    set(&mut h, "power_in", PValue::List(power));
    let mut log = msg(p, "EventLog");
    let evs = st
        .events
        .iter()
        .map(|e| {
            let mut m = msg(p, "Event");
            set(&mut m, "reason", PValue::EnumNumber(e.reason));
            set(&mut m, "severity", PValue::EnumNumber(e.severity));
            set(&mut m, "start_timestamp_ns", PValue::I64(e.start_ns));
            set(&mut m, "duration_ns", PValue::I64(e.duration_ns));
            set(&mut m, "device_id", PValue::String(e.device_id.clone()));
            PValue::Message(m)
        })
        .collect();
    set(&mut log, "events", PValue::List(evs));
    set(&mut log, "start_timestamp_ns", PValue::I64(st.events_start_ns));
    set(&mut log, "current_timestamp_ns", PValue::I64(st.events_now_ns + if frozen { 0 } else { st.boot.elapsed().as_nanos() as i64 }));
    set(&mut h, "event_log", PValue::Message(log));
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The checked-in descriptor set is what protoc makes of the checked-in .proto (skipped without protoc).
    #[test]
    fn descriptor_set_matches_the_proto() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/proto");
        let r = std::process::Command::new("protoc")
            .current_dir(dir)
            .args(["--include_imports", "--descriptor_set_out=/dev/stdout", "aetherrv_sim/starlink_device.proto"])
            .output();
        match r {
            Ok(o) if o.status.success() => assert!(o.stdout == FDS, "regenerate proto/starlink.fds (see the .proto header)"),
            _ => eprintln!("protoc not available; descriptor check skipped"),
        }
        let p = pool();
        assert!(p.get_service_by_name(SERVICE).is_some());
        assert!(p.get_message_by_name("SpaceX.API.Device.DishOutage").is_some() && p.get_message_by_name("SpaceX.API.Device.DishAlerts").is_some());
        let legacy = DescriptorPool::decode(served_fds(true).as_slice()).unwrap();
        let st = legacy.get_message_by_name("SpaceX.API.Device.DishGetStatusResponse").unwrap();
        assert!(st.get_field_by_name("signal_quality").is_none() && st.get_field_by_name("is_snr_persistently_low").is_some());
    }

    #[tokio::test]
    async fn loopback_only() {
        let cfg = StarlinkSimConfig { bind: "0.0.0.0:0".parse().unwrap(), ..Default::default() };
        assert!(StarlinkSim::start(cfg).await.is_err());
    }
}
