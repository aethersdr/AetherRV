#!/bin/bash
# Power-on → usable UI (prompt §8: ≤ 15 s), measured by a reboot of the BENCH Pi. The benchmark never runs this by
# itself: arming it replaces the login prompt on tty1 with the measured panel at the next boot.
#
#   boot-measure.sh arm <wpe|tauri|chromium> [WxH@Hz]   install + enable uibench-boot.service; then `sudo reboot`
#   boot-measure.sh report                              after the reboot: the figures (also /var/tmp/uibench-boot/)
#   boot-measure.sh disarm                              disable and remove the unit (getty on tty1 comes back)
#
# uibench-boot.service is packaging/systemd/aetherrv-panel.service's twin: the same ordering after the gateway, the
# same wait for :8080, the same Plymouth hand-off (`plymouth quit --retain-splash`), tty1, sway. Its sway config is
# the benchmark's (sway.conf.in) with one exec line that starts aetherrv-uibench on the runtime, which records the
# runtime's launch and its first usable frame on CLOCK_MONOTONIC, i.e. as time since the kernel started.
#
# Firmware time is invisible to Linux on the Pi 5 (systemd-analyze shows no firmware/loader figures). It is
# estimated as (this boot's kernel start, wall clock) − (the previous boot's last journal entry): a warm reboot,
# including the last moments of the shutdown, so slightly high. For a cold power-on, time plug-in → usable UI once
# with a stopwatch and compare. Needs the monitor connected (sway needs an output) and the aetherrv units enabled.
set -euo pipefail
if ip -br link | grep -qE '^(hat0|hat1)\b'; then echo "refused: this host has hat0/hat1, it is the van" >&2; exit 3; fi
H=$(cd "$(dirname "$0")" && pwd)
U=uibench-boot
DIR=/var/lib/uibench-boot
OUT=/var/tmp/uibench-boot
ME=$(id -un)
BIN=${UIBENCH_BIN:-$HOME/uibench/crate/target/release/aetherrv-uibench}
TAURI_BIN=${UIBENCH_TAURI_BIN:-$HOME/uibench/tauri/target/release/aetherrv-panel-tauri}

case "${1:-}" in
arm)
  RT=${2:?runtime: wpe|tauri|chromium}; MODE=${3:-1920x1200@60Hz}
  case $RT in
    wpe) CMD="env WPE_PLATFORM=wayland /usr/lib/wpe-webkit-2.0/MiniBrowser --fullscreen http://127.0.0.1:8080/?bench=1" ;;
    tauri) CMD="$TAURI_BIN http://127.0.0.1:8080/?bench=1" ;;
    chromium) CMD="chromium --kiosk --ozone-platform=wayland --noerrdialogs --no-first-run --disable-session-crashed-bubble --password-store=basic --user-data-dir=$HOME/uibench/profiles/chromium-chromium http://127.0.0.1:8080/?bench=1" ;;
    *) echo "runtime: wpe|tauri|chromium" >&2; exit 2 ;;
  esac
  sudo install -d -m 755 -o "$ME" "$DIR" "$OUT"
  sed -e "s|@OUTPUT@|*|g" -e "s|@MODE@|mode --custom $MODE|" -e '/map_to_output/d' "$H/sway.conf.in" > "$DIR/sway.conf"
  echo "exec $H/boot-measure.sh _measure $RT $CMD" >> "$DIR/sway.conf"
  sudo tee /etc/systemd/system/$U.service >/dev/null <<EOF
# UI runtime benchmark: power-on → usable UI (harness/uibench/boot-measure.sh). Remove with: boot-measure.sh disarm
[Unit]
Description=uibench boot measurement ($RT) on tty1
After=systemd-user-sessions.service aetherrv-gateway.service
Wants=aetherrv-gateway.service
Conflicts=getty@tty1.service
After=getty@tty1.service
After=plymouth-start.service plymouth-quit.service
Conflicts=plymouth-quit.service

