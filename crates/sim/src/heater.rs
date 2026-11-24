//! A simulated Espar Airtronic S2 D2L on a CAN interface (vcan), from this van's captures and the S3 project:
//!
//! * asleep until a controller frame arrives; awake it sends 2C4 every 200 ms and 625 every 100 ms;
//! * HEAT (054 mode 05) → 09 heating; FAN (02) → 21 fan; OFF (00) from heating → idle with the after-run flag (08)
//!   for `after_run_s`, from fan → idle at once;
//! * **controller loss**: no 054/60D for `loss_timeout_s` while running → it stops by itself, stores P000307 and
//!   P000440 in its fault memory and runs its after-run (the van's heater: ~15 s; the real figure is to be measured
//!   in the van, so it's a parameter);
//! * idle and no controller for `sleep_s` → the bus sleeps (it stops sending);
//! * read-only UDS on 7A0/73C (0x22 for a few DIDs, 0x19 fault memory, with ISO-TP flow control).
//!
//! Every controller frame and event goes to a JSON-lines log, so a test can assert on what the heater saw: the
//! largest gap between 054 frames while running, any controller-loss fault, an FE FF room word with mode 05 ("0 °C",
//! full power), any burst frame after a given time.

use aetherrv_can::raw::RawSocket;
use aetherrv_can::CanFrame;
use serde_json::{Value, json};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct HeaterConfig {
    pub iface: String,
    pub loss_timeout_s: f64,
    pub after_run_s: f64,
    pub sleep_s: f64,
    pub log: Option<PathBuf>,
}

impl Default for HeaterConfig {
    fn default() -> Self {
        HeaterConfig { iface: "vcan0".into(), loss_timeout_s: 15.0, after_run_s: 240.0, sleep_s: 10.0, log: None }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    /// Largest gap between consecutive 054 frames while the heater was heating or in fan (s).
    pub max_054_gap_running: f64,
    pub controller_loss_faults: u32,
    /// 054 frames with mode 05 and room word FE FF.
    pub full_power_frames: u32,
    pub frames_054: u64,
    pub state: String,
    pub dtcs: Vec<String>,
    /// (time since start, id) of every burst frame (05C-06D, 10A).
    pub burst_frames: Vec<(f64, u32)>,
    /// (time since start, mode byte) of every 054.
    pub modes: Vec<(f64, u8)>,
    pub heat_runs: u32,
}

struct Model {
    cfg: HeaterConfig,
    awake: bool,
    state: u8, // 03 idle, 09 heating, 21 fan
    flags: u8,
    after_until: Option<Instant>,
    last_ctrl: Option<Instant>,
    last_054: Option<Instant>,
    idle_since: Instant,
    temp: f64,
    room: Option<f64>,
    dtcs: Vec<(String, bool)>, // (code, active)
    paused: bool,
    report: Report,
    t0: Instant,
    log: Option<std::fs::File>,
    // ISO-TP pending multi-frame reply
    pending_tx: Vec<u8>,
}

impl Model {
    fn ev(&mut self, kind: &str, extra: Value) {
        if let Some(f) = self.log.as_mut() {
            let mut j = json!({"t": self.t0.elapsed().as_secs_f64(), "ev": kind});
            if let (Value::Object(m), Value::Object(e)) = (&mut j, extra) {
                m.extend(e);
            }
            let _ = writeln!(f, "{j}");
        }
    }

    fn running(&self) -> bool {
        self.state == 0x09 || self.state == 0x21
    }

