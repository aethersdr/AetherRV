#!/usr/bin/env python3
"""Replay a candump-format capture onto a VIRTUAL bus (vcanN) for bench development.

    python tools/replay.py captures/hat0-20261121-101500.log --channel vcan0 --loop

It refuses to write to anything that isn't a vcan interface: captures must never be
replayed onto a live heater or N2K bus.
"""

import argparse
import asyncio
from pathlib import Path

import can

from aetherrv.canbus import replay


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("log", type=Path)
    ap.add_argument("--channel", default="vcan0")
    ap.add_argument("--speed", type=float, default=1.0, help="1.0 = real time, 0 = as fast as possible")
    ap.add_argument("--loop", action="store_true")
    a = ap.parse_args()
    if not a.channel.startswith("vcan"):
        raise SystemExit(f"refusing to replay onto {a.channel!r}: only vcan* interfaces are allowed")
    bus = can.Bus(interface="socketcan", channel=a.channel)
    try:
        asyncio.run(replay(a.log, bus, a.speed, a.loop))
    finally:
        bus.shutdown()


if __name__ == "__main__":
    main()
