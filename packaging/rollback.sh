#!/bin/bash
# ONE COMMAND back to the Python system: sudo /opt/aetherrv-rs/src/packaging/rollback.sh [--force]
# Stops every Rust unit and starts the Python backend, kiosk and admin helper again. Data needs no conversion (both
# systems use the same files). Refuses while a heater is burning or in its after-run, unless --force: stopping the
# Espar's controller makes it shut down by itself and store a controller-loss fault.
set -euo pipefail
cd "$(dirname "$0")"
[ "$(id -u)" = 0 ] || { echo "run as root" >&2; exit 2; }
if [ "${1:-}" != "--force" ]; then ./heaters-idle.sh; fi
# only the Rust units (install.sh lists them); the Python ones share the prefix (kiosk, admin, update, splash-hold)
UNITS=/opt/aetherrv-rs/units.txt
[ -s "$UNITS" ] || { echo "no $UNITS: is the Rust system installed?" >&2; exit 4; }
units=$(grep -v '@\.service$' "$UNITS")
units="$units $(systemctl list-units --all --plain --no-legend 'aetherrv-can-aux@*' | awk '{print $1}')"
systemctl disable --now $units 2>/dev/null || true
rm -f /etc/systemd/system/aetherrv-.service.d/tx-allow.conf
systemctl daemon-reload
systemctl enable --now aetherrv-admin.socket
systemctl enable aetherrv.service aetherrv-kiosk.service
systemctl start aetherrv.service
for i in $(seq 30); do curl -sS --max-time 2 http://127.0.0.1:8080/api/state >/dev/null 2>&1 && break; sleep 1; done
systemctl start aetherrv-kiosk.service
echo "Python system: backend $(systemctl is-active aetherrv.service), kiosk $(systemctl is-active aetherrv-kiosk.service), admin $(systemctl is-active aetherrv-admin.socket)"
systemctl is-active -q aetherrv.service
