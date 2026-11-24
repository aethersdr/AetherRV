"""Serve a UI bundle for the benchmark WITHOUT installing it: static files from a directory, /api and /ws proxied to the
gateway (127.0.0.1:8080). Used when /opt/aetherrv-rs/ui must stay as deployed; the page still talks to the real
gateway on loopback (same rights as the kiosk), and the runtimes open http://127.0.0.1:<port>/?bench=1.

    python serve_bundle.py <bundle dir> [--port 18082] [--gateway http://127.0.0.1:8080]

Needs aiohttp. Adds one asyncio hop to each WebSocket message and serves the files from Python instead of the
gateway (docs/UI-RUNTIME-BENCHMARK.md notes it where it matters: the cold start).
"""
import argparse
import asyncio
from pathlib import Path

from aiohttp import ClientSession, WSMsgType, web


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("dir")
    ap.add_argument("--port", type=int, default=18082)
    ap.add_argument("--gateway", default="http://127.0.0.1:8080")
    a = ap.parse_args()
    root = Path(a.dir).resolve()
    gw = a.gateway.rstrip("/")

    async def static(req: web.Request) -> web.StreamResponse:
        rel = req.match_info.get("p", "") or "index.html"
        f = (root / rel).resolve()
        if root not in f.parents and f != root / "index.html" or not f.is_file():
            raise web.HTTPNotFound()
        cache = "no-cache" if f.name == "index.html" else "public, max-age=31536000, immutable"
        return web.FileResponse(f, headers={"Cache-Control": cache})

    async def api(req: web.Request) -> web.Response:
        async with req.app["http"].request(req.method, f"{gw}{req.rel_url}", data=await req.read(),
                                           headers={k: v for k, v in req.headers.items() if k.lower() in ("content-type", "accept")}) as r:
            return web.Response(body=await r.read(), status=r.status, content_type=r.content_type)

    async def ws(req: web.Request) -> web.WebSocketResponse:
        sock = web.WebSocketResponse()
        await sock.prepare(req)
        async with req.app["http"].ws_connect(f"{gw.replace('http', 'ws', 1)}/ws") as up:
            async def down() -> None:
                async for m in up:
                    if m.type == WSMsgType.TEXT:
                        await sock.send_str(m.data)
                    elif m.type == WSMsgType.BINARY:
                        await sock.send_bytes(m.data)
                await sock.close()
            t = asyncio.create_task(down())
            async for m in sock:
                if m.type == WSMsgType.TEXT:
                    await up.send_str(m.data)
                elif m.type == WSMsgType.BINARY:
                    await up.send_bytes(m.data)
            t.cancel()
        return sock

    async def start(app: web.Application) -> None:
        app["http"] = ClientSession()

    async def stop(app: web.Application) -> None:
        await app["http"].close()

    app = web.Application()
    app.on_startup.append(start)
    app.on_cleanup.append(stop)
    app.router.add_route("*", "/api/{tail:.*}", api)
    app.router.add_get("/ws", ws)
    app.router.add_get("/", static)
    app.router.add_get("/{p:.*}", static)
    print(f"bundle {root} on http://127.0.0.1:{a.port}/ (api, ws → {gw})", flush=True)
    web.run_app(app, host="127.0.0.1", port=a.port, print=None)


if __name__ == "__main__":
    main()
