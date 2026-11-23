//! NMEA 2000 switching: AetherRV commands the load controller (CLMD16) and watches its wired inputs. A port of
//! `switching.SwitchController`.
//!
//! Owner decision 2026-11-22: the CKM12 keypads are handled by the CLMD16's own configuration (dual control), so they
//! keep working while the Pi is off. AetherRV runs no switching logic of its own: no keypad relay, no input follows,
//! no ignition auto-off. It only sends the commands a person asks for (tiles, triggers) and reads the controller's
//! wired inputs (shown on the UI and used to refuse interlocked ONs).
//!
//! It never sends direct breaker commands (126208 for 127501/127500 on the breaker bank). A `set` presses the
//! channel's virtual switch (`press = N` on `vsw_bank`), which feeds the same controller logic as the keypad button.
//! The only direct sends: the trigger safety-off, and clearing a tripped channel (one OFF: the safe direction).
//!
//! Transmit rules: TxGate device "n2k", fresh status (≤ 3 s old; the CLMD16 reports every 1.5 s), send exactly once,
//! verify from status (the CLMD16's 126208 Acknowledge is always 0x44), never retry, log every command.

use crate::node::{N2kNode, NodeError};
use aetherrv_core::cmdlog::CommandLog;
use aetherrv_core::command::Reply;
use aetherrv_core::config::Cfg;
use aetherrv_core::state::StateStore;
use aetherrv_core::txgate::TxGate;
use proto_n2k::adapter::N2kAdapter;
use proto_n2k::decode::{BankStates, SwitchState};
use proto_n2k::encode::{PGN_GROUP_FUNCTION, switch_command};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const DEVICE: &str = "n2k";
/// s: newest status must be at most this old (the CLMD16 reports every 1.5 s).
pub const FRESH: f64 = 3.0;

fn now_s() -> f64 {
    aetherrv_core::clock::now_s()
}

#[derive(Debug, Clone, Default)]
pub struct Debounced {
    pub value: Option<bool>,
    pub pending: Option<bool>,
    pub since: f64,
    pub hold: f64,
}

impl Debounced {
    pub fn new(hold: f64) -> Self {
        Debounced { hold, ..Default::default() }
    }
    pub fn feed(&mut self, raw: bool, now: f64) {
        if Some(raw) != self.pending {
            self.pending = Some(raw);
            self.since = now;
        }
    }
    /// Commit the pending value once it has been stable for `hold`. Returns true on a change.
    pub fn settle(&mut self, now: f64) -> bool {
        if self.pending.is_some() && self.pending != self.value && now - self.since >= self.hold {
            self.value = self.pending;
            return true;
        }
        false
    }
}

/// A momentary press of a load-controller VIRTUAL switch whose logic (e.g. a 10 s timer) lives in the controller.
/// AetherRV pulses the virtual switch on → off and watches the output it drives; if that output is still on after
/// `expect_off_s`, AetherRV switches it off directly (safety net) and says so.
#[derive(Debug, Clone)]
pub struct Trigger {
    pub id: String,
    pub name: String,
    pub bank: u8,
    pub switch: u8,
    pub channel: u8,
    pub pulse_s: f64,
    pub expect_off_s: f64,
}

pub struct SwitchController {
    pub state: Arc<StateStore>,
    pub n2k: Arc<Mutex<N2kAdapter>>,
    pub node: Arc<N2kNode>,
    pub gate: Arc<TxGate>,
    pub log: CommandLog,
    pub controller: u8,
    pub bank: u8,
    pub vsw_bank: u8,
    pub press_s: Mutex<f64>,
    pub max_hold_s: Mutex<f64>,
    pub verify: Mutex<f64>,
    pub channels: Mutex<BTreeMap<u8, Cfg>>,
    pub triggers: Mutex<BTreeMap<String, Trigger>>,
    pub inputs: Mutex<BTreeMap<u8, Cfg>>,
    pub deb: Arc<Mutex<BTreeMap<u8, Debounced>>>,
    held: Mutex<HashMap<u8, Option<tokio::task::JoinHandle<()>>>>,
    held_at: Mutex<HashMap<u8, Instant>>,
    seq: AtomicU8,
    locks: Mutex<HashMap<u8, Arc<tokio::sync::Mutex<()>>>>,
    me: Mutex<std::sync::Weak<SwitchController>>,
}

