#!/bin/bash
# Does the panel work in a logind session with no user service manager (pam_systemd class=user-light)? On the BENCH
# Pi, as the kiosk user (who has no other session, so no user manager and no session bus exist, as on the van):
# a transient unit like the panel unit (PAM stack /etc/pam.d/uibench-light = deploy/pam/aetherrv-kiosk with
# class=user-light, TTYPath=/dev/tty8, Restart=always) runs sway on a headless output with libinput, the screen
# helper and a launch script (the runtime, then `swaymsg exit`), as packaging/panel does. Checks:
#   the session's class, seat and tty; XDG_RUNTIME_DIR owned by kiosk; no user@ manager and no session bus;
#   sway took the seat and its input devices (and, in a second short start, opened the DRM device);
#   the runtime is up (WebKit: bubblewrap and the web process) and the page loads and goes live;
#   a tap reaches the page (uinput); screen off/on through the page (the helper's path);
#   the alert chime reaches ALSA: snd-aloop's Loopback card is the session's default card (ALSA_CARD, no config file),
#     the capture side is recorded while the page chimes (at load, for the active alerts, and on the bench module's
#     "chime" command), and the page reports its AudioContext state; kiosk's ACL on the card;
#   the session comes back after `systemctl restart` and after a kill -9 of the runtime.
# The functional page comes from the gateway (:8080, a bundle with the bench module's "report" and "screen off|on":
# the screen helper only answers that origin); the audio page from AUDIO_URL (a bundle with "chime" and the audio
# state, e.g. `aetherrv-uibench serve` on :18082).
#   light-session.sh install | remove                    the PAM test file
#   light-session.sh check [wpe-launcher|chromium|tauri] [AUDIO_URL]
set -uo pipefail
if ip -br link | grep -qE '^(hat0|hat1)\b'; then echo "refused: this host has hat0/hat1, it is the van" >&2; exit 3; fi
H=$(cd "$(dirname "$0")" && pwd)
PAMF=/etc/pam.d/uibench-light
U=uibench-light
W=/var/tmp/uibench-light
BIN=${UIBENCH_BIN:-$HOME/uibench/crate/target/release/aetherrv-uibench}
KU=kiosk; KID=$(id -u $KU)
RUN=/run/user/$KID
case "${1:-}" in
install)
  printf '%s\n' "# uibench test only (harness/uibench/light-session.sh): deploy/pam/aetherrv-kiosk with class=user-light" \
    "auth     required pam_permit.so" "account  required pam_unix.so" "session  required pam_unix.so" \
    "session  required pam_systemd.so class=user-light" | sudo tee $PAMF >/dev/null
  echo "installed $PAMF"; exit 0 ;;
