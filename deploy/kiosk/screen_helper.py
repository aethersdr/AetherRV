#!/usr/bin/env python3
"""Screen power helper, run by sway inside the kiosk session (so it can talk to sway's IPC socket).

    GET  http://127.0.0.1:8091/screen        → {"on": true|false}
    POST http://127.0.0.1:8091/screen/off    → powers the HDMI output off (backlight off on the Waveshare panel)
    POST http://127.0.0.1:8091/screen/on     → powers it back on

Loopback only; responses allow the console origin (http://127.0.0.1:8080) to read them.
The Waveshare WSD060 panel has no DDC/CI or sysfs backlight, so HDMI power is the only software switch.
"""
import json
import subprocess
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ORIGIN = "http://127.0.0.1:8080"


def sway(*args: str) -> str:
    return subprocess.run(["swaymsg", *args], capture_output=True, text=True, timeout=5).stdout


def outputs() -> list[dict]:
    try:
        return json.loads(sway("-t", "get_outputs", "-r"))
    except Exception:
        return []


def is_on() -> bool:
    outs = outputs()
    return any(o.get("power", True) for o in outs) if outs else True


def drm_on(name: str) -> bool | None:
    """The kernel's view of the connector (sway can report success while the panel is still off)."""
    import glob
    for f in glob.glob(f"/sys/class/drm/card*-{name}/dpms"):
        try:
            return open(f).read().strip() == "On"
        except OSError:
            pass
    return None


def set_power(on: bool) -> None:
    # Address outputs by name: in sway 1.12 "output * power on" does not wake an output that is powered off.
    # Observed on the Pi 5 (sway 1.12, vc4): after "power off", the FIRST "power on" returns success but leaves the
    # HDMI connector off; the second one works. So for "on", verify against DRM and repeat (max 4 tries).
    import time
    for o in outputs():
        name = o["name"]
        if not on:
            sway("output", name, "power", "off")
            continue
        for _ in range(4):
            sway("output", name, "power", "on")
            time.sleep(0.4)
            if drm_on(name) is not False:
                break


class H(BaseHTTPRequestHandler):
    def _send(self, code: int, body: dict) -> None:
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Access-Control-Allow-Origin", ORIGIN)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self) -> None:
        self._send(200, {"on": is_on()}) if self.path == "/screen" else self._send(404, {"error": "not found"})

    def do_POST(self) -> None:
        if self.path not in ("/screen/on", "/screen/off"):
            return self._send(404, {"error": "not found"})
        set_power(self.path.endswith("/on"))
        self._send(200, {"on": is_on()})

    def log_message(self, *a) -> None:
        pass


def exit_with_sway() -> None:
    """sway's children live in the login session scope, so a kiosk restart can orphan this helper; an orphan would
    hold port 8091 while talking to a dead sway. Exit as soon as our sway socket is gone or we are reparented."""
    import os
    import threading
    import time
    sock = os.environ.get("SWAYSOCK", "")

    def watch() -> None:
        while True:
            time.sleep(2)
            if os.getppid() == 1 or (sock and not os.path.exists(sock)):
                os._exit(0)
    threading.Thread(target=watch, daemon=True).start()


def wake_on_touch() -> None:
    """While the output is powered off, sway doesn't deliver touches to Chromium, so the page can't wake the screen.
    Read the touch panel's evdev node directly (read-only, no grab: sway still gets every event) and power on at
    the first touch-down while the screen is off. The page keeps its black shield up a moment longer so the waking
    touch never presses a button."""
    import glob
    import os
    import struct
    import threading
    import time
    EV_KEY, BTN_TOUCH, EV_ABS = 0x01, 0x14A, 0x03
    fmt = "llHHi"; size = struct.calcsize(fmt)

    def watch(path: str) -> None:
        while True:
            try:
                fd = os.open(path, os.O_RDONLY)
                while True:
                    data = os.read(fd, size * 64)
                    if not data:
                        break
                    hit = False
                    for i in range(0, len(data) - size + 1, size):
                        _, _, typ, code, val = struct.unpack_from(fmt, data, i)
                        if (typ == EV_KEY and code == BTN_TOUCH and val == 1) or typ == EV_ABS:
                            hit = True
                    if hit and not is_on():
                        set_power(True)
            except OSError:
                pass
            time.sleep(2)                                  # device gone/replugged: retry

    paths = sorted(set(glob.glob("/dev/input/by-id/*-event-*")) | set(glob.glob("/dev/input/by-path/*-event*")))
    seen = set()
    for pth in paths:
        real = os.path.realpath(pth)
        if real not in seen:
            seen.add(real)
            threading.Thread(target=watch, args=(real,), daemon=True).start()


class Server(ThreadingHTTPServer):
    allow_reuse_address = True


if __name__ == "__main__":
    exit_with_sway()
    wake_on_touch()
    Server(("127.0.0.1", 8091), H).serve_forever()
