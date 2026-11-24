//! The scenario runner: one runtime, every page, every scenario, one JSON result.
//!
//! ```text
//! aetherrv-uibench run --runtime <name> --url <url> --out <file.json> [--mode headless|screen] [--label <text>]
//!     [--pages home,energy,…] [--warm-rounds 5] [--press-reps 8] [--null-reps 4] [--fps-secs 8] [--hold-ms 300]
//!     [--screen-power] [--port 18099] -- <runtime command…>
//! ```
//! Order: cold start (launch → first frame → first usable frame), the slide-off check, then per page: first visit
//! (a cold page switch), idle fps + CPU + memory, presses with no-tap control trials; then warm page-switch rounds,
//! final memory, and optionally screen power off/on. The runtime runs in its own transient user scope, so its whole
//! process tree is one cgroup.
//!
//! Touches never produce a click on a page control: presses hold, then slide off before lifting (a moved touch is
//! not a tap in any of the runtimes; the slide-off check at the start verifies it on a rail button). The only real
//! taps are on rail buttons (navigation).

use crate::analysis::{self, MosaicRef, SwitchParams, ref_patches, timeline};
use crate::capture::{Patch, Recorder, Rect, Update};
use crate::helper::Helper;
use crate::report::Reports;
use crate::{arg, flag, mono_ns, real_minus_mono, sway};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::time::Duration;

