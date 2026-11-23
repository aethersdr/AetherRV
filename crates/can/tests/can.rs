//! CAN transport tests: the Python reference tests for canbus.py, plus real-vcan tests of the TX echo and the
//! loopback rule behind "one process per CAN port".

use aetherrv_can::bus::{BoxFut, Link, Opener, fake};
use aetherrv_can::capture;
use aetherrv_can::failover::{FailoverBus, FAILBACK_S};
use aetherrv_can::recover::{Recoverable, Timing, recover_links};
use aetherrv_can::{CanBus, CanFrame, SendError};
use aetherrv_core::state::{StateExt, StateStore};
use aetherrv_core::txgate::TxGate;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn gate(dev: &str, on: bool) -> TxGate {
    TxGate::new([(dev.to_string(), on)])
}

/// py: tests/test_core.py::test_capture_roundtrip_and_pipeline (the CAN part: bus → handlers + capture → file →
/// read back; the N2K/capture-only decoding of the same test is ported in proto-n2k and the aux service)
#[tokio::test]
async fn test_capture_roundtrip_and_pipeline() {
    let dir = tempfile::tempdir().unwrap();
    let (cb, ctl) = fake::bus("t", Some(dir.path().to_path_buf()));
    let seen = Arc::new(Mutex::new(vec![]));
    let s2 = seen.clone();
    cb.add_handler(Arc::new(move |f: &CanFrame| s2.lock().unwrap().push(f.id)));
    let task = tokio::spawn(cb.clone().run());
    tokio::time::sleep(Duration::from_millis(100)).await;
    let sent: Vec<CanFrame> = (0..5).map(|i| CanFrame::ext(0x09F20123 + i, &[i as u8; 8])).collect();
    for f in &sent {
        assert!(ctl.hear(f.clone().with_ts(aetherrv_core::clock::now_s())));
    }
    tokio::time::sleep(Duration::from_millis(1500)).await; // the capture flushes every second
    task.abort();
    let _ = task.await;
    assert_eq!(cb.stats().frames, 5);
    assert_eq!(seen.lock().unwrap().len(), 5);
    let logs: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(logs.len(), 1);
    assert!(logs[0].file_name().unwrap().to_string_lossy().starts_with("t-"));
    let back = capture::read_file(&logs[0]).unwrap();
    assert_eq!(back.iter().map(|f| f.id).collect::<Vec<_>>(), sent.iter().map(|f| f.id).collect::<Vec<_>>());
    assert_eq!(back.iter().map(|f| f.data.clone()).collect::<Vec<_>>(), sent.iter().map(|f| f.data.clone()).collect::<Vec<_>>());
}

/// py: tests/test_core.py::test_canbus_send_is_gated
#[tokio::test]
async fn test_canbus_send_is_gated() {
    let (cb, ctl) = fake::bus("g", None);
    tokio::spawn(cb.clone().run());
    tokio::time::sleep(Duration::from_millis(100)).await;
    let f = CanFrame::std(0x123, &[1]);
    assert!(matches!(cb.send("espar", &f, &gate("espar", false), false), Err(SendError::Blocked(_))));
    assert!(ctl.sent().is_empty());
    cb.send("bench", &f, &gate("bench", true), false).unwrap(); // allowed path works
    assert_eq!(ctl.sent().len(), 1);
}

/// py: tests/test_core.py::test_prune_captures
#[test]
fn test_prune_captures() {
    let d = tempfile::tempdir().unwrap();
    let (old, new) = (d.path().join("a.log"), d.path().join("b.log"));
    std::fs::write(&old, b"xxxxxxxxxx").unwrap();
    std::fs::write(&new, b"xxxxxxxxxx").unwrap();
    let t = std::time::SystemTime::now() - Duration::from_secs(20 * 86400);
    std::fs::File::options().write(true).open(&old).unwrap().set_modified(t).unwrap();
    capture::prune(d.path(), 14.0, 20.0, aetherrv_core::clock::now_s());
    assert!(!old.exists() && new.exists());
    capture::prune(d.path(), 14.0, 0.000_000_005, aetherrv_core::clock::now_s()); // over the size cap: oldest go
    assert!(!new.exists());
}

