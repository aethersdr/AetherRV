# UI runtime benchmark

Which runtime should show the React UI on the van's touchscreen: (a) WPE WebKit, (b) a Tauri v2 app (WebKitGTK), or
(c) Chromium (prompt §7)? This page has the method, the UI changes the measurements led to, the final numbers, how
they stand against prompt §8, and a recommendation for the owner to confirm.

**The numbers are headless, by owner decision (2026-11-23).** Every figure here comes from sway's headless output at
**1920×1200 @ 60 Hz, the van panel's own resolution**, rendered on the bench Pi 5's GPU (V3D) by the compositor,
config and runtimes the van uses. No monitor was used. With no scanout, no figure includes the panel's display
latency. Frame times are the compositor's commit times (§2). The power-on time was measured by rebooting the bench
into a headless kiosk session (§8).

**In short:** WPE WebKit meets the §8 numbers for page switches, frame rate and memory. It misses the tap budget by
about 6 ms on one control. Power-on on the bench is 15.6–15.9 s with every runtime (upper bounds), at the 15 s limit
and not shown to meet it; most of it is the Pi's firmware. Tauri uses the same engine with more memory and CPU.
Chromium shows a press fastest on light pages, but it misses the switch and memory numbers, and its Weather frame
rate was not shown to reach 30 fps. Recommendation: **WPE** (§9).

## 1. Setup

| | |
|---|---|
| Machine | bench Pi 5, 16 GB, NVMe, Arch Linux ARM, kernel 7.0.3-v8+, Mesa 26.3.1 (V3D 7.1), no CAN HAT |
| Compositor | sway 1.12 (wlroots 0.20.2), the van kiosk's config (`deploy/kiosk/sway.conf`) for every runtime: `harness/uibench/sway.conf.in` |
| Session | a transient unit with a PAM session on VT 8 (`harness/uibench/session.sh`), like `aetherrv-panel.service` on tty1: logind gives sway its seat (libinput for the injected touchscreen) |
| Output | sway's headless output (`WLR_BACKENDS=headless,libinput`), 1920×1200 @ 60 Hz |
| Backend | the whole Rust system under systemd against simulators (`harness/bench/bench-up.sh`), gateway on 127.0.0.1:8080 |
| UI | the production bundle (`vite build`, hash `0bb37c08214f`, in every result). The gateway's installed bundle (`/opt/aetherrv-rs/ui`) was left as deployed: `harness/uibench/serve_bundle.py` served the final bundle from the bench user's directory on 127.0.0.1:18082 and passed `/api` and `/ws` through to the gateway. The runtimes opened `http://127.0.0.1:18082/?bench=1`. The source tree differs from that bundle only by the per-scene `?keepgl=` list, added for one attribution run (bundle `9c4426ea0987`, §4.1); the default behaviour is the same. The power-on runs (§8) used the gateway's installed bundle |
| Fonts | `noto-fonts` installed on the bench for this. The van has it; without it the UI fell back to FreeSans, which has a different layout and raster cost |
| (a) WPE | wpewebkit 2.54.3, `MiniBrowser --fullscreen` with `WPE_PLATFORM=wayland` (WPE Platform API) |
| (b) Tauri | Tauri 2.12 (`panel/tauri/`, its own Cargo workspace), webkit2gtk-4.1 2.54.3, built natively on the Pi |
| (c) Chromium | chromium 158, the van's flags (`deploy/kiosk/chromium.sh`): `--kiosk --ozone-platform=wayland`, GPU on |
| Before | the Python UI (`web/`) served by the Python backend's own web app with **no device controls** (`harness/uibench/serve_ref.py`, from `harness/ui-reference/serve_snapshot.py`), in Chromium kiosk as on the van. *python* gets the gateway's live (simulated) state every second; *python-static* gets one frozen snapshot |

