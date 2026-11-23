//! AetherRV as the Espar's controller, against a simulated heater (no real bus): the Python tests of
//! test_espar_control.py ported, plus the owner's 2026-11-23 rules (resume, running blind, lockout).

use aetherrv_core::cmdlog::CommandLog;
use aetherrv_core::command::{Reply, ResultKind};
use aetherrv_espar::control::{Begin, Control, Inputs, Pending, RunState, SendErr, Sink};
use proto_espar::Frame;
use proto_espar::decoder::{Decoder, Event};
use serde_json::{Value, json};

#[derive(Default)]
struct Bus {
    sent: Vec<(u32, Vec<u8>)>,
    enabled: bool,
    /// Fail the first n sends with ENOBUFS.
    fail_first: usize,
    n: usize,
}

impl Sink for Bus {
    fn send(&mut self, id: u32, data: &[u8]) -> Result<(), SendErr> {
        if !self.enabled {
            return Err(SendErr::Blocked("transmit to 'espar' is disabled".into()));
        }
        self.n += 1;
        if self.n <= self.fail_first {
            return Err(SendErr::Io("[Errno 105] No buffer space available".into()));
        }
        self.sent.push((id, data.to_vec()));
        Ok(())
    }
}

impl Bus {
    fn cmds(&self) -> Vec<Vec<u8>> {
        self.sent.iter().filter(|(i, _)| *i == 0x054).map(|(_, d)| d.clone()).collect()
    }
    fn ids(&self) -> std::collections::BTreeSet<u32> {
        self.sent.iter().map(|(i, _)| *i).collect()
    }
}

struct Rig {
    c: Control,
    a: Decoder,
    bus: Bus,
    inp: Inputs,
    now: f64,
}

fn rig(enabled: bool, room: bool) -> Rig {
    let mut c = Control::new(enabled, CommandLog::memory("espar"));
    c.t.verify_s = 0.5;
    c.t.wake_s = 0.5;
    let inp = Inputs { cabin: room.then_some((18.0, 1.0)), victron_connected: Some(true), ..Default::default() };
    Rig { c, a: Decoder::new("espar", 5.0, false), bus: Bus { enabled, ..Default::default() }, inp, now: 1000.0 }
}

fn f(i: u32, h: &str) -> Frame {
    Frame::hex(i, h)
}

impl Rig {
    fn heater_says(&mut self, state_hex: &str) {
        self.a.on_frame(&f(0x2C4, state_hex), self.now);
        self.a.on_frame(&f(0x625, "2510ABE5FC3B5806"), self.now);
    }
    fn hear(&mut self, fr: Frame) {
        if self.a.on_frame(&fr, self.now) == Event::OtherController {
            self.c.on_other_controller();
        }
    }
    fn step(&mut self) {
        let (now, inp) = (self.now, self.inp.clone());
        self.c.step(now, &self.a, &inp, &mut self.bus);
    }
    fn begin(&mut self, v: Value) -> Begin {
        let (now, inp) = (self.now, self.inp.clone());
        let gate_ok = self.bus.enabled;
        self.c.begin("set", &v, "kiosk", now, &self.a, &inp, gate_ok)
    }
    /// Run a command against a heater behaving like `heater(mode byte of our last 054) -> 2C4 hex`, stepping every
    /// 30 ms (as the Python tests' simulated heater did), until the command answers.
    fn command(&mut self, v: Value, heater: &dyn Fn(Option<u8>) -> &'static str, max_steps: usize) -> Reply {
        let p: Pending = match self.begin(v) {
            Begin::Done(r) => return r,
            Begin::Wait(p) => p,
        };
        // like the Python tests: the simulated heater runs all its steps (`await t` after the command)
        let mut since_check = 0.0;
        let mut reply = None;
        for _ in 0..max_steps {
            self.now += 0.03;
            self.step();
            let m = self.bus.cmds().last().map(|d| d[1]);
            let st = heater(m);
            self.heater_says(st);
            since_check += 0.03;
            if since_check >= 0.1 && reply.is_none() {
                since_check = 0.0;
                reply = self.c.check(&p, &self.a, self.now);
            }
        }
        if let Some(r) = reply {
            return r;
        }
        loop {
            self.now += 0.1;
            self.step();
            if let Some(r) = self.c.check(&p, &self.a, self.now) {
                return r;
            }
        }
    }
}

fn heats_on_heat(m: Option<u8>) -> &'static str {
    if m == Some(0x05) { "090400001C020000" } else { "030000001C020000" }
}

