//! The dish's local gRPC API without compiled protos (Python's `starlink_grpc` + yagrc): server reflection gives the
//! `SpaceX.API.Device.Device` descriptors, `Handle` is called with dynamic messages, and every field is found by
//! name, so a firmware that renumbers or adds fields still works.
//!
//! Two readers, as in Python:
//! * [`GrpcDish`] (Python `DishOps`): every call opens its OWN new channel and drops it, so nothing can repeat a
//!   send behind our back. Used by the commands (and the poll's config read, as in Python).
//! * [`Poller`] (Python `ChannelContext` + `call_with_channel`): one kept channel for the 5 s status poll; a failed
//!   read on a *reused* channel is retried once on a new one (reads only).

use crate::{DishError, DishFut, DishOps, EventLog, HistEvent, History, ObstructionMap, SignalRaw, config_fields};
use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Kind, MessageDescriptor, ReflectMessage, Value as PValue};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::transport::{Channel, Endpoint};
use tonic::{Code, Status};

pub const SERVICE: &str = "SpaceX.API.Device.Device";
/// starlink_grpc `REQUEST_TIMEOUT` and `DishOps._handle(timeout=10)`.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const QUERY_TIMEOUT: Duration = Duration::from_secs(10);

/// The descriptors resolved from the dish (Python's module-global `imports_pending` / `Request` / `DeviceStub`).
pub struct Reflected {
    pub pool: DescriptorPool,
    pub request: MessageDescriptor,
    pub response: MessageDescriptor,
    pub path: String,
}

/// Shared between the poller and the command ops: resolved once per process, as in Python.
pub type ReflectCache = Arc<Mutex<Option<Arc<Reflected>>>>;

fn derr(e: impl std::fmt::Display) -> DishError {
    DishError(e.to_string())
}

/// A gRPC failure as Python's `GrpcError` / `RpcError` text would carry it (tonic's own wording).
fn status_err(s: &Status) -> DishError {
    if s.message().is_empty() { DishError(format!("{:?}", s.code())) } else { DishError(s.message().to_string()) }
}

pub async fn connect(target: &str) -> Result<Channel, DishError> {
    let ep = Endpoint::from_shared(format!("http://{target}"))
        .map_err(derr)?
        .connect_timeout(REQUEST_TIMEOUT)
        .tcp_nodelay(true);
    ep.connect().await.map_err(|e| DishError(format!("failed to connect to {target}: {e}")))
}

// ── reflection ──────────────────────────────────────────────────────────────────────────────────────────────────

enum Q {
    Symbol(String),
    File(String),
}

macro_rules! reflect_query {
    ($name:ident, $m:path) => {
        async fn $name(ch: Channel, reqs: Vec<Q>) -> Result<Vec<Vec<u8>>, Status> {
            use $m as pb;
            use pb::server_reflection_request::MessageRequest;
            use pb::server_reflection_response::MessageResponse;
            let msgs: Vec<pb::ServerReflectionRequest> = reqs
                .into_iter()
                .map(|q| pb::ServerReflectionRequest {
                    host: String::new(),
                    message_request: Some(match q {
                        Q::Symbol(s) => MessageRequest::FileContainingSymbol(s),
                        Q::File(f) => MessageRequest::FileByFilename(f),
                    }),
                })
                .collect();
            let mut c = pb::server_reflection_client::ServerReflectionClient::new(ch);
            let mut req = tonic::Request::new(futures::stream::iter(msgs));
            req.set_timeout(QUERY_TIMEOUT);
            let fut = async {
                let mut stream = c.server_reflection_info(req).await?.into_inner();
                let mut out = Vec::new();
                while let Some(r) = stream.message().await? {
                    match r.message_response {
                        Some(MessageResponse::FileDescriptorResponse(f)) => out.extend(f.file_descriptor_proto),
                        Some(MessageResponse::ErrorResponse(e)) => {
                            // yagrc: ServiceError → starlink_grpc GrpcError("Reflection service error")
                            return Err(Status::new(Code::Internal, format!("Reflection service error: {}", e.error_message)));
                        }
                        _ => {}
                    }
                }
                Ok(out)
            };
            match tokio::time::timeout(QUERY_TIMEOUT + Duration::from_secs(1), fut).await {
                Ok(r) => r,
                Err(_) => Err(Status::deadline_exceeded("reflection query timed out")),
            }
        }
    };
}

