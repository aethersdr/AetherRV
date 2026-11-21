"""Derived energy figures for the UI: fill in what the Cerbo can't see.

The Cerbo only knows an alternator if it appears as a DC source it recognises. A Wakespeed WS500 on
NMEA 2000 is not one, so the Cerbo reports no alternator power and books the charging current as
NEGATIVE DC load (its Dc/System/Power = known sources − battery power). When that happens and the
N2K adapter has alternator readings, use them for the alternator, and take the DC load as the gap
between the battery shunt and the regulator's current (owner's model, 2026-11-21):
    DC load = (battery current − regulator current) × battery voltage
"""
from __future__ import annotations

import asyncio

from .state import State


def derive(state: State) -> None:
    cerbo_alt = state.get("victron.alternator.power")
    n2k_alt = state.get("alternator.power")
    dc = state.get("victron.dc.load")
    if cerbo_alt is not None:
        alt, src = cerbo_alt, "cerbo"
    elif n2k_alt is not None:
        alt, src = n2k_alt, "n2k"
        i_batt, v_batt, i_alt = (state.get("victron.battery.current"), state.get("victron.battery.voltage"),
                                 state.get("alternator.current"))
        if None in (i_batt, v_batt, i_alt):
            dc = None                          # the Cerbo's figure is wrong while charging; don't show it
        else:
            dc = max(0.0, (i_batt - i_alt) * v_batt)
    else:
        alt, src = None, None
    state.set("energy.alternator.power", alt)
    state.set("energy.alternator.source", src)
    state.set("energy.dc.load", None if dc is None else round(dc, 1))


async def run_derive(state: State, period: float = 1.0) -> None:
    while True:
        derive(state)
        await asyncio.sleep(period)
