//! The Espar decoder and UDS against the frames captured on this van on 2026-11-22 (Python tests ported).

use proto_espar::decoder::{Decoder, Event};
use proto_espar::uds::{self, FLOW_CONTROL, IsoTpRx, decode, read_did, read_dtcs, request};
use proto_espar::{Frame, unhex};
use serde_json::{Value, json};
use std::collections::HashMap;

fn f(i: u32, h: &str) -> Frame {
    Frame::hex(i, h)
}

fn snap(a: &Decoder, t: f64) -> HashMap<String, Value> {
    a.snapshot(t).into_iter().map(|(k, v)| (k.trim_start_matches("climate.espar.").to_string(), v)).collect()
}

/// py: tests/test_espar.py::test_espar_cycle_from_the_van_capture
#[test]
fn test_espar_cycle_from_the_van_capture() {
    let mut now = 1000.0;
    let mut a = Decoder::new("espar", 5.0, false);
    let s = snap(&a, now);
    assert_eq!((s["state"].clone(), s["asleep"].clone(), s["heard"].clone()), (json!("off"), json!(true), json!(false)));
    a.on_frame(&f(0x054, "0000FEFFDA020000"), now);
    a.on_frame(&f(0x2C4, "030000001B020000"), now);
    let s = snap(&a, now);
    assert_eq!(s["state"], "off");
    assert_eq!(s["mode"], "off");
    assert_eq!(s["room_c"], 23.0);
    assert_eq!(s["heater_c"], 3.9);
    assert!(s["setpoint_c"].is_null() && s["fault"].is_null() && s["asleep"] == false);
    a.on_frame(&f(0x054, "09050B01DA020000"), now); // HEAT, 80 °F
    a.on_frame(&f(0x2C4, "09000000DA020000"), now);
    now += 300.0;
    a.on_frame(&f(0x054, "09050B01DA020000"), now);
    a.on_frame(&f(0x2C4, "09040000DA020000"), now);
    let s = snap(&a, now);
    assert_eq!((s["state"].clone(), s["mode"].clone(), s["setpoint_c"].clone(), s["run_s"].clone()),
               (json!("heating"), json!("heat"), json!(26.7), json!(300)));
    assert!(s["heater_c"].is_null(), "mirrors the room value while running");
    a.on_frame(&f(0x054, "09050001DA020000"), now);
    assert_eq!(snap(&a, now)["setpoint_c"], 25.6); // 78 °F
    now += 10.0;
    a.on_frame(&f(0x054, "0800FEFFE4020000"), now);
    a.on_frame(&f(0x2C4, "030800009B020000"), now);
    let s = snap(&a, now);
    assert_eq!((s["state"].clone(), s["cooldown_left_s"].clone(), s["mode"].clone(), s["room_c"].clone()),
               (json!("cooldown"), json!(240), json!("off"), json!(24.0)));
    now += 240.0;
    a.on_frame(&f(0x054, "0800FEFFE4020000"), now);
    a.on_frame(&f(0x2C4, "0300000095020000"), now);
    let s = snap(&a, now);
    assert_eq!((s["state"].clone(), s["heater_c"].clone()), (json!("off"), json!(16.1)));
    assert!(s["cooldown_left_s"].is_null());
    now += 30.0; // the bus goes to sleep: normal
    let s = snap(&a, now);
    assert_eq!((s["state"].clone(), s["asleep"].clone()), (json!("off"), json!(true)));
    assert!(s["room_c"].is_null());
}

/// py: tests/test_espar.py::test_espar_fault_alert (the decoder part; the alert rule is ported in aetherrv-alerts)
#[test]
fn test_espar_fault_alert() {
    let mut a = Decoder::new("espar", 5.0, false);
    a.on_frame(&f(0x054, "09050B01DA020000"), 50.0);
    a.on_frame(&f(0x2C4, "0B082000DA020000"), 50.0); // fault: flame loss
    let s = snap(&a, 50.0);
    assert_eq!((s["state"].clone(), s["fault"].clone()), (json!("error"), json!("0x20")));
    assert!(s["fault_text"].as_str().unwrap().contains("flame loss"));
}

