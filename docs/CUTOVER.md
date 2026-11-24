# Cutover runbook: Python → Rust in the van

**Do not start without the owner present.** Every step that changes what a device is sent is the owner's call, in
person, one device at a time. The rollback is one command and can be run at any point (§9).

## 0. Before the day

- [ ] The bench results are green: `docs/STATUS.md` (all eight items), `docs/TEST-PARITY.md`, the parity runs,
      the isolation tests, the 1 h soak and the UI runtime benchmark (`docs/UI-RUNTIME-BENCHMARK.md`).
- [ ] The UI runtime the owner chose (WPE through the panel's launcher, 2026-11-24) has its packages on the van Pi
      (§3) and `aetherrv-panel.service` is in `packaging/systemd/`.
- [ ] Read §8 (checks the bench could not do) so they are on the day's list.

## 1. Pre-checks in the van (all must hold)

- [ ] **Both heaters off and cold**: Espar `climate.espar.state` is `off` (asleep, no after-run), MCS7 not heating
      (`climate.rixens.state` not `heating`, `heater_state_code` idle). `packaging/heaters-idle.sh` must say
      "heaters idle". The Espar loses its controller during the switch: it must not be burning or in its after-run.
- [ ] House battery above 30 %, not near a heat lockout.
- [ ] Shore or solar present (the Pi stays powered whatever happens).
- [ ] A laptop on the tailnet with an SSH session to the van Pi, and the touchscreen within reach.
- [ ] No other Claude session or person is deploying to the van (`deploy/push.sh` would undo the cutover).

## 2. Backups (on the van Pi, as root)

```sh
ts=$(date +%Y%m%d-%H%M%S)
cd /   # the unit glob below is expanded by the shell, so it must run in /
tar -czf /root/aetherrv-pre-rust-$ts.tgz etc/aetherrv etc/systemd/system/aetherrv* var/lib/aetherrv/settings.json
sudo -u aetherrv sqlite3 /var/lib/aetherrv/history.sqlite3 ".backup /var/lib/aetherrv/history-pre-rust-$ts.sqlite3"
systemctl list-unit-files 'aetherrv*' > /root/aetherrv-units-pre-rust-$ts.txt
```

## 3. Install next to Python (nothing starts)

From the workstation: `packaging/push.sh <van-host> --cutover`. It syncs the source to `/opt/aetherrv-rs/src`, builds
natively (about 6 minutes on a Pi 5), installs the binaries, units and Mosquitto config, and **starts nothing**.
The Python system keeps running. Install the packages the Rust system needs if missing, before the push (the build
and the panel need them):

- `clang` and `rustup` for the native build, then, as the admin user, `rustup default 1.100.0` (the toolchain the
  bench was tested with; `base-devel` is already there from the Python install);
- `jq`: `heaters-idle.sh`, and so `cutover.sh` and the **rollback**, read the heaters' state with it (the van had
  none on 2026-11-24);
