#!/bin/bash
# The OS pieces a Rust-only AetherRV needs (a fresh build, docs/BUILD-GUIDE.md §5): what the Python installer
# (deploy/install.sh) used to set up, without Python. Run ON THE PI as root: packaging/install.sh --system runs it.
# Idempotent. Never touches /boot (the boot splash's one-time /boot setup is tools/install_boot_splash.sh).
set -euo pipefail
SRC=${SRC:-/opt/aetherrv-rs/src}
cd "$SRC"
id aetherrv &>/dev/null || useradd --system --home-dir /var/lib/aetherrv --shell /usr/bin/nologin aetherrv
id kiosk    &>/dev/null || useradd --system --create-home --home-dir /home/kiosk --shell /usr/bin/nologin -G video,input,render kiosk
# CAN: the HATs' stable names and listen-only ports are set up by hand in BUILD-GUIDE §4; these are the vcans
install -m 644 deploy/network/05-vcan*.netdev deploy/network/05-vcan*.network /etc/systemd/network/
install -m 644 deploy/network/25-wireless.network /etc/systemd/network/
if pacman -Q iwd &>/dev/null; then
  install -d -m 755 /etc/iwd && install -m 644 deploy/iwd/main.conf /etc/iwd/main.conf
  systemctl enable --now iwd.service
fi
install -m 644 deploy/pam/aetherrv-kiosk /etc/pam.d/aetherrv-kiosk
install -m 644 packaging/pam/aetherrv-panel /etc/pam.d/aetherrv-panel     # the Rust panel's own stack (user-light)
# hold the kernel/boot packages (custom kernel; linux-rpi owns the /boot dtbs/overlays it boots with)
for pkg in linux-rpi raspberrypi-bootloader firmware-raspberrypi; do
  grep -qE "^IgnorePkg.*\b$pkg\b" /etc/pacman.conf || {
    [ -f /etc/pacman.conf.aetherrv-orig ] || cp -a /etc/pacman.conf /etc/pacman.conf.aetherrv-orig
    if grep -q "^IgnorePkg" /etc/pacman.conf; then sed -i "s/^IgnorePkg.*/& $pkg/" /etc/pacman.conf
    else sed -i "s/^#IgnorePkg.*/IgnorePkg = $pkg/" /etc/pacman.conf; fi
  }
done
# an invisible Xcursor theme for the touch-only panel (tools/make_blank_cursor.py, in bash): a 1x1 transparent image
le32() { printf "\\\\x%02x\\\\x%02x\\\\x%02x\\\\x%02x" $(($1 & 255)) $((($1 >> 8) & 255)) $((($1 >> 16) & 255)) $((($1 >> 24) & 255)); }
blank_cursor() {
  printf 'Xcur'
  for v in 16 65536 1  4294770690 24 28  36 4294770690 24 1 1 1 0 0 0  0; do printf "$(le32 $v)"; done
}
CUR=/usr/share/aetherrv/cursors/default
install -d -m 755 "$CUR/cursors"
for n in default left_ptr arrow top_left_arrow pointer hand1 hand2 text xterm wait watch progress crosshair grab grabbing move not-allowed help; do
  blank_cursor > "$CUR/cursors/$n"
done
printf '[Icon Theme]\nName=AetherRV blank\nComment=Invisible pointer for the touch kiosk\n' > "$CUR/index.theme"
# boot splash theme (when Plymouth is installed)
if pacman -Q plymouth &>/dev/null; then
  install -d -m 755 /usr/share/plymouth/themes/aetherrv
  install -m 644 deploy/boot/plymouth/aetherrv/* /usr/share/plymouth/themes/aetherrv/
  for u in plymouth-reboot plymouth-poweroff plymouth-halt; do
    install -d -m 755 /etc/systemd/system/$u.service.d
    install -m 644 deploy/boot/systemd/aetherrv-plymouth-after-kiosk.conf /etc/systemd/system/$u.service.d/aetherrv.conf
    install -m 644 packaging/boot/aetherrv-plymouth-after-panel.conf /etc/systemd/system/$u.service.d/aetherrv-panel.conf
  done
  install -m 644 deploy/boot/systemd/aetherrv-splash-hold.service /etc/systemd/system/
  systemctl daemon-reload
  systemctl enable aetherrv-splash-hold.service
fi
systemctl daemon-reload
networkctl reload 2>/dev/null || true
echo "system pieces installed (users aetherrv + kiosk, network files, PAM, cursor, package holds)"