fn sleep_ms(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

struct Ctx {
    rec: Recorder,
    rep: Reports,
    hp: Helper,
    refresh: i32,
    rt_cg: String,
    sway_cg: String,
    off: (i32, i32),
    dpr: f64,
    hold_ms: u64,
    log: Vec<String>,
}

impl Ctx {
    fn note(&mut self, s: String) {
        eprintln!("uibench: {s}");
        self.log.push(s);
    }
    fn to_screen(&self, r: &Value) -> Option<Rect> {
        let a = r.as_array()?;
        let f = |i: usize| a.get(i).and_then(Value::as_f64).unwrap_or(0.0) * self.dpr;
        Some(Rect { x: f(0) as i32 + self.off.0, y: f(1) as i32 + self.off.1, w: f(2) as i32, h: f(3) as i32 })
    }
    fn sample(&mut self) -> Value {
        let cgs = vec![self.rt_cg.clone(), self.sway_cg.clone()];
        match self.hp.sample(&cgs) {
            Ok(v) => json!({"runtime": v.first(), "sway": v.get(1)}),
            Err(e) => json!({"error": format!("{e:#}")}),
        }
    }
    fn tap(&mut self, x: i32, y: i32, hold: u64) -> Result<u64> {
        let t0 = self.hp.touch(&format!("down {x} {y}"))?;
        sleep_ms(hold);
        self.hp.touch("up")?;
        Ok(t0)
    }
    /// Press, hold, slide 120 px down in 4 steps, lift: no tap gesture, so no click.
    fn press_slide(&mut self, x: i32, y: i32, hold: u64) -> Result<u64> {
        let t0 = self.hp.touch(&format!("down {x} {y}"))?;
        sleep_ms(hold);
        for i in 1..=4 {
            self.hp.touch(&format!("move {x} {}", y + 30 * i))?;
            sleep_ms(16);
        }
        self.hp.touch("up")?;
        Ok(t0)
    }
}

fn pss_mb(s: &Value, who: &str) -> Option<f64> {
    s.get(who)?.get("pss_kb")?.as_f64().map(|k| analysis::r2(k / 1024.0))
}

fn cpu_pct(a: &Value, b: &Value, who: &str) -> Option<f64> {
    let ua = a.get(who)?.get("cpu_usec")?.as_f64()?;
    let ub = b.get(who)?.get("cpu_usec")?.as_f64()?;
    let ta = a.get(who)?.get("t")?.as_f64()?;
    let tb = b.get(who)?.get("t")?.as_f64()?;
    Some(analysis::r2((ub - ua) / ((tb - ta) / 1000.0) * 100.0))
}

fn epoch_ms_to_mono(ms: f64, off: i128) -> u64 {
    ((ms * 1e6) as i128 - off).max(0) as u64
}

/// The newest page report for `view` received after `t` (bench module, after the view settled).
fn page_report(rep: &Reports, t: u64, view: Option<&str>, ms: u64) -> Option<Value> {
    rep.wait(t, ms, |b| b["kind"] == "page" && view.is_none_or(|v| b["view"] == v)).map(|r| r.body)
}

const CONTENT_TAU: f64 = 0.08;
const TAP_HOLD_MS: u64 = 60;
const SETTLED_TAU: f64 = 0.004;
const PIX_THR: u8 = 24;

fn measure_switch(c: &mut Ctx, views: Rect, rail: Rect, page: &str, window_ms: u64, scene: Option<Rect>) -> Result<Value> {
    let patches = analysis::bands(views, 1);
    let refs = ref_patches(&patches, 12, 1000);
    c.rec.set_patches(patches.clone());
    let base = c.rec.force(patches.clone())?;
    sleep_ms(120);
    c.rec.clear();
    let (x, y) = rail.center();
    // navigation happens on click, i.e. at release: times are from the release (the tap's down→up is recorded)
    let t_down = c.hp.touch(&format!("down {x} {y}"))?;
    sleep_ms(TAP_HOLD_MS);
    let t0 = c.hp.touch("up")?;
    let _ = c.rec.take_fence_ms();
    sleep_ms(window_ms);
    let fin = c.rec.force(refs.clone())?;
    let (ticks, ups) = c.rec.drain();
    c.rec.set_patches(vec![]);
    let tl = timeline(&patches, &base, &ups, t0);
    let base_grid: Vec<u8> = patches.iter().flat_map(|p| base.iter().find(|u| u.id == p.id).map(|u| u.px.to_vec()).unwrap_or_default()).collect();
    let Some(mut r) = MosaicRef::new(&patches, &refs, &fin) else { bail!("reference capture incomplete") };
    let t_end = t0 + window_ms * 1_000_000;
    let settle: Vec<&[u8]> = tl.iter().filter(|(t, _)| *t + 500_000_000 >= t_end).map(|(_, g)| g.as_slice()).collect();
    r.mask_live(&settle, PIX_THR);
    let p = SwitchParams { tau_content: CONTENT_TAU, min_alpha: 0.25, tau_settled: SETTLED_TAU, thr: PIX_THR };
    let s = analysis::switch(&tl, &base_grid, &r, t0, &ticks, &p, c.refresh);
    let mut v = serde_json::to_value(&s)?;
    v["page"] = json!(page);
    // the same switch with the page's scene (its canvas / svg) left out: the rest of the page vs the scene's own start
    if let Some(sr) = scene {
        r.mask_rect(&sr);
        let s2 = analysis::switch(&tl, &base_grid, &r, t0, &ticks, &p, c.refresh);
        v["without_scene"] = json!({"content_ms": s2.content_ms, "settled_ms": s2.settled_ms, "live_frac": s2.live_frac});
    }
    v["tap_down_to_up_ms"] = json!(analysis::ms(t_down, t0));
    v["gpu_fence_ms_p50"] = json!(c.rec.take_fence_ms());
    Ok(v)
}

fn measure_press(c: &mut Ctx, target: Rect, slide: bool, null: bool) -> Result<Value> {
    // the control itself, at most 320×200 around its centre, every 2nd pixel
    let (cx, cy) = target.center();
    let w = target.w.min(320);
    let h = target.h.min(200);
    let patch = Patch { id: 7, rect: Rect { x: cx - w / 2, y: cy - h / 2, w, h }, step: 2 };
    c.rec.set_patches(vec![patch.clone()]);
    let base = c.rec.force(vec![patch.clone()])?;
    let base_px = base.first().map(|u| u.px.to_vec()).unwrap_or_default();
    sleep_ms(150);
    c.rec.clear();
    let hold = c.hold_ms;
    let t0 = if null {
        let t = mono_ns();
        sleep_ms(hold + 64);
        t
    } else if slide {
        c.press_slide(cx, cy, hold)?
    } else {
        c.tap(cx, cy, hold)?
    };
    sleep_ms(250);
    let (ticks, ups) = c.rec.drain();
    c.rec.set_patches(vec![]);
    let p = analysis::press(&ups, 7, &base_px, t0, t0 + hold * 1_000_000, 16, 0.02);
    let input = if null { None } else { c.rep.wait(t0, 800, |b| b["kind"] == "input" && b["type"] == "pointerdown").map(|r| r.body) };
    // if the patch stream skipped a presented frame before the detection, the press may have shown earlier: an upper bound
    let mut v = serde_json::to_value(&p)?;
    if let Some(i) = &input {
        let off = real_minus_mono();
        let at = |k: &str| i[k].as_f64().map(|e| analysis::ms(t0, epoch_ms_to_mono(e, off)));
        v["handler_ms"] = json!(at("handler_epoch"));                 // injection → the page's pointerdown handler
        v["event_ts_ms"] = json!(at("event_epoch"));                  // injection → the event's own timestamp
    }
    if let Some(l) = p.latency_ms {
        let td = t0 + (l * 1e6) as u64;
        let frames = ticks.iter().filter(|&&t| t >= t0 && t <= td).count();
        let copies = ups.iter().filter(|u| u.id == 7 && u.t >= t0 && u.t <= td).count();
        v["frames_to_detect"] = json!(frames);
        v["copies_to_detect"] = json!(copies);
        v["upper_bound"] = json!(frames > copies);
    }
    sleep_ms(200);
    Ok(v)
}

/// Idle window, two parts. (a) ticker only, nothing copied: every presented frame, CPU and memory. (b) five patch
/// streams (four bands across the view, one inside the page's scene: its largest canvas or svg): the frames in
/// which the page's content, and the scene itself, actually changed. (b) is what tells the radar loop or the 3D
/// sky apart from page-wide animations such as the critical-alert flash, which present a frame every refresh.
fn measure_fps(c: &mut Ctx, secs: f64, views: Rect, scene: Option<Rect>, stim: Option<Rect>) -> Result<Value> {
    // (z) nothing captured at all: the runtime's CPU, and the page's own requestAnimationFrame cadence (asked for
    // over the report port's long poll), with no instrument cost
    c.rec.set_patches(vec![]);
    c.rec.set_ticker(false);
    sleep_ms(300);
    let z0 = c.sample();                                           // (z1) CPU only: nothing captured, nothing sampled
    sleep_ms((secs * 1000.0) as u64);
    let z1 = c.sample();
    let t_cmd = mono_ns();                                         // (z2) the page's rAF cadence, still nothing captured
    c.rep.command(&format!("raf {}", (secs * 1000.0) as u64));
    let r0 = c.sample();
    sleep_ms((secs * 1000.0) as u64);
    let r1 = c.sample();
    let raf = c.rep.wait(t_cmd, 3000, |b| b["kind"] == "raf").map(|r| r.body);
    c.rep.clear_command();
    c.rec.set_ticker(true);
    let raf_iv: Vec<f64> = raf.as_ref().and_then(|b| b["iv"].as_array().cloned()).unwrap_or_default().iter().filter_map(Value::as_f64).collect();
    let period = if c.refresh > 0 { 1e6 / c.refresh as f64 } else { 1000.0 / 60.0 };
    let raf_v = json!({"n": raf_iv.len(), "interval_ms": analysis::summary(&raf_iv),
        "fps": analysis::pct(&raf_iv, 50.0).map(|m| analysis::r2(1000.0 / m)),
        "long_frames": raf_iv.iter().filter(|&&x| x > 1.5 * period).count()});
    // (a) the ticker only
    c.rec.clear();
    let a = c.sample();
    sleep_ms((secs * 1000.0) as u64);
    let b = c.sample();
    let (ticks, _) = c.rec.drain();
    let f = analysis::fps_stats(&ticks, c.refresh);
    // the scene alone (one copy job per frame), then the page bands alone
    let sc = scene.map(|r| {
        let (cx, cy) = r.center();
        let (w, h) = (r.w.min(320), r.h.min(200));
        Patch { id: 30, rect: Rect { x: cx - w / 2, y: cy - h / 2, w, h }, step: 2 }
    });
    let mut scene_t = vec![];
    let mut scene_stim_on = false;
    if let Some(p) = &sc {
        // the page's own stimulus first (Weather: Play, the radar loop doesn't start by itself), undone afterwards;
        // only undone if it took (with no radar frames loaded, Play does nothing and a second tap would start it later)
        let mut stim_on = false;
        if let Some(s) = stim {
            let (x, y) = s.center();
            let t = mono_ns();
            c.tap(x, y, 60)?;
            stim_on = c.rep.wait(t, 2000, |b| b["kind"] == "stim" && b["playing"] == true).is_some();
            scene_stim_on = stim_on;
            if !stim_on {
                c.note("stimulus: the Weather loop did not start (no radar frames?)".into());
            }
            sleep_ms(1500);
        }
        c.rec.set_patches(vec![p.clone()]);
        sleep_ms(SCENE_SECS * 1000);
        let (_, ups) = c.rec.drain();
        c.rec.set_patches(vec![]);
        scene_t = analysis::change_times(&ups, &[30], 4);
        if let (Some(s), true) = (stim, stim_on) {
            let (x, y) = s.center();
            c.tap(x, y, 60)?;
            sleep_ms(500);
        }
    }
    let inner = Rect { x: views.x + 16, y: views.y + 16, w: views.w - 32, h: views.h - 32 };
    c.rec.set_patches(analysis::bands(inner, 20));
    sleep_ms(CONTENT_SECS * 1000);
    let (_, ups) = c.rec.drain();
    c.rec.set_patches(vec![]);
    let page_t = analysis::change_times(&ups, &[20, 21, 22, 23], 4);
    Ok(json!({
        "zero_capture": {"cpu_pct_runtime": cpu_pct(&z0, &z1, "runtime"), "cpu_pct_sway": cpu_pct(&z0, &z1, "sway"), "raf": raf_v,
                         "cpu_pct_runtime_while_raf": cpu_pct(&r0, &r1, "runtime")},
        "fps": f, "cpu_pct_runtime": cpu_pct(&a, &b, "runtime"), "cpu_pct_sway": cpu_pct(&a, &b, "sway"),
        "pss_mb_runtime": pss_mb(&b, "runtime"), "procs": b["runtime"]["n"],
        "content_fps": analysis::fps_stats(&page_t, c.refresh), "content_secs": CONTENT_SECS, "scene_secs": SCENE_SECS,
        "scene": sc.as_ref().map(|p| json!(p.rect)), "scene_full": scene.map(|r| json!([r.x, r.y, r.w, r.h])),
        "stimulus": stim.is_some(), "stimulus_took": scene_stim_on, "scene_fps": sc.as_ref().map(|_| analysis::fps_stats(&scene_t, c.refresh)),
        "gpu_fence_ms_p50": c.rec.take_fence_ms(),
    }))
}

const CONTENT_SECS: u64 = 3;
const SCENE_SECS: u64 = 4;

/// Stops the runtime's scope however the run ends.
struct ScopeGuard(String);
impl Drop for ScopeGuard {
    fn drop(&mut self) {
        let _ = std::process::Command::new("systemctl").args(["--user", "stop", &format!("{}.scope", self.0)]).status();
    }
}

fn cgroup_of(pid: u32) -> Option<String> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let p = s.lines().find_map(|l| l.strip_prefix("0::"))?;
    Some(format!("/sys/fs/cgroup{p}"))
}

