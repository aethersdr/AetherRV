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

import time

import can

from ..state import State
from .capture_only import CaptureOnlyAdapter

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
    return ("", "")


class EsparAdapter(CaptureOnlyAdapter):
    """Decodes the heater's status and the EasyStart Pro's commands; never transmits (it has no send path)."""

    def __init__(self, device: str, state: State) -> None:
        super().__init__(device, state)
        self.cmd: dict = {}
        self.st: dict = {}
        self.last_rx: float | None = None
        self.last_heater: float | None = None
        self.last_ctrl: float | None = None
        self.heat_since: float | None = None
        self.after_since: float | None = None

    def on_frame(self, msg: can.Message) -> None:
        super().on_frame(msg)
        if msg.is_error_frame or msg.is_remote_frame or msg.is_extended_id:
            return
        now, d, i = time.time(), bytes(msg.data), msg.arbitration_id
        self.last_rx = now
        if i == 0x054 and len(d) >= 6:
            sp = d[2] | d[3] << 8
            self.cmd = {"flags": d[0], "mode": {0x05: "heat", 0x02: "fan", 0x00: "off"}.get(d[1], f"0x{d[1]:02x}"),
                        "setpoint_c": None if sp in (0xFFFE, 0xFFFF) else round(sp / 10, 1),
                        "room_c": temp_word(d[4], d[5])}
            self.last_ctrl = now
        elif i == 0x065 and len(d) >= 2:
            r = temp_word(d[0], d[1])
            if r is not None:
                self.cmd["room_c"] = r
        elif i in (0x60D,):
            self.last_ctrl = now
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
            p + "last_seen": self.last_rx,
        }

    async def run(self) -> None:
        import asyncio
        while True:
            self.state.set(f"{self.device}.ids", self.summary())
            self.state.update(self.snapshot())
            await asyncio.sleep(1.0)
