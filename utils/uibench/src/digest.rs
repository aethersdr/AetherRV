//! The harness scripts' one-line readings of the tool's own output, printed as they always were.
//!
//! `digest calibrate`, `digest perturb <page> <mode>`, `digest presscost <page> <runtime> <press variant>`: read
//! `aetherrv-uibench frames` output on stdin (calibrate.sh, perturb.sh, presscost.sh).
//! `digest outputs`: `swaymsg -t get_outputs -r` on stdin, one line per output (session.sh).
//! `digest poweron`: boot-time.sh result files named on stdin; every boot's total and the median (poweron.sh).
//! `wav-level <file.wav>`: a 16-bit capture's peak, its loudest 50 ms and its non-silent 50 ms windows.

use crate::pyfmt::{Py, fixed};
use anyhow::{Context, Result, bail};
use std::io::Read;

fn stdin() -> Result<String> {
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s)?;
    Ok(s)
}

/// statistics.median
fn median(mut v: Vec<f64>) -> Option<f64> {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    match n {
        0 => None,
        _ if n % 2 == 1 => Some(v[n / 2]),
        _ => Some((v[n / 2 - 1] + v[n / 2]) / 2.0),
    }
}

/// b − a as Python subtracts: integers stay integers
fn sub(b: &Py, a: &Py) -> Result<Py> {
    let int = |x: &Py| match x {
        Py::Int(i) => Some(*i),
        Py::Bool(b) => Some(*b as i128),
        _ => None,
    };
    match (int(b), int(a), b.num(), a.num()) {
        (Some(x), Some(y), _, _) => Ok(Py::Int(x - y)),
        (_, _, Some(x), Some(y)) => Ok(Py::Float(x - y)),
        _ => bail!("not a number: {} − {}", b.repr(), a.repr()),
    }
}

pub fn main(args: &[String]) -> Result<()> {
    let arg = |i: usize| args.get(i).map(String::as_str).unwrap_or("");
    match arg(0) {
        "calibrate" => print!("{}", calibrate(&Py::parse(&stdin()?)?)?),
        "perturb" => print!("{}", perturb(&Py::parse(&stdin()?)?, arg(1), arg(2))),
        "presscost" => {
            let (out, err) = presscost(&Py::parse(&stdin()?)?, arg(1), arg(2), arg(3));
            print!("{out}");
            eprint!("{err}");
        }
        "outputs" => {
            for o in Py::parse(&stdin()?)?.list() {
                println!("output {} {} power {}", o.at("name").str(), o.at("current_mode").str(), o.at("power").str());
            }
        }
        "poweron" => print!("{}", poweron(&stdin()?)?),
        _ => bail!("usage: digest calibrate | perturb <page> <mode> | presscost <page> <runtime> <press> | outputs | poweron"),
    }
    Ok(())
}

fn calibrate(d: &Py) -> Result<String> {
    let (f, r) = (d.at("fps"), d.at("recorder"));
    let rec: Vec<(String, Py)> = ["ticker", "fence_us_p50", "read_us_p50", "tick_delay_us_p50", "patch_updates"].iter()
        .map(|k| (k.to_string(), r.at(k).clone())).collect();
    let pg: Vec<&Py> = d.at("page").list().iter().filter(|p| p.at("kind").as_str() == Some("calib")).collect();
    let per_s = |k: &str| -> Result<Py> { Ok(Py::List(pg.windows(2).map(|w| sub(w[1].at(k), w[0].at(k))).collect::<Result<_>>()?)) };
    Ok(format!("presented: {} frames, fps {} interval {}\nrecorder: {}\npage rAF/s: {} paints/s: {}\n", f.at("frames").str(), f.at("fps_raw").str(),
        f.at("interval_ms").str(), Py::Dict(rec).str(), per_s("raf")?.str(), per_s("paints")?.str()))
}

