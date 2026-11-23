//! Port of tests/test_rixens.py (the MCS7 CAN decoder, encoders and adapter state).

use std::collections::BTreeSet;

use proto_mcs7::can::{self as rx, CanFrame, Mcs7Can};
use serde_json::{Value, json};

fn frame(i: u32, data: &[u8]) -> CanFrame<'_> {
    CanFrame::standard(i, data)
}

fn dict(d: Option<rx::Decoded>) -> Value {
    Value::Object(d.expect("decoded").to_map())
}

fn get<'a>(out: &'a [(String, Value)], key: &str) -> &'a Value {
    &out.iter().rev().find(|(k, _)| k == key).expect(key).1
}

/// py: tests/test_rixens.py::test_decode_temps_vector
#[test]
fn test_decode_temps_vector() {
    // glycol 0x1F40 = 8000 × 0.01 = 80.00 °C ; voltage 0x0084 = 132 × 0.1 = 13.2 V
    let d = rx::decode(0x726, &[0, 0, 0x40, 0x1F, 0, 0, 0x84, 0x00]);
    assert_eq!(dict(d), json!({"glycol_c": 80.0, "voltage": 13.2}));
}

/// py: tests/test_rixens.py::test_decode_negative_glycol_is_signed
#[test]
fn test_decode_negative_glycol_is_signed() {
    let d = dict(rx::decode(0x726, &[0, 0, 0x0C, 0xFE, 0, 0, 0x7E, 0x00])); // 0xFE0C = -500 → -5.00 °C
    assert!(d["glycol_c"] == json!(-5.0) && d["voltage"] == json!(12.6));
}

/// py: tests/test_rixens.py::test_decode_diag_vector
#[test]
fn test_decode_diag_vector() {
    // fuel 23 × 0.1 = 2.3 Hz ; fan 0x0BB8 = 3000 rpm ; glow 0x01F4 = 500 × 0.1 = 50.0 W
    let d = rx::decode(0x725, &[0, 0, 0, 23, 0xB8, 0x0B, 0xF4, 0x01]);
    assert_eq!(dict(d), json!({"fuel_hz": 2.3, "fan_rpm": 3000, "glow_w": 50.0}));
}

/// py: tests/test_rixens.py::test_decode_sources_bits
#[test]
fn test_decode_sources_bits() {
    let d = rx::decode(0x724, &[0, 0, 0, 0, 0, 0, 0x08 | 0x80, 0]);
    assert_eq!(
        dict(d),
        json!({"source.furnace": true, "source.electric": false, "source.engine": false, "source.preheat": true})
    );
}

/// py: tests/test_rixens.py::test_decode_ignores_unknown_and_short
#[test]
fn test_decode_ignores_unknown_and_short() {
    assert_eq!(rx::decode(0x123, &[0; 8]), None);
    assert_eq!(rx::decode(0x726, &[0; 4]), None);
}

/// py: tests/test_rixens.py::test_command_encoders_match_documented_bytes
#[test]
fn test_command_encoders_match_documented_bytes() {
    assert_eq!(rx::cmd_setpoint(20.0), Ok([0x01, 0xC8, 0x00, 0, 0, 0, 0, 0])); // 200
    assert_eq!(rx::cmd_ambient(-2.5), Ok([0x08, 0xE7, 0xFF, 0, 0, 0, 0, 0])); // -25
    assert_eq!(rx::cmd_fan(None), [0x02, 0xFF, 0, 0, 0, 0, 0, 0]);
    assert_eq!(rx::cmd_fan(Some(150))[1], 100);
    assert_eq!(rx::cmd_source("furnace", true), Ok([0x03, 0x01, 0, 0, 0, 0, 0, 0]));
    assert_eq!(rx::cmd_source("hot_water", false), Ok([0x06, 0, 0, 0, 0, 0, 0, 0]));
    assert_eq!(rx::cmd_engine_running(true)[..2], [0x0B, 0x01]);
}