fn window_offset() -> (i32, i32) {
    let t: Value = serde_json::from_str(&sway(&["-t", "get_tree", "-r"])).unwrap_or(Value::Null);
    fn find(n: &Value) -> Option<(i32, i32)> {
        if n["fullscreen_mode"].as_i64().unwrap_or(0) > 0 && n["pid"].is_number() {
            return Some((n["rect"]["x"].as_i64()? as i32, n["rect"]["y"].as_i64()? as i32));
        }
        for k in ["nodes", "floating_nodes"] {
            for c in n[k].as_array().into_iter().flatten() {
                if let Some(r) = find(c) {
                    return Some(r);
                }
            }
        }
        None
    }
    find(&t).unwrap_or((0, 0))
}

fn meta() -> Value {
    let sh = |c: &str| std::process::Command::new("sh").args(["-c", c]).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    json!({
        "kernel": sh("uname -r"), "model": sh("tr -d '\\0' < /proc/device-tree/model"),
        "loadavg": sh("cat /proc/loadavg"), "sway": sh("sway --version"),
        "outputs": serde_json::from_str::<Value>(&sway(&["-t", "get_outputs", "-r"])).ok().map(|o| o.as_array().map(|a| a.iter().map(|x| json!({
            "name": x["name"], "current_mode": x["current_mode"], "power": x["power"], "scale": x["scale"]})).collect::<Vec<_>>())),
        "bench_units_restarts": sh("systemctl show -p Id -p NRestarts -p ActiveState 'aetherrv-*' 2>/dev/null | paste -sd' ' | sed 's/Id=/\\n/g' | awk 'NF' | head -40"),
        "gateway_keys": sh("curl -s -m 2 127.0.0.1:8080/api/state | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))' 2>/dev/null"),
        // a critical alert makes the whole page flash (body.flash-critical), which keeps every page at 60 fps
        "alert_severities": sh("curl -s -m 2 127.0.0.1:8080/api/state | python3 -c 'import json,sys,collections; a=json.load(sys.stdin).get(\"alerts.active\") or []; print(dict(collections.Counter((x.get(\"severity\"), x.get(\"acked\")) for x in a)))' 2>/dev/null"),
    })
}

