# Espar control from AetherRV: design (draft, not implemented)

Status: discovery complete on this van's **Airtronic S2 D2L** + **EasyStart Pro** (2026-11-22). The bus is read
(adapters/espar.py), AetherRV sends read-only UDS diagnostics (espar_uds.py), and the controller is **live**
(adapters/espar_control.py; TxGate device `espar` on since 2026-11-22). Bench steps 1-3 passed: OFF-only takeover,
FAN, and a full HEAT burn ended by the Pi's own timer (regulating on the Ruuvi value the Pi sent; 240 s after-run).
The owner skipped steps 4-6: the heater's own ~15 s shutdown on controller loss was already shown in test 2.

**Owner decisions (2026-11-22):** the Pi **replaces** the EasyStart Pro (kept as the spare); room temperature = the
cabin Ruuvi, else the heater's own intake sensor; run times as on the EasyStart Pro: 1-240 min by the minute,
270-720 in 30-min steps, or unlimited (no extra hard maximum).

## What the captures established

| | This heater (S2 D2L) | Notes |
|---|---|---|
| Bus | 500 kbit/s, 11-bit IDs; heater ↔ one controller | listen-only tap on hat1, now a normal (ACKing) node |
| Controller → heater | 054 every 200 ms (flags, mode, setpoint, room); 055/056/057 static every 200 ms; 60D heartbeat every 100 ms; 065 room every ~10-40 s | all four of 054-057 appear required (S3) |
| Startup burst | 05C-061, 062, 063, 064, 066, 067-06C, 06D, 10A once per wake | our EasyStart Pro's exact bytes are captured |
| Heater → controller | 2C4 state/flags/fault/temp, 2C5, 2C6 every 200 ms; 625 heartbeat 100 ms | |
| Commands | HEAT `09 05 <set LE> <room>`; FAN `09 02 FE FF <room>`; OFF `08 00 FE FF <room>` (`00` flags before any run) | flags bit 0 = on |
| States | 00 init → 03 idle → 09 heating (no separate startup state); 21 fan; after-run = 03 + flags 08 | |
| After-run | 240 s after heat (3 runs); none after fan-only | |
| Run timer | **in the controller**: the EasyStart Pro sends OFF when its time is up | heater runs until told OFF |
| Controller lost | heater stops burning ~15 s later and runs its 240 s after-run; logs P000307/P000440; shows 0x04 briefly on reconnect | **the failsafe is the heater's own** |
| Controller restart | comes back OFF with flags 00; nothing resumes | |
| Bus sleep | ~5-15 s after idle; wakes when the controller transmits (its first frame is retried until ACKed) | |

From the S3 project (to verify on this heater): the heater regulates by itself (PI-like, ~10 %/K, control pause at
temperature with 2C4 still saying "heating"); 054's room word is what it regulates on, and **FE FF while heating
means "0 °C" → full power**; the 60D controller ID isn't checked; two controllers transmitting gives P000342
("too many CAN units"); a power-up lock (0x04 in idle) is released by FAN ≥ 5 s then OFF.

## The one big decision: the EasyStart Pro

The heater expects exactly one controller. Options:

1. **Pi replaces the EasyStart Pro** (unplug it, keep it in a drawer as the fallback). Simplest and the only setup
   the S3 work recommends. A dead Pi = heater stops within ~15 s (safe), no heat until the Pi is back or the EasyStart
   Pro is plugged back in.
2. **Changeover switch**: a 2-position switch/relay that connects either the EasyStart Pro or the Pi to the heater's
   CAN pair (never both). The Pi watches its own health and can drop back by releasing the relay; manual override
   always possible.
3. **Pi as a second controller alongside the EasyStart Pro**: not recommended (P000342, the Pro may send OFF when it
   sees another controller; untested).

## Control loop (option 1 or 2)

- **Takeover**: wait until hat1 has no other controller (no 60D/054 from anyone else for > 2 s), then send our startup
  burst (the EasyStart Pro's captured bytes) every 150 ms until the heater's 625/2C4 appear, then the cyclic set:
  054-057 every 200 ms, 60D every 100 ms, 065 every 10 s. If another controller's frames ever appear: stop
  transmitting immediately and alert ("two controllers").
- **Commands** (through TxGate device `espar`, one approval for the device): HEAT(setpoint), FAN, OFF. 054 is sent
  continuously (that's the protocol), but a *change* of command is a logged, verified action like every other device:
  fresh read → change → verify 2C4 within 2 s (09 / 21 / 03+08) → command_log; never "retried" beyond the protocol's
  own repetition.
- **Room temperature**: always a real value in 054 while heating (never FE FF). Source: the cabin Ruuvi (IAT) if fresh
  (< 2 min), else the heater's own intake reading (FD01 / 2C4 when idle). If neither is fresh: refuse HEAT / send OFF.
- **Run timer in the Pi**: a requested duration (default: until turned off) plus a **hard maximum** (e.g. 4 h, owner
  setting) after which the Pi sends OFF. Survives a backend restart: on restart the Pi starts OFF (like the EasyStart
  Pro), never resumes a run silently.
- **Watchdog**: the cyclic frames come from one asyncio task; if it stalls the heater sees no controller and shuts
  down by itself in ~15 s (proven). If the Pi's hat1 transmit fails (no TX echo, as on hat0), stop and alert.

## Safety checks and alerts

- HEAT commanded, heater not heating after 30 s → alert; power-up lock (0x04) → offer the FAN 5 s → OFF release
  (one button, logged), never automatic.
- Start-up taking > 4 min without flame (FD0F) → alert; heater lockout (P00012B / P00011A active) → alert with the
  fuse-reset procedure (built).
- Supply voltage (FD04) below the heater's cut-off (S3: 10.5 V) → alert before it trips.
- Every command and every automatic OFF (timer, max-run, stale room temperature) in command_log.

## UI

- Home: the Espar button becomes a toggle (HEAT at the last setpoint) like the Rixens buttons; long-press / Climate
  card for FAN and setpoint.
- Climate card: mode buttons (Off / Heat / Fan), setpoint (same ring/slider style), run time (∞ / 30 m / 1 h / 2 h / 4 h),
  the live diagnostic tiles (built), countdowns for run time and after-run.

## Test plan (bench order, each step approved)

1. EasyStart Pro unplugged; Pi takeover burst + OFF only (heater stays idle; check no faults).
2. FAN for 30 s → OFF.
3. HEAT at a low setpoint for 5 min → OFF; after-run completes.
4. Kill the backend mid-run → heater stops within ~15 s (expect P000307/P000440 in memory).
5. Pull hat1's CAN wires mid-run → same, and the Pi raises "lost the heater".
6. Plug the EasyStart Pro back in → it takes over normally.
