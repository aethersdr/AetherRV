import time

import can

from aetherrv.adapters import rixens as rx
from aetherrv.state import State


def frame(i, data):
    return can.Message(arbitration_id=i, data=bytes(data), is_extended_id=False)


def test_decode_temps_vector():
    # glycol 0x1F40 = 8000 × 0.01 = 80.00 °C ; voltage 0x0084 = 132 × 0.1 = 13.2 V
    d = rx.decode(0x726, bytes([0, 0, 0x40, 0x1F, 0, 0, 0x84, 0x00]))
    assert d == {"glycol_c": 80.0, "voltage": 13.2}


def test_decode_negative_glycol_is_signed():
    d = rx.decode(0x726, bytes([0, 0, 0x0C, 0xFE, 0, 0, 0x7E, 0x00]))  # 0xFE0C = -500 → -5.00 °C
    assert d["glycol_c"] == -5.0 and d["voltage"] == 12.6


def test_decode_diag_vector():
    # fuel 23 × 0.1 = 2.3 Hz ; fan 0x0BB8 = 3000 rpm ; glow 0x01F4 = 500 × 0.1 = 50.0 W
    d = rx.decode(0x725, bytes([0, 0, 0, 23, 0xB8, 0x0B, 0xF4, 0x01]))
    assert d == {"fuel_hz": 2.3, "fan_rpm": 3000, "glow_w": 50.0}


def test_decode_sources_bits():
    d = rx.decode(0x724, bytes([0, 0, 0, 0, 0, 0, 0x08 | 0x80, 0]))
    assert d == {"source.furnace": True, "source.electric": False, "source.engine": False, "source.preheat": True}


def test_decode_ignores_unknown_and_short():
    assert rx.decode(0x123, bytes(8)) is None
    assert rx.decode(0x726, bytes(4)) is None


def test_command_encoders_match_documented_bytes():
    assert rx.cmd_setpoint(20.0) == bytes([0x01, 0xC8, 0x00, 0, 0, 0, 0, 0])      # 200
    assert rx.cmd_ambient(-2.5) == bytes([0x08, 0xE7, 0xFF, 0, 0, 0, 0, 0])       # -25
    assert rx.cmd_fan(None) == bytes([0x02, 0xFF, 0, 0, 0, 0, 0, 0])
    assert rx.cmd_fan(150)[1] == 100
    assert rx.cmd_source("furnace", True) == bytes([0x03, 0x01, 0, 0, 0, 0, 0, 0])
    assert rx.cmd_source("hot_water", False) == bytes([0x06, 0, 0, 0, 0, 0, 0, 0])
    assert rx.cmd_engine_running(True)[:2] == bytes([0x0B, 0x01])


def test_adapter_state_and_staleness():
    a = rx.RixensAdapter("rixens", State())
    assert a.derived()["climate.rixens.state"] is None              # never heard from it
    a.on_frame(frame(0x724, [0, 0, 0, 0, 0, 0, 0x08, 0]))
    a.on_frame(frame(0x726, [0, 0, 0x40, 0x1F, 0, 0, 0x84, 0]))
    a.on_frame(frame(0x725, [0, 0, 0, 0, 0, 0, 0, 0]))
    d = a.derived()
    assert d["climate.rixens.state"] == "standby" and d["climate.rixens.coolant"] == 80.0
    assert d["climate.rixens.sources"] == ["furnace"] and d["climate.rixens.burner"] is False
    a.on_frame(frame(0x725, [0, 0, 0, 23, 0xB8, 0x0B, 0, 0]))
    assert a.derived()["climate.rixens.state"] == "heating"
    assert a.derived(now=time.time() + 11)["climate.rixens.state"] == "no data"
    assert {r["id"] for r in a.summary()} == {"724", "725", "726"}   # raw inventory still kept
