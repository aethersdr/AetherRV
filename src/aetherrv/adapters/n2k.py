"""NMEA 2000 adapter: Maretron switching plus an inventory of everything on the backbone. READ-ONLY.

Implemented, from CANboat's canboat.json (v10.0.0) definitions:
  * 29-bit ID split: priority / PGN / source / destination (J1939 PDU1 vs PDU2).
  * Fast-packet reassembly for PGNs CANboat marks as Type "Fast".
  * PGN 127501 Binary Switch Bank Status: single frame, 8 bytes. Byte 0 = bank instance,
    then 28 indicators of 2 bits each, packed LSB-first from bit 8. Values: 0 = Off, 1 = On
    (CANboat OFF_ON). 2 and 3 are unnamed in CANboat; by NMEA convention 2 = error and
    3 = not available (unused channel).
  * PGN 126208 group function: header only (function code + commanded PGN), logged so we can
    see which bank/channel a Maretron panel commands. Parameter pairs are kept as raw hex
    because their value widths still need checking against real Maretron traffic.

This adapter only listens. Switch control (126208 Command → 127501/127500) lives in
`aetherrv.switching`, behind TxGate; it subscribes to bank updates via `bank_listeners`.
  * PGN 127500 Load Controller Connection State/Control (CLMD16, single frame):
    [SID][connection][state 0 off/2 on/4 PWM][status][op][duty %][..]
  * PGN 127751 DC Voltage/Current: [SID][connection][V uint16 ×0.1][I int24 ×0.01 A, 0xFFFFFF = n/a].
    On the CLMD16, connections 0-15 are the outputs, 16-23 the wired inputs (voltage only).
"""

from __future__ import annotations

import asyncio
import logging
import struct
import time
from dataclasses import dataclass

import can

from ..state import State
from .n2k_pgns import DEVICE_CLASSES, FAST_PACKET_PGNS, MANUFACTURERS, PGN_NAMES

log = logging.getLogger(__name__)


@dataclass(frozen=True)
class N2kId:
    priority: int
    pgn: int
    source: int
    destination: int  # 255 = global/broadcast


def parse_id(arbitration_id: int) -> N2kId:
    priority = (arbitration_id >> 26) & 0x7
    pf = (arbitration_id >> 16) & 0xFF
    ps = (arbitration_id >> 8) & 0xFF
    source = arbitration_id & 0xFF
    pgn = (arbitration_id >> 8) & 0x3FFFF
    if pf < 240:  # PDU1: PS is a destination address, not part of the PGN
        pgn &= 0x3FF00
        dest = ps
    else:  # PDU2: broadcast
        dest = 255
    return N2kId(priority, pgn, source, dest)


# ── Fast-packet ──────────────────────────────────────────────────────────────
class FastPacketAssembler:
    """Reassembles NMEA 2000 fast-packet messages (up to 223 bytes over ≤32 frames).

    Frame byte 0 = sequence counter (bits 5-7) | frame index (bits 0-4).
    Frame 0: byte 1 = total length, bytes 2-7 = first 6 data bytes. Frames 1..n: 7 data bytes.
    Keyed by (source, PGN). A gap or sequence change drops the partial message.
    """

    TIMEOUT = 0.75  # seconds; a message's frames arrive back-to-back

    def __init__(self) -> None:
        self._partial: dict[tuple[int, int], dict] = {}
        self.dropped = 0

    def feed(self, source: int, pgn: int, data: bytes, ts: float | None = None) -> bytes | None:
        ts = time.monotonic() if ts is None else ts
        if not data:
            return None
        seq, idx = data[0] >> 5, data[0] & 0x1F
        key = (source, pgn)
        if idx == 0:
            if len(data) < 2:
                return None
            if key in self._partial:
                self.dropped += 1
            total = data[1]
            buf = bytearray(data[2:8])
            if len(buf) >= total:
                self._partial.pop(key, None)
                return bytes(buf[:total])
            self._partial[key] = {"seq": seq, "next": 1, "total": total, "buf": buf, "ts": ts}
            return None
        p = self._partial.get(key)
        if p is None:
            return None  # joined mid-message
        if p["seq"] != seq or p["next"] != idx or ts - p["ts"] > self.TIMEOUT:
            self.dropped += 1
            del self._partial[key]
            return None
        p["buf"] += data[1:8]
        p["next"] += 1
        p["ts"] = ts
        if len(p["buf"]) >= p["total"]:
            del self._partial[key]
            return bytes(p["buf"][: p["total"]])
        return None


