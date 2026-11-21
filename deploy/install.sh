#!/bin/bash
# Run ON THE PI as root, from the rsynced tree: sudo /opt/aetherrv/deploy/install.sh
# Idempotent. Doesn't touch /boot, the kernel, or existing config (aetherrv.toml is only created if missing).
set -euo pipefail
SRC=/opt/aetherrv
cd "$SRC"

# One-time VanPanel → AetherRV rename (guarded; a no-op once migrated). MUST run before the user/dirs below.
"$SRC"/deploy/migrate_from_vanpanel.sh

id aetherrv &>/dev/null || useradd --system --home-dir /var/lib/aetherrv --shell /usr/bin/nologin aetherrv
id kiosk    &>/dev/null || useradd --system --create-home --home-dir /home/kiosk --shell /usr/bin/nologin -G video,input,render kiosk

# Python venv (uv) owned by root, read-only to the service.
UV_PYTHON_PREFERENCE=only-system uv sync --frozen --no-dev --project "$SRC" -q
chmod -R a+rX "$SRC"

install -d -m 755 /etc/aetherrv
[ -f /etc/aetherrv/aetherrv.toml ] || install -m 644 config/aetherrv.example.toml /etc/aetherrv/aetherrv.toml
if [ ! -f /etc/aetherrv/secrets.toml ]; then
  printf '# AetherRV secrets (root:aetherrv 0640). Peplink api.read-only client:\n# peplink_client_id = ""\n# peplink_client_secret = ""\n' > /etc/aetherrv/secrets.toml
fi
chown root:aetherrv /etc/aetherrv/secrets.toml && chmod 640 /etc/aetherrv/secrets.toml

install -m 644 deploy/network/05-vcan*.netdev deploy/network/05-vcan*.network /etc/systemd/network/
install -m 644 deploy/network/25-wireless.network /etc/systemd/network/
if pacman -Q iwd &>/dev/null; then                      # Wi-Fi: iwd joins, networkd addresses
  install -d -m 755 /etc/iwd && install -m 644 deploy/iwd/main.conf /etc/iwd/main.conf
  systemctl enable --now iwd.service
fi
install -m 644 deploy/pam/aetherrv-kiosk /etc/pam.d/aetherrv-kiosk
install -m 644 deploy/systemd/aetherrv.service deploy/systemd/aetherrv-kiosk.service /etc/systemd/system/
# Admin helper: root-owned copy OUTSIDE the rsync'd tree, run with the system python (-I, stdlib only).
install -d -m 755 -o root -g root /usr/local/lib/aetherrv
install -m 755 -o root -g root deploy/admin/aetherrv_admin.py /usr/local/lib/aetherrv/aetherrv_admin.py
install -m 644 deploy/systemd/aetherrv-admin.socket deploy/systemd/aetherrv-admin.service /etc/systemd/system/
install -m 755 -o root -g root deploy/admin/aetherrv_update.sh /usr/local/lib/aetherrv/aetherrv_update.sh
install -m 644 deploy/systemd/aetherrv-update.service /etc/systemd/system/
# Hold the kernel/boot packages (custom kernel; linux-rpi owns the /boot dtbs/overlays it boots with). Idempotent.
for pkg in linux-rpi raspberrypi-bootloader firmware-raspberrypi; do
  grep -qE "^IgnorePkg.*\b$pkg\b" /etc/pacman.conf || {
    [ -f /etc/pacman.conf.aetherrv-orig ] || cp -a /etc/pacman.conf /etc/pacman.conf.aetherrv-orig
    if grep -q "^IgnorePkg" /etc/pacman.conf; then sed -i "s/^IgnorePkg.*/& $pkg/" /etc/pacman.conf
    else sed -i "s/^#IgnorePkg.*/IgnorePkg = $pkg/" /etc/pacman.conf; fi
  }
done
python3 tools/make_blank_cursor.py /usr/share/aetherrv/cursors
# Boot splash theme updates (the one-time /boot setup is tools/install_boot_splash.sh).
if pacman -Q plymouth &>/dev/null; then
  install -d -m 755 /usr/share/plymouth/themes/aetherrv
  install -m 644 deploy/boot/plymouth/aetherrv/* /usr/share/plymouth/themes/aetherrv/
  # Shutdown splash: draw after the kiosk lets go of the display, and hold it a moment.
  for u in plymouth-reboot plymouth-poweroff plymouth-halt; do
    install -d -m 755 /etc/systemd/system/$u.service.d
    install -m 644 deploy/boot/systemd/aetherrv-plymouth-after-kiosk.conf /etc/systemd/system/$u.service.d/aetherrv.conf
  done
  install -m 644 deploy/boot/systemd/aetherrv-splash-hold.service /etc/systemd/system/
  systemctl daemon-reload
  systemctl enable aetherrv-splash-hold.service
fi

systemctl daemon-reload
networkctl reload
systemctl enable aetherrv.service
systemctl restart aetherrv.service
systemctl enable aetherrv-kiosk.service
systemctl enable --now aetherrv-admin.socket
systemctl try-restart aetherrv-admin.service || true
systemctl restart aetherrv-kiosk.service
echo "installed: $(systemctl is-active aetherrv) / kiosk $(systemctl is-active aetherrv-kiosk)"
