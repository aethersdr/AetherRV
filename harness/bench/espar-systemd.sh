#!/bin/bash
# On the BENCH Pi (refuses a host with hat0/hat1), as root: the Espar service under real systemd against the heater
# simulator. A heat run survives `systemctl restart`, `kill -9`, a SIGSTOP (watchdog) and a broker restart.
set -euo pipefail
ip -br link | grep -qE '^(hat0|hat1)\b' && { echo "refused: this is the van" >&2; exit 3; }
SRC=/opt/aetherrv-rs/src
CTL="sudo -u aetherrv /opt/aetherrv-rs/bin/aetherrv-ctl"
REPORT=/run/aetherrv-sim-espar.json
$SRC/harness/dev-setup.sh >/dev/null
if ! cmp -s $SRC/harness/bench/bench-config.toml /etc/aetherrv/aetherrv.toml; then
  [ -f /etc/aetherrv/aetherrv.toml ] && cp -a /etc/aetherrv/aetherrv.toml /etc/aetherrv/aetherrv.toml.pre-bench-$(date +%s)
  install -m 644 $SRC/harness/bench/bench-config.toml /etc/aetherrv/aetherrv.toml
fi
systemctl stop aetherrv-espar aetherrv-gateway 2>/dev/null || true
systemctl stop aetherrv-sim-espar 2>/dev/null || true; systemctl reset-failed aetherrv-sim-espar 2>/dev/null || true
rm -f /run/aetherrv/espar/run.json
systemd-run --quiet --unit=aetherrv-sim-espar /opt/aetherrv-rs/bin/aetherrv-sim-espar vcan1 --loss-timeout 15 --after-run 20 --sleep 5 --report $REPORT --log /var/tmp/sim-espar.jsonl
systemctl enable --now aetherrv-admind.socket >/dev/null 2>&1
systemctl start aetherrv-bus aetherrv-espar aetherrv-gateway
sleep 3
step() { echo "== $*"; }
rep() { cat $REPORT; echo; }
step "FAN (wakes the heater; the read-only diagnostics then read its intake temperature)"
$CTL cmd espar set '{"mode":"fan"}'
for i in $(seq 30); do sleep 1; $CTL get climate.espar.ctl.room_c | grep -q 'owner espar' && $CTL get climate.espar.ctl.room_c | grep -qv '= null' && break; done
$CTL get climate.espar.ctl.room_c climate.espar.ctl.room_src
step "HEAT"
$CTL cmd espar set '{"mode":"heat","setpoint_c":21}'
sleep 3; rep
step "systemctl restart aetherrv-espar (SIGTERM: no OFF, resume)"; systemctl restart aetherrv-espar; sleep 5; rep
step "kill -9 (systemd restarts it in 100 ms; resume)"; systemctl kill -s KILL aetherrv-espar; sleep 5; rep
step "SIGSTOP (the 2 s watchdog kills and restarts it; resume)"; systemctl kill -s STOP aetherrv-espar; sleep 8; rep
step "broker restart (the run never notices)"; systemctl restart aetherrv-bus; sleep 5; rep
systemctl show aetherrv-espar -p NRestarts -p ActiveState
journalctl -u aetherrv-espar --since "-3min" --no-pager | grep -E 'resumed|not resuming|watchdog|stopping' | tail -8
sudo -u aetherrv sqlite3 /var/lib/aetherrv/history.sqlite3 "SELECT cmd, outcome, detail FROM command_log WHERE device='espar' ORDER BY t DESC LIMIT 8" 2>/dev/null || true
step "OFF"; $CTL cmd espar set '{"mode":"off"}'; sleep 2; rep
