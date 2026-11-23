//! AetherRV as the Espar heater's controller (the EasyStart Pro is unplugged: owner decision 2026-11-22).
//! A line-by-line port of `adapters/espar_control.py`, plus the owner's 2026-11-23 additions: seamless resume after
//! a restart, running blind, and the heat lockout inside this service (ARCHITECTURE.md §7, §8).
//!
//! Everything goes through TxGate device "espar" (tx_enabled in the Pi config); with it off this sends nothing and
//! every command is refused.
//!
//! Rules
//!   - Never transmit while another controller is on the bus (an EasyStart Pro plugged back in): stop at once.
//!   - The room temperature in 054 is always real while heating (FE FF there means 0 °C → full power): the cabin
//!     Ruuvi if fresh, else the heater's own intake reading; neither fresh → HEAT refused / OFF sent.
//!   - A command change is logged and verified against the heater's status (2C4) within VERIFY_S. The cyclic
//!     repetition is the protocol itself, not a retry.
//!   - Run time is ours (like the EasyStart Pro): 1-240 min by the minute, 270-720 in 30-min steps, or none (∞).
//!   - The heater must answer: no heater frames within WAKE_S of starting, or silent for LOST_S while we drive it →
//!     stop transmitting, reset hat1 (a frame nobody ACKs is retried forever by the MCP2515) and alert.
//!   - Idle (OFF, after-run done) for IDLE_STOP_S → stop transmitting so the bus and heater sleep.
//!
//! All times are seconds on `CLOCK_BOOTTIME` (the decoder's clock too), so a wall-clock jump never looks like a
//! silent heater and a run's deadline survives a restart.

use aetherrv_core::cmdlog::CommandLog;
use aetherrv_core::command::Reply;
use aetherrv_core::pyfmt::{py_float, py_str};
use proto_espar::decoder::Decoder;
use proto_espar::{HEARTBEAT, STATIC, cmd_frame, room_frame, run_minutes_ok, startup_burst};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const DEVICE: &str = "espar";
/// The cabin Ruuvi is stale after 15 min of silence (the Cerbo republishes it only on change; climate.py).
pub const FRONT_STALE: f64 = 15.0 * 60.0;
pub const SETPOINT_RANGE: (f64, f64) = (10.0, 30.0);
pub const RUN_STATE_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct Timing {
    pub verify_s: f64,
    pub wake_s: f64,
    pub lost_s: f64,
    pub idle_stop_s: f64,
    pub other_s: f64,
    /// No contact with AetherRV's bus for this long while HEAT/FAN runs → OFF (owner, §3; default 10 min).
    pub blind_off_s: f64,
    /// Resume: at most this many in `resume_window_s` (a crash loop must not keep a heater running).
    pub max_resumes: usize,
    pub resume_window_s: f64,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            verify_s: 3.0,
            wake_s: 3.0,
            lost_s: 3.0,
            idle_stop_s: 10.0,
            other_s: 3.0,
            blind_off_s: 600.0,
            max_resumes: 3,
            resume_window_s: 60.0,
        }
    }
}

/// What the loop knows from outside the heater bus.
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    /// `climate.cabin_front.temp` and its age (s), from the Victron service.
    pub cabin: Option<(f64, f64)>,
    /// `victron.connected` (`None` = unknown).
    pub victron_connected: Option<bool>,
    /// Locked out by our own tracker or a fresh guard (§8).
    pub heat_lockout: bool,
    /// Seconds since the last packet from the broker (`None` = no contact recorded yet).
    pub bus_contact_age: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendErr {
    /// TxGate refused (a programming or config change: fatal).
    Blocked(String),
    /// The socket refused (full queue while the heater wakes, link down).
    Io(String),
}

/// Where frames go (the service: CanBus + TxGate; tests: a recorder).
pub trait Sink {
    fn send(&mut self, id: u32, data: &[u8]) -> Result<(), SendErr>;
}

