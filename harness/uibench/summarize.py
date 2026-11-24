"""Summary tables for the UI runtime benchmark (harness/uibench/run.sh): one Markdown page from the result JSONs.

    python3 summarize.py <result.json>...

§8 verdicts are provisional and use these readings (docs/UI-RUNTIME-BENCHMARK.md, "Metric definitions"):
- tap → visible response: press latency (touch-down → first frame showing the pressed state), p95 over all pages
- page switch: release → first frame showing the new page complete (it may still be easing in), warm p95
- 60 fps steady (Home, Climate, Switches): with nothing captured (no instrument cost), the page's own
  requestAnimationFrame runs at ≥ 57 fps with ≤ 2 % of its frames longer than 1.5 refresh periods; the ease-in
  steps (compositor, instrument included) are shown alongside
- ≥ 30 fps animated scenes (Energy, Network, Weather): the scene (the page's largest canvas, else svg) changes
  ≥ 30 times a second while it moves (frames whose pixels inside the scene changed; presented frames alone would
  count page-wide animations such as the critical-alert flash)
- UI process memory: PSS summed over the runtime's whole process tree after every page was shown, ≤ 400 MB
Standard library only.
"""
import json
import sys
from pathlib import Path

PAGES = ["home", "energy", "climate", "weather", "switches", "network", "alerts", "systems", "settings"]
SMOOTH = ["home", "climate", "switches"]
SCENES = ["energy", "network", "weather"]


def pct(v, p):
    v = sorted(x for x in v if x is not None)
    if not v:
        return None
    return v[min(len(v) - 1, round(p / 100 * (len(v) - 1)))]


def f(x, d=0):
    return "–" if x is None else (f"{x:.{d}f}" if isinstance(x, (int, float)) else str(x))


def mb(sample):
    try:
        return sample["runtime"]["pss_kb"] / 1024
    except (KeyError, TypeError):
        return None


def verdict(ok):
    if isinstance(ok, str):
        return ok
    return "–" if ok is None else ("pass" if ok else "**FAIL**")


def units_changed(before, after):
    """aetherrv units whose restart counter rose or whose state changed (a socket-activated unit appearing, such as
    the admin helper when the Settings page asks for status, is not a change)."""
    def parse(txt):
        out = {}
        for line in txt.splitlines():
            parts = line.split()
            if not parts:
                continue
            kv = dict(x.split("=", 1) for x in parts[1:] if "=" in x)
            out[parts[0]] = (kv.get("ActiveState"), int(kv.get("NRestarts", "0") or 0))
        return out
    a, b = parse(before), parse(after)
    return [u for u in a if u in b and (b[u][1] > a[u][1] or b[u][0] != a[u][0])]


def load(p):
    d = json.loads(Path(p).read_text())
    d["_file"] = Path(p).name
    return d


