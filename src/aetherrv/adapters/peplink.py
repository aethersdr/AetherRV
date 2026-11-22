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
  status.traffic                   live per-WAN bandwidth (kbps, overall download/upload)
  info.location                    GPS (fw 8.0.1+)

PRIVACY: responses contain IMSI/ICCID/IMEI/APN credentials. `normalize_wans` keeps an explicit
whitelist of fields, so none of those ever reach state, logs or the UI.
Control endpoints (WAN priority, SIM switch, reboot) need the read-write scope and are NOT used.
"""

from __future__ import annotations

import asyncio
import json
import logging
import sqlite3
import ssl
import time

import aiohttp

from ..state import State
from ..txgate import TxBlocked, TxGate

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
        wifi = w.get("wifi") or w.get("wireless")          # fw 8.6.1 reports "wifi"; older docs say "wireless"
        if isinstance(wifi, dict) or w.get("type") == "wifi":
            wifi = wifi if isinstance(wifi, dict) else {}
            sig = wifi.get("signal") or {}
            ch = wifi.get("channel")
            row["wifi"] = {"ssid": wifi.get("ssid"), "strength": sig.get("strength"), "level": sig.get("level"),
                           "channel": ch, "band": None if not isinstance(ch, int) else ("2.4 GHz" if ch <= 14 else "5 GHz")}
        out.append(row)
    return out


def normalize_bandwidth(resp: dict) -> dict[int, dict]:
    """status.traffic → {wan id: {"down_mbps", "up_mbps"}} from the live per-WAN "overall" bandwidth."""
    bw = (resp or {}).get("bandwidth") or {}
    scale = {"bps": 1e-6, "kbps": 1e-3, "mbps": 1.0}.get(str(bw.get("unit", "kbps")).lower(), 1e-3)
    out = {}
    for k, v in bw.items():
        if k.isdigit() and isinstance(v, dict):
            o = v.get("overall") or {}
            if o.get("download") is not None or o.get("upload") is not None:
                out[int(k)] = {"down_mbps": round((o.get("download") or 0) * scale, 3),
                               "up_mbps": round((o.get("upload") or 0) * scale, 3)}
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


def current_priority(current: dict, conn_id: int) -> int | None:
    """Priority of one connection from GET config.wan.connection.priority, whose shape on this firmware is
    {"order": [1, 2, …], "1": {"name", "group", "priority", "enable"}, …}. Strict: unexpected shape → ValueError."""
    order = current.get("order") if isinstance(current, dict) else None
    if not isinstance(order, list) or not order or not all(isinstance(c, int) for c in order):
        raise ValueError(f"unexpected config.wan.connection.priority shape: {str(current)[:200]}")
    e = current.get(str(conn_id))
    if conn_id not in order or not isinstance(e, dict) or "priority" not in e:
        raise ValueError(f"connection {conn_id} is not in the router's priority list")
    if not e.get("enable"):
        raise ValueError(f"connection {conn_id} is disabled on the router")
    return e.get("priority")


def order_targets(current: dict, order: list) -> dict[int, int]:
    """Drag-to-reorder: the enabled connections in `order` become P1, P2, P3, … (capped at P4). Every enabled
    connection on the router must appear exactly once; disabled ones are never included. Strict → ValueError."""
    if not isinstance(order, list) or not order or not all(isinstance(c, int) for c in order):
        raise ValueError("order must be a list of connection ids")
    if len(set(order)) != len(order):
        raise ValueError("order repeats a connection")
    for c in order:
        current_priority(current, c)                      # shape + present + enabled
    enabled = {c for c in current["order"] if isinstance(current.get(str(c)), dict) and current[str(c)].get("enable")}
    if set(order) != enabled:
        raise ValueError("order must list every enabled connection exactly once")
    return {c: min(i + 1, 4) for i, c in enumerate(order)}


def targets_payload(current: dict, targets: dict[int, int]) -> dict:
    """POST body setting several connections' priorities in ONE change (only those connections are sent)."""
    if not targets:
        raise ValueError("nothing to change")
    for c, p in targets.items():
        current_priority(current, c)
        if not 1 <= p <= 4:
            raise ValueError("priority must be 1–4")
    return {"instantActive": True, "list": [{"connId": c, "priority": p} for c, p in targets.items()]}


