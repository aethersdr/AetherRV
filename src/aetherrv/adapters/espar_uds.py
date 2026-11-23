"""Espar heater diagnostics: UDS (ISO 14229) over ISO-TP on the heater's CAN bus. READ requests only.

Transport and data identifiers from the public Airtronic S3 B2L work (TrooperDuper/espar-airtronic-esphome,
docs/uds-diagnostics.md and docs/fault-codes.md); facts only, no code taken. Unverified on this S2 D2L until
the first replies decode sensibly.

  request 0x7A0 (tester → heater), reply 0x73C (heater → tester), 8-byte frames padded with 0xAA
  read:        03 22 <DID hi> <DID lo> AA AA AA AA     (default session, no unlock needed)
  faults:      03 19 02 08 AA AA AA AA                  (reportDTCByStatusMask, confirmed DTCs)
  flow control after a first frame: 30 00 14 AA AA AA AA AA   (clear to send, no block limit, 20 ms)
  replies: 62 <DID> <data…> | 59 02 <mask> {3 code bytes + 1 status}… | 7F <service> <NRC>
  multi-byte values on this channel are BIG-endian (the cyclic 2C4/054 frames are little-endian)

Never sent: writes (0x2E), routine control (0x31), ECU reset (0x11), session changes (0x10), security access
(0x27), clearing faults (0x14). `request()` refuses any service other than 0x22 and 0x19.
"""

from __future__ import annotations

import time

REQ_ID, RESP_ID = 0x7A0, 0x73C
PAD = 0xAA
FLOW_CONTROL = bytes([0x30, 0x00, 0x14] + [PAD] * 5)
READ_ONLY_SERVICES = {0x22, 0x19}

# 0x0C seen on this S2 D2L (2026-11-22): glow plug on (~80 W), then fuel pump 2 Hz, until the flame is detected (~2 min)
PHASES = {0x00: "off", 0xD2: "ventilation", 0x04: "startup", 0x0F: "startup", 0x10: "startup", 0x07: "pre-glow", 0x0C: "ignition",
          0x08: "ignition", 0x09: "flame ramp-up", 0x14: "burning", 0x28: "control pause", 0x1E: "cool-down"}

P_CODES = {
    "P0001A": "lockout: too many overheating events", "P00010": "overheat/outlet sensor open",
    "P000101": "overheat/outlet sensor short", "P000110": "air inlet sensor open", "P000111": "air inlet sensor short",
    "P00010A": "cold-blowing timeout", "P000114": "possible overheating (implausible signal)",
    "P000115": "overheating (software threshold)", "P000116": "overheating (hardware threshold, >150 °C)",
    "P00011A": "lockout: too many overheating events", "P000120": "flame sensor open", "P000121": "flame sensor short",
    "P000125": "flame cut-out during start", "P000126": "flame cut-out at 0-25 % power",
    "P000127": "flame cut-out at 25-50 % power", "P000128": "flame cut-out at 50-75 % power",
    "P000129": "flame cut-out at 75-100 % power", "P00012A": "start failed",
    "P00012B": "lockout: 10 failed starts in a row", "P000143": "air pressure sensor implausible",
    "P000200": "fuel pump open circuit", "P000201": "fuel pump short", "P000202": "fuel pump shorted to + / driver fault",
    "P000210": "glow plug open circuit", "P000211": "glow plug short", "P000212": "glow plug shorted to +",
    "P000213": "glow plug: ignition energy too low", "P000223": "burner motor blocked",
    "P000224": "burner motor current too high", "P000301": "watchdog reset", "P000302": "too many watchdog resets",
    "P000304": "too many resets (loose contact?)", "P000307": "CAN communication error in the control unit",
    "P00030A": "CAN communication error", "P000310": "over-voltage cut-out", "P000312": "under-voltage cut-out",
    "P000313": "under-voltage (>20 s)?", "P000342": "too many CAN controllers on the bus",
    "P000440": "timeout talking to the controller", "P000450": "LIN communication error",
}


def _u16(d: bytes, i: int) -> int | None:
    return int.from_bytes(d[i:i + 2], "big") if len(d) >= i + 2 else None


def _temp100(w: int | None) -> float | None:            # FD01 style: raw / 100 − 50 °C, FFFF = not fitted
    return None if w is None or w == 0xFFFF else round(w / 100 - 50, 1)


