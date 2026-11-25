#!/bin/bash
# UI runtime benchmark driver, on the BENCH Pi (prompt §7/§8, docs/UI-RUNTIME-BENCHMARK.md).
#
#   run.sh <runtime|all> <headless|screen> [WxH[@Hz]] [-- extra aetherrv-uibench run options]
#     runtime: wpe (MiniBrowser) | wpe-launcher (panel/wpe, the panel's own WPE kiosk launcher) | tauri | chromium
#     all = wpe tauri chromium (wpe-launcher only when named)
#
# Every runtime shows the same production bundle (/opt/aetherrv-rs/ui via the gateway on 127.0.0.1:8080, with
# ?bench=1) fullscreen in the same sway session (session.sh). Results: one JSON per runtime plus summary.md in
# $UIBENCH_OUT (default ~/uibench/out/<date>-<mode>-<WxH>). Headless is the benchmark's mode by owner decision
# (2026-11-23).
#
# It never stops, restarts or reconfigures an aetherrv-* unit and never edits /etc/aetherrv: it only reads
# /api/state and points browsers at the gateway, whose devices are all simulators on the bench.
set -euo pipefail
if ip -br link | grep -qE '^(hat0|hat1)\b'; then echo "refused: this host has hat0/hat1, it is the van" >&2; exit 3; fi
H=$(cd "$(dirname "$0")" && pwd)
WHICH=${1:?runtime|all}; MODE=${2:?headless|screen}; shift 2
RES=1920x1200
if [ $# -gt 0 ] && [ "$1" != "--" ]; then RES=$1; shift; fi
[ "${1:-}" = "--" ] && shift
EXTRA=("$@")
BIN=${UIBENCH_BIN:-$HOME/uibench/crate/target/release/aetherrv-uibench}
TAURI_BIN=${UIBENCH_TAURI_BIN:-$HOME/uibench/tauri/target/release/aetherrv-panel-tauri}
WPE_BIN=${UIBENCH_WPE_BIN:-$HOME/uibench/wpe/aetherrv-panel-wpe}
URL=${UIBENCH_URL:-http://127.0.0.1:8080/?bench=1}
OUT=${UIBENCH_OUT:-$HOME/uibench/out/$(date +%Y%m%d-%H%M)-$MODE-${RES%@*}}
mkdir -p "$OUT"
[ "$WHICH" = all ] && WHICH="wpe tauri chromium"

curl -sf -m 3 -o /dev/null http://127.0.0.1:8080/ || { echo "gateway on 127.0.0.1:8080 does not serve the UI (install the bundle: prepare.sh)" >&2; exit 4; }

# ── the sway session (reused when it already has this mode and size) ──
ENVF=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/uibench/env
want_kind=$MODE
want_extra=$(printf '%s' "${UIBENCH_SWAY_EXTRA:-}" | md5sum | cut -c1-8)     # session.sh's extra config lines, if any
if [ -f "$ENVF" ] && grep -qx "KIND=$want_kind" "$ENVF" && grep -qx "REQUESTED_MODE=$RES" "$ENVF" && grep -qx "SWAY_EXTRA=$want_extra" "$ENVF" \
   && systemctl is-active -q uibench-sway; then
  :
else
  if [ "$MODE" = screen ]; then "$H/session.sh" start screen "$RES"; else "$H/session.sh" start headless "${RES%@*}"; fi
fi
set -a; # shellcheck disable=SC1090
. "$ENVF"; set +a
read -r OW OH MHZ < <(swaymsg -t get_outputs -r | jq -r --arg o "$OUTPUT" 'first(.[] | select(.name == $o)) | .current_mode | "\(.width) \(.height) \(.refresh)"')
OUTMODE=$(printf '%dx%d@%d.%03dHz' "$OW" "$OH" $((MHZ / 1000)) $((MHZ % 1000)))   # sway's refresh is in mHz
if [ "$MODE" = screen ]; then
  CONN=$(cat /sys/class/drm/card*-"$OUTPUT"/status 2>/dev/null | head -1)
  # what the display controller actually drives (a custom mode the monitor rejects still "succeeds" in sway)
  KSTATE=$(sudo sh -c 'grep -hE "crtc\[|active=|mode:" /sys/kernel/debug/dri/*/state' 2>/dev/null | tr -s ' ' | paste -sd' ' | cut -c1-300)
  LABEL="real screen $OUTPUT: sway mode $OUTMODE (requested $RES; connector $CONN; kms: ${KSTATE:-n/a})"
else
  LABEL="headless (owner decision 2026-11-23): WLR headless output $OUTMODE"
fi
[ -n "${UIBENCH_SWAY_EXTRA:-}" ] && LABEL="$LABEL; sway extra: ${UIBENCH_SWAY_EXTRA//$'\n'/; }"
echo "uibench: $LABEL" | tee "$OUT/session.txt"
journalctl -u uibench-sway -b --no-pager -o cat 2>/dev/null | grep -iE 'mode|error|fail' | tail -20 >> "$OUT/session.txt" || true

BUNDLE=$(cd "${UIBENCH_BUNDLE_DIR:-/opt/aetherrv-rs/ui}" && find . -type f | sort | xargs sha256sum | sha256sum | cut -c1-12)
export UIBENCH_BUNDLE=$BUNDLE
{ echo "bundle $BUNDLE"; pacman -Q sway wlroots0.20 chromium wpewebkit webkit2gtk-4.1 mesa linux-rpi 2>/dev/null; uname -r; } > "$OUT/versions.txt"

# no other runtime may be running in the session while one is measured (a stop that names a unit that isn't
# loaded fails as a whole, so each leftover unit is stopped on its own, then checked)
stop_leftovers() {
  local u
  for u in $(systemctl --user list-units --plain --no-legend --all 'uibench-rt-*' | awk '{print $1}'); do
    systemctl --user stop "$u" 2>/dev/null || true
  done
  if systemctl --user list-units --plain --no-legend 'uibench-rt-*' | grep -q .; then
    echo "refused: a runtime from an earlier run is still running (systemctl --user list-units 'uibench-rt-*')" >&2; exit 5
  fi
}
stop_leftovers

# shellcheck disable=SC2054
CHROMIUM_FLAGS=(--kiosk --ozone-platform=wayland --noerrdialogs --autoplay-policy=no-user-gesture-required
  --disable-infobars --no-first-run --disable-session-crashed-bubble
  --disable-features=Translate,TouchpadOverscrollHistoryNavigation --overscroll-history-navigation=0
  --check-for-update-interval=31536000 --password-store=basic)   # as deploy/kiosk/chromium.sh

set_cmd() {   # runtime → CMD array (persistent profile, like the van's kiosk)
  case $1 in
    chromium)
      local prof=$HOME/uibench/profiles/chromium-$1
      mkdir -p "$prof"; rm -f "$prof"/Singleton*
      CMD=(chromium "${CHROMIUM_FLAGS[@]}" --user-data-dir="$prof" "$URL") ;;
    wpe) CMD=(env WPE_PLATFORM=wayland /usr/lib/wpe-webkit-2.0/MiniBrowser --fullscreen "$URL") ;;
    wpe-launcher) CMD=(env AETHERRV_PANEL_PROFILE="$HOME/uibench/profiles/wpe-launcher" "$WPE_BIN" "$URL") ;;
    tauri) CMD=("$TAURI_BIN" "$URL") ;;
    *) echo "unknown runtime $1" >&2; return 1 ;;
  esac
}