/// The persisted run (`/run/aetherrv/espar/run.json`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunState {
    pub version: u32,
    pub boot_id: String,
    pub active: bool,
    pub mode: String,
    pub target: Option<String>,
    pub setpoint_c: f64,
    pub flags: u8,
    pub run_min: Option<i64>,
    /// Deadline, `CLOCK_BOOTTIME` seconds.
    pub until: Option<f64>,
    pub burst_done: bool,
    pub last_room: Option<f64>,
    pub room_src: Option<String>,
    pub room_t: Option<f64>,
    pub last_contact: Option<f64>,
    pub resumes: Vec<f64>,
    /// The writer was alive at this time (`CLOCK_BOOTTIME`).
    pub alive: f64,
}

impl RunState {
    /// Everything but the liveness stamp (to tell a change from a heartbeat write).
    pub fn same_run(&self, o: &RunState) -> bool {
        let mut a = self.clone();
        a.alive = o.alive;
        a == *o
    }
}

/// A command waiting for the heater's confirmation.
#[derive(Debug, Clone)]
pub struct Pending {
    pub value: Value,
    pub client: String,
    pub mode: String,
    pub setpoint_c: f64,
    pub started: f64,
    pub end: f64,
}

pub enum Begin {
    Done(Reply),
    Wait(Pending),
}

pub struct Control {
    pub t: Timing,
    pub enabled: bool,
    pub mode: String,
    pub setpoint_c: f64,
    pub until: Option<f64>,
    /// Mode to apply once the woken heater reports idle.
    pub target: Option<String>,
    pub run_min: Option<i64>,
    burst: Vec<(u32, Vec<u8>)>,
    pub burst_i: usize,
    send_fail: u32,
    /// 00 until the first run since start-up, then 09 on / 08 off.
    pub flags: u8,
    /// Transmitting.
    pub active: bool,
    pub started_at: f64,
    pub idle_since: Option<f64>,
    pub last_room: Option<f64>,
    pub room_src: Option<String>,
    room_t: Option<f64>,
    pub problem: Option<String>,
    pub tick: u64,
    /// Set by a stop with a reason: the service resets hat1 (root helper `can_reset`) and clears it.
    pub reset_requested: bool,
    /// The room value saved before a restart: the last fallback after a resume, until live sources are back.
    saved_room: Option<(f64, f64)>,
    pub resumes: Vec<f64>,
    /// Last broker contact (`CLOCK_BOOTTIME`), carried across restarts.
    pub last_contact: Option<f64>,
    /// SIGTERM: stop transmitting, no OFF, keep the run file.
    pub suspended: bool,
    pub log: CommandLog,
}

fn round1(x: f64) -> f64 {
    aetherrv_core::num::round1(x)
}

impl Control {
    pub fn new(enabled: bool, log: CommandLog) -> Self {
        Control {
            t: Timing::default(),
            enabled,
            mode: "off".into(),
            setpoint_c: 21.0,
            until: None,
            target: None,
            run_min: None,
            burst: startup_burst(),
            burst_i: 0,
            send_fail: 0,
            flags: 0x00,
            active: false,
            started_at: 0.0,
            idle_since: None,
            last_room: None,
            room_src: None,
            room_t: None,
            problem: None,
            tick: 0,
            reset_requested: false,
            saved_room: None,
            resumes: vec![],
            last_contact: None,
            suspended: false,
            log,
        }
    }

    fn logc(&self, cmd: &str, value: &Value, outcome: &str, detail: &str, client: &str) {
        self.log.log(cmd, value, outcome, detail, client);
    }

    /// The cabin Ruuvi under climate.py's rule, else the heater's own intake reading (the decoder blanks it when
    /// old), else (only after a resume) the value saved before the restart while it is younger than 15 min.
    pub fn room(&self, dec: &Decoder, inp: &Inputs, now: f64) -> (Option<f64>, Option<String>) {
        if let Some((t, age)) = inp.cabin
            && age < FRONT_STALE
            && inp.victron_connected != Some(false)
        {
            return (Some(t), Some("cabin Ruuvi".into()));
        }
        if let Some(t) = dec.intake_c(now) {
            return (Some(t), Some("heater intake sensor".into()));
        }
        if let Some((t, at)) = self.saved_room
            && now - at < FRONT_STALE
        {
            return (Some(t), Some("saved before the restart".into()));
        }
        (None, None)
    }

    pub fn other_controller_recent(&self, dec: &Decoder, now: f64) -> bool {
        dec.last_other_ctrl.is_some_and(|t| now - t < self.t.other_s)
    }