def page_rows(d):
    rows = []
    for pg in PAGES:
        v = d["pages"].get(pg)
        if not v:
            continue
        warm = v.get("switch_warm") or []
        content = [w.get("content_ms") for w in warm]
        settled = [w.get("settled_ms") for w in warm]
        first = v.get("switch_first_visit") or {}
        presses = [p.get("latency_ms") for p in v.get("press", [])]
        handler = [p.get("handler_ms") for p in v.get("press", []) if p.get("handler_ms") is not None]
        miss = sum(1 for p in v.get("press", []) if p.get("latency_ms") is None)
        ub = sum(1 for p in v.get("press", []) if p.get("upper_bound"))
        idle = v.get("idle", {})
        fps = idle.get("fps", {})
        cfps = idle.get("content_fps") or {}
        sfps = idle.get("scene_fps") or {}
        ease = [((w.get("transition") or {}).get("ease_in_interval_ms") or {}).get("p95") for w in warm]
        z = idle.get("zero_capture") or {}
        zr = z.get("raf") or {}
        nosc = [(w.get("without_scene") or {}).get("content_ms") for w in warm if w.get("without_scene")]
        rows.append({
            "page": pg,
            "first_content": first.get("content_ms"), "first_settled": first.get("settled_ms"),
            "warm_content_p50": pct(content, 50), "warm_content_p95": pct(content, 95), "warm_settled_p50": pct(settled, 50),
            "warm_n": len(warm), "warm_missing": sum(1 for x in content if x is None),
            "press_p50": pct(presses, 50), "press_p95": pct(presses, 95), "press_n": len(presses), "press_miss": miss, "press_ub": ub, "handler_p50": pct(handler, 50),
            "press_fp": v.get("press_false_positive"), "press_target": (v.get("press_target") or {}).get("name"),
            "fps_raw": fps.get("fps_raw"), "fps_motion": fps.get("fps_motion"), "dropped": fps.get("dropped_in_motion"),
            "burst_frames": fps.get("burst_frames"), "cpu": idle.get("cpu_pct_runtime"), "cpu_sway": idle.get("cpu_pct_sway"),
            "content_motion": cfps.get("fps_motion"), "content_bursts": cfps.get("burst_frames"),
            "scene_motion": sfps.get("fps_motion"), "scene_raw": sfps.get("fps_raw"), "scene_dropped": sfps.get("dropped_in_motion"),
            "ease_p95": max([e for e in ease if e is not None], default=None),
            "z_cpu": z.get("cpu_pct_runtime"), "z_cpu_sway": z.get("cpu_pct_sway"), "raf_fps": zr.get("fps"),
            "raf_long": zr.get("long_frames"), "raf_n": zr.get("n"), "raf_p95": (zr.get("interval_ms") or {}).get("p95"),
            "nosc_p50": pct(nosc, 50), "nosc_p95": pct(nosc, 95), "stim": idle.get("stimulus"),
            "pss": idle.get("pss_mb_runtime"),
        })
    return rows


