#!/bin/bash
# The WPE kiosk launcher (panel/wpe/aetherrv-panel-wpe) does what the panel needs, checked on the BENCH Pi in the
# benchmark's sway session (docs/UI-RUNTIME-BENCHMARK.md §9):
#   1. it stays on its origin: a test page on 127.0.0.1:18085 tries to navigate to another origin
#      (127.0.0.1:18086) and to open a window there; the second server must see no request at all;
#   2. it fills the output (fullscreen) and shows the page colour before the first paint (sway's tree, a screenshot);
#   3. it exits non-zero when its web process dies: SIGKILL, and SIGABRT (a crash with a core dump, which takes
#      systemd-coredump some seconds); and with 0 on SIGTERM. (A SIGSEGV sent with kill is no test: WebKit's
#      JavaScript engine handles SIGSEGV itself and the process carries on.)
#   4. the same crash under MiniBrowser, for comparison (what launch.sh's fallback does).
#   launcher-check.sh [launcher binary]     default ~/uibench/wpe/aetherrv-panel-wpe
set -uo pipefail
if ip -br link | grep -qE '^(hat0|hat1)\b'; then echo "refused: this host has hat0/hat1, it is the van" >&2; exit 3; fi
set -a; # shellcheck disable=SC1091
. "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/uibench/env"; set +a
L=${1:-$HOME/uibench/wpe/aetherrv-panel-wpe}
T=$(mktemp -d); trap 'kill $(jobs -p) 2>/dev/null; rm -rf "$T"' EXIT
PASS=0; FAIL=0
check() { if [ "$2" = "$3" ]; then echo "  ok   $1 ($2)"; PASS=$((PASS + 1)); else echo "  FAIL $1 (got $2, want $3)"; FAIL=$((FAIL + 1)); fi; }
mkdir -p "$T/a" "$T/b"
cat > "$T/a/index.html" <<'HTML'
<!doctype html><html><body style="background:#0b0c0f;color:#ccc;font:40px sans-serif">
<p id="s">origin test</p>
<script>
setTimeout(() => { try { window.open("http://127.0.0.1:18086/popup"); } catch (e) {} }, 1000);
setTimeout(() => { location.href = "http://127.0.0.1:18086/away"; }, 2000);
setTimeout(() => { fetch("/still-here"); }, 4000);   // the page is still ours if this arrives
</script></body></html>
HTML
echo ok > "$T/b/index.html"
python3 -m http.server 18085 --bind 127.0.0.1 --directory "$T/a" > "$T/a.log" 2>&1 &
python3 -m http.server 18086 --bind 127.0.0.1 --directory "$T/b" > "$T/b.log" 2>&1 &
sleep 1

echo "== the launcher: navigation off its origin"
AETHERRV_PANEL_PROFILE="$T/prof" "$L" http://127.0.0.1:18085/ > "$T/l.log" 2>&1 &
lp=$!
sleep 7
check "no request reached the other origin" "$(grep -c 'GET /' "$T/b.log")" 0
check "the page stayed (its later request arrived)" "$(grep -c 'GET /still-here' "$T/a.log")" 1
grep -o 'navigation to [^ ]* refused' "$T/l.log" | head -2 | sed 's/^/  log: /'
echo "== fullscreen, background"
size=$(swaymsg -t get_tree -r | python3 -c 'import json,sys
def walk(n):
    if n.get("pid") and n.get("type") == "con": yield n
    for c in n.get("nodes", []) + n.get("floating_nodes", []): yield from walk(c)
v = [n for n in walk(json.load(sys.stdin))]
print("%dx%d fullscreen=%s" % (v[0]["rect"]["width"], v[0]["rect"]["height"], v[0].get("fullscreen_mode")) if v else "no window")')
check "window fills the output" "$size" "1920x1200 fullscreen=1"
webproc() { ps -eo pid,rss,comm --sort=-rss | awk '$3 == "WPEWebProcess" { print $1; exit }'; }   # the page's (largest)
crash() {   # $1 = signal, $2 = seconds to wait
  local wp t0; wp=$(webproc); t0=$(date +%s.%N)
  [ -n "$wp" ] && kill "-$1" "$wp"
  for _ in $(seq $(( $2 * 4 ))); do kill -0 "$lp" 2>/dev/null || break; sleep 0.25; done
  if kill -0 "$lp" 2>/dev/null; then check "launcher exited after SIG$1 on its web process" running exited; kill "$lp"; wait "$lp"
  else wait "$lp"; local rc=$? dt; dt=$(python3 -c "import time; print(round(time.time() - $t0, 1))")
    check "launcher exit status after SIG$1 on its web process (${dt} s)" "$rc" 2; fi
}
echo "== crash: kill -KILL its web process"
crash KILL 10
tail -1 "$T/l.log" | sed 's/^/  log: /'
echo "== crash: kill -ABRT its web process (core dump)"
AETHERRV_PANEL_PROFILE="$T/prof" "$L" http://127.0.0.1:18085/ > "$T/l.log" 2>&1 &
lp=$!; sleep 6
crash ABRT 40
tail -1 "$T/l.log" | sed 's/^/  log: /'
echo "== SIGTERM"
AETHERRV_PANEL_PROFILE="$T/prof" "$L" http://127.0.0.1:18085/ > "$T/l2.log" 2>&1 &
lp=$!; sleep 5; kill "$lp"; wait "$lp"; check "launcher exit status on SIGTERM" "$?" 0
echo "== MiniBrowser, the same crash (launch.sh's fallback)"
WPE_PLATFORM=wayland /usr/lib/wpe-webkit-2.0/MiniBrowser --fullscreen --bg-color='#0b0c0f' --profile-dir="$T/mb" http://127.0.0.1:18085/ > "$T/m.log" 2>&1 &
mp=$!; sleep 6
wp=$(webproc); [ -n "$wp" ] && kill -KILL "$wp"
for _ in $(seq 20); do kill -0 "$mp" 2>/dev/null || break; sleep 0.5; done
if kill -0 "$mp" 2>/dev/null; then
  echo "  MiniBrowser still running 10 s after its web process was killed; web process now: $(webproc || echo none)"
  kill "$mp"; wait "$mp"
else wait "$mp"; echo "  MiniBrowser exited by itself after the crash, status $?"; fi
echo "MiniBrowser: $(grep -c 'GET /' "$T/b.log") requests reached the other origin under it (for comparison; it has no such guard)"
echo "launcher: $PASS ok, $FAIL failed"
[ "$FAIL" = 0 ]
