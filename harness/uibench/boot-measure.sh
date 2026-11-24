#!/bin/bash
# Power-on → usable UI (prompt §8: ≤ 15 s) on the BENCH Pi, by a reboot, headless (owner decision 2026-11-23).
# Driven from the workstation by boot-time.sh, which also times the part before the kernel; usable on its own too.
#
#   boot-measure.sh arm <wpe|tauri|chromium|python> [order]   install + enable uibench-boot.service (next boot runs it)
#   boot-measure.sh report                     after the reboot: the figures, as text and JSON (/var/tmp/uibench-boot/)
#   boot-measure.sh disarm                     disable and remove the unit
#
# uibench-boot.service stands in for the panel unit and is ordered like it (order):
#   rust      packaging/systemd/aetherrv-panel.service as committed: After=systemd-logind aetherrv-gateway, its wait
#             (≤ 5 s) for the DRM connectors, PAM aetherrv-panel (packaging/pam: no pam_nologin, pam_systemd
#             class=user-light, so no user service manager; install.sh installs it); the runtime runs without a
#             scope (no user manager for systemd-run --user); the default for wpe|wpe-launcher|tauri|chromium
#   rust-kiosk  the same ordering with the kiosk's PAM stack (aetherrv-kiosk: a user service manager), as measured
#             before packaging/pam/aetherrv-panel existed
#   rust-light  as rust, with the test file /etc/pam.d/uibench-light (light-session.sh install) instead of the
#             installed aetherrv-panel stack (the same content)
#   rust-old  the earlier panel unit: After=systemd-user-sessions aetherrv-gateway, PAM login
#   python    deploy/systemd/aetherrv-kiosk.service (the Python kiosk): After=systemd-user-sessions aetherrv.service,
#             PAM aetherrv-kiosk, its 1 s wait loop for :8080; the default for `python` (Chromium with the van's flags
#             on the Python UI served by the Python backend: no bench module, so the cold start only)
# Like the panel unit it waits for :8080, then runs sway on a headless 1920×1200 output with the benchmark's config
# (sway.conf.in). Its one exec line starts aetherrv-uibench on the runtime, which records the runtime's launch and its
# first usable frame (the settled Home page) on CLOCK_MONOTONIC, i.e. as time since the kernel started. No
# touchscreen and no taps. Linux cannot see the firmware: boot-time.sh measures it from the workstation.
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
  RT=${2:?runtime: wpe|wpe-launcher|tauri|chromium|python}
  case $RT in wpe|wpe-launcher|tauri|chromium) ORDER=${3:-rust} ;; python) ORDER=${3:-python} ;; *) echo "runtime: wpe|wpe-launcher|tauri|chromium|python" >&2; exit 2 ;; esac
  DRMWAIT=""
  case $ORDER in
    rust|rust-kiosk|rust-light) AFTER="systemd-logind.service aetherrv-gateway.service"; WANTS=aetherrv-gateway.service; STEP=0.25; N=240
          case $ORDER in rust) PAM=aetherrv-panel ;; rust-kiosk) PAM=aetherrv-kiosk ;; *) PAM=uibench-light ;; esac
          # as the panel unit: wait (up to 5 s) for the display driver's connectors before sway starts
          DRMWAIT="ExecStartPre=/bin/bash -c 'for i in \$(seq 50); do ls /sys/class/drm | grep -qE \"^card[0-9]+-\" && exit 0; sleep 0.1; done; exit 0'" ;;
    rust-old) AFTER="systemd-user-sessions.service aetherrv-gateway.service"; WANTS=aetherrv-gateway.service; PAM=login; STEP=0.25; N=240 ;;
    python) AFTER="systemd-user-sessions.service aetherrv.service"; WANTS=aetherrv.service; PAM=aetherrv-kiosk; STEP=1; N=60 ;;
    *) echo "order: rust|rust-kiosk|rust-light|rust-old|python" >&2; exit 2 ;;
  esac
  # a login stack with pam_nologin would be refused before systemd-user-sessions removes /run/nologin
  [ -f "/etc/pam.d/$PAM" ] || { echo "refused: /etc/pam.d/$PAM is missing (packaging/pam/aetherrv-panel, installed by install.sh; or deploy/pam/aetherrv-kiosk)" >&2; exit 4; }
  sudo install -d -m 755 -o "$ME" "$DIR" "$OUT"
  sed -e "s|@OUTPUT@|HEADLESS-1|g" -e "s|@MODE@|mode 1920x1200@60Hz|" -e '/map_to_output/d' "$H/sway.conf.in" > "$DIR/sway.conf"
  echo "exec $H/boot-measure.sh _measure $RT" >> "$DIR/sway.conf"
  echo "$RT $ORDER" > "$DIR/armed"
  sudo tee /etc/systemd/system/$U.service >/dev/null <<EOF
