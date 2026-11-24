//! N2K switching: the gated controller and keypad/input logic (tests/test_switching.py ported). The wire-format
//! tests of the same file are in proto-n2k (encode::tests). The controller reads a real proto-n2k decoder fed with
//! 127501 frames, and sends through a recording sink with TxGate device "n2k".

use aetherrv_can::{CanFrame, SendError};
use aetherrv_core::cmdlog::CommandLog;
use aetherrv_core::config::Cfg;
use aetherrv_core::state::{StateExt, StateStore, StateView};
use aetherrv_core::txgate::TxGate;
use aetherrv_n2k::node::{N2kNode, N2kSink};
use aetherrv_n2k::switching::{Debounced, SwitchController, Trigger};
use proto_n2k::adapter::N2kAdapter;
use proto_n2k::encode::{PGN_GROUP_FUNCTION, can_id, switch_command};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct FakeBus {
    gate: TxGate,
    sent: Mutex<Vec<CanFrame>>,
}

impl N2kSink for FakeBus {
    fn send(&self, f: &CanFrame, quiet: bool) -> Result<(), SendError> {
        self.gate.check("n2k", "test", quiet)?;
        self.sent.lock().unwrap().push(f.clone());
        Ok(())
    }
}

impl FakeBus {
    fn sent(&self) -> Vec<CanFrame> {
        self.sent.lock().unwrap().clone()
    }
    fn clear(&self) {
        self.sent.lock().unwrap().clear();
    }
    /// Reassembled fast-packet payloads (two frames each, as the Python helper).
    fn payloads(&self) -> Vec<Vec<u8>> {
        let s = self.sent();
        s.chunks(2).filter(|c| c.len() == 2).map(|c| [&c[0].data[2..], &c[1].data[1..]].concat()).collect()
    }
}

struct Rig {
    s: Arc<StateStore>,
    n2k: Arc<Mutex<N2kAdapter>>,
    node: Arc<N2kNode>,
    bus: Arc<FakeBus>,
    ctl: Arc<SwitchController>,
}

fn now() -> f64 {
    aetherrv_core::clock::now_s()
}

fn cfg() -> Value {
    json!({"switching": {"controller": 0, "bank": 32, "vsw_bank": 33, "press_s": 0.05},
           "switch": [{"bank": 32, "channel": 1, "name": "Spot lights", "press": 1},
                      {"bank": 32, "channel": 4, "name": "Reverse lights", "press": 4},
                      {"bank": 32, "channel": 7, "name": "Dump valve", "press": 9},
                      {"bank": 32, "channel": 9, "name": "Drawer locks", "press": 3, "blocked": true,
                       "blocked_reason": "drawer locks are blocked until the CLMD16 cut-off timer is back"},
                      {"bank": 32, "channel": 12, "name": "Cabin lights", "kind": "dimmer", "press": 12},
                      {"bank": 32, "channel": 16, "name": "Spare"}],
           "input": [{"input": 4, "name": "Reverse"}, {"input": 6, "name": "Dome", "debounce_s": 1.5}]})
}

fn setup_with(tx: bool, c: Value) -> Rig {
    let s = Arc::new(StateStore::new());
    let n2k = Arc::new(Mutex::new(N2kAdapter::new(&[], true).unwrap()));
    let gate = Arc::new(TxGate::new([("n2k".to_string(), tx)]));
    let bus = Arc::new(FakeBus { gate: TxGate::new([("n2k".to_string(), tx)]), sent: Mutex::new(vec![]) });
    let node = Arc::new(N2kNode::new(bus.clone(), 150, 0x0AE7));
    let ctl = SwitchController::new(s.clone(), n2k.clone(), node.clone(), gate, &Cfg(c), CommandLog::memory("n2k"));
    *ctl.verify.lock().unwrap() = 0.4;
    Rig { s, n2k, node, bus, ctl }
}

fn setup(tx: bool) -> Rig {
    setup_with(tx, cfg())
}

