"""Victron Cerbo GX adapter over the GX's built-in MQTT broker. READ-ONLY.

UNTESTED: no Cerbo is reachable from the bench. On the GX, enable
Settings → Integrations (or Services) → MQTT; recent Venus OS versions use a "plaintext"
or "TLS" option for LAN access. Topic layout follows Victron's dbus-flashmq docs:
  N/<portal id>/<service>/<instance>/<path>  payload {"value": ...}
  R/<portal id>/keepalive                     a read request that keeps N/ topics publishing
Writes (W/...) are not implemented here. When they are, they must go through TxGate.

EPOCH batteries reach us through here: their BMS talks to the Cerbo's BMS-Can port, and
the Cerbo republishes them under battery/<instance>/... and system/0/Dc/Battery/....
"""

from __future__ import annotations

import asyncio
import json
import logging

import aiomqtt

from ..state import State
from ..txgate import TxBlocked, TxGate

log = logging.getLogger(__name__)

# (topic suffix after N/<portal>/) -> state key. Grow this list once a real Cerbo is visible.
# Paths are from Victron's dbus documentation and are UNVERIFIED against this van's Cerbo.
WATCH = {
    "system/0/Dc/Battery/Soc": "victron.battery.soc",
    "system/0/Dc/Battery/Voltage": "victron.battery.voltage",
    "system/0/Dc/Battery/Current": "victron.battery.current",
    "system/0/Dc/Battery/Power": "victron.battery.power",
    "system/0/Dc/Battery/TimeToGo": "victron.battery.ttg",
    "system/0/Dc/Battery/Temperature": "victron.battery.temp",
    "system/0/Dc/Pv/Power": "victron.pv.power",
    "system/0/Dc/System/Power": "victron.dc.load",
    "system/0/Dc/Alternator/Power": "victron.alternator.power",
    "system/0/Dc/Vebus/Power": "victron.vebus.dc_w",
    "system/0/Ac/Consumption/L1/Power": "victron.ac.load",
    "system/0/Ac/Grid/L1/Power": "victron.ac.shore",
    "system/0/SystemState/State": "victron.system.state",
}