fn idle(_m: Option<u8>) -> &'static str {
    "030000001C020000"
}

/// py: tests/test_espar_control.py::test_disabled_sends_nothing_and_refuses
#[test]
fn test_disabled_sends_nothing_and_refuses() {
    let mut r = rig(false, true);
    let rep = r.command(json!({"mode": "heat", "setpoint_c": 21}), &idle, 10);
    assert!(!rep.is_ok() && rep.detail.contains("disabled"));
    assert!(r.bus.sent.is_empty());
    assert_eq!(r.c.log.rows()[0].outcome, "blocked");
}

/// py: tests/test_espar_control.py::test_heat_takeover_verify_timer_and_idle_stop
#[test]
fn test_heat_takeover_verify_timer_and_idle_stop() {
    let mut r = rig(true, true);
    let rep = r.command(json!({"mode": "heat", "setpoint_c": 21.5, "run_min": 30}), &heats_on_heat, 40);
    assert!(rep.is_ok(), "{rep:?}");
    assert_eq!(rep.detail, "Espar heat 21.5 °C");
    let cmd = r.bus.cmds().last().unwrap().clone();
    assert_eq!(cmd[..4], [0x09, 0x05, 0xD7, 0x00]);
    assert_eq!(cmd[4..6], (((18.0 + 50.0) * 10.0) as u16).to_le_bytes());
    for id in [0x055, 0x056, 0x057, 0x60D, 0x062, 0x10A, 0x06D] {
        assert!(r.bus.ids().contains(&id), "{id:#x} missing (cyclic + settings burst)");
    }
    let left = r.c.until.unwrap() - r.now;
    assert!(1790.0 < left && left <= 1800.0, "{left}");
    r.c.until = Some(0.0); // run time over → OFF
    r.step();
    assert_eq!(r.bus.cmds().last().unwrap()[..2], [0x08, 0x00]);
    assert_eq!(r.c.mode, "off");
    assert!(!r.c.reset_requested);
    assert!(r.c.log.rows().iter().any(|x| x.cmd == "timer_off"));
}

/// py: tests/test_espar_control.py::test_no_room_temperature_refuses_heat_and_heater_silence_stops
#[test]
fn test_no_room_temperature_refuses_heat_and_heater_silence_stops() {
    let mut r = rig(true, false);
    let rep = r.command(json!({"mode": "heat", "setpoint_c": 21}), &idle, 10);
    assert!(!rep.is_ok() && rep.detail.contains("room temperature"));
    assert!(r.bus.sent.is_empty());
    r.inp.cabin = Some((18.0, 1.0));
    // nobody answers: stop, reset hat1, report
    let p = match r.begin(json!({"mode": "fan", "run_min": 10})) {
        Begin::Wait(p) => p,
        Begin::Done(x) => panic!("{x:?}"),
    };
    let mut rep = None;
    for _ in 0..20 {
        r.now += 0.05;
        r.step();
        if let Some(x) = r.c.check(&p, &r.a, r.now) {
            rep = Some(x);
            break;
        }
    }
    let rep = rep.unwrap();
    assert!(!rep.is_ok() && rep.result == ResultKind::Unverified);
    assert!(!r.c.active && r.c.reset_requested);
    assert!(r.c.problem.as_deref().unwrap().contains("didn't answer"));
}

/// py: tests/test_espar_control.py::test_another_controller_on_the_bus_stops_us
#[test]
fn test_another_controller_on_the_bus_stops_us() {
    let mut r = rig(true, true);
    r.hear(f(0x60D, "0D10FD1D02207000")); // an EasyStart Pro is talking
    let rep = r.command(json!({"mode": "fan", "run_min": 5}), &idle, 10);
    assert!(!rep.is_ok() && rep.detail.contains("Another controller"));
    assert!(r.bus.sent.is_empty());
    r.a.last_other_ctrl = None;
    r.c.active = true;
    r.c.mode = "fan".into();
    r.hear(f(0x054, "0000FEFFDA020000")); // it appears while we drive the heater
    assert!(!r.c.active);
    assert!(r.c.problem.as_deref().unwrap().contains("another controller"));
    assert_eq!(r.c.mode, "off");
    assert!(r.c.reset_requested);
}

