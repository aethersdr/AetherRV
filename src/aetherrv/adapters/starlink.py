"""Starlink dish adapter (local gRPC at 192.168.100.1:9200 via starlink-grpc-core).

Uses `starlink_grpc.status_data()`, which resolves the dish protobufs by server reflection
(nothing to compile). The library is synchronous, so each poll runs in a worker thread.
The obstruction map is read once a minute (get_obstruction_map).

Controls (owner-approved 2026-11-22; all through TxGate device "starlink", tx_enabled): each reads
fresh state first, sends exactly once on a brand-new gRPC channel (the library retries once on a
*reused* channel, so the read and the send never share one), never retries, and verifies:
  * install_update — reboot ONLY while a downloaded update waits (software_update_state REBOOT_REQUIRED)
  * reboot         — plain reboot; verified by the dish going away or its uptime resetting
  * check_update   — `update` request WITHOUT schedule_reboot (the dish checks/downloads, never reboots)
  * set_heating    — snow-melt mode auto | on | off (dish_set_config)
  * set_sleep      — sleep schedule {enabled, start_utc_min, duration_min} (dish_set_config power_save_*)
Not used: stow, GPS inhibit, factory reset, clear obstruction map, test modes.

Not available: location (removed from gRPC for most plans in May 2026). Use the Peplink GPS.
Behind the Peplink: give the Starlink WAN a "Management IP" (e.g. 192.168.100.100/24); on fw 8.6.1
that field is under "Click here for other DHCP settings" in the WAN dialog. The van LAN must not
overlap 192.168.100.0/24.
"""

from __future__ import annotations

import asyncio
import base64
import logging
import sqlite3
import time

from ..state import State
from ..txgate import TxBlocked, TxGate

log = logging.getLogger(__name__)

# Dish alerts we surface, with the severity the alert center should use. Unknown alert
# names (newer firmware) still show in the list as "warning".
ALERT_SEVERITY = {
    "alert_motors_stuck": "serious",
    "alert_thermal_shutdown": "serious",
    "alert_thermal_throttle": "warning",
    "alert_mast_not_near_vertical": "warning",
    "alert_unexpected_location": "warning",
    "alert_slow_ethernet_speeds": "warning",
    "alert_power_supply_thermal_throttle": "warning",
    "alert_dish_water_detected": "serious",
    "alert_no_ethernet_link": "serious",
    "alert_install_pending": "warning",
    "alert_is_heating": None,            # informational: snow melt / heating, not a fault
    "alert_is_power_save_idle": None,    # informational: sleep schedule
}


def pretty_alert(name: str) -> str:
    return name.removeprefix("alert_").replace("_", " ").capitalize()


def signal_fields(raw) -> dict:
    """Signal quality from the raw dish status (newer firmware): signal_quality 0..1 = SNR/10 dB, clamped,
    so 1.0 means SNR >= 10 dB. Absent on older firmware → None."""
    # proto3 omits a 0.0 on the wire, so "set" can't tell 0 dB from "not supported": instead check whether this
    # firmware's message (resolved from the dish by reflection) defines the field at all.
    q = getattr(raw, "signal_quality", None) if raw is not None else None
    desc = getattr(raw, "DESCRIPTOR", None)
    if desc is not None and "signal_quality" not in [f.name for f in desc.fields]:
        q = None
    q = float(q) if q is not None else None
    return {
        "starlink.signal_quality": None if q is None else round(q, 3),
        "starlink.snr_db": None if q is None else round(min(max(q, 0.0), 1.0) * 10, 1),
        "starlink.snr_capped": None if q is None else q >= 1.0,
        "starlink.snr_above_noise": getattr(raw, "is_snr_above_noise_floor", None) if raw is not None else None,
        "starlink.snr_persistently_low": getattr(raw, "is_snr_persistently_low", None) if raw is not None else None,
    }