/// py: tests/test_espar.py::test_espar_diag_is_gated_and_only_reads (the gate itself is the service's TxGate
/// device `espar_diag`; here: the decoder asks only while awake, only for reads, and flow control is the only
/// other frame it ever asks for)
#[test]
fn test_espar_diag_is_gated_and_only_reads() {
    let now = 100.0;
    let mut off = Decoder::new("espar", 5.0, false);
    off.on_frame(&f(0x054, "0000FEFFDA020000"), now);
    off.on_frame(&f(0x2C4, "030000001B020000"), now);
    assert_eq!(off.diag_due(now, false), None, "diagnostics disabled: nothing asked");
    let mut a = Decoder::new("espar", 5.0, false);
    assert!(!a.awake(now), "bus asleep: never send (no wake-ups)");
    assert_eq!(a.diag_due(now, true), None);
    a.on_frame(&f(0x054, "0000FEFFDA020000"), now);
    a.on_frame(&f(0x2C4, "030000001B020000"), now);
    assert!(a.awake(now));
    let ev = a.on_frame(&f(0x73C, "100B62FD10011400"), now); // first frame → we send flow control
    assert_eq!(ev, Event::SendDiag(unhex("30001400AAAAAAAA".replace("1400", "14AA").as_str()).try_into().unwrap()));
    a.on_frame(&f(0x73C, "2100000000000000"), now);
    a.on_frame(&f(0x73C, "0562FD040084FFFF"), now);
    let s: HashMap<String, Value> = a.diag_snapshot(now, true).into_iter().collect();
    assert_eq!(s["climate.espar.diag.phase"], json!({"request": "heat", "phase": "burning"}));
    assert_eq!(s["climate.espar.diag.voltage_v"], 13.2);
    assert_eq!(s["climate.espar.diag.enabled"], true);
    for _ in 0..40 {
        if let Some(req) = a.diag_due(now + 1000.0, true) {
            assert!(req[1] == 0x22 || req[1] == 0x19, "read services only: {req:02x?}");
        }
    }
}

/// py: tests/test_espar.py::test_espar_no_heat_and_power_up_lock_and_lockout_alerts (decoder part; the alert
/// rule half is ported in aetherrv-alerts)
#[test]
fn test_espar_no_heat_and_power_up_lock_and_lockout_alerts() {
    let mut now = 10.0;
    let mut a = Decoder::new("espar", 5.0, false);
    a.on_frame(&f(0x054, "09050B01DA020000"), now);
    a.on_frame(&f(0x2C4, "030004001B020000"), now); // HEAT, but idle + 0x04
    now += 31.0;
    a.on_frame(&f(0x054, "09050B01DA020000"), now);
    a.on_frame(&f(0x2C4, "030004001B020000"), now);
    assert!(snap(&a, now)["issue"].as_str().unwrap().contains("power-up lock"));
    a.dtcs = Some(vec![json!({"code": "P00012B", "text": "lockout", "active": true, "severity": 1, "lockout": true})]);
    let s: HashMap<String, Value> = a.diag_snapshot(now, true).into_iter().collect();
    assert_eq!(s["climate.espar.diag.lockout"]["code"], "P00012B");
    assert_eq!(s["climate.espar.diag.lockout"]["why"], "10 failed starts in a row");
}

/// py: tests/test_espar.py::test_espar_did_scan_reads_only_and_records
#[test]
fn test_espar_did_scan_reads_only_and_records() {
    let now = 100.0;
    let mut a = Decoder::new("espar", 5.0, true);
    a.scan_dids = vec![0xFD01, 0xFD02, 0xFD03];
    a.scan_todo = a.scan_dids.clone();
    a.on_frame(&f(0x054, "0000FEFFDA020000"), now);
    a.on_frame(&f(0x2C4, "030000001B020000"), now);
    let mut sent = vec![];
    let r = a.scan_step(now).unwrap();
    sent.push(r);
    assert_eq!(r.to_vec(), unhex("0322FD01AAAAAAAA"));
    a.on_frame(&f(0x73C, "0762FD0115E01608"), now); // known DID: raw kept, still decoded
    let r = a.scan_step(now).unwrap();
    sent.push(r);
    assert_eq!(r[1..4].to_vec(), unhex("22FD02"));
    a.on_frame(&f(0x73C, "037F2231AAAAAAAA"), now); // not supported
    sent.push(a.scan_step(now).unwrap());
    a.on_frame(&f(0x73C, "0562FD031234AAAA"), now); // unknown DID answers
    assert!(a.scan_step(now).is_none(), "pass 1 done → pass 2 set up");
    let want: Vec<(u16, String)> = vec![(0xFD01, "15 e0 16 08".into()), (0xFD02, "NRC 0x31".into()), (0xFD03, "12 34".into())];
    assert_eq!(a.scan_res.clone().into_iter().collect::<Vec<_>>(), want);
    assert_eq!(a.scan_watch, vec![0xFD03]);
    let r = a.scan_step(now).unwrap();
    sent.push(r);
    assert_eq!(r[1..4].to_vec(), unhex("22FD03"));
    assert!(sent.iter().all(|d| d[1] == 0x22), "read service only");
    let s: HashMap<String, Value> = a.diag_snapshot(now, true).into_iter().collect();
    assert_eq!(s["climate.espar.diag.scan"], json!({"FD01": "15 e0 16 08", "FD03": "12 34"}));
}

