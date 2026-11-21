"""Peplink MAX BR1 Pro 5G adapter: local Router API (documented for firmware 8.6.0). READ-ONLY.

Auth, one of:
  * client: an `api.read-only` client → POST /api/auth.token.grant → ?accessToken=…
  * user:   the router's READ-ONLY web-admin user → POST /api/login → session cookie "bauth".
            The router itself grants GET only to that account ("Permission GET is granted for
            Read-only user access"), so the Pi never holds admin rights. On fw 8.6.1 creating an API
            client failed with 401 (likely InControl config lock / API hardening), so this is the default.
Credentials live in the root:aetherrv 0640 secrets file, never in the browser.

Endpoints used:
  status.wan.connection            per-WAN status; cellular carrier/RAT/band/RSRP/RSRQ/SINR, SIM, roaming
  status.wan.connection.allowance  per-SIM monthly usage vs limit (MB)
  info.location                    GPS (fw 8.0.1+)

PRIVACY: responses contain IMSI/ICCID/IMEI/APN credentials. `normalize_wans` keeps an explicit
whitelist of fields, so none of those ever reach state, logs or the UI.
Control endpoints (WAN priority, SIM switch, reboot) need the read-write scope and are NOT used.
"""

from __future__ import annotations

import asyncio
import logging
import ssl
import time

import aiohttp

from ..state import State

log = logging.getLogger(__name__)


class PeplinkError(Exception):
    pass


# ── Pure normalizers (tested against the documented response shapes) ─────────
def _best_band(cell: dict) -> dict:
    """Pick the primary band: fw ≥ 8.1.2 nests bands under `rat`, older fw has a flat `band`."""
    bands = []
    for rat in cell.get("rat") or []:
        for b in rat.get("band") or []:
            bands.append((rat.get("name"), b))
    if not bands:
        bands = [(None, b) for b in cell.get("band") or []]
    if not bands:
        return {}
    rat, b = max(bands, key=lambda rb: (rb[1].get("signal") or {}).get("rsrp", -999))
    sig = b.get("signal") or {}
    return {"rat": rat, "band": b.get("name"), "rsrp": sig.get("rsrp"), "rsrq": sig.get("rsrq"),
            "sinr": sig.get("sinr"), "rssi": sig.get("rssi"), "bands": len(bands)}


def normalize_wans(resp: dict) -> list[dict]:
    order = resp.get("order") or sorted(int(k) for k in resp if k.isdigit())
    out = []
    for cid in order:
        w = resp.get(str(cid)) or {}
        row = {
            "id": cid,
            "name": w.get("name") or f"WAN {cid}",
            "type": w.get("virtualType") or w.get("type"),
            "message": w.get("message") or "",
            "led": w.get("statusLed") or "gray",
            "enabled": bool(w.get("enable")),
            "priority": w.get("priority"),
            "uptime": w.get("uptime"),
            "connected": w.get("statusLed") == "green",
        }
        name_l = row["name"].lower()
        row["starlink"] = "starlink" in name_l
        cell = w.get("cellular") or w.get("gobi")
        if cell:
            sims = cell.get("sim") or {}
            active_sim = next((sid for sid in sims.get("order") or [] if (sims.get(str(sid)) or {}).get("active")), None)
            row["cellular"] = {
                "carrier": (cell.get("carrier") or {}).get("name"),
                "network": cell.get("mobileType") or cell.get("network"),
                "signal_level": cell.get("signalLevel"),
                "roaming": (cell.get("roamingStatus") or {}).get("message"),
                "ca": cell.get("carrierAggregation"),
                "sim": active_sim,
                **_best_band(cell),
            }
        wifi = w.get("wireless")
        if wifi:
            row["wifi"] = {"ssid": wifi.get("ssid"), "strength": (wifi.get("signal") or {}).get("strength")}
        out.append(row)
    return out


def active_wan(wans: list[dict]) -> dict | None:
    """The connected WAN with the best (lowest) priority number. Peplink priority 1 = highest."""
    up = [w for w in wans if w["enabled"] and w["connected"] and w.get("priority") is not None]
    return min(up, key=lambda w: w["priority"]) if up else None


def normalize_allowance(resp: dict) -> list[dict]:
    out = []
    for cid in resp.get("order") or []:
        conn = resp.get(str(cid)) or {}
        if "usage" in conn:  # non-cellular WAN: a single allowance object
            items = [(None, conn)]
        else:
            items = [(sid, conn.get(str(sid)) or {}) for sid in conn.get("order") or []]
        for sid, a in items:
            if a.get("enable"):
                out.append({"conn": cid, "sim": sid, "usage_mb": a.get("usage"), "limit_mb": a.get("limit"),
                            "percent": a.get("percent"), "start_day": a.get("start")})
    return out


