from aetherrv.alerts import Alerts, Rule, default_rules, r_battery, r_cold, r_n2k_devices
from aetherrv.state import State


def mk(rules=None, **state):
    s = State()
    for k, v in state.items():
        s.set(k.replace("__", "."), v)
    return s, Alerts(s, rules=rules)


def test_debounce_raise_and_clear():
    s, al = mk([Rule("battery", r_battery, for_s=30, clear_s=60)], victron__battery__soc=15.0)
    al.evaluate(now=0)
    assert al.snapshot() == []                       # not yet: needs 30 s
    al.evaluate(now=31)
    (a,) = al.snapshot()
    assert a["id"] == "battery.soc" and a["severity"] == "warning"
    s.set("victron.battery.soc", 50.0)
    al.evaluate(now=40)
    assert len(al.snapshot()) == 1                   # still held during clear_s
    al.evaluate(now=101)
    assert al.snapshot() == []


def test_blip_shorter_than_for_s_never_raises():
    s, al = mk([Rule("cold", r_cold, for_s=600, clear_s=120)], climate__fridge__temp=7.0, ui__fridge_max_c=5.0)
    al.evaluate(now=0)
    s.set("climate.fridge.temp", 4.0)                # door closed again
    al.evaluate(now=300)
    s.set("climate.fridge.temp", 7.0)
    al.evaluate(now=400)
    al.evaluate(now=900)                             # only 500 s since it went warm again
    assert al.snapshot() == []
    al.evaluate(now=1001)
    assert al.snapshot()[0]["id"] == "cold.fridge"


def test_escalation_unacks_and_ack():
    s, al = mk([Rule("battery", r_battery, for_s=0, clear_s=0)], victron__battery__soc=18.0)
    al.evaluate(now=0)
    assert al.ack("battery.soc") and al.snapshot()[0]["acked"]
    s.set("victron.battery.soc", 8.0)
    al.evaluate(now=1)
    a = al.snapshot()[0]
    assert a["severity"] == "critical" and a["acked"] is False


def test_per_device_offline_alerts_and_sorting():
    s, al = mk([Rule("n2k", r_n2k_devices, for_s=0, clear_s=0), Rule("battery", r_battery, for_s=0, clear_s=0)],
               victron__battery__soc=5.0)
    s.set("n2k.devices", [{"source": 16, "model": "IPG100", "online": False, "age": 120},
                          {"source": 66, "model": "DCR100", "online": True, "age": 1}])
    al.evaluate(now=0)
    assert [a["id"] for a in al.snapshot()] == ["battery.soc", "n2k.offline.16"]   # critical first
    s.set("n2k.devices", [{"source": 16, "model": "IPG100", "online": True, "age": 1}])
    al.evaluate(now=1)
    assert [a["id"] for a in al.snapshot()] == ["battery.soc"]


def test_broken_rule_is_isolated():
    def boom(s, c):
        raise RuntimeError("x")
        yield
    s, al = mk([Rule("boom", boom), Rule("battery", r_battery, for_s=0)], victron__battery__soc=5.0)
    al.evaluate(now=0)
    assert al.snapshot()[0]["id"] == "battery.soc"


def test_log_persists(tmp_path):
    import sqlite3
    db = sqlite3.connect(tmp_path / "a.sqlite3", isolation_level=None)
    s = State()
    s.set("victron.battery.soc", 5.0)
    al = Alerts(s, db=db, rules=[Rule("battery", r_battery, for_s=0, clear_s=0)])
    al.evaluate(now=0)
    al.ack("battery.soc", now=1)
    s.set("victron.battery.soc", 90.0)
    al.evaluate(now=2)
    assert [h["event"] for h in al.history()] == ["cleared", "acked", "raised"]


def test_default_rules_cover_all_sources():
    assert {r.name for r in default_rules()} == {"battery", "cold", "n2k", "buses", "victron", "rixens", "starlink", "peplink", "netmon", "system", "bms", "switch_trip", "cerbo", "trigger", "epoch"}


