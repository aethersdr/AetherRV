"""Radar history beyond LibreWXR's 2 h: the Pi saves each published frame's tiles around the van and serves them
once the source has dropped them (it answers 404 for anything older than its 2 h list).

Same tiles as the Weather map asks for (512 px, colour scheme 6, no smoothing), at tile zooms 4-7 (map zooms
5-8), a small block around the van per zoom: ~21 tiles ≈ 0.2 MB a frame, ~30 MB a day. Frames are kept 6 h.
The newest frame is skipped each poll (it may still be filling in) and saved on the next one, while it's still
in the source's 2 h list. Forecast (nowcast) frames are never saved.
"""

from __future__ import annotations

import asyncio
import json
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
FLOW_Z, FLOW_N = 6, 32                          # motion estimated on the z6 block; served as a 32×32 grid
# Scheme-6 (NEXRAD) colours, weakest → strongest echo: radar tiles are palette images, the flow needs intensity.
NEXRAD = [(4, 233, 231), (1, 159, 244), (3, 0, 244), (2, 253, 2), (1, 197, 1), (0, 142, 0), (253, 248, 2), (229, 188, 0),
          (253, 149, 0), (253, 0, 0), (212, 0, 0), (188, 0, 0), (248, 0, 253), (152, 84, 198), (253, 253, 253)]


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
        self.flow_ok = True                             # False once numpy/OpenCV turn out to be unusable

    async def make_flows(self) -> None:
        """Motion between consecutive recorded frames (same z6 block, 10 min apart), off the event loop."""
        fr = self.frames()
        for a, b in zip(fr, fr[1:]):
            if b - a > 900 or (self.root / str(a) / "flow.json").exists():
                continue
            try:
                f = await asyncio.to_thread(flow_pair, self.root / str(a), self.root / str(b))
            except ImportError as e:
                log.warning("radar: no motion smoothing (%s)", e)
                self.flow_ok = False
                return
            except Exception as e:                          # noqa: BLE001 — a bad pair must not stop the recorder
                log.info("radar flow %s: %s", a, e)
                continue
            if f is not None:
                f["dt"] = b - a
                (self.root / str(a) / "flow.json").write_text(json.dumps(f, separators=(",", ":")))

    def frames(self) -> list[int]:
        """Times of the complete frames on disk, oldest first."""
        if not self.root.is_dir():
            return []
        return sorted(int(d.name) for d in self.root.iterdir() if d.name.isdigit() and (d / "done").exists())

    def flows(self) -> list[int]:
        """Start times of the frame pairs (t → next frame) that have a motion field."""
        return [t for t in self.frames() if (self.root / str(t) / "flow.json").exists()]

    def flow(self, t: int) -> Path | None:
        p = self.root / str(t) / "flow.json"
        return p if p.is_file() else None

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
        if self.flow_ok:
            await self.make_flows()
        self.state.update({"radar.recorded": len(self.frames()), "radar.flows": len(self.flows()),
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


def _origin(d: Path, z: int) -> tuple[int, int] | None:
    zd = d / str(z)
    xs = sorted(int(x.name) for x in zd.iterdir()) if zd.is_dir() else []
    ys = sorted(int(y.stem) for y in (zd / str(xs[0])).iterdir()) if xs else []
    return (xs[0], ys[0]) if xs and ys else None


def flow_pair(da: Path, db: Path) -> dict | None:
    """Coarse, smooth motion field from frame A to frame B (runs in a worker thread).

    Returned in z6 pixels (512 px tiles) over the pair's interval, on an N×N grid covering the z6 block:
    A(p) ≈ B(p + d). None when the two frames don't cover the same block (the van moved between them).
    """
    import cv2
    import numpy as np
    oa, ob = _origin(da, FLOW_Z), _origin(db, FLOW_Z)
    if oa is None or oa != ob:
        return None
    k = BLOCK[FLOW_Z]
    pal = np.array(NEXRAD, np.float32)

    def intensity(d: Path):
        rows = []
        for j in range(k):
            row = []
            for i in range(k):
                im = cv2.imread(str(d / str(FLOW_Z) / str(oa[0] + i) / f"{oa[1] + j}.png"), cv2.IMREAD_UNCHANGED)
                if im is None or im.ndim != 3 or im.shape[2] != 4:
                    raise ValueError("unreadable tile")
                row.append(im)
            rows.append(np.concatenate(row, axis=1))
        bgra = np.concatenate(rows, axis=0)
        out = np.zeros(bgra.shape[:2], np.float32)
        m = bgra[..., 3] > 0
        if m.any():
            cols, inv = np.unique(bgra[m][:, 2::-1], axis=0, return_inverse=True)      # BGR → RGB
            rank = np.argmin(((cols[:, None, :].astype(np.float32) - pal[None]) ** 2).sum(-1), axis=1) + 1
            out[m] = rank[inv.ravel()]
        small = cv2.resize(out * 16, None, fx=.5, fy=.5, interpolation=cv2.INTER_AREA)
        return cv2.GaussianBlur(np.clip(small, 0, 255).astype(np.uint8), (0, 0), 2)

    ga, gb = intensity(da), intensity(db)
    f = cv2.DISOpticalFlow_create(cv2.DISOPTICAL_FLOW_PRESET_MEDIUM).calc(ga, gb, None)
    f = cv2.GaussianBlur(f, (0, 0), 12)                      # coarse and smooth: per-pixel flow tears echoes
    g = cv2.resize(f, (FLOW_N, FLOW_N), interpolation=cv2.INTER_AREA) * 2      # half-res px → z6 px
    return {"z": FLOW_Z, "x0": oa[0], "y0": oa[1], "w": k, "n": FLOW_N,
            "dx": [round(float(v), 1) for v in g[..., 0].ravel()], "dy": [round(float(v), 1) for v in g[..., 1].ravel()]}