    fn on_frame(&mut self, f: &CanFrame, now: Instant) -> Vec<CanFrame> {
        let mut out = vec![];
        if f.extended || f.error {
            return out;
        }
        let ctrl = matches!(f.id, 0x054..=0x06D | 0x10A | 0x60D);
        if ctrl {
            if !self.awake {
                self.awake = true;
                self.idle_since = now;
                self.ev("wake", json!({}));
            }
            self.last_ctrl = Some(now);
        }
        let t = self.t0.elapsed().as_secs_f64();
        // the settings burst: 05C-06D and 10A, except 065 (the cyclic room temperature)
        if ((0x05C..=0x06D).contains(&f.id) && f.id != 0x065) || f.id == 0x10A {
            self.report.burst_frames.push((t, f.id));
        }
        if f.id == 0x054 && f.data.len() >= 6 {
            let d = &f.data;
            if self.running()
                && let Some(prev) = self.last_054
            {
                let gap = now.duration_since(prev).as_secs_f64();
                if gap > self.report.max_054_gap_running {
                    self.report.max_054_gap_running = gap;
                }
            }
            self.last_054 = Some(now);
            self.report.frames_054 += 1;
            self.report.modes.push((t, d[1]));
            let room = proto_espar::temp_word(d[4], d[5]);
            if d[1] == 0x05 && room.is_none() {
                self.report.full_power_frames += 1;
                self.ev("full_power_room_word", json!({"data": proto_espar::hex_spaced(d)}));
            }
            self.room = room;
            let want = d[1];
            match (want, self.state) {
                // HEAT from idle, or straight from FAN (the controller switches modes without an OFF between)
                (0x05, 0x03) | (0x05, 0x21) if self.after_until.is_none() => {
                    self.state = 0x09;
                    self.flags = 0x00;
                    self.report.heat_runs += 1;
                    self.ev("heat", json!({}));
                }
                (0x02, 0x03) if self.after_until.is_none() => {
                    self.state = 0x21;
                    self.ev("fan", json!({}));
                }
                (0x00, 0x09) => {
                    self.state = 0x03;
                    self.flags = 0x08;
                    self.after_until = Some(now + Duration::from_secs_f64(self.cfg.after_run_s));
                    self.ev("off_after_run", json!({}));
                }
                (0x00, 0x21) => {
                    self.state = 0x03;
                    self.ev("off", json!({}));
                }
                _ => {}
            }
        }
        if f.id == proto_espar::uds::REQ_ID && !f.data.is_empty() {
            out.extend(self.uds(&f.data));
        }
        out
    }

    /// One reply: a single frame, or a first frame that waits for the tester's flow control (ISO-TP).
    fn reply(&mut self, p: &[u8]) -> Vec<CanFrame> {
        if p.len() <= 7 {
            let mut b = vec![p.len() as u8];
            b.extend_from_slice(p);
            b.resize(8, 0xAA);
            return vec![CanFrame::std(proto_espar::uds::RESP_ID, &b)];
        }
        let mut b = vec![0x10 | ((p.len() >> 8) as u8 & 0x0F), p.len() as u8];
        b.extend_from_slice(&p[..6]);
        self.pending_tx = p[6..].to_vec();
        vec![CanFrame::std(proto_espar::uds::RESP_ID, &b)]
    }

    fn uds(&mut self, d: &[u8]) -> Vec<CanFrame> {
        if d[0] == 0x30 {
            // flow control: send the rest as consecutive frames
            let rest = std::mem::take(&mut self.pending_tx);
            let mut v = vec![];
            for (i, ch) in rest.chunks(7).enumerate() {
                let mut b = vec![0x20 | ((i as u8 + 1) & 0x0F)];
                b.extend_from_slice(ch);
                b.resize(8, 0xAA);
                v.push(CanFrame::std(proto_espar::uds::RESP_ID, &b));
            }
            return v;
        }
        let n = (d[0] & 0x0F) as usize;
        if d[0] >> 4 != 0 || n < 1 || d.len() < 1 + n {
            return vec![];
        }
        let svc = d[1];
        match svc {
            0x22 if n >= 3 => {
                let did = u16::from(d[2]) << 8 | u16::from(d[3]);
                let mut p = vec![0x62, d[2], d[3]];
                let w = |x: f64| ((x + 50.0) * 100.0).round() as u16;
                match did {
                    0xFD04 => p.extend_from_slice(&132u16.to_be_bytes()),
                    0xFD01 => {
                        let flame = if self.state == 0x09 { 300.0 } else { self.temp };
                        for x in [flame, self.temp, self.temp + if self.state == 0x09 { 30.0 } else { 0.0 }] {
                            p.extend_from_slice(&w(x).to_be_bytes());
                        }
                    }
                    0xFD10 => p.extend_from_slice(&[if self.state == 0x09 { 1 } else { 0 }, if self.state == 0x09 { 0x14 } else { 0 }]),
                    _ => return self.reply(&[0x7F, 0x22, 0x31]),
                }
                self.reply(&p)
            }
            0x19 => {
                let mut p = vec![0x59, 0x02, 0xFF];
                for (code, active) in &self.dtcs {
                    let hex = &code[1..];
                    for i in 0..3 {
                        p.push(u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap_or(0));
                    }
                    p.push(if *active { 0x09 } else { 0x08 });
                }
                self.reply(&p)
            }
            _ => self.reply(&[0x7F, svc, 0x11]), // service not supported (the heater refuses anything else too)
        }
    }