/// A 127501 from `src` for bank `inst` with channels `on` (1-based) on, `err` in error, at `t`.
fn status_full(n2k: &Mutex<N2kAdapter>, src: u8, inst: u8, on: &[u8], err: &[u8], t: f64) {
    let mut d = [0u8; 8];
    d[0] = inst;
    for ch in 1..=28u8 {
        let v: u8 = if err.contains(&ch) { 2 } else if on.contains(&ch) { 1 } else { 0 };
        let bit = 8 + (ch as usize - 1) * 2;
        d[bit / 8] |= v << (bit % 8);
    }
    let f = proto_n2k::Frame::extended(can_id(3, 127501, src, 255), &d);
    n2k.lock().unwrap().on_frame(&f, t);
}

fn status(n2k: &Mutex<N2kAdapter>, src: u8, inst: u8, on: &[u8]) {
    status_full(n2k, src, inst, on, &[], now());
}

fn claimed(node: &N2kNode) {
    node.claim().unwrap();
    node.settle_now();
}

fn ready_with(tx: bool, on: &[u8], vsw_on: &[u8]) -> Rig {
    let r = setup(tx);
    claimed(&r.node);
    r.bus.clear();
    status(&r.n2k, 0, 32, on);
    status(&r.n2k, 0, 33, vsw_on);
    r
}

fn ready() -> Rig {
    ready_with(true, &[], &[])
}

fn press(switch: u8, on: bool) -> Vec<u8> {
    switch_command(33, switch as i64, on).unwrap()
}

fn first10(p: &[Vec<u8>]) -> Vec<Vec<u8>> {
    p.iter().map(|x| x[..10].to_vec()).collect()
}

async fn cmd(r: &Rig, c: &str, v: Value) -> aetherrv_core::command::Reply {
    r.ctl.command(c, &v, "test").await
}

/// py: tests/test_switching.py::test_set_presses_the_virtual_switch_once_and_verifies
#[tokio::test]
async fn test_set_presses_the_virtual_switch_once_and_verifies() {
    let r = ready();
    let n2k = r.n2k.clone();
    tokio::spawn(async move {
        // its own logic switches ch1 on after the press
        tokio::time::sleep(Duration::from_millis(100)).await;
        status(&n2k, 0, 32, &[1]);
    });
    let rep = cmd(&r, "set", json!({"channel": 1, "on": true})).await;
    assert!(rep.is_ok(), "{rep:?}");
    assert_eq!(first10(&r.bus.payloads()), vec![press(1, true), press(1, false)], "ON, then OFF; nothing else");
    assert_eq!(r.bus.sent()[0].id, 0x0DED0096);
}

/// py: tests/test_switching.py::test_never_sends_to_the_breaker_bank
#[tokio::test]
async fn test_never_sends_to_the_breaker_bank() {
    let r = ready_with(true, &[12], &[]);
    for (c, v) in [
        ("set", json!({"channel": 1, "on": true})),
        ("set", json!({"channel": 12, "on": false})),
        ("set", json!({"channel": 7, "on": true})),
        ("dim", json!({"channel": 12, "level": 50})),
    ] {
        cmd(&r, c, v).await;
    }
    let p = r.bus.payloads();
    assert!(!p.is_empty());
    for x in &p {
        for ch in 1..=16 {
            for on in [true, false] {
                assert_ne!(x[..10].to_vec(), switch_command(32, ch, on).unwrap());
            }
        }
    }
}

/// py: tests/test_switching.py::test_dim_is_refused_for_now
#[tokio::test]
async fn test_dim_is_refused_for_now() {
    let r = ready_with(true, &[12], &[]);
    let rep = cmd(&r, "dim", json!({"channel": 12, "level": 50})).await;
    assert!(!rep.is_ok() && rep.detail.contains("Nothing was sent"));
    assert!(r.bus.sent().is_empty());
}

/// py: tests/test_switching.py::test_channel_without_a_virtual_switch_is_refused
#[tokio::test]
async fn test_channel_without_a_virtual_switch_is_refused() {
    let r = ready();
    let rep = cmd(&r, "set", json!({"channel": 16, "on": true})).await;
    assert!(!rep.is_ok() && rep.detail.contains("no virtual switch"));
    assert!(r.bus.sent().is_empty());
}

/// py: tests/test_switching.py::test_blocked_when_tx_disabled_and_nothing_sent
#[tokio::test]
async fn test_blocked_when_tx_disabled_and_nothing_sent() {
    let r = setup(false);
    status(&r.n2k, 0, 32, &[]);
    status(&r.n2k, 0, 33, &[]);
    let rep = cmd(&r, "set", json!({"channel": 1, "on": true})).await;
    assert!(!rep.is_ok() && r.bus.sent().is_empty());
    assert!(r.s.is_false("n2k.controls_ok"));
}

