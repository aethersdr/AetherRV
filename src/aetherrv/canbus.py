"""CAN transport: one reader per bus, per-bus capture to candump-format logs, replay source.

Capture files use the can-utils `candump -l` format, so `canplayer`, SavvyCAN and
python-can's CanutilsLogReader can all read them. Every decoder is developed against
these captures.
"""

from __future__ import annotations

import asyncio
import logging
import time
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING

import can

if TYPE_CHECKING:
    from .txgate import TxGate

log = logging.getLogger(__name__)

FrameHandler = Callable[[can.Message], None]


@dataclass
class BusStats:
    frames: int = 0
    errors: int = 0
    ids: dict[int, int] = field(default_factory=dict)
    last_rx: float | None = None
    last_good: float | None = None      # last frame another node sent (not an error frame, not our own echo)
    _window: list[float] = field(default_factory=list)

    def note(self, msg: can.Message) -> None:
        now = time.monotonic()
        self.frames += 1
        if msg.is_error_frame:
            self.errors += 1
        else:
            self.ids[msg.arbitration_id] = self.ids.get(msg.arbitration_id, 0) + 1
            if getattr(msg, "is_rx", True):
                self.last_good = time.time()
        self.last_rx = time.time()
        self._window.append(now)

    def rate(self) -> float:
        cutoff = time.monotonic() - 5.0
        self._window = [t for t in self._window if t >= cutoff]
        return len(self._window) / 5.0


class CanBus:
    """Wraps a python-can bus; fans frames out to handlers and an optional capture log."""

    def __init__(
        self,
        name: str,
        interface: str,
        channel: str,
        capture_dir: Path | None = None,
        bitrate: int | None = None,
        host: str | None = None,
        port: int | None = None,
        own_echo: bool = False,
    ) -> None:
        self.name = name
        self.interface = interface
        self.channel = channel
        self.bitrate = bitrate
        self.host, self.port = host, port          # socketcand (CAN over TCP, e.g. the Cerbo GX's can0)
        self.capture_dir = capture_dir
        self.own_echo = own_echo                   # socketcan: also receive our own frames once really sent (TX confirm)
        self.handlers: list[FrameHandler] = []
        self.stats = BusStats()
        self.connected = False
        self._bus: can.BusABC | None = None
        self._writer: can.Listener | None = None
        self._kick = asyncio.Event()                # set → drop the socket and reopen (e.g. after a driver rebind)

    def link_up(self) -> bool:
        """Kernel interface admin state (socketcan). A bound socket survives `ip link set
        down` silently, so this is the honest 'is the bus usable' signal."""
        if self.interface == "socketcand":
            return self.connected and self.stats.last_rx is not None and time.time() - self.stats.last_rx < 30
        if self.interface != "socketcan":
            return self.connected
        try:
            flags = int(Path(f"/sys/class/net/{self.channel}/flags").read_text(), 16)
        except (OSError, ValueError):
            return False
        return self.connected and bool(flags & 0x1)  # IFF_UP

    def add_handler(self, h: FrameHandler) -> None:
        self.handlers.append(h)

    def _open(self) -> can.BusABC:
        kw: dict = {"interface": self.interface, "channel": self.channel}
        if self.interface == "virtual":
            kw["receive_own_messages"] = False
        if self.interface == "socketcand":
            # Remote bus: bitrate is the remote side's; AetherRV never transmits on it (no send path for N2K).
            kw.update(host=self.host, port=int(self.port or 29536))
        # socketcan: bitrate/listen-only are owned by systemd-networkd, not by us.
        if self.interface == "socketcan" and self.own_echo:
            kw["receive_own_messages"] = True
        return can.Bus(**kw)

    def _open_capture(self) -> None:
        if not self.capture_dir:
            return
        self.capture_dir.mkdir(parents=True, exist_ok=True)
        stamp = time.strftime("%Y%m%d-%H%M%S")
        path = self.capture_dir / f"{self.name}-{stamp}.log"
        # SizedRotatingLogger picks the candump writer from the .log extension.
        self._writer = can.SizedRotatingLogger(base_filename=str(path), max_bytes=50_000_000)
        log.info("[%s] capturing to %s", self.name, path)

    def _dispatch(self, msg: can.Message) -> None:
        self.stats.note(msg)
        if self._writer is not None:
            msg.channel = self.name
            self._writer(msg)
        for h in self.handlers:
            try:
                h(msg)
            except Exception:  # a decoder bug must never kill the bus reader
                log.exception("[%s] handler %r failed on %s", self.name, h, msg)

    async def run(self) -> None:
        """Read forever; reopen on failure (cable unplugged, bus-off, interface down)."""
        backoff = 1.0
        while True:
            try:
                # Off the event loop: a socketcand open to an unreachable host blocks for ~10 s, which would
                # stall the whole backend and trip the systemd watchdog (WatchdogSec=10) → crash loop.
                self._bus = await asyncio.to_thread(self._open)
                self._open_capture()
                self.connected = True
                backoff = 1.0
                log.info("[%s] open on %s:%s", self.name, self.interface, self.channel)
                reader = can.AsyncBufferedReader()
                notifier = can.Notifier(self._bus, [reader], loop=asyncio.get_running_loop())
                self._kick.clear()

                async def pump() -> None:
                    async for msg in reader:
                        self._dispatch(msg)
                pump_t, kick_t = asyncio.create_task(pump()), asyncio.create_task(self._kick.wait())
                try:
                    done, _ = await asyncio.wait({pump_t, kick_t}, return_when=asyncio.FIRST_COMPLETED)
                    if kick_t in done:
                        raise ConnectionResetError("reopening (requested)")
                    pump_t.result()
                finally:
                    for t in (pump_t, kick_t):
                        t.cancel()
                    notifier.stop()
            except asyncio.CancelledError:
                raise
            except Exception as e:
                log.warning("[%s] bus error: %s; retry in %.0fs", self.name, e, backoff)
            finally:
                self.connected = False
                self.close()
            await asyncio.sleep(backoff)
            backoff = min(backoff * 2, 30.0)

    def send(self, device: str, msg: can.Message, gate: "TxGate", quiet: bool = False) -> None:
        """The only transmit path. Refused unless `device` has tx_enabled in config."""
        gate.check(device, f"{self.name} {msg.arbitration_id:X}#{bytes(msg.data).hex()}", quiet=quiet)
        if self._bus is None:
            raise can.CanOperationError(f"[{self.name}] bus not open")
        self._bus.send(msg)

    def reopen(self) -> None:
        self._kick.set()

    def close(self) -> None:
        if self._writer is not None:
            self._writer.stop()
            self._writer = None
        if self._bus is not None:
            self._bus.shutdown()
            self._bus = None


