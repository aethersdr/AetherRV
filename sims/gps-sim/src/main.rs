//! `aetherrv-sim-gps [--to 127.0.0.1:10110] [--lat 39.7392 --lon -104.9903] [--speed-kn 0] [--no-fix]`
//! Sends GPRMC + GPGGA once a second over UDP to a loopback address, like the router's NMEA forwarder. The default
//! position is a public landmark, not anyone's location. With `--speed-kn` the position drifts north-east.

use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn nmea(body: &str) -> String {
    let ck = body.bytes().fold(0u8, |a, b| a ^ b);
    format!("${body}*{ck:02X}\r\n")
}

fn dm(v: f64, lat: bool) -> String {
    let a = v.abs();
    let d = a.trunc();
    let m = (a - d) * 60.0;
    let h = if lat { if v >= 0.0 { "N" } else { "S" } } else if v >= 0.0 { "E" } else { "W" };
    if lat { format!("{:02}{:07.4},{h}", d as u32, m) } else { format!("{:03}{:07.4},{h}", d as u32, m) }
}

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let flag = |n: &str| a.iter().position(|x| x == n).and_then(|i| a.get(i + 1)).cloned();
    let to: SocketAddr = flag("--to").unwrap_or_else(|| "127.0.0.1:10110".into()).parse()?;
    if !to.ip().is_loopback() {
        anyhow::bail!("refusing {to}: loopback only");
    }
    let (mut lat, mut lon): (f64, f64) = (flag("--lat").map_or(Ok(39.7392), |s| s.parse())?, flag("--lon").map_or(Ok(-104.9903), |s| s.parse())?);
    let kn: f64 = flag("--speed-kn").map_or(Ok(0.0), |s| s.parse())?;
    let fix = !a.iter().any(|x| x == "--no-fix");
    let sock = UdpSocket::bind("127.0.0.1:0")?;
    loop {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let (h, m, s) = ((now / 3600) % 24, (now / 60) % 60, now % 60);
        let days = now / 86400;
        // civil date from days since 1970-01-01 (Howard Hinnant's algorithm)
        let z = days as i64 + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let mo = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + i64::from(mo <= 2);
        let t = format!("{h:02}{m:02}{s:02}.00");
        let date = format!("{d:02}{mo:02}{:02}", y % 100);
        let st = if fix { "A" } else { "V" };
        let rmc = nmea(&format!("GPRMC,{t},{st},{},{},{kn:.1},45.0,{date},,,A", dm(lat, true), dm(lon, false)));
        let gga = nmea(&format!("GPGGA,{t},{},{},{},09,0.9,1609.3,M,-17.0,M,,", dm(lat, true), dm(lon, false), if fix { 1 } else { 0 }));
        let _ = sock.send_to(rmc.as_bytes(), to);
        let _ = sock.send_to(gga.as_bytes(), to);
        let step = kn * 1.852 / 3600.0 / 111.0 / std::f64::consts::SQRT_2;
        lat += step;
        lon += step / lat.to_radians().cos();
        std::thread::sleep(Duration::from_secs(1));
    }
}