reflect_query!(query_v1, tonic_reflection::pb::v1);
reflect_query!(query_v1alpha, tonic_reflection::pb::v1alpha);

/// yagrc `load_protocols(channel, symbols=["SpaceX.API.Device.Device"])`: v1 first, v1alpha when v1 is UNIMPLEMENTED;
/// the file holding the service, then every import by file name, until nothing is missing.
pub async fn resolve(ch: &Channel) -> Result<Reflected, Status> {
    let walk = |v1: bool| {
        let ch = ch.clone();
        async move {
            let mut protos: BTreeMap<String, prost_types::FileDescriptorProto> = BTreeMap::new();
            let mut traversed: BTreeSet<String> = BTreeSet::new();
            let mut reqs = vec![Q::Symbol(SERVICE.to_string())];
            while !reqs.is_empty() {
                let files = if v1 { query_v1(ch.clone(), reqs).await? } else { query_v1alpha(ch.clone(), reqs).await? };
                let mut deps = BTreeSet::new();
                for b in files {
                    let p = prost_types::FileDescriptorProto::decode(b.as_slice())
                        .map_err(|e| Status::internal(format!("bad file descriptor: {e}")))?;
                    traversed.insert(p.name().to_string());
                    deps.extend(p.dependency.iter().cloned());
                    protos.insert(p.name().to_string(), p);
                }
                let new: Vec<String> = deps.difference(&traversed).cloned().collect();
                traversed.extend(new.iter().cloned());
                reqs = new.into_iter().map(Q::File).collect();
            }
            Ok::<_, Status>(protos)
        }
    };
    let protos = match walk(true).await {
        Ok(p) => p,
        Err(s) if s.code() == Code::Unimplemented => walk(false).await?,
        Err(s) => return Err(s),
    };
    let mut pool = DescriptorPool::new();
    pool.add_file_descriptor_protos(protos.into_values()).map_err(|e| Status::internal(format!("Reflection service error: {e}")))?;
    let svc = pool.get_service_by_name(SERVICE).ok_or_else(|| Status::internal("Reflection service error: no Device service"))?;
    let m = svc.methods().find(|m| m.name() == "Handle").ok_or_else(|| Status::internal("Reflection service error: no Handle"))?;
    Ok(Reflected { request: m.input(), response: m.output(), path: format!("/{}/{}", svc.full_name(), m.name()), pool })
}

// ── a codec for dynamic messages ────────────────────────────────────────────────────────────────────────────────

struct DynCodec(MessageDescriptor);
struct DynEncoder;
struct DynDecoder(MessageDescriptor);

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

/// `Request(<field>={...})`. An unknown field is a protocol error (Python raises before sending).
pub fn request(r: &Reflected, field: &str, sub: Option<DynamicMessage>) -> Result<DynamicMessage, DishError> {
    let fd = r.request.get_field_by_name(field).ok_or_else(|| DishError(format!("Protocol message Request has no \"{field}\" field.")))?;
    let Kind::Message(md) = fd.kind() else { return Err(DishError(format!("Request.{field} is not a message"))) };
    let mut m = DynamicMessage::new(r.request.clone());
    m.set_field(&fd, PValue::Message(sub.unwrap_or_else(|| DynamicMessage::new(md))));
    Ok(m)
}

/// ONE `Handle` call on `ch` (no retry here).
pub async fn handle(ch: Channel, r: &Reflected, req: DynamicMessage) -> Result<DynamicMessage, Status> {
    let mut g = tonic::client::Grpc::new(ch);
    g.ready().await.map_err(|e| Status::unavailable(e.to_string()))?;
    let path = tonic::codegen::http::uri::PathAndQuery::try_from(r.path.as_str()).map_err(|e| Status::internal(e.to_string()))?;
    let mut rq = tonic::Request::new(req);
    rq.set_timeout(REQUEST_TIMEOUT);
    match tokio::time::timeout(REQUEST_TIMEOUT + Duration::from_secs(1), g.unary(rq, path, DynCodec(r.response.clone()))).await {
        Ok(r) => r.map(|x| x.into_inner()),
        Err(_) => Err(Status::deadline_exceeded("Deadline Exceeded")),
    }
}

