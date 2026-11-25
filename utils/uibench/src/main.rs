//! `aetherrv-uibench`: the UI runtime benchmark (prompt §7/§8, docs/UI-RUNTIME-BENCHMARK.md). Bench Pi only, but for
//! boot-time (the workstation's side of a reboot) and the readings, which run anywhere.
//!
//! ```text
//! aetherrv-uibench run --runtime <name> --url <url> --out <file.json> [options] -- <runtime command…>
//!     the whole scenario set against one runtime: cold start, page switches, taps, fps, CPU, memory, screen power
//! aetherrv-uibench frames [--secs N] [--patch x,y,w,h,step]… [--grid cols,rows,size,step] [--no-capture] [--cmd C]
//!     calibration: presented frames and intervals, patch copies and their cost, and what a page posts to the
//!     report port (harness/uibench/calib.html posts its own rAF count) for N seconds; --cmd hands the page's
//!     bench module one command over its long poll (e.g. "presscost 12", harness/uibench/presscost.sh)
//! aetherrv-uibench globals
//!     the compositor's Wayland globals (which capture protocols it offers)
//! aetherrv-uibench touch-test <x> <y>
//!     create the touchscreen, show how sway sees it, tap once
//! aetherrv-uibench helper
//!     the root half (uinput, smaps_rollup); started by the others through `sudo -n`
//! aetherrv-uibench serve <dir> [--port 18082] [--gateway URL | --no-proxy] [--log]
//!     a bundle on 127.0.0.1 without installing it, /api and /ws passed to the gateway (serve.rs)
//! aetherrv-uibench boot-report <result.json> [armed] | boot-time <host> <addr> <out.json> <0|1>
//!     power-on → usable UI: the bench's figures after a reboot, and the reboot timed from the workstation (boot.rs)
//! aetherrv-uibench summarize <result.json>… | digest <what> [args] | wav-level <file.wav>
//!     the readings: summary.md, the harness scripts' one-line readings, a capture's level (summary.rs, digest.rs)
//! ```
//! Needs WAYLAND_DISPLAY (and SWAYSOCK) of the benchmark's sway session (harness/uibench/session.sh).

mod analysis;
mod boot;
mod capture;
mod digest;
mod helper;
mod pyfmt;
mod report;
mod run;
mod serve;
mod summary;
mod uinput;

use anyhow::{Result, bail};

pub fn mono_ns() -> u64 {
    clock_ns(libc::CLOCK_MONOTONIC)
}
pub fn real_ns() -> u64 {
    clock_ns(libc::CLOCK_REALTIME)
}
fn clock_ns(c: libc::clockid_t) -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(c, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}
/// CLOCK_REALTIME − CLOCK_MONOTONIC (ns), to place the page's wall-clock marks on the compositor's clock.
pub fn real_minus_mono() -> i128 {
    let a = mono_ns();
    let r = real_ns();
    let b = mono_ns();
    r as i128 - ((a as i128 + b as i128) / 2)
}

pub fn arg<'a>(args: &'a [String], k: &str) -> Option<&'a str> {
    args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).map(|s| s.as_str())
}
pub fn flag(args: &[String], k: &str) -> bool {
    args.iter().any(|a| a == k)
}

pub fn sway(args: &[&str]) -> String {
    std::process::Command::new("swaymsg").args(args).output().map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default()
}