class FailoverBus:
    """One logical bus over two paths to the SAME physical bus: a primary (the Pi's own CAN port) and a backup (e.g.
    the Cerbo's socketcand). Frames are forwarded from the active path only (the other path hears the same frames)
    and sends go out the active path only, through TxGate as always.

    Failover: the primary goes silent (no frame from another node for FRESH_S) while the backup still hears the bus.
    Both silent = the bus itself is quiet: stay put. Failback after FAILBACK_S of steady traffic on the primary.

    A frame nobody ACKs is retried by the CAN chip forever (the MCP2515 has no one-shot mode; a node stays
    error-passive, never bus-off, on ACK errors) and would go out whenever the wire came back. So on the primary:
    refuse to send unless it heard the bus within SEND_FRESH_S, and expect each frame's TX echo within ECHO_S; a
    missing echo resets the port at once (discarding the stuck frame) and fails over. A failed send is reported
    as failed, never replayed on the other path.
    """

    FRESH_S, SEND_FRESH_S, FAILBACK_S, ECHO_S = 2.0, 1.0, 30.0, 0.25

    def __init__(self, name: str, primary: CanBus, backup: CanBus, reset=None, state=None) -> None:
        self.name, self.primary, self.backup = name, primary, backup
        self.active = primary
        self.reset = reset                         # async (iface) -> None: down/up the port (root helper)
        self.state = state
        self.handlers: list[FrameHandler] = []
        self.on_switch: list[Callable[[CanBus], None]] = []
        self.primary.own_echo = True
        self._pending: dict[tuple[int, bytes], float] = {}
        self._good_since: float | None = None
        self._blocked_until = 0.0                  # after a stuck frame: no failback before this
        primary.add_handler(lambda m: self._rx(primary, m))
        backup.add_handler(lambda m: self._rx(backup, m))

    # CanBus-like surface used by the adapters and N2kNode
    @property
    def channel(self) -> str:
        return self.active.channel

    def add_handler(self, h: FrameHandler) -> None:
        self.handlers.append(h)

    def link_up(self) -> bool:
        return self.active.link_up()

    @staticmethod
    def fresh(bus: CanBus, within: float, now: float | None = None) -> bool:
        g = bus.stats.last_good
        return g is not None and (now or time.time()) - g < within and bus.link_up()

    def _rx(self, src: CanBus, msg: can.Message) -> None:
        if not getattr(msg, "is_rx", True):        # our own frame, confirmed sent (socketcan echo)
            self._pending.pop((msg.arbitration_id, bytes(msg.data)), None)
            return
        if src is not self.active:
            return
        for h in self.handlers:
            try:
                h(msg)
            except Exception:
                log.exception("[%s] handler %r failed on %s", self.name, h, msg)

    def send(self, device: str, msg: can.Message, gate: "TxGate", quiet: bool = False) -> None:
        bus = self.active
        if not self.fresh(bus, self.SEND_FRESH_S if bus is self.primary else self.FRESH_S):
            raise can.CanOperationError(f"[{self.name}] {bus.name} isn't hearing the bus; not sent")
        bus.send(device, msg, gate, quiet=quiet)
        if bus is self.primary:
            key = (msg.arbitration_id, bytes(msg.data))
            self._pending[key] = time.time()
            try:
                asyncio.get_running_loop().call_later(self.ECHO_S, self._check_echo, key)
            except RuntimeError:                   # no loop (tests calling synchronously)
                pass

    def _check_echo(self, key: tuple[int, bytes]) -> None:
        if self._pending.pop(key, None) is None or self.active is not self.primary:
            return
        log.error("[%s] %s: frame %X not sent within %.0f ms (no ACK: wiring?); resetting the port, switching to %s",
                  self.name, self.primary.name, key[0], self.ECHO_S * 1000, self.backup.name)
        self._pending.clear()
        self._blocked_until = time.time() + self.FAILBACK_S
        if self.reset is not None:
            asyncio.ensure_future(self._do_reset())
        self._switch(self.backup, "a frame was not acknowledged")

    async def _do_reset(self) -> None:
        try:
            await self.reset(self.primary.channel)
            log.warning("[%s] %s reset (pending frame discarded)", self.name, self.primary.name)
        except Exception as e:                     # noqa: BLE001
            log.error("[%s] %s reset failed: %s", self.name, self.primary.name, e)

    def _switch(self, to: CanBus, why: str) -> None:
        if to is self.active:
            return
        self.active, self._good_since = to, None
        (log.info if to is self.primary else log.warning)("[%s] now via %s (%s)", self.name, to.name, why)
        self._publish()
        for cb in self.on_switch:
            try:
                cb(to)
            except Exception:                      # noqa: BLE001
                log.exception("[%s] switch callback failed", self.name)

    def _publish(self) -> None:
        if self.state is not None:
            self.state.update({f"{self.name}.path": self.active.name,
                               f"{self.name}.path_backup": self.active is self.backup})

    def tick(self, now: float | None = None) -> None:
        now = now or time.time()
        p_ok, b_ok = self.fresh(self.primary, self.FRESH_S, now), self.fresh(self.backup, self.FRESH_S, now)
        if self.active is self.primary:
            if not p_ok and b_ok:
                self._switch(self.backup, f"{self.primary.name} silent")
            return
        if p_ok and now >= self._blocked_until:
            self._good_since = self._good_since or now
            if now - self._good_since >= self.FAILBACK_S:
                self._switch(self.primary, f"{self.primary.name} steady again")
        else:
            self._good_since = None

    async def run(self) -> None:
        self._publish()
        while True:
            await asyncio.sleep(0.5)
            self.tick()


