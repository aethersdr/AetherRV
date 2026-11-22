"""EPOCH lithium batteries over Bluetooth LE (RoyPow BMS protocol). READ-ONLY.

The van's four EPOCH 12 V 460 Ah batteries advertise as " B12460A  2402000xxx " and carry a RoyPow
"smart BMS". Its BLE UART is GATT service ffe0, characteristic ffe1 (write + notify). Each read is a
query frame and a framed reply:

    query  EA D1 01 <len=04> FF <cmd> <xor of len..cmd> F5
    reply  EA D1 01 <len> FF <cmd> <payload…> <xor> F5     (split across several notifications)

  cmd 0x04: SoC, pack voltage, remaining Ah, cycles, runtime
  cmd 0x03: current (sign bit), problem code, temperatures, charge/discharge MOSFETs
  cmd 0x02: cell voltages

Field positions come from aiobmsble's RoyPow plugin (Apache-2.0); its decoder is reused. Its own
connection code is not, because these modules reject the standard Battery Level subscription and
often fail the first ffe1 subscription, after which they drop the link. So each read is:
connect → short settle → subscribe (on failure: disconnect, wait, reconnect, up to 3 tries) →
3 queries → disconnect. The batteries are read one at a time, about once a minute.

Only the three read queries are ever written; nothing that changes a setting.
"""

from __future__ import annotations

import asyncio
import logging
import time

from ..state import State

log = logging.getLogger(__name__)

UART = "0000ffe1-0000-1000-8000-00805f9b34fb"
HEAD, TAIL = b"\xea\xd1\x01", 0xF5
CMDS = (0x04, 0x03, 0x02)


def query(cmd: int) -> bytes:
    data = bytes([0x04, 0xFF, cmd])
    x = 0
    for b in data:
        x ^= b
    return HEAD + data + bytes([x, TAIL])


class FrameAssembler:
    """Collect notifications into complete, checked RoyPow frames keyed by command byte."""

    def __init__(self) -> None:
        self.buf = bytearray()
        self.msgs: dict[int, bytes] = {}

    def feed(self, data: bytes) -> None:
        data = bytes(data).removeprefix(b"AT+STAT\r\n")
        if data.startswith(HEAD):
            self.buf = bytearray()
        self.buf += data
        if not self.buf.startswith(HEAD) or len(self.buf) < 4:
            if not self.buf.startswith(HEAD[: len(self.buf)]):
                self.buf.clear()
            return
        total = 4 + self.buf[3]                         # head(3) + len byte + len
        if len(self.buf) < total:
            return
        frame, self.buf = bytes(self.buf[:total]), bytearray()
        x = 0
        for b in frame[3:-2]:
            x ^= b
        if frame[-1] != TAIL or frame[-2] != x:
            log.debug("epoch: bad frame %s", frame.hex())
            return
        self.msgs[frame[5]] = frame