    /// The decoder heard 054/60D that aren't ours (we don't receive our own frames).
    pub fn on_other_controller(&mut self) {
        if self.active {
            self.stop(Some("another controller appeared on the heater's bus (EasyStart Pro plugged in?)"));
        }
    }

    fn cmd_bytes(&self, room: Option<f64>) -> [u8; 8] {
        cmd_frame(self.flags, &self.mode, self.setpoint_c, room)
    }

    pub fn stop(&mut self, why: Option<&str>) {
        if self.active {
            tracing::warn!("espar control: stopped transmitting ({})", why.unwrap_or("idle"));
        }
        self.active = false;
        if let Some(why) = why {
            self.problem = Some(why.to_string());
            if self.mode != "off" {
                self.logc("auto_off", &json!({"mode": self.mode}), "stopped", why, "aetherrv");
            }
            self.mode = "off".into();
            self.until = None;
            self.reset_requested = true;
        }
    }

    /// The 100 ms loop (Python `step()`).
    pub fn step(&mut self, now: f64, dec: &Decoder, inp: &Inputs, sink: &mut dyn Sink) {
        if !self.active || self.suspended {
            return;
        }
        if !self.enabled {
            self.stop(Some("transmit permission turned off"));
            return;
        }
        let heard = dec.last_heater.is_some_and(|t| t >= self.started_at);
        if !heard && now - self.started_at > self.t.wake_s {
            self.stop(Some("the heater didn't answer (check the heater's power and the CAN wiring)"));
            return;
        }
        if heard && now - dec.last_heater.unwrap() > self.t.lost_s {
            self.stop(Some("lost the heater (no status for 3 s)"));
            return;
        }
        let (room, src) = self.room(dec, inp, now);
        if self.target.is_some()
            && heard
            && matches!(dec.st_name(), Some("idle") | Some("heating") | Some("fan"))
            && now - dec.last_heater.unwrap() < 1.0
        {
            // woken and idle (as the EasyStart Pro did): apply the mode
            self.mode = self.target.take().unwrap();
        }
        if self.mode == "heat" && room.is_none() {
            self.logc("auto_off", &json!({"mode": "heat"}), "off", "no fresh room temperature (cabin Ruuvi or heater sensor)", "aetherrv");
            self.problem = Some("no fresh room temperature: heat stopped".into());
            self.set_off();
        }
        if self.until.is_some_and(|u| now >= u) && self.mode != "off" {
            self.logc("timer_off", &json!({"mode": self.mode}), "off", "run time finished", "aetherrv");
            self.set_off();
        }
        // running blind (owner, §3): no contact with AetherRV's bus for the grace period while HEAT/FAN runs
        if matches!(self.mode.as_str(), "heat" | "fan")
            && let Some(age) = inp.bus_contact_age
            && age > self.t.blind_off_s
        {
            let mins = (self.t.blind_off_s / 60.0).round();
            let detail = format!("no contact with AetherRV's bus for {mins:.0} min");
            self.logc("auto_off", &json!({"mode": self.mode}), "off", &detail, "aetherrv");
            self.problem = Some(format!("{detail}: heater turned off"));
            self.set_off();
        }
        if room.is_some() {
            self.room_t = Some(now);
        }
        self.last_room = room;
        self.room_src = src;
        let r: Result<(), SendErr> = (|| {
            sink.send(0x60D, &HEARTBEAT)?;
            if self.tick % 2 == 0 {
                sink.send(0x054, &self.cmd_bytes(room))?;
                for (aid, data) in STATIC {
                    sink.send(aid, &data)?;
                }
            }
            if self.tick % 100 == 1 {
                sink.send(0x065, &room_frame(room, 1 + ((self.tick / 100) % 3) as u8))?;
            }
            if heard && self.burst_i < self.burst.len() {
                // the settings burst, two frames per tick (hat1 qlen 10)
                let end = (self.burst_i + 2).min(self.burst.len());
                for (aid, h) in &self.burst[self.burst_i..end] {
                    sink.send(*aid, h)?;
                }
                self.burst_i += 2;
            }
            Ok(())
        })();
        match r {
            Ok(()) => self.send_fail = 0,
            Err(SendErr::Blocked(e)) => {
                self.stop(Some(&format!("send refused: {e}")));
                return;
            }
            Err(SendErr::Io(e)) => {
                // a full send queue is normal while the heater wakes (our first frame waits for its ACK); the wake
                // timeout handles a heater that never answers. Once running, ~1 s of failed sends is fatal.
                self.send_fail += 1;
                if heard && self.send_fail > 10 {
                    self.stop(Some(&format!("send failed: {e}")));
                    return;
                }
            }
        }
        self.tick += 1;
        let st = dec.snapshot(now)[0].1.as_str().map(str::to_string);
        if self.mode == "off" && self.target.is_none() && matches!(st.as_deref(), Some("off") | None) && heard {
            let since = *self.idle_since.get_or_insert(now);
            if now - since > self.t.idle_stop_s {
                self.stop(None); // idle: let the bus and heater sleep
            }
        } else {
            self.idle_since = None;
        }
    }

