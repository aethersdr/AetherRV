//! A simulated CLMD16 load controller (and its CKM12 keypads) on a virtual CAN interface.
//!
//! What the van's controller does, as far as AetherRV can see it on NMEA 2000:
//! * every 1.5 s (and right after a change) a 127501 status for the breaker bank (outputs 1..16 at indicators
//!   1..16, wired inputs 1..12 at indicators 17..28) and for the virtual-switch bank (28 indicators);
//! * a 126208 Command for 127501 on the virtual-switch bank is a keypad press: the rising edge runs that button's
//!   logic (toggle, toggle-if-input, timed pulse), the falling edge releases it;
//! * a 126208 Command for 127501 on the breaker bank sets that channel directly (AetherRV sends only OFFs there);
//! * an output held by a wired input (the stereo on ignition) stays on while the input is on.
//!
//! Every command received is appended to the log (JSON lines) so a test can assert exactly what was sent.
//! Faults: a tripped channel (reports error), a controller that ignores every command, silence.

use aetherrv_can::CanFrame;
use aetherrv_can::raw::RawSocket;
use proto_n2k::encode::{PGN_ADDRESS_CLAIM, PGN_GROUP_FUNCTION, PGN_ISO_REQUEST, PGN_SWITCH_STATUS, can_id, make_name};
use proto_n2k::fast_packet::FastPacketAssembler;
use proto_n2k::id::parse_id;
use serde::Serialize;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What a virtual-switch press does.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Toggle the output on the press.
    Toggle(u8),
    /// Toggle, but only turn it ON while this wired input is on (the controller's own interlock).
    ToggleIf(u8, u8),
    /// Output on for this long, then off (the drawer locks).
    Pulse(u8, Duration),
}

#[derive(Debug, Clone)]
pub struct Clmd16Config {
    pub iface: String,
    pub source: u8,
    pub bank: u8,
    pub vsw_bank: u8,
    pub period: Duration,
    /// Virtual switch N → action.
    pub buttons: Vec<(u8, Action)>,
    /// Output held on while a wired input is on: (channel, input).
    pub held_by: Vec<(u8, u8)>,
    /// Outputs that exist (others report "not available").
    pub outputs: u8,
    /// Wired inputs on at start (1..12).
    pub inputs_on: Vec<u8>,
    pub log: Option<PathBuf>,
}

