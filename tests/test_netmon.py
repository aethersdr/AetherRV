import asyncio

from aetherrv.adapters.netmon import NetMon, _tcp_ok
from aetherrv.alerts import Alerts, Rule, default_rules, r_netmon
from aetherrv.state import State


def mon():
    return NetMon(State(), [{"name": "IPG100", "host": "192.0.2.5", "tcp": 6544, "fail_after": 2,
                             "severity": "serious", "hint": "Power-cycle the IPG100 to recover it."}])


def test_debounce_and_reasons():
    m = mon(); t = m.targets[0]
    m.apply(t, 5.0, True, now=0)
    assert t["up"] is True
    m.apply(t, 5.1, False, now=60)                 # ping ok, service dead: 1st failure, not yet down
    assert t["up"] is True
    m.apply(t, 5.2, False, now=120)
    snap = m.snapshot(now=300)[0]
    assert snap["up"] is False and snap["reason"] == "service not answering" and snap["down_for"] == 300
    m.apply(t, None, None, now=360)
    assert m.snapshot(now=360)[0]["reason"] == "no response"
    m.apply(t, 4.9, True, now=420)
    assert m.snapshot(now=420)[0]["up"] is True and m.snapshot(now=420)[0]["down_for"] is None


def test_alert_text():
    m = mon(); t = m.targets[0]
    m.apply(t, 5.0, True, now=0); m.apply(t, 5.0, False, now=60); m.apply(t, 5.0, False, now=120)
    s = State(); s.set("netmon.targets", m.snapshot(now=600))
    (f,) = list(r_netmon(s, {}))
    assert f.severity == "serious" and f.title == "IPG100 service not answering"
    assert "10 min" in f.detail and "Power-cycle" in f.detail
    assert "netmon" in {r.name for r in default_rules()}


async def test_tcp_check_real_socket():
    srv = await asyncio.start_server(lambda r, w: w.close(), "127.0.0.1", 0)
    port = srv.sockets[0].getsockname()[1]
    assert await _tcp_ok("127.0.0.1", port, 1.0)
    srv.close(); await srv.wait_closed()
    assert not await _tcp_ok("127.0.0.1", port, 1.0)