fn perturb(d: &Py, page: &str, mode: &str) -> String {
    let mut iv: Vec<f64> = d.at("page").list().iter().filter(|p| p.at("why").as_str() == Some("raf"))
        .flat_map(|p| p.at("raf").at("iv").list()).filter_map(Py::num).collect();
    iv.sort_by(|a, b| a.partial_cmp(b).unwrap());
    // index round(p/100·(n−1)), ties to even
    let q = |p: f64| if iv.is_empty() { 0.0 } else { iv[((p / 100.0 * (iv.len() - 1) as f64).round_ties_even() as usize).min(iv.len() - 1)] };
    let fps = d.at("fps");
    let fps = if fps.has("fps_raw") { fps.at("fps_raw").str() } else { "-".into() };
    format!("{page:<8} {mode:<6} page rAF: n={} p50={} p95={} max={} ms | presented fps {fps}\n", iv.len(), fixed(q(50.0), 1), fixed(q(95.0), 1), fixed(q(100.0), 1))
}

/// (stdout, stderr): the reading, and the page's whole report
fn presscost(d: &Py, page: &str, rt: &str, press: &str) -> (String, String) {
    let press = if press.is_empty() { "-" } else { press };
    let Some(r) = d.at("page").list().iter().rfind(|p| p.at("kind").as_str() == Some("presscost")) else {
        return (format!("{page:<9} {rt:<8} {press:<10} no presscost report\n"), String::new());
    };
    if !r.at("sel").truthy() {
        return (format!("{page:<9} {rt:<8} no press target on this page\n"), String::new());
    }
    let v = |k: &str| r.at(k).list().iter().filter_map(Py::num).collect::<Vec<f64>>();
    let med = |k: &str| fixed(median(v(k)).unwrap_or(f64::NAN), 2);
    let mx = |k: &str, d: usize| fixed(v(k).into_iter().reduce(f64::max).unwrap_or(f64::NAN), d);
    let med1 = |k: &str| fixed(median(v(k)).unwrap_or(f64::NAN), 1);
    let size = r.at("rect").list().iter().skip(2).map(Py::str).collect::<Vec<_>>().join("x");
    (format!("{page:<9} {rt:<8} {press:<10} {:<22} {size}  style {} ms, layout {} ms (median of 10-toggle means; max {}); \
              rAF after the change p50 {} max {} ms, unchanged p50 {} max {} ms\n",
             r.at("sel").str(), med("style_ms"), med("layout_ms"), mx("layout_ms", 2), med1("frame_ms"), mx("frame_ms", 1), med1("control_ms"), mx("control_ms", 1)),
     format!("{}\n", r.dumps(None)))
}

fn poweron(files: &str) -> Result<String> {
    let mut o = String::new();
    let mut t = vec![];
    for f in files.split_whitespace() {
        let d = Py::parse(&std::fs::read_to_string(f).with_context(|| f.to_string())?)?;
        let (fw, ku) = (d.at("firmware_from_journal_s"), d.at("kernel_to_usable_s"));
        match (fw.num(), ku.num()) {
            (Some(a), Some(b)) => {
                t.push(a + b);
                o += &format!("  firmware {} + kernel → usable {} = {} s\n", fixed(a, 2), fixed(b, 2), fixed(a + b, 2));
            }
            _ => o += &format!("  {f}: incomplete (firmware {}, kernel → usable {})\n", fw.str(), ku.str()),
        }
    }
    if let Some(m) = median(t.clone()) {
        o += &format!("{} boots: median {} s, max {} s, {} over 15.0 s\n", t.len(), fixed(m, 2), fixed(t.iter().copied().fold(f64::MIN, f64::max), 2),
            t.iter().filter(|x| **x > 15.0).count());
    }
    Ok(o)
}

pub fn wav_level(args: &[String]) -> Result<()> {
    let f = args.first().context("usage: wav-level <file.wav>")?;
    print!("{}", wav_text(&std::fs::read(f).with_context(|| f.clone())?)?);
    Ok(())
}

