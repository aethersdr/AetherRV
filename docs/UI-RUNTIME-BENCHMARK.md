# UI runtime benchmark

Which runtime should show the React UI on the van's touchscreen: (a) WPE WebKit, (b) a Tauri v2 app (WebKitGTK), or
(c) Chromium (prompt §7)? This page has the method, the UI changes the measurements led to, the final numbers, how
they stand against prompt §8, and a recommendation for the owner to confirm.

**The numbers are headless, by owner decision (2026-11-23).** Every figure here comes from sway's headless output at
**1920×1200 @ 60 Hz, the van panel's own resolution**, rendered on the bench Pi 5's GPU (V3D) by the compositor,
config and runtimes the van uses. No monitor was used. With no scanout, no figure includes the panel's display
latency. Frame times are the compositor's commit times (§2). The power-on time was measured by rebooting the bench
into a headless kiosk session (§8).

**In short:** WPE WebKit, through the panel's own kiosk launcher, meets the §8 numbers for taps (at p95 on every
page; 4 of 436 presses over 50 ms), page switches, frame rate and memory (375 MB PSS; plus 1382 MB of GPU buffers
that PSS doesn't count). Power-on is not met on every boot: median 14.74 s, max 15.16 s, 1 of 10 boots over 15.0 s,
against 16.96 s for the Python system on the same procedure; most of it is the Pi's firmware. Tauri uses the same
engine with more memory and CPU. Chromium shows a press fastest on light pages, but it misses the switch and memory
numbers, and its Weather frame rate was not shown to reach 30 fps. Recommendation: **WPE through the launcher**
(§9); the owner's choice is pending.

## 1. Setup

| | |
|---|---|
| Machine | bench Pi 5, 16 GB, NVMe, Arch Linux ARM, kernel 7.0.3-v8+, Mesa 26.3.1 (V3D 7.1), no CAN HAT |
| Compositor | sway 1.12 (wlroots 0.20.2), the van kiosk's config (`deploy/kiosk/sway.conf`) for every runtime: `harness/uibench/sway.conf.in` |
| Session | a transient unit with a PAM session on VT 8 (`harness/uibench/session.sh`), like `aetherrv-panel.service` on tty1: logind gives sway its seat (libinput for the injected touchscreen) |
| Output | sway's headless output (`WLR_BACKENDS=headless,libinput`), 1920×1200 @ 60 Hz |
| Backend | the whole Rust system under systemd against simulators (`harness/bench/bench-up.sh`), gateway on 127.0.0.1:8080 |
| UI | **Final pass (§5.1):** the bundle built from commit f9968b1, deployed to the bench and served by the gateway on 127.0.0.1:8080; the bundle id is its `index.html` sha256, `a78d994cf492`; the runtimes open `http://127.0.0.1:8080/?bench=1`. **The five-runtime pass (§5.2) and §3–§4:** the production bundle (`vite build`, hash `0bb37c08214f`, in every result). The gateway's installed bundle (`/opt/aetherrv-rs/ui`) was left as deployed: `harness/uibench/serve_bundle.py` served the final bundle from the bench user's directory on 127.0.0.1:18082 and passed `/api` and `/ws` through to the gateway. The runtimes opened `http://127.0.0.1:18082/?bench=1`. The source tree differs from that bundle only by the per-scene `?keepgl=` list, added for one attribution run (bundle `9c4426ea0987`, §4.1); the default behaviour is the same. The power-on runs (§8) used the gateway's installed bundle |
| Batteries | **From the 2026-11-24 07:16 pass on (§5.1):** four simulated EPOCH batteries (`harness/uibench/batteries.sh`: `sims/batteries-sim`, the real batteries-ble core with a stand-in for BlueZ; SoC 84–87 %, the bank discharging at 24.8 A), so the Energy page shows its 3D battery bank (owner Q-U6) with its current flowing. **Every earlier run had no battery data** (the bench has no Bluetooth batteries): the Energy page said "No per-battery data" where the bank goes, so no earlier figure includes the bank: not Energy's scene, switch or press figures, and not the memory figures |
| Fonts | `noto-fonts` installed on the bench for this. The van has it; without it the UI fell back to FreeSans, which has a different layout and raster cost |
| (a) WPE | wpewebkit 2.54.3: in the final pass, the panel's own kiosk launcher (`panel/wpe`, §9); before it, `MiniBrowser --fullscreen` with `WPE_PLATFORM=wayland` (WPE Platform API) |
| (b) Tauri | Tauri 2.12 (`panel/tauri/`, its own Cargo workspace), webkit2gtk-4.1 2.54.3, built natively on the Pi |
| (c) Chromium | chromium 158, the van's flags (`deploy/kiosk/chromium.sh`): `--kiosk --ozone-platform=wayland`, GPU on |
| Before | the Python UI (`web/`) served by the Python backend's own web app with **no device controls** (`harness/uibench/serve_ref.py`, from `harness/ui-reference/serve_snapshot.py`), in Chromium kiosk as on the van. *python* gets the gateway's live (simulated) state every second; *python-static* gets one frozen snapshot |

**Deviation from the prompt: no `cog`.** The prompt names `cog` for candidate (a). Cog is the launcher for WPE's
legacy libwpe API, which Arch Linux ARM doesn't package: its `wpewebkit` 2.54 is built for the new WPE Platform API
only, with no libwpe and no wpebackend-fdo. Candidate (a) therefore runs WPE WebKit through its own `MiniBrowser`
on the WPE Platform Wayland backend, fullscreen. The engine, its processes and its rendering path are the ones a cog
kiosk would use on a current WPE. On the van it runs through the panel's own WPE Platform launcher (`panel/wpe`, §9).

## 2. Method

Everything is measured from the **compositor's frames**, not from the page. `harness/uibench/run.sh` drives the
tool `utils/uibench` (`aetherrv-uibench`) against one runtime at a time, in the same sway session.

**Touch.** A uinput virtual touchscreen (absolute multitouch protocol B, one slot, `BTN_TOUCH`,
`INPUT_PROP_DIRECT`), mapped to the output by the kiosk's own `input type:touch map_to_output` line. libinput and
sway treat it exactly like the van's capacitive panel. Every injection is timestamped on CLOCK_MONOTONIC just before
the write.

**Frames.** Two kinds of Wayland screencopy client (`wlr-screencopy-unstable-v1`):

- the **ticker** is a 16×16 region copied with `copy_with_damage` on its own connection. wlroots tracks screencopy
  damage per client, not per region, so this copy completes once for every frame the compositor commits with damage
  anywhere on the output. Its `ready` time is that commit's time (CLOCK_MONOTONIC);
- **patches** are small regions copied the same way, one connection each, into GPU buffers (linux-dmabuf via GBM).
  Each is read once its fence has signalled. Each copy carries the commit time of the frame it shows, so pixels
  and frame times line up exactly.

Calibration (a test page with a known cadence, `harness/uibench/calib.html`, in Chromium):

| what | result |
|---|---|
| ticker on a page painting at 0 / 30 / 60 Hz | 0 / 30.9 / 61.3 frames per second, matching the page's own paint count (the headless output runs at 61.4 Hz) |
| `wlr-export-dmabuf` as the ticker (first attempt) | **rejected**: every request makes sway render a frame, so a static page read ~62 fps |
| a full-output copy every frame | **rejected**: costs sway ~7.7 ms per frame, the 60 Hz page dropped to ~51 fps |
| 4 thin bands (3 × 1784×8, 1 × 8×1104) or one 320×200 patch per frame, light page | no effect: 61.4 fps presented, 0 dropped, the page's rAF unchanged |
| 12 or more small patches per frame | the GPU backs up (fence latency 134 ms), the page drops to ~40–57 fps; not used |
| reading a patch right after `ready` | waits ~8 ms for the copy's fence, so reads happen when the dmabuf fd polls readable (30–200 µs) |

On busy pages the captures do cost something, and §3 measures how much per runtime.

**Timestamps are commit times.** On a real display a frame is scanned out at the next vblank after its rendering
has finished on the GPU. Headless has no scanout at all. The tap and page-switch numbers therefore leave out up to
one refresh (16.7 ms), plus any GPU work still pending at the commit (§3), plus the panel's own response.

**Scenarios, per runtime** (`aetherrv-uibench run`; all nine pages, in rail order):

1. **Cold start**: one warm-up launch first, so that caches and profiles are in the state a used kiosk's are. Then
   the timeline: launch → the first frame that differs from the black background (*first paint*) → the frame after
   the page's own "data arrived" mark (`aetherrv:snapshot`) → the first frame showing the settled Home page
   (*usable*). It also checks whether any frame before the content is light (a white flash at the splash hand-off).
2. **Slide-off check**: press a rail button, hold, slide 120 px, lift. The page must not navigate. This proves that
   a moved touch is not a click in that runtime, and every press below relies on it. All five passed.
3. Per page, a **first visit** (a cold page switch: lazy chunk, first render, WebGL set-up), then:
   - **Idle, nothing captured.** The ticker is off and no patch is copied, so the instrument costs nothing. This
     window gives the CPU of the runtime and of sway. It also gives the page's own requestAnimationFrame cadence:
     the runner asks the page for a 5 s rAF sample over a long poll (`GET /cmd` on the report port), and the page
     reports its frame intervals. CPU and rAF are sampled in separate windows.
   - **Idle, ticker only**: presented frames per second and memory. Then it captures the page's **scene** (its
     largest canvas, else its largest svg) alone, then four bands across the page, and counts the frames in which
     their pixels actually changed (*scene fps*, *content fps*). This tells an animation inside the page apart from
     page-wide effects, and from frames with no visible change. Scene fps is a **lower bound**: it counts changes in
     a 320×200 patch at the scene's centre. On Weather the runner first presses Play (the radar loop runs only on
     demand, as in the reference UI), checks from the page's report that the loop is playing, and presses Play
     again afterwards.
   - **Press latency**: 12 trials plus 3 no-touch control trials (the final pass, §5.1: 48 per page, 100 on Network).
     Each trial touches down on a control, holds 300 ms, slides off and lifts. The latency runs to the first frame
     whose pixels inside the control differ from a reference taken just before. That is the pressed state that §8's
     *tap to visible response* is about. The page also reports when its `pointerdown` handler ran, which splits the
     latency into input delivery and rendering.