async def recover_links(buses: list[CanBus], admin, state=None, down_s: float = 30.0, every_s: float = 300.0,
                        poll_s: float = 10.0, settle_s: float = 2.0) -> None:
    """A local CAN port that stays down (IFF_UP clear) for down_s gets its SPI driver rebound by the root helper
    (verb can_rebind), at most once per every_s, and the bus reopens. Seen once: after a reboot with the FD HAT
    stacked, hat1's MCP2515 wouldn't enter config mode until its driver was rebound."""
    down_since: dict[str, float] = {}
    tried: dict[str, float] = {}
    while True:
        await asyncio.sleep(poll_s)
        now = time.time()
        for b in buses:
            if b.interface != "socketcan" or b.channel not in ("hat0", "hat1", "fd0", "fd1"):
                continue
            if b.link_up():
                down_since.pop(b.name, None)
                continue
            since = down_since.setdefault(b.name, now)
            if now - since < down_s or now - tried.get(b.name, 0) < every_s:
                continue
            tried[b.name] = now
            log.warning("[%s] %s down for %.0f s: rebinding its driver", b.name, b.channel, now - since)
            try:
                await admin.call("can_rebind", {"iface": b.channel}, timeout=30)
                await asyncio.sleep(settle_s)
                b.reopen()
                log.warning("[%s] driver rebound, reopening (%s)", b.name, "up" if b.link_up() else "still down")
            except Exception as e:                          # noqa: BLE001
                log.error("[%s] rebind failed: %s", b.name, e)
            if state is not None:
                state.update({f"bus.{b.name}.rebind_at": now, f"bus.{b.name}.rebind_ok": b.link_up()})


async def replay(path: Path, bus: can.BusABC, speed: float = 1.0, loop_forever: bool = False) -> None:
    """Replay a candump log onto `bus` (a vcan or virtual bus; never a live vehicle bus)."""
    while True:
        prev = None
        for msg in can.CanutilsLogReader(str(path)):
            if prev is not None and speed > 0:
                await asyncio.sleep(max(0.0, (msg.timestamp - prev) / speed))
            prev = msg.timestamp
            bus.send(msg)
        if not loop_forever:
            return


async def prune_captures(capture_dir: Path, keep_days: float = 14.0, max_gb: float = 20.0) -> None:
    """Delete capture logs older than keep_days, then oldest-first until under max_gb."""
    while True:
        try:
            files = sorted((f for f in capture_dir.glob("*.log*") if f.is_file()), key=lambda f: f.stat().st_mtime)
            cutoff = time.time() - keep_days * 86400
            for f in [f for f in files if f.stat().st_mtime < cutoff]:
                f.unlink(missing_ok=True)
            files = [f for f in files if f.exists()]
            total = sum(f.stat().st_size for f in files)
            while files and total > max_gb * 1e9:
                f = files.pop(0)
                total -= f.stat().st_size
                f.unlink(missing_ok=True)
        except OSError:
            log.exception("capture pruning failed")
        await asyncio.sleep(3600)
