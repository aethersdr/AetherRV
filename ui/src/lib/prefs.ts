// Per-screen preferences in localStorage (same keys as app.js: "vp.*"). A kiosk without storage just uses defaults.
export const prefs = {
  get(k: string, d: string): string {
    try { return localStorage.getItem(k) ?? d; } catch { return d; }
  },
  set(k: string, v: string): void {
    try { localStorage.setItem(k, v); } catch { /* kiosk without storage */ }
  },
};

/** The kiosk itself (the van's touchscreen) opens the UI on loopback; remote browsers don't. */
/** ?kiosk=0 turns the kiosk behaviour off on loopback (development on a workstation: keeps the mouse pointer). */
export const IS_LOCAL = typeof location !== "undefined" && ["127.0.0.1", "localhost", "::1", "[::1]"].includes(location.hostname)
  && new URLSearchParams(location.search).get("kiosk") !== "0";
