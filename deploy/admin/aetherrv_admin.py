#!/usr/bin/python3 -I
"""AetherRV admin helper: the ONLY root-privileged piece of AetherRV's settings.

Installed root-owned to /usr/local/lib/aetherrv/aetherrv_admin.py (outside the rsync'd /opt/aetherrv tree)
and socket-activated by aetherrv-admin.socket (/run/aetherrv-admin.sock, root:aetherrv 0660). Stdlib only,
run with `python3 -I`.

Protocol: one JSON object per connection, {"verb": str, "args": {...}} -> one JSON reply {"ok": bool, ...}.
Only uid 0 and the `aetherrv` service user may call (SO_PEERCRED). Verbs are a fixed allowlist; every
argument is validated; commands are argv lists, never shell strings. Secrets (Wi-Fi passphrases) are never
logged, never put on a command line, and never returned.

Design follows the AetherSDR Maestro console (narrow privileges for the kiosk, a one-shot update unit with
its log/state under /run), adapted to AetherRV: iwd + systemd-networkd, Tailscale, pacman.
"""
from __future__ import annotations

import json
import os
import pwd
import re
import socket
import struct
import subprocess
import sys
import threading
import time

ALLOWED_USERS = {"aetherrv"}
ZONEINFO = "/usr/share/zoneinfo"
UPDATE_RUN = "/run/aetherrv-update"


def log(msg: str) -> None:
    print(msg, flush=True)                       # journald


def run(argv: list[str], timeout: float = 30, input_: str | None = None) -> tuple[int, str, str]:
    try:
        p = subprocess.run(argv, capture_output=True, text=True, timeout=timeout, input=input_)
        return p.returncode, p.stdout, p.stderr
    except FileNotFoundError:
        return 127, "", f"{argv[0]} is not installed"
    except subprocess.TimeoutExpired:
        return 124, "", f"{argv[0]} timed out"


class Bad(Exception):
    pass


def _str(args: dict, key: str, pattern: str, maxlen: int = 64) -> str:
    v = args.get(key)
    if not isinstance(v, str) or not v or len(v) > maxlen or not re.fullmatch(pattern, v):
        raise Bad(f"invalid {key}")
    return v


# ── system ────────────────────────────────────────────────────────────────────
def v_status(_a: dict) -> dict:
    _, host, _ = run(["hostnamectl", "hostname"])
    _, tz, _ = run(["timedatectl", "show", "-p", "Timezone", "--value"])
    _, ntp, _ = run(["timedatectl", "show", "-p", "NTPSynchronized", "--value"])
    _, addrs, _ = run(["ip", "-j", "addr"])
    try:
        up = float(open("/proc/uptime").read().split()[0])
    except OSError:
        up = None
    st = os.statvfs("/")
    pk = {}
    for name in ("iwd", "tailscale", "sway", "chromium", "python", "pacman-contrib", "linux-rpi"):
        rc, out, _ = run(["pacman", "-Q", name])
        pk[name] = out.split()[1] if rc == 0 and out.split() else None
    ifs = []
    try:
        for i in json.loads(addrs or "[]"):
            if i.get("ifname") in ("lo",) or i.get("ifname", "").startswith(("vcan", "hat")):
                continue
            ifs.append({"name": i.get("ifname"), "state": i.get("operstate"),
                        "ipv4": [a["local"] for a in i.get("addr_info", []) if a.get("family") == "inet"]})
    except ValueError:
        pass
    return {"hostname": host.strip(), "timezone": tz.strip(), "ntp_synced": ntp.strip() == "yes",
            "kernel": os.uname().release, "uptime_s": up, "interfaces": ifs, "packages": pk,
            "disk": {"total": st.f_blocks * st.f_frsize, "free": st.f_bavail * st.f_frsize}}


def v_set_hostname(a: dict) -> dict:
    name = _str(a, "name", r"[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?")
    rc, _, err = run(["hostnamectl", "set-hostname", name])
    if rc:
        raise Bad(err.strip() or "hostnamectl failed")
    return {"hostname": name}


