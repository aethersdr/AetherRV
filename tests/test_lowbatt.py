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


def test_heat_lockout_turns_both_off_and_keeps_them_off_until_charging():
    s = State(); log = []
    s.update({"victron.battery.soc": 9.5, "victron.battery.current": -20.0,
              "climate.espar.ctl.active": True, "climate.espar.ctl.mode": "heat",
              "climate.rixens.live": True, "climate.rixens.state": "standby", "climate.rixens.sources": ["furnace"],
              "climate.rixens.hot_water": True, "climate.rixens.preheat": False})
    lb = LowBatteryShutdown(s, Admin(), {"heater_off_soc": 10, "hold_s": 120},
                            {"espar": Ctl("espar", log), "rixens": Ctl("mcs7", log)})
    run(lb, 0); run(lb, 60)
    assert log == [] and not s.get("power.heat_lockout")             # still inside the hold
    run(lb, 121)
    assert s.get("power.heat_lockout") is True
    assert log == [("espar", "set", {"mode": "off"}, "auto:low-battery"), ("mcs7", "furnace", False, "auto:low-battery"),
                   ("mcs7", "hot_water", False, "auto:low-battery")]
    ids = [f.id for f in r_power_shutdown(s, {})]
    assert "power.heat_lockout" in ids and "power.heaters_off" in ids
    run(lb, 150)
    assert len(log) == 3                                             # at most once a minute
    s.update({"climate.espar.ctl.mode": "off", "climate.rixens.sources": [], "climate.rixens.hot_water": False})
    run(lb, 300)
    assert len(log) == 3                                             # nothing heating: nothing sent
    s.update({"climate.espar.ctl.mode": "heat"})                     # someone got it going again: off again
    run(lb, 400)
    assert log[-1] == ("espar", "set", {"mode": "off"}, "auto:low-battery")
    s.set("victron.battery.current", 15.0)                           # charging lifts the lockout and clears alerts
    run(lb, 410)
    assert s.get("power.heat_lockout") is False and list(r_power_shutdown(s, {})) == []


def test_heat_starts_refused_during_lockout():
    from aetherrv.adapters.espar import EsparAdapter
    from aetherrv.adapters.espar_control import EsparControl
    from aetherrv.adapters.rixens_http import RixensHttpAdapter
    from aetherrv.txgate import TxGate
    s = State(); s.set("power.heat_lockout", True); s.set("climate.cabin_front.temp", 18.0)
    c = EsparControl(s, EsparAdapter("espar", s), object(), TxGate({"espar": True}))
    r = asyncio.run(c.command("set", {"mode": "heat", "setpoint_c": 20}))
    assert not r["ok"] and "locked out" in r["detail"]
    m = RixensHttpAdapter.__new__(RixensHttpAdapter)
    m.state, m._cmd_lock, m.db = s, asyncio.Lock(), None
    m._log = lambda *a: None
    r = asyncio.run(m.command("furnace", True))
    assert not r["ok"] and "locked out" in r["detail"]


def test_poweroff_waits_for_heater_after_run():
    s = State(); a = Admin()
    s.update({"victron.battery.soc": 4.0, "victron.battery.current": -5.0, "climate.espar.state": "cooldown"})
    lb = LowBatteryShutdown(s, a, {"hold_s": 10, "heater_wait_s": 600}, {})
    run(lb, 0); run(lb, 20)
    assert a.calls == [] and s.get("power.shutdown")["waiting_for_heaters"]
    s.set("climate.espar.state", "off")                              # after-run finished
    run(lb, 300)
    assert a.calls == ["poweroff"]


def test_poweroff_waits_at_most_heater_wait_s():
    s = State(); a = Admin()
    s.update({"victron.battery.soc": 4.0, "victron.battery.current": -5.0, "climate.rixens.heater_state_code": 30})
    lb = LowBatteryShutdown(s, a, {"hold_s": 10, "heater_wait_s": 300}, {})
    run(lb, 0); run(lb, 20); run(lb, 200)
    assert a.calls == []
    run(lb, 330)
    assert a.calls == ["poweroff"]


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