    fn set_off(&mut self) {
        self.mode = "off".into();
        self.until = None;
        self.flags = 0x08;
    }

    /// The `climate.espar.ctl.*` keys (and the decoder's "our control is present" flag).
    pub fn publish(&self, dec: &mut Decoder, now: f64) -> Vec<(String, Value)> {
        dec.ctl_active = self.active;
        let left = self.until.map(|u| json!(aetherrv_core::num::round_int(u - now).max(0))).unwrap_or(Value::Null);
        vec![
            ("espar.controls_ok".into(), json!(self.enabled)),
            ("climate.espar.ctl.active".into(), json!(self.active)),
            ("climate.espar.ctl.mode".into(), json!(self.mode)),
            ("climate.espar.ctl.setpoint_c".into(), json!(self.setpoint_c)),
            ("climate.espar.ctl.left_s".into(), left),
            ("climate.espar.ctl.room_c".into(), self.last_room.map(|r| json!(r)).unwrap_or(Value::Null)),
            ("climate.espar.ctl.room_src".into(), self.room_src.clone().map(Value::String).unwrap_or(Value::Null)),
            ("climate.espar.ctl.problem".into(), self.problem.clone().map(Value::String).unwrap_or(Value::Null)),
        ]
    }

    // ── commands ────────────────────────────────────────────────────────────────────────────────────────────────

