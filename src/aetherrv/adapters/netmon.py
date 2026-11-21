"""Network health monitor for van devices (IPG100, MCS7, router, …). Observation only.

Each target is checked every `interval` seconds with:
  * ICMP echo via an unprivileged ICMP datagram socket (needs net.ipv4.ping_group_range to
    include the service's group; no capabilities, so it works inside the hardened unit)
  * optionally a TCP connect to `tcp` (e.g. the IPG100's N2KServer port 6544). The connection is
    closed immediately: no handshake beyond TCP, no login, no data.
A target is DOWN after `fail_after` consecutive failed checks. A host that answers ping but not
its service port is reported as "service not answering" (the IPG100's usual lockup mode).
"""

from __future__ import annotations

import asyncio
import os
import socket
import struct
import time

from ..state import State


def _icmp_ping(host: str, timeout: float) -> float | None:
    """One echo request; returns RTT in ms, or None. Runs in a worker thread."""
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_ICMP)
    try:
        s.settimeout(timeout)
        seq = os.getpid() & 0xFFFF
        payload = b"aetherrv"
        hdr = struct.pack("!BBHHH", 8, 0, 0, 0, seq)          # kernel fills id + checksum for DGRAM ICMP
        t0 = time.monotonic()
        s.sendto(hdr + payload, (host, 0))
        while True:
            data, _ = s.recvfrom(1024)
            if data and data[0] == 0:                          # echo reply
                return round((time.monotonic() - t0) * 1000, 1)
    except OSError:
        return None
    finally:
        s.close()


async def _tcp_ok(host: str, port: int, timeout: float) -> bool:
    try:
        _, w = await asyncio.wait_for(asyncio.open_connection(host, port), timeout)
        w.close()
        try:
            await w.wait_closed()
        except OSError:
            pass
        return True
    except (OSError, asyncio.TimeoutError):
        return False


class NetMon:
    device = "netmon"

    def __init__(self, state: State, targets: list[dict], interval: float = 60.0, timeout: float = 3.0) -> None:
        self.state, self.interval, self.timeout = state, interval, timeout
        self.targets = [{"name": t["name"], "host": t["host"], "tcp": t.get("tcp"),
                         "fail_after": int(t.get("fail_after", 2)), "severity": t.get("severity", "warning"),
                         "hint": t.get("hint", ""), "fails": 0, "down_since": None, "last_ok": None,
                         "rtt_ms": None, "tcp_ok": None, "up": None} for t in targets]

    async def check(self, t: dict, now: float | None = None) -> None:
        rtt = await asyncio.to_thread(_icmp_ping, t["host"], self.timeout)
        tcp = await _tcp_ok(t["host"], int(t["tcp"]), self.timeout) if t["tcp"] else None
        self.apply(t, rtt, tcp, now)

    @staticmethod
    def apply(t: dict, rtt: float | None, tcp: bool | None, now: float | None = None) -> None:
        now = time.time() if now is None else now
        ok = (rtt is not None) and (tcp is not False)
        t["rtt_ms"], t["tcp_ok"] = rtt, tcp
        if ok:
            t["fails"], t["up"], t["down_since"], t["last_ok"] = 0, True, None, now
            return
        t["fails"] += 1
        if t["fails"] >= t["fail_after"]:
            if t["up"] is not False:
                t["down_since"] = t["last_ok"] if t["last_ok"] is not None else now
            t["up"] = False

    def snapshot(self, now: float | None = None) -> list[dict]:
        now = time.time() if now is None else now
        out = []
        for t in self.targets:
            reason = None
            if t["up"] is False:
                reason = "service not answering" if (t["rtt_ms"] is not None and t["tcp_ok"] is False) else "no response"
            out.append({"name": t["name"], "host": t["host"], "tcp": t["tcp"], "up": t["up"], "rtt_ms": t["rtt_ms"],
                        "tcp_ok": t["tcp_ok"], "reason": reason, "severity": t["severity"], "hint": t["hint"],
                        "down_for": round(now - t["down_since"]) if t["down_since"] is not None else None})
        return out

    async def run(self) -> None:
        while True:
            await asyncio.gather(*(self.check(t) for t in self.targets))
            self.state.set("netmon.targets", self.snapshot())
            await asyncio.sleep(self.interval)