    /// 100 ms tick: the heater's own timers and its broadcasts.
    fn tick(&mut self, now: Instant, n: u64) -> Vec<CanFrame> {
        let mut out = vec![];
        if !self.awake || self.paused {
            return out;
        }
        let lost = self.last_ctrl.is_none_or(|t| now.duration_since(t).as_secs_f64() > self.cfg.loss_timeout_s);
        if self.running() && lost {
            let was = self.state;
            self.state = 0x03;
            for c in ["P000307", "P000440"] {
                if !self.dtcs.iter().any(|(x, _)| x == c) {
                    self.dtcs.push((c.into(), false));
                }
            }
            self.report.controller_loss_faults += 1;
            if was == 0x09 {
                self.flags = 0x08;
                self.after_until = Some(now + Duration::from_secs_f64(self.cfg.after_run_s));
            }
            self.ev("controller_loss", json!({"was": format!("{was:02x}")}));
        }
        if let Some(u) = self.after_until
            && now >= u
        {
            self.after_until = None;
            self.flags = 0x00;
            self.idle_since = now;
            self.ev("after_run_done", json!({}));
        }
        if self.state == 0x09 {
            self.temp = (self.temp + 0.05).min(60.0);
        } else {
            self.temp = (self.temp - 0.02).max(10.0);
        }
        let idle = self.state == 0x03 && self.after_until.is_none();
        if idle && lost && now.duration_since(self.idle_since).as_secs_f64() > self.cfg.sleep_s {
            self.awake = false;
            self.ev("sleep", json!({}));
            return out;
        }
        if !idle {
            self.idle_since = now;
        }
        let tw = ((self.room.filter(|_| self.running()).unwrap_or(self.temp) + 50.0) * 10.0).round() as u16;
        let t = tw.to_le_bytes();
        out.push(CanFrame::std(0x625, &[0x25, (n % 2) as u8, (n % 256) as u8, 0xE5, 0xFC, 0x3B, 0x58, 0x06]));
        if n % 2 == 0 {
            out.push(CanFrame::std(0x2C4, &[self.state, self.flags, 0, 0, t[0], t[1], 0, 0]));
        }
        out
    }
}

/// A running heater. Cheap to clone.
#[derive(Clone)]
pub struct Heater {
    m: Arc<Mutex<Model>>,
}

impl Heater {
    /// Start on `cfg.iface` (vcan). Runs until the process ends.
    pub fn start(cfg: HeaterConfig) -> anyhow::Result<Heater> {
        let sock = Arc::new(RawSocket::open(&cfg.iface, false)?);
        let log = cfg.log.as_ref().map(|p| std::fs::File::create(p)).transpose()?;
        let now = Instant::now();
        let m = Arc::new(Mutex::new(Model {
            cfg,
            awake: false,
            state: 0x03,
            flags: 0x00,
            after_until: None,
            last_ctrl: None,
            last_054: None,
            idle_since: now,
            temp: 16.0,
            room: None,
            dtcs: vec![],
            paused: false,
            report: Report::default(),
            t0: now,
            log,
            pending_tx: vec![],
        }));
        let h = Heater { m: m.clone() };
        let rx = sock.clone();
        let mr = m.clone();
        tokio::spawn(async move {
            while let Ok(f) = rx.recv().await {
                let out = mr.lock().unwrap().on_frame(&f, Instant::now());
                for o in out {
                    let _ = rx.send(&o);
                }
            }
        });
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(100));
            let mut n = 0u64;
            loop {
                iv.tick().await;
                n += 1;
                let out = m.lock().unwrap().tick(Instant::now(), n);
                for o in out {
                    let _ = sock.send(&o);
                }
            }
        });
        Ok(h)
    }

    pub fn report(&self) -> Report {
        let g = self.m.lock().unwrap();
        let mut r = g.report.clone();
        r.state = match g.state {
            0x09 => "heating".into(),
            0x21 => "fan".into(),
            _ if g.after_until.is_some() => "after-run".into(),
            _ if !g.awake => "asleep".into(),
            _ => "idle".into(),
        };
        r.dtcs = g.dtcs.iter().map(|(c, _)| c.clone()).collect();
        r
    }

    /// Stop answering (a heater that lost power or a cut wire).
    pub fn pause(&self, p: bool) {
        self.m.lock().unwrap().paused = p;
    }

    /// Forget the gap statistics (e.g. after setting up a run).
    pub fn reset_stats(&self) {
        let mut g = self.m.lock().unwrap();
        g.report.max_054_gap_running = 0.0;
        g.report.burst_frames.clear();
        g.report.modes.clear();
    }

    pub fn elapsed(&self) -> f64 {
        self.m.lock().unwrap().t0.elapsed().as_secs_f64()
    }
}
