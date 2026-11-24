#!/bin/bash
# Isolation for the panel (the touchscreen session), which harness/bench/isolation.sh does not cover. On the BENCH Pi:
# the real aetherrv-panel.service (packaging/systemd, its PAM stack, launch.sh, sway.conf, screen.sh) with the WPE
# launcher, on a headless 1920x1200 output (a runtime drop-in in /run: the bench has no panel, owner Q-U7). Each case
# kills one part with SIGKILL (or restarts the unit), then checks:
#   - the panel is whole again within the limit: sway (the unit's main process), one launch.sh, one runtime and its
#     web process, one screen helper loop and one helper answering on :8091, and the page's connection to :8080;
#   - what should survive did (the screen helper case: the same sway, runtime and panel session);
#   - nothing of the old session is left (its sway, launch.sh, runtime, loop or helper);
#   - no other aetherrv unit restarted (NRestarts and MainPID unchanged) and the gateway answered throughout
#     (/api/state every 0.25 s; the longest gap is reported).
# Cases: the screen helper, the WPE launcher, its web process, sway, `systemctl restart`.
#   panel-isolation.sh     prints one line per case and PANEL ISOLATION: PASS | FAIL; leaves the panel stopped
set -uo pipefail
if ip -br link | grep -qE '^(hat0|hat1)\b'; then echo "refused: this host has hat0/hat1, it is the van" >&2; exit 3; fi
U=aetherrv-panel.service
DROPD=/run/systemd/system/$U.d
DROP=$DROPD/50-bench-headless.conf
K=kiosk
OUT=${PANEL_ISO_OUT:-/var/tmp/panel-isolation}
[ -x /opt/aetherrv-rs/panel/screen.sh ] || { echo "refused: /opt/aetherrv-rs/panel/screen.sh missing (deploy a build with it)" >&2; exit 4; }
[ -x /opt/aetherrv-rs/bin/aetherrv-panel-wpe ] || { echo "refused: the WPE launcher is not installed" >&2; exit 4; }
if [ -f /etc/aetherrv/panel.env ] && grep -q '^AETHERRV_PANEL_RUNTIME=' /etc/aetherrv/panel.env && ! grep -q '^AETHERRV_PANEL_RUNTIME=wpe$' /etc/aetherrv/panel.env; then
  echo "refused: /etc/aetherrv/panel.env sets another runtime" >&2; exit 4