/// py: tests/test_switching.py::test_needs_address_claim_and_fresh_status
#[tokio::test]
async fn test_needs_address_claim_and_fresh_status() {
    let r = setup(true);
    status(&r.n2k, 0, 32, &[]);
    status(&r.n2k, 0, 33, &[]);
    assert!(!cmd(&r, "set", json!({"channel": 1, "on": true})).await.is_ok(), "no address yet");
    claimed(&r.node);
    r.bus.clear();
    status_full(&r.n2k, 0, 32, &[], &[], now() - 10.0); // stale breakers
    let rep = cmd(&r, "set", json!({"channel": 1, "on": true})).await;
    assert!(!rep.is_ok() && rep.detail.contains("fresh") && r.bus.sent().is_empty());
    status(&r.n2k, 0, 32, &[]);
    status_full(&r.n2k, 0, 33, &[], &[], now() - 10.0); // stale virtual switches
    let rep = cmd(&r, "set", json!({"channel": 1, "on": true})).await;
    assert!(!rep.is_ok() && rep.detail.contains("virtual switches") && r.bus.sent().is_empty());
}

/// py: tests/test_switching.py::test_blocked_channel_refuses_on
#[tokio::test]
async fn test_blocked_channel_refuses_on() {
    let r = ready();
    let rep = cmd(&r, "set", json!({"channel": 9, "on": true})).await;
    assert!(!rep.is_ok() && rep.detail.contains("cut-off") && r.bus.sent().is_empty());
}

/// py: tests/test_switching.py::test_not_switched_by_the_controller_is_reported_and_not_retried
#[tokio::test]
async fn test_not_switched_by_the_controller_is_reported_and_not_retried() {
    let r = ready();
    let rep = cmd(&r, "set", json!({"channel": 4, "on": true})).await; // the CLMD16's logic doesn't act
    assert!(!rep.is_ok() && rep.detail.contains("didn't switch"), "{rep:?}");
    assert_eq!(first10(&r.bus.payloads()), vec![press(4, true), press(4, false)]);
}

/// py: tests/test_switching.py::test_noop_when_already_in_state
#[tokio::test]
async fn test_noop_when_already_in_state() {
    let r = ready_with(true, &[1], &[]);
    let rep = cmd(&r, "set", json!({"channel": 1, "on": true})).await;
    assert!(rep.is_ok() && r.bus.sent().is_empty());
}

/// py: tests/test_switching.py::test_refuses_while_its_virtual_switch_is_already_on
#[tokio::test]
async fn test_refuses_while_its_virtual_switch_is_already_on() {
    let r = ready_with(true, &[], &[1]);
    let rep = cmd(&r, "set", json!({"channel": 1, "on": true})).await;
    assert!(!rep.is_ok() && rep.detail.contains("already pressed") && r.bus.sent().is_empty());
}

/// py: tests/test_switching.py::test_release_stuck_releases_only_our_switches_that_are_on
#[tokio::test]
async fn test_release_stuck_releases_only_our_switches_that_are_on() {
    let r = ready_with(true, &[], &[4, 5, 12]); // 5 isn't ours
    assert_eq!(r.ctl.owned_switches(), vec![1, 3, 4, 9, 12]);
    let stuck = r.ctl.release_stuck(0.2).await;
    assert_eq!(stuck, vec![4, 12]);
    assert_eq!(first10(&r.bus.payloads()), vec![press(4, false), press(12, false)]);
}

/// py: tests/test_switching.py::test_release_stuck_skips_without_status
#[tokio::test]
async fn test_release_stuck_skips_without_status() {
    let r = setup(true);
    claimed(&r.node);
    r.bus.clear();
    assert!(r.ctl.release_stuck(0.2).await.is_empty() && r.bus.sent().is_empty());
}

