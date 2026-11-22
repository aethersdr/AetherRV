"""TomTom traffic tiles for the Weather map, fetched by the backend so the API key never reaches a browser.

Two raster layers (TomTom Traffic API v4): flow with the 'reduced-sensitivity' style (only roads well below
free-flow speed, i.e. real slowdowns) and incidents (s0 / s0-dark). Tiles are cached for a few minutes and
shared by every screen; a daily cap keeps us inside the free tier and limits WAN data.
"""

from __future__ import annotations

import logging
import time
from collections import OrderedDict

import aiohttp

log = logging.getLogger(__name__)

BASE = "https://api.tomtom.com/traffic/map/4/tile"
STYLES = {"flow": {"major"}, "incidents": {"s0", "s0-dark"}}
TTL_S = 270                     # traffic refreshes about once a minute; the map asks every 5 min
MAX_TILES = 1500                # in-memory cache (512 px PNGs, mostly a few KB each)
DAILY_CAP = 20000               # upstream fetches per UTC day (free tier is far above this)


class TrafficTiles:
    def __init__(self, key: str, daily_cap: int = DAILY_CAP, ttl_s: float = TTL_S):
        self.key, self.cap, self.ttl = key, daily_cap, ttl_s
        self.cache: OrderedDict[tuple, tuple[float, int, bytes]] = OrderedDict()
        self.day, self.fetched = "", 0
        self.session: aiohttp.ClientSession | None = None
        self.last_error: str | None = None

    @staticmethod
    def valid(kind: str, style: str, z: int, x: int, y: int) -> bool:
        return style in STYLES.get(kind, ()) and 2 <= z <= 12 and 0 <= x < 2 ** z and 0 <= y < 2 ** z

    def url(self, kind: str, style: str, z: int, x: int, y: int) -> str:
        if kind == "flow":                          # reduced-sensitivity: only significant slowdowns get coloured
            return f"{BASE}/flow/reduced-sensitivity/{z}/{x}/{y}.png?key={self.key}&tileSize=512&thickness=6"
        return f"{BASE}/incidents/{style}/{z}/{x}/{y}.png?key={self.key}&tileSize=512&t=-1"

    async def get(self, kind: str, style: str, z: int, x: int, y: int) -> tuple[int, bytes]:
        """→ (http status, PNG bytes). 429 when the daily cap is hit; upstream errors pass through as 502."""
        k, now = (kind, style, z, x, y), time.time()
        hit = self.cache.get(k)
        if hit and now - hit[0] < self.ttl:
            self.cache.move_to_end(k)
            return hit[1], hit[2]
        day = time.strftime("%Y-%m-%d", time.gmtime(now))
        if day != self.day:
            self.day, self.fetched = day, 0
        if self.fetched >= self.cap:
            return (hit[1], hit[2]) if hit else (429, b"")       # stale beats nothing
        self.fetched += 1
        if self.session is None or self.session.closed:
            self.session = aiohttp.ClientSession(timeout=aiohttp.ClientTimeout(total=15))
        try:
            async with self.session.get(self.url(kind, style, z, x, y)) as r:
                body = await r.read()
                if r.status != 200:
                    err = f"HTTP {r.status}"
                    if err != self.last_error:                      # once per change, never the URL (holds the key)
                        log.warning("traffic tiles: TomTom answered %s", err)
                    self.last_error = err
                    return (hit[1], hit[2]) if hit else (502, b"")
        except (aiohttp.ClientError, TimeoutError) as e:
            if self.last_error != type(e).__name__:
                log.warning("traffic tiles: %s", type(e).__name__)
            self.last_error = type(e).__name__
            return (hit[1], hit[2]) if hit else (502, b"")
        self.last_error = None
        self.cache[k] = (now, 200, body)
        self.cache.move_to_end(k)
        while len(self.cache) > MAX_TILES:
            self.cache.popitem(last=False)
        return 200, body
