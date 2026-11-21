"""Cabin temperature as the average of two sensors (config `[climate] cabin_source = "average"`).

The van has a Ruuvi at the front (via the Cerbo, published as climate.cabin_front.*) and the MCS7's
own sensor at the back by the bed (climate.rixens.cabin_*). Neither alone represents the cabin, so
the owner chose their mean (2026-11-21). If one sensor is missing or stale, the other is used alone.
The heater itself still regulates on the MCS7 sensor; this only changes what the console shows.
"""
from __future__ import annotations

import asyncio
import time

from .state import State

FRONT_STALE = 15 * 60   # s. The Cerbo republishes a Ruuvi only when the value changes, so allow quiet spells.
REAR_STALE = 60         # s since the last successful MCS7 poll (climate.rixens.updated)


def _mean(vals):
    vals = [v for v in vals if v is not None]
    return round(sum(vals) / len(vals), 2) if vals else None


def derive_cabin(state: State, now: float | None = None) -> None:
    now = time.time() if now is None else now
    age = state.age("climate.cabin_front.temp")
    front_ok = (state.get("climate.cabin_front.temp") is not None and state.get("victron.connected") is not False
                and age is not None and age < FRONT_STALE)
    upd = state.get("climate.rixens.updated")
    rear_ok = (state.get("climate.rixens.cabin_c") is not None and state.get("climate.rixens.live") is not False
               and upd is not None and now - upd < REAR_STALE)
    temps, hums, used = [], [], []
    if front_ok:
        temps.append(state.get("climate.cabin_front.temp")); hums.append(state.get("climate.cabin_front.humidity"))
        used.append("front")
    if rear_ok:
        temps.append(state.get("climate.rixens.cabin_c")); hums.append(state.get("climate.rixens.cabin_rh"))
        used.append("rear")
    state.update({
        "climate.cabin.temp": _mean(temps),
        "climate.cabin.humidity": _mean(hums),
        "climate.cabin.source": "average" if len(used) == 2 else (used[0] if used else None),
        "climate.cabin.front_temp": state.get("climate.cabin_front.temp") if front_ok else None,
        "climate.cabin.rear_temp": state.get("climate.rixens.cabin_c") if rear_ok else None,
    })


async def run_cabin_average(state: State, period: float = 2.0) -> None:
    while True:
        derive_cabin(state)
        await asyncio.sleep(period)
