#!/bin/bash
# Start the UI runtime on http://127.0.0.1:8080/ (the gateway) and end the sway session when it exits, so systemd
# restarts the whole panel. The runtime: WPE through the panel's own launcher (the owner's choice, 2026-11-24, from
# the benchmark: docs/UI-RUNTIME-BENCHMARK.md §9). /etc/aetherrv/panel.env can set AETHERRV_PANEL_RUNTIME=wpe |
# chromium | tauri; by default wpe, or chromium when the WPE launcher isn't installed (wpewebkit missing at build).
URL=${AETHERRV_PANEL_URL:-http://127.0.0.1:8080/}
STATE=/var/lib/aetherrv-panel
DEFAULT=chromium
[ -x /opt/aetherrv-rs/bin/aetherrv-panel-wpe ] && DEFAULT=wpe
case "${AETHERRV_PANEL_RUNTIME:-$DEFAULT}" in
  wpe)
    # the panel's own WPE kiosk launcher (panel/wpe): full screen, the page colour behind the view, no chrome, no
    # navigation off the UI, a persistent profile (the UI keeps per-screen preferences in localStorage); it exits
    # non-zero when its web process crashes or hangs, and this script then ends the session (Restart=always).
    if [ -x /opt/aetherrv-rs/bin/aetherrv-panel-wpe ]; then
      AETHERRV_PANEL_PROFILE=$STATE/wpe /opt/aetherrv-rs/bin/aetherrv-panel-wpe "$URL"
    else
      # fallback: WPE's MiniBrowser (#0b0c0f is the UI's --page colour, so a slow first paint can't flash white).
      # MiniBrowser does not exit when its web process dies (the page just stays blank), so a crash is not restarted
      # on this path; and it has no guard against navigating off the UI. The launcher above has both.
      WPE_PLATFORM=wayland /usr/lib/wpe-webkit-2.0/MiniBrowser --fullscreen --bg-color='#0b0c0f' --profile-dir="$STATE/wpe-minibrowser" "$URL"
    fi
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