**Deviation from the prompt: no `cog`.** The prompt names `cog` for candidate (a). Cog is the launcher for WPE's
legacy libwpe API, which Arch Linux ARM doesn't package: its `wpewebkit` 2.54 is built for the new WPE Platform API
only, with no libwpe and no wpebackend-fdo. Candidate (a) therefore runs WPE WebKit through its own `MiniBrowser`
on the WPE Platform Wayland backend, fullscreen. The engine, its processes and its rendering path are the ones a cog
kiosk would use on a current WPE. On the van it would be a few-line WPEPlatform launcher, or MiniBrowser itself.

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
   - **Press latency**: 12 trials plus 3 no-touch control trials. Each trial touches down on a control, holds
     300 ms, slides off and lifts. The latency runs to the first frame whose pixels inside the control differ from
     a reference taken just before. That is the pressed state that §8's *tap to visible response* is about. The page
     also reports when its `pointerdown` handler ran, which splits the latency into input delivery and rendering.
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

**What is pressed, and what never is.** No press ever becomes a click: the touch slides off before lifting, and
the slide-off check proves this per runtime. The targets are UI-local controls only: chart ranges, segmented
toggles, tabs, map zoom, the Home demo tiles, or a switch tile that is not a dimmer, pressed without a click (a
tile's pointerdown handler acts only for dimmers, and the bench module skips those). A heater control is never
pressed. Neither is anything that acts on pointerdown: dials, sliders, drag grips, dimmers, or any control inside an
element with a React pointer handler. A page with no such control (Systems) gets no press measurement. The only
real taps are on rail buttons (navigation). The device services' journals for the whole pass, reboots included
(2026-11-23 23:21 to 00:57 bench time), show no command at all. No line matches "command … from". The only "TX
allowed" lines are the n2k service's own address claim (PGN 60928), sent once or twice as it starts after each
reboot. The same search finds the soak's commands earlier the same evening (282 lines from 21:00 to 23:15).

**Page cross-checks.** The opt-in module `ui/src/bench/` (loaded only with `?bench=1`, in its own chunk) and
`harness/uibench/page/ref-bench.js` (injected into the Python UI by `serve_ref.py`) report geometry (where the rail,
the view and the press targets are), the boot marks, the pointerdown handler times, the rAF samples, Event Timing
and long tasks where the engine has them, and the WebGL renderer. No time in the results comes from the page except
the "data arrived" mark, which picks the cold-start frame, and the rAF samples.

## 3. The instrument's own cost

`perturb.sh` loads Home and Network in each runtime and reads the page's own rAF intervals three times: with
nothing captured, with one 320×200 patch (the press and scene measurements), and with the four bands (content fps
and page switches). Final bundle, headless 1920×1200:

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
  That shows in its Home press figures, which are partly the instrument's (§5).
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
*after* is the final pass. Both are headless 1920×1200 with the same pages, repetitions and runtimes. Where a
like-for-like comparison inside the final bundle exists, it is given too.

### 4.1 Page switches to the WebGL pages (Network, Weather, Energy)

**What the Network delay was made of.** Every hide disposed the page's WebGL scene (renderer, context, shaders,
textures), and every show rebuilt it. Warm switches to Network from Switches (not from Settings), p50 / p95:

| UI | sky | WPE | Chromium |
|---|---|---|---|
| before (dispose on hide) | 3D | 333 / 374 ms | 380 / 471 ms |
| before | 2D map (`?sky=2d`, no WebGL) | 67 / 120 ms | 78 / 91 ms |
| after (scene kept, paused), final pass | 3D | 108 / 164 ms (without the sky: 100 / 108) | 139 / 153 ms (without the sky: 90 / 105) |

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

**Before / after, all three runtimes** (warm, p50 / p95 ms; before = first pass, after = final pass):

| page | WPE | Tauri | Chromium |
|---|---|---|---|
| Network | 399 / 410 → **108 / 164** | 413 / 445 → **105 / 106** | 440 / 483 → **139 / 153** |
| Weather | 153 / 165 → **106 / 117** | 202 / 211 → **106 / 118** | 261 / 828 → **178 / 225** |
| Energy | 89 / 90 → 83 / 83 | 131 / 149 → 98 / 136 | 275 / 367 → 383 / 438 |