def main():
    runs = [d for d in (load(p) for p in sys.argv[1:] if p.endswith(".json")) if "pages" in d and "runtime" in d]   # not the state snapshot
    order = ["python", "python-static", "wpe", "tauri", "chromium"]
    runs.sort(key=lambda d: order.index(d["runtime"]) if d["runtime"] in order else 99)
    if not runs:
        print("no results")
        return
    r0 = runs[0]
    print(f"# UI runtime benchmark: {r0.get('label')}\n")
    print(f"Bundle `{r0.get('bundle')}`, started {r0.get('started')}, output {r0['recorder']['output']} "
          f"{r0['recorder']['size'][0]}×{r0['recorder']['size'][1]} @ {r0['recorder']['refresh_mhz'] / 1000:.2f} Hz.\n")
    if r0.get("mode") != "screen":
        print("**Headless** (owner decision 2026-11-23): sway's headless output on the Pi's GPU; no scanout, so no panel latency.\n")
    print("## Headline (§8)\n")
    print("| runtime | WebGL | tap → pressed p50 / p95 ms (no press seen) | switch → content p50 / p95 ms (warm) | first visit worst ms | "
          "settled p50 ms | page rAF fps, nothing captured: Home / Climate / Switches | scene fps Energy / Network / Weather | boot → usable ms | PSS after boot / final MB | procs |")
    print("|---|---|---|---|---|---|---|---|---|---|---|")
    verdicts = []
    for d in runs:
        rows = page_rows(d)
        by = {r["page"]: r for r in rows}
        presses = [p.get("latency_ms") for v in d["pages"].values() for p in v.get("press", [])]
        missed = sum(1 for x in presses if x is None)
        content = [w.get("content_ms") for v in d["pages"].values() for w in (v.get("switch_warm") or [])]
        settled = [w.get("settled_ms") for v in d["pages"].values() for w in (v.get("switch_warm") or [])]
        firsts = [r["first_content"] for r in rows if r["first_content"] is not None]
        cold = d.get("cold_start", {})
        usable = cold.get("settled_ms")
        pss_boot, pss_final = mb(d.get("memory_after_boot")), mb(d.get("memory_final"))
        nprocs = (d.get("memory_final") or {}).get("runtime", {}).get("n")
        sm = " / ".join(f(by.get(p, {}).get("raf_fps"), 0) for p in SMOOTH)
        sc = " / ".join(f(by.get(p, {}).get("scene_motion"), 0) for p in SCENES)
        gl = (d.get("webgl") or "").replace("webgl2: ", "")[:40]
        print(f"| {d['runtime']} | {gl} | {f(pct(presses, 50), 1)} / {f(pct(presses, 95), 1)} ({missed} of {len(presses)}) | {f(pct(content, 50), 1)} / {f(pct(content, 95), 1)} | "
              f"{f(max(firsts) if firsts else None, 0)} | {f(pct(settled, 50), 0)} | {sm} | {sc} | {f(usable, 0)} | {f(pss_boot, 0)} / {f(pss_final, 0)} | {f(nprocs)} |")
        smooth_ok = [by[p]["raf_fps"] >= 57 and (by[p]["raf_long"] or 0) <= 0.02 * max(1, by[p]["raf_n"] or 1)
                     for p in SMOOTH if p in by and by[p]["raf_fps"] is not None]
        scenes_ok = [by[p]["scene_motion"] >= 30 for p in SCENES if p in by and by[p]["scene_motion"] is not None]
        # a press never seen within the hold counts as at least the hold, i.e. a fail, not as a missing sample
        hold = (d.get("thresholds") or {}).get("hold_ms", 300)
        p95_all = pct([x if x is not None else hold for x in presses], 95) if presses else None
        mem = None if pss_final is None else ("at the limit (one run)" if abs(pss_final - 400) <= 12 else pss_final <= 400)
        verdicts.append((d["runtime"], {
            "tap ≤ 50 ms (p95, misses = hold)": p95_all <= 50 if p95_all is not None else None,
            "switch ≤ 150 ms (warm p95)": pct(content, 95) <= 150 if pct(content, 95) is not None else None,
            "60 fps Home/Climate/Switches": all(smooth_ok) if smooth_ok else None,
            "≥ 30 fps Energy/Network/Weather": (f"{verdict(all(scenes_ok))} ({len(scenes_ok)} of 3 measured)" if scenes_ok else None),
            "PSS ≤ 400 MB": mem,
        }))
    print("\n## Provisional verdicts\n")
    keys = list(verdicts[0][1].keys())
    print("| runtime | " + " | ".join(keys) + " | power-on ≤ 15 s |")
    print("|---|" + "---|" * (len(keys) + 1))
    for rt, v in verdicts:
        print(f"| {rt} | " + " | ".join(verdict(v[k]) for k in keys) + " | boot script (not run) |")

    for d in runs:
        print(f"\n## {d['runtime']}\n")
        cold = d.get("cold_start", {})
        print(f"Command: `{' '.join(d.get('command', []))}`  ")
        print(f"WebGL: {d.get('webgl')}; slide-off check (no click from a moved touch): {d.get('slide_off_no_click')}  ")
        print(f"Cold start (launch → ms): first compositor frame {f(cold.get('first_frame_ms'), 0)}, first paint {f(cold.get('first_paint_ms'), 0)}, "
              f"page's data mark → frame {f(cold.get('data_frame_ms'), 0)}, content {f(cold.get('content_ms'), 0)}, usable (settled) {f(cold.get('settled_ms'), 0)}; "
              f"light frame before content: {cold.get('white_flash')} (max mean luma {f(cold.get('max_luma_before_content'), 0)})  ")
        sp = d.get("screen_power") or {}
        if sp:
            print(f"Screen power off {sp.get('off_ms')} ms → on ({sp.get('on_tries')} tries): first frame after {f(sp.get('first_frame_after_on_ms'), 0)} ms, "
                  f"page responds afterwards: {sp.get('ok')}  ")
        mf = (d.get("memory_final") or {}).get("runtime") or {}
        bo = sorted(set(x.split(": ", 1)[1] for x in (mf.get("gpu_bo_stats") or []) if ": " in x))
        if mf.get("memory_current") is not None or bo:
            ms_ = mf.get("memory_stat") or {}
            mbv = lambda b: f"{b / 2**20:.0f}" if isinstance(b, (int, float)) else "–"
            print(f"Memory after every page: PSS {f(mb(d.get('memory_final')), 0)} MB; cgroup memory.current {mbv(mf.get('memory_current'))} MB "
                  f"(shmem {mbv(ms_.get('shmem'))}); GPU buffer objects, whole system (v3d): {'; '.join(bo) or '–'}  ")
        procs = (d.get("memory_final") or {}).get("runtime", {}).get("procs", [])
        if procs:
            top = sorted(procs, key=lambda p: -p["pss_kb"])[:6]
            gpu = [p["comm"] for p in procs if p.get("render_node")]
            soft = [p["comm"] for p in procs if p.get("llvmpipe_threads")]
            print("Processes (PSS MB): " + ", ".join(f"{p['comm']} {p['pss_kb'] / 1024:.0f}" for p in top)
                  + f"; render node open in: {', '.join(gpu) or 'none'}; software GL (llvmpipe threads) in: {', '.join(soft) or 'none'}  ")
        print()
        print("| page | first visit content / settled ms | warm content p50 / p95 ms | without the scene p50 / p95 ms | warm settled p50 ms | ease-in step p95 ms | press p50 / p95 ms (n, miss, null+) | of which to the handler, p50 | target | "
              "nothing captured: CPU runtime / sway %, page rAF fps (long frames) | presented fps | content fps (idle) | scene fps, lower bound (dropped) | CPU with ticker % | PSS MB |")
        print("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
        for r in page_rows(d):
            print(f"| {r['page']} | {f(r['first_content'], 0)} / {f(r['first_settled'], 0)} | {f(r['warm_content_p50'], 1)} / {f(r['warm_content_p95'], 1)}"
                  f"{' (' + str(r['warm_missing']) + ' undetected)' if r['warm_missing'] else ''} | {f(r['nosc_p50'], 1)} / {f(r['nosc_p95'], 1)} | {f(r['warm_settled_p50'], 0)} | {f(r['ease_p95'], 1)} | "
                  f"{f(r['press_p50'], 1)} / {f(r['press_p95'], 1)} ({r['press_n']}, {r['press_miss']}, {f(r['press_fp'])}{', ' + str(r['press_ub']) + ' upper bound' if r['press_ub'] else ''}) | {f(r['handler_p50'], 1)} | {r['press_target'] or '–'} | "
                  f"{f(r['z_cpu'], 1)} / {f(r['z_cpu_sway'], 1)}, {f(r['raf_fps'], 1)} ({f(r['raf_long'])}) | "
                  f"{f(r['fps_raw'], 1)} | {f(r['content_motion'], 1)} | {f(r['scene_motion'], 1)} ({f(r['scene_dropped'])}){' after Play' if r['stim'] else ''} | {f(r['cpu'], 1)} | {f(r['pss'], 0)} |")
        mb_ = d.get("meta_before") or {}
        ma_ = d.get("meta_after") or {}
        print(f"\nLoad before: `{mb_.get('loadavg')}`; alerts: `{mb_.get('alert_severities')}`")
        changed = units_changed(mb_.get("bench_units_restarts") or "", ma_.get("bench_units_restarts") or "")
        if changed:
            print(f"\n**Bench services restarted or changed state during this run ({', '.join(changed)}): discard it and rerun.**")
        for line in d.get("log", []):
            if "NAVIGATED" in line or "unavailable" in line:
                print(f"- {line}")


if __name__ == "__main__":
    main()
