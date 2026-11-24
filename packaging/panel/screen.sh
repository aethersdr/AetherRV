#!/bin/sh
# The panel's screen power helper (aetherrv-screen, 127.0.0.1:8091), run by sway (packaging/panel/sway.conf) and
# started again a second after it dies, for as long as this session's sway answers. The helper itself exits when
# sway goes (services/screen: its socket gone or refusing), and then so does this loop, so a panel restart leaves
# nothing behind.
while swaymsg -t get_version >/dev/null 2>&1; do
  /opt/aetherrv-rs/bin/aetherrv-screen
  sleep 1
done
