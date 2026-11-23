# Goal: convert AetherRV to a modular Rust system with a React UI

You are converting **AetherRV** (this repository) from a single Python process with a hand-written JavaScript UI
into a **modular control system in Rust with a React UI**. The owner has already made the architecture decisions
below in an interview; treat them as settled. Keep working until the goal condition at the end is met.

This is **not** a one-pass job and you are not expected to guess. Whenever the right answer is the owner's to
give, **stop and ask with the AskUserQuestion tool**, then carry on with the answer. §9 says when and how.

## 1. Why, and what "better" means

The owner is unhappy with the kiosk's performance and responsiveness, and with the fact that restarting the
backend disturbs everything, most painfully the Espar heater: AetherRV is the heater's only controller, so a
backend restart makes the heater lose its controller, shut down within ~15 s and come back OFF.

Two outcomes matter more than anything else:

1. **Isolation.** Every module is its own process and survives the restart, crash or upgrade of any other module.
   The owner must be able to iterate on one part of the system without touching another. Restarting the web
   gateway, the UI, the alert engine or the message broker must not interrupt a running heater.
2. **Responsiveness.** The touchscreen must feel instant, hit the numbers in §8, and look impressive doing it.

Measured on the van Pi today (Raspberry Pi 5, 16 GB, 1920×1200 touchscreen): Chromium uses ~1.15 GB across 14
processes; the UI is one 2,900-line `web/app.js` that rebuilds a whole page's HTML on every state update (76
`innerHTML` sites) several times a second. The rendering model, more than the engine, is the main cost. Fix both.

## 2. Read first (the existing system is the specification)

The Python system on `main` is **frozen** and is the behaviour reference. Before designing, read all of it:

- `README.md`, `docs/BUILD-GUIDE.md`, `INTEGRATION_PLAN.md` (start with §0 "The working system", then §41–48),
  `docs/espar-control-design.md`.
- `src/aetherrv/` (~7,500 lines): `__main__.py` (wiring), `state.py`, `txgate.py`, `canbus.py` (FailoverBus, TX
  echo guard, link recovery), `alerts.py`, `history.py`, `settings.py`, `switching.py`, `lowbatt.py`, `radar.py`,
  `traffic.py`, `climate.py`, `energy.py`, `web.py`, and every adapter in `adapters/`.
- `web/` (~5,000 lines): `app.js`, `style.css`, `index.html`, `charts.js`, `sky3d.js`, `icons.js`, `vendor/`.
- `deploy/` (systemd units, `admin/aetherrv_admin.py` root helper, kiosk, installer, `push.sh`) and `tools/`.
- `tests/` (~3,000 lines, 197 tests): these encode the safety rules and protocol details. Every one must have an
  equivalent in the new system.
- `config/aetherrv.example.toml`: the configuration contract.

## 3. Decisions already made by the owner

| Topic | Decision |
|---|---|
| Language | **All Rust, no Python at runtime.** This includes the Bluetooth battery protocol (RoyPow BMS), Starlink gRPC, NMEA 2000, the Espar protocol and UDS, and the radar motion fields (today OpenCV DIS optical flow). |
| Process model | **One service per device**, plus core services. Each is its own binary and systemd unit and restarts alone. |
| Inter-process bus | **A local MQTT broker** with retained state, so a restarted module or UI gets current state at once. |
| UI | **React** (TypeScript). **Keep the current layout** on every page; improve motion, feedback and graphics. |
| UI runtime | **Benchmark three options on a Pi 5 and choose by the numbers:** WPE WebKit kiosk (`cog`), a Tauri v2 app (WebKitGTK), and Chromium. See §7. |
| Other screens | Phones and laptops keep using the **same React app in a normal browser** over the van LAN, home LAN and the tailnet, with today's control rules. |
| Cutover | **Build everything complete first, then one cutover.** No module-by-module migration on the van. |
| Your reach | **Stop before the van.** Prove everything on the bench Pi, then hand over a cutover runbook and a rollback script. Do not install on or restart anything on the van Pi. |
| Test rig | A **second Pi 5 (16 GB, NVMe, Arch Linux ARM) with no CAN HAT**. Its screen is **not the van's panel**: a 13.3" 2560×1440 monitor with **no touch**, on HDMI (see §8 for how to measure on it). CAN is tested on virtual CAN with replayed captures and simulators. |
| Repository | **Same repo, new branch `rust`**: a Cargo workspace plus `ui/`. Python stays untouched on `main`. |
| Data | The existing `/etc/aetherrv/aetherrv.toml`, `/etc/aetherrv/secrets.toml`, `/var/lib/aetherrv/history.sqlite3`, `settings.json` and the radar cache must **keep working or be converted by a migration tool** you write and test. |
| Espar: its own restart | If the Espar service itself restarts while the heater runs and is back **within ~10 s**, it **resumes seamlessly** (same mode, setpoint, time left). If it was gone longer, it comes back OFF as today. |
| Espar: running blind | If the Espar service is healthy but has had **no contact with the bus for a grace period** (default 10 min, configurable), it sends OFF and lets the heater cool down. |
| Acceptance numbers | The strict set in §8. |