def v_set_timezone(a: dict) -> dict:
    tz = _str(a, "tz", r"[A-Za-z_]+(/[A-Za-z0-9_+-]+){0,2}")
    path = os.path.realpath(os.path.join(ZONEINFO, tz))
    if not path.startswith(ZONEINFO + "/") or not os.path.isfile(path):
        raise Bad("unknown time zone")
    rc, _, err = run(["timedatectl", "set-timezone", tz])
    if rc:
        raise Bad(err.strip() or "timedatectl failed")
    return {"timezone": tz}


def _later(argv: list[str]) -> None:
    # Reply first, then act (the caller wants its answer before the system goes away).
    threading.Timer(1.5, lambda: run(argv, timeout=60)).start()


def v_reboot(_a: dict) -> dict:
    _later(["systemctl", "reboot"])
    return {"rebooting": True}


def v_poweroff(_a: dict) -> dict:
    _later(["systemctl", "poweroff"])
    return {"powering_off": True}


def v_restart_kiosk(_a: dict) -> dict:
    _later(["systemctl", "restart", "aetherrv-kiosk.service"])
    return {"restarting": True}


# ── Wi-Fi (iwd over D-Bus via busctl; networkd does addresses) ──────────────────
IWD = "net.connman.iwd"
IWD_DIR = "/var/lib/iwd"


def bus(*args: str, timeout: float = 30) -> tuple[int, str, str]:
    return run(["busctl", "--json=short", f"--timeout={int(timeout)}", *args], timeout=timeout + 5)


def _p(props: dict, key: str, default=None):
    v = props.get(key)
    return v.get("data", default) if isinstance(v, dict) else default


def iwd_objects() -> dict:
    rc, out, _ = bus("call", IWD, "/", "org.freedesktop.DBus.ObjectManager", "GetManagedObjects", timeout=10)
    if rc:
        raise Bad("Wi-Fi service (iwd) isn't running")
    return json.loads(out)["data"][0]


def iwd_device(objs: dict) -> tuple[str, dict] | None:
    return next(((pth, ifs) for pth, ifs in sorted(objs.items()) if "net.connman.iwd.Device" in ifs), None)


def _ssid(a: dict) -> str:
    v = a.get("ssid")
    if not isinstance(v, str) or not v or len(v.encode()) > 32 or any(ord(c) < 32 for c in v):
        raise Bad("invalid network name")
    return v


def iwd_file(ssid: str, kind: str) -> str:
    # iwd(5) network file naming: plain if only [A-Za-z0-9 _-], else "=" + hex of the SSID bytes.
    plain = re.fullmatch(r"[A-Za-z0-9 _-]+", ssid)
    return os.path.join(IWD_DIR, (ssid if plain else "=" + ssid.encode().hex()) + "." + kind)


def v_wifi_status(_a: dict) -> dict:
    objs = iwd_objects()
    dev = iwd_device(objs)
    if dev is None:
        return {"present": False}
    path, ifs = dev
    d, st = ifs["net.connman.iwd.Device"], ifs.get("net.connman.iwd.Station", {})
    known = [{"ssid": _p(i["net.connman.iwd.KnownNetwork"], "Name"), "security": _p(i["net.connman.iwd.KnownNetwork"], "Type")}
             for i in objs.values() if "net.connman.iwd.KnownNetwork" in i]
    nets = []
    if st:
        rc, out, _ = bus("call", IWD, path, "net.connman.iwd.Station", "GetOrderedNetworks", timeout=10)
        for npath, sig in (json.loads(out)["data"][0] if rc == 0 else []):
            n = objs.get(npath, {}).get("net.connman.iwd.Network")
            if n:
                nets.append({"ssid": _p(n, "Name"), "security": _p(n, "Type"), "signal_dbm": round(sig / 100),
                             "known": bool(_p(n, "KnownNetwork")), "connected": bool(_p(n, "Connected"))})
    conn = next((n["ssid"] for n in nets if n["connected"]), None)
    return {"present": True, "device": _p(d, "Name"), "powered": bool(_p(d, "Powered")), "state": _p(st, "State"),
            "scanning": bool(_p(st, "Scanning")), "connected": conn, "networks": nets, "known": known}