The same comparison inside the final bundle (one warm round each, `?keepgl=0` vs the default, from the memory
runs below): Network WPE 401–431 → 106–156 ms, Tauri 449 → 156, Chromium 478 → 188; Weather WPE 140–144 → 114–118,
Tauri 144 → 114, Chromium 770 → 245. **Energy is unchanged by keeping its scene** (WPE 83–86 vs 78–82, Tauri 127 vs
134, Chromium 207 vs 175). The battery bank's rebuild is cheap. Chromium's slow Energy switch (210–440 ms, the same
with the bank masked out) comes from the rest of the page: the GPU queue during those switches stood at a median
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
| sky and bank | 366 / 540 | 108 / 150 / 67 | 403 / 649 | 188 / 816 / 172 |
| all three (default) | 352–362 / 1359–1363 | 106–156 / 114–118 / 78–82 | 597 / 789 | 188 / 245 / 175 |

In WPE, **the kept radar holds about 800 MB of the GPU buffers** (presumably its loop's frame textures; not broken
down further) and saves only 25–35 ms
on the Weather switch, which is 140–150 ms without it. That is under the 150 ms limit, but only just. The sky and
the bank together hold about 5 MB, and the sky saves about 300 ms on every switch to Network. Chromium's figures
don't add up per scene in a single round: its extra ~190 MB appears only with all three kept. The default stays
"all three kept", as measured in the final pass. Releasing the radar's frames on hide is the owner's choice: about
800 MB of GPU memory against a Weather switch at the limit. A middle way for later is to keep the radar's context
and shaders, drop its frame textures on hide, and reload them on show.

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
in the same session (`harness/out/uibench/r3/before-home/`); *after* is the final pass:

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
so input delivery is not the delay. The rest is the runtime presenting the frame. In WPE on Switches, the first frame
WebKit presents after the touch is already the pressed one, about 52 ms later. On Climate the same happens after
about 31 ms. So WebKit takes two to three refreshes to present any change after an input on an idle page. The Switches
target is the first switch tile on the board (`.sw[data-sw]`, switch `32.2`). The runner measured it at 331×27 px:
with the bench's simulated switch list, the board lays its tiles out much flatter than the free-standing tile's
214 px. That layout may deserve a look of its own.

**Before / after** (all pages, p50 / p95 ms): WPE 36.5 / 54.0 → 35.4 / 54.5; Tauri 41.7 / 69.8 → 38.8 / 64.1;
Chromium 26.9 / 95.5 (2 of 48 presses not seen) → 21.8 / 96.9 (none of 96 missed). Per page, see §5. Chromium's
Home (97 / 143) and Network (63 / 77) figures are partly the instrument's: for 8 and 7 of their 12 presses, the
compositor presented a frame that the patch stream skipped before the detection. Those are upper bounds. On its
other pages Chromium shows the press in 13–30 ms.

**Two experiments** (WPE and Tauri, 15 presses per page, the final bundle; URL switches in the bench module only):

| page | runtime | as is | every target on its own layer (`&press=promote`) | no brightness filter (`&press=nofilter`) |
|---|---|---|---|---|
| Home | WPE | 45 / 47 | 45 / 47 | 46 / 47 |
| Home | Tauri | 59 / 72 | 63 / 70 | 64 / 70 |
| Climate | WPE | 31 / 35 | 30 / 35 | 31 / 34 |
| Climate | Tauri | 34 / 37 | 32 / 34 | 33 / 42 |
| Switches | WPE | 52 / 54 | 60 / 64 | 53 / 55 |
| Switches | Tauri | 55 / 58 | 62 / 66 | 51 / 55 |

Neither helps. Pre-promoting every target makes Switches slower, and the filter costs nothing measurable. The
pressed style is not what costs; WebKit's frame pipeline is. **No change was adopted for this item, and it still
fails** on every runtime: WPE on one control (the switch tile, p95 56 ms), Tauri on three, and Chromium on Home and
Network. On the real panel, scanout adds up to one refresh to every figure. Next steps for a later pass: find why
the switch tile needs a third frame in WebKit (its page, not its style), and confirm Chromium's Home figure with
nothing but the press patch on screen.

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

