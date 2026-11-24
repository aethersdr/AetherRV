#!/bin/bash
# The van cutover (docs/CUTOVER.md). Run ON THE PI as root, with the owner present.
#   cutover.sh --read-only        stop the Python system, start the Rust one with EVERY transmit permission off
#   cutover.sh --allow dev[,dev]  allow transmit for these devices too (each must also have tx_enabled in the config)
#   cutover.sh --full             drop the allowlist: the config's tx_enabled decides, as in Python
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
    if systemctl is-active -q aetherrv.service && [ $FORCE = 0 ]; then ./heaters-idle.sh; fi
    write_allow ""
    systemctl daemon-reload
    systemctl disable --now aetherrv-kiosk.service aetherrv.service aetherrv-admin.socket aetherrv-admin.service 2>/dev/null || true
    systemctl enable --now aetherrv-admind.socket
    systemctl enable aetherrv.target aetherrv-bus.service $(cd /etc/systemd/system && ls aetherrv-*.service | grep -vE '^aetherrv-(admind|bus|can-aux@|update)\.service$|^aetherrv-panel' || true)
    for b in $(/opt/aetherrv-rs/bin/aetherrv-can-aux -c /etc/aetherrv/aetherrv.toml --list); do systemctl enable "aetherrv-can-aux@$b.service"; done
    systemctl start aetherrv.target
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
