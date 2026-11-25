#!/bin/bash
# Calibrate the frame recorder on the BENCH Pi (docs/UI-RUNTIME-BENCHMARK.md, "Calibration"): Chromium shows
# calib.html painting at a known rate, and `aetherrv-uibench frames` reports the presented frames, the patch copies
# and the page's own rAF/paint counts, once per set of capture options.
#   calibrate.sh <hz: 0|30|60> ["frames options"]...   e.g. calibrate.sh 60 "" "--grid 6,4,128,4" "--patch 768,480,320,200,2"
set -euo pipefail
if ip -br link | grep -qE '^(hat0|hat1)\b'; then echo "refused: this host has hat0/hat1, it is the van" >&2; exit 3; fi
H=$(cd "$(dirname "$0")" && pwd)
BIN=${UIBENCH_BIN:-$HOME/uibench/crate/target/release/aetherrv-uibench}
set -a; # shellcheck disable=SC1091
. "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/uibench/env"; set +a
HZ=${1:?hz}; shift
prof=$(mktemp -d)
systemd-run --user --scope --quiet --collect --unit=uibench-rt-calib -- chromium --kiosk --ozone-platform=wayland --noerrdialogs \
  --no-first-run --user-data-dir="$prof" "file://$H/calib.html?hz=$HZ" >/dev/null 2>&1 &
sleep 6
for a in "${@:-}"; do
  echo "== hz=$HZ ${a:-ticker only}"
  # shellcheck disable=SC2086
  "$BIN" frames --secs 5 $a | "$BIN" digest calibrate
done
systemctl --user stop uibench-rt-calib.scope 2>/dev/null || true
rm -rf "$prof"
