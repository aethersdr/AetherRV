#!/bin/bash
# Render the AetherRV boot hero (packaging/boot/aetherrv-hero.html) to the Plymouth splash PNG (1920×1200).
# Needs a local Chromium and the Inter / Inter Display fonts.
set -euo pipefail
cd "$(dirname "$0")/.."
OUT=packaging/boot/plymouth/aetherrv/splash.png
PROF=$(mktemp -d)
chromium --headless=new --no-sandbox --disable-gpu --hide-scrollbars --force-device-scale-factor=1 \
  --window-size=1920,1200 --user-data-dir="$PROF" --virtual-time-budget=3000 \
  --screenshot="$PWD/$OUT" "file://$PWD/packaging/boot/aetherrv-hero.html" >/dev/null 2>&1
rm -rf "$PROF"
file "$OUT"
