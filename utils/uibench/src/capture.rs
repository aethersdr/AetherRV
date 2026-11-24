//! Frame timing and pixels from the compositor, built so that measuring does not slow the thing measured.
//!
//! - **Ticker**: a 16×16 `wlr-screencopy-unstable-v1` region copied with `copy_with_damage` on its own Wayland
//!   connection. wlroots tracks screencopy damage per client and not per region, so this copy completes once for
//!   every frame the compositor commits with damage anywhere on the output, and its `ready` time is that commit's
//!   time (CLOCK_MONOTONIC). These are the presented frames the fps and frame-interval figures come from.
//!   (`wlr-export-dmabuf` was tried first and rejected: each request makes sway render a frame, so a static page
//!   read ~62 fps. Calibration in docs/UI-RUNTIME-BENCHMARK.md.)
//! - **Patches**: small regions (`capture_output_region` + `copy_with_damage`) copied into GPU buffers
//!   (linux-dmabuf via GBM), one connection each, read once their fence has signalled. Each copy carries the
//!   commit time of the frame it came from, so patch contents line up with ticker frames exactly.
//!
//! Calibration on the Pi 5: a full-output copy every frame costs sway ~7.7 ms per frame and drops a 60 Hz page to
//! ~51 fps; a few small regions per frame cost nothing measurable, 12 or more back the GPU up. Reading a dmabuf
//! straight after `ready` waits ~8 ms for the copy's fence, so reads happen when the fd polls readable.

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use std::collections::HashMap;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wayland_client::protocol::{wl_buffer, wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{zwp_linux_buffer_params_v1 as params_v1, zwp_linux_dmabuf_v1 as dmabuf_v1};
use wayland_protocols_wlr::screencopy::v1::client::{zwlr_screencopy_frame_v1 as frame_v1, zwlr_screencopy_manager_v1 as mgr_v1};

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, Default, Hash)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}
impl Rect {
    pub fn clip(&self, w: i32, h: i32) -> Rect {
        let x0 = self.x.clamp(0, w);
        let y0 = self.y.clamp(0, h);
        let x1 = (self.x + self.w).clamp(0, w);
        let y1 = (self.y + self.h).clamp(0, h);
        Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
    }
    pub fn center(&self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }
}

/// A region copied whenever it is damaged; every `step`-th pixel is kept, as RGB triples (row-major).
#[derive(Clone, Debug, PartialEq)]
pub struct Patch {
    pub id: u32,
    pub rect: Rect,
    pub step: i32,
}
impl Patch {
    pub fn dims(&self) -> (usize, usize) {
        let s = self.step.max(1);
        (((self.rect.w + s - 1) / s) as usize, ((self.rect.h + s - 1) / s) as usize)
    }
}

/// New contents of a patch at presentation time `t`.
#[derive(Clone, Debug)]
pub struct Update {
    pub id: u32,
    pub t: u64,
    /// when the copy reached us (diagnostics)
    #[allow(dead_code)]
    pub recv: u64,
    pub px: Arc<Vec<u8>>,
}

#[derive(Default, Clone, Debug, Serialize)]
pub struct Info {
    pub output: String,
    pub width: i32,
    pub height: i32,
    pub refresh_mhz: i32,
    pub ticker: String,
    pub patch_buffer: String,
    pub format: u32,
    pub ticks: u64,
    pub tick_cancels: u64,
    pub patch_updates: u64,
    pub patch_failed: u64,
    /// time to read a patch back, µs (last 512)
    pub read_us: Vec<u32>,
    /// export-dmabuf ready delivery delay (recv − presentation), µs (last 512)
    pub tick_delay_us: Vec<u32>,
    /// screencopy ready → the copy's fence signalled, µs (last 512)
    pub fence_us: Vec<u32>,
}

#[derive(Default)]
struct Shared {
    ticks: Vec<u64>,
    updates: Vec<Update>,
    patches: Vec<Patch>,
    generation: u64,
    force_req: Option<(u64, Vec<Patch>)>,
    forced: HashMap<u64, Vec<Update>>,
    stop: bool,
    err: Option<String>,
    info: Info,
}

pub struct Recorder {
    sh: Arc<Mutex<Shared>>,
    th: Option<std::thread::JoinHandle<()>>,
    next_force: std::sync::atomic::AtomicU64,
}

