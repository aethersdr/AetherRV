#!/bin/bash
# aetherrv-panel.service ExecStartPre: wait until an HDMI display reads "connected" and stays so for 2 s (at most 20 s),
# then let the unit end the splash and start sway. In the van (2026-11-24) sway started 4 s after the kernel found no
# display on HDMI and the panel went dark and stayed so; the Python kiosk, started at about 9 s, never did. Always
# exits 0: a panel without a display still starts (and the headless bench has no HDMI connector).
[ "${WLR_BACKENDS:-}" = headless ] && exit 0   # the bench's headless output (harness/bench drop-ins)
n=0
for _ in $(seq 200); do
  if grep -qx connected /sys/class/drm/card*-HDMI-A-*/status 2>/dev/null; then n=$((n + 1)); else n=0; fi
  if [ "$n" -ge 20 ]; then
    echo "panel: HDMI display connected and steady at $(cut -d' ' -f1 /proc/uptime) s"
    exit 0
  fi
  sleep 0.1
done
echo "panel: no steady HDMI display after 20 s ($(cat /sys/class/drm/card*-HDMI-A-*/status 2>/dev/null | tr '\n' ' ')), starting anyway"
exit 0
