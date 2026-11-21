import importlib.util
import sys
from pathlib import Path

# Expose tools/bench_traffic.py as `tools_bench` so tests share its N2K ID encoder.
_p = Path(__file__).resolve().parents[1] / "tools" / "bench_traffic.py"
_spec = importlib.util.spec_from_file_location("tools_bench", _p)
_mod = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_mod)
sys.modules["tools_bench"] = _mod
