#!/bin/bash
# What does the pressed state cost the engine? On the BENCH Pi, in the benchmark's sway session: a runtime on a page
# of the production UI with ?bench=1, and the page's bench module asked ("presscost N", over its long poll) to toggle
# .pressed on the page's first press target N times. It reports the forced style and layout time of adding the class,
# and the rAF interval right after adding it against one with no change. Nothing is pressed, injected or sent: the
# page only toggles the class a press sets. docs/UI-RUNTIME-BENCHMARK.md §4.3.
#   presscost.sh [--runtime wpe|chromium|tauri] [--press <variant>] [page...]   default: wpe, switches climate home
#   (variants: the bench module's &press= experiments, e.g. rest, bg, noscale; several comma-separated)
set -euo pipefail
if ip -br link | grep -qE '^(hat0|hat1)\b'; then echo "refused: this host has hat0/hat1, it is the van" >&2; exit 3; fi
BIN=${UIBENCH_BIN:-$HOME/uibench/crate/target/release/aetherrv-uibench}
set -a; # shellcheck disable=SC1091
. "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/uibench/env"; set +a
RT=wpe; PRESS=""
while [ $# -gt 0 ]; do
  case $1 in --runtime) RT=$2; shift 2 ;; --press) PRESS=$2; shift 2 ;; *) break ;; esac
done
PAGES=${*:-switches climate home}
TAURI_BIN=${UIBENCH_TAURI_BIN:-$HOME/uibench/tauri/target/release/aetherrv-panel-tauri}
for page in $PAGES; do
  prof=$(mktemp -d)
  u="${UIBENCH_BASE:-http://127.0.0.1:8080}/?bench=1${PRESS:+&press=$PRESS}#$page"
  case $RT in
    chromium) cmd=(chromium --kiosk --ozone-platform=wayland --noerrdialogs --no-first-run --user-data-dir="$prof" "$u") ;;
    wpe) cmd=(env WPE_PLATFORM=wayland /usr/lib/wpe-webkit-2.0/MiniBrowser --fullscreen "$u") ;;
    wpe-launcher) cmd=(env AETHERRV_PANEL_PROFILE="$prof" "${UIBENCH_WPE_BIN:-$HOME/uibench/wpe/aetherrv-panel-wpe}" "$u") ;;
    tauri) cmd=("$TAURI_BIN" "$u") ;;
    *) echo "runtime: chromium|wpe|wpe-launcher|tauri" >&2; exit 2 ;;
  esac
  systemd-run --user --scope --quiet --collect --unit="uibench-rt-presscost-$page" -- "${cmd[@]}" >/dev/null 2>&1 &
  sleep 10
  "$BIN" frames --secs 25 --no-capture --cmd "presscost 12" | "$BIN" digest presscost "$page" "$RT" "$PRESS"   # the whole report on stderr
  systemctl --user stop "uibench-rt-presscost-$page.scope" 2>/dev/null || true
  rm -rf "$prof"
done
