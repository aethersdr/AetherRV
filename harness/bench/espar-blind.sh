#!/bin/bash
# On the BENCH Pi as root: the Espar running blind under systemd (owner decision, ARCHITECTURE.md §7.3). With the
# broker STOPPED (no bus contact at all) a heat run continues for blind_off_s, then the service turns the heater OFF
# by itself and it goes through its normal after-run: no controller loss, no fault. A broker outage shorter than
# blind_off_s changes nothing. Uses a copy of the bench config with blind_off_s = 60 (restored at the end).
set -euo pipefail
ip -br link | grep -qE '^(hat0|hat1)\b' && { echo "refused: this is the van" >&2; exit 3; }
SRC=/opt/aetherrv-rs/src
CTL="sudo -u aetherrv /opt/aetherrv-rs/bin/aetherrv-ctl"
REPORT=/run/aetherrv-sim-espar.json
BLIND=${BLIND:-60}
$SRC/harness/dev-setup.sh >/dev/null
for u in aetherrv-soak-run; do systemctl is-active -q $u && { echo "refused: $u is running" >&2; exit 4; }; done
systemctl stop aetherrv.target 2>/dev/null || true
systemctl stop aetherrv-sim-espar 2>/dev/null || true; systemctl reset-failed aetherrv-sim-espar 2>/dev/null || true
sed "s/^diag_interval_s = 1$/diag_interval_s = 1\nblind_off_s = $BLIND/" $SRC/harness/bench/bench-config.toml > /etc/aetherrv/aetherrv.toml
trap 'install -m 644 $SRC/harness/bench/bench-config.toml /etc/aetherrv/aetherrv.toml' EXIT
rm -f /run/aetherrv/espar/run.json
systemd-run --quiet --unit=aetherrv-sim-espar /opt/aetherrv-rs/bin/aetherrv-sim-espar vcan1 --loss-timeout 15 --after-run 20 --sleep 5 --report $REPORT --log /var/tmp/sim-espar.jsonl
systemctl enable --now aetherrv-admind.socket >/dev/null 2>&1
systemctl start aetherrv-bus aetherrv-espar
sleep 3
step() { echo "== $(date +%T) $*"; }
rep() { cat $REPORT; echo; }
step "FAN, then HEAT"
$CTL cmd espar set '{"mode":"fan"}'
for i in $(seq 30); do sleep 1; $CTL get climate.espar.ctl.room_c | grep -qv '= null' && break; done
$CTL cmd espar set '{"mode":"heat","setpoint_c":21}'
sleep 3; rep
step "broker stopped for $((BLIND / 2)) s (shorter than blind_off_s = $BLIND s): the run continues"
systemctl stop aetherrv-bus; sleep $((BLIND / 2)); rep; systemctl start aetherrv-bus; sleep 5
$CTL get climate.espar.ctl.mode
step "broker stopped for good: after $BLIND s of no bus contact the service turns the heater OFF"
systemctl stop aetherrv-bus
t0=$(date +%s)
for i in $(seq $((BLIND + 60))); do
  sleep 1
  st=$(sed -n 's/.*"state":"\([^"]*\)".*/\1/p' $REPORT)
  [ "$st" != heating ] && break
done
echo "heater left 'heating' after $(( $(date +%s) - t0 )) s: $st"
sleep 25; rep
journalctl -u aetherrv-espar --since "-5min" --no-pager | grep -iE 'blind|no bus contact|off' | tail -5
systemctl start aetherrv-bus; sleep 3
sudo -u aetherrv sqlite3 /var/lib/aetherrv/history.sqlite3 "SELECT cmd, outcome, detail FROM command_log WHERE device='espar' ORDER BY t DESC LIMIT 3" 2>/dev/null || true
