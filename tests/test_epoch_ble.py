"""EPOCH (RoyPow BMS) BLE frames, as captured from battery 240200401 on 2026-11-21."""
from aetherrv.adapters.epoch_ble import EpochBleAdapter, FrameAssembler, decode, query
from aetherrv.state import State

NOTES = [  # notification chunks exactly as received, in order
    "ead10139ff040164020027030007040 4e00500070 6223e070006 08dffd091198 0affff0b000500d4",
    "0c2611040819470532 0cff0cfc0d14ce0080000f", "f5",
    "ead1011aff033102450000000004333237 3c0003", "000000128700000008f5",
    "ead1010fff020404040cfd0cfc0cfc0cfff4f5",
]


def test_queries_match_what_the_battery_answered():
    assert query(0x04).hex() == "ead10104ff04fff5"
    assert query(0x03).hex() == "ead10104ff03f8f5"
    assert query(0x02).hex() == "ead10104ff02f9f5"


def test_assemble_and_decode_real_frames():
    fa = FrameAssembler()
    for n in NOTES:
        fa.feed(bytes.fromhex(n.replace(" ", "")))
    assert sorted(fa.msgs) == [2, 3, 4]
    d = decode(fa.msgs)
    assert (d["soc"], d["voltage"], d["current"], d["cycles"], d["problem_code"]) == (100, 13.3, -5.81, 39, 0)
    assert d["cells"] == [3.325, 3.324, 3.324, 3.327] and d["cell_delta_mv"] == 3
    assert d["temps"] == [11.0, 10.0, 15.0, 20.0] and d["charge_fet"] and d["discharge_fet"]
    assert d["remaining_ah"] == 450.3


def test_bad_checksum_is_dropped():
    fa = FrameAssembler()
    fa.feed(bytes.fromhex("ead1010fff020404040cfd0cfc0cfc0cff00f5"))      # wrong xor
    assert fa.msgs == {}


def test_publish_counts_only_fresh_batteries():
    s = State()
    a = EpochBleAdapter(s, [{"name": "296", "address": "A"}, {"name": "401", "address": "B"}])
    a.last["B"] = {"t": 1000.0, "data": {"soc": 100, "current": -5.81}}
    a.publish(now=1030.0)
    assert s.get("batteries.ble.reporting") == 1 and s.get("batteries.ble.current") == -5.81
    a.publish(now=1000.0 + 31)
    assert s.get("batteries.ble.reporting") == 0