[Service]
Type=simple
User=$ME
PAMName=login
TTYPath=/dev/tty1
TTYReset=yes
TTYVHangup=yes
TTYVTDisallocate=yes
StandardInput=tty-fail
StandardOutput=journal
StandardError=journal
UtmpIdentifier=tty1
UtmpMode=user
Environment=XDG_SESSION_TYPE=wayland
ExecStartPre=/bin/bash -c 'for i in \$(seq 60); do (exec 3<>/dev/tcp/127.0.0.1/8080) 2>/dev/null && exit 0; sleep 0.25; done; exit 0'
ExecStartPre=-+/usr/bin/plymouth quit --retain-splash
ExecStart=/usr/bin/sway --config $DIR/sway.conf
Restart=no

[Install]
WantedBy=multi-user.target
EOF
  sudo systemctl daemon-reload
  sudo systemctl enable $U.service
  date +%s.%N > "$DIR/armed"
  echo "armed for $RT at $MODE. Reboot with: sudo reboot   (then: $0 report; afterwards: $0 disarm)"
  ;;
_measure)   # inside the boot session (sway's exec): measure the cold start (the runtime stops when it is done)
  RT=$2; shift 2
  OUTPUT=$(swaymsg -t get_outputs -r | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["name"])')
  export OUTPUT
  BOOT=$(cat /proc/sys/kernel/random/boot_id | cut -c1-8)
  "$BIN" run --runtime "$RT" --mode screen --label "boot measurement" --url "http://127.0.0.1:8080/?bench=1" \
    --out "$OUT/$RT-$BOOT.json" --pages home --warm-rounds 0 --press-reps 0 --null-reps 0 --fps-secs 1 -- "$@" \
    > "$OUT/$RT-$BOOT.log" 2>&1 || true
  ;;
report)
  f=$(ls -t "$OUT"/*.json 2>/dev/null | head -1)
  [ -n "$f" ] || { echo "no measurement in $OUT yet (journalctl -b -u $U)"; exit 1; }
  python3 - "$f" <<'EOF'
import json, subprocess, sys, time
d = json.load(open(sys.argv[1]))
sh = lambda c: subprocess.run(c, shell=True, capture_output=True, text=True).stdout.strip()
def mono_s(unit, prop):
    v = sh(f"systemctl show -p {prop} --value {unit}")
    return int(v) / 1e6 if v.isdigit() and int(v) > 0 else None
kstart = time.time() - time.clock_gettime(time.CLOCK_BOOTTIME)
prev = sh("journalctl -b -1 -n 1 -o short-unix --no-pager | awk 'NR==1{print $1}'")
fw = kstart - float(prev) if prev else None
c = d["cold_start"]
launch = c.get("t_launch_since_kernel_ms")
usable = (launch + c["settled_ms"]) / 1000 if launch is not None and c.get("settled_ms") is not None else None
gw = sh("journalctl -b -o short-monotonic -u aetherrv-gateway --no-pager | grep -m1 'web UI on' | tr -d '[]' | awk '{print $1}'")
rows = [
    ("firmware + end of previous shutdown (estimate)", fw),
    ("kernel → userspace (systemd-analyze)", sh("systemd-analyze | head -1")),
    ("gateway serving (since kernel start)", float(gw) if gw else None),
    ("panel unit started", mono_s("uibench-boot.service", "ExecMainStartTimestampMonotonic")),
    ("runtime launched", launch / 1000 if launch is not None else None),
    ("first usable frame (since kernel start)", usable),
    ("POWER-ON → USABLE (firmware estimate + the above)", (fw or 0) + usable if usable is not None else None),
]
print(f"runtime {d['runtime']} ({sys.argv[1]})")
for k, v in rows:
    print(f"  {k:52s} {v if isinstance(v, str) else ('–' if v is None else f'{v:.2f} s')}")
print("  cold start detail (ms from launch):", {k: c.get(k) for k in ("first_frame_ms", "first_paint_ms", "data_frame_ms", "content_ms", "settled_ms", "white_flash")})
EOF
  ;;
disarm)
  sudo systemctl disable --now $U.service 2>/dev/null || true
  sudo rm -f /etc/systemd/system/$U.service
  sudo systemctl daemon-reload
  echo "disarmed (getty@tty1 returns at the next boot, or: sudo systemctl start getty@tty1)"
  ;;
*) sed -n '2,8p' "$0"; exit 2 ;;
esac
