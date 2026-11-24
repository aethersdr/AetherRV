// Cabin dial geometry, ported from web/charts.js dial()/dialGeom() and app.js dialValueAt (same numbers, so the
// settled drawing is pixel-identical). 270° sweep from 135° (0° = +x, clockwise), value + setpoint knob.

export const DIAL_SIZE = 440;
export const DIAL_STROKE = 24;
export const A0 = 135;
export const SWEEP = 270;
/** Knob-centre radius (charts.js dialGeom). */
export const DIAL_R = (DIAL_SIZE - DIAL_STROKE) / 2 - 14;
const C = DIAL_SIZE / 2;

/** The dial's range in the display unit (app.js: 40–90 °F, 5–32 °C). */
export const dialRange = (unit: "F" | "C"): readonly [number, number] => (unit === "F" ? [40, 90] : [5, 32]);

/** Angle (deg) of a value, clamped to the range. */
export const angOf = (v: number, lo: number, hi: number) => A0 + SWEEP * (Math.max(lo, Math.min(hi, v)) - lo) / (hi - lo);

/** Point at angle `a` (deg) and radius `rr` around the dial centre (SVG units). */
export function pt(a: number, rr = DIAL_R): [number, number] {
  return [C + rr * Math.cos(a * Math.PI / 180), C + rr * Math.sin(a * Math.PI / 180)];
}

/** Arc path from → to (deg) on the knob radius, as charts.js wrote it. */
export function arc(from: number, to: number): string {
  const [x1, y1] = pt(from), [x2, y2] = pt(to);
  return `M${x1.toFixed(1)},${y1.toFixed(1)}A${DIAL_R},${DIAL_R} 0 ${to - from > 180 ? 1 : 0} 1 ${x2.toFixed(1)},${y2.toFixed(1)}`;
}

export interface Tick { x1: number; y1: number; x2: number; y2: number; major: boolean }
/** One tick per degree, a longer one every 5. */
export function ticks(lo: number, hi: number): Tick[] {
  const out: Tick[] = [];
  for (let v = lo; v <= hi; v += 1) {
    const a = angOf(v, lo, hi), major = v % 5 === 0;
    const [x1, y1] = pt(a, DIAL_R + DIAL_STROKE / 2 + 6), [x2, y2] = pt(a, DIAL_R + DIAL_STROKE / 2 + (major ? 18 : 11));
    out.push({ x1, y1, x2, y2, major });
  }
  return out;
}

/**
 * The whole-degree value under a pointer at (dx, dy) from the dial centre (any scale: only the angle counts).
 * The bottom gap snaps to the nearer end (app.js dialValueAt).
 */
export function valueAt(dx: number, dy: number, lo: number, hi: number): number {
  let rel = ((Math.atan2(dy, dx) * 180 / Math.PI) - A0 + 720) % 360;
  if (rel > SWEEP) rel = rel > SWEEP + (360 - SWEEP) / 2 ? 0 : SWEEP;
  return Math.round(lo + (hi - lo) * rel / SWEEP);
}

/** Is (dx, dy) (dial units, from the centre) within 60 units of the knob at value `cur`? (must grab the knob) */
export function onKnob(dx: number, dy: number, cur: number, lo: number, hi: number): boolean {
  const a = angOf(cur, lo, hi) * Math.PI / 180;
  return Math.hypot(dx - DIAL_R * Math.cos(a), dy - DIAL_R * Math.sin(a)) <= 60;
}
