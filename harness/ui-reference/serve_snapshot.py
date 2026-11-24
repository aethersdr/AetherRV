"""Serve the Python reference UI (web/) with a saved /api/state snapshot and NO device controls, on loopback.

Workstation-only dev tool (reference screenshots, UI comparisons, the Python/Chromium baseline). The controls table
is empty, so every command is answered "device not available": nothing can reach a device.

    uv run python harness/ui-reference/serve_snapshot.py <state.json> [port]
"""
import asyncio
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "src"))

from aiohttp import web  # noqa: E402

from aetherrv.state import State  # noqa: E402
from aetherrv.web import make_app  # noqa: E402


async def main() -> None:
    snap = json.loads(Path(sys.argv[1]).read_text())
    port = int(sys.argv[2]) if len(sys.argv) > 2 else 18080
    st = State()
    st.update(snap)
    app = make_app(st, ROOT / "web", controls={})          # no controls: nothing is ever sent
    runner = web.AppRunner(app)
    await runner.setup()
    await web.TCPSite(runner, "127.0.0.1", port).start()
    print(f"reference UI on http://127.0.0.1:{port}/", flush=True)
    while True:
        await asyncio.sleep(3600)


asyncio.run(main())
