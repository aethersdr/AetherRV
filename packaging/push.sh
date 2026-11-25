#!/bin/bash
# From the workstation: packaging/push.sh <ssh-host> [--cutover | --system]   (NO default host; never reads
# packaging/local.env, the van). Syncs the source to <host>:/opt/aetherrv-rs/src, builds natively there (cargo build
# --release), installs (packaging/install.sh, same flags). Refuses a host with hat0/hat1 unless --cutover (the
# cutover runbook does that, not you) or --system (a fresh Rust-only console; install.sh refuses where Python runs).
set -euo pipefail
HOST=${1:?usage: packaging/push.sh <ssh-host> [--cutover]}
MODE=${2:-}
cd "$(dirname "$0")/.."
if [ "$MODE" != "--cutover" ] && [ "$MODE" != "--system" ] && ssh -o BatchMode=yes "$HOST" "ip -br link" | grep -qE '^(hat0|hat1)\b'; then
  echo "refused: $HOST has hat0/hat1 (the van). Use the cutover runbook (docs/CUTOVER.md)." >&2
  exit 3
fi
if [ -f ui/package.json ] && [ -d ui/node_modules ]; then (cd ui && npm run build --silent); fi
git describe --always --dirty 2>/dev/null > packaging/VERSION || true
ssh "$HOST" "sudo mkdir -p /opt/aetherrv-rs/src"   # (rsync makes only the last level; a first install has no /opt/aetherrv-rs)
rsync -a --delete --exclude target --exclude .git --exclude ui/node_modules --exclude harness/out \
  --exclude packaging/local.env --exclude '.venv' --exclude __pycache__ --exclude docs/private \
  --rsync-path="sudo rsync" ./ "$HOST":/opt/aetherrv-rs/src/
ssh "$HOST" "set -o pipefail; cd /opt/aetherrv-rs/src && cargo build --release --workspace --bins 2>&1 | tail -3 && sudo packaging/install.sh $MODE"
