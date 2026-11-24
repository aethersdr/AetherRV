#!/bin/bash
# Workstation side of the UI runtime benchmark: build the production UI bundle and put it on the BENCH Pi's gateway
# (/opt/aetherrv-rs/ui only), copy the harness, the benchmark tool (utils/uibench), the Tauri panel (panel/tauri)
# and the Python reference UI (src/aetherrv + web, for the "before" numbers) to ~/uibench there, and build natively.
#   UIBENCH_HOST=<bench> harness/uibench/prepare.sh   (the bench's ssh host, never the van)
set -euo pipefail
HOST=${UIBENCH_HOST:?set UIBENCH_HOST to the ssh host of the bench Pi, never the van}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
if ! ssh -o BatchMode=yes "$HOST" '! ip -br link | grep -qE "^(hat0|hat1)\b"'; then echo "refused: $HOST has hat0/hat1, it is the van" >&2; exit 3; fi
TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
(cd "$ROOT/ui" && npx vite build --outDir "$TMP/ui-dist" --emptyOutDir >/dev/null)   # not ui/dist: others use it
rsync -a --delete "$TMP/ui-dist/" "$HOST:/tmp/ui-dist/"
ssh "$HOST" 'sudo rsync -a --delete /tmp/ui-dist/ /opt/aetherrv-rs/ui/ && mkdir -p ~/uibench/harness ~/uibench/crate ~/uibench/tauri ~/uibench/ref/src ~/uibench/ref/web ~/uibench/out'
rsync -a --delete "$ROOT/harness/uibench/" "$HOST:uibench/harness/"
rsync -a --delete --exclude target --exclude Cargo.lock "$ROOT/utils/uibench/" "$HOST:uibench/crate/"
rsync -a --delete --exclude target --exclude gen "$ROOT/panel/tauri/" "$HOST:uibench/tauri/"   # with its Cargo.lock
rsync -a --delete --exclude __pycache__ "$ROOT/src/aetherrv" "$HOST:uibench/ref/src/"
rsync -a --delete "$ROOT/web/" "$HOST:uibench/ref/web/"
# native builds, gently (the bench runs a soak): two jobs, lowest priority
ssh "$HOST" 'set -eo pipefail   # a failed build must stop here, not leave run.sh a stale binary
  cd ~/uibench/crate && nice -n 19 cargo build --release -j2 2>&1 | tail -3
  cd ~/uibench/tauri && nice -n 19 cargo build --release -j2 2>&1 | tail -3
  [ -x ~/uibench/venv/bin/python ] || { python3 -m venv ~/uibench/venv && ~/uibench/venv/bin/pip install -q aiohttp; }
  cd /opt/aetherrv-rs/ui && echo "bundle $(find . -type f | sort | xargs sha256sum | sha256sum | cut -c1-12)"'
