"""NMEA 2000 switching: AetherRV commands the load controller (CLMD16) and runs the keypad/input logic.

Owner decision 2026-11-21: the switching logic (keypad bindings, groups, input-follows-output) lives in
AetherRV and the CLMD16 is a plain output box. Consequences the owner accepted: with the Pi off, the
keypads and input-driven outputs (reverse lights) do nothing; after a backend restart the logic is
back within ~20 s. Anything that must work without the Pi (the drawer-lock cut-off) belongs in the
CLMD16's own config; until it is there, channels marked `blocked` refuse ON here.

Wire format (verified against captures of N2KAnalyzer → CLMD16, firmware 1.0.3.66):
  * Switch:  126208 Command for PGN 127501: priority byte 0xF8, pairs (1, bank instance), (N+1, 0|1).
  * Dimmer:  126208 Command for PGN 127500: pairs (2, connection = channel-1), (6, duty 0..100).
  * The CLMD16 always answers 126208 Acknowledge 0x44, whether or not it obeyed, so success is ONLY
    ever judged from its 127501 / 127500 status broadcasts.

Transmit rules (same as every AetherRV control): TxGate device "n2k" (tx_enabled), fresh status (≤ 3 s
old, the CLMD16 reports every 1.5 s), send exactly once, verify from status, never retry, log every
command. AetherRV claims its own N2K address (ISO 11783-5 address claim) before sending anything.
"""

from __future__ import annotations

import asyncio
import logging
import sqlite3
import time
from dataclasses import dataclass, field

import can

from .state import State
from .txgate import TxBlocked, TxGate

log = logging.getLogger(__name__)

PGN_ISO_REQUEST = 59904
PGN_ADDRESS_CLAIM = 60928
PGN_GROUP_FUNCTION = 126208
PGN_LOAD_CONTROLLER = 127500
PGN_SWITCH_STATUS = 127501


# ── Encoding ────────────────────────────────────────────────────────────────
def can_id(priority: int, pgn: int, source: int, dest: int = 255) -> int:
    """29-bit identifier. PDU1 PGNs (PF < 240) carry the destination in the PS byte."""
    pf = (pgn >> 8) & 0xFF
    if pf < 240:
        pgn = (pgn & 0x3FF00) | (dest & 0xFF)
    return ((priority & 7) << 26) | ((pgn & 0x3FFFF) << 8) | (source & 0xFF)


def fast_packet_frames(payload: bytes, seq: int) -> list[bytes]:
    """Split a fast-packet message into 8-byte frames (frame 0: seq|0, length, 6 bytes; then seq|n + 7)."""
    if len(payload) > 223:
        raise ValueError("fast-packet payload too long")
    seq = (seq & 7) << 5
    out = [bytes([seq, len(payload)]) + payload[:6]]
    rest, n = payload[6:], 1
    while rest:
        out.append(bytes([seq | n]) + rest[:7])
        rest, n = rest[7:], n + 1
    return [f.ljust(8, b"\xff") for f in out]


def group_command(pgn: int, pairs: list[tuple[int, bytes]]) -> bytes:
    """126208 Command (function 1): commanded PGN, priority setting 8 (= leave unchanged), N field pairs."""
    body = bytearray([0x01]) + pgn.to_bytes(3, "little") + bytes([0xF8, len(pairs)])
    for fieldno, value in pairs:
        body += bytes([fieldno]) + value
    return bytes(body)


def switch_command(bank: int, channel: int, on: bool) -> bytes:
    if not 1 <= channel <= 28:
        raise ValueError("channel must be 1..28")
    return group_command(PGN_SWITCH_STATUS, [(1, bytes([bank])), (channel + 1, bytes([1 if on else 0]))])


def dim_command(channel: int, level: int) -> bytes:
    if not 0 <= level <= 100:
        raise ValueError("level must be 0..100")
    return group_command(PGN_LOAD_CONTROLLER, [(2, bytes([channel - 1])), (6, bytes([level]))])


def make_name(unique: int, manufacturer: int = 2046, function: int = 130, dev_class: int = 25,
              industry: int = 4, instance: int = 0) -> int:
    """ISO 11783 NAME. Defaults: unregistered manufacturer code, class 25 (inter/intranetwork device),
    function 130 (PC gateway), marine industry group, arbitrary-address capable."""
    return ((unique & 0x1FFFFF) | ((manufacturer & 0x7FF) << 21) | ((instance & 0xFF) << 32)
            | ((function & 0xFF) << 40) | ((dev_class & 0x7F) << 49) | ((industry & 7) << 60) | (1 << 63))


