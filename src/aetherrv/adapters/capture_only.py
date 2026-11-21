"""Capture-only adapter for buses whose protocol is not yet verified (Espar S2 D2L, Rixens MCS7).

It decodes nothing and sends nothing. It publishes which arbitration IDs are present, how
often each one appears and its latest payload, which is the raw material for reverse
engineering. Real decoders replace this once a frame map has been verified against
captures from *this* van.
"""

from __future__ import annotations

import asyncio
import time

import can

from ..state import State


class CaptureOnlyAdapter:
    def __init__(self, device: str, state: State) -> None:
        self.device = device
        self.state = state
        self._ids: dict[int, dict] = {}

    def on_frame(self, msg: can.Message) -> None:
        if msg.is_error_frame or msg.is_remote_frame:
            return
        e = self._ids.setdefault(msg.arbitration_id, {"count": 0, "ext": msg.is_extended_id})
        e["count"] += 1
        e["data"] = bytes(msg.data).hex(" ")
        e["seen"] = time.time()

    def summary(self) -> list[dict]:
        rows = []
        for aid, e in sorted(self._ids.items()):
            width = 8 if e["ext"] else 3
            rows.append({"id": f"{aid:0{width}X}", "count": e["count"], "data": e["data"],
                         "age": round(time.time() - e["seen"], 1)})
        return rows

    async def run(self) -> None:
        while True:
            self.state.set(f"{self.device}.ids", self.summary())
            await asyncio.sleep(1.0)
