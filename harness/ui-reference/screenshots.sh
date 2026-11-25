#!/bin/bash
# The README's screenshots (docs/screenshots/*.png) from the van's real state, sanitized. Workstation only; needs jq,
# sqlite3, Chrome and the UI's npm packages. Reads the van only through tools/van-read.sh (read-only, logged):
#
#   harness/ui-reference/screenshots.sh fetch          the van's /api/state and a copy of its history database
#   harness/ui-reference/screenshots.sh render [out]   sanitize (sanitize-state.jq), then render home, energy,
#                                                      climate, weather and network (default out: harness/out/shots/png)
#
# Review the PNGs before copying them to docs/screenshots: they go public. The raw files stay in harness/out (gitignored).
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
W=$ROOT/harness/out/shots
case "${1:-}" in
  fetch)
    mkdir -p "$W"
    date +%s > "$W/taken-at"
    "$ROOT"/tools/van-read.sh state > "$W/state-raw.json"
    "$ROOT"/tools/van-read.sh history-copy "$W/hist"
    echo "fetched into $W"
    ;;
  render)
    OUT=$(realpath -m "${2:-$W/png}")
    jq -f "$ROOT"/harness/ui-reference/sanitize-state.jq "$W/state-raw.json" > "$W/state.json"
    cd "$ROOT/ui"
    SHOTS_STATE="$W/state.json" SHOTS_HISTORY="$W/hist/history.sqlite3" SHOTS_TAKEN_AT="$(cat "$W/taken-at")" \
      SHOTS_OUT="$OUT" npx playwright test -c playwright.shots.config.ts
    echo "screenshots in $OUT"
    ;;
  *) sed -n '2,9p' "$0" >&2; exit 2 ;;
esac
