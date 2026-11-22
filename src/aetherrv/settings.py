"""Settings: talks to the root admin helper (deploy/admin/aetherrv_admin.py) and keeps AetherRV's own prefs.

* AdminClient   one JSON request per connection over /run/aetherrv-admin.sock (root helper, fixed verb list)
* Prefs         AetherRV-only preferences in /var/lib/aetherrv/settings.json (screen auto-off minutes, ...);
                published as state keys `settings.*` so every screen sees them
* AutoTimezone  sets the system time zone from the van's GPS fix (owner: no manual time-zone setting).
                The zone name comes from Open-Meteo (timezone=auto, no key); offline, the last zone is kept.

Every change is accepted only from the van's own touchscreen (loopback), like device controls.
Secrets (Wi-Fi passphrases) pass straight through to the helper: never logged, never put in State.
"""
from __future__ import annotations

import asyncio
import json
import logging
import math
import os
import time
from pathlib import Path

import aiohttp

from .state import State

log = logging.getLogger(__name__)

SOCK = "/run/aetherrv-admin.sock"
PREFS_DEFAULT = {"screen_off_min": 10, "auto_timezone": True,
                 "fridge_off": False, "freezer_off": False}   # set from a warm alert; cleared automatically once cold
PREFS_LIMITS = {"screen_off_min": (0, 240)}          # 0 = never


class AdminError(Exception):
    pass


class AdminClient:
    def __init__(self, path: str = SOCK) -> None:
        self.path = path

    async def call(self, verb: str, args: dict | None = None, timeout: float = 60) -> dict:
        try:
            r, w = await asyncio.wait_for(asyncio.open_unix_connection(self.path), 5)
        except (OSError, asyncio.TimeoutError) as e:
            raise AdminError(f"admin helper unavailable ({e.__class__.__name__})") from e
        try:
            w.write(json.dumps({"verb": verb, "args": args or {}}).encode() + b"\n")
            await w.drain()
            line = await asyncio.wait_for(r.readline(), timeout)
            return json.loads(line or b"{}")
        except (OSError, ValueError, asyncio.TimeoutError) as e:
            raise AdminError(f"admin helper error ({e.__class__.__name__})") from e
        finally:
            w.close()


class Prefs:
    def __init__(self, state: State, path: str = "/var/lib/aetherrv/settings.json") -> None:
        self.state, self.path = state, Path(path)
        self.values = dict(PREFS_DEFAULT)
        try:
            self.values.update({k: v for k, v in json.loads(self.path.read_text()).items() if k in PREFS_DEFAULT})
        except (OSError, ValueError):
            pass
        self._publish()

    def _publish(self) -> None:
        self.state.update({f"settings.{k}": v for k, v in self.values.items()})

    def set(self, key: str, value) -> None:
        if key not in PREFS_DEFAULT:
            raise ValueError(f"unknown setting {key!r}")
        default = PREFS_DEFAULT[key]
        if isinstance(default, bool):
            value = bool(value)
        elif isinstance(default, int):
            value = int(value)
            lo, hi = PREFS_LIMITS.get(key, (value, value))
            if not lo <= value <= hi:
                raise ValueError(f"{key} must be {lo}–{hi}")
        self.values[key] = value
        try:
            tmp = self.path.with_suffix(".tmp")
            tmp.write_text(json.dumps(self.values, indent=1))
            os.replace(tmp, self.path)
        except OSError as e:
            log.warning("settings not saved: %s", e)
        self._publish()


def _km(a: tuple[float, float], b: tuple[float, float]) -> float:
    r = math.pi / 180
    h = (math.sin((b[0] - a[0]) * r / 2) ** 2
         + math.cos(a[0] * r) * math.cos(b[0] * r) * math.sin((b[1] - a[1]) * r / 2) ** 2)
    return 2 * 6371 * math.asin(math.sqrt(h))