def priority_payload(current: dict, conn_id: int, priority: int) -> dict:
    """Body for POST config.wan.connection.priority (Peplink API reference): ONLY the connection being changed,
    so no other link's settings are touched; instantActive applies it without a separate 'apply changes'."""
    current_priority(current, conn_id)                     # validates shape + connection
    if not 1 <= priority <= 4:
        raise ValueError("priority must be 1–4")
    return {"instantActive": True, "list": [{"connId": conn_id, "priority": priority}]}


# ── Client ───────────────────────────────────────────────────────────────────
class PeplinkClient:
    def __init__(self, host: str, client_id: str = "", client_secret: str = "", verify_tls: bool = False,
                 session: aiohttp.ClientSession | None = None, scheme: str = "https",
                 username: str = "", password: str = "", scope: str = "api.read-only", allow_write: bool = False) -> None:
        self.base = f"{scheme}://{host}/api"
        self.scope = scope                       # "api" (read-write) only for the separate WAN-priority writer
        self.allow_write = allow_write           # True only for that writer (API client or read-write web user)
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
                                "scope": self.scope}) as r:
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
        if perm.get("POST") and not self.allow_write:
            # Refuse to run with write rights: AetherRV's monitoring must use the router's read-only user.
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

    async def post_once(self, func: str, body: dict) -> dict:
        """One POST, never repeated (a token grant beforehand isn't a send). Client (API-key) mode only."""
        if not self.allow_write or (self.mode == "client" and self.scope != "api"):
            raise PeplinkError("writes need the read-write API client or user")
        if not self._token or time.time() > self._expires:
            await self._grant()
        s = await self._sess()
        params = {"accessToken": self._token} if self.mode == "client" else {}
        async with s.post(f"{self.base}/{func}", params=params, json=body, ssl=self.ssl) as r:
            resp = await r.json(content_type=None)
        if resp.get("stat") != "ok":
            raise PeplinkError(f"{func}: {resp.get('code')} {resp.get('message')}")
        return resp.get("response") or {}

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
    COMMANDS = ("set_wan_priority", "set_wan_order")

    def __init__(self, state: State, client: PeplinkClient, writer: PeplinkClient | None = None,
                 gate: TxGate | None = None, db: sqlite3.Connection | None = None) -> None:
        self.state = state
        self.client = client                     # read-only: all monitoring
        self.writer, self.gate, self.db = writer, gate, db   # read-write API client: WAN priority only
        self._cmd_lock = asyncio.Lock()
        state.set("peplink.controls_ok", bool(writer and gate and gate.is_enabled(self.device)))
        if db is not None:
            db.execute("CREATE TABLE IF NOT EXISTS command_log (t REAL, device TEXT, cmd TEXT, value TEXT, "
                       "outcome TEXT, detail TEXT, client TEXT)")

    def _log(self, cmd: str, value, outcome: str, detail: str, client: str) -> None:
        log.warning("peplink command %s=%s from %s: %s %s", cmd, value, client, outcome, detail)
        if self.db is not None:
            try:
                self.db.execute("INSERT INTO command_log VALUES (?,?,?,?,?,?,?)",
                                (time.time(), self.device, cmd, json.dumps(value), outcome, detail, client))
            except sqlite3.Error:
                log.exception("command log write failed")

    async def command(self, cmd: str, value=None, client: str = "kiosk", verify_timeout: float = 20.0,
                      poll: float = 2.0) -> dict:
        """set_wan_priority {id, priority} | set_wan_order {order: [ids]}: fresh read of the router's priority
        list → one POST with just the changed connections → verify they all show the new priority."""
        async with self._cmd_lock:
            try:
                if self.gate is None:
                    raise TxBlocked("no gate")
                self.gate.check(self.device, cmd)
            except TxBlocked as e:
                self._log(cmd, value, "blocked", str(e), client)
                return {"ok": False, "detail": "control is disabled for the Peplink router"}
            if cmd not in self.COMMANDS:
                self._log(cmd, value, "rejected", "not an allowed command", client)
                return {"ok": False, "detail": f"command {cmd!r} is not allowed"}
            if self.writer is None:
                self._log(cmd, value, "rejected", "no read-write API client", client)
                return {"ok": False, "detail": "WAN control isn't set up (no read-write API client)"}
            try:
                if cmd == "set_wan_priority":
                    req = {int(value["id"]): int(value["priority"])}
                else:                                      # set_wan_order {order: [id, …]}
                    req = [int(c) for c in value["order"]]
            except (TypeError, KeyError, ValueError):
                self._log(cmd, value, "rejected", "bad value", client)
                return {"ok": False, "detail": "bad request"}
            try:
                cur = await asyncio.wait_for(self.writer.get("config.wan.connection.priority"), timeout=10)   # fresh
                targets = req if cmd == "set_wan_priority" else order_targets(cur, req)
                was = {c: current_priority(cur, c) for c in targets}
                changed = {c: p for c, p in targets.items() if was[c] != p}
                body = targets_payload(cur, changed) if changed else None
            except ValueError as e:
                self._log(cmd, value, "rejected", str(e), client)
                return {"ok": False, "detail": f"not sent: {e}"}
            except Exception as e:
                self._log(cmd, value, "failed", f"read: {e}", client)
                return {"ok": False, "detail": "router not reachable; nothing sent"}
            name = lambda c: (cur.get(str(c)) or {}).get("name") or f"WAN {c}"
            if not changed:
                self._log(cmd, value, "noop", "already set", client)
                return {"ok": True, "detail": "already in that order" if cmd == "set_wan_order" else f"already P{targets[next(iter(targets))]}"}
            try:
                await asyncio.wait_for(self.writer.post_once("config.wan.connection.priority", body), timeout=10)  # once
            except Exception as e:
                send_err = e                     # it may still have applied: verify, never resend
            else:
                send_err = None
            what = ", ".join(f"{name(c)} P{was[c]}→P{p}" for c, p in changed.items())
            deadline = time.monotonic() + verify_timeout
            while time.monotonic() < deadline:
                await asyncio.sleep(poll)
                try:
                    wans = normalize_wans(await self.writer.get("status.wan.connection"))
                except Exception:
                    continue                     # the router may be busy switching links
                now = {x["id"]: x.get("priority") for x in wans}
                if all(now.get(c) == p for c, p in changed.items()):
                    try:
                        await self._wan()
                    except Exception:
                        pass
                    self._log(cmd, value, "ok", what + (f" (send raised {send_err})" if send_err else ""), client)
                    if cmd == "set_wan_priority":
                        c, p = next(iter(changed.items()))
                        return {"ok": True, "detail": f"{name(c)} is now P{p}"}
                    return {"ok": True, "detail": "new order: " + ", ".join(f"P{p} {name(c)}" for c, p in targets.items())}
            self._log(cmd, value, "unverified", f"{what}; send={'error: ' + str(send_err) if send_err else 'ok'}; not seen", client)
            return {"ok": False, "detail": "the router didn't confirm the change (not retried)"}

    async def _wan(self) -> None:
        wans = normalize_wans(await self.client.get("status.wan.connection"))
        act = active_wan(wans)
        up: dict = {"peplink.reachable": True, "peplink.wans": wans,
                    "peplink.active": act["name"] if act else None,
                    "peplink.active_type": ("starlink" if act and act["starlink"] else act["type"]) if act else None,
                    "peplink.updated": round(time.time(), 1)}
        try:
            bw = normalize_bandwidth(await self.client.get("status.traffic"))
        except PeplinkError as e:
            log.debug("peplink status.traffic failed: %s", e)
            bw = {}
        cur = bw.get(act["id"]) if act else None
        # The active WAN's live throughput, recorded as one continuous history series (whichever WAN carries traffic).
        up.update({"wan.down_mbps": cur["down_mbps"] if cur else None, "wan.up_mbps": cur["up_mbps"] if cur else None})
        self.state.update(up)

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