fn int_of(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        _ => None,
    }
}

fn st(states: &BankStates, idx: u8) -> Option<&'static str> {
    states.get((idx as usize).wrapping_sub(1)).copied().flatten().map(SwitchState::as_str)
}

impl SwitchController {
    pub fn new(state: Arc<StateStore>, n2k: Arc<Mutex<N2kAdapter>>, node: Arc<N2kNode>, gate: Arc<TxGate>, dev: &Cfg, log: CommandLog) -> Arc<Self> {
        let sw = dev.child("switching");
        let bank = sw.i64("bank", 32) as u8;
        let channels: BTreeMap<u8, Cfg> = dev
            .list("switch")
            .into_iter()
            .filter(|c| c.i64("bank", bank as i64) == bank as i64)
            .filter_map(|c| c.opt_i64("channel").map(|ch| (ch as u8, c)))
            .collect();
        let triggers: BTreeMap<String, Trigger> = dev
            .list("trigger")
            .into_iter()
            .filter_map(|g| {
                let id = match g.get("id")? {
                    Value::String(s) => s.clone(),
                    v => v.to_string(),
                };
                Some((
                    id.clone(),
                    Trigger {
                        name: g.str("name", &id),
                        id,
                        bank: g.opt_i64("bank")? as u8,
                        switch: g.opt_i64("switch")? as u8,
                        channel: g.opt_i64("channel")? as u8,
                        pulse_s: g.f64("pulse_s", 1.0),
                        expect_off_s: g.f64("expect_off_s", 12.0),
                    },
                ))
            })
            .collect();
        let inputs: BTreeMap<u8, Cfg> =
            dev.list("input").into_iter().filter_map(|i| i.opt_i64("input").map(|n| (n as u8, i))).collect();
        let deb: BTreeMap<u8, Debounced> = inputs.iter().map(|(n, i)| (*n, Debounced::new(i.f64("debounce_s", 0.3)))).collect();
        let ctl = Arc::new(SwitchController {
            state,
            n2k,
            node,
            gate,
            log,
            controller: sw.i64("controller", 0) as u8,
            bank,
            vsw_bank: sw.i64("vsw_bank", 33) as u8,
            press_s: Mutex::new(sw.f64("press_s", 0.4)),
            max_hold_s: Mutex::new(sw.f64("max_hold_s", 8.0)),
            verify: Mutex::new(4.0),
            channels: Mutex::new(channels),
            triggers: Mutex::new(triggers),
            inputs: Mutex::new(inputs),
            deb: Arc::new(Mutex::new(deb)),
            held: Mutex::new(HashMap::new()),
            held_at: Mutex::new(HashMap::new()),
            seq: AtomicU8::new(0),
            locks: Mutex::new(HashMap::new()),
            me: Mutex::new(std::sync::Weak::new()),
        });
        *ctl.me.lock().unwrap() = Arc::downgrade(&ctl);
        // the controller's wired inputs feed the debouncers; keypad banks are deliberately ignored
        let (deb, controller) = (ctl.deb.clone(), ctl.controller);
        ctl.n2k.lock().unwrap().add_bank_listener(move |src, inst, states, now| {
            if src != controller || inst != bank {
                return;
            }
            let mut d = deb.lock().unwrap();
            for (n, db) in d.iter_mut() {
                let idx = 16 + *n as usize - 1;
                if let Some(Some(raw)) = states.get(idx) {
                    db.feed(*raw == SwitchState::On, now);
                }
            }
        });
        ctl.state.set("n2k.controls_ok", json!(ctl.gate.is_enabled(DEVICE)));
        ctl.state.set("switching.automation", json!(false)); // no logic runs in AetherRV (the UI reads this flag)
        let trig: Vec<Value> = ctl
            .triggers
            .lock()
            .unwrap()
            .values()
            .map(|g| json!({"id": g.id, "name": g.name, "channel": g.channel, "bank": g.bank, "switch": g.switch}))
            .collect();
        ctl.state.set("switching.triggers", Value::Array(trig));
        ctl
    }

