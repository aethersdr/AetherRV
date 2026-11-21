"""Starlink dish adapter (local gRPC at 192.168.100.1:9200 via starlink-grpc-core).

Uses `starlink_grpc.status_data()`, which resolves the dish protobufs by server reflection
(nothing to compile). The library is synchronous, so each poll runs in a worker thread.
The obstruction map is read once a minute (get_obstruction_map).

ONE control exists: `install_update`, which reboots the dish ONLY while it reports a downloaded
update waiting for a reboot (software_update_state == REBOOT_REQUIRED). It goes through TxGate
(device "starlink", tx_enabled), reads fresh status first, sends exactly once on a brand-new gRPC
channel (the library retries once on a *reused* channel, so the read and the send never share
one), never retries, and verifies the reboot by the dish going away or its uptime resetting.
Every other control call in the library (plain reboot, stow, sleep, GPS config, reset
obstruction map) is deliberately NOT used.

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


class StarlinkAdapter:
    device = "starlink"
    COMMANDS = ("install_update",)

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
        """install_update: reboot the dish to install a downloaded update. Fresh read → send once → verify."""
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
            try:
                st = await asyncio.wait_for(asyncio.to_thread(self.ops.status), timeout=15)   # fresh, never the poll cache
            except Exception as e:
                self._log(cmd, value, "failed", f"status read: {e}", client)
                return {"ok": False, "detail": "dish not reachable; nothing sent"}
            upd = st.get("software_update_state")
            if upd != "REBOOT_REQUIRED":
                self._log(cmd, value, "rejected", f"software_update_state={upd}", client)
                return {"ok": False, "detail": "no update is waiting to install"}
            before, send_err = st.get("uptime"), None
            try:
                await asyncio.wait_for(asyncio.to_thread(self.ops.reboot), timeout=20)          # exactly once
            except Exception as e:
                # The dish may have received it (e.g. a deadline expiring as it goes down): verify, never resend.
                send_err = e
                log.info("starlink reboot call raised %s; verifying instead of retrying", e)
            self.state.set("starlink.restart_requested", round(time.time(), 1))
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
                    return {"ok": True, "detail": "the dish is restarting to install the update (offline a few minutes)"}
                up = now.get("uptime")
                if before is not None and up is not None and up < before:
                    self._log(cmd, value, "ok", f"uptime reset {before}s → {up}s", client)
                    return {"ok": True, "detail": "the dish restarted and is installing the update"}
            self._log(cmd, value, "unverified", f"no restart seen within {verify_timeout:.0f}s", client)
            return {"ok": False, "detail": "the dish did not restart (not retried)"}

    def _poll(self) -> dict:
        import starlink_grpc  # imported lazily so the backend still runs if grpc is missing
        if self._ctx is None:
            self._ctx = starlink_grpc.ChannelContext(target=self.target)
        st, obs, alerts, *_ = starlink_grpc.status_data(context=self._ctx)
        out = normalize(st, obs, alerts)
        if time.monotonic() - self._map_at >= self.MAP_EVERY:
            self._map_at = time.monotonic()
            try:
                m = starlink_grpc.get_obstruction_map(context=self._ctx)
                out["starlink.obstruction_map"] = encode_obstruction_map(
                    m.num_rows, m.num_cols, list(m.snr), m.min_elevation_deg, m.max_theta_deg, m.map_reference_frame)
            except Exception as e:  # older firmware / reflection mismatch: the rest of the card still works
                log.debug("starlink obstruction map failed: %s", e)
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
