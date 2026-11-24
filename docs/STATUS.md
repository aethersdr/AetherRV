# Rust conversion: status and evidence

Working file for the conversion in `docs/RUST-CONVERSION-PROMPT.md`. Each goal-condition item has its current
state and where its evidence lives. Updated as work lands; the final report is built from this page.

| # | Goal-condition item | State | Evidence |
|---|---|---|---|
| 1 | Every §11 deliverable exists on the pushed `rust` branch | in progress | 37 crates: core, CAN, codecs, 16 services + admind, simulators (heater, Cerbo, Starlink, Peplink, MCS7, CAN replay), migrate, ctl, isolation; CUTOVER.md draft; UI foundation in progress |
| 2 | Full test suite passes; `docs/TEST-PARITY.md` maps all 197 reference tests | in progress | 197 / 197 mapped (`harness/test-parity.sh`); full-workspace run to be repeated at the end |
| 3 | Golden-parity diffs empty or each difference accepted by the owner | in progress | `harness/parity/parity.py` (van captures replayed into Python and Rust at once): 60 s run and 300 s run, no Python-only keys, Rust-only keys exactly D7, remaining diffs are start-time skew of one alert (Q-P2 for the owner). Per-service diffs vs Python against shared simulators: Victron, Starlink, Peplink 0. D1–D10 accepted; D11–D18 waiting for the owner |
| 4 | Isolation tests pass for every service and the broker, incl. Espar resume and blind running | in progress | Espar process level (`services/espar/tests/process.rs`). Bench under systemd (`harness/bench/espar-systemd.sh`, c717323): FAN run and HEAT run each kept running through `systemctl restart`, `kill -9`, SIGSTOP→watchdog and a broker restart; HEAT run max 054 gap 3.06 s, 0 controller-loss faults, `resume ok` rows 0.5/1.5/3.1 s. Whole-system run (`harness/bench/isolation.sh`) next |
| 5 | 12-hour soak clean on the bench Pi | not started | — |
| 6 | UI runtime benchmark on the bench screen; every §8 number met at the §8 resolution | not started | — |
| 7 | Nothing installed, restarted or transmitted on the van Pi | holding | every van access goes through `tools/van-read.sh` (read-only subcommands), logged to `harness/out/van-access.log` (gitignored): identity, one /api/state dump, a listing and an rsync pull of the captures |
| 8 | No open owner question; every answer in `docs/ARCHITECTURE.md` | open | ARCHITECTURE.md §14 |

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
  harness/out/van (gitignored). Bench Pi `AetherPI`: checked (no hat0/hat1, sudo, vcan, uinput), kernel/boot packages
  held, upgraded, rustup/mosquitto/sway/chromium/wpewebkit/webkit2gtk-4.1 installed; `cog` is not in the Arch ARM repos
  (build from source). Agents running: victron + batteries-ble; starlink + peplink; alerts/history/derive/settings/
  netmon/gps; weather. Next for the lead: gateway (flat view, auth, WS, proxy), admind, can-aux, screen, parity
  harness against the Python reference, then the UI.
- 2026-11-23 (evening): every service ported (Victron, BLE, Starlink, Peplink, weather, alerts, history, derive,
  settings, netmon, gps, gateway, admind, can-aux, screen), 197/197 reference tests mapped. Golden-parity harness
  and first runs. Heater simulator fixed (multi-frame UDS, FAN → HEAT); the bench HEAT run resumed through every
  restart kind. MCS7 simulator, CAN replayer and the isolation orchestrator written; CUTOVER.md drafted.