remove) sudo rm -f $PAMF; echo "removed $PAMF"; exit 0 ;;
check) ;;
*) sed -n '2,21p' "$0"; exit 2 ;;
esac
RT=${2:-wpe-launcher}
AUDIO_URL=${3:-http://127.0.0.1:18082/?bench=1}
[ -f $PAMF ] || { echo "no $PAMF: run $0 install" >&2; exit 4; }
for _ in $(seq 30); do loginctl list-sessions --no-legend | awk '{print $3}' | grep -qx $KU || break; sleep 0.5; done   # a previous run's
loginctl list-sessions --no-legend | awk '{print $3}' | grep -qx $KU && { echo "refused: $KU already has a session" >&2; exit 5; }
curl -sf -m 3 -o /dev/null "$AUDIO_URL" || { echo "refused: the audio page $AUDIO_URL does not answer (aetherrv-uibench serve?)" >&2; exit 7; }
grep -q Loopback /proc/asound/cards || { echo "no snd-aloop Loopback card: sudo modprobe snd-aloop" >&2; exit 6; }
PASS=0; FAIL=0
check() { if [ "$2" = "$3" ]; then echo "  ok   $1 ($2)"; PASS=$((PASS + 1)); else echo "  FAIL $1 (got $2, want $3)"; FAIL=$((FAIL + 1)); fi; }
asroot() { sudo env XDG_RUNTIME_DIR=$RUN WAYLAND_DISPLAY=wayland-1 SWAYSOCK="$(sudo sh -c "ls -t $RUN/sway-ipc.$KID.*.sock 2>/dev/null | head -1")" "$@"; }

sudo rm -rf $W; sudo install -d -m 755 $W
case $RT in
  wpe-launcher) sudo install -m 755 "${UIBENCH_WPE_BIN:-$HOME/uibench/wpe/aetherrv-panel-wpe}" $W/runtime
                RCMD="AETHERRV_PANEL_PROFILE=$W/profile $W/runtime \"\$URL\""; KILL=$W/runtime ;;
  tauri) sudo install -m 755 "${UIBENCH_TAURI_BIN:-$HOME/uibench/tauri/target/release/aetherrv-panel-tauri}" $W/runtime
         RCMD="$W/runtime \"\$URL\""; KILL=$W/runtime ;;
  chromium) # the van's flags (packaging/panel/launch.sh)
    RCMD="/usr/bin/chromium --kiosk --ozone-platform=wayland --noerrdialogs --autoplay-policy=no-user-gesture-required --disable-infobars --no-first-run --disable-session-crashed-bubble --disable-features=Translate,TouchpadOverscrollHistoryNavigation --overscroll-history-navigation=0 --check-for-update-interval=31536000 --password-store=basic --user-data-dir=$W/profile \"\$URL\""
    KILL="^/usr/lib/chromium/chromium --kiosk" ;;   # /usr/bin/chromium execs it; its children run with --type=
  *) echo "runtime: wpe-launcher|chromium|tauri" >&2; exit 2 ;;
esac
sed -e "s|@OUTPUT@|HEADLESS-1|g" -e "s|@MODE@|mode 1920x1200@60Hz|" "$H/sway.conf.in" | sudo tee $W/sway.conf >/dev/null
printf '%s\n' "exec /opt/aetherrv-rs/bin/aetherrv-screen" "exec $W/launch.sh" | sudo tee -a $W/sway.conf >/dev/null
printf '%s\n' '#!/bin/bash' "URL=\$(cat $W/url)" "$RCMD >> $W/runtime.log 2>&1" "echo \"runtime exited \$?\" >> $W/runtime.log" 'swaymsg exit' \
  | sudo tee $W/launch.sh >/dev/null
echo "http://127.0.0.1:8080/?bench=1" | sudo tee $W/url >/dev/null
sudo chmod 755 $W/launch.sh; sudo install -d -o $KU -m 755 $W/profile; sudo install -m 644 -o $KU /dev/null $W/runtime.log
start() {   # $1 = backends; the Loopback card is the default ALSA card for everything in the session (ALSA_CARD)
  sudo systemctl stop $U 2>/dev/null; sudo systemctl reset-failed $U 2>/dev/null
  sudo systemd-run --quiet --unit=$U --uid=$KU -p PAMName=uibench-light -p TTYPath=/dev/tty8 -p StandardInput=tty-fail \
    -p StandardOutput=journal -p StandardError=journal -p TTYReset=yes -p TTYVHangup=yes -p UtmpIdentifier=tty8 -p UtmpMode=user \
    -p Restart=always -p RestartSec=1 -p Environment=XDG_SESSION_TYPE=wayland -p "Environment=WLR_BACKENDS=$1" \
    -p Environment=WLR_HEADLESS_OUTPUTS=1 -p Environment=ALSA_CARD=Loopback /usr/bin/sway --config $W/sway.conf
  sudo chvt 8
}
page() {   # $1 = command for the page, or "" ; prints the last few page reports as JSON
  "$BIN" frames --secs "${2:-4}" --no-capture ${1:+--cmd "$1"} \
    | jq -c '[.page[] | select(.kind == "page" or .kind == "input")] | .[-4:] | map({kind, why, screen, conn, ready, type, audio})'
}
last() {   # $1 = the report's reason, or * ; $2 = the field of the last such report
  jq -r --arg why "$1" --arg k "$2" '[.[] | select(.kind == "page" and ($why == "*" or .why == $why))]
    | if length > 0 then .[-1][$k] | if . == null then "None" else . end else "no report" end'
}
sid() { loginctl list-sessions --no-legend | awk -v u=$KU '$3 == u {print $1}' | head -1; }
rms() {   # a capture file → "peak dBFS, loudest 50 ms RMS dBFS, non-silent 50 ms windows"
  "$BIN" wav-level "$1"
}

