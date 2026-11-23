"""Espar UDS read-only diagnostics: request builder, ISO-TP reassembly and reply decoding (no bus)."""
import pytest

from aetherrv.adapters.espar_uds import FLOW_CONTROL, READ_DTCS, IsoTpRx, decode, read_did, request


def test_requests_are_read_only_single_frames():
    assert read_did(0xFD04) == bytes.fromhex("0322FD04AAAAAAAA")
    assert READ_DTCS == bytes.fromhex("03190208AAAAAAAA")
    assert FLOW_CONTROL == bytes.fromhex("30001 4AAAAAAAAAA".replace(" ", ""))
    for svc in (0x2E, 0x31, 0x11, 0x10, 0x27, 0x14):         # write, routine, reset, session, security, clear
        with pytest.raises(ValueError):
            request(svc, 0, 0)


def test_single_frame_replies():
    rx = IsoTpRx()
    p, fc = rx.feed(bytes.fromhex("0562FD040084FFFF"))            # 13.2 V; trailing bytes are junk
    assert not fc and decode(p) == {"did": 0xFD04, "key": "voltage_v", "value": 13.2}
    p, _ = rx.feed(bytes.fromhex("0462FD0D28AAAAAA"))
    assert decode(p)["value"] == 4.0                               # pump 4.0 Hz
    p, _ = rx.feed(bytes.fromhex("0562FD084E20AAAA"))
    assert decode(p)["value"] == 0                                 # blower stopped (raw 20000)
    p, _ = rx.feed(bytes.fromhex("037F2231AAAAAAAA"))
    assert decode(p) == {"negative": {"service": 0x22, "nrc": 0x31}}


def test_multi_frame_reply_and_phase_and_dtcs():
    rx = IsoTpRx()
    p, fc = rx.feed(bytes.fromhex("100B62FD10011400"), now=1.0)    # FD10: 8 data bytes → 11-byte reply
    assert p is None and fc is True
    p, fc = rx.feed(bytes.fromhex("2100000000000000"), now=1.01)
    assert decode(p) == {"did": 0xFD10, "key": "phase", "value": {"request": "heat", "phase": "burning"}}
    p, _ = rx.feed(bytes.fromhex("100B5902FF000115"), now=2.0)     # two DTCs: P000115 (stored), P000440 (active)
    p, _ = rx.feed(bytes.fromhex("2108000440090000"), now=2.01)
    assert decode(p)["dtcs"] == [
        {"code": "P000115", "text": "overheating (software threshold)", "active": False, "severity": 5, "lockout": False},
        {"code": "P000440", "text": "timeout talking to the controller", "active": True, "severity": 1, "lockout": False}]
    p, _ = rx.feed(bytes.fromhex("2208000440090000"), now=9.0)     # stray consecutive frame: ignored
    assert p is None


def test_hours_pressure_and_strokes():
    p = decode(bytes.fromhex("62FD17") + b"".join(m.to_bytes(4, "big") for m in (600, 60, 120, 180, 240, 30, 0)))
    assert p["value"] == {"operating": 10.0, "p0_25": 1.0, "p25_50": 2.0, "p50_75": 3.0, "p75_100": 4.0, "pause": 0.5, "altitude": 0.0}
    assert decode(bytes.fromhex("62FD3627B0"))["value"] == 1016.0
    assert decode(bytes.fromhex("62FD6A00989680"))["value"] == 10_000_000
