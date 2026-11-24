# Rust conversion: status and evidence

Working file for the conversion in `docs/RUST-CONVERSION-PROMPT.md`. Each goal-condition item has its current
state and where its evidence lives. Updated as work lands; the final report is built from this page.

| # | Goal-condition item | State | Evidence |
|---|---|---|---|
| 1 | Every §11 deliverable exists on the pushed `rust` branch | in progress | 37 crates: core, CAN, codecs, 16 services + admind, simulators (heater, Cerbo, Starlink, Peplink, MCS7, CAN replay), migrate, ctl, isolation; CUTOVER.md draft; UI foundation in progress |
| 2 | Full test suite passes; `docs/TEST-PARITY.md` maps all 197 reference tests | in progress | 197 / 197 mapped (`harness/test-parity.sh`). `cargo test --workspace` (2026-11-24, 3691e08): **390 passed, 0 failed** (a run at 9dd1346 had one flaky netmon port test, fixed in 3baf49a by holding the port; 20/20 since). UI: Vitest 102/102, Playwright per page (smoke 6, climate 22, settings 15, energy 13, systems 6); Weather/Network pending. Final run at the end |
| 3 | Golden-parity diffs empty or each difference accepted by the owner | met | `harness/parity/parity.py` (van captures replayed into Python and Rust at once, owner rule Q-P2 for time fields and alert raise times). **Final runs at 3691e08 (2026-11-24), 300 s each:** two N2K + Espar capture pairs (15,718 + 14,773 and 15,712 + 4,226 frames), read 1 s after the replay and once 6 s after: **differ 0** in all three, no Python-only keys, Rust-only keys exactly D7. A first read at 3 s showed `climate.espar.mode`/`room_c` differing. That is the decoders' 3 s EasyStart-command expiry: Python recomputes every 1 s, Rust every 250 ms, and a read on that edge compares sampling phase. Reads at 1 s and 6 s, either side of the edge, agree, so the harness now reads at 1 s (`--settle`). Victron, Starlink, Peplink per-service diffs 0. D1–D21 all accepted by the owner |
| 4 | Isolation tests pass for every service and the broker, incl. Espar resume and blind running | **met** (advisor review addressed) | One service build (no service, crate, sim or unit change between b1d7227 and 3516f81). **Whole system** (`harness/bench/isolation.sh`, 3516f81, heat lockout ON as in the van): `kill -9` of each of the 16 services, `can-aux@aux0` and the admin helper; the broker restarted and killed: ISOLATION: PASS. Each victim back as a new PID with a heartbeat after the kill and its state restamped (0.26–1.3 s); every OTHER unit's MainPID and NRestarts unchanged in every case; the gateway answered `/api/state` throughout; the switching path worked while each victim was down; the admin helper came back by socket activation and answered Settings; history kept writing (max gap 57 s at a 30 s interval); the Espar heating throughout (max 054 gap 0.67 s, 0 controller-loss faults); 0 commands to the MCS7. The journal of the earlier run shows exactly the intended kills. **Espar resume** (`espar-systemd.sh`, 3516f81): a HEAT run through `systemctl restart`, `kill -9`, SIGSTOP→watchdog and a broker restart, max gap 3.10 s, 0 faults, `resume ok` rows 0.5/1.4/3.3 s. **Blind running** (`espar-blind.sh`, b1d7227): broker down 30 s → the run continued; broker down for good → OFF after 61 s without bus contact, normal after-run, 0 faults. Not killed: `aetherrv-screen`, which runs inside the panel's sway session (no panel on the bench, owner Q-U7); the panel unit restarts it with the session |
| 5 | Soak clean on the bench Pi (12 h in the prompt; **1 h by owner decision 2026-11-23**) | rerun pending | The 1 h soak at 3516f81 (2026-11-23 22:17, lockout on, aux bus) failed one verdict: no restarts, no memory growth, owners always up, no stuck state, WebSocket never dropped (48,824 messages, max gap 0.69 s), heater never lost its controller (max 054 gap 0.20 s, 0 faults), but **4 of 24 commands** (n2k ch7/ch15 from 23:07) were refused "not claimed an NMEA 2000 address yet". Cause: a bug ported from switching.py. Each reply to the Cerbo's 10-s address-claim poll restarted the 250 ms claim wait, and the soak's 5-min schedule had drifted into that window. Fixed as D21 (owner, Q-N1), with a test. The 1 h soak reruns on the fixed build after the UI benchmark pass. |
| 6 | UI runtime benchmark; every §8 number met at the §8 resolution (**headless 1920×1200 on the bench Pi**: the owner skipped the monitor tests, Q-U7; power-on measured headless with a reboot; the real panel is re-checked in the van) | in progress | harness `harness/uibench` + `utils/uibench`; headless draft pass done (WPE lightest: 237→366 MB PSS, usable 0.67 s after launch); UI-side misses being fixed (Network page switch, Home CPU, press feedback, Weather scene); final pass and the owner's runtime choice to come |
| 7 | Nothing installed, restarted or transmitted on the van Pi | met (re-checked at the end) | Every van access went through `tools/van-read.sh` (read-only subcommands), logged to `harness/out/van-access.log` (gitignored): 11 entries (identity, one /api/state dump, captures listing and pull, config, secrets key names, history and settings copy). **The van's own record** (`tools/van-read.sh audit`, 2026-11-24 06:44 UTC): booted 2026-11-23 01:30 UTC, `aetherrv.service` active since 17:49 UTC and the kiosk since 17:57 UTC (both before the conversion began at 21:58 UTC), 0 restarts; no `/opt/aetherrv-rs` and no Rust unit files; no package changes since 2026-11-23; every ssh login since 21:00 UTC came from the workstation, at the times in the access log (plus the audit's own two); **no command or settings action** in the Python journal since 21:00 UTC (624 lines; the same pattern finds 240 the day before, so the zero is real) |
| 8 | No open owner question; every answer in `docs/ARCHITECTURE.md` | none open now | ARCHITECTURE.md §14 and Decisions (Q-A1 … Q-N1); the runtime choice is asked after the final benchmark pass |

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
- 2026-11-23 (late): rollback tested on the bench against a real Python install (`harness/bench/rollback-test.sh`,
  b1d7227): 33 ok, 0 failed. Read-only cutover to a serving Rust gateway 1.8 s with every transmit permission off;
  `--allow n2k`, `--full`; rollback refused while the Espar heated and during its after-run; the one-command
  rollback back to the Python system in 3.0 s (no Rust unit active or enabled, the Python units restored, the
  Python update unit untouched). Espar blind running under systemd passed. The first 1 h soak stalled on a bug in
  the soak tool (fixed, b1d7227; the system was clean up to minute 30); a fresh 1 h soak started 21:52.
- 2026-11-23 (night): stricter isolation PASS at 3516f81 (item 4 met). The 1 h soak at 3516f81 failed `every_command_ok`: 4 n2k commands fell in the 250 ms after a Cerbo address-claim poll reply and were refused "not claimed yet" (a switching.py bug, about 1 tap in 40 in the van). Fixed as D21 (owner, Q-N1); soak rerun pending.
