//! The Ruuvi listener's tracker and config, without Bluetooth: advertisements are fed by hand. Addresses are made up.
use aetherrv_core::config::Cfg;
use aetherrv_ruuvi::{Advert, MAX_UNASSIGNED, Settings, Tracker, norm_addr, short_id};
use serde_json::{Value, json};

fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

/// A format-5 advert with sequence `seq` and temperature `t` (°C, a multiple of 0.005).
fn f5(t: f64, seq: u16) -> Vec<u8> {
    let mut d = hex("0512FC5394C37C0004FFFC040CAC364200CDCBB8334C884F");
    d[1..3].copy_from_slice(&((t / 0.005).round() as i16).to_be_bytes());
    d[16..18].copy_from_slice(&seq.to_be_bytes());
    d
}

fn ad(addr: &str, data: Vec<u8>) -> Advert {
    Advert { addr: addr.into(), data, rssi: Some(-70) }
}

const CABIN: &str = "AA:00:00:00:00:01";
const REAR: &str = "AA:00:00:00:00:02";

fn settings() -> Settings {
    let cfg = Cfg::parse(&format!(
        "[device.ruuvi]\nkind = \"ruuvi\"\n[device.ruuvi.tags]\ncabin = \"{}\"\ncabin_rear = \"{}\"\n",
        CABIN.to_lowercase(),
        REAR.replace(':', "")
    ))
    .unwrap();
    Settings::from_config(&cfg).unwrap().unwrap()
}

fn row<'a>(rows: &'a Value, role: &str) -> &'a Value {
    rows.as_array().unwrap().iter().find(|r| r["role"] == json!(role)).unwrap()
}

#[test]
fn config_addresses_normalised_and_typos_refused() {
    let s = settings();
    assert_eq!(s.tags.get(CABIN).map(String::as_str), Some("cabin"));
    assert_eq!(s.tags.get(REAR).map(String::as_str), Some("cabin_rear"));
    assert_eq!(norm_addr("aa-bb-cc-dd-ee-0f").as_deref(), Some("AA:BB:CC:DD:EE:0F"));
    assert!(norm_addr("AA:BB:CC:DD:EE").is_none() && norm_addr("ZZ:BB:CC:DD:EE:FF").is_none());
    let bad = Cfg::parse("[device.ruuvi]\nkind = \"ruuvi\"\n[device.ruuvi.tags]\ncabin = \"AA:BB:CC\"\n").unwrap();
    assert!(Settings::from_config(&bad).unwrap_err().to_string().contains("not a Bluetooth address"));
    let twice = Cfg::parse(&format!("[device.ruuvi]\nkind = \"ruuvi\"\n[device.ruuvi.tags]\na = \"{CABIN}\"\nb = \"{CABIN}\"\n")).unwrap();
    assert!(Settings::from_config(&twice).unwrap_err().to_string().contains("same address"));
    let off = Cfg::parse(&format!("[device.ruuvi]\nkind = \"ruuvi\"\nenabled = false\n[device.ruuvi.tags]\na = \"{CABIN}\"\n")).unwrap();
    assert!(Settings::from_config(&off).unwrap().is_none());
    assert!(Settings::from_config(&Cfg::parse("").unwrap()).unwrap().is_none());
    assert_eq!(short_id("AA:00:00:00:12:3F"), "123F");
}

#[test]
fn configured_tags_are_listed_before_they_are_heard() {
    let tr = Tracker::new(&settings());
    let rows = tr.rows(1000.0);
    assert_eq!(rows.as_array().unwrap().len(), 2);
    let c = row(&rows, "cabin");
    assert_eq!((c["heard"].clone(), c["temp_c"].clone(), c["age_s"].clone()), (json!(false), Value::Null, Value::Null));
}

