"""Espar (Eberspächer) Airtronic S2 D2L + EasyStart Pro on its own CAN bus (500 kbit/s, 11-bit IDs). READ-ONLY.

Frame map: the public Airtronic S3 B2L work (TrooperDuper/espar-airtronic-esphome, docs/protocol-reference.md),
checked against this van's own captures on 2026-11-22 (wake/handshake, heat at 80 °F, setpoint 78 °F, off,
4-minute after-run, bus sleep). Facts only; no code taken from that project.

  0x054 controller → heater, 200 ms   D1 flags (00 never run, 09 on, 08 after a run; bit 0 = on)
                                       D2 mode (05 heat, 02 fan, 00 off)   D3-D4 setpoint 0.1 °C LE (FE FF = none)
                                       D5-D6 room (°C + 50) × 10 LE (FE FF = none)
  0x065 controller, ~10 s            D1-D2 room word as above
  0x2C4 heater → controller, 200 ms   D1 state (00 init, 01 stopping, 02 startup, 03 idle, 08 init, 09 heating,
                                       0B fault, 21 fan)   D2 flags (04 running ~5 min in, 08 after-run)
                                       D3 fault code   D5-D6 temperature, (°C + 50) × 10 LE: while running a copy
                                       of the controller's room value; otherwise the heater's own sensor
  0x625 heater heartbeat 100 ms; 0x60D controller heartbeat 100 ms

The bus sleeps ~5-15 s after the heater goes idle and wakes on any EasyStart Pro press: silence is "asleep",
not a fault. Fan speed, fuel pump and power level are not in these frames (only via UDS diagnostics).
"""

from __future__ import annotations

import logging
import time

import can

from ..state import State
from . import espar_uds as uds
from .capture_only import CaptureOnlyAdapter

log = logging.getLogger(__name__)

STATES = {0x00: "init", 0x01: "stopping", 0x02: "startup", 0x03: "idle", 0x08: "init", 0x09: "heating",
          0x0B: "fault", 0x21: "fan"}
FAULTS = {0x04: "power-up lock (heater ignores commands until it gets a non-idle command then idle)",
          0x20: "flame loss / fuel starvation", 0x40: "overtemperature / exhaust blocked"}
AFTER_RUN_S = 240            # measured: 240.0 s from OFF to idle on 2026-11-22
ASLEEP_S = 20                # no frames this long → the bus is asleep (normal when idle)


def temp_word(lo: int, hi: int) -> float | None:
    w = lo | hi << 8
    return None if w in (0xFFFE, 0xFFFF) else round(w / 10 - 50, 1)


def _cf(c: float | None) -> str:
    return "n/a" if c is None else f"{c:.1f} °C ({c * 9 / 5 + 32:.0f} °F)"


