"""HTTP + WebSocket front end. Serves the touch UI and streams state changes."""

from __future__ import annotations

import asyncio
import json
import logging
from pathlib import Path

from aiohttp import WSMsgType, web

from .history import History
from .state import State

log = logging.getLogger(__name__)


# Settings verbs the touchscreen may ask the root helper for (the helper enforces its own list too).
SETTINGS_VERBS = {"set_hostname", "reboot", "poweroff", "restart_kiosk",
                  "wifi_scan", "wifi_power", "wifi_connect", "wifi_forget",
                  "tailscale_install", "tailscale_login", "tailscale_logout", "update_check", "update_start"}
# (tailscale_whois is internal: used by RemoteAuth, never callable from a page.)
LONG_VERBS = {"tailscale_install": 660, "update_check": 330}   # seconds (package downloads)


def make_app(state: State, static_dir: Path, history: History | None = None, alerts=None,
             controls: dict | None = None, admin=None, prefs=None, auth=None) -> web.Application:
    controls = controls or {}

    async def who_of(req: web.Request) -> str | None:
        """'kiosk' (loopback), 'tailscale:<login>@<node>' (owner's own Tailscale device), or None (view-only)."""
        if auth is None:
            return "kiosk" if req.remote in ("127.0.0.1", "::1") else None
        sock = req.transport.get_extra_info("sockname") if req.transport else None
        return await auth.who(req.remote, sock[0] if sock else None)

    @web.middleware
    async def no_stale_ui(request: web.Request, handler):
        # The kiosk's Chromium otherwise heuristically caches app.js/style.css and keeps showing an
        # old UI after deploys. no-cache = always revalidate (ETag → cheap 304 when unchanged).
        resp = await handler(request)
        if request.path == "/" or request.path.startswith("/static/"):
            resp.headers["Cache-Control"] = "no-cache"
        return resp

    app = web.Application(middlewares=[no_stale_ui])

    async def index(_req: web.Request) -> web.FileResponse:
        return web.FileResponse(static_dir / "index.html")

    async def api_state(_req: web.Request) -> web.Response:
        return web.json_response(state.snapshot())

    async def api_history(req: web.Request) -> web.Response:
        if history is None:
            return web.json_response({})
        keys = [k for k in req.query.get("keys", "").split(",") if k]
        try:
            since = min(float(req.query.get("since", "86400")), 31 * 86400)
            points = max(10, min(int(req.query.get("points", "240")), 2000))
        except ValueError:
            raise web.HTTPBadRequest(text="bad since/points")
        return web.json_response({k: history.get(k, since, points) for k in keys})

    async def ws(req: web.Request) -> web.WebSocketResponse:
        sock = web.WebSocketResponse(heartbeat=15)
        await sock.prepare(req)
        req_remote = req.remote
        who = await who_of(req)
        q = state.subscribe()
        await sock.send_str(json.dumps({"type": "hello", "control": who is not None,
                                        "who": "touchscreen" if who == "kiosk" else (who or "viewer")}))
        await sock.send_str(json.dumps({"type": "snapshot", "data": state.snapshot()}))

        async def pump() -> None:
            while True:
                batch = await q.get()
                await asyncio.sleep(0.2)  # coalesce bursts, max 5 Hz to the UI
                while not q.empty():
                    batch.update(q.get_nowait())
                await sock.send_str(json.dumps({"type": "delta", "data": batch}))

        task = asyncio.create_task(pump())
        try:
            async for msg in sock:
                if msg.type != WSMsgType.TEXT:
                    continue
                try:
                    req = json.loads(msg.data)
                except ValueError:
                    continue
                # Acknowledging an alert only changes AetherRV's own UI state, never a device.
                if alerts is not None and req.get("type") == "ack":
                    alerts.ack(str(req.get("id", "")))
                    state.set("alerts.active", alerts.snapshot())
                elif alerts is not None and req.get("type") == "ack_all":
                    alerts.ack_all()
                    state.set("alerts.active", alerts.snapshot())
                elif req.get("type") == "control":
                    dev = controls.get(str(req.get("device")))
                    if who is None:
                        await sock.send_str(json.dumps({"type": "control_result", "id": req.get("id"), "ok": False,
                                                        "detail": "view only: control from the touchscreen or your Tailscale devices"}))
                    elif dev is None:
                        await sock.send_str(json.dumps({"type": "control_result", "id": req.get("id"), "ok": False,
                                                        "detail": "device not available"}))
                    else:
                        async def run_cmd(r=req, d=dev):
                            res = await d.command(str(r.get("cmd")), r.get("value"), client=who)
                            if not sock.closed:
                                await sock.send_str(json.dumps({"type": "control_result", "id": r.get("id"), **res}))
                        asyncio.create_task(run_cmd())
                else:
                    # Other device control requests will arrive here. With every device's tx_enabled
                    # false they are refused. Commands also need auth before any writes.
                    await sock.send_str(json.dumps({"type": "error", "error": "control is disabled"}))
        finally:
            task.cancel()
            state.unsubscribe(q)
        return sock

    def _local(req: web.Request) -> bool:
        return req.remote in ("127.0.0.1", "::1")

    async def api_settings(req: web.Request) -> web.Response:
        try:
            version = (static_dir.parent / "VERSION").read_text().strip()
        except OSError:
            version = None
        out = {"prefs": dict(prefs.values) if prefs else {}, "admin": None, "version": version}
        if admin is not None:
            try:
                out["admin"] = await admin.call("status", timeout=20)
            except Exception as e:                         # noqa: BLE001 — report, don't 500
                out["admin"] = {"ok": False, "error": str(e)}
            for key, verb in (("wifi", "wifi_status"), ("tailscale", "tailscale_status"), ("update", "update_status")):
                try:
                    out[key] = await admin.call(verb, timeout=20)
                except Exception as e:                     # noqa: BLE001
                    out[key] = {"ok": False, "error": str(e)}
            if await who_of(req) is None and isinstance(out.get("tailscale"), dict):
                out["tailscale"].pop("auth_url", None)     # a pending sign-in link is for the touchscreen only
        return web.json_response(out)

    async def api_settings_action(req: web.Request) -> web.Response:
        # Changes from the touchscreen or the owner's own Tailscale devices, like device controls.
        who = await who_of(req)
        if who is None:
            return web.json_response({"ok": False, "error": "view only: settings change from the touchscreen or your Tailscale devices"}, status=403)
        try:
            body = await req.json()
        except ValueError:
            return web.json_response({"ok": False, "error": "bad request"}, status=400)
        verb, args = body.get("verb"), body.get("args") or {}
        if verb == "pref" and prefs is not None:
            try:
                prefs.set(str(args.get("key")), args.get("value"))
                return web.json_response({"ok": True})
            except (ValueError, TypeError) as e:
                return web.json_response({"ok": False, "error": str(e)})
        if verb not in SETTINGS_VERBS or not isinstance(args, dict) or admin is None:
            return web.json_response({"ok": False, "error": "not allowed"}, status=403)
        log.info("settings action from %s: %s", who, verb)        # args never logged (may hold secrets)
        try:
            return web.json_response(await admin.call(verb, args, timeout=LONG_VERBS.get(verb, 120)))
        except Exception as e:                             # noqa: BLE001
            return web.json_response({"ok": False, "error": str(e)})

    app.router.add_get("/", index)
    app.router.add_get("/api/settings", api_settings)
    app.router.add_post("/api/settings/action", api_settings_action)
    app.router.add_get("/api/state", api_state)
    app.router.add_get("/api/history", api_history)
    app.router.add_get("/ws", ws)
    app.router.add_static("/static/", static_dir)
    return app
