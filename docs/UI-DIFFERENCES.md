# The React UI: where it deliberately differs from the Python UI

The React UI (`ui/`) has the same layout, pages, controls and safety behaviour as the Python UI (`web/`). Every
page was pixel-compared with the Python UI at 1920×1200, both fed the same state (a van snapshot and invented
states through the same stub). The remaining pixel differences are animation phases, the clock, anti-aliasing,
and the map/radar/3D areas, which were masked. This page lists every difference that is on purpose. Owner
decisions are marked with their question number (docs/ARCHITECTURE.md §14).

## Everywhere

- **Instant touch feedback:** the pressed state is set on pointerdown in the same frame and held for at least
  120 ms.
- **Optimistic controls:** switch tiles, Home quick switches and the MCS7 heat buttons and badges flip at once and
  pulse while the command is verified. They roll back with a message: "X failed: …" when refused, "X not
  confirmed: …" when unverified. Dimmers and triggers get the pressed state only.
- **Values tween** instead of jumping: flow watts, battery %, temperatures, Starlink speeds, stat tiles.
- **Page transitions:** a 180 ms ease-in that never blocks input. Pages stay mounted and hidden pages cost no
  CPU; the WebGL scenes (Network sky, Weather radar, Energy bank) keep their paused context while hidden, so a
  page shows its scene at once on return (`ui/src/lib/glkeep.ts`, measured in docs/UI-RUNTIME-BENCHMARK.md).
- **Skeletons until the first snapshot.** No false "not connected" or "no data" before the first state arrives.
  The Live chip says "Connecting…" (app.js said "Offline").
- **Busy taps:** a tap during another command now says "Wait: <label> is still being confirmed". app.js ignored
  it silently. The one-command-at-a-time rule itself is unchanged.
- **Commands that are never sent** (no connection, busy, no rights) clear their pending setpoint, shore limit or
  fan value. app.js could leave it stuck at "sending…".
- **Fixed from app.js:** the debounced setpoint ± could send `null`, which the Espar path read as 0 °C (OFF).
  It now sends the tapped value. The shore-limit timer had the same flaw and is fixed the same way.
- **A held dimmer's release is always sent**, even if control rights change mid-press.
- **Module health (Rust gateway only):** a module that is down shows "— / <module> module down" instead of
  "null", "undefined" or a misleading "not connected". Systems has a Modules card built from `svc.*`.
- **Phone and small-laptop layouts** for every page. Below 760 px the rail becomes a bottom bar, with no
  horizontal scroll at 390 px or 1024 px.
- **No request ever leaves the machine in tests**: every UI test aborts requests that are not to loopback.

## Home

- The energy flow is drawn once and patched in place, once per frame.
- **Kept as today (owner, Q-U3 and Q-U4):** the MCS7 heat buttons and the inverter overlay send without a
  confirmation.

## Energy

- **The battery bank is a WebGL (three.js) scene (owner, Q-U6)** that can be dragged to rotate. The SVG scene
  of the Python UI remains as the fallback when WebGL2 is unavailable.
- Battery levels ease over 0.9 s for changes of 0.5 % or more.
- The bus flow speed changes in 0.25 s steps. app.js restyled the whole scene on every reading, which made the
  dashes jump.
- Sparkline histories (battery current, bus rates) are recorded from app start, not only while the page is on
  screen.
- The power chart sizes itself to its box. app.js sized it once, before the battery card filled in, so it stayed
  oversized and clipped until a resize.

## Climate

- **Lockout:** a source locked out by a low battery shows a lock, as on Home, and its tooltip says "locked out:
  battery low" (app.js said "not enabled on this MCS7").
- **Espar heat confirm:** the lockout is checked again when Confirm is pressed.
- **Dial knob:**
  - It glides on a spring while dragging instead of jumping between whole degrees.
  - When the MCS7 is silent and the Espar is heating, the knob sits at the Espar's setpoint. app.js drew no
    knob there, but it could still be grabbed.
- **Kept exactly:**
  - every confirmation and the Espar out-of-range refusal;
  - the shared-setpoint rules (the Espar follows 10–30 °C, is sent off below that, keeps its setpoint above);
  - the lockout matrix: engine heat is exempt, the thermostat badge and Espar fan-only stay startable as today,
    and turning off always works.

## Weather

- **Radar loop:**
  - It is WebGL2 with motion-field interpolation, up to 60 fps (app.js capped it at 15).
  - The last image stays on screen across a pan, zoom or refresh instead of blanking, and the radar fades in.
  - A failed radar index retries after 1 minute instead of 10.
- **Fixed from app.js:** tapping the map while a forecast was loading showed the old spot's forecast.

## Network

- The 3D Starlink sky renders up to 60 fps (app.js capped it at 30), and the camera eases toward a drag or pinch.
- **Fixed from app.js:** with the 3D sky on, the throughput chart overflowed its box by 14 px.

## Systems

- The Espar card says "listen-only" only while `tx.enabled.espar` is off. app.js always said "listen-only, the
  Pi never transmits", which was written before AetherRV could control the Espar.
- Bus sparklines redraw every second.
- The "last heard" line shows "—" for an unknown time. app.js printed about "20000 d ago".

## Settings

- **Asks before turning Wi-Fi off (owner, Q-U5).** Joining a network and renaming stay one step.
- The keyboard's Done and Cancel act on click, so a closing tap can't land on a button underneath.
- **Loading and refresh:**
  - Skeleton rows show until `/api/settings` answers.
  - It refreshes every 10 s while the page is visible.
  - A forced reload during a fetch runs after it (app.js dropped it).
- The sign-in QR code library loads only when a code is shown.
- The MCS7 tab's controller uptime counts live.
- **Kept:** the Wi-Fi password field is hidden by default with a Show button. A typed password is never
  logged, stored or shown back (tested).
