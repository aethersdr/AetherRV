import asyncio

from aetherrv.alerts import r_power_shutdown
from aetherrv.lowbatt import LowBatteryShutdown
from aetherrv.state import State


class Admin:
    def __init__(self):
        self.calls = []

    async def call(self, verb, args=None):
        self.calls.append(verb); return {"powering_off": True}


def run(lb, now):
    asyncio.run(lb.tick(now))


def test_powers_off_after_hold_and_only_once():
    s = State(); s.update({"victron.battery.soc": 4.5, "victron.battery.current": -6.0})
    a = Admin(); lb = LowBatteryShutdown(s, a, {"shutdown_soc": 5, "hold_s": 120})
    run(lb, 1000); run(lb, 1060)
    assert a.calls == [] and s.get("power.shutdown")["in_s"] == 60
    assert [f.severity for f in r_power_shutdown(s, {})] == ["critical"]
    run(lb, 1120); run(lb, 1200)
    assert a.calls == ["poweroff"]


def test_charging_or_recovery_cancels_and_missing_soc_never_triggers():
    s = State(); s.update({"victron.battery.soc": 4.0, "victron.battery.current": -5.0})
    a = Admin(); lb = LowBatteryShutdown(s, a, {"hold_s": 60})
    run(lb, 0)
    s.set("victron.battery.current", 12.0)                 # solar came in
    run(lb, 70)
    assert a.calls == [] and lb.low_since is None and not s.get("power.shutdown")["warning"]
    s2 = State(); lb2 = LowBatteryShutdown(s2, a, {"hold_s": 0})
    run(lb2, 0); run(lb2, 10)
    assert a.calls == [] and s2.get("power.shutdown")["soc"] is None


def test_bluetooth_fallback_and_warning():
    s = State(); s.update({"batteries.ble": [{"ok": True, "soc": 7.0}, {"ok": True, "soc": 8.0}, {"ok": False, "soc": 1.0}],
                           "batteries.ble.current": -4.0})
    lb = LowBatteryShutdown(s, Admin(), {"warn_soc": 8, "shutdown_soc": 5})
    run(lb, 0)
    p = s.get("power.shutdown")
    assert p["soc"] == 7.5 and p["source"] == "batteries" and p["warning"] and p["in_s"] is None
    assert [f.severity for f in r_power_shutdown(s, {})] == ["serious"]


class Ctl:
    def __init__(self, name, log):
        self.name, self.log = name, log

    async def command(self, cmd, value, client="kiosk"):
        self.log.append((self.name, cmd, value, client)); return {"ok": True, "detail": "ok"}


def test_heaters_both_off_at_10_percent_once_then_rearm():
    s = State(); log = []
    s.update({"victron.battery.soc": 9.5, "victron.battery.current": -20.0,
              "climate.espar.ctl.active": True, "climate.espar.ctl.mode": "heat",
              "climate.rixens.live": True, "climate.rixens.state": "standby", "climate.rixens.sources": ["furnace"],
              "climate.rixens.hot_water": True, "climate.rixens.preheat": False})
    lb = LowBatteryShutdown(s, Admin(), {"heater_off_soc": 10, "hold_s": 120},
                            {"espar": Ctl("espar", log), "rixens": Ctl("mcs7", log)})
    run(lb, 0); run(lb, 60)
    assert log == []                                                     # still inside the hold
    run(lb, 121)
    assert log == [("espar", "set", {"mode": "off"}, "auto:low-battery"), ("mcs7", "furnace", False, "auto:low-battery"),
                   ("mcs7", "hot_water", False, "auto:low-battery")]
    assert [f.id for f in r_power_shutdown(s, {})] == ["power.heaters_off"]
    run(lb, 300)
    assert len(log) == 3                                                 # once per low episode
    s.set("victron.battery.current", 15.0)                               # charging re-arms and clears the alert
    run(lb, 310)
    assert lb.heaters_done is False and list(r_power_shutdown(s, {})) == []


def test_heaters_left_alone_when_not_heating_or_above_threshold():
    s = State(); log = []
    s.update({"victron.battery.soc": 9.0, "victron.battery.current": -3.0,
              "climate.espar.ctl.active": False, "climate.rixens.state": "standby", "climate.rixens.sources": ["furnace"]})
    lb = LowBatteryShutdown(s, Admin(), {"hold_s": 10}, {"espar": Ctl("espar", log), "rixens": Ctl("mcs7", log)})
    run(lb, 0); run(lb, 60)
    assert log == []                                                     # nothing heating: nothing sent
    s.update({"victron.battery.soc": 30.0, "climate.rixens.state": "heating"})
    run(lb, 70); run(lb, 200)
    assert log == []