    /// The first half of Python `_command()`: validation, the refusals, and taking the bus. `gate_ok` is TxGate
    /// device "espar" (checked by the caller with the request text, so a refusal is logged by the gate too).
    pub fn begin(&mut self, cmd: &str, v: &Value, client: &str, now: f64, dec: &Decoder, inp: &Inputs, gate_ok: bool) -> Begin {
        if cmd != "set" {
            return Begin::Done(Reply::refused(format!("command '{cmd}' is not allowed")));
        }
        if !gate_ok {
            self.logc(cmd, v, "blocked", "tx_enabled is off", client);
            return Begin::Done(Reply::refused("Espar control is disabled (tx_enabled is off)"));
        }
        let mode = match v.get("mode").and_then(Value::as_str) {
            Some(m @ ("heat" | "fan" | "off")) => m.to_string(),
            _ => return Begin::Done(Reply::refused("mode must be heat, fan or off")),
        };
        if mode == "heat" && inp.heat_lockout {
            // lowbatt.py: protect the heater's after-run
            self.logc(cmd, v, "rejected", "heat locked out: house battery low", client);
            return Begin::Done(Reply::refused("Heat is locked out: the house battery is too low. Charge first."));
        }
        // Python: v.get("setpoint_c", self.setpoint_c) → float(); a key present with null is "bad setpoint"
        let sp = match v.get("setpoint_c") {
            None => Some(self.setpoint_c),
            Some(Value::Number(n)) => n.as_f64(),
            Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
            _ => None,
        };
        let Some(sp) = sp.filter(|x| x.is_finite()).map(round1) else {
            return Begin::Done(Reply::refused("bad setpoint"));
        };
        if mode == "heat" && !(SETPOINT_RANGE.0..=SETPOINT_RANGE.1).contains(&sp) {
            return Begin::Done(Reply::refused(format!("setpoint must be {:.0}-{:.0} °C", SETPOINT_RANGE.0, SETPOINT_RANGE.1)));
        }
        // a run time is a JSON integer (Python isinstance(m, int)); anything else is refused
        let run = match v.get("run_min") {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Number(n)) if n.is_i64() => Ok(n.as_i64()),
            _ => Err(()),
        };
        if mode != "off" && !matches!(run, Ok(r) if run_minutes_ok(r)) {
            return Begin::Done(Reply::refused("run time must be 1-240 min, 270-720 in 30-min steps, or unlimited"));
        }
        let run = run.unwrap_or(None);
        if self.other_controller_recent(dec, now) {
            self.logc(cmd, v, "rejected", "another controller is on the bus", client);
            return Begin::Done(Reply::refused("Another controller (the EasyStart Pro?) is on the heater's bus. Unplug it first."));
        }
        if mode == "heat" && self.room(dec, inp, now).0.is_none() {
            self.logc(cmd, v, "rejected", "no fresh room temperature", client);
            return Begin::Done(Reply::refused("No fresh room temperature (cabin Ruuvi or heater sensor). Nothing sent."));
        }
        let attach = v.get("attach").is_some_and(aetherrv_core::config::truthy);
        if mode == "off" && !self.active && !attach {
            self.mode = "off".into();
            self.until = None;
            self.logc(cmd, v, "noop", "already off (not driving the heater)", client);
            return Begin::Done(Reply::ok("already off"));
        }
        let cur = self.target.clone().unwrap_or_else(|| self.mode.clone());
        let same_run = self.active && mode == cur && run == self.run_min;
        if !same_run {
            // a setpoint nudge keeps the countdown running
            self.until = if mode == "off" { None } else { run.map(|r| now + r as f64 * 60.0) };
        }
        self.run_min = if mode == "off" { None } else { run };
        if mode == "heat" {
            self.setpoint_c = sp;
        }
        self.problem = None;
        if !self.active {
            // take over the bus: wake the heater in OFF first
            self.active = true;
            self.started_at = now;
            self.tick = 0;
            self.burst_i = 0;
            self.idle_since = None;
            self.send_fail = 0;
            self.mode = "off".into();
            self.target = if mode != "off" { Some(mode.clone()) } else { None };
        } else {
            self.mode = mode.clone();
            self.target = None;
        }
        if mode != "off" {
            self.flags = 0x08; // bit 3 stays set once a run has started (as the Pro does)
        }
        Begin::Wait(Pending {
            value: v.clone(),
            client: client.to_string(),
            mode,
            setpoint_c: sp,
            started: now,
            end: now + self.t.wake_s + self.t.verify_s,
        })
    }

    /// One verification look (every 100 ms): `Some(reply)` when done.
    pub fn check(&mut self, p: &Pending, dec: &Decoder, now: f64) -> Option<Reply> {
        if !self.active {
            return Some(self.unverified(p));
        }
        let st = dec.snapshot(now)[0].1.as_str().unwrap_or("").to_string();
        let want: &[&str] = match p.mode.as_str() {
            "heat" => &["heating"],
            "fan" => &["fan"],
            _ => &["idle", "cooldown", "off"],
        };
        if want.contains(&st.as_str()) && self.target.is_none() && dec.last_heater.unwrap_or(0.0) > p.started {
            self.logc("set", &p.value, "ok", &format!("heater {st}"), &p.client);
            let extra = if p.mode == "heat" { format!(" {:.1} °C", p.setpoint_c) } else { String::new() };
            return Some(Reply::ok(format!("Espar {}{extra}", p.mode)));
        }
        if now >= p.end {
            return Some(self.unverified(p));
        }
        None
    }

    fn unverified(&self, p: &Pending) -> Reply {
        let why = self.problem.clone().unwrap_or_else(|| "the heater didn't confirm".into());
        self.logc("set", &p.value, "unverified", &why, &p.client);
        Reply::unverified(format!("Espar {}: {why}. Not retried.", p.mode))
    }

    // ── restart: persist and resume ─────────────────────────────────────────────────────────────────────────────

    pub fn run_state(&self, boot_id: &str, now: f64) -> RunState {
        RunState {
            version: RUN_STATE_VERSION,
            boot_id: boot_id.to_string(),
            active: self.active,
            mode: self.mode.clone(),
            target: self.target.clone(),
            setpoint_c: self.setpoint_c,
            flags: self.flags,
            run_min: self.run_min,
            until: self.until,
            burst_done: self.burst_i >= self.burst.len(),
            last_room: self.last_room,
            room_src: self.room_src.clone(),
            room_t: self.room_t,
            last_contact: self.last_contact,
            resumes: self.resumes.clone(),
            alive: now,
        }
    }

    /// May a run saved by the previous process be continued? `Ok(())` or why not. `dec` has listened to the bus
    /// since this process started (ARCHITECTURE.md §7.2; owner 2026-11-23: the heater's own state decides, not age).
    pub fn resume_check(&self, saved: &RunState, boot_id: &str, dec: &Decoder, now: f64) -> Result<(), String> {
        if saved.version != RUN_STATE_VERSION {
            return Err(format!("saved run has format {} (this is {})", saved.version, RUN_STATE_VERSION));
        }
        if saved.boot_id != boot_id {
            return Err("saved run is from an earlier boot".into());
        }
        if !saved.active {
            return Err("nothing was running".into());
        }
        let recent = saved.resumes.iter().filter(|t| now - **t < self.t.resume_window_s).count();
        if recent >= self.t.max_resumes {
            return Err(format!("{recent} resumes in the last {:.0} s (crash loop?)", self.t.resume_window_s));
        }
        if dec.last_other_ctrl.is_some() {
            return Err("another controller is on the heater's bus".into());
        }
        if !dec.last_heater.is_some_and(|t| now - t < 1.0) {
            return Err("the heater isn't reporting".into());
        }
        let want = saved.target.clone().unwrap_or_else(|| saved.mode.clone());
        let st = dec.st.clone().unwrap_or_default();
        let ok = match want.as_str() {
            "heat" => st.name == "heating",
            "fan" => st.name == "fan",
            _ => st.name == "idle" && st.flags & 0x08 != 0,
        };
        if !ok {
            return Err(format!("the heater reports {} (flags {:02x}), not the saved {want}", st.name, st.flags));
        }
        Ok(())
    }

    /// Continue the saved run: same flags, mode, setpoint, deadline and room value; no wake burst, no OFF.
    pub fn resume(&mut self, saved: &RunState, dec: &Decoder, now: f64) {
        self.active = true;
        self.mode = saved.mode.clone();
        self.target = saved.target.clone();
        self.setpoint_c = saved.setpoint_c;
        self.flags = saved.flags;
        self.run_min = saved.run_min;
        self.until = saved.until;
        self.burst_i = if saved.burst_done { self.burst.len() } else { 0 };
        self.started_at = dec.last_heater.unwrap_or(now).min(now);
        self.tick = 0;
        self.send_fail = 0;
        self.idle_since = None;
        self.problem = None;
        self.last_room = saved.last_room;
        self.room_src = saved.room_src.clone();
        self.saved_room = saved.last_room.zip(saved.room_t);
        self.last_contact = saved.last_contact;
        self.resumes = saved.resumes.iter().copied().filter(|t| now - *t < self.t.resume_window_s).collect();
        self.resumes.push(now);
        let gap = now - saved.alive;
        self.logc(
            "resume",
            &json!({"mode": saved.target.clone().unwrap_or_else(|| saved.mode.clone()), "gap_s": round1(gap)}),
            "ok",
            &format!("continued the run after a {} s gap (heater still {})", py_float(round1(gap)),
                     dec.st_name().unwrap_or("?")),
            "aetherrv",
        );
    }

    /// Log why a saved run wasn't continued (only when something was running).
    pub fn note_not_resumed(&self, saved: &RunState, why: &str, now: f64) {
        if saved.active {
            self.logc(
                "resume",
                &json!({"mode": saved.target.clone().unwrap_or_else(|| saved.mode.clone()), "gap_s": round1(now - saved.alive)}),
                "skipped",
                &format!("not resumed: {why}; back OFF and silent"),
                "aetherrv",
            );
        }
    }
}

/// The value as Python's log text (`set {'mode': 'off'}`).
pub fn request_text(v: &Value) -> String {
    format!("set {}", py_str(v))
}