def v_wifi_scan(_a: dict) -> dict:
    dev = iwd_device(iwd_objects())
    if dev is None:
        raise Bad("no Wi-Fi device")
    bus("call", IWD, dev[0], "net.connman.iwd.Station", "Scan", timeout=10)    # Busy is fine: a scan is running
    return {"scanning": True}


def v_wifi_power(a: dict) -> dict:
    on = a.get("on")
    if not isinstance(on, bool):
        raise Bad("invalid on")
    dev = iwd_device(iwd_objects())
    if dev is None:
        raise Bad("no Wi-Fi device")
    rc, _, err = bus("set-property", IWD, dev[0], "net.connman.iwd.Device", "Powered", "b", "true" if on else "false")
    if rc:
        raise Bad(err.strip() or "couldn't change Wi-Fi power")
    return {"powered": on}


def v_wifi_connect(a: dict) -> dict:
    import hashlib
    ssid = _ssid(a)
    objs = iwd_objects()
    net = next(((pth, i["net.connman.iwd.Network"]) for pth, i in objs.items()
                if "net.connman.iwd.Network" in i and _p(i["net.connman.iwd.Network"], "Name") == ssid), None)
    if net is None:
        raise Bad("that network isn't in range — scan again")
    npath, n = net
    typ = _p(n, "Type")
    if typ == "8021x":
        raise Bad("enterprise (802.1X) Wi-Fi isn't supported")
    created = None
    if typ == "psk" and not _p(n, "KnownNetwork"):
        pw = a.get("passphrase")
        if isinstance(pw, str) and re.fullmatch(r"[0-9A-Fa-f]{64}", pw):
            psk, phrase = pw.lower(), None          # raw PSK: WPA2 only (WPA3/SAE needs the passphrase)
        elif isinstance(pw, str) and 8 <= len(pw) <= 63 and all(32 <= ord(c) < 127 for c in pw):
            psk, phrase = hashlib.pbkdf2_hmac("sha1", pw.encode(), ssid.encode(), 4096, 32).hex(), pw
        else:
            raise Bad("the password must be 8–63 characters")
        fpath = iwd_file(ssid, "psk")
        fd = os.open(fpath, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "w") as f:
            # Passphrase for WPA3/SAE (and transition-mode APs), PreSharedKey for WPA2. iwd's settings
            # format escapes backslash as \\ and space as \s (so leading/trailing spaces survive).
            if phrase is not None:
                f.write("[Security]\nPassphrase=" + phrase.replace("\\", "\\\\").replace(" ", "\\s") + f"\nPreSharedKey={psk}\n")
            else:
                f.write(f"[Security]\nPreSharedKey={psk}\n")
        created = fpath
        for _ in range(30):                     # iwd picks the file up via inotify
            if _p(iwd_objects().get(npath, {}).get("net.connman.iwd.Network", {}), "KnownNetwork"):
                break
            time.sleep(0.1)
    rc, _, err = bus("call", IWD, npath, "net.connman.iwd.Network", "Connect", timeout=40)
    if rc:
        if created:                             # a wrong password must not stay saved
            try:
                os.unlink(created)
            except OSError:
                pass
        e = err.strip()
        raise Bad("couldn't join — check the password" if ("Failed" in e or "InvalidFormat" in e or "Aborted" in e)
                  else (e or "couldn't join"))
    return {"connected": ssid}


def v_wifi_forget(a: dict) -> dict:
    ssid = _ssid(a)
    k = next((pth for pth, i in iwd_objects().items()
              if "net.connman.iwd.KnownNetwork" in i and _p(i["net.connman.iwd.KnownNetwork"], "Name") == ssid), None)
    if k is None:
        raise Bad("not a saved network")
    rc, _, err = bus("call", IWD, k, "net.connman.iwd.KnownNetwork", "Forget", timeout=10)
    if rc:
        raise Bad(err.strip() or "couldn't forget")
    return {"forgot": ssid}


# ── Tailscale ─────────────────────────────────────────────────────────────────
def _ts_status_json() -> dict | None:
    rc, out, _ = run(["tailscale", "status", "--json"], timeout=10)
    try:
        return json.loads(out) if out.strip() else None
    except ValueError:
        return None


