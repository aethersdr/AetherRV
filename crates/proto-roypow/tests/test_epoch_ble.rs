//! EPOCH (RoyPow BMS) BLE frames, as captured from battery 240200401 on 2026-11-21.
//! Port of tests/test_epoch_ble.py.

use std::collections::HashMap;

use proto_roypow::{Battery, FrameAssembler, Reading, Record, decode, publish, query};
use serde_json::{Value, json};

const NOTES: [&str; 6] = [
    // notification chunks exactly as received, in order
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

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn get<'a>(out: &'a [(String, Value)], key: &str) -> &'a Value {
    &out.iter().rev().find(|(k, _)| k == key).unwrap().1
}

/// py: tests/test_epoch_ble.py::test_queries_match_what_the_battery_answered
#[test]
fn test_queries_match_what_the_battery_answered() {
    assert_eq!(to_hex(&query(0x04)), "ead10104ff04fff5");
    assert_eq!(to_hex(&query(0x03)), "ead10104ff03f8f5");
    assert_eq!(to_hex(&query(0x02)), "ead10104ff02f9f5");
}

/// py: tests/test_epoch_ble.py::test_assemble_and_decode_real_frames
#[test]
fn test_assemble_and_decode_real_frames() {
    let mut fa = FrameAssembler::new();
    for n in NOTES {
        fa.feed(&hex(n));
    }
    assert_eq!(fa.msgs.keys().copied().collect::<Vec<_>>(), vec![2, 3, 4]);
    let d = decode(&fa.msgs);
    assert_eq!(
        (d.soc, d.voltage, d.current, d.cycles, d.problem_code),
        (Some(100), Some(13.3), Some(-5.81), Some(39), Some(0))
    );
    assert!(d.cells == vec![3.325, 3.324, 3.324, 3.327] && d.cell_delta_mv == Some(3));
    assert!(d.temps == vec![11.0, 10.0, 15.0, 20.0] && d.charge_fet == Some(true) && d.discharge_fet == Some(true));
    assert_eq!(d.remaining_ah, Some(450.3));
}

/// py: tests/test_epoch_ble.py::test_bad_checksum_is_dropped
#[test]
fn test_bad_checksum_is_dropped() {
    let mut fa = FrameAssembler::new();
    fa.feed(&hex("ead1010fff020404040cfd0cfc0cfc0cff00f5")); // wrong xor
    assert!(fa.msgs.is_empty());
}

/// py: tests/test_epoch_ble.py::test_publish_counts_only_fresh_batteries
#[test]
fn test_publish_counts_only_fresh_batteries() {
    let batteries = [Battery::new(Some("296"), "A", None), Battery::new(Some("401"), "B", None)];
    let mut last = HashMap::new();
    last.insert(
        "B".to_string(),
        Record {
            t: Some(1000.0),
            data: Some(Reading { soc: Some(100), current: Some(-5.81), ..Reading::default() }),
            ..Record::default()
        },
    );
    let s = publish(&batteries, &last, 1030.0);
    assert!(get(&s, "batteries.ble.reporting") == &json!(1) && get(&s, "batteries.ble.current") == &json!(-5.81));
    let s = publish(&batteries, &last, 1000.0 + 31.0);
    assert_eq!(get(&s, "batteries.ble.reporting"), &json!(0));
}

/// Extra (no Python test): the decoded dict and the published rows match, key for key and in order, what the
/// Python reference printed (`json.dumps`) for the same frames and records.
#[test]
fn decode_and_publish_match_python_reference() {
    let mut fa = FrameAssembler::new();
    for n in NOTES {
        fa.feed(&hex(n));
    }
    let d = decode(&fa.msgs);
    assert_eq!(
        serde_json::to_string(&d.to_map()).unwrap(),
        r#"{"soc":100,"voltage":13.3,"current":-5.81,"remaining_ah":450.3,"cycles":39,"problem_code":0,"charge_fet":true,"discharge_fet":true,"runtime_h":75.1,"cells":[3.325,3.324,3.324,3.327],"cell_delta_mv":3,"temps":[11.0,10.0,15.0,20.0]}"#
    );
    assert_eq!(
        serde_json::to_string(&decode(&Default::default()).to_map()).unwrap(),
        r#"{"soc":null,"voltage":null,"current":null,"remaining_ah":null,"cycles":null,"problem_code":null,"charge_fet":null,"discharge_fet":null,"runtime_h":null,"cells":[],"cell_delta_mv":null,"temps":[]}"#
    );
    let batteries = [Battery::new(Some("296"), "A", None), Battery::new(Some("401"), "B", Some("x"))];
    let mut last = HashMap::new();
    last.insert("B".to_string(), Record { t: Some(1000.0), data: Some(d), connected: true, error: None });
    last.insert("A".to_string(), Record { error: Some("TimeoutError: x".into()), ..Record::default() });
    let out = publish(&batteries, &last, 1030.0);
    let obj: serde_json::Map<String, Value> = out.into_iter().collect();
    assert_eq!(
        serde_json::to_string(&obj).unwrap(),
        concat!(
            r#"{"batteries.ble":[{"name":"296","address":"A","serial":"","ok":false,"age":null,"connected":false,"error":"TimeoutError: x"},"#,
            r#"{"name":"401","address":"B","serial":"x","soc":100,"voltage":13.3,"current":-5.81,"remaining_ah":450.3,"cycles":39,"problem_code":0,"charge_fet":true,"discharge_fet":true,"runtime_h":75.1,"cells":[3.325,3.324,3.324,3.327],"cell_delta_mv":3,"temps":[11.0,10.0,15.0,20.0],"ok":true,"age":30.0,"connected":true,"error":null}],"#,
            r#""batteries.ble.reporting":1,"batteries.ble.expected":2,"batteries.ble.current":-5.81}"#
        )
    );
}
