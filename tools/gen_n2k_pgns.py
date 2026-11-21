#!/usr/bin/env python3
"""Regenerate src/aetherrv/adapters/n2k_pgns.py from CANboat's canboat.json.

    curl -sSfLo /tmp/canboat.json https://raw.githubusercontent.com/canboat/canboat/master/docs/canboat.json
    python3 -I tools/gen_n2k_pgns.py /tmp/canboat.json src/aetherrv/adapters/n2k_pgns.py
"""
import json
import sys

d = json.load(open(sys.argv[1]))
fast, names = set(), {}
for p in d["PGNs"]:
    n = p["PGN"]
    if p.get("Type") == "Fast":
        fast.add(n)
    desc = p.get("Description") or ""
    if n not in names and desc and not desc.startswith(("0x", "Manufacturer Proprietary")):
        names[n] = desc
# Manufacturer-proprietary PGN ranges: canboat lists per-manufacturer variants; the first variant's
# name (e.g. "Seatalk: …") would mislabel every other maker's traffic.
def _proprietary(n):
    return n == 61184 or 65280 <= n <= 65535 or n == 126720 or 130816 <= n <= 131071
for n in list(names):
    if _proprietary(n):
        names[n] = "Manufacturer proprietary"
# 126208 has several canboat variants; name it for the whole group, not the first variant.
names[126208] = "NMEA Request/Command/Acknowledge group function"
out = ['"""NMEA 2000 PGN metadata generated from CANboat %s (canboat.json).' % d.get("Version"), "",
       "CANboat (C) 2009-2026 Kees Verruijt and contributors, https://github.com/canboat/canboat,",
       "licensed under the Apache License 2.0.",
       "Only PGN numbers, names and frame type are taken. Regenerate with tools/gen_n2k_pgns.py.",
       '"""', "", "FAST_PACKET_PGNS = frozenset({" + ", ".join(str(x) for x in sorted(fast)) + "})", "", "PGN_NAMES = {"]
out += [f"    {k}: {v!r}," for k, v in sorted(names.items())]
out += ["}", ""]
lookups = {e["Name"]: e for e in d.get("LookupEnumerations", [])}
for name, var in (("MANUFACTURER_CODE", "MANUFACTURERS"), ("DEVICE_CLASS", "DEVICE_CLASSES")):
    out.append(f"{var} = {{")
    out += [f"    {v['Value']}: {v['Name']!r}," for v in sorted(lookups[name]["EnumValues"], key=lambda v: v["Value"])]
    out += ["}", ""]
open(sys.argv[2], "w").write("\n".join(out))