/// py: tests/test_espar_control.py::test_attach_in_off_then_heat_only_after_idle_and_burst_is_spread
#[test]
fn test_attach_in_off_then_heat_only_after_idle_and_burst_is_spread() {
    let mut r = rig(true, true);
    let p = match r.begin(json!({"mode": "off", "attach": true})) {
        Begin::Wait(p) => p,
        Begin::Done(x) => panic!("{x:?}"),
    };
    let mut rep = None;
    for k in 0..30 {
        r.now += 0.03;
        let before = r.bus.sent.len();
        r.step();
        assert!(r.bus.sent.len() - before <= 7, "never more than hat1's queue can take");
        r.heater_says("030000001C020000"); // woken: idle
        if k % 3 == 2 && rep.is_none() {
            rep = r.c.check(&p, &r.a, r.now);
        }
    }
    let rep = rep.unwrap();
    assert!(rep.is_ok() && r.c.active && r.c.mode == "off", "{rep:?}");
    assert!(r.bus.cmds().iter().all(|d| d[1] == 0x00), "nothing but OFF was ever sent");
    for id in [0x062, 0x06D, 0x10A] {
        assert!(r.bus.ids().contains(&id), "the whole settings burst went out");
    }

    let mut r2 = rig(true, true);
    let rep2 = r2.command(json!({"mode": "heat", "setpoint_c": 20, "run_min": 60}), &heats_on_heat, 40);
    let modes: Vec<u8> = r2.bus.cmds().iter().map(|d| d[1]).collect();
    assert!(rep2.is_ok() && modes[0] == 0x00 && modes.contains(&0x05), "woke in OFF, HEAT only after idle");
    let until = r2.c.until;
    let rep3 = r2.command(json!({"mode": "heat", "setpoint_c": 21, "run_min": 60}), &heats_on_heat, 40); // setpoint nudge
    assert!(rep3.is_ok());
    assert_eq!(r2.c.until, until, "countdown not restarted");
    assert_eq!(r2.c.setpoint_c, 21.0);
}

/// py: tests/test_espar_control.py::test_full_queue_while_waking_is_not_fatal
#[test]
fn test_full_queue_while_waking_is_not_fatal() {
    let mut r = rig(true, true);
    r.bus.fail_first = 5; // ENOBUFS behind the un-ACKed first frame
    let rep = r.command(json!({"mode": "off", "attach": true}), &idle, 20);
    assert!(rep.is_ok() && r.c.active && !r.c.reset_requested, "{rep:?}");
}

/// py: tests/test_lowbatt.py::test_heat_starts_refused_during_lockout (the Espar half)
#[test]
fn test_heat_starts_refused_during_lockout() {
    let mut r = rig(true, true);
    r.inp.heat_lockout = true;
    let rep = r.command(json!({"mode": "heat", "setpoint_c": 20}), &idle, 5);
    assert!(!rep.is_ok() && rep.detail.contains("locked out"));
    assert!(r.bus.sent.is_empty());
    // fan is still allowed (no burning)
    let rep = r.command(json!({"mode": "fan"}), &|_| "210000001C020000", 40);
    assert!(rep.is_ok(), "{rep:?}");
}

// ── owner rules of 2026-11-23 ────────────────────────────────────────────────────────────────────────────────────

fn heating_run() -> Rig {
    let mut r = rig(true, true);
    let rep = r.command(json!({"mode": "heat", "setpoint_c": 21.5, "run_min": 30}), &heats_on_heat, 60);
    assert!(rep.is_ok());
    for _ in 0..30 {
        r.now += 0.1;
        r.step();
        r.heater_says("090400001C020000");
    }
    r
}

/// A restarted process continues the run: same flags, mode, setpoint, deadline, real room value; no burst, no OFF.
#[test]
fn resume_continues_a_heat_run_without_burst_or_off() {
    let mut old = heating_run();
    let saved: RunState = old.c.run_state("boot-1", old.now);
    assert!(saved.active && saved.mode == "heat" && saved.burst_done);
    // the new process: fresh decoder, fresh controller, the bus quiet except the heater (still heating)
    let mut new = rig(true, false); // the cabin Ruuvi isn't back yet (bus reconnecting)
    new.now = old.now + 1.5;
    new.heater_says("090400001C020000");
    new.c.resume_check(&saved, "boot-1", &new.a, new.now).unwrap();
    new.c.resume(&saved, &new.a, new.now);
    for _ in 0..10 {
        new.now += 0.1;
        new.step();
        new.heater_says("090400001C020000");
    }
    let cmds = new.bus.cmds();
    assert!(!cmds.is_empty());
    assert!(cmds.iter().all(|d| d[..4] == [0x09, 0x05, 0xD7, 0x00]), "the same HEAT command from the first frame");
    assert!(cmds.iter().all(|d| d[4..6] != [0xFE, 0xFF]), "never FE FF (0 °C = full power)");
    assert_eq!(cmds[0][4..6], (680u16).to_le_bytes(), "the saved room value");
    assert!(!new.bus.ids().contains(&0x062), "no wake burst");
    assert_eq!(new.c.until, saved.until, "deadline kept");
    let rows = new.c.log.rows();
    assert_eq!((rows[0].cmd.as_str(), rows[0].outcome.as_str()), ("resume", "ok"));
    assert!(new.c.active && new.c.mode == "heat");
    old.c.stop(None);
}

