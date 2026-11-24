# Results: the Python system and the Rust system, before and after

The results page for the conversion in `docs/RUST-CONVERSION-PROMPT.md` (§11 item 3). It puts the prompt's §8
acceptance numbers side by side, Python first, then Rust, and lists the other evidence and what the bench could not
show. The detail lives in the documents linked from each row; `docs/STATUS.md` holds the goal-condition checklist.

## Where and how

- **Bench Pi**: Raspberry Pi 5, 16 GB, NVMe, Arch Linux ARM; no CAN HAT. Every device is a simulator on loopback or
  a vcan (`harness/bench/bench-up.sh`). Nothing here ran on the van Pi.
- **Screen**: headless 1920×1200 at 60 Hz (sway's headless output). The owner skipped the real-screen runs (Q-U7),
  so every §8 number, power-on included, is judged on this output. The van's panel is re-checked in the van
  (`docs/CUTOVER.md` §8).
- **Builds**: Rust system **f9968b1** as installed by `packaging/push.sh`, UI bundle `index.html` **a78d994cf492**
  (built from f9968b1). The UI runtime is WPE WebKit 2.54 through the panel's own launcher
  (`panel/wpe/aetherrv-panel-wpe`). "Before" is the Python reference: its own UI (`web/`) in Chromium kiosk with
  the van's flags, and for power-on the whole Python system (backend and kiosk) as installed by `deploy/install.sh`.
- **Method**: `harness/uibench` + `utils/uibench` (injected uinput touches, frames timed from the compositor's
  screencopy, PSS from `smaps_rollup`). The method and its own cost are in `docs/UI-RUNTIME-BENCHMARK.md` §2–§3.
  Final pass 2026-11-24 (bench time 04:56–05:23): 3 warm rounds, 48 presses per page and 100 on Network, sims up.
  Power-on: 10 reboots of the Rust system and 2 of the Python system, same procedure (`harness/uibench/poweron.sh`).

## §8 numbers

| §8 number | Before: Python + Chromium | After: Rust + WPE launcher | Met? |
|---|---|---|---|
| Tap to visible response ≤ 50 ms | 179 / 244 ms p50 / p95; 341 of 484 presses showed nothing within the 300 ms hold (only CSS `:active`) | **34 / 47 ms** over 436 presses; every page's p95 ≤ 48 ms; Network 42 / 47 over 100 presses, none over 50. 4 of 436 presses over 50 ms (slowest 89 ms, Weather) | **yes** at p95 on every page |
| Page switch ≤ 150 ms | 285 / 390 ms warm over all pages; every page's p50 207–390 ms; first visit, worst 2317 ms (Network) | **77 / 109 ms** warm over all pages; every page ≤ 114 ms p95. First visit to a page, worst: 839 ms (Weather, lazy chunk + WebGL set-up) | **yes** (warm) |
| 60 fps on Home, Climate, Switches | page rAF 62 / 60 / 60 fps, Home at 82 % of a core | **62 / 62 / 62 fps**, Home at 22 % of a core | **yes** |
| ≥ 30 fps for the Energy, Network, Weather scenes | Network 30 fps; no radar loop | **Network 61 fps, Weather ≥ 32 fps** (a lower bound); Energy has no idle animation | **yes** |
| Power-on to a usable UI ≤ 15 s | **16.89, 17.04 s** (2 boots) | **median 14.74 s, max 15.16 s**; 1 of 10 boots over 15.0 s (firmware 8.92 s on that boot). Firmware 8.2–8.9 s of every boot; kernel → usable 6.10–6.36 s (Python 8.24–8.28 s) | **not on every boot** (owner's call, below) |
| UI process memory ≤ 400 MB | 553 MB PSS after every page (the Chromium process tree) | **375 MB PSS** after every page (250 MB after boot); GPU buffers 1.38 GB for the whole system, not counted in PSS (almost all the kept radar loop, `docs/UI-RUNTIME-BENCHMARK.md` §4.1) | **yes** |

Also measured: cold start to a usable UI 0.72 s (Python UI 1.70 s); no light frame before the UI. In the van today,
Chromium uses about 1.15 GB across 14 processes (prompt §1; measured differently, RSS on the van).

Tauri and Chromium running the same React bundle are in `docs/UI-RUNTIME-BENCHMARK.md` §5–§6 (the 2026-11-23
pass). Chromium misses tap (Home, Network), page switch (Energy, Alerts, Weather) and memory (586 MB); Tauri misses
tap (Home, Network) and memory (457 MB). WPE is the only one of the three that meets the numbers, and the lightest
(recommendation, §9 there).

**Power-on.** Two thirds of every boot is the Pi 5's firmware, before Linux starts: 8.2–8.9 s here, once 11.1 s.
From the kernel to a usable UI, the Rust system takes 6.1–6.4 s, against 8.2–8.3 s for the Python system. The
changes that got it there:

- the gateway and the panel no longer wait for `network.target`;
- the panel's own PAM session has no user service manager (`class=user-light`);
- the runtime starts at once.

The bench's own firewall adds 1.4 s before `network.target`, and logind still orders the panel's session after it.
The van runs no firewall unit. The remaining levers are the owner's: the bootloader configuration, or the bench
firewall for a measurement (`docs/UI-RUNTIME-BENCHMARK.md` §8).

## The other evidence

| Area | Result | Where |
|---|---|---|
| Tests | `cargo test --workspace` 390 passed, 0 failed; `tsc` clean; Vitest 109/109; Playwright 99/99 (f9968b1) | `docs/STATUS.md` item 2 |
| Reference tests | 197 / 197 Python tests mapped to a Rust or UI test | `docs/TEST-PARITY.md` |
| Golden parity | van captures replayed into both systems: differ 0, Rust-only keys exactly D7; D1–D21 accepted by the owner, D22 asked | `docs/STATUS.md` item 3, `docs/ARCHITECTURE.md` §13a |
| Isolation | **PASS** on f9968b1 (2026-11-24 05:32 bench time): `kill -9` of each of the 16 services, `can-aux@aux0` and the admin helper, the broker restarted and killed. Each victim back with fresh state in 0.26–1.3 s, no other unit restarted, the gateway serving throughout, the Espar heating throughout (max status gap 0.89 s, 0 controller-loss faults), 0 commands to the MCS7, history max gap 30 s | `harness/bench/isolation.sh` |
| Espar seamless resume | **PASS** on f9968b1: a HEAT run through `systemctl restart`, `kill -9`, a SIGSTOP (watchdog kill) and a broker restart; resumed after 0.5, 1.2 and 3.4 s gaps (max status gap 3.17 s), 0 controller-loss faults, no stored fault, NRestarts 2 | `harness/bench/espar-systemd.sh` |
| Espar blind running | **PASS** on f9968b1: broker down 30 s → the run continued; broker down for good → the service turned the heater OFF after 61 s without bus contact, normal after-run, 0 faults | `harness/bench/espar-blind.sh` |
| Soak (1 h, owner decision) | **PASS** on f9968b1 (2026-11-24 05:38–06:38 bench time), every verdict true: 24/24 commands ok (n2k over the bus and the WebSocket, MCS7, Espar FAN/HEAT/OFF), no restarts, no memory growth, the WebSocket never dropped (48,657 messages, max gap 0.66 s), the heater never lost its controller (max status gap 0.21 s, 0 faults). Also PASS at 6501abe (01:15–02:16) | `harness/bench/soak.sh`, `docs/STATUS.md` item 5 |
| Rollback | **33 ok, 0 failed** on f9968b1 against a real Python install: read-only cutover to a serving Rust gateway 2.2 s with every transmit permission off, `--allow n2k`, `--full`, rollback refused while the Espar heats, the one-command rollback to the Python system 3.1 s, then back to Rust | `harness/bench/rollback-test.sh` |
| Van untouched | every van access read-only and logged; the van's own journal and package log show no install, restart or command since the conversion began | `docs/STATUS.md` item 7 |

## What the bench could not show

On the van's list (`docs/CUTOVER.md` §8):

- real touch feel and the panel's own latency;
- the §8 numbers on the real panel, and power-on by stopwatch;
- touch-to-wake through the screen helper;
- the boot splash hand-off;
- the alert chime from the panel's speaker;
- the MCP2515 TX-echo guard;
- the Bluetooth batteries on real BlueZ;
- the Cerbo failover under a wiring fault;
- the Espar's real controller-loss tolerance.

## Owner decisions

Pending, asked when the owner is back: the UI runtime (WPE launcher recommended) and the kept radar loop's GPU memory; D22 (the Espar keys' 250 ms refresh, Q-P3); and power-on, which is not met on every boot.
