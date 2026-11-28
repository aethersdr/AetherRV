# AetherRV — the Pi as Sprinter central console (build log)

Status: **in service in the van, 2026-11-23.** Sections 1–40 are the original log, kept as written (several
early decisions were later reversed; the newer section says so). Section 41 onward covers 2026-11-22/23. For
building one from scratch, see [`docs/BUILD-GUIDE.md`](docs/BUILD-GUIDE.md).

## 0. The working system (2026-11-23)

| Area | Device | Path | AetherRV does |
|---|---|---|---|
| Console | Raspberry Pi 5 16 GB, NVMe, Arch Linux ARM (custom 7.3.5 kernel), Waveshare 10.1" HDMI LCD (H) | sway + Chromium kiosk on the touchscreen; web UI for the LANs and the tailnet | Everything below |
| Power | Victron Cerbo GX, MultiPlus 12/3000, SmartSolar MPPT 100/50, Lynx Shunt, Wakespeed WS500 | MQTT (Cerbo); WS500 via NMEA 2000 | Read; MultiPlus mode, shore limit, MPPT on/off |
| Batteries | 4 × EPOCH 460 Ah (RoyPow BMS) | Bluetooth LE, 1 Hz persistent links | Read; per-battery alerts (cold, cells, faults) |
| Sensors | 4 Ruuvi tags (cabin, outside, fridge, freezer) | Via the Cerbo | Read |
| Hydronic heat | Rixens MCS7 | Local HTTP API | Read; setpoint, fan, furnace/electric/engine, cabin heat (thermostat), hot water, engine pre-heat |
| Air heat | Espar Airtronic S2 D2L | Its own CAN bus on `hat1`, 500 kbit/s | **Is the controller** (the EasyStart Pro is unplugged); read-only UDS diagnostics |
| Switching | Carling/Maretron CLMD16 + 2 × CKM12 keypads | NMEA 2000 on `hat0`, Cerbo socketcand as automatic backup | Read; presses one CLMD16 virtual switch per channel. All logic lives in the CLMD16 |
| Internet | Starlink (on its own fused feed), Peplink MAX BR1 Pro 5G | Starlink gRPC; Peplink router API | Read; Starlink heating, sleep, updates, reboot; Peplink WAN priority |
| Position | Peplink GPS | NMEA 0183 over UDP | Read; time zone, weather, map |
| Weather | LibreWXR radar, Open-Meteo, TomTom traffic | Internet | Radar loop (recorded + forecast), traffic, 7-day forecast |

**CAN ports:** `hat0` NMEA 2000 (250k, transmitting), `hat1` Espar (500k, transmitting), `fd0`/`fd1` spare
(listen-only). **Safety model** unchanged: TxGate deny-by-default per device; fresh read → send once → verify →
never retry → log; the devices keep their own interlocks and fail-safes.

## 1. Current state of the Pi (surveyed 2026-11-20)

| Item | Finding |
|---|---|
| Board | Raspberry Pi 5 Model B Rev 1.1, 16 GB RAM, NVMe root (468 GB, 2% used) |
| OS | Arch Linux ARM, custom kernel 7.3.5-v8 (built 2026-11-20 14:41 PST), about 218 packages |
| Network | `end0` <pi-home-ip> (home DHCP reservation). Wi-Fi `wld0` is DOWN |
| CAN support | Kernel modules present: `can`, `can_raw`, `slcan`, `mcp251x`, `mcp251xfd`, `gs_usb`. Overlays present: mcp2515, mcp251xfd, Waveshare and Seeed CAN-FD HATs |
| USB serial | `ftdi_sio`, `cp210x`, `ch341`, `cdc_acm` modules present (needed for K-line/W-bus adapters) |
| Attached hardware | No CAN HAT, no USB devices, no display configured beyond the vc4-kms HDMI ports |
| Software | Bare: python 3.14, sshd, systemd-networkd, tetragon. No GUI, no Docker, no Node.js |
| Home dir | Only dotfiles and `kernel-build.log`. Nothing to preserve |
| Cerbo reachable now? | No. `venus.local` does not resolve, so this is a bench machine, not in the van |

### Update after user answers (2026-11-20)
- **Mode:** bench planning. The Pi stays on the home LAN for now.
- **CAN HAT:** a Waveshare **2-CH CAN HAT+** is physically installed: 2x MCP2515 (16 MHz) + SN65HVD230, isolated, with a 120 Ω terminator on a jumper. It is **not enabled yet**: there's no HAT EEPROM and no overlay in `/boot/config.txt`, so there are no `can` interfaces. Required lines (from the Waveshare wiki; this box's config is `/boot/config.txt`, and overlays load from `/boot/7.3.5/overlays`, where all needed `.dtbo` files exist):
  ```
  dtparam=spi=on
  dtoverlay=i2c0
  dtoverlay=spi1-3cs
  dtoverlay=mcp2515,spi1-1,oscillator=16000000,interrupt=22
  dtoverlay=mcp2515,spi1-2,oscillator=16000000,interrupt=13
  ```
  MCP2515 is CAN 2.0 only, not FD. That's fine for N2K/RV-C (250k) and the Espar (500k).
