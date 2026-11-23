//! Port of tests/test_nmea.py.

use std::collections::HashMap;

use proto_nmea::{NmeaGps, Sentence, checksum_ok, parse};
use serde_json::{Value, json};

fn nmea(body: &str) -> String {
    let cs = body.chars().fold(0u32, |a, c| a ^ c as u32);
    format!("${body}*{cs:02X}")
}

/// Python `round(x, n)` on a float, for the expected values.
fn round(x: f64, n: usize) -> f64 {
    format!("{x:.n$}").parse().unwrap()
}

// Classic reference sentences (widely published examples)
const GGA: &str = "$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47";
const RMC: &str = "$GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W*6A";

/// A stand-in for Python's `State`: the updates applied in order.
#[derive(Default)]
struct State(HashMap<String, Value>);

impl State {
    fn update(&mut self, up: Vec<(String, Value)>) {
        self.0.extend(up);
    }
    fn get(&self, key: &str) -> &Value {
        self.0.get(key).unwrap_or(&Value::Null)
    }
}

/// py: tests/test_nmea.py::test_checksums
#[test]
fn test_checksums() {
    assert!(checksum_ok(GGA) && checksum_ok(RMC));
    assert!(!checksum_ok(&GGA.replace("*47", "*48")));
}

/// py: tests/test_nmea.py::test_parse_reference_sentences
#[test]
fn test_parse_reference_sentences() {
    let Some(Sentence::Gga(g)) = parse(GGA) else { panic!("GGA") };
    assert!(g.sats == Some(8) && g.hdop == Some(0.9) && g.alt_m == Some(545.4));
    assert!(round(g.lat.unwrap(), 4) == 48.1173 && round(g.lon.unwrap(), 4) == 11.5167);
    let Some(Sentence::Rmc(r)) = parse(RMC) else { panic!("RMC") };
    assert!(r.valid && r.speed_kn == Some(22.4) && r.course == Some(84.4));
}

/// py: tests/test_nmea.py::test_southern_western_and_gn_talker
#[test]
fn test_southern_western_and_gn_talker() {
    let s = nmea("GNRMC,010203.00,A,4522.4100,N,12141.7540,W,0.0,0.0,201126,,,A");
    let Some(Sentence::Rmc(r)) = parse(&s) else { panic!("GNRMC") };
    assert!(round(r.lat.unwrap(), 5) == 45.37350 && round(r.lon.unwrap(), 5) == -121.69590);
    let Some(Sentence::Rmc(r)) = parse(&nmea("GPRMC,010203,A,3351.000,S,15112.000,E,0,0,201126,,")) else {
        panic!("GPRMC")
    };
    assert!(r.lat.unwrap() < 0.0 && r.lon.unwrap() > 0.0);
}

/// py: tests/test_nmea.py::test_listener_updates_state_and_no_fix
#[test]
fn test_listener_updates_state_and_no_fix() {
    let mut st = State::default();
    let mut g = NmeaGps::default();
    let now = 1_795_136_523.0;
    st.update(g.feed_datagram(format!("{GGA}\r\n{RMC}\r\n").as_bytes(), now));
    assert!(st.get("location.fix") == &json!(true) && st.get("location.sats") == &json!(8));
    assert!(
        st.get("location.speed") == &json!(round(22.4 * 1.852, 1))
            && st.get("location.source") == &json!("peplink NMEA")
    );
    st.update(g.feed(&nmea("GPRMC,123520,V,,,,,,,230394,,"), now));
    assert_eq!(st.get("location.fix"), &json!(false));
    st.update(g.feed("$GPGGA,garbage*00", now));
    assert_eq!(g.bad(), 1);
}

/// Extra (no Python test): the exact updates, in order, that the Python listener applies for the reference
/// sentences, and the periodic no-fix check.
#[test]
fn updates_match_python_reference() {
    let mut g = NmeaGps::default();
    let up = g.feed(GGA, 100.0);
    assert_eq!(
        serde_json::to_string(&up).unwrap(),
        r#"[["location.source","peplink NMEA"],["gps.sentences",1],["location.sats",8],["location.hdop",0.9],["location.alt_m",545.4]]"#
    );
    let up = g.feed(RMC, 100.0);
    assert_eq!(
        serde_json::to_string(&up).unwrap(),
        r#"[["location.source","peplink NMEA"],["gps.sentences",2],["location.fix",true],["location.lat",48.1173],["location.lon",11.5166667],["location.speed",41.5],["location.heading",84.4],["location.time",764426119.0]]"#
    );
    assert_eq!(g.stale(130.0), None);
    assert_eq!(g.stale(130.5), Some(("location.fix".to_string(), json!(false))));
    assert_eq!(NmeaGps::default().stale(1e9), None); // never had a fix
}
