#!/bin/bash
# Staged transmit for the Rust system (how the van was cut over, docs/CUTOVER.md). Run ON THE PI as root.
#   cutover.sh --read-only        (re)start the Rust system with EVERY transmit permission off
#   cutover.sh --allow dev[,dev]  allow transmit for these devices too (each must also have tx_enabled in the config)
#   cutover.sh --full             drop the allowlist: the config's tx_enabled decides
#   add --force to skip the "heaters idle" check (not recommended)
set -euo pipefail
cd "$(dirname "$0")"
MODE=${1:?usage: cutover.sh --read-only | --allow dev[,dev] | --full [--force]}
FORCE=0; for a in "$@"; do [ "$a" = "--force" ] && FORCE=1; done
DROPIN=/etc/systemd/system/aetherrv-.service.d/tx-allow.conf
[ "$(id -u)" = 0 ] || { echo "run as root" >&2; exit 2; }
write_allow() {
  install -d -m 755 "$(dirname "$DROPIN")"
  printf '[Service]\nEnvironment=AETHERRV_TX_ALLOW=%s\n' "$1" > "$DROPIN"
}
case "$MODE" in
  --read-only)
    # a running system restarts to take the empty allowlist, and restarting the Espar's service must not stop a burn
    if systemctl is-active -q aetherrv.target && [ $FORCE = 0 ]; then ./heaters-idle.sh; fi
    write_allow ""
    systemctl daemon-reload
    systemctl enable --now aetherrv-admind.socket
    UNITS=/opt/aetherrv-rs/units.txt
    [ -s "$UNITS" ] || { echo "no $UNITS: run install.sh first" >&2; exit 4; }
    # every Rust service except the socket-activated helper, the can-aux template and the panel (enabled below)
    systemctl enable aetherrv.target $(grep -E '\.service$' "$UNITS" | grep -vE '^aetherrv-(admind|can-aux@|panel)\.service$')
    for b in $(/opt/aetherrv-rs/bin/aetherrv-can-aux -c /etc/aetherrv/aetherrv.toml --list); do systemctl enable "aetherrv-can-aux@$b.service"; done
    if systemctl is-active -q aetherrv.target; then systemctl restart aetherrv.target; else systemctl start aetherrv.target; fi
    [ -f /etc/systemd/system/aetherrv-panel.service ] && systemctl enable --now aetherrv-panel.service
    echo "Rust system running READ-ONLY (no device may transmit). Compare for a while, then: cutover.sh --allow <device>"
    ;;
  --allow)
    devs=${2:?--allow needs a device list}
    cur=$(sed -n 's/^Environment=AETHERRV_TX_ALLOW=//p' "$DROPIN" 2>/dev/null || true)
    new=$(printf '%s\n' ${cur//,/ } ${devs//,/ } | sort -u | paste -sd, -)
    write_allow "$new"
    systemctl daemon-reload
    for d in ${devs//,/ }; do
      case "$d" in espar|espar_diag) u=aetherrv-espar;; n2k) u=aetherrv-n2k;; rixens_http) u=aetherrv-rixens;;
                   victron) u=aetherrv-victron;; starlink) u=aetherrv-starlink;; peplink) u=aetherrv-peplink;;
                   *) echo "unknown device $d" >&2; exit 2;; esac
      if [ "$u" = aetherrv-espar ] && [ $FORCE = 0 ]; then ./heaters-idle.sh; fi
      systemctl restart "$u.service"
    done
    systemctl restart aetherrv-gateway.service      # the UI's tx.enabled view
    echo "transmit allowed for: $new"
    ;;
  --full)
    rm -f "$DROPIN"; systemctl daemon-reload
    if [ $FORCE = 0 ]; then ./heaters-idle.sh; fi
    systemctl restart aetherrv.target
    echo "allowlist removed: the config's tx_enabled decides"
    ;;
  *) echo "unknown mode $MODE" >&2; exit 2 ;;
esac
