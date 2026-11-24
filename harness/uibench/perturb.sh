#!/bin/bash
# Does measuring slow the page? On the BENCH Pi, in the benchmark's sway session: a runtime on a page of the
# production UI with ?bench=1&raf=1 (the page reports its own requestAnimationFrame intervals), first with nothing
# copied, then with the four bands the page-switch detector copies every frame, then with one 320×200 patch.
#   perturb.sh [--runtime chromium|wpe|tauri] [page...]     default: chromium, home weather
set -euo pipefail
if ip -br link | grep -qE '^(hat0|hat1)\b'; then echo "refused: this host has hat0/hat1, it is the van" >&2; exit 3; fi
BIN=${UIBENCH_BIN:-$HOME/uibench/crate/target/release/aetherrv-uibench}
set -a; # shellcheck disable=SC1091
. "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/uibench/env"; set +a
RT=chromium
if [ "${1:-}" = --runtime ]; then RT=${2:?runtime}; shift 2; fi
PAGES=${*:-home weather}
TAURI_BIN=${UIBENCH_TAURI_BIN:-$HOME/uibench/tauri/target/release/aetherrv-panel-tauri}
# shellcheck disable=SC2054
BANDS=(--patch 152,372,1752,8,2 --patch 152,648,1752,8,2 --patch 152,924,1752,8,2 --patch 1024,112,8,1072,2)
# shellcheck disable=SC2054
PATCH=(--patch 868,548,320,200,2)
for page in $PAGES; do
  prof=$(mktemp -d)
  u="${UIBENCH_BASE:-http://127.0.0.1:8080}/?bench=1&raf=1#$page"
  case $RT in
    chromium) cmd=(chromium --kiosk --ozone-platform=wayland --noerrdialogs --no-first-run --user-data-dir="$prof" "$u") ;;
    wpe) cmd=(env WPE_PLATFORM=wayland /usr/lib/wpe-webkit-2.0/MiniBrowser --fullscreen "$u") ;;
    tauri) cmd=("$TAURI_BIN" "$u") ;;
    *) echo "runtime: chromium|wpe|tauri" >&2; exit 2 ;;
  esac
  systemd-run --user --scope --quiet --collect --unit="uibench-rt-perturb-$page" -- "${cmd[@]}" >/dev/null 2>&1 &
  sleep 10
  for mode in none bands patch; do
    case $mode in none) a=(--no-capture) ;; bands) a=("${BANDS[@]}") ;; patch) a=("${PATCH[@]}") ;; esac
    "$BIN" frames --secs 12 "${a[@]}" | python3 -c '
import json, sys
d = json.load(sys.stdin)
iv = [x for p in d["page"] if p.get("why") == "raf" for x in (p.get("raf") or {}).get("iv", [])]
iv.sort()
q = lambda p: iv[min(len(iv) - 1, round(p / 100 * (len(iv) - 1)))] if iv else None
fps = (d.get("fps") or {}).get("fps_raw", "-")
print("%-8s %-6s page rAF: n=%d p50=%.1f p95=%.1f max=%.1f ms | presented fps %s" % (sys.argv[1], sys.argv[2], len(iv), q(50) or 0, q(95) or 0, q(100) or 0, fps))
' "$page" "$mode"
  done
  systemctl --user stop "uibench-rt-perturb-$page.scope" 2>/dev/null || true
  rm -rf "$prof"
done