echo "== 1. a user-light session for $KU (uid $KID) running $RT: headless output + libinput, helper, runtime"
start headless,libinput
for _ in $(seq 50); do [ -n "$(sid)" ] && break; sleep 0.1; done
S=$(sid); first=$(page "" 14)
check "session class" "$(loginctl show-session "$S" -p Class --value)" user-light
check "session seat / tty" "$(loginctl show-session "$S" -p Seat --value)/$(loginctl show-session "$S" -p TTY --value)" seat0/tty8
check "XDG_RUNTIME_DIR $RUN owned by $KU" "$(stat -c %U $RUN 2>/dev/null)" $KU
check "no user service manager for $KU" "$(systemctl is-active user@$KID.service)" inactive
check "no session bus ($RUN/bus)" "$(sudo test -e $RUN/bus && echo present || echo absent)" absent
check "sway runs" "$(pgrep -u $KU -x sway >/dev/null && echo yes || echo no)" yes
check "sway has seat0's input (libinput)" "$(asroot swaymsg -t get_seats -r | jq -r 'if length > 0 then .[0].name else "none" end')" seat0
case $RT in
  wpe-launcher)   # xdg-dbus-proxy only runs when there is a session (a11y) bus to proxy
    check "WebKit sandbox up (bwrap, the web process)" "$(pgrep -u $KU -x bwrap >/dev/null && echo -n "bwrap "; pgrep -u $KU -x WPEWebProcess >/dev/null && echo -n web-process)" "bwrap web-process"
    echo "  (xdg-dbus-proxy: $(pgrep -u $KU -x xdg-dbus-proxy >/dev/null && echo running || echo 'not started: no bus to proxy'))" ;;
  tauri) check "WebKitGTK's web process up (Tauri runs it without bubblewrap)" "$(pgrep -u $KU -x WebKitWebProces >/dev/null && echo yes || echo no)" yes ;;
  chromium) check "Chromium up (browser and renderer processes)" "$( [ "$(pgrep -u $KU -x chromium | wc -l)" -ge 3 ] && echo yes || echo no)" yes ;;
esac
check "the page reported (loaded, live)" "$(echo "$first" | last '*' conn)" live
echo "  (the runtime's log, last lines:)"; sudo tail -2 $W/runtime.log | cut -c1-150 | sed 's/^/    /'
sudo journalctl -u $U -b --since "-60s" --no-pager -o cat | grep -iE "a11y|at-spi|dbus|timeout" | head -3 | sed 's/^/  journal: /'
echo "== 2. a tap reaches the page (uinput; on the page title, which has no handler)"
out=$( { sleep 2; asroot "$BIN" touch-test 400 48 >/dev/null 2>&1; } & page "" 6 )
check "pointerdown seen by the page" "$(echo "$out" | grep -c '"type":"pointerdown"' | awk '{print ($1 > 0) ? "yes" : "no"}')" yes
echo "== 3. screen off/on through the page"
pw() { asroot swaymsg -t get_outputs -r 2>/dev/null \
  | jq -rn 'try (input | .[0] | if has("power") then .power | tostring | ascii_downcase else error end) catch "no sway"'; }