/// `response.<field>` (a default message when another oneof member is set, as Python's attribute access gives).
pub fn sub(resp: &DynamicMessage, field: &str) -> Result<DynamicMessage, DishError> {
    let fd = resp.descriptor().get_field_by_name(field).ok_or_else(|| DishError(format!("Response has no field {field}")))?;
    match resp.get_field(&fd).as_ref() {
        PValue::Message(m) => Ok(m.clone()),
        _ => Err(DishError(format!("Response.{field} is not a message"))),
    }
}

// ── reading fields like Python's getattr(msg, name, None) ────────────────────────────────────────────────────

/// `None` when this firmware's message has no such field; the value (or its default) otherwise.
pub fn field(m: &DynamicMessage, name: &str) -> Option<PValue> {
    m.get_field_by_name(name).map(|c| c.into_owned())
}

fn sub_msg(m: Option<&DynamicMessage>, name: &str) -> Option<DynamicMessage> {
    match field(m?, name)? {
        PValue::Message(x) => Some(x),
        _ => None,
    }
}

/// A protobuf scalar as the JSON value Python would hold (`float(f32)` widened, ints as ints).
pub fn json_of(v: &PValue) -> Value {
    match v {
        PValue::Bool(b) => json!(b),
        PValue::I32(x) => json!(x),
        PValue::I64(x) => json!(x),
        PValue::U32(x) => json!(x),
        PValue::U64(x) => json!(x),
        PValue::F32(x) => aetherrv_core::num::jf(*x as f64),
        PValue::F64(x) => aetherrv_core::num::jf(*x),
        PValue::String(s) => json!(s),
        PValue::EnumNumber(n) => json!(n),
        _ => Value::Null,
    }
}

fn num_of(v: &PValue) -> Option<f64> {
    match v {
        PValue::I32(x) => Some(*x as f64),
        PValue::I64(x) => Some(*x as f64),
        PValue::U32(x) => Some(*x as f64),
        PValue::U64(x) => Some(*x as f64),
        PValue::F32(x) => Some(*x as f64),
        PValue::F64(x) => Some(*x),
        PValue::EnumNumber(n) => Some(*n as f64),
        PValue::Bool(b) => Some(*b as i32 as f64),
        _ => None,
    }
}

fn int_of(v: &PValue) -> Option<i64> {
    match v {
        PValue::I32(x) => Some(*x as i64),
        PValue::I64(x) => Some(*x),
        PValue::U32(x) => Some(*x as i64),
        PValue::U64(x) => Some(*x as i64),
        PValue::EnumNumber(n) => Some(*n as i64),
        PValue::Bool(b) => Some(*b as i64),
        _ => None,
    }
}

fn opt_json(m: Option<&DynamicMessage>, name: &str) -> Value {
    m.and_then(|m| field(m, name)).map(|v| json_of(&v)).unwrap_or(Value::Null)
}

fn enum_name(m: &DynamicMessage, name: &str) -> Option<String> {
    let fd = m.descriptor().get_field_by_name(name)?;
    let v = m.get_field(&fd);
    let n = match v.as_ref() {
        PValue::EnumNumber(n) => *n,
        other => return int_of(other).map(|x| x.to_string()),
    };
    match fd.kind() {
        Kind::Enum(e) => Some(e.get_value(n).map(|x| x.name().to_string()).unwrap_or_else(|| n.to_string())),
        _ => Some(n.to_string()),
    }
}