for rt in $WHICH; do
  case $rt in
    tauri) [ -x "$TAURI_BIN" ] || { echo "skip tauri: $TAURI_BIN not built" >&2; continue; } ;;
    wpe-launcher) [ -x "$WPE_BIN" ] || { echo "skip wpe-launcher: $WPE_BIN not built" >&2; continue; } ;;
  esac
  set_cmd "$rt" || continue
  if [ "${UIBENCH_WARMUP:-1}" = 1 ]; then   # one launch first, so caches and profiles are in the state a used kiosk has
    systemd-run --user --scope --quiet --collect --unit="uibench-rt-warmup-$rt" -- timeout 12 "${CMD[@]}" >/dev/null 2>&1 || true
    sleep 2
  fi
  echo "uibench: $rt → $OUT/$rt.json"
  "$BIN" run --runtime "$rt" --mode "$MODE" --label "$LABEL" --url "$URL" --out "$OUT/$rt.json" --screen-power "${EXTRA[@]}" -- "${CMD[@]}" \
    2> >(tee "$OUT/$rt.log" >&2) || echo "uibench: $rt FAILED (see $OUT/$rt.log)" >&2
  stop_leftovers
  sleep 3
done

"$BIN" summarize "$OUT"/*.json > "$OUT/summary.md" && cat "$OUT/summary.md"
echo "uibench: results in $OUT"
