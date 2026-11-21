"""aetherrv daemon entry point: `aetherrv -c /etc/aetherrv/aetherrv.toml`."""

from __future__ import annotations

import argparse
import asyncio
import logging
import os
import socket
import tomllib
from pathlib import Path

from aiohttp import web

from .adapters.capture_only import CaptureOnlyAdapter
from .adapters.demo import DemoAdapter
from .adapters.n2k import N2kAdapter
from .climate import run_cabin_average
from .energy import run_derive
from .adapters.netmon import NetMon
from .adapters.nmea_gps import NmeaGps
from .adapters.peplink import PeplinkAdapter, PeplinkClient
from .adapters.starlink import StarlinkAdapter
from .adapters.rixens import RixensAdapter
from .adapters.rixens_http import RixensHttp, RixensHttpAdapter
from .adapters.victron import VictronAdapter
from .alerts import Alerts
from .canbus import CanBus, prune_captures
from .history import History
from .state import State
from .txgate import TxGate
from .settings import AdminClient, AutoTimezone, Prefs, RemoteAuth
from .web import make_app

log = logging.getLogger("aetherrv")


def load_secrets(cfg: dict) -> dict:
    """Secrets live in a separate root:aetherrv 0640 file, not in the world-readable config."""
    path = cfg.get("secrets_file", "/etc/aetherrv/secrets.toml")
    try:
        with open(path, "rb") as f:
            return tomllib.load(f)
    except FileNotFoundError:
        return {}
    except (OSError, tomllib.TOMLDecodeError) as e:
        log.warning("cannot read secrets file %s: %s", path, e)
        return {}


