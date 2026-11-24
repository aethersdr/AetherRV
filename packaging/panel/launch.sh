#!/bin/bash
# Start the UI runtime on http://127.0.0.1:8080/ (the gateway) and end the sway session when it exits, so systemd
# restarts the whole panel. The runtime is chosen by the benchmark (docs/UI-RUNTIME-BENCHMARK.md) and set in
# /etc/aetherrv/panel.env: AETHERRV_PANEL_RUNTIME=chromium | wpe | tauri (default chromium).
URL=${AETHERRV_PANEL_URL:-http://127.0.0.1:8080/}
STATE=/var/lib/aetherrv-panel
case "${AETHERRV_PANEL_RUNTIME:-chromium}" in
  wpe)
    WPE_PLATFORM=wayland /usr/lib/wpe-webkit-2.0/MiniBrowser --fullscreen "$URL"
    ;;
  tauri)
    /opt/aetherrv-rs/bin/aetherrv-panel-tauri "$URL"
    ;;
  *)
    PROFILE=$STATE/chromium
    # the profile lock records the hostname; only one panel Chromium ever runs, so stale locks are safe to clear
    rm -f "$PROFILE/SingletonLock" "$PROFILE/SingletonSocket" "$PROFILE/SingletonCookie"
    /usr/bin/chromium --kiosk --ozone-platform=wayland \
      --noerrdialogs --autoplay-policy=no-user-gesture-required --disable-infobars --no-first-run --disable-session-crashed-bubble \
      --disable-features=Translate,TouchpadOverscrollHistoryNavigation --overscroll-history-navigation=0 \
      --check-for-update-interval=31536000 --password-store=basic \
      --user-data-dir="$PROFILE" "$URL"
    ;;
esac
swaymsg exit
