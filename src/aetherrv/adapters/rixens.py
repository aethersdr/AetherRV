"""Rixens MCS7 hydronic heat adapter. READ-ONLY.

Frame map source: the community project https://github.com/mikegoubeaux/rixens-esphome
(MCS7.yaml, Feb 2024). It has no license, so no code is copied; only the documented
frame meanings are reimplemented here. It is ONE community source and is UNVERIFIED on this
van's MCS7 and firmware. Rixens reportedly shares the official DBC on request, which would
supersede this.

Bus: 250 kbit/s, 11-bit identifiers. MCS7 board 4-pin CAN port (pins unlabeled; see the MCS7
Installation Manual p.10; CAN-H idles ~2.5-3.75 V, CAN-L ~1.25-2.5 V).

  0x724  byte 6 bit flags: 0x08 furnace, 0x10 electric, 0x20 engine, 0x80 preheat (selected heat sources)
  0x725  byte 3 ×0.1 = burner fuel pump Hz; bytes 4-5 LE = burner fan RPM; bytes 6-7 LE ×0.1 = glow plug W
  0x726  bytes 2-3 LE ×0.01 = glycol outlet °C; bytes 6-7 LE ×0.1 = supply V

Commands TO the MCS7 (ID 0x788, byte 0 = property) are encoded below as pure functions for
later use, but NOTHING SENDS THEM. Sending requires TxGate approval (tx_enabled) AND lifting
ListenOnly on that bus, both after verification on this van.
"""

from __future__ import annotations

import asyncio
import time

import can

from ..state import State
from .capture_only import CaptureOnlyAdapter

ID_SOURCES, ID_DIAG, ID_TEMPS = 0x724, 0x725, 0x726
ID_COMMAND = 0x788

SOURCE_BITS = {"furnace": 0x08, "electric": 0x10, "engine": 0x20, "preheat": 0x80}


def _u16(b: bytes, i: int) -> int:
    return int.from_bytes(b[i:i + 2], "little")


def _s16(b: bytes, i: int) -> int:
    return int.from_bytes(b[i:i + 2], "little", signed=True)


def decode(arbitration_id: int, data: bytes) -> dict | None:
    """Decode one MCS7 frame into {field: value}, or None if the ID isn't mapped."""
    if len(data) < 8:
        return None
    if arbitration_id == ID_SOURCES:
        return {f"source.{k}": bool(data[6] & bit) for k, bit in SOURCE_BITS.items()}
    if arbitration_id == ID_DIAG:
        return {"fuel_hz": round(data[3] * 0.1, 1), "fan_rpm": _u16(data, 4), "glow_w": round(_u16(data, 6) * 0.1, 1)}
    if arbitration_id == ID_TEMPS:
        # Signed on purpose: a cold-soaked loop can be below 0 °C. The source used unsigned.
        return {"glycol_c": round(_s16(data, 2) * 0.01, 2), "voltage": round(_u16(data, 6) * 0.1, 1)}
    return None


# ── Command encoders (NOT SENT; for the gated control phase) ─────────────────
def _cmd(prop: int, value: int = 0, signed: bool = False) -> bytes:
    return bytes([prop]) + int(value).to_bytes(2, "little", signed=signed) + bytes(5)


def cmd_setpoint(celsius: float) -> bytes:
    return _cmd(0x01, round(celsius * 10), signed=True)


def cmd_fan(percent: int | None) -> bytes:
    """percent 0-100, or None for AUTO (0xFF)."""
    return _cmd(0x02, 0xFF if percent is None else max(0, min(100, int(percent))))


def cmd_source(name: str, on: bool) -> bytes:
    prop = {"furnace": 0x03, "electric": 0x04, "engine": 0x05, "hot_water": 0x06, "preheat": 0x07}[name]
    return _cmd(prop, 1 if on else 0)


def cmd_ambient(celsius: float) -> bytes:
    return _cmd(0x08, round(celsius * 10), signed=True)


def cmd_engine_running(on: bool) -> bytes:
    return _cmd(0x0B, 1 if on else 0)


# ── Adapter ──────────────────────────────────────────────────────────────────
class RixensAdapter(CaptureOnlyAdapter):
    """Decodes the known MCS7 frames into climate.rixens.* and keeps the raw ID inventory too."""

    STALE_AFTER = 10.0

    def __init__(self, device: str, state: State, publish_climate: bool = True) -> None:
        super().__init__(device, state)
        self.publish_climate = publish_climate
        self.values: dict[str, object] = {}
        self.last_decoded: float | None = None

    def on_frame(self, msg: can.Message) -> None:
        super().on_frame(msg)
        if msg.is_error_frame or msg.is_remote_frame or msg.is_extended_id:
            return
        d = decode(msg.arbitration_id, bytes(msg.data))
        if d:
            self.values.update(d)
            self.last_decoded = time.time()

    def derived(self, now: float | None = None) -> dict:
        now = time.time() if now is None else now
        if self.last_decoded is None or now - self.last_decoded > self.STALE_AFTER:
            return {"climate.rixens.state": None if self.last_decoded is None else "no data"}
        v = self.values
        burning = (v.get("fuel_hz") or 0) > 0
        sources = [k for k in SOURCE_BITS if v.get(f"source.{k}")]
        out = {f"climate.rixens.{k.replace('.', '_')}": val for k, val in v.items()}
        out.update({
            "climate.rixens.coolant": v.get("glycol_c"),
            "climate.rixens.burner": burning,
            "climate.rixens.sources": sources,
            # "heating" only when the burner is actually firing; selected sources alone = standby.
            "climate.rixens.state": "heating" if burning else ("standby" if sources else "off"),
            "climate.rixens.live": True,
        })
        return out

    async def run(self) -> None:
        while True:
            self.state.set(f"{self.device}.ids", self.summary())
            if self.publish_climate and self.last_decoded is not None:
                self.state.update(self.derived())
            await asyncio.sleep(1.0)
