#!/bin/bash
# On the BENCH Pi as root: the full system against simulators (bench-up.sh), then aetherrv-isolation kills and
# restarts every service and the broker in turn. Usage: isolation.sh <n2k-capture.log>
set -euo pipefail
SRC=/opt/aetherrv-rs/src; BIN=/opt/aetherrv-rs/bin
SERVICES=$($SRC/harness/bench/bench-up.sh "${1:?usage: isolation.sh <n2k capture>}" | tail -1)
$BIN/aetherrv-isolation --services "$(echo $SERVICES | tr ' ' ',')" --out /var/tmp/isolation-report.json