#[test]
fn resume_refused_when_the_heater_is_not_running_the_saved_command() {
    let old = heating_run();
    let saved = old.c.run_state("boot-1", old.now);
    let mut new = rig(true, true);
    new.now = old.now + 20.0;
    new.heater_says("030800001C020000"); // it already stopped itself: idle + after-run
    let e = new.c.resume_check(&saved, "boot-1", &new.a, new.now).unwrap_err();
    assert!(e.contains("not the saved heat"), "{e}");
    new.c.note_not_resumed(&saved, &e, new.now);
    assert_eq!(new.c.log.rows()[0].outcome, "skipped");
    new.step();
    assert!(new.bus.sent.is_empty(), "back OFF and silent");
}

#[test]
fn resume_refused_on_another_boot_another_controller_silence_or_a_crash_loop() {
    let old = heating_run();
    let mut saved = old.c.run_state("boot-1", old.now);
    let mut new = rig(true, true);
    new.now = old.now + 1.0;
    new.heater_says("090400001C020000");
    assert!(new.c.resume_check(&saved, "boot-2", &new.a, new.now).unwrap_err().contains("earlier boot"));
    saved.resumes = vec![new.now - 10.0, new.now - 20.0, new.now - 30.0];
    assert!(new.c.resume_check(&saved, "boot-1", &new.a, new.now).unwrap_err().contains("crash loop"));
    saved.resumes.clear();
    new.hear(f(0x60D, "0D10FD1D02207000"));
    assert!(new.c.resume_check(&saved, "boot-1", &new.a, new.now).unwrap_err().contains("another controller"));
    let mut quiet = rig(true, true);
    quiet.now = old.now + 1.0;
    assert!(quiet.c.resume_check(&saved, "boot-1", &quiet.a, quiet.now).unwrap_err().contains("isn't reporting"));
}

#[test]
fn a_deadline_that_passed_during_the_gap_ends_cleanly_after_the_resume() {
    let mut old = heating_run();
    old.c.until = Some(old.now + 0.5);
    let saved = old.c.run_state("boot-1", old.now);
    let mut new = rig(true, true);
    new.now = old.now + 2.0;
    new.heater_says("090400001C020000");
    new.c.resume_check(&saved, "boot-1", &new.a, new.now).unwrap();
    new.c.resume(&saved, &new.a, new.now);
    new.now += 0.1;
    new.step();
    assert_eq!(new.bus.cmds()[0][..2], [0x08, 0x00], "the normal timer OFF");
    assert!(new.c.log.rows().iter().any(|x| x.cmd == "timer_off"));
}

#[test]
fn running_blind_turns_heat_off_after_the_grace_period() {
    let mut r = heating_run();
    r.c.t.blind_off_s = 600.0;
    r.inp.bus_contact_age = Some(599.0);
    r.now += 0.1;
    r.step();
    assert_eq!(r.c.mode, "heat");
    r.inp.bus_contact_age = Some(601.0);
    for _ in 0..2 {
        r.now += 0.1;
        r.step(); // 054 goes out every other tick
    }
    assert_eq!(r.c.mode, "off");
    assert_eq!(r.bus.cmds().last().unwrap()[..2], [0x08, 0x00]);
    assert!(r.c.active, "keeps driving the after-run");
    let row = r.c.log.rows().into_iter().find(|x| x.cmd == "auto_off").unwrap();
    assert!(row.detail.contains("no contact with AetherRV's bus for 10 min"), "{}", row.detail);
}

#[test]
fn suspended_sends_nothing() {
    let mut r = heating_run();
    let n = r.bus.sent.len();
    r.c.suspended = true;
    r.now += 0.1;
    r.step();
    assert_eq!(r.bus.sent.len(), n, "SIGTERM: stop transmitting, no OFF");
}