def build(cfg: dict, state: State, history: History | None = None) -> tuple[list[CanBus], list, TxGate]:
    capture_dir = Path(cfg.get("capture_dir", "captures")) if cfg.get("capture", True) else None
    buses: dict[str, CanBus] = {}
    for name, b in cfg.get("bus", {}).items():
        buses[name] = CanBus(name, b.get("interface", "socketcan"), b["channel"],
                             capture_dir if b.get("capture", True) else None, host=b.get("host"), port=b.get("port"))

    devices = cfg.get("device", {})
    gate = TxGate({d: bool(v.get("tx_enabled", False)) for d, v in devices.items()})
    adapters: list = []
    demo = any(d.get("kind") == "demo" and d.get("enabled") for d in devices.values())
    cabin_source = cfg.get("climate", {}).get("cabin_source", "victron")
    rixens_http_on = any(d.get("kind") == "rixens_http" and d.get("enabled") for d in devices.values())
    # Keys owned by a configured real adapter are never written by the demo (decided from config,
    # not at runtime, so heater controls can never act on synthetic values).
    demo_excludes = []
    if rixens_http_on:
        demo_excludes += ["climate.rixens.", "climate.cabin.setpoint"]
    if cabin_source in ("rixens", "average"):
        demo_excludes += ["climate.cabin.temp", "climate.cabin.humidity"]
    if cfg.get("gps", {}).get("enabled"):
        demo_excludes += ["location."]
    if devices.get("victron", {}).get("enabled"):
        demo_excludes += ["victron.", "climate.fridge.", "climate.freezer.", "climate.outside."]
    for dev, d in devices.items():
        kind = d.get("kind")
        if kind == "demo":
            if d.get("enabled", False):
                log.warning("DEMO data source enabled: synthetic values, UI shows a DEMO badge")
                adapters.append(DemoAdapter(state, history, exclude=demo_excludes))
            continue
        if kind in ("starlink", "peplink"):
            if not d.get("enabled", False):
                continue
            if demo:
                log.warning("%s adapter disabled while DEMO data is enabled", kind)
                continue
            if kind == "starlink":
                adapters.append(StarlinkAdapter(state, d.get("target", "192.168.100.1:9200"), float(d.get("interval", 5)),
                                                gate=gate, db=history.db if history else None))
            else:
                sec = load_secrets(cfg)
                cid, csec = sec.get("peplink_client_id", ""), sec.get("peplink_client_secret", "")
                user, pw = sec.get("peplink_ro_user", ""), sec.get("peplink_ro_password", "")
                if not (d.get("host") and ((user and pw) or (cid and csec))):
                    log.warning("peplink enabled but host / credentials missing; skipping")
                    continue
                adapters.append(PeplinkAdapter(state, PeplinkClient(
                    d["host"], cid, csec, bool(d.get("verify_tls", False)), username=user, password=pw)))
            continue
        if kind == "rixens_http":
            if d.get("enabled", False):
                adapters.append(RixensHttpAdapter(
                    state, RixensHttp(d["host"]), gate, db=history.db if history else None,
                    interval=float(d.get("interval", 5)), cabin_from_rixens=(cabin_source == "rixens"),
                    ml_per_pulse=d.get("fuel_ml_per_pulse")))
            continue
        if kind == "victron":
            if demo and not d.get("enabled", False):
                log.warning("victron adapter disabled while DEMO data is enabled (set enabled = true to use the real Cerbo)")
                continue
            vsec = load_secrets(cfg)
            adapters.append(VictronAdapter(state, d["host"], d.get("port", 1883),
                                           cabin_temp=d.get("cabin_temp") if cabin_source in ("victron", "average") else False, outside_temp=d.get("outside_temp"),
                                           cabin_role="cabin_front" if cabin_source == "average" else "cabin",
                                           fridge_temp=d.get("fridge_temp"), freezer_temp=d.get("freezer_temp"),
                                           username=vsec.get("victron_mqtt_user"), password=vsec.get("victron_mqtt_password"),
                                           tls=bool(d.get("tls", False))))
            continue
        bus = buses.get(d.get("bus", ""))
        if bus is None:
            log.warning("device %s: bus %r not configured; skipping", dev, d.get("bus"))
            continue
        if kind == "n2k":
            a = N2kAdapter(state, d.get("switch", []))
        elif kind == "rixens":
            a = RixensAdapter(dev, state, publish_climate=not rixens_http_on)   # CAN = fallback/inventory
        elif kind == "capture":
            a = CaptureOnlyAdapter(dev, state)
        else:
            log.warning("device %s: unknown kind %r", dev, kind)
            continue
        bus.add_handler(a.on_frame)
        adapters.append(a)
    gps = cfg.get("gps", {})
    if gps.get("enabled"):
        adapters.append(NmeaGps(state, int(gps.get("port", 10110)), gps.get("bind", "0.0.0.0"),
                                source=gps.get("source", "peplink")))
    nm = cfg.get("netmon", {})
    if nm.get("targets"):
        adapters.append(NetMon(state, nm["targets"], float(nm.get("interval", 60))))
    return list(buses.values()), adapters, gate


async def publish_bus_stats(buses: list[CanBus], gate: TxGate, state: State) -> None:
    while True:
        for b in buses:
            state.update({
                f"bus.{b.name}.connected": b.link_up(),
                f"bus.{b.name}.channel": b.channel,
                f"bus.{b.name}.frames": b.stats.frames,
                f"bus.{b.name}.errors": b.stats.errors,
                f"bus.{b.name}.rate": round(b.stats.rate(), 1),
                f"bus.{b.name}.ids": len(b.stats.ids),
            })
        state.set("tx.blocked", dict(gate.blocked_count))
        await asyncio.sleep(1.0)


async def systemd_watchdog() -> None:
    """Feed systemd's watchdog from the event loop. If the loop hangs, systemd kills and
    restarts us. This only covers a hung process. It is NOT the device-level failsafe."""
    addr = os.environ.get("NOTIFY_SOCKET")
    usec = int(os.environ.get("WATCHDOG_USEC", "0"))
    if not addr:
        return
    if addr.startswith("@"):
        addr = "\0" + addr[1:]
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
    sock.connect(addr)
    sock.send(b"READY=1")
    if not usec:
        return
    while True:
        sock.send(b"WATCHDOG=1")
        await asyncio.sleep(usec / 1e6 / 3)