def _temp10(w: int | None) -> float | None:             # FD45 style: (°C + 50) × 10
    return None if w is None or w in (0xFFFE, 0xFFFF) else round(w / 10 - 50, 1)


# DID → (state key, decoder returning a value or a dict of values)
DIDS = {
    0xFD04: ("voltage_v", lambda d: None if _u16(d, 0) is None else round(_u16(d, 0) / 10, 1)),
    0xFD08: ("blower", lambda d: None if _u16(d, 0) is None else max(0, _u16(d, 0) - 20000)),   # 0 = stopped
    0xFD0A: ("glow_w", lambda d: None if _u16(d, 0) is None else round(_u16(d, 0) / 10, 1)),
    0xFD0D: ("pump_hz", lambda d: round(d[0] / 10, 1) if d else None),
    0xFD0E: ("power_pct", lambda d: None if _u16(d, 0) is None else round(_u16(d, 0) / 100, 1)),
    0xFD0F: ("flame", lambda d: {"flame": bool(d[0]), "fuel_state": {1: "no fuel", 2: "pumping"}.get(d[1], d[1])}
             if len(d) >= 2 else None),
    0xFD10: ("phase", lambda d: {"request": {0: "off", 1: "heat", 3: "fan"}.get(d[0], d[0]),
                                 "phase": PHASES.get(d[1], f"0x{d[1]:02x}")} if len(d) >= 2 else None),
    0xFD11: ("fuel", lambda d: {"rate_ul_s": d[1], "total_l": round(int.from_bytes(d[2:6], "big")
                                                                   + int.from_bytes(d[6:10], "big") / 1e9, 3)}
             if len(d) >= 10 else None),
    # 4 words on the S2 D2L: flame, intake, outlet (found by the 2026-11-22 scan: tracks the warm-air outlet,
    # 8.8 °C cold → 52 °C at full heat while intake stayed 9-18 °C), and a 4th that reads exactly 0 °C (not fitted)
    0xFD01: ("temps", lambda d: {"flame_c": _temp100(_u16(d, 0)), "intake_c": _temp100(_u16(d, 2)),
                                 "outlet_c": _temp100(_u16(d, 4))} if len(d) >= 4 else None),
    0xFD45: ("regulation", lambda d: {"setpoint_c": _temp10(_u16(d, 0)), "regulated_c": _temp10(_u16(d, 2))}
             if len(d) >= 4 else None),
    0xFD2A: ("burn_s", lambda d: None if _u16(d, 0) is None else round(_u16(d, 0) / 100, 1)),
    0xFD17: ("hours", lambda d: {k: round(int.from_bytes(d[i * 4:i * 4 + 4], "big") / 60, 1) for i, k in enumerate(
             ("operating", "p0_25", "p25_50", "p50_75", "p75_100", "pause", "altitude")) if len(d) >= i * 4 + 4}),
    0xFD36: ("air_hpa", lambda d: None if _u16(d, 0) is None else round(_u16(d, 0) / 10, 1)),
    0xFD6A: ("pump_strokes", lambda d: int.from_bytes(d[0:4], "big") if len(d) >= 4 else None),
    0xFD2B: ("ignition_s", lambda d: None if _u16(d, 0) is None else round(_u16(d, 0) / 100, 1)),
    0xFD35: ("unit_c", lambda d: _temp100(_u16(d, 0))),
    0xFD06: ("event_count", lambda d: _u16(d, 0)),
    0xF195: ("software", lambda d: d.decode("ascii", "replace").strip("\x00 ")),
    0xF193: ("hardware", lambda d: d.decode("ascii", "replace").strip("\x00 ")),
    0xFD58: ("variant", lambda d: d.decode("ascii", "replace").strip("\x00 ")),
}
# What the Climate card shows is read every cycle (phase, temperatures, blower, glow, pump, power, flame, fuel);
# the slower values one per cycle, in turn. At diag_interval_s = 1 a card tile refreshes every ~9 s.
FAST_DIDS = [0xFD10, 0xFD01, 0xFD08, 0xFD0A, 0xFD0D, 0xFD0E, 0xFD0F, 0xFD11]
SLOW_DIDS = [0xFD04, 0xFD45, 0xFD2A, 0xFD2B, 0xFD36, 0xFD35]
LIVE_DIDS = [x for s in SLOW_DIDS for x in (*FAST_DIDS, s)]
ONCE_DIDS = [0xF195, 0xF193, 0xFD17, 0xFD06, 0xFD6A]    # (FD58 variant: 'out of range' on the S2 D2L)

