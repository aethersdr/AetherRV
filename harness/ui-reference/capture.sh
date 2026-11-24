#!/bin/bash
# Reference screenshots of every page at 1920x1200 (prompt §5): harness/ui-reference/capture.sh <url> <out-dir> [w h]
set -euo pipefail
URL=${1:?url}; OUT=${2:?out dir}; W=${3:-1920}; H=${4:-1200}
mkdir -p "$OUT"
for v in home energy climate weather switches network alerts systems settings; do
  "${CHROME:-google-chrome-stable}" --headless=new --hide-scrollbars --disable-gpu --user-data-dir="$(mktemp -d)" \
    --window-size="$W,$H" --virtual-time-budget=6000 --screenshot="$OUT/$v.png" "$URL#$v" >/dev/null 2>&1
  echo "$OUT/$v.png"
done
