//! Ruuvi tag advertisements (Bluetooth LE manufacturer data, company id 0x0499), decoded. Read-only: a tag only
//! broadcasts, nothing is ever sent to it.
//!
//! * **Data format 5** ("RAWv2", current firmware): temperature, humidity, pressure, acceleration, battery voltage, TX
//!   power, a movement counter and a measurement sequence number (which tells a new measurement from a repeated
//!   advertisement), and the tag's MAC.
//! * **Data format 3** ("RAWv1", older firmware): temperature, humidity, pressure, acceleration, battery voltage; no
//!   sequence number.
//!
//! Every other format byte is [`Error::Format`], reported by the caller rather than guessed at. Values the tag marks as
//! invalid (format 5's reserved "not available" codes) are `None`. Specification: docs.ruuvi.com, "Data format 5" and
//! "Data format 3"; the tests use its published test vectors.

/// Ruuvi Innovations' Bluetooth SIG company identifier (the manufacturer data key).
pub const COMPANY_ID: u16 = 0x0499;

#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    /// 5 or 3.
    pub format: u8,
    pub temp_c: Option<f64>,
    /// Relative humidity, %.
    pub humidity: Option<f64>,
    /// Pa.
    pub pressure_pa: Option<u32>,
    /// mG, x/y/z.
    pub accel_mg: Option<[i16; 3]>,
    /// V.
    pub battery_v: Option<f64>,
    /// dBm (format 5 only).
    pub tx_power_dbm: Option<i8>,
    /// Format 5 only.
    pub movement: Option<u8>,
    /// Format 5 only: increments with every new measurement.
    pub sequence: Option<u16>,
    /// The tag's own MAC as it reports it, "AA:BB:CC:DD:EE:FF" (format 5 only).
    pub mac: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// No data.
    Empty,
    /// A format this decoder doesn't know (e.g. a future tag): the byte.
    Format(u8),
    /// Too short for its format: (format, length).
    Short(u8, usize),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Empty => f.write_str("empty Ruuvi data"),
            Error::Format(b) => write!(f, "unknown Ruuvi data format 0x{b:02X}"),
            Error::Short(fmt, n) => write!(f, "Ruuvi data format {fmt}: {n} bytes is too short"),
        }
    }
}

impl std::error::Error for Error {}

/// Rounded to a sensible resolution for each quantity (the formats' own steps are 0.005 °C and 0.0025 %).
fn r(x: f64, digits: i32) -> f64 {
    let m = 10f64.powi(digits);
    (x * m).round() / m
}

fn be16(d: &[u8], i: usize) -> u16 {
    u16::from_be_bytes([d[i], d[i + 1]])
}

/// Decode the manufacturer data for [`COMPANY_ID`] (the bytes after the company id, as BlueZ's `ManufacturerData`
/// holds them).
pub fn decode(d: &[u8]) -> Result<Reading, Error> {
    match d.first() {
        None => Err(Error::Empty),
        Some(5) => format5(d),
        Some(3) => format3(d),
        Some(&b) => Err(Error::Format(b)),
    }
}

fn format5(d: &[u8]) -> Result<Reading, Error> {
    if d.len() < 24 {
        return Err(Error::Short(5, d.len()));
    }
    let t = be16(d, 1) as i16;
    let h = be16(d, 3);
    let p = be16(d, 5);
    let acc: [i16; 3] = [be16(d, 7) as i16, be16(d, 9) as i16, be16(d, 11) as i16];
    let power = be16(d, 13);
    let (bat, tx) = (power >> 5, power & 0x1F);
    let mac = d[18..24].iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":");
    Ok(Reading {
        format: 5,
        temp_c: (t != i16::MIN).then(|| r(t as f64 * 0.005, 3)),
        humidity: (h != 0xFFFF).then(|| r(h as f64 * 0.0025, 4)),
        pressure_pa: (p != 0xFFFF).then(|| p as u32 + 50_000),
        accel_mg: acc.iter().all(|a| *a != i16::MIN).then_some(acc),
        battery_v: (bat != 0x7FF).then(|| r((bat as f64 + 1600.0) / 1000.0, 3)),
        tx_power_dbm: (tx != 0x1F).then(|| -40 + 2 * tx as i8),
        movement: (d[15] != 0xFF).then_some(d[15]),
        sequence: (be16(d, 16) != 0xFFFF).then(|| be16(d, 16)),
        mac: (mac != "FF:FF:FF:FF:FF:FF").then_some(mac),
    })
}