fi
systemctl is-active -q $U && { echo "refused: the panel is already running" >&2; exit 5; }
mkdir -p "$OUT"; rm -f "$OUT"/*

cleanup() {
  [ -n "${GW:-}" ] && kill "$GW" 2>/dev/null
  sudo systemctl stop "$U" 2>/dev/null
  sudo rm -f "$DROP"; sudo rmdir "$DROPD" 2>/dev/null
  sudo systemctl daemon-reload
}
trap cleanup EXIT
sudo mkdir -p "$DROPD"
sudo tee "$DROP" >/dev/null <<'EOF'
# bench only (harness/bench/panel-isolation.sh), in /run: a headless output in place of the panel, the WPE launcher
[Service]
Environment=WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 WLR_LIBINPUT_NO_DEVICES=1 AETHERRV_PANEL_RUNTIME=wpe
EOF
sudo systemctl daemon-reload

# the gateway, every 0.25 s, for the whole run
( while :; do printf '%s %s\n' "$(date +%s.%N)" "$(curl -s -o /dev/null -m 1 -w '%{http_code}' http://127.0.0.1:8080/api/state)"; sleep 0.25; done ) > "$OUT/gateway.log" 2>/dev/null &
GW=$!

pids() { pgrep -u $K "$@" | tr '\n' ' ' | sed 's/ $//'; }
count() { pgrep -u $K "$@" | wc -l; }
sway_pid() { systemctl show -p MainPID --value $U; }
nrestarts() { systemctl show -p NRestarts --value $U; }
# the other aetherrv units: Id NRestarts MainPID (the panel left out)
others() { systemctl list-units --no-legend --plain 'aetherrv*' | awk '{print $1}' | grep -v "^$U$" | sort \
  | while read -r u; do echo "$u $(systemctl show -p NRestarts --value "$u") $(systemctl show -p MainPID --value "$u")"; done; }
snapshot() {   # sway launch.sh launcher webprocs loop helper
  echo "$(sway_pid)|$(pids -f '^/bin/bash /opt/aetherrv-rs/panel/launch.sh')|$(pids -f '^/opt/aetherrv-rs/bin/aetherrv-panel-wpe')|$(pids -x WPEWebProcess)|$(pids -f '/opt/aetherrv-rs/panel/screen.sh')|$(pids -x aetherrv-screen)"
}
whole() {
  local s; s=$(sway_pid)
  [ "${s:-0}" != 0 ] && kill -0 "$s" 2>/dev/null || return 1
  [ "$(count -f '^/bin/bash /opt/aetherrv-rs/panel/launch.sh')" = 1 ] || return 1
  [ "$(count -f '^/opt/aetherrv-rs/bin/aetherrv-panel-wpe')" = 1 ] || return 1
  [ "$(count -x WPEWebProcess)" -ge 1 ] || return 1
  [ "$(count -f '/opt/aetherrv-rs/panel/screen.sh')" = 1 ] || return 1
  [ "$(count -x aetherrv-screen)" = 1 ] || return 1
  curl -sf -m 1 -o /dev/null http://127.0.0.1:8091/screen || return 1
  # the page is live: a kiosk-owned process holds a connection to the gateway
  sudo ss -Htnp state established '( dport = :8080 )' | grep -q "pid=" || return 1
  sudo ss -Htnp state established '( dport = :8080 )' | grep -oE 'pid=[0-9]+' | cut -d= -f2 \
    | while read -r p; do [ "$(stat -c %U /proc/"$p" 2>/dev/null)" = $K ] && echo y; done | grep -q y
}
wait_whole() {   # limit_s → seconds taken, or empty
  local t0 t; t0=$(date +%s.%N)
  while :; do
    t=$(echo "$(date +%s.%N) - $t0" | bc)
    if whole; then echo "$t"; return 0; fi
    (( $(echo "$t > $1" | bc) )) && return 1
    sleep 0.2
  done
}
alive_of() { for p in $1; do kill -0 "$p" 2>/dev/null && echo -n "$p "; done; }

echo "panel-isolation: $(cat /opt/aetherrv-rs/VERSION 2>/dev/null), $(date '+%F %T')"
sudo systemctl start $U
T=$(wait_whole 60) || { echo "the panel did not come up within 60 s"; snapshot; sudo journalctl -u $U -n 30 --no-pager; exit 1; }
echo "start: whole after ${T}s ($(snapshot))"
sleep 3
FAIL=0
run_case() {   # name kill-target-cmd survives(yes|no)
  local name=$1 survive=$3 before o_before n_before old t left after o_after
  before=$(snapshot); o_before=$(others); n_before=$(nrestarts)
  IFS='|' read -r b_sway b_launch b_rt b_web b_loop b_helper <<< "$before"
  eval "$2"
  if [ "$survive" = yes ]; then
    t=$(wait_whole 10); sleep 0.5
  else
    sleep 1; t=$(wait_whole 30); sleep 4   # the old helper polls its sway every 2 s
  fi
  after=$(snapshot); o_after=$(others)
  IFS='|' read -r a_sway _ a_rt _ _ a_helper <<< "$after"
  local ok=1 why=""
  [ -n "$t" ] || { ok=0; why+=" not whole again;"; }
  if [ "$survive" = yes ]; then
    [ "$a_sway" = "$b_sway" ] && [ "$a_rt" = "$b_rt" ] && [ "$(nrestarts)" = "$n_before" ] || { ok=0; why+=" the session did not survive;"; }
    [ "$a_helper" != "$b_helper" ] || { ok=0; why+=" the helper was not restarted;"; }
  else
    [ "$a_sway" != "$b_sway" ] || { ok=0; why+=" sway not new;"; }
    old="$b_sway $b_launch $b_rt $b_web $b_loop $b_helper"
    left=$(alive_of "$old")
    [ -z "$left" ] || { ok=0; why+=" left over: $left;"; }
  fi
  [ "$o_after" = "$o_before" ] || { ok=0; why+=" another unit changed: $(diff <(echo "$o_before") <(echo "$o_after") | grep '^>' | tr '\n' ' ');"; }
  if [ $ok = 1 ]; then echo "PASS $name: whole again after ${t}s; no other unit restarted"; else echo "FAIL $name:$why"; FAIL=1; fi
  echo "  before $before"; echo "  after  $after"
  sleep 3
}
# shellcheck disable=SC2016  # expanded inside run_case, after its snapshot
run_case "screen helper (kill -9)"  'sudo kill -9 $b_helper' yes
run_case "WPE launcher (kill -9)"   'sudo kill -9 $b_rt' no
run_case "web process (kill -9)"    'sudo kill -9 $b_web' no
run_case "sway (kill -9)"           'sudo kill -9 $b_sway' no
run_case "systemctl restart"        'sudo systemctl restart $U' no
kill "$GW" 2>/dev/null; GW=
# the gateway: the longest gap between answers, and anything but 200
python3 -I - "$OUT/gateway.log" <<'EOF'
import sys
rows=[l.split() for l in open(sys.argv[1]) if l.strip()]
ok=[float(t) for t,c in rows if c=="200"]; bad=[c for t,c in rows if c!="200"]
gap=max((b-a for a,b in zip(ok,ok[1:])), default=0)
print(f"gateway: {len(ok)} answers, {len(bad)} not 200, longest gap {gap:.2f} s")
sys.exit(1 if bad or gap > 1.5 else 0)
EOF
[ $? = 0 ] || FAIL=1
echo "panel exit codes: $(sudo journalctl -u $U --since "-15min" --no-pager -o cat | grep -oE 'aetherrv-panel-wpe: [^:]+' | sort | uniq -c | tr '\n' ';')"
[ $FAIL = 0 ] && echo "PANEL ISOLATION: PASS" || echo "PANEL ISOLATION: FAIL"
exit $FAIL
