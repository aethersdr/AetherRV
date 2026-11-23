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
