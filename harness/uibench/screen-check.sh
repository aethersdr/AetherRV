#!/bin/bash
# Screen off/on from the page, per runtime (docs/UI-RUNTIME-BENCHMARK.md §7). On the BENCH Pi, in the benchmark's
# sway session: the screen helper runs in the session as the panel runs it (aetherrv-screen on 127.0.0.1:8091, which
# answers with Access-Control-Allow-Origin: http://127.0.0.1:8080), the runtime opens the gateway's UI on :8080, and
# the page's bench module is asked to take the screen-off control's own path (logic/kiosk.ts: POST /screen/off),
# then the wake path an alert takes (POST /screen/on). Checks: the page found the helper ($screen.available, so the
# control is offered), the output went off and came back, and the page is live afterwards. Nothing is tapped.
#   screen-check.sh [--runtime wpe|tauri|chromium]      default: wpe
set -euo pipefail
if ip -br link | grep -qE '^(hat0|hat1)\b'; then echo "refused: this host has hat0/hat1, it is the van" >&2; exit 3; fi
BIN=${UIBENCH_BIN:-$HOME/uibench/crate/target/release/aetherrv-uibench}
SCREEN_BIN=${UIBENCH_SCREEN_BIN:-/opt/aetherrv-rs/bin/aetherrv-screen}
TAURI_BIN=${UIBENCH_TAURI_BIN:-$HOME/uibench/tauri/target/release/aetherrv-panel-tauri}
set -a; # shellcheck disable=SC1091
. "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/uibench/env"; set +a
RT=wpe
[ "${1:-}" = --runtime ] && RT=${2:?runtime}
U="http://127.0.0.1:8080/?bench=1"         # the helper's CORS allows the gateway's origin only
prof=$(mktemp -d)
case $RT in
  chromium) cmd=(chromium --kiosk --ozone-platform=wayland --noerrdialogs --no-first-run --user-data-dir="$prof" "$U") ;;
  wpe) cmd=(env WPE_PLATFORM=wayland /usr/lib/wpe-webkit-2.0/MiniBrowser --fullscreen "$U") ;;
  wpe-launcher) cmd=(env AETHERRV_PANEL_PROFILE="$prof" "${UIBENCH_WPE_BIN:-$HOME/uibench/wpe/aetherrv-panel-wpe}" "$U") ;;
  tauri) cmd=("$TAURI_BIN" "$U") ;;
  *) echo "runtime: chromium|wpe|wpe-launcher|tauri" >&2; exit 2 ;;
esac
cleanup() {
  systemctl --user stop uibench-rt-screencheck.scope uibench-screen.scope 2>/dev/null || true
  swaymsg "output $OUTPUT power on" >/dev/null 2>&1 || true
  rm -rf "$prof"
}
trap cleanup EXIT
power() { swaymsg -t get_outputs -r | python3 -c 'import json,sys; print(str(json.load(sys.stdin)[0]["power"]).lower())'; }
PASS=0; FAIL=0
check() { if [ "$2" = "$3" ]; then echo "  ok   $1 ($2)"; PASS=$((PASS + 1)); else echo "  FAIL $1 (got $2, want $3)"; FAIL=$((FAIL + 1)); fi; }
lastrep() {   # the page's last report for a reason, as "available shield conn ready"
  python3 -c '
import json, sys
d = json.load(sys.stdin)
r = [p for p in d["page"] if p.get("kind") == "page" and (sys.argv[1] == "*" or p.get("why") == sys.argv[1])]
if not r: print("none none none none"); sys.exit()
p = r[-1]; s = p.get("screen") or {}
print(str(s.get("available")).lower(), str(s.get("shield")).lower(), p.get("conn"), str(p.get("ready")).lower())' "$1"
}

echo "== $RT: screen off/on from the page"
systemd-run --user --scope --quiet --collect --unit=uibench-screen -- "$SCREEN_BIN" >/dev/null 2>&1 &
for _ in $(seq 25); do curl -sf -m 1 -o /dev/null http://127.0.0.1:8091/screen && break; sleep 0.2; done
check "helper answers" "$(curl -s -m 2 http://127.0.0.1:8091/screen)" '{"on":true}'
check "helper allows the gateway's origin" "$(curl -s -m 2 -D - -o /dev/null http://127.0.0.1:8091/screen | tr -d '\r' | sed -n 's/^Access-Control-Allow-Origin: //p')" "http://127.0.0.1:8080"
systemd-run --user --scope --quiet --collect --unit=uibench-rt-screencheck -- "${cmd[@]}" >/dev/null 2>&1 &
# the page's first reports (ready, after it has asked the helper)
read -r avail _ conn ready < <("$BIN" frames --secs 12 --no-capture | lastrep '*')
check "page found the helper (\$screen.available)" "$avail" true
check "page ready" "$ready" true
# 10 s: with the output off the page is hidden, and its timers (the report is due 2.5 s after the command) run late
read -r avail shield conn ready < <("$BIN" frames --secs 10 --no-capture --cmd "screen off" | lastrep screen-off)
check "output off after POST /screen/off" "$(power)" false
check "page raised its wake shield" "$shield" true
sleep 3
read -r avail shield conn ready < <("$BIN" frames --secs 4 --no-capture --cmd "screen on" | lastrep screen-on)
check "output on after POST /screen/on" "$(power)" true
sleep 2   # the shield drops 0.7 s after the page's next poll of the helper sees the screen on
read -r avail shield conn ready < <("$BIN" frames --secs 3 --no-capture --cmd report | lastrep asked)
check "page live afterwards (WebSocket)" "$conn" "${UIBENCH_CONN_LIVE:-live}"
check "wake shield dropped" "$shield" false
echo "$RT: $PASS ok, $FAIL failed"
[ "$FAIL" = 0 ]
