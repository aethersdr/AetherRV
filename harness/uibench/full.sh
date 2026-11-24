#!/bin/bash
# The whole UI runtime benchmark in one command, from the workstation: prepare, run the runtimes and the Python
# baseline on the bench (headless sway output, owner decision 2026-11-23), measure the instrument's own cost per
# runtime (perturb.sh), and fetch the results into harness/out/uibench/ (gitignored).
#   UIBENCH_HOST=<bench> harness/uibench/full.sh [WxH] [runtime...]
#     WxH       default 1920x1200, the van panel's resolution
#     runtime   default: python python-static wpe tauri chromium (run.sh's names; wpe-launcher is panel/wpe)
#   UIBENCH_KEEP_BUNDLE=1   measure the bundle the bench's gateway already serves (a deploy put it there); otherwise
#                           prepare.sh builds this tree's bundle and installs it in /opt/aetherrv-rs/ui
#   UIBENCH_SKIP_PREPARE=1  skip prepare.sh entirely (harness and tools already on the bench)
#   UIBENCH_ARGS            the runner's options (default: the documented ones, below)
# The run itself is a transient user unit on the bench (uibench-full), so a dropped ssh connection can't stop it.
set -euo pipefail
HOST=${UIBENCH_HOST:?set UIBENCH_HOST to the ssh host of the bench Pi, never the van}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
RES=1920x1200
if [[ ${1:-} =~ ^[0-9]+x[0-9]+$ ]]; then RES=$1; shift; fi
RTS=${*:-python python-static wpe tauri chromium}
ARGS=${UIBENCH_ARGS:---warm-rounds 3 --press-reps 12 --null-reps 3 --fps-secs 5}
if ! ssh -o BatchMode=yes "$HOST" '! ip -br link | grep -qE "^(hat0|hat1)\b"'; then echo "refused: $HOST has hat0/hat1, it is the van" >&2; exit 3; fi
[ "${UIBENCH_SKIP_PREPARE:-0}" = 1 ] || "$ROOT/harness/uibench/prepare.sh"
ssh -o BatchMode=yes "$HOST" 'systemctl --user is-active -q uibench-full' && { echo "refused: a uibench-full run is still active on $HOST" >&2; exit 5; }
dir="uibench/out/$(date +%Y%m%d-%H%M)-headless-$RES"
PERTURB=""
for rt in $RTS; do case $rt in wpe|wpe-launcher|tauri|chromium) PERTURB="$PERTURB $rt" ;; esac; done
# the run, as a script on the bench (its own unit: a dropped ssh connection can't stop it)
# shellcheck disable=SC2087  # the values are this side's on purpose; \$ marks what the bench expands
ssh -o BatchMode=yes "$HOST" "mkdir -p \$HOME/$dir && cat > \$HOME/$dir/full-run.sh" <<EOF
#!/bin/bash
UIBENCH_OUT=\$HOME/$dir ~/uibench/harness/run.sh "$RTS" headless $RES -- $ARGS
for rt in $PERTURB; do ~/uibench/harness/perturb.sh --runtime \$rt home network > \$HOME/$dir/perturb-\$rt.txt 2>&1; done
~/uibench/harness/session.sh stop >/dev/null 2>&1 || true
EOF
# shellcheck disable=SC2029  # expanded here on purpose
ssh -o BatchMode=yes "$HOST" "systemctl --user reset-failed uibench-full 2>/dev/null; systemd-run --user --quiet --unit=uibench-full \
  -p StandardOutput=append:\$HOME/$dir/full.log -p StandardError=append:\$HOME/$dir/full.log bash \$HOME/$dir/full-run.sh"
echo "running on $HOST as uibench-full: $RTS at $RES ($ARGS); log ~/$dir/full.log"
while ssh -o BatchMode=yes -o ConnectTimeout=10 "$HOST" 'systemctl --user is-active -q uibench-full'; do sleep 30; done
mkdir -p "$ROOT/harness/out/uibench"
rsync -a "$HOST:$dir" "$ROOT/harness/out/uibench/"
echo "results: $ROOT/harness/out/uibench/$(basename "$dir")"
grep -E "FAILED|refused" "$ROOT/harness/out/uibench/$(basename "$dir")/full.log" || true