## 4. Architecture requirements

Design the details yourself and write them up in `docs/ARCHITECTURE.md` before building. It must satisfy:

**Services** (names are suggestions): `espar`, `n2k` (NMEA 2000 decode + CLMD16 switching), `victron`,
`batteries-ble`, `rixens`, `starlink`, `peplink`, `gps`, `netmon`, `weather` (radar recorder, motion fields,
traffic proxy), `alerts`, `history`, `power-guard` (today's `lowbatt.py`), `climate` (derived cabin values),
`gateway` (HTTP + WebSocket for the UI, auth), `admin` (the root helper), and the kiosk. A shared library crate
holds the bus client, config loading, the transmit gate and the command contract.

**Bus contract.**
- State is published as retained messages under a documented topic tree. Keep today's state key names (e.g.
  `climate.espar.*`, `victron.battery.soc`, `switches.list`, about 250 keys in 23 namespaces) recoverable from
  the topics so the parity tests in §6 can compare old and new directly.
- Commands are request/reply with a correlation id and a timeout. Every command result is explicit: ok, refused
  (with the reason shown to the user), or unverified.
- Each service announces itself, publishes a heartbeat and has a last-will message, so the UI and the alert
  engine can show "module down" per module.
- The broker listens on loopback or a Unix socket only. Nothing on the LAN can publish to it. Remote users go
  through the gateway, which applies the control rules.
- **Every service must ride out a broker restart**: reconnect, republish its state, lose nothing important.
  Hardware-facing services keep doing their job while the bus is down.

**The safety model is not negotiable. Carry all of it over, with tests:**
- **Deny by default.** A transmit gate refuses every write to a device until that device's `tx_enabled` is set in
  the Pi's own config. CAN ports are listen-only at the kernel level unless configured otherwise.
- **The command path**: fresh read of device state, send only if it differs, send once, verify, never retry, log
  every attempt (who, what, outcome) to the command log.
- **Who may control**: the touchscreen (loopback), the owner's Tailscale devices, and configured subnets /
  tailnet policy. Everyone else is view-only. Enforce it in the gateway, never in the UI alone.
- **The devices stay in charge.** AetherRV never switches a CLMD16 breaker directly: it presses virtual switches
  only (one documented exception: a single direct OFF to clear a tripped channel). Drawer locks are trigger-only.
  `requires_input` / `on_with_input` refusals stay. No keypad relaying, no automations.
- **The one automatic action**: the low-battery guard (`lowbatt.py`). Heat lockout at ≤ 10 % (held 2 min, not
  charging): both heaters refuse to start heat and are turned off; before the 5 % clean power-off, wait for
  heater after-runs. The owner requires this because **the Espar and the MCS7 can be seriously damaged if they
  lose power while burning or in their after-run**. The lockout must be enforced inside the heater services
  themselves, and must keep protecting them if the guard service or the bus is down.
- **Root is minimal**: one socket-activated helper with a fixed list of validated verbs. Secrets never appear in
  logs, state, topics or command lines. Wi-Fi keys, SIM identifiers and similar are never parsed into state.
- **Fail-safe direction**: on any doubt (stale state, another Espar controller on the bus, no TX echo, heater
  silent), stop transmitting and report. Port the MCP2515 TX-echo guard and the port auto-recovery.

**The Espar service specifically** (the owner's headline example):
- It owns the heater's CAN port and sends the cyclic frames itself (054/055/056/057 every 200 ms, 60D every
  100 ms, 065 room temperature). It has **no runtime dependency** on the broker, gateway, UI or any other
  service to keep a run going. Prove this with a test that kills each of those while a simulated heater runs.
- Seamless resume: it persists its run state (mode, setpoint, deadline, flags) on every change; on start, if the
  state is younger than ~10 s and the heater is still reporting the same mode, it re-attaches without the wake
  burst or an OFF. Give it a fast restart policy. Prove the heater simulator sees no gap longer than its 15 s
  tolerance and logs no controller-loss fault.
- Blind-running grace period as decided in §3.
- Room temperature comes over the bus (cabin Ruuvi via the Victron service) with today's fallback to the
  heater's own intake sensor when that is stale or the bus is down.
- Keep the read-only UDS diagnostics (services 0x22 / 0x19 only, anything else refused in code and tested).

## 5. The React UI

- **Same layout, same pages, same controls** as today on every view (Home, Energy, Climate, Weather, Switches,
  Network, Alerts, Systems, Settings and its tabs). Capture reference screenshots of every page of the Python UI
  at 1920×1200 first, and compare your pages against them. Differences must be deliberate improvements.