/// "peak X dBFS, loudest RMS Y dBFS, N non-silent 50 ms windows (from A s to B s)"; RMS over 50 ms windows of all
/// channels' samples, a window above 100 (of 32768) counts as sound
fn wav_text(b: &[u8]) -> Result<String> {
    if b.len() < 12 || &b[..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        bail!("not a WAV file");
    }
    let (mut fmt, mut data) = (None, None);
    let mut i = 12;
    while i + 8 <= b.len() {
        let n = u32::from_le_bytes(b[i + 4..i + 8].try_into()?) as usize;
        let body = &b[i + 8..(i + 8).saturating_add(n).min(b.len())];
        match &b[i..i + 4] {
            b"fmt " if body.len() >= 16 => fmt = Some(body),
            b"data" => data = Some(body),
            _ => {}
        }
        i = i.saturating_add(8 + n + n % 2);
    }
    let (Some(fmt), Some(data)) = (fmt, data) else { bail!("no fmt or data chunk") };
    let ch = u16::from_le_bytes([fmt[2], fmt[3]]) as usize;
    let rate = u32::from_le_bytes(fmt[4..8].try_into()?) as usize;
    if u16::from_le_bytes([fmt[14], fmt[15]]) != 16 || ch == 0 {
        bail!("not 16-bit PCM");
    }
    let frame = 2 * ch;
    let s: Vec<i64> = data[..data.len() / frame * frame].as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes(*c) as i64).collect();
    let peak = s.iter().map(|x| x.abs()).max().unwrap_or(0);
    let win = rate / 20 * ch;
    let r: Vec<f64> = (0..s.len().saturating_sub(win)).step_by(win.max(1))
        .map(|i| ((s[i..i + win].iter().map(|x| x * x).sum::<i64>() as f64) / win as f64).sqrt()).collect();
    let db = |v: f64| if v != 0.0 { fixed(20.0 * (v / 32768.0).log10(), 1) } else { "-inf".into() };
    let loud: Vec<usize> = r.iter().enumerate().filter(|(_, v)| **v > 100.0).map(|(i, _)| i).collect();
    let span = match (loud.first(), loud.last()) {
        (Some(a), Some(z)) => format!(" (from {} s to {} s)", fixed(*a as f64 / 20.0, 2), fixed(*z as f64 / 20.0 + 0.05, 2)),
        _ => String::new(),
    };
    Ok(format!("peak {} dBFS, loudest RMS {} dBFS, {} non-silent 50 ms windows{span}\n", db(peak as f64), db(r.iter().copied().fold(0.0, f64::max)),
        loud.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn medians() {
        assert_eq!(median(vec![3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(vec![4.0, 1.0, 2.0, 3.0]), Some(2.5));
        assert_eq!(median(vec![]), None);
    }

    #[test]
    fn readings() {
        let d = Py::parse(r#"{"fps": {"fps_raw": 59.98}, "page": [{"why": "raf", "raf": {"iv": [16.6, 16.7, 33.4, 16.65]}}]}"#).unwrap();
        assert_eq!(perturb(&d, "home", "none"), "home     none   page rAF: n=4 p50=16.7 p95=33.4 max=33.4 ms | presented fps 59.98\n");
        let p = Py::parse(r#"{"page": [{"kind": "presscost", "sel": ".tile", "rect": [0, 0, 120, 48.5], "style_ms": [0.1, 0.3],
            "layout_ms": [0.2], "frame_ms": [16.7, 16.6, 16.8], "control_ms": []}]}"#).unwrap();
        let (o, e) = presscost(&p, "switches", "wpe", "");
        assert_eq!(o, "switches  wpe      -          .tile                  120x48.5  style 0.20 ms, layout 0.20 ms (median of 10-toggle means; \
            max 0.20); rAF after the change p50 16.7 max 16.8 ms, unchanged p50 nan max nan ms\n");
        assert!(e.starts_with(r#"{"kind": "presscost", "sel": ".tile""#));
    }

    #[test]
    fn wav() {
        let mut w = b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x40\x1f\0\0\x80\x3e\0\0\x02\0\x10\0data".to_vec(); // 8 kHz mono
        let s: Vec<i16> = (0..8000).map(|i| if (2000..2800).contains(&i) { if i % 2 == 0 { 3000 } else { -3000 } } else { 0 }).collect();
        w.extend_from_slice(&(s.len() as u32 * 2).to_le_bytes());
        s.iter().for_each(|x| w.extend_from_slice(&x.to_le_bytes()));
        assert_eq!(wav_text(&w).unwrap(), "peak -20.8 dBFS, loudest RMS -20.8 dBFS, 2 non-silent 50 ms windows (from 0.25 s to 0.35 s)\n");
    }
}
