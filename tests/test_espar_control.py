"""AetherRV as the Espar's controller, against a simulated heater (no real bus)."""
import asyncio

import can

from aetherrv.adapters.espar import EsparAdapter
from aetherrv.adapters.espar_control import EsparControl, run_minutes_ok
from aetherrv.state import State
from aetherrv.txgate import TxGate


def f(i, h):
    return can.Message(arbitration_id=i, data=bytes.fromhex(h), is_extended_id=False)


class Bus:
    channel = "hat1"

    def __init__(self):
        self.sent = []

    def send(self, device, msg, gate, quiet=False):
        gate.check(device, "x"); self.sent.append((msg.arbitration_id, bytes(msg.data)))


def rig(enabled=True, room=True):
    st = State()
    if room:
        st.set("climate.cabin_front.temp", 18.0)
    a = EsparAdapter("espar", st)
    bus = Bus(); resets = []
    async def reset(iface): resets.append(iface)
    c = EsparControl(st, a, bus, TxGate({"espar": enabled}), reset=reset)
    c.VERIFY_S = c.WAKE_S = 0.5
    return st, a, c, bus, resets


def heater_says(a, state_hex):
    a.on_frame(f(0x2C4, state_hex)); a.on_frame(f(0x625, "2510ABE5FC3B5806"))


def test_run_minutes_match_the_easystart_pro():
    assert all(run_minutes_ok(m) for m in (None, 1, 45, 240, 270, 300, 720))
    assert not any(run_minutes_ok(m) for m in (0, 241, 250, 735, 721, -5, 1.5))


async def test_disabled_sends_nothing_and_refuses():
    st, a, c, bus, _ = rig(enabled=False)
    r = await c.command("set", {"mode": "heat", "setpoint_c": 21})
    assert not r["ok"] and "disabled" in r["detail"] and bus.sent == []


async def test_heat_takeover_verify_timer_and_idle_stop():
    st, a, c, bus, resets = rig()
    async def heater():                                         # simulated heater: answers, then heats
        for _ in range(40):
            await asyncio.sleep(0.03)
            c.step()
            cmd = [d for i, d in bus.sent if i == 0x054]
            heater_says(a, "09040000" + "1C02" + "0000" if cmd and cmd[-1][1] == 0x05 else "030000001C020000")
    t = asyncio.ensure_future(heater())
    r = await c.command("set", {"mode": "heat", "setpoint_c": 21.5, "run_min": 30})
    await t
    assert r["ok"], r
    cmd = [d for i, d in bus.sent if i == 0x054][-1]
    assert cmd[:4] == bytes.fromhex("0905D700") and cmd[4:6] == (int((18.0 + 50) * 10)).to_bytes(2, "little")
    assert {0x055, 0x056, 0x057, 0x60D, 0x062, 0x10A, 0x06D} <= {i for i, _ in bus.sent}     # cyclic + settings burst
    assert c.until is not None and 1790 < c.until - __import__("time").time() <= 1800
    c.until = 0                                                 # run time over → OFF
    c.step()
    assert [d for i, d in bus.sent if i == 0x054][-1][:2] == bytes([0x08, 0x00]) and c.mode == "off"
    assert resets == []


async def test_no_room_temperature_refuses_heat_and_heater_silence_stops():
    st, a, c, bus, resets = rig(room=False)
    r = await c.command("set", {"mode": "heat", "setpoint_c": 21})
    assert not r["ok"] and "room temperature" in r["detail"] and bus.sent == []
    st.set("climate.cabin_front.temp", 18.0)
    r = await c.command("set", {"mode": "fan", "run_min": 10})   # nobody answers: stop, reset hat1, report
    for _ in range(10):
        c.step(); await asyncio.sleep(0.05)
    assert not r["ok"] and not c.active and resets == ["hat1"] and "didn't answer" in c.problem