def test_connectivity_rules():
    from aetherrv.alerts import r_peplink, r_starlink, DEFAULTS
    s = State()
    assert list(r_starlink(s, DEFAULTS)) == [] and list(r_peplink(s, DEFAULTS)) == []   # not enabled → silent
    s.set("starlink.reachable", True); s.set("starlink.online", False); s.set("starlink.state", "OBSTRUCTED")
    s.set("starlink.alerts", [{"id": "alert_motors_stuck", "label": "Motors stuck", "severity": "serious"},
                              {"id": "alert_is_heating", "label": "Is heating", "severity": None}])
    ids = {f.id: f.severity for f in r_starlink(s, DEFAULTS)}
    assert ids == {"starlink.offline": "warning", "starlink.alert_motors_stuck": "serious"}
    s.set("peplink.reachable", True)
    s.set("peplink.allowance", [{"conn": 2, "sim": 1, "percent": 96, "usage_mb": 48000, "limit_mb": 50000},
                                {"conn": 2, "sim": 2, "percent": 81, "usage_mb": 1, "limit_mb": 1}])
    assert [f.severity for f in r_peplink(s, DEFAULTS)] == ["serious", "warning"]



def test_bms_rules():
    from aetherrv.alerts import r_bms
    s = State()
    s.set("victron.batteries", [
        {"instance": "0", "kind": "monitor", "ProductName": "Lynx Shunt", "active_alarms": {}},
        {"instance": "512", "kind": "bms", "CustomName": "Epoch 460", "System/NrOfModulesOnline": 1,
         "System/NrOfModulesBlockingCharge": 0, "System/NrOfModulesBlockingDischarge": 1,
         "active_alarms": {"CellImbalance": 1, "HighTemperature": 2}}])
    f = {x.id: (x.severity, x.title) for x in r_bms(s, {"bms_expected_modules": 4})}
    assert f["bms.modules.512"] == ("serious", "Only 1 of 4 batteries reporting")
    assert f["bms.alarm.512.CellImbalance"][0] == "warning" and f["bms.alarm.512.HighTemperature"][0] == "serious"
    assert f["bms.block.512.discharging"][0] == "serious" and "bms.block.512.charging" not in f
    assert "bms.modules.512" not in {x.id for x in r_bms(s, {"bms_expected_modules": 0})}


def test_switch_trip_and_cerbo_notification_alerts():
    from aetherrv.alerts import r_cerbo_notifications, r_switch_trip
    s = State()
    s.set("switches.list", [{"bank": 32, "channel": 4, "name": "Reverse lights", "state": "error"},
                            {"bank": 32, "channel": 1, "name": "Spot", "state": "on"}])
    assert [(f.id, f.severity) for f in r_switch_trip(s, {})] == [("switch.trip.32.4", "serious")]
    s.set("victron.notifications", [
        {"index": 0, "active": True, "type": 1, "description": "Breaker Tripped", "device": "CLMD16", "acknowledged": False},
        {"index": 1, "active": True, "type": 2, "description": "info only", "device": "", "acknowledged": False}])
    f = list(r_cerbo_notifications(s, {}))
    assert [(x.id, x.severity, x.title) for x in f] == [("cerbo.notif.0", "serious", "Cerbo: Breaker Tripped (CLMD16)")]


def test_epoch_alerts_and_bms_count_downgrade():
    from aetherrv.alerts import r_bms, r_epoch
    s = State()
    s.set("batteries.ble", [
        {"name": "296", "ok": True, "problem_code": 0, "charge_fet": True, "discharge_fet": True, "cell_delta_mv": 4, "temps": [12]},
        {"name": "337", "ok": True, "problem_code": 0, "charge_fet": False, "discharge_fet": True, "cell_delta_mv": 80, "temps": [12]},
        {"name": "354", "ok": False, "age": 900, "error": "TimeoutError"}])
    ids = {f.id: f.severity for f in r_epoch(s, {})}
    assert ids == {"epoch.337.charge_fet": "serious", "epoch.337.cells": "warning", "epoch.354.silent": "warning"}
    s.set("victron.batteries", [{"kind": "bms", "instance": 512, "System/NrOfModulesOnline": 1}])
    s.set("batteries.ble.reporting", 4)
    f = list(r_bms(s, {"bms_expected_modules": 4}))
    assert f[0].severity == "warning" and "CAN" in f[0].title