def decode(msgs: dict[int, bytes]) -> dict:
    """RoyPow frames → AetherRV battery dict (field map from aiobmsble)."""
    from aiobmsble.bms.roypow_bms import BMS
    r = BMS._decode_data(BMS._FIELDS, msgs)
    cells = BMS._cell_voltages(msgs.get(0x2, b""), cells=max(0, (len(msgs.get(0x2, b"")) - 11) // 2), start=9) \
        if 0x2 in msgs else []
    temps = [t[0] if isinstance(t, tuple) else float(t) for t in
             BMS._temp_values(msgs.get(0x3, b""), values=r.get("temp_sensors", 0), start=14, size=1,
                              signed=False, offset=40)] if 0x3 in msgs else []
    runtime = r.get("runtime")
    return {
        "soc": r.get("battery_level"), "voltage": r.get("voltage"), "current": r.get("current"),
        "remaining_ah": None if r.get("cycle_charge") is None else round(r["cycle_charge"], 1),
        "cycles": r.get("cycles"), "problem_code": r.get("problem_code"),
        "charge_fet": r.get("chrg_mosfet"), "discharge_fet": r.get("dischrg_mosfet"),
        "runtime_h": None if runtime in (None, 0xFFFF * 60) else round(runtime / 3600, 1),
        "cells": [round(c, 3) for c in cells],
        "cell_delta_mv": round((max(cells) - min(cells)) * 1000) if cells else None,
        "temps": temps,
    }


_DEVICES: dict = {}          # address → BLEDevice from the last scan (rescanned only after a failure)


async def read_battery(address: str, settle: float = 0.8, tries: int = 3, timeout: float = 20.0) -> dict:
    """One full read of one battery. Raises on failure."""
    from bleak import BleakClient, BleakScanner
    dev = _DEVICES.get(address) or await BleakScanner.find_device_by_address(address, timeout=12)
    if dev is None:
        raise ConnectionError("not advertising")
    _DEVICES[address] = dev
    last: Exception | None = None
    for attempt in range(tries):
        fa = FrameAssembler()
        try:
            async with BleakClient(dev, timeout=timeout) as c:
                await asyncio.sleep(settle)
                await c.start_notify(UART, lambda _ch, data: fa.feed(data))
                for cmd in CMDS:
                    await c.write_gatt_char(UART, query(cmd), response=False)
                    end = time.monotonic() + 3.0
                    while cmd not in fa.msgs and time.monotonic() < end:
                        await asyncio.sleep(0.05)
                if not all(cmd in fa.msgs for cmd in CMDS):
                    raise TimeoutError(f"incomplete reply {sorted(fa.msgs)}")
                return decode(fa.msgs)
        except Exception as e:                        # flaky module: drop the link and start over
            last = e
            log.debug("epoch %s attempt %d: %s", address, attempt + 1, e)
            await asyncio.sleep(2.0)
    _DEVICES.pop(address, None)                       # rescan next time
    raise last or ConnectionError("failed")


class EpochBleAdapter:
    device = "epoch_ble"
    STALE = 600.0        # s without a good read → that battery counts as not reporting

    def __init__(self, state: State, batteries: list[dict], interval: float = 60.0) -> None:
        self.state = state
        self.batteries = [{"name": str(b.get("name") or b["address"]), "address": str(b["address"]),
                           "serial": str(b.get("serial", ""))} for b in batteries]
        self.interval = interval
        self.last: dict[str, dict] = {}

    def publish(self, now: float | None = None) -> None:
        now = time.time() if now is None else now
        rows = []
        for b in self.batteries:
            d = self.last.get(b["address"], {})
            ok = bool(d.get("t")) and now - d["t"] <= self.STALE
            rows.append({**b, **(d.get("data") or {}), "ok": ok, "age": round(now - d["t"], 0) if d.get("t") else None,
                         "error": d.get("error")})
        good = [r for r in rows if r["ok"]]
        self.state.update({
            "batteries.ble": rows,
            "batteries.ble.reporting": len(good),
            "batteries.ble.expected": len(rows),
            "batteries.ble.current": round(sum(r["current"] for r in good if r.get("current") is not None), 2) if good else None,
        })

    async def run(self) -> None:
        while True:
            start = time.monotonic()
            for b in self.batteries:
                try:
                    data = await asyncio.wait_for(read_battery(b["address"]), timeout=90)
                    self.last[b["address"]] = {"t": time.time(), "data": data}
                except Exception as e:
                    prev = self.last.get(b["address"], {})
                    self.last[b["address"]] = {**prev, "error": f"{type(e).__name__}: {e}"[:120]}
                    log.info("epoch %s: read failed: %s", b["name"], e)
                self.publish()
            await asyncio.sleep(max(5.0, self.interval - (time.monotonic() - start)))


if __name__ == "__main__":                            # one-off check: python -m aetherrv.adapters.epoch_ble ADDR…
    import json
    import sys

    async def _main() -> None:
        for a in sys.argv[1:]:
            try:
                print(a, json.dumps(await read_battery(a)))
            except Exception as e:
                print(a, "FAILED", repr(e))
    logging.basicConfig(level=logging.INFO)
    asyncio.run(_main())
