"""NMEA 0183 GPS listener (UDP). Receives the Peplink's "GPS Forwarding" stream. Receive-only.

The Peplink pushes NMEA sentences over UDP to configured destinations. Broadcasts to the van LAN
(e.g. 192.0.2.255) only reach hosts ON that LAN, so a Pi on the home LAN needs a unicast
destination (its own IP). Either way, AetherRV just binds a UDP port and parses what arrives.

Parsed (standard NMEA 0183, checksum verified; talker prefix ignored, so GP/GN/GL/… all work):
  RMC: fix status, lat/lon, speed over ground (knots), course, UTC date/time
  GGA: fix quality, satellites used, HDOP, altitude (m)
"""

from __future__ import annotations

import asyncio
import logging
import time
from datetime import datetime, timezone

from ..state import State

log = logging.getLogger(__name__)


def checksum_ok(sentence: str) -> bool:
    if not sentence.startswith("$") or "*" not in sentence:
        return False
    body, _, cs = sentence[1:].partition("*")
    calc = 0
    for ch in body:
        calc ^= ord(ch)
    try:
        return calc == int(cs[:2], 16)
    except ValueError:
        return False


def _coord(v: str, hemi: str) -> float | None:
    """ddmm.mmmm / dddmm.mmmm + N/S/E/W → signed decimal degrees."""
    if not v or not hemi:
        return None
    dot = v.find(".")
    deg_len = (dot if dot >= 0 else len(v)) - 2
    try:
        deg = float(v[:deg_len]) + float(v[deg_len:]) / 60.0
    except ValueError:
        return None
    return round(-deg if hemi in ("S", "W") else deg, 7)


def _f(v: str) -> float | None:
    try:
        return float(v)
    except ValueError:
        return None


def parse(sentence: str) -> tuple[str, dict] | None:
    """One NMEA sentence → (type, fields) for RMC/GGA, else None."""
    sentence = sentence.strip()
    if not checksum_ok(sentence):
        return None
    f = sentence[1:sentence.index("*")].split(",")
    kind = f[0][-3:]
    if kind == "RMC" and len(f) >= 10:
        valid = f[2] == "A"
        out = {"valid": valid, "lat": _coord(f[3], f[4]), "lon": _coord(f[5], f[6]),
               "speed_kn": _f(f[7]), "course": _f(f[8])}
        if f[1] and f[9] and len(f[9]) == 6:
            try:
                dt = datetime.strptime(f[9] + f[1].split(".")[0], "%d%m%y%H%M%S").replace(tzinfo=timezone.utc)
                out["time"] = dt.timestamp()
            except ValueError:
                pass
        return "RMC", out
    if kind == "GGA" and len(f) >= 10:
        q = int(f[6]) if f[6].isdigit() else 0
        return "GGA", {"quality": q, "lat": _coord(f[2], f[3]), "lon": _coord(f[4], f[5]),
                       "sats": int(f[7]) if f[7].isdigit() else None, "hdop": _f(f[8]), "alt_m": _f(f[9])}
    return None


class NmeaGps(asyncio.DatagramProtocol):
    device = "gps"

    def __init__(self, state: State, port: int = 10110, bind: str = "0.0.0.0", stale_after: float = 30.0,
                 source: str = "peplink") -> None:
        self.state, self.port, self.bind, self.stale_after, self.source = state, port, bind, stale_after, source
        self.last_fix = 0.0
        self.sentences = 0
        self.bad = 0

    # asyncio.DatagramProtocol
    def datagram_received(self, data: bytes, addr) -> None:
        for line in data.decode("ascii", "replace").splitlines():
            self.feed(line)

    def feed(self, line: str, now: float | None = None) -> None:
        now = time.time() if now is None else now
        r = parse(line)
        if r is None:
            if line.startswith("$"):
                self.bad += 1
            return
        self.sentences += 1
        kind, d = r
        up = {"location.source": f"{self.source} NMEA", "gps.sentences": self.sentences}
        if kind == "RMC":
            if d["valid"] and d["lat"] is not None:
                self.last_fix = now
                up.update({"location.fix": True, "location.lat": d["lat"], "location.lon": d["lon"],
                           "location.speed": round((d["speed_kn"] or 0) * 1.852, 1),   # km/h
                           "location.heading": d["course"], "location.time": d.get("time")})
            else:
                up["location.fix"] = False
        elif kind == "GGA":
            up.update({"location.sats": d["sats"], "location.hdop": d["hdop"]})
            if d["quality"] > 0 and d["alt_m"] is not None:
                up["location.alt_m"] = d["alt_m"]
        self.state.update(up)

    async def run(self) -> None:
        loop = asyncio.get_running_loop()
        transport, _ = await loop.create_datagram_endpoint(lambda: self, local_addr=(self.bind, self.port),
                                                           allow_broadcast=True)
        log.info("NMEA GPS listening on udp/%s:%d", self.bind, self.port)
        try:
            while True:
                await asyncio.sleep(5)
                if self.last_fix and time.time() - self.last_fix > self.stale_after:
                    self.state.set("location.fix", False)
        finally:
            transport.close()