def _decode(i: int, d: bytes) -> tuple[str, str]:
    """(what the frame is, its decoded value) for the Systems page's frame list."""
    w = lambda a: d[a] | d[a + 1] << 8 if len(d) > a + 1 else None                      # noqa: E731
    if i == 0x054 and len(d) >= 6:
        sp = w(2)
        return ("EasyStart Pro command", f"{ {0x05: 'heat', 0x02: 'fan', 0x00: 'off'}.get(d[1], hex(d[1])) } · "
                f"set {'n/a' if sp in (0xFFFE, 0xFFFF) else _cf(sp / 10)} · room {_cf(temp_word(d[4], d[5]))} · flags {d[0]:02x}")
    if i == 0x065 and len(d) >= 3:
        return ("EasyStart Pro room temperature", f"{_cf(temp_word(d[0], d[1]))} · #{d[2]}")
    if i == 0x2C4 and len(d) >= 6:
        f = d[2]
        return ("Heater status", f"{STATES.get(d[0], hex(d[0]))} · flags {d[1]:02x}"
                f"{' (after-run)' if d[1] & 0x08 else ' (running)' if d[1] & 0x04 else ''} · "
                f"{'fault ' + hex(f) + ' ' + FAULTS.get(f, '') if f else 'no fault'} · temp {_cf(temp_word(d[4], d[5]))}")
    if i == 0x2C5:
        return ("Heater status 2", "reserved")
    if i == 0x2C6:
        return ("Heater config ID", d.hex(" ") if d[:2] != b"\xfe\xff" else "n/a (starting)")
    if i == 0x625 and len(d) >= 3:
        return ("Heater heartbeat", f"counter {d[2]:02x} · {'tick' if d[1] else 'tock'}")
    if i == 0x60D and len(d) >= 6:
        return ("EasyStart Pro heartbeat", f"controller {d[2]:02X}{d[3]:02X}{d[4]:02X}")
    if i in (0x055, 0x056, 0x057):
        return ("EasyStart Pro (static)", "required with 054")
    if i in (0x05C, 0x05F) and len(d) >= 1:
        return ("Preset run time", f"{d[0]} min")
    if i in (0x05D, 0x05E, 0x060, 0x061, 0x066):
        return ("EasyStart Pro settings", "")
    if i == 0x062:
        return ("EasyStart Pro type", d.hex(" "))
    if i == 0x063 and len(d) >= 2:
        return ("Room sensor settings", f"offset {(d[0] - 50) / 10:+.1f} °C · sensor {'control unit' if d[1] == 1 else 'none'}")
    if i == 0x064:
        return ("Service interval", "off" if d[6:8] == b"\xff\x03" else d[6:8].hex(" "))
    if 0x067 <= i <= 0x06C:
        return (f"Timer {i - 0x066}", d.hex(" "))
    if i == 0x06D:
        return ("EasyStart Pro settings", "")
    if i == 0x10A:
        return ("Clock", d.hex(" "))
    if i == 0x7A0:
        return ("Diagnostic request", "flow control" if d[:1] == b"\x30" else d.hex(" "))
    if i == 0x73C:
        return ("Diagnostic reply", d.hex(" "))
    return ("", "")


