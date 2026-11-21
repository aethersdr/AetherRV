#!/bin/bash
# One-time rename VanPanel → AetherRV on the Pi. Run by install.sh before anything else; every step is guarded
# ("old exists and new doesn't"), so a partial run can simply be re-run, and on an already-migrated or fresh
# system it does nothing. Keeps the service user's UID/GID (file ownership carries over), the live config and
# secrets (incl. the Cerbo MQTT pairing token), history, settings and the kiosk's Chromium profile.
# Rollback material: /root/vanpanel-pre-aetherrv-<time>/ and /opt/vanpanel.pre-aetherrv (old code tree).
set -euo pipefail

if ! id vanpanel &>/dev/null && [ ! -e /etc/vanpanel ] && [ ! -e /var/lib/vanpanel ] && \
   ! ls /etc/systemd/system/vanpanel* &>/dev/null; then
  exit 0                                                     # nothing to migrate
fi
if id vanpanel &>/dev/null && id aetherrv &>/dev/null; then
  echo "migrate: both users 'vanpanel' and 'aetherrv' exist — refusing; resolve by hand" >&2; exit 1
fi

BK=/root/vanpanel-pre-aetherrv-$(date +%Y%m%d-%H%M%S)
mkdir -p "$BK"; chmod 700 "$BK"
[ -d /etc/vanpanel ] && cp -a /etc/vanpanel "$BK/etc-vanpanel"
cp -a /etc/systemd/system/vanpanel* "$BK/" 2>/dev/null || true
cp -a /etc/pam.d/vanpanel-kiosk "$BK/" 2>/dev/null || true
getent passwd vanpanel > "$BK/passwd" || true; getent group vanpanel > "$BK/group" || true
echo "migrate: backup in $BK"

echo "migrate: stopping the VanPanel units"
for u in vanpanel-kiosk.service vanpanel.service vanpanel-admin.socket vanpanel-admin.service vanpanel-update.service; do
  systemctl disable --now "$u" 2>/dev/null || systemctl stop "$u" 2>/dev/null || true
done
pkill -f /opt/vanpanel/deploy/kiosk/screen_helper.py 2>/dev/null || true   # lives in the login session scope
if id vanpanel &>/dev/null; then
  for _ in $(seq 20); do pgrep -u vanpanel >/dev/null || break; sleep 0.5; done
  pkill -u vanpanel 2>/dev/null || true; sleep 1
  usermod -l aetherrv -d /var/lib/aetherrv vanpanel          # same UID 963: ownership carries over
  echo "migrate: user vanpanel → aetherrv"
fi
if getent group vanpanel >/dev/null; then groupmod -n aetherrv vanpanel; echo "migrate: group vanpanel → aetherrv"; fi

if [ -d /etc/vanpanel ] && [ ! -e /etc/aetherrv ]; then mv /etc/vanpanel /etc/aetherrv; echo "migrate: /etc/aetherrv"; fi
if [ -f /etc/aetherrv/vanpanel.toml ] && [ ! -e /etc/aetherrv/aetherrv.toml ]; then
  mv /etc/aetherrv/vanpanel.toml /etc/aetherrv/aetherrv.toml
fi
if [ -f /etc/aetherrv/aetherrv.toml ]; then                   # absolute paths inside the live config
  sed -i -e 's#/var/lib/vanpanel#/var/lib/aetherrv#g' -e 's#/opt/vanpanel#/opt/aetherrv#g' \
         -e 's#/etc/vanpanel#/etc/aetherrv#g' -e 's/vanpanel\.toml/aetherrv.toml/g' -e 's/VanPanel/AetherRV/g' /etc/aetherrv/aetherrv.toml
fi
# secrets.toml: only the comment header (the Cerbo MQTT pairing token is an identity — kept as-is)
[ -f /etc/aetherrv/secrets.toml ] && sed -i 's/^# VanPanel secrets (root:vanpanel /# AetherRV secrets (root:aetherrv /' /etc/aetherrv/secrets.toml

for d in "" -kiosk -update; do
  if [ -d /var/lib/vanpanel$d ] && [ ! -e /var/lib/aetherrv$d ]; then mv /var/lib/vanpanel$d /var/lib/aetherrv$d; echo "migrate: /var/lib/aetherrv$d"; fi
done

rm -f /etc/systemd/system/vanpanel*.service /etc/systemd/system/vanpanel*.socket /etc/pam.d/vanpanel-kiosk
rm -rf /usr/local/lib/vanpanel /usr/share/vanpanel /run/vanpanel-update
if [ -d /opt/vanpanel ] && [ ! -e /opt/vanpanel.pre-aetherrv ]; then mv /opt/vanpanel /opt/vanpanel.pre-aetherrv; fi
systemctl daemon-reload
echo "migrate: done"