## 5. Final results (headless 1920×1200)

Final bundle `0bb37c08214f`, 2026-11-23 23:22 – 00:01 bench time, all five in one pass, then `perturb.sh` per
runtime (§3). Results: `harness/out/uibench/r3/final/` (gitignored), summary from `harness/uibench/summarize.py`.

| runtime | tap → pressed p50 / p95 ms | switch → content p50 / p95 ms (warm, all pages) | first visit, worst ms | page rAF fps, nothing captured: Home / Climate / Switches | scene fps: Network / Weather | cold start → usable ms | PSS after boot / after every page MB | processes |
|---|---|---|---|---|---|---|---|---|
| WPE | 35 / 55 | 73 / 118 | 678 (Weather) | 62 / 62 / 62 | 61 / 32 | 684 | 242 / 358 | 8 |
| Tauri | 39 / 64 | 90 / 125 | 730 (Weather) | 59 / 62 / 62 | 56 / 39 | 903 | 287 / 457 | 3 |
| Chromium | 22 / 97 | 114 / 384 | 1479 (Weather) | 62 / 60 / 60 | 60 / 29 | 1584 | 326 / 586 | 11 |
| Python UI, live | 178 / 232 (73 of 108 not seen) | 296 / 406 | 2314 (Network) | 62 / 60 / 60 | 30 / – | 1662 | 316 / 642 | 11 |
| Python UI, static | 180 / 227 (73 of 108 not seen) | 296 / 362 | 2340 (Network) | 62 / 60 / 60 | 29 / – | 1591 | 313 / 621 | 11 |

The Energy page has no idle animation: its battery bank moves only when touched, so it has no scene fps. Every
runtime renders on the V3D: the render node is open in the GPU-rendering process, with no llvmpipe threads anywhere.
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
panel adds up to one refresh to the tap and switch figures (§2).

| §8 number | WPE | Tauri | Chromium |
|---|---|---|---|
| tap → visible response ≤ 50 ms (p95) | **miss**: 55 ms overall. The switch tile on Switches 52 / 56 ms; every other page passes (Home 46 / 47) | **miss**: 64 ms. Home 56 / 63, Network 62 / 72, Switches 54 / 58 | **miss**: 97 ms. Home 97 / 143 (partly the instrument, §3), Network 63 / 77; the other six pages pass at 13–30 ms p50 |
| page switch ≤ 150 ms (warm p95) | **pass**: 118 ms. Worst page Network, p50 108 ms, one of three switches at 164 ms | **pass**: 125 ms; every page ≤ 136 | **miss**: 384 ms. Energy 383 / 438, Alerts 144 / 266, Weather 178 / 225 |
| 60 fps Home / Climate / Switches | **pass**: page rAF at refresh; at most one frame over 33 ms in a 5 s sample (Switches) | **pass**: Home 59 fps, none over 33 ms; one on Switches | **pass**: none over 33 ms |
| ≥ 30 fps Energy / Network / Weather scenes | **pass**: Network 61, Weather ≥ 32; Energy has no idle animation | **pass**: 56, ≥ 39 | Network 60; **Weather not shown**: the lower bound is 29 fps, so it may or may not reach 30 |
| memory ≤ 400 MB (PSS) | **pass**: 358 MB (plus ~825 MB of GPU buffers, almost all for the kept radar loop, which PSS doesn't count; §4.1) | **miss**: 457 MB | **miss**: 586 MB (410 with the scenes disposed) |
| power-on ≤ 15 s (bench reboot, §8) | **at the limit, not shown to meet**: 15.6 s to the settled Home, 15.2 s to its first frame with data (both upper bounds; 21.3 s on the first reboot) | **at the limit, not shown**: 15.6 s | **at the limit, not shown**: 15.9 s (15.6 s to the first frame with data) |

Not measurable headless: the panel's scanout and response, and real touch feel (§10).

## 7. Screen power and the boot splash

**Screen power.** Today sway turns the panel off and on (`swaymsg output <name> power off|on`, through the screen
helper, `services/screen` in the Rust system). The runtime never sees it: sway stops sending it frame callbacks
while the output is off, and the page keeps running (WebSocket, timers). Each run turned the output off for 6 s and
back on as the helper does (by name, repeated until the connector reports on). It then checked that a page switch
still completes and that the page reports its WebSocket live. All five passed (§5). On the headless output this
proves the runtime side only. The Pi 5's HDMI behaviour (the first power-on after an off is ignored, which the helper
works around) and the van panel's backlight need the real screen. Not exercised: the page's own screen-off button,
which calls the helper over HTTP on 127.0.0.1:8091 (a cross-origin `fetch` from the page). The helper was not running
in the benchmark session, so the page did not offer the button. Whether WPE's and WebKitGTK's fetch to it works as
Chromium's does is still to check.

