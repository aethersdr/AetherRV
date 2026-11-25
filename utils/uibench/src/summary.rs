//! `aetherrv-uibench summarize <result.json>...`: one Markdown page of summary tables from the results of
//! harness/uibench/run.sh (it writes summary.md). Other JSON files are skipped.
//!
//! §8 verdicts are provisional and use these readings (docs/UI-RUNTIME-BENCHMARK.md, "Metric definitions"):
//! - tap → visible response: press latency (touch-down → first frame showing the pressed state), p95 over all pages
//! - page switch: release → first frame showing the new page complete (it may still be easing in), warm p95
//! - 60 fps steady (Home, Climate, Switches): with nothing captured (no instrument cost), the page's own
//!   requestAnimationFrame runs at ≥ 57 fps with ≤ 2 % of its frames longer than 1.5 refresh periods; the ease-in
//!   steps (compositor, instrument included) are shown alongside
//! - ≥ 30 fps animated scenes (Energy, Network, Weather): the scene (the page's largest canvas, else svg) changes
//!   ≥ 30 times a second while it moves (frames whose pixels inside the scene changed; presented frames alone would
//!   count page-wide animations such as the critical-alert flash)
//! - UI process memory: PSS summed over the runtime's whole process tree after every page was shown, ≤ 400 MB

use crate::pyfmt::{Py, fixed};
use anyhow::{Context, Result};
use std::fmt::Write;

const PAGES: [&str; 9] = ["home", "energy", "climate", "weather", "switches", "network", "alerts", "systems", "settings"];
const SMOOTH: [&str; 3] = ["home", "climate", "switches"];
const SCENES: [&str; 3] = ["energy", "network", "weather"];
const ORDER: [&str; 3] = ["wpe", "tauri", "chromium"];

/// The p-th percentile as the harness has always picked it: index round(p/100·(n−1)), ties to even.
fn pct<'a>(v: impl IntoIterator<Item = &'a Py>, p: f64) -> Option<f64> {
    pctf(v.into_iter().filter_map(Py::num).collect(), p)
}
fn pctf(mut v: Vec<f64>, p: f64) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let i = (p / 100.0 * (v.len() - 1) as f64).round_ties_even() as usize;
    Some(v[i.min(v.len() - 1)])
}

/// A number with d decimals, "–" when missing, anything else as Python's str()
fn f(x: &Py, d: usize) -> String {
    match (x, x.num()) {
        (Py::None, _) => "–".into(),
        (_, Some(v)) => fixed(v, d),
        _ => x.str(),
    }
}
fn fo(x: Option<f64>, d: usize) -> String {
    x.map_or("–".into(), |v| fixed(v, d))
}

fn mb(sample: &Py) -> Option<f64> {
    sample.at("runtime").at("pss_kb").num().map(|v| v / 1024.0)
}

enum Verdict {
    None,
    Ok(bool),
    Text(String),
}
fn verdict(v: &Verdict) -> String {
    match v {
        Verdict::None => "–".into(),
        Verdict::Ok(true) => "pass".into(),
        Verdict::Ok(false) => "**FAIL**".into(),
        Verdict::Text(s) => s.clone(),
    }
}

/// aetherrv units whose restart counter rose or whose state changed (a socket-activated unit appearing, such as
/// the admin helper when the Settings page asks for status, is not a change).
fn units_changed(before: &str, after: &str) -> Vec<String> {
    fn parse(txt: &str) -> Vec<(String, (Option<String>, i64))> {
        let mut out: Vec<(String, (Option<String>, i64))> = vec![];
        for line in txt.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            let Some(unit) = parts.first() else { continue };
            let mut kv: Vec<(&str, &str)> = vec![];
            for (k, v) in parts[1..].iter().filter_map(|x| x.split_once('=')) {
                match kv.iter_mut().find(|e| e.0 == k) {
                    Some(e) => e.1 = v,
                    None => kv.push((k, v)),
                }
            }
            let get = |k: &str| kv.iter().find(|e| e.0 == k).map(|e| e.1);
            let n = match get("NRestarts") {
                Some("") => 0,
                Some(x) => x.parse().unwrap_or(0),
                None => 0,
            };
            let val = (get("ActiveState").map(String::from), n);
            match out.iter_mut().find(|e| e.0 == *unit) {
                Some(e) => e.1 = val,
                None => out.push((unit.to_string(), val)),
            }
        }
        out
    }
    let (a, b) = (parse(before), parse(after));
    a.iter()
        .filter(|(u, av)| b.iter().find(|(bu, _)| bu == u).is_some_and(|(_, bv)| bv.1 > av.1 || bv.0 != av.0))
        .map(|(u, _)| u.clone())
        .collect()
}

