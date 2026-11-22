import asyncio
import time

import can
import pytest

from aetherrv.adapters.capture_only import CaptureOnlyAdapter
from aetherrv.adapters.n2k import N2kAdapter, parse_id
from aetherrv.canbus import CanBus, replay
from aetherrv.state import State
from aetherrv.txgate import TxBlocked, TxGate


def test_txgate_denies_by_default():
    g = TxGate({"espar": False})
    for dev in ("espar", "rixens", "unknown"):
        with pytest.raises(TxBlocked):
            g.check(dev, "test")
    assert g.blocked_count == {"espar": 1, "rixens": 1, "unknown": 1}


def test_txgate_requires_literal_true():
    g = TxGate({"a": True, "b": "yes", "c": 1})  # type: ignore[dict-item]
    g.check("a", "ok")
    for d in ("b", "c"):
        with pytest.raises(TxBlocked):
            g.check(d, "x")


@pytest.mark.parametrize("pgn,src", [(127501, 0x23), (130312, 0x10), (126208, 0x01)])
def test_n2k_parse_pdu2_and_pdu1(pgn, src):
    from tools_bench import n2k_id  # noqa: F401  (see conftest)
    nid = parse_id(n2k_id(pgn, src, priority=3, dest=0x55))
    assert (nid.pgn, nid.source, nid.priority) == (pgn & (0x3FF00 if ((pgn >> 8) & 0xFF) < 240 else 0x3FFFF), src, 3)
    if ((pgn >> 8) & 0xFF) < 240:
        assert nid.destination == 0x55
    else:
        assert nid.destination == 255


def test_known_id_vector():
    # 0x09F20123: priority 2, PGN 0x1F201 = 127489 (Engine Parameters, Dynamic), source 0x23
    nid = parse_id(0x09F20123)
    assert (nid.priority, nid.pgn, nid.source, nid.destination) == (2, 127489, 0x23, 255)
    # 0x18EAFF01: priority 6, PGN 59904 (ISO Request, PDU1) to global 0xFF from 0x01
    nid = parse_id(0x18EAFF01)
    assert (nid.priority, nid.pgn, nid.source, nid.destination) == (6, 59904, 0x01, 0xFF)


async def test_capture_roundtrip_and_pipeline(tmp_path):
    """virtual bus → CanBus (captures + adapters) → capture file → replay → same frames."""
    state = State()
    cb = CanBus("t", "virtual", "vp-test", capture_dir=tmp_path)
    n2k = N2kAdapter(state)
    cap = CaptureOnlyAdapter("espar", state)
    cb.add_handler(n2k.on_frame)
    cb.add_handler(cap.on_frame)
    task = asyncio.create_task(cb.run())
    await asyncio.sleep(0.3)

    tx = can.Bus(interface="virtual", channel="vp-test")
    sent = [can.Message(arbitration_id=0x09F20123 + i, data=bytes([i] * 8), is_extended_id=True) for i in range(5)]
    for m in sent:
        tx.send(m)
    await asyncio.sleep(0.5)
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task
    tx.shutdown()

    assert cb.stats.frames == 5
    assert {r["pgn"] for r in n2k.summary()} == {127489}
    assert len(cap.summary()) == 5

    logs = list(tmp_path.glob("t-*.log"))
    assert len(logs) == 1
    back = list(can.CanutilsLogReader(str(logs[0])))
    assert [m.arbitration_id for m in back] == [m.arbitration_id for m in sent]
    assert [bytes(m.data) for m in back] == [bytes(m.data) for m in sent]

    rx = can.Bus(interface="virtual", channel="vp-replay")
    out = can.Bus(interface="virtual", channel="vp-replay")
    await replay(logs[0], out, speed=0)
    got = [rx.recv(0.5) for _ in sent]
    assert [m.arbitration_id for m in got] == [m.arbitration_id for m in sent]
    rx.shutdown(); out.shutdown()


async def test_state_pubsub():
    s = State()
    q = s.subscribe()
    s.set("a", 1); s.set("a", 1); s.set("a", 2)
    assert [q.get_nowait(), q.get_nowait()] == [{"a": 1}, {"a": 2}]
    assert q.empty()


