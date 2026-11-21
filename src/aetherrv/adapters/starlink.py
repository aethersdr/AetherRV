"""Starlink dish adapter (local gRPC at 192.168.100.1:9200 via starlink-grpc-core). READ-ONLY.

Uses `starlink_grpc.status_data()`, which resolves the dish protobufs by server reflection
(nothing to compile). The library is synchronous, so each poll runs in a worker thread.
Control calls (reboot, stow, sleep, GPS config) exist in the library and are deliberately
NOT used.

Not available: location (removed from gRPC for most plans in May 2026). Use the Peplink GPS.
Behind the Peplink: firmware 8.4.0+ reportedly routes 192.168.100.1 automatically; older
firmware needs a WAN "Management IP" (e.g. 192.168.100.100/24) on the Starlink WAN.
"""

from __future__ import annotations

import asyncio
import logging
import time

from ..state import State

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


def _r(v, n):
    return None if v is None else round(float(v), n)


class StarlinkAdapter:
    device = "starlink"

    def __init__(self, state: State, target: str = "192.168.100.1:9200", interval: float = 5.0) -> None:
        self.state = state
        self.target = target
        self.interval = interval
        self._ctx = None

    def _poll(self) -> dict:
        import starlink_grpc  # imported lazily so the backend still runs if grpc is missing
        if self._ctx is None:
            self._ctx = starlink_grpc.ChannelContext(target=self.target)
        st, obs, alerts, *_ = starlink_grpc.status_data(context=self._ctx)
        return normalize(st, obs, alerts)

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