/// py: tests/test_switching.py::test_keypad_presses_never_send
#[tokio::test]
async fn test_keypad_presses_never_send() {
    let r = setup_with(true, json!({"switching": {"controller": 0, "bank": 32, "automation": true},
                                     "binding": [{"keypad": 101, "button": 9, "channels": [7, 16]}]}));
    claimed(&r.node);
    r.bus.clear();
    status(&r.n2k, 0, 32, &[]);
    for on in [&[][..], &[9], &[], &[9], &[]] {
        status(&r.n2k, 101, 50, on);
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(r.bus.sent().is_empty(), "CKM12 presses are the CLMD16's job (dual control)");
}

/// py: tests/test_switching.py::test_inputs_are_read_but_never_switch_anything
#[tokio::test]
async fn test_inputs_are_read_but_never_switch_anything() {
    let r = setup_with(true, json!({
        "switching": {"controller": 0, "bank": 32, "automation": true},
        "switch": [{"bank": 32, "channel": 1, "name": "Spot", "requires_input": 2}],
        "input": [{"input": 2, "name": "Ignition"}, {"input": 4, "name": "Reverse", "follow": [4]}]}));
    claimed(&r.node);
    r.bus.clear();
    let t0 = now();
    status_full(&r.n2k, 0, 32, &[18, 20, 1], &[], t0); // ignition + reverse on, spot on
    r.ctl.tick(t0 + 1.0);
    status_full(&r.n2k, 0, 32, &[1], &[], t0 + 2.0); // ignition and reverse go off
    r.ctl.tick(t0 + 2.5);
    r.ctl.tick(t0 + 3.0);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(r.bus.sent().is_empty());
    let inputs: Vec<(String, Value)> = r.s.list("switching.inputs").iter().map(|i| (i["name"].as_str().unwrap().to_string(), i["on"].clone())).collect();
    assert_eq!(inputs, vec![("Ignition".to_string(), json!(false)), ("Reverse".to_string(), json!(false))]);
    assert!(r.s.is_false("switching.automation"));
}

/// py: tests/test_switching.py::test_address_claim_moves_off_a_contested_address
#[tokio::test]
async fn test_address_claim_moves_off_a_contested_address() {
    let r = setup(true);
    r.node.claim().unwrap();
    assert_eq!(r.node.address(), 150);
    assert_eq!(r.bus.sent().last().unwrap().id, 0x18EEFF96);
    let lower = (r.node.name - 1).to_le_bytes(); // a device with a lower (stronger) NAME claims 150
    r.node.on_frame(&CanFrame::ext(0x18EEFF96, &lower));
    assert_ne!(r.node.address(), 150);
    assert_eq!(r.bus.sent().last().unwrap().id, 0x18EEFF00 | r.node.address() as u32);
}

/// py: tests/test_switching.py::test_tripped_channel_refuses_on_and_off_clears_it_with_one_direct_off
#[tokio::test]
async fn test_tripped_channel_refuses_on_and_off_clears_it_with_one_direct_off() {
    let r = ready();
    status_full(&r.n2k, 0, 32, &[], &[4], now()); // ch4 tripped
    let rep = cmd(&r, "set", json!({"channel": 4, "on": true})).await;
    assert!(!rep.is_ok() && rep.detail.contains("tripped") && r.bus.sent().is_empty());
    *r.ctl.verify.lock().unwrap() = 0.3;
    let rep = cmd(&r, "set", json!({"channel": 4, "on": false})).await; // still tripped: one OFF sent, reported
    assert!(!rep.is_ok() && rep.detail.contains("still reports a fault"), "{rep:?}");
    let sent = r.bus.sent();
    let seqs: std::collections::BTreeSet<u8> = sent.iter().map(|m| m.data[0] >> 5).collect();
    assert_eq!(seqs.len(), 1, "one message (its fast-packet frames)");
    assert!(sent.iter().all(|m| ((m.id >> 8) & 0x3FFFF) >> 8 == PGN_GROUP_FUNCTION >> 8));
    assert_eq!(r.bus.payloads()[0][..10].to_vec(), switch_command(32, 4, false).unwrap(), "the one direct OFF");
    r.bus.clear();
    let n2k = r.n2k.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        status(&n2k, 0, 32, &[]);
    });
    let rep = cmd(&r, "set", json!({"channel": 4, "on": false})).await;
    assert!(rep.is_ok() && rep.detail.contains("cleared") && !r.bus.sent().is_empty(), "{rep:?}");
}