struct SlowOpener;
impl Opener for SlowOpener {
    fn open(&self) -> BoxFut<std::io::Result<Link>> {
        Box::pin(async {
            tokio::time::sleep(Duration::from_millis(500)).await; // an open to an unreachable host
            Err(std::io::Error::other("No route to host"))
        })
    }
    fn kind(&self) -> &'static str {
        "socketcand"
    }
}

/// py: tests/test_core.py::test_slow_bus_open_does_not_block_event_loop
#[tokio::test(flavor = "current_thread")]
async fn test_slow_bus_open_does_not_block_event_loop() {
    let cb = CanBus::new("slow", "can0", Box::new(SlowOpener), None);
    let task = tokio::spawn(cb.run());
    let t0 = std::time::Instant::now();
    let mut ticks = 0;
    while t0.elapsed() < Duration::from_millis(400) {
        tokio::time::sleep(Duration::from_millis(20)).await;
        ticks += 1;
    }
    task.abort();
    assert!(ticks >= 10, "the runtime kept running while the open was in progress ({ticks})");
}

fn rx(id: u32, ts: f64) -> CanFrame {
    CanFrame::ext(id, &[1]).with_ts(ts)
}

/// py: tests/test_core.py::test_failover_bus_paths
#[tokio::test]
async fn test_failover_bus_paths() {
    let (hat, hat_ctl) = fake::bus("hat0", None);
    let (cerbo, cerbo_ctl) = fake::bus("n2k_cerbo", None);
    hat.set_connected(true);
    cerbo.set_connected(true);
    // sends go straight to the fake links: open them without the reader
    let st = Arc::new(StateStore::new());
    let fb = FailoverBus::new("n2k", hat.clone(), cerbo.clone(), Some(st.clone()));
    let got = Arc::new(AtomicU32::new(0));
    let g2 = got.clone();
    fb.add_handler(Arc::new(move |_f: &CanFrame| {
        g2.fetch_add(1, Ordering::SeqCst);
    }));
    let switched = Arc::new(Mutex::new(vec![]));
    let s2 = switched.clone();
    fb.on_switch(Arc::new(move |to: &str| s2.lock().unwrap().push(to.to_string())));
    let g = gate("n2k", true);
    let t0 = 1000.0;
    let mut now = t0;
    hat.dispatch(&rx(0x09F20100, now));
    cerbo.dispatch(&rx(0x09F20100, now)); // same physical bus: both paths hear it
    assert_eq!(got.load(Ordering::SeqCst), 1, "forwarded once, from the active path");
    // the fake links need opening for sends: run both readers
    tokio::spawn(hat.clone().run());
    tokio::spawn(cerbo.clone().run());
    tokio::time::sleep(Duration::from_millis(50)).await;
    fb.send_at("n2k", &CanFrame::ext(0x09F20100, &[1]).tx(), &g, false, now).unwrap();
    assert_eq!((hat_ctl.sent().len(), cerbo_ctl.sent().len()), (1, 0));
    now += 3.0;
    cerbo.dispatch(&rx(0x09F20100, now));
    fb.tick(now); // hat0 silent, the Cerbo still hears the bus → backup
    assert!(!fb.on_primary());
    assert_eq!(*switched.lock().unwrap(), vec!["n2k_cerbo".to_string()]);
    assert!(st.is_true("n2k.path_backup"));
    fb.send_at("n2k", &CanFrame::ext(0x09F20100, &[1]).tx(), &g, false, now).unwrap();
    assert_eq!(cerbo_ctl.sent().len(), 1);
    now += 1.0;
    hat.dispatch(&rx(0x09F20100, now));
    cerbo.dispatch(&rx(0x09F20100, now));
    fb.tick(now);
    assert!(!fb.on_primary(), "no instant failback");
    for _ in 0..31 {
        now += 1.0;
        hat.dispatch(&rx(0x09F20100, now));
        cerbo.dispatch(&rx(0x09F20100, now));
        fb.tick(now);
    }
    assert!(fb.on_primary() && st.is_false("n2k.path_backup"));
    now += 5.0;
    fb.tick(now); // both silent = the bus is quiet: stay put
    assert!(fb.on_primary());
    let r = fb.send_at("n2k", &CanFrame::ext(0x09F20100, &[1]).tx(), &g, false, now); // but don't send into silence
    assert!(matches!(r, Err(SendError::Refused(_))));
    assert_eq!(FAILBACK_S, 30.0);
}

