# Rust conversion: status and evidence

Working file for the conversion in `docs/RUST-CONVERSION-PROMPT.md`. Each goal-condition item has its current
state and where its evidence lives. Updated as work lands; the final report is built from this page.

| # | Goal-condition item | State | Evidence |
|---|---|---|---|
| 1 | Every §11 deliverable exists on the pushed `rust` branch | in progress | 37 crates: core, CAN, codecs, 16 services + admind, simulators (heater, Cerbo, Starlink, Peplink, MCS7, CAN replay), migrate, ctl, isolation; CUTOVER.md draft; UI foundation in progress |
| 2 | Full test suite passes; `docs/TEST-PARITY.md` maps all 197 reference tests | in progress | 197 / 197 mapped (`harness/test-parity.sh`). `cargo test --workspace` (2026-11-23, 2ddd5bb+): 389 passed, 0 failed in 126 test binaries. UI: Vitest 102/102, Playwright per page (smoke 6, climate 22, settings 15, energy 13, systems 6); Weather/Network pending. Final run at the end |
| 3 | Golden-parity diffs empty or each difference accepted by the owner | owner rule set; runs to repeat at the end | `harness/parity/parity.py` (van captures replayed into Python and Rust at once): 60 s and 300 s runs: no Python-only keys, Rust-only keys exactly D7, the remaining diffs fall under the owner's tolerance rule (Q-P2). Victron, Starlink, Peplink per-service diffs 0. D1–D20 all accepted by the owner |
| 4 | Isolation tests pass for every service and the broker, incl. Espar resume and blind running | passing; blind case on the bench after the soak | Bench under systemd, 87b712d (`harness/bench/isolation.sh`): `kill -9` of each of the 16 services, the broker restarted and killed: ISOLATION: PASS. Each victim back as a new PID with a heartbeat after the kill and its state restamped in 0.26–1.6 s, no other service down, the switching command path working while each victim was down, the Espar heating throughout (max 054 gap 0.58 s, 0 controller-loss faults), 0 commands to the MCS7. Espar alone (c717323): HEAT run through restart / kill -9 / watchdog / broker restart, max gap 3.06 s. Blind running: process test (`services/espar/tests/process.rs`); bench run `harness/bench/espar-blind.sh` after the soak |
| 5 | Soak clean on the bench Pi (12 h in the prompt; **1 h by owner decision 2026-11-23**) | running | `harness/bench/soak.sh … 1` started 2026-11-23 19:02 (87b712d; the 12 h run started at 18:36 was stopped at the owner's word) as `aetherrv-soak-run`: whole system on simulators, switch toggles over the bus and the WebSocket, MCS7 nudges, hourly Espar FAN → HEAT → OFF, ignition flips; verdicts in /var/tmp/soak/summary.json |
| 6 | UI runtime benchmark; every §8 number met at the §8 resolution (**headless 1920×1200 on the bench Pi by owner decision Q-U7**; real-screen numbers and power-on become van checks) | in progress | harness `harness/uibench` + `utils/uibench`; headless draft pass done (WPE lightest: 237→366 MB PSS, usable 0.67 s after launch); UI-side misses being fixed (Network page switch, Home CPU, press feedback, Weather scene); final pass and the owner's runtime choice to come |
| 7 | Nothing installed, restarted or transmitted on the van Pi | holding | every van access goes through `tools/van-read.sh` (read-only subcommands), logged to `harness/out/van-access.log` (gitignored): identity, one /api/state dump, a listing and an rsync pull of the captures |
| 8 | No open owner question; every answer in `docs/ARCHITECTURE.md` | none open now | ARCHITECTURE.md §14 and Decisions (Q-A1 … Q-U6) |

## Log

- 2026-11-23: read the Python reference; 197 tests collected and passing. Architecture draft written
  (`docs/ARCHITECTURE.md`), waiting for the owner's review before any service is written.
- 2026-11-23: owner approved the architecture (31dde83) and answered Q-A1…Q-D2. Built and pushed: aetherrv-core
  (bus contract tested against a private Mosquitto), aetherrv-can (incl. real-vcan echo and loopback proofs),
  proto-n2k / -espar / -roypow / -nmea / -mcs7, aetherrv-power, the Espar service and the heater simulator.
  To do next: the remaining services (docs/SERVICE-GUIDE.md), the MCS7 XML reader onto quick-xml, the bench Pi
  setup, the van captures (read-only wrapper), the React UI.
- 2026-11-23 (later): N2K switching (all 28 controller tests), the MCS7 command path (5 tests + lockout), the power
  guard (7 tests) done and pushed. Van read-only: 249 keys / 23 namespaces in /api/state; 1.1 GB of captures pulled to
  harness/out/van (gitignored). Bench Pi: checked (no hat0/hat1, sudo, vcan, uinput), kernel/boot packages
  held, upgraded, rustup/mosquitto/sway/chromium/wpewebkit/webkit2gtk-4.1 installed; `cog` is not in the Arch ARM repos
  (build from source). Agents running: victron + batteries-ble; starlink + peplink; alerts/history/derive/settings/
  netmon/gps; weather. Next for the lead: gateway (flat view, auth, WS, proxy), admind, can-aux, screen, parity
  harness against the Python reference, then the UI.
- 2026-11-23 (evening): every service ported (Victron, BLE, Starlink, Peplink, weather, alerts, history, derive,
  settings, netmon, gps, gateway, admind, can-aux, screen), 197/197 reference tests mapped. Golden-parity harness
  and first runs. Heater simulator fixed (multi-frame UDS, FAN → HEAT); the bench HEAT run resumed through every
  restart kind. MCS7 simulator, CAN replayer and the isolation orchestrator written; CUTOVER.md drafted.
- 2026-11-23 (night): CLMD16, GPS simulators; whole-system isolation PASS under systemd with stricter checks;
  the soak started (1 h, owner decision). UI: Home, Switches, Alerts, Climate, Settings, Energy, Systems committed; Weather and
  Network in progress; the WebGL battery bank (owner Q-U6) and the Wi-Fi-off confirm (Q-U5) in progress.
  Packaging: exact unit lists for cutover/rollback, gateway `ui_dir`, `install.sh --system`, the panel unit,
  `aetherrv-setup victron-pair`. Docs: README, BUILD-GUIDE, CRATES.md. Owner answers Q-P2, Q-D3, Q-U1…Q-U6,
  Q-A2, Q-S1 recorded (D19, D20 implemented).