struct Row<'a> {
    page: &'static str,
    first_content: &'a Py,
    first_settled: &'a Py,
    warm_content_p50: Option<f64>,
    warm_content_p95: Option<f64>,
    warm_settled_p50: Option<f64>,
    warm_missing: usize,
    press_p50: Option<f64>,
    press_p95: Option<f64>,
    press_n: usize,
    press_miss: usize,
    press_ub: usize,
    handler_p50: Option<f64>,
    press_fp: &'a Py,
    press_target: &'a Py,
    fps_raw: &'a Py,
    cpu: &'a Py,
    content_motion: &'a Py,
    scene_motion: &'a Py,
    scene_dropped: &'a Py,
    ease_p95: Option<f64>,
    z_cpu: &'a Py,
    z_cpu_sway: &'a Py,
    raf_fps: &'a Py,
    raf_long: &'a Py,
    raf_n: &'a Py,
    nosc_p50: Option<f64>,
    nosc_p95: Option<f64>,
    stim: &'a Py,
    pss: &'a Py,
}

fn page_rows(d: &Py) -> Vec<Row<'_>> {
    let mut rows = vec![];
    for pg in PAGES {
        let v = d.at("pages").at(pg);
        if !v.truthy() {
            continue;
        }
        let warm = v.at("switch_warm").list();
        let content: Vec<&Py> = warm.iter().map(|w| w.at("content_ms")).collect();
        let press = v.at("press").list();
        let idle = v.at("idle");
        let z = idle.at("zero_capture");
        let zr = z.at("raf");
        // the slowest ease-in step of any warm switch
        let ease = warm.iter().filter_map(|w| w.at("transition").at("ease_in_interval_ms").at("p95").num()).reduce(f64::max);
        let nosc: Vec<&Py> = warm.iter().filter(|w| w.at("without_scene").truthy()).map(|w| w.at("without_scene").at("content_ms")).collect();
        rows.push(Row {
            page: pg,
            first_content: v.at("switch_first_visit").at("content_ms"),
            first_settled: v.at("switch_first_visit").at("settled_ms"),
            warm_content_p50: pct(content.iter().copied(), 50.0),
            warm_content_p95: pct(content.iter().copied(), 95.0),
            warm_settled_p50: pct(warm.iter().map(|w| w.at("settled_ms")), 50.0),
            warm_missing: content.iter().filter(|x| x.is_none()).count(),
            press_p50: pct(press.iter().map(|p| p.at("latency_ms")), 50.0),
            press_p95: pct(press.iter().map(|p| p.at("latency_ms")), 95.0),
            press_n: press.len(),
            press_miss: press.iter().filter(|p| p.at("latency_ms").is_none()).count(),
            press_ub: press.iter().filter(|p| p.at("upper_bound").truthy()).count(),
            handler_p50: pct(press.iter().map(|p| p.at("handler_ms")), 50.0),
            press_fp: v.at("press_false_positive"),
            press_target: v.at("press_target").at("name"),
            fps_raw: idle.at("fps").at("fps_raw"),
            cpu: idle.at("cpu_pct_runtime"),
            content_motion: idle.at("content_fps").at("fps_motion"),
            scene_motion: idle.at("scene_fps").at("fps_motion"),
            scene_dropped: idle.at("scene_fps").at("dropped_in_motion"),
            ease_p95: ease,
            z_cpu: z.at("cpu_pct_runtime"),
            z_cpu_sway: z.at("cpu_pct_sway"),
            raf_fps: zr.at("fps"),
            raf_long: zr.at("long_frames"),
            raf_n: zr.at("n"),
            nosc_p50: pct(nosc.iter().copied(), 50.0),
            nosc_p95: pct(nosc.iter().copied(), 95.0),
            stim: idle.at("stimulus"),
            pss: idle.at("pss_mb_runtime"),
        });
    }
    rows
}