/// py: tests/test_core.py::test_failover_bus_unacked_frame_resets_and_fails_over
#[tokio::test]
async fn test_failover_bus_unacked_frame_resets_and_fails_over() {
    let (hat, hat_ctl) = fake::bus("hat0", None);
    let (cerbo, cerbo_ctl) = fake::bus("n2k_cerbo", None);
    tokio::spawn(hat.clone().run());
    tokio::spawn(cerbo.clone().run());
    tokio::time::sleep(Duration::from_millis(50)).await;
    let fb = FailoverBus::new("n2k", hat.clone(), cerbo.clone(), None);
    let resets = Arc::new(Mutex::new(vec![]));
    let r2 = resets.clone();
    fb.set_reset(Arc::new(move |iface: String| {
        let r2 = r2.clone();
        Box::pin(async move {
            r2.lock().unwrap().push(iface);
            Ok(())
        })
    }));
    fb.set_echo_s(0.05);
    let g = gate("n2k", true);
    let now = aetherrv_core::clock::now_s();
    hat.dispatch(&rx(0x09F20100, now));
    cerbo.dispatch(&rx(0x09F20100, now));
    let msg = CanFrame::ext(0x0DED0096, &[0x11, 0x22]).tx();
    hat_ctl.echo.store(true, Ordering::SeqCst); // echoed = really sent
    fb.send("n2k", &msg, &g, false).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(fb.on_primary() && resets.lock().unwrap().is_empty());
    hat_ctl.echo.store(false, Ordering::SeqCst); // no echo: stuck in the chip
    hat.dispatch(&rx(0x09F20100, aetherrv_core::clock::now_s()));
    fb.send("n2k", &msg, &g, false).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(*resets.lock().unwrap(), vec!["hat0".to_string()]);
    assert!(!fb.on_primary());
    assert!(cerbo_ctl.sent().is_empty(), "not replayed on the backup");
    hat.dispatch(&rx(0x09F20100, aetherrv_core::clock::now_s()));
    fb.tick(aetherrv_core::clock::now_s());
    assert!(!fb.on_primary(), "failback held off after a stuck frame");
}

struct B {
    up: AtomicBool,
    reopened: AtomicU32,
    kind: &'static str,
}
impl Recoverable for B {
    fn bus_name(&self) -> String {
        "hat1".into()
    }
    fn channel(&self) -> String {
        "hat1".into()
    }
    fn interface(&self) -> &'static str {
        self.kind
    }
    fn link_up(&self) -> bool {
        self.up.load(Ordering::SeqCst)
    }
    fn reopen(&self) {
        self.reopened.fetch_add(1, Ordering::SeqCst);
    }
}