class VictronAdapter:
    device = "victron"

    # Venus OS temperature service TemperatureType (dbus-adc / Ruuvi): UNVERIFIED against this Cerbo.
    TEMP_TYPE = {0: "battery", 1: "fridge", 2: "generic", 3: "room", 4: "outdoor", 5: "water heater", 6: "freezer"}

    def __init__(self, state: State, host: str, port: int = 1883,
                 cabin_temp: str | int | None = None, outside_temp: str | int | None = None,
                 fridge_temp: str | int | None = None, freezer_temp: str | int | None = None,
                 username: str | None = None, password: str | None = None, tls: bool = False) -> None:
        self.state = state
        self.host = host
        # Venus OS >= 3.80 pairing gives a token username/password; TLS (8883) uses the GX's
        # self-signed certificate, so verification is off (LAN only).
        self.username, self.password, self.tls = username or None, password or None, tls
        self.gate: TxGate | None = None      # set by main; control only if the gate enables "victron"
        self.db = None                       # command_log (shared SQLite connection)
        self._client: aiomqtt.Client | None = None
        self.vals: dict[str, object] = {}    # raw "<svc>/<inst>/<path>" -> value for control-relevant paths
        self._upd: dict[str, int] = {}       # per-path update counter (fresh-read detection)
        self._changed = asyncio.Condition()
        self._cmd_lock = asyncio.Lock()
        self.port = port
        self.portal: str | None = None
        self._tanks: dict[str, dict] = {}
        self._vecan: dict[str, dict] = {}
        self._batts: dict[str, dict] = {}
        self._batts_dirty = False
        self._temps: dict[str, dict] = {}
        # role -> (configured sensor: device instance or Custom name, fallback TemperatureType).
        # Names match case-insensitively. If a role isn't configured, the first sensor of its type is used.
        self.roles = {
            "cabin": (cabin_temp, "room"), "outside": (outside_temp, "outdoor"),
            "fridge": (fridge_temp, "fridge"), "freezer": (freezer_temp, "freezer"),
        }

    async def run(self) -> None:
        backoff = 2.0
        while True:
            try:
                tls_ctx = None
                if self.tls:
                    import ssl
                    tls_ctx = ssl.create_default_context()
                    tls_ctx.check_hostname = False
                    tls_ctx.verify_mode = ssl.CERT_NONE
                async with aiomqtt.Client(self.host, self.port, identifier="aetherrv", username=self.username,
                                          password=self.password, tls_context=tls_ctx) as c:
                    self._client = c
                    self.state.set("victron.connected", True)
                    backoff = 2.0
                    await c.subscribe("N/+/system/0/Serial")
                    ka = asyncio.create_task(self._keepalive(c))
                    fb = asyncio.create_task(self._flush_batteries())
                    try:
                        async for m in c.messages:
                            await self._on_message(c, str(m.topic), m.payload)
                    finally:
                        ka.cancel()
                        fb.cancel()
            except asyncio.CancelledError:
                raise
            except Exception as e:
                log.warning("victron mqtt %s:%s: %s", self.host, self.port, e)
            self._client = None
            self.state.set("victron.connected", False)
            await asyncio.sleep(backoff)
            backoff = min(backoff * 2, 60.0)

    BATT_FIELDS = (
        "ProductName", "CustomName", "Soc", "Soh", "Dc/0/Voltage", "Dc/0/Current", "Dc/0/Power", "Dc/0/Temperature",
        "ConsumedAmphours", "TimeToGo", "Capacity", "InstalledCapacity",
        "System/NrOfModulesOnline", "System/NrOfModulesOffline", "System/NrOfModulesBlockingCharge",
        "System/NrOfModulesBlockingDischarge", "System/MinCellVoltage", "System/MaxCellVoltage",
        "System/MinVoltageCellId", "System/MaxVoltageCellId", "System/MinCellTemperature", "System/MaxCellTemperature",
        "Info/MaxChargeCurrent", "Info/MaxChargeVoltage", "Info/MaxDischargeCurrent", "Info/BatteryLowVoltage",
    )

    def _on_battery(self, rest: str, value) -> None:
        """Collect every Cerbo battery service (EPOCH BMS, Lynx Shunt …); published at most 1/s by run()."""
        parts = rest.split("/", 2)
        if len(parts) != 3:
            return
        _, inst, path = parts
        b = self._batts.setdefault(inst, {"instance": inst, "alarms": {}})
        if path.startswith("Alarms/"):
            b["alarms"][path[7:]] = value
        elif path in self.BATT_FIELDS:
            b[path] = value
        else:
            return
        self._batts_dirty = True

    def batteries(self) -> list[dict]:
        out = []
        for inst, b in sorted(self._batts.items(), key=lambda kv: int(kv[0]) if kv[0].isdigit() else 0):
            d = dict(b)
            d["kind"] = "bms" if any(k.startswith(("System/", "Info/")) for k in b) else "monitor"
            d["active_alarms"] = {k: v for k, v in b["alarms"].items() if v}
            out.append(d)
        return out

    async def _flush_batteries(self) -> None:
        while True:
            if self._batts_dirty:
                self._batts_dirty = False
                bl = self.batteries()
                self.state.set("victron.batteries", bl)
                bms = next((b for b in bl if b["kind"] == "bms"), None)
                if bms:
                    self.state.set("victron.bms.modules_online", bms.get("System/NrOfModulesOnline"))
            await asyncio.sleep(1.0)

    def _on_vecan(self, rest: str, value) -> None:
        """The Cerbo's own NMEA 2000 device list (VE.Can): source address → model/maker/serial.
        Lets the N2K device table name devices immediately instead of waiting for their broadcasts."""
        parts = rest.split("/")
        if len(parts) != 5 or parts[2] != "Devices":
            return
        dev = self._vecan.setdefault(parts[3], {})
        dev[parts[4]] = value
        table = {}
        for d in self._vecan.values():
            nad = d.get("Nad")
            if isinstance(nad, int):
                table[str(nad)] = {k: d.get(k) for k in ("ModelName", "Manufacturer", "Serial", "FirmwareVersion", "CustomName")}
        self.state.set("n2k.names_from_cerbo", table)

    # Venus FluidType enum: 0 fuel, 1 fresh water, 2 waste water, 3 live well, 4 oil, 5 black water
    FLUID = {0: "fuel", 1: "fresh", 2: "grey", 3: "livewell", 4: "oil", 5: "black"}

    def _on_tank(self, rest: str, value) -> None:
        _, inst, field = rest.split("/", 2)
        t = self._tanks.setdefault(inst, {"name": f"Tank {inst}", "type": "fresh", "level": None})
        if field == "Level":
            t["level"] = value
        elif field == "FluidType":
            t["type"] = self.FLUID.get(value, "fresh")
        elif field == "CustomName" and value:
            t["name"] = value
        self.state.set("victron.tanks", [dict(v) for _, v in sorted(self._tanks.items())])

    def _pick(self, want, kind: str) -> dict | None:
        if want is not None:
            w = str(want).strip().lower()
        for inst, t in sorted(self._temps.items()):
            if want is not None and (inst == w or str(t.get("name", "")).strip().lower() == w):
                return t
        return next((t for _, t in sorted(self._temps.items()) if t.get("type") == kind), None)

    def _on_temp(self, rest: str, value) -> None:
        parts = rest.split("/", 2)
        if len(parts) != 3:
            return
        _, inst, field = parts
        t = self._temps.setdefault(inst, {"instance": inst, "name": f"Sensor {inst}", "type": "generic", "temp": None})
        if field == "Temperature":
            t["temp"] = value
        elif field == "TemperatureType":
            t["type"] = self.TEMP_TYPE.get(value, "generic")
        elif field == "CustomName" and value:
            t["name"] = value
        elif field == "Humidity":
            t["humidity"] = value
        self.state.set("victron.temps", [dict(v) for _, v in sorted(self._temps.items())])
        for role, (want, kind) in self.roles.items():
            if want is False:          # role owned by another source (e.g. cabin from the MCS7)
                continue
            t = self._pick(want, kind)
            if t:
                self.state.set(f"climate.{role}.temp", t.get("temp"))
                if t.get("humidity") is not None:
                    self.state.set(f"climate.{role}.humidity", t["humidity"])

    async def _keepalive(self, c: aiomqtt.Client) -> None:
        while True:
            if self.portal:
                await c.publish(f"R/{self.portal}/keepalive", b"")
            await asyncio.sleep(30)

    async def _on_message(self, c: aiomqtt.Client, topic: str, payload) -> None:
        parts = topic.split("/", 2)
        if len(parts) < 3 or parts[0] != "N":
            return
        _, portal, rest = parts
        try:
            value = json.loads(payload).get("value")
        except (ValueError, AttributeError, TypeError):
            return
        if rest == "system/0/Serial" and self.portal is None:
            self.portal = portal
            self.state.set("victron.portal", portal)
            log.info("victron portal id %s", portal)
            await c.subscribe(f"N/{portal}/system/0/#")
            await c.subscribe(f"N/{portal}/battery/#")
            await c.subscribe(f"N/{portal}/tank/+/Level")
            await c.subscribe(f"N/{portal}/tank/+/FluidType")
            await c.subscribe(f"N/{portal}/tank/+/CustomName")
            for f in ("Temperature", "TemperatureType", "CustomName", "Humidity"):
                await c.subscribe(f"N/{portal}/temperature/+/{f}")
            for path in CONTROL_PATHS:
                await c.subscribe(f"N/{portal}/{path}")
            for f in ("Nad", "ModelName", "Manufacturer", "Serial", "FirmwareVersion", "CustomName", "DeviceInstance"):
                await c.subscribe(f"N/{portal}/vecan/+/Devices/+/{f}")
            await c.publish(f"R/{portal}/keepalive", b"")
            return
        if rest.startswith("vecan/"):
            self._on_vecan(rest, value)
            return
        if rest.startswith("battery/"):
            self._on_battery(rest, value)
        if rest.split("/", 1)[0] in ("vebus", "solarcharger"):
            await self._on_control_path(rest, value)
            return
        if rest.startswith("tank/"):
            self._on_tank(rest, value)
            return
        if rest.startswith("temperature/"):
            self._on_temp(rest, value)
            return
        key = WATCH.get(rest)
        if key:
            self.state.set(key, value)

    # ── Control (owner-approved 2026-11-20): MultiPlus mode, shore current limit, MPPT on/off ──────
    async def _on_control_path(self, rest: str, value) -> None:
        self.vals[rest] = value
        self._upd[rest] = self._upd.get(rest, 0) + 1
        self._publish_control_state()
        async with self._changed:
            self._changed.notify_all()

    def _inst(self, svc: str) -> str | None:
        insts = sorted({k.split("/")[1] for k in self.vals if k.startswith(svc + "/")}, key=lambda x: int(x) if x.isdigit() else 0)
        return insts[0] if insts else None

    def _publish_control_state(self) -> None:
        vb, mp = self._inst("vebus"), self._inst("solarcharger")
        g = lambda svc, inst, path: self.vals.get(f"{svc}/{inst}/{path}") if inst else None
        self.state.update({
            "victron.vebus.instance": vb,
            "victron.vebus.name": g("vebus", vb, "CustomName") or g("vebus", vb, "ProductName"),
            "victron.vebus.product": g("vebus", vb, "ProductName"),
            "victron.vebus.mode": g("vebus", vb, "Mode"),
            "victron.vebus.mode_adjustable": g("vebus", vb, "ModeIsAdjustable") == 1,
            "victron.vebus.state": g("vebus", vb, "State"),
            "victron.vebus.connected": g("vebus", vb, "Connected") == 1,
            "victron.vebus.current_limit": g("vebus", vb, "Ac/In/1/CurrentLimit"),
            "victron.vebus.limit_adjustable": g("vebus", vb, "Ac/In/1/CurrentLimitIsAdjustable") == 1,
            "victron.vebus.ac_out_w": g("vebus", vb, "Ac/Out/L1/P"),
            "victron.vebus.ac_in_w": g("vebus", vb, "Ac/ActiveIn/L1/P"),
            "victron.vebus.shore_connected": g("vebus", vb, "Ac/ActiveIn/Connected") == 1,
            "victron.mppt.instance": mp,
            "victron.mppt.name": g("solarcharger", mp, "CustomName") or g("solarcharger", mp, "ProductName"),
            "victron.mppt.mode": g("solarcharger", mp, "Mode"),
            "victron.mppt.state": g("solarcharger", mp, "State"),
            "victron.controls_ok": bool(self.gate and self.gate.is_enabled("victron") and self._client is not None),
        })

    def plan(self, cmd: str, value) -> tuple[str, object]:
        """Validate against the allowlist + the Cerbo's own 'IsAdjustable' flags → (path, value)."""
        if cmd == "inverter_mode":
            v = int(value)
            if v not in VEBUS_MODES:
                raise ValueError(f"MultiPlus mode must be one of {sorted(VEBUS_MODES)}")
            vb = self._inst("vebus")
            if vb is None:
                raise ValueError("no MultiPlus (vebus) found on the Cerbo")
            if self.vals.get(f"vebus/{vb}/ModeIsAdjustable") != 1:
                raise ValueError("the Cerbo reports the MultiPlus mode is not adjustable (remote panel/switch?)")
            return f"vebus/{vb}/Mode", v
        if cmd == "shore_limit":
            a = round(float(value), 1)
            if not SHORE_MIN_A <= a <= SHORE_MAX_A:
                raise ValueError(f"shore current limit must be {SHORE_MIN_A}-{SHORE_MAX_A} A")
            vb = self._inst("vebus")
            if vb is None or self.vals.get(f"vebus/{vb}/Ac/In/1/CurrentLimitIsAdjustable") != 1:
                raise ValueError("the Cerbo reports the shore current limit is not adjustable right now")
            return f"vebus/{vb}/Ac/In/1/CurrentLimit", a
        if cmd == "mppt_mode":
            v = int(value)
            if v not in (1, 4):
                raise ValueError("MPPT mode must be 1 (on) or 4 (off)")
            mp = self._inst("solarcharger")
            if mp is None:
                raise ValueError("no solar charger found on the Cerbo")
            return f"solarcharger/{mp}/Mode", v
        raise ValueError(f"command {cmd!r} is not allowed")

    @staticmethod
    def _same(a, b) -> bool:
        if isinstance(a, (int, float)) and isinstance(b, (int, float)):
            return abs(float(a) - float(b)) < 0.05
        return a == b

    async def _wait(self, pred, timeout: float) -> bool:
        async with self._changed:
            try:
                await asyncio.wait_for(self._changed.wait_for(pred), timeout)
                return True
            except asyncio.TimeoutError:
                return False

    def _log(self, cmd, value, outcome, detail, client) -> None:
        log.warning("victron command %s=%s from %s: %s %s", cmd, value, client, outcome, detail)
        if self.db is not None:
            try:
                self.db.execute("CREATE TABLE IF NOT EXISTS command_log (t REAL, device TEXT, cmd TEXT, value TEXT, "
                                "outcome TEXT, detail TEXT, client TEXT)")
                import time as _t
                self.db.execute("INSERT INTO command_log VALUES (?,?,?,?,?,?,?)",
                                (_t.time(), "victron", cmd, str(value), outcome, detail, client))
            except Exception:
                log.exception("command log write failed")

    async def command(self, cmd: str, value, client: str = "kiosk", verify_timeout: float = 12.0) -> dict:
        async with self._cmd_lock:
            try:
                if self.gate is None:
                    raise TxBlocked("no gate")
                self.gate.check("victron", f"{cmd}={value}")
            except TxBlocked:
                self._log(cmd, value, "blocked", "tx_enabled false", client)
                return {"ok": False, "detail": "Victron control is disabled"}
            c, portal = self._client, self.portal
            if c is None or portal is None:
                return {"ok": False, "detail": "not connected to the Cerbo"}
            try:
                path, target = self.plan(cmd, value)
            except (ValueError, TypeError) as e:
                self._log(cmd, value, "rejected", str(e), client)
                return {"ok": False, "detail": str(e)}
            # fresh read: ask the Cerbo to republish this path, then compare
            n0 = self._upd.get(path, 0)
            await c.publish(f"R/{portal}/{path}", b"")
            if not await self._wait(lambda: self._upd.get(path, 0) > n0, 4.0):
                self._log(cmd, value, "failed", f"no fresh value for {path}", client)
                return {"ok": False, "detail": "the Cerbo did not report the current value"}
            if self._same(self.vals[path], target):
                self._log(cmd, value, "noop", "already in requested state", client)
                return {"ok": True, "detail": "already set"}
            await c.publish(f"W/{portal}/{path}", json.dumps({"value": target}))   # exactly once, never retried
            if await self._wait(lambda: self._same(self.vals.get(path), target), verify_timeout):
                self._log(cmd, value, "ok", f"{path}={target} verified", client)
                return {"ok": True, "detail": "confirmed by the Cerbo"}
            self._log(cmd, value, "unverified", f"{path} still {self.vals.get(path)!r}", client)
            return {"ok": False, "detail": f"the Cerbo did not confirm (now {self.vals.get(path)!r}; not retried)"}


VEBUS_MODES = {1: "charger only", 2: "inverter only", 3: "on", 4: "off"}
SHORE_MIN_A, SHORE_MAX_A = 3.0, 50.0       # MultiPlus 12/3000/120-50: 50 A transfer switch
CONTROL_PATHS = [
    "vebus/+/Mode", "vebus/+/ModeIsAdjustable", "vebus/+/State", "vebus/+/Connected", "vebus/+/ProductName",
    "vebus/+/CustomName", "vebus/+/Ac/In/1/CurrentLimit", "vebus/+/Ac/In/1/CurrentLimitIsAdjustable",
    "vebus/+/Ac/Out/L1/P", "vebus/+/Ac/ActiveIn/Connected", "vebus/+/Ac/ActiveIn/L1/P",
    "solarcharger/+/Mode", "solarcharger/+/State", "solarcharger/+/ProductName", "solarcharger/+/CustomName",
]
