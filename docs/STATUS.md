# Rust conversion: status and evidence

Working file for the conversion in `docs/RUST-CONVERSION-PROMPT.md`. Each goal-condition item has its current
state and where its evidence lives. Updated as work lands; the final report is built from this page.

| # | Goal-condition item | State | Evidence |
|---|---|---|---|
| 1 | Every §11 deliverable exists on the pushed `rust` branch | in progress | workspace, core, CAN, codecs; services: espar, n2k, rixens, power-guard (+ skeletons for the rest); heater simulator |
| 2 | Full test suite passes; `docs/TEST-PARITY.md` maps all 197 reference tests | in progress | 117 / 197 mapped (`harness/test-parity.sh`); reference: 197 collected, 197 passed (Python) |
| 3 | Golden-parity diffs empty or each difference accepted by the owner | in progress | deliberate differences D1–D10 accepted by the owner (ARCHITECTURE.md §13a) |
| 4 | Isolation tests pass for every service and the broker, incl. Espar resume and blind running | in progress | Espar (process level, vcan + private broker): SIGTERM + SIGKILL restarts mid-run, max 054 gap 0.55 s, no controller-loss fault, no burst/OFF/FE FF; broker down; blind OFF; foreign controller; silent heater (`services/espar/tests/process.rs`) |
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
