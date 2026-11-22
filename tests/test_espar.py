"""Espar decoder against the frame sequence captured on this van on 2026-11-22 (read-only)."""
import can

from aetherrv.adapters.espar import EsparAdapter
from aetherrv.alerts import r_espar
from aetherrv.state import State


def f(i, hexdata):
    return can.Message(arbitration_id=i, data=bytes.fromhex(hexdata), is_extended_id=False)


def snap(a, t):
    return {k.split(".", 2)[2]: v for k, v in a.snapshot(t).items()}


def test_espar_cycle_from_the_van_capture(monkeypatch):
    import time as _t
    now = [1000.0]; monkeypatch.setattr(_t, "time", lambda: now[0])
    a = EsparAdapter("espar", State())
    s = snap(a, now[0]); assert s["state"] == "off" and s["asleep"] is True and s["heard"] is False   # asleep since start
    a.on_frame(f(0x054, "0000FEFFDA020000")); a.on_frame(f(0x2C4, "030000001B020000"))
    s = snap(a, now[0])
    assert s["state"] == "off" and s["mode"] == "off" and s["room_c"] == 23.0 and s["heater_c"] == 3.9
    assert s["setpoint_c"] is None and s["fault"] is None and s["asleep"] is False
    a.on_frame(f(0x054, "09050B01DA020000"))                           # HEAT, 80 °F
    a.on_frame(f(0x2C4, "09000000DA020000"))
    now[0] += 300; a.on_frame(f(0x2C4, "09040000DA020000"))
    s = snap(a, now[0])
    assert s["state"] == "heating" and s["mode"] == "heat" and s["setpoint_c"] == 26.7 and s["run_s"] == 300
    assert s["heater_c"] is None                                       # mirrors the room value while running
    a.on_frame(f(0x054, "09050001DA020000")); assert snap(a, now[0])["setpoint_c"] == 25.6   # 78 °F
    now[0] += 10; a.on_frame(f(0x054, "0800FEFFE4020000")); a.on_frame(f(0x2C4, "030800009B020000"))
    s = snap(a, now[0])
    assert s["state"] == "cooldown" and s["cooldown_left_s"] == 240 and s["mode"] == "off" and s["room_c"] == 24.0
    now[0] += 240; a.on_frame(f(0x2C4, "0300000095020000"))
    s = snap(a, now[0]); assert s["state"] == "off" and s["heater_c"] == 16.1 and s["cooldown_left_s"] is None
    now[0] += 30                                                       # the bus goes to sleep: normal
    s = snap(a, now[0]); assert s["state"] == "off" and s["asleep"] is True and s["room_c"] is None


def test_espar_fault_alert(monkeypatch):
    import time as _t
    monkeypatch.setattr(_t, "time", lambda: 50.0)
    st = State(); a = EsparAdapter("espar", st)
    a.on_frame(f(0x054, "09050B01DA020000")); a.on_frame(f(0x2C4, "0B082000DA020000"))   # fault: flame loss
    st.update(a.snapshot(50.0))
    assert st.get("climate.espar.state") == "error" and st.get("climate.espar.fault") == "0x20"
    (x,) = list(r_espar(st, {}))
    assert x.severity == "serious" and "flame loss" in x.detail
