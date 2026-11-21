#!/usr/bin/env python3
"""Generate SYNTHETIC NMEA 2000-shaped traffic on vcan0 to exercise the pipeline end to end.

Identifiers are real PGN numbers from made-up source addresses. Payloads are random bytes,
EXCEPT PGN 127501, which is encoded per CANboat's layout (two 8-channel banks whose channels
toggle slowly) so the switch tiles can be exercised end to end. Use van captures to test decoders.
"""

import argparse
import os
import random
import time

import can

PGNS = [127501, 127508, 130312]


def n2k_id(pgn: int, source: int, priority: int = 6, dest: int = 255) -> int:
    pf = (pgn >> 8) & 0xFF
    if pf < 240:
        pgn = (pgn & 0x3FF00) | dest
    return (priority << 26) | (pgn << 8) | source


def encode_127501(instance: int, states: list[int]) -> bytes:
    """states: up to 28 values of 0 off / 1 on / 2 error / 3 n/a (missing → 3)."""
    states = (list(states) + [3] * 28)[:28]
    bits = sum((v & 3) << (2 * i) for i, v in enumerate(states))
    return bytes([instance]) + bits.to_bytes(7, "little")


def name_bytes(unique, mfr, function, dclass, instance=0):
    n = unique | mfr << 21 | (instance & 7) << 32 | (instance >> 3) << 35 | function << 40 | dclass << 49 | 4 << 60 | 1 << 63
    return n.to_bytes(8, "little")


def product(model, sw, serial, len_=1):
    f = lambda t: t.encode().ljust(32, b"\xff")
    return (2100).to_bytes(2, "little") + (1000).to_bytes(2, "little") + f(model) + f(sw) + f("1") + f(serial) + bytes([1, len_])


def fast_frames(payload: bytes, seq: int) -> list[bytes]:
    out = [bytes([seq << 5, len(payload)]) + payload[:6]]
    rest, i = payload[6:], 1
    while rest:
        out.append(bytes([seq << 5 | i]) + rest[:7])
        rest, i = rest[7:], i + 1
    return [f.ljust(8, b"\xff") for f in out]


# Synthetic bench devices (Maretron = 137, Victron = 358). --ipg-flaky makes the IPG100 vanish.
DEVICES = [
    (0x10, name_bytes(1001, 137, 130, 25), product("IPG100", "1.6.2", "BENCH-IPG")),
    (0x42, name_bytes(1002, 137, 140, 30), product("DCR100", "1.3.1", "BENCH-DCR1")),
    (0x43, name_bytes(1003, 137, 140, 30, 1), product("DCR100", "1.3.1", "BENCH-DCR2")),
    (0x23, name_bytes(1004, 358, 130, 25), product("Cerbo GX", "v3.60", "BENCH-CERBO")),
]


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--channel", default="vcan0")
    ap.add_argument("--rate", type=float, default=20.0, help="frames per second")
    ap.add_argument("--ipg-flaky", action="store_true", help="stop the bench IPG100 after 20 s")
    a = ap.parse_args()
    if not a.channel.startswith("vcan"):
        raise SystemExit("synthetic traffic only goes to vcan* interfaces")
    bus = can.Bus(interface="socketcan", channel=a.channel)
    sources = [0x10, 0x23, 0x42]
    t0, seq, last_announce = time.time(), 0, 0.0
    try:
        while True:
            if time.time() - last_announce > 10:          # identity + heartbeat every 10 s
                last_announce = time.time()
                for sa, name, prod in DEVICES:
                    if a.ipg_flaky and sa == 0x10 and time.time() - t0 > 20:
                        continue
                    bus.send(can.Message(arbitration_id=n2k_id(60928, sa, dest=255), data=name, is_extended_id=True))
                    bus.send(can.Message(arbitration_id=n2k_id(126993, sa, priority=7), data=bytes([0x60, 0xEA, seq & 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]), is_extended_id=True))
                    seq += 1
                    for fr in fast_frames(prod, seq & 7):
                        bus.send(can.Message(arbitration_id=n2k_id(126996, sa), data=fr, is_extended_id=True))
            pgn = random.choice(PGNS)
            if pgn == 127501:
                inst = random.choice([0, 1])
                phase = int(time.time() / 5)
                data = encode_127501(inst, [((phase >> (c % 4)) + c + inst) & 1 for c in range(8)])
                msg = can.Message(arbitration_id=n2k_id(pgn, 0x42 + inst, priority=3), data=data, is_extended_id=True)
            else:
                msg = can.Message(arbitration_id=n2k_id(pgn, random.choice([x for x in sources if not (a.ipg_flaky and x == 0x10 and time.time() - t0 > 20)])), data=os.urandom(8), is_extended_id=True)
            bus.send(msg)
            time.sleep(1.0 / a.rate)
    except KeyboardInterrupt:
        pass
    finally:
        bus.shutdown()


if __name__ == "__main__":
    main()
