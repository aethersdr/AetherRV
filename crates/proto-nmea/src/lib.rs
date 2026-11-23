//! NMEA 0183 GPS: sentence parsing and the `location.*` state of the listener. Receive-only.
//!
//! Port of `src/aetherrv/adapters/nmea_gps.py` (the pure parts; the UDP socket lives in the `aetherrv-gps`
//! service). The Peplink's "GPS Forwarding" pushes NMEA sentences over UDP; the service binds the port and
//! hands every datagram to [`NmeaGps::feed_datagram`].
//!
//! Parsed (standard NMEA 0183, checksum verified; talker prefix ignored, so GP/GN/GL/… all work):
//! * RMC: fix status, lat/lon, speed over ground (knots), course, UTC date/time
//! * GGA: fix quality, satellites used, HDOP, altitude (m)

use serde_json::{Value, json};

mod py;

/// `$<body>*<hh>`: the XOR of the body's characters equals the two hex digits after the first `*`.
pub fn checksum_ok(sentence: &str) -> bool {
    let Some(rest) = sentence.strip_prefix('$') else {
        return false;
    };
    let Some((body, cs)) = rest.split_once('*') else {
        return false;
    };
    let calc = body.chars().fold(0u32, |a, c| a ^ c as u32);
    let end = cs.char_indices().nth(2).map_or(cs.len(), |(i, _)| i);
    py::int_base16(&cs[..end]).is_some_and(|v| v == calc as i64)
}

/// `ddmm.mmmm` / `dddmm.mmmm` + N/S/E/W → signed decimal degrees (7 decimals). Python `_coord`, including
/// its slicing: the degrees are everything up to two characters before the dot.
pub fn coord(v: &str, hemi: &str) -> Option<f64> {
    if v.is_empty() || hemi.is_empty() {
        return None;
    }
    let chars: Vec<char> = v.chars().collect();
    let n = chars.len() as isize;
    let dot = chars.iter().position(|c| *c == '.').map_or(n, |i| i as isize);
    let cut = py::slice_index(dot - 2, chars.len());
    let a: String = chars[..cut].iter().collect();
    let b: String = chars[cut..].iter().collect();
    let deg = py::float(&a)? + py::float(&b)? / 60.0;
    Some(py::round(if hemi == "S" || hemi == "W" { -deg } else { deg }, 7))
}

/// A parsed RMC sentence.
#[derive(Debug, Clone, PartialEq)]
pub struct Rmc {
    /// Status `A` (a fix).
    pub valid: bool,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub speed_kn: Option<f64>,
    pub course: Option<f64>,
    /// UTC time of the fix (Unix s), when date and time are both present and valid.
    pub time: Option<f64>,
}

/// A parsed GGA sentence.
#[derive(Debug, Clone, PartialEq)]
pub struct Gga {
    /// Fix quality (0 = no fix; anything that isn't all digits reads as 0).
    pub quality: u64,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub sats: Option<u64>,
    pub hdop: Option<f64>,
    pub alt_m: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Sentence {
    Rmc(Rmc),
    Gga(Gga),
}

/// One NMEA sentence → RMC/GGA, else `None` (bad checksum, other sentence types, too few fields).
pub fn parse(sentence: &str) -> Option<Sentence> {
    let sentence = py::strip(sentence);
    if !checksum_ok(sentence) {
        return None;
    }
    let star = sentence.find('*')?;
    let f: Vec<&str> = sentence[1..star].split(',').collect();
    let kind = py::last_chars(f[0], 3);
    if kind == "RMC" && f.len() >= 10 {
        let time = if !f[1].is_empty() && !f[9].is_empty() && f[9].chars().count() == 6 {
            let hms = f[1].split('.').next().unwrap_or("");
            py::strptime_dmyhms_utc(&format!("{}{hms}", f[9]))
        } else {
            None
        };
        return Some(Sentence::Rmc(Rmc {
            valid: f[2] == "A",
            lat: coord(f[3], f[4]),
            lon: coord(f[5], f[6]),
            speed_kn: py::float(f[7]),
            course: py::float(f[8]),
            time,
        }));
    }
    if kind == "GGA" && f.len() >= 10 {
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        return Some(Sentence::Gga(Gga {
            // an all-digit value too big for u64 is certainly > 0, which is all `quality` is used for
            quality: if digits(f[6]) { f[6].parse().unwrap_or(u64::MAX) } else { 0 },
            lat: coord(f[2], f[3]),
            lon: coord(f[4], f[5]),
            sats: if digits(f[7]) { f[7].parse().ok() } else { None },
            hdop: py::float(f[8]),
            alt_m: py::float(f[9]),
        }));
    }
    None
}

/// The listener's state logic (Python `NmeaGps` without the socket).
#[derive(Debug, Clone)]
pub struct NmeaGps {
    /// Published as `location.source = "<source> NMEA"`.
    pub source: String,
    /// Seconds after the last fix at which [`Self::stale`] reports `location.fix = false`.
    pub stale_after: f64,
    last_fix: f64,
    sentences: u64,
    bad: u64,
}

impl Default for NmeaGps {
    fn default() -> Self {
        Self::new("peplink", 30.0)
    }
}

impl NmeaGps {
    pub fn new(source: &str, stale_after: f64) -> Self {
        Self { source: source.to_string(), stale_after, last_fix: 0.0, sentences: 0, bad: 0 }
    }