def normalize(status: dict, obstruction: dict, alerts: dict) -> dict:
    """Map starlink_grpc.status_data() dicts to AetherRV state keys (pure, testable)."""
    active = [k for k, v in (alerts or {}).items() if v]
    out = {
        "starlink.reachable": True,
        "starlink.state": status.get("state") or "UNKNOWN",
        "starlink.online": status.get("state") == "CONNECTED",
        "starlink.uptime": status.get("uptime"),
        "starlink.software": status.get("software_version"),
        "starlink.hardware": status.get("hardware_version"),
        "starlink.latency_ms": _r(status.get("pop_ping_latency_ms"), 1),
        "starlink.drop_rate": _r(status.get("pop_ping_drop_rate"), 4),
        "starlink.down_mbps": _r((status.get("downlink_throughput_bps") or 0) / 1e6, 2),
        "starlink.up_mbps": _r((status.get("uplink_throughput_bps") or 0) / 1e6, 2),
        "starlink.obstructed_pct": _r((status.get("fraction_obstructed") or 0) * 100, 2),
        "starlink.currently_obstructed": bool(status.get("currently_obstructed")),
        "starlink.azimuth": _r(status.get("direction_azimuth"), 1),
        "starlink.elevation": _r(status.get("direction_elevation"), 1),
        "starlink.gps_sats": status.get("gps_sats"),
        "starlink.update_state": status.get("software_update_state"),
        "starlink.alerts": [{"id": a, "label": pretty_alert(a), "severity": ALERT_SEVERITY.get(a, "warning")}
                            for a in active],
    }
    return out


FRAMES = {1: "earth", 2: "dish"}   # MapReferenceFrame: FRAME_EARTH (fixed plans, top = north), FRAME_UT (mobile/roam:
                                   # bottom edge = the direction the dish faces, i.e. direction_azimuth)


def encode_obstruction_map(rows: int, cols: int, snr, min_el: float | None = None, max_theta: float | None = None,
                           frame: int | None = None) -> dict:
    """Pack the dish's sky map (rows×cols SNR quality, -1 = no data, 0 = obstructed … 1 = clear) into one byte per
    cell, base64: 255 = no data, else round(snr × 254). 123×123 → ~20 KB of state, refreshed about once a minute."""
    if rows * cols != len(snr):
        raise ValueError(f"obstruction map {rows}x{cols} but {len(snr)} cells")
    raw = bytes(255 if v is None or v < 0 else round(min(1.0, float(v)) * 254) for v in snr)
    return {"rows": rows, "cols": cols, "min_el": min_el, "max_theta": max_theta,
            "frame": FRAMES.get(frame or 0, "unknown"), "data": base64.b64encode(raw).decode("ascii")}


def _r(v, n):
    return None if v is None else round(float(v), n)


class DishOps:
    """The two dish calls a command needs. Each call opens its OWN fresh channel and closes it, so the
    library's retry-on-reused-channel never applies (a send can't be repeated behind our back)."""

    def __init__(self, target: str) -> None:
        self.target = target

    def status(self) -> dict:
        import starlink_grpc
        ctx = starlink_grpc.ChannelContext(target=self.target)
        try:
            st, *_ = starlink_grpc.status_data(context=ctx)
            return st
        finally:
            ctx.close()

    def reboot(self) -> None:
        import starlink_grpc
        ctx = starlink_grpc.ChannelContext(target=self.target)
        try:
            starlink_grpc.reboot(context=ctx)
        finally:
            ctx.close()

    def _handle(self, **req):
        """One Handle() call on a fresh channel (never retried)."""
        import grpc
        import starlink_grpc
        ch = grpc.insecure_channel(self.target)
        try:
            if starlink_grpc.imports_pending:
                starlink_grpc.resolve_imports(ch)
            return starlink_grpc.DeviceStub(ch).Handle(starlink_grpc.Request(**req), timeout=10)
        finally:
            ch.close()

    def get_config(self) -> dict:
        return config_fields(self._handle(dish_get_config={}).dish_get_config.dish_config)

    def set_config(self, **fields) -> None:
        cfg = {}
        for k, v in fields.items():
            cfg[k] = v
            cfg["apply_" + k] = True
        self._handle(dish_set_config={"dish_config": cfg})

    def check_update(self) -> None:
        self._handle(update={})                     # no schedule_reboot: check/download only


SNOW_MELT = {0: "auto", 1: "on", 2: "off"}           # DishConfig.SnowMeltMode AUTO / ALWAYS_ON / ALWAYS_OFF


def _pretty_reason(name: str) -> tuple[str, bool]:
    """EVENT_REASON_OUTAGE_NO_DOWNLINK → ("No downlink", True). Second item: is it an outage."""
    n = name.removeprefix("EVENT_REASON_")
    outage = n.startswith("OUTAGE_")
    n = n.removeprefix("OUTAGE_").removeprefix("UT_ALERT_").removeprefix("ROUTER_")
    s = n.replace("_", " ").capitalize()
    for a, b in ((" snr", " SNR"), ("Snr", "SNR"), ("ipv4", "IPv4"), ("ipv6", "IPv6"), (" lan", " LAN"), (" wan", " WAN"),
                 (" eth", " Ethernet"), ("Eth ", "Ethernet "), (" dhcp", " DHCP"), (" dns", " DNS"), (" pop", " PoP"),
                 (" rf", " RF"), (" ut", " dish")):
        s = s.replace(a, b)
    return s, outage


