"""Golden parity: the Python reference and the Rust system fed the same inputs, /api/state diffed key by key.

Workstation-only dev tool. Both systems listen to the SAME vcan interfaces while real captures from the van are
replayed onto them (so both see identical frames at identical times), and poll the same fake MCS7. Every
tx_enabled is false in both configs: nothing transmits.

    .venv/bin/python harness/parity/parity.py --config <van aetherrv.toml copy> --n2k <hat0 capture> \
        --espar <hat1 capture> [--seconds 90] [--out harness/out/parity/<name>]

Writes <out>/python.json, <out>/rust.json, <out>/diff.json and prints a summary.
"""
import argparse
import http.server
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import tomllib
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BIN = ROOT / "target" / "debug"
FIXTURE = ROOT / "tests" / "fixtures" / "mcs7_status_hw1_1.208.xml"
SERVICES = ["n2k", "espar", "rixens", "victron", "weather", "derive", "alerts", "history", "settings", "power-guard", "gateway"]


def free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


def toml_dump(d: dict, prefix: str = "") -> str:
    """A small TOML writer for the config shapes AetherRV uses."""
    out, tables, arrays = [], [], []
    for k, v in d.items():
        if isinstance(v, dict):
            tables.append((k, v))
        elif isinstance(v, list) and v and all(isinstance(x, dict) for x in v):
            arrays.append((k, v))
        else:
            out.append(f"{k} = {json.dumps(v)}")
    text = "\n".join(out) + ("\n" if out else "")
    for k, v in tables:
        name = f"{prefix}{k}"
        text += f"\n[{name}]\n" + toml_dump(v, name + ".")
    for k, v in arrays:
        for item in v:
            text += f"\n[[{prefix}{k}]]\n" + toml_dump(item, f"{prefix}{k}.")
    return text


def make_config(van: dict, tmp: Path, mcs7_port: int, web_port: int, n2k_if: str, espar_if: str) -> dict:
    c = json.loads(json.dumps(van))
    c["capture"] = False
    c["secrets_file"] = str(tmp / "no-secrets.toml")
    c["history"] = {"path": str(tmp / f"history-{web_port}.sqlite3"), "interval": 30}
    c["web"] = {"host": "127.0.0.1", "port": web_port, "static_dir": str(ROOT / "web"),
                "admin_socket": str(tmp / "no-admin.sock"), "settings_path": str(tmp / f"settings-{web_port}.json")}
    c["bus"] = {"hat0": {"interface": "socketcan", "channel": n2k_if},
                "hat1": {"interface": "socketcan", "channel": espar_if}}
    for name, d in c.get("device", {}).items():
        d["tx_enabled"] = False
        if "diag_tx" in d:
            d["diag_tx"] = False
        kind = d.get("kind")
        if kind == "n2k":
            d["bus"] = "hat0"
            d.pop("backup_bus", None)
        elif kind == "espar":
            d["bus"] = "hat1"
            d["run_state"] = str(tmp / "espar-run.json")
            d["lockout_state"] = str(tmp / "espar-lockout.json")
        elif kind == "rixens_http":
            d["host"] = f"127.0.0.1:{mcs7_port}"
            d["enabled"] = True
            d["min_gap_s"] = 0.2
        elif kind == "victron":
            # Python runs the Victron adapter whatever `enabled` says (only demo mode skips it); both point at nothing
            d["host"], d["port"], d["tls"] = "127.0.0.1", 1, False
        elif kind in ("starlink", "peplink", "epoch_ble", "demo"):
            d["enabled"] = False
        elif kind in ("rixens", "capture"):
            d["bus"] = "none"
    c.setdefault("power", {})["lockout_state"] = str(tmp / "guard-lockout.json")
    c["power"]["rixens_lockout_state"] = str(tmp / "rixens-lockout.json")
    c["gps"] = {"enabled": False}
    c["netmon"] = {"interval": 60}
    c["radar"] = {"dir": str(tmp / "radar"), "socket": str(tmp / "weather.sock")}
    return c


class Mcs7(http.server.BaseHTTPRequestHandler):
    xml = b""

    def do_GET(self):
        if self.path.startswith("/status.xml"):
            self.send_response(200)
            self.send_header("Content-Type", "text/xml")
            self.end_headers()
            self.wfile.write(self.xml)
        else:
            self.send_response(404)
            self.end_headers()

    def log_message(self, *a):
        pass


