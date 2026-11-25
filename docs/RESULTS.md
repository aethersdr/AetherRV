# Results: the Python system and the Rust system, before and after

The results page for the conversion in `docs/RUST-CONVERSION-PROMPT.md` (§11 item 3). It puts the prompt's §8
acceptance numbers side by side, Python first, then Rust, and lists the other evidence and what the bench could not
show. The detail lives in the documents linked from each row; `docs/STATUS.md` holds the goal-condition checklist.

## Where and how

- **Bench Pi**: Raspberry Pi 5, 16 GB, NVMe, Arch Linux ARM; no CAN HAT. Every device is a simulator on loopback or
  a vcan (`harness/bench/bench-up.sh`), and for the UI runs four simulated EPOCH batteries
  (`harness/uibench/batteries.sh`), so the Energy page shows its 3D bank. Nothing here ran on the van Pi.
- **Screen**: headless 1920×1200 at 60 Hz (sway's headless output). The owner skipped the real-screen runs (Q-U7),
  so every §8 number, power-on included, is judged on this output. The van's panel is re-checked in the van
  (`docs/CUTOVER.md` §8).
- **Builds**: the final build is **01aa558**, cross-compiled on the workstation for the bench
  (`harness/bench/cross-push.sh`, the same Rust 1.100.0 as the Pi). Earlier runs used native builds through
  `packaging/push.sh`, as the van will. The UI runtime is WPE WebKit 2.54 through the panel's own launcher
  (`panel/wpe/aetherrv-panel-wpe`), the owner's choice (Q-U8). "Before" is the Python reference: its own UI (`web/`)
  in Chromium kiosk with the van's flags, and for power-on the whole Python system (backend and kiosk) as installed by
  `deploy/install.sh`.
- **Method**: `harness/uibench` + `utils/uibench` (injected uinput touches, frames timed from the compositor's
  screencopy, PSS from `smaps_rollup`). The method and its own cost are in `docs/UI-RUNTIME-BENCHMARK.md` §2–§3.
  Final pass 2026-11-24 (bench time 08:22–08:55): 3 warm rounds, 48 presses per page and 100 on Network, sims up.
  Power-on: 10 reboots of the Rust system and 2 of the Python system, same procedure (`harness/uibench/poweron.sh`).
- **How §8 is judged** (the owner, 2026-11-24): taps each under 100 ms, with 50 ms a loose target (Q-U12); page
  switches warm, to a page already visited since boot (Q-U13).

## §8 numbers

| §8 number | Before: Python + Chromium | After: Rust + WPE launcher | Met? |
|---|---|---|---|
| Tap to visible response ≤ 50 ms (each under 100 ms, Q-U12) | 182 / 253 ms p50 / p95; 341 of 484 presses showed nothing within the 300 ms hold (only CSS `:active`) | **35 / 47 ms** over 436 presses, **every one under 100 ms** (the slowest 94); 13 over 50 ms; per page p95 ≤ 48 ms except Switches 53 | **yes** |
| Page switch ≤ 150 ms (warm, Q-U13) | 312 / 450 ms warm over all pages; every page's p50 220–450 ms; first visit, worst 2216 ms (Network) | **93 / 139 ms** warm over all pages; every page ≤ 141 ms p95 (Weather, its radar rebuilt on return). First visit to a page, worst: 647 ms (Weather) | **yes** |
| 60 fps on Home, Climate, Switches | page rAF 62 / 60 / 60 fps, Home at 82 % of a core | **62 / 62 / 62 fps**, Home at 23 % of a core | **yes** |
| ≥ 30 fps for the Energy, Network, Weather scenes | Network 34 fps; no 3D bank, no radar loop | **Energy's bank 62, Network 60, Weather ≥ 32 fps** (a lower bound, from the f9968b1 pass: in the final one the loop's frames didn't change in the sampled patch, most likely no rain in view) | **yes** |
| Power-on to a usable UI ≤ 15 s | **16.89, 17.04 s** (2 boots) | **median 14.74 s, max 15.16 s**; 1 of 10 boots over 15.0 s (firmware 8.92 s on that boot). Firmware 8.2–8.9 s of every boot; kernel → usable 6.10–6.36 s (Python 8.24–8.28 s) | **not on every boot; accepted by the owner as measured** (Q-U10) |
| UI process memory ≤ 400 MB | 667 MB PSS after every page (the Chromium process tree) | **392 MB PSS** after every page (247 MB after boot). Under the same heavy tapping, other runs ended at 381–421 MB (499 once, before the safety net): accepted by the owner, with the launcher's WebKit memory limit as a safety net (Q-U11) | **yes** in the final pass; heavy use accepted |

Also measured: cold start to a usable UI 0.74 s (Python UI 1.66 s); no light frame before the UI. In the van today,
Chromium uses about 1.15 GB across 14 processes (prompt §1; measured differently, RSS on the van).

Tauri and Chromium running the same React bundle are in `docs/UI-RUNTIME-BENCHMARK.md` §5.2–§6 (the 2026-11-23
pass, before the bench had battery data). Chromium misses tap (Home, Network), page switch (Energy, Alerts, Weather)
and memory (586 MB); Tauri misses tap (Home, Network) and memory (457 MB). WPE is the only one of the three that
meets the numbers, and the lightest (§9 there).

**Power-on.** Two thirds of every boot is the Pi 5's firmware, before Linux starts: 8.2–8.9 s here, once 11.1 s.
From the kernel to a usable UI, the Rust system takes 6.1–6.4 s, against 8.2–8.3 s for the Python system. The
changes that got it there:

- the gateway and the panel no longer wait for `network.target`;
- the panel's own PAM session has no user service manager (`class=user-light`);
- the runtime starts at once.

The bench's own firewall adds 1.4 s before `network.target`, and logind still orders the panel's session after it.
The van runs no firewall unit.

**Memory.** WebKit's web process grows with heavy tapping (its own caches and allocator, not the UI's data:
`docs/UI-RUNTIME-BENCHMARK.md` §4.6). A smaller JavaScript heap target made no difference. WebKit's memory pressure
handling with a low limit kept it at 341 MB but slowed page switches to 207 ms p95. The launcher therefore sets a
high limit only as a safety net: it frees caches past 450 MB and never kills the web process.