page "screen off" 4 >/dev/null; check "output off after the page's POST /screen/off" "$(pw)" false
sleep 2; page "screen on" 4 >/dev/null; check "output on after POST /screen/on" "$(pw)" true
echo "== 4. the alert chime reaches ALSA (Loopback card as the session's default; the page from $AUDIO_URL)"
check "kiosk's ACL on the Loopback card (playback device)" "$(getfacl -p /dev/snd/pcmC2D0p 2>/dev/null | grep -c "^user:$KU:rw")" 1
echo "$AUDIO_URL" | sudo tee $W/url >/dev/null
sudo rm -f $W/cap.wav
sudo arecord -q -D plughw:Loopback,1,0 -f S16_LE -r 48000 -c 2 -d 22 $W/cap.wav &
rec=$!; sleep 0.5
sudo systemctl restart $U
a1=$(page "" 12)                       # the page loads; its active alerts chime once (day mode, sound on)
a2=$(page "chime" 6)                   # and the bench module's "chime": playChime("serious"), the UI's own path
wait $rec
echo "  capture: $(rms $W/cap.wav)"
check "the capture is not silent" "$(rms $W/cap.wav | grep -qE ' [1-9][0-9]* non-silent' && echo sound || echo silent)" sound
check "AudioContext state after the chime" "$(echo "$a2" | last chime audio)" running
echo "  (AudioContext state at load: $(echo "$a1" | last '*' audio))"
echo "http://127.0.0.1:8080/?bench=1" | sudo tee $W/url >/dev/null
echo "== 5. restart: three systemctl restarts (no process or session left from an earlier one), then kill -9 of the runtime"
nrt() { case $RT in chromium) pgrep -u $KU -f -c "$KILL" ;; *) pgrep -u $KU -f -c "^$W/runtime" ;; esac; }
for n in 1 2 3; do
  sudo systemctl restart $U; r=$(page "" 14); sleep 3
  check "restart $n: page live" "$(echo "$r" | last '*' conn)" live
  check "restart $n: one runtime, one session (no orphans, none closing)" "$(nrt) $(loginctl list-sessions --no-legend | awk -v u=$KU '$3 == u' | wc -l) $(loginctl list-sessions --no-legend | awk -v u=$KU '$3 == u {print $1}' | while read -r x; do loginctl show-session "$x" -p State --value; done | grep -c closing)" "1 1 0"
  ps -u $KU -o pid=,lstart=,args= | cut -c1-110 | grep -E "runtime|chromium --kiosk|sway|launch.sh|aetherrv-screen" | sed 's/^/    /'
done
n0=$(systemctl show -p NRestarts --value $U)
sudo pkill -9 -n -u $KU -f "$( [ "$RT" = chromium ] && echo "$KILL" || echo "^$W/runtime")"   # the current runtime
r=$(page "" 16); sleep 2
check "back after kill -9 of the runtime (page live)" "$(echo "$r" | last '*' conn)" live
check "the unit restarted the session (NRestarts +1)" "$(( $(systemctl show -p NRestarts --value $U) - n0 ))" 1
check "after the kill: one runtime, one session" "$(nrt) $(loginctl list-sessions --no-legend | awk -v u=$KU '$3 == u' | wc -l)" "1 1"
sudo tail -2 $W/runtime.log | cut -c1-150 | sed 's/^/  runtime log: /'
echo "== 6. sway on the DRM backend (no monitor: only that it opens the DRM device through the seat)"
sudo systemctl stop $U; sleep 1; start drm,libinput; sleep 4
check "sway alive on drm,libinput" "$(pgrep -u $KU -x sway >/dev/null && echo yes || echo no)" yes
sudo systemctl stop $U; sudo systemctl reset-failed $U 2>/dev/null; sudo chvt 1
sudo rm -rf $W
echo "light session ($RT): $PASS ok, $FAIL failed (the PAM test file stays until: $0 remove)"
[ "$FAIL" = 0 ]