- **Display:** USB touch "WSD101" (Waveshare 10.1" class) on HDMI-A-1, 1920x1200 native.
- **Espar:** **Airtronic S2 D2L + EasyStart Pro.** Goal: remove the EasyStart Pro and drive the heater directly over CAN. The closest public work is the S3 B2L reverse-engineering (500 kbit/s; heater status 0x2C4–0x2C6, heartbeat 0x625; controller cmds 0x054, 0x60D). Whether the S2 D2L matches is **unverified**, so we must capture our own traffic **with the EasyStart Pro still connected** before it can be removed.
- **Platform:** fully custom app.

### Decisions and progress (2026-11-20, later)
- **CAN HAT+ enabled.** The overlay block was appended to `/boot/config.txt` (backup: `/boot/config.txt.pre-canhat.bak`), followed by a reboot. Both MCP2515s initialize, and an internal-loopback send/recv passed on `can0` and `can1` (250k). This covers SPI and the controller only, **not** the transceiver or wiring.
  - ⚠ Naming: `spi1.2` (HAT CAN_1, INT 13) came up as **can0** and `spi1.1` (HAT CAN_0, INT 22) as **can1**. Probe order isn't stable, so we need udev/systemd-networkd `.link` names (for example `n2k`, `espar`, `rixens`) keyed on the SPI parent device.
  - `can-utils` is not in the Arch ARM repos (`pacman -Ss` confirms; AUR only). For now, use Python's raw `socket.AF_CAN`, or build can-utils from the AUR.
- **Stack:** Python asyncio backend + web UI in a Chromium kiosk (cage) on the 10.1" touch screen; phone access over the van LAN.
- **Espar:** keep the EasyStart Pro connected through capture and testing. Remove it only after Pi control **and** failsafes (Pi dies → heater shuts down) are verified on the bench.
- **Third bus:** a Waveshare **2-CH CAN FD HAT** arrives 2026-11-21. The CAN HAT+ wiki gives these stacking lines (add after the five existing ones):
  ```
  dtoverlay=mcp251xfd,spi0-0,interrupt=25
  dtoverlay=mcp251xfd,spi1-0,interrupt=24
  ```
  That gives 4 channels: N2K, Espar, Rixens, and one spare. Verify the FD HAT's interrupt and CS resistor positions against its own wiki before enabling.

### Channel allocation problem
Two CAN channels, three CAN buses that should not be merged:
| Bus | Bitrate | Notes |
|---|---|---|
| NMEA 2000 (Maretron, maybe the Cerbo VE.Can) | 250k | standard N2K |
| Espar S2 heater bus | 500k (assumed, per S3 work) | proprietary; heater ↔ EasyStart Pro |
| Rixens MCS7 | ? (RV-C would be 250k) | putting RV-C traffic on the N2K backbone is not recommended |

Options: a USB CAN adapter (candleLight/gs_usb; the kernel module is present) as `can2`, **or** put Rixens on the N2K channel if its baud rate matches and its traffic is benign (verify by sniffing).

## 2. Integration matrix

Confidence levels: **V** = verified in vendor docs or a working community project. **U** = unverified (recalled or inferred). **?** = unknown.

| Device | Physical link | Protocol | Pi hardware needed | Read | Control | Confidence | Open unknowns |
|---|---|---|---|---|---|---|---|
| **Victron Cerbo GX** + all VE devices | Ethernet or Wi-Fi to the Cerbo | MQTT (built-in broker, port 1883) recommended; Modbus-TCP (port 502) as an alternative | None (LAN) | Full | Partial. Many settings are writable via MQTT/Modbus, but some (for example MultiPlus mode) are restricted | V | Venus OS version; whether MQTT/Modbus are enabled; which VE devices are attached |
| **EPOCH LiFePO4** batteries | RJ45 CAN to the Cerbo **BMS-Can** port (500 kbit/s) | Victron CAN-BMS profile. The Cerbo republishes it | None. Read via the Cerbo | SoC, V, I, temp, CCL/DCL, alarms | None needed (the BMS is autonomous). Charge limits are handled by DVCC | V (per EPOCH docs) | Is it already wired to BMS-Can and showing in the Cerbo? Model (V2-T Elite 460 Ah?) and how many are in parallel. Known Ekrano/VE.Can mis-wiring issue |
| **Maretron DSM** switching (DCR100 etc.) | NMEA 2000 backbone (CAN 250 kbit/s) | Status via PGN **127501**; control via **126208 Command → 127501** | N2K-capable CAN interface: a CAN HAT (MCP2515/MCP251xFD + isolated transceiver, e.g. PiCAN-M) or a USB gateway (e.g. Actisense NGX-1, Yacht Devices YDNU-02) | Full | Probably yes, via 126208. Maretron's own KB articles contradict each other on non-Maretron control | V for read, U for control | Exact modules (DCR100? SIM100? DSM250 panel?); firmware versions; bank and channel instances; whether the Cerbo VE.Can is also on this backbone |
| **Rixens MCS7** hydronic | MCS7 board 4-pin CAN port (unlabeled pins; see MCS7 Install Manual p.10) | Rixens CAN messages. Rixens reportedly shares the DBC file on request; RV-C/Garmin-compatible harness exists | CAN interface (a second channel, or a shared bus if the baud rate matches) + 4-pin harness (Artek sells one) | Temps, pump, fan, status | Yes. A community ESPHome project already controls it | V (community project) | Baud rate and whether it is RV-C (250k) proper; get the DBC from Rixens; MCS7 firmware |
| **Espar Airtronic D2 (S2/S3?)** air heater | Depends on generation: **W-bus** (single-wire K-line, 2400 8E1) on older D2/S2, **CAN** on S3 / EasyStart Pro | W-bus (libwbus) or proprietary Espar CAN | W-bus: USB K-line (KKL) adapter or ESP32 + K-line transceiver. CAN: another CAN channel | Likely | Likely, but the least documented device here | U | **Exact model from the data plate** (for example "Airtronic S2 D2L" vs "S3 D2L"); current controller (EasyStart Pro/Select/Timer?); is it standalone or the Rixens burner? |

Note: Rixens says the MCS7's internal Espar burner sits on a separate, closed CAN bus. Control the burner **through the MCS7**, not directly.

## 3. Proposed architecture (draft, pending answers)

```
            ┌──────────────────── the Pi (Pi 5) ────────────────────┐
 Touch      │  Kiosk UI (browser in cage/Wayland)                        │
 display ◄──┤        ▲                                                   │
            │        │ WebSocket / REST                                  │
            │  Hub: Home Assistant (container) — or Signal K + custom UI │
            │   ├─ Victron:  MQTT client → Cerbo                          │
            │   ├─ Maretron: canboat / Signal K N2K → can0 (250k)         │
            │   ├─ Rixens:   CAN decoder (DBC) → can0 or can1             │
            │   └─ Espar:    W-bus (libwbus/serial) or CAN → ttyUSB/can1  │
            └─────────┬───────────────┬──────────────┬───────────────────┘
                 Ethernet/Wi-Fi    can0 (N2K)     can1 / K-line
                      │               │              │
                  Cerbo GX ◄─BMS-Can─ EPOCH     Rixens MCS7 / Espar
                  (VE.Can may also be bridged to N2K)
```

Two-channel isolated CAN HAT candidates: Waveshare 2-CH CAN FD HAT (MCP2518FD; its overlay is already on the box) or a PiCAN-M (N2K Micro-C connector + 12 V SMPS).

## 4. Hard constraints

1. **Safety.** The OEM controllers (Rixens MCS7 panel, Espar controller, Maretron/Cerbo panels) stay installed and fully working. The Pi is an *additional* controller. If it dies or hangs, the heaters and loads must fall back safely.
2. **No writes until verified.** Every device starts read-only (bus sniffing). Control frames are sent only after the protocol is confirmed for that exact device or firmware and you've approved it.
3. **Van power.** 12 V to 5 V/5 A buck with ignition/low-voltage handling and a clean shutdown (NVMe). Optional: the Pi 5 RTC battery.
4. **Network.** The <pi-home-ip> reservation is home-only. The van needs its own LAN (Cerbo + Pi + optional travel router), with Cerbo VRM for remote access or Tailscale (already used in this fleet).
5. **Kernel/boot config.** The custom kernel was built today. Ask before changing `/boot/config.txt` or the kernel (CAN HAT overlay needed).

## 5. Phased plan (draft)

1. **Bench (now):** base OS packages, hub software, kiosk UI skeleton; connect to the Cerbo over the LAN.
2. **CAN hardware:** install the HAT, add the overlay, bring up can0/can1, passive `candump` on each bus in the van.
3. **Read-only integration:** Victron → EPOCH (via the Cerbo) → Maretron status → Rixens telemetry → Espar status.
4. **Control (per device, with approval):** Maretron switches → Rixens setpoints/modes → Espar on/off/setpoint.
5. **Hardening:** read-only root or overlay, watchdog, 12 V power-down, remote access, backups.

## Sources
- Waveshare 2-CH CAN HAT+ wiki: https://www.waveshare.com/wiki/2-CH_CAN_HAT+
- Espar Airtronic S3 B2L CAN reverse engineering: https://github.com/TrooperDuper/espar-airtronic-esphome (variant notes: https://github.com/TrooperDuper/espar-airtronic-esphome/issues/3)
- Spoofing an EasyStart Pro (forum): https://www.fordtransitusaforum.com/threads/spoofing-a-second-easystart-pro-via-arduino-and-canbus.101989/
- Rixens MCS7 ESPHome project: https://github.com/mikegoubeaux/rixens-esphome
- Rixens MCS7 CAN thread (DBC on request, Espar bus closed): https://www.fordtransitusaforum.com/threads/rixens-hydronic-heating-mcs7-can-bus-control-via-esphome-home-assistant.97261/
- Rixens MCS7 CAN connector: https://artek.energy/products/rixens-mcs7-can-bus-connector
- Maretron DCR100 non-Maretron control (KB 587): https://www.maretron.com/wp-content/phpkbv96/article.php?id=587
- Maretron KB 622 (conflicting): https://www.maretron.com/wp-content/phpkbv96/article.php?id=622
- Panbo on DCR100 / 127501: https://panbo.com/maretron-dcr100-nmea-2000-switching/
- EPOCH V2-T + Victron: https://www.epochbatteries.com/products/12v-460ah-v2-t-lifepo4-battery
- Epoch on Ekrano GX issue: https://community.victronenergy.com/t/epoch-batteries-do-not-show-on-devices-at-ekrano-gx/42065
- libwbus (W-bus): https://sourceforge.net/projects/libwbus/
- Espar Airtronic S3 CAN ESPHome: https://community.home-assistant.io/t/espar-airtronic-s3-can-bus-climate-integration/1009609
- Pi + Cerbo via MQTT: https://community.victronenergy.com/t/running-node-red-on-rpi5-on-a-local-net-and-trying-to-use-victron-ve-bus-system-control-node/59228

## 6. Scaffold status (2026-11-20)

Deployed to the Pi at `/opt/vanpanel`. Both services are enabled and come back after a reboot (verified).

| Piece | Where | State |
|---|---|---|
| CAN names | `/etc/systemd/network/10-hat{0,1}.link` (match on SPI CS path) | `hat0` = HAT CAN_0 (spi1.1), `hat1` = HAT CAN_1 (spi1.2). Stable across 3 reboots |
| CAN config | `/etc/systemd/network/30-hat{0,1}.network` | **ListenOnly=yes**, 250k **placeholder**. A test `send()` on hat0 was refused (EACCES), TX=0. That's consistent with listen-only, but the cause isn't proven |
| Bench bus | `vcan0` (networkd netdev) | Used for synthetic traffic and capture replay |
| Backend | `vanpanel.service` (user `vanpanel`, hardened) | Python 3.14 venv via uv; config at `/etc/vanpanel/vanpanel.toml` |
| Captures | `/var/lib/vanpanel/captures/<bus>-<time>.log` | candump format, rotates at 50 MB |
| Kiosk | `vanpanel-kiosk.service` (cage + Chromium as `kiosk` on tty1) | 1920x1200 touch UI at http://127.0.0.1:8080 (also :8080 on the LAN) |
| Watchdog | `WatchdogSec=10`, fed from the event loop | Verified: SIGSTOP → killed and restarted in ~14 s. **Hung-process only, not a device failsafe** |
| Link-down | `bus.<n>.connected` reads IFF_UP | Verified: vcan0 down/up → UI shows down, frames + capture resume on up |
| Tests | `uv run pytest` | 9 passing: gated `CanBus.send` refuses, TxGate deny-by-default, N2K ID parsing, capture→replay round-trip, state pub/sub |

Verified end to end: `tools/bench_traffic.py` → vcan0 → backend → websocket → touchscreen (30 fps, 5 PGNs, 3 sources).

**Not tested:** the Victron adapter (no Cerbo reachable); it logs name-resolution retries for `venus.local`.

**Known gaps / TODO:**
- Map physical ports to buses and set real bitrates once the van is wired (Espar likely 500k; Rixens TBD).
- Enable the 2-CH CAN FD HAT (arrives 2026-11-21) → `fd0`/`fd1` .link files.
- N2K payload decoders (127501 / 126208) from canboat `pgns.json`; Rixens decoder from the Rixens DBC (ask Rixens) and the mikegoubeaux/rixens-esphome repo.
- Espar S2 D2L: capture with the EasyStart Pro connected, then compare against the S3 B2L frame map.
- Before ANY control: UI auth, a per-device heartbeat/failsafe design (Pi dies → heater stops), and lifting ListenOnly on that one bus only.
- Bitrate discovery on a new bus: stay listen-only and step through 125k/250k/500k/1M while watching the error counters (`ip -s -d link`). The right rate gives clean frames and zero errors.
- The UI shows CAN controller state (error-passive/bus-off) only as error counts; add netlink CAN state later.
- 12 V power supply and clean shutdown; van network and remote access.

**Workflow:** edit in `~/Projects/vanpanel` → `./deploy/push.sh` (rsync + `deploy/install.sh` on the Pi).

## 7. UI redesign (2026-11-20)

Five touch views (Home, Energy, Climate, Switches, Systems) at 1920x1200: an animated energy-flow diagram with a battery ring, tanks, 24 h trend charts with touch crosshair, a thermostat dial, heater cards, grouped switch tiles, and a bus inspector. Night mode (warm and dim) is on the moon button or `?night=1`. Tap the clock to switch °F/°C. Entity colors are validated for CVD (solar yellow, shore aqua, alternator violet, loads magenta, battery blue).

- History: SQLite at `/var/lib/vanpanel/history.sqlite3` (WAL), sampled every 30 s, 30-day retention pruned hourly, bucket-averaged in SQL. Served at `/api/history` (≤31 d). Charts offer 6 h/24 h/7 d/30 d (`?power=`/`?temp=` set the initial range). **While DEMO is on, history is in memory only**, so synthetic data never reaches the real DB. Persistence was verified on the Pi as the `vanpanel` user (write → reopen → read).
- **DEMO data is ON in the Pi's `/etc/vanpanel/vanpanel.toml`** (`[device.demo] enabled = true`). It disables the real Victron adapter and shows a DEMO badge. **Turn it off before the van.**
- Climate/tank/switch state keys (`climate.*`, `victron.tanks`, `switches.list`) are currently produced only by the demo. Real decoders must emit the same keys.
- Kiosk CPU while animating: about 80% of one core (about 20% of the Pi). Optimize later if needed (GPU flags, fewer full re-renders).
- Touch input itself has not been tested (screenshots only).

## 8. Maretron / NMEA 2000 decoding (2026-11-20)

Source of truth: CANboat `canboat.json` v10.0.0 (Apache-2.0). `tools/gen_n2k_pgns.py` generates `adapters/n2k_pgns.py` (the 181 fast-packet PGNs + 346 names).

- **127501 Binary Switch Bank Status:** decoded. Byte 0 = bank instance, then 28 × 2-bit indicators LSB-first (0 off, 1 on). CANboat leaves 2 and 3 unnamed; we treat them as fault / not available by NMEA convention, so **verify on real Maretron traffic**.
- **Fast-packet reassembly:** implemented, keyed by (source, PGN), and drops on a sequence change, gap, or 0.75 s stall.
- **126208 group function:** header decoded (function + commanded PGN) and logged to `n2k.commands`, shown in Systems → "N2K commands". The parameter pairs stay raw hex until real Maretron panel commands are captured. **No transmit.**
- **Switch tiles:** every reporting channel becomes a tile. Name channels with `[[device.n2k.switch]]` (bank, channel, name, icon, group) in `/etc/vanpanel/vanpanel.toml`. Unlabelled channels appear under "Unassigned". A bank silent for 30 s shows "No data". Real 127501 data replaces the demo tiles.
- Verified end to end on the Pi with `tools/bench_traffic.py` (synthetic 127501 for 2 banks → decoded → tiles). **Not yet verified against a real DCR100/DSM.** In the van: capture, confirm bank instances and channel mapping, then press buttons on a Maretron panel to record its 126208 commands.

## 9. Rixens MCS7 decoder (2026-11-20)

Frame map from the community project mikegoubeaux/rixens-esphome (MCS7.yaml, 2024-02). That repo has **no license**, so only the documented frame meanings are reimplemented, not its code. It is a **single, unverified source**; ask Rixens for the official DBC.

| ID (11-bit, 250k) | Field |
|---|---|
| 0x724 | byte 6 flags: 0x08 furnace · 0x10 electric · 0x20 engine · 0x80 preheat (selected sources) |
| 0x725 | byte 3 ×0.1 fuel-pump Hz · bytes 4-5 LE burner fan rpm · bytes 6-7 LE ×0.1 glow W |
| 0x726 | bytes 2-3 LE ×0.01 glycol °C (we read it **signed**; the source read it unsigned) · bytes 6-7 LE ×0.1 supply V |
| 0x788 → MCS7 | byte 0 property: 01 setpoint (°C×10) · 02 fan % (FF auto) · 03 furnace · 04 electric · 05 engine · 06 hot water · 07 preheat · 08 ambient (°C×10) · 0B D+ |

- `adapters/rixens.py` decodes 0x724-0x726 into `climate.rixens.*`. State is **heating** only while the burner fuel pump runs, otherwise standby (a source is selected) or off. After 10 s of silence it shows "no data". It keeps the raw ID inventory too, for frames not mapped yet.
- Command encoders for 0x788 exist as pure functions with tests. **Nothing sends them.**
- The MCS7 doesn't appear to broadcast cabin temperature. rixens-esphome *sends* ambient temp to it (0x08). The cabin sensor source is still TBD (N2K 130312? a Victron temp input?).
- Coolant and fuel-pump Hz are added to history.
- **Bench:** the Pi's `/etc/vanpanel/vanpanel.toml` points `device.rixens.bus = "bench_rixens"` (vcan1) and drives it with `tools/bench_rixens.py`. **Repoint to the real port in the van.**
- In the van: verify the bitrate (listen-only sweep), confirm each field against the MCS7 panel's own readings, and look for frames not in the map (Systems → Rixens inspector).

## 10. MCS7 operations manual findings (Rixens, 09.2023) and cabin temperature

Source: *MCS7 Touchscreen Controller Operations Manual* (Rixens, 09/14/2023), provided by the owner.

- **Wi-Fi path:** the control board runs its own hotspot, SSID `RIXEN<board serial>`, with a web UI at **http://10.10.10.10**. The 5.5" touchscreen talks to the board over this Wi-Fi. The touchscreen, control board and furnace ECU also talk CAN. → **A second integration path:** the Pi's unused Wi-Fi (`wld0`) could join the RIXEN hotspot. That web UI likely exposes everything the phone sees. Probe it in the van, read-only first.
- **Security:** the default touchscreen admin PIN is **3333** and the default Wi-Fi password is **12341234**. The manual recommends changing them; do this before the Pi joins.
- **The MCS7 has its own cabin temperature + humidity** ("external air sensor attached to the controller"). These aren't in the community CAN map, so they may be readable via Wi-Fi or other CAN IDs.
- **Controls:** heat sources Furnace (Eberspächer 5 kW hydronic) / Electric (1500 W, shore power) / Engine (status-only while the engine runs; selectable with the engine off for a 20 min waste-heat run). Hot water (furnace runs continuously). Radiant floor. Engine Pre-Heat (40 min). Aux zone. Fan auto or manual 10–99 % in 5 % steps, plus a fan enable. The fan only runs with a heat source selected and fluid ≥ 122 °F. Setpoint ±. Presets (fan, source, temp). °F/°C.
- **Status colors:** Furnace Off / Standby (blue) / Active (orange). Electric ready (blue) / active (yellow). Fluid temp: cold → 122 °F fan activation → 158 °F+ hot water ready.
- **Diagnostics page:** active status, operational hours, RPM-Watts-Hertz, coolant inlet & outlet temp, flame sensor, voltage, altitude, auto-restart indicator, engine signal. Up to 5 stored fault codes; "AF" = live fault (Eberspächer codes).
- **Settings:** glycol pump speed (High only for purging), floor/fan offset, engine-on furnace auto-off (auto) vs both (manual), hardware icon set-up, 2/3-wire fan.

**Cabin temperature (owner's choice): Victron temperature inputs.** The Victron adapter subscribes to `temperature/+/{Temperature,TemperatureType,CustomName,Humidity}`. Cabin = `device.victron.cabin_temp` (instance or Custom name), else the first sensor of type "room". Outside = `outside_temp`, else the first "outdoor". Humidity is shown when present (e.g. a Ruuvi tag). The TemperatureType enum mapping is **unverified** against this Cerbo.

## 11. Maretron IPG100 and the N2K device list (2026-11-20)

The IPG100 (manual rev 1.2, firmware 1.0.0) is Maretron's embedded **N2KServer**: it bridges N2K to N2KView clients over a closed SSL protocol on TCP 6544, with a password and a USB license key. It relays N2KView switching onto the bus (126208). **VanPanel doesn't need it**: the Pi is on the backbone directly. The owner's IPG100 locks up often. **Decision: keep it for now and monitor it.** While N2KView switches loads through it, its 126208 commands are visible in Systems → N2K commands, which is useful for learning the command format.

**Device list** (Systems → NMEA 2000 devices), from CANboat layouts:
- 60928 ISO Address Claim → NAME: manufacturer (CANboat MANUFACTURER_CODE), device class, function, instance. The same NAME re-claiming a new address replaces the old entry.
- 126996 Product Information (fast-packet, 134 B) → model, software, serial, LEN.
- 126993 Heartbeat → presence. A device is **offline** after 90 s with no frames from its address (heartbeats default to 60 s).
- Heard **passively**: the Pi sends no ISO Requests (listen-only), so model names appear when devices power up or another device (IPG100, DSM) requests them.
- A red top-bar chip ("IPG100 offline" / "N offline") appears on every screen and links to Systems. Offline devices stay listed until a backend restart.
- Verified on the Pi with `tools/bench_traffic.py --ipg-flaky` (synthetic IPG100 stops after 20 s → OFFLINE at ~106 s with the alert chip). Not yet seen with real devices.

## 12. Alert center (2026-11-20)

`alerts.py` evaluates rules every second. A condition must hold `for_s` to raise and be false for `clear_s` to clear. Severity follows the status palette (warning < serious < critical). Acknowledging only silences an alert; it stays active until its condition clears, and it re-alerts if it escalates. Events (raised, escalated, acked, cleared) are logged to `alert_log` in the history SQLite DB (in memory while demo is on).

| Rule | Severity | Debounce |
|---|---|---|
| Battery SoC ≤ 20 % / ≤ 10 % | warning / critical | 30 s raise, 60 s clear |
| Charging LiFePO4 with battery temp ≤ 2 °C | serious | 30 s |
| Fridge / freezer above [ui] limits | warning | **10 min** (ignores door openings) |
| N2K device offline (per device) | serious | already 90 s silence |
| CAN bus interface down (non-bench) | serious | 10 s |
| Cerbo MQTT disconnected (non-demo) | serious | 60 s |
| Rixens MCS7 silent | warning | 30 s |

UI: an **Alerts** page (nav badge with count and severity), Acknowledge / Acknowledge all (over the websocket; this changes VanPanel's own state, never a device), and a recent-events log. A top-bar chip appears on every screen. A pulsing red screen edge shows while an unacknowledged **critical** is active. A WebAudio chime plays for new serious/critical alerts (warnings only in day mode), and unacknowledged criticals repeat every 60 s. A sound toggle is on the page. The kiosk Chromium runs with `--autoplay-policy=no-user-gesture-required`. Thresholds are in `[alerts]`.
Verified on the Pi: the flaky-IPG100 bench scenario raises a serious alert; ack over the websocket → the chip turns neutral; the log records raised → acked. The chime wasn't heard (no audio check from here).

## 13. Starlink + Peplink MAX BR1 Pro 5G: integration design (research 2026-11-20, not built)

**Starlink (dish gRPC, local):** `192.168.100.1:9200`, plaintext gRPC `SpaceX.API.Device.Device/Handle`. Use **`starlink-grpc-core`** (sparky8512; v1.3.0, 2026-11-17; the library Home Assistant uses). It resolves protobufs by server reflection, so there's nothing to compile. grpcio ships cp314 aarch64 wheels. The API is sync → `asyncio.to_thread`.
- Read: state, uptime, sw/hw version, PoP latency, ping drop, down/up throughput, fraction obstructed / currently obstructed, azimuth/elevation, GPS sats, alerts (motors stuck, thermal throttle/shutdown, heating, mast not vertical, water detected, no ethernet, power-save idle…), power (history mean/min/max), and the obstruction map.
- **Not available:** location via gRPC (removed for most plans, May 2026) → use the Peplink GPS. The history buffer is now only 15 min.
- Reachability behind the Peplink: firmware ≥ 8.4.0 auto-detects Starlink and lets LAN clients reach 192.168.100.1 (forum/staff, unverified). Older firmware: add a WAN "Management IP" (e.g. 192.168.100.100). The dish may be unreachable when its link is down.
- Control exists (reboot, stow/unstow, sleep schedule) but is **out of scope**: never automatic, and only behind confirm + TxGate-style approval.
- Firmware drift risk (the May 2025 namespace change broke tools) → pin the library, treat missing fields as None.

**Peplink (local Router API, documented for fw 8.6.0):** `https://<router>/api/<func>`. Auth: create a **`api.read-only` client** once as admin (`auth.client`) → `auth.token.grant` → `?accessToken=`. The token stays server-side and admin creds never reach the browser.
- `status.wan.connection`: per-WAN name/type/status LED/priority/uptime/IP; cellular: carrier, RAT/band, RSRP/RSRQ/SINR/RSSI, active SIM; Wi-Fi-as-WAN SSID/strength.
- `status.wan.connection.allowance`: per-SIM data usage vs limit.
- `info.location`: **GPS** lat/lon/alt/speed/heading (fw 8.0.1+). An NMEA TCP stream on port 60660 is also reported (forum).
- `status.client`: LAN clients. Modem temperature: `status.cellularModule.temperature`.
- Control (WAN priority/enable, SIM switch, reboot) needs the `api` scope → out of scope for now.
- InControl2 cloud API exists but is delayed, rate-limited and fails exactly when the WAN is down → local preferred. No maintained PyPI library; HA has a HACS integration (weirded/ha-peplink-local) → write a small aiohttp client.
- **Privacy:** responses include ICCID/IMSI; don't log or display them.

**Planned UI:** a "Connectivity" card on Home (active WAN, Starlink state + obstruction %, cell carrier/band/RSRP/SINR, throughput, data used %), a Systems detail page with the obstruction map, and Peplink GPS feeding a map/weather tile and Victron/N2K fallbacks. **Planned alerts:** Starlink offline/obstructed > N min, thermal throttle/shutdown, motors stuck, mast not vertical; cellular data > 80 % / 95 % of allowance; WAN failover events.
**Polling:** dish status 5–10 s, history 5–10 min, obstruction map hourly; Peplink WAN 10–15 s, allowance/clients 5 min, location 30–60 s.
Sources: github.com/sparky8512/starlink-grpc-tools, pypi.org/project/starlink-grpc-core, home-assistant.io/integrations/starlink, download.peplink.com/resources/Peplink-Router-API-Documentation-for-Firmware-8.6.0.pdf, forum.peplink.com (threads 35125, 50598, 55216), github.com/weirded/ha-peplink-local.

### 13a. Built (2026-11-20)
- `adapters/starlink.py`: `starlink_grpc.status_data()` in a worker thread every 5 s (backoff to 60 s). Pure `normalize()` → `starlink.*`. Dish alerts carry severities (motors stuck / thermal shutdown / water / no ethernet = serious; throttle, mast, slow ethernet = warning; heating, power-save = informational). grpcio 1.85.3 cp314 aarch64 wheel installs fine on the Pi.
- `adapters/peplink.py`: aiohttp client with an `api.read-only` token (granted, cached for `expiresIn` − 2 min, re-granted once on 401/403). Polls WAN status every 15 s, location every ~60 s, allowance every ~5 min. **Field whitelist**: IMSI/ICCID/IMEI/APN creds never enter state (tested). The active WAN is the connected WAN with the lowest priority number.
- Secrets: `/etc/vanpanel/secrets.toml` (root:vanpanel 0640) with `peplink_client_id` / `peplink_client_secret`. It's created as a template by `install.sh`.
- Config: `[device.starlink]` / `[device.peplink]` with `enabled = false` until the van. Both are skipped while demo is on.
- Alerts: Starlink unreachable/offline (after `starlink_offline_s`, default 180 s), dish alerts; router API unreachable (60 s); cellular data ≥ 80 % warning, ≥ 95 % serious.
- UI: a **Network** page (Starlink speeds/latency/drop/uptime/obstruction ring/dish alerts; cellular bars, carrier, RSRP/SINR with quality labels, band/RAT/CA, active SIM, data-usage ring; WAN priority list with the in-use WAN; Peplink GPS location with locally computed sunrise/sunset/daylight; Starlink throughput chart 6 h/24 h/7 d). A top-bar chip shows the active link ("Starlink · 95 Mbps" / "5G · Verizon"). Starlink down/up/latency go into history.
- Tests: normalizers against the documented shapes, a fake router (grant, re-grant, bad secret), alert rules. **Not yet run against the real dish/router.**

**Creating the Peplink read-only client** (once, from any laptop on the van LAN; replace ROUTER and the password):
```
curl -k -c /tmp/pep.txt -H 'Content-Type: application/json' -d '{"username":"admin","password":"…"}' https://ROUTER/api/login
curl -k -b /tmp/pep.txt -H 'Content-Type: application/json' -d '{"action":"add","name":"VanPanel","scope":"api.read-only"}' https://ROUTER/api/auth.client
rm /tmp/pep.txt
```
Put the returned `clientId` / `clientSecret` into `/etc/vanpanel/secrets.toml`, set `[device.peplink] enabled = true` and `host = "ROUTER"`. To revoke: `{"action":"remove","clientId":"…"}` to the same endpoint.

## 14. Home ↔ van network and the MCS7 Local HTTP API (2026-11-20)

**Network (done, owner-approved):** at home, the home router routes the van LAN via the Peplink's WAN address (a DHCP reservation, a static route and DHCP option 121, with the Peplink WAN in IP Forwarding mode), so hosts on the home LAN can reach van devices while parked. Two open issues: the van LAN overlaps the Starlink dish's fixed management address, and the route should be restricted on the Peplink's inbound rules.

**MCS7 Local API** (Rixens Postman doc "Rixen MCS Local API": https://documenter.getpostman.com/view/55318949/2sBXwnsBuf). Unauthenticated HTTP on the control board (AP mode 10.10.10.10, or the infrastructure IP — here **<mcs7-ip>**):
- `GET /status.xml`: full snapshot. Poll every 2–5 s (never faster than 1/s). It **contains Wi-Fi secrets** (`settings/key`, `settings/ssid`, `infrastructure/infra_pass`, `infra_ssid`), so they must be stripped before state/logs.
- `GET /interface.cgi?act=N&val=V` commands (integers only): 1 setpoint, 2 fan 0–100/999 auto, 3 engine heat, 4 electric, 5 furnace, 6 hot water, 7 engine preheat, 8 fan state, 9 thermostat, 10 floor, 11 aux (toggles use val=1). `GET /buttons.cgi?act=7&id=1` reset faults, `act=13` prime fuel pump, `act=20` 30-min hot water. Commands are not authoritative: verify via status.xml.
- Key fields: `currenttemp`, `currenthumidity`, `settings/{setpoint,fanspeed,fanstate,heatsources(bitmask eng=1 elec=2 furn=4),furnacesrc/electricsrc/enginesrc/floorsrc/auxsrc (0 off,1 ready,2 active),cnstheat,cnstheattimerval,preheatsrc,thermenabled,glycol (0 off,1 warming,2 ready 122°F,3 max 158°F)}`, `heaterstate` (0 off, 10 cold start, 12 warm start, 13 cold blow, 15 flame stabilization, 20 running, 30/31 after-run, 40 pause, 200 error), `heater1/{battv,runtime(min),flametemp,inlettemp,outlettemp (÷100 °C),altitude,glowpin %,dosingpump Hz,burnermotor}`, `heater1-faults` (AF live fault + F1–F5 history).
- **Live unit (verified 2026-11-20, read-only GETs):** fw `HW1W6-1.208 2026_06_18` (HW1 board), heat 1.07. Temperatures ARE tenths °C as documented: `currenttemp=72` = 7.2 °C (45 °F; the owner confirms the van is ~46 °F), `setpoint=72` = 7.2 °C. (An earlier note here wrongly said °F; the MCS7's own UI JS divides by 10 and converts to °F only for display.) `fanspeed` can be the string "Auto" or "Off". Humidity is tenths (461 = 46.1 %). `battv` is tenths V. Faults are all 0.
- **Command encoding, from the MCS7 web UI JS:** act=1 val=round(°C×10), with its UI limits 1–33 °C / 35–90 °F. act=2 val 0–100 or 999 = auto. act=8/9 explicit 0/1. act=3/4/5 the UI sends the opposite of the current state (0/1), so set-vs-toggle is ambiguous → VanPanel reads fresh state, sends only if it differs, sends ONCE, verifies, and never auto-retries. The MCS7 remains the thermostat: VanPanel sends nothing autonomously, so a dead Pi changes nothing.
- → This HTTP API is a better primary Rixens source than the community CAN map (it adds cabin temp/humidity, faults, runtime, heater state machine and hot-water timer). Keep CAN as a fallback/cross-check.

### 14a. MCS7 HTTP adapter with control (built 2026-11-20, owner-approved)
- `adapters/rixens_http.py` polls `status.xml` every 5 s (≥ 1 s between any requests, one lock). Whitelisted parse, so Wi-Fi keys never reach state or logs (tested against a redacted live fixture `tests/fixtures/mcs7_status_hw1_1.208.xml`). Publishes `climate.rixens.*`, `climate.cabin.setpoint`, and (with `[climate] cabin_source = "rixens"`) the cabin temp/RH. The CAN Rixens adapter becomes fallback/inventory only.
- **Control scope (approved):** setpoint (1–33 °C), fan (0–100 / auto), furnace / electric / engine. Hard allowlist; hot water, preheat, floor, aux, thermostat, buttons.cgi, infra.cgi and Wi-Fi acts are unreachable from code (ask before adding). TxGate `rixens_http` is enabled only in the Pi's `/etc` config.
- **Command protocol:** fresh status read → send only if the state differs → send exactly once → poll up to 15 s to verify → never auto-retry (safe under both set and toggle semantics; tested with a fake MCS7 in both modes). Every command goes to `command_log` (SQLite; in memory while demo is on) and the journal.
- **Who can command:** loopback only (the kiosk). LAN/phone clients get "control only from the van touchscreen" (verified from the workstation). Controls are disabled when MCS7 data is > 15 s old or the gate is off. The top-bar chip shows "Heat control" vs "Read-only".
- UI: setpoint −/+ (debounced 1.5 s, one absolute value), fan Auto/−/+, and Furnace/Electric/Engine buttons with a confirm dialog. A toast shows "confirmed by the MCS7" or the failure reason.
- **Live test (non-combustion):** fan 40 % → confirmed in 5.1 s; back to Auto → confirmed in 6.0 s; Auto again → no-op (nothing sent). The fan was left on Auto as found. Setpoint and heat-source commands haven't been exercised live yet (left for the owner at the van).
- Alerts: MCS7 not responding (warning), heater state 200 (serious), live fault code AF (serious).
- Cabin temperature currently comes from the MCS7 (Ruuvi "Inside Temp" battery dead). Switch back with `[climate] cabin_source = "victron"`.

## 15. Network health monitor (2026-11-20, owner-approved)
`adapters/netmon.py`: every 60 s it pings each target with an unprivileged ICMP datagram socket (the Pi's `ping_group_range` allows all groups, so there are no capabilities in the hardened unit). It also opens and immediately closes a TCP connection to an optional service port: no login, no data. A target is DOWN after `fail_after` consecutive failures. "Service not answering" (ping OK, port dead) is distinguished from "no response".
- **IPG100 at <ipg100-ip>**: N2KServer ports 6543/6544 open; no web UI or SSH. Checked on TCP 6544 with fail_after 2 → a **serious** alert "IPG100 service not answering / not responding for N min. Power-cycle the IPG100 to recover it." The owner recovers it by power-cycling by hand.
- **MCS7 at <mcs7-ip>**: TCP 80, fail_after 3 → warning.
- Systems page cards show status, RTT and how long a device has been down. This complements the N2K device list (bus heartbeats), which only works once the Pi is on the N2K backbone.

## 16. Peplink access (2026-11-20)
The router is reachable over SSH with a dedicated key (firmware 8.6.1, ≥ 8.4.0 for LAN access to the Starlink dish). It has Starlink (Ethernet), cellular and Wi-Fi-as-WAN links; at home the Wi-Fi WAN carries the home link in IP Forwarding mode, and the Starlink WAN keeps NAT.

## 17. GPS from the Peplink (2026-11-20)
The Peplink SSH CLI has no GPS command and no API-client management (only device, LAN, cellular, SNMP, time and web-admin settings), and TCP 60660 is closed. The owner already had **GPS Forwarding** to the van-LAN broadcast `<van-lan-broadcast>`. Broadcasts don't cross into the home LAN, so the owner added a **unicast destination <pi-home-ip> UDP 10110**.
- `adapters/nmea_gps.py` binds UDP 10110 (also accepts broadcasts, so once the Pi is on the van LAN the existing broadcast works too), verifies NMEA checksums and parses RMC/GGA from any talker → `location.fix/lat/lon/alt_m/speed (km/h)/heading/sats/hdop/time`. The fix goes stale after 30 s.
- Verified live: a 3D fix with 12 satellites and HDOP 0.8. The Network page's Location card and the sunrise/sunset calculation use it.
- With `[gps] enabled = true`, demo mode no longer writes `location.*`.
- Also learned from the CLI config: the Peplink's LAN is 192.168.100.1/24, which overlaps the Starlink dish's fixed management address.
- The Router API read-only client is still not created (it needs the admin password); WAN/cellular/data-usage via the API remain pending.

## 18. Peplink Router API access (2026-11-20)
- `POST /api/auth.client` (create client) returns **401** on fw 8.6.1 even with a working admin session (`permission {"GET":1,"POST":1}`, cookie `bauth`, and GET status/auth.client both OK). The router is **not** InControl-managed, so the 8.6.1 API hardening (release note 37690) is the likely cause. Unresolved.
- Workaround built: `PeplinkClient` "user" mode logs in as the router's **read-only web-admin user** (System → Admin Security). It refuses any account granted POST and re-logs in on 401. Credentials go in `/etc/vanpanel/secrets.toml` as `peplink_ro_user` / `peplink_ro_password` via `tools/peplink_install_readonly_user.sh`.
- **Pending (owner, later):** create the read-only user, run the installer, then set `[device.peplink] enabled = true`, `host = "<peplink-wan-ip>"` in the Pi's /etc config. GPS already works independently via NMEA UDP (§17).

## 19. Victron Cerbo GX connected (2026-11-20)
- Cerbo at **<cerbo-ip>** (van LAN). Modbus-TCP 502 is on; MQTT was off. Venus OS ≥ 3.80 uses **MQTT pairing**: `tools/victron_pair.py` (run on the Pi as root) retries `POST https://<gx>/auth/generate-token/` (form role/device_id, as victron-mqtt, Apache-2.0, does) while the owner enables pairing mode (120 s). It stores `victron_mqtt_user` / `victron_mqtt_password` in secrets.toml (0640) and never prints the password. Token user: `token/<role>/<device-id>`. After pairing, MQTT is on **8883/TLS** (1883 stays closed).
- `VictronAdapter` gained username/password and TLS (self-signed GX cert, verification off on the LAN). Pi config: host <cerbo-ip>, port 8883, `tls = true`, `enabled = true`. With `enabled = true` the real Cerbo runs even in demo mode, and the demo yields `victron.*` plus the Ruuvi climate roles.
- Verified live: portal `<portal-id>`, SoC 85.9 %, 13.26 V, −2.0 A, battery 11.0 °C, DC loads 26 W, PV 0 W (night), fresh tank 70 %. Temperature services seen: "BUS" (battery) and "WTR" (generic). **None of the four Ruuvi tags appear on the Cerbo** (dead battery / not paired / Bluetooth sensors disabled?), so outside/fridge/freezer show "—".
- Still read-only toward Victron (only the R/…/keepalive read request is published).

## 20. Demo off; real data only (2026-11-20)
- The Pi's `/etc` config now has `[device.demo] enabled = false`. Every value on screen is real: Cerbo (MQTT/TLS), MCS7 (HTTP), Peplink GPS (NMEA), netmon. Starlink / Peplink router API / switch banks show "Not enabled" or "No switch banks" until connected. History now persists in `/var/lib/vanpanel/history.sqlite3`.
- Incident: the first restart failed with "attempt to write a readonly database". An earlier `sudo sqlite3 …` debugging command had created an empty **root-owned** history.sqlite3. Fixed by deleting it. Hardening: `History` now falls back to in-memory history with a warning alert ("Chart history not being saved") instead of crashing the console.
- The Ruuvi tags (Inside/Outside/Fridge/Freezer) all have dead batteries. They're already mapped by name and will appear once replaced; then set `[climate] cabin_source = "victron"` to move the cabin reading back from the MCS7.

## 21. Victron control over MQTT (2026-11-20, owner-approved: MultiPlus mode, shore limit, MPPT on/off)
- Discovered (read-only): **MultiPlus 12/3000/120-50 120V** = `vebus/276` (Mode 4 = off, **ModeIsAdjustable = 1**), **SmartSolar MPPT 100/50** = `solarcharger/277`, **Lynx Shunt 1000A VE.Can** = `battery/0`, **EPOCH 460** BMS = `battery/512`, plus Shelly, BLE, digital-input, GPS and tank services. Venus firmware 3.85-ish.
- Writes use `W/<portal>/<path>` `{"value": X}`. Allowlist in `VictronAdapter.plan()`:
  - `inverter_mode`: vebus Mode ∈ {1 charger only, 2 inverter only, 3 on, 4 off}; requires `ModeIsAdjustable == 1`.
  - `shore_limit`: `Ac/In/1/CurrentLimit` 3–50 A (0.1 A); requires `CurrentLimitIsAdjustable == 1` (false while the MultiPlus is off).
  - `mppt_mode`: solarcharger Mode ∈ {1 on, 4 off}.
- Protocol as with the MCS7: TxGate "victron" (tx_enabled only in the Pi's /etc) → fresh read via `R/<portal>/<path>` → no-op if already equal → publish once → wait for the N/ value to match (12 s) → never retry. Every command goes to `command_log` (SQLite) and the journal. Touchscreen (loopback) only.
- UI: an Energy page "Inverter & solar" card. MultiPlus mode buttons and MPPT on/off have confirm dialogs explaining the effect; shore limit −/+ is debounced 1.5 s. The top chip shows "Control: heat + power".
- Live test: MPPT off → confirmed 0.4 s, on → confirmed 0.2 s, repeat → no-op. Left on (as found). **The MultiPlus mode has not been changed** (left Off for the owner).

## 22. NMEA 2000 via the Cerbo GX (socketcand), 2026-11-20
- **IPG100:** its N2KServer protocol (6543/6544) is closed and encrypted (no TLS cert on connect; Maretron-only, password + license key). No public client or raw feed exists, so it isn't used as a data source.
- **The Cerbo's VE.Can `can0` IS on the N2K backbone.** Its device list showed **IPG100** (Maretron, addr 40, fw 4.2.0.1), **CLMD16** (Carling, addr 0, device instance 32), and **CKM12 ×2** (Carling, addr 100/101, instance 50), plus the Victron devices. Maretron's digital switching is Carling hardware.
- The Cerbo runs Venus OS **Large v3.85** with Signal K 2.27 enabled (security off) but **no data connections**, and Node-RED enabled.
- Owner chose **socketcand**: enabled via MQTT write `settings/0/Settings/Services/Socketcand` = 1 (verified 0→1). TCP 29536.
- VanPanel bus `[bus.n2k_cerbo] interface = "socketcand"`, host <cerbo-ip>, channel can0. `device.n2k` moved to it. About 46–58 frames/s, 43 IDs, 0 errors. **VanPanel never transmits on it** (no N2K send path; TxGate n2k off). socketcand has no auth: anything on the LAN could inject frames (owner declined a Peplink lockdown for now).
- Seen: 127501 banks **32** (28 ch, CLMD16; **ch 11 ON**) and **50** (12 ch, CKM12 keypad buttons). Also 127500 Load Controller (Carling/N2K digital switching), 127506/127508/127751 DC, 127507/127509/127750 charger/inverter/converter, 65013–65016 AC, 61184/65xxx proprietary. No 126208 yet (press a keypad button to capture it).
- Device names: `VictronAdapter` mirrors the Cerbo's `vecan/+/Devices/+` (Nad → ModelName/Manufacturer/Serial/Firmware) into `n2k.names_from_cerbo`, and the N2K device table uses it. All devices are now named.
- Captures: the socketcand bus is captured like the others. Retention was added: `capture_keep_days` 14, `capture_max_gb` 20, hourly pruning.
- Proprietary PGN ranges (61184, 65280–65535, 126720, 130816–131071) are labelled "Manufacturer proprietary" (canboat's first-variant names were misleading).

## 23. EPOCH battery bank / BMS (2026-11-20)
- The Cerbo's BMS-Can (`can1`, profile 3, 500k) carries **one** CAN-BMS: Victron CAN-BMS frames 0x351/355/356/35A/35E("BB24160A")/35F/372–377, cell IDs all "P00" (pack 0). There are also extended 0x19FDF146/0x19FE8F46… frames from address 0x46, not decoded.
- Cerbo `battery/512` "Epoch 460": **NrOfModulesOnline 1**, InstalledCapacity 460 Ah, limits 14.6 V / 230 A charge, 300 A discharge, 10.8 V low; no alarms. The MultiPlus is configured for 1840 Ah (4 × 460).
- **Diagnosis:** the BMS current is −3.2 A while the Lynx shows the bank at −12.7 A (≈ ¼), so all 4 batteries are in parallel and sharing load, but only one reports. That points to the EPOCH **comms daisy-chain / master-slave DIP switches** (owner says they're meant to be daisy-chained). The BMS SoC (100 %) is unreliable; the **Lynx Shunt (battery/0) is the bank's SoC source** (VanPanel uses system values = Lynx).
- Built: `VictronAdapter` collects all battery services (whitelisted fields + Alarms/*) into `victron.batteries` (1/s), with kind bms vs monitor. Alert `r_bms`: serious when modules online < `[alerts] bms_expected_modules` (Pi: 4), warning/serious per BMS alarm (1/2), serious when modules block charge/discharge. **Battery overlay** (tap the Home battery ring): Lynx bank figures next to BMS modules/cells/spread/temps/limits/alarms with a fix-it note.

## 24. MCS7 burner diagnostics (2026-11-20)
The MCS7 web UI's diagnostics line "Fan-Glow-Fuel" = `burnermotor` rpm, `glowpin ÷ 10` W, `dosingpump ÷ 10` Hz; inlet/outlet/flame are `÷ 100` °C. (VanPanel previously exposed glow/fuel unscaled; fixed and tested.) The Climate page's Rixens card shows Fan / Glow / Fuel / Inlet / Outlet / Flame. Inlet, outlet, flame and fan rpm are tracked in history. Live while running: fan 9,260 rpm, glow 0 W, fuel 5.5 Hz, inlet 41.5 °C, outlet 52.3 °C, flame 223.5 °C.

## 25. Fuel flow from the dosing pump (2026-11-20)
- Sources (primary): Espar Hydronic D5 spec sheet: **5.0 kW → 0.62 l/h (0.16 gal/h) high, 2.4 kW → 0.27 l/h (0.07 gal/h) low**. Eberspächer Hydronic D5 W SC workshop manual p. 41, "Measuring the fuel quantity": 90 s pump test, **nominal 9.0 cm³** (8.5–9.5). The test frequency isn't stated. The third-party "0.02 ml/pulse" figure is generic and unsourced, so it isn't used.
- Calibration: during the 2026-11-20 cold-start warm-up (full output) the pump held **5.5 Hz** with the fan pegged at ~9,250 rpm → **0.0313 ml/stroke** = 0.62 l/h ÷ (5.5 × 3600). This implies ~2.4 Hz at the low rating and ~3.2 Hz in the 90 s test. **It assumes 5.5 Hz is full output**: verify that the pump never exceeds 5.5 Hz while running. Ideal re-calibration: measure the fuel actually used over a long run. Setting: `[device.rixens_http] fuel_ml_per_pulse`.
- VanPanel: `climate.rixens.fuel_lph` / `fuel_gph` live; `fuel_today_l` / `fuel_today_gal` are integrated between polls (gaps > 120 s skipped), persisted in SQLite `counters`, and reset at local midnight. Shown on the Climate card (Fuel tile gal/h, "Fuel today"); fuel_gph is in history. Units are gal/**h** (full output is only 0.0027 gal/min).

## 26. Auto fan speed (2026-11-20)
In Auto the MCS7 drives the cabin fan with a PID loop. `<autofan>` fanset 0 = 999 means PID is enabled, and fansets 1–3 are P/I/D = 50/25/0, as the MCS7 web UI reads them. The live output is `heater1/pidspeed`, on the same 0–100 % scale as manual fan speeds. It was steady at 31 % with the cabin 0.6 °C under setpoint, then dropped to 25 % as the cabin approached setpoint. VanPanel publishes it as `climate.rixens.fan_auto_pct` only while the fan is in Auto. The Climate fan control shows "25 %" with the label "fan · auto".

## 27. Home heat buttons; hot water + pre-heat control (2026-11-20)
- Home has a 3×2 card of round buttons: Burner, Engine heat, Electric, Hot water, Pre-heat and Espar. They are **one touch with no confirmation** (owner's choice). Each tap runs the TxGate path: fresh read → send only if different → send once → verify → never retry. Every command is logged.
- **Approved by the owner:** hot water (constant heat, act 6) and engine pre-heat (act 7). The MCS7 web UI sends both as `interface.cgi?act=N&val=<new 0/1>` (`constantHeatDisable` / `preheatFunctionToggle`). They are verified against `settings/cnstheat` and `settings/preheatsrc`. Pre-heat is refused if `preheatenable` = 0.
- **Still not allowed:** floor, aux, thermostat, override, buttons.cgi (including the act=20 hot-water *timer* setting), infra and Wi-Fi.
- Espar is a placeholder showing "not connected" until its CAN control exists.

## 28. Screen off / backlight (2026-11-20)
- The Waveshare panel (EDID "WSD060", HDMI-A-1) has no `/sys/class/backlight` device and **does not answer DDC/CI** (`ddcutil detect`: "I2C slave address x37 is unresponsive"). Powering the HDMI output down is the only software way to turn it off.
- Cage can't do that. `wlr-randr --off` removes the output, Chromium then crashes (exit 133), and cage has no output-power protocol. **The kiosk now runs on sway 1.12.** See `deploy/kiosk/sway.conf` and `chromium.sh`; Chromium exiting ends sway, and systemd restarts the kiosk.
- `deploy/kiosk/screen_helper.py` runs inside the sway session and listens on 127.0.0.1:8091: `GET /screen`, `POST /screen/on`, `POST /screen/off`. It addresses outputs by name, because `output * power on` doesn't wake a sleeping output. **Sway 1.12 quirk on vc4:** the first `power on` after `power off` reports success but leaves the connector off. The helper checks `/sys/class/drm/*/dpms` and retries; wake takes about 0.9 s. The helper exits when its sway goes away, and the unit pkills stragglers, because sway's children live in the login-session scope.
- In the console, the top-bar monitor button (kiosk only) turns the screen off, and it also turns off automatically after **10 min with no touch** (owner's choice). A black wake shield then swallows the first touch, so waking can't press a button underneath. Tested: 3/3 off/on cycles, with Chromium staying up.
- **Wake on touch (fix after the owner's test):** while the output is powered off, sway doesn't deliver touches, so the page never saw the waking tap. The helper now reads the touch panel's evdev node (`/dev/input/event0`, WSD101; read-only and not grabbed) and powers on at the first touch while the screen is off. The page polls `GET /screen` and drops its shield about 0.7 s after the screen is back. Tested end to end: injected touch-down → on in about 0.5 s → shield dropped.
- **A critical alert wakes the screen (2026-11-20):** when an unacknowledged critical alert appears (new, re-raised or escalated), the kiosk powers the screen on and drops the shield. Warnings and serious alerts don't wake it. The 10-minute auto-off still applies while alerts are unacknowledged (owner's choice); only a *new* critical wakes the screen again. Tested with injected alerts and a fake helper: warning → no wake; critical → wake + shield drop; same critical again → no repeat.

## 29. Settings (2026-11-20, modelled on the AetherSDR Maestro kiosk)
- **Privileges:** the kiosk and backend never get sudo. The only root piece is `deploy/admin/vanpanel_admin.py`. It is installed root-owned to `/usr/local/lib/vanpanel/` (outside the rsync'd tree), uses only the standard library, and runs with `python3 -I`. It is socket-activated on `/run/vanpanel-admin.sock` (root:vanpanel 0660) and checks the caller with SO_PEERCRED (root or `vanpanel` only). It runs a fixed verb list with validated arguments, as argv lists only.
- **Backend:** `GET /api/settings` reports status. `POST /api/settings/action` accepts changes **from loopback only** (the touchscreen), with a second allowlist, and never logs arguments. VanPanel's own preferences (screen auto-off minutes) live in `/var/lib/vanpanel/settings.json` and are published as `settings.*`.
- **Device:** rename (on-screen keyboard), time zone (automatic only: GPS → Open-Meteo `timezone=auto`; two agreeing lookups required; offline keeps the current zone; then `tzset()`), screen auto-off (5/10/15/30/60 min or Never), °F/°C, day/night, About, and restart display / reboot / shut down with confirmation.
  - A rename used to make the kiosk loop: Chromium's SingletonLock names the host. `chromium.sh` now clears the stale locks.
- **Network:** iwd joins Wi-Fi (`EnableNetworkConfiguration=false`) and networkd addresses `Type=wlan` (metric 600, `RequiredForOnline=no`). Passphrases are turned into a PSK (PBKDF2) inside the helper and written to a root-only iwd `.psk` file, which is removed if the join fails. They never reach argv, logs or State.
- **Tailscale:** install (`pacman -S --needed`, no `-Sy`) and enable tailscaled. Sign-in runs `tailscale up --hostname=… --accept-routes=false --accept-dns=false` in the background and shows the AuthURL as a QR code (vendored qrcode-generator 1.4.4). Status and sign-out are on the same tab. Never front port 8080 with `tailscale serve`: remote clients would then look like loopback.
- **Updates:** the check syncs a throwaway copy of the package lists, like checkupdates (never the system's). The install runs `vanpanel-update.service` (`vanpanel_update.sh`: keyrings, then `-Su`, with its log and state in `/run/vanpanel-update`). It rebuilds the venv if a Python upgrade broke it. **Held in IgnorePkg (owner's choice):** `linux-rpi`, `raspberrypi-bootloader` and `firmware-raspberrypi`, because the custom kernel boots with linux-rpi's `/boot` dtbs and overlays (including mcp2515). Backup: `/etc/pacman.conf.vanpanel-orig`.

## 30. Remote control over Tailscale (2026-11-20, owner's choice)
- **Who can control:** the touchscreen (loopback), and devices owned by the **same Tailscale user** as this console that are **untagged**. A shared-in user, a tagged node or any LAN browser is view-only.
- **How a peer is identified:** the backend's `RemoteAuth` asks the root helper (`tailscale_whois`, internal only) who owns the source address, and compares the numeric user ID with this node's. The connection must also have arrived on the console's own Tailscale IP. Results are cached for 60 s.
- **Scope (owner chose "Everything"):** the same rights as the touchscreen, including Tailscale sign-out, Wi-Fi off, shut down and reboot. Any of those can cut off remote access until someone is at the van.
- **Kiosk only:** screen off, auto-off and the wake shield.
- **Page:** each WebSocket gets a `hello` with `control` and `who`. The page gates its buttons on that, not on its own hostname.
- **Logging:** the command log records the actor (for example `tailscale:<login>@<device>`). Verified: a no-op setpoint over Tailscale returned "already set" and was logged with the Tailscale identity. The LAN got 403 / view-only.
- **Note:** every device in the owner's tailnet under that login gets control. That can include home-automation servers and other always-on nodes, not just phones and laptops. To narrow it, tag those devices in the Tailscale admin console; tagged nodes are excluded.
- **Never** publish port 8080 through `tailscale serve` or `tailscale funnel`.

## 31. AetherRV boot splash (2026-11-20, owner-approved /boot changes)
- **Hero:** `deploy/boot/aetherrv-hero.html` is rendered by `tools/render_hero.sh` to `deploy/boot/plymouth/aetherrv/splash.png` (1920×1200), using the aethersdr.com colour tokens.
- **Plymouth:** the `aetherrv` theme (script module: scaled and centred on `#060b13`). There's no initramfs on this Pi, so Plymouth starts from the root fs at about 1.4 s.
- **Hand-off:** the kiosk unit `Conflicts=plymouth-quit.service`. It runs `plymouth quit --retain-splash` after the backend is up, so the splash stays on screen until sway draws.
- **Boot command line:** `tools/install_boot_splash.sh` (one time, idempotent, makes `*.pre-aetherrv` backups) edits the cmdline the firmware actually reads. **This Pi sets `os_prefix=7.3.5/`, so the firmware reads `/boot/7.3.5/cmdline.txt`**, along with the custom kernel's dtbs and overlays; the first attempt edited `/boot/cmdline.txt` and had no effect. Changes:
  - `console=tty1` → `tty3`
  - added `quiet splash loglevel=3 vt.global_cursor_default=0 systemd.show_status=false udev.log_level=3 plymouth.ignore-serial-consoles logo.nologo`
  - the serial console stays on `ttyAMA10`
  - `config.txt` gets `disable_splash=1`
- **Revert:** restore `/boot/7.3.5/cmdline.txt.pre-aetherrv` and `/boot/config.txt.pre-aetherrv`.
- **Correction to §29:** the custom kernel's dtbs and overlays live in `/boot/7.3.5/`, not in linux-rpi's files, so holding linux-rpi was more cautious than necessary. The hold stays.

## 32. Project renamed VanPanel → AetherRV (2026-11-20)
Sections 1–31 use the old names. Mapping:

| Was | Now |
|---|---|
| repo `~/Projects/vanpanel` | `~/Projects/AetherRV` |
| Python package `vanpanel` (entry point `vanpanel`) | `aetherrv` |
| `/opt/vanpanel` | `/opt/aetherrv` (the old tree was kept as `/opt/vanpanel.pre-aetherrv`) |
| `/etc/vanpanel/vanpanel.toml`, `secrets.toml` | `/etc/aetherrv/aetherrv.toml`, `secrets.toml` |
| `/var/lib/vanpanel{,-kiosk,-update}` | `/var/lib/aetherrv{,-kiosk,-update}` |
| units `vanpanel`, `vanpanel-kiosk`, `vanpanel-admin.{socket,service}`, `vanpanel-update` | `aetherrv`, `aetherrv-kiosk`, `aetherrv-admin.*`, `aetherrv-update` |
| `/run/vanpanel-admin.sock`, `/run/vanpanel-update` | `/run/aetherrv-admin.sock`, `/run/aetherrv-update` |
| `/usr/local/lib/vanpanel/*`, `/usr/share/vanpanel/cursors` | `/usr/local/lib/aetherrv/*`, `/usr/share/aetherrv/cursors` |
| service user/group `vanpanel` (963) | `aetherrv`, same UID/GID 963, so ownership carried over |
| PAM `vanpanel-kiosk` | `aetherrv-kiosk` |

- **Kept:** the Cerbo MQTT pairing token `token/<role>/<device-id>`, because it's a pairing identity; renaming it would mean re-pairing. Also the page's localStorage keys (`vp.*`, internal).
- **Migration:** `deploy/migrate_from_vanpanel.sh` runs first from `install.sh`. It's guarded and is a no-op on a migrated system. Backup: `/root/vanpanel-pre-aetherrv-20261120-230221/`.
- **Verified:** history from before the cut-over, the live config (Victron and controls enabled), prefs, fuel today, Tailscale owner control, LAN view-only, and a cold boot (12.2 s, no failed units, both splashes).


## 33. Wakespeed WS500 alternator over NMEA 2000; two reconnect bugs (2026-11-21)

- **WS500 on the bus.** It claims an N2K address (128 in this van) as "Dragonfly Energy WS500". It sends 127506 (DC type 1 = Alternator, instance 49), 127507 (charger state: Bulk/Absorption/Float…), 127508 twice (instance 49 = voltage + temperature with current N/A; **instance 0 = voltage + current**), 127510 and 127750. The Cerbo does not treat it as an alternator: `Dc/Alternator/Power` stays empty and `Dc/System/Power` goes about 1.8 kW **negative** while the engine charges, because the charge current is booked as negative load.
- **What AetherRV does (read-only).** The N2K adapter finds the regulator from 127506 DC type = alternator and publishes `alternator.{voltage,current,power,state,temp,online}`. Current comes from whichever of its 127508 instances carries one. `energy.py` derives `energy.alternator.power` (Cerbo if it has one, otherwise N2K) and `energy.dc.load`. While the regulator stands in for the alternator, DC load = (battery-shunt current − regulator current) × battery voltage, at the owner's direction: the shunt−regulator gap is DC load (about 85 W with the engine and MCS7 engine-heat running, against 27 W with everything off before the drive). Shown as unknown if any input is missing. The UI uses the `energy.*` keys, and the flow label shows the charge stage.
- **Open question: shunt location.** The WS500 current tracked the Lynx Shunt about 6.5 A (≈ 4.5 %) **lower**. Alternator output would have to read higher than battery current, so the WS500 shunt is probably at the battery, or calibrated differently. Until that is confirmed, alternator power = WS500 V × A. If it is battery current, that power is off by (DC loads − solar).
- **Temperature.** The WS500 instance-49 temperature reads about −17 °C with the engine running, so it is probably not fitted or not configured. It is published but not shown.
- **Fix: Victron MQTT reconnect.** Data topics were subscribed only the first time the portal ID was seen. After any reconnect (clean session) the console showed "connected" with no data. It now re-subscribes on every connect.
- **Fix: watchdog crash loop.** A `socketcand` open to an unreachable Cerbo blocks for about 10 s on the event loop, which tripped `WatchdogSec=10` and restarted the backend every 26 s while the van LAN was down. The open now runs in a worker thread.
- **Peplink LAN moved to .254 (owner, 2026-11-21).** This freed 192.168.100.1 for the Starlink dish. Afterwards the Cerbo (MQTT and socketcand) and the MCS7 worked. The IPG100 and MCS-Panel stayed on the LAN but did not answer, which points to the old .1 gateway (static, or lease not yet renewed). The dish at 192.168.100.1:9200 still timed out from the Pi: the Peplink keeps 192.168.100.0/24 as its connected LAN, and Pi traffic arrives on a different WAN than Starlink. The Starlink adapter stays off.

## 34. Van LAN renumbered to <van-lan>.0/24; Pi moved into the van (2026-11-21)

- **Why.** Starlink's dish is fixed at 192.168.100.1. That address sat inside the van LAN, so the Peplink kept it on its LAN side. Moving the Peplink's own LAN IP to .254 didn't help: from the Pi in the van, .1 gave no ARP reply, and a /32 route via .254 also failed.
  - A side effect of the old setup: the Peplink hands out NTP server .1, so systemd-networkd (RoutesToNTP) put an on-link route to it.
- **Change (owner, Peplink UI).** The LAN is now <van-lan>.1/24 (a new /24 off 192.168.100.x). New addresses:
  - AetherPIII end0: DHCP reservation at <van-lan>.110.
  - Cerbo .6, MCS7 .111, oneax .10, IC-705 .100.
  - GPS Forwarding now unicasts to the Pi UDP 10110. Verified: 12 satellites, fix.
- **RB5009 (home).** Route `<van-lan>.0/24 via <peplink-wan>` added. DHCP option 121 `van-route` now carries the new /24 + the old /24 + default.
  - Backup: deploy/network-backups/rb5009-export-pre-110-route-20261121.rsc (gitignored).
  - The RB5009 resets SSH from the van subnet, so manage it from the home LAN. Verified from home: the van AP answers.
- **Pi config.** /etc/aetherrv/aetherrv.toml hosts changed to the new subnet (Cerbo ×2, MCS7 ×2, IPG100). Backup: aetherrv.toml.pre-110-20261121.
- **DHCP leases.** The old leases had to be cleared by power-cycling or renewing; until then, devices had no gateway and the Pi dropped off Tailscale.
- **socketcand after the Cerbo reboot.** The setting was still 1, but nothing was listening on 29536. It probably started before DHCP. Toggling MQTT `Settings/Services/Socketcand` 1→0→1 (owner OK) brought it back; N2K is at ~57 fps.
- **Starlink dish reachable (2026-11-21).** On fw 8.6.1, WAN 1 had no separate Starlink toggle. The **Management IP Address** field appears only after "Click here for other DHCP settings" in the WAN 1 dialog; the owner set it to 192.168.100.100/24.
  - NAT over Starlink is unchanged: the WAN keeps its DHCP CGNAT address and gateway.
  - The Pi then reached 192.168.100.1 (ping, :80, :9200 gRPC). `[device.starlink]` is enabled (backup aetherrv.toml.pre-starlink-20261121).
  - Live: CONNECTED, ~37 ms, 1.35 % obstructed, 10 GPS sats. The dish reports update_state REBOOT_REQUIRED (a pending dish firmware update).
  - IPv6 was considered and rejected: the dish gRPC is IPv4 192.168.100.1 only.
- **Still open.**
  - The IPG100 is not on the new subnet after its power-cycle (static .100.5 on the device?). The owner will check later.
  - The old 192.168.100.0/24 route on the home router is to be removed from the home LAN (owner approved).

## 35. Starlink obstruction sky map (2026-11-21)

- The dish's `get_obstruction_map` returns a 123×123 grid of SNR quality: −1 = no data, 0 = obstructed … 1 = clear. With it come `min_elevation_deg` 10, `max_theta_deg` 80 (the grid's inscribed circle is 80° from zenith) and `map_reference_frame`.
- **Frame.** This dish reports 2 = FRAME_UT (mobile/roam plans): the map is in the dish's own frame, with its bottom edge toward the dish's boresight (`direction_azimuth`). FRAME_EARTH (1, fixed plans) has true north up.
  - The UI rotates the image by azimuth − 180° so the view is north-up, and marks the facing direction with a small amber triangle.
  - Source: secondary (research write-ups). **The owner verified it against the Starlink app on 2026-11-21: it matches.**
- **Backend.** The Starlink adapter reads the map once a minute (read-only; `reset_obstruction_map` is never called). It packs one byte per cell (255 = no data, else round(snr × 254)) as base64 into the `starlink.obstruction_map` state key, about 20 KB. The key isn't recorded in history.
- **UI.** The Network → Starlink card draws the map once per new map into a cached canvas image. It's shown as a dark sky dome: clear cells blue, obstructed red, dashed rings at 60° and 30° elevation, N/E/S/W labels and the % obstructed caption. The old % ring is the fallback when there is no map.
- Also fixed: daylight could show "8 h 60 m" (rounding the minutes separately from the hours).

## 36. Peplink router API enabled with the read-only web user (2026-11-21)

- Creating an API client (`auth.client`) still returns 401 from the LAN side too, so the WAN path wasn't the cause.
- The owner created the read-only web user under System → Admin Security. Gotcha: Chrome autofills the "New Admin Password" boxes on that page; clear them before Save.
- `tools/peplink_install_readonly_user.sh` confirmed `{'GET': 1, 'POST': 0}` and that `info.location` returns 200, then installed the credentials in the Pi's secrets.toml.
- `[device.peplink]` is enabled (host = <peplink-lan>). The Network view's WAN priority and Cellular cards are now live. The active WAN is the home Wi-Fi WAN at P1; Starlink is P2 standby; Cellular is P3 standby. The field whitelist holds: no SIM identifiers in state.

## 37. Starlink "Install update" control (2026-11-21, owner-approved)

- **The command.** It's the first Starlink write, and the only one allowed: `install_update`. It reboots the dish, but only while the dish reports `software_update_state == REBOOT_REQUIRED` (a downloaded update waiting for a restart). It is not a general reboot.
- **Safety path**, same as the other controls:
  - TxGate device `starlink` (`tx_enabled`); controls only from the touchscreen or the owner's Tailscale devices.
  - A fresh status read on its own gRPC channel.
  - The reboot is sent exactly once on another brand-new channel. starlink_grpc's `call_with_channel` retries once on a reused channel, so the read and the send never share one, and the poller's channel is never used.
  - Never retried. A send error is verified, not resent.
  - Verified by the dish going offline or its uptime resetting within 90 s. Results go to `command_log`.
  - Dish alerts are quiet for 10 min after an accepted request (`starlink.restart_requested`).
- **UI.** An "Install update" button on the Starlink card, shown only when an update is pending and control is enabled. It asks for confirmation, warning that the Starlink internet and remote access drop for a few minutes.
- **Tests:** blocked without tx, other commands and no pending update rejected, exactly one send, a send error not retried, unverified when no restart, alerts quieted.
- **Pi:** `[device.starlink] tx_enabled = true` (owner approved; backup aetherrv.toml.pre-starlink-tx-20261121). Nothing was sent during development; the owner presses the button.

- **2026-11-21:** the outside-air Ruuvi reappeared after a battery swap. On the Cerbo it is a generic temperature service named "OAT" (with humidity), and the Pi config now uses `outside_temp = "OAT"`. The other three Ruuvis are still missing.
- **2026-11-21:** the inside Ruuvi is back as "IAT" (instance 20, with humidity). Pi config: `cabin_temp = "IAT"`, `[climate] cabin_source = "victron"` (it had been on the MCS7 sensor while the Ruuvi was dead). The two read about 6 °C apart: IAT about 21 °C, MCS7 sensor about 15.6 °C.
- **2026-11-21, cabin = average (owner):** the Ruuvi (IAT) is at the front of the van and the MCS7 sensor is at the back by the bed, so the cabin reading is now their mean (`[climate] cabin_source = "average"`, new module `climate.py`).
  - Humidity is averaged too. A sensor drops out when stale: the Ruuvi after 15 min without a value (the Cerbo only republishes on change) or with the Cerbo disconnected; the MCS7 60 s after its last poll. The other sensor is then used alone.
  - The Climate dial shows "average of front X° · back Y°".
  - First reading: front 20.0 °C, back 15.5 °C, so the cabin shows 17.7 °C (64 °F). Display only: the MCS7 still regulates on its own sensor.
- **2026-11-21, freezer Ruuvi back as "FRZ"** (instance 22; the Cerbo types it as "fridge", no humidity). Pi config: `freezer_temp = "FRZ"`.
  - **Bug fixed:** a configured sensor name that is missing used to fall back to the first sensor of the same type, so FRZ showed as the *fridge*. Configured names are now strict, and a missing one shows "—".
  - The fridge tag (still "Fridge Temp" in the config) is not on the Cerbo yet.
- **2026-11-21, fridge Ruuvi back as "FRG"** (instance 23, typed "fridge"). Pi config: `fridge_temp = "FRG"`. All four Ruuvis now report: IAT, OAT, FRZ, FRG.
- **2026-11-21, fridge/freezer off switch (owner design).** The fridge and freezer run only on trips.
  - A warm reading still raises "Fridge/Freezer warm" (after 10 min). The alert card gets a "Fridge is off" / "Freezer is off" button (with a confirmation), which sets prefs `fridge_off` / `freezer_off`.
  - That silences the rule, and the card shows "off · alerts resume when cold". `ColdRearm` clears it automatically once the unit is at or below its limit.
  - The console never infers "off" by itself (owner: no guessing).
- **2026-11-21, Network chart follows the active WAN.** The Peplink `status.traffic` returns live per-WAN bandwidth (kbps, "overall" down/up). Each 15 s WAN poll records the active WAN's throughput as `wan.down_mbps` / `wan.up_mbps`: one continuous history series, whichever WAN carries the traffic.
  - The chart title follows the active WAN live (e.g. "WWAN on 5 GHz throughput").
  - Without the Peplink API it falls back to the Starlink dish series.

## 38. Waveshare 2-CH CAN FD HAT installed (2026-11-21)

- **Owner-approved boot change:** `/boot/config.txt` gets the wiki's stacking lines after the CAN HAT+ block: `dtoverlay=mcp251xfd,spi0-0,interrupt=25` and `dtoverlay=mcp251xfd,spi1-0,interrupt=24`. The FD HAT stays in its default "A" (dual-SPI) mode. Backup: `/boot/config.txt.pre-canfd.bak`.
- **No pin conflicts:** the HAT+ uses SPI1 CE1/CE2 with INT 22/13; the FD HAT uses SPI0 CE0 with INT 25 and SPI1 CE0 with INT 24.
- **Stable names:** `10-fd0.link` (Path platform-1f00050000.spi-cs-0) and `10-fd1.link` (platform-1f00054000.spi-cs-0). `30-fd*.network` sets ListenOnly=yes at a 250k classic placeholder; FD data rate is not enabled.
- **Live (owner confirmed VIO = 3.3V, rebooted 2026-11-21 14:50):** both MCP2518FDs initialized (40 MHz osc, CRC/ECC on) and were renamed to `fd0` (spi0.0) and `fd1` (spi1.0). `hat0`/`hat1` are unchanged. All four ports are UP LISTEN-ONLY ERROR-ACTIVE at 250k. After the reboot, the Cerbo, N2K, MCS7, Starlink, Peplink and GPS were all back. Nothing is wired to fd0/fd1 yet.

## 39. CLMD16 switching: baseline, firmware 1.0.7.1, and the logic moved into AetherRV (2026-11-21)

> **Superseded 2026-11-22 (§42):** the logic went back into the CLMD16. AetherRV no longer relays keypads or
> switches anything by itself; it only presses CLMD16 virtual switches.

**Baseline on fw 1.0.3.66.** The owner toggled each channel from N2KAnalyzer (via the IPG100) and from the keypads while the bus was recorded. The baseline folder on the Pi holds the raw logs, one file per step, and CHANNEL-MAP.md.

*Commands*
- **Switch:** `126208` Command for PGN 127501: priority byte F8, pairs (1, bank instance), (N+1, 0/1). Raw frames: `600A010DF201F802` + `6101200201FFFFFF`. Setting a channel to its current state is a no-op.
- **Dimmer:** `126208` Command for PGN 127500: pairs (2, connection = channel−1), (6, duty 0–100). Channel 12 followed a 100→1→100 sweep.
- **Acknowledgement:** the CLMD16 always ACKs `0x44`, even when it ignores a command, so success is judged only from 127501/127500 status.

*Status*
- **127501:** sent every 1.5 s, and immediately on a change.
- **127500:** per connection, [SID][conn][state 0 off / 2 on / 4 PWM][status][op][duty].
- **127751:** connections 0–15 = outputs (V ×0.1, I ×0.01 A); 16–23 = the 8 wired inputs (voltage only). Input N is also bank indicator 16+N.

*Inputs*
- Input 2 = ignition.
- Input 4 = reverse gear; it drove output 4 (reverse lights), which is why ch4 ignored remote commands.
- Input 6 = dome light. It is PWM-faded, so the raw input chatters (~180 changes in 30 s).

*Keypads*
- The keypads send no commands. A CKM12 pulses its own bank-50 status bit, and the CLMD16 config mapped those bits to outputs.
- Bindings: button 4→ch4, 7→ch14 (water pump), 9→ch7+ch16 (group), 10→ch10 (stereo), 11→ch9 (drawer locks, 10 s auto-off), 12→ch12 (cabin lights).
- Ch11 was always on and refused remote control.

**Firmware upgrade to 1.0.7.1**
- The CLMD16 came back with a factory configuration: bank instance 0 and all outputs off. N2KAnalyzer 3.4.2.5's config restore crashed, and the owner could not find its backup.
- The whole session is kept in the baseline folder, including the configuration read just before the flash.

**Owner decision: the logic lives in AetherRV** (`aetherrv/switching.py`)
- `N2kNode` claims address 150 (60928) and answers ISO requests for it.
- `SwitchController` (device `n2k`):
  - `set` / `dim` behind TxGate.
  - Needs a fresh (≤3 s) 127501; sends exactly once via the Cerbo socketcand link.
  - Verifies from status within 4 s, never retries, logs to command_log.
- Keypad logic: acts on a rising edge of the bank-50 bit (0.6 s lockout), never on the first value seen after start. A toggle reads the first channel and sets the whole group alike.
- Input follows are debounced (dome 1.5 s) and edge-only.
- Ch9 (drawer locks) is `blocked` (refuses ON) until the owner puts its 10 s cut-off back into the CLMD16.
- The README safety model now states this exception: with automation on, the keypads and reverse lights don't work while the Pi is down.
- Tests: the encoder reproduces the captured N2KAnalyzer frames byte for byte.
- Status: deployed read-only (`tx_enabled = false`). Next: the owner sets the bank back to 32 → approve n2k transmit → first live send on spare ch8 → keypads.
- **2026-11-21, ignition interlock (owner):** the spot lights (ch1) and flood lights (ch3) draw a lot, so they carry `requires_input = 2` (ignition). AetherRV refuses ON unless the debounced ignition input is on, for touchscreen taps and keypad presses alike; OFF is always allowed. The tiles show "needs Ignition".
- **Ignition-off auto-off (owner):** when the debounced ignition input goes from on to off, AetherRV switches off every channel with `requires_input = 2` that is on (spot + flood lights). This fires on the change only, never on the first value after start.
- **Drawer locks moved to ch11 (owner rewired, 2026-11-21).** ch11 is blocked (refuses ON); keypad #2 button 11 → ch11; ch9 is spare. The 10 s cut-off in the CLMD16 now belongs on ch11.
- **2026-11-21, drawer locks via a CLMD16 virtual switch.**
  - **The hole:** a direct ON to ch11 from N2KAnalyzer (16:44:43) held the locks on for 19 s, so a plain ON bypasses any timer. ch11 stays blocked for `set ON` in AetherRV, permanently.
  - **CLMD16 internals** (from N2KAnalyzer 3.4.2.5's own strings): signals are Discrete I/O 1–36 (keypads and wires), Network Input 1–28 (the 126208 commands to bank 32), Virtual Switch Input/Status 1–12, Timer Output 1–12 and more. A timer is Input (signal) + Period (s) + Mode (0 none / 1 ON delay / 2 OFF delay). Each output is driven by one signal.
  - **The owner's setup:** virtual switches enabled at **127501 instance 33**. The CLMD16 broadcasts bank 33 (12 switches) every 1.5 s. Plan: Timer 1 input = Virtual Switch Input 1, 10 s; ch11 output = Timer Output 1.
  - **AetherRV `[[device.n2k.trigger]] drawer_locks`:** fresh status → vsw 33.1 ON once → verify → OFF once after 1 s → watch ch11. If ch11 is still on after 13 s, a direct OFF is sent and the alert "controller timer failed" is raised. Keypad #2 button 11 and the Drawer locks tile use the trigger.

## 40. EPOCH batteries over Bluetooth (RoyPow BMS), 2026-11-21

- **Discovery.** A raw-HCI BLE scan (no BlueZ needed) found all four batteries advertising " B12460A  2402000xxx " with services ffe0 + fee7. Their manufacturer data holds only an ID and the BLE address, no telemetry.
  - The same scan also heard the four Ruuvis, the SmartSolar, "Sprinter" and a Garmin Overland.
- **Protocol.** aiobmsble (Apache-2.0, the library behind HA's BMS_BLE) matches the name pattern " [BS]12*" to its **RoyPow** plugin, and the batteries answer RoyPow queries on ffe1 (one characteristic, write + notify).
  - Query frame: `EA D1 01 04 FF <cmd> <xor> F5`.
  - 0x04: SoC, voltage, remaining Ah, cycles, runtime.
  - 0x03: current, problem code, temperatures, MOSFETs.
  - 0x02: cell voltages.
  - Decoded with aiobmsble's field map.
  - aiobmsble's own connect fails (GATT "Unlikely Error"); these modules also often fail the first ffe1 subscription and then drop the link. So `adapters/epoch_ble.py` reconnects from scratch on a failure (3 tries) and reads one battery at a time, one connect → 3 queries → disconnect per read. Each battery refreshes every ~2.5–3 min. Read-only.
- **First reads.** All four at 99–100 %, 13.29 V, −5.9 to −6.6 A each (bank about −24.8 A), cell spread 2–4 mV, 11–20 °C, 38–40 cycles, no faults.
- **Alerts.** Per battery: silent > 10 min, BMS problem code, MOSFET off, cell spread > 50 mV, ≥ 50 °C.
  - The Cerbo's "Only 1 of 4 batteries reporting" drops to a warning ("… on the CAN bus") when Bluetooth reads all four, because then it is only the CAN comms daisy-chain.
- **UI.** The battery popup gains "Individual batteries": per-battery SoC, V, A, Ah, cycles, cell spread, max temperature and cell voltages.
- **Pi changes (owner-approved).** `pacman -S bluez bluez-utils` and bluetooth.service enabled; bleak + aiobmsble added as dependencies.
  - Side effect: the pacman mkinitcpio hook rebuilt /boot/initramfs-linux.img. That file is unused by the custom 7.3.5 boot (os_prefix 7.3.5/, which has no initramfs).
- **2026-11-21, 1 Hz persistent connections (owner: like the phone app).**
  - **Owner decision:** AetherRV holds one link per battery and polls 0x04 + 0x03 every second, 0x02 every 5 s. The modules take one connection, so the phone app can't connect meanwhile.
  - **Root cause of the flaky connects** (btmon). On a FIRST contact, bluetoothd reads the GAP characteristic "Peripheral Preferred Connection Parameters" (2a04), which these modules never answer, so BlueZ hits its 30 s ATT timeout and drops the link. Within BlueZ's 30 s TemporaryTimeout the values are cached, so an immediate reconnect works in ~1.8 s. Confirmed on one battery: fail, OK, OK, then fail after 60 s.
  - **The fix in the adapter:** reconnect at once (1 s) after a failed open, inside the connect lock; use StartNotify; clear stale BlueZ links at start. No BlueZ config change.
  - **Ruled out:** the phone app, Wi-Fi coexistence, btmon, the GATT cache files.
  - **Result:** all 4 connected at once and updating every second, stable over 3 min. Startup takes ~2.5 min (one 30 s first contact per battery).
- **2026-11-21, Energy screen v2 (owner).** The battery card spans the top two rows. The six power readings are smaller; MultiPlus sits above the Shore limit + MPPT cards.
  - **Main glyph:** four batteries in parallel inside one big battery. A red + bus runs to the + terminal and the − bus to −; each battery fills to its own Bluetooth SoC.
  - **Card data, modeled on the EPOCH app:** bank SoC/state/V/A/time to go, Ah remaining of 4 × 460, and VAVG/VDIFF/VLO/VHI across all 16 cells.
  - **Temperatures:** the four BMS sensors are assumed to be T1, T2, Ambient, MOS in that order.
  - **Per-battery table:** SoC, V, A, W, cycles, Ah/460, and charge/discharge MOS.
- **2026-11-21, 3D battery-bank scene (Energy).** Each battery is a stack of four glowing discs (one per 25 %), with a partial "liquid" fill on the current quarter, blue/amber/red by SoC. The stacks stand on one glowing platform with pulsing rings and halos.
  - Animated + / − buses: flow pulses run outward when discharging and inward when charging, faster at higher current. A scan line runs up when charging and down when discharging.
  - Per-battery current sparklines (2 min, client-side, auto-ranged).
  - The scene is built once per battery set and updated in place, so animations don't restart.
  - Cost: headless CPU on the Energy view was 156 % with blur filters, 123 % after replacing them with stroke glows and making the halos static, vs 83 % on Home. The owner accepted that: the page is rarely shown, and a later move to React may help.
- **2026-11-21, control from the LANs (owner).** Removed "view only" for the owner's networks. New `[web] control_subnets` (set to the van and home LANs on the Pi) and `control_tailnet = "all"` (every tailnet peer, still required to arrive on the console's Tailscale address).
  - Effect: anyone on the van or home Wi-Fi can now use every control, including the heater, inverter, switches, dump valve, drawer-lock trigger and Starlink update.
  - The README safety model was updated to say so.
- **2026-11-21, Starlink signal quality / SNR.** Newer dish firmware reports `signal_quality` (0–1 = SNR/10 dB, clamped, so 1.0 means ≥ 10 dB) plus `is_snr_above_noise_floor` and `is_snr_persistently_low`.
  - These are not in `status_data()`, so they are read from the raw `get_status()`. Field support is judged from the reflected message definition, because proto3 drops 0.0.
  - The Network card shows "signal quality" % and "SNR" dB ("≥ 10 dB" when capped). SNR is recorded in history.
  - A warning is raised when the dish flags SNR as persistently low. First reading: 1.0 (≥ 10 dB).
- **2026-11-21, 3D Starlink sky (Network card).** three.js r160 (MIT) is vendored at `web/vendor/three/`; `web/sky3d.js` renders the obstruction map as a dome of light over a stylised Sprinter.
  - **Orientation:** the van's nose points at the dish azimuth, the panel tilts to its elevation, and a beam plus an amber heading line and chevron mark the boresight. The cell orientation matches the verified 2D map.
  - **Look:** custom point shader (clear = dim cyan dust, obstructions = coral embers sized by severity, a horizon→zenith scan sweep, near-side dust faded) and a fresnel glass dome.
  - **Interaction:** drag rotates and tilts, pinch or wheel zooms, double-tap resets; it auto-rotates after 6 s idle.
  - **Cost and fallback:** lazy-loaded only on the Network view, capped at 30 fps, and stops when hidden. It falls back to the 2D map without WebGL.
- **2026-11-21, WAN priority control (Peplink).** The router has `config.wan.connection.priority` (it returns 401 to the read-only user, versus 999 for an unknown function).
  - **Credentials:** a separate read-write API client (scope `api`, secrets `peplink_rw_client_id` and `peplink_rw_client_secret`) is used only for this command; monitoring stays on the read-only login.
  - **Command:** `set_wan_priority {id, priority 1–4}` goes through TxGate. A fresh GET of the priority list feeds a strict payload builder (any unexpected shape means nothing is sent); one POST (`instantActive`), never retried; then status.wan.connection is polled until the priority shows. Every attempt goes to command_log.
  - **Kiosk:** tap P# to get a P1–P4 picker, then a confirmation.
  - **Status:** off until `[device.peplink] tx_enabled = true` and the read-write client exist, and the GET shape has been checked against the builder.
  - **2026-11-21, update.** This firmware will not create API clients over a web session (`auth.client` returns 401 even for admin), so the writer is a read-write web login (`peplink_rw_user`/`peplink_rw_password`; the owner chose the admin account).
    - The GET shape is `{order:[…], "<id>":{name, group, priority, enable}}`. Per Peplink's API reference, the POST sends only the changed connection: `{instantActive:true, list:[{connId, priority}]}`. Connections disabled on the router are refused.
    - Dry-run checked against the live router. `[device.peplink] tx_enabled = true` is set (config backup `*.pre-peplink-tx-20261121`).
- **2026-11-21, WAN drag-to-reorder.** Grab handles on the WAN priority rows.
  - Dropping a row renumbers the enabled WANs P1, P2, P3… in list order (anything past P4 stays at P4), after one confirmation.
  - `set_wan_order {order}`: strict `order_targets` (every enabled connection exactly once, disabled ones never sent); one POST containing only the connections whose priority changes; verified until all show the new priority.
  - The card stops re-rendering while a drag is in progress. Tested on the Pi with CDP mouse events: drag, confirm dialog, cancel, nothing sent.
- **2026-11-21, kiosk Home controls dead: root cause.** The startup code (`connect()` + initial `go(view)`) ran mid-file. When the page opened on Home (the kiosk always does), `renderQuick()` hit `QUICK_DEMO` in its temporal dead zone and threw, which aborted the rest of the module.
  - Lost with it: the tile, heater and +/− handlers (+ fell through to the card → Climate) and the kiosk screen-off block. Laptops that loaded another view first were unaffected.
  - Fix: start-up runs last; every start view was scanned headless for load errors.
  - Also: in-place DOM morph for tappable containers, a `[data-goto]` guard, and a ctlPending watchdog.
  - Lesson: a headless test that stubs clicks proved nothing; the real-handler test with a command trap caught it. Once the trap failed to load and a real Cabin lights ON was sent; it was turned off on owner OK.
- **2026-11-22, N2K address-claim chatter.**
  - The Cerbo GX (src 228) sends an ISO request for PGN 60928 to AetherRV (150) every 10 s, and the IPG100 (40) broadcasts the same request. AetherRV answers each one, as NMEA 2000 requires.
  - Bus cost: 0.2 frames/s of ~55 (0.36 %). Log cost: ~57 % of all journal lines.
  - These poll replies are now logged at DEBUG (TxGate `quiet`). First claims, moves/contests and blocked transmits stay visible.
- **2026-11-22, Settings → Starlink tab.**
  - **Controls:** heating (snow_melt_mode auto/on/off) and sleep schedule (power_save_* in dish_set_config; UTC minutes, the UI edits local start/end), both verified by re-reading `dish_get_config`.
  - **Updates:** "Check for updates" sends the `update` request WITHOUT schedule_reboot and watches software_update_state for 30 s. "Install update" (REBOOT_REQUIRED only) and "Reboot dish" are verified by an uptime reset or the dish going offline. All via TxGate, one send, never retried.
  - **Power graph:** `starlink.power_w`, the last-minute average of the dish history `power_in` (1 Hz, 900 s ring), recorded each minute.
  - **Events & outages:** the newest 50 from history `event_log` (~10 h kept by the dish; device ids not copied).

## 41. NMEA 2000 on the Pi's own CAN port, the Cerbo as backup (2026-11-22, owner-approved)

- **Wiring:** a cut N2K drop cable on CAN HAT+ port 0 (`hat0`): white → H, blue → L, black → G; red (+12 V) and
  the drain not connected; the HAT's 120 Ω off (the backbone is terminated). `30-hat0.network` has
  `ListenOnly=no` at 250k.
- **FailoverBus** (`canbus.py`): `[device.n2k] bus = "hat0"`, `backup_bus = "n2k_cerbo"` (the Cerbo's
  socketcand). It reads and sends on hat0, fails over after 2 s of silence or a failed send, and comes back
  after 30 s steady. A path-backup alert shows while it runs on the Cerbo.
- **MCP2515 has no one-shot mode:** an un-ACKed frame is retried forever and floods the bus. AetherRV expects
  its own TX echo within 250 ms; if it doesn't come, it resets the port through the root helper (`can_reset`).
- `hat1`, `fd0` and `fd1` stayed listen-only until the Espar work (§43).

## 42. CLMD16: dual control, logic back in the load controller (2026-11-22)

- **Owner decision:** the keypads must not depend on the Pi. AetherRV's keypad relay, the reverse-light follow
  and the ignition auto-off were removed. **AetherRV switches nothing by itself.**
- **How AetherRV switches now:** CLMD16 Virtual Switch N (bank 33) mirrors CKM12 button N, and the CLMD16's own
  logic (OR, toggles, timers, counters) drives each breaker. A tile press pulses VSW N (ON, short hold, OFF) and
  verifies the channel via 127501. It never sets a breaker directly, with one owner-approved exception: OFF to a
  **tripped** channel sends one direct breaker OFF to clear the trip.
- **Dimming:** ch12 dims through a CLMD16 counter (hold to dim). Kiosk dimmer tiles hold the VSW while the finger
  is down (`hold` command, released on lift, capped at 8 s).
- **Element numbering (owner):** Logic N always feeds breaker N; extra elements are N+16, then N+32.
- **Channels (now matching button/VS numbers):** ch4 "Rear scene lights" (button OR VSW → toggle, OR the
  reverse input through an OFF-delay timer that rides out the van's PWM reverse signal; input 4 debounced
  1.5 s in AetherRV's display), ch7 water pump (6.5 A average, 10 A peak), ch9 dump valve, ch10 stereo
  (ignition OR toggle; AetherRV refuses OFF while the ignition is on, `on_with_input = 2`), ch11 drawer locks
  (10 s OFF-delay in the CLMD16; trigger-only in AetherRV), ch12 cabin lights (dimmer), ch14 spare.
- **Home grid:** a switch's `home = <n>` pins it to position n; the three scene lights are the top row.

## 43. Espar Airtronic S2 D2L: decoded, then AetherRV as its controller (2026-11-22)

- **Bus:** the EasyStart Pro harness's CAN pair (blue/black = H, blue/red = L, brown = ground; red +12 V unused)
  tapped where the harness terminator plugs in, terminator kept, `hat1`'s 120 Ω off. 500 kbit/s, 11-bit IDs.
- **Captured from the EasyStart Pro** (step by step with the owner: handshake, heat, setpoint, off, fan,
  unplug, timed run):
  - Controller → heater: `054` every 200 ms `[flags][mode 05 heat / 02 fan / 00 off][setpoint LE ×0.1 °C][room
    (°C+50)×10]`; `055`/`056`/`057` static; `60D` heartbeat 100 ms; `065` room; a settings burst
    `05C`–`06D` + `10A` once per wake.
  - Heater → controller: `2C4` state (00 init, 03 idle, 09 heating, 21 fan, 0B fault) with run/after-run flags,
    fault byte and temperature; `625` heartbeat.
  - After-run 240 s after heat, none after fan. The bus sleeps 5–15 s after idle.
  - Controller lost: the heater stops within ~15 s on its own and logs P000307/P000440. **The fail-safe is the
    heater's.**
- **Diagnostics** (`espar_uds.py`, read-only UDS on 0x7A0/0x73C, services 0x22/0x19 only): voltage, blower,
  glow, pump, power, flame, combustion phase (0x0C = ignition on the S2), fuel, temperatures, regulation,
  hours by power band, air pressure, fault memory with severity and lockout detection.
  - A **read-only scan of FD00–FDFF** (49 answer) showed `FD01` carries four temperatures: flame, intake,
    **outlet** (8.8 °C cold → 52 °C at full heat while intake stayed 9–18 °C) and a fourth that reads exactly
    0 °C (not fitted).
  - The card's readings are requested every cycle at 1/s, so tiles refresh every ~9 s.
- **AetherRV as the controller** (`espar_control.py`; owner decisions: the Pi replaces the EasyStart Pro,
  room temperature = cabin Ruuvi else the heater's intake sensor, run times as on the Pro: 1–240 min, 270–720
  in 30-min steps, unlimited). It wakes the heater in OFF, replays the Pro's settings burst, applies the target
  mode once idle and verifies on `2C4`. It stops if another controller appears or the heater goes silent.
  Bench steps OFF takeover, FAN and a full HEAT run passed; the owner skipped the fail-safe steps (already shown
  by the capture).
- **Fan speed is not adjustable in FAN mode:** a supervised test put 15/20/30 °C words, levels 1/5/10 and 20 %
  into FAN's setpoint field; the blower stayed at ~56 % every time. FAN sends exactly what the Pro sends.
- **Restarting the backend stops a running Espar** (it loses its controller). Check the heater is off before
  any restart.

## 44. CAN port recovery (2026-11-22)

After a reboot with the FD HAT refitted, `hat1`'s MCP2515 "didn't enter config mode" and stayed down. Unbinding
and rebinding the driver fixed it. `recover_links()` now does that by itself: a local port down for 30 s gets a
driver rebind through the root helper (`can_rebind`), at most every 5 minutes.

## 45. Weather map (2026-11-22)

TomTom traffic (relative-delay flow + incidents, proxied and cached by the backend); glass controls (locate,
zoom, opacity slider, timeline with play/scrub, speed 1×/2×/4×, loop length 1/2/6 h, layers menu with
Streets/Satellite/Topo, radar/IR/off, public lands and traffic); radar history recorded on the Pi (6 h) plus a
1 h nowcast; a ~15 fps loop interpolated on the GPU from optical-flow motion fields (OpenCV DIS).

## 46. Climate, Energy, Settings and alerts (2026-11-22)

- **One cabin setpoint for both heaters.** The Climate dial sets the MCS7 and, at the same moment and
  independently, a heating Espar. The Espar accepts 10–30 °C: turning it on below that gives an error toast
  and sends nothing; dragging the setpoint below 10 °C while it heats turns it off; above 30 °C it keeps its
  setpoint. Heaters are chosen with their own buttons.
- **Heater cards:** the MCS7's Heat (thermostat, MCS7 act 9, newly allowed), Hot water and Engine pre-heat
  icons are tappable toggles (confirm, one verified command); the Espar's flame/fan/clock stack sets heat, fan
  and run time. Tiles keep their slots with "—" when there's no live data. Battery, runtime and fuel totals
  moved to Settings.
- **Settings → MCS7 / Espar:** device details, hours (by power band for the Espar), fuel, fault memory.
- **Alerts:** stored faults in the Espar fault memory and the MCS7 fault history raise warnings; an
  acknowledged stored fault stays acknowledged across restarts while it's still stored. Per-battery warning
  below 32 °F. The BMS-Can "N of 4 modules" alert is dropped when Bluetooth reads all four.
- **Energy:** battery temperature shows the hottest probe, or the coldest (blue) when any is below 40 °F.

## 47. Starlink power: CLMD16 output cut-outs (2026-11-22)

The Starlink (through a 12→48 V boost converter) on CLMD16 ch16 lost power after 99–822 s, five times; ch15
held 42 minutes, then the same. Each time the channel's output dropped to ~2 V / 0 A while 127501 still said ON
and no trip was reported (127500 status 0x01 → 0x09); it stayed off until switched OFF and ON. Current was 2–3.5 A
(13.5 A with the dish heater), far below the 25 A rating and every trip curve. No public reports of this were
found. **Resolution:** the Starlink runs from the fuse block; a relay or SSR driven by ch15 (VS 8) is planned so
the tile can switch it again without the CLMD16 carrying the converter's load.

## 48. Clean shutdown on a nearly empty house bank (2026-11-23, owner)

Considered a UPS HAT (Waveshare UPS Module Mini) and decided against it: 5 V 2.5 A is below the Pi 5's 5 A, the
cells are tiny and dislike a hot van, and the Pi already runs from a 1840 Ah bank. Instead `lowbatt.py`
(`[power] auto_shutdown`) powers the Pi off through the root helper when the bank's SoC (Victron shunt, else the
Bluetooth batteries' mean) stays ≤ 5 % for 2 minutes while not charging; a serious alert from 8 % and a
critical countdown alert before it happens. Charging or recovery cancels it; no SoC never triggers it. The Pi 5
then stays off until power is cycled (e.g. the BMS cuts and restores it) or its button is pressed.

**Heat lockout at 10 % (owner, 2026-11-23): required, because the Espar and the MCS7 can be seriously damaged if
they lose power while burning or in their after-run.** Once the SoC has been ≤ 10 % for 2 minutes and not
charging, heat is locked out (`power.heat_lockout`, serious alert): both heaters' command paths refuse to start
heat, and a heater found heating is turned off with normal verified commands (Espar OFF; MCS7 furnace, electric,
hot water and pre-heat off; `auto:low-battery`), again whenever one is found heating, at most once a minute.
Each runs its own after-run. The lockout lifts when charging or above 12 %. Before the 5 % power-off, AetherRV
turns the heaters off too and waits (up to 10 min) until both are idle, so the heaters are cold before the BMS
can cut power. This is the one owner-approved exception to "AetherRV acts only when a person asks".

## 49. The Rust system in the van: the cutover (2026-11-24, owner present)

Version **8ad8f60** (`/opt/aetherrv-rs/VERSION`), built natively on the van Pi (16 min 42 s), installed next to
Python and switched with `docs/CUTOVER.md`. The Python tree stays in `/opt/aetherrv` for at least two weeks; the
rollback is `sudo /opt/aetherrv-rs/src/packaging/rollback.sh`.

- **Read-only, 30 min**: every key Python had was there (the 54 new keys are all D7); energy, the four EPOCH batteries
  (real BlueZ, about 1 Hz), N2K's 11 devices, the MCS7 and the Espar as Python showed them; the owner checked the
  Cerbo, the MCS7 panel, the keypad and the touchscreen. hat0 and hat1 sent **no frame** in the whole period. (Two
  hat0 frames during the switch were Python's last answers to the Cerbo's 10 s address-claim poll, before it
  stopped.)
- **Then one device at a time**, each with the owner's yes: N2K (13 commands, each verified; hold-to-dim 44 → 98 %),
  the MCS7, Victron, the Espar's diagnostics, then the Espar (attach OFF; FAN twice; HEAT 10 °C for 5 min, resumed
  after a `systemctl restart` in 0.5 s and a `kill -9` while burning in 1.6 s, the timer's OFF and a 240 s after-run,
  no new fault), and finally `cutover.sh --full`.
- **The Espar's controller-loss tolerance**, measured at last: **15.2 s** in FAN, then idle and asleep.

Surprises, all handled on the day:
- The van had no Rust toolchain, `mosquitto`, WPE, GStreamer or `jq`; `heaters-idle.sh` (and so the cutover and the
  rollback) needs `jq` and now says so. `push.sh` could not create `/opt/aetherrv-rs` on a first install, and the
  runbook's backup `tar` expanded its glob in the wrong directory: both fixed.
- The runbook's N2K test named a spare ch8 that no longer exists; the "Spare (ch 14)" tile has no virtual switch and
  is refused, as in Python.
- Testing the MCS7 from the Climate dial sent 10 °C (a whole display degree where the finger let go) and started the
  furnace; set back to 7.2 °C, a normal after-run. Testing Victron switched the MultiPlus on and the MPPT off and on;
  set back. The runbook now says to step with Home's ± and to note the Cerbo's settings first.
- The Espar reports "heating" (2C4 state 09) from the first second of a HEAT run, so "heating" does not mean burning;
  the first resume test therefore ran during the glow-plug preheat. Burning is the pump and power from the
  diagnostics.
- The heater's stored codes P000307, P000129 and P000440 date from 2026-11-22 (all inactive). AetherRV only reads
  them (UDS 0x22/0x19); clearing them needs the owner's own Espar tool.
- The owner kept the resume timings (`resume_listen_s` 1 s, `RestartSec=100ms`).
- **The first boot of the Rust system** (a reboot, heaters idle first): no failed unit, all 18 services up, Python's
  units off, the gateway 2.6 s and the panel 4.0 s after the kernel started, N2K address 150 claimed again, the four
  batteries back within minutes. Three services logged "network unreachable" until the network came up, then
  connected (they no longer wait for it).

**The display stayed dark after the first Rust boots** (2026-11-24 evening). After the splash the panel went blank,
and the kernel read both HDMI connectors as disconnected with no EDID, so sway had no output. Sway started 4 s after
the kernel; the Python kiosk, which never failed, started sway at about 9 s. While diagnosing, the cable was moved to
HDMI 2 and back (touch is mapped to HDMI-A-1, so it only works on HDMI 1), and the screen came back once the panel
was awake and sway lit it. Fix: the panel unit now waits until the HDMI display reads connected and steady for 2 s
(at most 20 s; `packaging/panel/wait-hdmi.sh`), installed in the van as an `/etc` drop-in. **Not yet proven by a
reboot**: the reboot test was refused by the heater check (the Espar was heating) and waits for the heater to be off
and cold. The screen auto-off was set to never for the evening, then back to 10 min (owner); whether a touch wakes the panel
after a long sleep is still untested (it may drop off HDMI when it sleeps, and touch can't wake an output sway no
longer has). The touchscreen's own commands were confirmed
that evening (client `kiosk`: the MultiPlus, the MCS7 setpoint, an Espar heat run).

**The dark weather map stayed light on the panel** (same evening). The map's dark mode was an SVG `url()` filter,
which WebKit (WPE) ignores on composited layers, and Leaflet moves every tile with `translate3d`; Chromium (Python's
kiosk, and the UI tests) applied it. Now CSS filter functions fitted to the same matrix (within 7/255 per channel),
checked in WPE on the bench (the map's mean luminance 0.19 against 0.90) and confirmed dark on the van by the owner.
Installed with `packaging/push.sh --cutover` at 3bde579 (with the panel's HDMI wait in the unit itself now; the
`/etc` drop-in duplicates it and can go).

**A panel restart does not disturb the Espar** (owner's test, same evening): the panel restarted during a heat run,
the Espar recorded every 2 s from 30 s before to 210 s after, the history log and the heater's own 2C4 frames: no
change at all (burning at 100 %, flame on, no fault, no frame gap). The earlier dip began 78 s before that day's
first panel restart and was the heater's own (flame-out and relight, nothing stored).

**The Systems page's Modules card filled with tools** (same evening): every `aetherrv-ctl` run left a retained
"down" status, 32 by then. Tools no longer announce themselves (`BusOptions::tool()`), the UI hides the old ones,
and the 32 were cleared from the broker. The first deploy cross-compiled for the van (d8f3adf, `cross-push.sh
--cutover`, owner), so the Pi wasn't loaded for 17 min during a burn. The Espar's diagnostics are in the history
database too (every 30 s, 30 days, D23).

**A service restart stopped an Espar burn** (same evening; my restart, with the owner's go-ahead). Restarting the
services onto a new build mid-run: the Victron service reported the Cerbo disconnected for a moment, and the Espar
service switched its room value from the cabin Ruuvi to the heater's intake sensor, which reads about 40 °C while the
heater burns. It sent 39.4 °C, the heater stopped burning (no fault; two events in its log), cooled for about 5 min
and relit by itself. Fix 493935c (owner): during a heat run the last cabin reading is held for up to 5 min
(`CABIN_HOLD_S`), then the intake sensor as before; tested with a Victron restart mid-run on the bench and installed
in the van. Lesson: a restart of any service the Espar reads (the Victron service carries the cabin Ruuvi) can
affect a burn; ask before restarting services during a heat run.

Still to do (`docs/CUTOVER.md` §8): ~~a command from the touchscreen itself~~ (done, see above) (all of the day's commands came from a
browser on the LAN; a tap the owner made on the panel left no command in `command_log`, so it was not seen to
switch through AetherRV), the boot splash watched and power-on by stopwatch, the panel's memory after a day and
after several days, the §8 UI numbers on the real panel, and the Cerbo failover only if wanted. The alert chime was
heard (the owner).
Cosmetic, for later: exited `aetherrv-ctl` runs stay listed as DOWN in `aetherrv-ctl health`; the Espar service clears
its retained diagnostics keys about 10 s after a restart until its next read; a stopping service logs one
"bus: disconnected" warning; `aetherrv-espar.service` sets `RestartSec` twice (the later 100 ms applies).

## 50. The Python code removed (2026-11-25, owner)

`rust` was fast-forwarded into `main` (a238271, tagged `python-final`: the last tree with the Python system), then the
Python code left the repo: the backend (`src/`), its tests, its UI (`web/`), its deploy (`deploy/`), `pyproject.toml` /
`uv.lock`, and the Python tools and harness pieces. What the Rust system still used moved to `packaging/` first
(network, iwd and boot-splash files, and the system update, `packaging/update`, without the Python environment check
that would have failed every update once `/opt/aetherrv` is gone; the van gets it with its next deploy). Ported:
`aetherrv-setup peplink-rw-client` and `store-secrets` (secrets written only after the services' own parser reads them
back), the N2K table generator and the licence list as jq scripts (byte-identical output), the UI benchmark's Python
as `aetherrv-uibench` subcommands and jq, each diffed against its original before deletion. The van is unchanged:
`/opt/aetherrv` and `packaging/rollback.sh` stay for the rollback window.

The README screenshots are now the Rust UI (`harness/ui-reference/screenshots.sh`): the van's state and history
copied read-only, sanitized (`sanitize-state.jq`: Denver, "Campground Wi-Fi", Batt 1-4, serials/MACs/private IPs
replaced), rendered against a stub gateway that refuses every command.

On the bench, 20be112 installed with `cross-push.sh` (the new `aetherrv-update.service` in place, outside
`units.txt`), then `rollback-test.sh` against the bench's own Python install: 36 ok, 0 failed, as before. The test's
Python install step puts the Python update unit back, so the bench's was restored afterwards. The uibench scripts
have not been run end to end on the bench yet.

Later the same day the owner chose to drop the rollback at once rather than after two weeks: `packaging/rollback.sh`
and `harness/bench/rollback-test.sh` were deleted, and `cutover.sh` lost its Python half (it keeps staged transmit;
`--read-only` now restarts a running system after the heater check, tested on the bench). Then, with the owner's yes,
the van: 773090c deployed (cross-compiled; installs only: the new update unit and `aetherrv-setup`), a full backup of
the Python install taken to the workstation (verified), and `/opt/aetherrv`, the `aetherrv`/`-kiosk`/`-admin` units,
`/usr/local/lib/aetherrv`, the kiosk PAM file and its shutdown-splash drop-ins deleted, then a daemon-reload. No
service restarted (the same PIDs before and after; the Rixens was burning throughout), no unit failed, the gateway
and the panel kept running. The services still run 493935c/d8f3adf binaries until their next restart.

The same day (owner: "deploy to the van and restart the services"), with both heaters idle: d29284c deployed
(cross-compiled; the binaries equal 773090c's) and all 18 services restarted onto it, the Espar's last (it stayed off and
asleep; nothing sent). All active, no failed unit, no warnings. Not restarted: the broker, the on-demand admind, the panel.

## 51. A rear cabin Ruuvi (2026-11-27, owner)

The owner fitted a fifth Ruuvi at the back of the cabin. The Cerbo heard it but, as Venus OS does with every new
Bluetooth sensor, kept it off and unnamed. Venus OS lists each sensor under `ble/0/Devices/<dev>/` (`Name`, `Enabled`,
`CustomName`, `Age`) and keeps its settings under `settings/0/Settings/Devices/<dev>/` (`CustomName`, `Enabled`, and
once enabled `TemperatureType` and `ClassAndVrmInstance`). Read first from the Pi (a 40 s listen with
`mosquitto_sub`; the MQTT login in a 0600 options file, never on a command line; the Victron service's keepalive makes
the Cerbo republish everything), then, with the owner's yes, two writes: `CustomName = "RCT"`, and once that showed,
`Enabled = 1`. The Cerbo created a new temperature service (generic, with humidity) and AetherRV's `victron.temps`
listed it at once. Nothing in AetherRV restarted.

Owner decisions: the cabin value becomes the mean of all three sensors (front Ruuvi, rear Ruuvi, the MCS7's sensor by
the bed), and the Espar should regulate on that mean too. The Espar part is a heater change: it is designed and
bench-tested on its own and needs a fresh yes before it reaches the van.

The display part (no heater code):
- `[device.victron] cabin_rear_temp` names the rear tag; the service publishes `climate.cabin_rear.*`. Only when it
  is named: unlike the other roles there is no first-of-type stand-in, which would be the front tag counted twice.
- `aetherrv_core::climate` holds the cabin mean, shared so that every reader agrees on which sensors count: the fresh
  ones out of the three, each Ruuvi stale after 15 min or with the Cerbo disconnected, the MCS7 60 s after its last
  poll. With no rear tag the result is the front + MCS7 mean exactly as before.
- `aetherrv-derive` publishes it (`[climate] cabin_source = "average"`), with the new `climate.cabin.rear_ruuvi_temp`;
  `climate.cabin.rear_temp` stays the MCS7 sensor. The Climate dial reads "average of front · rear · MCS7", listing only
  the sensors in use (with no rear tag, "front · back" as before).
- The Cerbo simulator has the fifth tag. The Espar still reads `climate.cabin_front.temp` only.

## 52. Heater runs on the Climate chart (2026-11-27, owner)

The Temperature chart shades the time each heater ran ("operational = running", owner), from history already
recorded, so past runs show too: the MCS7 while its fuel pump runs (`climate.rixens.fuel_hz` > 0, i.e. the burner
firing), the Espar while its state isn't off (`climate.espar.state` code > 0: starting, heating, fan or cool-down; an
error state would also count). A bucket of `/api/history` with any running in it is shaded whole and neighbouring
ones merge, so a band is as precise as the bucket: 4 min at 24 h, 28 min at 7 d, 2 h at 30 d. Each band is a light
wash with a solid strip along the top (MCS7 violet, Espar amber, validated against each other and the surface in day
and night mode), named in the legend and as "running" in the tooltip. `TimeChart` takes the bands; other charts are
unchanged. Checked against a copy of the van's history: the MCS7's burns line up with the cabin warming, and the
Espar's run on 11-24 shows.

Deployed the same day (owner: "Deploy, restart panel too"), heaters idle: 0d0afe4 installed (cross-compiled; installs
only), `cabin_rear_temp = "RCT"` added to the van's `[device.victron]` (backup `*.pre-rear-ruuvi-20261127`;
`aetherrv-migrate` OK), then the Victron and derive services and the panel restarted. The rear Ruuvi read 6.3 °C, and the
cabin value became the mean of all three (front 5.0, rear 6.3, MCS7 5.5 → 5.6 °C). The Espar was not restarted and still
reads the front Ruuvi. No failed unit, no warnings.

## 53. The Espar on the cabin mean: bench test (2026-11-27, owner)

Owner: the Espar regulates on the cabin mean of all three sensors, like the dial. `[device.espar] room_source =
"average"` (3996f7a; the default "front" keeps the old behaviour): the live room value is the mean of the fresh front
Ruuvi, rear Ruuvi and MCS7 sensor from `aetherrv_core::climate`, computed in the Espar service from the raw keys, so
the derive service is not in its path. Each sensor's freshness is its own (Ruuvis 15 min and the Cerbo connected, the
MCS7 60 s); a sensor that drops out leaves the mean of the others. With all of them gone during a heat run, the last
mean is held for 5 min, then the intake sensor, as with the front Ruuvi. The hold is fed by a flag now, not by
matching the label "cabin Ruuvi", which a label change would have silently broken. Control tests: the mean in every
054, never the front value alone; a dropout of every sensor mid-run holds the last mean and never the hot intake;
heat refused with nothing at all.

On the bench (3996f7a cross-compiled; bench config as the van: rear Ruuvi, cabin average, room source average; the
simulators give front 20.5, rear 19.5 and MCS7 7.2 °C), with every 054 recorded from the heater's bus:
- `isolation.sh` (every service, the aux bus, admind and the broker killed or restarted in turn during a heat run):
  **ISOLATION: PASS**. Of 909 HEAT frames, 897 carried the three-sensor mean (15.7), 6 the two Ruuvis (20.0) while the
  Rixens service restarted, 6 the MCS7 alone (7.2) while the Victron service restarted: no other value and no FE FF.
  Killing the derive service changed nothing. A kill of the Espar service itself resumed on the same value.
- `espar-systemd.sh` (restart, kill -9, watchdog, broker restart; no cabin services, so the intake sensor): PASS,
  max 054 gap 3.0 s, 0 controller-loss faults, 0 full-power frames.

The simulator's intake reading starts at 16 °C and climbs while it heats, so it would show in the recording: it never
did. Then every input lost mid-burn, on the full stack with `blind_off_s = 60` (as `espar-blind.sh`): the broker
stopped for 30 s and started, then stopped for good. All 591 HEAT frames carried 15.7 (the last mean, held), in every
phase, never the intake; the service turned the heater off 60 s after the last bus contact, normal after-run, 0
controller-loss faults, 0 full-power frames. The journal has the start-up line "room temperature from the cabin mean
…". On the workstation, with vcan up, all 23 Espar tests ran (the 7 process tests on the default "front" path
included) and the workspace passes (413).

Not changed, found on the way: a Ruuvi that goes silent is never "stale" while the Cerbo is connected, because the
Victron service republishes every role on every temperature message (an unchanged value refreshes its age) and the
Cerbo republishes everything every 30 s; it drops out only when the Cerbo removes or blanks it. The front Ruuvi has
always worked this way. The van keeps "front" until the owner says yes.

On the van the same evening (owner: "Yes, put it on the van", and a short FAN as the live check), heaters idle:
7705351 installed (cross-compiled; only the Espar's binary differs from what ran), `room_source = "average"` added to
`[device.espar]` (backup `*.pre-espar-mean-20261127`; `aetherrv-migrate` OK), the Espar's service restarted alone (it
stayed off and asleep; start-up line "room temperature from the cabin mean …"). Then FAN with a 2-min timer through
`aetherrv-ctl`: the room value was 5.26 °C from "cabin mean (front, rear, MCS7)", the dial's cabin value exactly (front
4.9, rear 5.6, MCS7 5.3), and OFF at once; both commands ok in `command_log`. Waking the heater re-raised its three
stored faults (P000307, P000129, P000440), the same fault memory as on 11-24, nothing new. No failed unit.

## 54. The Ruuvi tags read directly: the listener, observe-only (2026-11-27, owner)

Found while testing §53: a Ruuvi that goes silent never counts as stale while the Cerbo is connected (the Victron
service republishes every role on each temperature message, and the Cerbo republishes everything every 30 s). The owner
chose to read the tags directly on the Pi, with no Cerbo fallback ("Direct only"). Each broadcast carries the tag's
own measurement counter and battery voltage, so a silent tag shows as silent, and a low tag battery can be seen
before it dies (all four died unnoticed earlier this month).

Step 1, this build: nothing that runs changes.
- `proto-ruuvi`: data formats 5 (RAWv2) and 3 (RAWv1), tested against Ruuvi's published test vectors (valid,
  maximum, minimum, invalid). Any other format byte is an error the caller counts, never a guess.
- `aetherrv-ruuvi`, listen only, **observe-only**: one BlueZ discovery session (LE, duplicate data on, so every
  advertisement is signalled), watching only devices whose manufacturer data carries Ruuvi's company id. Values cached
  on BlueZ's device objects at start never count as heard. It publishes `ruuvi.tags` (per tag: role, short id,
  format, temperature, humidity, battery voltage, RSSI, sequence, last heard, advertisements, new measurements, the
  longest gaps, gaps over 60 s, decode errors; other people's Ruuvis by short id only, at most 16) and `ruuvi.scan`.
  It owns no `climate.*` key. The tags' addresses live in the Pi's config only. Its unit is installed, not enabled.
- The van's BlueZ (5.88) doesn't offer the passive advertisement monitor without experimental mode, so the listener
  uses an ordinary discovery session, the same mechanism the battery service uses to reconnect.

Next, with the owner's yes: run it in the van for about a day to measure whether every tag is heard continuously
alongside the four battery links (per-tag gaps from `ruuvi.tags`; battery reconnects from the journal, against the
day before). Only then the switch: the climate roles from this service, the Victron roles off, one owner function for
every reader (the Espar's follow list included), `victron.connected` no longer gating directly-read tags.

Measurement started the same evening (owner: "Yes, start it"): 38828c0 installed (cross-compiled; installs only), the
five tags added to the van's config as `[device.ruuvi]` (backup `*.pre-ruuvi-20261127`; the addresses stay on the
van), and `aetherrv-ruuvi` started, not enabled (a reboot ends it). After 45 s: every tag heard, all data format 5, no
decode errors, readings equal to the Cerbo's (the front tag 4.76 against 4.74 a moment apart, the others the same),
tag batteries 2.89–3.18 V, longest gap 7.7 s; the four battery links still connected; no other service's PID changed.
Baseline for the battery links: no reconnect in the 24 h before. Found and fixed for the next build (not deployed, to
keep the measurement running): `aetherrv-migrate` didn't know the `ruuvi` kind (a harmless WARN), and RSSI showed
only once BlueZ signalled a change (now read when a tag is first watched).

Later that evening (owner: "deploy to the van and restart all services"), heaters idle: 88f6440 installed
(cross-compiled) and 19 services restarted onto it, the Ruuvi listener included (still not enabled) and the Espar's
last (it stayed off and asleep, nothing sent; start-up line "cabin mean"). All up, no failed unit, no warnings; the
four battery links connected; every Ruuvi now shows its RSSI (-59 to -75 dBm). Not restarted: the broker, the
on-demand admind, the panel (unchanged since its restart earlier that day). The listener's measurement counts from
this restart.

**2026-11-27, top bar (owner).** The "Control: heat + power" pill is gone from the top bar (every page shares it). A
screen that can control shows no chip; a view-only phone or laptop still shows "Read-only", which says why its
buttons are off. On the van the same evening (owner yes): f03ca13 installed (UI only) and the panel restarted; no other
service restarted, no failed unit.

## 55. Volts and amps on the Energy tiles (2026-11-27, owner)

Owner: small data points on the Energy tiles, solar on the panel side, shore in, DC loads and AC loads. From a 40 s
read-only listen to the van's Cerbo: it publishes the panel voltage (`solarcharger/<n>/Pv/V`) but no panel current, the
MultiPlus's input and output voltage (`vebus/<n>/Ac/ActiveIn/L1/V`, `Ac/Out/L1/V`), and the system totals' currents
(`system/0/Ac/Grid/L1/Current`, `Ac/Consumption/L1/Current`, `Dc/System/Current`). New keys:
- `victron.pv.panel_v`, and `victron.pv.panel_a` = the charger's PV power (`Yield/Power`) over `Pv/V` (no power is
  0 A; with power but under 1 V, unknown);
- `victron.ac.shore_v` / `victron.ac.shore_a`, `victron.ac.load_v` / `victron.ac.load_a`: the currents from the same
  system totals as the tiles' power;
- `energy.dc.load_a` (derive) = the tile's corrected DC load over the battery voltage, not the Cerbo's
  `Dc/System/Current`, which is wrong while the alternator is measured over N2K; the tile's voltage is the battery's.

The tiles show them on the label row ("13.2 V · 12.8 A", wrapping under the label at phone width); a missing value
reads "—", and no values at all hide the line. The Alternator and Battery temp tiles are unchanged.

On the van the same evening (owner yes), heaters idle: e80b3da installed (cross-compiled), then only the Victron and
derive services and the panel restarted. Live: solar 0 W at night (panel 0.04 V, 0 A), shore 1092 W at 119.3 V and
9.1 A (charging), DC loads 117.7 W at 13.37 V and 8.8 A, AC loads 0 W at 119.3 V. The Espar stayed off and asleep, the
cabin average kept going, the Ruuvi listener's measurement was untouched (same PID); no failed unit, no warnings.

**2026-11-27, switches (owner).** The Home page's Lights & switches card lost its "live" pill, and the Switches page its
"Live: tap to switch. Keypad logic is off." banner. Both still explain when something is off: the card says "view only"
or "demo · not connected", and the page "No switch banks decoded yet" or that switch control is off. On the van the same
evening (owner yes): b5117fb installed (UI only) and the panel restarted; no other service restarted, no failed unit.

## 56. The Pi's load on Settings → Device (2026-11-27, owner)

Owner: a chart at the bottom of the Device tab of CPU, GPU and memory use, plus the SoC temperature. From a read-only
look at the van: 4 cores and 16 GB; the SoC's own sensor is the `cpu-thermal` zone (also NVMe, RP1 and fan sensors,
not used); the V3D GPU's `gpu_stats` gives each GPU queue's busy runtime and a timestamp, readable without root.
- The settings service (no new unit to enable) reads them: CPU % from `/proc/stat` (idle + iowait over all), GPU % as
  the busiest queue's Δruntime / Δtimestamp (binning and rendering overlap, so not summed), memory % as 1 −
  MemAvailable / MemTotal, the SoC temperature from the zone found by its type. The counters are read once per history
  interval (30 s), so CPU and GPU are the exact means over what history records; memory and temperature are the mean
  of 5 s samples. A missing file is null and the UI leaves that series out.
- `pi.cpu_pct`, `pi.gpu_pct`, `pi.mem_pct`, `pi.soc_c` (owner: settings) are in history's and the gateway's lists.
- The chart: the three loads on one 0–100 % axis (CPU blue, GPU amber, memory pink, validated for day and night) and
  the SoC temperature in its own lane in the display unit, sharing the time window (6 h / 24 h / 7 d); live values in
  the legend.

On the bench (Pi 5, e536b45, under systemd as `aetherrv`): both files found by the service itself, values published
(CPU 1.6 %, GPU 0.0 %, memory 4.9 %, SoC 42 °C), and after restarting history and the gateway `/api/history` returned
points for all four.

On the van the same evening (owner yes), heaters idle: a3b02a4 installed (cross-compiled), then only the settings,
history and gateway services and the panel restarted; the Ruuvi listener and the battery service untouched. The service
found the same two files as on the bench. First interval: CPU 11.4 %, GPU 43.1 % (the panel had just restarted and was
drawing its pages), memory 6.7 %, SoC 35.7 °C; history had points for all four. No failed unit, no warnings.

Later that evening (owner): the RP1 and NVMe temperatures join the SoC's (`pi.rp1_c` from hwmon `rp1_adc`, `pi.nvme_c`
from hwmon `nvme`'s "Composite" reading, both found by name; SoC orange, RP1 violet, NVMe green, validated for day
and night with a brighter night green), and the page was too tall, so the load and temperature charts now sit side by
side (stacked at phone width). At the panel's size the Device tab still scrolls 164 px (about 380 px stacked). Bench
(6f562f5): both sensors found, SoC 41.5, RP1 49.5, NVMe 36.9 °C, history points for both.
