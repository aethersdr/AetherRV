import json

import pytest
from aiohttp import web

from aetherrv.adapters.peplink import (PeplinkClient, active_wan, normalize_allowance, normalize_location,
                                       normalize_wans)
from aetherrv.adapters.starlink import encode_obstruction_map, normalize

# Shapes follow the Peplink Router API doc (fw 8.6.0) examples / object tables.
WAN_RESP = {
    "1": {"name": "Starlink", "enable": True, "message": "Connected", "uptime": 3600, "type": "ethernet",
          "virtualType": "ethernet", "priority": 1, "ip": "100.64.0.2", "statusLed": "green"},
    "2": {"name": "Cellular", "enable": True, "message": "Connected", "uptime": 7200, "type": "cellular",
          "virtualType": "cellular", "priority": 2, "statusLed": "green",
          "cellular": {"mobileType": "5G", "signalLevel": 4, "carrierAggregation": True,
                       "carrier": {"name": "Verizon", "country": "US"},
                       "roamingStatus": {"code": 1, "message": "home"},
                       "imei": "SECRET-IMEI",
                       "sim": {"order": [1, 2], "1": {"status": "In Use", "active": True, "iccid": "SECRET-ICCID", "imsi": "SECRET-IMSI", "password": "SECRET-APN"},
                               "2": {"status": "No SIM Card Detected", "active": False}},
                       "rat": [{"name": "LTE", "band": [{"name": "B66", "channel": 66786, "signal": {"rsrp": -101, "rsrq": -11, "sinr": 9.5, "rssi": -70}}]},
                               {"name": "5G NR", "band": [{"name": "n77", "channel": 650000, "signal": {"rsrp": -95, "rsrq": -10, "sinr": 14.0}}]}]}},
    "3": {"name": "WAN2", "enable": True, "message": "No Cable Detected", "type": "ethernet", "statusLed": "red", "priority": 3},
    "order": [1, 2, 3],
}


def test_normalize_wans_and_active():
    wans = normalize_wans(WAN_RESP)
    assert [w["name"] for w in wans] == ["Starlink", "Cellular", "WAN2"]
    assert wans[0]["starlink"] and wans[0]["connected"] and not wans[2]["connected"]
    c = wans[1]["cellular"]
    assert c["carrier"] == "Verizon" and c["network"] == "5G" and c["sim"] == 1 and c["roaming"] == "home"
    assert (c["rat"], c["band"], c["rsrp"], c["sinr"], c["bands"]) == ("5G NR", "n77", -95, 14.0, 2)  # strongest band
    assert active_wan(wans)["name"] == "Starlink"
    wans[0]["connected"] = False
    assert active_wan(wans)["name"] == "Cellular"


def test_no_identifiers_leak():
    blob = json.dumps(normalize_wans(WAN_RESP))
    assert "SECRET" not in blob


def test_allowance_shapes():
    resp = {"2": {"1": {"enable": True, "usage": 41000, "limit": 50000, "percent": 82, "start": 1, "unit": "MB"},
                  "2": {"enable": False}, "order": [1, 2]},
            "1": {"enable": True, "usage": 5, "limit": 1024, "percent": 0, "start": 1, "unit": "MB"},
            "order": [1, 2]}
    assert normalize_allowance(resp) == [
        {"conn": 1, "sim": None, "usage_mb": 5, "limit_mb": 1024, "percent": 0, "start_day": 1},
        {"conn": 2, "sim": 1, "usage_mb": 41000, "limit_mb": 50000, "percent": 82, "start_day": 1}]


def test_location_fix_and_nofix():
    d = normalize_location({"gps": True, "location": {"latitude": 45.5, "longitude": -122.6, "altitude": 55.1,
                                                       "speed": 0.02, "heading": 356.9, "hdop": 1, "timestamp": 1311972720}})
    assert d["location.fix"] and d["location.lat"] == 45.5 and d["location.source"] == "peplink"
    assert normalize_location({"gps": False, "location": {}})["location.lat"] is None


