#!/bin/bash
# Power-on to a usable UI over many reboots of the BENCH Pi, from the workstation (docs/UI-RUNTIME-BENCHMARK.md §8).
# Each reboot is boot-time.sh (firmware from the previous boot's last journal entry; the bench's own figures after);
# the summary is every boot's total (firmware + kernel → usable), the median, the max and how many are over 15.0 s.
#   UIBENCH_HOST=<bench> harness/uibench/poweron.sh rust <N> [runtime]
#       N reboots, the committed panel ordering and PAM stack (boot-measure.sh order rust); runtime default wpe-launcher
# The simulators don't survive a reboot: the boots run without them (bench-up.sh brings them back).
set -euo pipefail
HOST=${UIBENCH_HOST:?set UIBENCH_HOST to the ssh host of the bench Pi, never the van}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
SYS=${1:?rust}; N=${2:?number of reboots}
if ! ssh -o BatchMode=yes "$HOST" '! ip -br link | grep -qE "^(hat0|hat1)\b"'; then echo "refused: $HOST has hat0/hat1, it is the van" >&2; exit 3; fi
P=/opt/aetherrv-rs/src/packaging
MARK=$(mktemp); trap 'rm -f "$MARK"' EXIT
cargo build --release --quiet --manifest-path "$ROOT/utils/uibench/Cargo.toml"   # it sums the boots up (as boot-time.sh)
BIN=${CARGO_TARGET_DIR:-$ROOT/utils/uibench/target}/release/aetherrv-uibench
case $SYS in
  rust) RT=${3:-wpe-launcher}; ORDER=rust
    # shellcheck disable=SC2029
    ssh -o BatchMode=yes "$HOST" "$P/heaters-idle.sh" ;;     # a reboot stops the Espar's controller
  *) echo "rust" >&2; exit 2 ;;
esac
for i in $(seq "$N"); do
  echo "== boot $i of $N ($RT, $ORDER)"
  "$ROOT/harness/uibench/boot-time.sh" "$RT" "$ORDER" | grep -E "usable_frame_s|firmware from|refused" || true
done
find "$ROOT/harness/out/uibench" -maxdepth 1 -name "boot-$RT-$ORDER-*.json" -newer "$MARK" | sort | "$BIN" digest poweron