def history_fields(h, max_events: int = 50) -> dict:
    """From the dish's rolling history (get_history): average power over the last minute (power_in is 1 Hz,
    900 s ring) and the newest events/outages (event_log; device ids deliberately not copied)."""
    out: dict = {}
    pin = list(getattr(h, "power_in", []) or [])
    if pin:
        n, cur = len(pin), int(getattr(h, "current", 0))
        last = [pin[(cur - 1 - i) % n] for i in range(min(60, n, cur))] if cur else pin[-60:]
        last = [x for x in last if x and x > 0]
        if last:
            out["starlink.power_w"] = round(sum(last) / len(last), 1)
    evs = []
    log_ = getattr(h, "event_log", None)
    for e in list(getattr(log_, "events", []) or []):
        try:
            rname = e.DESCRIPTOR.fields_by_name["reason"].enum_type.values_by_number[e.reason].name
            sname = e.DESCRIPTOR.fields_by_name["severity"].enum_type.values_by_number[e.severity].name
        except (KeyError, AttributeError):
            rname, sname = f"EVENT_REASON_{getattr(e, 'reason', 0)}", "EVENT_SEVERITY_UNKNOWN"
        label, outage = _pretty_reason(rname)
        evs.append({"t": round(e.start_timestamp_ns / 1e9, 1), "dur_s": round(e.duration_ns / 1e9, 1),
                    "what": label, "outage": outage, "sev": sname.removeprefix("EVENT_SEVERITY_").lower()})
    if log_ is not None:
        evs.sort(key=lambda x: -x["t"])
        out["starlink.events"] = evs[:max_events]
        span = (getattr(log_, "current_timestamp_ns", 0) - getattr(log_, "start_timestamp_ns", 0)) / 1e9
        out["starlink.events_span_s"] = round(span) if span > 0 else None
    return out


def config_fields(c) -> dict:
    """The dish settings AetherRV shows (heating, sleep schedule) from a DishConfig message."""
    return {"heating": SNOW_MELT.get(int(getattr(c, "snow_melt_mode", 0)), "auto"),
            "sleep": {"enabled": bool(getattr(c, "power_save_mode", False)),
                      "start_utc_min": int(getattr(c, "power_save_start_minutes", 0)),
                      "duration_min": int(getattr(c, "power_save_duration_minutes", 0))}}