def normalize_location(resp: dict) -> dict:
    loc = resp.get("location") or {}
    fix = bool(resp.get("gps")) and loc.get("latitude") is not None
    return {
        "location.fix": fix,
        "location.source": "peplink",
        "location.lat": loc.get("latitude") if fix else None,
        "location.lon": loc.get("longitude") if fix else None,
        "location.alt_m": loc.get("altitude") if fix else None,
        "location.speed": loc.get("speed") if fix else None,      # units undocumented (likely km/h)
        "location.heading": loc.get("heading") if fix else None,
        "location.hdop": loc.get("hdop") if fix else None,
        "location.time": loc.get("timestamp") if fix else None,
    }


# ── Client ───────────────────────────────────────────────────────────────────
class PeplinkClient:
    def __init__(self, host: str, client_id: str = "", client_secret: str = "", verify_tls: bool = False,
                 session: aiohttp.ClientSession | None = None, scheme: str = "https",
                 username: str = "", password: str = "") -> None:
        self.base = f"{scheme}://{host}/api"
        self.client_id, self.client_secret = client_id, client_secret
        self.username, self.password = username, password
        self.mode = "user" if username else "client"
        self.ssl = None if (verify_tls or scheme != "https") else _insecure_ctx()
        self._session = session
        self._token: str | None = None
        self._expires = 0.0

    async def _sess(self) -> aiohttp.ClientSession:
        if self._session is None or self._session.closed:
            # unsafe=True: aiohttp otherwise ignores cookies from bare-IP hosts like 192.0.2.10
            self._session = aiohttp.ClientSession(timeout=aiohttp.ClientTimeout(total=10),
                                                  cookie_jar=aiohttp.CookieJar(unsafe=True))
        return self._session

    async def _grant(self) -> None:
        if self.mode == "user":
            return await self._login()
        s = await self._sess()
        async with s.post(f"{self.base}/auth.token.grant", ssl=self.ssl,
                          json={"clientId": self.client_id, "clientSecret": self.client_secret,
                                "scope": "api.read-only"}) as r:
            body = await r.json(content_type=None)
        if body.get("stat") != "ok":
            raise PeplinkError(f"token grant failed: {body.get('code')} {body.get('message')}")
        resp = body["response"]
        self._token = resp["accessToken"]
        self._expires = time.time() + float(resp.get("expiresIn", 3600)) - 120

    async def _login(self) -> None:
        s = await self._sess()
        async with s.post(f"{self.base}/login", ssl=self.ssl,
                          json={"username": self.username, "password": self.password}) as r:
            body = await r.json(content_type=None)
        if body.get("stat") != "ok":
            raise PeplinkError(f"token grant failed (login): {body.get('code')} {body.get('message')}")
        perm = (body.get("response") or {}).get("permission") or {}
        if perm.get("POST"):
            # Refuse to run with write rights: AetherRV must use the router's read-only user.
            raise PeplinkError("login has POST (write) permission; configure the Peplink READ-ONLY user instead")
        self._token = "session"
        self._expires = time.time() + 3000      # re-login well before typical web-admin idle timeouts

    async def get(self, func: str, **params) -> dict:
        for attempt in (1, 2):
            if not self._token or time.time() > self._expires:
                await self._grant()
            s = await self._sess()
            q = {k: str(v) for k, v in params.items()}
            if self.mode == "client":
                q["accessToken"] = self._token
            async with s.get(f"{self.base}/{func}", params=q, ssl=self.ssl) as r:
                body = await r.json(content_type=None)
            if body.get("stat") == "ok":
                return body.get("response") or {}
            if attempt == 1 and body.get("code") in (401, 403):  # token expired/revoked → re-grant once
                self._token = None
                continue
            raise PeplinkError(f"{func}: {body.get('code')} {body.get('message')}")
        raise PeplinkError(f"{func}: unauthorized")

    async def close(self) -> None:
        if self._session is not None:
            await self._session.close()


def _insecure_ctx() -> ssl.SSLContext:
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE  # router uses a self-signed cert on the van LAN
    return ctx


class PeplinkAdapter:
    device = "peplink"

    def __init__(self, state: State, client: PeplinkClient) -> None:
        self.state = state
        self.client = client

    async def _wan(self) -> None:
        wans = normalize_wans(await self.client.get("status.wan.connection"))
        act = active_wan(wans)
        self.state.update({"peplink.reachable": True, "peplink.wans": wans,
                           "peplink.active": act["name"] if act else None,
                           "peplink.active_type": ("starlink" if act and act["starlink"] else act["type"]) if act else None,
                           "peplink.updated": round(time.time(), 1)})

    async def run(self) -> None:
        tick = 0
        while True:
            try:
                await self._wan()
                if tick % 4 == 0:                       # ~60 s
                    self.state.update(normalize_location(await self.client.get("info.location")))
                if tick % 20 == 0:                      # ~5 min
                    self.state.set("peplink.allowance", normalize_allowance(await self.client.get("status.wan.connection.allowance")))
            except asyncio.CancelledError:
                raise
            except Exception as e:
                log.debug("peplink poll failed: %s", e)
                self.state.set("peplink.reachable", False)
                if isinstance(e, PeplinkError) and "token grant" in str(e):
                    log.warning("peplink: %s (check the read-only client ID/secret)", e)
            tick += 1
            await asyncio.sleep(15)
