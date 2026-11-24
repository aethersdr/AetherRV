//! The connection rules of `src/aetherrv/adapters/epoch_ble.py` (`EpochBleAdapter`), against a mock BlueZ. The
//! Python file has no adapter tests (its codec tests are ported in `crates/proto-roypow`); each test here cites the
//! Python lines whose behaviour it pins. Time is tokio's paused clock, so the 20 s first contacts and the 30 s
//! backoffs run instantly.

use aetherrv_batteries_ble::{Batteries, Ble, BleError, Link, Settings};
use aetherrv_core::config::Cfg;
use aetherrv_core::state::{StateExt, StateStore, StateView};
use proto_roypow::{Battery, query};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

/// Notification chunks as a battery sent them (`crates/proto-roypow/tests/test_epoch_ble.rs`).
const NOTES: [&str; 6] = [
    "ead10139ff040164020027030007040 4e00500070 6223e070006 08dffd091198 0affff0b000500d4",
    "0c2611040819470532 0cff0cfc0d14ce0080000f",
    "f5",
    "ead1011aff033102450000000004333237 3c0003",
    "000000128700000008f5",
    "ead1010fff020404040cfd0cfc0cfc0cfff4f5",
];

fn hex(s: &str) -> Vec<u8> {
    let s: String = s.chars().filter(|c| *c != ' ').collect();
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn reply(cmd: u8) -> Vec<Vec<u8>> {
    let idx: &[usize] = match cmd {
        0x04 => &[0, 1, 2],
        0x03 => &[3, 4],
        0x02 => &[5],
        _ => &[],
    };
    idx.iter().map(|i| hex(NOTES[*i])).collect()
}

#[derive(Debug, Clone, PartialEq)]
enum Ev {
    Find,
    Connect,
    ConnectFailed,
    Notify,
    NotifyFailed,
    Write(u8),
    /// `Link::disconnect` (the session's own link).
    Disconnect,
    /// `Ble::disconnect(addr)` (stale-link clearing, or after a failed connect).
    DisconnectAddr,
}

/// One simulated battery and what BlueZ knows about it.
#[derive(Default)]
struct Dev {
    advertising: bool,
    /// First contact fails: bluetoothd's 2a04 read is never answered unless the values are cached (30 s window).
    quirk: bool,
    cached_until: Option<Instant>,
    /// start_notify failures still to come, with this message.
    notify_fails: u32,
    notify_msg: String,
    /// Commands the battery doesn't answer.
    silent: Vec<u8>,
    /// Drop the link instead of answering the next write.
    drop_on_write: bool,
    lost: Option<watch::Sender<bool>>,
    notes: Option<mpsc::UnboundedSender<Vec<u8>>>,
}

struct Mock {
    devs: Mutex<HashMap<String, Dev>>,
    log: Mutex<Vec<(f64, String, Ev)>>,
    inflight: Mutex<(u32, u32)>,
    t0: Instant,
}

impl Mock {
    fn new(devs: Vec<(&str, Dev)>) -> Arc<Mock> {
        Arc::new(Mock {
            devs: Mutex::new(devs.into_iter().map(|(a, d)| (a.to_string(), d)).collect()),
            log: Mutex::new(Vec::new()),
            inflight: Mutex::new((0, 0)),
            t0: Instant::now(),
        })
    }
    fn ev(&self, addr: &str, e: Ev) {
        self.log.lock().unwrap().push((self.t0.elapsed().as_secs_f64(), addr.to_string(), e));
    }
    fn events(&self, addr: &str) -> Vec<(f64, Ev)> {
        self.log.lock().unwrap().iter().filter(|(_, a, _)| a == addr).map(|(t, _, e)| (*t, e.clone())).collect()
    }
    fn kinds(&self, addr: &str) -> Vec<Ev> {
        self.events(addr).into_iter().map(|(_, e)| e).collect()
    }
    fn times(&self, addr: &str, e: &Ev) -> Vec<f64> {
        self.events(addr).into_iter().filter(|(_, x)| x == e).map(|(t, _)| t).collect()
    }
    fn with(&self, addr: &str, f: impl FnOnce(&mut Dev)) {
        f(self.devs.lock().unwrap().get_mut(addr).unwrap());
    }
    /// The battery drops the link (out of range, the module resets).
    fn drop_link(&self, addr: &str) {
        self.with(addr, |d| {
            if let Some(l) = d.lost.take() {
                let _ = l.send(true);
            }
            d.notes = None;
            d.cached_until = Some(Instant::now() + Duration::from_secs(30));
        });
    }
}

struct M(Arc<Mock>);

impl Ble for M {
    type Link = MockLink;

    async fn find(&self, addr: &str, timeout: Duration) -> Result<bool, BleError> {
        self.0.ev(addr, Ev::Find);
        let adv = self.0.devs.lock().unwrap().get(addr).is_some_and(|d| d.advertising);
        if adv {
            tokio::time::sleep(Duration::from_millis(500)).await;
            Ok(true)
        } else {
            tokio::time::sleep(timeout).await;
            Ok(false)
        }
    }

    async fn connect(&self, addr: &str, timeout: Duration) -> Result<MockLink, BleError> {
        self.0.ev(addr, Ev::Connect);
        {
            let mut f = self.0.inflight.lock().unwrap();
            f.0 += 1;
            f.1 = f.1.max(f.0);
        }
        let first_contact = {
            let d = self.0.devs.lock().unwrap();
            let d = &d[addr];
            d.quirk && d.cached_until.is_none_or(|t| Instant::now() >= t)
        };
        let r = if first_contact {
            // the 2a04 read is never answered: bleak gives up at its timeout; BlueZ has the values cached afterwards
            tokio::time::sleep(timeout).await;
            self.0.with(addr, |d| d.cached_until = Some(Instant::now() + Duration::from_secs(30)));
            self.0.ev(addr, Ev::ConnectFailed);
            Err(BleError::timeout(""))
        } else {
            tokio::time::sleep(Duration::from_millis(1800)).await;
            let (tx, rx) = watch::channel(false);
            self.0.with(addr, |d| d.lost = Some(tx));
            Ok(MockLink { m: self.0.clone(), addr: addr.to_string(), lost: rx })
        };
        self.0.inflight.lock().unwrap().0 -= 1;
        r
    }

    async fn disconnect(&self, addr: &str) {
        self.0.ev(addr, Ev::DisconnectAddr);
    }
}

struct MockLink {
    m: Arc<Mock>,
    addr: String,
    lost: watch::Receiver<bool>,
}

impl Link for MockLink {
    async fn start_notify(&mut self) -> Result<mpsc::UnboundedReceiver<Vec<u8>>, BleError> {
        let mut fail = None;
        self.m.with(&self.addr, |d| {
            if d.notify_fails > 0 {
                d.notify_fails -= 1;
                fail = Some(d.notify_msg.clone());
            }
        });
        if let Some(msg) = fail {
            self.m.ev(&self.addr, Ev::NotifyFailed);
            return Err(BleError::bluez(msg));
        }
        self.m.ev(&self.addr, Ev::Notify);
        let (tx, rx) = mpsc::unbounded_channel();
        self.m.with(&self.addr, |d| d.notes = Some(tx));
        Ok(rx)
    }

    async fn write(&mut self, data: &[u8]) -> Result<(), BleError> {
        let cmd = data[5];
        assert!([query(0x04), query(0x03), query(0x02)].iter().any(|q| q[..] == *data), "only the three read queries are written");
        self.m.ev(&self.addr, Ev::Write(cmd));
        let (drop, silent) = {
            let d = self.m.devs.lock().unwrap();
            (d[&self.addr].drop_on_write, d[&self.addr].silent.contains(&cmd))
        };
        if drop {
            self.m.with(&self.addr, |d| d.drop_on_write = false);
            self.m.drop_link(&self.addr);
        } else if !silent {
            let tx = self.m.devs.lock().unwrap()[&self.addr].notes.clone();
            if let Some(tx) = tx {
                for n in reply(cmd) {
                    let _ = tx.send(n);
                }
            }
        }
        Ok(())
    }

    fn lost(&self) -> watch::Receiver<bool> {
        self.lost.clone()
    }

    async fn disconnect(&mut self) {
        self.m.ev(&self.addr, Ev::Disconnect);
        self.m.drop_link(&self.addr);
    }
}

const A: &str = "AA:00:00:00:00:01";
const B: &str = "AA:00:00:00:00:02";
const C: &str = "AA:00:00:00:00:03";
const D: &str = "AA:00:00:00:00:04";

fn good() -> Dev {
    Dev { advertising: true, ..Default::default() }
}

fn quirky() -> Dev {
    Dev { advertising: true, quirk: true, ..Default::default() }
}

fn service(mock: &Arc<Mock>, addrs: &[&str], period: f64) -> Arc<Batteries<M>> {
    let batteries = addrs.iter().enumerate().map(|(i, a)| Battery::new(Some(&(i + 1).to_string()), a, None)).collect();
    let base = aetherrv_core::clock::now_s();
    let t0 = Instant::now();
    Batteries::with_clock(
        M(mock.clone()),
        Settings { batteries, period },
        Arc::new(StateStore::new()),
        Box::new(move || base + t0.elapsed().as_secs_f64()),
    )
}

async fn run_for(s: f64) {
    tokio::time::sleep(Duration::from_secs_f64(s)).await;
}

fn row(svc: &Batteries<M>, i: usize) -> Value {
    svc.publish();
    svc.state.list("batteries.ble")[i].clone()
}

/// Lines 167-194 (`_open`) and INTEGRATION_PLAN.md §40: the first contact fails (bleak's 20 s timeout on the
/// unanswered 2a04 read); 1 s later, inside BlueZ's 30 s cache window, the reconnect works. The device is not
/// rescanned between the attempts, and the failed client is disconnected.
#[tokio::test(start_paused = true)]
async fn first_contact_fails_then_the_immediate_reconnect_works() {
    let mock = Mock::new(vec![(A, quirky())]);
    let svc = service(&mock, &[A], 1.0);
    tokio::spawn(svc.clone().battery(0));
    run_for(30.0).await;
    let k = mock.kinds(A);
    assert_eq!(k[..6], [Ev::Find, Ev::Connect, Ev::ConnectFailed, Ev::DisconnectAddr, Ev::Connect, Ev::Notify]);
    assert_eq!(k.iter().filter(|e| **e == Ev::Find).count(), 1, "no rescan between attempts");
    let c = mock.times(A, &Ev::Connect);
    assert!((c[1] - c[0] - 21.0).abs() < 0.01, "20 s timeout + 1 s: {c:?}");
    let r = row(&svc, 0);
    assert_eq!((r["connected"].clone(), r["ok"].clone(), r["error"].clone()), (json!(true), json!(true), Value::Null));
    assert_eq!(r["soc"], json!(100));
}

/// Lines 148 and 202-203: connecting is serialized across all batteries (one lock), so four quirky batteries
/// connect one after the other, each with its own failed first contact (~23 s each here; ~2.5 min in the van).
#[tokio::test(start_paused = true)]
async fn connects_are_serialized() {
    let mock = Mock::new(vec![(A, quirky()), (B, quirky()), (C, quirky()), (D, quirky())]);
    let svc = service(&mock, &[A, B, C, D], 1.0);
    for i in 0..4 {
        tokio::spawn(svc.clone().battery(i));
    }
    run_for(120.0).await;
    assert_eq!(mock.inflight.lock().unwrap().1, 1, "never two connects in flight");
    for a in [A, B, C, D] {
        assert_eq!(mock.kinds(a).iter().filter(|e| **e == Ev::ConnectFailed).count(), 1, "{a}");
        assert!(svc.rec(a).unwrap().record.connected, "{a}");
    }
    // each battery's open (scan, failed first contact, retry, subscribe) finishes before the next one starts
    let notify_a = mock.times(A, &Ev::Notify)[0];
    let find_b = mock.times(B, &Ev::Find)[0];
    assert!(find_b >= notify_a, "B scanned at {find_b}, A subscribed at {notify_a}");
    svc.publish();
    assert_eq!(svc.state.get("batteries.ble.reporting"), Some(json!(4)));
    assert_eq!(svc.state.num("batteries.ble.current"), Some(-23.24));
}

/// Lines 175-177, 241-246: not advertising ends the session at once (no connect); the error says so; the next
/// scan follows after 2, 4, 8, 16, 30, 30 s (each scan itself takes the full 12 s).
#[tokio::test(start_paused = true)]
async fn not_advertising_aborts_at_once_and_backs_off_to_30s() {
    let mock = Mock::new(vec![(A, Dev::default())]);
    let svc = service(&mock, &[A], 1.0);
    tokio::spawn(svc.clone().battery(0));
    run_for(200.0).await;
    assert!(!mock.kinds(A).contains(&Ev::Connect));
    let f = mock.times(A, &Ev::Find);
    let gaps: Vec<f64> = f.windows(2).map(|w| (w[1] - w[0] - 12.0).round()).collect();
    assert_eq!(gaps[..6], [2.0, 4.0, 8.0, 16.0, 30.0, 30.0]);
    assert_eq!(svc.rec(A).unwrap().record.error.as_deref(), Some("ConnectionError: not advertising (another device connected?)"));
    let r = row(&svc, 0);
    assert_eq!((r["ok"].clone(), r["age"].clone(), r["connected"].clone()), (json!(false), Value::Null, json!(false)));
}

/// Lines 174-194, 244: a subscription that keeps failing is tried 3 times, 1 s apart, each failed client
/// disconnected, then the session ends with the last error; the device cache is dropped, so the next session
/// rescans first. (Error text: lines 241, `"<Type>: <message>"` cut at 120 characters.)
#[tokio::test(start_paused = true)]
async fn three_failed_opens_end_with_the_last_error() {
    let long = "Unlikely Error ".repeat(12);
    let mock = Mock::new(vec![(A, Dev { advertising: true, notify_fails: 5, notify_msg: long.clone(), ..Default::default() })]);
    let svc = service(&mock, &[A], 1.0);
    tokio::spawn(svc.clone().battery(0));
    run_for(12.0).await; // scan 0.5, then 3 × (connect 1.8 + settle 0.8 + 1 s): the session ends at 11.3 s
    let k = mock.kinds(A);
    assert_eq!(k.len(), 10, "{k:?}");
    let open3 = [Ev::Connect, Ev::NotifyFailed, Ev::Disconnect];
    assert_eq!(k[0], Ev::Find);
    assert_eq!(k[1..10], [open3.clone(), open3.clone(), open3.clone()].concat());
    let err = svc.rec(A).unwrap().record.error.unwrap();
    assert_eq!(err.chars().count(), 120);
    assert!(err.starts_with("BlueZError: Unlikely Error"));
    let c = mock.times(A, &Ev::Connect);
    assert!(((c[1] - c[0]) - (1.8 + 0.8 + 1.0)).abs() < 0.01, "connect + settle + 1 s: {c:?}");
    // the next session (after the 2 s backoff) rescans, fails twice more, then subscribes; a good read clears the error
    run_for(20.0).await;
    let k = mock.kinds(A);
    assert_eq!(k[10], Ev::Find, "rescanned after the failed session");
    assert!(k.contains(&Ev::Notify));
    assert_eq!(row(&svc, 0)["error"], Value::Null, "line 220: a good read clears the error");
}

/// Lines 210-213 and the module docstring: the polls send 0x04 + 0x03 every period and 0x02 every 5th poll
/// (and nothing but those three read queries: the mock asserts it on every write), one period apart.
#[tokio::test(start_paused = true)]
async fn poll_schedule_and_only_read_queries() {
    let mock = Mock::new(vec![(A, good())]);
    let svc = service(&mock, &[A], 1.0);
    tokio::spawn(svc.clone().battery(0));
    run_for(2.3 + 6.5).await; // scan 0.5 + connect 1.8 → polls at 3.1, 4.1, …
    let writes: Vec<u8> = mock.kinds(A).into_iter().filter_map(|e| if let Ev::Write(c) = e { Some(c) } else { None }).collect();
    assert_eq!(writes[..14], [4, 3, 2, 4, 3, 4, 3, 4, 3, 4, 3, 4, 3, 2]);
    let w4 = mock.times(A, &Ev::Write(4));
    assert!(w4.windows(2).all(|w| (w[1] - w[0] - 1.0).abs() < 0.01), "{w4:?}");
    // interval 0.2 → the 0.5 s floor (line 146)
    let c = Cfg::parse("[device.epoch]\nkind = \"epoch_ble\"\nenabled = true\ninterval = 0.2\n[[device.epoch.battery]]\naddress = \"AA:00:00:00:00:01\"").unwrap();
    assert_eq!(Settings::from_config(&c).unwrap().unwrap().period, 0.5);
}

/// Lines 214-218 and 225-230: a missed reply (3 s) ends the session with `TimeoutError: no reply to 0x..`, the
/// link is disconnected and `connected` cleared; the last reading stays until it is 30 s old (then not `ok`).
#[tokio::test(start_paused = true)]
async fn missed_reply_drops_the_link_and_reconnects() {
    let mock = Mock::new(vec![(A, good())]);
    let svc = service(&mock, &[A], 1.0);
    tokio::spawn(svc.clone().battery(0));
    run_for(5.0).await;
    let t_good = svc.rec(A).unwrap().record.t.unwrap();
    mock.with(A, |d| d.silent = vec![0x03]);
    run_for(1.0).await;
    let w3 = *mock.times(A, &Ev::Write(3)).last().unwrap();
    run_for(3.1).await;
    let ev = mock.events(A);
    let disc = ev.iter().find(|(t, e)| *e == Ev::Disconnect && *t > w3).unwrap().0;
    assert!((disc - w3 - 3.0).abs() < 0.01, "dropped 3 s after the unanswered query");
    let rec = svc.rec(A).unwrap();
    assert_eq!(rec.record.error.as_deref(), Some("TimeoutError: no reply to 0x03"));
    assert!(!rec.record.connected);
    assert!(rec.record.t.unwrap() - t_good < 1.5, "no new reading");
    assert_eq!(row(&svc, 0)["ok"], json!(true), "still fresh");
    run_for(30.0).await; // it rescans (the cache was dropped) and reconnects, but 0x03 stays silent
    assert!(mock.times(A, &Ev::Find).len() >= 2);
    assert_eq!(row(&svc, 0)["ok"], json!(false), "30 s without a good reply");
    assert_eq!(svc.state.get("batteries.ble.current"), Some(Value::Null));
}

/// Lines 208, 215, 224: a link that drops between polls ends the session with `ConnectionError: link dropped`;
/// one that drops while a reply is awaited is reported as the missed reply, at once (not after 3 s).
#[tokio::test(start_paused = true)]
async fn link_drop_between_polls_and_during_a_reply_wait() {
    let mock = Mock::new(vec![(A, good())]);
    let svc = service(&mock, &[A], 1.0);
    tokio::spawn(svc.clone().battery(0));
    run_for(5.5).await; // between polls
    mock.drop_link(A);
    run_for(1.0).await;
    assert_eq!(svc.rec(A).unwrap().record.error.as_deref(), Some("ConnectionError: link dropped"));
    run_for(6.0).await; // reconnected (backoff 2 s, rescan, connect), polling again
    assert!(svc.rec(A).unwrap().record.connected);
    mock.with(A, |d| d.drop_on_write = true);
    run_for(1.0).await;
    let w = *mock.times(A, &Ev::Write(4)).last().unwrap();
    let disc = *mock.times(A, &Ev::Disconnect).last().unwrap();
    assert!(disc - w < 0.05, "ends at once: write {w}, disconnect {disc}");
    assert_eq!(svc.rec(A).unwrap().record.error.as_deref(), Some("TimeoutError: no reply to 0x04"));
}

/// Lines 233-248: the backoff is never reset, because `_session` always ends with an exception (the
/// `backoff = 2.0` after it is unreachable). After a long good session the next reconnect still waits the doubled
/// value. Ported as is; see the report.
#[tokio::test(start_paused = true)]
async fn backoff_is_not_reset_by_a_good_session() {
    let mock = Mock::new(vec![(A, good())]);
    let svc = service(&mock, &[A], 1.0);
    tokio::spawn(svc.clone().battery(0));
    run_for(10.0).await;
    mock.drop_link(A); // session 1 ends → wait 2 s
    run_for(60.0).await; // session 2: a minute of good polls
    mock.drop_link(A); // session 2 ends → wait 4 s, not 2
    run_for(10.0).await;
    let finds = mock.times(A, &Ev::Find);
    let errs: Vec<f64> = mock.times(A, &Ev::Disconnect);
    assert_eq!(finds.len(), 3);
    assert!(((finds[1] - errs[0]) - 2.0).abs() < 0.05, "{finds:?} {errs:?}");
    assert!(((finds[2] - errs[1]) - 4.0).abs() < 0.05, "{finds:?} {errs:?}");
}

/// Lines 250-264: `run()` lists the batteries at once ("connecting…"), clears any stale BlueZ link to each before
/// the first scan, then polls them all; the keys tick every second.
#[tokio::test(start_paused = true)]
async fn run_lists_batteries_and_clears_stale_links_first() {
    let mock = Mock::new(vec![(A, good()), (B, good())]);
    let svc = service(&mock, &[A, B], 1.0);
    tokio::spawn(svc.clone().run());
    tokio::time::sleep(Duration::from_millis(1)).await;
    let rows = svc.state.list("batteries.ble");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["name"], "1");
    assert_eq!((rows[0]["ok"].clone(), rows[0]["connected"].clone()), (json!(false), json!(false)));
    assert_eq!(svc.state.get("batteries.ble.reporting"), Some(json!(0)));
    assert_eq!(svc.state.get("batteries.ble.expected"), Some(json!(2)));
    run_for(12.0).await;
    for a in [A, B] {
        assert_eq!(mock.kinds(a)[0], Ev::DisconnectAddr, "{a}: stale link cleared before anything else");
    }
    let first_find = mock.log.lock().unwrap().iter().position(|(_, _, e)| *e == Ev::Find).unwrap();
    let last_clear = mock.log.lock().unwrap().iter().rposition(|(t, _, e)| *e == Ev::DisconnectAddr && *t < 1.0).unwrap();
    assert!(last_clear < first_find);
    assert_eq!(svc.state.get("batteries.ble.reporting"), Some(json!(2)));
    let age = svc.state.list("batteries.ble")[0]["age"].as_f64().unwrap();
    assert!(age < 1.5, "age {age}");
}

