#!/bin/bash
# Exit 0 if neither heater is burning or in its after-run, judged from whichever system answers on :8080.
# Used by cutover.sh and rollback.sh before they stop the system that drives the Espar.
set -euo pipefail
S=$(curl -sS --max-time 5 http://127.0.0.1:8080/api/state) || { echo "no /api/state on :8080: can't tell (use --force)" >&2; exit 2; }
python_or_jq() { jq -r "$1" <<<"$S" 2>/dev/null; }
esp_active=$(python_or_jq '."climate.espar.ctl.active" // false')
esp_mode=$(python_or_jq '."climate.espar.ctl.mode" // "off"')
esp_state=$(python_or_jq '."climate.espar.state" // "off"')
mcs_state=$(python_or_jq '."climate.rixens.state" // ""')
mcs_code=$(python_or_jq '."climate.rixens.heater_state_code" // 0')
busy=""
[ "$esp_active" = "true" ] && [ "$esp_mode" != "off" ] && busy+=" espar:$esp_mode"
case "$esp_state" in heating|starting|cooldown|fan) busy+=" espar:$esp_state";; esac
[ "$mcs_state" = "heating" ] && busy+=" mcs7:heating"
case "$mcs_code" in 10|12|13|15|20|30|31) busy+=" mcs7:code$mcs_code";; esac
if [ -n "$busy" ]; then echo "heaters not idle:$busy" >&2; exit 1; fi
echo "heaters idle"
