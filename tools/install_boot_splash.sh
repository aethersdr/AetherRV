#!/bin/bash
# One-time (idempotent) boot-splash setup for AetherRV, run ON THE PI as root:
#   sudo /opt/aetherrv-rs/src/tools/install_boot_splash.sh
# Installs Plymouth + the AetherRV theme, quiets the kernel/systemd console and moves it off the touchscreen
# (tty1 → tty3; the serial console is untouched), and hides the firmware rainbow. Backs up both /boot files
# once (*.pre-aetherrv). Only adds what's missing, so re-running changes nothing. (Owner-approved 2026-11-20.)
set -euo pipefail
SRC=$(cd "$(dirname "$0")/.." && pwd)
CFG=/boot/config.txt
# The firmware reads cmdline.txt (and the kernel/dtbs) from /boot/<os_prefix> when config.txt sets os_prefix
# (this Pi: os_prefix=7.3.5/ for the custom kernel) — edit THAT file, not /boot/cmdline.txt.
PREFIX=$(sed -nE 's/^os_prefix=([^#[:space:]]*).*/\1/p' $CFG | tail -1)
CMD=/boot/${PREFIX}cmdline.txt
[ -f "$CMD" ] || { echo "no $CMD (os_prefix=$PREFIX)"; exit 1; }
[ -f $CMD.pre-aetherrv ] || cp -a $CMD $CMD.pre-aetherrv
[ -f $CFG.pre-aetherrv ] || cp -a $CFG $CFG.pre-aetherrv

pacman -S --needed --noconfirm plymouth
install -d -m 755 /usr/share/plymouth/themes/aetherrv
install -m 644 "$SRC"/packaging/boot/plymouth/aetherrv/* /usr/share/plymouth/themes/aetherrv/
plymouth-set-default-theme aetherrv            # no initramfs on this Pi: Plymouth starts from the root fs

line=$(tr -d '\n' < $CMD)
line=$(sed -E 's/(^| )console=tty1( |$)/\1console=tty3\2/' <<< "$line")
for opt in quiet splash loglevel=3 vt.global_cursor_default=0 systemd.show_status=false udev.log_level=3 \
           plymouth.ignore-serial-consoles logo.nologo; do
  grep -qE "(^| )$opt( |$)" <<< "$line" || line="$line $opt"
done
printf '%s\n' "$line" > $CMD.new && mv $CMD.new $CMD
grep -q '^disable_splash=1' $CFG || printf '\n# AetherRV: no firmware rainbow (Plymouth shows the AetherRV splash)\ndisable_splash=1\n' >> $CFG
echo "cmdline: $(cat $CMD)"
echo "config:  $(grep '^disable_splash' $CFG)"
echo "theme:   $(plymouth-set-default-theme)"