    /// Good RMC/GGA sentences so far (published as `gps.sentences`).
    pub fn sentences(&self) -> u64 {
        self.sentences
    }

    /// Lines that start with `$` but don't parse (bad checksum, unknown type, too short).
    pub fn bad(&self) -> u64 {
        self.bad
    }

    /// Wall time of the last valid RMC fix, 0 before the first.
    pub fn last_fix(&self) -> f64 {
        self.last_fix
    }

    /// One datagram: decoded as ASCII (a non-ASCII byte becomes U+FFFD) and split into lines as Python's
    /// `str.splitlines()`; the updates of every line, in order.
    pub fn feed_datagram(&mut self, data: &[u8], now: f64) -> Vec<(String, Value)> {
        let text: String = data.iter().map(|&b| if b.is_ascii() { b as char } else { '\u{FFFD}' }).collect();
        let mut out = Vec::new();
        for line in py::splitlines(&text) {
            out.extend(self.feed(line, now));
        }
        out
    }

    /// One sentence → the state updates, in Python's order (empty when it doesn't parse).
    pub fn feed(&mut self, line: &str, now: f64) -> Vec<(String, Value)> {
        let Some(r) = parse(line) else {
            if line.starts_with('$') {
                self.bad += 1;
            }
            return Vec::new();
        };
        self.sentences += 1;
        let mut up: Vec<(String, Value)> = vec![
            ("location.source".into(), json!(format!("{} NMEA", self.source))),
            ("gps.sentences".into(), json!(self.sentences)),
        ];
        match r {
            Sentence::Rmc(d) => {
                if d.valid && d.lat.is_some() {
                    self.last_fix = now;
                    // Python: (speed_kn or 0) * 1.852, km/h
                    let kn = d.speed_kn.filter(|v| *v != 0.0).unwrap_or(0.0);
                    up.extend([
                        ("location.fix".into(), json!(true)),
                        ("location.lat".into(), py::num(d.lat)),
                        ("location.lon".into(), py::num(d.lon)),
                        ("location.speed".into(), py::num(Some(py::round(kn * 1.852, 1)))),
                        ("location.heading".into(), py::num(d.course)),
                        ("location.time".into(), py::num(d.time)),
                    ]);
                } else {
                    up.push(("location.fix".into(), json!(false)));
                }
            }
            Sentence::Gga(d) => {
                up.push(("location.sats".into(), json!(d.sats)));
                up.push(("location.hdop".into(), py::num(d.hdop)));
                if d.quality > 0 && d.alt_m.is_some() {
                    up.push(("location.alt_m".into(), py::num(d.alt_m)));
                }
            }
        }
        up
    }

    /// The periodic check (Python: every 5 s): `location.fix = false` once the last fix is older than
    /// `stale_after`. `None` when there is nothing to set.
    pub fn stale(&self, now: f64) -> Option<(String, Value)> {
        (self.last_fix != 0.0 && now - self.last_fix > self.stale_after)
            .then(|| ("location.fix".to_string(), json!(false)))
    }
}
