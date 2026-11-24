"""The Python reference UI (web/) for the runtime benchmark's "before" numbers, on the BENCH Pi, with NO device controls.

Based on harness/ui-reference/serve_snapshot.py: the Python backend's own web app (src/aetherrv/web.py) with an empty
controls table, so every command a page sends is answered "device not available" and nothing can reach a device.
Two additions for the benchmark:

- `?bench=1` injects harness/uibench/page/ref-bench.js into index.html (the reference UI itself is not changed): it
  reports layout and boot marks to the bench runner, like ui/src/bench/ does for the React UI.
- `--live URL` keeps the state moving: it polls a gateway's read-only /api/state every `--every` seconds and feeds
  the values in, so the reference UI does its normal per-update work (app.js rebuilds a page's HTML on every update,
  at most 5 per second over the WebSocket). Without it the page shows one frozen snapshot.

    python serve_ref.py <snapshot.json> [--port 18080] [--live http://127.0.0.1:8080/api/state] [--every 1.0]

Needs aiohttp only. Run with PYTHONPATH pointing at the `src` tree and --web at the `web` tree.
"""
import argparse
import asyncio
import json
import sys
from pathlib import Path

from aiohttp import ClientSession, ClientTimeout, web

HERE = Path(__file__).resolve().parent


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("snapshot")
    ap.add_argument("--port", type=int, default=18080)
    ap.add_argument("--web", default=str(HERE.parents[1] / "web"))
    ap.add_argument("--src", default=str(HERE.parents[1] / "src"))
    ap.add_argument("--live", default=None, help="poll this /api/state URL (read-only) and feed its values in")
    ap.add_argument("--every", type=float, default=1.0)
    a = ap.parse_args()
    sys.path.insert(0, a.src)
    from aetherrv.state import State
    from aetherrv.web import make_app

    web_dir = Path(a.web)
    bench_js = (HERE / "page" / "ref-bench.js").read_text()
    index = (web_dir / "index.html").read_text()
    # before the first script, so the WebSocket hook is in place before app.js opens its socket
    i = index.find("<script")
    bench_index = index[:i] + '<script src="/__uibench/ref-bench.js"></script>\n' + index[i:]

    @web.middleware
    async def inject(request: web.Request, handler):
        if request.path == "/__uibench/ref-bench.js":
            return web.Response(text=bench_js, content_type="application/javascript")
        if request.path == "/" and request.query.get("bench") == "1":
            return web.Response(text=bench_index, content_type="text/html", headers={"Cache-Control": "no-cache"})
        return await handler(request)

    async def run() -> None:
        st = State()
        st.update(json.loads(Path(a.snapshot).read_text()))
        app = make_app(st, web_dir, controls={})          # no controls: nothing is ever sent to a device
        app.middlewares.insert(0, inject)
        runner = web.AppRunner(app)
        await runner.setup()
        await web.TCPSite(runner, "127.0.0.1", a.port).start()
        print(f"reference UI on http://127.0.0.1:{a.port}/ (live: {a.live or 'no'})", flush=True)
        if not a.live:
            while True:
                await asyncio.sleep(3600)
        async with ClientSession(timeout=ClientTimeout(total=5)) as s:
            while True:
                try:
                    async with s.get(a.live) as r:
                        st.update(await r.json())
                except Exception as e:  # gateway restarting (isolation tests): keep the last values
                    print(f"live poll: {e}", flush=True)
                await asyncio.sleep(a.every)

    asyncio.run(run())


if __name__ == "__main__":
    main()
