# VanPanel — the Pi as Sprinter central console

Status: **planning draft, 2026-11-20.** Nothing has been installed or configured on the Pi yet.

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
