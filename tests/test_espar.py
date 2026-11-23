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
    now[0] += 300; a.on_frame(f(0x054, "09050B01DA020000")); a.on_frame(f(0x2C4, "09040000DA020000"))
    s = snap(a, now[0])
    assert s["state"] == "heating" and s["mode"] == "heat" and s["setpoint_c"] == 26.7 and s["run_s"] == 300
    assert s["heater_c"] is None                                       # mirrors the room value while running
    a.on_frame(f(0x054, "09050001DA020000")); assert snap(a, now[0])["setpoint_c"] == 25.6   # 78 °F
    now[0] += 10; a.on_frame(f(0x054, "0800FEFFE4020000")); a.on_frame(f(0x2C4, "030800009B020000"))
    s = snap(a, now[0])
    assert s["state"] == "cooldown" and s["cooldown_left_s"] == 240 and s["mode"] == "off" and s["room_c"] == 24.0
    now[0] += 240; a.on_frame(f(0x054, "0800FEFFE4020000")); a.on_frame(f(0x2C4, "0300000095020000"))
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


def test_espar_diag_is_gated_and_only_reads(monkeypatch):
    import time as _t
    from aetherrv.txgate import TxGate
    now = [100.0]; monkeypatch.setattr(_t, "time", lambda: now[0])
    sent = []

    class Bus:
        def send(self, device, msg, gate, quiet=False):
            gate.check(device, "x"); sent.append((device, msg.arbitration_id, bytes(msg.data)))
    off = EsparAdapter("espar", State(), bus=Bus(), gate=TxGate({"espar_diag": False}))
    assert not off.diag_enabled and not off._send_diag(b"\x03\x22\xfd\x04" + b"\xaa" * 4, "read") and sent == []
    a = EsparAdapter("espar", State(), bus=Bus(), gate=TxGate({"espar_diag": True}))
    assert not a.awake()                                               # bus asleep: never send (no wake-ups)
    a.on_frame(f(0x054, "0000FEFFDA020000")); a.on_frame(f(0x2C4, "030000001B020000"))
    assert a.awake()
    a.on_frame(f(0x73C, "100B62FD10011400"))                           # first frame → we send flow control
    assert sent == [("espar_diag", 0x7A0, bytes.fromhex("30001400AAAAAAAA".replace("1400", "14AA")))]
    a.on_frame(f(0x73C, "2100000000000000"))
    a.on_frame(f(0x73C, "0562FD040084FFFF"))
    s = a.diag_snapshot(now[0])
    assert s["climate.espar.diag.phase"] == {"request": "heat", "phase": "burning"}
    assert s["climate.espar.diag.voltage_v"] == 13.2 and s["climate.espar.diag.enabled"] is True
    assert all(dev == "espar_diag" and aid == 0x7A0 for dev, aid, _ in sent)


def test_espar_no_heat_and_power_up_lock_and_lockout_alerts(monkeypatch):
    import time as _t
    now = [10.0]; monkeypatch.setattr(_t, "time", lambda: now[0])
    st = State(); a = EsparAdapter("espar", st)
    a.on_frame(f(0x054, "09050B01DA020000")); a.on_frame(f(0x2C4, "030004001B020000"))   # HEAT, but idle + 0x04
    now[0] += 31; a.on_frame(f(0x054, "09050B01DA020000")); a.on_frame(f(0x2C4, "030004001B020000"))
    st.update(a.snapshot(now[0]))
    assert "power-up lock" in st.get("climate.espar.issue")
    a.dtcs = [{"code": "P00012B", "text": "lockout", "active": True, "severity": 1, "lockout": True}]
    st.update(a.diag_snapshot(now[0]))
    ids = {x.id: x.severity for x in r_espar(st, {})}
    assert ids["espar.lockout"] == "serious" and "espar.issue" in ids
