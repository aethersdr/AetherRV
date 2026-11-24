"""Regenerate docs/CRATES.md: the licence of every third-party Rust crate the binaries link and every npm package
the UI bundle ships (the project is GPL-3.0; every licence listed must be compatible). Workstation dev tool:
    python3 harness/crates.py
"""
import collections
import json
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
m = json.loads(subprocess.run(["cargo", "metadata", "--offline", "--format-version", "1"], cwd=ROOT,
                              capture_output=True, check=True, text=True).stdout)
ws = set(m["workspace_members"])
pk = {p["id"]: p for p in m["packages"]}
nodes = {n["id"]: n for n in m["resolve"]["nodes"]}
seen, stack = set(), list(ws)
while stack:
    i = stack.pop()
    if i in seen:
        continue
    seen.add(i)
    for d in nodes[i]["deps"]:
        if any(k["kind"] in (None, "build") for k in d["dep_kinds"]):   # what ships, not dev-only
            stack.append(d["pkg"])
third = sorted((pk[i] for i in seen if i not in ws), key=lambda p: (p["name"], p["version"]))
direct = sorted({pk[d["pkg"]]["name"] for w in ws for d in nodes[w]["deps"]
                 if d["pkg"] not in ws and any(k["kind"] in (None, "build") for k in d["dep_kinds"])})
OK = ("MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Zlib", "Unicode-3.0", "Unlicense", "0BSD",
      "CC0-1.0", "MIT-0", "BSL-1.0", "CDLA-Permissive-2.0", "LGPL-2.1-or-later", "LLVM-exception")


def ok(lic: str) -> bool:
    words = lic.replace("(", " ").replace(")", " ").replace("/", " OR ").split()
    terms = [w for w in words if w not in ("OR", "AND", "WITH")]
    return bool(terms) and all(t in OK for t in terms)


npm = []
lock = json.loads((ROOT / "ui/package-lock.json").read_text())
prod = json.loads((ROOT / "ui/package.json").read_text()).get("dependencies", {})
for name in sorted(prod):
    meta = lock["packages"].get(f"node_modules/{name}", {})
    npm.append((name, meta.get("version", "?"), meta.get("license", "?")))

out = ["# Third-party licences", "",
       "AetherRV is GPL-3.0. Everything below is linked into the binaries or shipped in the UI bundle, and every",
       "licence is GPL-3.0-compatible (permissive). Regenerate with `python3 harness/crates.py`; dev-only tools",
       "(test frameworks, the bundler) are not listed.", "",
       f"## Rust: {len(third)} crates", "", "| Licence | Crates |", "|---|---|"]
for lic, c in collections.Counter(p.get("license") or "NONE" for p in third).most_common():
    out.append(f"| {lic} | {c} |")
bad = [p["name"] for p in third if not ok(p.get("license") or "")]
out += ["", f"Not on the permissive list: {', '.join(bad) if bad else 'none'}.", "",
        "### Direct dependencies", "", "| Crate | Version | Licence |", "|---|---|---|"]
for p in third:
    if p["name"] in direct:
        out.append(f"| {p['name']} | {p['version']} | {p.get('license')} |")
out += ["", "## UI (npm, shipped in the bundle)", "", "| Package | Version | Licence |", "|---|---|---|"]
out += [f"| {n} | {v} | {l} |" for n, v, l in npm]
out += ["", "## Not vendored", "",
        "Run-time system packages from Arch Linux ARM (Mosquitto EPL-2.0/EDL-1.0, the UI runtime, BlueZ GPL-2.0) are",
        "separate programs the system talks to, not linked or redistributed by this repository.", ""]
(ROOT / "docs/CRATES.md").write_text("\n".join(out))
print(f"{len(third)} crates, {len(npm)} npm packages, not permissive: {bad}")