/// py: tests/test_rixens.py::test_adapter_state_and_staleness
#[test]
fn test_adapter_state_and_staleness() {
    let now = 1_795_136_523.0;
    let mut a = Mcs7Can::new("rixens", true);
    assert_eq!(get(&a.derived(now), "climate.rixens.state"), &Value::Null); // never heard from it
    a.on_frame(&frame(0x724, &[0, 0, 0, 0, 0, 0, 0x08, 0]), now);
    a.on_frame(&frame(0x726, &[0, 0, 0x40, 0x1F, 0, 0, 0x84, 0]), now);
    a.on_frame(&frame(0x725, &[0, 0, 0, 0, 0, 0, 0, 0]), now);
    let d = a.derived(now);
    assert!(get(&d, "climate.rixens.state") == &json!("standby") && get(&d, "climate.rixens.coolant") == &json!(80.0));
    assert!(
        get(&d, "climate.rixens.sources") == &json!(["furnace"]) && get(&d, "climate.rixens.burner") == &json!(false)
    );
    a.on_frame(&frame(0x725, &[0, 0, 0, 23, 0xB8, 0x0B, 0, 0]), now);
    assert_eq!(get(&a.derived(now), "climate.rixens.state"), &json!("heating"));
    assert_eq!(get(&a.derived(now + 11.0), "climate.rixens.state"), &json!("no data"));
    let ids: BTreeSet<String> =
        a.summary(now).as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap().to_string()).collect();
    assert_eq!(ids, BTreeSet::from(["724".into(), "725".into(), "726".into()])); // raw inventory still kept
}

/// Extra (no Python test): the published keys, in order, match what the Python adapter produces for the same
/// frames (`json.dumps` of `derived()` and `summary()`), and extended/error frames are handled the same way.
#[test]
fn derived_and_summary_match_python_reference() {
    let now = 1000.0;
    let mut a = Mcs7Can::new("rixens", true);
    a.on_frame(&frame(0x724, &[0, 0, 0, 0, 0, 0, 0x08, 0]), now);
    a.on_frame(&frame(0x726, &[0, 0, 0x40, 0x1F, 0, 0, 0x84, 0]), now);
    a.on_frame(&frame(0x725, &[0, 0, 0, 23, 0xB8, 0x0B, 0, 0]), now + 0.25);
    a.on_frame(&CanFrame { extended: true, ..frame(0x726, &[0, 0, 0, 0, 0, 0, 0, 0]) }, now + 0.5);
    a.on_frame(&CanFrame { error: true, ..frame(0x7FF, &[0; 8]) }, now);
    let out: serde_json::Map<String, Value> = a.publish(now + 2.0).into_iter().collect();
    assert_eq!(
        serde_json::to_string(&out).unwrap(),
        concat!(
            r#"{"rixens.ids":[{"id":"724","count":1,"data":"00 00 00 00 00 00 08 00","age":2.0},"#,
            r#"{"id":"725","count":1,"data":"00 00 00 17 b8 0b 00 00","age":1.8},"#,
            r#"{"id":"726","count":2,"data":"00 00 00 00 00 00 00 00","age":1.5}],"#,
            r#""climate.rixens.source_furnace":true,"climate.rixens.source_electric":false,"#,
            r#""climate.rixens.source_engine":false,"climate.rixens.source_preheat":false,"#,
            r#""climate.rixens.glycol_c":80.0,"climate.rixens.voltage":13.2,"climate.rixens.fuel_hz":2.3,"#,
            r#""climate.rixens.fan_rpm":3000,"climate.rixens.glow_w":0.0,"climate.rixens.coolant":80.0,"#,
            r#""climate.rixens.burner":true,"climate.rixens.sources":["furnace"],"climate.rixens.state":"heating","#,
            r#""climate.rixens.live":true}"#
        )
    );
    assert_eq!(Mcs7Can::new("rixens", false).publish(now), vec![("rixens.ids".to_string(), json!([]))]);
    assert!(rx::cmd_setpoint(f64::NAN).is_err());
    assert!(rx::cmd_setpoint(3276.8).is_err()); // 32768 doesn't fit a signed 16-bit field
    assert!(rx::cmd_source("floor", true).is_err());
}
