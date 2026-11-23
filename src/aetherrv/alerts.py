"""Alert center: evaluates rules against live state, debounces them, and keeps an event log.

Severity follows the status palette: warning < serious < critical. A rule must hold for its
`for_s` before it raises, and must be false for `clear_s` before it clears, so door-open fridge
blips and brief CAN hiccups don't page anyone. Acknowledging only silences an alert; it stays
active until its condition clears.

Events (raised / cleared / acked) go to SQLite (same DB as history) so the log survives restarts.
"""

from __future__ import annotations

import asyncio
import logging
import sqlite3
import time
from collections.abc import Callable, Iterable
from dataclasses import dataclass, field

from .state import State

log = logging.getLogger(__name__)

SEVERITY = {"warning": 1, "serious": 2, "critical": 3}


@dataclass
class Finding:
    """One condition a rule currently sees as true."""
    id: str
    severity: str
    title: str
    detail: str = ""
    source: str = ""


@dataclass
class Active:
    finding: Finding
    since: float
    rule: str = ""
    acked: bool = False
    false_since: float | None = None


@dataclass
class Rule:
    name: str
    check: Callable[[State, dict], Iterable[Finding]]
    for_s: float = 0.0
    clear_s: float = 5.0
    _pending: dict[str, float] = field(default_factory=dict)


# ── Rules ────────────────────────────────────────────────────────────────────
def _num(v):
    return v if isinstance(v, (int, float)) and not isinstance(v, bool) else None


def r_battery(s: State, c: dict):
    soc = _num(s.get("victron.battery.soc"))
    if soc is not None:
        if soc <= c["soc_critical"]:
            yield Finding("battery.soc", "critical", f"Battery critically low: {soc:.0f}%", "Reduce loads or start charging now.", "victron")
        elif soc <= c["soc_warning"]:
            yield Finding("battery.soc", "warning", f"Battery low: {soc:.0f}%", f"Below {c['soc_warning']:.0f}%.", "victron")
    t, cur = _num(s.get("victron.battery.temp")), _num(s.get("victron.battery.current"))
    if t is not None and cur is not None and cur > 1 and t <= c["lfp_charge_min_c"]:
        yield Finding("battery.cold_charge", "serious", f"Charging cold LiFePO4 ({t:.0f} °C)",
                      "Charging lithium near/below freezing can damage cells; check the BMS/DVCC limits.", "victron")


BMS_ALARM_NAMES = {
    "LowVoltage": "low voltage", "HighVoltage": "high voltage", "HighCellVoltage": "high cell voltage",
    "LowCellVoltage": "low cell voltage", "CellImbalance": "cell imbalance", "HighChargeCurrent": "high charge current",
    "HighDischargeCurrent": "high discharge current", "HighTemperature": "high temperature",
    "LowTemperature": "low temperature", "HighChargeTemperature": "high charge temperature",
    "LowChargeTemperature": "low charge temperature", "InternalFailure": "internal failure",
    "ChargeBlocked": "charge blocked", "DischargeBlocked": "discharge blocked",
}


def r_bms(s: State, c: dict):
    for b in s.get("victron.batteries") or []:
        if b.get("kind") != "bms":
            continue
        name = b.get("CustomName") or b.get("ProductName") or "Battery BMS"
        exp = c.get("bms_expected_modules")
        online = b.get("System/NrOfModulesOnline")
        if exp and isinstance(online, (int, float)) and online < exp:
            ble_ok = s.get("batteries.ble.reporting")
            if ble_ok is not None and ble_ok >= exp:
                # Bluetooth reads every battery directly and they're healthy: only the CAN comms link is missing.
                yield Finding(f"bms.modules.{b['instance']}", "warning", f"Only {int(online)} of {int(exp)} batteries on the CAN bus",
                              f"Bluetooth reads all {int(exp)} batteries directly and they are healthy, but {name} sees only "
                              f"{int(online)} on its CAN comms. Check the battery comms daisy-chain / DIP switches.", "victron")
            else:
                yield Finding(f"bms.modules.{b['instance']}", "serious", f"Only {int(online)} of {int(exp)} batteries reporting",
                              f"{name}: the BMS reports {int(online)} module(s) online, expected {int(exp)}. "
                              "Check the battery comms daisy-chain / DIP switches (and that every battery's breaker is closed).", "victron")
        for k, v in (b.get("active_alarms") or {}).items():
            sev = "serious" if v == 2 else "warning"
            yield Finding(f"bms.alarm.{b['instance']}.{k}", sev, f"{name}: {BMS_ALARM_NAMES.get(k, k)}",
                          f"BMS {'alarm' if v == 2 else 'warning'} ({k}).", "victron")
        for key, what in (("System/NrOfModulesBlockingCharge", "charging"), ("System/NrOfModulesBlockingDischarge", "discharging")):
            n = b.get(key)
            if isinstance(n, (int, float)) and n > 0:
                yield Finding(f"bms.block.{b['instance']}.{what}", "serious", f"{name}: {int(n)} module(s) blocking {what}",
                              "The BMS has disabled this direction on part of the bank.", "victron")


