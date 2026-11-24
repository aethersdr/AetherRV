#!/bin/bash
# The whole UI runtime benchmark in one command, from the workstation: prepare, run every runtime and the Python
# baseline on the bench (headless sway output, owner decision 2026-11-23), measure the instrument's own cost per
# runtime (perturb.sh), and fetch the results into harness/out/uibench/ (gitignored).
#   UIBENCH_HOST=<bench> harness/uibench/full.sh [WxH]     default 1920x1200, the van panel's resolution
set -euo pipefail
HOST=${UIBENCH_HOST:?set UIBENCH_HOST to the ssh host of the bench Pi, never the van}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
RES=${1:-1920x1200}
[ "${UIBENCH_SKIP_PREPARE:-0}" = 1 ] || "$ROOT/harness/uibench/prepare.sh"
dir="uibench/out/$(date +%Y%m%d-%H%M)-headless-$RES"
ssh -t "$HOST" "UIBENCH_OUT=\$HOME/$dir ~/uibench/harness/run.sh all headless $RES && \
  for rt in wpe tauri chromium; do ~/uibench/harness/perturb.sh --runtime \$rt home network > \$HOME/$dir/perturb-\$rt.txt 2>&1; done" || true
mkdir -p "$ROOT/harness/out/uibench"
rsync -a "$HOST:$dir" "$ROOT/harness/out/uibench/"
echo "results: $ROOT/harness/out/uibench/$(basename "$dir")"