def test_starlink_normalize():
    st = {"state": "CONNECTED", "uptime": 1000, "software_version": "2026.11", "pop_ping_latency_ms": 31.24,
          "pop_ping_drop_rate": 0.0012, "downlink_throughput_bps": 85_400_000, "uplink_throughput_bps": 9_100_000,
          "fraction_obstructed": 0.0234, "currently_obstructed": False, "gps_sats": 12}
    d = normalize(st, {}, {"alert_motors_stuck": False, "alert_is_heating": True, "alert_thermal_throttle": True,
                           "alert_brand_new_thing": True})
    assert d["starlink.online"] and d["starlink.down_mbps"] == 85.4 and d["starlink.obstructed_pct"] == 2.34
    assert {a["id"]: a["severity"] for a in d["starlink.alerts"]} == {
        "alert_is_heating": None, "alert_thermal_throttle": "warning", "alert_brand_new_thing": "warning"}
    assert normalize({"state": "OBSTRUCTED"}, {}, {})["starlink.online"] is False


def test_starlink_obstruction_map_encoding():
    import base64
    m = encode_obstruction_map(2, 3, [-1.0, 0.0, 1.0, 0.5, 1.7, -0.2], 10.0, 80.0, 2)
    assert (m["rows"], m["cols"], m["frame"], m["min_el"], m["max_theta"]) == (2, 3, "dish", 10.0, 80.0)
    assert list(base64.b64decode(m["data"])) == [255, 0, 254, 127, 254, 255]
    assert encode_obstruction_map(1, 1, [1.0], frame=1)["frame"] == "earth"
    with pytest.raises(ValueError):
        encode_obstruction_map(2, 2, [1.0])


@pytest.fixture
async def fake_router(aiohttp_server):
    calls = {"grant": 0}

    async def grant(req):
        body = await req.json()
        calls["grant"] += 1
        if body.get("clientSecret") != "s3cret" or body.get("scope") != "api.read-only":
            return web.json_response({"stat": "fail", "code": 401, "message": "Unauthorized"})
        return web.json_response({"stat": "ok", "response": {"accessToken": f"tok{calls['grant']}", "expiresIn": 172800}})

    async def wan(req):
        if req.query.get("accessToken") != f"tok{calls['grant']}":
            return web.json_response({"stat": "fail", "code": 401, "message": "Unauthorized"})
        return web.json_response({"stat": "ok", "response": WAN_RESP})

    app = web.Application()
    app.router.add_post("/api/auth.token.grant", grant)
    app.router.add_get("/api/status.wan.connection", wan)
    srv = await aiohttp_server(app)
    return srv, calls


async def test_client_grants_and_regrants(fake_router):
    srv, calls = fake_router
    c = PeplinkClient(f"{srv.host}:{srv.port}", "id", "s3cret", scheme="http")
    r = await c.get("status.wan.connection")
    assert r["order"] == [1, 2, 3] and calls["grant"] == 1
    c._token = "stale"                      # router revoked/expired the token
    await c.get("status.wan.connection")
    assert calls["grant"] == 2
    await c.close()


async def test_client_bad_secret(fake_router):
    from aetherrv.adapters.peplink import PeplinkError
    srv, _ = fake_router
    c = PeplinkClient(f"{srv.host}:{srv.port}", "id", "wrong", scheme="http")
    with pytest.raises(PeplinkError):
        await c.get("status.wan.connection")
    await c.close()



def test_starlink_obstruction_map_encoding():
    import base64
    m = encode_obstruction_map(2, 3, [-1.0, 0.0, 1.0, 0.5, 1.7, -0.2], 10.0, 80.0, 2)
    assert (m["rows"], m["cols"], m["frame"], m["min_el"], m["max_theta"]) == (2, 3, "dish", 10.0, 80.0)
    assert list(base64.b64decode(m["data"])) == [255, 0, 254, 127, 254, 255]
    assert encode_obstruction_map(1, 1, [1.0], frame=1)["frame"] == "earth"
    with pytest.raises(ValueError):
        encode_obstruction_map(2, 2, [1.0])