def r_cold(s: State, c: dict):
    for role, label in (("fridge", "Fridge"), ("freezer", "Freezer")):
        if s.get(f"settings.{role}_off"):
            continue                 # the owner marked it switched off (from the alert); re-armed once it's cold again
        t, mx = _num(s.get(f"climate.{role}.temp")), _num(s.get(f"ui.{role}_max_c"))
        if t is not None and mx is not None and t > mx:
            yield Finding(f"cold.{role}", "warning", f"{label} warm: {t:.1f} °C", f"Above {mx:.0f} °C.", "ruuvi")


def r_switch_trip(s: State, c: dict):
    """A load-controller channel latched in fault (its electronic breaker tripped). Reset = switch it off."""
    for w in s.get("switches.list") or []:
        if w.get("state") == "error" and w.get("channel"):
            yield Finding(f"switch.trip.{w.get('bank')}.{w['channel']}", "serious", f"Breaker tripped: {w.get('name')}",
                          f"The load controller shut off channel {w['channel']} (overcurrent or load fault) and holds it off. "
                          "Reset switches it off and clears the fault; check the load and its current limit before using it again.",
                          "n2k")


def r_trigger_failsafe(s: State, c: dict):
    """A load-controller timer that did not cut its output off (AetherRV's safety net had to)."""
    for g in s.get("switching.triggers") or []:
        r = s.get(f"switching.trigger.{g['id']}")
        if r and r.get("ok") is False:
            yield Finding(f"trigger.failsafe.{g['id']}", "serious", f"{g['name']}: controller timer failed",
                          r.get("detail", "") + " Check the load controller's timer configuration.", "n2k")


def r_epoch(s: State, c: dict):
    """Individual EPOCH batteries read over Bluetooth."""
    for b in s.get("batteries.ble") or []:
        nm, key = f"Battery {b.get('name')}", f"epoch.{b.get('name')}"
        if not b.get("ok"):
            if b.get("age") is not None or b.get("error"):
                yield Finding(f"{key}.silent", "warning", f"{nm} not answering over Bluetooth",
                              f"No good read for 10 min ({b.get('error') or 'no data'}).", "epoch")
            continue
        if b.get("problem_code"):
            yield Finding(f"{key}.fault", "serious", f"{nm}: BMS fault", f"Problem code {b['problem_code']}.", "epoch")
        for fet, what in (("charge_fet", "charging"), ("discharge_fet", "discharging")):
            if b.get(fet) is False:
                yield Finding(f"{key}.{fet}", "serious", f"{nm}: {what} switched off by its BMS",
                              f"The battery's BMS has opened its {what} MOSFET.", "epoch")
        if (b.get("cell_delta_mv") or 0) > 50:
            yield Finding(f"{key}.cells", "warning", f"{nm}: cells out of balance ({b['cell_delta_mv']} mV)",
                          "Cell voltage spread above 50 mV.", "epoch")
        temps = [t for t in b.get("temps") or [] if t is not None]
        if temps and max(temps) >= 50:
            yield Finding(f"{key}.hot", "serious", f"{nm}: hot ({max(temps):.0f} °C)", "A cell/BMS temperature is 50 °C or more.", "epoch")
        cell_t = [t for t in (b.get("temps") or [])[:2] if t is not None]     # T1/T2: the cell probes
        if cell_t and min(cell_t) < c.get("batt_cold_c", 0.0):
            t = min(cell_t)
            yield Finding(f"{key}.cold", "warning", f"{nm}: cells freezing ({t * 9 / 5 + 32:.0f} °F)",
                          "A cell probe is below 32 °F: LiFePO4 won't accept a charge. Check the battery heater.", "epoch")