/// py: tests/test_core.py::test_recover_links_rebinds_a_port_that_stays_down_once_per_interval
#[tokio::test]
async fn test_recover_links_rebinds_a_port_that_stays_down_once_per_interval() {
    let b = Arc::new(B { up: AtomicBool::new(false), reopened: AtomicU32::new(0), kind: "socketcan" });
    let other = Arc::new(B { up: AtomicBool::new(false), reopened: AtomicU32::new(0), kind: "socketcand" });
    let calls = Arc::new(Mutex::new(vec![]));
    let (c2, b2) = (calls.clone(), b.clone());
    let st = Arc::new(StateStore::new());
    let t = tokio::spawn(recover_links(
        vec![b.clone() as Arc<dyn Recoverable>, other.clone()],
        Arc::new(move |iface: String| {
            let (c2, b2) = (c2.clone(), b2.clone());
            Box::pin(async move {
                let n = {
                    let mut c = c2.lock().unwrap();
                    c.push(iface);
                    c.len()
                };
                b2.up.store(n > 1, Ordering::SeqCst); // first rebind doesn't help, second does
                Ok(())
            })
        }),
        Some(st.clone()),
        Timing { down_s: 0.0, every_s: 0.05, poll_s: 0.01, settle_s: 0.0 },
    ));
    tokio::time::sleep(Duration::from_millis(300)).await;
    t.abort();
    assert_eq!(*calls.lock().unwrap(), vec!["hat1".to_string(), "hat1".to_string()]);
    assert_eq!(b.reopened.load(Ordering::SeqCst), 2);
    assert_eq!(other.reopened.load(Ordering::SeqCst), 0);
    assert!(st.is_true("bus.hat1.rebind_ok"));
}

// ── real SocketCAN on vcan (skipped when the interface is missing) ─────────────────────────────────────────────

fn vcan(n: u8) -> Option<String> {
    let i = format!("vcan{n}");
    aetherrv_can::raw::iface_up(&i).then_some(i)
}

/// The rule behind "one process per CAN port" (ARCHITECTURE.md §2, §12): a second socket on the same interface hears
/// the first one's frames, while a socket without own-echo never hears itself.
#[tokio::test]
async fn vcan_second_socket_hears_our_frames_but_the_sender_does_not() {
    let Some(i) = vcan(6) else { return eprintln!("vcan6 missing: skipped") };
    let a = aetherrv_can::raw::RawSocket::open(&i, false).unwrap();
    let b = aetherrv_can::raw::RawSocket::open(&i, false).unwrap();
    a.send(&CanFrame::std(0x054, &[9, 5])).unwrap();
    let got = tokio::time::timeout(Duration::from_secs(1), b.recv()).await.expect("b hears a").unwrap();
    assert_eq!((got.id, got.data.clone(), got.is_rx), (0x054, vec![9, 5], true));
    assert!(tokio::time::timeout(Duration::from_millis(200), a.recv()).await.is_err(), "a never hears itself");
}

/// With own-echo, our frame comes back flagged as ours (`MSG_CONFIRM` → is_rx = false): the TX-echo guard's signal.
#[tokio::test]
async fn vcan_own_echo_is_flagged() {
    let Some(i) = vcan(7) else { return eprintln!("vcan7 missing: skipped") };
    let a = aetherrv_can::raw::RawSocket::open(&i, true).unwrap();
    a.send(&CanFrame::ext(0x0DED0096, &[0x11, 0x22])).unwrap();
    let got = tokio::time::timeout(Duration::from_secs(1), a.recv()).await.unwrap().unwrap();
    assert!(!got.is_rx && got.extended && got.id == 0x0DED0096);
}

/// A real bus end to end on vcan: run(), handlers, link_up, send through the gate.
#[tokio::test]
async fn vcan_canbus_end_to_end() {
    let Some(i) = vcan(8) else { return eprintln!("vcan8 missing: skipped") };
    let a = CanBus::socketcan("a", &i, false, None);
    let b = CanBus::socketcan("b", &i, false, None);
    let got = Arc::new(Mutex::new(vec![]));
    let g2 = got.clone();
    b.add_handler(Arc::new(move |f: &CanFrame| g2.lock().unwrap().push(f.clone())));
    tokio::spawn(a.clone().run());
    tokio::spawn(b.clone().run());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(a.link_up() && b.link_up());
    a.send("bench", &CanFrame::std(0x2C4, &[3, 0, 0, 0, 0x6E, 0x02]), &gate("bench", true), false).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(got.lock().unwrap().len(), 1);
    assert_eq!(b.stats().frames, 1);
}
