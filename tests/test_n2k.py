from aetherrv.adapters.n2k import (FastPacketAssembler, N2kAdapter, decode_126208_header, decode_126996,
                                  decode_127501, decode_60928, parse_id)
from aetherrv.state import State

import can
import pytest
from tools_bench import encode_127501, n2k_id


def test_127501_independent_vector():
    # ch1 on (01), ch2 off (00), ch3 on (01), ch4 error (10) -> byte1 = 0b10_01_00_01 = 0x91,
    # every other channel "not available" (11) -> 0xFF
    data = bytes([3, 0x91, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF])
    inst, st = decode_127501(data)
    assert inst == 3
    assert st[:5] == ["on", "off", "on", "error", None]
    assert st[4:] == [None] * 24 and len(st) == 28


def test_127501_roundtrip_with_bench_encoder():
    states = [1, 0, 1, 1, 0, 0, 1, 0]
    inst, st = decode_127501(encode_127501(7, states))
    assert inst == 7 and st[:8] == ["on" if s else "off" for s in states] and st[8] is None


def test_127501_short_frame_rejected():
    with pytest.raises(ValueError):
        decode_127501(b"\x00\x01")


def frames(payload: bytes, seq: int = 2) -> list[bytes]:
    out = [bytes([seq << 5 | 0, len(payload)]) + payload[:6]]
    rest, i = payload[6:], 1
    while rest:
        out.append(bytes([seq << 5 | i]) + rest[:7])
        rest, i = rest[7:], i + 1
    return [f.ljust(8, b"\xff") for f in out]


def test_fast_packet_reassembly_and_interleave():
    fp = FastPacketAssembler()
    a = bytes(range(20))          # 3 frames
    b = bytes(range(100, 113))    # 2 frames, different source
    fa, fb = frames(a), frames(b, seq=5)
    assert fp.feed(1, 126208, fa[0], 0.0) is None
    assert fp.feed(2, 126208, fb[0], 0.0) is None
    assert fp.feed(1, 126208, fa[1], 0.01) is None
    assert fp.feed(2, 126208, fb[1], 0.01) == b
    assert fp.feed(1, 126208, fa[2], 0.02) == a


def test_fast_packet_drops_gaps_and_stale():
    fp = FastPacketAssembler()
    fa = frames(bytes(range(20)))
    fp.feed(1, 126208, fa[0], 0.0)
    assert fp.feed(1, 126208, fa[2], 0.01) is None and fp.dropped == 1   # skipped frame 1
    fp.feed(1, 126208, fa[0], 1.0)
    assert fp.feed(1, 126208, fa[1], 5.0) is None and fp.dropped == 2    # too slow
    assert fp.feed(9, 126208, fa[1], 0.0) is None                        # joined mid-message


def test_126208_header():
    # Command (1) for PGN 127501 = 0x01F20D little-endian, then raw parameters
    h = decode_126208_header(bytes([1, 0x0D, 0xF2, 0x01, 0xF8, 0x02, 0x01, 0x00]))
    assert h["function"] == "Command" and h["pgn"] == 127501 and h["rest"] == "f8 02 01 00"


def test_adapter_switch_list_with_labels():
    s = State()
    a = N2kAdapter(s, [{"bank": 0, "channel": 2, "name": "Galley", "icon": "light", "group": "Lights"},
                       {"bank": 0, "channel": 12, "name": "Spare", "icon": "plug", "group": "Power"}])
    msg = can.Message(arbitration_id=n2k_id(127501, 0x30, priority=3), data=encode_127501(0, [1, 0, 1]),
                      is_extended_id=True)
    a.on_frame(msg)
    lst = {x["id"]: x for x in a.switch_list()}
    assert set(lst) == {"0.1", "0.2", "0.3", "0.12"}   # available channels + configured ones
    assert lst["0.2"]["name"] == "Galley" and lst["0.2"]["on"] is False and lst["0.2"]["group"] == "Lights"
    assert lst["0.1"]["group"] == "Unassigned" and lst["0.1"]["on"] is True
    assert lst["0.12"]["state"] == "n/a" and lst["0.12"]["on"] is False


def test_adapter_reassembles_126208_command():
    s = State()
    a = N2kAdapter(s)
    payload = bytes([1, 0x0D, 0xF2, 0x01, 0xF8, 0x02, 0x01, 0x00, 0x02, 0x01])
    for f in frames(payload):
        a.on_frame(can.Message(arbitration_id=n2k_id(126208, 0x10, priority=3, dest=0x30), data=f, is_extended_id=True))
    assert len(a.commands) == 1 and a.commands[0]["pgn"] == 127501 and a.commands[0]["dest"] == 0x30


