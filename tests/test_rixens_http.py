import re
import time
from pathlib import Path

import pytest
from aiohttp import web

from aetherrv.adapters.rixens_http import (CommandError, RixensHttp, RixensHttpAdapter, parse_status, to_state)
from aetherrv.state import State
from aetherrv.txgate import TxGate

FIXTURE = (Path(__file__).parent / "fixtures" / "mcs7_status_hw1_1.208.xml").read_text()


def test_parse_live_fixture():
    d = parse_status(FIXTURE)
    assert d["cabin_c"] == 7.2 and d["cabin_rh"] == 46.1 and d["setpoint_c"] == 7.2   # owner: van ~46 °F
    assert d["fan_speed"] == "auto" and d["sources"] == ["furnace"] and d["furnace"] == "ready"
    raw = lambda tag: int(re.search(rf"<{tag}>(-?\d+)</{tag}>", FIXTURE).group(1))
    assert d["battery_v"] == raw("battv") / 10 and d["inlet_c"] == raw("inlettemp") / 100
    assert d["outlet_c"] == raw("outlettemp") / 100 and d["heater_state"] == "off"
    assert d["runtime_h"] == round(raw("runtime") / 60, 1)
    assert d["fault_active"] is None and d["fault_history"] == []
    assert d["available"]["electric"] and not d["available"]["aux"]


def test_no_secrets_reach_state():
    blob = repr(to_state(parse_status(FIXTURE), True))
    assert "REDACTED" not in blob and "key" not in parse_status(FIXTURE)


def test_sentinels_and_fan_variants():
    x = FIXTURE.replace("<currenttemp>72</currenttemp>", "<currenttemp>-9990</currenttemp>")
    x = x.replace("<fanspeed>Auto</fanspeed>", "<fanspeed>40</fanspeed>")
    d = parse_status(x)
    assert d["cabin_c"] is None and d["fan_speed"] == 40
    assert parse_status(FIXTURE.replace("<fanspeed>Auto</fanspeed>", "<fanspeed>Off</fanspeed>"))["fan_speed"] == "off"


def test_cabin_source_switch():
    d = parse_status(FIXTURE)
    assert to_state(d, True)["climate.cabin.temp"] == 7.2
    assert "climate.cabin.temp" not in to_state(d, False)


def test_plan_allowlist_and_ranges():
    cur = parse_status(FIXTURE)
    assert RixensHttpAdapter.plan("setpoint", 7.2, cur) is None                 # no change → no send
    act, val, verify = RixensHttpAdapter.plan("setpoint", 20.0, cur)
    assert (act, val) == (1, 200)
    assert RixensHttpAdapter.plan("fan", "auto", cur) is None
    assert RixensHttpAdapter.plan("fan", 40, cur)[:2] == (2, 40)
    assert RixensHttpAdapter.plan("electric", True, cur)[:2] == (4, 1)
    assert RixensHttpAdapter.plan("furnace", True, cur) is None                  # already selected
    assert RixensHttpAdapter.plan("furnace", False, cur)[:2] == (5, 0)
    for bad in [("setpoint", 40.0), ("setpoint", 0.0), ("fan", 150), ("floor", True), ("prime", 1),
                ("aux", True), ("wifi", "x")]:
        with pytest.raises(CommandError):
            RixensHttpAdapter.plan(*bad, cur)


# ── fake MCS7 ────────────────────────────────────────────────────────────────
class FakeMcs7:
    """Serves the fixture; applies act=1/2/3/4/5 with either 'set' or 'toggle' semantics."""

    def __init__(self, semantics="set", apply=True):
        self.xml, self.semantics, self.apply, self.sent = FIXTURE, semantics, apply, []

    def _hs(self):
        return int(re.search(r"<heatsources>(\d+)</heatsources>", self.xml).group(1))

    async def status(self, req):
        return web.Response(body=self.xml.encode(), content_type="text/xml")

    async def iface(self, req):
        act, val = int(req.query["act"]), int(req.query["val"])
        self.sent.append((act, val))
        if self.apply:
            if act == 1:
                self.xml = re.sub(r"<setpoint>\d+</setpoint>", f"<setpoint>{val}</setpoint>", self.xml)
            elif act == 2:
                self.xml = re.sub(r"<fanspeed>[^<]*</fanspeed>", f"<fanspeed>{'Auto' if val == 999 else val}</fanspeed>", self.xml)
            elif act in (3, 4, 5):
                bit = {3: 1, 4: 2, 5: 4}[act]
                hs = self._hs()
                hs = (hs ^ bit) if self.semantics == "toggle" else ((hs | bit) if val else (hs & ~bit))
                self.xml = re.sub(r"<heatsources>\d+</heatsources>", f"<heatsources>{hs}</heatsources>", self.xml)
        return web.Response(text="OK")


async def make(aiohttp_server, fake, enabled=True):
    app = web.Application()
    app.router.add_get("/status.xml", fake.status)
    app.router.add_get("/interface.cgi", fake.iface)
    srv = await aiohttp_server(app)
    http = RixensHttp(f"{srv.host}:{srv.port}", min_gap=0.01)
    return RixensHttpAdapter(State(), http, TxGate({"rixens_http": enabled})), http


@pytest.mark.parametrize("semantics", ["set", "toggle"])
async def test_heat_source_sends_once_and_verifies(aiohttp_server, semantics, monkeypatch):
    import aetherrv.adapters.rixens_http as rh
    monkeypatch.setattr(rh.asyncio, "sleep", _fast_sleep)
    fake = FakeMcs7(semantics)
    a, http = await make(aiohttp_server, fake)
    r = await a.command("electric", True)
    assert r["ok"] and fake.sent == [(4, 1)] and "electric" in a.last["sources"]
    r = await a.command("electric", True)                   # already on → nothing sent (toggle-safe)
    assert r["ok"] and fake.sent == [(4, 1)]
    r = await a.command("furnace", False)
    assert r["ok"] and fake.sent[-1] == (5, 0) and "furnace" not in a.last["sources"]
    await http.close()