async def test_another_controller_on_the_bus_stops_us():
    st, a, c, bus, resets = rig()
    a.on_frame(f(0x60D, "0D10FD1D02207000"))                    # an EasyStart Pro is talking
    r = await c.command("set", {"mode": "fan", "run_min": 5})
    assert not r["ok"] and "Another controller" in r["detail"] and bus.sent == []
    a.last_other_ctrl = None
    c.active, c.mode = True, "fan"
    a.on_frame(f(0x054, "0000FEFFDA020000"))                    # it appears while we drive the heater
    assert not c.active and "another controller" in c.problem and c.mode == "off"


async def test_attach_in_off_then_heat_only_after_idle_and_burst_is_spread():
    st, a, c, bus, resets = rig()
    async def heater():
        for _ in range(30):
            await asyncio.sleep(0.03)
            before = len(bus.sent); c.step()
            assert len(bus.sent) - before <= 7                       # never more than hat1's queue can take
            heater_says(a, "030000001C020000")                      # woken: idle
    t = asyncio.ensure_future(heater())
    r = await c.command("set", {"mode": "off", "attach": True})     # bench step 1: take over, OFF only
    await t
    assert r["ok"] and c.active and c.mode == "off"
    assert all(d[1] == 0x00 for i, d in bus.sent if i == 0x054)     # nothing but OFF was ever sent
    assert {i for i, _ in bus.sent} >= {0x062, 0x06D, 0x10A}        # the whole settings burst went out

    st2, a2, c2, bus2, _ = rig()
    async def heater2():
        for k in range(40):
            await asyncio.sleep(0.03); c2.step()
            cmds = [d for i, d in bus2.sent if i == 0x054]
            heater_says(a2, "090400001C020000" if cmds and cmds[-1][1] == 0x05 else "030000001C020000")
    t2 = asyncio.ensure_future(heater2())
    r2 = await c2.command("set", {"mode": "heat", "setpoint_c": 20, "run_min": 60})
    await t2
    modes = [d[1] for i, d in bus2.sent if i == 0x054]
    assert r2["ok"] and modes[0] == 0x00 and 0x05 in modes            # woke in OFF, HEAT only after idle
    until = c2.until
    r3 = await c2.command("set", {"mode": "heat", "setpoint_c": 21, "run_min": 60})   # setpoint nudge
    assert c2.until == until and c2.setpoint_c == 21.0                # countdown not restarted


async def test_full_queue_while_waking_is_not_fatal():
    st, a, c, bus, resets = rig()
    n = [0]
    real = bus.send
    def flaky(device, msg, gate, quiet=False):
        n[0] += 1
        if n[0] < 6:
            raise OSError(105, "No buffer space available")         # ENOBUFS behind the un-ACKed first frame
        real(device, msg, gate, quiet)
    bus.send = flaky
    async def heater():
        for _ in range(20):
            await asyncio.sleep(0.03); c.step(); heater_says(a, "030000001C020000")
    t = asyncio.ensure_future(heater())
    r = await c.command("set", {"mode": "off", "attach": True})
    await t
    assert r["ok"] and c.active and resets == []


def test_fan_word_experiment_only_in_fan_mode():
    st, a, c, bus, _ = rig()
    c.mode, c.flags = "fan", 0x08
    assert c._cmd_frame(18.0)[2:4] == b"\xfe\xff"               # default: exactly what the EasyStart Pro sends
    c.fan_word = 0x0096
    assert c._cmd_frame(18.0)[2:4] == bytes([0x96, 0x00])
    c.mode = "heat"                                              # never leaks into HEAT
    assert c._cmd_frame(18.0)[2:4] == int(round(c.setpoint_c * 10)).to_bytes(2, "little")
    bad = asyncio.run(c.command("set", {"mode": "heat", "setpoint_c": 20, "fan_word": 5}))
    assert not bad["ok"] and "FAN mode only" in bad["detail"]
    assert not asyncio.run(c.command("set", {"mode": "fan", "fan_word": 70000}))["ok"]
