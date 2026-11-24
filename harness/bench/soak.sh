#!/bin/bash
# On the BENCH Pi as root: the 12-hour soak (prompt §6). The full system against simulators (bench-up.sh, with the
# CLMD16 simulator flipping ignition every 10 min), then aetherrv-soak keeps it busy and samples it.
# Usage: soak.sh <n2k-capture.log> [hours]   Results: /var/tmp/soak/{samples.jsonl,summary.json}
set -euo pipefail
SRC=/opt/aetherrv-rs/src; BIN=/opt/aetherrv-rs/bin
CAP=${1:?usage: soak.sh <n2k capture> [hours]}; HOURS=${2:-12}
SERVICES=$($SRC/harness/bench/bench-up.sh "$CAP" --toggle-input 2 --every 600 | tail -1)
UNITS="aetherrv-bus.service"
for s in $SERVICES; do UNITS="$UNITS,aetherrv-$s.service"; done
rm -rf /var/tmp/soak
exec $BIN/aetherrv-soak --hours "$HOURS" --units "$UNITS" --out /var/tmp/soak