def v_tailscale_status(_a: dict) -> dict:
    rc, out, _ = run(["pacman", "-Q", "tailscale"])
    if rc:
        return {"installed": False}
    _, act, _ = run(["systemctl", "is-active", "tailscaled"])
    st = _ts_status_json() or {}
    me = st.get("Self") or {}
    return {"installed": True, "version": out.split()[1] if out.split() else None, "running": act.strip() == "active",
            "state": st.get("BackendState"), "auth_url": st.get("AuthURL") or None,
            "name": me.get("HostName"), "dns": (me.get("DNSName") or "").rstrip(".") or None,
            "ips": me.get("TailscaleIPs") or [], "online": bool(me.get("Online")),
            "tailnet": (st.get("CurrentTailnet") or {}).get("Name"),
            "user": ((st.get("User") or {}).get(str(me.get("UserID"))) or {}).get("LoginName")}


def v_tailscale_install(_a: dict) -> dict:
    # From the already-synced package lists only (never a bare -Sy: no partial upgrades).
    rc, out, err = run(["pacman", "-S", "--needed", "--noconfirm", "tailscale"], timeout=600)
    if rc:
        raise Bad((err or out).strip().splitlines()[-1] if (err or out).strip() else "install failed")
    rc, _, err = run(["systemctl", "enable", "--now", "tailscaled.service"], timeout=60)
    if rc:
        raise Bad(err.strip() or "couldn't start tailscaled")
    return {"installed": True}