pub fn main(args: &[String]) -> Result<()> {
    let split = args.iter().position(|a| a == "--").ok_or_else(|| anyhow!("missing -- <runtime command>"))?;
    let (opts, cmd) = (&args[..split], &args[split + 1..]);
    let runtime = arg(opts, "--runtime").unwrap_or("runtime").to_string();
    let url = arg(opts, "--url").unwrap_or("").to_string();
    let out = arg(opts, "--out").context("--out")?.to_string();
    let pages: Vec<String> = arg(opts, "--pages").unwrap_or("home,energy,climate,weather,switches,network,alerts,systems,settings").split(',').map(String::from).collect();
    let warm_rounds: usize = arg(opts, "--warm-rounds").unwrap_or("5").parse()?;
    let press_reps: usize = arg(opts, "--press-reps").unwrap_or("8").parse()?;
    // --press-reps-for page=N[,page=N]: more (or fewer) presses on the pages named, e.g. network=100
    let press_reps_for: std::collections::HashMap<String, usize> = arg(opts, "--press-reps-for").unwrap_or("").split(',')
        .filter_map(|kv| kv.split_once('=').and_then(|(k, v)| Some((k.to_string(), v.parse().ok()?)))).collect();
    let null_reps: usize = arg(opts, "--null-reps").unwrap_or("4").parse()?;
    let fps_secs: f64 = arg(opts, "--fps-secs").unwrap_or("8").parse()?;
    let port: u16 = arg(opts, "--port").unwrap_or("18099").parse()?;
    // --cold-only: the cold start and nothing else (the power-on measurement; works for a page with no bench
    // module, e.g. the Python UI served by its own backend: then no page report ever comes, so wait only
    // --report-wait-ms for one before taking the settled reference)
    let cold_only = flag(opts, "--cold-only");
    // --no-scope: start the runtime as a plain child, not in its own `systemd-run --user` scope (a session with no
    // user service manager, pam_systemd class=user-light): no per-runtime memory or CPU figures then
    let no_scope = flag(opts, "--no-scope");
    let report_wait: u64 = arg(opts, "--report-wait-ms").unwrap_or("60000").parse()?;
    let output = std::env::var("OUTPUT").ok();
    let started = chrono_like();
    let t_main = mono_ns();                  // the runner's own set-up before it launches the runtime, itemised
    // --launch-first (with --cold-only): start the runtime at once, as the panel's launch.sh would, and attach the
    // capture while it starts (the power-on measurement: the runner's set-up must not delay the launch). The cold
    // start's reference "before" frame is then the black output, not a captured one.
    let launch_first = flag(opts, "--launch-first");
    let unit = format!("uibench-rt-{}-{}", runtime, std::process::id());
    let spawn = || -> Result<std::process::Child> {
        let mut b = if no_scope {
            let mut b = std::process::Command::new(&cmd[0]);
            b.args(&cmd[1..]);
            b
        } else {
            let mut b = std::process::Command::new("systemd-run");
            b.args(["--user", "--scope", "--quiet", "--collect", &format!("--unit={unit}"), "--"]).args(cmd);
            b
        };
        Ok(b.stdout(std::process::Stdio::null()).stderr(std::fs::File::create(format!("{out}.stderr.log"))?).spawn()?)
    };
    let early = if launch_first { Some((mono_ns(), spawn()?)) } else { None };

    let rec = Recorder::start(output.clone())?;
    let t_rec = mono_ns();
    let rep = Reports::listen(port)?;
    let mut hp = Helper::spawn()?;
    let t_help = mono_ns();
    let info = rec.info();
    // --no-touch: no injected touchscreen (the boot measurement: a headless session with no seat, nothing tapped)
    let touch = !flag(opts, "--no-touch");
    if touch {
        let r = hp.req(&format!("touch {} {}", info.width, info.height))?;
        if !r.starts_with("ok") {
            bail!("touchscreen: {r}");
        }
        let mut mapped = false;
        for _ in 0..30 {
            if sway(&["-t", "get_inputs", "-r"]).contains("uibench_touchscreen") {
                mapped = true;
                break;
            }
            sleep_ms(100);
        }
        if !mapped {
            bail!("sway does not see the uinput touchscreen");
        }
    }
    // sway's own cgroup: PAMName puts it in its login session's scope, not in the service's cgroup
    let sway_pid = std::env::var("SWAYSOCK").ok().and_then(|p| p.rsplit('.').nth(1).and_then(|x| x.parse::<u32>().ok()));
    let sway_cg = std::env::var("UIBENCH_SWAY_CG").ok().or_else(|| sway_pid.and_then(cgroup_of)).unwrap_or_default();
    let mut c = Ctx { rec, rep, hp, refresh: info.refresh_mhz, rt_cg: String::new(), sway_cg, off: (0, 0), dpr: 1.0, hold_ms: arg(opts, "--hold-ms").unwrap_or("300").parse()?, log: vec![] };
    let meta_before = meta();

    // ── cold start ──
    let full = Rect { x: 0, y: 0, w: info.width, h: info.height };
    let cpatches = analysis::bands(full, 1);
    let crefs = ref_patches(&cpatches, 0, 1000);
    c.rec.set_patches(cpatches.clone());
    let cbase = if launch_first {
        cpatches.iter().map(|p| { let (w, h) = p.dims(); Update { id: p.id, t: 0, recv: 0, px: std::sync::Arc::new(vec![0u8; w * h * 3]) } }).collect()
    } else {
        c.rec.force(cpatches.clone())?
    };
    let t_base = mono_ns();
    if !launch_first {
        sleep_ms(200);
        c.rec.clear();
    }
    let (t_launch, mut child) = match early {
        Some(e) => e,
        None => (mono_ns(), spawn()?),
    };
    let pid = child.id();
    let _guard = ScopeGuard(unit.clone());
    for _ in 0..if no_scope { 0 } else { 50 } {
        if let Some(cg) = cgroup_of(pid)
            && cg.contains(&unit)
        {
            c.rt_cg = cg;
            break;
        }
        sleep_ms(20);
    }
    if c.rt_cg.is_empty() {
        c.note(format!("runtime pid {pid} not in its scope; memory/CPU figures unavailable"));
    }
    let ready = page_report(&c.rep, t_launch, None, report_wait);
    let t_ready = mono_ns();
    sleep_ms(4000);
    let cfin = c.rec.force(crefs.clone())?;
    let (cticks, cups) = c.rec.drain();
    c.rec.set_patches(vec![]);
    let off = real_minus_mono();
    let cold = {
        let tl = timeline(&cpatches, &cbase, &cups, t_launch);
        let base_grid: Vec<u8> = cpatches.iter().flat_map(|p| cbase.iter().find(|u| u.id == p.id).map(|u| u.px.to_vec()).unwrap_or_default()).collect();
        let mut v = json!({});
        if let Some(mut r) = MosaicRef::new(&cpatches, &crefs, &cfin) {
            let t_end = mono_ns();
            let settle: Vec<&[u8]> = tl.iter().filter(|(t, _)| *t + 1_500_000_000 >= t_end).map(|(_, g)| g.as_slice()).collect();
            r.mask_live(&settle, PIX_THR);
            let p = SwitchParams { tau_content: CONTENT_TAU, min_alpha: 0.25, tau_settled: SETTLED_TAU, thr: PIX_THR };
            let s = analysis::switch(&tl, &base_grid, &r, t_launch, &cticks, &p, c.refresh);
            v = serde_json::to_value(&s)?;
            let first_paint = tl.iter().find(|(_, g)| analysis::diff_frac(&base_grid, g, PIX_THR) > 0.01);
            v["first_paint_ms"] = json!(first_paint.map(|(t, _)| analysis::ms(t_launch, *t)));
            // a light frame before the content is the white flash the boot splash hand-off would show
            let content_t = s.content_ms.map(|m| t_launch + (m * 1e6) as u64).unwrap_or(u64::MAX);
            let max_luma = tl.iter().filter(|(t, _)| *t < content_t).map(|(_, g)| analysis::mean_luma(g)).fold(0.0, f64::max);
            v["max_luma_before_content"] = json!(analysis::r2(max_luma));
            v["white_flash"] = json!(max_luma > 150.0);
        }
        v["first_frame_ms"] = json!(cticks.iter().find(|&&t| t >= t_launch).map(|&t| analysis::ms(t_launch, t)));
        // CLOCK_MONOTONIC counts from (about) the kernel's start: the launch time since boot, for the power-on components
        v["t_launch_since_kernel_ms"] = json!(analysis::r2(t_launch as f64 / 1e6));
        if let Some(p) = &ready {
            for (k, name) in [("aetherrv:snapshot", "data_frame_ms"), ("first-contentful-paint", "fcp_frame_ms")] {
                if let Some(m) = p["marks"][k].as_f64() {
                    let tm = epoch_ms_to_mono(m, off);
                    v[name] = json!(cticks.iter().find(|&&t| t >= tm).map(|&t| analysis::ms(t_launch, t)));
                    v[format!("{name}_mark")] = json!(analysis::ms(t_launch, tm));
                }
            }
        }
        v["report_ms"] = json!(ready.as_ref().map(|_| analysis::ms(t_launch, t_ready)));
        v
    };
    let mem_after_boot = c.sample();
    if cold_only {
        let _ = std::process::Command::new("systemctl").args(["--user", "stop", &format!("{unit}.scope")]).status();
        if no_scope {
            let _ = child.kill();
            let _ = child.wait();
        }
        let info = c.rec.info();
        let res = json!({
            "runtime": runtime, "url": url, "command": cmd, "mode": arg(opts, "--mode").unwrap_or("headless"),
            "label": arg(opts, "--label"), "started": started, "bundle": std::env::var("UIBENCH_BUNDLE").ok(), "cold_only": true,
            "page_report": ready.is_some(),
            "recorder": {"output": info.output, "size": [info.width, info.height], "refresh_mhz": info.refresh_mhz},
            "thresholds": {"content_mismatch": CONTENT_TAU, "settled_frac": SETTLED_TAU, "pixel": PIX_THR},
            "runner_setup_ms": {"launch_first": launch_first, "start_since_kernel": analysis::r2(t_main as f64 / 1e6), "recorder": analysis::ms(t_main, t_rec),
                "helper": analysis::ms(t_main, t_help), "baseline_frame": analysis::ms(t_main, t_base), "launch": analysis::ms(t_main, t_launch)},
            "cold_start": cold, "memory_after_boot": mem_after_boot, "no_scope": no_scope, "meta_before": meta_before, "meta_after": meta(), "log": c.log,
        });
        std::fs::write(&out, serde_json::to_string_pretty(&res)?)?;
        eprintln!("uibench: wrote {out} (cold start only)");
        c.rec.stop();
        return Ok(());
    }
    // the layout is only valid once the window has its final (fullscreen) size: wait for a report at that size
    let ready = ready.map(|r| {
        let full_size = |b: &Value| {
            let dpr = b["dpr"].as_f64().unwrap_or(1.0);
            let vw = b["viewport"][0].as_f64().unwrap_or(0.0) * dpr;
            let vh = b["viewport"][1].as_f64().unwrap_or(0.0) * dpr;
            (vw - info.width as f64).abs() < 2.0 && (vh - info.height as f64).abs() < 2.0
        };
        if full_size(&r) {
            return r;
        }
        match c.rep.wait(0, 10_000, |b| b["kind"] == "page" && full_size(b)) {
            Some(x) => x.body,
            None => {
                eprintln!("uibench: the page never reported a full-output viewport (last {:?}): the window is not fullscreen", r["viewport"]);
                let mut r = r;
                r["not_fullscreen"] = json!(true);
                r
            }
        }
    });
    let Some(home) = ready.clone() else {
        bail!("no report from the page (is ?bench=1 in the URL and the bench module built in?)");
    };
    if home["not_fullscreen"] == true {
        bail!("the runtime's window never became fullscreen ({:?}): nothing measured", home["viewport"]);
    }
    c.off = window_offset();
    c.dpr = home["dpr"].as_f64().unwrap_or(1.0);
    let views = c.to_screen(&home["views"]).ok_or_else(|| anyhow!("no #views rect"))?;
    let rail: std::collections::HashMap<String, Rect> =
        home["rail"].as_object().map(|m| m.iter().filter_map(|(k, v)| Some((k.clone(), c.to_screen(v)?))).collect()).unwrap_or_default();
    c.note(format!("layout: views {views:?}, {} rail buttons, window offset {:?}, dpr {}", rail.len(), c.off, c.dpr));

    // ── slide-off check: press a rail button, slide, lift: the page must not change ──
    let probe_page = pages.iter().find(|p| p.as_str() != "home" && rail.contains_key(*p)).cloned().filter(|_| touch);
    let mut moved = false;
    if let Some(pp) = &probe_page {
        let t = mono_ns();
        let (x, y) = rail[pp].center();
        c.press_slide(x, y, c.hold_ms)?;
        sleep_ms(1500);
        moved = page_report(&c.rep, t, Some(pp), 10).is_some();
    }
    let slide_ok = !moved;
    let probe_page = probe_page.unwrap_or_default();
    c.note(format!("slide-off check on rail '{probe_page}': {}", if slide_ok { "no navigation (ok)" } else { "NAVIGATED: presses limited to UI-local targets" }));
    if moved {
        let (x, y) = rail["home"].center();
        c.tap(x, y, 60)?;
        sleep_ms(1500);
    }

    // ── first visit of every page: cold switch, idle fps/CPU/memory, presses ──
    let mut per_page = serde_json::Map::new();
    let mut cur = "home".to_string();
    for page in &pages {
        let Some(r) = rail.get(page).copied() else {
            c.note(format!("no rail button for {page}"));
            continue;
        };
        let t = mono_ns();
        let sw = if *page == cur { json!({"skipped": "already on this page"}) } else { measure_switch(&mut c, views, r, page, 4000, None)? };
        cur = page.clone();
        let rpt = page_report(&c.rep, if sw.get("skipped").is_some() { 0 } else { t }, Some(page), 4000);
        let scene = rpt.as_ref().and_then(|p| c.to_screen(&p["scene"]["rect"]));
        let stim = rpt.as_ref().and_then(|p| c.to_screen(&p["stim"]["rect"]));
        let fps = measure_fps(&mut c, fps_secs, views, scene, stim)?;
        let mut presses = vec![];
        let mut nulls = vec![];
        let mut target = Value::Null;
        if let Some(tg) = rpt.as_ref().and_then(|p| p["targets"].as_array().and_then(|a| a.first()).cloned()) {
            let local = tg["sel"].as_str().is_some_and(|s| s.contains("data-i") || s.contains("#hub") || s.contains("btn-sound"));
            if slide_ok || local {
                if let Some(tr) = c.to_screen(&tg["rect"]) {
                    target = tg.clone();
                    let press_reps = press_reps_for.get(page.as_str()).copied().unwrap_or(press_reps);
                    for i in 0..press_reps.max(null_reps) {
                        if i < press_reps {
                            presses.push(measure_press(&mut c, tr, true, false)?);
                        }
                        if i < null_reps {
                            nulls.push(measure_press(&mut c, tr, true, true)?);
                        }
                    }
                }
            }
        }
        let lat: Vec<f64> = presses.iter().filter_map(|p| p["latency_ms"].as_f64()).collect();
        let fp = nulls.iter().filter(|p| p["latency_ms"].as_f64().is_some()).count();
        c.note(format!("{page}: switch content {:?} settled {:?} | zero-capture CPU {}% rAF {} fps ({} long) | presented fps {} | scene fps {} | CPU {}% | press median {:?} ({}/{} seen, {} null false+)",
            sw["content_ms"], sw["settled_ms"], fps["zero_capture"]["cpu_pct_runtime"], fps["zero_capture"]["raf"]["fps"], fps["zero_capture"]["raf"]["long_frames"],
            fps["fps"]["fps_raw"], fps["scene_fps"]["fps_raw"], fps["cpu_pct_runtime"], analysis::pct(&lat, 50.0), lat.len(), presses.len(), fp));
        per_page.insert(page.clone(), json!({
            "switch_first_visit": sw, "idle": fps, "press_target": target, "press": presses, "press_null": nulls,
            "press_summary": analysis::summary(&lat), "press_false_positive": fp,
            "report": rpt.as_ref().map(|p| json!({"webgl": p["webgl"], "conn": p["conn"], "event_timing": p["event_timing"], "longtasks": p["longtasks"], "targets": p["targets"]})),
        }));
    }

    // ── warm switches ──
    let mut warm: std::collections::BTreeMap<String, Vec<Value>> = Default::default();
    let scenes: std::collections::HashMap<String, Rect> = per_page.iter()
        .filter_map(|(k, v)| Some((k.clone(), serde_json::from_value::<[i32; 4]>(v["idle"]["scene_full"].clone()).ok()
            .map(|a| Rect { x: a[0], y: a[1], w: a[2], h: a[3] })?)))
        .collect();
    for _ in 0..warm_rounds {
        for page in &pages {
            let Some(r) = rail.get(page).copied() else { continue };
            if *page == cur {
                continue;
            }
            let v = measure_switch(&mut c, views, r, page, 1300, scenes.get(page).copied())?;
            cur = page.clone();
            warm.entry(page.clone()).or_default().push(v);
        }
    }
    for (page, v) in &warm {
        let content: Vec<f64> = v.iter().filter_map(|x| x["content_ms"].as_f64()).collect();
        let settled: Vec<f64> = v.iter().filter_map(|x| x["settled_ms"].as_f64()).collect();
        if let Some(pp) = per_page.get_mut(page) {
            pp["switch_warm"] = json!(v);
            pp["switch_warm_summary"] = json!({"content_ms": analysis::summary(&content), "settled_ms": analysis::summary(&settled)});
        }
    }
    let mem_final = c.sample();

    // ── screen power off/on ──
    let screen = if flag(opts, "--screen-power") { Some(screen_power(&mut c, views, &rail, &pages)?) } else { None };

    let _ = std::process::Command::new("systemctl").args(["--user", "stop", &format!("{unit}.scope")]).status();
    let info = c.rec.info();
    let res = json!({
        "runtime": runtime, "url": url, "command": cmd, "mode": arg(opts, "--mode").unwrap_or("headless"),
        "label": arg(opts, "--label"), "started": started, "bundle": std::env::var("UIBENCH_BUNDLE").ok(),
        "recorder": {"output": info.output, "size": [info.width, info.height], "refresh_mhz": info.refresh_mhz, "ticker": info.ticker,
                     "patch_buffer": info.patch_buffer, "tick_cancels": info.tick_cancels, "patch_failed": info.patch_failed,
                     "fence_us_p50": analysis::pct_u32(&info.fence_us, 50.0), "read_us_p50": analysis::pct_u32(&info.read_us, 50.0)},
        "webgl": home["webgl"], "ua": home["ua"], "viewport": home["viewport"], "dpr": home["dpr"],
        "thresholds": {"content_mismatch": CONTENT_TAU, "settled_frac": SETTLED_TAU, "pixel": PIX_THR, "press_changed_frac": 0.02, "hold_ms": c.hold_ms},
        "slide_off_no_click": slide_ok,
        "cold_start": cold, "memory_after_boot": mem_after_boot, "memory_final": mem_final,
        "pages": per_page, "screen_power": screen, "meta_before": meta_before, "meta_after": meta(), "log": c.log,
    });
    std::fs::write(&out, serde_json::to_string_pretty(&res)?)?;
    eprintln!("uibench: wrote {out}");
    c.rec.stop();
    Ok(())
}