/// starlink_grpc `status_data()` (the status dict, the alerts in field-declaration order) plus the raw signal fields
/// for `signal_fields` (Python reads those from a second `get_status()`; here from the same response).
pub fn status_data(status: &DynamicMessage) -> (Map<String, Value>, Vec<(String, bool)>, SignalRaw) {
    let desc = status.descriptor();
    // state: the outage cause, NO_SCHEDULE → SEARCHING, else CONNECTED; anything missing → UNKNOWN
    let state = (|| -> Option<String> {
        let fd = desc.get_field_by_name("outage")?;
        let Kind::Message(od) = fd.kind() else { return None };
        if !status.has_field(&fd) {
            return Some("CONNECTED".into());
        }
        let PValue::Message(o) = status.get_field(&fd).into_owned() else { return None };
        let cfd = od.get_field_by_name("cause")?;
        let Kind::Enum(ce) = cfd.kind() else { return None };
        let no_schedule = ce.get_value_by_name("NO_SCHEDULE")?.number();
        let n = o.get_field(&cfd).as_enum_number()?;
        if n == no_schedule {
            return Some("SEARCHING".into());
        }
        Some(ce.get_value(n).map(|v| v.name().to_string()).unwrap_or_else(|| n.to_string()))
    })()
    .unwrap_or_else(|| "UNKNOWN".into());

    let mut alerts = Vec::new();
    if let Some(a) = sub_msg(Some(status), "alerts") {
        // Python iterates DESCRIPTOR.fields: declaration order
        let ad = a.descriptor();
        for fp in &ad.descriptor_proto().field {
            if let Some(f) = ad.get_field_by_name(fp.name()) {
                alerts.push((format!("alert_{}", f.name()), a.has_field(&f)));
            }
        }
    }

    let device_info = sub_msg(Some(status), "device_info");
    let device_state = sub_msg(Some(status), "device_state");
    let obs = sub_msg(Some(status), "obstruction_stats");
    let gps = sub_msg(Some(status), "gps_stats");
    let top = |n: &str| opt_json(Some(status), n);
    let mut d = Map::new();
    d.insert("hardware_version".into(), opt_json(device_info.as_ref(), "hardware_version"));
    d.insert("software_version".into(), opt_json(device_info.as_ref(), "software_version"));
    d.insert("state".into(), json!(state));
    d.insert("uptime".into(), opt_json(device_state.as_ref(), "uptime_s"));
    d.insert("pop_ping_drop_rate".into(), top("pop_ping_drop_rate"));
    d.insert("downlink_throughput_bps".into(), top("downlink_throughput_bps"));
    d.insert("uplink_throughput_bps".into(), top("uplink_throughput_bps"));
    d.insert("pop_ping_latency_ms".into(), top("pop_ping_latency_ms"));
    d.insert("fraction_obstructed".into(), opt_json(obs.as_ref(), "fraction_obstructed"));
    d.insert("currently_obstructed".into(), opt_json(obs.as_ref(), "currently_obstructed"));
    d.insert("direction_azimuth".into(), top("boresight_azimuth_deg"));
    d.insert("direction_elevation".into(), top("boresight_elevation_deg"));
    d.insert("is_snr_above_noise_floor".into(), top("is_snr_above_noise_floor"));
    d.insert("gps_sats".into(), opt_json(gps.as_ref(), "gps_sats"));
    d.insert("software_update_state".into(), enum_name(status, "software_update_state").map(Value::String).unwrap_or(Value::Null));

    let raw = SignalRaw {
        signal_quality: field(status, "signal_quality").as_ref().and_then(num_of),
        is_snr_above_noise_floor: field(status, "is_snr_above_noise_floor").and_then(|v| v.as_bool()),
        is_snr_persistently_low: field(status, "is_snr_persistently_low").and_then(|v| v.as_bool()),
    };
    (d, alerts, raw)
}

pub fn obstruction_map(m: &DynamicMessage) -> Result<ObstructionMap, DishError> {
    let need = |n: &str| field(m, n).ok_or_else(|| DishError(format!("'DishGetObstructionMapResponse' object has no attribute '{n}'")));
    let snr = match need("snr")? {
        PValue::List(l) => l.iter().filter_map(num_of).collect(),
        _ => return Err(DishError("snr is not a list".into())),
    };
    Ok(ObstructionMap {
        num_rows: need("num_rows").map(|v| int_of(&v).unwrap_or(0))?,
        num_cols: need("num_cols").map(|v| int_of(&v).unwrap_or(0))?,
        snr,
        min_elevation_deg: num_of(&need("min_elevation_deg")?),
        max_theta_deg: num_of(&need("max_theta_deg")?),
        map_reference_frame: int_of(&need("map_reference_frame")?),
    })
}