CERBO_SEVERITY = {0: "warning", 1: "serious"}       # Venus notification Type: 0 warning, 1 alarm, 2 info


def r_cerbo_notifications(s: State, c: dict):
    """Mirror the Cerbo's active alarms/warnings (the ones that sound its buzzer)."""
    for n in s.get("victron.notifications") or []:
        sev = CERBO_SEVERITY.get(n.get("type"))
        if n.get("active") and sev:
            dev = f" ({n['device']})" if n.get("device") else ""
            yield Finding(f"cerbo.notif.{n['index']}", sev, f"Cerbo: {n.get('description') or 'alarm'}{dev}",
                          ("Acknowledged on the Cerbo." if n.get("acknowledged") else
                           "Unacknowledged: the Cerbo's buzzer sounds until it is acknowledged."), "victron")


def r_n2k_devices(s: State, c: dict):
    for d in s.get("n2k.devices") or []:
        if not d.get("online"):
            name = d.get("model") or f"device {d.get('source')}"
            yield Finding(f"n2k.offline.{d.get('source')}", "serious", f"{name} offline",
                          f"No NMEA 2000 traffic from address {d.get('source')} for {d.get('age', 0):.0f} s.", "n2k")


def r_buses(s: State, c: dict):
    names = {k.split(".")[1] for k in s.snapshot() if k.startswith("bus.")}
    for n in sorted(names):
        if n.startswith("bench"):
            continue
        if s.get(f"bus.{n}.connected") is False:
            yield Finding(f"bus.down.{n}", "serious", f"CAN bus {n} down", f"Interface {s.get(f'bus.{n}.channel')} is not up.", "can")
    for k in s.snapshot():
        if k.endswith(".path_backup") and s.get(k):
            dev = k[:-len(".path_backup")]
            yield Finding(f"path.backup.{dev}", "warning", f"{dev.upper()}: on the backup path ({s.get(f'{dev}.path')})",
                          "The Pi's own CAN connection to this bus went silent or failed to send; running through the backup. "
                          "Check the CAN wiring to the Pi.", "can")


def r_espar(s: State, c: dict):
    """Espar heater fault code from its own status frame (read-only decoder, adapters/espar.py)."""
    lock = s.get("climate.espar.diag.lockout")
    if lock:
        yield Finding("espar.lockout", "serious", f"Espar locked out ({lock['code']})", f"{lock['why']}. {lock['help']}", "espar")
    for d in s.get("climate.espar.diag.dtcs") or []:
        if d.get("active") and not d.get("lockout"):
            sev = "serious" if d.get("severity") in (4, 5, 6) else "warning"
            yield Finding(f"espar.dtc.{d['code']}", sev, f"Espar fault {d['code']}", d.get("text") or "", "espar")
    issue = s.get("climate.espar.issue")
    if issue:
        yield Finding("espar.issue", "warning", "Espar not heating", issue[0].upper() + issue[1:] + ".", "espar")
    code = s.get("climate.espar.fault")
    if code:
        sev = "serious" if s.get("climate.espar.state") == "error" else "warning"
        yield Finding(f"espar.fault.{code}", sev, f"Espar heater fault {code}",
                      f"{s.get('climate.espar.fault_text') or 'unknown fault code'}. Check the EasyStart Pro for the full code.",
                      "espar")


def r_victron(s: State, c: dict):
    if s.get("victron.connected") is False and not s.get("demo.active"):
        yield Finding("victron.link", "serious", "Cerbo GX not connected", "No MQTT connection to the Cerbo GX.", "victron")


def r_rixens(s: State, c: dict):
    if s.get("climate.rixens.state") == "no data" or s.get("climate.rixens.reachable") is False:
        yield Finding("rixens.link", "warning", "Rixens MCS7 not responding", "No data from the MCS7 controller.", "rixens")
    if s.get("climate.rixens.heater_state_code") == 200:
        yield Finding("rixens.error", "serious", "Furnace in error state", "The Eberspächer burner reports an error (state 200).", "rixens")
    af = s.get("climate.rixens.fault_active")
    if af:
        yield Finding("rixens.fault", "serious", f"Furnace fault code {af}", "Live fault reported by the MCS7 (AF). See the Eberspächer fault table.", "rixens")