class AutoTimezone:
    """Keep the system time zone matching the van's GPS position.

    Looks up at most every 15 min, or sooner after a > 25 km move; changes the zone only when the lookup
    returns a different, valid zone twice in a row (no flapping at a border). Uses only the van's own fix
    (location.fix), never a forecast pin. Offline → keeps the current zone."""

    def __init__(self, state: State, admin: AdminClient, prefs: Prefs, interval: float = 60) -> None:
        self.state, self.admin, self.prefs, self.interval = state, admin, prefs, interval
        self._last_pos: tuple[float, float] | None = None
        self._last_at = 0.0
        self._pending: str | None = None

    async def lookup(self, lat: float, lon: float) -> str | None:
        url = "https://api.open-meteo.com/v1/forecast"
        q = {"latitude": f"{lat:.3f}", "longitude": f"{lon:.3f}", "timezone": "auto", "forecast_days": "1",
             "daily": "temperature_2m_max"}
        async with aiohttp.ClientSession(timeout=aiohttp.ClientTimeout(total=15)) as s:
            async with s.get(url, params=q) as r:
                if r.status != 200:
                    return None
                tz = (await r.json()).get("timezone")
                return tz if isinstance(tz, str) and tz and tz != "GMT" else None

    @staticmethod
    def current() -> str | None:
        try:
            p = os.path.realpath("/etc/localtime")
            return p.split("/zoneinfo/", 1)[1] if "/zoneinfo/" in p else None
        except OSError:
            return None

    async def step(self) -> None:
        self.state.set("settings.timezone", self.current())
        if not self.prefs.values.get("auto_timezone", True) or not self.state.get("location.fix"):
            return
        lat, lon = self.state.get("location.lat"), self.state.get("location.lon")
        if lat is None or lon is None:
            return
        pos = (float(lat), float(lon))
        moved = self._last_pos is None or _km(self._last_pos, pos) > 25
        if not moved and time.time() - self._last_at < 15 * 60 and self._pending is None:
            return
        try:
            tz = await self.lookup(*pos)
        except (aiohttp.ClientError, asyncio.TimeoutError, ValueError):
            tz = None                                   # offline: keep the current zone
        self._last_pos, self._last_at = pos, time.time()
        if not tz or tz == self.current():
            self._pending = None
            return
        if self._pending != tz:                         # need the same answer twice before changing
            self._pending = tz
            self._last_at = 0                           # re-check on the next step
            return
        try:
            res = await self.admin.call("set_timezone", {"tz": tz})
        except AdminError as e:
            log.warning("auto time zone: %s", e)
            return
        if res.get("ok"):
            log.info("time zone set from GPS: %s", tz)
            time.tzset()                                # this process: local midnight etc. follow the new zone
            self.state.set("settings.timezone", tz)
        else:
            log.warning("auto time zone refused: %s", res.get("error"))
        self._pending = None

    async def run(self) -> None:
        while True:
            try:
                await self.step()
            except Exception:
                log.exception("auto time zone step failed")
            await asyncio.sleep(self.interval)


TS_V4, TS_V6 = "100.64.0.0/10", "fd7a:115c:a1e0::/48"


class RemoteAuth:
    """Who may control the van: the touchscreen (loopback); the owner's own Tailscale devices (owner's choice,
    2026-11-20) or, with tailnet = "all", every tailnet peer; and browsers in the configured control_subnets
    (owner, 2026-11-21: the van and home LANs). Everyone else is view-only.

    A Tailscale peer is identified by asking tailscaled (via the root helper) who owns the source address,
    and the connection must have arrived on this console's own Tailscale address. Results cached 60 s."""

    def __init__(self, admin: AdminClient, ttl: float = 60, control_subnets: list[str] | None = None,
                 tailnet: str = "owner") -> None:
        """control_subnets: LAN ranges whose browsers get full control (owner, 2026-11-21: the van and home
        LANs). tailnet: "owner" (only the console owner's own untagged devices) or "all" (any tailnet peer)."""
        import ipaddress
        self.admin, self.ttl = admin, ttl
        self._cache: dict[str, tuple[float, str | None]] = {}
        self.subnets = [ipaddress.ip_network(s, strict=False) for s in (control_subnets or [])]
        self.tailnet_all = str(tailnet).lower() == "all"

    @staticmethod
    def _is_ts(ip: str) -> bool:
        import ipaddress
        try:
            a = ipaddress.ip_address(ip.split("%")[0])
        except ValueError:
            return False
        return a in ipaddress.ip_network(TS_V4) or a in ipaddress.ip_network(TS_V6)

    async def who(self, remote: str | None, local_ip: str | None = None) -> str | None:
        """'kiosk', 'tailscale:<login>@<node>', or None (view-only)."""
        if remote in ("127.0.0.1", "::1"):
            return "kiosk"
        if remote and self.subnets:
            import ipaddress
            try:
                a = ipaddress.ip_address(remote.split("%")[0])
            except ValueError:
                a = None
            if a is not None and any(a in n for n in self.subnets):
                return f"lan:{a}"
        if not remote or not self._is_ts(remote):
            return None
        hit = self._cache.get(remote)
        if hit and time.monotonic() - hit[0] < self.ttl:
            who, self_ips = hit[1]
        else:
            try:
                r = await self.admin.call("tailscale_whois", {"ip": remote.split("%")[0]}, timeout=15)
            except AdminError:
                return None
            peer = r.get("ok") and r.get("known") is not False and (r.get("owner") or self.tailnet_all)
            who = f"tailscale:{r.get('login')}@{r.get('node')}" if peer else None
            self_ips = r.get("self_ips") or []
            self._cache[remote] = (time.monotonic(), (who, self_ips))
        # Must have come in on this console's own Tailscale address (not a LAN path with a 100.x source).
        if who and local_ip is not None and self_ips and local_ip.split("%")[0] not in self_ips:
            return None
        return who
