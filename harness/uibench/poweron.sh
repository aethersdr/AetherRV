#!/bin/bash
# Power-on to a usable UI over many reboots of the BENCH Pi, from the workstation (docs/UI-RUNTIME-BENCHMARK.md §8).
# Each reboot is boot-time.sh (firmware from the previous boot's last journal entry; the bench's own figures after);
# the summary is every boot's total (firmware + kernel → usable), the median, the max and how many are over 15.0 s.
#   UIBENCH_HOST=<bench> harness/uibench/poweron.sh rust <N> [runtime]
#       N reboots, the committed panel ordering and PAM stack (boot-measure.sh order rust); runtime default wpe-launcher
#   UIBENCH_HOST=<bench> harness/uibench/poweron.sh python <N>
#       the Python baseline: heaters idle → packaging/rollback.sh → the Python kiosk off (`disable --now`: the measured
#       Chromium, ordered like it, is the only UI) → N reboots → cutover.sh --read-only, then --full → the Rust panel
#       unit disabled again → checks (the Rust gateway serves, the Python units disabled)
# The simulators don't survive a reboot: the boots run without them (bench-up.sh brings them back).
set -euo pipefail
HOST=${UIBENCH_HOST:?set UIBENCH_HOST to the ssh host of the bench Pi, never the van}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
SYS=${1:?rust|python}; N=${2:?number of reboots}
if ! ssh -o BatchMode=yes "$HOST" '! ip -br link | grep -qE "^(hat0|hat1)\b"'; then echo "refused: $HOST has hat0/hat1, it is the van" >&2; exit 3; fi
P=/opt/aetherrv-rs/src/packaging
MARK=$(mktemp); trap 'rm -f "$MARK"' EXIT
case $SYS in
  rust) RT=${3:-wpe-launcher}; ORDER=rust
    # shellcheck disable=SC2029
    ssh -o BatchMode=yes "$HOST" "$P/heaters-idle.sh" ;;     # a reboot stops the Espar's controller
  python) RT=python; ORDER=python
    # shellcheck disable=SC2029
    ssh -o BatchMode=yes "$HOST" "set -e; $P/heaters-idle.sh; sudo $P/rollback.sh | tail -1; sudo systemctl disable --now aetherrv-kiosk.service 2>&1 | tail -1
      curl -sf -m 5 -o /dev/null http://127.0.0.1:8080/ && echo 'the Python system serves :8080'" ;;
  *) echo "rust|python" >&2; exit 2 ;;
esac
for i in $(seq "$N"); do
  echo "== boot $i of $N ($RT, $ORDER)"
  "$ROOT/harness/uibench/boot-time.sh" "$RT" "$ORDER" | grep -E "usable_frame_s|firmware from|refused" || true
done
if [ "$SYS" = python ]; then
  # shellcheck disable=SC2029
  ssh -o BatchMode=yes "$HOST" "set -e; sudo $P/cutover.sh --read-only | tail -1
    for i in \$(seq 30); do curl -s -m 2 127.0.0.1:8080/api/state | jq -e 'has(\"svc.gateway.up\")' >/dev/null 2>&1 && break; sleep 1; done
    sudo $P/cutover.sh --full | tail -1; sudo systemctl disable --now aetherrv-panel.service 2>&1 | tail -1
    echo \"rust gateway serving: \$(curl -s 127.0.0.1:8080/api/state | jq 'has(\"svc.gateway.up\")')\"
    for u in aetherrv.service aetherrv-kiosk.service aetherrv-admin.socket aetherrv-panel.service; do echo \"\$u: \$(systemctl is-active \$u) \$(systemctl is-enabled \$u)\"; done
    echo \"failed units: \$(systemctl --failed --plain --no-legend | wc -l)\""
fi
find "$ROOT/harness/out/uibench" -maxdepth 1 -name "boot-$RT-$ORDER-*.json" -newer "$MARK" | sort | python3 -c '
import json, statistics, sys
t = []
for f in sys.stdin.read().split():
    d = json.load(open(f))
    fw, ku = d.get("firmware_from_journal_s"), d.get("kernel_to_usable_s")
    if fw is None or ku is None: print("  %s: incomplete (firmware %s, kernel → usable %s)" % (f, fw, ku)); continue
    t.append(fw + ku); print("  firmware %.2f + kernel → usable %.2f = %.2f s" % (fw, ku, fw + ku))
if t: print("%d boots: median %.2f s, max %.2f s, %d over 15.0 s" % (len(t), statistics.median(t), max(t), sum(x > 15.0 for x in t)))'