#[test]
fn readings_counts_and_gaps() {
    let mut tr = Tracker::new(&settings());
    // the cabin tag: adverts every second; a new measurement every second advert
    for i in 0..10u16 {
        tr.on_advert(&ad(CABIN, f5(21.5, i / 2)), 1000.0 + i as f64, 50.0 + i as f64);
    }
    // a 90 s silence, then it is back with a new measurement
    tr.on_advert(&ad(CABIN, f5(21.0, 7)), 1099.0, 149.0);
    let rows = tr.rows(1100.0);
    let c = row(&rows, "cabin");
    assert_eq!((c["temp_c"].clone(), c["humidity"].clone(), c["battery_v"].clone()), (json!(21.0), json!(53.49), json!(2.977)));
    assert_eq!((c["format"].clone(), c["sequence"].clone(), c["rssi"].clone()), (json!(5), json!(7), json!(-70)));
    assert_eq!((c["adverts"].clone(), c["measurements"].clone()), (json!(11), json!(6)));
    assert_eq!((c["max_gap_s"].clone(), c["gaps_over_60"].clone()), (json!(90.0), json!(1)));
    assert_eq!(c["max_new_gap_s"].clone(), json!(91.0));
    assert_eq!((c["last_heard"].clone(), c["age_s"].clone()), (json!(1099.0), json!(1.0)));
    // the rear tag was never heard
    assert_eq!(row(&rows, "cabin_rear")["heard"], json!(false));
}

#[test]
fn format3_has_no_sequence_so_a_change_of_data_is_a_new_measurement() {
    let mut tr = Tracker::new(&settings());
    let a = hex("03291A1ECE1EFC18F94202CA0B53");
    let mut b = a.clone();
    b[3] = 0x28; // 26.40 °C
    for (i, d) in [&a, &a, &b, &b, &a].into_iter().enumerate() {
        tr.on_advert(&ad(REAR, d.clone()), 1000.0 + i as f64, 10.0 + i as f64);
    }
    let rows = tr.rows(1005.0);
    let r = row(&rows, "cabin_rear");
    assert_eq!((r["format"].clone(), r["sequence"].clone(), r["temp_c"].clone()), (json!(3), Value::Null, json!(26.3)));
    assert_eq!((r["adverts"].clone(), r["measurements"].clone()), (json!(5), json!(3)));
}

#[test]
fn an_unknown_format_is_counted_and_the_last_good_reading_kept() {
    let mut tr = Tracker::new(&settings());
    tr.on_advert(&ad(CABIN, f5(20.0, 1)), 1000.0, 1.0);
    tr.on_advert(&ad(CABIN, hex("E1000102")), 1001.0, 2.0);
    let rows = tr.rows(1002.0);
    let c = row(&rows, "cabin");
    assert_eq!((c["errors"].clone(), c["last_error"].clone()), (json!(1), json!("unknown Ruuvi data format 0xE1")));
    assert_eq!((c["temp_c"].clone(), c["adverts"].clone()), (json!(20.0), json!(2)));
}

#[test]
fn other_ruuvis_are_listed_by_short_id_and_capped() {
    let mut tr = Tracker::new(&settings());
    for i in 0..(MAX_UNASSIGNED + 4) {
        tr.on_advert(&ad(&format!("BB:00:00:00:00:{i:02X}"), f5(5.0, 1)), 1000.0 + i as f64, 1.0 + i as f64);
    }
    let rows = tr.rows(1100.0);
    let a = rows.as_array().unwrap();
    assert_eq!(a.len(), 2 + MAX_UNASSIGNED, "the configured two plus the newest strangers");
    assert_eq!(a[0]["role"], json!("cabin"), "configured tags first");
    let strangers: Vec<&str> = a.iter().filter(|r| r["role"].is_null()).map(|r| r["id"].as_str().unwrap()).collect();
    assert!(!strangers.contains(&"0000") && !strangers.contains(&"0003"), "the oldest dropped: {strangers:?}");
    assert!(strangers.contains(&"0013"));
}
