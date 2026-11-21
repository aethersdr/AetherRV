#!/bin/bash
# From the workstation: sync the tree to the Pi and run the installer.
set -euo pipefail
# Target from the argument, or AETHERRV_HOST in deploy/local.env (gitignored; see deploy/local.env.example).
[ -f "$(dirname "$0")/local.env" ] && . "$(dirname "$0")/local.env"
HOST=${1:-${AETHERRV_HOST:-}}
[ -n "$HOST" ] || { echo "usage: deploy/push.sh <ssh-host>   (or set AETHERRV_HOST in deploy/local.env)" >&2; exit 2; }
cd "$(dirname "$0")/.."
uv lock -q
git describe --always --dirty --tags 2>/dev/null > VERSION || echo unknown > VERSION   # shown in Settings → About
rsync -a --delete --exclude .venv --exclude .git --exclude __pycache__ --exclude captures --exclude deploy/local.env \
  --rsync-path="sudo rsync" ./ "$HOST":/opt/aetherrv/
ssh "$HOST" sudo /opt/aetherrv/deploy/install.sh
