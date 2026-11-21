#!/usr/bin/env python3
"""SYNTHETIC Rixens MCS7 frames on a vcan bus, laid out per the rixens-esphome frame map
(0x724 sources, 0x725 burner diag, 0x726 temps/voltage). The burner cycles every 60 s.
Only exercises our decoder against our own reading of that map; it proves nothing about a real MCS7.
"""
import argparse
import math
import time

import can


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--channel", default="vcan1")
    a = ap.parse_args()
    if not a.channel.startswith("vcan"):
        raise SystemExit("synthetic traffic only goes to vcan* interfaces")
    bus = can.Bus(interface="socketcan", channel=a.channel)
    t0 = time.time()
    try:
        while True:
            t = time.time() - t0
            burning = (t % 60) < 35
            glycol = 55 + 20 * math.sin(t / 40)
            fuel = 23 if burning else 0
            fan = 3200 if burning else 0
            glow = 0 if burning else 0
            msgs = [
                (0x724, bytes([0, 0, 0, 0, 0, 0, 0x08, 0])),
                (0x725, bytes([0, 0, 0, fuel]) + fan.to_bytes(2, "little") + glow.to_bytes(2, "little")),
                (0x726, bytes([0, 0]) + int(glycol * 100).to_bytes(2, "little", signed=True) + bytes(2) + (131).to_bytes(2, "little")),
            ]
            for i, d in msgs:
                bus.send(can.Message(arbitration_id=i, data=d, is_extended_id=False))
            time.sleep(0.5)
    except KeyboardInterrupt:
        pass
    finally:
        bus.shutdown()


if __name__ == "__main__":
    main()