4. **Warm page switches**: 3 rounds through every page. Each switch taps a rail button (60 ms) and is measured
   **from the release**, because navigation happens on click. Two frames are found: the first frame showing the
   new page complete (*content*), and the frame from which it stays settled. The 180 ms `view-in` ease means the
   page is complete while it is still easing in, so the detector correlates the view's bands with the settled page
   at any opacity and up to 10 px of offset. Every switch is read a second time with the page's scene left out, so
   that a scene's own start is told apart from the rest of the page.
5. **Memory** after every page has been shown: PSS (`smaps_rollup`) summed over every process in the runtime's
   cgroup. Each runtime runs in its own transient user scope, so the whole tree is counted: Chromium's renderer,
   GPU and utility processes, and WebKit's Web, Network and GPU processes, bwrap and dbus-proxy. Also recorded is the
   v3d driver's total of GPU buffer objects (debugfs `bo_stats`, whole system). Most of a WebGL scene's memory is
   held there, outside any process's PSS.
6. **Screen power**: `swaymsg output … power off`, 6 s, then `power on`, repeated until the connector agrees, as the
   screen helper (`services/screen`) does. Then a page switch must still complete and the page must report its
   WebSocket live.

**What is pressed, and what never is.** No press ever becomes a click: the touch slides off before lifting, and the
slide-off check proves this per runtime. The targets are UI-local controls only: chart ranges, segmented toggles,
tabs, map zoom, the Home demo tiles, or a switch tile that is not a dimmer, pressed without a click (a tile's
pointerdown handler acts only for dimmers, and the bench module skips those). A heater control is never pressed.
Neither is anything that acts on pointerdown: dials, sliders, drag grips, dimmers, or any control inside an element
with a React pointer handler. A page with no such control (Systems) gets no press measurement. The real taps are on
rail buttons (navigation), and in the screen and session checks (§7, §8) on the top bar's screen-off chip and the
page title, which has no handler. For the five-runtime pass, the device services' journals (2026-11-23 23:21 to
2026-11-24 00:57 bench time, reboots included) show no command at all: no line matches "command … from", and the
only "TX allowed" lines are the n2k service's own address claim (PGN 60928), sent once or twice as it starts after
each reboot. The same search finds the soak's commands earlier the same evening (282 lines from 21:00 to 23:15). The
later runs (2026-11-24 02:17 to 05:29, the final pass and every boot included, the Python system's two boots too)
show the same: no command line, and no "TX allowed" line but the address claim (59, one or two per start). The
first command after that window is the Espar rerun's own (`harness/bench/espar-systemd.sh`, 05:29:35, `from
ctl:aetherrv`).

**Page cross-checks.** The opt-in module `ui/src/bench/` (loaded only with `?bench=1`, in its own chunk) and
`harness/uibench/page/ref-bench.js` (injected into the Python UI by `serve_ref.py`) report geometry (where the rail,
the view and the press targets are), the boot marks, the pointerdown handler times, the rAF samples, Event Timing
and long tasks where the engine has them, and the WebGL renderer. No time in the results comes from the page except
the "data arrived" mark, which picks the cold-start frame, and the rAF samples.

## 3. The instrument's own cost

`perturb.sh` loads Home and Network in each runtime and reads the page's own rAF intervals three times: with
nothing captured, with one 320×200 patch (the press and scene measurements), and with the four bands (content fps
and page switches). The five-runtime pass's bundle, headless 1920×1200:

| runtime | page | page rAF p95 ms, nothing captured | one 320×200 patch | four bands (max) | presented fps with the bands |
|---|---|---|---|---|---|
| WPE | Home | 17.0 | 17.0 | 17.0 (30) | 61.4 |
| WPE | Network | 17.0 | 18.0 | 32.0 (52) | 54.7 |
| Tauri | Home | 18.0 | 25.0 | 26.0 (36) | 41.6 |
| Tauri | Network | 18.0 | 24.0 | 25.0 (41) | 44.0 |
| Chromium | Home | 16.1 | 16.7 | 23.2 (317) | 55.1 |
| Chromium | Network | 16.1 | 16.7 | 21.4 (117) | 54.9 |

(WebKit reports rAF times in whole milliseconds, so 16–17 ms is one refresh.)

What this means for the results:

- **Frame rates are read with nothing captured.** The frame-rate verdicts use the page's own rAF cadence from the
  zero-capture window. Scene fps and content fps are lower bounds, most of all on Tauri, which loses a third of its
  frames to the bands.
- **WPE is the least disturbed.** One patch changes nothing measurable, so its press and switch figures are its
  own. Tauri's are slightly pessimistic. Chromium keeps its p95 but has occasional 100–300 ms stalls under capture.
  That shows in its Home press figures, which are partly the instrument's (§5.2).
- **GPU work still pending at each commit.** Every patch's fence latency (from the copy's `ready` to the buffer
  being readable) shows how much GPU work was queued ahead of the copy. Median per warm page switch: **WPE
  10–37 ms, Tauri 14–73 ms, Chromium 57–198 ms** (Chromium Home 198, Network 136, Energy 90). The bands add their
  own copies, so these are upper bounds. Part of that queue is the runtime's own rendering of the frame, which a
  real display waits for before scanning out. So, as an inference that needs scanout to confirm, Chromium's frames
  would appear on a real panel later than its commit times say, and WPE's much less so.
- One Chromium launch in `perturb.sh` aborted at shutdown (a core dump while its scope was stopped, between the
  Home and Network loads). The measurements on either side are complete.

## 4. UI changes the measurements led to

The first headless pass (2026-11-23, the earlier version of this page) found §8 misses that were the same on every
runtime, so they were in the UI. Below: what changed, why, and the effect. *Before* is that first pass (HEAD's UI);
*after* is the five-runtime pass (§5.2). Both are headless 1920×1200 with the same pages, repetitions and runtimes.
Where a like-for-like comparison inside that bundle exists, it is given too.

### 4.1 Page switches to the WebGL pages (Network, Weather, Energy)

**What the Network delay was made of.** Every hide disposed the page's WebGL scene (renderer, context, shaders,
textures), and every show rebuilt it. Warm switches to Network from Switches (not from Settings), p50 / p95:

| UI | sky | WPE | Chromium |
|---|---|---|---|
| before (dispose on hide) | 3D | 333 / 374 ms | 380 / 471 ms |
| before | 2D map (`?sky=2d`, no WebGL) | 67 / 120 ms | 78 / 91 ms |
| after (scene kept, paused), the five-runtime pass | 3D | 108 / 164 ms (without the sky: 100 / 108) | 139 / 153 ms (without the sky: 90 / 105) |

About 260–300 ms of the switch was the sky's WebGL rebuild: a new context, the shaders and the first frame. The
rest of the page switches in 70–80 ms. **No Wi-Fi scan or admin-helper call played any part.** During those
switches (Switches ↔ Network only, never Settings) the admin helper was not started (its socket had accepted no
connection), and neither it nor the gateway logged anything. Only the Settings page asks the helper for Wi-Fi status
(`/api/settings`, every 10 s while shown). On the bench iwd isn't running, so that call fails; it was not timed
here.

**Change** (`ui/src/lib/glkeep.ts`, `pages/network/Sky.tsx` + `sky3d.ts`, `pages/weather/wxmap.ts`,
`pages/energy/Bank3D.tsx` + `bank3d.ts`). A hidden page now **pauses** its scene: the loop stops and nothing is
drawn, but the context, shaders and textures stay. On show, the kept canvas still holds its last image, so the page
is complete at once. The scene resumes just after the page's first frame, so the switch frame carries no GL work.
An earlier version resumed inside that frame: WPE Network 106 / 165 ms with the 3D sky in the switch frame. A real
unmount still frees everything, and radar frames loaded while the page was hidden are attached on return.
`?keepgl=0` restores dispose-on-hide, and `?keepgl=sky,radar,bank` keeps only the scenes named. New Playwright specs
cover both modes: no context lost or created on hide/show, one context per scene after twenty switches, nothing
drawn while hidden.

**Before / after, all three runtimes** (warm, p50 / p95 ms; before = first pass, after = the five-runtime pass):

| page | WPE | Tauri | Chromium |
|---|---|---|---|
| Network | 399 / 410 → **108 / 164** | 413 / 445 → **105 / 106** | 440 / 483 → **139 / 153** |
| Weather | 153 / 165 → **106 / 117** | 202 / 211 → **106 / 118** | 261 / 828 → **178 / 225** |
| Energy | 89 / 90 → 83 / 83 | 131 / 149 → 98 / 136 | 275 / 367 → 383 / 438 |

The same comparison inside the final bundle (one warm round each, `?keepgl=0` vs the default, from the memory
runs below): Network WPE 401–431 → 106–156 ms, Tauri 449 → 156, Chromium 478 → 188; Weather WPE 140–144 → 114–118,
Tauri 144 → 114, Chromium 770 → 245. Energy: WPE 83–86 vs 78–82, Tauri 127 vs 134, Chromium 207 vs 175, **but the
bank was not on the page in these runs** (no battery data on the bench until §5.1; §1), so these Energy figures are
the rest of the page only. With the bank, §5.1. Chromium's slow Energy switch (210–440 ms) comes from the rest of the
page: the GPU queue during those switches stood at a median
80–90 ms, against 12–13 ms in WPE. It is not a WebGL cost and keeping the scene does not fix it.

**Memory, both ways** (every page shown once; PSS of the runtime's process tree; the v3d total of GPU buffer
objects, whole system, which includes sway's):

| runtime | run | kept (default): PSS / GPU buffers MB | disposed (`?keepgl=0`): PSS / GPU buffers MB |
|---|---|---|---|
| WPE | 1 | 362 / 1359 | 347 / 535 |
| WPE | 2 | 352 / 1363 | 347 / 535 |
| Tauri | 1 | 415 / 868 | 405 / 656 |
| Chromium | 1 | 597 / 789 | 410 / 606 |

**Which scene holds it.** The bundle with the per-scene switch (`?keepgl=` lists the scenes kept) ran one warm
round per setting:

| scenes kept | WPE: PSS / GPU buffers MB | WPE switch to Network / Weather / Energy ms | Chromium: PSS / GPU buffers MB | Chromium switch to Network / Weather / Energy ms |
|---|---|---|---|---|
| none (`0`) | 347 / 535 | 401–431 / 140–144 / 83–86 | 410 / 606 | 478 / 770 / 207 |
| radar only | 359 / 1331 | 416 / 115 / 86 | 418 / 613 | 469 / 217 / 195 |
| sky and bank (the bank was not on the page: §1) | 366 / 540 | 108 / 150 / 67 | 403 / 649 | 188 / 816 / 172 |
| all three (default) | 352–362 / 1359–1363 | 106–156 / 114–118 / 78–82 | 597 / 789 | 188 / 245 / 175 |

In WPE, **the kept radar holds about 800 MB of the GPU buffers** (presumably its loop's frame textures; not broken
down further) and saves only 25–35 ms
on the Weather switch, which is 140–150 ms without it. That is under the 150 ms limit, but only just. The sky
holds about 5 MB (the bank was not on the page: §1), and it saves about 300 ms on every switch to Network. Chromium's figures
don't add up per scene in a single round: its extra ~190 MB appears only with all three kept. The default stays
"all three kept", as measured in both passes of §5 up to f9968b1. **The owner chose to release the radar on hide**
(Q-U9, 2026-11-24): about 800 MB of GPU memory freed, for a Weather switch at 140–150 ms; the sky and the bank stay
kept. The final pass (§5.1) measures that default. A middle way for later is to keep the radar's context and
shaders, drop its frame textures on hide, and reload them on show.

- **WPE stays under 400 MB PSS both ways** (352–362 kept). Keeping costs it ~10 MB of PSS but ~825 MB of GPU
  buffers, almost all of it the radar's, which PSS doesn't count. WPE passes the 400 MB number only because §8
  counts PSS. On the 16 GB bench that memory is affordable; the van Pi's RAM decides whether it is there.
- **Chromium's PSS is 187 MB higher with all three scenes kept** (not with any one or two of them, in one round
  each), which puts it far over 400 MB. It was at 410 MB with dispose-on-hide.
- **Tauri** is over 400 MB both ways (405–415 here, 457 in the full pass).

### 4.2 Home at 60 fps

**Cause.** The energy flow's running dots were a CSS `stroke-dashoffset` animation on SVG paths. That made the
engine repaint the whole diagram (text, icons, rings) every frame, so Home cost 50–85 % of a core. A workstation
trace (Chrome, all edges running) showed 13 ms/s of paint and 60 ms/s of raster for it.

**Change** (`pages/home/Flow.tsx`, `styles/app.css`). The edges are now drawn on a canvas under the SVG: each frame,
six strokes and about a hundred dots, at the same positions, sizes, colours, speeds and fade. The SVG keeps the
nodes, the inverter and the hub. The `.f-edge` paths stay (hidden) as the edges' state. The same trace afterwards
shows no paint, no raster, and 6.6 ms/s of script. The look is unchanged; the screenshot differences are
anti-aliasing along the outlines (the diagram's layers are composited now) and the dots' animation phase. A new
spec checks that the dots run while an edge carries power, stop when none does, and are there at once on returning
to Home. One attempt that did not work was one SVG layer per edge group: Chrome repainted the whole promoted layer,
doubling the raster.

**Before / after** (Home idle, one core = 100 %). *Before* is HEAD's UI with today's bench module, run the same night
in the same session (`harness/out/uibench/r3/before-home/`); *after* is the five-runtime pass:

| runtime | CPU, nothing captured | CPU, ticker on | page rAF, nothing captured: fps (frames over 33 ms) | presented fps, ticker only |
|---|---|---|---|---|
| WPE | 43 → **22 %** | 54 → 26 % | 62.5 (0) → 62.5 (0) | 61.3 → 61.6 |
| Tauri | 63 → **37 %** | 67 → 50 % | 58.8 (1) → 58.8 (0) | 59.4 → 58.6 |
| Chromium | 81 → **34 %** | 78 → 42 % | 62.5 (0) → 62.5 (0) | 60.9 → 61.0 |

With the instrument's cost taken out (§3), **Home already held its frame rate before the change on all three**:
the page's rAF ran at refresh, and the frames presented with only the ticker were 59–61 a second. The first pass's
miss (32–37 ms steps, 40–50 content fps) was the four bands' cost on a busy page. What the canvas changes is the
cost of holding that rate: half the CPU or less. Content fps under the bands is still lower (Tauri 40, Chromium 39)
and is not a verdict.

### 4.3 Press feedback ≤ 50 ms

**Where the time goes.** `lib/press.ts` sets `.pressed` in the pointerdown event itself, and the pressed style has
no transition. The page's handler runs 0–2 ms after the injection on every runtime (the bench module reports it),
so input delivery is not the delay. The rest is the runtime presenting the frame. WebKit needs two refreshes to
present a change after an input on an idle page (Climate: about 31 ms). On Switches it needed three (about 52 ms),
and the first frame it presented after the touch was already the pressed one. The Switches target is the first
switch tile on the board (`.sw[data-sw]`). The runner measured it at 331×27 px: with the bench's simulated switch
list, the board lays its tiles out much flatter than the free-standing tile's 214 px.

**First pass → the five-runtime pass, before any fix** (all pages, p50 / p95 ms): WPE 36.5 / 54.0 → 35.4 / 54.5; Tauri
41.7 / 69.8 → 38.8 / 64.1; Chromium 26.9 / 95.5 (2 of 48 presses not seen) → 21.8 / 96.9 (none of 96 missed).
Chromium's Home (97 / 143) and Network (63 / 77) figures are partly the instrument's: for 8 and 7 of their 12
presses, the compositor presented a frame that the patch stream skipped before the detection. Those are upper
bounds. On its other pages Chromium shows the press in 13–30 ms.

**Compositor settings can't be tested headless.** sway's `max_render_time` (per output, and per window with
`for_window`) delays the repaint and the clients' frame callbacks to just before the next refresh. sway predicts that
refresh as the last presentation plus the refresh interval. wlroots 0.20.2's headless backend sends its present
events without a refresh interval (`.refresh` = 0), so the prediction always lies in the past and the delay is never
positive. Measured to confirm, WPE with `output * max_render_time 4` and `for_window [app_id=".*"] max_render_time
4`, 24 presses each: Climate 31 / 35 and Switches 50 / 55 ms, against 31 / 35 and 51 / 54 without. It needs a real
KMS output to show anything: a van check.

**What the pressed state costs WebKit** (2026-11-24). `harness/uibench/presscost.sh` asks the page's bench module to
toggle `.pressed` on the page's press target. Nothing is pressed or sent. It times the forced style and layout of
adding the class, and the rAF interval right after adding it against an unchanged one, so an overrun shows as a
doubled interval. Median of 12:

| runtime | target | style + layout of adding `.pressed` | rAF interval after it (unchanged: 16 ms) |
|---|---|---|---|
| WPE | Switches: a switch tile | **6.6 ms** | **30 ms**: the rendering update overruns the frame |
| WPE | Climate: a range button | 0.7 ms | 16 ms |
| WPE | Home: a demo tile | 1.2 ms | 18 ms |
| Chromium | Switches: a switch tile | 0.15 ms | 16 ms |

The tile does not repaint anything large. The cost comes from the tile *gaining* a transform and a filter
(`scale(.98)`, `brightness(1.18)`), so WebKit makes it a new render layer. The same toggle with an identity transform
and filter on the tile at rest costs 0.9 ms. A pressed state with only a background tint (no transform, no filter)
costs 0.1 ms and fits the frame. Dropping only the filter, or only the scale, changes nothing (7–8 ms).

**Real presses with the pressed-style variants** (WPE, 24 presses per page, p50 / p95 ms). The bench module
injects the variants with `&press=`:

| page | as is | identity transform + filter at rest (`restfx`) | background tint only (`bg`) | inset-shadow tint on the tiles (`tint`) |
|---|---|---|---|---|
| Home | 45 / 48 | 45 / 47 | 45 / 47 | 45 / 47 |
| Climate | 31 / 34 | 32 / 34 | 32 / 34 | 31 / 34 |
| Switches | **52 / 59** | **32 / 36** | 19 / 30 | 50 / 53 |
| Network | 43 / 50 | 43 / 45 | 43 / 47 | 43 / 46 |

The first round's experiments did not hit this: giving every target its own compositing layer up front (`promote`)
made Switches slower (60 / 64 ms), and dropping only the filter (`nofilter`) changed nothing. The background-only
state is fastest but changes the look of every control. The identity values at rest keep the look.

**Adopted** (`ui/src/styles/motion.css`, `ui/src/lib/press.ts`; commit 6bb2e87). In WebKit only, switch tiles carry
`transform: scale(1); filter: brightness(1)` at rest. The rule is `:where(html.wk-press .sw:not(:disabled))`, so it
has zero specificity and every state rule still wins. `html.wk-press` is set from the user agent: AppleWebKit without
Chrome/, i.e. WPE and WebKitGTK. The look is the same: Chrome screenshots of the board differ in 0.03 % of pixels at
rest (anti-aliasing at the tiles' rounded edges, at most 25 levels). Chromium is excluded because the rule made it
slower: Switches 22 / 24 → 38 / 61 ms unscoped, and 22 / 26 with the rule scoped to WebKit.

**After the change** (headless, p50 / p95 ms):

- **WPE**, a full run (all pages, 3 warm rounds, 24 presses per page): **33 / 46 over all pages (n = 192)**, against
  35 / 55 before. Switches **32 / 35**, Home 45 / 47, Climate 31 / 35, Weather 35 / 39, Energy 32 / 35, Alerts
  28 / 31, Settings 36 / 39.
- **Network sits at the 50 ms limit.** Its p95 varies from run to run between 45 and 59 ms (24 presses per run); over
  all 108 WPE Network presses, the p95 is 49 ms, with 4 of 108 above 50. Its target is unchanged by the fix; the 3D
  sky drawing at 60 fps on the same page is the likely source of the spread.
- **Tauri** (WebKitGTK, the same rule): Switches 54 / 58 → 34 / 36. Its Home (60 / 71) and Network (61 / 77) still
  miss.
- **No regressions on WPE.** Warm switches over all pages 77 / 112 ms (73 / 117 before); the page's rAF at refresh on
  every page, as before. Memory, from two light runs each, alternated, with and without the rule: PSS 353 / 359 MB
  with it, 356 / 364 MB without; GPU buffers the same. The full run read 390 MB PSS, but with twice the presses; the
  controlled comparison shows no difference.
- On the real panel, scanout adds up to one refresh to every figure.

### 4.4 The Weather radar loop

The scene "never changed" in the first pass because the radar loop does not play by itself. As in the reference UI
(`web/app.js` `rvPlay`), it starts on Play and stops when the page is hidden. Nothing in the UI was wrong. The
runner now presses Play before measuring the scene (a UI-local control), checks from the page's report that the
loop really started (`#wx-tl.playing`, frames loaded), and presses Play again afterwards. Measured with the loop
playing (scene fps, a lower bound, §2): **WPE 32, Tauri 39, Chromium 29 fps**. The loop draws up to 60 fps with
interpolated in-between images (`RADAR_MAX_FPS`), so the true rate is at or above these. The scene patch sees no
change where the radar shows no echo.

### 4.5 Tauri's white first frame

`panel/tauri/src/main.rs` now sets the window and webview background to the panel's page colour (#0b0c0f,
`--page`) from the first frame. Before, the first frame had luminance 245 of 255, 240 ms after launch. After, no
frame before the UI is lighter than 12 of 255, and the cold start is the same as before (usable at 903 ms; 904
before).