def replay(path: Path, iface: str, seconds: float, t0: float) -> int:
    """Send a capture's received frames onto a vcan at their recorded pace (python-can, from the repo's .venv)."""
    import can
    bus = can.Bus(interface="socketcan", channel=iface, receive_own_messages=False)
    n, first = 0, None
    for msg in can.CanutilsLogReader(str(path)):
        if msg.is_error_frame or not getattr(msg, "is_rx", True):
            continue
        if first is None:
            first = msg.timestamp
        rel = msg.timestamp - first
        if rel > seconds:
            break
        delay = t0 + rel - time.time()
        if delay > 0:
            time.sleep(delay)
        try:
            bus.send(can.Message(arbitration_id=msg.arbitration_id, data=msg.data, is_extended_id=msg.is_extended_id))
            n += 1
        except can.CanError:
            time.sleep(0.001)
    bus.shutdown()
    return n


def fetch(port: int) -> dict:
    with urllib.request.urlopen(f"http://127.0.0.1:{port}/api/state", timeout=10) as r:
        return json.loads(r.read())


# keys whose values depend on when each system started or polled (Q-P2: compared loosely, see normalize())
TIME_KEYS = re.compile(r"(\.age|_age|age_s|last_seen|\.t$|\.since|\.rate$|\.frames$|\.errors$|\.ids$|\.updated$|"
                       r"run_s$|left_s$|cooldown_left_s$|rebind_at$|uptime|\.checked$|fuel_today)")


def normalize(k: str, v):
    if isinstance(v, float):
        return round(v, 6)
    return v


# fields inside list/dict values that depend on sampling time (e.g. a device's "age"): compared within TIME_TOL
TIME_FIELDS = {"age", "seen", "t", "since", "updated", "last_seen", "age_s"}
TIME_TOL = 1.5


def close(a, b, field: str = "") -> bool:
    if isinstance(a, dict) and isinstance(b, dict):
        return a.keys() == b.keys() and all(close(a[k], b[k], k) for k in a)
    if isinstance(a, list) and isinstance(b, list):
        return len(a) == len(b) and all(close(x, y, field) for x, y in zip(a, b))
    if field in TIME_FIELDS and isinstance(a, (int, float)) and isinstance(b, (int, float)):
        return abs(a - b) <= TIME_TOL
    if isinstance(a, float) or isinstance(b, float):
        return isinstance(a, (int, float)) and isinstance(b, (int, float)) and abs(a - b) <= 1e-9 * max(1, abs(a))
    return a == b


# Owner rule Q-P2 (2026-11-23): an alert may be raised a little earlier or later in one system because the two
# started at different moments (each rule waits its own `for_s` from its first evaluation). In alerts.* the raise
# times (`since`, `t`) may differ by up to ALERT_SKEW_S; ids, severities, titles, details, sources, acks and event
# names must match exactly, in the same order.
ALERT_SKEW_S = 10.0


def alerts_equal(a, b) -> bool:
    if not (isinstance(a, list) and isinstance(b, list) and len(a) == len(b)):
        return False
    for x, y in zip(a, b):
        if not (isinstance(x, dict) and isinstance(y, dict) and x.keys() == y.keys()):
            return False
        for k in x:
            if k in ("since", "t"):
                if not (isinstance(x[k], (int, float)) and isinstance(y[k], (int, float)) and abs(x[k] - y[k]) <= ALERT_SKEW_S):
                    return False
            elif x[k] != y[k]:
                return False
    return True


# keys the Rust system adds on purpose (ARCHITECTURE.md §13a D7)
RUST_ONLY_OK = re.compile(r"^(svc\.|tx\.(enabled|blocked)\.|climate\.(espar|rixens)\.lockout)")