def r_starlink(s: State, c: dict):
    if s.get("starlink.reachable") is None:
        return                                   # adapter not enabled
    if time.time() - (s.get("starlink.restart_requested") or 0) < 600:
        return                                   # owner-requested restart (update install): expected outage
    if s.get("starlink.reachable") is False:
        yield Finding("starlink.unreachable", "warning", "Starlink dish unreachable",
                      "No gRPC response from 192.168.100.1 (powered off, stowed, or not routed by the Peplink).", "starlink")
        return
    if not s.get("starlink.online"):
        yield Finding("starlink.offline", "warning", f"Starlink offline: {str(s.get('starlink.state')).replace('_', ' ').lower()}",
                      "The dish is not connected to the constellation.", "starlink")
    if s.get("starlink.snr_persistently_low"):
        yield Finding("starlink.snr_low", "warning", "Starlink: signal persistently weak",
                      f"The dish reports its SNR as persistently low (now {s.get('starlink.snr_db')} dB).", "starlink")
    for a in s.get("starlink.alerts") or []:
        if a.get("severity"):
            yield Finding(f"starlink.{a['id']}", a["severity"], f"Starlink: {a['label']}", "Reported by the dish.", "starlink")


def r_peplink(s: State, c: dict):
    if s.get("peplink.reachable") is None:
        return
    if s.get("peplink.reachable") is False:
        yield Finding("peplink.unreachable", "serious", "Router API unreachable", "No response from the Peplink Router API.", "peplink")
    for a in s.get("peplink.allowance") or []:
        pct = _num(a.get("percent"))
        if pct is None:
            continue
        sim = f" SIM {a['sim']}" if a.get("sim") else ""
        if pct >= c["data_serious_pct"]:
            yield Finding(f"peplink.data.{a['conn']}.{a.get('sim')}", "serious", f"Cellular data{sim} at {pct:.0f}%",
                          f"{a.get('usage_mb')} of {a.get('limit_mb')} MB used this cycle.", "peplink")
        elif pct >= c["data_warning_pct"]:
            yield Finding(f"peplink.data.{a['conn']}.{a.get('sim')}", "warning", f"Cellular data{sim} at {pct:.0f}%",
                          f"{a.get('usage_mb')} of {a.get('limit_mb')} MB used this cycle.", "peplink")


def _dur(sec) -> str:
    if sec is None:
        return ""
    return f"{sec // 3600} h {sec % 3600 // 60} min" if sec >= 3600 else f"{max(1, sec // 60)} min"


def r_netmon(s: State, c: dict):
    for t in s.get("netmon.targets") or []:
        if t.get("up") is False:
            what = "service not answering" if t.get("reason") == "service not answering" else "not responding"
            detail = f"{t['host']}{':' + str(t['tcp']) if t.get('tcp') else ''} {what} for {_dur(t.get('down_for'))}."
            if t.get("hint"):
                detail += " " + t["hint"]
            yield Finding(f"net.{t['name']}", t.get("severity") or "warning", f"{t['name']} {what}", detail, "network")


def r_system(s: State, c: dict):
    if s.get("history.degraded"):
        yield Finding("system.history", "warning", "Chart history not being saved",
                      f"History database unusable, running in memory: {s.get('history.degraded')}", "system")


def default_rules(cfg: dict | None = None) -> list[Rule]:
    sl = float((cfg or {}).get("starlink_offline_s", 180))
    return default_rules_base() + [
        Rule("starlink", r_starlink, for_s=sl, clear_s=30),
        Rule("peplink", r_peplink, for_s=60, clear_s=30),
        Rule("netmon", r_netmon, for_s=0, clear_s=0),     # NetMon already debounces (fail_after)
        Rule("system", r_system, for_s=0, clear_s=0),
        Rule("bms", r_bms, for_s=30, clear_s=30),
    ]


def default_rules_base() -> list[Rule]:
    return [
        Rule("battery", r_battery, for_s=30, clear_s=60),
        Rule("cold", r_cold, for_s=600, clear_s=120),    # 10 min: ride through door openings
        Rule("n2k", r_n2k_devices, for_s=0, clear_s=10),  # the 90 s offline threshold is already a debounce
        Rule("buses", r_buses, for_s=10, clear_s=10),
        Rule("victron", r_victron, for_s=60, clear_s=10),
        Rule("rixens", r_rixens, for_s=30, clear_s=10),
        Rule("switch_trip", r_switch_trip, for_s=3, clear_s=5),
        Rule("cerbo", r_cerbo_notifications, for_s=0, clear_s=5),
        Rule("trigger", r_trigger_failsafe, for_s=0, clear_s=0),
        Rule("epoch", r_epoch, for_s=60, clear_s=60),
        Rule("espar", r_espar, for_s=5, clear_s=30),
    ]