fn screen_power(c: &mut Ctx, views: Rect, rail: &std::collections::HashMap<String, Rect>, pages: &[String]) -> Result<Value> {
    let out = std::env::var("OUTPUT").unwrap_or_default();
    let off_t = mono_ns();
    sway(&["output", &out, "power", "off"]);
    sleep_ms(6000);
    c.rec.clear();
    let t_on = mono_ns();
    // as the screen helper (services/screen): power on by name, repeat until DRM agrees (headless: once)
    let mut tries = 0;
    for _ in 0..4 {
        tries += 1;
        sway(&["output", &out, "power", "on"]);
        sleep_ms(400);
        let drm = std::fs::read_dir("/sys/class/drm").ok().and_then(|rd| {
            rd.flatten().find(|e| e.file_name().to_string_lossy().ends_with(&format!("-{out}"))).and_then(|e| std::fs::read_to_string(e.path().join("dpms")).ok())
        });
        if drm.as_deref().map(str::trim) != Some("Off") {
            break;
        }
    }
    sleep_ms(1500);
    let (ticks, _) = c.rec.drain();
    let first = ticks.iter().find(|&&t| t >= t_on).map(|&t| analysis::ms(t_on, t));
    // the page must still respond: switch to another page and back
    let target = pages.iter().find(|p| p.as_str() != "home" && rail.contains_key(*p)).cloned().unwrap_or_default();
    let t = mono_ns();
    let sw = match rail.get(&target) {
        Some(r) => measure_switch(c, views, *r, &target, 1500, None)?,
        None => Value::Null,
    };
    let rpt = page_report(&c.rep, t, Some(&target), 3000);
    let ok = sw["content_ms"].is_number() && rpt.is_some();
    c.note(format!("screen power: off {} ms, on after {tries} tries, first frame {first:?} ms, page responds: {ok}", (t_on - off_t) / 1_000_000));
    Ok(json!({"off_ms": (t_on - off_t) / 1_000_000, "on_tries": tries, "first_frame_after_on_ms": first, "frames_1_5s_after_on": ticks.len(),
              "switch_after_on": sw, "page_report_after_on": rpt.map(|p| json!({"view": p["view"], "conn": p["conn"]})), "ok": ok}))
}

fn chrono_like() -> String {
    std::process::Command::new("date").arg("-Iseconds").output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
}