def diff(py: dict, rs: dict) -> dict:
    only_py = sorted(k for k in py if k not in rs)
    only_rs = sorted(k for k in rs if k not in py)
    same, differ, loose = [], [], []
    for k in sorted(set(py) & set(rs)):
        a, b = normalize(k, py[k]), normalize(k, rs[k])
        if a == b or close(a, b):
            same.append(k)
        elif k in ("alerts.active", "alerts.log") and alerts_equal(a, b):
            loose.append({"key": k, "python": a, "rust": b, "rule": f"raise times within {ALERT_SKEW_S:.0f} s (Q-P2)"})
        elif TIME_KEYS.search(k):
            loose.append({"key": k, "python": a, "rust": b})
        else:
            differ.append({"key": k, "python": a, "rust": b})
    unexpected_rs = [k for k in only_rs if not RUST_ONLY_OK.search(k)]
    return {"same": len(same), "only_python": only_py, "only_rust": only_rs, "only_rust_unexpected": unexpected_rs,
            "differ": differ, "time_dependent": loose}


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--config", required=True)
    ap.add_argument("--n2k", required=True)
    ap.add_argument("--espar", required=True)
    ap.add_argument("--seconds", type=float, default=90)
    ap.add_argument("--n2k-if", default="vcan10")
    ap.add_argument("--espar-if", default="vcan11")
    ap.add_argument("--out", default=str(ROOT / "harness/out/parity/run"))
    # When both systems are read after the replay. Not 3 s: that is the decoders' EasyStart command expiry, and
    # Python recomputes every 1 s, Rust every 250 ms, so a read on that edge compares sampling phase, not behaviour.
    ap.add_argument("--settle", type=float, default=1.0)
    a = ap.parse_args()
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    tmp = Path(tempfile.mkdtemp(prefix="parity-"))
    van = tomllib.loads(Path(a.config).read_text())
    procs = []
    try:
        Mcs7.xml = FIXTURE.read_bytes()
        mport = free_port()
        srv = http.server.ThreadingHTTPServer(("127.0.0.1", mport), Mcs7)
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        py_port, rs_port = free_port(), free_port()
        (tmp / "py.toml").write_text(toml_dump(make_config(van, tmp, mport, py_port, a.n2k_if, a.espar_if)))
        (tmp / "rs.toml").write_text(toml_dump(make_config(van, tmp, mport, rs_port, a.n2k_if, a.espar_if)))
        # the Rust bus
        sock = tmp / "mqtt.sock"
        (tmp / "mosq.conf").write_text(f"listener 0 {sock}\nallow_anonymous true\nlog_dest none\npersistence false\n")
        procs.append(subprocess.Popen(["mosquitto", "-c", str(tmp / "mosq.conf")]))
        for _ in range(50):
            if sock.exists():
                break
            time.sleep(0.1)
        env = dict(os.environ, AETHERRV_BUS=str(sock), RUST_LOG="warn")
        log = open(out / "rust.log", "w")
        for s in SERVICES:
            procs.append(subprocess.Popen([str(BIN / f"aetherrv-{s}"), "-c", str(tmp / "rs.toml")], env=env, stdout=log, stderr=log))
        pyenv = dict(os.environ, PYTHONPATH=str(ROOT / "src"))
        procs.append(subprocess.Popen([sys.executable, "-m", "aetherrv", "-c", str(tmp / "py.toml")], env=pyenv,
                                      stdout=open(out / "python.log", "w"), stderr=subprocess.STDOUT))
        time.sleep(6)
        t0 = time.time() + 1
        res = {}
        th = [threading.Thread(target=lambda k=k, p=p, i=i: res.__setitem__(k, replay(Path(p), i, a.seconds, t0)))
              for k, p, i in (("n2k", a.n2k, a.n2k_if), ("espar", a.espar, a.espar_if))]
        for t in th:
            t.start()
        for t in th:
            t.join()
        time.sleep(a.settle)
        py, rs = fetch(py_port), fetch(rs_port)
        (out / "python.json").write_text(json.dumps(py, indent=1, sort_keys=True))
        (out / "rust.json").write_text(json.dumps(rs, indent=1, sort_keys=True))
        d = diff(py, rs)
        d["replayed"] = res
        (out / "diff.json").write_text(json.dumps(d, indent=1))
        print(f"replayed {res}; keys: python {len(py)}, rust {len(rs)}; identical {d['same']}; "
              f"only python {len(d['only_python'])}; only rust {len(d['only_rust'])} "
              f"({len(d['only_rust_unexpected'])} not in D7); differ {len(d['differ'])}; "
              f"time-dependent {len(d['time_dependent'])}")
    finally:
        for p in procs:
            p.send_signal(signal.SIGTERM)
        time.sleep(1)
        for p in procs:
            if p.poll() is None:
                p.kill()
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    main()