/// Python `__init__` line 144-146 and `build()`: name falls back to the address, values go through `str()`, only
/// an enabled `kind = "epoch_ble"` device counts.
#[test]
fn settings_from_config() {
    let c = Cfg::parse(
        r#"[device.epoch]
kind = "epoch_ble"
enabled = true
[[device.epoch.battery]]
address = "AA:00:00:00:00:01"
[[device.epoch.battery]]
name = 2
address = "AA:00:00:00:00:02"
serial = "SN-PLACEHOLDER"
"#,
    )
    .unwrap();
    let s = Settings::from_config(&c).unwrap().unwrap();
    assert_eq!(s.batteries[0], Battery::new(None, "AA:00:00:00:00:01", None));
    assert_eq!(s.batteries[0].name, "AA:00:00:00:00:01");
    assert_eq!((s.batteries[1].name.as_str(), s.batteries[1].serial.as_str()), ("2", "SN-PLACEHOLDER"));
    assert_eq!(s.period, 1.0);
    let off = Cfg::parse("[device.epoch]\nkind = \"epoch_ble\"\nenabled = false").unwrap();
    assert!(Settings::from_config(&off).unwrap().is_none());
    let bad = Cfg::parse("[device.epoch]\nkind = \"epoch_ble\"\nenabled = true\n[[device.epoch.battery]]\nname = \"x\"").unwrap();
    assert!(Settings::from_config(&bad).is_err(), "a battery without an address (Python: KeyError at start)");
}
