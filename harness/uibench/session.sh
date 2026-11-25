#!/bin/bash
# The benchmark's sway session on the BENCH Pi (never the van). Started like the van's kiosk unit
# (deploy/systemd/aetherrv-kiosk.service): a transient system unit with a PAM session on a VT, so logind gives sway
# its seat (DRM + libinput) exactly as on the van. It uses VT 8, not tty1, and runs as the calling user.
#
#   session.sh start headless [WxH]    WLR headless output (+ libinput for the injected touchscreen); not acceptance
#   session.sh start screen [WxH@Hz]   the real HDMI monitor, mode as given (custom modes allowed, see below)
#   session.sh stop | status | env     env prints WAYLAND_DISPLAY / SWAYSOCK / OUTPUT for the runner
#
# On the real screen a mode not in the EDID list is set with `output <name> mode --custom`; session.sh then reads
# back the mode sway and the kernel report so the result says which mode was actually driven.
set -euo pipefail
if ip -br link | grep -qE '^(hat0|hat1)\b'; then echo "refused: this host has hat0/hat1, it is the van" >&2; exit 3; fi
UNIT=uibench-sway
VT=${UIBENCH_VT:-8}
HERE=$(cd "$(dirname "$0")" && pwd)
STATE=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/uibench
BIN=${UIBENCH_BIN:-$HOME/uibench/crate/target/release/aetherrv-uibench}
mkdir -p "$STATE"

swaysock() { ls -t "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/sway-ipc.$(id -u)."*.sock 2>/dev/null | head -1; }

case "${1:-}" in
start)
  KIND=${2:?headless|screen}; MODE=${3:-}
  sudo systemctl stop $UNIT 2>/dev/null || true
  sudo systemctl reset-failed $UNIT 2>/dev/null || true
  rm -f "$STATE/env"
  ENVS=(-p Environment=XDG_SESSION_TYPE=wayland -p Environment=XCURSOR_SIZE=24)
  if [ "$KIND" = headless ]; then
    MODE=${MODE:-1920x1200}
    OUT=HEADLESS-1
    # shellcheck disable=SC2054
    ENVS+=(-p Environment=WLR_BACKENDS=headless,libinput -p Environment=WLR_HEADLESS_OUTPUTS=1)
    OUTMODE="mode ${MODE}@60Hz"
  else
    OUT=$(for s in /sys/class/drm/card*-HDMI-A-*; do [ "$(cat "$s/status")" = connected ] && basename "$s" | sed 's/^card[0-9]*-//'; done | head -1)
    [ -n "$OUT" ] || { echo "no connected HDMI connector (/sys/class/drm/card*-HDMI-A-*/status)" >&2; exit 4; }
    MODE=${MODE:-1920x1200@60Hz}
    OUTMODE="mode --custom $MODE"
    grep -qx "${MODE%@*}" "/sys/class/drm/card1-$OUT/modes" 2>/dev/null && OUTMODE="mode $MODE"
  fi
  sed -e "s|@OUTPUT@|$OUT|g" -e "s|@MODE@|$OUTMODE|" "$HERE/sway.conf.in" > "$STATE/sway.conf"
  # experiments: extra sway config lines, e.g. UIBENCH_SWAY_EXTRA=$'output * max_render_time 4\nfor_window [app_id=".*"] max_render_time 4'
  if [ -n "${UIBENCH_SWAY_EXTRA:-}" ]; then printf '%s\n' "$UIBENCH_SWAY_EXTRA" >> "$STATE/sway.conf"; fi
  T0=$(date +%s%N)
  sudo systemd-run --quiet --unit=$UNIT --uid="$(id -un)" -p PAMName=login -p TTYPath=/dev/tty$VT \
    -p StandardInput=tty-fail -p StandardOutput=journal -p StandardError=journal -p TTYReset=yes -p TTYVHangup=yes \
    -p UtmpIdentifier=tty$VT -p UtmpMode=user "${ENVS[@]}" /usr/bin/sway --config "$STATE/sway.conf"
  sudo chvt $VT
  for _ in $(seq 50); do [ -n "$(swaysock)" ] && SWAYSOCK=$(swaysock) swaymsg -t get_outputs >/dev/null 2>&1 && break; sleep 0.2; done
  export SWAYSOCK; SWAYSOCK=$(swaysock)
  [ -n "$SWAYSOCK" ] || { echo "sway did not start: journalctl -u $UNIT" >&2; exit 5; }
  WD=$(ls -t "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"/wayland-? 2>/dev/null | head -1)
  SWAY_START_MS=$(( ($(date +%s%N) - T0) / 1000000 ))       # unit start → sway answering on its IPC socket
  printf 'SWAYSOCK=%s\nWAYLAND_DISPLAY=%s\nOUTPUT=%s\nKIND=%s\nREQUESTED_MODE=%s\nSWAY_START_MS=%s\nSWAY_EXTRA=%s\n' "$SWAYSOCK" "$(basename "$WD")" "$OUT" "$KIND" "$MODE" "$SWAY_START_MS" "$(printf '%s' "${UIBENCH_SWAY_EXTRA:-}" | md5sum | cut -c1-8)" > "$STATE/env"
  cat "$STATE/env"
  swaymsg -t get_outputs -r | "$BIN" digest outputs
  ;;
stop)
  sudo systemctl stop $UNIT 2>/dev/null || true
  rm -f "$STATE/env"
  ;;
status)
  systemctl is-active $UNIT || true
  [ -f "$STATE/env" ] && cat "$STATE/env"
  ;;
env)
  cat "$STATE/env"
  ;;
*)
  sed -n '2,12p' "$0"; exit 2 ;;
esac
