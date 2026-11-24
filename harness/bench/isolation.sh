#!/bin/bash
# On the BENCH Pi as root: the full system under systemd against simulators, then aetherrv-isolation kills and
# restarts every service and the broker in turn. Usage: isolation.sh <n2k-capture.log> [services...]
set -euo pipefail
ip -br link | grep -qE '^(hat0|hat1)\b' && { echo "refused: this is the van" >&2; exit 3; }
SRC=/opt/aetherrv-rs/src; BIN=/opt/aetherrv-rs/bin
CAP=${1:?usage: isolation.sh <n2k capture> [services]}; shift
SERVICES=${*:-"espar n2k rixens victron starlink peplink weather derive settings alerts history power-guard gateway netmon gps batteries-ble"}
$SRC/harness/dev-setup.sh >/dev/null
cmp -s $SRC/harness/bench/bench-config.toml /etc/aetherrv/aetherrv.toml || install -m 644 $SRC/harness/bench/bench-config.toml /etc/aetherrv/aetherrv.toml
if [ -f /etc/aetherrv/secrets.toml ] && ! cmp -s $SRC/harness/bench/bench-secrets.toml /etc/aetherrv/secrets.toml; then
  cp -n /etc/aetherrv/secrets.toml /etc/aetherrv/secrets.toml.pre-bench
fi
install -m 640 -g aetherrv $SRC/harness/bench/bench-secrets.toml /etc/aetherrv/secrets.toml
for u in aetherrv-sim-espar aetherrv-sim-mcs7 aetherrv-sim-replay aetherrv-sim-cerbo aetherrv-sim-starlink aetherrv-sim-peplink aetherrv-sim-clmd16 aetherrv-sim-gps; do systemctl stop $u 2>/dev/null || true; systemctl reset-failed $u 2>/dev/null || true; done
systemctl stop aetherrv.target 2>/dev/null || true
rm -f /run/aetherrv/espar/run.json /var/tmp/sim-mcs7.jsonl /var/tmp/sim-clmd16.jsonl
systemd-run --quiet --unit=aetherrv-sim-espar $BIN/aetherrv-sim-espar vcan1 --loss-timeout 15 --after-run 20 --sleep 5 --report /run/aetherrv-sim-espar.json
systemd-run --quiet --unit=aetherrv-sim-mcs7 $BIN/aetherrv-sim-mcs7 --port 18111 --log /var/tmp/sim-mcs7.jsonl
systemd-run --quiet --unit=aetherrv-sim-clmd16 $BIN/aetherrv-sim-clmd16 vcan0 --log /var/tmp/sim-clmd16.jsonl --report /run/aetherrv-sim-clmd16.json
systemd-run --quiet --unit=aetherrv-sim-replay $BIN/aetherrv-sim-replay "$CAP" vcan0 --loop --drop-src 0
systemd-run --quiet --unit=aetherrv-sim-gps $BIN/aetherrv-sim-gps --to 127.0.0.1:10110
systemd-run --quiet --unit=aetherrv-sim-cerbo $BIN/aetherrv-sim-cerbo --port 18883
systemd-run --quiet --unit=aetherrv-sim-starlink $BIN/aetherrv-sim-starlink --bind 127.0.0.1:9200
systemd-run --quiet --unit=aetherrv-sim-peplink $BIN/aetherrv-sim-peplink --bind 127.0.0.1:8443
sleep 2
systemctl enable --now aetherrv-admind.socket >/dev/null 2>&1
systemctl start aetherrv-bus
for s in $SERVICES; do systemctl start aetherrv-$s.service; done
sleep 8
csv=$(echo $SERVICES | tr ' ' ',')
$BIN/aetherrv-isolation --services "$csv" --out /var/tmp/isolation-report.json