@pytest.fixture
async def fake_router_login(aiohttp_server):
    calls = {"login": 0}

    async def login(req):
        body = await req.json()
        calls["login"] += 1
        if body.get("password") != "ro-pass":
            return web.json_response({"stat": "fail", "code": 401, "message": "Unauthorized"})
        perm = {"GET": 1, "POST": 1} if body.get("username") == "admin" else {"GET": 1}
        resp = web.json_response({"stat": "ok", "response": {"permission": perm}})
        resp.set_cookie("bauth", f"s{calls['login']}")
        return resp

    async def wan(req):
        if req.cookies.get("bauth") != f"s{calls['login']}":
            return web.json_response({"stat": "fail", "code": 401, "message": "Unauthorized"})
        return web.json_response({"stat": "ok", "response": WAN_RESP})

    app = web.Application()
    app.router.add_post("/api/login", login)
    app.router.add_get("/api/status.wan.connection", wan)
    return await aiohttp_server(app), calls


async def test_readonly_user_login_and_relogin(fake_router_login):
    srv, calls = fake_router_login
    c = PeplinkClient(f"{srv.host}:{srv.port}", scheme="http", username="viewer", password="ro-pass")
    assert (await c.get("status.wan.connection"))["order"] == [1, 2, 3] and calls["login"] == 1
    c._session.cookie_jar.clear()               # session expired on the router
    await c.get("status.wan.connection")
    assert calls["login"] == 2
    await c.close()


async def test_refuses_account_with_write_permission(fake_router_login):
    from aetherrv.adapters.peplink import PeplinkError
    srv, _ = fake_router_login
    c = PeplinkClient(f"{srv.host}:{srv.port}", scheme="http", username="admin", password="ro-pass")
    with pytest.raises(PeplinkError, match="write"):
        await c.get("status.wan.connection")
    await c.close()


class FakeDish:
    """Stand-in for DishOps: scripted status reads, counts reboot sends."""
    def __init__(self, statuses, reboot_exc=None):
        self.statuses, self.reboots, self.reboot_exc = list(statuses), 0, reboot_exc

    def status(self):
        s = self.statuses.pop(0) if len(self.statuses) > 1 else self.statuses[0]
        if isinstance(s, Exception):
            raise s
        return s

    def reboot(self):
        self.reboots += 1
        if self.reboot_exc:
            raise self.reboot_exc


def _sl(ops, enabled=True):
    from aetherrv.adapters.starlink import StarlinkAdapter
    from aetherrv.state import State
    from aetherrv.txgate import TxGate
    return StarlinkAdapter(State(), gate=TxGate({"starlink": enabled}), ops=ops)


PENDING = {"software_update_state": "REBOOT_REQUIRED", "uptime": 5000}


async def test_starlink_install_blocked_without_tx_enabled():
    ops = FakeDish([PENDING])
    a = _sl(ops, enabled=False)
    r = await a.command("install_update", client="test", poll=0)
    assert not r["ok"] and ops.reboots == 0 and a.state.get("starlink.controls_ok") is False


async def test_starlink_install_rejects_other_commands_and_no_pending_update():
    ops = FakeDish([{"software_update_state": "IDLE", "uptime": 5000}])
    a = _sl(ops)
    assert not (await a.command("reboot", client="test", poll=0))["ok"]
    r = await a.command("install_update", client="test", poll=0)
    assert not r["ok"] and "no update" in r["detail"] and ops.reboots == 0