pub fn history(h: &DynamicMessage) -> Result<History, DishError> {
    let power_in = match field(h, "power_in") {
        Some(PValue::List(l)) => l.iter().filter_map(num_of).collect(),
        _ => Vec::new(),
    };
    let current = field(h, "current").as_ref().and_then(int_of).unwrap_or(0);
    let event_log = match field(h, "event_log") {
        Some(PValue::Message(log)) => {
            let mut events = Vec::new();
            if let Some(PValue::List(evs)) = field(&log, "events") {
                for e in evs {
                    let PValue::Message(e) = e else { continue };
                    let need = |n: &str| {
                        field(&e, n).as_ref().and_then(int_of).ok_or_else(|| DishError(format!("event has no attribute '{n}'")))
                    };
                    let names = |n: &str| -> (i64, Option<String>) {
                        let num = field(&e, n).as_ref().and_then(int_of).unwrap_or(0);
                        let name = e.descriptor().get_field_by_name(n).and_then(|fd| match fd.kind() {
                            Kind::Enum(en) => en.get_value(num as i32).map(|v| v.name().to_string()),
                            _ => None,
                        });
                        (num, name)
                    };
                    let (reason, reason_name) = names("reason");
                    let (severity, severity_name) = names("severity");
                    events.push(HistEvent {
                        reason,
                        severity,
                        reason_name,
                        severity_name,
                        start_timestamp_ns: need("start_timestamp_ns")?,
                        duration_ns: need("duration_ns")?,
                    });
                }
            }
            Some(EventLog {
                events,
                start_timestamp_ns: field(&log, "start_timestamp_ns").as_ref().and_then(int_of).unwrap_or(0),
                current_timestamp_ns: field(&log, "current_timestamp_ns").as_ref().and_then(int_of).unwrap_or(0),
            })
        }
        _ => None,
    };
    Ok(History { power_in, current, event_log })
}

/// The DishConfig fields `config_fields` reads (`getattr(c, name, default)`).
pub fn config_of(c: &DynamicMessage) -> Value {
    let i = |n: &str| field(c, n).as_ref().and_then(int_of).unwrap_or(0);
    let b = |n: &str| field(c, n).and_then(|v| v.as_bool()).unwrap_or(false);
    config_fields(i("snow_melt_mode"), b("power_save_mode"), i("power_save_start_minutes"), i("power_save_duration_minutes"))
}

/// `dish_set_config={"dish_config": {field: value, "apply_<field>": True, ...}}`.
fn set_config_request(r: &Reflected, fields: &Map<String, Value>) -> Result<DynamicMessage, DishError> {
    let sfd = r.request.get_field_by_name("dish_set_config").ok_or_else(|| DishError("Request has no dish_set_config".into()))?;
    let Kind::Message(smd) = sfd.kind() else { return Err(DishError("dish_set_config is not a message".into())) };
    let cfd = smd.get_field_by_name("dish_config").ok_or_else(|| DishError("DishSetConfigRequest has no dish_config".into()))?;
    let Kind::Message(cmd) = cfd.kind() else { return Err(DishError("dish_config is not a message".into())) };
    let mut c = DynamicMessage::new(cmd.clone());
    for (k, v) in fields {
        for (name, val) in [(k.clone(), v.clone()), (format!("apply_{k}"), Value::Bool(true))] {
            let fd = cmd.get_field_by_name(&name).ok_or_else(|| DishError(format!("Protocol message DishConfig has no \"{name}\" field.")))?;
            let pv = match (fd.kind(), &val) {
                (Kind::Bool, Value::Bool(b)) => PValue::Bool(*b),
                (Kind::Enum(_), Value::Number(n)) => PValue::EnumNumber(n.as_i64().unwrap_or(0) as i32),
                (Kind::Uint32, Value::Number(n)) => PValue::U32(n.as_u64().ok_or_else(|| DishError(format!("bad {name}")))? as u32),
                (Kind::Int32, Value::Number(n)) => PValue::I32(n.as_i64().ok_or_else(|| DishError(format!("bad {name}")))? as i32),
                (Kind::Uint64, Value::Number(n)) => PValue::U64(n.as_u64().ok_or_else(|| DishError(format!("bad {name}")))?),
                (Kind::Int64, Value::Number(n)) => PValue::I64(n.as_i64().ok_or_else(|| DishError(format!("bad {name}")))?),
                (k, v) => return Err(DishError(format!("DishConfig.{name}: cannot set {k:?} from {v}"))),
            };
            c.set_field(&fd, pv);
        }
    }
    let mut s = DynamicMessage::new(smd);
    s.set_field(&cfd, PValue::Message(c));
    request(r, "dish_set_config", Some(s))
}