impl Recorder {
    pub fn start(output: Option<String>) -> Result<Self> {
        let sh = Arc::new(Mutex::new(Shared::default()));
        let (tx, rx) = std::sync::mpsc::channel::<Result<()>>();
        let sh2 = sh.clone();
        let th = std::thread::Builder::new().name("capture".into()).spawn(move || {
            if let Err(e) = run(sh2.clone(), output, &tx) {
                let _ = tx.send(Err(anyhow!("{e:#}")));
                sh2.lock().unwrap().err = Some(format!("{e:#}"));
            }
        })?;
        rx.recv_timeout(Duration::from_secs(10)).map_err(|_| anyhow!("capture: not ready in 10 s"))??;
        Ok(Recorder { sh, th: Some(th), next_force: 1.into() })
    }
    pub fn info(&self) -> Info {
        self.sh.lock().unwrap().info.clone()
    }
    /// Replace the set of patches being copied (empty = ticker only).
    pub fn set_patches(&self, p: Vec<Patch>) {
        let mut s = self.sh.lock().unwrap();
        if s.patches != p {
            s.patches = p;
            s.generation += 1;
        }
    }
    /// Take everything recorded so far: (presentation times of all frames, patch updates).
    pub fn drain(&self) -> (Vec<u64>, Vec<Update>) {
        let mut s = self.sh.lock().unwrap();
        (std::mem::take(&mut s.ticks), std::mem::take(&mut s.updates))
    }
    pub fn clear(&self) {
        let _ = self.drain();
    }
    /// Median fence latency (copy ready → readable) since the last call, ms: how busy the GPU was.
    pub fn take_fence_ms(&self) -> Option<f64> {
        let mut s = self.sh.lock().unwrap();
        let v = std::mem::take(&mut s.info.fence_us);
        crate::analysis::pct_u32(&v, 50.0).map(|u| u as f64 / 1000.0)
    }
    /// Copy these regions now (forces a compositor frame; for references and baselines, never a measured frame).
    pub fn force(&self, patches: Vec<Patch>) -> Result<Vec<Update>> {
        let id = self.next_force.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let n = patches.len();
        self.sh.lock().unwrap().force_req = Some((id, patches));
        for _ in 0..600 {
            std::thread::sleep(Duration::from_millis(5));
            let mut s = self.sh.lock().unwrap();
            if s.forced.get(&id).is_some_and(|v| v.len() >= n) {
                return Ok(s.forced.remove(&id).unwrap());
            }
            if let Some(e) = &s.err {
                bail!("capture: {e}");
            }
        }
        bail!("forced capture timed out")
    }
    pub fn stop(mut self) {
        self.sh.lock().unwrap().stop = true;
        if let Some(t) = self.th.take() {
            let _ = t.join();
        }
    }
}

// ── libgbm (Mesa) ──
#[allow(non_camel_case_types)]
type gbm_ptr = *mut libc::c_void;
#[link(name = "gbm")]
unsafe extern "C" {
    fn gbm_create_device(fd: libc::c_int) -> gbm_ptr;
    fn gbm_bo_create_with_modifiers2(dev: gbm_ptr, w: u32, h: u32, format: u32, mods: *const u64, count: libc::c_uint, flags: u32) -> gbm_ptr;
    fn gbm_bo_create(dev: gbm_ptr, w: u32, h: u32, format: u32, flags: u32) -> gbm_ptr;
    fn gbm_bo_destroy(bo: gbm_ptr);
    fn gbm_bo_get_fd(bo: gbm_ptr) -> libc::c_int;
    fn gbm_bo_get_stride(bo: gbm_ptr) -> u32;
    fn gbm_bo_get_offset(bo: gbm_ptr, plane: libc::c_int) -> u32;
    fn gbm_bo_get_modifier(bo: gbm_ptr) -> u64;
    fn gbm_bo_map(bo: gbm_ptr, x: u32, y: u32, w: u32, h: u32, flags: u32, stride: *mut u32, map_data: *mut *mut libc::c_void) -> *mut libc::c_void;
    fn gbm_bo_unmap(bo: gbm_ptr, map_data: *mut libc::c_void);
}
const GBM_BO_USE_RENDERING: u32 = 1 << 2;
const GBM_BO_USE_LINEAR: u32 = 1 << 4;
const GBM_BO_TRANSFER_READ: u32 = 1;