async def test_starlink_install_sends_once_and_verifies_uptime_reset():
    ops = FakeDish([PENDING, {"software_update_state": "REBOOT_REQUIRED", "uptime": 5003},
                    {"software_update_state": "IDLE", "uptime": 12}])
    a = _sl(ops)
    r = await a.command("install_update", client="test", poll=0)
    assert r["ok"] and ops.reboots == 1 and a.state.get("starlink.restart_requested")


async def test_starlink_install_send_error_is_not_retried():
    ops = FakeDish([PENDING, OSError("unreachable")], reboot_exc=RuntimeError("deadline"))
    a = _sl(ops)
    r = await a.command("install_update", client="test", poll=0, verify_timeout=1)
    assert not r["ok"] and ops.reboots == 1


async def test_starlink_install_unverified_when_dish_never_restarts():
    ops = FakeDish([PENDING])
    a = _sl(ops)
    r = await a.command("install_update", client="test", poll=0.01, verify_timeout=0.05)
    assert not r["ok"] and "did not restart" in r["detail"] and ops.reboots == 1


def test_starlink_alerts_quiet_after_requested_restart():
    import time as _t
    from aetherrv.alerts import DEFAULTS, r_starlink
    from aetherrv.state import State
    s = State(); s.set("starlink.reachable", False)
    assert list(r_starlink(s, DEFAULTS))
    s.set("starlink.restart_requested", _t.time())
    assert list(r_starlink(s, DEFAULTS)) == []


def test_peplink_bandwidth_per_wan():
    from aetherrv.adapters.peplink import normalize_bandwidth
    r = {"bandwidth": {"timestamp": 1, "unit": "kbps",
                       "1": {"name": "Starlink", "overall": {"download": 8, "upload": 13}},
                       "4": {"name": "WWAN on 5 GHz", "overall": {"download": 497, "upload": 38}}},
         "lifetime": {"unit": "MB"}}
    assert normalize_bandwidth(r) == {1: {"down_mbps": 0.008, "up_mbps": 0.013}, 4: {"down_mbps": 0.497, "up_mbps": 0.038}}
    assert normalize_bandwidth({}) == {}


def test_starlink_signal_quality_to_snr():
    from types import SimpleNamespace
    from aetherrv.adapters.starlink import signal_fields
    d = signal_fields(SimpleNamespace(signal_quality=0.68, is_snr_above_noise_floor=True, is_snr_persistently_low=False))
    assert (d["starlink.signal_quality"], d["starlink.snr_db"], d["starlink.snr_capped"]) == (0.68, 6.8, False)
    d = signal_fields(SimpleNamespace(signal_quality=1.0, is_snr_above_noise_floor=True, is_snr_persistently_low=False))
    assert d["starlink.snr_db"] == 10.0 and d["starlink.snr_capped"] is True
    assert signal_fields(None)["starlink.snr_db"] is None


ROUTER_PRIO = {"1": {"name": "Starlink", "group": 1, "priority": 3, "enable": True},
               "4": {"name": "WWAN on 5 GHz", "group": 1, "priority": 1, "enable": True},
               "5": {"name": "VLAN WAN 1", "priority": 1, "enable": False}, "order": [1, 4, 5]}   # real router shape


def test_peplink_priority_payload_strict():
    import pytest
    from aetherrv.adapters.peplink import priority_payload
    assert priority_payload(ROUTER_PRIO, 1, 1) == {"instantActive": True, "list": [{"connId": 1, "priority": 1}]}
    for bad in ({}, {"order": []}, {"list": [{"connId": 1}]}, {"order": ["1"]}, []):
        with pytest.raises(ValueError):
            priority_payload(bad, 1, 1)
    for conn, prio in ((9, 1), (5, 2), (1, 5)):                 # unknown, disabled on the router, out of range
        with pytest.raises(ValueError):
            priority_payload(ROUTER_PRIO, conn, prio)


