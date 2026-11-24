// Position helpers for the Weather page, ported from app.js (sunTimes, kmBetween, fmtLL, wxSun). Pure: no DOM.

export type LatLon = readonly [number, number];

/** Sunrise / sunset / solar noon (NOAA simplified algorithm, computed locally: no internet). Epoch ms, or null
 *  in polar day / night. */
export function sunTimes(lat: number, lon: number, date = new Date()): { rise: number; set: number; noon: number } | null {
  const rad = Math.PI / 180, day = Date.UTC(date.getFullYear(), date.getMonth(), date.getDate());
  const n = Math.round((day - Date.UTC(2000, 0, 1, 12)) / 86400000);
  const Jstar = n - lon / 360, M = (357.5291 + 0.98560028 * Jstar) % 360;
  const C = 1.9148 * Math.sin(M * rad) + 0.02 * Math.sin(2 * M * rad) + 0.0003 * Math.sin(3 * M * rad);
  const L = (M + C + 180 + 102.9372) % 360;
  const Jt = 2451545 + Jstar + 0.0053 * Math.sin(M * rad) - 0.0069 * Math.sin(2 * L * rad);
  const dec = Math.asin(Math.sin(L * rad) * Math.sin(23.44 * rad));
  const cosw = (Math.sin(-0.833 * rad) - Math.sin(lat * rad) * Math.sin(dec)) / (Math.cos(lat * rad) * Math.cos(dec));
  if (cosw < -1 || cosw > 1) return null;
  const w = Math.acos(cosw) / rad / 360, toMs = (j: number) => (j - 2440587.5) * 86400000;
  return { rise: toMs(Jt - w), set: toMs(Jt + w), noon: toMs(Jt) };
}

/** Great-circle distance in km. */
export function kmBetween(a: LatLon, b: LatLon): number {
  const R = 6371, r = Math.PI / 180, dLat = (b[0] - a[0]) * r, dLon = (b[1] - a[1]) * r;
  const h = Math.sin(dLat / 2) ** 2 + Math.cos(a[0] * r) * Math.cos(b[0] * r) * Math.sin(dLon / 2) ** 2;
  return 2 * R * Math.asin(Math.sqrt(h));
}

export const fmtLL = (lat: number, lon: number) =>
  `${Math.abs(lat).toFixed(3)}° ${lat >= 0 ? "N" : "S"}, ${Math.abs(lon).toFixed(3)}° ${lon >= 0 ? "E" : "W"}`;

/** Is it dark at the van now (sunset → sunrise), and when does that next change? null without a position. */
export function wxSun(lat: number | null | undefined, lon: number | null | undefined, now = Date.now()): { dark: boolean; next: number } | null {
  const st = lat != null && lon != null ? sunTimes(+lat, +lon, new Date(now)) : null;
  if (!st || lat == null || lon == null) return null;
  const dark = now < st.rise || now >= st.set;
  const next = now < st.rise ? st.rise : now < st.set ? st.set : (sunTimes(+lat, +lon, new Date(now + 86400000))?.rise ?? st.rise + 86400000);
  return { dark, next };
}

export type MapMode = "auto" | "dark" | "light";
export const WX_MODES: readonly MapMode[] = ["auto", "dark", "light"];

/** The dark basemap: Dark / Light as set; Auto = dark from sunset to sunrise at the van, else the display's night mode. */
export function darkMapOn(mode: MapMode, sun: { dark: boolean } | null, night: boolean): boolean {
  return mode === "dark" ? true : mode === "light" ? false : sun ? sun.dark : night;
}

export function mapModeTitle(mode: MapMode, on: boolean): string {
  return mode === "auto" ? `Auto: ${on ? "dark (sunset to sunrise)" : "light (sunrise to sunset)"} — tap for Dark` :
    mode === "dark" ? "Always dark — tap for Light" : "Always light — tap for Auto";
}
