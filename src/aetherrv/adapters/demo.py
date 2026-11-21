"""DEMO data source for bench UI work. Synthetic values only.

It writes plausible, slowly varying values to the same state keys the real Victron adapter
uses, plus climate/tank/switch keys that future decoders will own. When enabled it sets
`demo.active = true`, and the UI shows a DEMO badge on every screen. Never enable it in the van.
"""

from __future__ import annotations

import asyncio
import math
import random
import time

from ..state import State


class DemoAdapter:
    device = "demo"

    def __init__(self, state: State, history=None, exclude: list[str] | None = None) -> None:
        self.state = state
        self.exclude = tuple(exclude or ())
        self.history = history
        self.soc = 78.0

    def _solar(self, hour: float) -> float:
        # bell curve peaking ~1 pm, 400 W array, some cloud noise
        x = (hour - 13.0) / 3.2
        return max(0.0, 400 * math.exp(-x * x) * random.uniform(0.85, 1.0))

    def _frame(self, t: float, dt: float = 1.0) -> dict:
        lt = time.localtime(t)
        hour = lt.tm_hour + lt.tm_min / 60 + lt.tm_sec / 3600
        pv = self._solar(hour)
        fridge = 45 if (t % 2400) < 900 else 0          # compressor duty cycle
        lights = 30 if (hour >= 17 or hour < 6.5) and hour < 23 else 0
        dc_load = 22 + fridge + lights + random.uniform(-2, 2)
        # inverter loads: morning coffee 7:00-7:20, evening TV 19:00-21:30
        ac_load = 850.0 if 7.0 <= hour < 7.33 else 110.0 if 19.0 <= hour < 21.5 else 0.0
        ac_load += random.uniform(-5, 5) if ac_load else 0.0
        alt = 0.0
        shore = 0.0
        batt_power = pv + alt + shore - dc_load - ac_load * 1.1
        self.soc = min(100.0, max(5.0, self.soc + batt_power * dt / 3600 / (12.8 * 460) * 100))
        volt = 13.05 + (self.soc - 50) * 0.006 + batt_power * 0.0004
        cabin = 19.5 + 1.2 * math.sin(t / 900)
        outside = 2.0 + 6 * math.exp(-((hour - 14) / 4) ** 2)
        heater_on = cabin < 19.8
        return {
            "demo.active": True,
            "victron.connected": True,
            "victron.portal": "demo",
            "victron.system.state": 9 if batt_power < 0 else 3,
            "victron.battery.soc": round(self.soc, 1),
            "victron.battery.voltage": round(volt, 2),
            "victron.battery.current": round(batt_power / volt, 1),
            "victron.battery.power": round(batt_power),
            "victron.battery.temp": round(14 + 0.3 * math.sin(t / 1300), 1),
            "victron.battery.ttg": None if batt_power >= 0 else int(self.soc / 100 * 460 * 12.8 / -batt_power * 3600),
            "victron.pv.power": round(pv),
            "victron.dc.load": round(dc_load),
            "victron.ac.load": round(ac_load),
            "victron.ac.shore": round(shore),
            "victron.alternator.power": round(alt),
            "victron.tanks": [
                {"name": "Fresh", "type": "fresh", "level": round(64 - (t % 86400) / 86400 * 10, 1)},
                {"name": "Grey", "type": "grey", "level": round(31 + (t % 86400) / 86400 * 8, 1)},
            ],
            "starlink.reachable": True, "starlink.online": True, "starlink.state": "CONNECTED",
            "starlink.down_mbps": round(max(5.0, 95 + 60 * math.sin(t / 300) + random.uniform(-15, 15)), 1),
            "starlink.up_mbps": round(max(1.0, 12 + 5 * math.sin(t / 240) + random.uniform(-2, 2)), 1),
            "starlink.latency_ms": round(32 + 8 * math.sin(t / 200) + random.uniform(-3, 3), 1),
            "starlink.drop_rate": 0.002, "starlink.obstructed_pct": 1.7, "starlink.currently_obstructed": False,
            "starlink.gps_sats": 14, "starlink.software": "2026.11.10.mr51000", "starlink.uptime": int(t % 864000),
            "starlink.alerts": [{"id": "alert_is_heating", "label": "Is heating", "severity": None}] if outside < 3 else [],
            "peplink.reachable": True, "peplink.active": "Starlink", "peplink.active_type": "starlink",
            "peplink.wans": [
                {"id": 1, "name": "Starlink", "type": "ethernet", "message": "Connected", "led": "green", "enabled": True,
                 "priority": 1, "uptime": int(t % 864000), "connected": True, "starlink": True},
                {"id": 2, "name": "Cellular", "type": "cellular", "message": "Connected", "led": "green", "enabled": True,
                 "priority": 2, "uptime": int(t % 400000), "connected": True, "starlink": False,
                 "cellular": {"carrier": "Verizon", "network": "5G", "signal_level": 4, "roaming": "home", "ca": True,
                              "sim": 1, "rat": "5G NR", "band": "n77", "rsrp": round(-96 + 4 * math.sin(t / 120)),
                              "rsrq": -10, "sinr": round(13 + 3 * math.sin(t / 90), 1), "rssi": None, "bands": 2}},
                {"id": 3, "name": "Campground Wi-Fi", "type": "wireless", "message": "Disabled", "led": "gray",
                 "enabled": False, "priority": None, "uptime": None, "connected": False, "starlink": False}],
            "peplink.allowance": [{"conn": 2, "sim": 1, "usage_mb": 31200, "limit_mb": 50000, "percent": 62, "start_day": 1}],
            "location.fix": True, "location.source": "peplink", "location.lat": 45.3735, "location.lon": -121.6959,
            "location.alt_m": 1830.0, "location.speed": 0.0, "location.heading": 0.0, "location.hdop": 0.9,
            "climate.cabin.temp": round(cabin, 1),
            "climate.cabin.setpoint": 20.0,
            "climate.outside.temp": round(outside, 1),
            "climate.cabin.humidity": round(42 + 4 * math.sin(t / 2000), 0),
            "climate.fridge.temp": round(3.2 + 1.3 * math.sin(t / 700), 1),
            "climate.freezer.temp": round(-17.5 + 1.5 * math.sin(t / 900), 1),
            "climate.rixens.state": "heating" if heater_on else "idle",
            "climate.rixens.fan": 2 if heater_on else 0,
            "climate.rixens.coolant": round(62 if heater_on else 38, 0),
            "climate.espar.state": "off",
            "switches.list": [
                {"id": "1.1", "name": "Cabin lights", "group": "Lights", "icon": "light", "on": True},
                {"id": "1.2", "name": "Galley lights", "group": "Lights", "icon": "light", "on": False},
                {"id": "1.3", "name": "Awning light", "group": "Lights", "icon": "light", "on": False},
                {"id": "1.4", "name": "Reading lights", "group": "Lights", "icon": "light", "on": True},
                {"id": "2.1", "name": "Water pump", "group": "Systems", "icon": "pump", "on": True},
                {"id": "2.2", "name": "Fridge", "group": "Systems", "icon": "fridge", "on": True},
                {"id": "2.3", "name": "Roof fan", "group": "Systems", "icon": "fan", "on": False},
                {"id": "2.4", "name": "Inverter", "group": "Power", "icon": "inverter", "on": False},
                {"id": "3.1", "name": "USB / 12V", "group": "Power", "icon": "plug", "on": True},
                {"id": "3.2", "name": "Starlink", "group": "Power", "icon": "dish", "on": True},
            ],
        }

    def _apply(self, frame: dict) -> None:
        # Real data wins: drop demo keys for anything a live adapter now owns.
        owned = []
        if self.state.get("switches.source") == "n2k":
            owned.append("switches.")
        if self.state.get("climate.rixens.live"):
            owned.append("climate.rixens.")
        owned_t = tuple(owned) + self.exclude
        if owned_t:
            frame = {k: v for k, v in frame.items() if not k.startswith(owned_t)}
        self.state.update(frame)

    async def run(self) -> None:
        if self.history is not None:  # backfill 7 days (in-memory DB) so trend charts render immediately
            now = time.time()
            self.soc = 70.0
            for i in range(int(7 * 86400 / self.history.interval), 0, -1):
                ts = now - i * self.history.interval
                self._apply(self._frame(ts, self.history.interval))
                self.history.sample(ts)
        while True:
            self._apply(self._frame(time.time()))
            await asyncio.sleep(1.0)