### 4.6 Memory under use (WPE, with the battery bank)

The runner reads the runtime's PSS once, at the end of a run. In WebKit's web process that figure moves with
WebKit's caches and uncollected garbage, so one reading can mislead. The final pass on f9968b1 reported 375 MB, but
its per-page readings had reached 397 MB. From 2026-11-24 07:16 on, with the battery bank on the page (§1), a
sampler also read the web process every 5–15 s.

- **What sets it is tapping, not time or page visits.** With few taps the web process levels off:

  | run | WPEWebProcess (sampled) | UI total at the end |
  |---|---|---|
  | 1 warm round, 6 taps per page, all scenes kept | – | 382 MB |
  | the same, the bank freed on hide | – | 381 MB |
  | the same, every scene freed on hide | – | 361 MB |
  | 8 warm rounds through every page, 6 taps per page | levels off at about 300 MB | 367 MB |

  The stress pattern of §5.1 (48 taps on every page, 100 on Network, 3 warm rounds, about 13 minutes) grows the web
  process steadily. Three runs ended at 499, 421 and 412 MB for the whole UI, with the web process peaking at
  360–374 MB. On one page alone: 60 taps on Alerts or Climate add about 20 MB, and 100 on Network add 45 MB.
- **It is WebKit's own memory, not the UI's data.** The growth is anonymous memory: WebKit's JavaScript heap,
  allocator and caches. The UI's own caches are bounded: the history cache holds one entry per chart and span.