def v_tailscale_login(_a: dict) -> dict:
    """Start `tailscale up` in the background (it waits for the browser sign-in) and return the login URL,
    which the console shows as a QR code. No subnet routes / exit node are accepted or advertised."""
    st = _ts_status_json() or {}
    if st.get("BackendState") == "Running":
        return {"state": "Running"}
    _, host, _ = run(["hostnamectl", "hostname"])
    name = re.sub(r"[^a-z0-9-]", "-", host.strip().lower()) or "aetherrv"
    subprocess.Popen(["tailscale", "up", f"--hostname={name}", "--accept-routes=false", "--accept-dns=false"],
                     stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
    for _ in range(40):
        time.sleep(0.5)
        st = _ts_status_json() or {}
        if st.get("AuthURL") or st.get("BackendState") == "Running":
            break
    return {"state": st.get("BackendState"), "auth_url": st.get("AuthURL") or None}


def v_tailscale_whois(a: dict) -> dict:
    """Who is behind a Tailscale address, and is it the account this console is signed in with?
    owner = same user ID as this node AND not a tagged device. Used by AetherRV to allow remote control
    only for the owner's own devices (owner's choice, 2026-11-20)."""
    import ipaddress
    ip = a.get("ip")
    try:
        addr = ipaddress.ip_address(ip if isinstance(ip, str) else "")
    except ValueError:
        raise Bad("invalid ip")
    if not (addr in ipaddress.ip_network("100.64.0.0/10") or addr in ipaddress.ip_network("fd7a:115c:a1e0::/48")):
        raise Bad("not a Tailscale address")
    rc, out, _ = run(["tailscale", "whois", "--json", str(addr)], timeout=10)
    if rc:
        return {"known": False}
    try:
        w = json.loads(out)
    except ValueError:
        return {"known": False}
    st = _ts_status_json() or {}
    me = (st.get("Self") or {}).get("UserID")
    node, prof = w.get("Node") or {}, w.get("UserProfile") or {}
    tagged = bool(node.get("Tags"))
    return {"known": True, "owner": (not tagged) and me is not None and prof.get("ID") == me,
            "login": prof.get("LoginName"), "node": (node.get("Name") or "").split(".")[0], "tagged": tagged,
            "self_ips": (st.get("Self") or {}).get("TailscaleIPs") or []}


def v_tailscale_logout(_a: dict) -> dict:
    rc, _, err = run(["tailscale", "logout"], timeout=30)
    if rc:
        raise Bad(err.strip() or "logout failed")
    return {"logged_out": True}


# ── System updates (pacman; kernel/boot packages held in IgnorePkg) ──────────────
def _read_kv(path: str) -> dict:
    try:
        return dict(l.split("=", 1) for l in open(path).read().splitlines() if "=" in l)
    except OSError:
        return {}


def v_update_check(_a: dict) -> dict:
    """Like checkupdates(8): sync a throwaway copy of the package lists, never the system's (no partial upgrades)."""
    import shutil
    tmp = "/var/tmp/aetherrv-checkupdates"
    os.makedirs(os.path.join(tmp, "sync"), exist_ok=True)
    if not os.path.islink(os.path.join(tmp, "local")):
        os.symlink("/var/lib/pacman/local", os.path.join(tmp, "local"))
    for f in os.listdir("/var/lib/pacman/sync"):
        if f.endswith(".db"):
            shutil.copy2(os.path.join("/var/lib/pacman/sync", f), os.path.join(tmp, "sync", f))
    rc, out, err = run(["pacman", "-Sy", "--dbpath", tmp, "--logfile", "/dev/null", "--noconfirm"], timeout=300)
    if rc:
        raise Bad("couldn't reach the package servers" if "failed retrieving" in (err + out) else (err.strip() or "check failed"))
    rc, out, _ = run(["pacman", "-Qu", "--dbpath", tmp], timeout=60)
    ups, held = [], []
    for line in out.splitlines():
        m = re.match(r"(\S+) (\S+) -> (\S+)( \[ignored\])?", line)
        if m:
            (held if m.group(4) else ups).append({"name": m.group(1), "old": m.group(2), "new": m.group(3)})
    res = {"updates": ups, "held": held, "checked": int(time.time())}
    os.makedirs(UPDATE_RUN, exist_ok=True)
    with open(os.path.join(UPDATE_RUN, "check.json"), "w") as f:
        json.dump(res, f)
    return res


def v_update_start(_a: dict) -> dict:
    if _read_kv(os.path.join(UPDATE_RUN, "state")).get("status") == "running":
        raise Bad("an update is already running")
    rc, _, err = run(["systemctl", "start", "--no-block", "aetherrv-update.service"])
    if rc:
        raise Bad(err.strip() or "couldn't start the update")
    return {"started": True}


def v_update_status(_a: dict) -> dict:
    st = _read_kv(os.path.join(UPDATE_RUN, "state"))
    _, act, _ = run(["systemctl", "is-active", "aetherrv-update.service"])
    if st.get("status") == "running" and act.strip() not in ("active", "activating"):
        st["status"] = "failed"                               # the unit died without finishing
    try:
        log_tail = open(os.path.join(UPDATE_RUN, "log")).read().splitlines()[-40:]
    except OSError:
        log_tail = []
    try:
        check = json.load(open(os.path.join(UPDATE_RUN, "check.json")))
    except (OSError, ValueError):
        check = None
    return {"state": st, "log": log_tail, "check": check, "last": _read_kv("/var/lib/aetherrv-update/last")}


CAN_PORTS = {"hat0", "hat1", "fd0", "fd1"}


def v_can_reset(a: dict) -> dict:
    """Down/up a local CAN port: discards a frame stuck waiting for an ACK (the MCP2515 retries forever)."""
    iface = a.get("iface")
    if iface not in CAN_PORTS:
        raise Bad("invalid iface")
    for st in ("down", "up"):
        rc, _, err = run(["ip", "link", "set", iface, st], timeout=10)
        if rc:
            raise Bad(err.strip() or f"ip link set {st} failed")
    return {"iface": iface}


def v_can_rebind(a: dict) -> dict:
    """Unbind + rebind a CAN port's SPI driver: a full chip re-init. Fixes an MCP2515 that won't enter config mode
    (seen once after a reboot with the FD HAT stacked; `ip link down/up` alone didn't help)."""
    iface = a.get("iface")
    if iface not in CAN_PORTS:
        raise Bad("invalid iface")
    dev = os.path.realpath(f"/sys/class/net/{iface}/device")
    drv = os.path.realpath(os.path.join(dev, "driver"))
    name = os.path.basename(dev)
    if not name.startswith("spi") or not os.path.isdir(drv):
        raise Bad("no SPI device for that interface")
    with open(os.path.join(drv, "unbind"), "w") as f:
        f.write(name)
    time.sleep(0.5)
    with open(os.path.join(drv, "bind"), "w") as f:
        f.write(name)
    time.sleep(2.0)
    run(["networkctl", "reconfigure", iface], timeout=15)
    return {"iface": iface, "device": name}


VERBS = {
    "can_reset": v_can_reset,
    "can_rebind": v_can_rebind,
    "status": v_status,
    "set_hostname": v_set_hostname,
    "set_timezone": v_set_timezone,
    "reboot": v_reboot,
    "poweroff": v_poweroff,
    "restart_kiosk": v_restart_kiosk,
    "wifi_status": v_wifi_status,
    "wifi_scan": v_wifi_scan,
    "wifi_power": v_wifi_power,
    "wifi_connect": v_wifi_connect,
    "wifi_forget": v_wifi_forget,
    "tailscale_status": v_tailscale_status,
    "tailscale_install": v_tailscale_install,
    "tailscale_login": v_tailscale_login,
    "tailscale_logout": v_tailscale_logout,
    "tailscale_whois": v_tailscale_whois,
    "update_check": v_update_check,
    "update_start": v_update_start,
    "update_status": v_update_status,
}
QUIET = {"status", "wifi_status", "wifi_scan", "tailscale_status", "update_status", "tailscale_whois"}                               # read-only verbs: not logged


# ── server ────────────────────────────────────────────────────────────────────
def peer_ok(conn: socket.socket) -> str | None:
    pid, uid, gid = struct.unpack("3i", conn.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("3i")))
    if uid == 0:
        return "root"
    try:
        name = pwd.getpwuid(uid).pw_name
    except KeyError:
        return None
    return name if name in ALLOWED_USERS else None