class EsparAdapter(CaptureOnlyAdapter):
    """Decodes the heater's status and the EasyStart Pro's commands; never transmits (it has no send path)."""

    SCAN_DIDS = list(range(0xFD00, 0xFE00))         # owner-approved read-only scan (looking for the outlet temperature)
    SCAN_GAP_S, SCAN_WATCH_S = 0.3, 1800.0

    def __init__(self, device: str, state: State, bus=None, gate=None, diag_interval: float = 5.0,
                 scan: bool = False) -> None:
        super().__init__(device, state)
        # Diagnostics: UDS *read* requests through TxGate device "<device>_diag" (off unless diag_tx = true).
        self.bus, self.gate, self.diag_interval = bus, gate, max(1.0, diag_interval)
        self.isotp = uds.IsoTpRx()
        self.diag: dict = {}                  # key → (value, time)
        self.dtcs: list | None = None
        self.diag_sent = 0
        self.diag_err: str | None = None
        self.cmd: dict = {}
        self.st: dict = {}
        self.last_rx: float | None = None
        self.last_heater: float | None = None
        self.last_ctrl: float | None = None
        self.heat_since: float | None = None
        self.heat_cmd_since: float | None = None      # when the controller started commanding HEAT
        # 054/60D we *receive* come from another controller (an EasyStart Pro): we never receive our own frames
        self.last_other_ctrl: float | None = None
        self.on_other_controller = None
        self.ctl_active = False                       # set by EsparControl while AetherRV drives the heater
        self.after_since: float | None = None
        # Read-only DID scan (owner-approved, looking for the outlet temperature): pass 1 asks every FDxx once and
        # keeps the raw replies; pass 2 keeps re-reading the DIDs that answered but aren't decoded yet and logs each
        # change, so a value that tracks the outlet temperature shows up against the flame/intake readings.
        self.scan_todo: list[int] = list(self.SCAN_DIDS) if scan else []
        self.scan_res: dict[int, str] = {}           # DID → raw reply hex | "NRC 0xNN"
        self.scan_wait: int | None = None            # DID of the outstanding scan request
        self.scan_watch: list[int] = []
        self.scan_until: float | None = None

    def on_frame(self, msg: can.Message) -> None:
        super().on_frame(msg)
        if msg.is_error_frame or msg.is_remote_frame or msg.is_extended_id:
            return
        now, d, i = time.time(), bytes(msg.data), msg.arbitration_id
        if i == uds.RESP_ID:                          # a diagnostic reply (ours, or the EasyStart Pro's workshop menu)
            payload, need_fc = self.isotp.feed(d, now)
            if need_fc:
                self._send_diag(uds.FLOW_CONTROL, "flow control")
            if payload:
                self._scan_reply(payload)
                r = uds.decode(payload)
                if "key" in r:
                    self.diag[r["key"]] = (r["value"], now)
                elif "dtcs" in r:
                    self.dtcs = r["dtcs"]
                elif "negative" in r:
                    self.diag_err = f"negative reply {r['negative']}"
            return
        if i == uds.REQ_ID:
            return
        self.last_rx = now
        if i == 0x054 and len(d) >= 6:
            sp = d[2] | d[3] << 8
            self.cmd = {"flags": d[0], "mode": {0x05: "heat", 0x02: "fan", 0x00: "off"}.get(d[1], f"0x{d[1]:02x}"),
                        "setpoint_c": None if sp in (0xFFFE, 0xFFFF) else round(sp / 10, 1),
                        "room_c": temp_word(d[4], d[5])}
            if self.cmd["mode"] == "heat":
                self.heat_cmd_since = self.heat_cmd_since or now
            else:
                self.heat_cmd_since = None
            self.last_ctrl = self.last_other_ctrl = now
            if self.on_other_controller:
                self.on_other_controller()
        elif i == 0x065 and len(d) >= 2:
            r = temp_word(d[0], d[1])
            if r is not None:
                self.cmd["room_c"] = r
        elif i in (0x60D,):
            self.last_ctrl = self.last_other_ctrl = now
            if self.on_other_controller:
                self.on_other_controller()
        elif i == 0x2C4 and len(d) >= 6:
            code, flags = d[0], d[1]
            name = STATES.get(code, f"0x{code:02x}")
            if name == "heating" and self.heat_since is None:
                self.heat_since = now
            if name != "heating":
                self.heat_since = None
            after = name == "idle" and bool(flags & 0x08)
            if after and self.after_since is None:
                self.after_since = now
            if not after:
                self.after_since = None
            self.st = {"code": code, "name": name, "flags": flags, "fault": d[2], "temp_c": temp_word(d[4], d[5])}
            self.last_heater = now
        elif i == 0x625:
            self.last_heater = now

    def summary(self) -> list[dict]:
        rows = []
        for r in super().summary():
            aid = int(r["id"], 16)
            ext = len(r["id"]) > 3
            name, val = ("wake-up (29-bit)", "") if ext else _decode(aid, bytes.fromhex(r["data"].replace(" ", "")))
            rows.append({"id": r["id"], "frame": name, "decoded": val, "count": r["count"], "data": r["data"], "age": r["age"]})
        return rows

    def snapshot(self, now: float | None = None) -> dict:
        now = now or time.time()
        p = "climate.espar."
        asleep = self.last_rx is None or now - self.last_rx > ASLEEP_S
        st, cmd = self.st, self.cmd
        if self.last_other_ctrl is None or now - self.last_other_ctrl > 3:
            cmd = {}                                  # the EasyStart Pro's last command expires once it's gone
        if asleep:                                        # also: nothing heard yet since start (it sleeps when idle)
            state = "off"
        elif st.get("name") == "fault" or (st.get("fault") and st.get("name") != "idle"):
            state = "error"
        elif st.get("name") == "heating":
            state = "heating"
        elif st.get("name") == "fan":
            state = "fan"
        elif st.get("name") in ("startup", "init", "stopping"):
            state = "starting" if cmd.get("mode") in ("heat", "fan") else "off"    # a wake-up's 0.2 s init
        elif self.after_since is not None:
            state = "cooldown"
        else:
            state = "off"
        fault = st.get("fault") or 0
        running = state in ("heating", "fan", "starting", "cooldown")
        # behavioural checks (the S3 project's): HEAT commanded but the heater isn't heating after 30 s;
        # the power-up lock (fault 0x04 while idle) makes it ignore HEAT/FAN until a ≥5 s FAN, then OFF
        issue = None
        if not asleep and self.heat_cmd_since and now - self.heat_cmd_since > 30 and st.get("name") != "heating":
            issue = ("power-up lock: the heater ignores HEAT after a power interruption. Run FAN for 5 s, then OFF, then HEAT"
                     if fault == 0x04 else "heat commanded for 30 s but the heater isn't heating")
        return {
            p + "state": state,
            p + "asleep": asleep,
            p + "heard": self.last_rx is not None,
            p + "mode": None if asleep else cmd.get("mode"),
            p + "setpoint_c": None if asleep or cmd.get("mode") != "heat" else cmd.get("setpoint_c"),
            p + "room_c": None if asleep else cmd.get("room_c"),
            # while running the heater's temperature field copies the room value; only report its own sensor
            p + "heater_c": None if asleep or running else st.get("temp_c"),
            p + "fault": f"0x{fault:02x}" if fault else None,
            p + "fault_text": FAULTS.get(fault, "unknown fault code") if fault else None,
            p + "run_s": round(now - self.heat_since) if self.heat_since and not asleep else None,
            p + "cooldown_left_s": max(0, round(AFTER_RUN_S - (now - self.after_since)))
            if self.after_since and not asleep else None,
            p + "controller": None if self.last_ctrl is None else now - self.last_ctrl < ASLEEP_S,
            p + "issue": issue,
            p + "last_seen": self.last_rx,
        }

    @property
    def diag_enabled(self) -> bool:
        return self.bus is not None and self.gate is not None and self.gate.is_enabled(f"{self.device}_diag")

    def _scan_reply(self, payload: bytes) -> None:
        did = payload[1] << 8 | payload[2] if payload[0] == 0x62 and len(payload) >= 3 else None
        if did is None and payload[:2] == b"\x7f\x22" and self.scan_wait is not None and len(payload) >= 3:
            did, val = self.scan_wait, f"NRC 0x{payload[2]:02x}"
        elif did is not None and (did == self.scan_wait or did in self.scan_res):
            val = payload[3:].hex(" ")
        else:
            return
        if did == self.scan_wait:
            self.scan_wait = None
        if val != self.scan_res.get(did):
            if self.scan_res.get(did) is not None or not val.startswith("NRC"):
                log.info("espar scan %04X: %s", did, val)
            self.scan_res[did] = val

    def _scan_step(self, now: float) -> bool:
        """One scan request if one is due (returns True if it sent). Read service 0x22 only (uds.read_did)."""
        if self.scan_todo:
            did = self.scan_todo.pop(0)
        elif self.scan_until is None and self.scan_res:          # pass 1 done → pass 2 on the undecoded responders
            self.scan_watch = [d for d, v in sorted(self.scan_res.items()) if not v.startswith("NRC") and d not in uds.DIDS]
            self.scan_until = now + self.SCAN_WATCH_S
            log.warning("espar scan: pass 1 done, %d answered (%s); watching the %d undecoded ones for %d min",
                        sum(not v.startswith("NRC") for v in self.scan_res.values()),
                        " ".join(f"{d:04X}" for d, v in sorted(self.scan_res.items()) if not v.startswith("NRC")),
                        len(self.scan_watch), self.SCAN_WATCH_S // 60)
            return False
        elif self.scan_watch and self.scan_until and now < self.scan_until:
            did = self.scan_watch[0]; self.scan_watch.append(self.scan_watch.pop(0))
        else:
            return False
        self.scan_wait = did
        return self._send_diag(uds.read_did(did), "scan")

    def _send_diag(self, data: bytes, what: str) -> bool:
        if not self.diag_enabled:
            return False
        try:
            self.bus.send(f"{self.device}_diag", can.Message(arbitration_id=uds.REQ_ID, data=data, is_extended_id=False),
                          self.gate, quiet=True)
            self.diag_sent += 1
            return True
        except Exception as e:                        # noqa: BLE001 — report, never retry
            self.diag_err = f"{what}: {e}"
            return False

    def awake(self, now: float | None = None) -> bool:
        """Heater and controller both talking within the last 2 s: only then do we ask (we never wake the bus)."""
        now = now or time.time()
        ctrl = self.ctl_active or bool(self.last_ctrl and now - self.last_ctrl < 2)
        return bool(self.last_heater and now - self.last_heater < 2 and ctrl)

    def diag_snapshot(self, now: float | None = None) -> dict:
        now = now or time.time()
        p = "climate.espar.diag."
        out = {p + "enabled": self.diag_enabled, p + "sent": self.diag_sent, p + "error": self.diag_err,
               p + "dtcs": self.dtcs}
        for k, (v, t) in self.diag.items():
            out[p + k] = v if now - t < max(30, self.diag_interval * (len(uds.LIVE_DIDS) + 5)) or k in (
                "software", "hardware", "hours", "event_count", "pump_strokes", "air_hpa") else None
        # the heater's own fuel per pump stroke: lifetime fuel ÷ lifetime strokes (µl)
        fuel, strokes = self.diag.get("fuel", (None, 0))[0], self.diag.get("pump_strokes", (None, 0))[0]
        out[p + "ul_per_stroke"] = round(fuel["total_l"] * 1e6 / strokes, 1) if fuel and strokes else None
        if self.scan_res:
            out[p + "scan"] = {f"{d:04X}": v for d, v in sorted(self.scan_res.items()) if not v.startswith("NRC")}
        lock = [d for d in (self.dtcs or []) if d.get("lockout") and d.get("active")]
        out[p + "lockout"] = {"code": lock[0]["code"], "why": uds.LOCKOUT[lock[0]["code"]], "help": uds.LOCKOUT_HELP} if lock else None
        return out

    async def run(self) -> None:
        import asyncio
        once = list(uds.ONCE_DIDS)
        live, dtc_at, next_req, scan_at, tick = 0, 0.0, 0.0, 0.0, 0
        while True:
            now = time.time()
            self.state.set(f"{self.device}.ids", self.summary())
            self.state.update(self.snapshot(now) | self.diag_snapshot(now))
            if self.diag_enabled and self.awake(now) and (self.scan_todo or (self.scan_res and self.scan_until is None)
                                                          or (self.scan_until and now < self.scan_until)) \
                    and now >= scan_at and not once:
                # scanning: pass 1 back to back; pass 2 alternates with the normal live reads (flame/intake to compare)
                tick += 1
                if self.scan_todo or tick % 2:
                    self._scan_step(now); scan_at = now + self.SCAN_GAP_S if self.scan_todo else now + 1.0
                    await asyncio.sleep(0.05)
                    continue
                scan_at = now + 1.0
            if self.diag_enabled and self.awake(now) and now >= next_req:
                if once:                              # versions etc. once, then the live set round-robin
                    self._send_diag(uds.read_did(once.pop(0)), "read")
                elif now - dtc_at > 600:
                    self._send_diag(uds.READ_DTCS, "fault memory"); dtc_at = now
                else:
                    self._send_diag(uds.read_did(uds.LIVE_DIDS[live % len(uds.LIVE_DIDS)]), "read"); live += 1
                next_req = now + self.diag_interval
            await asyncio.sleep(0.25 if self.diag_enabled else 1.0)