- TypeScript, a modern bundler, everything vendored: **no CDN or internet dependency** for the app itself.
- State arrives over one WebSocket from the gateway. Components subscribe to exactly the keys they show, so a
  battery reading changing re-renders one number, never a page.
- The owner asked for all four of these:
  1. **Instant touch feedback**: pressed state within a frame; optimistic toggles that roll back with a clear
     message when the device refuses or doesn't verify.
  2. **Smooth 60 fps motion**: page transitions, spring-animated dials, values that tween instead of jumping.
  3. **Richer live graphics**: the energy flow, the 3D battery bank, charts that scrub smoothly, the 3D Starlink
     sky and the GPU radar loop, kept and improved.
  4. **Fast boot**: skeletons at once, data as it arrives.
- Must also work in current Safari, Chrome and Firefox on phones and laptops, including at phone width.
- Keep the UI's existing safety behaviours: confirmations before heat/fan/critical actions, the heat-lockout
  banners and locked buttons, view-only mode, hold-to-dim, the shared setpoint rules for the two heaters.
- **Never test taps against a page connected to live devices.** UI tests run against simulators or a gateway
  whose command path is stubbed. A past headless tap test sent a real command to the van; do not repeat it.

## 6. How to prove it works without the van

- **Port the test suite.** Each of the 197 Python tests gets an equivalent (Rust unit/integration tests, UI
  tests). Keep a mapping table in `docs/TEST-PARITY.md`.
- **Simulators** for every device, good enough to exercise the whole command path: Espar heater (states, after-run,
  bus sleep, 15 s controller-loss shutdown, fault memory over UDS), CLMD16 + keypads on virtual CAN, MCS7 HTTP
  (`tests/fixtures/mcs7_status_hw1_1.208.xml` is a real status document), Victron MQTT, Starlink gRPC, Peplink
  API, BLE batteries (mock the BlueZ layer), GPS.
- **Golden parity.** Replay recorded CAN captures and recorded API responses into both the Python reference and
  the Rust system and diff the resulting state key by key. The van Pi holds ~1 GB of raw CAN captures and the
  live history database. You may **read** from the van Pi over SSH to copy captures and sample state; you may not
  install, restart or send anything there. Sanitize anything you commit (§10).
- **Isolation tests** (the point of the project): with the full system running against simulators, kill and
  restart each service in turn, and the broker, and assert that every other service keeps working, that state
  recovers, and that a running Espar and an MCS7 are not interrupted.
- **Soak**: run the whole system on the bench Pi for at least 12 hours against simulators; no crashes, no memory
  growth, no stuck state.
- **Bench Pi** setup (Arch Linux ARM as in `docs/BUILD-GUIDE.md`, virtual CAN) is yours to do. Ask the owner
  for its SSH name (AskUserQuestion) before you start on it. The screen is already attached.

## 7. Choosing the UI runtime

Build the same React production bundle and run it, on the bench Pi's real screen, under: (a) WPE WebKit with
`cog` in kiosk mode, (b) a Tauri v2 application, (c) Chromium in kiosk mode. For each, measure on every page:
frame rate (including the WebGL2 radar loop and the 3D sky), tap-to-visual latency, page-switch time, cold start
to first usable frame, CPU and memory. WebKit's WebGL performance on the Pi 5 is unproven, so measure it; do not
assume. Recommend the lightest option that meets every number in §8, show the owner the numbers and let them
confirm the choice (AskUserQuestion). Write the method, the numbers and the choice
to `docs/UI-RUNTIME-BENCHMARK.md`. Screen power control (today done through sway) and the boot splash hand-off
must keep working with whatever you choose.

## 8. Acceptance numbers (bench Pi 5, 1920×1200, chosen runtime)

- Tap to visible response: **≤ 50 ms**.
- Page switch: **≤ 150 ms**.
- **60 fps** steady on Home, Climate and Switches; **≥ 30 fps** for the animated scenes on Energy, Network and
  Weather.
- Power-on to a usable UI: **≤ 15 s**.
- UI process memory: **≤ 400 MB**.
- Measure the Python/Chromium system the same way first and report before/after.

**The bench screen differs from the van's** (owner's decision, 2026-11-23). The van has a 10.1" 1920×1200
capacitive touchscreen; the bench has a 2560×1440 non-touch monitor whose mode list has no 1920×1200.
- First try a custom 1920×1200 mode on the bench screen. If it displays correctly, the numbers above are judged
  there.
- If the monitor rejects it, the numbers must pass at **1920×1080**, and you also measure and report native
  **2560×1440** as the headroom check (about 60 % more pixels than the van). Say plainly in the results which
  case applied.
