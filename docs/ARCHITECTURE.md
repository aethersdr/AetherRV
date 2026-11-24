# AetherRV in Rust: architecture

Status: **approved by the owner on 2026-11-23** (RUST-CONVERSION-PROMPT §9). Sections marked **[open]** are waiting
on the owner. Answers go into "Decisions" at the end, with their date.

The Python system on `main` is the behaviour reference. When this document doesn't say otherwise, a service does
exactly what the Python module it replaces does: the same rules, timings, state keys, command-log rows and alert
texts.

## 1. Goals

1. **Isolation.** Every module is its own process and systemd unit, and survives the restart, crash or upgrade of
   any other module. Restarting the gateway, the UI, the alert engine or the broker never interrupts a running
   heater.
2. **Responsiveness.** The touchscreen meets the numbers in the prompt's §8: tap ≤ 50 ms, page switch ≤ 150 ms,
   60 fps on Home/Climate/Switches, power-on to usable UI ≤ 15 s, UI process ≤ 400 MB.
3. **The same safety model**, with tests. Every rule lives in the service that owns the device. No rule depends on
   the UI, the gateway or another service being up.

## 2. Process map

All binaries are Rust (`services/*`), built natively on the Pi, installed to `/opt/aetherrv-rs/bin/`, and run under
systemd as the `aetherrv` user unless noted. The units are grouped by `aetherrv.target`.