- **A smaller JavaScript heap target changes nothing.** `JSC_forceRAMSize` at 1 GB and at 512 MB reached the web
  process (checked in its environment). The web process still peaked at 362 and 359 MB, and the UI ended at 381 and
  412 MB, on the same curve as without it.
- **WebKit's memory pressure handling holds it down, but slows page switches.** A 600 MB limit (frees caches past
  270 MB, checked every 5 s) kept the web process at or under 294 MB, with the UI at 341 MB at the end, and taps
  unchanged (47 ms p95). But warm page switches went to 118 / 207 ms p50 / p95: Home 158 ms p50, Energy 206,
  Network 242 p95. The caches it frees are what makes switching fast.
- **The owner's decision (Q-U11, 2026-11-24): accept, with a safety net.** The launcher sets WebKit's memory limit
  to 1000 MB: past 450 MB WebKit frees caches, past 600 MB everything it can, and it never kills the web process.
  Normal and heavy use stay under 450 MB, so it acts only on runaway growth. `docs/CUTOVER.md` §8 checks the panel's
  memory over days in the van.

## 5. Final results (headless 1920×1200)

### 5.1 Final pass (2026-11-24, commit 01aa558, the battery bank on the page)

The deployed build: commit 01aa558 on the bench (cross-compiled on the workstation, `harness/bench/cross-push.sh`;
same Rust 1.100.0 as the Pi; no Rust source changed after f9944f1), its UI bundle served by the gateway (bundle id
`52cec2bc33e8` in the results), the WPE launcher with its memory safety net (§4.6), and the owner's 2026-11-24
defaults: the radar freed on hide (Q-U9), the sky and the bank kept. The simulators were up, with four simulated
batteries (§1), so the Energy page showed its 3D bank with its current flowing. The run used the committed
harness, as `full.sh 1920x1200 python wpe-launcher` with `UIBENCH_KEEP_BUNDLE=1` and the §11 options, 2026-11-24
08:22–08:55 bench time. Results: `harness/out/uibench/20261124-0822-headless-1920x1200/` (gitignored).

| runtime | tap → pressed p50 / p95 ms | switch → content p50 / p95 ms (warm, all pages) | first visit, worst ms | page rAF fps, nothing captured: Home / Climate / Switches | scene fps: Energy / Network / Weather | cold start → usable ms | PSS after boot / after every page MB | processes |
|---|---|---|---|---|---|---|---|---|
| WPE (launcher) | **35 / 47** (436 presses; 13 over 50 ms, the slowest 94) | **93 / 139** | 647 (Weather) | 62 / 62 / 62 | 62 / 60 / – | 741 | 247 / **392** | 8 |
| Python UI, live (Chromium) | 182 / 253 (341 of 484 not seen) | 312 / 450 | 2216 (Network) | 62 / 60 / 60 | – / 34 / – | 1663 | 315 / 667 | 11 |

- **Tap:** 13 of 436 presses took over 50 ms and the slowest took 94 ms. Per page, p95 is at most 48 ms except
  Switches, at 53 ms (3 of 48). The owner judges taps as under 100 ms each, with 50 ms a loose target (Q-U12):
  **every press was under 100 ms**.
- **Page switch:** warm, every page ≤ 141 ms p95 (Weather 128 / 141 with its radar rebuilt on return, Energy
  120 / 139 with the bank). The owner judges switches warm (Q-U13). First visits after boot: Weather 647 ms,
  Network 492, Energy 436, the others 91–160.
- **Scenes:** Energy's bank 62 fps with its current flowing, at 53 % of a core; Network 60. The radar loop played,
  but there was no rain on the map this morning, so its frames didn't change and the method (screen changes) could
  not count them. The loop's figure is from the f9968b1 pass below: ≥ 32 fps, a lower bound, the same loop code.
- **Memory:** 392 MB after every page (WPEWebProcess 324, WPENetworkProcess 40, the launcher 26), under 400. In
  this pattern of heavy use it ranges 381–421 MB between runs (§4.6). The owner accepted that, with the launcher's
  safety net (Q-U11).
- **Cold start** 741 ms; no light frame before the UI. Screen power off 6 s, then on: the first frame 58 ms after.

**Warm page switches** (p50 / p95 ms over 3 rounds) **and presses** (p50 / p95 ms, *n* in brackets):

| page | switch, WPE | switch, Python UI | press, WPE | press, Python UI | target |
|---|---|---|---|---|---|
| Home | 59 / 64 | 269 / 284 | 45 / 47 (48) | 226 / 264 (48, 1 not seen) | `.quick-btn[data-i]` |
| Energy | 120 / 139 | 450 / 493 | 45 / 47 (48) | not seen (48) | `.seg button` |
| Climate | 66 / 82 | 372 / 392 | 32 / 40 (48) | not seen (48) | `#range-temp button` |
| Weather | 128 / 141 | 367 / 387 | 36 / 42 (48) | 175 / 201 (48) | `.wx-zoom button` |
| Switches | 78 / 87 | 383 / 388 | 32 / 53 (48) | 175 / 213 (48) | `.sw[data-sw]` |
| Network | 106 / 107 | 294 / 373 | 43 / 48 (100) | not seen (100) | `.seg button` |
| Alerts | 93 / 95 | 221 / 252 | 28 / 31 (48) | not seen (48) | `#btn-sound` |
| Systems | 97 / 99 | 220 / 240 | – | – | (none safe) |
| Settings | 69 / 94 | 312 / 330 | 35 / 39 (48) | not seen (48) | `.seg button` |

### 5.1a The pass on f9968b1 (2026-11-24, before the bench had battery data)

The Energy page had no bank in this pass (§1); everything else as above, with the radar kept on hide.

The deployed build: commit f9968b1 on the bench, its UI bundle served by the gateway on :8080 (bundle id:
`index.html` sha256 `a78d994cf492`; the whole-bundle hashes in the logs, `038ef4170074` from `prepare.sh` and
`4a6de7f14e65` in the results, hash the same files in two environments, an ssh shell and the run's user unit), the
WPE kiosk launcher built from `install.sh`'s source (`panel/wpe`), and the Python UI as the baseline
(`serve_ref.py`, live-fed). Run from the committed harness: `UIBENCH_KEEP_BUNDLE=1 UIBENCH_ARGS="--warm-rounds 3
--press-reps 48 --press-reps-for network=100 --null-reps 3 --fps-secs 5" full.sh 1920x1200 python wpe-launcher`,
2026-11-24 04:56–05:23 bench time, simulators up. Results: `harness/out/uibench/20261124-0354-headless-1920x1200/`
(gitignored).

| runtime | tap → pressed p50 / p95 ms | switch → content p50 / p95 ms (warm, all pages) | first visit, worst ms | page rAF fps, nothing captured: Home / Climate / Switches | scene fps: Network / Weather | cold start → usable ms | PSS after boot / after every page MB | processes |
|---|---|---|---|---|---|---|---|---|
| WPE (launcher) | **34 / 47** (436 presses, 4 over 50 ms) | **77 / 109** | 839 (Weather) | 62 / 62 / 62 | 61 / 32 | 716 | 250 / 375 | 8 |
| Python UI, live (Chromium) | 179 / 244 (341 of 484 not seen) | 285 / 390 | 2317 (Network) | 62 / 60 / 60 | 30 / – | 1701 | 317 / 553 | 11 |

- **Tap:** every page's p95 is at or under 50 ms with the launcher: the highest are Home and Weather at 48 ms. 4 of 436
  presses were over 50 ms (Home 1, Switches 1, Weather 2; the slowest 89 ms on Weather). **Network**, with 100
  presses, has a p95 of 47 ms and none over 50 (the slowest 49.9 ms).
- **Instrument cost** (`perturb.sh --runtime wpe-launcher`): the page's rAF p95 is 17 ms with nothing captured, with
  one patch and, on Home, with the bands; on Network the bands raise it to 32 ms (presented 54 fps), as with
  MiniBrowser (§3).
- **Cold start:** first compositor frame 309 ms, first paint 662 ms, usable 716 ms; no light frame before the UI
  (largest mean luminance 12 of 255). Screen power off 6 s → on: first frame 53 ms after, the page responds.
- **Memory:** PSS 375 MB after every page (WPEWebProcess 307, WPENetworkProcess 40, the launcher 26,
  xdg-dbus-proxy 1); the v3d driver's GPU buffers 1382 MB, whole system, most for the kept radar loop (§4.1).

**Warm page switches**, from the release to the new page complete, p50 / p95 ms over the warm rounds; in brackets,
the same switches with the page's scene left out where that differs. Last row: the slowest first visit (lazy chunk,
first render, WebGL set-up).

