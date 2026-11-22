"""Radar history beyond LibreWXR's 2 h: the Pi saves each published frame's tiles around the van and serves them
once the source has dropped them (it answers 404 for anything older than its 2 h list).

Same tiles as the Weather map asks for (512 px, colour scheme 6, no smoothing), at tile zooms 4-7 (map zooms
5-8), a small block around the van per zoom: ~21 tiles ≈ 0.2 MB a frame, ~30 MB a day. Frames are kept 6 h.
The newest frame is skipped each poll (it may still be filling in) and saved on the next one, while it's still
in the source's 2 h list. Forecast (nowcast) frames are never saved.
"""

from __future__ import annotations

import asyncio
import logging
import math
import shutil
import struct
import time
import zlib
from pathlib import Path

import aiohttp

from .state import State

log = logging.getLogger(__name__)

INDEX = "https://api.librewxr.net/public/weather-maps.json"
HEADERS = {"User-Agent": "AetherRV/1.0"}        # the source rejects some default client agents
BLOCK = {4: 2, 5: 2, 6: 3, 7: 2}                # tile zoom → block width/height around the van (6 = default view)
KEEP_S = 6 * 3600


def _png_1x1_clear() -> bytes:
    def chunk(t: bytes, d: bytes) -> bytes:
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(b"\x00\x00\x00\x00\x00")) + chunk(b"IEND", b""))


CLEAR_PNG = _png_1x1_clear()                    # served for tiles outside the recorded block


def block(lat: float, lon: float, z: int, size: int) -> list[tuple[int, int]]:
    """The size×size tiles nearest the point (odd: centred on its tile; even: the quadrant it's closest to)."""
    n = 2 ** z
    fx = (lon + 180) / 360 * n
    r = math.radians(max(-85.0511, min(85.0511, lat)))
    fy = (1 - math.log(math.tan(r) + 1 / math.cos(r)) / math.pi) / 2 * n
    x0 = (math.floor(fx) if size % 2 else round(fx)) - size // 2
    y0 = (math.floor(fy) if size % 2 else round(fy)) - size // 2
    return [((x0 + i) % n, y0 + j) for j in range(size) for i in range(size) if 0 <= y0 + j < n]


class RadarRecorder:
    def __init__(self, state: State, root: Path, keep_s: float = KEEP_S, poll_s: float = 300):
        self.state, self.root, self.keep_s, self.poll_s = state, Path(root), keep_s, poll_s
        self.bytes, self.since = 0, time.time()

    def frames(self) -> list[int]:
        """Times of the complete frames on disk, oldest first."""
        if not self.root.is_dir():
            return []
        return sorted(int(d.name) for d in self.root.iterdir() if d.name.isdigit() and (d / "done").exists())

    def tile(self, t: int, z: int, x: int, y: int) -> Path | None:
        p = self.root / str(t) / str(z) / str(x) / f"{y}.png"
        return p if p.is_file() else None

    def prune(self, now: float | None = None) -> None:
        cut = (now or time.time()) - self.keep_s
        if not self.root.is_dir():
            return
        for d in self.root.iterdir():
            if d.name.isdigit() and int(d.name) < cut:
                shutil.rmtree(d, ignore_errors=True)

    async def run(self) -> None:
        try:
            self.root.mkdir(parents=True, exist_ok=True)
        except OSError as e:                                # e.g. a dev machine without /var/lib/aetherrv
            log.warning("radar recorder off: cannot create %s (%s)", self.root, e)
            return
        async with aiohttp.ClientSession(headers=HEADERS, timeout=aiohttp.ClientTimeout(total=30)) as s:
            while True:
                ran = True
                try:
                    ran = await self.poll(s)
                except Exception as e:                      # noqa: BLE001 — offline etc.; try again next poll
                    log.info("radar recorder: %s", type(e).__name__)
                await asyncio.sleep(self.poll_s if ran else 30)     # no GPS position yet (just started): soon

    async def poll(self, s: aiohttp.ClientSession) -> bool:
        lat, lon = self.state.get("location.lat"), self.state.get("location.lon")
        if lat is None or lon is None:
            return False
        async with s.get(INDEX) as r:
            r.raise_for_status()
            j = await r.json(content_type=None)
        host, past = j["host"], j["radar"]["past"]
        have = set(self.frames())
        for f in past[:-1]:                                 # every gap, oldest first; never the newest
            if f["time"] not in have:
                await self.save(s, host, f, float(lat), float(lon))
        self.prune()
        self.state.update({"radar.recorded": len(self.frames()),
                           "radar.rec_mb_per_day": round(self.bytes / 1e6 / max(1 / 24, (time.time() - self.since) / 86400), 1)})
        return True

    async def save(self, s: aiohttp.ClientSession, host: str, f: dict, lat: float, lon: float) -> None:
        d = self.root / str(f["time"])
        jobs = [(z, x, y) for z, size in BLOCK.items() for x, y in block(lat, lon, z, size)]
        sem = asyncio.Semaphore(4)

        async def one(z: int, x: int, y: int) -> bool:
            async with sem, s.get(f"{host}{f['path']}/512/{z}/{x}/{y}/6/0_0.png") as r:
                if r.status != 200:
                    return False
                body = await r.read()
            p = d / str(z) / str(x) / f"{y}.png"
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_bytes(body)
            self.bytes += len(body)
            return True
        ok = await asyncio.gather(*(one(*j) for j in jobs), return_exceptions=True)
        if all(o is True for o in ok):
            (d / "done").write_text(str(len(jobs)))
        else:                                               # incomplete: dropped, retried next poll if still listed
            shutil.rmtree(d, ignore_errors=True)