def test_adapter_marks_stale_banks():
    import time
    a = N2kAdapter(State())
    a.on_frame(can.Message(arbitration_id=n2k_id(127501, 0x30, priority=3), data=encode_127501(0, [1]), is_extended_id=True))
    fresh = a.switch_list()[0]
    assert fresh["state"] == "on" and fresh["on"] is True
    old = a.switch_list(now=time.time() + 31)[0]
    assert old["state"] == "stale" and old["on"] is False



def name_bytes(unique, mfr, inst_lo, inst_hi, function, dclass, sys_inst=0, industry=4, aac=1):
    n = (unique | mfr << 21 | inst_lo << 32 | inst_hi << 35 | function << 40 | dclass << 49
         | sys_inst << 56 | industry << 60 | aac << 63)
    return n.to_bytes(8, "little")


def test_60928_vector():
    # Hand-packed NAME: unique 12345, Maretron (137), instance 0b00011_010 = 26, function 140, class 30
    d = decode_60928(name_bytes(12345, 137, 0b010, 0b00011, 140, 30))
    assert d["unique"] == 12345 and d["manufacturer"] == "Maretron"
    assert d["device_instance"] == 26 and d["function"] == 140 and d["device_class"] == "Electrical Distribution"
    # independent byte check: unique 12345 = 0x003039, mfr 137 << 21 sets byte 2 high bits 0b001 << 5 and byte 3 = 0x11
    raw = name_bytes(12345, 137, 0, 0, 0, 0, industry=0, aac=0)
    assert raw[:4] == bytes([0x39, 0x30, 0x20, 0x11])


def product_payload(model, sw, serial, len_=3):
    f = lambda t: t.encode().ljust(32, b"\xff")
    return (2100).to_bytes(2, "little") + (1234).to_bytes(2, "little") + f(model) + f(sw) + f("1") + f(serial) + bytes([1, len_])


def test_126996_vector():
    d = decode_126996(product_payload("IPG100", "1.6.2", "1234567"))
    assert d == {"n2k_version": 2.1, "product_code": 1234, "model": "IPG100", "software": "1.6.2",
                 "model_version": "1", "serial": "1234567", "len": 3}


def test_device_list_passive_and_offline():
    a = N2kAdapter(State())
    send = lambda pgn, src, data: [a.on_frame(can.Message(arbitration_id=n2k_id(pgn, src, priority=6), data=f, is_extended_id=True))
                                   for f in (frames(data) if len(data) > 8 else [data])]
    send(60928, 0x23, name_bytes(777, 137, 0, 0, 130, 25))
    send(126996, 0x23, product_payload("IPG100", "1.6.2", "SN42"))
    send(126993, 0x23, bytes([0x60, 0xEA, 1, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]))
    send(127501, 0x30, encode_127501(0, [1]))                       # a device we only see traffic from
    devs = {d["source"]: d for d in a.device_list()}
    assert devs[0x23]["model"] == "IPG100" and devs[0x23]["manufacturer"] == "Maretron" and devs[0x23]["heartbeat"]
    assert devs[0x23]["class"] == "Internetwork device" and devs[0x30]["model"] == "" and devs[0x30]["online"]
    import time
    assert not {d["source"]: d for d in a.device_list(now=time.time() + 120)}[0x23]["online"]
    # same NAME re-claims a new address -> old entry removed
    send(60928, 0x24, name_bytes(777, 137, 0, 0, 130, 25))
    assert 0x23 not in {d["source"] for d in a.device_list()}



def test_device_names_from_cerbo_hints():
    s = State()
    a = N2kAdapter(s)
    a.on_frame(can.Message(arbitration_id=n2k_id(127501, 0, priority=3), data=encode_127501(32, [0]), is_extended_id=True))
    s.set("n2k.names_from_cerbo", {"0": {"ModelName": "CLMD16", "Manufacturer": 176, "Serial": 1540324, "FirmwareVersion": "1.0.3.66"}})
    d = a.device_list()[0]
    assert d["model"] == "CLMD16" and d["serial"] == "1540324" and d["software"] == "1.0.3.66"



def test_same_instance_from_two_sources_does_not_collide():
    a = N2kAdapter(State())
    a.on_frame(can.Message(arbitration_id=n2k_id(127501, 225, priority=3), data=encode_127501(0, [1]), is_extended_id=True))
    a.on_frame(can.Message(arbitration_id=n2k_id(127501, 226, priority=3), data=encode_127501(0, [0, 1]), is_extended_id=True))
    ids = {x["id"]: x["state"] for x in a.switch_list()}
    assert ids == {"0.1@225": "on", "0.1@226": "off", "0.2@226": "on"}
