#!/bin/bash
# On the BENCH Pi as root: the cutover and the ONE-COMMAND rollback, tested against a real Python install (deliverable
# 5). Starts where the van starts: the Python system running (/opt/aetherrv, deploy/install.sh), the Rust system
# installed but stopped. Simulators stand in for every device (bench-up.sh). Each step asserts the unit states and
# which system answers on :8080. Ends back on the Rust system (cutover.sh --full) for the other bench work.
#   Prerequisite (from the workstation): rsync the repo to aetherpi:/opt/aetherrv (see docs/STATUS.md).
set -uo pipefail
ip -br link | grep -qE '^(hat0|hat1)\b' && { echo "refused: this is the van" >&2; exit 3; }
SRC=/opt/aetherrv-rs/src; P=$SRC/packaging
CTL="sudo -u aetherrv /opt/aetherrv-rs/bin/aetherrv-ctl"
PASS=0; FAIL=0
ok()   { echo "  ok   $*"; PASS=$((PASS + 1)); }
bad()  { echo "  FAIL $*"; FAIL=$((FAIL + 1)); }
check() { local what=$1; shift; if "$@" >/dev/null 2>&1; then ok "$what"; else bad "$what"; fi; }
state() { curl -sS --max-time 5 http://127.0.0.1:8080/api/state; }
rust_serving()   { state | jq -e 'has("svc.gateway.up")' >/dev/null; }     # the Rust gateway's own keys (D7)
python_serving() { state | jq -e 'has("svc.gateway.up") | not' >/dev/null && state | jq -e 'length > 20' >/dev/null; }
wait_for() { local n=$1; shift; for _ in $(seq "$n"); do "$@" >/dev/null 2>&1 && return 0; sleep 1; done; return 1; }
step() { echo "== $(date +%T) $*"; }
secs() { awk -v a="$1" -v b="$(date +%s%N)" 'BEGIN { printf "%.1f", (b - a) / 1e9 }'; }
rust_units() { grep -v '@\.service$' /opt/aetherrv-rs/units.txt | grep -vE '^aetherrv-(admind\.service|panel\.service)$'; }
none_active() { for u in $(rust_units); do systemctl is-active -q "$u" && { echo "still active: $u"; return 1; }; done; return 0; }
none_enabled() { for u in $(rust_units); do [ "$(systemctl is-enabled "$u" 2>/dev/null)" = enabled ] && { echo "still enabled: $u"; return 1; }; done; return 0; }

step "0. the van's starting point: simulators up, the Rust system stopped and disabled, the Python system installed"
$SRC/harness/bench/bench-up.sh /var/tmp/n2k-replay.log >/dev/null
systemctl disable --now aetherrv.target $(rust_units) aetherrv-admind.socket >/dev/null 2>&1 || true
[ -d /opt/aetherrv/src/aetherrv ] || { echo "no Python tree in /opt/aetherrv (rsync it first)" >&2; exit 4; }
/opt/aetherrv/deploy/install.sh 2>&1 | tail -2
check "Python backend active" systemctl is-active -q aetherrv.service
check "Python admin socket active" systemctl is-active -q aetherrv-admin.socket
check "Python kiosk enabled" bash -c '[ "$(systemctl is-enabled aetherrv-kiosk.service)" = enabled ]'
check "the Python system answers on :8080" wait_for 30 python_serving
check "heaters idle (Python's view)" $P/heaters-idle.sh
UPD0=$(systemctl is-enabled aetherrv-update.service 2>/dev/null || echo missing)

step "1. cutover.sh --read-only"
t0=$(date +%s%N)
$P/cutover.sh --read-only
check "Python backend stopped and disabled" bash -c '! systemctl is-active -q aetherrv.service && [ "$(systemctl is-enabled aetherrv.service)" != enabled ]'
check "Python kiosk stopped and NOT re-enabled" bash -c '! systemctl is-active -q aetherrv-kiosk.service && [ "$(systemctl is-enabled aetherrv-kiosk.service)" != enabled ]'
check "Python admin socket stopped" bash -c '! systemctl is-active -q aetherrv-admin.socket'
check "the Rust system answers on :8080" wait_for 30 rust_serving
echo "  (read-only cutover to a serving Rust gateway: $(secs "$t0") s)"
check "every transmit permission off" bash -c "curl -sS http://127.0.0.1:8080/api/state | jq -e '[to_entries[] | select(.key | startswith(\"tx.enabled.\")) | .value] | length > 0 and all(. == false)'"
check "an Espar command is refused" bash -c "$CTL cmd espar set '{\"mode\":\"fan\"}' | grep -q refused"
check "the Python update unit untouched" bash -c "[ \"\$(systemctl is-enabled aetherrv-update.service 2>/dev/null || echo missing)\" = '$UPD0' ]"

step "2. cutover.sh --allow n2k"
$P/cutover.sh --allow n2k
check "tx.enabled.n2k on" wait_for 15 bash -c "curl -sS http://127.0.0.1:8080/api/state | jq -e '.\"tx.enabled.n2k\" == true'"
check "tx.enabled.espar still off" bash -c "curl -sS http://127.0.0.1:8080/api/state | jq -e '.\"tx.enabled.espar\" == false'"

step "3. cutover.sh --full (the config's tx_enabled decides)"
$P/cutover.sh --full
check "allowlist drop-in removed" bash -c '[ ! -f /etc/systemd/system/aetherrv-.service.d/tx-allow.conf ]'
check "tx.enabled.espar on (bench config)" wait_for 20 bash -c "curl -sS http://127.0.0.1:8080/api/state | jq -e '.\"tx.enabled.espar\" == true'"

step "4. rollback refuses while the Espar heats"
$CTL cmd espar set '{"mode":"fan"}' >/dev/null
wait_for 30 bash -c "$CTL get climate.espar.ctl.room_c | grep -qv '= null'" || true
$CTL cmd espar set '{"mode":"heat","setpoint_c":21}'
wait_for 20 bash -c "curl -sS http://127.0.0.1:8080/api/state | jq -e '.\"climate.espar.state\" == \"heating\"'" || true
$P/rollback.sh; rc=$?
check "rollback.sh refused (exit $rc)" test "$rc" -ne 0
check "the Rust system still running" systemctl is-active -q aetherrv-espar.service
check "the heater still heating" bash -c "grep -q '\"state\":\"heating\"' /run/aetherrv-sim-espar.json"
$CTL cmd espar set '{"mode":"off"}'
check "and refused during the after-run too" bash -c "sleep 3; ! $P/heaters-idle.sh"
check "heaters idle after the after-run" wait_for 90 $P/heaters-idle.sh

step "5. rollback.sh (one command)"
t0=$(date +%s%N)
$P/rollback.sh; rc=$?
echo "  (rollback: $(secs "$t0") s)"
check "rollback.sh exit 0" test "$rc" -eq 0
check "no Rust unit active" none_active
check "no Rust unit enabled" none_enabled
check "Rust admin helper socket stopped" bash -c '! systemctl is-active -q aetherrv-admind.socket'
check "Python backend active and enabled" bash -c 'systemctl is-active -q aetherrv.service && [ "$(systemctl is-enabled aetherrv.service)" = enabled ]'
check "Python kiosk enabled" bash -c '[ "$(systemctl is-enabled aetherrv-kiosk.service)" = enabled ]'
check "Python admin socket active" systemctl is-active -q aetherrv-admin.socket
check "the Python system answers on :8080" wait_for 30 python_serving
check "the Python update unit untouched" bash -c "[ \"\$(systemctl is-enabled aetherrv-update.service 2>/dev/null || echo missing)\" = '$UPD0' ]"
check "no transmit allowlist left behind" bash -c '[ ! -f /etc/systemd/system/aetherrv-.service.d/tx-allow.conf ]'

step "6. back to the Rust system for the rest of the bench work"
$P/cutover.sh --read-only && $P/cutover.sh --full
check "the Rust system answers on :8080" wait_for 30 rust_serving
check "the Python backend disabled again" bash -c '[ "$(systemctl is-enabled aetherrv.service)" != enabled ]'
echo "ROLLBACK TEST: $PASS ok, $FAIL failed"
[ "$FAIL" = 0 ]