class StarlinkAdapter:
    device = "starlink"
    COMMANDS = ("install_update", "reboot", "check_update", "set_heating", "set_sleep")

    MAP_EVERY = 60.0   # seconds between obstruction-map fetches (the map only accumulates slowly)

    def __init__(self, state: State, target: str = "192.168.100.1:9200", interval: float = 5.0,
                 gate: TxGate | None = None, db: sqlite3.Connection | None = None, ops: DishOps | None = None) -> None:
        self.state = state
        self.target = target
        self.interval = interval
        self._ctx = None
        self._map_at = 0.0
        self.gate, self.db = gate, db
        self.ops = ops or DishOps(target)
        self._cmd_lock = asyncio.Lock()
        state.set("starlink.controls_ok", bool(gate and gate.is_enabled(self.device)))
        if db is not None:
            db.execute("CREATE TABLE IF NOT EXISTS command_log (t REAL, device TEXT, cmd TEXT, value TEXT, "
                       "outcome TEXT, detail TEXT, client TEXT)")

    def _log(self, cmd: str, value, outcome: str, detail: str, client: str) -> None:
        log.warning("starlink command %s=%s from %s: %s %s", cmd, value, client, outcome, detail)
        if self.db is not None:
            try:
                self.db.execute("INSERT INTO command_log VALUES (?,?,?,?,?,?,?)",
                                (time.time(), self.device, cmd, str(value), outcome, detail, client))
            except sqlite3.Error:
                log.exception("command log write failed")

    async def command(self, cmd: str, value=None, client: str = "kiosk", verify_timeout: float = 90.0,
                      poll: float = 3.0) -> dict:
        """Fresh read → send once (never retried) → verify. See the module docstring for each command."""
        async with self._cmd_lock:
            try:
                if self.gate is None:
                    raise TxBlocked("no gate")
                self.gate.check(self.device, cmd)
            except TxBlocked as e:
                self._log(cmd, value, "blocked", str(e), client)
                return {"ok": False, "detail": "control is disabled for the Starlink dish"}
            if cmd not in self.COMMANDS:
                self._log(cmd, value, "rejected", "not an allowed command", client)
                return {"ok": False, "detail": f"command {cmd!r} is not allowed"}
            if cmd in ("set_heating", "set_sleep"):
                return await self._set_config(cmd, value, client, poll)
            try:
                st = await asyncio.wait_for(asyncio.to_thread(self.ops.status), timeout=15)   # fresh, never the poll cache
            except Exception as e:
                self._log(cmd, value, "failed", f"status read: {e}", client)
                return {"ok": False, "detail": "dish not reachable; nothing sent"}
            upd = st.get("software_update_state")
            if cmd == "check_update":
                return await self._check_update(value, client, upd, poll)
            if cmd == "install_update" and upd != "REBOOT_REQUIRED":
                self._log(cmd, value, "rejected", f"software_update_state={upd}", client)
                return {"ok": False, "detail": "no update is waiting to install"}
            return await self._reboot_verify(cmd, value, client, st, verify_timeout, poll)

    async def _reboot_verify(self, cmd, value, client, st, verify_timeout, poll) -> dict:
        before, send_err = st.get("uptime"), None
        try:
            await asyncio.wait_for(asyncio.to_thread(self.ops.reboot), timeout=20)          # exactly once
        except Exception as e:
            # The dish may have received it (e.g. a deadline expiring as it goes down): verify, never resend.
            send_err = e
            log.info("starlink reboot call raised %s; verifying instead of retrying", e)
        self.state.set("starlink.restart_requested", round(time.time(), 1))
        what = "installing the update" if cmd == "install_update" else "rebooting"
        deadline = time.monotonic() + verify_timeout
        while time.monotonic() < deadline:
            await asyncio.sleep(poll)
            try:
                now = await asyncio.wait_for(asyncio.to_thread(self.ops.status), timeout=8)
            except Exception:
                if send_err is not None:        # unreachable after a failed send: can't tell what happened
                    self._log(cmd, value, "unverified", f"send raised {send_err}; dish unreachable since", client)
                    return {"ok": False, "detail": "the request may not have reached the dish (not retried)"}
                self._log(cmd, value, "ok", f"dish went offline after the reboot request (uptime was {before}s)", client)
                return {"ok": True, "detail": f"the dish is {what} (offline a few minutes)"}
            up = now.get("uptime")
            if before is not None and up is not None and up < before:
                self._log(cmd, value, "ok", f"uptime reset {before}s → {up}s", client)
                return {"ok": True, "detail": f"the dish restarted ({what})"}
        self._log(cmd, value, "unverified", f"no restart seen within {verify_timeout:.0f}s", client)
        return {"ok": False, "detail": "the dish did not restart (not retried)"}

    async def _check_update(self, value, client, before, poll, watch: float = 30.0) -> dict:
        send_err = None
        try:
            await asyncio.wait_for(asyncio.to_thread(self.ops.check_update), timeout=15)    # once, no reboot
        except Exception as e:
            send_err = e
        seen, deadline = before, time.monotonic() + watch
        while time.monotonic() < deadline:
            await asyncio.sleep(poll)
            try:
                seen = (await asyncio.wait_for(asyncio.to_thread(self.ops.status), timeout=8)).get("software_update_state")
            except Exception:
                continue
            if seen != before or seen == "REBOOT_REQUIRED":
                break
        self.state.set("starlink.update_state", seen)
        self.state.set("starlink.update_checked", round(time.time(), 1))
        detail = {"REBOOT_REQUIRED": "an update is downloaded and ready to install",
                  "IDLE": "no update available (up to date)"}.get(seen or "", f"update state: {str(seen).replace('_', ' ').lower()}")
        self._log("check_update", value, "error" if send_err else "ok",
                  f"{before} → {seen}" + (f"; send raised {send_err}" if send_err else ""), client)
        if send_err and seen == before:
            return {"ok": False, "detail": f"the dish didn't accept the check ({send_err}); {detail}"}
        return {"ok": True, "detail": detail}

    async def _set_config(self, cmd, value, client, poll, verify_timeout: float = 15.0) -> dict:
        try:
            if cmd == "set_heating":
                mode = {"auto": 0, "on": 1, "off": 2}[str(value)]
                want = {"heating": str(value)}
                fields = {"snow_melt_mode": mode}
            else:
                en = bool(value["enabled"])
                start, dur = int(value.get("start_utc_min", 0)), int(value.get("duration_min", 0))
                if en and not (0 <= start < 1440 and 1 <= dur <= 1439):
                    raise ValueError("start 0–1439 min, duration 1–1439 min")
                want = {"sleep": {"enabled": en, **({"start_utc_min": start, "duration_min": dur} if en else {})}}
                fields = {"power_save_mode": en, **({"power_save_start_minutes": start, "power_save_duration_minutes": dur} if en else {})}
        except (KeyError, TypeError, ValueError) as e:
            self._log(cmd, value, "rejected", f"bad value: {e}", client)
            return {"ok": False, "detail": "bad request"}
        def matches(c: dict) -> bool:
            if "heating" in want:
                return c["heating"] == want["heating"]
            return all(c["sleep"].get(k) == v for k, v in want["sleep"].items())
        try:
            cur = await asyncio.wait_for(asyncio.to_thread(self.ops.get_config), timeout=15)     # fresh
        except Exception as e:
            self._log(cmd, value, "failed", f"config read: {e}", client)
            return {"ok": False, "detail": "dish not reachable; nothing sent"}
        if matches(cur):
            self._log(cmd, value, "noop", "already set", client)
            return {"ok": True, "detail": "already set"}
        send_err = None
        try:
            await asyncio.wait_for(asyncio.to_thread(self.ops.set_config, **fields), timeout=15)   # once
        except Exception as e:
            send_err = e                         # it may still have applied: verify, never resend
        deadline = time.monotonic() + verify_timeout
        while time.monotonic() < deadline:
            await asyncio.sleep(poll)
            try:
                now = await asyncio.wait_for(asyncio.to_thread(self.ops.get_config), timeout=8)
            except Exception:
                continue
            if matches(now):
                self.state.update({"starlink.heating": now["heating"], "starlink.sleep": now["sleep"]})
                self._log(cmd, value, "ok", f"{cur} → {now}" + (f" (send raised {send_err})" if send_err else ""), client)
                return {"ok": True, "detail": "heating " + now["heating"] if cmd == "set_heating"
                        else ("sleep schedule on" if now["sleep"]["enabled"] else "sleep schedule off")}
        self._log(cmd, value, "unverified", f"send={'error: ' + str(send_err) if send_err else 'ok'}; not seen", client)
        return {"ok": False, "detail": "the dish didn't confirm the change (not retried)"}

    def _poll(self) -> dict:
        import starlink_grpc  # imported lazily so the backend still runs if grpc is missing
        if self._ctx is None:
            self._ctx = starlink_grpc.ChannelContext(target=self.target)
        st, obs, alerts, *_ = starlink_grpc.status_data(context=self._ctx)
        out = normalize(st, obs, alerts)
        try:                                  # signal quality isn't in status_data(): read the raw status
            out.update(signal_fields(starlink_grpc.get_status(context=self._ctx)))
        except Exception as e:
            log.debug("starlink signal quality unavailable: %s", e)
        if time.monotonic() - self._map_at >= self.MAP_EVERY:
            self._map_at = time.monotonic()
            try:
                m = starlink_grpc.get_obstruction_map(context=self._ctx)
                out["starlink.obstruction_map"] = encode_obstruction_map(
                    m.num_rows, m.num_cols, list(m.snr), m.min_elevation_deg, m.max_theta_deg, m.map_reference_frame)
            except Exception as e:  # older firmware / reflection mismatch: the rest of the card still works
                log.debug("starlink obstruction map failed: %s", e)
            try:                                  # power (graph) + events/outages for Settings → Starlink
                out.update(history_fields(starlink_grpc.get_history(context=self._ctx)))
            except Exception as e:
                log.debug("starlink history read failed: %s", e)
            try:                                  # heating + sleep schedule shown in Settings → Starlink
                cfg = self.ops.get_config()
                out["starlink.heating"], out["starlink.sleep"] = cfg["heating"], cfg["sleep"]
            except Exception as e:
                log.debug("starlink config read failed: %s", e)
        return out

    async def run(self) -> None:
        backoff = self.interval
        while True:
            try:
                data = await asyncio.wait_for(asyncio.to_thread(self._poll), timeout=15)
                data["starlink.updated"] = round(time.time(), 1)
                self.state.update(data)
                backoff = self.interval
            except asyncio.CancelledError:
                raise
            except Exception as e:  # GrpcError, timeouts, reflection mismatches after firmware updates
                log.debug("starlink poll failed: %s", e)
                self.state.update({"starlink.reachable": False, "starlink.online": False})
                if self._ctx is not None:
                    self._ctx.close()
                    self._ctx = None
                backoff = min(backoff * 2, 60.0)
            await asyncio.sleep(backoff)
