#!/bin/bash
# Run ON THE PI as root, from the synced tree: sudo /opt/aetherrv-rs/src/packaging/install.sh [--cutover]
# Installs the Rust system NEXT TO the Python one (/opt/aetherrv is left alone). Idempotent.
# Until the cutover it refuses a host with hat0/hat1 (the van) and never starts anything: the bench only.
set -euo pipefail
SRC=$(cd "$(dirname "$0")/.." && pwd)
DEST=/opt/aetherrv-rs
CUTOVER=0
[ "${1:-}" = "--cutover" ] && CUTOVER=1

if [ $CUTOVER = 0 ] && ip -br link | grep -qE '^(hat0|hat1)\b'; then
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
install -m 755 "$SRC"/packaging/panel/launch.sh "$DEST/panel/"
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
# the exact list of Rust units: cutover.sh and rollback.sh act on these and nothing else (the Python system's
# units share the aetherrv- prefix: aetherrv-kiosk, aetherrv-admin, aetherrv-update, aetherrv-splash-hold)
(cd "$SRC"/packaging/systemd && ls *.service *.socket *.target) > "$DEST/units.txt"
systemctl daemon-reload
echo "installed to $DEST (version $(cat "$DEST/VERSION")); nothing started. Start with: systemctl start aetherrv.target"