/// py: tests/test_switching.py::test_ignition_interlock_refuses_on_until_ignition_and_explains_a_refusal
#[tokio::test]
async fn test_ignition_interlock_refuses_on_until_ignition_and_explains_a_refusal() {
    let r = ready();
    {
        let mut ch = r.ctl.channels.lock().unwrap();
        let mut c = ch.get(&1).unwrap().0.clone();
        c["requires_input"] = json!(2);
        ch.insert(1, Cfg(c));
    }
    r.ctl.inputs.lock().unwrap().insert(2, Cfg(json!({"input": 2, "name": "Ignition"})));
    r.ctl.deb.lock().unwrap().insert(2, Debounced::default());
    let rep = cmd(&r, "set", json!({"channel": 1, "on": true})).await;
    assert!(!rep.is_ok() && rep.detail.contains("Ignition") && r.bus.sent().is_empty());
    r.ctl.deb.lock().unwrap().get_mut(&2).unwrap().value = Some(true);
    let rep = cmd(&r, "set", json!({"channel": 1, "on": true})).await; // pressed; the fake CLMD16 doesn't act
    assert!(!rep.is_ok() && r.bus.payloads().len() == 2);
}

fn setup_trigger() -> Rig {
    let r = setup(true);
    r.ctl.triggers.lock().unwrap().insert(
        "drawer_locks".into(),
        Trigger { id: "drawer_locks".into(), name: "Drawer locks".into(), bank: 33, switch: 1, channel: 11, pulse_s: 0.2, expect_off_s: 0.8 },
    );
    claimed(&r.node);
    r.bus.clear();
    r
}

/// py: tests/test_switching.py::test_trigger_pulses_virtual_switch_and_controller_timer_cuts_off
#[tokio::test]
async fn test_trigger_pulses_virtual_switch_and_controller_timer_cuts_off() {
    let r = setup_trigger();
    status(&r.n2k, 0, 32, &[]);
    status(&r.n2k, 0, 33, &[]);
    let n2k = r.n2k.clone();
    tokio::spawn(async move {
        // the controller: vsw ON → ch11 on; then its timer turns ch11 off
        tokio::time::sleep(Duration::from_millis(50)).await;
        status(&n2k, 0, 33, &[1]);
        status(&n2k, 0, 32, &[11]);
        tokio::time::sleep(Duration::from_millis(300)).await;
        status(&n2k, 0, 33, &[]);
        tokio::time::sleep(Duration::from_millis(200)).await;
        status(&n2k, 0, 32, &[]);
    });
    let rep = cmd(&r, "trigger", json!({"trigger": "drawer_locks"})).await;
    assert!(rep.is_ok(), "{rep:?}");
    let p = r.bus.payloads();
    assert_eq!(p[0][..10].to_vec(), switch_command(33, 1, true).unwrap());
    assert_eq!(p[1][..10].to_vec(), switch_command(33, 1, false).unwrap());
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(r.bus.payloads().len(), 2, "no safety-off needed");
    assert_eq!(r.s.get("switching.trigger.drawer_locks").unwrap()["ok"], true);
}

/// py: tests/test_switching.py::test_trigger_safety_net_switches_output_off_if_controller_does_not
#[tokio::test]
async fn test_trigger_safety_net_switches_output_off_if_controller_does_not() {
    let r = setup_trigger();
    status(&r.n2k, 0, 32, &[]);
    status(&r.n2k, 0, 33, &[]);
    let n2k = r.n2k.clone();
    tokio::spawn(async move {
        // vsw works, ch11 comes on, but never goes off
        tokio::time::sleep(Duration::from_millis(50)).await;
        status(&n2k, 0, 33, &[1]);
        status(&n2k, 0, 32, &[11]);
        for _ in 0..12 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            status(&n2k, 0, 32, &[11]);
            status(&n2k, 0, 33, &[]);
        }
    });
    cmd(&r, "trigger", json!({"trigger": "drawer_locks"})).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let p = r.bus.payloads();
    assert_eq!(p.last().unwrap()[..10].to_vec(), switch_command(32, 11, false).unwrap(), "safety net: direct OFF to the output");
    assert_eq!(r.s.get("switching.trigger.drawer_locks").unwrap()["ok"], false);
}