**Boot splash hand-off.** As on the Python kiosk, `aetherrv-panel.service` orders itself after Plymouth and runs
`plymouth quit --retain-splash` before starting sway. The splash's last frame stays in the display's framebuffer
until sway's first frame replaces it. That frame is sway's own background (`output * bg #000000 solid_color`), and
the runtime's first paint follows. So for every runtime the sequence is splash → black → UI. The black gap is the
runtime's launch → first paint (§5: WPE 0.64 s, Tauri 0.82 s, Chromium 1.14 s), plus sway's own start (§8). None of
the three runtimes needs anything runtime-specific for the hand-off, since sway owns the display:

- no runtime shows a light frame before the UI (§5). Tauri did until its window and webview background were set to
  the page colour (§4.5);
- WPE's MiniBrowser defaults to a white view background (`--bg-color`). It did not show here because the page
  painted first, but a WPE launcher should set the page colour as well, so that a slow first paint can never flash;
- to keep the splash on screen until the UI paints (no black gap at all), give sway the splash as its background
  (`output * bg /usr/share/plymouth/themes/aetherrv/splash.png center #000000`, which needs the `swaybg` package).
  This is independent of the runtime.

Plymouth is not installed on the bench, so this is reasoned from the units and the measured first frames, not
watched. The shutdown splash's ordering (`aetherrv-plymouth-after-panel.conf`) is unaffected by the runtime.

## 8. Power-on to usable UI (§8: ≤ 15 s)

**Measured by rebooting the bench** four times (2026-11-24 00:53–00:57 bench time; `harness/uibench/boot-time.sh`
from the workstation, `boot-measure.sh` on the bench). Before each reboot, `boot-measure.sh arm <runtime>` enables
a one-shot unit, `uibench-boot.service`. It is ordered like `aetherrv-panel.service`: after the gateway, waiting for
it on :8080. It starts sway with the kiosk config on a headless 1920×1200 output, and sway's one exec line starts
the runtime under `aetherrv-uibench`. The runner records the runtime's launch and the first frame of the settled
Home page on CLOCK_MONOTONIC, so every figure is time since the kernel started. Then:

- **the firmware part** (the Pi 5 bootloader, which Linux cannot see) runs from the previous boot's last journal
  entry (`Journal stopped`, late in the shutdown) to the new kernel's start (the bench's wall clock minus its
  uptime). Both are on the bench's own clock. It is an upper bound: the last steps of the shutdown (unmounting, the
  reboot call) run after the journal stops. For these four runs it was read by hand after each boot
  (`journalctl -b -1 -o short-unix -n 1`). `boot-time.sh` now reads it itself, but that addition has not run yet;
- **from the workstation**, the script pings the bench's wired address every ~0.1 s while it answers and every
  ~0.15 s while it doesn't. It then polls the gateway on that address every ~0.05 s. The bench's figures are placed
  on the workstation's clock with an offset taken over ssh (best of five round trips, 0.12 s). This cross-checks
  the split: in every run the kernel started between the last ping reply before the reboot and the first after it.
  The last ping reply came 0.1 s before the journal stopped.

