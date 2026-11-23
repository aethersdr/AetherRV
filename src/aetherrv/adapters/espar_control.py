"""AetherRV as the Espar heater's controller (the EasyStart Pro is unplugged: owner decision 2026-11-22).

Design: docs/espar-control-design.md. Everything goes through TxGate device "espar" (tx_enabled in the Pi config);
with it off this sends nothing and every command is refused.

Protocol (this van's captures + the S3 project): the controller sends 054 (command) / 055 / 056 / 057 every 200 ms,
60D (heartbeat) every 100 ms, 065 (room) every ~10 s, and its settings burst once per wake-up. The heater runs until it
is told OFF and shuts itself down ~15 s after its controller goes quiet (verified), so a dead Pi = a safe stop.

Rules
  - Never transmit while another controller is on the bus (an EasyStart Pro plugged back in): stop at once and say so.
  - The room temperature in 054 is always real while heating (FE FF there means 0 °C → full power): the cabin Ruuvi
    if fresh, else the heater's own intake reading; neither fresh → HEAT refused / OFF sent.
  - A command change is logged and verified against the heater's status (2C4) within VERIFY_S. The cyclic repetition
    is the protocol itself, not a retry.
  - Run time is ours (like the EasyStart Pro): 1-240 min by the minute, 270-720 in 30-min steps, or none (∞).
    On start-up we are OFF and never resume a run.
  - The heater must answer: no heater frames within WAKE_S of starting, or silent for LOST_S while we drive it →
    stop transmitting, reset hat1 (a frame nobody ACKs is retried forever by the MCP2515) and alert.
  - Idle (OFF, after-run done) for IDLE_STOP_S → stop transmitting so the bus and heater sleep.
"""

from __future__ import annotations

import asyncio
import logging
import sqlite3
import time

import can

from ..state import State
from ..txgate import TxBlocked, TxGate

log = logging.getLogger(__name__)

# Our EasyStart Pro's own bytes (captured 2026-11-22), replayed so the heater sees the same controller settings.
STARTUP_BURST = [
    (0x062, "12000000000001F1"), (0x066, "0000000000000000"), (0x10A, "100001201001"),
    (0x05C, "1E001E0000000000"), (0x05D, "000000000000FEFF"), (0x05E, "0000000000000000"),
    (0x05F, "1E001E0000000000"), (0x060, "000000000000FEFF"), (0x061, "0000000000000000"),
    (0x063, "3201010000000000"), (0x064, "3C000500C800FF03"),
    *[(i, "BE3E2700011E0000") for i in range(0x067, 0x06D)], (0x06D, "12000000000000F0"),
]
STATIC = {0x055: bytes.fromhex("1027000000000000"), 0x056: bytes.fromhex("1027000000000000"),
          0x057: bytes.fromhex("0000FEFFFEFF0000")}
HEARTBEAT = bytes.fromhex("0D10FD1D02207000")       # 60D as our Pro sends it (the heater doesn't check the ID)
MODES = {"off": 0x00, "fan": 0x02, "heat": 0x05}
EXPECT = {"heat": {"heating"}, "fan": {"fan"}, "off": {"idle", "cooldown", "off"}}


def run_minutes_ok(m) -> bool:
    """EasyStart Pro's choices: 1-240 by the minute, 270-720 every 30 min, or None (no limit)."""
    return m is None or (isinstance(m, int) and (1 <= m <= 240 or (240 < m <= 720 and m % 30 == 0)))


def room_word(c: float | None) -> bytes:
    if c is None:
        return b"\xfe\xff"
    return int(round((max(-40.0, min(80.0, c)) + 50) * 10)).to_bytes(2, "little")


