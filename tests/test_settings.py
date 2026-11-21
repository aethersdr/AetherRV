import asyncio
import importlib.util
import json
import os
from pathlib import Path

import pytest
from aiohttp.test_utils import TestClient, TestServer

from aetherrv.settings import AdminClient, AutoTimezone, Prefs
from aetherrv.state import State
from aetherrv.web import make_app

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("aetherrv_admin", ROOT / "deploy/admin/aetherrv_admin.py")
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)


# ── root helper: argument validation (no command ever runs with a bad value) ──────────────────
def test_helper_rejects_bad_hostnames_and_zones(monkeypatch):
    calls = []
    monkeypatch.setattr(helper, "run", lambda argv, **k: calls.append(argv) or (0, "", ""))
    for bad in ["", "-x", "a b", "x;reboot", "$(id)", "a" * 70, "bad_name"]:
        with pytest.raises(helper.Bad):
            helper.v_set_hostname({"name": bad})
    for bad in ["", "../../etc/passwd", "America/../../etc", "Not/AZone", "UTC;id", "/etc/localtime"]:
        with pytest.raises(helper.Bad):
            helper.v_set_timezone({"tz": bad})
    assert calls == []                                            # nothing executed
    helper.v_set_hostname({"name": "testpi"})
    helper.v_set_timezone({"tz": "America/Los_Angeles"})
    assert calls == [["hostnamectl", "set-hostname", "testpi"], ["timedatectl", "set-timezone", "America/Los_Angeles"]]


def test_helper_unknown_verb_and_peer_check(tmp_path, monkeypatch):
    import socket
    import threading
    path = str(tmp_path / "s.sock")
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(path); srv.listen(2)
    monkeypatch.setattr(helper, "ALLOWED_USERS", {"nobody-at-all"})          # the test user is NOT allowed
    def serve():
        c, _ = srv.accept(); helper.handle(c)
    t = threading.Thread(target=serve); t.start()
    c = socket.socket(socket.AF_UNIX); c.connect(path)
    c.sendall(json.dumps({"verb": "reboot"}).encode() + b"\n")
    reply = json.loads(c.makefile().readline()); t.join()
    if os.getuid() != 0:
        assert reply == {"ok": False, "error": "not allowed"}
    monkeypatch.setattr(helper, "ALLOWED_USERS", {__import__("pwd").getpwuid(os.getuid()).pw_name})
    t = threading.Thread(target=serve); t.start()
    c = socket.socket(socket.AF_UNIX); c.connect(path)
    c.sendall(json.dumps({"verb": "rm_rf", "args": {}}).encode() + b"\n")
    assert json.loads(c.makefile().readline()) == {"ok": False, "error": "unknown verb"}
    t.join()


# ── prefs ────────────────────────────────────────────────────────────────────
def test_prefs_validate_persist_publish(tmp_path):
    st = State()
    p = Prefs(st, str(tmp_path / "settings.json"))
    assert st.get("settings.screen_off_min") == 10
    p.set("screen_off_min", 30)
    with pytest.raises(ValueError):
        p.set("screen_off_min", 999)
    with pytest.raises(ValueError):
        p.set("rm", 1)
    assert Prefs(State(), str(tmp_path / "settings.json")).values["screen_off_min"] == 30


# ── auto time zone from GPS ──────────────────────────────────────────────────
class FakeAdmin:
    def __init__(self): self.calls = []
    async def call(self, verb, args=None, timeout=60):
        self.calls.append((verb, args)); return {"ok": True}


async def test_auto_timezone_needs_fix_and_two_agreeing_lookups(tmp_path, monkeypatch):
    st, adm = State(), FakeAdmin()
    tz = AutoTimezone(st, adm, Prefs(st, str(tmp_path / "s.json")))
    monkeypatch.setattr(AutoTimezone, "current", staticmethod(lambda: "America/Los_Angeles"))
    answers = iter(["America/Denver"] * 5)
    async def lookup(lat, lon): return next(answers)
    tz.lookup = lookup
    monkeypatch.setattr("time.tzset", lambda: None)
    st.update({"location.lat": 46.6, "location.lon": -112.0, "location.fix": False})
    await tz.step(); assert adm.calls == []                          # no fix → nothing
    st.set("location.fix", True)
    await tz.step(); assert adm.calls == []                          # first answer only arms it
    await tz.step(); assert adm.calls == [("set_timezone", {"tz": "America/Denver"})]