# UI runtime benchmark: power-on → usable UI, headless (harness/uibench/boot-measure.sh). Remove: boot-measure.sh disarm
[Unit]
Description=uibench boot measurement ($RT), ordered like the panel ($ORDER)
After=$AFTER
Wants=$WANTS

[Service]
Type=simple
User=$ME
PAMName=$PAM
StandardOutput=journal
StandardError=journal
Environment=XDG_SESSION_TYPE=wayland WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1
ExecStartPre=/bin/bash -c 'echo "uibench: wait loop started"; for i in \$(seq $N); do (exec 3<>/dev/tcp/127.0.0.1/8080) 2>/dev/null && { echo "uibench: :8080 answers (try \$i)"; exit 0; }; sleep $STEP; done; exit 0'
$DRMWAIT
ExecStart=/usr/bin/sway --config $DIR/sway.conf
Restart=no

[Install]
WantedBy=multi-user.target
EOF
  sudo systemctl daemon-reload
  sudo systemctl enable $U.service
  echo "armed for $RT, ordered as $ORDER: the next boot measures it (then: $0 report; afterwards: $0 disarm)"
  ;;
_measure)   # inside the boot session (sway's exec)
  RT=$2
  T_EXEC=$(cut -d" " -f1 /proc/uptime)     # sway ran its exec line (its outputs and IPC are up by then)
  case $RT in
    wpe) CMD=(env WPE_PLATFORM=wayland /usr/lib/wpe-webkit-2.0/MiniBrowser --fullscreen "$URL") ;;
    wpe-launcher) CMD=(env AETHERRV_PANEL_PROFILE="$HOME/uibench/profiles/wpe-launcher" "${UIBENCH_WPE_BIN:-$HOME/uibench/wpe/aetherrv-panel-wpe}" "$URL") ;;
    tauri) CMD=("$TAURI_BIN" "$URL") ;;
    chromium|python)
              prof=$HOME/uibench/profiles/chromium-$RT; mkdir -p "$prof"; rm -f "$prof"/Singleton*
              # the Python kiosk opens the Python backend's own UI, which has no bench module
              [ "$RT" = python ] && URL="http://127.0.0.1:8080/"
              # the van's flags (deploy/kiosk/chromium.sh, packaging/panel/launch.sh), as run.sh
              # shellcheck disable=SC2054
              CMD=(chromium --kiosk --ozone-platform=wayland --noerrdialogs --autoplay-policy=no-user-gesture-required
                   --disable-infobars --no-first-run --disable-session-crashed-bubble
                   --disable-features=Translate,TouchpadOverscrollHistoryNavigation --overscroll-history-navigation=0
                   --check-for-update-interval=31536000 --password-store=basic --user-data-dir="$prof" "$URL") ;;
  esac
  export OUTPUT=HEADLESS-1
  BOOT=$(cut -c1-8 /proc/sys/kernel/random/boot_id)
  echo "$T_EXEC" > "$OUT/$RT-$BOOT.exec"
  # the cold start only: launch → first paint → the settled Home page (and the page's data mark where it has the
  # bench module). The Python UI never reports, so its settled reference is taken 8 s + 4 s after the launch.
  # --launch-first: the runner starts the runtime before its own capture set-up, as launch.sh would start it.
  SCOPE=(); grep -qE " rust(-light)?$" "$DIR/armed" 2>/dev/null && SCOPE=(--no-scope)   # user-light: no systemd-run --user
  "$BIN" run --runtime "$RT" --mode headless --label "boot measurement ($(cat "$DIR/armed" 2>/dev/null))" --url "$URL" \
    --out "$OUT/$RT-$BOOT.json" --no-touch --cold-only --launch-first --report-wait-ms 8000 "${SCOPE[@]}" --pages home -- "${CMD[@]}" > "$OUT/$RT-$BOOT.log" 2>&1 || true
  ;;