| Unit | Replaces (Python) | Owns | Publishes (state namespaces) | Commands |
|---|---|---|---|---|
| `aetherrv-espar` | `adapters/espar.py`, `espar_uds.py`, `espar_control.py` | CAN port `hat1` (one socket: decode, control, UDS, capture) | `climate.espar.*`, `espar.*`, `bus.hat1.*` | `espar`: `set` |
| `aetherrv-n2k` | `adapters/n2k.py`, `switching.py`, `FailoverBus` | `hat0` + the Cerbo's socketcand (one logical bus) | `n2k.*`, `switches.*`, `switching.*`, `alternator.*`, `bus.hat0.*`, `bus.n2k_cerbo.*` | `n2k`: `set`, `hold`, `trigger`, (`dim` refused) |
| `aetherrv-can-aux@<bus>` | `CaptureOnlyAdapter`, `adapters/rixens.py` | one spare CAN port each (`fd0`, `fd1`, MCS7 CAN if wired) | `<device>.*`, `bus.<bus>.*` | none (no send path) |
| `aetherrv-victron` | `adapters/victron.py` | MQTT client to the Cerbo GX | `victron.*`, `climate.cabin*`/`outside`/`fridge`/`freezer` temps | `victron`: `inverter_mode`, `shore_limit`, `mppt_mode`, `ack_alarms` |
| `aetherrv-batteries-ble` | `adapters/epoch_ble.py` | BlueZ links to the EPOCH batteries | `batteries.ble*` | none |
| `aetherrv-rixens` | `adapters/rixens_http.py` | MCS7 local HTTP API | `climate.rixens.*` | `rixens`: `setpoint`, `fan`, `furnace`, `electric`, `engine`, `hot_water`, `preheat`, `thermostat` |
| `aetherrv-starlink` | `adapters/starlink.py` | dish gRPC | `starlink.*` | `starlink`: `install_update`, `reboot`, `check_update`, `set_heating`, `set_sleep` |
| `aetherrv-peplink` | `adapters/peplink.py` | router API | `peplink.*`, `wan.*` | `peplink`: `set_wan_priority`, `set_wan_order` |
| `aetherrv-gps` | `adapters/nmea_gps.py` | UDP 10110 | `location.*` | none |
| `aetherrv-netmon` | `adapters/netmon.py` | ping + TCP checks | `netmon.*` | none |
| `aetherrv-weather` | `radar.py`, `traffic.py` | radar cache, motion fields, TomTom proxy | `radar.*`, `traffic.*` | none (HTTP on a Unix socket for the gateway) |
| `aetherrv-power-guard` | `lowbatt.py` | the clean power-off | `power.*` | none (calls the heaters' commands, the admin `poweroff`) |
| `aetherrv-derive` | `climate.py`, `energy.py` | derived values | `climate.cabin.*` (average), `energy.*` | none |
| `aetherrv-settings` | `settings.py` `Prefs`, `AutoTimezone`, `climate.ColdRearm` | `/var/lib/aetherrv/settings.json` | `settings.*` | `settings`: `pref` |
| `aetherrv-alerts` | `alerts.py` | `alert_log` | `alerts.*` | `alerts`: `ack`, `ack_all` |
| `aetherrv-history` | `history.py` | `samples` | `history.*` | none (the gateway reads the DB read-only) |
| `aetherrv-gateway` | `web.py`, `RemoteAuth` | HTTP + WebSocket on :8080, the UI bundle | `svc.*` (module health, derived) | relays UI commands to the owners |
| `aetherrv-bus` | (new) | Mosquitto broker on a Unix socket | — | — |
| `aetherrv-admind.socket/.service` (root) | `deploy/admin/aetherrv_admin.py` | the fixed verb list | — | Unix-socket JSON, as today |
| `aetherrv-panel` (user `kiosk`) | `aetherrv-kiosk.service`, `screen_helper.py` | sway + the chosen UI runtime, screen power | — | — |

Why these boundaries:
- **One process per CAN port.** SocketCAN loops a socket's sent frames back to every *other* socket on the same
  interface. The Espar service tells "another controller is on the bus" from "I hear a 054/60D", which only works if
  no second socket of ours sends on `hat1`. So decode, control, UDS and capture share one socket per port, in the
  service that owns the device. A two-socket vcan test proves the loopback rule (§12).
- **Derived values in their own service**, so the alert engine and the gateway stay thin.
- **Settings in their own service**, because `ColdRearm` writes prefs and `AutoTimezone` needs the admin helper;
  neither belongs in the gateway, which must be restartable at any time.

## 3. Shared crates

| Crate | Contents |
|---|---|
| `aetherrv-core` | config loading (the same TOML contract), secrets loading, `TxGate`, the bus client, the state publisher, the command contract, the command log writer, sd_notify + watchdog, logging to journald, clocks (`CLOCK_BOOTTIME` for persisted timers) |
| `aetherrv-can` | raw SocketCAN with `MSG_CONFIRM` (TX echo) detection, a socketcand client, `FailoverBus`, the TX-echo guard, candump-format capture with 50 MB rotation, link-down recovery (`can_rebind`) |
| `proto-n2k` | 29-bit ID split, fast packet, PGN decoders (127501, 127500, 127751, 126208, 60928, 126996, 127508, 127507), the CANboat-derived PGN table, 126208 command encoding, ISO NAME |
| `proto-espar` | 054/065/2C4/625/60D decode and encode, the startup burst, UDS request building (0x22/0x19 only), ISO-TP reassembly, DID decoders, P-code tables |
| `proto-roypow` | RoyPow BMS query/frame assembly/decode |
| `proto-nmea` | NMEA 0183 RMC/GGA with checksum |
| `proto-mcs7` | `status.xml` parsing (whitelisted fields, no Wi-Fi secrets), the MCS7 CAN decoder |
| `aetherrv-power` | `bank_soc`, `charging`, the heat-lockout tracker (2 min hold, +2 % release): used by the guard **and** inside both heater services |
| `aetherrv-sim` | shared simulator plumbing (vcan helpers, a heater model) |

Crates from crates.io are boring and maintained; licences are MIT, Apache-2.0 or BSD (all GPL-3.0-compatible). The
list with versions is kept in `docs/CRATES.md` as they're added. Mosquitto (EPL-2.0/EDL-1.0) runs as a separate
program; nothing links it.

## 4. Bus contract

### 4.1 Broker

**Mosquitto 2.x** (owner, 2026-11-23) from the Arch Linux ARM `extra` repo (aarch64 package 2.0.22 at the time of
writing). It supports MQTT 5 (response topic, correlation data, message expiry), a Unix-socket
listener (`listener 0 /run/aetherrv/bus/mqtt.sock`), per-username ACLs and retained-message persistence. It's C, not
Rust: it's a system daemon like sway or systemd. The Rust alternative, rumqttd, lists MQTT 5 but has no Unix-socket
listener that I could confirm and much less field use.

- Listener: **only** the Unix socket `/run/aetherrv/bus/mqtt.sock` (directory `root:aetherrv 0750`). No TCP
  listener, so nothing on the LAN or the tailnet can reach the broker. Remote users go through the gateway.
- Persistence on (retained messages survive a broker restart); every service republishes its state on reconnect
  anyway.
- ACL by username (one per service). All services run as one Unix user, so the ACL contains bugs rather than
  attackers: a service may publish only under its own prefixes. The client library enforces the same rule before
  anything is sent.

### 4.2 Topic tree

```
aetherrv/v1/state/<service>/<key>     retained   {"v": <JSON value>, "t": <ms since epoch>}
aetherrv/v1/svc/<service>/status      retained   {"up": true, "pid", "version", "started", "t"}  (last will: "up": false)
aetherrv/v1/svc/<service>/hb          retained   {"t", "seq", "bus_rtt_ms"}  every 2 s
aetherrv/v1/cmd/<device>              not retained, QoS 1, MQTT 5 request (response topic + correlation data)
aetherrv/v1/reply/<client-id>         not retained, QoS 1, the reply
aetherrv/v1/event/<service>/<name>    not retained (one-shot facts, e.g. "kiosk page info")
```

- `<key>` is the **Python state key, unchanged** (dots kept; MQTT allows them), so every key of today's
  `/api/state` is recoverable from the topics. The gateway's flat state (what the UI and the parity tests see) is
  the union of all `state/+/#` payloads' `v`.
- Each key has exactly one owner service. Two keys today are filled by several modules: `tx.enabled` and
  `tx.blocked` (a dict per device). Each service publishes `tx.enabled.<device>` / `tx.blocked.<device>`, and the
  gateway rebuilds the two dicts under their old names, so the UI and the parity diff see today's keys.
- A key whose value becomes `None` is published as `{"v": null}`. A key that disappears is cleared with an empty
  retained payload.

### 4.3 Freshness ("retained is not fresh")

Python judges staleness with `State.age()`, the time since the last `set()`, even when the value didn't change.
Lowbatt (120 s), the Espar room temperature (15 min) and the cabin average depend on it. A retained message looks
new to a fresh subscriber, so:
- every payload carries `t`, the publisher's time of the last `set()` of that key;
- the state publisher republishes an **unchanged** key at most every 10 s (only when it was `set()` again), so a
  consumer's age is never more than 10 s too old; values that change are published at once;
- consumers compute `age = now − t` and apply the Python thresholds to it. All processes share one clock.
- A "fresh read before send" never uses bus state: it is always the owning service's own device read, exactly as
  in Python (the CLMD16 status ≤ 3 s old, a new `status.xml`, an `R/` read on the Cerbo, a new dish call).

### 4.4 Keys left behind by a dead or restarted module

In Python a silent *device* flips its own keys (`"no data"`, `reachable: false`, `asleep`), but a dead *process*
flips nothing, and the broker keeps its retained keys (`climate.espar.state: heating`, `ctl.active: true`) forever.
So:
- **At start**, a service reads the retained keys under its own prefix and, after its first full publish, clears
  every key it no longer publishes (e.g. after a config change moves `climate.cabin.*` to another service).
- **The shared state reader** (used by services and the gateway) treats every key of an owner whose `status` is
  down, or whose heartbeat is older than 6 s, as **unknown**. Each consumer says what unknown means for it; the
  defaults are Python's `None`, except where a heater's safety needs the cautious reading (§8: the guard waits for a
  heater whose service is down as if it were still in its after-run, up to `heater_wait_s`).
- **The gateway** sends a down owner's keys to the UI as `null` (tiles show "—", as they already do without data)
  and sets `svc.<service>.up = false`, so the page can say which module is down.
- **A reconnect republishes with the original `t`**, so a broker restart never makes an old value look fresh.

### 4.5 Key ownership details

- `ui.bms_expected`, `ui.fridge_max_c`, `ui.freezer_max_c` (set from config by Python's `__main__`): published by
  `aetherrv-settings`.
- `tx.enabled.<device>` and `tx.blocked.<device>`: by the service that owns the device (the gateway rebuilds the
  old dicts).
- `climate.cabin.*`: one owner chosen by `[climate] cabin_source`, exactly as in Python: `victron` → the Victron
  service, `rixens` → the MCS7 service, `average` → `aetherrv-derive`. The gateway raises a config alert if two
  services publish the same key.
- `history.degraded` → history; `traffic.available` → weather; `bus.<bus>.*` → the CAN service owning that bus.
- `demo.*`: none. Demo mode is dropped (owner, 2026-11-23); the simulators provide bench data. The migration tool
  warns when `[device.demo]` is enabled.

### 4.6 Commands

Request payload: `{"id", "cmd", "value", "client", "sent_ms", "ttl_ms"}`; MQTT 5 correlation data = `id`, response
topic = the requester's reply topic, message expiry = `ttl_ms`.

- **No retained commands, no queueing.** Services connect with a clean session, so the broker never stores
  commands for an offline service. A service ignores any command with the retain flag set.
- **TTL.** A service refuses a command older than its `ttl_ms` (default 3 s) with "the request arrived too late;
  nothing sent". A late "heat on" never runs. Exempt, because they are the safe direction: a `hold` *release*
  (refusing it would let the dimmer ramp until the 8 s cap) and every heater OFF (owner, 2026-11-23, Q-T1).
- **At most once.** The service keeps the ids of the last 10 minutes and answers a repeat with the first result
  without acting again. (QoS 1 may deliver twice.)
- **Results.** Reply: `{"id", "result": "ok" | "refused" | "unverified", "detail"}`. `detail` is the Python
  `detail` text, shown to the user. The Python command-log outcome maps onto `result` like this:

  | Python outcome | result | Meaning |
  |---|---|---|
  | `ok`, `noop` | ok | done and verified, or already so |
  | `rejected`, `blocked` | refused | nothing was sent |
  | `failed` before any send (no fresh status, device unreachable) | refused | nothing was sent |
  | `failed` on the send itself, `unverified`, `error` | unverified | something may have been sent; not retried |

  The UI's existing `ok` flag stays in the gateway's `control_result` message (`ok = result == "ok"`).
- **The gateway** refuses at once, "<module> is down; nothing sent", when the owner's `status` is down or its
  heartbeat is older than 6 s. When a reply doesn't come within the command's own timeout (per device: Espar 15 s,
  N2K 15 s, MCS7 40 s, Victron 40 s, Peplink 70 s, Starlink 150 s: each service's own verify window plus a queued
  command), the gateway answers "unverified: no answer from <module>; it may have
  acted". The gateway never retries.
- Every attempt (who, what, outcome) goes to `command_log` **in the owning service**, so logging works with the
  broker down (§8). The reply itself carries `result` and `detail` only.

### 4.7 Lifecycle and module health

- On connect: publish `status` (`up: true`), set the last will (`up: false`, retained), then republish every state
  key. Heartbeat every 2 s.
- The gateway derives `svc.<service>.up` and `svc.<service>.age_s` for the UI ("module down" badges). The alert
  engine raises a **new** rule "`<module>` down" (serious for espar, n2k, power-guard and the heater services;
  warning for the rest) after 15 s down. This is an addition the prompt asks for.

### 4.8 Riding out a broker restart

- Every service keeps its device loop running with the bus down. The bus client runs in its own task, with a
  "latest value per key" outbox, so a reconnect publishes the newest state and nothing stale is replayed.
- Reconnect backoff 100 ms doubling to 2 s. On reconnect: resubscribe, republish status, all state, heartbeat.
- What each service *loses* while the bus is down is listed in §11.

## 5. Gateway and the UI protocol

- axum HTTP server on `0.0.0.0:8080` (as today), serving the React bundle (`/`, `/assets/*`) and the same
  endpoints: `/api/state`, `/api/history` (read-only SQLite connection; works while `aetherrv-history` restarts),
  `/api/settings`, `/api/settings/action`, `/api/radar/*` and `/api/traffic/*` (proxied to the weather service's
  Unix socket), `/api/client-info` (kiosk only), `/ws`.
- **Who may control**: `RemoteAuth` ported exactly: loopback = `kiosk`; `control_subnets`; Tailscale peers via
  the admin verb `tailscale_whois` (the owner's own devices, or all with `control_tailnet = "all"`), checked
  against this console's own Tailscale address; 60 s cache; everyone else is view-only. Enforced here, never in
  the UI alone.
- WebSocket messages keep today's shapes (`hello`, `snapshot`, `delta`, `control` → `control_result`, `ack`,
  `ack_all`), plus `result` in `control_result`. Deltas are coalesced over **33 ms** (today 200 ms), so a
  confirmed state change reaches the screen within about two frames. The client store notifies only the components
  subscribed to a changed key.
- The gateway holds no device state of its own. Restarting it costs the UI a reconnect (snapshot again) and
  nothing else.

## 6. The safety model, rule by rule

| Rule | Where it's enforced | Test (§12) |
|---|---|---|
| Deny by default: `tx_enabled` per device, from the Pi's own config | `TxGate` in each owning service; the config is read only at start | gate tests per service |
| Listen-only CAN at kernel level | systemd-networkd `.network` files, unchanged | cutover check |
| Fresh read → send if different → once → verify → never retry → log | each service's `command()`; ported test for test | the 197 ported tests |
| Who may control | gateway `RemoteAuth`; services trust only the gateway's `client` field, and the broker is reachable only through a `0750` Unix socket | auth tests, broker-socket permission test |
| No direct breaker commands (one OFF to clear a trip; trigger safety-off) | `aetherrv-n2k` switching | switching tests |
| Drawer locks trigger-only; `blocked`, `requires_input`, `on_with_input` | `aetherrv-n2k` | switching tests |
| No keypad relay, no automations | `aetherrv-n2k` has no code for them; `switching.automation` is always false | |
| Heat lockout ≤ 10 % (2 min, not charging), heaters off, wait for after-runs before the 5 % power-off | the guard **and inside** `aetherrv-espar` and `aetherrv-rixens` (§8) | lowbatt tests + new lockout isolation tests |
| Root is minimal | `aetherrv-admind`, fixed verbs, validated args, SO_PEERCRED | admin tests |
| Secrets never in logs, state, topics or command lines | config/secrets split; whitelisted parsers (Peplink, MCS7); a test greps every published topic and the logs of a simulator run for the sims' secrets | secret-leak test |
| Fail-safe direction (stale state, other controller, no TX echo, silent heater) | `aetherrv-can` echo guard + `can_reset`; the Espar rules | Espar and failover tests with fault injection |

## 7. The Espar service

### 7.1 Loop

One tokio runtime with the 100 ms control loop on its own task. Nothing in the loop awaits the bus, the database
or the network: bus publishing and DB logging go through bounded channels to other tasks (dropped-oldest with a
counter, never blocking). The loop sends 60D every 100 ms, 054/055/056/057 every 200 ms, 065 every ~10 s and the
burst once per wake, all exactly as `espar_control.py`. A systemd watchdog (`WatchdogSec=2`) is fed from the loop
itself, so a stalled loop is killed and restarted, and then resumes (§7.2).

### 7.2 Seamless resume (owner decisions §3 and 2026-11-23)

The owner's refinement (2026-11-23): **the ~10 s figure is not proven.** How long the heater tolerates a silent
controller before its own shutdown is unknown and will be measured in the van after cutover. The deciding test is
therefore *"is the heater still running what we commanded?"*, not the age of the saved state: if it is, the run
continues uninterrupted. Every timing below is configurable and gets recomputed from the van measurement
(`docs/CUTOVER.md`).

- **Persisted run state** in `/run/aetherrv/espar/run.json` (tmpfs: survives a service restart, cleared by a reboot,
  which the heater can't ride out anyway; `RuntimeDirectoryPreserve=restart`). Written atomically on every change:
  version, boot id, active, mode, target, setpoint, flags byte, `run_min`, the deadline as `CLOCK_BOOTTIME` (immune
  to wall clock jumps, e.g. GPS time), burst done, last room value + source + its time, last bus contact, resume
  history.
- **Liveness stamp** written every 1 s (`alive_boottime`), so the gap since the old process last ran is known (for
  the log and the van measurement).
- **On start**, if the file is from this boot, the old process was active, the format version matches, and fewer
  than 3 resumes happened in the last 60 s (a crash loop must not keep a heater running forever):
  - listen to `hat1` for up to 1 s (`resume_listen_s`). Re-attach when no other controller is heard **and** the
    heater's 2C4 matches the saved command: `heating` for HEAT, `fan` for FAN, idle with the after-run flag for
    OFF-in-after-run.
  - Re-attaching means: resume the cyclic frames at once with the same flags byte, mode, setpoint, deadline and the
    saved room value (never `FE FF`, which means 0 °C, i.e. full power), no wake burst, no OFF. Logged as `resume`
    in `command_log` with the measured gap.
  - A run whose deadline passed during the gap is re-attached too, and the normal timer OFF then fires at once:
    a clean stop with the heater's own after-run, instead of the stored controller-loss fault that silence causes.
- **SIGTERM** (a restart, an upgrade): the service stops transmitting, sends **no** OFF, leaves the run file, and
  exits, so the next process can resume. (A `systemctl stop` that isn't followed by a start leaves the heater to
  stop itself, as a stopped Python backend does today.)
- Otherwise (heater no longer running the saved command, another controller heard, crash loop, a different boot):
  come back OFF as today, i.e. inactive and silent. A heater that has already shut itself down finishes its own
  after-run.
- Restart policy: `Restart=always`, `RestartSec=100ms`, a start-limit wide enough for the crash-loop rule above to
  decide, not systemd.
- The heater simulator's controller-loss timeout is a parameter (default 15 s, the one observation so far), and
  the isolation tests run it at several values.

### 7.3 Running blind (owner decision §3)

"No contact with the bus" = no packet from the broker (any publish, or the keepalive answer) for the grace period
(default 600 s, `[device.espar] blind_off_s`). The time of the last contact is persisted, so a restart doesn't
reset the clock. When it expires while HEAT or FAN runs: a normal OFF (`auto_off`, "no contact with AetherRV's bus
for 10 min"), the after-run is driven to the end, then the idle stop.

### 7.4 Heat lockout inside the service

See §8. The service refuses HEAT while locked out and turns a running HEAT off (at most once a minute), logged as
`auto:low-battery`.

### 7.5 Room temperature

From `aetherrv/v1/state/victron/climate.cabin_front.temp` under `climate.py`'s rule (fresh < 15 min by `t`, Cerbo
connected), else the heater's intake sensor (`FD01`, read by this service's own UDS), else HEAT refused / OFF sent,
exactly as today. With the bus down the intake sensor takes over by itself.

### 7.6 UDS

Read-only, ported as is: requests are built only by a function that refuses any service other than 0x22 and 0x19,
with a test that tries every other service byte.

## 8. Heat lockout without a single point of failure

As in Python, the lockout and the power-off run only with `[power] auto_shutdown = true` (on in the van); all three
enforcement points follow that one switch.

- `aetherrv-power` holds the Python logic: `bank_soc` (shunt while fresh < 120 s, else the Bluetooth mean),
  `charging` (> +1 A), the lockout tracker (≤ 10 % for 120 s and not charging → locked; released by charging or
  > 12 %).
- **The guard acts first, exactly as today** (its tests port one for one): it publishes `power.heat_lockout` (what
  the UI shows), turns a heating heater off through the heaters' own commands (`client = auto:low-battery`, at most
  once a minute), owns `power.heaters_off` (the alert's list of what was done), and runs the 5 % power-off sequence
  (heaters off, wait up to 10 min for both idle, then `poweroff`). While waiting, a heater whose service is down
  counts as busy (§4.4), so the wait runs its full `heater_wait_s` rather than powering off early.
- **Each heater service also runs the tracker itself**, on the raw `victron.battery.*` and `batteries.ble*` topics,
  and is locked out if **either** its own tracker or a fresh (< 30 s) `power.heat_lockout` from the guard says so:
  - it always **refuses heat starts** while locked out;
  - it **turns its own heater off** only as the backstop, when the guard's lockout key isn't fresh (guard down or
    stale), at most once a minute, logged as `auto:low-battery` with "guard not answering" in the detail, and
    published as `climate.espar.lockout_off` / `climate.rixens.lockout_off`.

  A guard that is down removes nothing; a guard that is up is the only one sending, so there are no duplicate OFFs.
- The latch is **persisted** in `/var/lib/aetherrv/<service>/lockout.json`, so a restart or reboot at 9 % stays
  locked until fresh readings show charging or > 12 %. (Python forgets the lockout on restart; this is a deliberate
  improvement.)
- With no fresh SoC at all (Victron and Bluetooth silent, or the bus down): the latched value holds, and an
  unlatched heater **may start heat**, as today (owner, 2026-11-23: an unknown SoC never blocks heat).

## 9. Data and storage

| Data | Where | Writers | Readers |
|---|---|---|---|
| `samples` | `/var/lib/aetherrv/history.sqlite3` (unchanged schema, WAL) | history | gateway (read-only) |
| `alert_log` | same file | alerts | alerts |
| `command_log` | same file | each controlling service | (diagnostics) |
| `counters` (MCS7 fuel today) | same file | rixens | rixens |
| prefs | `/var/lib/aetherrv/settings.json` (unchanged format) | settings | settings |
| radar cache | `/var/lib/aetherrv/radar/<t>/<z>/<x>/<y>.png`, `flow.json` (unchanged layout) | weather | weather |
| captures | `/var/lib/aetherrv/captures/<bus>-<stamp>.log` (candump format, unchanged) | each CAN service | pruned by `aetherrv-prune.timer` |
| Espar run state | `/run/aetherrv/espar/run.json` | espar | espar |
| lockout latches | `/var/lib/aetherrv/<service>/lockout.json` | espar, rixens, power-guard | same |

- One SQLite file, several writer processes: WAL mode with `busy_timeout`, every write from a worker thread, so a
  lock held elsewhere never stalls a control loop. A write that fails is logged to the journal instead.
- **Keeping the files and schema means the rollback needs no data conversion**: the Python system reads the same
  files. The migration tool (`aetherrv-migrate`) checks and, where needed, converts config, secrets, history,
  settings and the radar cache, and is tested against copies of the real files.

## 10. Root helper, kiosk and UI runtime

- `aetherrv-admind`: the Python helper's verbs ported one for one, with the same validation patterns, argv-only
  commands, secrets on stdin only, `SO_PEERCRED` allowing root and `aetherrv`. `restart_kiosk` restarts
  `aetherrv-panel`. Socket path unchanged (`/run/aetherrv-admin.sock`); the unit conflicts with the Python one.
- `aetherrv-panel`: sway (as today, for output power) plus the UI runtime chosen by the benchmark (WPE `cog`,
  Tauri v2 or Chromium; prompt §7), and `aetherrv-screen` (Rust, sway IPC; replaces `screen_helper.py`, same HTTP
  API on 127.0.0.1:8091). The Plymouth hand-off (`plymouth quit --retain-splash` at kiosk start) stays.
- No Python on the Pi at runtime: the admin helper, the screen helper, the update script's helpers and the tools
  that run on the Pi (pairing, Peplink user setup, boot-splash install) are ported to Rust or shell.

## 11. Failure behaviour

| Down | Effect | Recovers |
|---|---|---|
| broker | Every device loop continues. Commands stop (the gateway refuses with "bus down"). The UI shows the last values, marked stale after 6 s. The Espar keeps its run; after `blind_off_s` it turns itself off. Alerts and history pause. | services republish on reconnect; history resumes |
| gateway | The UI reconnects; nothing else notices. | UI snapshot on reconnect |
| UI / panel | Nothing else notices. | |
| espar | The run continues if the heater is still running the saved command when the service is back (§7.2); otherwise OFF as today. | |
| n2k | Switch tiles show "module down"; the CLMD16 and keypads work on their own (dual control). | |
| victron | Energy values stale; the heater services' own lockout trackers hold their latch; the Espar falls back to its intake sensor after 15 min. | |
| rixens | MCS7 runs on its own (it is its own thermostat). | |
| power-guard | Lockout still enforced inside the heater services; the 5 % power-off doesn't happen until it's back. | |
| alerts / history / derive / settings / weather / netmon / gps / peplink / starlink | Their own values pause; nothing else changes. | |
| admin helper | Settings actions fail with an error; CAN port resets wait. | socket activation |

## 12. How it's proven

- **Unit tests** per crate, ported from the Python tests (mapping in `docs/TEST-PARITY.md`, all 197 collected ids).
- **Simulators** (plain processes, wire level): Espar heater on vcan (states, after-run, bus sleep, the 15 s
  controller-loss shutdown with P000307/P000440 in fault memory, UDS), CLMD16 + CKM12 keypads on vcan, MCS7 HTTP
  (from the real `status.xml` fixture), the Cerbo (a second Mosquitto on a loopback port plus a simulator client
  answering `R/` and `W/`), Starlink gRPC (with reflection), Peplink HTTPS, GPS over UDP, BLE batteries (a mock
  behind the BlueZ trait).
- **Fault injection** in `aetherrv-can`'s transport seam (no TX echo, ENOBUFS, link down), because vcan always
  echoes and never loses an ACK. The real MCP2515 behaviour is a van check in `docs/CUTOVER.md`.
- **A two-socket vcan test** shows that a second socket sees the first one's frames, which is why one service owns
  each port.
- **Golden parity**: the Python reference (from `main`) and the Rust system are fed the same replayed captures and
  simulator inputs; `harness/parity/parity.py` diffs `/api/state` key by key. Owner rule (Q-P2): fields that depend
  on when each system sampled or started (ages, `since`/`t` times within 1.5 s, CAN frame rates, alert raise times
  skewed by the start-time difference) compare as equal within that tolerance; every other key must match exactly.
- **Isolation tests** (`tools/isolation`): the full system on simulators; kill and restart each service and the
  broker in turn; assert the others keep working, state recovers, a simulated Espar run and the MCS7 are not
  interrupted (the heater simulator logs every gap and any controller-loss fault).
- **Soak**: 1 h on the bench Pi against the simulators (owner, 2026-11-23: at most an hour instead of the prompt's 12 h), with memory, restarts, state freshness and every command's result logged.
- **UI tests** (Playwright) only against simulators or a gateway whose command path is stubbed, never a live page.

## 13. Build and deploy

- Cargo workspace at the repo root (`crates/`, `services/`, `sim/`, `tools/`), `ui/` (React + TypeScript + Vite,
  every dependency vendored through the lockfile and bundled; no CDN). Native build on the Pi (`cargo build
  --release`), the UI bundle built with Node on the workstation or the Pi.
- `packaging/`: systemd units, `aetherrv.target`, the Mosquitto config, `install.sh`, and `push.sh`, which has **no
  default host** (it never reads `deploy/local.env`, whose host is the van). Python's `deploy/` stays as it is for
  the rollback. Until the cutover, `push.sh` and `install.sh` **refuse a host that has `hat0` or `hat1`**, i.e. the
  van; the bench Pi has no CAN HAT. The cutover script lifts that guard explicitly.
- All services run as the one `aetherrv` user, as today, so the admin helper's caller check is unchanged.
  Per-service users would tighten the broker ACL and the admin verbs per caller, at the cost of a shared group for
  the database and state directory; it can be added later without changing the design.
- Paths: binaries and the UI under `/opt/aetherrv-rs/` (the Python tree at `/opt/aetherrv/` is left alone),
  config `/etc/aetherrv/` (shared), state `/var/lib/aetherrv/` (shared).
- Cutover and rollback are one-command scripts (`packaging/cutover.sh`, `packaging/rollback.sh`) that stop one
  target and start the other. Both are tested on the bench Pi. The runbook is `docs/CUTOVER.md`.

## 13a. Where the Rust system deliberately differs from the Python reference

Each of these changes what a device is sent or what a user sees, so each is listed for the owner (§9).

| # | Difference | Why |
|---|---|---|
| D1 | All Espar timing (heard/lost/wake/verify, run deadline, after-run countdown) runs on `CLOCK_BOOTTIME`, not the wall clock | In Python a forward clock jump during a run (NTP sync after boot on the RTC-less Pi) reads as "lost the heater (no status for 3 s)" and stops the heater; a backward jump stretches the run timer |
| D2 | Ages of bus values (the cabin Ruuvi, battery readings) are still wall-clock (`t` in the payload). A forward jump of over 15 min makes the Ruuvi look stale; the Espar then uses its intake sensor, or with diagnostics off stops heat (same as Python) | One clock shared by all processes; only the in-process Espar loop needs a monotonic clock |
| D3 | After a resume, the room value saved before the restart is used for up to 15 min if neither the Ruuvi nor the intake sensor is available yet | The first 054 after a resume must carry a real room value (never FE FF = full power), and the bus may still be reconnecting |
| D4 | A command older than its TTL (3 s) is refused, except the safe direction: a dimmer `hold` release and every heater OFF (Espar `set` mode off; MCS7 furnace / electric / hot water / pre-heat off) still run | A late "heat on" must never run (owner, Q-T1) |
| D5 | `tx_enabled` / `diag_tx` must be TOML booleans; Python's `bool()` would also accept e.g. the string `"false"` as true | Safe direction; `aetherrv-migrate` warns |
| D6 | The heat-lockout latch is persisted (Python forgets it on restart) | Owner (Q-L1) |
| D7 | New state keys: `climate.espar.lockout`, `climate.espar.lockout_off`, `climate.rixens.lockout*`, `svc.*`, `tx.enabled.<device>`, `tx.blocked.<device>` (the gateway also rebuilds the old `tx.enabled`/`tx.blocked` dicts) | §4.5, §8 |
| D8 | The Espar resumes a run after its own restart (owner, §3 / Q-E1, Q-E2); Python always came back OFF | Owner |
| D9 | The command reply carries `result` (ok / refused / unverified) and `detail`; the Python outcome string stays in `command_log`, not in the reply | §4.6 |
| D11 | On/off command values must be JSON booleans and numbers JSON numbers (CLMD16 `set`/`hold`, MCS7 sources and modes, Victron); anything else is refused with "bad request". Python's `bool("false")` is True, so a string "false" would have switched a device ON | Safe direction; the UI always sends real booleans |
| D12 | The alert engine reads a down module's keys at their last known values; the outage is the new "<module> down" alert (serious for espar, n2k, power-guard, rixens). Without this, every alert of a restarting module would clear and come back un-acked | Python had one process: its alerts never saw a module go away |
| D13 | Radar motion fields: a pure-Rust coarse-to-fine patch Lucas–Kanade instead of OpenCV DIS (owner, Q-R1). On synthetic radar the median shift matches DIS within 0.1 px; on growing echoes and rotation it differs more (numbers in the weather service's tests). `flow.json` is written atomically | Owner decision; the real side-by-side is for the owner |
| D14 | Settings prefs and Victron command values must have the right JSON type (like D11): Python's `bool()`/`int()` coercion would, for example, have sent MultiPlus mode 1 (charger only) for `true` | Safe direction |
| D15 | Weather HTTP: no Range requests, every TomTom failure is a 502 (Python: 500 for unexpected errors); the van's last GPS position is kept while the GPS module is down | Only the UI's own fetches use these |
| D16 | Bluetooth batteries: the reconnect backoff never resets after a good session (Python's reset was unreachable code; ported as is) | Parity; the owner may prefer a reset |
| D17 | `location.*`: Python's Peplink adapter and the GPS listener both wrote them; now the GPS service owns them and the Peplink service publishes its position only when `[gps]` is disabled | One owner per key (§4.5) |
| D18 | Starlink and Peplink command values need the right JSON type (like D11/D14) | Safe direction |
| D19 | An alert still active when the alerts service restarts (same boot) carries on with its start time and acknowledgement and is not raised again; one that cleared meanwhile is closed in the log; a reboot re-raises as Python did (owner, Q-A2) | Python restarted as a whole; a single module restarting must not re-chime acknowledged alerts |
| D20 | Starlink "check for update": a failed check request whose update state moved anyway answers **unverified** with the dish's state (Python: ok) (owner, Q-S1) | The request itself failed |
| D10 | The gateway waits up to 15 s for an Espar reply (one verify window is 6 s, and a queued command waits for the one before it) | Avoids reporting "unverified" for a command that is still being verified |

## 14. Open questions for the owner

| Id | Question | State |
|---|---|---|
| Q-A1 | Broker | answered 2026-11-23: Mosquitto |
| Q-E1 | Seamless resume re-attach test and crash-loop limit | answered 2026-11-23: as proposed; timings to be measured in the van |
| Q-E2 | Espar back after a longer gap | answered 2026-11-23: if the heater still runs the saved command, continue uninterrupted |
| Q-L1 | No fresh battery reading: may HEAT start? | answered 2026-11-23: yes, as today |
| Q-R1 | Radar motion fields | answered 2026-11-23: pure-Rust optical flow; the visual difference goes to the owner for acceptance |
| Q-P1 | Recording API inputs through the van Pi | answered 2026-11-23: yes, read-only requests only, through the logged wrapper |
| Q-P2 | Parity normalisation of time-dependent keys | answered 2026-11-23: time/age/rate fields within tolerance, alert raise times may differ by the start skew; all else exact |
| Q-D1 | Demo mode: port or drop? | answered 2026-11-23: drop it |
| Q-T1 | Should a late heater OFF (older than the 3 s TTL) still run, as the safe direction? (D4) | answered 2026-11-23: yes |
| Q-D2 | Accept the differences D1–D10 in §13a | answered 2026-11-23: all accepted |
| Q-D3 | Accept D11–D18 | answered 2026-11-23: all accepted |
| Q-U1 | WPE candidate: `cog` is not packaged for Arch ARM and targets the legacy libwpe API | answered 2026-11-23: benchmark WPE through its MiniBrowser (wpewebkit 2.52, WPE Platform API); if WPE wins, ship a small WPE Platform kiosk launcher |
| Q-U2 | Bench monitor not detected on HDMI | answered 2026-11-23: develop the benchmark headless for now; the owner reports when the monitor is back |
| Q-U3 | Home page MCS7 heat buttons send without a confirmation (as app.js) | answered 2026-11-23: keep as today (optimistic light, rollback on failure) |
| Q-U4 | Home inverter overlay changes the MultiPlus mode without a confirmation (Energy asks first) | answered 2026-11-23: keep as today |
| Q-A2 | Acknowledged alerts when the alerts service restarts on its own | answered 2026-11-23: keep the acks (D19) |
| Q-S1 | Starlink check_update: request failed but the update state moved | answered 2026-11-23: unverified (D20) |
| Q-U5 | Settings: Wi-Fi off / join / rename act without a confirmation | answered 2026-11-23: confirm Wi-Fi off only (remote access over Wi-Fi drops) |
| Q-U6 | Energy "3D battery bank": today an SVG scene; keep it or build a WebGL bank? | answered 2026-11-23: build a real WebGL (three.js) bank; it must hold ≥ 30 fps on the Pi |
| Q-B1 | Bench Pi; workstation toolchain | answered 2026-11-23: bench host provided (kept out of the repo, in a gitignored env file); sudo on the workstation for dependencies |

## Decisions

| Date | Decision | Source |
|---|---|---|
| 2026-11-23 | All Rust, no Python at runtime (BMS, Starlink gRPC, N2K, Espar + UDS, radar motion included) | owner, prompt §3 |
| 2026-11-23 | One service per device plus core services, each its own binary and systemd unit | owner, prompt §3 |
| 2026-11-23 | No containers at all: native binaries under systemd, native build, simulators as plain processes | owner, prompt §3 |
| 2026-11-23 | A local MQTT broker with retained state | owner, prompt §3 |
| 2026-11-23 | React (TypeScript), same layout on every page; better motion, feedback and graphics | owner, prompt §3 |
| 2026-11-23 | UI runtime chosen by benchmark on a Pi 5: WPE `cog`, Tauri v2, Chromium | owner, prompt §3 |
| 2026-11-23 | Phones and laptops use the same React app over the LANs and the tailnet, with today's control rules | owner, prompt §3 |
| 2026-11-23 | Build everything first, then one cutover; stop before the van (runbook + rollback only) | owner, prompt §3 |
| 2026-11-23 | Bench: second Pi 5 16 GB, NVMe, Arch Linux ARM, no CAN HAT; 13.3" 2560×1440 non-touch HDMI monitor | owner, prompt §3, §8 |
| 2026-11-23 | Same repo, branch `rust`; Python untouched on `main` | owner, prompt §3 |
| 2026-11-23 | Existing config, secrets, history, settings and radar cache keep working or are migrated by a tested tool | owner, prompt §3 |
| 2026-11-23 | Espar restart ≤ ~10 s: resume seamlessly; longer: back OFF | owner, prompt §3 |
| 2026-11-23 | Espar with no bus contact for a grace period (default 10 min, configurable): OFF, cool down | owner, prompt §3 |
| 2026-11-23 | Acceptance numbers: the strict set in prompt §8 | owner, prompt §3 |
| 2026-11-23 | Broker: Mosquitto 2.x on a Unix socket only | owner (Q-A1) |
| 2026-11-23 | Espar resume: re-attach when no other controller is heard and the heater's 2C4 matches the saved command; max 3 resumes per 60 s. The 10 s figure is unproven; the heater's real controller-loss tolerance is measured in the van and the timings recomputed | owner (Q-E1) |
| 2026-11-23 | Espar back after a longer gap: if the heater is still running the saved command, continue uninterrupted (age is not the criterion); otherwise inactive and silent, as today | owner (Q-E2) |
| 2026-11-23 | Heat lockout enforced in the guard and inside both heater services, latch persisted; an unknown SoC never blocks a heat start | owner (Q-L1) |
| 2026-11-23 | Radar motion fields: pure-Rust optical flow, difference from DIS shown to the owner for acceptance | owner (Q-R1) |
| 2026-11-23 | Van Pi: read-only access through a logged wrapper; recording device responses with read-only requests (GET, subscribe, status) is allowed; no installs, restarts or writes | owner (Q-P1) |
| 2026-11-23 | Architecture approved as written in this document (commit 31dde83) | owner |
| 2026-11-23 | Demo mode dropped; simulators replace it | owner (Q-D1) |
| 2026-11-23 | Late heater OFF commands are exempt from the command TTL (safe direction); late ONs are refused | owner (Q-T1) |
| 2026-11-23 | Differences D1–D10 (§13a) accepted | owner (Q-D2) |
| 2026-11-23 | Workstation: sudo allowed to install the build and test dependencies (Rust, Mosquitto, vcan, …) | owner (Q-B1) |
| 2026-11-23 | Golden-parity rule: sampling-time fields within tolerance, everything else exact | owner (Q-P2) |
| 2026-11-23 | Differences D11–D18 (§13a) accepted | owner (Q-D3) |
| 2026-11-23 | WPE is benchmarked through MiniBrowser (WPE Platform API) instead of `cog` | owner (Q-U1) |
| 2026-11-23 | Home MCS7 heat buttons and the Home inverter overlay keep sending without a confirmation, as today | owner (Q-U3, Q-U4) |
| 2026-11-23 | Alerts keep their acknowledgement over a restart of the alerts service within one boot (D19) | owner (Q-A2) |
| 2026-11-23 | Starlink check_update with a failed request answers unverified (D20) | owner (Q-S1) |
| 2026-11-23 | Settings asks before turning Wi-Fi off; join and rename stay one step | owner (Q-U5) |
| 2026-11-23 | Energy page: a WebGL (three.js) battery bank replaces the SVG scene | owner (Q-U6) |
| 2026-11-23 | Soak: at most 1 hour on the bench (replaces the prompt's 12 hours, goal item 5) | owner |
