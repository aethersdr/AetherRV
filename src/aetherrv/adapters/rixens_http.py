"""Rixens MCS7 adapter over the controller's Local HTTP API (Rixens "MCS Local API").

  GET /status.xml                     full snapshot (polled every `interval` s; never < 1 s apart)
  GET /interface.cgi?act=N&val=V      commands (integers only)

Encodings, confirmed from the MCS7's own web UI JavaScript and the live unit (fw HW1W6-1.208):
  temperatures are tenths of °C (currenttemp=72 → 7.2 °C); heater temps are hundredths of °C;
  humidity is tenths of %RH; battv is tenths of V; fanspeed is an integer or "Auto"/"Off".

CONTROL (owner-approved 2026-11-20): setpoint, fan speed, the three heat sources, and (approved later
the same day) hot water (constant heat, act 6) and engine pre-heat (act 7). The MCS7 web UI sends those
two exactly like the sources: act=N&val=<new state 0/1> (constantHeatDisable / preheatFunctionToggle).
Cabin heat on/off (thermostat, act 9, explicit 0/1) added 2026-11-22 at the owner's request.
Safety rules, all enforced in `command()`:
  * hard allowlist of act numbers + integer range checks; nothing else is reachable from code
    (no buttons.cgi / infra.cgi / floor / aux / override / Wi-Fi acts)
  * TxGate must allow device "rixens_http" (tx_enabled only in the Pi's /etc config)
  * fresh status read immediately before acting; send only if the state differs; send ONCE;
    then poll to verify; NEVER auto-retry (the doc calls the source acts "toggles", so a retry
    could flip the state back)
  * one lock + ≥1 s spacing shared by polls and commands
  * nothing is ever sent autonomously: the MCS7 stays the thermostat, a dead Pi changes nothing
Wi-Fi secrets in status.xml (key, ssid, infra_pass, infra_ssid, scan results) are never parsed.
"""

from __future__ import annotations

import asyncio
import logging
import sqlite3
import time
import xml.etree.ElementTree as ET

import aiohttp

from ..state import State
from ..txgate import TxBlocked, TxGate

log = logging.getLogger(__name__)

DEVICE = "rixens_http"

HEATER_STATE = {0: "off", 10: "cold start", 12: "warm start", 13: "cold blow", 15: "flame stabilizing",
                20: "running", 30: "after-run (shutdown)", 31: "after-run (standby)", 40: "pause", 200: "error"}
SRC_STATE = {0: "off", 1: "ready", 2: "active"}
# System fluid temperature indicator (the MCS7 touchscreen's "fluid temp" icon). Live value is the
# top-level <glycolstate> (doc says 0/1, but the unit reports the 0-3 scale): 0 cold, 1 warming,
# 2 ready (≥ 122 °F: cabin fan may run), 3 hot (≥ 158 °F: hot water ready).
# settings/glycol is the separate "Comfort hot tank" status (0 unless hot-water mode is used).
GLYCOL = {0: "cold", 1: "warming", 2: "ready", 3: "hot"}
SOURCE_BITS = {"engine": 1, "electric": 2, "furnace": 4}
SOURCE_ACT = {"engine": 3, "electric": 4, "furnace": 5}
# act 9 = thermostat (cabin "Heat" destination), explicit 0/1, verified on settings/thermenabled:
# owner-approved 2026-11-22 (make the Heat / Hot water / Pre-heat icons on the Climate card tappable)
MODE_ACT = {"hot_water": 6, "preheat": 7, "thermostat": 9}
SETPOINT_MIN_C, SETPOINT_MAX_C = 1.0, 33.0           # the MCS7 UI's own limits (°C mode)


class CommandError(Exception):
    pass


def _int(v):
    try:
        return int(str(v).strip())
    except (TypeError, ValueError):
        return None


def _scaled(v, div):
    n = _int(v)
    return None if n is None or n <= -999 else round(n / div, 2)