# ── Address claim ───────────────────────────────────────────────────────────
class N2kNode:
    """Our own node on the bus: claims an address and defends it. Sending waits until claimed."""

    CLAIM_SETTLE = 0.25   # s without a contest before the address counts as ours (ISO 11783-5)

    def __init__(self, bus, gate: TxGate, preferred: int = 150, unique: int = 0x0AE7) -> None:
        self.bus, self.gate = bus, gate
        self.address = preferred
        self.name = make_name(unique)
        self.claimed_at: float | None = None
        self.taken: set[int] = set()           # addresses other devices use
        self.lost = False

    @property
    def ready(self) -> bool:
        return (self.claimed_at is not None and not self.lost
                and time.monotonic() - self.claimed_at >= self.CLAIM_SETTLE)

    def _send_claim(self) -> None:
        msg = can.Message(arbitration_id=can_id(6, PGN_ADDRESS_CLAIM, self.address, 255),
                          data=self.name.to_bytes(8, "little"), is_extended_id=True)
        self.bus.send("n2k", msg, self.gate)
        self.claimed_at = time.monotonic()
        log.info("n2k: claiming address %d", self.address)

    def claim(self) -> None:
        free = [a for a in range(self.address, 252) if a not in self.taken] + \
               [a for a in range(0, self.address) if a not in self.taken]
        if not free:
            self.lost = True
            log.error("n2k: no free address to claim")
            return
        self.address, self.lost = free[0], False
        self._send_claim()

    def on_frame(self, msg: can.Message) -> None:
        if msg.is_error_frame or not msg.is_extended_id:
            return
        a = msg.arbitration_id
        src, pf, ps = a & 0xFF, (a >> 16) & 0xFF, (a >> 8) & 0xFF
        if pf == 0xEE and len(msg.data) >= 8:                       # someone's address claim
            if src == self.address and self.claimed_at is not None:
                theirs = int.from_bytes(bytes(msg.data[:8]), "little")
                if theirs == self.name:
                    return                                          # our own frame echoed back
                if theirs < self.name:                              # lower NAME wins: move
                    log.warning("n2k: address %d contested by a higher-priority NAME; moving", src)
                    self.taken.add(src)
                    self.claimed_at = None
                    try:
                        self.claim()
                    except TxBlocked:
                        pass
                else:                                               # we win: re-assert
                    try:
                        self._send_claim()
                    except TxBlocked:
                        pass
            else:
                self.taken.add(src)
        elif pf == 0xEA and (ps in (255, self.address)) and len(msg.data) >= 3:   # ISO request
            if int.from_bytes(bytes(msg.data[:3]), "little") == PGN_ADDRESS_CLAIM and self.claimed_at is not None:
                try:
                    self._send_claim()
                except TxBlocked:
                    pass
        elif src < 252:
            self.taken.add(src)

    def send_fast(self, pgn: int, dest: int, payload: bytes, seq: int, priority: int = 3) -> None:
        if not self.ready:
            raise TxBlocked("AetherRV has not claimed an NMEA 2000 address yet")
        for frame in fast_packet_frames(payload, seq):
            self.bus.send("n2k", can.Message(arbitration_id=can_id(priority, pgn, self.address, dest),
                                             data=frame, is_extended_id=True), self.gate)


# ── Status view ─────────────────────────────────────────────────────────────
@dataclass
class Debounced:
    value: bool | None = None
    pending: bool | None = None
    since: float = 0.0
    hold: float = 0.3
    volts: float | None = None

    def feed(self, raw: bool, now: float) -> None:
        if raw != self.pending:
            self.pending, self.since = raw, now

    def settle(self, now: float) -> bool:
        """Commit the pending value once it has been stable for `hold`. Returns True on a change."""
        if self.pending is not None and self.pending != self.value and now - self.since >= self.hold:
            self.value = self.pending
            return True
        return False


@dataclass
class Binding:
    keypad: int                    # source address of the CKM12
    button: int                    # 1..12 on its bank-50 status
    channels: list[int]
    bank: int = 50
    lockout: float = 0.6
    last: float = field(default=0.0)


