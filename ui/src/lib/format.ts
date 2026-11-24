// Units and number formatting, ported from app.js (same output, character for character).
import { prefs } from "./prefs";

/** °F by default (US van); tap the clock to switch (reloads, as app.js). */
export const UNIT: "F" | "C" = prefs.get("vp.temp", "F") === "C" ? "C" : "F";
export const tC = (c: number | null | undefined): number | null => (c == null ? null : UNIT === "F" ? c * 9 / 5 + 32 : c);
export const fmtT = (c: number | null | undefined): string => (c == null ? "—" : `${Math.round(tC(c)!)}°`);
export const F2C = (f: number) => (f - 32) / 1.8;

export const fmtW = (w: number | null | undefined): string =>
  w == null ? "—" : Math.abs(w) >= 1000 ? `${(w / 1000).toFixed(1)} kW` : `${Math.round(w)} W`;
export const num = (v: unknown, d = 0): string =>
  v == null || isNaN(v as number) ? "—" : (+(v as number)).toFixed(d);
export const fmtTTG = (s: number | null | undefined): string => {
  if (s == null || s <= 0) return "—";
  const h = Math.floor(s / 3600);
  return h >= 48 ? `${Math.round(h / 24)} d` : `${h} h ${Math.round((s % 3600) / 60)} m`;
};
/** Charge % from Ah: one decimal below 100 %, none at or above (99.96 → "100", not "100.0"). */
export const fmtPct = (p: number | null | undefined): string =>
  p == null ? "—" : p >= 99.95 ? `${Math.round(p)}` : p.toFixed(1);

// Battery temps (°C): the hottest, unless any reading is below 40 °F; then the coldest (LiFePO4 won't charge cold).
const BATT_COLD_C = (40 - 32) * 5 / 9;
export const battT = (vals: (number | null | undefined)[]): { c: number; cold: boolean } | null => {
  const v = vals.filter((x): x is number => x != null);
  if (!v.length) return null;
  const lo = Math.min(...v);
  return lo < BATT_COLD_C ? { c: lo, cold: true } : { c: Math.max(...v), cold: false };
};

export function socColor(soc: number | null | undefined): string {
  if (soc == null) return "var(--track)";
  if (soc < 15) return "var(--critical)";
  if (soc < 30) return "var(--warning)";
  return "var(--c-batt)";
}

export const ago = (t: number): string => {
  const s = Date.now() / 1000 - t;
  return s < 60 ? "just now" : s < 3600 ? `${Math.round(s / 60)} min ago` : s < 86400 ? `${Math.round(s / 3600)} h ago` : `${Math.round(s / 86400)} d ago`;
};

export const mbps = (v: number | null | undefined): string | number =>
  v == null ? "—" : v >= 100 ? Math.round(v) : (+v).toFixed(1);

export const cap = (s: string) => s ? s[0]!.toUpperCase() + s.slice(1) : s;