| page | WPE (launcher) | Python UI, live |
|---|---|---|
| Home | 54 / 62 (70 / 70) | 250 / 320 (284 / 320) |
| Energy | 72 / 80 (72 / 96) | 390 / 406 |
| Climate | 48 / 50 | 219 / 221 (219 / 238) |
| Weather | 109 / 114 (77 / 82) | 357 / 373 (259 / 292) |
| Switches | 75 / 97 | 281 / 330 |
| Network | 107 / 109 | 322 / 345 (316 / 322) |
| Alerts | 80 / 97 | 207 / 258 |
| Systems | 96 / 100 | 248 / 285 |
| Settings | 66 / 85 | 297 / 325 |
| first visit, worst | 839 (weather) | 2317 (network) |

**Press → pressed state shown**, p50 / p95 ms (*n* presses per page in brackets; *not seen*: no change within the
300 ms hold). The control pressed on each page is in the last column; Systems has none that is safe to press.

| page | WPE (launcher) | Python UI, live | target |
|---|---|---|---|
| Home | 45 / 48 (n 48) | 222 / 259 (n 48, 1 not seen) | `.quick-btn[data-i]` |
| Energy | 31 / 35 (n 48) | – / – (n 48, 48 not seen) | `.seg button` |
| Climate | 31 / 35 (n 48) | – / – (n 48, 48 not seen) | `#range-temp button` |
| Weather | 35 / 48 (n 48) | 172 / 196 (n 48) | `.wx-zoom button` |
| Switches | 33 / 38 (n 48) | 175 / 191 (n 48) | `.sw[data-sw]` |
| Network | 42 / 47 (n 100) | – / – (n 100, 100 not seen) | `.seg button` |
| Alerts | 28 / 34 (n 48) | – / – (n 48, 48 not seen) | `#btn-sound` |
| Systems | – | – / – (n 48, 48 not seen) | `.seg button` |
| Settings | 36 / 40 (n 48) | – / – (n 48, 48 not seen) | `.seg button` |

**Idle**: with nothing captured, the CPU of the runtime's process tree (% of one core) and the page's own rAF rate
(fps; frames over 33 ms in brackets); then scene fps with one patch (a lower bound).

| page | WPE (launcher) | Python UI, live |
|---|---|---|
| Home | 22 %, 62 (0); scene 62 | 82 %, 62 (0); scene 48 |
| Energy | 2 %, 62 (0) | 16 %, 60 (0) |
| Climate | 2 %, 62 (0) | 19 %, 60 (0) |
| Weather | 5 %, 62 (0); scene 32 | 21 %, 60 (0) |
| Switches | 2 %, 62 (1) | 23 %, 60 (1) |
| Network | 59 %, 62 (1); scene 61 | 49 %, 60 (7); scene 30 |
| Alerts | 5 %, 62 (0) | 37 %, 60 (5) |
| Systems | 13 %, 62 (0) | 41 %, 60 (6) |
| Settings | 2 %, 62 (0) | 35 %, 60 (6) |

### 5.2 The five-runtime pass (2026-11-23)

Bundle `0bb37c08214f`, 2026-11-23 23:22 – 2026-11-24 00:01 bench time, all five in one pass, then `perturb.sh` per
runtime (§3); WPE through MiniBrowser, before the switch-tile fix of §4.3 and the launcher. Driven by a scratch script
(§11). Results: `harness/out/uibench/r3/final/` (gitignored). Sections 3, 4, 6 (Tauri and Chromium) and 7 refer to
these figures.

| runtime | tap → pressed p50 / p95 ms | switch → content p50 / p95 ms (warm, all pages) | first visit, worst ms | page rAF fps, nothing captured: Home / Climate / Switches | scene fps: Network / Weather | cold start → usable ms | PSS after boot / after every page MB | processes |
|---|---|---|---|---|---|---|---|---|
| WPE | 35 / 55 | 73 / 118 | 678 (Weather) | 62 / 62 / 62 | 61 / 32 | 684 | 242 / 358 | 8 |
| Tauri | 39 / 64 | 90 / 125 | 730 (Weather) | 59 / 62 / 62 | 56 / 39 | 903 | 287 / 457 | 3 |
| Chromium | 22 / 97 | 114 / 384 | 1479 (Weather) | 62 / 60 / 60 | 60 / 29 | 1584 | 326 / 586 | 11 |
| Python UI, live | 178 / 232 (73 of 108 not seen) | 296 / 406 | 2314 (Network) | 62 / 60 / 60 | 30 / – | 1662 | 316 / 642 | 11 |
| Python UI, static | 180 / 227 (73 of 108 not seen) | 296 / 362 | 2340 (Network) | 62 / 60 / 60 | 29 / – | 1591 | 313 / 621 | 11 |

The Energy page had no battery data in this pass, so no bank and no scene fps (§1); with batteries its current
flow runs all the time and the bank is measured in §5.1. Every runtime renders on the V3D: the render node is open in the GPU-rendering process, with no llvmpipe threads anywhere.
Chromium's WebGL reports `ANGLE (Broadcom, V3D 7.1.10.2, OpenGL ES 3.1)`; WebKit masks its renderer string as
"Apple GPU", hence the process check.

**Warm page switches**, from the release to the new page complete, p50 / p95 ms over 3 rounds (p95 of three is their
maximum); in brackets, the same switches with the page's scene left out where that differs. Last row: the slowest
first visit (lazy chunk, first render, WebGL set-up).

| page | WPE | Tauri | Chromium | Python UI, live | Python UI, static |
|---|---|---|---|---|---|
| Home | 56 / 63 | 61 / 68 | 85 / 120 (101 / 120) | 232 / 270 (270 / 281) | 241 / 277 (277 / 294) |
| Energy | 83 / 83 | 98 / 136 (129 / 136) | 383 / 438 (400 / 438) | 346 / 417 | 338 / 362 |
| Climate | 47 / 50 | 46 / 49 | 62 / 67 | 199 / 210 | 177 / 195 (195 / 226) |
| Weather | 106 / 117 (74 / 85) | 106 / 118 (73 / 86) | 178 / 225 (161 / 190) | 340 / 348 (235 / 270) | 356 / 581 (234 / 339) |
| Switches | 77 / 85 | 80 / 87 | 84 / 98 | 305 / 406 | 342 / 349 |
| Network | 108 / 164 (100 / 108) | 105 / 106 | 139 / 153 (90 / 105) | 306 / 343 (306 / 327) | 303 / 327 (332 / 343) |
| Alerts | 84 / 103 | 109 / 125 | 144 / 266 | 240 / 247 | 228 / 241 |
| Systems | 61 / 63 | 62 / 77 | 114 / 125 | 249 / 299 | 231 / 255 |
| Settings | 72 / 88 | 95 / 97 | 88 / 96 | 298 / 330 | 296 / 305 |
| first visit, worst | 678 (Weather) | 730 (Weather) | 1479 (Weather) | 2314 (Network) | 2340 (Network) |

**Press → pressed state shown**, p50 / p95 ms (12 presses each; *not seen*: no change within the 300 ms hold). The
control pressed on each page is in the last column; Systems has none that is safe to press.

| page | WPE | Tauri | Chromium | Python UI, live | Python UI, static | target |
|---|---|---|---|---|---|---|
| Home | 46 / 47 | 56 / 63 | 97 / 143 | 222 / 277 (1 not seen) | 213 / 248 (1 not seen) | `.quick-btn[data-i]` |
| Energy | 31 / 35 | 33 / 36 | 13 / 22 | – / – (12 not seen) | – / – (12 not seen) | `.seg button` |
| Climate | 32 / 38 | 33 / 37 | 15 / 17 | – / – (12 not seen) | – / – (12 not seen) | `#range-temp button` |
| Weather | 38 / 43 | 41 / 44 | 30 / 38 | 171 / 192 | 174 / 182 | `.wx-zoom button` |
| Switches | 52 / 56 | 54 / 58 | 22 / 24 | 171 / 179 | 174 / 196 | `.sw[data-sw]` |
| Network | 40 / 44 | 62 / 72 | 63 / 77 | – / – (12 not seen) | – / – (12 not seen) | `.seg button` |
| Alerts | 28 / 32 | 31 / 33 | 17 / 20 | – / – (12 not seen) | – / – (12 not seen) | `#btn-sound` |
| Systems | – | – | – | – / – (12 not seen) | – / – (12 not seen) | |
| Settings | 36 / 39 | 39 / 40 | 15 / 23 | – / – (12 not seen) | – / – (12 not seen) | `.seg button` |

**Idle**: with nothing captured, the CPU of the runtime's process tree (% of one core) and the page's own rAF rate
(fps; frames over 33 ms in brackets); then scene fps with one patch (a lower bound).

