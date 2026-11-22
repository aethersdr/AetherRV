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
    _window: list[float] = field(default_factory=list)

    def note(self, msg: can.Message) -> None:
        now = time.monotonic()
        self.frames += 1
        if msg.is_error_frame:
            self.errors += 1
        else:
            self.ids[msg.arbitration_id] = self.ids.get(msg.arbitration_id, 0) + 1
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
    ) -> None:
        self.name = name
        self.interface = interface
        self.channel = channel
        self.bitrate = bitrate
        self.host, self.port = host, port          # socketcand (CAN over TCP, e.g. the Cerbo GX's can0)
        self.capture_dir = capture_dir
        self.handlers: list[FrameHandler] = []
        self.stats = BusStats()
        self.connected = False
        self._bus: can.BusABC | None = None
        self._writer: can.Listener | None = None

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
                try:
                    async for msg in reader:
                        self._dispatch(msg)
                finally:
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

    def close(self) -> None:
        if self._writer is not None:
            self._writer.stop()
            self._writer = None
        if self._bus is not None:
            self._bus.shutdown()
            self._bus = None


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