async fn reflected(cache: &ReflectCache, ch: &Channel) -> Result<Arc<Reflected>, Status> {
    if let Some(r) = cache.lock().unwrap().clone() {
        return Ok(r);
    }
    let r = Arc::new(resolve(ch).await?);
    *cache.lock().unwrap() = Some(r.clone());
    Ok(r)
}

// ── command ops: a new channel per call ─────────────────────────────────────────────────────────────────────────

pub struct GrpcDish {
    pub target: String,
    pub cache: ReflectCache,
}

impl GrpcDish {
    pub fn new(target: &str, cache: ReflectCache) -> GrpcDish {
        GrpcDish { target: target.to_string(), cache }
    }

    /// One `Handle` call on a brand-new channel, closed afterwards. Never retried.
    pub async fn call(&self, field: &str, sub_msg: Option<&Map<String, Value>>) -> Result<DynamicMessage, DishError> {
        let ch = connect(&self.target).await?;
        let r = reflected(&self.cache, &ch).await.map_err(|s| status_err(&s))?;
        let req = match sub_msg {
            Some(fields) => set_config_request(&r, fields)?,
            None => request(&r, field, None)?,
        };
        handle(ch, &r, req).await.map_err(|s| status_err(&s))
    }
}

impl DishOps for GrpcDish {
    fn status(&self) -> DishFut<'_, Map<String, Value>> {
        Box::pin(async move {
            let resp = self.call("get_status", None).await?;
            Ok(status_data(&sub(&resp, "dish_get_status")?).0)
        })
    }
    fn reboot(&self) -> DishFut<'_, ()> {
        Box::pin(async move { self.call("reboot", None).await.map(|_| ()) })
    }
    fn get_config(&self) -> DishFut<'_, Value> {
        Box::pin(async move {
            let resp = self.call("dish_get_config", None).await?;
            let c = sub(&sub(&resp, "dish_get_config")?, "dish_config")?;
            Ok(config_of(&c))
        })
    }
    fn set_config(&self, fields: Map<String, Value>) -> DishFut<'_, ()> {
        Box::pin(async move { self.call("dish_set_config", Some(&fields)).await.map(|_| ()) })
    }
    fn check_update(&self) -> DishFut<'_, ()> {
        Box::pin(async move { self.call("update", None).await.map(|_| ()) })
    }
}

// ── the poll reader: one kept channel, a read retried once on a new channel if the kept one failed ─────────────

pub struct Poller {
    pub target: String,
    pub cache: ReflectCache,
    chan: Option<Channel>,
}

impl Poller {
    pub fn new(target: &str, cache: ReflectCache) -> Poller {
        Poller { target: target.to_string(), cache, chan: None }
    }

    /// starlink_grpc `call_with_channel`: a gRPC failure on a reused channel closes it and tries once more.
    pub async fn read(&mut self, field: &str) -> Result<DynamicMessage, DishError> {
        loop {
            let (ch, reused) = match &self.chan {
                Some(c) => (c.clone(), true),
                None => {
                    let c = connect(&self.target).await?;
                    self.chan = Some(c.clone());
                    (c, false)
                }
            };
            let r = match reflected(&self.cache, &ch).await {
                Ok(r) => r,
                Err(s) => {
                    self.chan = None;
                    if reused && s.code() != Code::Internal {
                        continue;
                    }
                    return Err(status_err(&s));
                }
            };
            let req = request(&r, field, None)?;
            match handle(ch, &r, req).await {
                Ok(m) => return Ok(m),
                Err(s) => {
                    self.chan = None;
                    if !reused {
                        return Err(status_err(&s));
                    }
                }
            }
        }
    }

    /// Python `ctx.close()`.
    pub fn close(&mut self) {
        self.chan = None;
    }
}