# Severity class per P-code (Airtronic S3 service manual via the S3 project): 1 service, 2 under-voltage,
# 3 over-voltage, 4 fuel supply (heat stops), 5 air system (immediate stop), 6 overheating lockout, 7 limp/log.
SEVERITY = {"P0001A": 6, "P00011A": 6, "P000115": 5, "P000116": 5, "P00012A": 4, "P000200": 4, "P000201": 4,
            "P000202": 4, "P000310": 3, "P000312": 2, "P000143": 7, "P000130": 7, "P000131": 7}
LOCKOUT = {"P00012B": "10 failed starts in a row", "P00011A": "too many overheating events",
           "P0001A": "too many overheating events"}
LOCKOUT_HELP = ("Locked out by the heater. Reset: switch the heater on, pull the heater's fuse within 20 s, "
                "wait ~5 s, refit it. Find and fix the cause first (fuel, exhaust, air path).")


def request(service: int, *args: int) -> bytes:
    """One single-frame read request. Only ReadDataByIdentifier (0x22) and ReadDTCInformation (0x19)."""
    if service not in READ_ONLY_SERVICES:
        raise ValueError(f"UDS service 0x{service:02x} is not allowed (read-only)")
    body = bytes([service, *args])
    if len(body) > 7:
        raise ValueError("request too long for a single frame")
    return bytes([len(body)]) + body + bytes([PAD] * (7 - len(body)))


def read_did(did: int) -> bytes:
    return request(0x22, did >> 8, did & 0xFF)


READ_DTCS = request(0x19, 0x02, 0x08)


class IsoTpRx:
    """Reassembles replies on 0x73C. feed() → (complete payload | None, need_flow_control)."""

    def __init__(self) -> None:
        self.buf = b""
        self.want = 0
        self.next_sn = 1
        self.t = 0.0

    def feed(self, d: bytes, now: float | None = None) -> tuple[bytes | None, bool]:
        now = now or time.time()
        if not d:
            return None, False
        pci = d[0] >> 4
        if pci == 0:                                   # single frame: length in the low nibble; extra bytes are junk
            n = d[0] & 0x0F
            self.want = 0
            return (bytes(d[1:1 + n]) if 0 < n <= 7 else None), False
        if pci == 1:                                   # first frame: 12-bit length, then 6 data bytes
            self.want = ((d[0] & 0x0F) << 8) | d[1]
            self.buf, self.next_sn, self.t = bytes(d[2:8]), 1, now
            return None, True
        if pci == 2 and self.want:                     # consecutive frame
            if (d[0] & 0x0F) != self.next_sn or now - self.t > 1.0:
                self.want = 0                          # out of order or stale: drop it
                return None, False
            self.buf += bytes(d[1:8])
            self.next_sn, self.t = (self.next_sn + 1) & 0x0F, now
            if len(self.buf) >= self.want:
                out, self.want = self.buf[:self.want], 0
                return out, False
        return None, False


def decode(payload: bytes) -> dict:
    """A complete UDS reply → {"did": .., "key": .., "value": ..} | {"dtcs": [...]} | {"negative": ..}."""
    if not payload:
        return {}
    sid = payload[0]
    if sid == 0x62 and len(payload) >= 3:
        did = payload[1] << 8 | payload[2]
        key, fn = DIDS.get(did, (f"did_{did:04x}", lambda d: d.hex(" ")))
        try:
            value = fn(bytes(payload[3:]))
        except (IndexError, ValueError):
            value = None
        return {"did": did, "key": key, "value": value}
    if sid == 0x59 and len(payload) >= 3 and payload[1] == 0x02:
        dtcs = []
        for i in range(3, len(payload) - 3, 4):
            code = "P" + payload[i:i + 3].hex().upper()
            dtcs.append({"code": code, "text": P_CODES.get(code, "unknown"), "active": bool(payload[i + 3] & 0x01),
                         "severity": SEVERITY.get(code, 1), "lockout": code in LOCKOUT})
        return {"dtcs": dtcs}
    if sid == 0x7F and len(payload) >= 3:
        return {"negative": {"service": payload[1], "nrc": payload[2]}}
    return {"raw": payload.hex(" ")}