fn frames(args: &[String]) -> Result<()> {
    let secs: f64 = arg(args, "--secs").unwrap_or("10").parse()?;
    let out = arg(args, "--output").map(String::from).or_else(|| std::env::var("OUTPUT").ok());
    let reports = report::Reports::listen(arg(args, "--port").unwrap_or("18099").parse()?)?;
    let rec = if flag(args, "--no-capture") { None } else { Some(capture::Recorder::start(out)?) };
    if let Some(rec) = &rec {
        let mut ps = vec![];
        for (i, p) in args.iter().enumerate().filter(|(_, a)| *a == "--patch").map(|(i, _)| (i, args.get(i + 1))) {
            let v: Vec<i32> = p.map(|s| s.split(',').map(|x| x.parse().unwrap_or(0)).collect()).unwrap_or_default();
            ps.push(capture::Patch { id: i as u32, rect: capture::Rect { x: v[0], y: v[1], w: v[2], h: v[3] }, step: *v.get(4).unwrap_or(&1) });
        }
        if let Some(g) = arg(args, "--grid") {
            let v: Vec<i32> = g.split(',').map(|x| x.parse().unwrap_or(0)).collect();
            let i = rec.info();
            ps.extend(analysis::grid_patches(capture::Rect { x: 0, y: 0, w: i.width, h: i.height }, v[0], v[1], v[2], v[3], 100));
        }
        rec.set_patches(ps);
        std::thread::sleep(std::time::Duration::from_millis(300));
        rec.clear();
    }
    let t0 = mono_ns();
    if let Some(c) = arg(args, "--cmd") {
        reports.command(c);
    }
    std::thread::sleep(std::time::Duration::from_secs_f64(secs));
    let page: Vec<serde_json::Value> = reports.since(t0).into_iter().map(|r| r.body).collect();
    let Some(rec) = rec else {
        println!("{}", serde_json::to_string(&serde_json::json!({"page": page}))?);
        return Ok(());
    };
    let (ticks, ups) = rec.drain();
    let info = rec.info();
    rec.stop();
    let s = analysis::fps_stats(&ticks, info.refresh_mhz);
    let mut per_patch = std::collections::BTreeMap::<u32, usize>::new();
    for u in &ups {
        *per_patch.entry(u.id).or_default() += 1;
    }
    println!("{}", serde_json::to_string(&serde_json::json!({
        "recorder": {"output": info.output, "size": [info.width, info.height], "refresh_mhz": info.refresh_mhz, "ticker": info.ticker,
                     "format": format!("{:#x}", info.format), "tick_cancels": info.tick_cancels, "patch_failed": info.patch_failed,
                     "read_us_p50": analysis::pct_u32(&info.read_us, 50.0), "read_us_p95": analysis::pct_u32(&info.read_us, 95.0),
                     "tick_delay_us_p50": analysis::pct_u32(&info.tick_delay_us, 50.0), "fence_us_p50": analysis::pct_u32(&info.fence_us, 50.0), "fence_us_p95": analysis::pct_u32(&info.fence_us, 95.0), "tick_delay_us_max": info.tick_delay_us.iter().max(),
                     "patch_updates": per_patch},
        "fps": s, "page": page,
    }))?);
    Ok(())
}

fn touch_test(args: &[String]) -> Result<()> {
    let (x, y): (i32, i32) = (args.first().map(|s| s.parse()).transpose()?.unwrap_or(100), args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(100));
    let o = sway(&["-t", "get_outputs", "-r"]);
    let v: serde_json::Value = serde_json::from_str(&o)?;
    let first = &v[0];
    let (w, h) = (first["current_mode"]["width"].as_i64().unwrap_or(1920) as i32, first["current_mode"]["height"].as_i64().unwrap_or(1200) as i32);
    let mut hp = helper::Helper::spawn()?;
    println!("{}", hp.req(&format!("touch {w} {h}"))?);
    std::thread::sleep(std::time::Duration::from_millis(800));
    let inputs = sway(&["-t", "get_inputs", "-r"]);
    let iv: serde_json::Value = serde_json::from_str(&inputs)?;
    for i in iv.as_array().into_iter().flatten() {
        println!("input {} type={} mapped={:?}", i["identifier"], i["type"], i.get("libinput").map(|l| l["send_events"].clone()));
    }
    let t0 = hp.touch(&format!("down {x} {y}"))?;
    std::thread::sleep(std::time::Duration::from_millis(100));
    let t1 = hp.touch("up")?;
    println!("tap at {x},{y}: down {t0} up {t1} (ns monotonic)");
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Ok(o) = std::process::Command::new("ip").args(["-br", "link"]).output()
        && String::from_utf8_lossy(&o.stdout).lines().any(|l| l.starts_with("hat0 ") || l.starts_with("hat1 "))
    {
        bail!("refused: this host has hat0/hat1, it is the van");
    }
    match args.first().map(|s| s.as_str()) {
        Some("helper") => helper::main(),
        Some("frames") => frames(&args[1..]),
        Some("touch-test") => touch_test(&args[1..]),
        Some("globals") => capture::globals(),
        Some("run") => run::main(&args[1..]),
        Some("serve") => serve::main(&args[1..]),
        Some("boot-report") => boot::report(&args[1..]),
        Some("boot-time") => boot::time(&args[1..]),
        Some("summarize") => summary::main(&args[1..]),
        Some("digest") => digest::main(&args[1..]),
        Some("wav-level") => digest::wav_level(&args[1..]),
        _ => {
            eprintln!("usage: aetherrv-uibench run|frames|globals|touch-test|helper|serve|boot-report|boot-time|summarize|digest|wav-level (see the source header)");
            std::process::exit(2);
        }
    }
}
