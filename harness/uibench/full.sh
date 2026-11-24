#!/bin/bash
# The whole UI runtime benchmark in one command, from the workstation: prepare, run every runtime and the Python
# baseline on the bench, fetch the results into harness/out/uibench/ (gitignored).
#   harness/uibench/full.sh headless [WxH]      development numbers (not acceptance)
#   harness/uibench/full.sh screen [WxH@Hz]     on the bench monitor; default 1920x1200@60Hz (custom mode)
#   harness/uibench/full.sh screen-fallback     1920x1080 and the native 2560x1440 headroom check (§8)
set -euo pipefail
HOST=${UIBENCH_HOST:?set UIBENCH_HOST to the bench Pi's ssh host (never the van)}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
MODE=${1:?headless|screen|screen-fallback}; RES=${2:-}
[ "${UIBENCH_SKIP_PREPARE:-0}" = 1 ] || "$ROOT/harness/uibench/prepare.sh"
run() {   # mode res
  local dir
  dir="uibench/out/$(date +%Y%m%d-%H%M)-$1-${2%@*}"
  ssh -t "$HOST" "UIBENCH_OUT=\$HOME/$dir ~/uibench/harness/run.sh all $1 $2" || true
  mkdir -p "$ROOT/harness/out/uibench"
  rsync -a "$HOST:$dir" "$ROOT/harness/out/uibench/"
  echo "results: $ROOT/harness/out/uibench/$(basename "$dir")"
}
case $MODE in
  headless) run headless "${RES:-1920x1200}" ;;
  screen) run screen "${RES:-1920x1200@60Hz}" ;;
  screen-fallback) run screen 1920x1080@60Hz; run screen 2560x1440@60Hz ;;
  *) echo "usage: full.sh headless|screen|screen-fallback [WxH]" >&2; exit 2 ;;
esac
