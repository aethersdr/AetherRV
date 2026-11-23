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
