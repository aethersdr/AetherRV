#!/bin/bash
# Run ON THE PI as root, from the synced tree: sudo /opt/aetherrv-rs/src/packaging/install.sh [--cutover | --system]
#   (none)      install, start nothing: the bench
#   --cutover   the same on a console (the van): installs next to what runs, starts nothing (the name is from the
#               cutover; the van's deploys use it since)
#   --system    a fresh console (docs/BUILD-GUIDE.md §5): the OS pieces too, then enable and start it; refused where
#               the old Python system is installed (that one goes through the cutover, docs/CUTOVER.md)
# Idempotent. Without --cutover/--system it refuses a host with hat0/hat1 (a console, not the bench).
set -euo pipefail
SRC=$(cd "$(dirname "$0")/.." && pwd)
DEST=/opt/aetherrv-rs
MODE=${1:-}
CUTOVER=0
[ "$MODE" = "--cutover" ] && CUTOVER=1

if [ "$MODE" = "--system" ]; then
  if [ -f /etc/systemd/system/aetherrv.service ] || [ -d /opt/aetherrv/src/aetherrv ]; then
    echo "refused: a Python AetherRV is installed here; switch with the cutover runbook (docs/CUTOVER.md)" >&2
    exit 3
  fi
  SRC=$SRC "$SRC"/packaging/system-setup.sh
elif [ $CUTOVER = 0 ] && ip -br link | grep -qE '^(hat0|hat1)\b'; then
  echo "refused: this host has hat0/hat1 (the van). The Rust system is installed in the van only by the cutover." >&2
  exit 3
fi

id aetherrv &>/dev/null || useradd --system --home-dir /var/lib/aetherrv --shell /usr/bin/nologin aetherrv
install -d -m 755 "$DEST" "$DEST/bin" "$DEST/etc" "$DEST/ui"
install -m 755 "$SRC"/target/release/aetherrv-* "$DEST/bin/" 2>/dev/null || true
rm -f "$DEST"/bin/*.d
install -m 644 "$SRC"/packaging/etc/mosquitto.conf "$SRC"/packaging/etc/mosquitto.acl "$DEST/etc/"
install -d -m 755 "$DEST/panel"
install -m 644 "$SRC"/packaging/panel/sway.conf "$DEST/panel/"
install -m 755 "$SRC"/packaging/panel/launch.sh "$SRC"/packaging/panel/screen.sh "$SRC"/packaging/panel/wait-hdmi.sh "$DEST/panel/"
# the WPE kiosk launcher (panel/wpe), built here where WPE's headers are; launch.sh falls back to MiniBrowser without it
if pkg-config --exists wpe-webkit-2.0 2>/dev/null && command -v cc >/dev/null; then
  # built beside the running one and renamed over it, so a panel running the old launcher is not disturbed
  # shellcheck disable=SC2046  # pkg-config's flags are words
  if cc -O2 -Wall -o "$DEST/bin/aetherrv-panel-wpe.new" "$SRC"/panel/wpe/aetherrv-panel-wpe.c $(pkg-config --cflags --libs wpe-webkit-2.0); then
    mv -f "$DEST/bin/aetherrv-panel-wpe.new" "$DEST/bin/aetherrv-panel-wpe"
  else
    rm -f "$DEST/bin/aetherrv-panel-wpe.new"
    echo "aetherrv-panel-wpe did not build: the panel's wpe runtime uses MiniBrowser" >&2
  fi
fi
if [ -d "$SRC/ui/dist" ]; then
  rm -rf "$DEST/ui.new" && cp -a "$SRC/ui/dist" "$DEST/ui.new" && rm -rf "$DEST/ui" && mv "$DEST/ui.new" "$DEST/ui"
fi
if [ -s "$SRC/packaging/VERSION" ]; then cp "$SRC/packaging/VERSION" "$DEST/VERSION"; else (cd "$SRC" && git describe --always --dirty 2>/dev/null || echo unknown) > "$DEST/VERSION"; fi
ls "$DEST"/bin/aetherrv-* >/dev/null 2>&1 || { echo "no binaries built: nothing installed" >&2; exit 4; }
chmod -R a+rX "$DEST"

install -d -m 755 /etc/aetherrv
[ -f /etc/aetherrv/aetherrv.toml ] || install -m 644 "$SRC"/config/aetherrv.example.toml /etc/aetherrv/aetherrv.toml
[ -f /etc/aetherrv/secrets.toml ] || printf '# AetherRV secrets (root:aetherrv 0640)\n' > /etc/aetherrv/secrets.toml
chown root:aetherrv /etc/aetherrv/secrets.toml && chmod 640 /etc/aetherrv/secrets.toml
install -d -o aetherrv -g aetherrv -m 750 /var/lib/aetherrv

install -m 644 "$SRC"/packaging/systemd/*.service "$SRC"/packaging/systemd/*.socket "$SRC"/packaging/systemd/*.target /etc/systemd/system/
# the panel unit's PAM stack (PAMName=aetherrv-panel): here too, as the cutover doesn't run system-setup.sh
install -m 644 "$SRC"/packaging/pam/aetherrv-panel /etc/pam.d/aetherrv-panel
# the exact list of the system's units: cutover.sh acts on these and nothing else (other units share the aetherrv-
# prefix: aetherrv-update and aetherrv-splash-hold, and the old Python system's aetherrv, -kiosk and -admin)
(cd "$SRC"/packaging/systemd && ls *.service *.socket *.target) > "$DEST/units.txt"
# Settings → Updates (admind starts aetherrv-update.service): outside units.txt, so cutover.sh never enables or
# restarts it. Replaces the Python install's unit of the same name, whose script checks the Python environment and
# would fail every update once /opt/aetherrv is gone.
install -m 755 "$SRC"/packaging/update/aetherrv-update.sh "$DEST/bin/"
install -m 644 "$SRC"/packaging/update/aetherrv-update.service /etc/systemd/system/
# the shutdown splash waits for the panel to release the display (as it does for the Python kiosk)
for u in plymouth-reboot plymouth-poweroff plymouth-halt; do
  if [ -d /etc/systemd/system/$u.service.d ] || [ "$MODE" = "--system" ]; then
    install -d -m 755 /etc/systemd/system/$u.service.d
    install -m 644 "$SRC"/packaging/boot/aetherrv-plymouth-after-panel.conf /etc/systemd/system/$u.service.d/aetherrv-panel.conf
  fi
done
systemctl daemon-reload
if [ "$MODE" = "--system" ]; then
  systemctl enable --now aetherrv-admind.socket
  systemctl enable aetherrv.target $(grep -E '\.service$' "$DEST/units.txt" | grep -vE '^aetherrv-(admind|can-aux@|panel)\.service$')
  for b in $("$DEST"/bin/aetherrv-can-aux -c /etc/aetherrv/aetherrv.toml --list 2>/dev/null); do systemctl enable "aetherrv-can-aux@$b.service"; done
  systemctl restart aetherrv.target
  systemctl enable aetherrv-panel.service && systemctl restart aetherrv-panel.service
  echo "installed to $DEST (version $(cat "$DEST/VERSION")) and started: $(systemctl is-active aetherrv-gateway) / panel $(systemctl is-active aetherrv-panel)"
else
  echo "installed to $DEST (version $(cat "$DEST/VERSION")); nothing started. Start with: systemctl start aetherrv.target"
fi