# ── Decoders ─────────────────────────────────────────────────────────────────
SWITCH_STATE = {0: "off", 1: "on", 2: "error", 3: None}  # 3 = not available / unused channel


def decode_127501(data: bytes) -> tuple[int, list[str | None]]:
    """Binary Switch Bank Status → (bank instance, 28 channel states)."""
    if len(data) < 8:
        raise ValueError(f"127501 needs 8 bytes, got {len(data)}")
    instance = data[0]
    bits = int.from_bytes(data[1:8], "little")
    return instance, [SWITCH_STATE[(bits >> (2 * i)) & 0b11] for i in range(28)]


GROUP_FUNCTION = {0: "Request", 1: "Command", 2: "Acknowledge", 3: "Read Fields",
                  4: "Read Fields Reply", 5: "Write Fields", 6: "Write Fields Reply"}


def decode_126208_header(data: bytes) -> dict:
    """Group function header: function code (8 bits) + commanded/requested PGN (24 bits LE)."""
    if len(data) < 4:
        raise ValueError("126208 header needs 4 bytes")
    return {"function": GROUP_FUNCTION.get(data[0], f"0x{data[0]:02X}"),
            "pgn": int.from_bytes(data[1:4], "little"), "rest": data[4:].hex(" ")}


def decode_60928(data: bytes) -> dict:
    """ISO Address Claim: the 64-bit NAME (CANboat isoAddressClaim, bit offsets LSB-first)."""
    if len(data) < 8:
        raise ValueError("60928 needs 8 bytes")
    n = int.from_bytes(data[:8], "little")
    mfr = (n >> 21) & 0x7FF
    cls = (n >> 49) & 0x7F
    return {
        "name": f"{n:016X}",
        "unique": n & 0x1FFFFF,
        "manufacturer": MANUFACTURERS.get(mfr, f"#{mfr}"),
        "function": (n >> 40) & 0xFF,
        "device_class": DEVICE_CLASSES.get(cls, f"#{cls}"),
        "device_instance": ((n >> 35) & 0x1F) << 3 | ((n >> 32) & 0x7),
    }


def _fixstr(b: bytes) -> str:
    """STRING_FIX field: padded with 0x00, 0xFF, '@' or spaces."""
    return b.split(b"\x00")[0].replace(b"\xff", b"").decode("ascii", "replace").rstrip(" @")


def decode_126996(data: bytes) -> dict:
    """Product Information (fast-packet, 134 bytes): model, software, serial, LEN."""
    if len(data) < 134:
        raise ValueError(f"126996 needs 134 bytes, got {len(data)}")
    return {
        "n2k_version": round(int.from_bytes(data[0:2], "little") * 0.001, 3),
        "product_code": int.from_bytes(data[2:4], "little"),
        "model": _fixstr(data[4:36]),
        "software": _fixstr(data[36:68]),
        "model_version": _fixstr(data[68:100]),
        "serial": _fixstr(data[100:132]),
        "len": data[133],
    }


# ── Adapter ──────────────────────────────────────────────────────────────────
# Alternator regulators (e.g. Wakespeed WS500) announce themselves with 127506 DC type 1 = Alternator.
CHARGER_STATE = {0: "not charging", 1: "bulk", 2: "absorption", 3: "overcharge", 4: "equalise", 5: "float",
                 6: "no float", 7: "constant VI", 8: "disabled", 9: "fault"}
DC_TYPE_ALTERNATOR = 1