/// py: tests/test_espar_uds.py::test_requests_are_read_only_single_frames
#[test]
fn test_requests_are_read_only_single_frames() {
    assert_eq!(read_did(0xFD04).to_vec(), unhex("0322FD04AAAAAAAA"));
    assert_eq!(read_dtcs().to_vec(), unhex("03190208AAAAAAAA"));
    assert_eq!(FLOW_CONTROL.to_vec(), unhex("300014AAAAAAAAAA"));
    for svc in [0x2E, 0x31, 0x11, 0x10, 0x27, 0x14] {
        assert!(request(svc, &[0, 0]).is_err(), "service {svc:#x} must be refused");
    }
    // every service byte other than 0x22 and 0x19 is refused (ARCHITECTURE.md §7.6)
    for svc in 0u8..=255 {
        assert_eq!(request(svc, &[0, 0]).is_ok(), svc == 0x22 || svc == 0x19, "{svc:#x}");
    }
}

/// py: tests/test_espar_uds.py::test_single_frame_replies
#[test]
fn test_single_frame_replies() {
    let mut rx = IsoTpRx::new();
    let (p, fc) = rx.feed(&unhex("0562FD040084FFFF"), 0.0); // 13.2 V; trailing bytes are junk
    assert!(!fc);
    assert_eq!(decode(&p.unwrap()).to_json(), json!({"did": 0xFD04, "key": "voltage_v", "value": 13.2}));
    let (p, _) = rx.feed(&unhex("0462FD0D28AAAAAA"), 0.0);
    assert_eq!(decode(&p.unwrap()).to_json()["value"], 4.0); // pump 4.0 Hz
    let (p, _) = rx.feed(&unhex("0562FD084E20AAAA"), 0.0);
    assert_eq!(decode(&p.unwrap()).to_json()["value"], 0); // blower stopped (raw 20000)
    let (p, _) = rx.feed(&unhex("037F2231AAAAAAAA"), 0.0);
    assert_eq!(decode(&p.unwrap()).to_json(), json!({"negative": {"service": 0x22, "nrc": 0x31}}));
}

/// py: tests/test_espar_uds.py::test_multi_frame_reply_and_phase_and_dtcs
#[test]
fn test_multi_frame_reply_and_phase_and_dtcs() {
    let mut rx = IsoTpRx::new();
    let (p, fc) = rx.feed(&unhex("100B62FD10011400"), 1.0); // FD10: 8 data bytes → 11-byte reply
    assert!(p.is_none() && fc);
    let (p, _) = rx.feed(&unhex("2100000000000000"), 1.01);
    assert_eq!(decode(&p.unwrap()).to_json(), json!({"did": 0xFD10, "key": "phase", "value": {"request": "heat", "phase": "burning"}}));
    let _ = rx.feed(&unhex("100B5902FF000115"), 2.0); // two DTCs: P000115 (stored), P000440 (active)
    let (p, _) = rx.feed(&unhex("2108000440090000"), 2.01);
    assert_eq!(decode(&p.unwrap()).to_json()["dtcs"], json!([
        {"code": "P000115", "text": "overheating (software threshold)", "active": false, "severity": 5, "lockout": false},
        {"code": "P000440", "text": "timeout talking to the controller", "active": true, "severity": 1, "lockout": false}]));
    let (p, _) = rx.feed(&unhex("2208000440090000"), 9.0); // stray consecutive frame: ignored
    assert!(p.is_none());
}

/// py: tests/test_espar_uds.py::test_hours_pressure_and_strokes
#[test]
fn test_hours_pressure_and_strokes() {
    let mut b = unhex("62FD17");
    for m in [600u32, 60, 120, 180, 240, 30, 0] {
        b.extend_from_slice(&m.to_be_bytes());
    }
    assert_eq!(decode(&b).to_json()["value"],
               json!({"operating": 10.0, "p0_25": 1.0, "p25_50": 2.0, "p50_75": 3.0, "p75_100": 4.0, "pause": 0.5, "altitude": 0.0}));
    assert_eq!(decode(&unhex("62FD3627B0")).to_json()["value"], 1016.0);
    assert_eq!(decode(&unhex("62FD6A00989680")).to_json()["value"], 10_000_000);
    let _ = uds::live_dids();
}
