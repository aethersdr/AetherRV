#!/bin/bash
# Which GStreamer plugin packages does WebKit's WebAudio need to reach ALSA? On the BENCH Pi, in the benchmark's sway
# session, with snd-aloop's Loopback card as the default ALSA card (ALSA_CARD): the WPE kiosk launcher loads the
# page, the bench module's "chime" command plays the UI's own chime, and the capture side of the loopback is recorded.
# Each run lets GStreamer load only the plugins of the packages named (GST_PLUGIN_LOADING_WHITELIST, its own
# registry), which stands in for a system where the others aren't installed. Nothing is installed or removed.
#   audio-gst-check.sh [page URL]      default http://127.0.0.1:18082/?bench=1 (a bundle with "chime")
set -uo pipefail
if ip -br link | grep -qE '^(hat0|hat1)\b'; then echo "refused: this host has hat0/hat1, it is the van" >&2; exit 3; fi
set -a; # shellcheck disable=SC1091
. "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/uibench/env"; set +a
BIN=${UIBENCH_BIN:-$HOME/uibench/crate/target/release/aetherrv-uibench}
L=${UIBENCH_WPE_BIN:-$HOME/uibench/wpe/aetherrv-panel-wpe}
URL=${1:-http://127.0.0.1:18082/?bench=1}
grep -q Loopback /proc/asound/cards || { echo "no snd-aloop Loopback card" >&2; exit 6; }
T=$(mktemp -d); trap 'rm -rf "$T"' EXIT
level() { python3 - "$1" <<'PY'
import math, struct, sys, wave
w = wave.open(sys.argv[1]); n = w.getnframes(); s = struct.unpack("<%dh" % (n * w.getnchannels()), w.readframes(n))
peak = max((abs(x) for x in s), default=0)
print("peak %s dBFS" % ("%.1f" % (20 * math.log10(peak / 32768)) if peak else "-inf"))
PY
}
for set in "gstreamer:gst-plugins-base:gst-plugins-good" "gstreamer:gst-plugins-base" "gstreamer:gst-plugins-good"; do
  echo "== plugins from: ${set//:/, }"
  echo "  autoaudiosink: $(GST_PLUGIN_LOADING_WHITELIST=$set GST_REGISTRY=$T/r.bin gst-inspect-1.0 autoaudiosink >/dev/null 2>&1 && echo yes || echo no), alsasink: $(GST_PLUGIN_LOADING_WHITELIST=$set GST_REGISTRY=$T/r.bin gst-inspect-1.0 alsasink >/dev/null 2>&1 && echo yes || echo no)"
  rm -f "$T/cap.wav" "$T/r.bin"
  arecord -q -D plughw:Loopback,1,0 -f S16_LE -r 48000 -c 2 -d 16 "$T/cap.wav" &
  rec=$!; sleep 0.5
  env ALSA_CARD=Loopback GST_PLUGIN_LOADING_WHITELIST="$set" GST_REGISTRY=/tmp/uibench-gst-registry.bin \
    AETHERRV_PANEL_PROFILE="$T/prof" "$L" "$URL" > "$T/l.log" 2>&1 &
  lp=$!; sleep 9
  st=$("$BIN" frames --secs 4 --no-capture --cmd chime | python3 -c 'import json,sys; p=[x for x in json.load(sys.stdin)["page"] if x.get("why")=="chime"]; print(p[-1].get("audio") if p else "no report")')
  wait $rec; kill $lp 2>/dev/null; wait $lp 2>/dev/null
  echo "  capture: $(level "$T/cap.wav"); AudioContext after the chime: $st"
  grep -iE "gstreamer|autoaudiosink|alsasink|audio" "$T/l.log" | sort -u | head -3 | cut -c1-140 | sed 's/^/  log: /'
done