- `mosquitto` (the bus);
- the UI runtime, **WPE** (the owner's choice, 2026-11-24; `docs/UI-RUNTIME-BENCHMARK.md` §9). `install.sh` builds the
  panel's WPE launcher only if `wpewebkit` is installed when it runs, and `launch.sh` falls back to Chromium without
  it, so install these first and check `/opt/aetherrv-rs/bin/aetherrv-panel-wpe` exists after the push. Keep
  Chromium installed: the Python kiosk, and so the rollback, needs it.
  - **WPE**: `wpewebkit` (it brings `bubblewrap` and `xdg-dbus-proxy`), `noto-fonts` (the bench had to add the
    fonts: WPE has no bundled fallback font), and **`gst-plugins-base gst-plugins-good`** for the alert chime.
    WebKit plays WebAudio through GStreamer's `autoaudiosink` (gst-plugins-good), which picks `alsasink`
    (gst-plugins-base) when there is no sound server, as in the van. `wpewebkit` depends on neither: without them the
    page is silent and its AudioContext stays suspended (`harness/uibench/audio-gst-check.sh`);
  - (not chosen) Tauri would need `webkit2gtk-4.1` and the same two GStreamer packages; Chromium is already there
    and plays to ALSA itself.

The panel's PAM stack (`packaging/pam/aetherrv-panel`, installed by `install.sh`) uses `pam_systemd class=user-light`,
which needs systemd 256 or later: check `systemctl --version` (the van ran 262 on 2026-11-24).

`pacman -S --needed <packages>` on an up-to-date system only (Arch does not support partial upgrades). If
`pacman -Syu` is due, do it on another day, not on the cutover day.

## 4. Check the data

`sudo /opt/aetherrv-rs/bin/aetherrv-migrate` → expect `OK` lines only (`NOTE` lines are informational). On a
`WARN`, stop and read it with the owner. `--fix` only creates missing database tables.

## 5. Switch over, read-only

`sudo /opt/aetherrv-rs/src/packaging/cutover.sh --read-only`

This stops the Python backend, kiosk and admin helper, and starts the Rust system with **every transmit permission
off** (`AETHERRV_TX_ALLOW=` in a systemd drop-in), whatever the config says. Nothing is sent to any device.

## 6. The read-only comparison period (at least 30 minutes)

| Check | How | Expect |
|---|---|---|
| Every module up | `sudo -u aetherrv /opt/aetherrv-rs/bin/aetherrv-ctl health` and the UI | all `up`, heartbeats < 3 s |
| Energy | the Cerbo's own screen / VRM vs. Home and Energy | same SoC, power, solar, shore |
| Batteries | the four EPOCH batteries on Energy | all four reading, ~1 Hz |
| Switching | press keypad buttons by hand | tiles follow within ~2 s; inputs (ignition, reverse) shown |
| N2K | Systems page | same device list as before; hat0 primary, Cerbo backup path idle |
| MCS7 | the MCS7 panel vs. Climate | same setpoint, sources, temperatures |
| Espar | Climate | `off`/asleep; diagnostics off (no transmit) |
| Network, weather | Network, Weather | Starlink, Peplink, GPS, radar loop |
| Alerts | Alerts page | no new alerts except the expected ones |
| The kiosk | the touchscreen | page switches, touch feedback, screen off/on |

## 7. Enable control, one device at a time (owner present)

For each line: `sudo packaging/cutover.sh --allow <device>`, do the test, watch the result in the UI and in
`command_log`, and stop at the first surprise (rollback, §9).

| Device | Test | Expect |
|---|---|---|
| `n2k` | tap the spare ch8 tile on, then off; hold the cabin-lights tile to dim | ch8 follows, verified; dimming ramps while held |
| `rixens_http` | setpoint +0.5 °C then back; fan to auto | verified by the MCS7 |
| `victron` | shore limit to its current value (a no-op), then MPPT on (already on) | "already set" both times |
| `espar_diag` | wait 30 s with the heater idle | nothing sent (diagnostics only ask while the heater is awake) |
| `espar` | (heater cold) attach OFF; FAN 30 s → OFF; HEAT 5 min at a low setpoint → OFF; let the 240 s after-run finish | each verified on 2C4; no stored fault |
| `espar` resume | during a HEAT run: `systemctl restart aetherrv-espar`; then `systemctl kill -s KILL aetherrv-espar` | the heater keeps heating (no restart of its start-up), `command_log` shows `resume ok` with the gap |
| `starlink`, `peplink` | only if the owner wants: read-only verification is enough | — |

**Measuring the heater's real controller-loss tolerance** (owner, 2026-11-23: the ~10–15 s figure is unproven).
Only with the owner's go-ahead at the time, with the heater in FAN (not burning): stop the Espar service
(`systemctl stop aetherrv-espar`, which sends no OFF) and time how long the heater keeps reporting `fan` before it
stops by itself; then start the service. Expect a stored P000307/P000440 afterwards (clear it the usual way). Use
the number to set `resume_listen_s` and the restart policy, and record it in `docs/ARCHITECTURE.md`.

Then `sudo packaging/cutover.sh --full` (the config's `tx_enabled` decides, as in Python).

## 8. Checks the bench could not do

- Real touch feel on the Waveshare panel, its own latency, hold-to-dim by finger, screen power off/on and touch-to-wake
  on that panel.
- The MCP2515 TX-echo guard and port recovery on real hardware (vcan always echoes and never loses an ACK).
- The Bluetooth batteries on real BlueZ (the bench has no batteries).
- The Cerbo socketcand failover path (hat0 ↔ Cerbo) under a real wiring fault: only if the owner wants to pull hat0.
- The real Espar controller-loss tolerance (§7).
- **The §8 UI numbers on the real panel** (owner, Q-U7: the bench ran headless only): tap to visible response
  under 100 ms each (50 ms the target; owner, 2026-11-24), page switch ≤ 150 ms (warm), 60 fps on Home/Climate/Switches and ≥ 30 fps for the Energy/Network/Weather scenes,
  UI memory ≤ 400 MB, and **power-on to a usable UI ≤ 15 s** (time a cold power-up with a stopwatch from power
  applied to the first touch that works). `harness/uibench` can measure the panel in the van only with every
  transmit gate off (the read-only period, §6): it injects touches.
- **The boot splash hand-off**, watched (the bench has no Plymouth): splash, then black for under a second, then the
  UI, with no white frame; and the shutdown splash.
- **The runtime's memory over days**: the bench measured passes of about 13 minutes (`docs/UI-RUNTIME-BENCHMARK.md`
  §4.6). Check the panel's PSS (`smem -P WPEWebProcess` or `/proc/<pid>/smaps_rollup` as root) after a day and again
  after several days. It should stay around 400 MB or under; the launcher's safety net makes WebKit free its caches
  past 450 MB. The radar is freed when Weather is hidden (owner, 2026-11-24), so the GPU's buffers stay modest.
- **Screen off from the page** (Settings, the screen-off control) and touch-to-wake under the chosen runtime, on
  the real HDMI panel.
- **The alert chime, heard** under the chosen runtime: on Alerts, Sound off, then Sound on plays the serious chime
  (UI-local, nothing is sent to a device). The van has no sound server: the runtime plays to ALSA on the HDMI audio
  directly, so check it comes out of the panel's speaker. The bench proved only that samples reach ALSA; the chime
  has never been confirmed audible in the van on any runtime (`INTEGRATION_PLAN.md` §12).

## 9. Rollback (one command)

`sudo /opt/aetherrv-rs/src/packaging/rollback.sh`

Stops every Rust unit and starts the Python backend, kiosk and admin helper. Both systems use the same files, so no
data needs converting. It refuses while a heater is burning or in its after-run (`--force` overrides; the Espar then
stops itself and stores a controller-loss fault). Tested on the bench: see `docs/STATUS.md`.

## 10. After

- [ ] Leave the Python tree in `/opt/aetherrv` for at least two weeks (the rollback needs it).
- [ ] Record the date, the version (`/opt/aetherrv-rs/VERSION`) and anything surprising in `INTEGRATION_PLAN.md`.