/// py: tests/test_switching.py::test_plain_on_to_trigger_channel_stays_blocked
#[tokio::test]
async fn test_plain_on_to_trigger_channel_stays_blocked() {
    let r = setup_trigger();
    r.ctl.channels.lock().unwrap().insert(11, Cfg(json!({"channel": 11, "name": "Drawer locks", "blocked": true, "blocked_reason": "use the trigger"})));
    status(&r.n2k, 0, 32, &[]);
    status(&r.n2k, 0, 33, &[]);
    let rep = cmd(&r, "set", json!({"channel": 11, "on": true})).await;
    assert!(!rep.is_ok() && r.bus.sent().is_empty());
}

/// py: tests/test_switching.py::test_claim_poll_reply_is_sent_but_logged_quietly (the reply is sent with the
/// quiet flag, which logs it at DEBUG)
#[tokio::test]
async fn test_claim_poll_reply_is_sent_but_logged_quietly() {
    struct Q(Mutex<Vec<bool>>);
    impl N2kSink for Q {
        fn send(&self, _f: &CanFrame, quiet: bool) -> Result<(), SendError> {
            self.0.lock().unwrap().push(quiet);
            Ok(())
        }
    }
    let q = Arc::new(Q(Mutex::new(vec![])));
    let node = N2kNode::new(q.clone(), 150, 0x0AE7);
    node.claim().unwrap(); // first claim: visible
    let req = CanFrame::ext(can_id(6, 59904, 228, 150), &60928u32.to_le_bytes()[..3]);
    node.on_frame(&req); // the Cerbo's 10-s poll
    assert_eq!(*q.0.lock().unwrap(), vec![false, true], "still answered, but quietly");
}

/// D21 (found by the bench soak): answering the Cerbo's poll keeps the address ready. switching.py restarted the
/// 250 ms claim wait on every reply, so a tap within 250 ms after a poll was refused as "not claimed yet".
#[tokio::test]
async fn test_answering_a_claim_poll_keeps_the_address_ready() {
    let r = ready();
    assert!(r.node.ready());
    let req = CanFrame::ext(can_id(6, 59904, 228, 150), &60928u32.to_le_bytes()[..3]);
    r.node.on_frame(&req); // the Cerbo's 10-s poll, answered
    assert_eq!(r.bus.sent().len(), 1, "the poll is still answered");
    assert!(r.node.ready(), "a poll reply is not a new claim");
    r.bus.clear();
    let rep = cmd(&r, "set", json!({"channel": 7, "on": true})).await;
    assert!(!rep.detail.contains("not claimed"), "{rep:?}");
    assert_eq!(first10(&r.bus.payloads()), vec![press(9, true), press(9, false)], "sent at once");
    // a contest by a lower-priority NAME still restarts the wait
    let theirs = r.node.name + 1;
    r.node.on_frame(&CanFrame::ext(can_id(6, 60928, 150, 255), &theirs.to_le_bytes()));
    assert!(!r.node.ready(), "a re-assert after a contest waits 250 ms again");
}

/// py: tests/test_switching.py::test_hold_puts_the_virtual_switch_on_until_release
#[tokio::test]
async fn test_hold_puts_the_virtual_switch_on_until_release() {
    let r = ready_with(true, &[12], &[]);
    let rep = cmd(&r, "hold", json!({"channel": 12, "on": true})).await;
    assert!(rep.is_ok(), "{rep:?}");
    assert_eq!(first10(&r.bus.payloads()), vec![press(12, true)]);
    status(&r.n2k, 0, 33, &[12]);
    let r2 = cmd(&r, "hold", json!({"channel": 12, "on": true})).await;
    assert!(!r2.is_ok() && r.bus.sent().len() == 2, "a second hold sends nothing");
    let rep = cmd(&r, "hold", json!({"channel": 12, "on": false})).await;
    assert!(rep.is_ok(), "{rep:?}");
    assert_eq!(first10(&r.bus.payloads()), vec![press(12, true), press(12, false)]);
    assert!(!r.ctl.holding());
}

/// py: tests/test_switching.py::test_hold_is_released_after_max_hold
#[tokio::test]
async fn test_hold_is_released_after_max_hold() {
    let r = ready();
    *r.ctl.max_hold_s.lock().unwrap() = 0.1;
    assert!(cmd(&r, "hold", json!({"channel": 12, "on": true})).await.is_ok());
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(first10(&r.bus.payloads()), vec![press(12, true), press(12, false)]);
    assert!(!r.ctl.holding());
}