pub fn main(args: &[String]) -> Result<()> {
    let mut runs = vec![];
    for p in args.iter().filter(|p| p.ends_with(".json")) {
        let d = Py::parse(&std::fs::read_to_string(p).with_context(|| p.clone())?).with_context(|| p.clone())?;
        if d.has("pages") && d.has("runtime") {
            runs.push(d);
        }
    }
    let rank = |d: &Py| ORDER.iter().position(|o| d.at("runtime").as_str() == Some(o)).unwrap_or(99);
    runs.sort_by_key(rank);
    print!("{}", render(&runs));
    Ok(())
}

fn render(runs: &[Py]) -> String {
    let mut o = String::new();
    let Some(r0) = runs.first() else { return "no results\n".into() };
    let rec = r0.at("recorder");
    let _ = writeln!(o, "# UI runtime benchmark: {}\n", r0.at("label").str());
    let _ = writeln!(o, "Bundle `{}`, started {}, output {} {}×{} @ {} Hz.\n", r0.at("bundle").str(), r0.at("started").str(), rec.at("output").str(),
        rec.at("size").idx(0).str(), rec.at("size").idx(1).str(), fixed(rec.at("refresh_mhz").num().unwrap_or(f64::NAN) / 1000.0, 2));
    if r0.at("mode").as_str() != Some("screen") {
        o.push_str("**Headless** (owner decision 2026-11-23): sway's headless output on the Pi's GPU; no scanout, so no panel latency.\n\n");
    }
    o.push_str("## Headline (§8)\n\n");
    o.push_str("| runtime | WebGL | tap → pressed p50 / p95 ms (no press seen) | switch → content p50 / p95 ms (warm) | first visit worst ms | \
        settled p50 ms | page rAF fps, nothing captured: Home / Climate / Switches | scene fps Energy / Network / Weather | boot → usable ms | PSS after boot / final MB | procs |\n");
    o.push_str("|---|---|---|---|---|---|---|---|---|---|---|\n");
    let mut verdicts: Vec<(String, Vec<(&str, Verdict)>)> = vec![];
    for d in runs {
        let rows = page_rows(d);
        let by = |p: &str| rows.iter().find(|r| r.page == p);
        let pages: Vec<&Py> = d.at("pages").values();
        let presses: Vec<&Py> = pages.iter().flat_map(|v| v.at("press").list()).map(|p| p.at("latency_ms")).collect();
        let missed = presses.iter().filter(|x| x.is_none()).count();
        let warm = || pages.iter().flat_map(|v| v.at("switch_warm").list());
        let content: Vec<&Py> = warm().map(|w| w.at("content_ms")).collect();
        let settled: Vec<&Py> = warm().map(|w| w.at("settled_ms")).collect();
        let first_worst = rows.iter().filter_map(|r| r.first_content.num()).reduce(f64::max);
        let usable = d.at("cold_start").at("settled_ms");
        let (pss_boot, pss_final) = (mb(d.at("memory_after_boot")), mb(d.at("memory_final")));
        let nprocs = d.at("memory_final").at("runtime").at("n");
        let sm = SMOOTH.map(|p| by(p).map_or("–".into(), |r| f(r.raf_fps, 0))).join(" / ");
        let sc = SCENES.map(|p| by(p).map_or("–".into(), |r| f(r.scene_motion, 0))).join(" / ");
        let gl: String = d.at("webgl").or(&Py::Str(String::new())).str().replace("webgl2: ", "").chars().take(40).collect();
        let rt = d.at("runtime").str();
        let _ = writeln!(o, "| {rt} | {gl} | {} / {} ({missed} of {}) | {} / {} | {} | {} | {sm} | {sc} | {} | {} / {} | {} |",
            fo(pct(presses.iter().copied(), 50.0), 1), fo(pct(presses.iter().copied(), 95.0), 1), presses.len(),
            fo(pct(content.iter().copied(), 50.0), 1), fo(pct(content.iter().copied(), 95.0), 1),
            fo(first_worst, 0), fo(pct(settled.iter().copied(), 50.0), 0), f(usable, 0), fo(pss_boot, 0), fo(pss_final, 0), f(nprocs, 0));
        let smooth_ok: Vec<bool> = SMOOTH.iter().filter_map(|p| by(p)).filter_map(|r| {
            let fps = r.raf_fps.num()?;
            let long = r.raf_long.or(&Py::Int(0)).num().unwrap_or(0.0);
            let n = r.raf_n.or(&Py::Int(1)).num().unwrap_or(1.0).max(1.0);
            Some(fps >= 57.0 && long <= 0.02 * n)
        }).collect();
        let scenes_ok: Vec<bool> = SCENES.iter().filter_map(|p| by(p)).filter_map(|r| Some(r.scene_motion.num()? >= 30.0)).collect();
        // a press never seen within the hold counts as at least the hold, i.e. a fail, not as a missing sample
        let t = d.at("thresholds");
        let hold = if t.has("hold_ms") { t.at("hold_ms") } else { &Py::Int(300) };
        let p95_all = if presses.is_empty() { None } else { pct(presses.iter().map(|x| if x.is_none() { hold } else { *x }), 95.0) };
        let c95 = pct(content.iter().copied(), 95.0);
        let mem = match pss_final {
            None => Verdict::None,
            Some(v) if (v - 400.0).abs() <= 12.0 => Verdict::Text("at the limit (one run)".into()),
            Some(v) => Verdict::Ok(v <= 400.0),
        };
        let all = |v: &[bool]| if v.is_empty() { Verdict::None } else { Verdict::Ok(v.iter().all(|x| *x)) };
        verdicts.push((rt, vec![
            ("tap ≤ 50 ms (p95, misses = hold)", p95_all.map_or(Verdict::None, |x| Verdict::Ok(x <= 50.0))),
            ("switch ≤ 150 ms (warm p95)", c95.map_or(Verdict::None, |x| Verdict::Ok(x <= 150.0))),
            ("60 fps Home/Climate/Switches", all(&smooth_ok)),
            ("≥ 30 fps Energy/Network/Weather", if scenes_ok.is_empty() { Verdict::None } else {
                Verdict::Text(format!("{} ({} of 3 measured)", verdict(&all(&scenes_ok)), scenes_ok.len()))
            }),
            ("PSS ≤ 400 MB", mem),
        ]));
    }
    o.push_str("\n## Provisional verdicts\n\n");
    let keys: Vec<&str> = verdicts[0].1.iter().map(|(k, _)| *k).collect();
    let _ = writeln!(o, "| runtime | {} | power-on ≤ 15 s |", keys.join(" | "));
    let _ = writeln!(o, "|---|{}", "---|".repeat(keys.len() + 1));
    for (rt, v) in &verdicts {
        let _ = writeln!(o, "| {rt} | {} | boot script (not run) |", v.iter().map(|(_, x)| verdict(x)).collect::<Vec<_>>().join(" | "));
    }

    for d in runs {
        let _ = writeln!(o, "\n## {}\n", d.at("runtime").str());
        let cold = d.at("cold_start");
        let cmd: Vec<String> = d.at("command").list().iter().map(Py::str).collect();
        let _ = writeln!(o, "Command: `{}`  ", cmd.join(" "));
        let _ = writeln!(o, "WebGL: {}; slide-off check (no click from a moved touch): {}  ", d.at("webgl").str(), d.at("slide_off_no_click").str());
        let _ = writeln!(o, "Cold start (launch → ms): first compositor frame {}, first paint {}, page's data mark → frame {}, content {}, usable (settled) {}; \
            light frame before content: {} (max mean luma {})  ", f(cold.at("first_frame_ms"), 0), f(cold.at("first_paint_ms"), 0), f(cold.at("data_frame_ms"), 0),
            f(cold.at("content_ms"), 0), f(cold.at("settled_ms"), 0), cold.at("white_flash").str(), f(cold.at("max_luma_before_content"), 0));
        let sp = d.at("screen_power");
        if sp.truthy() {
            let _ = writeln!(o, "Screen power off {} ms → on ({} tries): first frame after {} ms, page responds afterwards: {}  ",
                sp.at("off_ms").str(), sp.at("on_tries").str(), f(sp.at("first_frame_after_on_ms"), 0), sp.at("ok").str());
        }
        let mf = d.at("memory_final").at("runtime");
        let mut bo: Vec<&str> = mf.at("gpu_bo_stats").list().iter().filter_map(|x| x.as_str()?.split_once(": ").map(|(_, b)| b)).collect();
        bo.sort();
        bo.dedup();
        if !mf.at("memory_current").is_none() || !bo.is_empty() {
            let ms = mf.at("memory_stat");
            let mbv = |b: &Py| b.num().map_or("–".into(), |b| fixed(b / 1048576.0, 0));
            let _ = writeln!(o, "Memory after every page: PSS {} MB; cgroup memory.current {} MB (shmem {}); GPU buffer objects, whole system (v3d): {}  ",
                fo(mb(d.at("memory_final")), 0), mbv(mf.at("memory_current")), mbv(ms.at("shmem")), if bo.is_empty() { "–".into() } else { bo.join("; ") });
        }
        let procs = mf.at("procs").list();
        if !procs.is_empty() {
            let pss = |p: &Py| p.at("pss_kb").num().unwrap_or(0.0);
            let mut top: Vec<&Py> = procs.iter().collect();
            top.sort_by(|a, b| pss(b).partial_cmp(&pss(a)).unwrap()); // stable: equal ones keep their order
            let names = |key: &str| procs.iter().filter(|p| p.at(key).truthy()).map(|p| p.at("comm").str()).collect::<Vec<_>>().join(", ");
            let (gpu, soft) = (names("render_node"), names("llvmpipe_threads"));
            let _ = writeln!(o, "Processes (PSS MB): {}; render node open in: {}; software GL (llvmpipe threads) in: {}  ",
                top.iter().take(6).map(|p| format!("{} {}", p.at("comm").str(), fixed(pss(p) / 1024.0, 0))).collect::<Vec<_>>().join(", "),
                if gpu.is_empty() { "none" } else { &gpu }, if soft.is_empty() { "none" } else { &soft });
        }
        o.push('\n');
        o.push_str("| page | first visit content / settled ms | warm content p50 / p95 ms | without the scene p50 / p95 ms | warm settled p50 ms | ease-in step p95 ms | \
            press p50 / p95 ms (n, miss, null+) | of which to the handler, p50 | target | nothing captured: CPU runtime / sway %, page rAF fps (long frames) | \
            presented fps | content fps (idle) | scene fps, lower bound (dropped) | CPU with ticker % | PSS MB |\n");
        o.push_str("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n");
        for r in page_rows(d) {
            let undetected = if r.warm_missing > 0 { format!(" ({} undetected)", r.warm_missing) } else { String::new() };
            let ub = if r.press_ub > 0 { format!(", {} upper bound", r.press_ub) } else { String::new() };
            let target = if r.press_target.truthy() { r.press_target.str() } else { "–".into() };
            let _ = writeln!(o, "| {} | {} / {} | {} / {}{undetected} | {} / {} | {} | {} | {} / {} ({}, {}, {}{ub}) | {} | {target} | {} / {}, {} ({}) | {} | {} | {} ({}){} | {} | {} |",
                r.page, f(r.first_content, 0), f(r.first_settled, 0), fo(r.warm_content_p50, 1), fo(r.warm_content_p95, 1),
                fo(r.nosc_p50, 1), fo(r.nosc_p95, 1), fo(r.warm_settled_p50, 0), fo(r.ease_p95, 1),
                fo(r.press_p50, 1), fo(r.press_p95, 1), r.press_n, r.press_miss, f(r.press_fp, 0), fo(r.handler_p50, 1),
                f(r.z_cpu, 1), f(r.z_cpu_sway, 1), f(r.raf_fps, 1), f(r.raf_long, 0),
                f(r.fps_raw, 1), f(r.content_motion, 1), f(r.scene_motion, 1), f(r.scene_dropped, 0), if r.stim.truthy() { " after Play" } else { "" },
                f(r.cpu, 1), f(r.pss, 0));
        }
        let (mb_, ma_) = (d.at("meta_before"), d.at("meta_after"));
        let _ = writeln!(o, "\nLoad before: `{}`; alerts: `{}`", mb_.at("loadavg").str(), mb_.at("alert_severities").str());
        let txt = |m: &Py| m.at("bench_units_restarts").as_str().unwrap_or("").to_string();
        let changed = units_changed(&txt(mb_), &txt(ma_));
        if !changed.is_empty() {
            let _ = writeln!(o, "\n**Bench services restarted or changed state during this run ({}): discard it and rerun.**", changed.join(", "));
        }
        for line in d.at("log").list().iter().filter_map(Py::as_str) {
            if line.contains("NAVIGATED") || line.contains("unavailable") {
                let _ = writeln!(o, "- {line}");
            }
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_ties_to_even() {
        // index 0.5 → 0, 1.5 → 2, 2.5 → 2 (Python's round), not away from zero
        assert_eq!(pctf(vec![10.0, 20.0], 50.0), Some(10.0));
        assert_eq!(pctf(vec![10.0, 20.0, 30.0, 40.0], 50.0), Some(30.0));
        assert_eq!(pctf(vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 50.0), Some(3.0));
        assert_eq!(pctf(vec![], 50.0), None);
    }

    #[test]
    fn units() {
        let a = "aetherrv-gateway.service NRestarts=0 ActiveState=active\naetherrv-bus.service NRestarts=1 ActiveState=active\n";
        let b = "aetherrv-gateway.service NRestarts=1 ActiveState=active\naetherrv-bus.service NRestarts=1 ActiveState=active\naetherrv-admin.service NRestarts=0 ActiveState=active\n";
        assert_eq!(units_changed(a, b), vec!["aetherrv-gateway.service"]);
    }

    #[test]
    fn synthetic_run() {
        let d = Py::parse(r#"{"runtime": "wpe", "label": "headless", "bundle": "abc", "started": "2026-01-01", "mode": "headless",
            "recorder": {"output": "HEADLESS-1", "size": [1920, 1200], "refresh_mhz": 60000}, "webgl": "webgl2: V3D 7.1",
            "pages": {"home": {"press": [{"latency_ms": 20.5}, {"latency_ms": null}], "switch_warm": [{"content_ms": 80}],
                               "idle": {"zero_capture": {"raf": {"fps": 59.9, "long_frames": 0, "n": 300}}}}},
            "memory_final": {"runtime": {"pss_kb": 307200, "n": 4}}}"#).unwrap();
        let s = render(&[d]);
        assert!(s.contains("| wpe | V3D 7.1 | 20.5 / 20.5 (1 of 2) | 80.0 / 80.0 | – | – | 60 / – / – | – / – / – | – | – / 300 | 4 |"), "{s}");
        assert!(s.contains("| wpe | **FAIL** | pass | pass | – | pass | boot script (not run) |"), "{s}");
    }
}