class _FakeWriter:
    def __init__(self, prio, apply=True, post_raises=None, raw=None, disabled=()):
        self.prio, self.apply, self.post_raises, self.posts, self.raw = dict(prio), apply, post_raises, [], raw
        self.en = {k: k not in disabled for k in self.prio}

    def _cfg(self):
        return {"order": list(self.prio), **{str(k): {"name": f"W{k}", "group": 1, "enable": self.en[k], "priority": p}
                                              for k, p in self.prio.items()}}

    async def get(self, func, **_):
        if func == "config.wan.connection.priority":
            return self.raw if self.raw is not None else self._cfg()
        return self._cfg()

    async def post_once(self, func, body):
        self.posts.append(body)
        if self.apply:
            for e in body["list"]:
                if "priority" in e:
                    self.prio[e["connId"]] = e["priority"]
                if "enable" in e:
                    self.en[e["connId"]] = e["enable"]
        if self.post_raises:
            raise self.post_raises


def _pep(writer, enabled=True):
    from aetherrv.adapters.peplink import PeplinkAdapter
    from aetherrv.state import State
    from aetherrv.txgate import TxGate

    class RO:
        async def get(self, func, **_):
            return await writer.get(func)
    return PeplinkAdapter(State(), RO(), writer=writer, gate=TxGate({"peplink": enabled}))


def test_peplink_set_priority_sends_once_and_verifies():
    import asyncio
    w = _FakeWriter({1: 3, 4: 1})
    a = _pep(w)
    assert a.state.get("peplink.controls_ok") is True
    r = asyncio.run(a.command("set_wan_priority", {"id": 1, "priority": 1}, poll=0.01, verify_timeout=1))
    assert r["ok"] and w.posts == [{"instantActive": True, "list": [{"connId": 1, "priority": 1}]}] and "P1" in r["detail"]


def test_peplink_set_priority_refusals_send_nothing():
    import asyncio
    w = _FakeWriter({1: 3})
    assert not asyncio.run(_pep(w, enabled=False).command("set_wan_priority", {"id": 1, "priority": 1}))["ok"]
    assert not asyncio.run(_pep(w).command("reboot", None))["ok"]
    assert not asyncio.run(_pep(w).command("set_wan_priority", {"id": 1}))["ok"]
    assert not asyncio.run(_pep(_FakeWriter({1: 3}, raw={"weird": 1, "list": "x"})).command("set_wan_priority", {"id": 1, "priority": 1}))["ok"]
    assert asyncio.run(_pep(w).command("set_wan_priority", {"id": 1, "priority": 3}))["detail"] == "already P3"
    assert w.posts == []


def test_peplink_set_priority_unconfirmed_is_not_retried():
    import asyncio
    w = _FakeWriter({1: 3}, apply=False, post_raises=RuntimeError("timeout"))
    r = asyncio.run(_pep(w).command("set_wan_priority", {"id": 1, "priority": 1}, poll=0.01, verify_timeout=.05))
    assert not r["ok"] and len(w.posts) == 1


def test_peplink_write_needs_explicit_writer():
    import asyncio, pytest
    from aetherrv.adapters.peplink import PeplinkClient, PeplinkError
    ro = PeplinkClient("192.0.2.1", username="ro", password="x")
    with pytest.raises(PeplinkError):
        asyncio.run(ro.post_once("config.wan.connection.priority", {}))      # monitoring client can never write
    api_ro = PeplinkClient("192.0.2.1", "id", "sec", allow_write=True)        # read-only scope
    with pytest.raises(PeplinkError):
        asyncio.run(api_ro.post_once("config.wan.connection.priority", {}))