| page | WPE | Tauri | Chromium | Python UI, live | Python UI, static |
|---|---|---|---|---|---|
| Home | 22 %, 62 (0); scene 62 | 37 %, 59 (0); scene 53 | 34 %, 62 (0); scene 45 | 81 %, 62 (0); scene 49 | 79 %, 62 (0); scene 50 |
| Energy | 2 %, 62 (0) | 2 %, 62 (1) | 4 %, 60 (0) | 15 %, 60 (0) | 13 %, 60 (0) |
| Climate | 2 %, 62 (0) | 2 %, 62 (0) | 3 %, 60 (0) | 19 %, 60 (0) | 14 %, 60 (0) |
| Weather | 4 %, 62 (0); scene 32 | 7 %, 62 (0); scene 39 | 4 %, 60 (0); scene 29 | 21 %, 60 (0) | 16 %, 60 (0) |
| Switches | 3 %, 62 (1) | 2 %, 62 (1) | 3 %, 60 (0) | 23 %, 60 (2) | 16 %, 60 (1) |
| Network | 61 %, 62 (1); scene 61 | 79 %, 62 (2); scene 56 | 58 %, 62 (0); scene 60 | 50 %, 60 (4); scene 30 | 42 %, 60 (0); scene 29 |
| Alerts | 4 %, 62 (0) | 7 %, 62 (0) | 5 %, 60 (0) | 37 %, 60 (7) | 32 %, 60 (0) |
| Systems | 9 %, 62 (0) | 12 %, 62 (0) | 6 %, 60 (0) | 40 %, 60 (8) | 30 %, 60 (0) |
| Settings | 2 %, 62 (1) | 2 %, 62 (0) | 2 %, 60 (0) | 35 %, 60 (8) | 33 %, 60 (1) |

**Cold start** (launch → ms; warm caches): first frame / first paint / usable. WPE 319 / 637 / 684, Tauri
245 / 816 / 903, Chromium 1008 / 1135 / 1584, Python UI 981 / 981 / 1662. No runtime shows a light frame before the
UI (largest mean luminance before the content: WPE 12, Tauri 12, Chromium 29 of 255).

**Screen power** off 6 s → on: every runtime kept running and recovered, with its first frame 31–121 ms after
power-on. A page switch then completed and the WebSocket was live.

**Processes** (PSS MB after every page): WPE: WPEWebProcess 293, WPENetworkProcess 37, MiniBrowser 27, bwrap and
xdg-dbus-proxy ~1. Tauri: WebKitWebProcess 334, the panel app 84, WebKitNetworkProcess 39. Chromium: 312, 117, 55, 40,
18, 18 and smaller.

**The Python UI**, as the reference: presses show only through CSS `:active`, which Chromium applies to a touch
late (170–220 ms). On 73 of 108 trials the control had no `:active` style at all, so nothing was seen within the
300 ms hold. Its switches take 200–420 ms, and it uses 79–81 % of a core on Home.

## 6. Against §8

Headless, by owner decision (above). *Pass* means the figure meets the number on this output; scanout on the real
panel adds up to one refresh to the tap and switch figures (§2). WPE's column is the final pass (§5.1: the kiosk
launcher, commit 01aa558, the battery bank on the page, judged as the owner set on 2026-11-24: taps each under
100 ms, Q-U12; switches warm, Q-U13). Tauri's and Chromium's are the five-runtime pass (§5.2), before the bank was
on the page, with the switch-tile fix measured
separately (§4.3).

| §8 number | WPE (launcher) | Tauri | Chromium |
|---|---|---|---|
| tap → visible response ≤ 50 ms (a loose target: each under 100 ms, Q-U12) | **met**: 35 / 47 ms p50 / p95 over 436 presses, every one under 100 ms (the slowest 94); 13 over 50 ms. Per page p95 ≤ 48 ms but Switches 53 | **not met**: 64 ms. Home 56 / 63, Network 62 / 72; Switches 54 / 58, 34 / 36 after the fix | **not met**: 97 ms. Home 97 / 143 (partly the instrument, §3), Network 63 / 77; the other six pages pass at 13–30 ms p50 |
| page switch ≤ 150 ms (warm p95, Q-U13) | **met**: 139 ms over all pages; every page ≤ 141 ms (Weather, its radar rebuilt on return). First visits after boot up to 647 ms | **met**: 125 ms; every page ≤ 136 | **not met**: 384 ms. Energy 383 / 438, Alerts 144 / 266, Weather 178 / 225 |
| 60 fps Home / Climate / Switches | **met**: 62 / 62 / 62 fps, the page's rAF at refresh; at most one frame over 33 ms in a 5 s sample | **met**: Home 59 fps, none over 33 ms; one on Switches | **met**: none over 33 ms |
| ≥ 30 fps Energy / Network / Weather scenes | **met**: Energy's bank 62, Network 60; Weather ≥ 32 (a lower bound, the f9968b1 pass: no rain on the map for the final one) | **met**: 56, ≥ 39 | Network 60; **Weather not shown**: the lower bound is 29 fps, so it may or may not reach 30 |
| memory ≤ 400 MB (PSS) | **met** in the final pass: 392 MB after every page; 381–421 MB under the same heavy use in other runs (499 once, before the safety net), accepted by the owner with the launcher's safety net (Q-U11, §4.6). PSS doesn't count the GPU's buffers (687 MB whole system, the radar freed on hide) | **not met**: 457 MB | **not met**: 586 MB (410 with the scenes disposed) |
| power-on ≤ 15 s (bench reboot, §8) | **not met on every boot, accepted by the owner as measured** (Q-U10): median 14.74 s, max 15.16 s, 1 of 10 boots over 15.0 s; the Python system 16.96 s median on the same procedure | **not met**: 15.6 s (old runner, earlier panel unit) | **not met**: 15.9 s (old runner, earlier panel unit) |

Not measurable headless: the panel's scanout and response, and real touch feel (§10).

## 7. Screen power and the boot splash

**Screen power.** Today sway turns the panel off and on (`swaymsg output <name> power off|on`, through the screen
helper, `services/screen` in the Rust system). The runtime never sees it: sway stops sending it frame callbacks
while the output is off, and the page keeps running (WebSocket, timers). Each run turned the output off for 6 s and
back on as the helper does (by name, repeated until the connector reports on). It then checked that a page switch
still completes and that the page reports its WebSocket live. All five passed (§5.2). On the headless output this
proves the runtime side only. The Pi 5's HDMI behaviour (the first power-on after an off is ignored, which the helper
works around) and the van panel's backlight need the real screen.

**Screen off from the page** (2026-11-24, `harness/uibench/screen-check.sh`). The screen helper ran in the session as
the panel runs it (`aetherrv-screen` on 127.0.0.1:8091, answering with `Access-Control-Allow-Origin:
http://127.0.0.1:8080`), and the page came from the gateway on :8080. For the WPE launcher, Tauri and Chromium alike
(9 of 9 checks each):

- the page found the helper (`$screen.available`), so the screen-off control is offered;
- the control's own path (`screenOff()`: the cross-origin `POST /screen/off`) turned the output off, and the page
  raised its wake shield;
- the wake path an alert takes (`POST /screen/on`) turned it back on, the shield dropped and the page was live.

While the output is off the page is hidden, so its timers run late (its report came about 6 s after the command
instead of 2.5 s); nothing depends on that.

With a real tap (the WPE launcher; an injected touch on the screen-off chip in the top bar), the output went off. A
second tap while it was off woke it, through the page: the touch still reached the page, and its shield's
pointerdown called `POST /screen/on`. The helper's own wake-on-touch path did not take part. It reads the touch
panel's evdev node through `/dev/input/by-id` and `by-path`, and an injected (uinput) touchscreen has neither link.
Touch-to-wake through the helper stays a van check (CUTOVER §8).

**Boot splash hand-off.** As on the Python kiosk, `aetherrv-panel.service` orders itself after Plymouth and runs
`plymouth quit --retain-splash` before starting sway. The splash's last frame stays in the display's framebuffer
until sway's first frame replaces it. That frame is sway's own background (`output * bg #000000 solid_color`), and
the runtime's first paint follows. So for every runtime the sequence is splash → black → UI. The black gap is the
runtime's launch → first paint (§5.2: WPE 0.64 s, Tauri 0.82 s, Chromium 1.14 s), plus sway's own start (§8). None of
the three runtimes needs anything runtime-specific for the hand-off, since sway owns the display:

- no runtime shows a light frame before the UI (§5). Tauri did until its window and webview background were set to
  the page colour (§4.5);
- WPE's MiniBrowser defaults to a white view background (`--bg-color`). It did not show here because the page
  painted first; the panel's launcher sets the page colour as the view's background (§9), and `launch.sh` passes
  `--bg-color` to MiniBrowser on its fallback path, so a slow first paint can't flash;
- to keep the splash on screen until the UI paints (no black gap at all), give sway the splash as its background
  (`output * bg /usr/share/plymouth/themes/aetherrv/splash.png center #000000`, which needs the `swaybg` package).
  This is independent of the runtime.

Plymouth is not installed on the bench, so this is reasoned from the units and the measured first frames, not
watched. The shutdown splash's ordering (`aetherrv-plymouth-after-panel.conf`) is unaffected by the runtime.

## 8. Power-on to usable UI (§8: ≤ 15 s)

**Not met on every boot.** On the final build (commit f9968b1: the WPE kiosk launcher, the runtime launched as
`launch.sh` launches it), 10 reboots took **median 14.74 s and at most 15.16 s from the end of the shutdown to a
usable UI; 1 of the 10 was over 15.0 s** (its firmware took 8.92 s). The Python system with Chromium, as on the van
today, measured the same way, took **16.89 and 17.04 s**. So the Rust system with WPE reaches a usable UI about 2.2
s sooner. 8.2–8.9 s of every figure is the Pi's firmware, before Linux starts; from the kernel's start to a usable
UI took 6.10–6.36 s (WPE) against 8.24–8.28 s (Python). What booted is a stand-in for the panel unit
(`uibench-boot`), ordered like the committed `aetherrv-panel.service` and using its installed PAM stack
(`/etc/pam.d/aetherrv-panel`), but running as the bench user on a headless output, with the runner in place of
`launch.sh` and the launcher built from the same source in the bench user's directory. `aetherrv-panel.service`
itself has not been run as kiosk on the bench.

**How it was measured.** Each figure is a reboot of the bench, driven from the workstation by
`harness/uibench/boot-time.sh`, with `harness/uibench/boot-measure.sh` on the bench. Before the reboot, `boot-measure.sh
arm <runtime> <order>` enables a one-shot unit, `uibench-boot.service`, that stands in for the panel unit and is
ordered like it (below). It waits for :8080 like the panel unit, then starts sway with the kiosk config on a headless
1920×1200 output. sway's one exec line starts the runtime under `aetherrv-uibench`, which records the launch and the
first frame of the settled Home page on CLOCK_MONOTONIC: time since the kernel started. After the boot, the bench's
own figures (unit start times from systemd, log lines from the journal) are read on the same clock.