def parse_status(xml: bytes | str) -> dict:
    """status.xml → normalized dict. Whitelisted fields only (no Wi-Fi secrets)."""
    root = ET.fromstring(xml)
    t = lambda path: (root.findtext(path) or "").strip()
    s = lambda name: t(f"settings/{name}")
    fan_raw = s("fanspeed")
    fan = {"auto": "auto", "off": "off"}.get(fan_raw.lower()) if fan_raw else None
    if fan is None:
        n = _int(fan_raw)
        fan = "auto" if n == 999 else n
    hs = _int(s("heatsources")) or 0
    faults = {}
    for f in root.findall("heater1-faults/fault"):
        faults[(f.findtext("name") or "").strip()] = _int(f.findtext("value"))
    af = faults.pop("AF", None)
    hstate = _int(t("heaterstate"))
    return {
        "version": t("version"),
        "heat_version": t("heatversion"),
        "uptime": _int(t("uptime")),
        "system_heat": t("systemheat") == "1",
        "heater_state_code": hstate,
        "heater_state": HEATER_STATE.get(hstate, f"state {hstate}") if hstate is not None else None,
        "cabin_c": _scaled(t("currenttemp"), 10),
        "cabin_rh": _scaled(t("currenthumidity"), 10),
        "setpoint_c": _scaled(s("setpoint"), 10),
        "thermostat": s("thermenabled") == "1",
        "fan_enabled": s("fanenabled") == "1",
        "fan_running": s("fanstate") == "1",
        "fan_speed": fan,
        # In Auto the MCS7 runs the cabin fan from a PID loop (<autofan> fanset 0 = 999 → PID enabled);
        # heater1/pidspeed is that loop's live output on the same 0–100 % scale as manual fan speeds.
        "fan_auto_pct": _int(t("heater1/pidspeed")) if fan == "auto" else None,
        "pump_running": s("pumpstate") == "1",
        "sources": [k for k, bit in SOURCE_BITS.items() if hs & bit],
        "furnace": SRC_STATE.get(_int(s("furnacesrc"))),
        "electric": SRC_STATE.get(_int(s("electricsrc"))),
        "engine": SRC_STATE.get(_int(s("enginesrc"))),
        "floor": SRC_STATE.get(_int(s("floorsrc"))),
        "aux": SRC_STATE.get(_int(s("auxsrc"))),
        "hot_water": s("cnstheat") == "1",
        "hot_water_timer_s": (_int(s("cnstheattimerval")) if (_int(s("cnstheattimerval")) or -1) >= 0 else None),
        "preheat": s("preheatsrc") == "1",
        "glycol": GLYCOL.get(_int(t("glycolstate"))),
        "hot_tank": {0: "off", 1: "warming", 2: "ready", 3: "max"}.get(_int(s("glycol"))),
        "available": {"electric": s("electricenable") == "1", "engine": s("engineenable") == "1",
                      "preheat": s("preheatenable") == "1", "floor": s("floorenable") == "1",
                      "aux": s("auxenable") == "1"},
        "battery_v": _scaled(t("heater1/battv"), 10),
        "runtime_h": (round(_int(t("heater1/runtime")) / 60, 1) if _int(t("heater1/runtime")) else None),
        "flame_c": _scaled(t("heater1/flametemp"), 100),
        "inlet_c": _scaled(t("heater1/inlettemp"), 100),
        "outlet_c": _scaled(t("heater1/outlettemp"), 100),
        "altitude_m": (round(float(t("heater1/altitude")), 0) if t("heater1/altitude") else None),
        # "Fan-Glow-Fuel" exactly as the MCS7's own diagnostics page scales it (web UI JS):
        # burnermotor = rpm, glowpin ÷ 10 = W, dosingpump ÷ 10 = Hz.
        "fan_rpm": _int(t("heater1/burnermotor")),
        "glow_w": _scaled(t("heater1/glowpin"), 10),
        "fuel_hz": _scaled(t("heater1/dosingpump"), 10),
        "fault_active": af if af not in (None, 0, 255) else None,   # AF: live fault (255 seen as "none" in the doc example)
        "fault_history": [v for k, v in sorted(faults.items()) if v],
    }


