# Rust conversion: status and evidence

Working file for the conversion in `docs/RUST-CONVERSION-PROMPT.md`. Each goal-condition item has its current
state and where its evidence lives. Updated as work lands; the final report is built from this page.

| # | Goal-condition item | State | Evidence |
|---|---|---|---|
| 1 | Every §11 deliverable exists on the pushed `rust` branch | in progress | workspace, core, CAN, codecs, Espar service + heater simulator |
| 2 | Full test suite passes; `docs/TEST-PARITY.md` maps all 197 reference tests | in progress | 77 / 197 mapped (`harness/test-parity.sh`); reference: 197 collected, 197 passed (Python) |
| 3 | Golden-parity diffs empty or each difference accepted by the owner | in progress | deliberate differences D1–D10 accepted by the owner (ARCHITECTURE.md §13a) |
| 4 | Isolation tests pass for every service and the broker, incl. Espar resume and blind running | in progress | Espar (process level, vcan + private broker): SIGTERM + SIGKILL restarts mid-run, max 054 gap 0.55 s, no controller-loss fault, no burst/OFF/FE FF; broker down; blind OFF; foreign controller; silent heater (`services/espar/tests/process.rs`) |
| 5 | 12-hour soak clean on the bench Pi | not started | — |
| 6 | UI runtime benchmark on the bench screen; every §8 number met at the §8 resolution | not started | — |
| 7 | Nothing installed, restarted or transmitted on the van Pi | holding | every van access goes through `tools/van-read.sh`, which logs each command (none yet) |
| 8 | No open owner question; every answer in `docs/ARCHITECTURE.md` | open | ARCHITECTURE.md §14 |

## Log

- 2026-11-23: read the Python reference; 197 tests collected and passing. Architecture draft written
  (`docs/ARCHITECTURE.md`), waiting for the owner's review before any service is written.
- 2026-11-23: owner approved the architecture (31dde83) and answered Q-A1…Q-D2. Built and pushed: aetherrv-core
  (bus contract tested against a private Mosquitto), aetherrv-can (incl. real-vcan echo and loopback proofs),
  proto-n2k / -espar / -roypow / -nmea / -mcs7, aetherrv-power, the Espar service and the heater simulator.
  To do next: the remaining services (docs/SERVICE-GUIDE.md), the MCS7 XML reader onto quick-xml, the bench Pi
  setup, the van captures (read-only wrapper), the React UI.