struct Dma {
    buffer: wl_buffer::WlBuffer,
    bo: gbm_ptr,
    w: i32,
    h: i32,
    fd: OwnedFd,
    /// persistent read mapping of the dmabuf (None: fall back to gbm_bo_map per read)
    map: Option<(*const u8, usize, usize)>, // ptr, len, stride
}
impl Drop for Dma {
    fn drop(&mut self) {
        self.buffer.destroy();
        if let Some((p, len, _)) = self.map {
            unsafe { libc::munmap(p as *mut libc::c_void, len) };
        }
        unsafe { gbm_bo_destroy(self.bo) };
    }
}
// DMA_BUF_IOCTL_SYNC = _IOW('b', 0, struct dma_buf_sync { __u64 flags; })
const DMA_BUF_IOCTL_SYNC: u64 = (1 << 30) | (8 << 16) | (0x62 << 8);
const DMA_BUF_SYNC_READ: u64 = 1;
const DMA_BUF_SYNC_END: u64 = 4;
fn dmabuf_sync(fd: i32, flags: u64) -> bool {
    let f = flags;
    unsafe { libc::ioctl(fd, DMA_BUF_IOCTL_SYNC as _, &f as *const u64) == 0 }
}
unsafe impl Send for Dma {}

#[derive(Default)]
struct FrameEv {
    dma: Option<(u32, i32, i32)>,
    buffer_done: bool,
    flags: u32,
    ready: Option<u64>,
    failed: bool,
}

struct OutInfo {
    o: wl_output::WlOutput,
    name: Option<String>,
    w: i32,
    h: i32,
    refresh: i32,
}

struct St {
    outputs: Vec<OutInfo>,
    mgr: Option<mgr_v1::ZwlrScreencopyManagerV1>,
    dmabuf: Option<dmabuf_v1::ZwpLinuxDmabufV1>,
    fev: HashMap<u64, FrameEv>,
    created: Option<Result<wl_buffer::WlBuffer, ()>>,
}