class SwitchController:
    """device "n2k": on/off and dimming commands to the load controller, plus the keypad/input logic."""

    device = "n2k"
    FRESH = 3.0       # s: newest status must be at most this old (CLMD16 reports every 1.5 s)
    VERIFY = 4.0      # s: the CLMD16 broadcasts a changed status immediately

    def __init__(self, state: State, n2k, node: N2kNode, gate: TxGate, cfg: dict,
                 db: sqlite3.Connection | None = None) -> None:
        self.state, self.n2k, self.node, self.gate, self.db = state, n2k, node, gate, db
        sw = cfg.get("switching", {})
        self.controller = int(sw.get("controller", 0))       # CLMD16 source address
        self.bank = int(sw.get("bank", 32))
        self.automation = bool(sw.get("automation", True))
        self.channels = {int(c["channel"]): c for c in cfg.get("switch", []) if int(c.get("bank", self.bank)) == self.bank}
        self.bindings = [Binding(int(b["keypad"]), int(b["button"]), [int(x) for x in b["channels"]],
                                 int(b.get("bank", 50))) for b in cfg.get("binding", [])]
        self.inputs = {int(i["input"]): i for i in cfg.get("input", [])}
        self.deb = {n: Debounced(hold=float(i.get("debounce_s", 0.3))) for n, i in self.inputs.items()}
        self._seq = 0
        self._locks: dict[int, asyncio.Lock] = {}
        self._keypad_prev: dict[tuple[int, int], list] = {}
        self._tasks: set[asyncio.Task] = set()
        if db is not None:
            db.execute("CREATE TABLE IF NOT EXISTS command_log (t REAL, device TEXT, cmd TEXT, value TEXT, "
                       "outcome TEXT, detail TEXT, client TEXT)")
        n2k.bank_listeners.append(self.on_bank)
        state.set("n2k.controls_ok", gate.is_enabled(self.device))
        state.set("switching.automation", self.automation)

    # -- status helpers --
    def status(self) -> tuple[list | None, float | None]:
        b = self.n2k.banks.get((self.controller, self.bank))
        if not b:
            return None, None
        return b["states"], time.time() - b["seen"]

    def channel_on(self, ch: int) -> bool | None:
        st, age = self.status()
        if st is None or age > self.FRESH:
            return None
        return st[ch - 1] == "on"

    def _log(self, cmd: str, value, outcome: str, detail: str, client: str) -> None:
        log.warning("n2k command %s=%s from %s: %s %s", cmd, value, client, outcome, detail)
        if self.db is not None:
            try:
                self.db.execute("INSERT INTO command_log VALUES (?,?,?,?,?,?,?)",
                                (time.time(), self.device, cmd, str(value), outcome, detail, client))
            except sqlite3.Error:
                log.exception("command log write failed")

    # -- commands --
    async def command(self, cmd: str, value, client: str = "kiosk") -> dict:
        try:
            ch = int((value or {}).get("channel"))
        except (TypeError, ValueError, AttributeError):
            return {"ok": False, "detail": "bad request"}
        lock = self._locks.setdefault(ch, asyncio.Lock())
        async with lock:
            return await self._command(cmd, value, ch, client)

    async def _command(self, cmd: str, value: dict, ch: int, client: str) -> dict:
        conf = self.channels.get(ch)
        try:
            self.gate.check(self.device, f"{cmd} ch{ch}={value}")
        except TxBlocked as e:
            self._log(cmd, value, "blocked", str(e), client)
            return {"ok": False, "detail": "switch control is disabled (tx_enabled is off)"}
        if conf is None:
            self._log(cmd, value, "rejected", "channel not configured", client)
            return {"ok": False, "detail": f"channel {ch} is not configured"}
        st, age = self.status()
        if st is None or age > self.FRESH:
            self._log(cmd, value, "failed", "no fresh 127501 from the load controller", client)
            return {"ok": False, "detail": "no fresh status from the load controller; nothing sent"}
        cur = st[ch - 1]                      # "on" | "off" | "error" (tripped) | None
        if cmd == "set":
            on = bool(value.get("on"))
            if on and conf.get("blocked"):
                self._log(cmd, value, "rejected", f"channel blocked: {conf.get('blocked_reason', '')}", client)
                return {"ok": False, "detail": conf.get("blocked_reason") or f"{conf.get('name', ch)} is blocked"}
            need = conf.get("requires_input")
            if on and need is not None:          # interlock: e.g. high-draw lights only with the ignition on
                d = self.deb.get(int(need))
                if d is None or d.value is not True:
                    nm = self.inputs.get(int(need), {}).get("name", f"input {need}")
                    self._log(cmd, value, "rejected", f"interlock: {nm} is not on", client)
                    return {"ok": False, "detail": f"{conf.get('name', ch)} needs {nm} on"}
            if cur == ("on" if on else "off"):  # a tripped channel ("error") still takes OFF: that clears the trip
                self._log(cmd, value, "noop", "already in requested state", client)
                return {"ok": True, "detail": "already " + ("on" if on else "off")}
            if on and cur == "error":
                self._log(cmd, value, "rejected", "channel is tripped; switch it off first to clear", client)
                return {"ok": False, "detail": f"{conf.get('name', ch)} is tripped (fault). Switch it off to clear, then on."}
            payload = switch_command(self.bank, ch, on)
            want = "on" if on else "off"
            verify = lambda: (self.status()[0] or [None] * 28)[ch - 1] == want and self.status()[1] <= self.FRESH  # noqa: E731
        elif cmd == "dim":
            if conf.get("kind") != "dimmer":
                return {"ok": False, "detail": f"{conf.get('name', ch)} is not a dimmer"}
            level = int(value.get("level"))
            if not 0 <= level <= 100:
                return {"ok": False, "detail": "level must be 0–100"}
            payload = dim_command(ch, level)
            lc = lambda: self.n2k.loads.get((self.controller, ch - 1)) or {}     # noqa: E731
            verify = lambda: lc().get("duty") == level and time.time() - lc().get("seen", 0) < self.VERIFY  # noqa: E731
        else:
            self._log(cmd, value, "rejected", "not an allowed command", client)
            return {"ok": False, "detail": f"command {cmd!r} is not allowed"}
        self._seq = (self._seq + 1) & 7
        sent_at = time.time()
        try:
            self.node.send_fast(PGN_GROUP_FUNCTION, self.controller, payload, self._seq)   # exactly once
        except TxBlocked as e:
            self._log(cmd, value, "blocked", str(e), client)
            return {"ok": False, "detail": str(e)}
        except Exception as e:
            self._log(cmd, value, "failed", f"send: {e}", client)
            return {"ok": False, "detail": "send failed (not retried)"}
        while time.time() - sent_at < self.VERIFY:
            await asyncio.sleep(0.1)
            b = self.n2k.banks.get((self.controller, self.bank))
            if b and b["seen"] >= sent_at and verify():
                self._log(cmd, value, "ok", "confirmed by status", client)
                return {"ok": True, "detail": "confirmed by the load controller"}
        self._log(cmd, value, "unverified", f"no matching status within {self.VERIFY:.0f}s", client)
        return {"ok": False, "detail": "the load controller did not confirm (it may refuse remote control for this channel; not retried)"}

    # -- logic --
    def _spawn(self, coro) -> None:
        t = asyncio.get_running_loop().create_task(coro)
        self._tasks.add(t)
        t.add_done_callback(self._tasks.discard)

    def on_bank(self, src: int, inst: int, states: list, now: float) -> None:
        """Called by the N2K adapter for every 127501. Keypad rising edges → bound channels."""
        key = (src, inst)
        prev = self._keypad_prev.get(key)
        self._keypad_prev[key] = states
        if src == self.controller and inst == self.bank:
            for n in self.inputs:
                raw = states[16 + n - 1] if 16 + n - 1 < len(states) else None
                if raw is not None:
                    self.deb[n].feed(raw == "on", now)
            return
        if prev is None or not self.automation:
            return                                   # first sight of a keypad: baseline only, never act
        for b in self.bindings:
            if b.keypad != src or b.bank != inst or b.button > len(states):
                continue
            if states[b.button - 1] == "on" and prev[b.button - 1] != "on" and now - b.last >= b.lockout:
                b.last = now
                self._spawn(self._toggle(b))

    async def _toggle(self, b: Binding) -> None:
        first = self.channel_on(b.channels[0])
        if first is None:
            log.warning("keypad %d button %d: no fresh status, ignored", b.keypad, b.button)
            return
        for ch in b.channels:
            r = await self.command("set", {"channel": ch, "on": not first}, client=f"keypad:{b.keypad}.{b.button}")
            if not r["ok"]:
                log.info("keypad %d button %d → ch%d: %s", b.keypad, b.button, ch, r["detail"])

    def tick(self, now: float | None = None) -> None:
        now = time.time() if now is None else now
        for n, d in self.deb.items():
            first = d.value is None
            if d.settle(now) and not first and self.automation:
                for ch in self.inputs[n].get("follow", []):
                    self._spawn(self.command("set", {"channel": int(ch), "on": bool(d.value)},
                                             client=f"input:{self.inputs[n].get('name', n)}"))
        loads = self.n2k.volts
        self.state.set("switching.inputs", [
            {"input": n, "name": i.get("name", f"Input {n}"), "icon": i.get("icon", "toggle"),
             "on": self.deb[n].value, "volts": (loads.get((self.controller, 15 + n)) or {}).get("volts")}
            for n, i in sorted(self.inputs.items())])

    async def run(self) -> None:
        backoff = 2.0
        while not self.node.ready:
            try:
                if self.gate.is_enabled(self.device) and self.node.claimed_at is None:
                    self.node.claim()
            except Exception as e:                      # bus not open yet, etc.
                log.debug("n2k address claim deferred: %s", e)
            self.tick()
            await asyncio.sleep(0.5 if self.node.claimed_at else backoff)
            if not self.gate.is_enabled(self.device):
                break                                    # tx disabled: read-only (inputs still published)
        while True:
            self.tick()
            self.state.set("n2k.address", self.node.address if self.node.ready else None)
            await asyncio.sleep(0.1)