async def test_unverified_is_not_retried(aiohttp_server, monkeypatch):
    import aetherrv.adapters.rixens_http as rh
    monkeypatch.setattr(rh.asyncio, "sleep", _fast_sleep)
    fake = FakeMcs7(apply=False)                            # controller ignores/clamps the request
    a, http = await make(aiohttp_server, fake)
    r = await a.command("setpoint", 20.0, verify_timeout=0.2)
    assert not r["ok"] and fake.sent == [(1, 200)]          # exactly one send
    await http.close()


async def test_gate_blocks_without_sending(aiohttp_server):
    fake = FakeMcs7()
    a, http = await make(aiohttp_server, fake, enabled=False)
    r = await a.command("setpoint", 20.0)
    assert not r["ok"] and fake.sent == []
    await http.close()


async def test_rejected_command_never_sent(aiohttp_server):
    fake = FakeMcs7()
    a, http = await make(aiohttp_server, fake)
    r = await a.command("setpoint", 45.0)
    assert not r["ok"] and "outside" in r["detail"] and fake.sent == []
    await http.close()


_real_sleep = __import__("asyncio").sleep


async def _fast_sleep(s):
    await _real_sleep(min(s, 0.01))



def test_fan_glow_fuel_scaling_matches_mcs7_ui():
    x = FIXTURE
    for tag, val in (("burnermotor", 9257), ("glowpin", 655), ("dosingpump", 55), ("flametemp", 20775),
                     ("inlettemp", 3550), ("outlettemp", 4650)):
        x = re.sub(rf"<{tag}>-?\d+</{tag}>", f"<{tag}>{val}</{tag}>", x)
    d = parse_status(x)
    assert (d["fan_rpm"], d["glow_w"], d["fuel_hz"]) == (9257, 65.5, 5.5)
    assert (d["flame_c"], d["inlet_c"], d["outlet_c"]) == (207.75, 35.5, 46.5)



def test_fuel_flow_and_daily_total(tmp_path):
    import sqlite3
    db = sqlite3.connect(tmp_path / "h.sqlite3", isolation_level=None)
    a = RixensHttpAdapter(State(), None, TxGate({}), db=db)
    assert a.fuel_lph(5.5) == 0.62                       # calibration point: 5.0 kW full output
    assert round(a.fuel_lph(2.4), 2) == 0.27             # ≈ Eberspächer low output (0.27 l/h)
    d = {"fuel_hz": 5.5}
    lt = time.localtime()                               # local noon today: the run never crosses midnight
    t0 = time.mktime((lt.tm_year, lt.tm_mon, lt.tm_mday, 12, 0, 0, 0, 0, -1))
    a._account_fuel(d, t0)
    for i in range(1, 721):                              # one hour of 5 s polls at full output
        a._account_fuel(d, t0 + 5 * i)
    assert round(a.fuel_today_l, 2) == 0.62
    a2 = RixensHttpAdapter(State(), None, TxGate({}), db=db)   # survives a restart (same day)
    assert round(a2.fuel_today_l, 2) == 0.62
    a._account_fuel({"fuel_hz": 5.5}, t0 + 5 * 721 + 600)     # a gap > 120 s is not integrated
    assert round(a.fuel_today_l, 2) == 0.62



def test_fluid_indicator_uses_glycolstate():
    d = parse_status(FIXTURE.replace("<glycolstate>0</glycolstate>", "<glycolstate>2</glycolstate>"))
    assert d["glycol"] == "ready" and d["hot_tank"] == "off"     # settings/glycol stays 0 (hot tank mode off)
    assert parse_status(FIXTURE)["glycol"] == "cold"



def test_auto_fan_pct_from_pidspeed():
    x = FIXTURE.replace("<pidspeed>0</pidspeed>", "<pidspeed>31</pidspeed>")
    d = parse_status(x)
    assert d["fan_speed"] == "auto" and d["fan_auto_pct"] == 31
    d = parse_status(x.replace("<fanspeed>Auto</fanspeed>", "<fanspeed>40</fanspeed>"))
    assert d["fan_speed"] == 40 and d["fan_auto_pct"] is None



def test_hot_water_and_preheat_plan():
    cur = {"hot_water": False, "preheat": True, "available": {"preheat": True}}
    act, val, verify = RixensHttpAdapter.plan("hot_water", True, cur)
    assert (act, val) == (6, 1) and verify({"hot_water": True}) and not verify({"hot_water": False})
    assert RixensHttpAdapter.plan("hot_water", False, cur) is None          # already off → no send
    assert RixensHttpAdapter.plan("preheat", False, cur)[:2] == (7, 0)
    assert RixensHttpAdapter.plan("preheat", True, cur) is None
    import pytest
    with pytest.raises(Exception):
        RixensHttpAdapter.plan("preheat", True, {"preheat": False, "available": {"preheat": False}})
    with pytest.raises(Exception):
        RixensHttpAdapter.plan("floor", True, cur)                           # still not allowed


def test_thermostat_plan():
    cur = {"thermostat": True}
    assert RixensHttpAdapter.plan("thermostat", True, cur) is None            # already on → nothing sent
    act, val, verify = RixensHttpAdapter.plan("thermostat", False, cur)
    assert (act, val) == (9, 0) and verify({"thermostat": False}) and not verify({"thermostat": True})