def handle(conn: socket.socket) -> None:
    with conn:
        conn.settimeout(15)
        who = peer_ok(conn)
        buf = b""
        try:
            while not buf.endswith(b"\n") and len(buf) < 65536:
                chunk = conn.recv(4096)
                if not chunk:
                    break
                buf += chunk
            if who is None:
                reply = {"ok": False, "error": "not allowed"}
            else:
                req = json.loads(buf or b"{}")
                verb, args = req.get("verb"), req.get("args") or {}
                fn = VERBS.get(verb) if isinstance(verb, str) else None
                if fn is None or not isinstance(args, dict):
                    reply = {"ok": False, "error": "unknown verb"}
                else:
                    try:
                        reply = {"ok": True, **fn(args)}
                    except Bad as e:
                        reply = {"ok": False, "error": str(e)}
                    if verb not in QUIET:
                        log(f"{who}: {verb} {'ok' if reply.get('ok') else 'refused: ' + str(reply.get('error'))}")
        except (ValueError, OSError) as e:
            reply = {"ok": False, "error": f"bad request: {e}"}
        try:
            conn.sendall(json.dumps(reply).encode() + b"\n")
        except OSError:
            pass


def main() -> None:
    if os.environ.get("LISTEN_FDS") == "1":                 # socket activation (fd 3)
        srv = socket.socket(fileno=3)
    else:                                                   # manual run, for testing
        path = sys.argv[1] if len(sys.argv) > 1 else "/run/aetherrv-admin.sock"
        if os.path.exists(path):
            os.unlink(path)
        srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        srv.bind(path)
        srv.listen(8)
    idle_exit = float(os.environ.get("AETHERRV_ADMIN_IDLE", "600"))
    last = [time.monotonic()]
    srv.settimeout(5)
    while True:
        try:
            conn, _ = srv.accept()
        except socket.timeout:
            # Socket-activated: exit when idle; systemd restarts us on the next connection.
            if os.environ.get("LISTEN_FDS") and time.monotonic() - last[0] > idle_exit and threading.active_count() == 1:
                return
            continue
        last[0] = time.monotonic()
        threading.Thread(target=handle, args=(conn,), daemon=True).start()


if __name__ == "__main__":
    main()