fn wenum<T: Into<u32>>(w: WEnum<T>) -> u32 {
    match w {
        WEnum::Value(v) => v.into(),
        WEnum::Unknown(u) => u,
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for St {
    fn event(st: &mut Self, reg: &wl_registry::WlRegistry, ev: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, version } = ev {
            match interface.as_str() {
                "wl_output" => {
                    let o = reg.bind::<wl_output::WlOutput, _, _>(name, version.min(4), qh, ());
                    st.outputs.push(OutInfo { o, name: None, w: 0, h: 0, refresh: 0 });
                }
                "zwlr_screencopy_manager_v1" if version >= 3 => st.mgr = Some(reg.bind(name, 3, qh, ())),
                "zwp_linux_dmabuf_v1" if version >= 3 => st.dmabuf = Some(reg.bind(name, 3, qh, ())),
                _ => {}
            }
        }
    }
}
impl Dispatch<wl_output::WlOutput, ()> for St {
    fn event(st: &mut Self, o: &wl_output::WlOutput, ev: wl_output::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let Some(oi) = st.outputs.iter_mut().find(|x| &x.o == o) else { return };
        match ev {
            wl_output::Event::Name { name } => oi.name = Some(name),
            wl_output::Event::Mode { flags, width, height, refresh } => {
                if wenum(flags) & 1 != 0 {
                    (oi.w, oi.h, oi.refresh) = (width, height, refresh);
                }
            }
            _ => {}
        }
    }
}
impl Dispatch<wl_buffer::WlBuffer, ()> for St {
    fn event(_: &mut Self, _: &wl_buffer::WlBuffer, _: wl_buffer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<mgr_v1::ZwlrScreencopyManagerV1, ()> for St {
    fn event(_: &mut Self, _: &mgr_v1::ZwlrScreencopyManagerV1, _: mgr_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<dmabuf_v1::ZwpLinuxDmabufV1, ()> for St {
    fn event(_: &mut Self, _: &dmabuf_v1::ZwpLinuxDmabufV1, _: dmabuf_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<params_v1::ZwpLinuxBufferParamsV1, ()> for St {
    fn event(st: &mut Self, _: &params_v1::ZwpLinuxBufferParamsV1, ev: params_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            params_v1::Event::Created { buffer } => st.created = Some(Ok(buffer)),
            params_v1::Event::Failed => st.created = Some(Err(())),
            _ => {}
        }
    }
    wayland_client::event_created_child!(St, params_v1::ZwpLinuxBufferParamsV1, [
        params_v1::EVT_CREATED_OPCODE => (wl_buffer::WlBuffer, ()),
    ]);
}
impl Dispatch<frame_v1::ZwlrScreencopyFrameV1, u64> for St {
    fn event(st: &mut Self, _: &frame_v1::ZwlrScreencopyFrameV1, ev: frame_v1::Event, key: &u64, _: &Connection, _: &QueueHandle<Self>) {
        let fe = st.fev.entry(*key).or_default();
        match ev {
            frame_v1::Event::LinuxDmabuf { format, width, height } => fe.dma = Some((format, width as i32, height as i32)),
            frame_v1::Event::BufferDone => fe.buffer_done = true,
            frame_v1::Event::Flags { flags } => fe.flags = wenum(flags),
            frame_v1::Event::Ready { tv_sec_hi, tv_sec_lo, tv_nsec } => {
                fe.ready = Some((((tv_sec_hi as u64) << 32) | tv_sec_lo as u64) * 1_000_000_000 + tv_nsec as u64)
            }
            frame_v1::Event::Failed => fe.failed = true,
            _ => {}
        }
    }
}
fn would_block(e: &wayland_client::backend::WaylandError) -> bool {
    matches!(e, wayland_client::backend::WaylandError::Io(io) if io.kind() == std::io::ErrorKind::WouldBlock)
}

fn dispatch_timeout(q: &mut EventQueue<St>, st: &mut St, ms: i32) -> Result<()> {
    // a full socket buffer (many patch requests at once) is not an error: the rest goes out on the next flush
    if let Err(e) = q.flush()
        && !would_block(&e)
    {
        return Err(e.into());
    }
    q.dispatch_pending(st)?;
    if let Some(guard) = q.prepare_read() {
        let fd = guard.connection_fd().as_raw_fd();
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        if unsafe { libc::poll(&mut pfd, 1, ms) } > 0
            && let Err(e) = guard.read()
            && !would_block(&e)
        {
            return Err(e.into());
        }
    }
    q.dispatch_pending(st)?;
    Ok(())
}

struct Gbm(gbm_ptr);
unsafe impl Send for Gbm {}

fn gbm_device() -> Result<Gbm> {
    let node = std::env::var("UIBENCH_RENDER_NODE").unwrap_or_else(|_| "/dev/dri/renderD128".into());
    let f = std::fs::OpenOptions::new().read(true).write(true).open(&node).with_context(|| node.clone())?;
    let dfd: OwnedFd = f.into();
    let dev = unsafe { gbm_create_device(dfd.as_raw_fd()) };
    std::mem::forget(dfd); // used by the gbm device for the life of the process
    if dev.is_null() {
        bail!("gbm_create_device {node}");
    }
    Ok(Gbm(dev))
}

fn alloc_dma(st: &mut St, q: &mut EventQueue<St>, qh: &QueueHandle<St>, gbm: &Gbm, fmt: u32, w: i32, h: i32) -> Result<Dma> {
    let dmabuf = st.dmabuf.clone().ok_or_else(|| anyhow!("compositor has no zwp_linux_dmabuf_v1"))?;
    let linear = [0u64];
    let mut bo = unsafe { gbm_bo_create_with_modifiers2(gbm.0, w as u32, h as u32, fmt, linear.as_ptr(), 1, GBM_BO_USE_RENDERING) };
    if bo.is_null() {
        bo = unsafe { gbm_bo_create(gbm.0, w as u32, h as u32, fmt, GBM_BO_USE_RENDERING | GBM_BO_USE_LINEAR) };
    }
    if bo.is_null() {
        bail!("gbm_bo_create {w}x{h} {fmt:#x}");
    }
    let fd = unsafe { gbm_bo_get_fd(bo) };
    if fd < 0 {
        unsafe { gbm_bo_destroy(bo) };
        bail!("gbm_bo_get_fd");
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let (stride, offset, modifier) = unsafe { (gbm_bo_get_stride(bo), gbm_bo_get_offset(bo, 0), gbm_bo_get_modifier(bo)) };
    let params = dmabuf.create_params(qh, ());
    params.add(fd.as_fd(), 0, offset, stride, (modifier >> 32) as u32, modifier as u32);
    st.created = None;
    params.create(w, h, fmt, params_v1::Flags::empty());
    for _ in 0..200 {
        dispatch_timeout(q, st, 10)?;
        if st.created.is_some() {
            break;
        }
    }
    params.destroy();
    match st.created.take() {
        Some(Ok(buffer)) => {
            let len = stride as usize * h as usize + offset as usize;
            let p = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_SHARED, fd.as_raw_fd(), 0) };
            let map = (p != libc::MAP_FAILED && std::env::var("UIBENCH_GBM_MAP").is_err())
                .then(|| (unsafe { (p as *const u8).add(offset as usize) }, len, stride as usize));
            Ok(Dma { buffer, bo, w, h, fd, map })
        }
        _ => {
            unsafe { gbm_bo_destroy(bo) };
            bail!("zwp_linux_buffer_params_v1 failed (modifier {modifier:#x})")
        }
    }
}

/// RGB triples of every `step`-th pixel. Each sampled row is copied in bulk first (the mapping is uncached).
fn read_patch(d: &Dma, fmt: u32, yinv: bool, step: i32) -> Vec<u8> {
    let bgr = matches!(fmt, 0x34325258 | 0x34325241); // DRM XR24 / AR24 = B,G,R,X in memory; XB24 / AB24 = R,G,B,X
    let s = step.max(1) as usize;
    let mut out = Vec::new();
    unsafe {
        let mut stride = 0u32;
        let mut md: *mut libc::c_void = std::ptr::null_mut();
        // persistent mapping + DMA_BUF_IOCTL_SYNC (waits for the compositor's copy to land, keeps caches right)
        let p = if let Some((p, _, st)) = d.map {
            stride = st as u32;
            dmabuf_sync(d.fd.as_raw_fd(), DMA_BUF_SYNC_READ);
            p
        } else {
            gbm_bo_map(d.bo, 0, 0, d.w as u32, d.h as u32, GBM_BO_TRANSFER_READ, &mut stride, &mut md) as *const u8
        };
        if p.is_null() {
            return out;
        }
        let span = d.w as usize * 4;
        let mut row = vec![0u8; span];
        let mut y = 0usize;
        while y < d.h as usize {
            let sy = if yinv { d.h as usize - 1 - y } else { y };
            std::ptr::copy_nonoverlapping(p.add(sy * stride as usize), row.as_mut_ptr(), span);
            let mut x = 0usize;
            while x < d.w as usize {
                let px = &row[x * 4..x * 4 + 4];
                if bgr {
                    out.extend_from_slice(&[px[2], px[1], px[0]]);
                } else {
                    out.extend_from_slice(&[px[0], px[1], px[2]]);
                }
                x += s;
            }
            y += s;
        }
        if d.map.is_some() {
            dmabuf_sync(d.fd.as_raw_fd(), DMA_BUF_SYNC_READ | DMA_BUF_SYNC_END);
        } else {
            gbm_bo_unmap(d.bo, md);
        }
    }
    out
}

fn push_cap(v: &mut Vec<u32>, x: u32) {
    if v.len() >= 512 {
        v.remove(0);
    }
    v.push(x);
}

enum Phase {
    Idle,
    AwaitBuffer(frame_v1::ZwlrScreencopyFrameV1),
    Copying(frame_v1::ZwlrScreencopyFrameV1, Dma),
}

/// A copied buffer waiting for its GPU fence (dma-buf poll: readable once the compositor's blit has landed).
struct Pending {
    buf: Dma,
    t: u64,
    recv: u64,
    yinv: bool,
}

/// One Wayland connection. wlroots tracks screencopy damage per client: with several region captures on one
/// connection, the copy for one region consumes the damage another is waiting for. So every patch gets its own
/// connection (pooled and reused), and the ticker has the first one.
struct Conn {
    pool: HashMap<(i32, i32), Vec<Dma>>, // dropped before the connection (field order)
    st: St,
    output: wl_output::WlOutput,
    mgr: mgr_v1::ZwlrScreencopyManagerV1,
    qh: QueueHandle<St>,
    q: EventQueue<St>,
    _conn: Connection,
}

fn open_conn(want: &Option<String>) -> Result<(Conn, (String, i32, i32, i32))> {
    let conn = Connection::connect_to_env().context("wayland connect (WAYLAND_DISPLAY)")?;
    let mut q = conn.new_event_queue::<St>();
    let qh = q.handle();
    conn.display().get_registry(&qh, ());
    let mut st = St { outputs: vec![], mgr: None, dmabuf: None, fev: HashMap::new(), created: None };
    q.roundtrip(&mut st)?;
    q.roundtrip(&mut st)?;
    let mgr = st.mgr.clone().ok_or_else(|| anyhow!("compositor has no zwlr_screencopy_manager_v1 v3"))?;
    let oi = match want {
        Some(n) => st.outputs.iter().find(|o| o.name.as_deref() == Some(n.as_str())),
        None => st.outputs.first(),
    }
    .ok_or_else(|| anyhow!("output {:?} not found ({:?})", want, st.outputs.iter().map(|o| o.name.clone()).collect::<Vec<_>>()))?;
    let output = oi.o.clone();
    let info = (oi.name.clone().unwrap_or_default(), oi.w, oi.h, oi.refresh);
    Ok((Conn { pool: HashMap::new(), st, output, mgr, qh, q, _conn: conn }, info))
}

/// Flush, poll and dispatch every connection at once (wait at most `ms`).
fn dispatch_all(conns: &mut [&mut Conn], ms: i32) -> Result<()> {
    for c in conns.iter_mut() {
        if let Err(e) = c.q.flush()
            && !would_block(&e)
        {
            return Err(e.into());
        }
        c.q.dispatch_pending(&mut c.st)?;
    }
    let mut guards = Vec::with_capacity(conns.len());
    let mut pfds = Vec::with_capacity(conns.len());
    let mut timeout = ms;
    for c in conns.iter() {
        match c.q.prepare_read() {
            Some(g) => {
                pfds.push(libc::pollfd { fd: g.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 });
                guards.push(Some(g));
            }
            None => {
                timeout = 0; // events already queued
                pfds.push(libc::pollfd { fd: -1, events: 0, revents: 0 });
                guards.push(None);
            }
        }
    }
    unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, timeout) };
    for (g, p) in guards.into_iter().zip(&pfds) {
        if let Some(g) = g
            && p.revents & libc::POLLIN != 0
            && let Err(e) = g.read()
            && !would_block(&e)
        {
            return Err(e.into());
        }
    }
    for c in conns.iter_mut() {
        c.q.dispatch_pending(&mut c.st)?;
    }
    Ok(())
}

struct Stream {
    patch: Patch,
    key: u64,
    forced: Option<u64>,
    phase: Phase,
    free: Vec<Dma>,
    pending: std::collections::VecDeque<Pending>,
    nbufs: usize,
    w: Conn,
}

const MAX_BUFS: usize = 4;

/// Return a stream's connection and buffers to the spare pool. A buffer still being copied into or waiting for its
/// fence is dropped instead (the compositor may still write it).
fn recycle(spare: &mut Vec<Conn>, mut x: Stream) {
    match std::mem::replace(&mut x.phase, Phase::Idle) {
        Phase::AwaitBuffer(f) => f.destroy(),
        Phase::Copying(f, _b) => f.destroy(),
        Phase::Idle => {}
    }
    x.pending.clear();
    let mut w = x.w;
    for b in x.free {
        let v = w.pool.entry((b.w, b.h)).or_default();
        if v.len() < MAX_BUFS {
            v.push(b);
        }
    }
    if spare.len() < 48 {
        spare.push(w);
    }
}

fn readable(fd: i32) -> bool {
    let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    unsafe { libc::poll(&mut pfd, 1, 0) > 0 && pfd.revents & libc::POLLIN != 0 }
}

fn run(sh: Arc<Mutex<Shared>>, want: Option<String>, started: &std::sync::mpsc::Sender<Result<()>>) -> Result<()> {
    let (mut main, (oname, ow, oh, orefresh)) = open_conn(&want)?;
    {
        let mut s = sh.lock().unwrap();
        s.info.output = oname;
        (s.info.width, s.info.height, s.info.refresh_mhz) = (ow, oh, orefresh);
        s.info.ticker = "screencopy copy_with_damage, 16x16, own connection".into();
        s.info.patch_buffer = "dmabuf (linear, GBM), one connection per patch, read after its fence (poll)".into();
    }
    let gbm = gbm_device()?;
    // ticker state (key 0 on the main connection)
    enum Tick {
        Idle,
        Await(frame_v1::ZwlrScreencopyFrameV1),
        Copy(frame_v1::ZwlrScreencopyFrameV1),
    }
    let mut tick = Tick::Idle;
    let mut tick_buf: Option<Dma> = None;
    let mut streams: Vec<Stream> = vec![];
    let mut spare: Vec<Conn> = vec![];
    let mut seen_gen = u64::MAX;
    let mut next_key = 1u64;
    let _ = started.send(Ok(()));
    loop {
        let busy = streams.iter().any(|x| !x.pending.is_empty());
        {
            let mut all: Vec<&mut Conn> = Vec::with_capacity(streams.len() + 1);
            all.push(&mut main);
            all.extend(streams.iter_mut().map(|x| &mut x.w));
            dispatch_all(&mut all, if busy { 1 } else { 4 })?;
        }
        // ── ticker ──
        tick = match tick {
            Tick::Idle => {
                main.st.fev.insert(0, FrameEv::default());
                Tick::Await(main.mgr.capture_output_region(0, &main.output, 0, 0, 16, 16, &main.qh, 0))
            }
            Tick::Await(f) => {
                let fe = main.st.fev.get(&0).unwrap();
                if fe.failed {
                    f.destroy();
                    sh.lock().unwrap().info.tick_cancels += 1;
                    Tick::Idle
                } else if !fe.buffer_done {
                    Tick::Await(f)
                } else {
                    let (dfmt, w, h) = fe.dma.ok_or_else(|| anyhow!("compositor offered no dmabuf screencopy"))?;
                    if tick_buf.is_none() {
                        let c = &mut main;
                        tick_buf = Some(alloc_dma(&mut c.st, &mut c.q, &c.qh, &gbm, dfmt, w, h)?);
                    }
                    f.copy_with_damage(&tick_buf.as_ref().unwrap().buffer); // never read: only its time counts
                    Tick::Copy(f)
                }
            }
            Tick::Copy(f) => {
                let fe = main.st.fev.get(&0).unwrap();
                if fe.failed {
                    f.destroy();
                    sh.lock().unwrap().info.tick_cancels += 1;
                    Tick::Idle
                } else if let Some(t) = fe.ready {
                    let now = crate::mono_ns();
                    f.destroy();
                    let mut s = sh.lock().unwrap();
                    s.ticks.push(t);
                    s.info.ticks += 1;
                    push_cap(&mut s.info.tick_delay_us, (now.saturating_sub(t) / 1000) as u32);
                    Tick::Idle
                } else {
                    Tick::Copy(f)
                }
            }
        };
        // ── configuration ──
        let (new_streams, forced_new) = {
            let mut s = sh.lock().unwrap();
            if s.stop {
                break;
            }
            let mut ns: Vec<Patch> = vec![];
            if s.generation != seen_gen {
                seen_gen = s.generation;
                let want: Vec<Patch> = s.patches.iter().map(|p| Patch { rect: p.rect.clip(ow, oh), ..p.clone() }).collect();
                let (keep, gone): (Vec<Stream>, Vec<Stream>) =
                    std::mem::take(&mut streams).into_iter().partition(|x| x.forced.is_some() || want.contains(&x.patch));
                streams = keep;
                for x in gone {
                    recycle(&mut spare, x);
                }
                ns = want.into_iter().filter(|p| !streams.iter().any(|x| x.forced.is_none() && &x.patch == p)).collect();
            }
            let mut fs: Vec<(u64, Patch)> = vec![];
            if let Some((fid, ps)) = s.force_req.take() {
                s.forced.entry(fid).or_default();
                fs = ps.into_iter().map(|p| (fid, Patch { rect: p.rect.clip(ow, oh), ..p })).collect();
            }
            (ns, fs)
        };
        for (fid, p) in new_streams.into_iter().map(|p| (None, p)).chain(forced_new.into_iter().map(|(f, p)| (Some(f), p))) {
            let w = match spare.pop() {
                Some(w) => w,
                None => open_conn(&want)?.0,
            };
            streams.push(Stream { patch: p, key: next_key, forced: fid, phase: Phase::Idle, free: vec![], pending: Default::default(), nbufs: 0, w });
            next_key += 1;
        }
        // ── patch streams ──
        let mut done: Vec<u64> = vec![];
        let fmt = sh.lock().unwrap().info.format;
        for x in streams.iter_mut() {
            // read every copy whose fence has signalled, oldest first
            while x.pending.front().is_some_and(|p| readable(p.buf.fd.as_raw_fd())) {
                let p = x.pending.pop_front().unwrap();
                let fence_us = ((crate::mono_ns().saturating_sub(p.recv)) / 1000) as u32;
                let t0 = crate::mono_ns();
                let px = Arc::new(read_patch(&p.buf, fmt, p.yinv, x.patch.step));
                let us = ((crate::mono_ns() - t0) / 1000) as u32;
                let u = Update { id: x.patch.id, t: p.t, recv: p.recv, px };
                x.free.push(p.buf);
                let mut s = sh.lock().unwrap();
                push_cap(&mut s.info.read_us, us);
                push_cap(&mut s.info.fence_us, fence_us);
                match x.forced {
                    Some(fid) => {
                        s.forced.entry(fid).or_default().push(u);
                        done.push(x.key);
                    }
                    None => {
                        s.info.patch_updates += 1;
                        s.updates.push(u);
                    }
                }
            }
            match std::mem::replace(&mut x.phase, Phase::Idle) {
                Phase::Idle => {
                    if done.contains(&x.key) || (x.forced.is_some() && !x.pending.is_empty()) {
                        continue;
                    }
                    if x.patch.rect.w <= 0 || x.patch.rect.h <= 0 {
                        if let Some(fid) = x.forced {
                            let u = Update { id: x.patch.id, t: 0, recv: 0, px: Arc::new(vec![]) };
                            sh.lock().unwrap().forced.entry(fid).or_default().push(u);
                        }
                        done.push(x.key);
                        continue;
                    }
                    if x.free.is_empty() && x.nbufs >= MAX_BUFS {
                        continue; // all buffers wait for their fences
                    }
                    x.w.st.fev.insert(x.key, FrameEv::default());
                    let r = x.patch.rect;
                    x.phase = Phase::AwaitBuffer(x.w.mgr.capture_output_region(0, &x.w.output, r.x, r.y, r.w, r.h, &x.w.qh, x.key));
                }
                Phase::AwaitBuffer(f) => {
                    let fe = x.w.st.fev.get(&x.key).unwrap();
                    if fe.failed {
                        f.destroy();
                        sh.lock().unwrap().info.patch_failed += 1;
                        continue;
                    }
                    if !fe.buffer_done {
                        x.phase = Phase::AwaitBuffer(f);
                        continue;
                    }
                    let (dfmt, w, h) = fe.dma.ok_or_else(|| anyhow!("compositor offered no dmabuf screencopy"))?;
                    x.free.retain(|b| b.w == w && b.h == h);
                    let b = match x.free.pop().or_else(|| x.w.pool.get_mut(&(w, h)).and_then(|v| v.pop())) {
                        Some(b) => b,
                        None => {
                            x.nbufs += 1;
                            sh.lock().unwrap().info.format = dfmt;
                            let c = &mut x.w;
                            alloc_dma(&mut c.st, &mut c.q, &c.qh, &gbm, dfmt, w, h)?
                        }
                    };
                    if x.forced.is_some() {
                        f.copy(&b.buffer);
                    } else {
                        f.copy_with_damage(&b.buffer);
                    }
                    x.phase = Phase::Copying(f, b);
                }
                Phase::Copying(f, b) => {
                    let fe = x.w.st.fev.get(&x.key).unwrap();
                    if fe.failed {
                        f.destroy();
                        x.free.push(b);
                        sh.lock().unwrap().info.patch_failed += 1;
                        continue;
                    }
                    let Some(t) = fe.ready else {
                        x.phase = Phase::Copying(f, b);
                        continue;
                    };
                    let yinv = fe.flags & 1 != 0;
                    f.destroy();
                    x.pending.push_back(Pending { buf: b, t, recv: crate::mono_ns(), yinv });
                }
            }
        }
        if !done.is_empty() {
            let (gone, keep): (Vec<Stream>, Vec<Stream>) = std::mem::take(&mut streams).into_iter().partition(|x| done.contains(&x.key));
            streams = keep;
            for x in gone {
                recycle(&mut spare, x);
            }
        }
    }
    for x in streams {
        recycle(&mut spare, x);
    }
    if let Tick::Await(f) | Tick::Copy(f) = tick {
        f.destroy();
    }
    drop(tick_buf);
    let _ = main.q.flush();
    Ok(())
}

struct G(Vec<(String, u32)>);
impl Dispatch<wl_registry::WlRegistry, ()> for G {
    fn event(st: &mut Self, _: &wl_registry::WlRegistry, ev: wl_registry::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { interface, version, .. } = ev {
            st.0.push((interface, version));
        }
    }
}
/// Print the compositor's globals (which capture protocols it offers).
pub fn globals() -> Result<()> {
    let conn = Connection::connect_to_env()?;
    let mut q = conn.new_event_queue::<G>();
    conn.display().get_registry(&q.handle(), ());
    let mut g = G(vec![]);
    q.roundtrip(&mut g)?;
    for (i, v) in g.0 {
        println!("{i} v{v}");
    }
    Ok(())
}