DEFAULTS = {"soc_warning": 20.0, "soc_critical": 10.0, "lfp_charge_min_c": 2.0, "batt_cold_c": 0.0,
            "data_warning_pct": 80.0, "data_serious_pct": 95.0, "starlink_offline_s": 180.0,
            "bms_expected_modules": 0}


class Alerts:
    def __init__(self, state: State, db: sqlite3.Connection | None = None, cfg: dict | None = None,
                 rules: list[Rule] | None = None) -> None:
        self.state = state
        self.cfg = {**DEFAULTS, **(cfg or {})}
        self.rules = rules if rules is not None else default_rules(self.cfg)
        self.active: dict[str, Active] = {}
        self.db = db
        if db is not None:
            db.execute("CREATE TABLE IF NOT EXISTS alert_log (t REAL, event TEXT, id TEXT, severity TEXT, title TEXT, detail TEXT)")

    def _log(self, event: str, f: Finding, now: float) -> None:
        log.log(logging.WARNING if event == "raised" else logging.INFO, "alert %s: [%s] %s", event, f.severity, f.title)
        if self.db is not None:
            try:
                self.db.execute("INSERT INTO alert_log VALUES (?,?,?,?,?,?)", (now, event, f.id, f.severity, f.title, f.detail))
            except sqlite3.Error:
                log.exception("alert log write failed")

    def evaluate(self, now: float | None = None) -> None:
        now = time.time() if now is None else now
        for rule in self.rules:
            try:
                findings = {f.id: f for f in rule.check(self.state, self.cfg)}
            except Exception:  # a broken rule must not take down the others
                log.exception("alert rule %s failed", rule.name)
                continue
            for fid, f in findings.items():
                a = self.active.get(fid)
                if a is not None:
                    escalated = SEVERITY[f.severity] > SEVERITY[a.finding.severity]
                    a.finding, a.false_since = f, None
                    if escalated:  # e.g. warning -> critical needs fresh attention
                        a.acked = False
                        self._log("escalated", f, now)
                    continue
                first = rule._pending.setdefault(fid, now)
                if now - first >= rule.for_s:
                    self.active[fid] = Active(f, now, rule=rule.name)
                    rule._pending.pop(fid, None)
                    self._log("raised", f, now)
            for pid in [p for p in rule._pending if p not in findings]:
                del rule._pending[pid]           # condition went away before it was due
            for aid, a in list(self.active.items()):
                if a.rule != rule.name or aid in findings:
                    continue
                a.false_since = a.false_since or now
                if now - a.false_since >= rule.clear_s:
                    del self.active[aid]
                    self._log("cleared", a.finding, now)

    def ack(self, aid: str, now: float | None = None) -> bool:
        a = self.active.get(aid)
        if a is None or a.acked:
            return False
        a.acked = True
        self._log("acked", a.finding, time.time() if now is None else now)
        return True

    def ack_all(self) -> int:
        return sum(self.ack(aid) for aid in list(self.active))

    def snapshot(self) -> list[dict]:
        rows = [{"id": a.finding.id, "severity": a.finding.severity, "title": a.finding.title,
                 "detail": a.finding.detail, "source": a.finding.source, "since": round(a.since, 1), "acked": a.acked}
                for a in self.active.values()]
        return sorted(rows, key=lambda r: (-SEVERITY[r["severity"]], r["since"]))

    def history(self, limit: int = 50) -> list[dict]:
        if self.db is None:
            return []
        rows = self.db.execute("SELECT t, event, id, severity, title FROM alert_log ORDER BY t DESC LIMIT ?", (limit,)).fetchall()
        return [{"t": round(t, 1), "event": e, "id": i, "severity": sv, "title": ti} for t, e, i, sv, ti in rows]

    async def run(self) -> None:
        tick = 0
        while True:
            self.evaluate()
            self.state.set("alerts.active", self.snapshot())
            if tick % 5 == 0:
                self.state.set("alerts.log", self.history(40))
            tick += 1
            await asyncio.sleep(1.0)