| run | firmware (journal stopped → kernel) | kernel → bus started | → gateway serving | → panel unit started | → runtime launched | → first frame with data | → **usable** (settled Home) | **firmware + kernel → usable** |
|---|---|---|---|---|---|---|---|---|
| WPE, 1st | 11.14 s | 3.64 | 5.10 | 5.37 | 6.22 | 7.35 | **10.17** | **21.3 s** |
| Tauri | 8.54 s | 3.23 | 4.70 | 4.97 | 5.80 | – | **7.07** | **15.6 s** |
| Chromium | 8.22 s | 2.89 | 4.72 | 4.97 | 5.81 | 7.42 | **7.65** | **15.9 s** |
| WPE, 2nd | 8.57 s | 3.06 | 4.86 | 5.11 | 5.95 | 6.60 | **7.06** | **15.6 s** |

(Times after the kernel started, in s. Tauri's page reported its data mark too late to place.)

- **15.6–15.9 s from the end of the shutdown to a usable UI with any runtime** (15.2 s to WPE's first frame with
  data). These are upper bounds at the 15 s limit, so the bench does not show the budget met. The
  firmware is the largest part: 8.2–8.6 s, and 11.1 s on the first reboot after 13 h of uptime (cause not
  established). The bootloader is the Pi 5's (2025-12-08, `BOOT_ORDER=0xf416`, NVMe first, `BOOT_UART=1`).
- Linux takes about 5 s to the panel unit. The kernel takes 0.75–1.1 s, and userspace reaches the default target
  at 5.0–5.4 s; the gateway serves at 4.7–5.1 s and the panel unit follows it.
- **The runtime's part is 1.1–1.8 s** from its launch to a usable Home: WPE 1.11 s on its second boot (0.65 s to
  its first frame with data), Tauri 1.27 s, Chromium 1.84 s. From the panel unit's start it is 2.0–2.7 s, which
  includes sway's start (~0.3 s) and the runner's capture set-up. On WPE's first boot the Home page settled 2.8 s
  after its first frame with data (cause not established).
- Seen from the workstation, the bench stopped answering 0.7–0.8 s after the reboot command. It answered ping
  again 17.3–17.5 s later (20.1 s the first time), and its gateway answered on the LAN 0.04–0.07 s after that. The
  wired network came up 8.6–8.8 s after the kernel started, later than the UI. So "the gateway answers" from
  outside is not the UI's critical path.

**Caveats.** The bench booted without its simulators: they are transient units, started by
`harness/bench/bench-up.sh`, and do not come back after a reboot. So the services started with no devices. There
was no Plymouth and no display: the headless output has no modeset, and the panel unit's `plymouth quit` was not
there. The runtime loaded the gateway's installed bundle (`181a8efb6420`), not the final pass's. A cold power-on
skips the shutdown but adds the power supply's ramp and the PMIC's start, which a reboot doesn't show.

**What would bring it under 15 s:** the firmware is the lever (the bootloader's configuration on the van: no UART
logging, no other boot devices tried). Next are the services the panel unit waits for: it starts only once the
gateway serves, at about 5 s. The runtime choice moves the total by about 0.6 s, with WPE fastest. The van's own
figure is a van check (§10).

## 9. Recommendation

**WPE WebKit.** Run it fullscreen on sway as today, through MiniBrowser or a few-line WPEPlatform launcher in place
of cog (§1).

- **It is the only runtime that meets the switch, frame-rate and memory numbers together.** Warm switches are
  118 ms p95 over all pages, with the page's rAF at refresh on Home, Climate and Switches. The Network sky runs at
  61 fps and the Weather loop at ≥ 32. It uses 358 MB PSS after every page, and reaches a usable UI 7.1 s after the
  kernel starts. Its misses are the tap budget, by about 6 ms on one control (a switch tile, p95 56 ms; §4.3),
  and power-on is at the limit: 15.6 s on the bench (15.2 s to the first frame with data), with every runtime
  within 0.3 s of that and most of it the Pi's firmware (§8).
- **It is the lightest.** It uses 22 % of a core on Home and at most 9 % on the static pages (Tauri 37 %, Chromium
  34 %, the Python UI 81 % on Home). It has the fastest cold start: 0.68 s from launch to a usable Home, against
  0.90 s for Tauri and 1.58 s for Chromium.