report)
  BOOT=$(cut -c1-8 /proc/sys/kernel/random/boot_id)
  f=$(ls -t "$OUT"/*-"$BOOT".json 2>/dev/null | head -1 || true)
  [ -n "$f" ] || { echo "no measurement for this boot in $OUT yet (journalctl -b -u $U)"; exit 1; }
  python3 - "$f" "$(cat "$DIR/armed" 2>/dev/null)" <<'EOF'
import json, subprocess, sys, time
d = json.load(open(sys.argv[1]))
armed = sys.argv[2].split()
sh = lambda c: subprocess.run(c, shell=True, capture_output=True, text=True).stdout.strip()
def mono_s(unit, prop):
    v = sh(f"systemctl show -p {prop} --value {unit}")
    return int(v) / 1e6 if v.isdigit() and int(v) > 0 else None
def first_mono(unit, pat):
    # unit None: the whole boot's journal (the wait loop runs in the PAM session's scope, not in the unit)
    v = sh(f"journalctl -b -o short-monotonic {'-u ' + unit if unit else ''} --no-pager | grep -m1 '{pat}' | tr -d '[]' | awk '{{print $1}}'")
    return float(v) if v else None
c = d["cold_start"]
launch = c.get("t_launch_since_kernel_ms")
usable = (launch + c["settled_ms"]) / 1000 if launch is not None and c.get("settled_ms") is not None else None
data = (launch + c["data_frame_ms"]) / 1000 if launch is not None and c.get("data_frame_ms") is not None else None
uptime = float(open("/proc/uptime").read().split()[0])
python = sh("systemctl is-active aetherrv.service") == "active"
backend = "aetherrv.service" if python else "aetherrv-gateway.service"
out = {
    "runtime": d["runtime"], "order": armed[1] if len(armed) > 1 else None, "system": "python" if python else "rust",
    "systemd_analyze": sh("systemd-analyze | head -1"),
    "kernel_start_epoch": time.time() - uptime,                      # the wall clock when the kernel started
    "logind_s": mono_s("systemd-logind.service", "ActiveEnterTimestampMonotonic"),
    "network_target_s": mono_s("network.target", "ActiveEnterTimestampMonotonic"),
    "user_sessions_s": mono_s("systemd-user-sessions.service", "ActiveEnterTimestampMonotonic"),
    "default_target_s": mono_s("multi-user.target", "ActiveEnterTimestampMonotonic"),
    "bus_started_s": None if python else mono_s("aetherrv-bus.service", "ActiveEnterTimestampMonotonic"),
    "backend_started_s": mono_s(backend, "ExecMainStartTimestampMonotonic"),
    "gateway_serving_s": first_mono(backend, "web UI on"),            # both systems log it when they listen
    "wait_loop_started_s": first_mono(None, "uibench: wait loop started"),
    "port_8080_answered_s": first_mono(None, "uibench: :8080 answers"),
    "loopback_up_s": first_mono("systemd-networkd.service", " lo: Link UP"),
    "panel_unit_started_s": mono_s("uibench-boot.service", "ExecMainStartTimestampMonotonic"),
    "sway_exec_s": (lambda f: float(open(f).read()) if __import__("os").path.exists(f) else None)(sys.argv[1].replace(".json", ".exec")),
    "runner_setup_ms": d.get("runner_setup_ms"),
    "runtime_launched_s": launch / 1000 if launch is not None else None,
    "first_data_frame_s": data, "first_usable_frame_s": usable,
    "cold_start_ms": {k: c.get(k) for k in ("first_paint_ms", "data_frame_ms", "content_ms", "settled_ms", "white_flash")},
}
json.dump(out, open(sys.argv[1].replace(".json", "-boot.json"), "w"), indent=1)
print(f"runtime {out['runtime']} on the {out['system']} system, ordered as {out['order']} ({sys.argv[1]}): {out['systemd_analyze']}")
for k in ("logind_s", "bus_started_s", "backend_started_s", "gateway_serving_s", "loopback_up_s", "network_target_s", "user_sessions_s",
          "wait_loop_started_s", "port_8080_answered_s", "panel_unit_started_s", "sway_exec_s", "runtime_launched_s", "first_data_frame_s", "first_usable_frame_s",
          "default_target_s"):
    v = out[k]
    print(f"  {k:24s} {'–' if v is None else f'{v:.2f} s after the kernel started'}")
print(f"  runner set-up (ms from the runner's start): {out['runner_setup_ms']}")
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
