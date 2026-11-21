import asyncio
import json

import pytest

from aetherrv.adapters.victron import VictronAdapter
from aetherrv.state import State
from aetherrv.txgate import TxGate


class FakeCerbo:
    """Answers R/ (republish current value) and applies W/ (unless apply=False)."""

    def __init__(self, adapter, values, apply=True):
        self.a, self.values, self.apply, self.writes = adapter, dict(values), apply, []

    async def publish(self, topic, payload=b""):
        kind, _, path = topic.split("/", 2)
        if kind == "W":
            self.writes.append((path, json.loads(payload)["value"]))
            if self.apply:
                self.values[path] = json.loads(payload)["value"]
                asyncio.get_running_loop().call_later(0.01, lambda: asyncio.ensure_future(self.a._on_control_path(path, self.values[path])))
        elif kind == "R" and path in self.values:
            asyncio.get_running_loop().call_later(0.01, lambda: asyncio.ensure_future(self.a._on_control_path(path, self.values[path])))


BASE = {"vebus/276/Mode": 4, "vebus/276/ModeIsAdjustable": 1, "vebus/276/Ac/In/1/CurrentLimitIsAdjustable": 1,
        "vebus/276/Ac/In/1/CurrentLimit": 30.0, "solarcharger/277/Mode": 1}


async def make(values=BASE, apply=True, gate=True):
    a = VictronAdapter(State(), "x")
    a.gate = TxGate({"victron": gate})
    a.portal = "p"
    for k, v in values.items():
        await a._on_control_path(k, v)
    fake = FakeCerbo(a, values, apply)
    a._client = fake
    return a, fake


async def test_inverter_on_then_noop():
    a, cerbo = await make()
    r = await a.command("inverter_mode", 3, verify_timeout=1)
    assert r["ok"] and cerbo.writes == [("vebus/276/Mode", 3)] and a.state.get("victron.vebus.mode") == 3
    r = await a.command("inverter_mode", 3)
    assert r["ok"] and r["detail"] == "already set" and len(cerbo.writes) == 1


async def test_unverified_is_sent_once():
    a, cerbo = await make(apply=False)
    r = await a.command("mppt_mode", 4, verify_timeout=0.2)
    assert not r["ok"] and cerbo.writes == [("solarcharger/277/Mode", 4)]


async def test_guards():
    a, cerbo = await make(values={**BASE, "vebus/276/ModeIsAdjustable": 0})
    assert "not adjustable" in (await a.command("inverter_mode", 3))["detail"]
    a, cerbo = await make()
    for cmd, v in [("inverter_mode", 5), ("shore_limit", 80), ("shore_limit", 1), ("mppt_mode", 2), ("relay", 1)]:
        r = await a.command(cmd, v)
        assert not r["ok"]
    assert cerbo.writes == []
    a, cerbo = await make(gate=False)
    assert (await a.command("inverter_mode", 3))["detail"] == "Victron control is disabled" and cerbo.writes == []


async def test_shore_limit_rounding():
    a, cerbo = await make()
    r = await a.command("shore_limit", 15.04, verify_timeout=1)
    assert r["ok"] and cerbo.writes == [("vebus/276/Ac/In/1/CurrentLimit", 15.0)]


async def test_state_is_json_safe():
    a, _ = await make()
    json.dumps(a.state.snapshot())