def to_state(d: dict, cabin_from_rixens: bool) -> dict:
    out = {f"climate.rixens.{k}": v for k, v in d.items() if k not in ("available",)}
    out.update({
        "climate.rixens.available": d["available"],
        "climate.rixens.live": True,
        "climate.rixens.coolant": d["outlet_c"],
        "climate.rixens.burner": d["heater_state_code"] in (10, 12, 15, 20),
        "climate.rixens.state": ("heating" if d["heater_state_code"] in (10, 12, 15, 20)
                                 else "error" if d["heater_state_code"] == 200
                                 else "standby" if d["sources"] else "off"),
        "climate.cabin.setpoint": d["setpoint_c"],
        "climate.rixens.updated": round(time.time(), 1),
    })
    if cabin_from_rixens:
        out.update({"climate.cabin.temp": d["cabin_c"], "climate.cabin.humidity": d["cabin_rh"],
                    "climate.cabin.source": "rixens"})
    return out


class RixensHttp:
    """Serialized HTTP access: one lock, ≥ min_gap seconds between any two requests."""

    def __init__(self, host: str, min_gap: float = 1.0, scheme: str = "http") -> None:
        self.base = f"{scheme}://{host}"
        self.min_gap = min_gap
        self._lock = asyncio.Lock()
        self._last = 0.0
        self._session: aiohttp.ClientSession | None = None

    async def _get(self, path: str, params: dict | None = None) -> bytes:
        async with self._lock:
            wait = self.min_gap - (time.monotonic() - self._last)
            if wait > 0:
                await asyncio.sleep(wait)
            if self._session is None or self._session.closed:
                self._session = aiohttp.ClientSession(timeout=aiohttp.ClientTimeout(total=8))
            try:
                async with self._session.get(f"{self.base}{path}", params=params) as r:
                    r.raise_for_status()
                    return await r.read()
            finally:
                self._last = time.monotonic()

    async def status(self) -> dict:
        return parse_status(await self._get("/status.xml"))

    async def send(self, act: int, val: int) -> None:
        await self._get("/interface.cgi", {"act": str(int(act)), "val": str(int(val))})

    async def close(self) -> None:
        if self._session is not None:
            await self._session.close()