/// py: tests/test_switching.py::test_hold_only_on_dimmers_and_never_to_the_breaker_bank
#[tokio::test]
async fn test_hold_only_on_dimmers_and_never_to_the_breaker_bank() {
    let r = ready();
    let rep = cmd(&r, "hold", json!({"channel": 1, "on": true})).await;
    assert!(!rep.is_ok() && r.bus.sent().is_empty());
    cmd(&r, "hold", json!({"channel": 12, "on": true})).await;
    cmd(&r, "hold", json!({"channel": 12, "on": false})).await;
    for p in r.bus.payloads() {
        for on in [true, false] {
            assert_ne!(p[..10].to_vec(), switch_command(32, 12, on).unwrap());
        }
    }
}

/// py: tests/test_switching.py::test_hold_needs_fresh_status_and_a_released_switch
#[tokio::test]
async fn test_hold_needs_fresh_status_and_a_released_switch() {
    let r = ready_with(true, &[], &[12]);
    let rep = cmd(&r, "hold", json!({"channel": 12, "on": true})).await;
    assert!(!rep.is_ok() && r.bus.sent().is_empty());
    let r = setup(true);
    claimed(&r.node);
    r.bus.clear();
    let rep = cmd(&r, "hold", json!({"channel": 12, "on": true})).await;
    assert!(!rep.is_ok() && r.bus.sent().is_empty());
}

/// py: tests/test_switching.py::test_hold_blocked_when_tx_disabled
#[tokio::test]
async fn test_hold_blocked_when_tx_disabled() {
    let r = setup(false);
    status(&r.n2k, 0, 32, &[]);
    status(&r.n2k, 0, 33, &[]);
    let rep = cmd(&r, "hold", json!({"channel": 12, "on": true})).await;
    assert!(!rep.is_ok() && r.bus.sent().is_empty());
}

/// py: tests/test_switching.py::test_a_quick_hold_still_makes_a_full_length_press
#[tokio::test]
async fn test_a_quick_hold_still_makes_a_full_length_press() {
    let r = ready();
    *r.ctl.press_s.lock().unwrap() = 0.2;
    cmd(&r, "hold", json!({"channel": 12, "on": true})).await;
    let t0 = Instant::now();
    cmd(&r, "hold", json!({"channel": 12, "on": false})).await;
    assert!(t0.elapsed().as_secs_f64() >= 0.15);
    assert_eq!(first10(&r.bus.payloads()), vec![press(12, true), press(12, false)]);
}

/// py: tests/test_switching.py::test_off_refused_while_the_input_holds_it_on
#[tokio::test]
async fn test_off_refused_while_the_input_holds_it_on() {
    let r = ready_with(true, &[10], &[]);
    r.ctl.channels.lock().unwrap().insert(10, Cfg(json!({"channel": 10, "name": "Stereo", "press": 10, "on_with_input": 4})));
    r.ctl.deb.lock().unwrap().get_mut(&4).unwrap().value = Some(true);
    let rep = cmd(&r, "set", json!({"channel": 10, "on": false})).await;
    assert!(!rep.is_ok() && rep.detail.contains("stays on while Reverse is on") && r.bus.sent().is_empty(), "{rep:?}");
    r.ctl.deb.lock().unwrap().get_mut(&4).unwrap().value = Some(false);
    let n2k = r.n2k.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        status(&n2k, 0, 32, &[]);
    });
    let rep = cmd(&r, "set", json!({"channel": 10, "on": false})).await;
    assert!(rep.is_ok(), "{rep:?}");
}

/// Safety-relevant value parsing: an on/off that isn't a JSON boolean is refused, nothing sent (D11).
#[tokio::test]
async fn non_boolean_on_is_refused() {
    let r = ready();
    for v in [json!("false"), json!(1), json!(null)] {
        let rep = cmd(&r, "set", json!({"channel": 1, "on": v})).await;
        assert!(!rep.is_ok() && rep.detail == "bad request", "{rep:?}");
    }
    assert!(r.bus.sent().is_empty());
}
