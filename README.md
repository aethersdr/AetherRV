# AetherRV

A touchscreen console for a camper van, running on a Raspberry Pi 5: power, climate, network and switching on one
screen, with chrome from the [AetherSDR](https://aethersdr.com) project.

![AetherRV home screen on the van's 10.1" touchscreen](docs/screenshots/home.png)

<img src="docs/screenshots/energy.png" width="24%" alt="Energy screen: 3D battery bank with four batteries read over Bluetooth, power readings, inverter and solar controls, power history"> <img src="docs/screenshots/climate.png" width="24%" alt="Climate screen: drag-to-set dial with a live fan-speed slider, Rixens MCS7 and Espar heater status, refrigeration and temperature history"> <img src="docs/screenshots/weather.png" width="24%" alt="Weather screen: sunrise/sunset day bar, radar map and a 7-day forecast"> <img src="docs/screenshots/network.png" width="24%" alt="Network screen: Starlink stats over a 3D obstruction sky with the van, Cellular and WWAN cards, drag-to-reorder WAN priority with a disabled section, and throughput with SNR">

- **Energy:** a Victron Cerbo GX over MQTT shows battery, solar, shore, alternator, inverter and loads as an
  animated flow, with history. It controls MultiPlus mode, shore current limit and MPPT on/off. Individual
  lithium batteries with a RoyPow BMS (e.g. EPOCH) are read over Bluetooth: SoC, current, cells,
  temperatures and faults per battery.
- **Climate:** Rixens MCS7 hydronic heat over its local HTTP API shows setpoint, fan, heat sources, hot water,
  engine pre-heat, burner diagnostics and fuel flow. Fridge and freezer temperatures come from Cerbo sensors.
- **NMEA 2000:** device list and digital switching (Carling/Maretron CLMD16 load controller and CKM12 keypads).
  It shows on/off, dimmer level, per-channel current and the controller's wired inputs. It can switch and dim
  (126208 commands, gated) and optionally run the keypad logic, via the Cerbo's `socketcand` or a CAN HAT.
- **Network:** Starlink (local gRPC), Peplink router API, GPS (NMEA 0183 over UDP), device health checks, and a
  weather radar with a 7-day forecast for the van's position.
- **Settings:** Wi-Fi (iwd), Tailscale, system updates, rename, screen auto-off, reboot. The time zone is set
  automatically from GPS.
- **Panel:** sway + the UI runtime chosen by benchmark ([`docs/UI-RUNTIME-BENCHMARK.md`](docs/UI-RUNTIME-BENCHMARK.md)),
  screen off with touch-to-wake, and a Plymouth boot/shutdown splash. The same React app works in Safari, Chrome
  and Firefox on phones and laptops.

AetherRV is written in Rust: one small service per device plus a few core services, talking over a local MQTT
broker, with a React + TypeScript UI. It replaced the original Python system, which was the reference it was tested
against (197 ported tests, golden-parity replays of real CAN captures); that system is in the git history (tag
`python-final`). See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## Safety model

- **Deny by default.** A `TxGate` refuses every write until a device's `tx_enabled` is set in the Pi's own config.
- **Careful command path.** Every command reads fresh state, sends only if the value differs, sends once,
  verifies the result and never retries. Each command is logged.
- **Who can control.** By default, controls and settings changes work only from the van's touchscreen
  (loopback) or from devices owned by the same Tailscale user as the console. `[web] control_subnets` can
  also trust whole LANs, and `control_tailnet = "all"` can trust every tailnet device. Everything else is
  view-only.
- **The devices stay in charge.** Heat, power, Starlink, switching and every other control only act when a
  person asks. NMEA 2000 keypads, groups, timers and "output follows input" rules live in the load
  controller's own configuration (dual control), so they keep working while the Pi is off; AetherRV never
  relays keypad presses or switches anything by itself. A channel whose safety logic isn't in the load
  controller yet is `blocked` (it refuses ON).
- **Root access is minimal.** The only root component is a small, socket-activated helper with a fixed list of
  validated actions. Wi-Fi passwords never appear in logs, the live state or command lines.
- **Isolation.** Every device has its own process and systemd unit. A crash, restart or upgrade of one module (or
  of the broker) leaves the others running; this is tested by killing each one in turn under systemd while a
  simulated heater runs.
- **The Espar never notices a restart.** The Espar service owns the heater's CAN port and keeps the cyclic frames
  going itself, with no runtime dependency on the broker, gateway or UI. If it restarts mid-run it re-attaches to
  the running heater without a new start-up or an OFF; with no bus contact for a grace period it turns the heater
  off. Heat lockout on a low battery is enforced by a guard and again inside each heater service.

## Layout

| Path | What |
|---|---|
| `crates/` | Shared libraries: `core` (bus client, state, commands, TxGate, config), `can` (SocketCAN, socketcand, failover, capture), protocol codecs (`proto-n2k`, `proto-espar`, `proto-roypow`, `proto-nmea`, `proto-mcs7`), `power`, `sim` (device simulators) |
| `services/` | One binary per device or core function: `espar`, `n2k`, `rixens`, `victron`, `batteries-ble`, `starlink`, `peplink`, `gps`, `weather`, `netmon`, `can-aux`, `power-guard`, `alerts`, `history`, `derive`, `settings`, `gateway` (web + WebSocket), `screen`, and `admind` (the root helper) |
| `ui/` | The React + TypeScript touch UI (Vite; Leaflet and three.js bundled, no CDN) |
| `sims/` | Simulator binaries for the bench: heater, CLMD16 + keypads, MCS7, Cerbo, Starlink, Peplink, GPS, CAN capture replay |
| `utils/` | `aetherrv-ctl` (bus client), `aetherrv-migrate` (checks config, secrets, history and settings), `aetherrv-setup` (Victron pairing, the Peplink read-write API client, secrets writes), the isolation and soak tools |
| `packaging/` | systemd units, the panel session, Mosquitto config, network/PAM/boot-splash files, the system update, installer, `push.sh`, staged transmit (`cutover.sh`) |
| `harness/` | Test harnesses: the bench scripts, UI screenshots, the runtime benchmark, the licence list (`crates.sh`) |
| `tools/` | Laptop-side setup (Peplink API clients, the TomTom key), the boot splash, the N2K table generator, read-only van access |
| `config/aetherrv.example.toml` | **Sample** configuration. Copy it and fill in your devices |

## Install (on the Pi)

**Building one from scratch?** [`docs/BUILD-GUIDE.md`](docs/BUILD-GUIDE.md) walks through the hardware, installing
and configuring Arch Linux ARM, wiring the CAN buses, and has prompts for adapting AetherRV with an AI assistant
when your equipment differs.

```sh
packaging/push.sh <your-pi> --system     # rsync, native cargo build on the Pi, install, enable and start
```

`push.sh` never has a default host. `--system` is for a fresh console; a console that still runs the old Python
system switches with the runbook in [`docs/CUTOVER.md`](docs/CUTOVER.md) (read-only first, then one
device at a time).

The installer creates `/etc/aetherrv/aetherrv.toml` from the sample only if it's missing. Edit it on the Pi with
your devices' addresses. Secrets (Victron MQTT token, Peplink API clients and read-only login, TomTom key) go in
`/etc/aetherrv/secrets.toml` (root:aetherrv 0640), written by `aetherrv-setup` and the tools in `tools/`, never into
the repo. The boot splash changes `/boot` and is a separate, one-time step: `sudo tools/install_boot_splash.sh`.

## Development

```sh
harness/dev-setup.sh                      # once per boot: the virtual CAN interfaces the tests use (sudo)
cargo test --workspace                    # Rust: unit, integration and process tests (vcan + a private Mosquitto)
(cd ui && npm ci && npx vitest run && npx playwright test)   # UI: unit tests and smoke tests against a stubbed gateway
```

No containers: everything runs as plain processes. The simulators let the whole system run on a Pi with no van
attached (`harness/bench/bench-up.sh`). [`docs/TEST-PARITY.md`](docs/TEST-PARITY.md) maps each test of the Python
system to its Rust or UI equivalent, and [`docs/STATUS.md`](docs/STATUS.md) holds the evidence for the conversion.

`INTEGRATION_PLAN.md` is the (sanitized) build log: protocol findings, decisions and verification notes.

## License

GPL-3.0 — see [LICENSE](LICENSE).
