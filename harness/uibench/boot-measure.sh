#!/bin/bash
# Power-on → usable UI (prompt §8: ≤ 15 s) on the BENCH Pi, by a reboot, headless (owner decision 2026-11-23).
# Driven from the workstation by boot-time.sh, which also times the part before the kernel; usable on its own too.
#
#   boot-measure.sh arm <wpe|tauri|chromium>   install + enable uibench-boot.service (the next boot runs it)
#   boot-measure.sh report                     after the reboot: the figures, as text and JSON (/var/tmp/uibench-boot/)
#   boot-measure.sh disarm                     disable and remove the unit
#
# uibench-boot.service is ordered like packaging/systemd/aetherrv-panel.service (after the gateway, waiting for :8080)
# and runs sway on a headless 1920×1200 output with the benchmark's config (sway.conf.in). Its one exec line starts
# aetherrv-uibench on the runtime, which records the runtime's launch and its first usable frame (the settled Home
# page with the first snapshot's data) on CLOCK_MONOTONIC, i.e. as time since the kernel started. No touchscreen and
# no taps. Linux cannot see the firmware: boot-time.sh measures it from the workstation.
set -euo pipefail
if ip -br link | grep -qE '^(hat0|hat1)\b'; then echo "refused: this host has hat0/hat1, it is the van" >&2; exit 3; fi
H=$(cd "$(dirname "$0")" && pwd)
U=uibench-boot
DIR=/var/lib/uibench-boot
OUT=/var/tmp/uibench-boot
ME=$(id -un)
BIN=${UIBENCH_BIN:-$HOME/uibench/crate/target/release/aetherrv-uibench}
TAURI_BIN=${UIBENCH_TAURI_BIN:-$HOME/uibench/tauri/target/release/aetherrv-panel-tauri}
URL="http://127.0.0.1:8080/?bench=1"

case "${1:-}" in
arm)
  RT=${2:?runtime: wpe|tauri|chromium}
  case $RT in wpe|tauri|chromium) ;; *) echo "runtime: wpe|tauri|chromium" >&2; exit 2 ;; esac
  sudo install -d -m 755 -o "$ME" "$DIR" "$OUT"
  sed -e "s|@OUTPUT@|HEADLESS-1|g" -e "s|@MODE@|mode 1920x1200@60Hz|" -e '/map_to_output/d' "$H/sway.conf.in" > "$DIR/sway.conf"
  echo "exec $H/boot-measure.sh _measure $RT" >> "$DIR/sway.conf"
  sudo tee /etc/systemd/system/$U.service >/dev/null <<EOF
# UI runtime benchmark: power-on → usable UI, headless (harness/uibench/boot-measure.sh). Remove: boot-measure.sh disarm
[Unit]
Description=uibench boot measurement ($RT), ordered like aetherrv-panel.service
After=systemd-user-sessions.service aetherrv-gateway.service
Wants=aetherrv-gateway.service

[Service]
Type=simple
User=$ME
PAMName=login
StandardOutput=journal
StandardError=journal
Environment=XDG_SESSION_TYPE=wayland WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1
ExecStartPre=/bin/bash -c 'for i in \$(seq 240); do (exec 3<>/dev/tcp/127.0.0.1/8080) 2>/dev/null && exit 0; sleep 0.25; done; exit 0'
ExecStart=/usr/bin/sway --config $DIR/sway.conf
Restart=no

[Install]
WantedBy=multi-user.target
EOF
  sudo systemctl daemon-reload
  sudo systemctl enable $U.service
  echo "armed for $RT: the next boot measures it (then: $0 report; afterwards: $0 disarm)"
  ;;