impl Clmd16Config {
    /// The van's layout (config/aetherrv.example.toml `[device.n2k]`): outputs 1..16 on bank 32, presses on bank 33.
    pub fn van(iface: &str) -> Self {
        let mut buttons: Vec<(u8, Action)> = (1..=16u8).map(|n| (n, Action::Toggle(n))).collect();
        let mut set = |n: u8, a: Action| {
            if let Some(b) = buttons.iter_mut().find(|(k, _)| *k == n) {
                b.1 = a;
            }
        };
        set(1, Action::ToggleIf(1, 2)); // spot lights need ignition
        set(3, Action::ToggleIf(3, 2)); // flood lights need ignition
        set(8, Action::Toggle(15)); // the Starlink output is ch15, its button is 8
        set(11, Action::Pulse(11, Duration::from_secs(10))); // drawer locks: timed pulse
        set(15, Action::Toggle(8));
        Clmd16Config {
            iface: iface.into(),
            source: 0,
            bank: 32,
            vsw_bank: 33,
            period: Duration::from_millis(1500),
            buttons,
            held_by: vec![(10, 2)], // the stereo stays on with ignition
            outputs: 16,
            inputs_on: vec![],
            log: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Clmd16Report {
    pub commands: u64,
    pub presses: u64,
    pub releases: u64,
    pub direct: u64,
    pub ignored: u64,
    pub refused_interlock: u64,
    /// Every channel currently on (1..16).
    pub on: Vec<u8>,
    pub inputs_on: Vec<u8>,
    pub vsw_on: Vec<u8>,
    pub status_frames: u64,
}

struct Model {
    cfg: Clmd16Config,
    out: [u8; 16], // 0 off, 1 on, 2 error
    inputs: [bool; 12],
    vsw: [bool; 28],
    pulses: Vec<(u8, Instant)>,
    fp: FastPacketAssembler,
    ignore: bool,
    silent: bool,
    report: Clmd16Report,
    log: Option<std::fs::File>,
    t0: Instant,
    changed: bool,
}

impl Model {
    fn status(&self, bank: u8) -> CanFrame {
        let mut bits: u64 = 0;
        for i in 0..28usize {
            let v: u64 = if bank == self.cfg.bank {
                if i < 16 {
                    if i < usize::from(self.cfg.outputs) { u64::from(self.out[i]) } else { 3 }
                } else {
                    u64::from(self.inputs[i - 16])
                }
            } else {
                u64::from(self.vsw[i])
            };
            bits |= v << (2 * i);
        }
        let mut d = vec![bank];
        d.extend_from_slice(&bits.to_le_bytes()[..7]);
        CanFrame::ext(can_id(3, PGN_SWITCH_STATUS, self.cfg.source, 255), &d)
    }

    fn statuses(&mut self) -> Vec<CanFrame> {
        if self.silent {
            return vec![];
        }
        self.report.status_frames += 2;
        vec![self.status(self.cfg.bank), self.status(self.cfg.vsw_bank)]
    }

    fn address_claim(&self) -> CanFrame {
        let name = make_name(0x1C16, 2046, 140, 30, 4, 0);
        CanFrame::ext(can_id(6, PGN_ADDRESS_CLAIM, self.cfg.source, 255), &name.to_le_bytes())
    }

    fn set_out(&mut self, ch: u8, on: bool) {
        let i = usize::from(ch.saturating_sub(1));
        if i >= 16 || self.out[i] == 2 {
            return;
        }
        let held = self.cfg.held_by.iter().any(|(c, inp)| *c == ch && self.inputs[usize::from(*inp - 1)]);
        let v = u8::from(on || held);
        if self.out[i] != v {
            self.out[i] = v;
            self.changed = true;
        }
    }

    fn press(&mut self, n: u8, now: Instant) {
        let Some(a) = self.cfg.buttons.iter().find(|(k, _)| *k == n).map(|(_, a)| a.clone()) else { return };
        let is_on = |m: &Model, ch: u8| m.out.get(usize::from(ch.saturating_sub(1))) == Some(&1);
        match a {
            Action::Toggle(ch) => {
                let v = !is_on(self, ch);
                self.set_out(ch, v);
            }
            Action::ToggleIf(ch, input) => {
                if is_on(self, ch) {
                    self.set_out(ch, false);
                } else if self.inputs[usize::from(input - 1)] {
                    self.set_out(ch, true);
                } else {
                    self.report.refused_interlock += 1;
                }
            }
            Action::Pulse(ch, d) => {
                self.set_out(ch, true);
                self.pulses.retain(|(c, _)| *c != ch);
                self.pulses.push((ch, now + d));
            }
        }
    }

    fn on_frame(&mut self, f: &CanFrame, now: Instant) -> Vec<CanFrame> {
        if !f.extended || f.error {
            return vec![];
        }
        let id = parse_id(f.id);
        if id.pgn == PGN_ISO_REQUEST && (id.destination == self.cfg.source || id.destination == 255) {
            let req = f.data.get(..3).map(|b| u32::from(b[0]) | (u32::from(b[1]) << 8) | (u32::from(b[2]) << 16));
            return match req {
                Some(PGN_ADDRESS_CLAIM) => vec![self.address_claim()],
                Some(PGN_SWITCH_STATUS) => self.statuses(),
                _ => vec![],
            };
        }
        if id.pgn != PGN_GROUP_FUNCTION || (id.destination != self.cfg.source && id.destination != 255) {
            return vec![];
        }
        let ts = now.duration_since(self.t0).as_secs_f64();
        let Some(p) = self.fp.feed(id.source, id.pgn, &f.data, ts) else { return vec![] };
        // function 1 = Command; commanded PGN 127501; priority byte; n pairs of (field, 1-byte value)
        if p.len() < 6 || p[0] != 1 {
            return vec![];
        }
        let pgn = u32::from(p[1]) | (u32::from(p[2]) << 8) | (u32::from(p[3]) << 16);
        let n = usize::from(p[5]);
        let pairs: Vec<(u8, u8)> = p[6..].chunks(2).take(n).filter(|c| c.len() == 2).map(|c| (c[0], c[1])).collect();
        self.report.commands += 1;
        if let Some(l) = self.log.as_mut() {
            let line = serde_json::json!({"t": aetherrv_core::clock::now_s(), "src": id.source, "pgn": pgn, "pairs": pairs, "ignored": self.ignore});
            let _ = writeln!(l, "{line}");
            let _ = l.flush();
        }
        if self.ignore || pgn != PGN_SWITCH_STATUS {
            self.report.ignored += 1;
            return vec![];
        }
        let Some(bank) = pairs.iter().find(|(k, _)| *k == 1).map(|(_, v)| *v) else { return vec![] };
        for (field, v) in pairs.iter().filter(|(k, _)| *k >= 2) {
            let ch = field - 1;
            if bank == self.cfg.vsw_bank && (1..=28).contains(&ch) {
                let i = usize::from(ch - 1);
                let on = *v == 1;
                if on && !self.vsw[i] {
                    self.report.presses += 1;
                    self.vsw[i] = true;
                    self.changed = true;
                    self.press(ch, now);
                } else if !on && self.vsw[i] {
                    self.report.releases += 1;
                    self.vsw[i] = false;
                    self.changed = true;
                }
            } else if bank == self.cfg.bank && (1..=16).contains(&ch) {
                self.report.direct += 1;
                self.set_out(ch, *v == 1);
            }
        }
        if std::mem::take(&mut self.changed) { self.statuses() } else { vec![] }
    }

    fn tick(&mut self, now: Instant) -> Vec<CanFrame> {
        let due: Vec<u8> = self.pulses.iter().filter(|(_, t)| *t <= now).map(|(c, _)| *c).collect();
        self.pulses.retain(|(_, t)| *t > now);
        for ch in due {
            self.set_out(ch, false);
        }
        let held: Vec<u8> = self.cfg.held_by.iter().filter(|(_, i)| self.inputs[usize::from(*i - 1)]).map(|(c, _)| *c).collect();
        for ch in held {
            self.set_out(ch, true);
        }
        if std::mem::take(&mut self.changed) { self.statuses() } else { vec![] }
    }
}

/// A running controller. Cheap to clone.
#[derive(Clone)]
pub struct Clmd16 {
    m: Arc<Mutex<Model>>,
    sock: Arc<RawSocket>,
}

impl Clmd16 {
    pub fn start(cfg: Clmd16Config) -> anyhow::Result<Clmd16> {
        if !cfg.iface.starts_with("vcan") {
            anyhow::bail!("refusing {:?}: the CLMD16 simulator runs on vcan only", cfg.iface);
        }
        let sock = Arc::new(RawSocket::open(&cfg.iface, false)?);
        let log = cfg.log.as_ref().map(std::fs::File::create).transpose()?;
        let mut inputs = [false; 12];
        for i in &cfg.inputs_on {
            if (1..=12).contains(i) {
                inputs[usize::from(i - 1)] = true;
            }
        }
        let period = cfg.period;
        let m = Arc::new(Mutex::new(Model {
            cfg,
            out: [0; 16],
            inputs,
            vsw: [false; 28],
            pulses: vec![],
            fp: FastPacketAssembler::new(),
            ignore: false,
            silent: false,
            report: Clmd16Report::default(),
            log,
            t0: Instant::now(),
            changed: false,
        }));
        let me = Clmd16 { m: m.clone(), sock: sock.clone() };
        let _ = sock.send(&m.lock().unwrap().address_claim());
        let (rx, mr) = (sock.clone(), m.clone());
        tokio::spawn(async move {
            while let Ok(f) = rx.recv().await {
                let out = mr.lock().unwrap().on_frame(&f, Instant::now());
                for o in out {
                    let _ = rx.send(&o);
                }
            }
        });
        let (tx, mt) = (sock, m);
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(100));
            let mut last = Instant::now() - period;
            loop {
                iv.tick().await;
                let now = Instant::now();
                let mut out = mt.lock().unwrap().tick(now);
                if now.duration_since(last) >= period {
                    last = now;
                    out = mt.lock().unwrap().statuses();
                }
                for o in out {
                    let _ = tx.send(&o);
                }
            }
        });
        Ok(me)
    }

