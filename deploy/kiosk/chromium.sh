#!/bin/bash
# Run the console in Chromium; if Chromium exits, exit sway so systemd restarts the kiosk cleanly.
PROFILE=/var/lib/aetherrv-kiosk/chromium
# Chromium's profile lock records the hostname. After a rename (Settings → Device) it would refuse the profile
# ("in use on another computer") and exit at once, looping the kiosk. Only one kiosk Chromium ever runs and any
# previous one died with its sway session, so stale locks are always safe to clear here.
rm -f "$PROFILE/SingletonLock" "$PROFILE/SingletonSocket" "$PROFILE/SingletonCookie"
/usr/bin/chromium --kiosk --ozone-platform=wayland \
  --noerrdialogs --autoplay-policy=no-user-gesture-required --disable-infobars --no-first-run --disable-session-crashed-bubble \
  --disable-features=Translate,TouchpadOverscrollHistoryNavigation --overscroll-history-navigation=0 \
  --check-for-update-interval=31536000 --password-store=basic \
  --user-data-dir="$PROFILE" http://127.0.0.1:8080/
swaymsg exit