class EsparControl:
    device = "espar"
    VERIFY_S, WAKE_S, LOST_S, IDLE_STOP_S, ROOM_FRESH_S, OTHER_S = 3.0, 3.0, 3.0, 10.0, 180.0, 3.0
    SETPOINT_RANGE = (10.0, 30.0)                   # °C, the heater's accepted range (S3: from 10 °C)

    def __init__(self, state: State, adapter, bus, gate: TxGate, db=None, reset=None) -> None:
        self.state, self.a, self.bus, self.gate, self.db = state, adapter, bus, gate, db
        self.reset = reset                           # async (iface): down/up hat1 (root helper)
        self.mode, self.setpoint_c, self.until = "off", 21.0, None
        self.flags = 0x00                            # 00 until the first run since start-up, then 09 on / 08 off
        self.active = False                          # transmitting
        self.started_at = 0.0
        self.idle_since: float | None = None
        self.last_room = None
        self.room_src = None
        self.problem: str | None = None
        self._lock = asyncio.Lock()
        self._tick = 0
        self._burst_sent = False
        adapter.on_other_controller = self._other_controller

    # ---- inputs ----
    @property
    def enabled(self) -> bool:
        return self.gate.is_enabled(self.device)

    def room(self) -> tuple[float | None, str | None]:
        s = self.state
        t, age = s.get("climate.cabin_front.temp"), s.age("climate.cabin_front.temp")
        if t is not None and age is not None and age < self.ROOM_FRESH_S:
            return float(t), "cabin Ruuvi"
        temps = s.get("climate.espar.diag.temps") or {}
        if temps.get("intake_c") is not None and (s.age("climate.espar.diag.temps") or 1e9) < self.ROOM_FRESH_S:
            return float(temps["intake_c"]), "heater intake sensor"
        return None, None

    def other_controller_recent(self, now: float) -> bool:
        return self.a.last_other_ctrl is not None and now - self.a.last_other_ctrl < self.OTHER_S

    def _other_controller(self) -> None:
        """Called by the adapter when it hears 054/60D that aren't ours (we don't receive our own frames)."""
        if self.active:
            self._stop("another controller appeared on the heater's bus (EasyStart Pro plugged in?)")

    # ---- frames ----
    def _send(self, aid: int, data: bytes) -> None:
        self.bus.send(self.device, can.Message(arbitration_id=aid, data=data, is_extended_id=False), self.gate, quiet=True)

    def _cmd_frame(self, room_c: float | None) -> bytes:
        on = self.mode in ("heat", "fan")
        sp = int(round(self.setpoint_c * 10)).to_bytes(2, "little") if self.mode == "heat" else b"\xfe\xff"
        return bytes([self.flags | (0x01 if on else 0x00), MODES[self.mode]]) + sp + room_word(room_c) + b"\x00\x00"

    def _stop(self, why: str | None) -> None:
        if self.active:
            log.warning("espar control: stopped transmitting (%s)", why or "idle")
        self.active = False
        if why:
            self.problem = why
            if self.mode != "off":
                self._log("auto_off", {"mode": self.mode}, "stopped", why, "aetherrv")
            self.mode, self.until = "off", None
            if self.reset is not None:
                asyncio.ensure_future(self._reset())

    async def _reset(self) -> None:
        try:
            await self.reset(self.bus.channel)
        except Exception as e:                          # noqa: BLE001
            log.error("espar control: %s reset failed: %s", self.bus.channel, e)

    # ---- the 100 ms loop ----
    def step(self, now: float | None = None) -> None:
        now = now or time.time()
        if not self.active:
            return
        if not self.enabled:
            self._stop("transmit permission turned off")
            return
        a = self.a
        heard = a.last_heater is not None and a.last_heater >= self.started_at
        if not heard and now - self.started_at > self.WAKE_S:
            self._stop("the heater didn't answer (check the heater's power and the CAN wiring)")
            return
        if heard and now - a.last_heater > self.LOST_S:
            self._stop("lost the heater (no status for 3 s)")
            return
        room, src = self.room()
        if self.mode == "heat" and room is None:
            self._log("auto_off", {"mode": "heat"}, "off", "no fresh room temperature (cabin Ruuvi or heater sensor)", "aetherrv")
            self.problem = "no fresh room temperature: heat stopped"
            self.mode, self.until, self.flags = "off", None, 0x08
        if self.until is not None and now >= self.until and self.mode != "off":
            self._log("timer_off", {"mode": self.mode}, "off", "run time finished", "aetherrv")
            self.mode, self.until, self.flags = "off", None, 0x08
        self.last_room, self.room_src = room, src
        try:
            self._send(0x60D, HEARTBEAT)
            if self._tick % 2 == 0:
                self._send(0x054, self._cmd_frame(room))
                for aid, data in STATIC.items():
                    self._send(aid, data)
            if self._tick % 100 == 1:
                self._send(0x065, room_word(room) + bytes([1 + (self._tick // 100) % 3]) + bytes(5))
            if heard and not self._burst_sent:
                for aid, h in STARTUP_BURST:
                    self._send(aid, bytes.fromhex(h))
                self._burst_sent = True
        except (TxBlocked, can.CanError, OSError) as e:
            self._stop(f"send failed: {e}")
            return
        self._tick += 1
        st = a.snapshot(now).get("climate.espar.state")
        if self.mode == "off" and st in ("off", None) and heard:
            self.idle_since = self.idle_since or now
            if now - self.idle_since > self.IDLE_STOP_S:
                self._stop(None)                         # idle: let the bus and heater sleep
        else:
            self.idle_since = None

    async def run(self) -> None:
        while True:
            self.step()
            self.publish()
            await asyncio.sleep(0.1)

    def publish(self) -> None:
        now = time.time()
        self.state.update({
            "espar.controls_ok": self.enabled,
            "climate.espar.ctl.active": self.active, "climate.espar.ctl.mode": self.mode,
            "climate.espar.ctl.setpoint_c": self.setpoint_c,
            "climate.espar.ctl.left_s": None if self.until is None else max(0, round(self.until - now)),
            "climate.espar.ctl.room_c": self.last_room, "climate.espar.ctl.room_src": self.room_src,
            "climate.espar.ctl.problem": self.problem,
        })

    # ---- commands ----
    def _log(self, cmd: str, value, outcome: str, detail: str, client: str) -> None:
        log.warning("espar command %s=%s from %s: %s %s", cmd, value, client, outcome, detail)
        if self.db is not None:
            try:
                self.db.execute("INSERT INTO command_log VALUES (?,?,?,?,?,?,?)",
                                (time.time(), self.device, cmd, str(value), outcome, detail, client))
            except sqlite3.Error:
                log.exception("command log write failed")

    async def command(self, cmd: str, value, client: str = "kiosk") -> dict:
        async with self._lock:
            return await self._command(cmd, value or {}, client)

    async def _command(self, cmd: str, v: dict, client: str) -> dict:
        if cmd != "set":
            return {"ok": False, "detail": f"command {cmd!r} is not allowed"}
        try:
            self.gate.check(self.device, f"set {v}")
        except TxBlocked:
            self._log(cmd, v, "blocked", "tx_enabled is off", client)
            return {"ok": False, "detail": "Espar control is disabled (tx_enabled is off)"}
        mode = v.get("mode")
        if mode not in MODES:
            return {"ok": False, "detail": "mode must be heat, fan or off"}
        sp = v.get("setpoint_c", self.setpoint_c)
        try:
            sp = round(float(sp), 1)
        except (TypeError, ValueError):
            return {"ok": False, "detail": "bad setpoint"}
        if mode == "heat" and not self.SETPOINT_RANGE[0] <= sp <= self.SETPOINT_RANGE[1]:
            return {"ok": False, "detail": f"setpoint must be {self.SETPOINT_RANGE[0]:.0f}-{self.SETPOINT_RANGE[1]:.0f} °C"}
        run = v.get("run_min")
        if mode != "off" and not run_minutes_ok(run):
            return {"ok": False, "detail": "run time must be 1-240 min, 270-720 in 30-min steps, or unlimited"}
        now = time.time()
        if self.other_controller_recent(now):
            self._log(cmd, v, "rejected", "another controller is on the bus", client)
            return {"ok": False, "detail": "Another controller (the EasyStart Pro?) is on the heater's bus. Unplug it first."}
        if mode == "heat" and self.room()[0] is None:
            self._log(cmd, v, "rejected", "no fresh room temperature", client)
            return {"ok": False, "detail": "No fresh room temperature (cabin Ruuvi or heater sensor). Nothing sent."}
        if mode == "off" and not self.active:
            self.mode, self.until = "off", None
            self._log(cmd, v, "noop", "already off (not driving the heater)", client)
            return {"ok": True, "detail": "already off"}
        self.mode, self.setpoint_c = mode, sp if mode == "heat" else self.setpoint_c
        self.until = None if mode == "off" or run is None else now + run * 60
        if mode != "off":
            self.flags = 0x08                            # bit 3 stays set once a run has started (as the Pro does)
        self.problem = None
        if not self.active:                              # take over the bus: wake the heater
            self.active, self.started_at, self._tick, self._burst_sent, self.idle_since = True, now, 0, False, None
        want = EXPECT[mode]
        end = time.time() + self.WAKE_S + self.VERIFY_S
        while time.time() < end:
            await asyncio.sleep(0.1)
            if not self.active:
                break
            st = self.a.snapshot().get("climate.espar.state")
            if st in want and (self.a.last_heater or 0) > now:
                self._log(cmd, v, "ok", f"heater {st}", client)
                return {"ok": True, "detail": f"Espar {mode}" + (f" {sp:.1f} °C" if mode == "heat" else "")}
        why = self.problem or "the heater didn't confirm"
        self._log(cmd, v, "unverified", why, client)
        return {"ok": False, "detail": f"Espar {mode}: {why}. Not retried."}