- There is no touch panel, so measure tap latency with **injected touch events** (for example a uinput virtual
  touchscreen) timed against the frame that shows the response. Run these only against simulators (§5).
- What the bench cannot prove (real touch feel, the Waveshare panel's own latency, hold-to-dim, screen power
  control on that panel) goes into `docs/CUTOVER.md` as checks to do on the van.

## 9. Asking the owner

Use the **AskUserQuestion** tool for every question. Never put a question, an open decision or a "let me know"
in closing prose, and never end a turn with something only the owner can unblock left unasked.

**Ask when:**
- The reference behaviour is ambiguous, looks like a bug, or two parts of the Python system disagree, and the
  choice changes what the user sees or what a device is sent. Never guess on anything that touches the safety
  model or a heater.
- A design choice has a real trade-off that §3 does not settle (for example the broker, the topic layout where
  it affects parity, a crate with a licence or maintenance concern, how a service behaves when a dependency is
  down).
- You find evidence that a settled decision in §3 is wrong or cannot be met. Show the evidence and the options;
  do not quietly work around it or re-open it without evidence.
- A number in §8 cannot be met, a test cannot be ported, or a parity difference needs accepting. Do not lower a
  target, drop a test or accept a difference on your own.
- You need something physical or an approval: the bench Pi's SSH name, the screen, a secret typed by the owner,
  permission for a blocked action, reading from the van Pi for the first time.
- A "deliberate improvement" to a page changes a control's position, meaning or behaviour.

**Do not ask** what you can find out by reading the code, the tests, the docs or the device captures, or what
§3 already answers.

**How:**
- First do all the work that does not depend on the answer. Then ask, then continue.
- Group related questions into one call (up to four). Each question must stand alone: say what you found and
  name the things you are asking about.
- Offer two to four concrete options, with your recommendation first and marked, and say what each one costs.
- Ask early. Raise the architecture questions before building on them: present `docs/ARCHITECTURE.md` and get the
  owner's go-ahead before writing the services.
- Record every answer, with the date, in a "Decisions" section of `docs/ARCHITECTURE.md`, so a later session
  does not ask again.
- If the owner is away and a question is still open, work on parts that do not depend on it; do not pick an
  answer for them.

## 10. Working rules

- Branch `rust` only. Never touch `main` (bug-fix exceptions need the owner's request) and never push or rewrite
  the `private-history` branch. Commit in small, described steps; push `rust` regularly.
- **This is a public repository.** No personal data in committed files: no real IP addresses, hostnames,
  Bluetooth addresses, serial numbers, tokens, SSIDs or the owner's name. Use documentation ranges and
  placeholders, as the existing files do.
- You type no passwords or tokens. When a secret is needed, give the owner the command to run.
- Record every design decision and every surprising finding in `docs/ARCHITECTURE.md` or a new
  `INTEGRATION_PLAN.md` section, in the style of the existing ones.
- Prefer boring, maintained crates. Note the licence of anything you vendor; the project is GPL-3.0.

## 11. Deliverables

1. The `rust` branch: Cargo workspace, `ui/`, systemd units, installer and `deploy/push.sh` equivalent, the root
   helper, all tests and simulators.
2. `docs/ARCHITECTURE.md`: services, topic tree, command contract, failure behaviour of each service.
3. `docs/UI-RUNTIME-BENCHMARK.md`, `docs/TEST-PARITY.md`, and a results page with the §8 numbers before/after.
4. A **migration tool** for config, secrets, history and settings, tested against copies of the real files.
5. **`docs/CUTOVER.md`**: a step-by-step runbook for the van (pre-checks such as "both heaters off and cold",
   install, a read-only comparison period with every transmit gate off, enabling control one device at a time
   with the owner present, what to watch) and a **tested rollback script** that restores the Python system in
   one command.
6. An updated `docs/BUILD-GUIDE.md` and README for the new system.

## Goal condition

Stop and report when **all** of these are true, each backed by evidence you can show (test output, benchmark
files, logs):

- Every item in §11 exists on the pushed `rust` branch.
- The full test suite passes, and `docs/TEST-PARITY.md` maps all 197 reference tests.
- Golden-parity diffs against the Python reference are empty or every difference is explained and accepted in
  writing by the owner.
- The isolation tests pass for every service and the broker, including the Espar seamless-resume and
  blind-running cases.
- The 12-hour soak completed cleanly on the bench Pi.
- The runtime benchmark is done on the bench Pi's real screen and every number in §8 is met by the chosen runtime, at the resolution §8 specifies.
- Nothing has been installed, restarted or transmitted on the van Pi.
- No question for the owner is open, and every answer is recorded in `docs/ARCHITECTURE.md`.

Then give the owner a short summary, the measured before/after numbers, anything you could not prove on the
bench, and the cutover runbook. Do not begin the cutover.