- **The firmware** (the Pi 5's bootloader and firmware, which Linux cannot see) runs from the previous boot's last
  journal entry to the new kernel's start (the wall clock minus the uptime), both on the bench's clock.
  `boot-time.sh` reads it itself. That is an upper bound: the end of the shutdown runs after the journal stops. A
  reboot with no shutdown at all measures it exactly. Heaters idle, then `aetherrv.target`, the CAN helpers and the
  simulators stopped, the wall time written and synced, and `systemctl reboot -ff` (reboot(2) at once): **8.26 s**
  from reboot(2) to the kernel's start. The journal method gave 8.22 s on the same boot, so the shutdown tail is
  negligible and the firmware figures below stand. The firmware's own log (`vclog -m`) puts the bootloader's part
  at about 5.7 s (its own log is not visible), and the firmware's from there to "Starting OS" at 7.7 s.
- **From the workstation**, the script pings the bench's wired address (every ~0.1 s while it answers, ~0.15 s
  while it doesn't), then polls the gateway there. The bench's figures are placed on the workstation's clock with
  an offset taken over ssh. In every run the kernel started between the last ping reply before the reboot and the
  first after it.
- **The bench booted without its simulators** (they are transient units and don't come back after a reboot), with
  no Plymouth and no display. A cold power-on skips the shutdown but adds the power supply's ramp and the PMIC's
  start, which a reboot doesn't show.

**Results** (seconds after the kernel started; the firmware before it):

Rows marked *old runner* were measured with the runner starting the runtime only after its own capture set-up, which
put the launch about 0.45 s after sway's exec line (itemised below). The figures are as measured; nothing is
subtracted. The last three rows launch the runtime as `launch.sh` would.

| system, runtime, panel ordering | boots | firmware | gateway / backend serving | panel unit's main process | runtime launched | usable (settled Home) | **total** |
|---|---|---|---|---|---|---|---|
| Python, Chromium, the Python kiosk's unit (after `systemd-user-sessions`, `aetherrv.service`); *old runner* | 2 | 8.33, 8.41 | 5.81, 5.89 | 6.08, 6.21 | 6.85, 6.97 | 8.66, 8.85 | **17.0, 17.3** |
| Rust, WPE (MiniBrowser), the earlier panel unit (after `systemd-user-sessions`); *old runner* | 2 | 8.57, 11.14 | 4.86, 5.10 | 5.11, 5.37 | 5.95, 6.22 | 7.06, 10.17 | **15.6, 21.3** |
| Rust, Tauri / Chromium, the earlier panel unit; *old runner* | 1 + 1 | 8.54 / 8.22 | 4.70 / 4.72 | 4.97 / 4.97 | 5.80 / 5.81 | 7.07 / 7.65 | **15.6 / 15.9** |
| Rust, WPE (MiniBrowser), the new ordering: the gateway not after `network.target`, the panel after `systemd-logind` only (commit 5d8aeee); *old runner* | 3 | 8.26, 8.30, 8.66 | 3.64–3.79 | 4.92–5.33 | 5.80–6.17 | 6.93, 6.93, 7.37 | **15.6, 15.2, 15.6** |
| Rust, WPE (the kiosk launcher), new ordering + a PAM stack with `class=user-light` (no user service manager); *old runner* | 5 | 8.42–8.88 | 3.33–3.75 | 4.61–4.82 | 5.67–5.78 | 6.65–6.74 | **15.1–15.6 (median 15.4)** |
| the same, the runtime launched first (as `launch.sh` would) | 3 | 8.74, 8.44, 8.42 | – | 4.73, 4.55, 4.54 | 5.36, 5.19, 5.13 | 6.39, 6.23, 6.13 | **15.13, 14.67, 14.55** |
| **final build** (f9968b1; the stand-in ordered like the committed panel unit, with its installed PAM stack; runtime launched first; `poweron.sh rust 10`) | 10 | 8.23–8.92 | – | – | – | 6.10–6.36 | **14.44–15.16, median 14.74; 1 over 15.0** |
| **Python, Chromium** (as above), runtime launched first; `poweron.sh python 2` | 2 | 8.60, 8.80 | – | – | – | 8.28, 8.24 | **16.89, 17.04** |

- **Rust + WPE is 1.9–2.2 s faster than Python + Chromium from the kernel to a usable UI** on the final build
  (6.10–6.36 s against 8.24–8.28 s, both with the runtime launched first; with the old runner, 6.7–6.9 s against
  8.7–8.9 s). The Python backend starts only after `network.target` and takes about 1.4 s to serve; Chromium takes
  1.8 s from launch to a usable page, WPE about 1.0 s.
- **The new ordering serves the gateway about 1.1 s earlier** (3.6–3.8 s instead of 4.7–5.1 s), **but the panel's
  sway does not start earlier.** Its PAM session waits. pam_systemd opens a logind session, and logind orders every
  session's scope after `systemd-user-sessions.service`, which is ordered after `network.target`. Without
  `class=user-light`, the session also waits for the user's service manager (`user@.service`, likewise after
  `systemd-user-sessions`), which takes about 0.3 s to start. Measured on one boot: the unit starts at 3.69 s, its
  PAM session opens at 3.75 s, logind creates the session at 3.76 s, `systemd-user-sessions` finishes at 4.57 s, and
  only then does the unit's first command run (4.60 s). A unit with no PAM session connected to 127.0.0.1:8080 at
  3.51 s on its first try, so neither loopback nor the gateway is the wait.
- **`class=user-light` saves about 0.2–0.3 s** (usable at 6.65–6.74 s instead of 6.93–7.37 s, both with the old
  runner; the user-light boots ran the launcher, the others MiniBrowser, whose launch-to-usable times are within
  about 0.1 s of each other). It is now the panel's own PAM stack (`packaging/pam/aetherrv-panel`, commit 89c2e56).
  Everything the panel needs works in such a session, checked as the kiosk user
  (`harness/uibench/light-session.sh`): its own `XDG_RUNTIME_DIR`, the seat with its input devices and the DRM
  device, the page live, a tap, screen off/on through the page, the alert chime on ALSA (with kiosk's device ACL),
  and restarts after `systemctl restart` and after a `kill -9` of the runtime, with no process or session left from
  an earlier one: 25 of 25 checks for the WPE launcher, Chromium and Tauri each. WebKit starts its sandbox without
  `xdg-dbus-proxy` when there is no session bus to proxy, and works.
- **What holds `network.target` on the bench is the bench's own firewall**: `nftables.service` (ordered before
  `network-pre.target`) takes 1.4 s to load its ruleset (3.0 → 4.4 s), then `systemd-networkd` (4.4 → 4.5 s).
  `systemd-analyze critical-chain network.target` shows `network.target @3.730s ← systemd-networkd.service @3.647s
  +81ms ← network-pre.target @3.645s` (times since userspace started); it leaves out nftables because that oneshot
  is inactive afterwards. Nothing in the repo installs a firewall, and the van doesn't run nftables, so this part of
  the wait is the bench's. On the van, `network.target` and `systemd-user-sessions` come at about 3.6 s.
- **The firmware is the largest part (8.2–8.9 s; the one boot of the 10 over 15.0 s had 8.92 s), and the owner's
  lever.** `BOOT_UART=1` makes the bootloader write its log to the UART at 115200 baud; at about 11.5 KB/s, a 2–4 KB
  bootloader log costs 0.2–0.35 s if the writes block. That is an estimate: the UART is not captured here and the
  bootloader's log is not visible from Linux. The van's bootloader configuration could not be read.
- **From the panel unit's start to a usable Home**, itemised on one boot (old runner): the unit's main process (sway)
  at 5.02 s; sway runs its exec line at 5.43 s (sway's own start: 0.4–0.6 s); the runner starts at 5.44 s; its
  recorder and helper are up after 24 ms, its baseline frame after 246 ms, and after a 200 ms pause it launches the
  runtime at 5.89 s (+0.45 s); the first frame with data at 6.96 s; usable at 7.03 s. With the runtime launched
  first, the launch comes 10–20 ms after sway's exec line, and usable about 1.0 s after the launch. From the launch
  to a usable Home: the WPE launcher 1.0 s, Chromium 1.8 s, the Python UI under Chromium 1.8 s.

The van's own figure is a van check (§10): its kernel phase is longer (1.65 s against 0.76 s on the bench), and it
has no firewall to wait for.

## 9. Recommendation

**WPE WebKit through the panel's own kiosk launcher** (`panel/wpe`, below), fullscreen on sway as today, in the
user-light session of the committed panel unit (`packaging/pam/aetherrv-panel`, §8). The final pass (§5.1) measured
that build; its power-on figures come from a stand-in ordered like the panel unit (§8). For the alert chime the van
needs `gst-plugins-base` and `gst-plugins-good` (§9, below; CUTOVER §3).

**Owner's choice (Q-U8, 2026-11-24): the WPE launcher.** `packaging/panel/launch.sh` uses it by default.

- **It is the only runtime that meets the tap, switch, frame-rate and memory numbers together.** In the final pass
  (§5.1, the battery bank on the page): taps 35 / 47 ms p50 / p95 over 436 presses, every one under 100 ms (the
  owner's measure, Q-U12); warm switches 139 ms p95 over all pages; the page's rAF at refresh on Home, Climate and
  Switches; the bank at 62 fps, the Network sky at 60 and the Weather loop at ≥ 32 (§5.1a); 392 MB PSS after every
  page (381–421 MB under heavy use in other runs, accepted: §4.6). Power-on is not met on every boot: median
  14.74 s, max 15.16 s, 1 of 10 boots over 15.0 s, about 2.2 s faster than the Python system on the same procedure;
  the Pi's firmware is 8.2–8.9 s of it (§8). The owner accepted it as measured (Q-U10).
- **It is the lightest.** It uses 22 % of a core on Home and at most 13 % on the static pages (Tauri 37 %, Chromium
  34 %, the Python UI 81 % on Home). It has the fastest cold start: 0.72 s from launch to a usable Home (0.68 s through
  MiniBrowser), against 0.90 s for Tauri, 1.58 s for Chromium and 1.70 s for the Python UI.
- **Its numbers should hold best on the real panel.** One capture patch does not disturb it, and it had the least
  GPU work pending behind each frame (§3; an inference until scanout is measured).
- **What it costs.** A kept radar loop would hold about 800 MB of GPU buffers (§4.1); the owner chose to release it
  on hide (Q-U9), which puts the Weather switch at 128 / 141 ms. It needs its own small
  kiosk launcher (below) and two GStreamer packages for the alert chime (gst-plugins-base and gst-plugins-good).
  After a crash of its web process the panel takes about 11 s to come back, most of it systemd-coredump writing the
  core, which is kept on purpose as the crash's evidence.

**Tauri** runs the same engine (WebKitGTK). In the five-runtime pass (§5.2) it used 457 MB PSS against WPE's 358 MB
(over the limit), more CPU on Home, slower presses (Home 56 / 63, Network 62 / 72 ms) and a slower start (0.90 s).
It also adds a native app to build and maintain, and nothing in the UI needs one: the page only talks to the gateway
over HTTP and the WebSocket.

**Chromium**, the van's runtime today, shows a press fastest on light pages (13–30 ms). It misses the switch number
(Energy 383 / 438 ms, Alerts 266, Weather 225 p95) and memory (586 MB with the scenes kept), and its Weather frame
rate was not shown to reach 30 fps (lower bound 29). It also had the most GPU work pending behind its frames
(57–198 ms median during switches), which suggests that its real-panel times are later than measured. Disposing the
scenes on hide (`?keepgl=0`) brings its memory back to 410 MB, but its Network and Weather switches back to 480–770
ms. In this benchmark it did not meet the memory and switch numbers at once, though the bands do stall it by 100–300
ms at times (§3).

**Still open, in the van:** the figures on the real panel, where scanout adds up to a refresh to every tap and switch
(§10, `docs/CUTOVER.md` §8), and the panel's memory over days.

### The WPE kiosk launcher

The owner's answer (Q-U1) was to benchmark WPE through MiniBrowser and, if WPE wins, ship a small WPE Platform
launcher. That is `panel/wpe/aetherrv-panel-wpe.c` (about 180 lines of C against `wpe-webkit-2.0`), built on the Pi
by `packaging/install.sh` and started by `packaging/panel/launch.sh` for the `wpe` runtime (MiniBrowser is the
fallback). It shows one full-screen web view on sway with the page colour as its background, and no browser chrome or
context menu. It refuses new windows and any navigation off the UI's origin, keeps a persistent profile (the UI keeps
its per-screen preferences in localStorage), and allows sound without a first touch, for the alert chime. It exits
non-zero when its web process dies (2), hangs (3) or its compositor goes away (4), so the panel unit restarts the
session.

Checked on the bench with `harness/uibench/launcher-check.sh` (6 of 6, on the launcher before its exit-4 change;
the script was not rerun after it):

- a test page's navigation and `window.open` to another origin are refused, no request reaches that origin, and the
  page stays;
- the window fills the output (1920×1200, fullscreen);
- SIGKILL on the web process: exit 2 after 0.3 s;
- SIGABRT on the web process: exit 2 after 11.3 s, most of it systemd-coredump writing a 230 MB core;
- SIGTERM: exit 0.

And by hand:

- A hang (SIGSTOP on the web process) is only noticed when WebKit has input to deliver. With no touch the launcher
  keeps running; after a tap it exits 3 once the hang timeout passes (`AETHERRV_PANEL_HANG_S`, default 30 s).
- `kill -SEGV` is no crash test for WebKit: its JavaScript engine handles SIGSEGV and the process carries on.

When sway goes away (a restart of the panel unit), the launcher exits with 4. An earlier version did not, and left a
launcher and its WebKit processes behind in the old session for every restart. The panel unit also kills any stray
launcher before it starts (`pkill -u kiosk -f ^/opt/aetherrv-rs/bin/aetherrv-panel-wpe`). With the exit-4 launcher,
stopping sway made it exit 4 at once, leaving no WebKit process, and three restarts of a
panel-like session then left exactly one runtime and one session each time, for the launcher, Chromium and Tauri.

**MiniBrowser**, by contrast, keeps running when its web process is killed: the page stays blank and nothing restarts
it. It also has no guard against navigating off the UI.

**The launcher against MiniBrowser** (the same session and bundle; Home, Climate, Switches and Network; 3 warm rounds,
12 presses per page). *The simulators were down*, so Home had no energy flow, Network no sky and Switches no tiles:
the pages were light, the same for both. The full figures come from the final pass (§5.1).

| | MiniBrowser | launcher |
|---|---|---|
| cold start: first paint / usable | 600 / 671 ms | 586 / 651 ms |
| tap → pressed, all pages, p50 / p95 | 33 / 39 ms | 35 / 38 ms |
| warm switch, p50 / p95 | 68 / 85 ms | 68 / 85 ms |
| PSS after boot / after every page | 231 / 257 MB | 230 / 261 MB |

**Sound.** WebKit's WebAudio plays through GStreamer's `autoaudiosink` (gst-plugins-good), which picks `alsasink`
(gst-plugins-base) when no sound server runs, as on the van. Both packages are needed: with only -base, WebKit logs
"autoaudiosink not found" and the AudioContext stays suspended; with only -good, the pipeline can't be built. The
`wpewebkit` package lists gst-plugins-good as optional and gst-plugins-base not at all. In a user-light session as the
kiosk user, with snd-aloop's loopback as the default card, the UI's chime reached ALSA from the launcher, Chromium and
Tauri alike: peak −12.1 dBFS, the AudioContext running (`harness/uibench/light-session.sh`,
`harness/uibench/audio-gst-check.sh`).

## 10. What the bench cannot show (van checks)

- **The panel itself**: scanout (up to one refresh after the commit times measured here, plus whatever GPU work is
  still pending, §3), the Waveshare panel's touch controller and its response time, and real touch feel. The injected
  touchscreen starts at the kernel's input layer.
- **Power-on on the van**: the van's firmware and boot order with the CAN HATs, its own NVMe and the real panel.
  The bench's figure (§8) has no Plymouth and no display to bring up.
- Hold-to-dim on a real dimmer; screen power control and wake-on-touch on the van's panel. Headless only proves
  that the runtime survives the output going off and on.
- Plymouth: not installed on the bench, so the splash → sway → runtime hand-off is reasoned from the units and the
  measured first frames (§7), not watched.
- Long-run memory: each figure here is after one pass (about eight minutes). The van's 1.15 GB for the Python UI is
  after days of uptime. A soak of the chosen runtime is the way to see growth.

These go into `docs/CUTOVER.md` as checks on the van.

## 11. Reproducing

```sh
# the final pass (§5): the bundle a deploy put on the bench's gateway, the WPE launcher and the Python UI baseline
UIBENCH_HOST=<bench> UIBENCH_KEEP_BUNDLE=1 \
  UIBENCH_ARGS="--warm-rounds 3 --press-reps 48 --press-reps-for network=100 --null-reps 3 --fps-secs 5" \
  harness/uibench/full.sh 1920x1200 python wpe-launcher
UIBENCH_HOST=<bench> harness/uibench/full.sh               # all five runtimes, this tree's bundle installed first
UIBENCH_HOST=<bench> harness/uibench/poweron.sh rust 10     # reboots the bench 10 times: power-on to usable (§8)
UIBENCH_HOST=<bench> harness/uibench/poweron.sh python 2    # the Python baseline: rollback, 2 reboots, cutover back
```

`full.sh` runs `prepare.sh` (it syncs the harness, builds the tool, the Tauri app and the WPE launcher natively on
the Pi and, unless `UIBENCH_KEEP_BUNDLE=1`, builds this tree's bundle and installs it on the gateway), then `run.sh`
for the runtimes named, and `perturb.sh` for each browser runtime, as a transient unit on the bench (`uibench-full`),
and copies the results to `harness/out/uibench/` (gitignored). `poweron.sh` repeats `boot-time.sh` (one reboot;
`boot-measure.sh arm|report|disarm` is its bench side) and summarises every boot's total, the median, the max and the
boots over 15.0 s. Everything refuses to run on a host with `hat0`/`hat1` (the van).

On the bench: `run.sh <runtime|all> headless [WxH]` runs one runtime (`wpe` is MiniBrowser, `wpe-launcher` the
panel's launcher). `perturb.sh [--runtime …] [page…]` is the cost check of §3 and `calibrate.sh <0|30|60>` the
calibration of §2. `presscost.sh [--runtime …] [--press variant] [page…]` measures in the page what the pressed state
costs (§4.3). `screen-check.sh --runtime …` checks screen off/on from the page (§7). `launcher-check.sh` checks the
WPE launcher (§9). `light-session.sh install|check <runtime>|remove` checks a user-light session as the kiosk user,
chime included (§8), and `audio-gst-check.sh` shows which GStreamer packages WebAudio needs (§9). `serve_bundle.py`
serves a bundle without installing it (`UIBENCH_URL`, `UIBENCH_BUNDLE_DIR`, `UIBENCH_BASE` point the scripts at it).
`aetherrv-uibench frames` (with `--cmd` for the bench module), `globals` and `touch-test` are the tool's own
diagnostics. URL switches for experiments: `?keepgl=0` or `?keepgl=sky,radar,bank`, `?sky=2d`, `?skyfps=N`, and
`&press=promote|nofilter|noscale|bg|rest|restfx|tint` (bench module only, §4.3).

**How the earlier figures were run.** The five-runtime pass (§5.2), the memory runs (§4.1), the per-scene runs and
the first press experiments were driven by a scratch script outside the repo, which called `run.sh` and `perturb.sh`
with the overrides above against `serve_bundle.py`. So were the 2026-11-24 press-variant runs, the adoption run and
the memory A/B for the switch-tile fix, the scoped run (§4.3) and the launcher-against-MiniBrowser comparison (§9).
`presscost.sh`, `screen-check.sh`, `launcher-check.sh`, `light-session.sh`, `audio-gst-check.sh`, `boot-time.sh`
(the power-on runs) and the final pass (`full.sh`, `poweron.sh`) ran as committed. `calibrate.sh` has not run as
written (the calibration used the same commands), and the 2560×1440 size was not run.