async def test_auto_timezone_offline_keeps_zone(tmp_path, monkeypatch):
    st, adm = State(), FakeAdmin()
    tz = AutoTimezone(st, adm, Prefs(st, str(tmp_path / "s.json")))
    async def lookup(lat, lon): raise asyncio.TimeoutError()
    tz.lookup = lookup
    st.update({"location.lat": 46.6, "location.lon": -112.0, "location.fix": True})
    await tz.step(); await tz.step()
    assert adm.calls == []


# ── web: changes only from loopback, unknown verbs refused ───────────────────
async def test_settings_actions_loopback_only(tmp_path):
    st, adm = State(), FakeAdmin()
    app = make_app(st, tmp_path, admin=adm, prefs=Prefs(st, str(tmp_path / "s.json")))
    async with TestClient(TestServer(app, host="127.0.0.1")) as c:
        r = await c.post("/api/settings/action", json={"verb": "reboot"})
        assert (await r.json())["ok"] and adm.calls[-1] == ("reboot", {})
        r = await c.post("/api/settings/action", json={"verb": "set_timezone", "args": {"tz": "UTC"}})
        assert r.status == 403                                       # time zone is automatic only
        r = await c.post("/api/settings/action", json={"verb": "pref", "args": {"key": "screen_off_min", "value": 5}})
        assert (await r.json())["ok"] and st.get("settings.screen_off_min") == 5
    async with TestClient(TestServer(app, host="127.0.0.1")) as c:
        # Simulate a LAN viewer: aiohttp's request.remote comes from the transport; patch it.
        from aiohttp import web
        orig = web.BaseRequest.remote
        web.BaseRequest.remote = property(lambda self: "192.0.2.20")
        try:
            r = await c.post("/api/settings/action", json={"verb": "reboot"})
            assert r.status == 403
        finally:
            web.BaseRequest.remote = orig


# ── Wi-Fi via iwd (fake D-Bus) ──────────────────────────────────────────────
def _fake_iwd(monkeypatch, tmp_path, connect_rc=0):
    net = "/net/connman/iwd/0/3/4e6574_psk"
    objs = {"/net/connman/iwd/0/3": {"net.connman.iwd.Device": {"Name": {"data": "wld0"}, "Powered": {"data": True}},
                                     "net.connman.iwd.Station": {"State": {"data": "disconnected"}}},
            net: {"net.connman.iwd.Network": {"Name": {"data": "Van Wifi"}, "Type": {"data": "psk"}}}}
    calls = []
    def bus(*args, timeout=30):
        calls.append(args)
        if args[-1] == "Connect":
            return connect_rc, "", "Call failed: net.connman.iwd.Failed" if connect_rc else ""
        return 0, "", ""
    monkeypatch.setattr(helper, "iwd_objects", lambda: objs)
    monkeypatch.setattr(helper, "bus", bus)
    monkeypatch.setattr(helper, "IWD_DIR", str(tmp_path))
    monkeypatch.setattr(helper.time, "sleep", lambda s: None)
    return calls


def test_wifi_psk_vector_and_file(monkeypatch, tmp_path):
    import hashlib
    # IEEE 802.11i-2004 Annex H.4 test vector
    assert hashlib.pbkdf2_hmac("sha1", b"password", b"IEEE", 4096, 32).hex() == \
        "f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e"
    assert helper.iwd_file("Van Wifi", "psk").endswith("/Van Wifi.psk")
    assert helper.iwd_file("café!", "psk").endswith("/=" + "café!".encode().hex() + ".psk")
    calls = _fake_iwd(monkeypatch, tmp_path)
    helper.v_wifi_connect({"ssid": "Van Wifi", "passphrase": "correct horse"})
    f = tmp_path / "Van Wifi.psk"
    assert oct(f.stat().st_mode & 0o777) == "0o600"
    expect = hashlib.pbkdf2_hmac("sha1", b"correct horse", b"Van Wifi", 4096, 32).hex()
    assert f.read_text() == f"[Security]\nPassphrase=correct\\shorse\nPreSharedKey={expect}\n"   # \s = space (iwd)
    helper.v_wifi_forget  # (exists)
    f.unlink()
    helper.v_wifi_connect({"ssid": "Van Wifi", "passphrase": " a\\b cdef "})          # 10 chars: ' a\\b cdef '
    assert f.read_text().splitlines()[1] == "Passphrase=\\sa\\\\b\\scdef\\s"
    assert "correct horse" not in repr(calls)                     # never on a command line