    fn push(&self) {
        let out = {
            let mut g = self.m.lock().unwrap();
            g.changed = false;
            g.statuses()
        };
        for o in out {
            let _ = self.sock.send(&o);
        }
    }

    /// A wired input changes (1..12); status goes out at once.
    pub fn set_input(&self, n: u8, on: bool) {
        {
            let mut g = self.m.lock().unwrap();
            if !(1..=12).contains(&n) {
                return;
            }
            g.inputs[usize::from(n - 1)] = on;
            let held: Vec<u8> = g.cfg.held_by.iter().filter(|(_, i)| *i == n).map(|(c, _)| *c).collect();
            for ch in held {
                g.set_out(ch, on);
            }
        }
        self.push();
    }

    pub fn input(&self, n: u8) -> bool {
        self.m.lock().unwrap().inputs.get(usize::from(n.saturating_sub(1))).copied().unwrap_or(false)
    }

    /// Trip (`true`) or reset a channel (a tripped channel reports "error" until reset).
    pub fn trip(&self, ch: u8, tripped: bool) {
        {
            let mut g = self.m.lock().unwrap();
            if let Some(o) = g.out.get_mut(usize::from(ch.saturating_sub(1))) {
                *o = if tripped { 2 } else { 0 };
            }
        }
        self.push();
    }

    /// Set an output as if a keypad (not AetherRV) pressed it.
    pub fn keypad(&self, ch: u8, on: bool) {
        self.m.lock().unwrap().set_out(ch, on);
        self.push();
    }

    pub fn set_ignore(&self, v: bool) {
        self.m.lock().unwrap().ignore = v;
    }

    pub fn set_silent(&self, v: bool) {
        self.m.lock().unwrap().silent = v;
    }

    pub fn is_on(&self, ch: u8) -> bool {
        self.m.lock().unwrap().out.get(usize::from(ch.saturating_sub(1))) == Some(&1)
    }

    pub fn report(&self) -> Clmd16Report {
        let g = self.m.lock().unwrap();
        let mut r = g.report.clone();
        r.on = (1..=16u8).filter(|c| g.out[usize::from(c - 1)] == 1).collect();
        r.inputs_on = (1..=12u8).filter(|i| g.inputs[usize::from(i - 1)]).collect();
        r.vsw_on = (1..=28u8).filter(|i| g.vsw[usize::from(i - 1)]).collect();
        r
    }
}
