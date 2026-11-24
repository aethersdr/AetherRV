# UI runtime benchmark (draft)

Which runtime should show the React UI on the van's touchscreen: (a) WPE WebKit, (b) a Tauri v2 app (WebKitGTK), or
(c) Chromium (prompt §7)? This page has the method, the numbers and a provisional recommendation. It is a
**draft**: every number below is from a **headless** sway output (no monitor was detected on the bench), so none of
them is an acceptance number. The acceptance run is one command once the monitor is back (§9).

## 1. Setup

| | |
|---|---|
| Machine | bench Pi 5, 16 GB, NVMe, Arch Linux ARM, kernel 7.0.3-v8+, Mesa 26.3.1 (V3D 7.1), no CAN HAT |
| Compositor | sway 1.12 (wlroots 0.20.2), the van kiosk's config (`deploy/kiosk/sway.conf`) for every runtime: `harness/uibench/sway.conf.in` |
| Session | a transient unit with a PAM session on VT 8 (`harness/uibench/session.sh`), like `aetherrv-panel.service` on tty1: logind gives sway its seat (DRM + libinput) |
| Output | **headless**: `WLR_BACKENDS=headless,libinput`, one output at 1920×1200 @ 60 Hz (the monitor's connectors report `disconnected`) |
| Backend | the whole Rust system under systemd against simulators (`harness/bench/bench-up.sh`), gateway on 127.0.0.1:8080 |
| UI | the production bundle (`vite build`) in `/opt/aetherrv-rs/ui`, opened as `http://127.0.0.1:8080/?bench=1`; the bundle hash is in every result |
| Fonts | `noto-fonts` installed on the bench for this (the van has it; without it the UI fell back to FreeSans, a different layout and raster cost) |
| (a) WPE | wpewebkit 2.54.3, `MiniBrowser --fullscreen` with `WPE_PLATFORM=wayland` (WPE Platform API) |
| (b) Tauri | Tauri 2 (`panel/tauri/`, its own Cargo workspace), webkit2gtk-4.1 2.54.3, built natively on the Pi |
| (c) Chromium | chromium 158, the van's flags (`deploy/kiosk/chromium.sh`): `--kiosk --ozone-platform=wayland`, GPU on |
| Before | the Python UI (`web/`) served by the Python backend's own web app with **no device controls** (`harness/uibench/serve_ref.py`, from `harness/ui-reference/serve_snapshot.py`), in Chromium kiosk as on the van |

**Deviation from the prompt: no `cog`.** The prompt names `cog` for candidate (a). Cog is the launcher for WPE's
legacy libwpe API, which Arch Linux ARM doesn't package: its `wpewebkit` 2.54 is built for the new WPE Platform API
only (no libwpe, no wpebackend-fdo). Candidate (a) therefore runs WPE WebKit through its own `MiniBrowser` on the
WPE Platform Wayland backend, fullscreen. The engine, its processes and its rendering path are the ones a cog kiosk
would use on a current WPE; on the van it would be a few-line WPEPlatform launcher, or MiniBrowser itself.

## 2. Method

Everything is measured from the **compositor's frames**, not from the page: `harness/uibench/run.sh` drives the
tool `utils/uibench` (`aetherrv-uibench`) against one runtime at a time, in the same sway session.

**Touch.** A uinput virtual touchscreen (absolute multitouch protocol B, one slot, `BTN_TOUCH`,
`INPUT_PROP_DIRECT`), mapped to the output by the kiosk's own `input type:touch map_to_output` line. libinput and
sway treat it exactly like the van's capacitive panel. Every injection is timestamped on CLOCK_MONOTONIC just before
the write.

**Frames.** Two Wayland screencopy clients (`wlr-screencopy-unstable-v1`):

- the **ticker**: a 16×16 region copied with `copy_with_damage` on its own connection. wlroots tracks screencopy
  damage per client, not per region, so this copy completes once for every frame the compositor commits with damage
  anywhere on the output. Its `ready` time is that commit's time (CLOCK_MONOTONIC);
- **patches**: small regions copied the same way, one connection each, into GPU buffers (linux-dmabuf via GBM), read
  once their fence has signalled. Each copy carries the commit time of the frame it shows, so pixels and frame
  times line up exactly.

Calibration (a test page with a known cadence, `harness/uibench/calib.html`, in Chromium):

| what | result |
|---|---|
| ticker on a page painting at 0 / 30 / 60 Hz | 0 / 30.9 / 61.3 frames per second, matching the page's own paint count (the headless output runs at 61.4 Hz) |
| `wlr-export-dmabuf` as the ticker (first attempt) | **rejected**: every request makes sway render a frame, so a static page read ~62 fps |
| a full-output copy every frame | **rejected**: costs sway ~7.7 ms per frame, the 60 Hz page dropped to ~51 fps |
| 4 thin bands (3 × 1784×8, 1 × 8×1104) or one 320×200 patch per frame | no effect: 61.4 fps presented, 0 dropped, the page's rAF unchanged |
| 12 or more small patches per frame | the GPU backs up (fence latency 134 ms), the page drops to ~40–57 fps; not used |
| the same captures on a GPU-heavy page (Home in Chromium, `perturb.sh`) | **measurable**: the page's rAF p95 16.7 ms → 22.5 ms with the ticker alone, 34.9 ms with four bands (§3, caveat 2) |
| reading a patch right after `ready` | waits ~8 ms for the copy's fence, so reads happen when the dmabuf fd polls readable (30–200 µs) |

**Timestamps are commit times.** On a real display the frame is scanned out at the next vblank after its rendering
completes, up to one refresh later. Headless has no scanout at all. The tap and page-switch numbers do not
include that last step; on the real screen, add up to 16.7 ms.

**Scenarios, per runtime** (`aetherrv-uibench run`; all pages, in rail order):

1. **Cold start**: one warm-up launch first, so caches and profiles are in the state a used kiosk's are. Then
   launch → the first frame that differs from the black background (first paint) → the frame after the page's
   own "data arrived" mark (`aetherrv:snapshot`) → the first frame showing the settled Home page (*usable*).
   Also checked: whether any frame before the content is light (a white flash at the splash hand-off).
2. **Slide-off check**: press a rail button, hold, slide 120 px, lift. The page must not navigate. This proves that
   a moved touch is not a click in that runtime; every press below relies on it.
3. Per page, **first visit** (a cold page switch: lazy chunk, first render, WebGL set-up), then:
   - **Idle**: presented frames per second, CPU of the runtime's whole process tree and of sway, memory. Nothing
     is copied during this window. Then the page's **scene** (its largest canvas, else its largest svg) alone,
     then four bands across the page: the frames in which their pixels actually changed (*scene fps*, *content
     fps*). This tells an animation inside the page apart from page-wide effects, and from frames with no
     visible change. Scene fps is a **lower bound**: it counts changes in a 320×200 patch at the scene's centre.
   - **Press latency**, 8 trials plus 4 no-touch control trials (6 + 3 in this draft's run, to finish before the
     bench's other tests): touch down on a control, hold 300 ms, slide off,
     lift. The latency runs to the first frame whose pixels inside the control differ from a reference taken just
     before. This is the "pressed state" that §8's *tap to visible response* is about.
4. **Warm page switches**: 4 rounds through every page (3 in this draft's run). Tap a rail button (60 ms), and measure **from the release**
   (navigation happens on click): the first frame showing the new page complete (*content*), and the frame from
   which it stays settled. The 180 ms `view-in` ease means "complete" is reached while the page is still easing in:
   the detector correlates the view's bands with the settled page at any opacity and up to 10 px of offset. The
   ease-in itself is measured as the interval between frames that move the page.
5. **Memory** after every page has been shown: PSS (`smaps_rollup`) summed over every process in the runtime's
   cgroup (each runtime runs in its own transient user scope, so the whole tree is counted: Chromium's renderer,
   GPU and utility processes, WebKit's Web, Network and GPU processes, bwrap and dbus-proxy).
6. **Screen power**: `swaymsg output … power off`, 6 s, `power on`, repeated until the connector agrees, as the
   screen helper (`services/screen`) does. Then a page switch must still complete and the page must report the
   WebSocket live.

**What is pressed, and what never is.** Presses never produce a click: the touch slides off before lifting, and
the slide-off check proves this per runtime. The targets are UI-local controls (chart ranges, segmented toggles,
tabs, map zoom, the Home demo tiles) or a switch tile pressed without a click. Never a heater control, and nothing
that acts on pointerdown: dials, sliders, drag grips, dimmers, or any control inside an element with a React
pointer handler. A page with no such control gets no press measurement. The only real taps are on rail buttons
(navigation). Evidence that nothing was sent, from two sources for the whole benchmark window (18:30–20:05 bench
time): the `command_log` table and the device services' own journals ("TX allowed" / "command … from") show
only the soak's commands (channel 7 over the bus, channel 15 over the gateway WebSocket, labelled "kiosk" because
it connects on loopback, every ten minutes; `utils/soak/src/main.rs`), and none at all while the candidates ran.

**Page cross-checks.** The opt-in module `ui/src/bench/` (loaded only with `?bench=1`, its own chunk) and
`harness/uibench/page/ref-bench.js` (injected into the Python UI by `serve_ref.py`) report geometry (where the rail,
the view and the press targets are), the boot marks, Event Timing and long tasks where the engine has them, and the
WebGL renderer. No time in the results comes from the page except the "data arrived" mark used to pick the cold-start
frame.

## 3. Results (headless, not acceptance)

Bundle `1d0df4e1bdf9` (all nine pages; the Energy page's WebGL battery bank not yet in it), headless 1920×1200 @ 60 Hz,
2026-11-23 19:21–19:57 bench time, with the 1-hour soak running on the same machine (load 0.6–2.4). Raw results:
`harness/out/uibench/20261123-headless-1920x1200/` (one JSON per runtime, `summary.md`, `perturb.txt`).

**Read these with three caveats:**

1. **Headless, not acceptance.** No scanout; timestamps are commit times (§2).
2. **The instrument costs something on GPU-heavy pages** (`perturb.txt`, Chromium, the page's own rAF over 12 s):

   | page | nothing captured | ticker only (16×16) | one 320×200 patch | four bands |
   |---|---|---|---|---|
   | Home | p95 16.7 ms | p95 22.5 ms, 56.5 fps presented | p95 32.4 ms | p95 34.9 ms, 49.4 fps presented |
   | Network | p95 16.1 ms | – | p95 16.7 ms | p95 21.6 ms |

   On the calibration page nothing was measurable (static pages were not checked). On Home, where the energy flow
   keeps Chromium at 70–85 % of a core, every wlroots screencopy, even 16×16, costs sway part of each frame. So on
   Home and Network the press and page-switch times are **upper bounds** and the fps figures **lower bounds**.
   This was measured in **Chromium only**: whether the cost differs between runtimes, and so whether the runtimes'
   differences on Home are real, is not established yet (`perturb.sh` gets a runtime argument for the next run).
   The fix for the acceptance run is in §9.
3. **The Python runs are contaminated**: two runtime windows left over from an aborted pass (a WPE and a Tauri,
   hidden behind the measured window) were alive while `python` and `python-static` ran. The candidates' runs
   were clean (checked: no other runtime scope). The Python numbers are shown as an indication; a clean rerun is
   pending (`run.sh python headless`, then `python-static`).

### Headline

| runtime | WebGL | tap → pressed p50 / p95 ms (no press seen) | switch → content p50 / p95 ms (warm) | first visit worst ms | settled p50 ms | ease-in step p95 ms Home / Climate / Switches | scene fps Energy / Network / Weather | boot → usable ms | PSS after boot / final MB | procs |
|---|---|---|---|---|---|---|---|---|---|---|
| python | ANGLE (Broadcom, V3D 7.1.10.2, OpenGL ES | 173.2 / 267.5 (50 of 72) | 288.4 / 394.0 | 2300 | 376 | 59 / 48 / 57 | – / 32 / – | 2298 | 307 / 512 | 11 |
| python-static | ANGLE (Broadcom, V3D 7.1.10.2, OpenGL ES | 174.0 / 235.3 (50 of 72) | 300.6 / 408.8 | 2326 | 364 | 48 / 60 / 33 | – / 32 / – | 2330 | 303 / 526 | 11 |
| wpe | Apple GPU | 36.5 / 54.0 (0 of 48) | 80.8 / 398.9 | 828 | 159 | 32 / 16 / 16 | – / 61 / – | 674 | 237 / 366 | 8 |
| tauri | Apple GPU | 41.7 / 69.8 (0 of 48) | 101.4 / 413.3 | 707 | 174 | 37 / 33 / 16 | – / 55 / – | 904 | 285 / 396 | 3 |
| chromium | ANGLE (Broadcom, V3D 7.1.10.2, OpenGL ES | 26.9 / 95.5 (2 of 48) | 98.7 / 483.4 | 1497 | 207 | 37 / 16 / 16 | – / 61 / – | 1450 | 330 / 407 | 11 |

### Provisional verdicts

| runtime | tap ≤ 50 ms (p95, misses = hold) | switch ≤ 150 ms (warm p95) | 60 fps Home/Climate/Switches | ≥ 30 fps Energy/Network/Weather | PSS ≤ 400 MB | power-on ≤ 15 s |
|---|---|---|---|---|---|---|
| python | **FAIL** | **FAIL** | **FAIL** | pass (1 of 3 measured) | **FAIL** | boot script (not run) |
| python-static | **FAIL** | **FAIL** | **FAIL** | pass (1 of 3 measured) | **FAIL** | boot script (not run) |
| wpe | **FAIL** | **FAIL** | **FAIL** | pass (1 of 3 measured) | pass | boot script (not run) |
| tauri | **FAIL** | **FAIL** | **FAIL** | pass (1 of 3 measured) | at the limit (one run) | boot script (not run) |
| chromium | **FAIL** | **FAIL** | **FAIL** | pass (1 of 3 measured) | at the limit (one run) | boot script (not run) |

### python (reference UI, live-fed; see the note above: contaminated, rerun pending)

WebGL: webgl2: ANGLE (Broadcom, V3D 7.1.10.2, OpenGL ES 3.1); slide-off check (no click from a moved touch): True  
Cold start (launch → ms): first compositor frame 5, first paint 1597, page's data mark → frame 1684, content 1990, usable (settled) 2298; light frame before content: False (max mean luma 33)  
Screen power off 6001 ms → on (1 tries): first frame after 130 ms, page responds afterwards: True  
Processes (PSS MB): chromium 241, chromium 117, chromium 54, chromium 40, chromium 18, chromium 17; render node open in: chromium; software GL (llvmpipe threads) in: none  

| page | first visit content / settled ms | warm content p50 / p95 ms | warm settled p50 ms | ease-in step p95 ms | press p50 / p95 ms (n, miss, null+) | target | presented fps | content fps (idle) | scene fps, lower bound (dropped) | CPU runtime / sway % | PSS MB |
|---|---|---|---|---|---|---|---|---|---|---|---|
| home | – / – | 268.8 / 288.4 | 408 | 59.3 | 223.9 / 291.3 (8, 2, 0, 6 upper bound) | Cabin | 59.9 | 30.4 | 40.3 (24) | 82.5 / 6.2 | 304 |
| energy | 429 / 429 | 394.0 / 399.6 | 401 | 48.6 | – / – (8, 8, 0) | 6 h | 2.0 | – | – (–) | 18.7 / 1.7 | 304 |
| climate | 223 / 367 | 232.7 / 256.1 | 341 | 47.8 | – / – (8, 8, 0) | 24 h | 2.0 | – | – (0) | 23.6 / 1.3 | 313 |
| weather | 1414 / 1414 | 380.6 / 384.9 | 430 | 72.6 | 171.8 / 201.8 (8, 0, 0) | wx-zin | 2.1 | – | – (0) | 23.9 / 1.3 | 427 |
| switches | 312 / 444 | 371.4 / 408.7 | 462 | 57.1 | 170.7 / 179.0 (8, 0, 0) | 32.2 | 2.1 | – | – (–) | 25.6 / 1.3 | 444 |
| network | 2300 / 2300 | 346.8 / 376.0 | 347 | – | – / – (8, 8, 0) | 6 h | 30.2 | 29.3 | 32.1 (97) | 58.0 / 4.0 | 484 |
| alerts | 284 / 397 | 279.6 / 282.9 | 365 | 43.5 | – / – (8, 8, 0) | btn-sound | 2.1 | – | – (–) | 39.7 / 1.6 | 493 |
| systems | 247 / 377 | 216.1 / 220.3 | 330 | 96.9 | – / – (8, 8, 0) | NMEA 2000 | 2.0 | – | – (–) | 42.7 / 1.2 | 500 |
| settings | 305 / 305 | 287.2 / 307.2 | 287 | – | – / – (8, 8, 0) | 5 min | 2.0 | – | – (–) | 40.6 / 1.2 | 503 |

Load before: `1.16 1.26 1.44 2/361 79496`; alerts: `{('warning', False): 4}`

### python-static (reference UI, frozen snapshot; contaminated, rerun pending)

WebGL: webgl2: ANGLE (Broadcom, V3D 7.1.10.2, OpenGL ES 3.1); slide-off check (no click from a moved touch): True  
Cold start (launch → ms): first compositor frame 5, first paint 1575, page's data mark → frame 1787, content 1951, usable (settled) 2330; light frame before content: False (max mean luma 33)  
Screen power off 6001 ms → on (1 tries): first frame after 133 ms, page responds afterwards: True  
Processes (PSS MB): chromium 247, chromium 124, chromium 54, chromium 40, chromium 18, chromium 17; render node open in: chromium; software GL (llvmpipe threads) in: none  

| page | first visit content / settled ms | warm content p50 / p95 ms | warm settled p50 ms | ease-in step p95 ms | press p50 / p95 ms (n, miss, null+) | target | presented fps | content fps (idle) | scene fps, lower bound (dropped) | CPU runtime / sway % | PSS MB |
|---|---|---|---|---|---|---|---|---|---|---|---|
| home | – / – | 300.6 / 320.3 | 381 | 48.1 | 226.6 / 240.8 (8, 2, 0, 6 upper bound) | Cabin | 60.9 | – | 41.1 (16) | 80.7 / 6.3 | 307 |
| energy | 317 / 414 | 408.8 / 425.6 | 426 | 48.9 | – / – (8, 8, 0) | 6 h | 1.0 | – | – (–) | 15.7 / 1.4 | 302 |
| climate | 215 / 376 | 174.4 / 260.5 | 359 | 60.0 | – / – (8, 8, 0) | 24 h | 1.0 | – | – (0) | 17.9 / 1.3 | 303 |
| weather | 1204 / 1237 | 344.5 / 350.5 | 416 | 39.7 | 171.6 / 175.7 (8, 0, 0) | wx-zin | 1.0 | – | – (0) | 18.5 / 1.3 | 438 |
| switches | 311 / 434 | 390.1 / 431.6 | 434 | 32.5 | 173.2 / 180.4 (8, 0, 0) | 32.2 | 1.0 | – | – (–) | 22.2 / 1.3 | 451 |
| network | 2326 / 2326 | 336.7 / 344.4 | 356 | 32.6 | – / – (8, 8, 0) | 6 h | 30.2 | 28.8 | 31.6 (110) | 55.6 / 4.0 | 493 |
| alerts | 238 / 389 | 265.6 / 286.6 | 359 | 16.7 | – / – (8, 8, 0) | btn-sound | 1.0 | – | – (–) | 33.0 / 1.3 | 504 |
| systems | 298 / 376 | 234.5 / 278.0 | 343 | 32.6 | – / – (8, 8, 0) | NMEA 2000 | 1.0 | – | – (–) | 32.9 / 1.2 | 509 |
| settings | 341 / 341 | 288.6 / 298.4 | 289 | – | – / – (8, 8, 0) | 5 min | 1.0 | – | – (–) | 33.7 / 1.2 | 515 |

Load before: `2.76 2.05 1.70 2/367 80839`; alerts: `{('warning', False): 4}`

### wpe

WebGL: webgl2: Apple GPU; slide-off check (no click from a moved touch): True  
Cold start (launch → ms): first compositor frame 302, first paint 600, page's data mark → frame 600, content 600, usable (settled) 674; light frame before content: False (max mean luma 30)  
Screen power off 6001 ms → on (1 tries): first frame after 53 ms, page responds afterwards: True  
Processes (PSS MB): WPEWebProcess 301, WPENetworkProce 37, MiniBrowser 27, xdg-dbus-proxy 1, bwrap 0, bwrap 0; render node open in: WPEWebProcess; software GL (llvmpipe threads) in: none  

| page | first visit content / settled ms | warm content p50 / p95 ms | warm settled p50 ms | ease-in step p95 ms | press p50 / p95 ms (n, miss, null+) | target | presented fps | content fps (idle) | scene fps, lower bound (dropped) | CPU runtime / sway % | PSS MB |
|---|---|---|---|---|---|---|---|---|---|---|---|
| home | – / – | 52.9 / 61.9 | 166 | 32.4 | 32.4 / 61.4 (6, 0, 0) | Cabin | 61.7 | 61.3 | 61.7 (0) | 50.5 / 4.2 | 239 |
| energy | 150 / 150 | 89.4 / 89.7 | 154 | 16.3 | 30.4 / 33.9 (6, 0, 0) | 6 h | 0.0 | – | – (0) | 1.9 / 0.2 | 252 |
| climate | 109 / 158 | 51.6 / 80.5 | 148 | 16.3 | 46.3 / 51.7 (6, 0, 0) | 24 h | 61.8 | – | – (0) | 10.5 / 3.5 | 264 |
| weather | 828 / 1397 | 152.8 / 164.6 | 410 | 94.0 | 32.0 / 43.4 (6, 0, 0) | wx-zin | 1.3 | – | – (0) | 4.9 / 0.2 | 313 |
| switches | 119 / 152 | 80.9 / 100.2 | 162 | 16.4 | 49.8 / 56.6 (6, 0, 0) | 32.2 | 0.0 | – | – (–) | 2.1 / 0.2 | 320 |
| network | 514 / 2300 | 398.9 / 410.1 | 399 | – | 41.8 / 45.2 (6, 0, 0) | 6 h | 60.8 | 54.9 | 60.6 (4) | 71.0 / 4.1 | 343 |
| alerts | 131 / 213 | 87.5 / 88.3 | 185 | 16.4 | 19.1 / 27.7 (6, 0, 0) | btn-sound | 19.5 | – | – (–) | 5.7 / 0.7 | 347 |
| systems | 83 / 180 | 63.0 / 70.0 | 152 | 16.4 | – / – (0, 0, 0) | – | 17.5 | – | – (–) | 13.1 / 1.0 | 356 |
| settings | 76 / 76 | 75.1 / 75.2 | 75 | – | 35.2 / 39.4 (6, 0, 0) | 5 min | 0.4 | – | – (–) | 2.0 / 0.2 | 358 |

Load before: `0.64 1.33 1.66 1/264 85474`; alerts: `{('warning', False): 4}`

### tauri

WebGL: webgl2: Apple GPU; slide-off check (no click from a moved touch): True  
Cold start (launch → ms): first compositor frame 240, first paint 240, page's data mark → frame 485, content 776, usable (settled) 904; light frame before content: True (max mean luma 245)  
Screen power off 6001 ms → on (1 tries): first frame after 65 ms, page responds afterwards: True  
Processes (PSS MB): WebKitWebProces 273, aetherrv-panel- 84, WebKitNetworkPr 39; render node open in: aetherrv-panel-, WebKitWebProces; software GL (llvmpipe threads) in: none  

| page | first visit content / settled ms | warm content p50 / p95 ms | warm settled p50 ms | ease-in step p95 ms | press p50 / p95 ms (n, miss, null+) | target | presented fps | content fps (idle) | scene fps, lower bound (dropped) | CPU runtime / sway % | PSS MB |
|---|---|---|---|---|---|---|---|---|---|---|---|
| home | – / – | 56.6 / 60.2 | 171 | 37.1 | 41.7 / 68.6 (6, 0, 0) | Cabin | 60.0 | 49.9 | 59.3 (8) | 64.2 / 4.7 | 296 |
| energy | 195 / 228 | 130.8 / 148.5 | 195 | 32.5 | 32.9 / 39.1 (6, 0, 0) | 6 h | 0.0 | – | – (0) | 2.1 / 0.0 | 311 |
| climate | 111 / 175 | 55.0 / 57.0 | 166 | 32.7 | 51.2 / 64.3 (6, 0, 0) | 24 h | 59.9 | – | – (0) | 25.6 / 3.9 | 323 |
| weather | 707 / 1221 | 201.7 / 211.2 | 560 | 98.5 | 40.3 / 77.9 (6, 0, 0) | wx-zin | 1.2 | – | – (0) | 3.6 / 0.7 | 349 |
| switches | 122 / 171 | 115.1 / 115.8 | 164 | 16.4 | 51.3 / 59.5 (6, 0, 0) | 32.2 | 0.0 | – | – (–) | 2.2 / 0.3 | 358 |
| network | 563 / 2376 | 413.3 / 444.6 | 413 | – | 64.0 / 71.4 (6, 0, 0) | 6 h | 57.8 | 44.5 | 55.4 (23) | 92.3 / 5.0 | 397 |
| alerts | 153 / 218 | 108.0 / 109.6 | 190 | 32.5 | 29.2 / 32.6 (6, 0, 0) | btn-sound | 19.3 | – | – (–) | 10.6 / 1.6 | 384 |
| systems | 84 / 170 | 60.7 / 64.7 | 174 | 32.6 | – / – (0, 0, 0) | – | 17.4 | – | – (–) | 17.4 / 1.5 | 396 |
| settings | 85 / 85 | 92.6 / 98.9 | 93 | – | 37.5 / 41.6 (6, 0, 0) | 5 min | 0.0 | – | – (–) | 1.8 / 0.2 | 398 |

Load before: `2.35 2.16 1.95 1/257 84904`; alerts: `{('warning', False): 4}`

### chromium

WebGL: webgl2: ANGLE (Broadcom, V3D 7.1.10.2, OpenGL ES 3.1); slide-off check (no click from a moved touch): True  
Cold start (launch → ms): first compositor frame 1020, first paint 1020, page's data mark → frame 1020, content 1392, usable (settled) 1450; light frame before content: False (max mean luma 22)  
Screen power off 6001 ms → on (1 tries): first frame after 141 ms, page responds afterwards: True  
Processes (PSS MB): chromium 142, chromium 110, chromium 54, chromium 40, chromium 18, chromium 17; render node open in: chromium; software GL (llvmpipe threads) in: none  

| page | first visit content / settled ms | warm content p50 / p95 ms | warm settled p50 ms | ease-in step p95 ms | press p50 / p95 ms (n, miss, null+) | target | presented fps | content fps (idle) | scene fps, lower bound (dropped) | CPU runtime / sway % | PSS MB |
|---|---|---|---|---|---|---|---|---|---|---|---|
| home | – / – | 77.0 / 114.0 | 202 | 36.5 | 114.8 / 195.5 (6, 2, 0, 4 upper bound) | Cabin | 58.2 | 40.0 | 48.1 (7) | 85.5 / 5.6 | 351 |
| energy | 501 / 501 | 274.6 / 366.5 | 356 | 16.4 | 12.7 / 26.9 (6, 0, 0) | 6 h | 0.4 | – | – (0) | 3.7 / 0.1 | 368 |
| climate | 153 / 225 | 62.1 / 63.6 | 191 | 16.5 | 61.9 / 63.8 (6, 0, 0) | 24 h | 61.4 | – | – (0) | 28.6 / 4.9 | 316 |
| weather | 1497 / 1497 | 260.7 / 827.7 | 266 | – | 31.1 / 36.8 (6, 0, 0) | wx-zin | 0.0 | – | – (0) | 7.0 / 0.4 | 416 |
| switches | 180 / 246 | 88.5 / 88.7 | 203 | 16.4 | 22.1 / 32.1 (6, 0, 0) | 32.2 | 0.0 | – | – (–) | 4.6 / 0.0 | 375 |
| network | 770 / 2357 | 439.6 / 483.4 | 603 | 32.6 | 58.5 / 95.5 (6, 0, 0) | 6 h | 61.0 | 50.6 | 61.0 (1) | 72.5 / 4.8 | 400 |
| alerts | 151 / 217 | 139.4 / 160.5 | 226 | 16.4 | 18.0 / 26.6 (6, 0, 0) | btn-sound | 0.2 | – | – (–) | 2.1 / 0.1 | 391 |
| systems | 69 / 254 | 52.6 / 55.6 | 167 | 16.4 | – / – (0, 0, 0) | – | 3.4 | – | – (–) | 7.1 / 0.3 | 387 |
| settings | 79 / 79 | 97.2 / 98.7 | 97 | – | 14.3 / 21.1 (6, 0, 0) | 5 min | 0.0 | – | – (–) | 2.3 / 0.0 | 385 |

Load before: `1.38 1.20 1.51 1/267 86305`; alerts: `{('warning', False): 4}`

### What the numbers say

- **Press feedback.** Chromium is fastest on light pages (Switches 22 / 32 ms, Alerts 18 / 27 ms, p50 / p95). WPE
  and Tauri take ~50 / 57–60 ms on Switches, and there the press shows in the first or second frame the runtime
  presents after the touch (`frames_to_detect` 0–1): WebKit simply presents nothing for ~50 ms. On Home every
  runtime reads slower (WPE 32 / 61, Tauri 42 / 69, Chromium 115 / 196 with 2 of 6 presses never seen), and those
  figures include the instrument's cost (caveat 2). The Python UI shows a pressed state only through CSS
  `:active`, which Chromium applies to a touch late (Home ~225 ms, Switches ~172 ms), and on 50 of 72 trials
  (controls with no `:active` style) not at all within the 300 ms hold.
- **Event Timing cross-check (Chromium).** The page's own `pointerdown` durations (to the next paint, 8 ms
  granularity) agree with the compositor's press times on every page but Home: Switches ~24 ms (compositor 22),
  Energy 16–40 (13–27), Climate 56–64 (62), Weather 32–48 (31–37), Alerts 16–40 (18–27), Network 48–96 (59–96).
  On Home they are mostly 56–80 ms (two at 224 and 376) where the compositor reads 87–195 ms: the instrument
  inflates Home, and Chromium's true press response there is about 60–80 ms, still over 50.
- **Page switches** (warm, from the release, new page complete): 50–140 ms on most pages for all three; exceptions
  are **Network ~380–480 ms** on every runtime, **Weather** 150–260 ms, and Chromium's **Energy** 275 / 367 ms.
  The Network page's sky (`ui/src/pages/network/Sky.tsx`) disposes its renderer, textures and WebGL context
  whenever the page is hidden and builds them again when it is shown, which fits a ~400 ms switch on all three
  runtimes; the measurement itself has not isolated that cause (the next run masks the scene from the bands, so
  that a slowly moving sky cannot delay the "complete" test). First visits (lazy chunk, first render, WebGL set-up)
  are 0.5–1.5 s for Weather and Network. The Python UI is 230–400 ms on every page.
- **Motion.** The page-switch ease-in moves in 16 ms steps on Climate and Switches everywhere. On Home it reads 32–37
  ms on all three, the same as the instrument's own cost on that page (rAF p95 34.9 ms with the bands), so Home's
  ease-in is **not measurable** with this instrument yet. The Network sky changes 55–61 times a second on all
  three (lower bound); the Home energy flow 61 (WPE), 50 (Tauri), 40–48 (Chromium). The Weather scene did not change
  at all during its measurement window, on any runtime (why is not established: whether recorded radar frames
  were playing was not checked), and the Energy page had no animated scene in this bundle: **1 of the 3 scenes is
  measured.**
- **Idle CPU** (one core = 100 %): Home WPE 50 %, Tauri 64 %, Chromium 85 %, Python 82 %; Network 71 / 92 / 73 %;
  Climate 10 / 26 / 29 % (something small there presents 60 frames a second; the bands saw no change, so it is
  outside them); static pages 2–7 %.
- **Memory** after every page was shown (PSS of the whole tree, one five-minute run each): WPE **366 MB** (8
  processes, 3 of them bwrap / dbus-proxy at ~1 MB), Tauri **396 MB** (3 processes), Chromium **407 MB** (11),
  Python UI 512–526 MB. Tauri and Chromium are within 3 % of the 400 MB limit after one run: at the limit, not a
  verdict. This is not a long-run figure: the van's 1.15 GB for the Python UI is after days of uptime, and a soak of
  the chosen runtime is the way to see growth. At boot (Home only): 237 / 285 / 330 / 303–307 MB.
- **Cold start** (warm caches, launch → first usable frame): WPE **0.67 s**, Tauri 0.90 s, Chromium 1.45 s,
  Python UI 2.3 s. **Tauri shows a light frame first** (mean luminance 245 of 255 at 240 ms, its GTK window's
  default background) before the dark UI: a white flash at the splash hand-off (§4). WPE and Chromium do not.
- **Screen power off/on**: every runtime kept running and recovered (first frame 53–141 ms after power-on, a page
  switch completed, WebSocket live).
- **GPU**: every runtime renders on the V3D (the render node is open in the GPU-rendering process, no llvmpipe
  threads anywhere). Chromium's WebGL reports `ANGLE (Broadcom, V3D 7.1.10.2, OpenGL ES 3.1)`; WebKit masks its
  renderer string as "Apple GPU", hence the process check.

## 4. Screen power and the boot splash

**Screen power.** Today sway turns the panel off and on (`swaymsg output <name> power off|on`, through the screen
helper, `services/screen` in the Rust system). The runtime never sees it: sway stops sending it frame callbacks
while the output is off, and the page keeps running (WebSocket, timers). Each run turns the output off for 6 s,
back on as the helper does (by name, repeated until the connector reports on), then checks that a page switch
still completes and that the page reports its WebSocket live. Results per runtime are in §3 ("screen power"). On
the headless output this proves the runtime side only: the Pi 5's HDMI behaviour (the first power-on after an off
being ignored, which the helper works around) and the van panel's backlight need the real screen and the van.
Not exercised: the page's own screen-off button, which calls the helper over HTTP on 127.0.0.1:8091 (a
cross-origin `fetch` from the page). The helper was not running in the benchmark session, so the page did not
offer the button. Whether WPE's and WebKitGTK's fetch to it works as Chromium's does is still to check.

**Boot splash hand-off.** As on the Python kiosk, `aetherrv-panel.service` orders itself after Plymouth and runs
`plymouth quit --retain-splash` before starting sway. The splash's last frame stays in the display's framebuffer
until sway's first frame replaces it, which is sway's own background (`output * bg #000000 solid_color`), and the
runtime's first paint follows. So for every runtime the sequence is splash → black → UI. The black gap is the
runtime's launch → first paint (§3, cold start), plus sway's own start (not measured). None of the three runtimes needs
anything runtime-specific for the hand-off, since sway owns the display:

- the cold start checks every frame before the UI's content for a light frame (mean luminance). WPE (≤ 30 of
  255) and Chromium (≤ 22) show none: their first frame is the page's dark background. **Tauri shows a white
  frame first** (luminance 245, 240 ms after launch, ~0.5 s before the UI), its GTK window's default background:
  at the hand-off that is a white flash between the splash and the UI. The Tauri app must set its window and
  webview background to black before it could be used;
- WPE's MiniBrowser defaults to a white view background too (`--bg-color`); it did not show here because the page
  painted first, but a WPE launcher should set black as well, so that a slow first paint can never flash;
- to keep the splash on screen until the UI paints (no black gap at all), give sway the splash as its background
  (`output * bg /usr/share/plymouth/themes/aetherrv/splash.png center #000000`, which needs the `swaybg` package).
  This is independent of the runtime.

Plymouth is not installed on the bench, so this is reasoned from the units and the measured first frames, not
watched. The shutdown splash's ordering (`aetherrv-plymouth-after-panel.conf`) is unaffected by the runtime.

## 5. Power-on to usable UI (§8: ≤ 15 s)

Not measured yet: it needs a reboot of the bench, which the isolation tests and the soak did not allow, and the
monitor (sway needs an output). What is known without rebooting:

| stage | bench figure | source |
|---|---|---|
| firmware (Pi 5 bootloader, until the kernel starts) | not visible to Linux | measured by `boot-measure.sh` (estimate) or a stopwatch |
| kernel | 0.77 s | `systemd-analyze` (this boot) |
| userspace to `graphical.target` | 4.53 s | `systemd-analyze` (this boot; the aetherrv units were not enabled at boot, so the gateway is not in it) |
| sway start → runtime launched | not measured | `boot-measure.sh` reports it |
| runtime launch → usable frame | see §3, "boot → usable" | the cold-start scenario (a used profile, warm page cache: a lower bound for a real boot) |

`harness/uibench/boot-measure.sh` measures the whole chain by a reboot: `arm <runtime>` installs
`uibench-boot.service`, `aetherrv-panel.service`'s twin on tty1 (same ordering after the gateway, same wait for
:8080, same `plymouth quit --retain-splash`), whose sway session launches the runtime under `aetherrv-uibench`.
After `sudo reboot`, `report` prints firmware (estimated from the previous boot's last journal entry, so slightly
high), kernel, userspace, gateway serving, panel started, runtime launched and first usable frame, all on the
kernel's clock, and the total. `disarm` removes it. A cold power-on adds the power supply's and the bootloader's
cold path: time it once with a stopwatch to compare.

## 6. Recommendation (provisional)

Provisional, from headless numbers with the caveats in §3; the owner confirms the choice after the acceptance run.

**WPE WebKit is the lightest, and nothing measured so far puts it behind the others where it matters.** Lowest
memory (366 MB after every page, the only one clearly under the 400 MB limit; Tauri 396, Chromium 407), lowest CPU on
the animated pages (Home 50 % of a core against 64 % and 85 %), fastest cold start (0.67 s against 0.90 and 1.45 s),
and page switches as fast as the others or faster. Chromium shows the pressed state fastest on light pages (20–30 ms
against ~50 ms) but is the heaviest on Home and over the memory limit. Tauri sits between the two, adds a native
binary to build and maintain, and flashes white at start-up until its background is fixed.

**No runtime meets every §8 number yet, and most of what fails is the same on all three:**

| §8 number | status (headless) | what decides it |
|---|---|---|
| tap ≤ 50 ms | fails on Home everywhere (Chromium ~60–80 ms by Event Timing; the compositor figures are inflated there); WPE/Tauri ~57–60 ms p95 on Switches; Chromium passes on light pages | Home's energy flow; on Switches, WebKit presents no frame for ~50 ms after a touch |
| page switch ≤ 150 ms | fails everywhere on Network (~380–480 ms) and Weather (150–260 ms), and on Chromium's Energy (275 / 367 ms); the other pages pass | the Network sky disposes its WebGL context when hidden and rebuilds it when shown (from its source; not isolated by measurement); the Weather map |
| 60 fps Home / Climate / Switches | Climate and Switches pass; Home not measurable yet (its 32–37 ms equals the instrument's cost there) | the instrument (§9), then Home's energy flow at 50–85 % of a core |
| ≥ 30 fps scenes | 1 of 3 measured: Network passes on all (55–61 fps) | the Weather scene did not change during the run (cause not established); the Energy WebGL bank is not in the bundle yet |
| memory ≤ 400 MB | WPE 366 MB; Tauri 396 and Chromium 407 are within 3 % of the limit after one five-minute run each | a soak of the chosen runtime for growth |
| power-on ≤ 15 s | not measured (needs a reboot and the monitor) | `boot-measure.sh` |

The runtime differences on Home rest on Chromium-only perturbation data (§3, caveat 2), so they are provisional.
The next steps are in the UI as much as in the runtime choice: make the Network sky (and probably the Weather map)
survive a page switch, and lighten the Home energy flow. Then rerun on the real screen (§9) and pick by those
numbers. If the owner prefers the most familiar engine over the lightest, Chromium is the van's runtime today and
works, but at about 85 % of a core on Home and at the memory limit.

## 7. What the bench cannot prove

- Real touch feel and the Waveshare panel's own touch latency (the injected touchscreen starts at the kernel's
  input layer).
- The panel's scanout and response time; on the bench monitor, its own processing (game mode or not).
- Hold-to-dim on a real dimmer, and screen power control and wake-on-touch on the van's panel (the bench monitor
  has no touch, and its HDMI power behaviour differs).
- Plymouth: not installed on the bench, so the splash → sway → runtime hand-off is reasoned from the panel unit and
  the measured first frames, not watched.

These go into `docs/CUTOVER.md` as checks on the van.

## 8. Reproducing

```sh
harness/uibench/full.sh headless            # development numbers (no monitor needed)
harness/uibench/full.sh screen              # acceptance: 1920x1200 custom mode on the bench monitor
harness/uibench/full.sh screen-fallback     # if the monitor rejects it: 1920x1080, plus native 2560x1440
```

`full.sh` builds the bundle, installs it on the bench gateway (`/opt/aetherrv-rs/ui` only), builds the tool and the
Tauri app natively on the Pi, runs `run.sh all` there, and copies the results to `harness/out/uibench/`
(gitignored). On the bench, `harness/uibench/run.sh <runtime|all> <headless|screen> [WxH]` runs one runtime.
`boot-measure.sh` measures power-on to usable by a reboot (§5). `calibrate.sh <0|30|60> [frames options]` repeats the
calibration of §2 and `perturb.sh [page…]` the cost check of §3; `aetherrv-uibench frames`, `globals` and
`touch-test` are the tool's own diagnostics. Everything refuses to run on a host with `hat0`/`hat1` (the van).
Run so far as written: `session.sh` (headless), `run.sh` (headless), `aetherrv-uibench run/frames/globals/
touch-test`, `summarize.py`, `serve_ref.py`, `perturb.sh` (Chromium). Not yet run as written: `full.sh`,
`prepare.sh` (its steps were done by hand), `calibrate.sh` (the calibration used the same commands from a
scratch script), `boot-measure.sh`, the `screen` branch of `session.sh`/`run.sh`, and the runner with
`--pages home` (the boot measurement's settings).

## 9. Acceptance run on the real screen

The monitor was not detected while this draft was written (`/sys/class/drm/card1-HDMI-A-*/status` =
`disconnected`). When it is back:

1. **Improve the frame clock first.** On the DRM backend the kernel knows when each frame is actually scanned out.
   The `drm_vblank_event` tracepoint (tracefs, read by the root helper) gives the flip time of every frame sway
   presents at no cost to sway. It replaces the screencopy ticker, whose cost on GPU-heavy pages is caveat 2 in §3.
   Pixel patches stay only where a frame's content must be identified (presses, page switches), and
   `perturb.sh` reports their cost per page. Not built yet: it needs the monitor to develop against.
2. **Mode.** `harness/uibench/full.sh screen` asks sway for a custom 1920×1200 @ 60 Hz mode and records what the
   display controller actually drives (sway's mode and the KMS state from debugfs, in each run's `session.txt` and
   label). A screenshot is no proof that the monitor shows it, so the owner looks at the screen once. If the
   monitor rejects the mode: `full.sh screen-fallback` runs 1920×1080, which §8 then judges, and native 2560×1440 as
   the headroom check. The results say which case applied.
3. **Clean Python baseline**: `run.sh python screen …` and `run.sh python-static screen …` (both are in `all`).
4. **Power-on**: `boot-measure.sh arm <chosen runtime>`, `sudo reboot`, `boot-measure.sh report`, `disarm`.
5. **Pages still to come**: rerun once the Energy page's WebGL battery bank has landed, and with recorded radar
   frames playing on Weather, so that both WebGL scenes are measured.