    fn logc(&self, cmd: &str, value: &Value, outcome: &str, detail: &str, client: &str) {
        self.log.log(cmd, value, outcome, detail, client);
    }

    /// (states, age) of the breaker bank.
    pub fn status(&self) -> Option<(BankStates, f64)> {
        let n = self.n2k.lock().unwrap();
        n.bank(self.controller, self.bank).map(|b| (b.states, now_s() - b.seen))
    }

    fn bank_seen(&self, bank: u8) -> Option<(BankStates, f64)> {
        let n = self.n2k.lock().unwrap();
        n.bank(self.controller, bank).map(|b| (b.states, b.seen))
    }

    pub fn bank_state(&self, bank: u8, idx: u8) -> Option<&'static str> {
        let (states, seen) = self.bank_seen(bank)?;
        if now_s() - seen > FRESH {
            return None;
        }
        st(&states, idx)
    }

    fn lock_for(&self, ch: u8) -> Arc<tokio::sync::Mutex<()>> {
        self.locks.lock().unwrap().entry(ch).or_default().clone()
    }

    fn next_seq(&self) -> u8 {
        (self.seq.fetch_add(1, Ordering::SeqCst).wrapping_add(1)) & 7
    }

    fn send_press(&self, switch: u8, on: bool) -> Result<(), NodeError> {
        let payload = switch_command(self.vsw_bank as i64, switch as i64, on).map_err(|e| NodeError::Encode(e.to_string()))?;
        self.node.send_fast(PGN_GROUP_FUNCTION, self.controller, &payload, self.next_seq(), 3)
    }

    fn send_direct(&self, bank: u8, ch: u8, on: bool) -> Result<(), NodeError> {
        let payload = switch_command(bank as i64, ch as i64, on).map_err(|e| NodeError::Encode(e.to_string()))?;
        self.node.send_fast(PGN_GROUP_FUNCTION, self.controller, &payload, self.next_seq(), 3)
    }

    async fn wait(&self, mut cond: impl FnMut() -> bool, timeout: f64) -> bool {
        let end = Instant::now() + Duration::from_secs_f64(timeout.max(0.0));
        while Instant::now() < end {
            if cond() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        cond()
    }

    // ── commands ────────────────────────────────────────────────────────────────────────────────────────────────

    pub async fn command(self: &Arc<Self>, cmd: &str, value: &Value, client: &str) -> Reply {
        if cmd == "trigger" {
            let id = value.get("trigger").map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()));
            let g = id.and_then(|id| self.triggers.lock().unwrap().get(&id).cloned());
            let Some(g) = g else { return Reply::refused("unknown trigger") };
            let l = self.lock_for(g.channel);
            let _g = l.lock().await;
            return self.trigger(&g, client).await;
        }
        let Some(ch) = int_of(value.get("channel")).filter(|c| (0..=255).contains(c)).map(|c| c as u8) else {
            return Reply::refused("bad request");
        };
        let l = self.lock_for(ch);
        let _g = l.lock().await;
        self.command_ch(cmd, value, ch, client).await
    }

    async fn command_ch(self: &Arc<Self>, cmd: &str, value: &Value, ch: u8, client: &str) -> Reply {
        let conf = self.channels.lock().unwrap().get(&ch).cloned();
        if let Err(e) = self.gate.check(DEVICE, &format!("{cmd} ch{ch}={}", aetherrv_core::pyfmt::py_str(value)), false) {
            self.logc(cmd, value, "blocked", &e.to_string(), client);
            return Reply::refused("switch control is disabled (tx_enabled is off)");
        }
        let Some(conf) = conf else {
            self.logc(cmd, value, "rejected", "channel not configured", client);
            return Reply::refused(format!("channel {ch} is not configured"));
        };
        if cmd == "dim" {
            self.logc(cmd, value, "rejected", "dimming runs in the load controller (hold-to-dim)", client);
            return Reply::refused("Dimming goes through the load controller's own hold-to-dim: hold the tile. Nothing was sent.");
        }
        if cmd == "hold" {
            return self.hold(&conf, value, ch, client).await;
        }
        if cmd != "set" {
            self.logc(cmd, value, "rejected", "not an allowed command", client);
            return Reply::refused(format!("command '{cmd}' is not allowed"));
        }
        let name = conf.str("name", &format!("channel {ch}"));
        let on = match value.get("on") {
            Some(Value::Bool(b)) => *b,
            _ => {
                self.logc(cmd, value, "rejected", "on must be true or false", client);
                return Reply::refused("bad request");
            }
        };
        let want = if on { "on" } else { "off" };
        if !on
            && let Some((states, age)) = self.status()
            && age <= FRESH
            && st(&states, ch) == Some("error")
        {
            // a tripped channel: OFF goes straight to the breaker to clear it
            return self.clear_trip(ch, &name, cmd, value, client).await;
        }
        if on && conf.bool("blocked", false) {
            self.logc(cmd, value, "rejected", &format!("channel blocked: {}", conf.str("blocked_reason", "")), client);
            let r = conf.str("blocked_reason", "");
            return Reply::refused(if r.is_empty() { format!("{name} is blocked") } else { r });
        }
        let Some(press) = conf.opt_i64("press").map(|p| p as u8) else {
            self.logc(cmd, value, "rejected", "no virtual switch configured for this channel", client);
            return Reply::refused(format!(
                "{name} has no virtual switch, so AetherRV can't ask the load controller to switch it. Nothing was sent."
            ));
        };
        let input_name = |n: i64| {
            self.inputs.lock().unwrap().get(&(n as u8)).map(|i| i.str("name", &format!("input {n}"))).unwrap_or_else(|| format!("input {n}"))
        };
        if !on && let Some(held) = conf.opt_i64("on_with_input") {
            // the controller keeps it on while this input is on
            let v = self.deb.lock().unwrap().get(&(held as u8)).and_then(|d| d.value);
            if v == Some(true) {
                let nm = input_name(held);
                self.logc(cmd, value, "rejected", &format!("held on by {nm}"), client);
                return Reply::refused(format!("{name} stays on while {nm} is on (load controller logic). Nothing was sent."));
            }
        }
        let need = conf.opt_i64("requires_input");
        if on && let Some(need) = need {
            // extra caution on top of the controller's own interlock
            let v = self.deb.lock().unwrap().get(&(need as u8)).and_then(|d| d.value);
            if v != Some(true) {
                let nm = input_name(need);
                self.logc(cmd, value, "rejected", &format!("interlock: {nm} is not on"), client);
                return Reply::refused(format!("{name} needs {nm} on"));
            }
        }
        let Some((states, age)) = self.status().filter(|(_, a)| *a <= FRESH) else {
            self.logc(cmd, value, "failed", "no fresh 127501 from the load controller", client);
            return Reply::refused("no fresh status from the load controller; nothing sent");
        };
        let _ = age;
        let cur = st(&states, ch); // on | off | error | None
        if cur == Some(want) {
            self.logc(cmd, value, "noop", "already in requested state", client);
            return Reply::ok(format!("already {want}"));
        }
        if cur == Some("error") {
            self.logc(cmd, value, "rejected", "channel is tripped", client);
            return Reply::refused(format!("{name} is tripped (fault). Clear it at the load controller."));
        }
        let vs = self.bank_state(self.vsw_bank, press);
        let Some(vs) = vs else {
            self.logc(cmd, value, "failed", &format!("no fresh status for virtual switch {}.{press}", self.vsw_bank), client);
            return Reply::refused("no fresh status for the load controller's virtual switches; nothing sent");
        };
        if vs == "on" {
            self.logc(cmd, value, "rejected", &format!("virtual switch {}.{press} is already on (held or stuck)", self.vsw_bank), client);
            return Reply::refused(format!("{name}: its virtual switch is already pressed (held or stuck). Nothing sent."));
        }
        let pulse = conf.f64("press_s", *self.press_s.lock().unwrap());
        let sent_at = now_s();
        // press: exactly one ON, then exactly one OFF
        if let Err(e) = self.send_press(press, true) {
            if e.is_blocked() {
                self.logc(cmd, value, "blocked", &e.to_string(), client);
                return Reply::refused(e.to_string());
            }
            self.logc(cmd, value, "failed", &format!("send ON: {e}"), client);
            return Reply::unverified("send failed (not retried)");
        }
        tokio::time::sleep(Duration::from_secs_f64(pulse)).await;
        if let Err(e) = self.send_press(press, false) {
            // startup release (release_stuck) clears it next time
            self.logc(cmd, value, "failed", &format!("send OFF (release): {e}"), client);
        }
        let (me, ctl, bank) = (self.clone(), self.controller, self.bank);
        let done = move || {
            let n = me.n2k.lock().unwrap();
            n.bank(ctl, bank).is_some_and(|b| b.seen >= sent_at && st(&b.states, ch) == Some(want))
        };
        let verify = *self.verify.lock().unwrap();
        if self.wait(done, verify).await {
            self.logc(cmd, value, "ok", &format!("virtual switch {}.{press} pressed; ch{ch} {want}", self.vsw_bank), client);
            return Reply::ok(format!("{name} {want}"));
        }
        let mut why = "its logic decides when it acts".to_string();
        if let Some(need) = need
            && self.deb.lock().unwrap().get(&(need as u8)).is_some_and(|d| d.value != Some(true))
        {
            why = format!("{} is off", input_name(need));
        }
        let cur_s = cur.map(str::to_string).unwrap_or_else(|| "None".into());
        self.logc(cmd, value, "unverified", &format!("virtual switch {}.{press} pressed; ch{ch} stayed {cur_s} ({why})", self.vsw_bank), client);
        Reply::unverified(format!("Pressed, but the load controller didn't switch {name} {want} ({why}). Not retried."))
    }

    /// Hold-to-dim, like a finger on the keypad button: `hold` on=true puts the channel's virtual switch ON,
    /// on=false releases it. Dimmer channels only.
    async fn hold(self: &Arc<Self>, conf: &Cfg, value: &Value, ch: u8, client: &str) -> Reply {
        let name = conf.str("name", &format!("channel {ch}"));
        let press = conf.opt_i64("press").map(|p| p as u8);
        let (Some(press), true) = (press, conf.str("kind", "switch") == "dimmer") else {
            self.logc("hold", value, "rejected", "not a dimmer with a virtual switch", client);
            return Reply::refused(format!("{name} can't be held (not a dimmer). Nothing was sent."));
        };
        let on = match value.get("on") {
            Some(Value::Bool(b)) => *b,
            _ => {
                self.logc("hold", value, "rejected", "on must be true or false", client);
                return Reply::refused("bad request");
            }
        };
        let press_s = *self.press_s.lock().unwrap();
        if !on {
            // release: always allowed, always sent
            if let Some(Some(t)) = self.held.lock().unwrap().remove(&press) {
                t.abort();
            }
            let since = self.held_at.lock().unwrap().remove(&press);
            let held_for = since.map(|t| t.elapsed().as_secs_f64()).unwrap_or(f64::INFINITY);
            let short = press_s - held_for;
            if short > 0.0 && short <= press_s {
                // a quick tap still makes a full-length press
                tokio::time::sleep(Duration::from_secs_f64(short)).await;
            }
            if let Err(e) = self.send_press(press, false) {
                self.logc("hold", value, "failed", &format!("release {}.{press}: {e}", self.vsw_bank), client);
                return Reply::unverified("release failed to send");
            }
            self.logc("hold", value, "ok", &format!("virtual switch {}.{press} released", self.vsw_bank), client);
            return Reply::ok(format!("{name} released"));
        }
        if self.held.lock().unwrap().contains_key(&press) {
            self.logc("hold", value, "rejected", "already held", client);
            return Reply::refused(format!("{name} is already being held. Nothing sent."));
        }
        if conf.bool("blocked", false) {
            self.logc("hold", value, "rejected", &format!("channel blocked: {}", conf.str("blocked_reason", "")), client);
            let r = conf.str("blocked_reason", "");
            return Reply::refused(if r.is_empty() { format!("{name} is blocked") } else { r });
        }
        let Some((states, _)) = self.status().filter(|(_, a)| *a <= FRESH) else {
            self.logc("hold", value, "failed", "no fresh 127501 from the load controller", client);
            return Reply::refused("no fresh status from the load controller; nothing sent");
        };
        if st(&states, ch) == Some("error") {
            self.logc("hold", value, "rejected", "channel is tripped", client);
            return Reply::refused(format!("{name} is tripped (fault). Nothing sent."));
        }
        let vs = self.bank_state(self.vsw_bank, press);
        if vs != Some("off") {
            self.logc("hold", value, "rejected", &format!("virtual switch {}.{press} is {}", self.vsw_bank, vs.unwrap_or("not reported")), client);
            return Reply::refused(format!("{name}: its virtual switch is already pressed or not reported. Nothing sent."));
        }
        if let Err(e) = self.send_press(press, true) {
            if e.is_blocked() {
                self.logc("hold", value, "blocked", &e.to_string(), client);
                return Reply::refused(e.to_string());
            }
            self.logc("hold", value, "failed", &format!("send ON: {e}"), client);
            return Reply::unverified("send failed (not retried)");
        }
        self.held_at.lock().unwrap().insert(press, Instant::now());
        let max_hold = *self.max_hold_s.lock().unwrap();
        let task = if max_hold > 0.0 {
            let (me, client) = (self.clone(), client.to_string());
            Some(tokio::spawn(async move { me.max_hold(press, ch, &client, max_hold).await }))
        } else {
            None
        };
        self.held.lock().unwrap().insert(press, task);
        self.logc("hold", value, "ok", &format!("virtual switch {}.{press} held", self.vsw_bank), client);
        Reply::ok(format!("{name} held"))
    }

    async fn max_hold(&self, press: u8, ch: u8, client: &str, max_hold: f64) {
        tokio::time::sleep(Duration::from_secs_f64(max_hold)).await;
        if self.held.lock().unwrap().remove(&press).is_none() {
            return;
        }
        self.held_at.lock().unwrap().remove(&press);
        let v = json!({"channel": ch, "on": false});
        match self.send_press(press, false) {
            Ok(()) => self.logc("hold", &v, "ok", &format!("virtual switch {}.{press} released after {} s (no release came)", self.vsw_bank, aetherrv_core::pyfmt::py_float(max_hold).trim_end_matches(".0")), client),
            Err(e) => self.logc("hold", &v, "failed", &format!("max-hold release: {e}"), client),
        }
    }

    pub fn holding(&self) -> bool {
        !self.held.lock().unwrap().is_empty()
    }

    /// Exactly one direct OFF to the tripped breaker channel (the CLMD16 clears the fault on OFF); verified.
    async fn clear_trip(self: &Arc<Self>, ch: u8, name: &str, cmd: &str, value: &Value, client: &str) -> Reply {
        let sent_at = now_s();
        if let Err(e) = self.send_direct(self.bank, ch, false) {
            if e.is_blocked() {
                self.logc(cmd, value, "blocked", &e.to_string(), client);
                return Reply::refused(e.to_string());
            }
            self.logc(cmd, value, "failed", &format!("send trip-clear OFF: {e}"), client);
            return Reply::unverified("send failed (not retried)");
        }
        let (me, ctl, bank) = (self.clone(), self.controller, self.bank);
        let cleared = move || {
            let n = me.n2k.lock().unwrap();
            n.bank(ctl, bank).is_some_and(|b| b.seen >= sent_at && st(&b.states, ch) == Some("off"))
        };
        let verify = *self.verify.lock().unwrap();
        if self.wait(cleared, verify).await {
            self.logc(cmd, value, "ok", &format!("trip cleared: direct OFF to ch{ch}"), client);
            return Reply::ok(format!("{name}: fault cleared (off)"));
        }
        self.logc(cmd, value, "unverified", &format!("trip-clear OFF sent to ch{ch}; still tripped"), client);
        Reply::unverified(format!("Sent OFF to clear {name}, but the load controller still reports a fault. Not retried."))
    }

    /// Virtual switches AetherRV presses (bank vsw_bank): channel `press` numbers and trigger switches.
    pub fn owned_switches(&self) -> Vec<u8> {
        let mut v: Vec<u8> = self.channels.lock().unwrap().values().filter_map(|c| c.opt_i64("press").map(|p| p as u8)).collect();
        v.extend(self.triggers.lock().unwrap().values().filter(|g| g.bank == self.vsw_bank).map(|g| g.switch));
        v.sort();
        v.dedup();
        v
    }

    /// At start-up: a virtual switch of ours that reads ON was left pressed (the Pi died mid-press). Release each
    /// one once (a falling edge: the controller's logic acts on presses, not releases).
    pub async fn release_stuck(self: &Arc<Self>, wait: f64) -> Vec<u8> {
        let me = self.clone();
        if !self.wait(move || me.bank_state(me.vsw_bank, 1).is_some(), wait).await {
            tracing::warn!("no virtual-switch status (bank {}): stuck-switch check skipped", self.vsw_bank);
            return vec![];
        }
        let stuck: Vec<u8> = self.owned_switches().into_iter().filter(|n| self.bank_state(self.vsw_bank, *n) == Some("on")).collect();
        for n in &stuck {
            let v = json!({"switch": n});
            match self.send_press(*n, false) {
                Ok(()) => self.logc("release", &v, "ok", &format!("virtual switch {}.{n} was on at start-up; released", self.vsw_bank), "startup"),
                Err(e) => self.logc("release", &v, "failed", &format!("send OFF: {e}"), "startup"),
            }
        }
        stuck
    }

    /// Pulse the virtual switch: fresh status → ON once → verify → OFF once after pulse_s → verify. The output's
    /// on-time is the controller's job; watch_off is only the safety net.
    async fn trigger(self: &Arc<Self>, g: &Trigger, client: &str) -> Reply {
        let tag = format!("trigger:{}", g.id);
        if let Err(e) = self.gate.check(DEVICE, &tag, false) {
            self.logc(&tag, &Value::Null, "blocked", &e.to_string(), client);
            return Reply::refused("switch control is disabled (tx_enabled is off)");
        }
        let (vsw, out) = (self.bank_state(g.bank, g.switch), self.bank_state(self.bank, g.channel));
        let (Some(vsw), Some(out)) = (vsw, out) else {
            self.logc(&tag, &Value::Null, "failed", "no fresh status for the virtual switch or its output", client);
            return Reply::refused("no fresh status from the load controller; nothing sent");
        };
        if out == "on" || vsw == "on" {
            self.logc(&tag, &Value::Null, "noop", "already active", client);
            return Reply::ok(format!("{}: already active", g.name));
        }
        let send_vsw = |on: bool| -> Result<(), NodeError> {
            let payload = switch_command(g.bank as i64, g.switch as i64, on).map_err(|e| NodeError::Encode(e.to_string()))?;
            self.node.send_fast(PGN_GROUP_FUNCTION, self.controller, &payload, self.next_seq(), 3)
        };
        if let Err(e) = send_vsw(true) {
            self.logc(&tag, &Value::Null, "failed", &format!("send ON: {e}"), client);
            return Reply::unverified("send failed (not retried)");
        }
        let started = now_s();
        let (me, b, s) = (self.clone(), g.bank, g.switch);
        let pressed = self.wait(move || me.bank_state(b, s) == Some("on"), 3.0).await;
        let rest = g.pulse_s - (now_s() - started);
        if rest > 0.0 {
            tokio::time::sleep(Duration::from_secs_f64(rest)).await;
        }
        if let Err(e) = send_vsw(false) {
            self.logc(&tag, &Value::Null, "failed", &format!("send OFF: {e}"), client);
        }
        {
            let (me, g2, c2) = (self.clone(), g.clone(), client.to_string());
            tokio::spawn(async move { me.watch_off(&g2, started, &c2).await });
        }
        if !pressed {
            self.logc(&tag, &Value::Null, "unverified", "virtual switch never reported ON", client);
            return Reply::unverified("the load controller did not confirm the virtual switch (not retried)");
        }
        let (me, bank, ch) = (self.clone(), self.bank, g.channel);
        let fired = self.wait(move || me.bank_state(bank, ch) == Some("on"), 2.0).await;
        self.logc(
            &tag,
            &Value::Null,
            if fired { "ok" } else { "unverified" },
            &format!("virtual switch {}.{} pulsed; ch{} {}", g.bank, g.switch, g.channel, if fired { "on" } else { "did not come on" }),
            client,
        );
        if fired {
            Reply::ok(format!("{}: on (the load controller turns it off)", g.name))
        } else {
            Reply::unverified(format!("virtual switch pressed, but channel {} did not come on (check the controller's logic)", g.channel))
        }
    }

    /// Safety net: the controller should switch the output off by itself within expect_off_s.
    async fn watch_off(&self, g: &Trigger, started: f64, client: &str) {
        while now_s() - started < g.expect_off_s {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let key = format!("switching.trigger.{}", g.id);
        if self.bank_state(self.bank, g.channel) != Some("on") {
            self.state.set(&key, json!({"ok": true, "t": aetherrv_core::num::round1(started)}));
            tracing::info!("trigger {}: ch{} off within {:.1}s (controller timer)", g.id, g.channel, now_s() - started);
            return;
        }
        self.logc(
            &format!("trigger:{}", g.id),
            &Value::Null,
            "safety-off",
            &format!("ch{} still on after {:.0}s: controller timer did not cut it off; switching it off directly", g.channel, g.expect_off_s),
            client,
        );
        self.state.set(
            &key,
            json!({"ok": false, "t": aetherrv_core::num::round1(started),
                   "detail": format!("{}: the load controller's timer did not switch channel {} off; AetherRV switched it off", g.name, g.channel)}),
        );
        if let Err(e) = self.send_direct(self.bank, g.channel, false) {
            tracing::error!("trigger {} safety-off send failed: {e}", g.id);
        }
    }

    /// Debounce the inputs and publish `switching.inputs`.
    pub fn tick(&self, now: f64) {
        let inputs = self.inputs.lock().unwrap().clone();
        let mut list = vec![];
        {
            let mut deb = self.deb.lock().unwrap();
            for (n, d) in deb.iter_mut() {
                let first = d.value.is_none();
                if d.settle(now) && !first {
                    let nm = inputs.get(n).map(|i| i.str("name", &n.to_string())).unwrap_or_else(|| n.to_string());
                    tracing::info!("input {nm}: {}", if d.value == Some(true) { "on" } else { "off" });
                }
            }
            let n2k = self.n2k.lock().unwrap();
            for (n, i) in &inputs {
                let volts = n2k.volts(self.controller, 15 + *n).and_then(|v| v.volts);
                list.push(json!({"input": n, "name": i.str("name", &format!("Input {n}")), "icon": i.str("icon", "toggle"),
                                 "on": deb.get(n).and_then(|d| d.value), "volts": volts}));
            }
        }
        self.state.set("switching.inputs", Value::Array(list));
    }

    /// Claim an address (when transmit is allowed), release stuck switches, then keep the inputs published.
    pub async fn run(self: Arc<Self>) {
        let mut backoff = 2.0;
        while !self.node.ready() {
            if self.gate.is_enabled(DEVICE)
                && !self.node.claimed()
                && let Err(e) = self.node.claim()
            {
                tracing::debug!("n2k address claim deferred: {e}");
                backoff = 2.0;
            }
            self.tick(now_s());
            let wait = if self.node.claimed() { 0.5 } else { backoff };
            tokio::time::sleep(Duration::from_secs_f64(wait)).await;
            if !self.gate.is_enabled(DEVICE) {
                break; // tx disabled: read-only (inputs still published)
            }
        }
        if self.node.ready() {
            let me = self.clone();
            tokio::spawn(async move {
                me.release_stuck(10.0).await;
            });
        }
        loop {
            self.tick(now_s());
            self.state.set("n2k.address", if self.node.ready() { json!(self.node.address()) } else { Value::Null });
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}