- **Its numbers should hold best on the real panel.** One capture patch does not disturb it, and it had the least
  GPU work pending behind each frame (§3; an inference until scanout is measured).
- **What it costs.** The kept radar loop holds about 800 MB of GPU buffers (§4.1), outside the PSS figure.
  Releasing it on hide puts the Weather switch at 140–150 ms; that choice is the owner's. MiniBrowser is not a
  kiosk shell, so the launcher must set the view background to the page colour, and the panel unit must restart it
  if it crashes. Still to check in WebKit: the page's cross-origin call to the screen helper (§7).

**Tauri** runs the same engine (WebKitGTK) with about 100 MB more (457 MB, over the limit), more CPU on Home, slower
presses (Home 56 / 63, Network 62 / 72 ms) and a slower start (0.90 s). It also adds a native app to build and
maintain, and nothing in the UI needs one: the page only talks to the gateway over HTTP and the WebSocket.

**Chromium**, the van's runtime today, shows a press fastest on light pages (13–30 ms). It misses the switch number
(Energy 383 / 438 ms, Alerts 266, Weather 225 p95) and memory (586 MB with the scenes kept), and its Weather frame
rate was not shown to reach 30 fps (lower bound 29). It also had the most GPU work pending behind its frames
(57–198 ms median during switches), which suggests that its real-panel times are later than measured. Disposing the
scenes on hide (`?keepgl=0`) brings its memory back to 410 MB, but its Network and Weather switches back to 480–770
ms. In this benchmark it did not meet the memory and switch numbers at once, though the bands do stall it by 100–300
ms at times (§3).

**Open whichever runtime is chosen:** the tap budget (§4.3, the next UI task), the van checks (§10), and a soak of
the chosen runtime for memory growth.

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
UIBENCH_HOST=<bench> harness/uibench/full.sh               # all five at 1920x1200, plus perturb.sh per runtime
UIBENCH_HOST=<bench> harness/uibench/full.sh 2560x1440     # the same at another size
UIBENCH_HOST=<bench> harness/uibench/boot-time.sh wpe      # reboots the bench: power-on to usable (§8)
```

`full.sh` builds the bundle, installs it on the bench gateway (`/opt/aetherrv-rs/ui` only), builds the tool and the
Tauri app natively on the Pi, runs `run.sh all headless` there and `perturb.sh` for each runtime, and copies the
results to `harness/out/uibench/` (gitignored). To measure a bundle without installing it, as the final pass did,
start `serve_bundle.py <dir>` on the bench and run `run.sh` with `UIBENCH_URL=http://127.0.0.1:18082/?bench=1`
and `UIBENCH_BUNDLE_DIR=<dir>`, and `perturb.sh` with `UIBENCH_BASE=http://127.0.0.1:18082`. On the bench,
`harness/uibench/run.sh <runtime|all> headless [WxH]` runs one runtime. `calibrate.sh <0|30|60> [frames options]`
repeats the calibration of §2, and `perturb.sh [--runtime …] [page…]` the cost check of §3. `boot-measure.sh
arm|report|disarm` is the bench side of `boot-time.sh`. `aetherrv-uibench frames`, `globals` and `touch-test` are
the tool's own diagnostics. Everything refuses to run on a host with `hat0`/`hat1` (the van). URL switches for
experiments: `?keepgl=0` or `?keepgl=sky,radar,bank` (which WebGL scenes stay alive on hide), `?sky=2d`,
`?skyfps=N`, and `&press=promote|nofilter` (bench module only, §4.3).

**How the final pass was actually run, and what has not run as written.** The final pass, the memory runs (§4.1),
the per-scene runs, the press experiments (§4.3) and the before-Home run (§4.2) were driven by a scratch script
outside the repo. It called `run.sh` and `perturb.sh` with the URL and bundle overrides above, against
`serve_bundle.py`, and the commands are as given in each section. The power-on runs used `boot-time.sh` as
committed, except its journal-based firmware figure, which was added afterwards (§8). Not run as written:
`full.sh` and `prepare.sh` since their headless rewrite, and `calibrate.sh` (the calibration used the same
commands). The 2560×1440 size was not run.