async def test_canbus_send_is_gated():
    cb = CanBus("g", "virtual", "vp-gate")
    task = asyncio.create_task(cb.run())
    await asyncio.sleep(0.2)
    msg = can.Message(arbitration_id=0x123, data=b"\x01", is_extended_id=False)
    with pytest.raises(TxBlocked):
        cb.send("espar", msg, TxGate({"espar": False}))
    cb.send("bench", msg, TxGate({"bench": True}))  # allowed path works
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task


def test_history_samples_numeric_and_downsamples():
    from aetherrv.history import History
    s = State()
    h = History(s, interval=1)
    now = time.time()
    for i in range(50):
        s.set("victron.battery.soc", float(i))
        s.set("victron.pv.power", True)  # bools are not samples
        h.sample(now - 50 + i)
    assert len(h.get("victron.battery.soc", since=100, max_points=1000, now=now)) == 50
    assert h.get("victron.pv.power", now=now) == []
    assert h.get("not.tracked", now=now) == []
    ds = h.get("victron.battery.soc", since=60, max_points=10, now=now)
    assert len(ds) <= 10
    assert [p[1] for p in ds] == sorted(p[1] for p in ds)       # bucket means stay ordered
    assert ds[0][1] < 5 and ds[-1][1] > 45


def test_history_persists_and_prunes(tmp_path):
    from aetherrv.history import History
    db = tmp_path / "h.sqlite3"
    s = State()
    h = History(s, path=db, interval=30, retention_days=1)
    now = time.time()
    s.set("victron.battery.soc", 80.0)
    h.sample(now - 2 * 86400)          # older than the 1-day retention
    assert h.count() == 1
    h.close()

    h2 = History(State(), path=db, interval=30, retention_days=1)
    assert h2.count() == 1                                      # survived a restart
    assert h2.prune(now) == 1                                   # and ages out
    h2.state.set("victron.battery.soc", 81.0)
    h2.sample(now - 60)
    h2.close()
    h3 = History(State(), path=db, interval=30, retention_days=1)
    assert [p[1] for p in h3.get("victron.battery.soc", since=3600, now=now)] == [81.0]
    h3.close()



def test_history_falls_back_when_db_unwritable(tmp_path):
    import os
    from aetherrv.history import History
    db = tmp_path / "h.sqlite3"
    db.write_bytes(b"")
    os.chmod(db, 0o444)
    os.chmod(tmp_path, 0o555)
    try:
        h = History(State(), path=db)
        assert h.degraded and h.path == ":memory:"
        h.state.set("victron.battery.soc", 50.0)
        h.sample()
        assert h.count() == 1
    finally:
        os.chmod(tmp_path, 0o755)



async def test_ui_is_not_cached(aiohttp_client, tmp_path):
    from aetherrv.web import make_app
    (tmp_path / "index.html").write_text("<html></html>")
    (tmp_path / "app.js").write_text("//")
    c = await aiohttp_client(make_app(State(), tmp_path))
    for path in ("/", "/static/app.js"):
        r = await c.get(path)
        assert r.status == 200 and r.headers.get("Cache-Control") == "no-cache"



async def test_prune_captures(tmp_path):
    import os
    from aetherrv.canbus import prune_captures
    old, new = tmp_path / "a.log", tmp_path / "b.log"
    old.write_bytes(b"x" * 10); new.write_bytes(b"x" * 10)
    os.utime(old, (time.time() - 20 * 86400,) * 2)
    task = asyncio.create_task(prune_captures(tmp_path, keep_days=14, max_gb=20))
    await asyncio.sleep(0.05); task.cancel()
    assert not old.exists() and new.exists()


async def test_slow_bus_open_does_not_block_event_loop(monkeypatch):
    """A socketcand open to an unreachable host blocks ~10 s; it must not stall the loop (systemd watchdog)."""
    cb = CanBus("slow", "socketcand", "can0", host="192.0.2.1", port=29536)

    def slow_open():
        time.sleep(0.5)
        raise OSError("No route to host")
    monkeypatch.setattr(cb, "_open", slow_open)
    task = asyncio.create_task(cb.run())
    t0 = time.monotonic(); ticks = 0
    while time.monotonic() - t0 < 0.4:
        await asyncio.sleep(0.02); ticks += 1
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task
    assert ticks >= 10          # the loop kept running while the open was in progress