def test_wifi_bad_password_and_failed_join(monkeypatch, tmp_path):
    _fake_iwd(monkeypatch, tmp_path)
    for bad in ["short", "x" * 64, "tab\there"]:
        with pytest.raises(helper.Bad):
            helper.v_wifi_connect({"ssid": "Van Wifi", "passphrase": bad})
    assert not list(tmp_path.iterdir())                           # nothing written
    _fake_iwd(monkeypatch, tmp_path, connect_rc=1)
    with pytest.raises(helper.Bad, match="password"):
        helper.v_wifi_connect({"ssid": "Van Wifi", "passphrase": "wrong password"})
    assert not list(tmp_path.iterdir())                           # a wrong password isn't kept
    with pytest.raises(helper.Bad):
        helper.v_wifi_connect({"ssid": "Not Here"})


def test_update_check_separates_held_and_never_syncs_system_db(monkeypatch, tmp_path):
    calls = []
    def run(argv, **k):
        calls.append(argv)
        if argv[:2] == ["pacman", "-Qu"]:
            return 0, ("chromium 158.0.1-1 -> 158.0.2-1\nsway 1:1.12-4 -> 1:1.12-5\n"
                       "linux-rpi 6.18.64-1 -> 6.18.70-1 [ignored]\n"), ""
        return 0, "", ""
    monkeypatch.setattr(helper, "run", run)
    monkeypatch.setattr(helper, "UPDATE_RUN", str(tmp_path / "run"))
    monkeypatch.setattr(helper.os, "symlink", lambda *a: None)
    monkeypatch.setattr(helper.os, "listdir", lambda p: [])
    monkeypatch.setattr(helper.os.path, "islink", lambda p: True)
    real_makedirs = helper.os.makedirs
    monkeypatch.setattr(helper.os, "makedirs", lambda p, exist_ok=False: real_makedirs(str(tmp_path / "x"), exist_ok=True)
                        if p.startswith("/var/tmp") else real_makedirs(p, exist_ok=exist_ok))
    r = helper.v_update_check({})
    assert [u["name"] for u in r["updates"]] == ["chromium", "sway"]
    assert r["held"] == [{"name": "linux-rpi", "old": "6.18.64-1", "new": "6.18.70-1"}]
    syncs = [c for c in calls if "-Sy" in c]
    assert syncs and all("--dbpath" in c and "/var/lib/pacman" not in c[c.index("--dbpath") + 1] for c in syncs)


# ── remote control over Tailscale: owner's own untagged devices only ─────────
class WhoisAdmin:
    def __init__(self, table): self.table, self.calls = table, []
    async def call(self, verb, args=None, timeout=60):
        self.calls.append((verb, args))
        r = self.table.get(args["ip"], {"ok": True, "known": False})
        return {"ok": True, "self_ips": ["100.64.0.10", "fd7a:115c:a1e0::10"], **r}


async def test_remote_auth_owner_only():
    from aetherrv.settings import RemoteAuth
    adm = WhoisAdmin({
        "100.101.1.1": {"known": True, "owner": True, "login": "me@x", "node": "macbook", "tagged": False},
        "100.101.1.2": {"known": True, "owner": False, "login": "friend@y", "node": "phone", "tagged": False},
        "100.101.1.3": {"known": True, "owner": False, "login": "tagged-devices", "node": "srv", "tagged": True},
    })
    a = RemoteAuth(adm)
    assert await a.who("127.0.0.1") == "kiosk"
    assert await a.who("192.0.2.20") is None and adm.calls == []              # LAN: never even asks
    assert await a.who("100.101.1.1", "100.64.0.10") == "tailscale:me@x@macbook"
    assert await a.who("100.101.1.2", "100.64.0.10") is None                  # shared-in user
    assert await a.who("100.101.1.3", "100.64.0.10") is None                  # tagged node
    assert await a.who("100.101.1.1", "192.0.2.143") is None                  # 100.x source, not via tailscale0
    n = len(adm.calls); await a.who("100.101.1.1", "100.64.0.10")
    assert len(adm.calls) == n                                                    # cached


def test_helper_whois_rejects_non_tailscale_ips(monkeypatch):
    monkeypatch.setattr(helper, "run", lambda argv, **k: (_ for _ in ()).throw(AssertionError("must not run")))
    for bad in ["192.0.2.1", "8.8.8.8", "not-an-ip", "", "100.128.0.1"]:
        with pytest.raises(helper.Bad):
            helper.v_tailscale_whois({"ip": bad})