async def amain(cfg: dict) -> None:
    state = State()
    hcfg = cfg.get("history", {})
    demo = any(d.get("kind") == "demo" and d.get("enabled") for d in cfg.get("device", {}).values())
    history = History(
        state,
        path=":memory:" if demo else hcfg.get("path", "/var/lib/aetherrv/history.sqlite3"),
        interval=float(hcfg.get("interval", 30)),
        retention_days=float(hcfg.get("retention_days", 30)),
    )
    if history.degraded:
        state.set("history.degraded", history.degraded)
    buses, adapters, gate = build(cfg, state, history)
    adapters.append(history)
    alerts = Alerts(state, db=history.db, cfg=cfg.get("alerts"))
    adapters.append(alerts)
    state.set("tx.enabled", {d: gate.is_enabled(d) for d in cfg.get("device", {})})
    ui = cfg.get("ui", {})
    state.set("ui.bms_expected", int(cfg.get("alerts", {}).get("bms_expected_modules", 0)))
    state.set("ui.fridge_max_c", float(ui.get("fridge_max_c", 5.0)))
    state.set("ui.freezer_max_c", float(ui.get("freezer_max_c", -12.0)))

    web_cfg = cfg.get("web", {})
    static = Path(web_cfg.get("static_dir", Path(__file__).resolve().parents[2] / "web"))
    vic = next((a for a in adapters if isinstance(a, VictronAdapter)), None)
    if vic is not None:
        vic.gate, vic.db = gate, history.db
    controls = {"rixens": next((a for a in adapters if isinstance(a, RixensHttpAdapter)), None), "victron": vic,
                "starlink": next((a for a in adapters if isinstance(a, StarlinkAdapter)), None)}
    admin = AdminClient(web_cfg.get("admin_socket", "/run/aetherrv-admin.sock"))
    prefs = Prefs(state, web_cfg.get("settings_path", "/var/lib/aetherrv/settings.json"))
    adapters.append(AutoTimezone(state, admin, prefs))
    runner = web.AppRunner(make_app(state, static, history, alerts, controls, admin=admin, prefs=prefs,
                                    auth=RemoteAuth(admin)))
    await runner.setup()
    await web.TCPSite(runner, web_cfg.get("host", "0.0.0.0"), web_cfg.get("port", 8080)).start()
    log.info("web UI on http://%s:%s", web_cfg.get("host", "0.0.0.0"), web_cfg.get("port", 8080))

    tasks = [asyncio.create_task(b.run(), name=f"bus:{b.name}") for b in buses]
    tasks += [asyncio.create_task(a.run(), name=type(a).__name__) for a in adapters]
    tasks.append(asyncio.create_task(systemd_watchdog(), name="watchdog"))
    if cfg.get("capture", True):
        tasks.append(asyncio.create_task(prune_captures(Path(cfg.get("capture_dir", "captures")),
                                                        float(cfg.get("capture_keep_days", 14)),
                                                        float(cfg.get("capture_max_gb", 20))), name="prune"))
    tasks.append(asyncio.create_task(publish_bus_stats(buses, gate, state), name="stats"))
    tasks.append(asyncio.create_task(run_derive(state), name="energy"))
    if cfg.get("climate", {}).get("cabin_source") == "average":
        tasks.append(asyncio.create_task(run_cabin_average(state), name="cabin-average"))
    try:
        await asyncio.gather(*tasks)
    finally:
        await runner.cleanup()
        history.close()


def main() -> None:
    ap = argparse.ArgumentParser(prog="aetherrv")
    ap.add_argument("-c", "--config", default="config/aetherrv.toml")
    ap.add_argument("-v", "--verbose", action="store_true")
    args = ap.parse_args()
    logging.basicConfig(level=logging.DEBUG if args.verbose else logging.INFO,
                        format="%(asctime)s %(levelname)s %(name)s: %(message)s")
    with open(args.config, "rb") as f:
        cfg = tomllib.load(f)
    try:
        asyncio.run(amain(cfg))
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