def test_cabin_average_of_front_ruuvi_and_rear_mcs7():
    import time as _t
    from aetherrv.climate import derive_cabin
    from aetherrv.state import State
    s = State(); now = _t.time()
    s.update({"climate.cabin_front.temp": 21.2, "climate.cabin_front.humidity": 33.3, "victron.connected": True,
              "climate.rixens.cabin_c": 15.6, "climate.rixens.cabin_rh": 42.5, "climate.rixens.live": True,
              "climate.rixens.updated": now})
    derive_cabin(s, now)
    assert s.get("climate.cabin.temp") == 18.4 and s.get("climate.cabin.humidity") == 37.9
    assert s.get("climate.cabin.source") == "average"
    assert (s.get("climate.cabin.front_temp"), s.get("climate.cabin.rear_temp")) == (21.2, 15.6)
    # MCS7 stale → front alone
    derive_cabin(s, now + 120)
    assert s.get("climate.cabin.temp") == 21.2 and s.get("climate.cabin.source") == "front"
    # Cerbo gone too → nothing
    s.set("victron.connected", False)
    derive_cabin(s, now + 120)
    assert s.get("climate.cabin.temp") is None and s.get("climate.cabin.source") is None


def test_fridge_off_silences_warm_alert_and_rearms_when_cold(tmp_path):
    from aetherrv.alerts import DEFAULTS, r_cold
    from aetherrv.climate import ColdRearm
    from aetherrv.settings import Prefs
    from aetherrv.state import State
    s = State(); prefs = Prefs(s, str(tmp_path / "settings.json"))
    s.update({"ui.fridge_max_c": 5.0, "ui.freezer_max_c": -12.0, "climate.fridge.temp": 21.0, "climate.freezer.temp": 21.0})
    assert {f.id for f in r_cold(s, DEFAULTS)} == {"cold.fridge", "cold.freezer"}      # warm → alerts, never guessed off
    prefs.set("fridge_off", True)                                                       # owner: "fridge is off"
    assert {f.id for f in r_cold(s, DEFAULTS)} == {"cold.freezer"}
    rr = ColdRearm(s, prefs)
    assert rr.check() == [] and s.get("settings.fridge_off") is True                    # still warm: stays off
    s.set("climate.fridge.temp", 4.5)
    assert rr.check() == ["fridge"] and s.get("settings.fridge_off") is False           # cold again: re-armed
    s.set("climate.fridge.temp", 8.0)
    assert "cold.fridge" in {f.id for f in r_cold(s, DEFAULTS)}


async def test_traffic_tiles_proxy_cache_and_cap(aiohttp_client, tmp_path):
    from aetherrv.traffic import TrafficTiles
    from aetherrv.web import make_app
    calls = []

    t = TrafficTiles("KEY", daily_cap=2)
    assert "key=KEY" in t.url("flow", "major", 5, 1, 2) and "reduced-sensitivity" in t.url("flow", "major", 5, 1, 2)
    assert t.valid("incidents", "s0-dark", 7, 10, 20) and not t.valid("incidents", "s9", 7, 1, 1)
    assert not t.valid("flow", "major", 13, 0, 0) and not t.valid("flow", "major", 3, 8, 0)

    class Resp:
        status = 200
        async def read(self): return b"PNG"
        async def __aenter__(self): return self
        async def __aexit__(self, *a): return False

    class Sess:
        closed = False
        def get(self, url):
            calls.append(url); return Resp()
    t.session = Sess()
    c = await aiohttp_client(make_app(State(), tmp_path, traffic=t))
    r = await c.get("/api/traffic/flow/major/5/1/2.png")
    assert r.status == 200 and await r.read() == b"PNG" and r.content_type == "image/png"
    await c.get("/api/traffic/flow/major/5/1/2.png?b=2")         # cached: no second upstream call
    assert len(calls) == 1
    assert (await c.get("/api/traffic/incidents/s0/5/1/2.png")).status == 200
    assert (await c.get("/api/traffic/incidents/s0/5/1/3.png")).status == 429   # daily cap reached
    assert (await c.get("/api/traffic/flow/bogus/5/1/2.png")).status == 404
    c2 = await aiohttp_client(make_app(State(), tmp_path))         # no key configured
    assert (await c2.get("/api/traffic/flow/major/5/1/2.png")).status == 404