**Correction.** Until 2026-11-24 07:16 the bench had no battery data, so the Energy page never showed its 3D bank,
and no earlier figure includes it. Every figure above is from the pass with the bank on the page.

## The other evidence

| Area | Result | Where |
|---|---|---|
| Tests | `cargo test --workspace` 393 passed, 0 failed (f9944f1; no Rust source changed after it); `tsc` clean, Vitest 109/109, Playwright 100/100 (the final UI, a192ca8) | `docs/STATUS.md` item 2 |
| Reference tests | 197 / 197 Python tests mapped to a Rust or UI test | `docs/TEST-PARITY.md` |
| Golden parity | van captures replayed into both systems: differ 0, Rust-only keys exactly D7; D1–D22 accepted by the owner | `docs/STATUS.md` item 3, `docs/ARCHITECTURE.md` §13a |
| Isolation | **PASS** on 01aa558 (2026-11-24 09:55 bench time): `kill -9` of each of the 16 services, `can-aux@aux0` and the admin helper, the broker restarted and killed. Each victim back with fresh state in 0.26–1.3 s, no other unit down, the gateway serving throughout, the Espar heating throughout (max status gap 0.53 s, 0 controller-loss faults), 0 commands to the MCS7; history's longest gap 56 s against its 30 s interval (one sample missed while it restarted; 30 s on f9968b1). Also PASS on f9968b1 | `harness/bench/isolation.sh` |
| Panel isolation | **PASS** on 01aa558: the real panel unit (WPE launcher, headless output). A killed screen helper is back in 1.1 s in the same session; after a kill -9 of the WPE launcher, its web process or sway the whole panel is back in 3.4–3.6 s; `systemctl restart` 1.2 s. Nothing of the old session left, no other unit restarted, the gateway answering throughout | `harness/bench/panel-isolation.sh` |
| Espar seamless resume | **PASS** on 01aa558: a HEAT run through `systemctl restart`, `kill -9`, a SIGSTOP (watchdog kill) and a broker restart; resumed after 0.5, 1.3 and 3.3 s gaps (max status gap 3.21 s), 0 controller-loss faults, no stored fault, NRestarts 2. Also PASS on f9968b1 | `harness/bench/espar-systemd.sh` |
| Espar blind running | **PASS** on 01aa558: broker down 30 s → the run continued; broker down for good → the service turned the heater OFF after 61 s without bus contact, normal after-run, 0 faults. Also PASS on f9968b1 | `harness/bench/espar-blind.sh` |
| Soak (1 h, owner decision) | **PASS** on f9968b1 (2026-11-24 05:38–06:38 bench time), every verdict true: 24/24 commands ok (n2k over the bus and the WebSocket, MCS7, Espar FAN/HEAT/OFF), no restarts, no memory growth, the WebSocket never dropped (48,657 messages, max gap 0.66 s), the heater never lost its controller (max status gap 0.21 s, 0 faults). No service source changed after f9968b1. Also PASS at 6501abe | `harness/bench/soak.sh`, `docs/STATUS.md` item 5 |
| Rollback | **36 ok, 0 failed** on 01aa558 against a real Python install: read-only cutover to a serving Rust gateway 1.9 s with every transmit permission off, `--allow n2k`, `--full` (the Rust panel up with the WPE launcher), rollback refused while the Espar heats, the one-command rollback to the Python system 3.0 s with nothing of the Rust panel left and :8091 free, then back to Rust. Also 33/0 on f9968b1 | `harness/bench/rollback-test.sh` |
| Van untouched | every van access read-only and logged; the van's own journal and package log show no install, restart or command since the conversion began | `docs/STATUS.md` item 7 |

## What the bench could not show

Checked in the van at the cutover (2026-11-24, `INTEGRATION_PLAN.md` §49): the Bluetooth batteries on real BlueZ, the
Espar's controller-loss tolerance (15.2 s in FAN), and, by the owner, touch, screen off/on and the alert chime. The
rest stays on the list.

On the van's list (`docs/CUTOVER.md` §8):

- real touch feel and the panel's own latency;
- the §8 numbers on the real panel, and power-on by stopwatch;
- touch-to-wake through the screen helper;
- the boot splash hand-off;
- the alert chime from the panel's speaker;
- the panel's memory over days;
- the MCP2515 TX-echo guard;
- the Bluetooth batteries on real BlueZ;
- the Cerbo failover under a wiring fault;
- the Espar's real controller-loss tolerance.

## Owner decisions on these results (2026-11-24)

- The UI runtime: the WPE launcher (Q-U8).
- The Weather radar is freed when its page is hidden (Q-U9).
- D22, the Espar keys' 250 ms refresh, is accepted (Q-P3).
- Power-on is accepted as measured (Q-U10).
- UI memory under heavy use is accepted, with the launcher's safety net (Q-U11).
- Taps are judged each under 100 ms (Q-U12), page switches warm (Q-U13).