def test_peplink_wifi_wan_fields():
    from aetherrv.adapters.peplink import normalize_wans
    resp = {"order": [3, 4],
            "3": {"name": "WWAN on 2.4 GHz", "enable": True, "statusLed": "flash", "message": "Scanning...", "type": "wifi"},
            "4": {"name": "WWAN on 5 GHz", "enable": True, "statusLed": "green", "message": "Connected to PTP",
                  "type": "wifi", "wifi": {"signal": {"strength": 0, "level": 5}, "ssid": "PTP", "bssid": "x", "channel": 149}}}
    w3, w4 = normalize_wans(resp)
    assert w4["wifi"] == {"ssid": "PTP", "strength": 0, "level": 5, "channel": 149, "band": "5 GHz"}
    assert "bssid" not in w4["wifi"]
    assert w3["wifi"]["ssid"] is None and w3["wifi"]["band"] is None          # scanning: still marked as Wi-Fi


def test_peplink_layout_targets_strict():
    import pytest
    from aetherrv.adapters.peplink import layout_targets, layout_changes, layout_payload
    cur = {"order": [1, 2, 4, 5], **{str(c): {"name": f"W{c}", "priority": p, "enable": c != 5}
                                     for c, p in ((1, 3), (2, 2), (4, 1), (5, 1))}}
    T = layout_targets(cur, [4, 1, 2])                              # disabled omitted: 5 stays disabled
    assert T == {4: {"enable": True, "priority": 1}, 1: {"enable": True, "priority": 2},
                 2: {"enable": True, "priority": 3}, 5: {"enable": False, "priority": None}}
    assert layout_payload(cur, layout_changes(cur, T)) == {"instantActive": True, "list": [
        {"connId": 1, "priority": 2}, {"connId": 2, "priority": 3}]}
    # enable 5 at P2, disable 2
    T = layout_targets(cur, [4, 5, 1], [2])
    assert layout_payload(cur, layout_changes(cur, T))["list"] == [
        {"connId": 5, "priority": 2, "enable": True}, {"connId": 2, "enable": False}]       # 1 is already P3: not sent
    for bad in (([4, 1], None), ([4, 1, 2, 5], None), ([4, 4, 1, 2], None), ([], [1, 2, 4, 5]), ([4, 1, 9], None),
                ([4, 1], [5]), ([4, 1, 2], [5, 2])):
        with pytest.raises(ValueError):
            layout_targets(cur, *bad)
    # a hidden, already-disabled connection (5) may be omitted: it stays disabled, nothing sent for it
    T = layout_targets(cur, [4, 1], [2])
    assert T[5] == {"enable": False, "priority": None} and 5 not in layout_changes(cur, T)


def test_peplink_set_order_one_post_only_changed():
    import asyncio
    w = _FakeWriter({1: 3, 2: 2, 4: 1})
    r = asyncio.run(_pep(w).command("set_wan_order", {"order": [4, 1, 2]}, poll=0.01, verify_timeout=1))
    assert r["ok"] and len(w.posts) == 1
    assert w.posts[0] == {"instantActive": True, "list": [{"connId": 1, "priority": 2}, {"connId": 2, "priority": 3}]}
    assert asyncio.run(_pep(w).command("set_wan_order", {"order": [4, 1, 2]}))["detail"] == "already in that order"
    assert len(w.posts) == 1


def test_peplink_drag_disable_and_enable():
    import asyncio
    w = _FakeWriter({1: 3, 2: 2, 4: 1, 5: 1}, disabled=(5,))
    r = asyncio.run(_pep(w).command("set_wan_order", {"order": [4, 2], "disabled": [1, 5]}, poll=0.01, verify_timeout=1))
    assert r["ok"] and w.posts[-1]["list"] == [{"connId": 1, "enable": False}] and w.en[1] is False
    r = asyncio.run(_pep(w).command("set_wan_order", {"order": [4, 1, 2], "disabled": [5]}, poll=0.01, verify_timeout=1))
    assert r["ok"] and w.posts[-1]["list"] == [{"connId": 1, "priority": 2, "enable": True}, {"connId": 2, "priority": 3}]
    n = len(w.posts)
    r = asyncio.run(_pep(w).command("set_wan_order", {"order": [], "disabled": [1, 2, 4, 5]}))
    assert not r["ok"] and len(w.posts) == n                       # never disable everything