_measure)   # inside the boot session (sway's exec)
  RT=$2
  case $RT in
    wpe) CMD=(env WPE_PLATFORM=wayland /usr/lib/wpe-webkit-2.0/MiniBrowser --fullscreen "$URL") ;;
    tauri) CMD=("$TAURI_BIN" "$URL") ;;
    chromium) prof=$HOME/uibench/profiles/chromium-chromium; mkdir -p "$prof"; rm -f "$prof"/Singleton*
              # the van's flags (deploy/kiosk/chromium.sh), as run.sh
              # shellcheck disable=SC2054
              CMD=(chromium --kiosk --ozone-platform=wayland --noerrdialogs --autoplay-policy=no-user-gesture-required
                   --disable-infobars --no-first-run --disable-session-crashed-bubble
                   --disable-features=Translate,TouchpadOverscrollHistoryNavigation --overscroll-history-navigation=0
                   --check-for-update-interval=31536000 --password-store=basic --user-data-dir="$prof" "$URL") ;;
  esac
  export OUTPUT=HEADLESS-1
  BOOT=$(cut -c1-8 /proc/sys/kernel/random/boot_id)
  "$BIN" run --runtime "$RT" --mode headless --label "boot measurement" --url "$URL" --out "$OUT/$RT-$BOOT.json" \
    --no-touch --pages home --warm-rounds 0 --press-reps 0 --null-reps 0 --fps-secs 1 -- "${CMD[@]}" > "$OUT/$RT-$BOOT.log" 2>&1 || true
  ;;
report)
  BOOT=$(cut -c1-8 /proc/sys/kernel/random/boot_id)
  f=$(ls -t "$OUT"/*-"$BOOT".json 2>/dev/null | head -1 || true)
  [ -n "$f" ] || { echo "no measurement for this boot in $OUT yet (journalctl -b -u $U)"; exit 1; }
  python3 - "$f" <<'EOF'
import json, subprocess, sys, time
d = json.load(open(sys.argv[1]))
sh = lambda c: subprocess.run(c, shell=True, capture_output=True, text=True).stdout.strip()
def mono_s(unit, prop):
    v = sh(f"systemctl show -p {prop} --value {unit}")
    return int(v) / 1e6 if v.isdigit() and int(v) > 0 else None
def first_mono(unit, pat):
    v = sh(f"journalctl -b -o short-monotonic -u {unit} --no-pager | grep -m1 '{pat}' | tr -d '[]' | awk '{{print $1}}'")
    return float(v) if v else None
c = d["cold_start"]
launch = c.get("t_launch_since_kernel_ms")
usable = (launch + c["settled_ms"]) / 1000 if launch is not None and c.get("settled_ms") is not None else None
data = (launch + c["data_frame_ms"]) / 1000 if launch is not None and c.get("data_frame_ms") is not None else None
uptime = float(open("/proc/uptime").read().split()[0])
out = {
    "runtime": d["runtime"], "systemd_analyze": sh("systemd-analyze | head -1"),
    "kernel_start_epoch": time.time() - uptime,                      # the wall clock when the kernel started
    "bus_started_s": mono_s("aetherrv-bus.service", "ActiveEnterTimestampMonotonic"),
    "gateway_serving_s": first_mono("aetherrv-gateway.service", "web UI on"),
    "panel_unit_started_s": mono_s("uibench-boot.service", "ExecMainStartTimestampMonotonic"),
    "runtime_launched_s": launch / 1000 if launch is not None else None,
    "first_data_frame_s": data, "first_usable_frame_s": usable,
    "cold_start_ms": {k: c.get(k) for k in ("first_paint_ms", "data_frame_ms", "content_ms", "settled_ms", "white_flash")},
}
json.dump(out, open(sys.argv[1].replace(".json", "-boot.json"), "w"), indent=1)
print(f"runtime {out['runtime']} ({sys.argv[1]}): {out['systemd_analyze']}")
for k in ("bus_started_s", "gateway_serving_s", "panel_unit_started_s", "runtime_launched_s", "first_data_frame_s", "first_usable_frame_s"):
    v = out[k]
    print(f"  {k:24s} {'–' if v is None else f'{v:.2f} s after the kernel started'}")
print(json.dumps({"kernel_start_epoch": out["kernel_start_epoch"], "first_usable_frame_s": usable}))
EOF
  ;;
disarm)
  sudo systemctl disable --now $U.service 2>/dev/null || true
  sudo rm -f /etc/systemd/system/$U.service
  sudo systemctl daemon-reload
  echo "disarmed"
  ;;
*) sed -n '2,8p' "$0"; exit 2 ;;
esac