fn format3(d: &[u8]) -> Result<Reading, Error> {
    if d.len() < 14 {
        return Err(Error::Short(3, d.len()));
    }
    // temperature: sign-and-magnitude integer part (bit 7 = negative), then hundredths
    let whole = (d[2] & 0x7F) as f64 + d[3] as f64 / 100.0;
    let temp = if d[2] & 0x80 != 0 { -whole } else { whole };
    Ok(Reading {
        format: 3,
        temp_c: Some(r(temp, 2)),
        humidity: Some(d[1] as f64 * 0.5),
        pressure_pa: Some(be16(d, 4) as u32 + 50_000),
        accel_mg: Some([be16(d, 6) as i16, be16(d, 8) as i16, be16(d, 10) as i16]),
        battery_v: Some(r(be16(d, 12) as f64 / 1000.0, 3)),
        tx_power_dbm: None,
        movement: None,
        sequence: None,
        mac: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// docs.ruuvi.com, Data format 5, test vector "valid data".
    #[test]
    fn format5_valid() {
        let r = decode(&hex("0512FC5394C37C0004FFFC040CAC364200CDCBB8334C884F")).unwrap();
        assert_eq!(r.format, 5);
        assert_eq!((r.temp_c, r.humidity, r.pressure_pa), (Some(24.3), Some(53.49), Some(100_044)));
        assert_eq!(r.accel_mg, Some([4, -4, 1036]));
        assert_eq!((r.battery_v, r.tx_power_dbm), (Some(2.977), Some(4)));
        assert_eq!((r.movement, r.sequence), (Some(66), Some(205)));
        assert_eq!(r.mac.as_deref(), Some("CB:B8:33:4C:88:4F"));
    }

    /// Test vectors "maximum values" and "minimum values".
    #[test]
    fn format5_limits() {
        let r = decode(&hex("057FFFFFFEFFFE7FFF7FFF7FFFFFDEFEFFFECBB8334C884F")).unwrap();
        assert_eq!((r.temp_c, r.humidity, r.pressure_pa), (Some(163.835), Some(163.835), Some(115_534)));
        assert_eq!(r.accel_mg, Some([32767, 32767, 32767]));
        assert_eq!((r.battery_v, r.tx_power_dbm, r.movement, r.sequence), (Some(3.646), Some(20), Some(254), Some(65534)));
        let r = decode(&hex("058001000000008001800180010000000000CBB8334C884F")).unwrap();
        assert_eq!((r.temp_c, r.humidity, r.pressure_pa), (Some(-163.835), Some(0.0), Some(50_000)));
        assert_eq!(r.accel_mg, Some([-32767, -32767, -32767]));
        assert_eq!((r.battery_v, r.tx_power_dbm, r.movement, r.sequence), (Some(1.6), Some(-40), Some(0), Some(0)));
    }

    /// Test vector "invalid values": every field marked not available.
    #[test]
    fn format5_invalid_values_are_none() {
        let r = decode(&hex("058000FFFFFFFF800080008000FFFFFFFFFFFFFFFFFFFFFF")).unwrap();
        assert_eq!(r.format, 5);
        assert!(r.temp_c.is_none() && r.humidity.is_none() && r.pressure_pa.is_none() && r.accel_mg.is_none());
        assert!(r.battery_v.is_none() && r.tx_power_dbm.is_none() && r.movement.is_none() && r.sequence.is_none());
        assert!(r.mac.is_none());
    }

    /// docs.ruuvi.com, Data format 3, test vectors.
    #[test]
    fn format3_vectors() {
        let r = decode(&hex("03291A1ECE1EFC18F94202CA0B53")).unwrap();
        assert_eq!(r.format, 3);
        assert_eq!((r.humidity, r.temp_c, r.pressure_pa), (Some(20.5), Some(26.3), Some(102_766)));
        assert_eq!((r.accel_mg, r.battery_v), (Some([-1000, -1726, 714]), Some(2.899)));
        assert!(r.sequence.is_none() && r.mac.is_none());
        let max = decode(&hex("03FF7F63FFFF7FFF7FFF7FFFFFFF")).unwrap();
        assert_eq!((max.humidity, max.temp_c, max.pressure_pa, max.battery_v), (Some(127.5), Some(127.99), Some(115_535), Some(65.535)));
        let min = decode(&hex("0300FF6300008001800180010000")).unwrap();
        assert_eq!((min.humidity, min.temp_c, min.pressure_pa, min.battery_v), (Some(0.0), Some(-127.99), Some(50_000), Some(0.0)));
        assert_eq!(min.accel_mg, Some([-32767, -32767, -32767]));
    }

    #[test]
    fn unknown_short_and_empty_are_errors() {
        assert_eq!(decode(&[]), Err(Error::Empty));
        assert_eq!(decode(&hex("E1000000")), Err(Error::Format(0xE1)));
        assert_eq!(decode(&hex("0512FC5394")), Err(Error::Short(5, 5)));
        assert_eq!(decode(&hex("03291A")), Err(Error::Short(3, 3)));
        assert_eq!(Error::Format(0xE1).to_string(), "unknown Ruuvi data format 0xE1");
    }
}