class RixensHttpAdapter:
    device = DEVICE

    # Fuel flow = dosing-pump Hz × volume per stroke. Calibrated 2026-11-20: during a cold-start warm-up
    # (full output) the pump held 5.5 Hz; Eberspächer Hydronic D5 SC full output = 5.0 kW = 0.62 l/h
    # (spec sheet) → 0.62 l/h ÷ (5.5 × 3600) = 0.0313 ml/stroke. Cross-check: the D5 W SC fuel test
    # (9.0 cm³ / 90 s nominal) implies ~3.2 Hz at that volume. Override with fuel_ml_per_pulse.
    DEFAULT_ML_PER_PULSE = 0.0313
    L_PER_GAL = 3.785411784

    def __init__(self, state: State, http: RixensHttp, gate: TxGate, db: sqlite3.Connection | None = None,
                 interval: float = 5.0, cabin_from_rixens: bool = False, stale_after: float = 15.0,
                 ml_per_pulse: float | None = None) -> None:
        self.state, self.http, self.gate, self.db = state, http, gate, db
        self.interval, self.cabin_from_rixens, self.stale_after = interval, cabin_from_rixens, stale_after
        self.last: dict | None = None
        self.last_ok = 0.0
        self.ml_per_pulse = float(ml_per_pulse or self.DEFAULT_ML_PER_PULSE)
        self._fuel_t: float | None = None
        self.fuel_day, self.fuel_today_l = time.strftime("%Y-%m-%d"), 0.0
        self._cmd_lock = asyncio.Lock()
        if db is not None:
            db.execute("CREATE TABLE IF NOT EXISTS command_log (t REAL, device TEXT, cmd TEXT, value TEXT, "
                       "outcome TEXT, detail TEXT, client TEXT)")
            db.execute("CREATE TABLE IF NOT EXISTS counters (key TEXT PRIMARY KEY, day TEXT, value REAL)")
            row = db.execute("SELECT day, value FROM counters WHERE key='rixens.fuel_l'").fetchone()
            if row and row[0] == self.fuel_day:
                self.fuel_today_l = float(row[1])

    # ── polling ──────────────────────────────────────────────────────────────
    def fuel_lph(self, hz) -> float | None:
        return None if hz is None else round(hz * self.ml_per_pulse * 3.6, 3)    # ml/s × 3600 / 1000

    def _account_fuel(self, d: dict, now: float) -> None:
        """Integrate fuel flow between polls into today's total (persisted, resets at local midnight)."""
        day = time.strftime("%Y-%m-%d", time.localtime(now))
        if day != self.fuel_day:
            self.fuel_day, self.fuel_today_l = day, 0.0
        lph = self.fuel_lph(d.get("fuel_hz")) or 0.0
        if self._fuel_t is not None and 0 < now - self._fuel_t < 120 and lph > 0:
            self.fuel_today_l += lph * (now - self._fuel_t) / 3600
            if self.db is not None:
                try:
                    self.db.execute("INSERT INTO counters VALUES ('rixens.fuel_l', ?, ?) ON CONFLICT(key) DO UPDATE "
                                    "SET day=excluded.day, value=excluded.value", (self.fuel_day, self.fuel_today_l))
                except sqlite3.Error:
                    log.exception("fuel counter write failed")
        self._fuel_t = now

    def _publish(self, d: dict) -> None:
        now = time.time()
        self.last, self.last_ok = d, now
        self._account_fuel(d, now)
        lph = self.fuel_lph(d.get("fuel_hz"))
        self.state.update(to_state(d, self.cabin_from_rixens))
        self.state.update({
            "climate.rixens.fuel_lph": lph,
            "climate.rixens.fuel_gph": None if lph is None else round(lph / self.L_PER_GAL, 4),
            "climate.rixens.fuel_today_l": round(self.fuel_today_l, 3),
            "climate.rixens.fuel_today_gal": round(self.fuel_today_l / self.L_PER_GAL, 4),
            "climate.rixens.fuel_ml_per_pulse": self.ml_per_pulse,
        })
        self.state.set("climate.rixens.reachable", True)

    async def poll_once(self) -> None:
        self._publish(await self.http.status())

    async def run(self) -> None:
        backoff = self.interval
        while True:
            try:
                await self.poll_once()
                backoff = self.interval
            except asyncio.CancelledError:
                raise
            except Exception as e:
                log.debug("rixens http poll failed: %s", e)
                self.state.set("climate.rixens.reachable", False)
                backoff = min(backoff * 2, 60.0)
            stale = time.time() - self.last_ok > self.stale_after
            self.state.set("climate.rixens.controls_ok", self.gate.is_enabled(DEVICE) and not stale)
            if stale and self.last is not None:
                self.state.set("climate.rixens.state", "no data")
            await asyncio.sleep(backoff)

    # ── commands ─────────────────────────────────────────────────────────────
    def _log(self, cmd: str, value, outcome: str, detail: str, client: str) -> None:
        log.warning("rixens command %s=%s from %s: %s %s", cmd, value, client, outcome, detail)
        if self.db is not None:
            try:
                self.db.execute("INSERT INTO command_log VALUES (?,?,?,?,?,?,?)",
                                (time.time(), DEVICE, cmd, str(value), outcome, detail, client))
            except sqlite3.Error:
                log.exception("command log write failed")

    @staticmethod
    def plan(cmd: str, value, cur: dict) -> tuple[int, int, callable] | None:
        """Validate a request against the allowlist → (act, val, verify(status)->bool), or None if no change needed."""
        if cmd == "setpoint":
            c = float(value)
            if not (SETPOINT_MIN_C <= c <= SETPOINT_MAX_C):
                raise CommandError(f"setpoint {c} °C outside {SETPOINT_MIN_C}–{SETPOINT_MAX_C} °C")
            val = round(c * 10)
            if cur.get("setpoint_c") is not None and round(cur["setpoint_c"] * 10) == val:
                return None
            return 1, val, lambda s: s.get("setpoint_c") is not None and round(s["setpoint_c"] * 10) == val
        if cmd == "fan":
            if value == "auto":
                if cur.get("fan_speed") == "auto":
                    return None
                return 2, 999, lambda s: s.get("fan_speed") == "auto"
            pct = int(value)
            if not 0 <= pct <= 100:
                raise CommandError("fan speed must be 0–100 or 'auto'")
            if cur.get("fan_speed") == pct:
                return None
            return 2, pct, lambda s: s.get("fan_speed") == pct
        if cmd in ("furnace", "electric", "engine"):
            on = bool(value)
            if not (cur.get("available") or {}).get(cmd, True) and cmd != "furnace":
                raise CommandError(f"{cmd} heat is not installed/enabled on this MCS7")
            if (cmd in cur.get("sources", [])) == on:
                return None
            return SOURCE_ACT[cmd], 1 if on else 0, lambda s: (cmd in s.get("sources", [])) == on
        if cmd in MODE_ACT:
            on = bool(value)
            if cmd == "preheat" and not (cur.get("available") or {}).get("preheat", True):
                raise CommandError("engine pre-heat is not enabled on this MCS7")
            if bool(cur.get(cmd)) == on:
                return None
            return MODE_ACT[cmd], 1 if on else 0, lambda s: bool(s.get(cmd)) == on
        raise CommandError(f"command {cmd!r} is not allowed")

    async def command(self, cmd: str, value, client: str = "kiosk", verify_timeout: float = 15.0) -> dict:
        async with self._cmd_lock:
            if cmd in ("furnace", "electric", "hot_water", "preheat") and bool(value) \
                    and self.state.get("power.heat_lockout"):          # lowbatt.py: protect the burner's after-run
                self._log(cmd, value, "rejected", "heat locked out: house battery low", client)
                return {"ok": False, "detail": "Heat is locked out: the house battery is too low. Charge first."}
            try:
                self.gate.check(DEVICE, f"{cmd}={value}")
            except TxBlocked as e:
                self._log(cmd, value, "blocked", str(e), client)
                return {"ok": False, "detail": "control is disabled for the MCS7"}
            try:
                cur = await self.http.status()              # fresh read, never the poll cache
                self._publish(cur)
                p = self.plan(cmd, value, cur)
            except CommandError as e:
                self._log(cmd, value, "rejected", str(e), client)
                return {"ok": False, "detail": str(e)}
            except Exception as e:
                self._log(cmd, value, "failed", f"status read: {e}", client)
                return {"ok": False, "detail": "MCS7 not reachable"}
            if p is None:
                self._log(cmd, value, "noop", "already in requested state", client)
                return {"ok": True, "detail": "already set"}
            act, val, verify = p
            try:
                await self.http.send(act, val)               # exactly once, never retried
            except Exception as e:
                self._log(cmd, value, "failed", f"send act={act} val={val}: {e}", client)
                return {"ok": False, "detail": "send failed (not retried)"}
            deadline = time.monotonic() + verify_timeout
            while time.monotonic() < deadline:
                await asyncio.sleep(1.5)
                try:
                    s = await self.http.status()
                    self._publish(s)
                    if verify(s):
                        self._log(cmd, value, "ok", f"act={act} val={val} verified", client)
                        return {"ok": True, "detail": "confirmed by the MCS7"}
                except Exception:
                    continue
            self._log(cmd, value, "unverified", f"act={act} val={val}: state did not change within {verify_timeout:.0f}s", client)
            return {"ok": False, "detail": "the MCS7 did not confirm the change (it may clamp or delay; not retried)"}
