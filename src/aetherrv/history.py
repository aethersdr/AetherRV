"""History of selected numeric state keys for the UI's trend charts, persisted in SQLite.

Samples every `interval` seconds into `samples(key, t, v)`. Rows older than `retention_days`
are pruned hourly. Queries bucket-average on the SQL side, so a 30-day chart is cheap.

Demo mode passes path=":memory:" so synthetic values never reach the real database.
Write volume is about 9 rows per 30 s (around 25 k rows/day), which is negligible for the NVMe.
"""

from __future__ import annotations

import asyncio
import logging
import sqlite3
import time
from pathlib import Path

from .state import State

log = logging.getLogger(__name__)

TRACKED = [
    "victron.battery.soc",
    "victron.battery.power",
    "victron.pv.power",
    "victron.dc.load",
    "victron.ac.load",
    "victron.ac.shore",
    "victron.alternator.power",
    "climate.cabin.temp",
    "climate.outside.temp",
    "climate.fridge.temp",
    "climate.freezer.temp",
    "climate.rixens.coolant",
    "climate.rixens.fuel_hz",
    "climate.rixens.inlet_c",
    "climate.rixens.outlet_c",
    "climate.rixens.flame_c",
    "climate.rixens.fan_rpm",
    "climate.rixens.fuel_gph",
    "starlink.down_mbps",
    "starlink.up_mbps",
    "starlink.latency_ms",
]

SCHEMA = """
CREATE TABLE IF NOT EXISTS samples (key TEXT NOT NULL, t REAL NOT NULL, v REAL NOT NULL);
CREATE INDEX IF NOT EXISTS samples_key_t ON samples (key, t);
"""


class History:
    def __init__(self, state: State, path: str | Path = ":memory:", interval: float = 30.0,
                 retention_days: float = 30.0) -> None:
        self.state = state
        self.interval = interval
        self.retention = retention_days * 86400
        self.path = str(path)
        if self.path != ":memory:":
            Path(self.path).parent.mkdir(parents=True, exist_ok=True)
        self.degraded = None
        try:
            self.db = self._open(self.path)
        except sqlite3.Error as e:
            # A broken/unwritable history DB must not take the console down: keep running in memory.
            log.error("history DB %s unusable (%s); falling back to in-memory history", self.path, e)
            self.degraded = f"{self.path}: {e}"
            self.path = ":memory:"
            self.db = self._open(":memory:")
        self._last_prune = 0.0
        log.info("history: %s (%d rows, %.0f-day retention)", self.path, self.count(), retention_days)

    @staticmethod
    def _open(path: str) -> sqlite3.Connection:
        db = sqlite3.connect(path, isolation_level=None, check_same_thread=False)
        if path != ":memory:":
            db.execute("PRAGMA journal_mode=WAL")
            db.execute("PRAGMA synchronous=NORMAL")
        db.executescript(SCHEMA)
        db.execute("INSERT INTO samples (key, t, v) SELECT 'x', 0, 0 WHERE 0")  # proves writability
        return db

    def count(self) -> int:
        return self.db.execute("SELECT count(*) FROM samples").fetchone()[0]

    def sample(self, now: float | None = None) -> None:
        now = time.time() if now is None else now
        rows = []
        for k in TRACKED:
            v = self.state.get(k)
            if isinstance(v, (int, float)) and not isinstance(v, bool):
                rows.append((k, round(now, 1), float(v)))
        if rows:
            self.db.executemany("INSERT INTO samples (key, t, v) VALUES (?, ?, ?)", rows)
        if now - self._last_prune > 3600:
            self.prune(now)

    def prune(self, now: float | None = None) -> int:
        now = time.time() if now is None else now
        self._last_prune = now
        cur = self.db.execute("DELETE FROM samples WHERE t < ?", (now - self.retention,))
        return cur.rowcount

    def get(self, key: str, since: float = 86400.0, max_points: int = 480,
            now: float | None = None) -> list[list[float]]:
        """Points for `key` over the last `since` seconds, averaged into at most `max_points` buckets."""
        if key not in TRACKED:
            return []
        now = time.time() if now is None else now
        start = now - since
        bucket = max(self.interval, since / max(1, max_points))
        rows = self.db.execute(
            "SELECT MAX(t), AVG(v) FROM samples WHERE key = ? AND t >= ? "
            "GROUP BY CAST((t - ?) / ? AS INTEGER) ORDER BY 1",
            (key, start, start, bucket),
        ).fetchall()
        return [[t, round(v, 2)] for t, v in rows]

    async def run(self) -> None:
        while True:
            try:
                self.sample()
            except sqlite3.Error:
                log.exception("history sample failed")
            await asyncio.sleep(self.interval)

    def close(self) -> None:
        self.db.close()
