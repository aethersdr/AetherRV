#!/bin/bash
# Simulated EPOCH batteries for the UI benchmark. The Energy page shows its 3D battery bank (owner Q-U6) only with
# per-battery data, and the bench has no Bluetooth batteries, so without this the bank is never on the page.
# On the BENCH Pi, after harness/bench/bench-up.sh (which installs the bench config and leaves batteries-ble idle):
#   batteries.sh on    add four made-up batteries to /etc/aetherrv/aetherrv.toml (a marked block, locally administered
#                      addresses) and run aetherrv-sim-batteries (sims/batteries-sim: the real batteries-ble core with a
#                      stand-in for BlueZ; SoC 86/85/87/84 %, -6.2 A each, the Cerbo sim's -24.8 A bank current) in
#                      place of aetherrv-batteries-ble (the same bus identity)
#   batteries.sh off   stop the simulator, remove the block, start aetherrv-batteries-ble again
# The isolation tests and the soak run without it (bench-up's config).
set -euo pipefail
if ip -br link | grep -qE '^(hat0|hat1)\b'; then echo "refused: this host has hat0/hat1, it is the van" >&2; exit 3; fi
CFG=/etc/aetherrv/aetherrv.toml
SIM=/opt/aetherrv-rs/bin/aetherrv-sim-batteries
B="# >>> uibench batteries.sh"; E="# <<< uibench batteries.sh"
strip() { sudo sed -i "/^$B/,/^$E/d" "$CFG"; }
case "${1:-}" in
on)
  [ -x "$SIM" ] || { echo "refused: $SIM is not installed (a build with sims/batteries-sim)" >&2; exit 4; }
  sudo systemctl stop aetherrv-sim-batteries 2>/dev/null || true
  strip
  { echo "$B (bench only: made-up batteries for the Energy page's 3D bank)"
    echo '[device.epoch_bench]'; echo 'kind = "epoch_ble"'; echo 'enabled = true'; echo 'interval = 1.0'
    for i in 1 2 3 4; do echo; echo '[[device.epoch_bench.battery]]'; echo "name = \"Bench $i\""; echo "address = \"02:00:00:00:00:0$i\""; done
    echo "$E"; } | sudo tee -a "$CFG" >/dev/null
  sudo systemctl stop aetherrv-batteries-ble
  sudo systemctl reset-failed aetherrv-sim-batteries 2>/dev/null || true
  sudo systemd-run --quiet --unit=aetherrv-sim-batteries -p User=aetherrv "$SIM" -c "$CFG"
  for _ in $(seq 40); do
    curl -s -m 2 http://127.0.0.1:8080/api/state | grep -q '"batteries.ble.reporting":4' && { echo "batteries: 4 reporting"; exit 0; }
    sleep 0.5
  done
  echo "batteries: not reporting after 20 s" >&2; exit 1 ;;
off)
  sudo systemctl stop aetherrv-sim-batteries 2>/dev/null || true
  strip
  sudo systemctl start aetherrv-batteries-ble
  echo "batteries: simulator off, aetherrv-batteries-ble back" ;;
*) sed -n '2,11p' "$0"; exit 2 ;;
esac
