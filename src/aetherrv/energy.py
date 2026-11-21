"""Derived energy figures for the UI: fill in what the Cerbo can't see.

The Cerbo only knows an alternator if it appears as a DC source it recognises. A Wakespeed WS500 on
NMEA 2000 is not one, so the Cerbo reports no alternator power and books the charging current as
NEGATIVE DC load (its Dc/System/Power = known sources − battery power). When that happens and the
N2K adapter has alternator readings, use them and add the alternator back into the DC load.
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
        if dc is not None:
            dc = max(0.0, dc + n2k_alt)       # the Cerbo left the alternator out of its balance
    else:
        alt, src = None, None
    state.set("energy.alternator.power", alt)
    state.set("energy.alternator.source", src)
    state.set("energy.dc.load", None if dc is None else round(dc, 1))


async def run_derive(state: State, period: float = 1.0) -> None:
    while True:
        derive(state)
        await asyncio.sleep(period)