def decode_127508(data: bytes) -> dict:
    """Battery Status (single frame): instance, voltage 0.01 V, current 0.1 A (signed), temperature 0.01 K."""
    inst = data[0]
    v, a, t = struct.unpack_from("<HhH", data, 1)
    return {"instance": inst, "voltage": None if v == 0xFFFF else round(v * 0.01, 2),
            "current": None if a == 0x7FFF else round(a * 0.1, 1),
            "temp": None if t == 0xFFFF else round(t * 0.01 - 273.15, 1)}


def decode_127507(data: bytes) -> dict:
    """Charger Status: charger instance, battery instance, operating state (low nibble), enabled (2 bits)."""
    en = data[3] & 0x03
    return {"instance": data[0], "battery": data[1], "state": CHARGER_STATE.get(data[2] & 0x0F),
            "enabled": None if en == 3 else bool(en)}


class N2kAdapter:
    device = "n2k"

    def __init__(self, state: State, switches: list[dict] | None = None, show_unlabeled: bool = True) -> None:
        self.state = state
        self.show_unlabeled = show_unlabeled     # False: only channels named in config (hides input indicators, keypads…)
        self._pgns: dict[int, dict] = {}
        self.fp = FastPacketAssembler()
        # bank instance → {"source": sa, "states": [...28], "seen": t}
        self.banks: dict[int, dict] = {}
        # (bank, channel 1-based) → {"name", "icon", "group"} from config
        self.labels = {}
        for sw in switches or []:
            key = (int(sw["bank"]), int(sw["channel"])) + ((int(sw["source"]),) if "source" in sw else ())
            self.labels[key] = sw
        self.commands: list[dict] = []  # recent 126208 seen on the bus (newest last)
        self.bank_listeners: list = []   # callables (source, instance, states, t) for every 127501
        self.loads: dict[tuple[int, int], dict] = {}   # (source, connection) → 127500 state/duty
        self.volts: dict[tuple[int, int], dict] = {}   # (source, connection) → 127751 volts/amps
        # source address → identity (from 60928 / 126996), last frame time, last heartbeat.
        # Populated passively: we never send ISO Requests (listen-only), so names and products
        # appear when a device powers up or another device (IPG100, DSM) requests them.
        self.devices: dict[int, dict] = {}
        # Alternator regulator(s): source → alternator DC instance (from 127506 DC type = alternator),
        # plus that source's latest 127508 readings per instance and 127507 charger state.
        self.alt_inst: dict[int, int] = {}
        self.dc_status: dict[tuple[int, int], dict] = {}
        self.charger: dict[tuple[int, int], dict] = {}

    def on_frame(self, msg: can.Message) -> None:
        if msg.is_error_frame or not msg.is_extended_id:
            return
        nid = parse_id(msg.arbitration_id)
        e = self._pgns.setdefault(nid.pgn, {"count": 0, "sources": set()})
        e["count"] += 1
        e["sources"].add(nid.source)
        e["seen"] = now = time.time()
        if nid.source < 254:  # 254 = cannot claim, 255 = global
            self.devices.setdefault(nid.source, {"source": nid.source, "first": now})["last"] = now

        data = bytes(msg.data)
        if nid.pgn in FAST_PACKET_PGNS:
            data = self.fp.feed(nid.source, nid.pgn, data, msg.timestamp or None)
            if data is None:
                return
        try:
            self._decode(nid, data)
        except (ValueError, KeyError) as ex:
            log.debug("n2k decode %s from %d failed: %s", nid.pgn, nid.source, ex)

    def _decode(self, nid: N2kId, data: bytes) -> None:
        if nid.pgn == 60928:
            ident = decode_60928(data)
            for sa, dev in list(self.devices.items()):  # same NAME moved to a new address
                if sa != nid.source and dev.get("name") == ident["name"]:
                    del self.devices[sa]
            self.devices.setdefault(nid.source, {"source": nid.source, "first": time.time(), "last": time.time()}).update(ident)
        elif nid.pgn == 126996:
            self.devices.setdefault(nid.source, {"source": nid.source, "first": time.time(), "last": time.time()}).update(decode_126996(data))
        elif nid.pgn == 126993:
            self.devices.setdefault(nid.source, {"source": nid.source, "first": time.time(), "last": time.time()})["heartbeat"] = time.time()
        elif nid.pgn == 127506 and len(data) >= 3:
            if data[2] == DC_TYPE_ALTERNATOR:
                self.alt_inst[nid.source] = data[1]
        elif nid.pgn == 127508 and len(data) >= 7:
            r = decode_127508(data)
            r["seen"] = time.time()
            self.dc_status[(nid.source, r["instance"])] = r
        elif nid.pgn == 127507 and len(data) >= 4:
            r = decode_127507(data)
            r["seen"] = time.time()
            self.charger[(nid.source, r["instance"])] = r
        elif nid.pgn == 127501:
            inst, states = decode_127501(data)
            # Key by (source, instance): several devices (e.g. Victron relays/alarms) may all use instance 0.
            now = time.time()
            self.banks[(nid.source, inst)] = {"source": nid.source, "instance": inst, "states": states, "seen": now}
            for cb in self.bank_listeners:
                try:
                    cb(nid.source, inst, states, now)
                except Exception:
                    log.exception("bank listener failed")
        elif nid.pgn == 127500 and len(data) >= 6:
            self.loads[(nid.source, data[1])] = {"state": data[2], "status": data[3], "duty": data[5],
                                                 "seen": time.time()}
        elif nid.pgn == 127751 and len(data) >= 7:
            v = int.from_bytes(data[2:4], "little")
            i = int.from_bytes(data[4:7], "little", signed=True)
            self.volts[(nid.source, data[1])] = {"volts": None if v == 0xFFFF else round(v * 0.1, 1),
                                                 "amps": None if i in (0x7FFFFF, -1) else round(i * 0.01, 2),
                                                 "seen": time.time()}
        elif nid.pgn == 126208:
            h = decode_126208_header(data)
            h.update({"source": nid.source, "dest": nid.destination, "t": round(time.time(), 1)})
            self.commands = (self.commands + [h])[-20:]
            log.info("n2k 126208 %s for PGN %d from %d → %d: %s",
                     h["function"], h["pgn"], nid.source, nid.destination, h["rest"])

    STALE_AFTER = 30.0  # seconds without a 127501 from a bank → its channels show as stale, not on/off

    def switch_list(self, now: float | None = None) -> list[dict]:
        """Every available channel of every bank; config labels name them and choose an icon and group."""
        now = time.time() if now is None else now
        out = []
        per_inst: dict[int, int] = {}
        for (_src, inst) in self.banks:
            per_inst[inst] = per_inst.get(inst, 0) + 1
        for (src, inst) in sorted(self.banks):
            b = self.banks[(src, inst)]
            stale = now - b["seen"] > self.STALE_AFTER
            shared = per_inst[inst] > 1                       # instance used by more than one source
            for ch, st in enumerate(b["states"], start=1):
                lab = self.labels.get((inst, ch)) if not shared else (self.labels.get((inst, ch, src)) or None)
                if lab is None and (st is None or not self.show_unlabeled):
                    continue  # unused or unconfigured channel
                sid = f"{inst}.{ch}" + (f"@{src}" if shared else "")
                out.append({
                    "id": sid,
                    "name": (lab or {}).get("name", f"Bank {inst} · Ch {ch}" + (f" (src {src})" if shared else "")),
                    "icon": (lab or {}).get("icon", "toggle"),
                    "group": (lab or {}).get("group", "Unassigned" if lab is None else "Switches"),
                    "on": st == "on" and not stale,
                    "state": "stale" if stale else (st or "n/a"),
                    "source": src,
                    "age": round(now - b["seen"], 1),
                    "bank": inst, "channel": ch,
                    "kind": (lab or {}).get("kind", "switch"),
                    "blocked": bool((lab or {}).get("blocked")),
                    "home": bool((lab or {}).get("home")),
                    # home = <n> places it at position n on the Home grid (home = true: after those, by channel)
                    "home_pos": (lab or {}).get("home") if type((lab or {}).get("home")) is int else None,
                    "requires_input": (lab or {}).get("requires_input"),
                    "rating_a": (lab or {}).get("rating_a"),
                    "level": (self.loads.get((src, ch - 1)) or {}).get("duty") if (lab or {}).get("kind") == "dimmer" else None,
                    "amps": (self.volts.get((src, ch - 1)) or {}).get("amps") if ch <= 16 else None,
                })
        return out

    OFFLINE_AFTER = 90.0  # s; NMEA 2000 heartbeats default to every 60 s

    def device_list(self, now: float | None = None) -> list[dict]:
        now = time.time() if now is None else now
        hints = self.state.get("n2k.names_from_cerbo") or {}     # Cerbo's VE.Can device list, by address
        out = []
        for sa, d in sorted(self.devices.items()):
            age = now - d["last"]
            h = hints.get(str(sa)) or {}
            if not d.get("model") and h.get("ModelName"):
                d = {**d, "model": h["ModelName"], "serial": d.get("serial") or str(h.get("Serial") or ""),
                     "software": d.get("software") or str(h.get("FirmwareVersion") or ""),
                     "manufacturer": d.get("manufacturer") or MANUFACTURERS.get(h.get("Manufacturer"), "")}
            out.append({
                "source": sa,
                "model": d.get("model") or "",
                "manufacturer": d.get("manufacturer") or "",
                "class": d.get("device_class") or "",
                "instance": d.get("device_instance"),
                "software": d.get("software") or "",
                "serial": d.get("serial") or "",
                "online": age <= self.OFFLINE_AFTER,
                "age": round(age, 1),
                "heartbeat": d.get("heartbeat") is not None,
            })
        return out

    ALT_STALE = 10.0  # s; the WS500 sends 127508 about once a second

    def alternator(self, now: float | None = None) -> dict | None:
        """Alternator readings from the first regulator seen (READ-ONLY, decoded from its broadcasts).

        The WS500 puts voltage + temperature on its alternator instance (127508, e.g. 49) but the
        current on instance 0, so the current is taken from any of that source's instances.
        NOTE: which current that is depends on where the regulator's shunt is wired (alternator output
        vs battery). It is used as alternator current; see INTEGRATION_PLAN.
        """
        now = time.time() if now is None else now
        for src, inst in sorted(self.alt_inst.items()):
            fresh = {i: r for (s, i), r in self.dc_status.items() if s == src and now - r["seen"] <= self.ALT_STALE}
            own = fresh.get(inst) or {}
            volts = own.get("voltage") or next((r["voltage"] for r in fresh.values() if r["voltage"] is not None), None)
            amps = own.get("current")
            if amps is None:
                amps = next((r["current"] for _, r in sorted(fresh.items()) if r["current"] is not None), None)
            ch = self.charger.get((src, inst))
            state = ch["state"] if ch and now - ch["seen"] <= self.ALT_STALE else None
            return {"source": src, "voltage": volts, "current": amps,
                    "power": None if volts is None or amps is None else round(volts * amps),
                    "state": state, "temp": own.get("temp"), "online": bool(fresh)}
        return None

    def summary(self) -> list[dict]:
        return [
            {"pgn": pgn, "name": PGN_NAMES.get(pgn, ""), "count": e["count"],
             "sources": sorted(e["sources"]), "age": round(time.time() - e["seen"], 1)}
            for pgn, e in sorted(self._pgns.items())
        ]

    async def run(self) -> None:
        while True:
            rows = self.summary()
            self.state.set("n2k.pgns", rows)
            self.state.set("n2k.sources", sorted({s for r in rows for s in r["sources"]}))
            self.state.set("n2k.commands", list(self.commands))
            self.state.set("n2k.devices", self.device_list())
            alt = self.alternator()
            if alt is not None:
                for k, v in alt.items():
                    self.state.set(f"alternator.{k}", v)
            if self.banks:
                self.state.set("switches.source", "n2k")
                self.state.set("switches.list", self.switch_list())
            await asyncio.sleep(1.0)
