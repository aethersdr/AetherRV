// Energy page logic, ported from app.js (renderBattCard / bankScene / updateBankScene / renderPowerControls /
// sumSeries). Pure functions: the components read the store and call these.
import { battT, fmtPct, fmtT, fmtTTG, fmtW, num } from "../../lib/format";

/** One EPOCH battery as the batteries-ble service reports it (`batteries.ble`). */
export interface Ble {
  name: string; ok?: boolean; soc?: number | null; voltage?: number | null; current?: number | null;
  remaining_ah?: number | null; cycles?: number | null; temps?: (number | null)[] | null; cells?: number[] | null;
  error?: string | null; charge_fet?: boolean; discharge_fet?: boolean; age?: number | null;
}

export const BATT_AH = 460;                  // EPOCH B12460A nominal capacity per battery

/**
 * Charge of one battery: available Ah ÷ capacity (finer than the BMS's own SoC, which sits at 100 % for a long
 * time); the BMS SoC when it doesn't report remaining Ah; null without data.
 */
export function battPct(b: Ble | undefined): number | null {
  if (!b || !b.ok) return null;
  return b.remaining_ah != null ? b.remaining_ah / BATT_AH * 100 : b.soc ?? null;
}

/** The headline: available Ah ÷ capacity of the batteries reporting it; the shunt's SoC as the fallback. */
export function bankPct(ble: readonly Ble[], shuntSoc: number | null | undefined): number | null {
  const withAh = ble.filter(b => b.ok && b.remaining_ah != null);
  if (withAh.length) return withAh.reduce((a, b) => a + b.remaining_ah!, 0) / (BATT_AH * withAh.length) * 100;
  return shuntSoc ?? null;
}

/** "Charging" / "Discharging" / "Standby" from the bank power (W): [label, icon, class]. */
export function bankState(bp: number | null | undefined): readonly [string, string, string] {
  return bp == null ? ["", "", ""] : bp > 5 ? ["Charging", "bolt", "chg"] : bp < -5 ? ["Discharging", "battery", "dis"] : ["Standby", "battery", ""];
}

export interface HeadView { pct: number | null; pctTitle: string; st: readonly [string, string, string]; line1: string; line2: string }
/** The time to go (derive: `energy.battery.to_go_*`, the batteries' own Ah over the averaged bank current), shown only
 *  when it agrees with the Charging / Discharging label: " · 2 h 42 m to full" or " · 3 d to go"; nothing at rest. */
export function toGoText(get: <T>(k: string) => T | undefined): string {
  const s = get<number | null>("energy.battery.to_go_s"), dir = get<string | null>("energy.battery.to_go_dir");
  const st = bankState(get<number | null>("victron.battery.power"))[2];
  if (!s) return "";
  return dir === "full" && st === "chg" ? ` · ${fmtTTG(s)} to full` : dir === "empty" && st === "dis" ? ` · ${fmtTTG(s)} to go` : "";
}

export function headView(get: <T>(k: string) => T | undefined): HeadView {
  const ble = get<Ble[]>("batteries.ble") || [], ok = ble.filter(b => b.ok);
  const withAh = ok.filter(b => b.remaining_ah != null);
  const rem = ok.length ? ok.reduce((a, b) => a + (b.remaining_ah || 0), 0) : null;
  return {
    pct: bankPct(ble, get<number | null>("victron.battery.soc")),
    pctTitle: withAh.length ? `${withAh.length} batteries: available Ah ÷ ${BATT_AH * withAh.length} Ah` : "Lynx shunt SoC",
    st: bankState(get<number | null>("victron.battery.power")),
    line1: `${num(get("victron.battery.voltage"), 2)} V · ${num(get("victron.battery.current"), 1)} A${toGoText(get)}`,
    line2: `${rem == null ? "" : `${Math.round(rem)} / ${BATT_AH * ble.length} Ah · `}${ok.length} of ${ble.length} batteries via Bluetooth`,
  };
}

/** The eight cell / temperature tiles under the scene: [label, value, class, title]. */
export type CellTile = readonly [string, string, string, string];
export function cellTiles(ble: readonly Ble[], shuntTemp: number | null | undefined): CellTile[] {
  const ok = ble.filter(b => b.ok);
  const cells = ok.flatMap(b => b.cells || []);
  const avg = cells.length ? cells.reduce((a, c) => a + c, 0) / cells.length : null;
  const lo = cells.length ? Math.min(...cells) : null, hi = cells.length ? Math.max(...cells) : null;
  const v3 = (v: number | null) => v == null ? "—" : `${v.toFixed(3)} V`;
  const tblk = (label: string, i0: number, i1: number): CellTile => {
    const t = battT(ok.flatMap(b => (b.temps || []).slice(i0, i1)));
    return [`${label}${t?.cold ? " · min" : ""}`, t ? fmtT(t.c) : "—", t?.cold ? "t-cold" : "", t?.cold ? "Coldest reading (below 40 °F)" : "Hottest reading"];
  };
  return [
    ["VAVG", v3(avg), "", ""], ["VDIFF", v3(hi == null ? null : hi - lo!), hi != null && hi - lo! > .05 ? "warn" : "", ""],
    ["VLO", v3(lo), "", ""], ["VHI", v3(hi), "", ""],
    tblk("Cells T1/T2", 0, 2), tblk("Ambient", 2, 3), tblk("MOS", 3, 4),
    ["Battery (shunt)", fmtT(shuntTemp), "", ""],
  ];
}

/** One row of the per-battery table, as display strings (never `age`, so a fresher read alone re-renders nothing). */
export type RowView =
  | { ok: true; name: string; soc: string; socColor: string; volts: string; amps: string; power: string; cycles: string;
      ah: string; tempTitle: string; temp: string; cold: boolean; cfet: boolean; dfet: boolean }
  | { ok: false; name: string; msg: string };
export function rowView(b: Ble | undefined, socColor: (s: number | null | undefined) => string): RowView | null {
  if (!b) return null;
  if (!b.ok) return { ok: false, name: b.name, msg: b.error ? "no data · reconnecting" : "connecting…" };
  const t = battT((b.temps || []).slice(0, 2));
  const tp = b.temps || [];
  return {
    ok: true, name: b.name, soc: `${b.soc ?? "—"}%`, socColor: socColor(b.soc),
    volts: `${(+(b.voltage as number)).toFixed(2)} V`,
    amps: b.current == null ? "—" : `${(+b.current).toFixed(1)} A`,
    power: b.current == null ? "—" : fmtW((b.voltage as number) * b.current),
    cycles: String(b.cycles ?? "—"),
    ah: b.remaining_ah == null ? "—" : (+b.remaining_ah).toFixed(1),
    tempTitle: tp.length >= 4 ? `T1 ${fmtT(tp[0])} · T2 ${fmtT(tp[1])} · ambient ${fmtT(tp[2])} · MOS ${fmtT(tp[3])}` : "",
    temp: t ? fmtT(t.c) : "—", cold: !!t?.cold, cfet: !!b.charge_fet, dfet: !!b.discharge_fet,
  };
}

// ── 3D battery-bank scene geometry (app.js BS / discSide) ──
// Each battery is a stack of four glowing discs (one per 25 %) on a shared platform, wired in parallel by animated
// + / − buses. 2 px seams: one cell in four quarters, not a stack of drums.
export const BS = { w: 800, h: 300, rx: 56, ry: 13, dh: 28, step: 30, yb0: 202 } as const;
export const discSide = (cx: number, yt: number, yb: number) =>
  `M${cx - BS.rx} ${yt} V${yb} A${BS.rx} ${BS.ry} 0 0 0 ${cx + BS.rx} ${yb} V${yt} Z`;
export const stackX = (i: number, n: number) => (i + .5) * BS.w / n;

export type Hue = "blue" | "amber" | "red";
export const hueOf = (soc: number | null): Hue => soc == null ? "blue" : soc < 15 ? "red" : soc < 30 ? "amber" : "blue";

/** The lit part of quarter k (0 = bottom) at a charge of `soc` %: the clip rect's y and height (as strings). */
export function discClip(soc: number | null, k: number): { f: number; y: string; height: string; surfaceY: string } {
  const f = soc == null ? 0 : Math.max(0, Math.min(1, (soc - 25 * k) / 25));
  const yb = BS.yb0 - k * BS.step, yt = yb - BS.dh;
  const top = f >= 1 ? yt - BS.ry - 2 : yb - f * BS.dh;
  return {
    f,
    y: f > 0 ? top.toFixed(1) : String(yb + BS.ry + 2),
    height: f > 0 ? (yb + BS.ry + 2 - top).toFixed(1) : "0",
    surfaceY: (yb - f * BS.dh).toFixed(1),
  };
}

/** Bank current → the buses' flow direction and dash speed. */
export const flowOf = (a: number | null | undefined): "" | "chg" | "dis" => a == null ? "" : a > 0.5 ? "chg" : a < -0.5 ? "dis" : "";
/** Dash cycle (s): faster with more current, in 0.25 s steps. (app.js wrote it to 0.01 s on every reading; a change
 *  restyles the whole scene and makes the dashes jump, so it now changes only when the speed visibly does.) */
export const flowDur = (a: number | null | undefined) =>
  `${(Math.round(Math.max(.5, Math.min(4, 4 - Math.abs(a || 0) / 12)) * 4) / 4).toFixed(2)}s`;

// ── Per-battery current history for the sparklines (2 min, client-side) ──
export type BankHist = Map<string, [number, number][]>;
export const SPARK_S = 120;

/** Add one reading per battery (stamped with when the battery was read, not when it arrived). */
export function recordBank(hist: BankHist, batts: readonly Ble[], now: number): void {
  for (const b of batts) {
    let h = hist.get(b.name);
    if (!h) hist.set(b.name, (h = []));
    if (b.ok && b.current != null) {
      const ts = Math.round(now - (b.age || 0));
      if (!h.length || h[h.length - 1]![0] !== ts) h.push([ts, +b.current]);
    }
    while (h.length && h[0]![0] < now - SPARK_S) h.shift();
  }
}

/** Sparkline of one battery's current, scaled to its own range (≥ 0.4 A so small swings show). */
export function sparkPaths(h: readonly (readonly [number, number])[], cx: number, now: number): { line: string; area: string } | null {
  const pts0 = h.filter(p => p[0] >= now - SPARK_S);
  if (pts0.length < 2) return null;
  const x0 = cx - 78, x1 = cx + 34, yTop = BS.h - 32, yBot = BS.h - 6;
  let lo = Math.min(...pts0.map(p => p[1])), hi = Math.max(...pts0.map(p => p[1]));
  if (hi - lo < .4) { const m = (hi + lo) / 2; lo = m - .2; hi = m + .2; }
  const pts = pts0.map(([t, a]) => [x0 + (x1 - x0) * (1 - (now - t) / SPARK_S), yBot - (a - lo) / (hi - lo) * (yBot - yTop)] as const);
  const line = pts.map(([x, y], j) => `${j ? "L" : "M"}${x.toFixed(1)} ${y.toFixed(1)}`).join("");
  return { line, area: `${line}L${pts[pts.length - 1]![0].toFixed(1)} ${yBot}L${pts[0]![0].toFixed(1)} ${yBot}Z` };
}

export const pctText = (p: number | null) => p == null ? "—" : `${fmtPct(p)}%`;

// ── Power chart ──
export type Pt = readonly [number, number];
/** Sum two bucketed series point by point, matching on the nearest timestamp. */
export function sumSeries(a: readonly Pt[] = [], b: readonly Pt[] = []): Pt[] {
  if (!b.length) return [...a];
  let j = 0;
  return a.map(([t, v]) => {
    while (j + 1 < b.length && Math.abs(b[j + 1]![0] - t) <= Math.abs(b[j]![0] - t)) j++;
    return [t, v + (b[j] ? b[j]![1] : 0)] as const;
  });
}

// ── Inverter & solar ──
export const VEBUS_STATE: Record<number, string> = { 0: "Off", 1: "Low power", 2: "Fault", 3: "Bulk", 4: "Absorption", 5: "Float",
  6: "Storage", 7: "Equalize", 8: "Passthru", 9: "Inverting", 10: "Power assist", 11: "Power supply", 244: "Sustain", 252: "External control" };
export const MPPT_STATE: Record<number, string> = { 0: "Off", 2: "Fault", 3: "Bulk", 4: "Absorption", 5: "Float", 6: "Storage",
  7: "Equalize", 252: "External control" };
export const MODES = [[3, "On"], [1, "Charger only"], [2, "Inverter only"], [4, "Off"]] as const;
/** What each MultiPlus mode does, asked before it is sent (app.js, verbatim). */
export const MODE_WARN: Record<number, string> = {
  3: "Turns the MultiPlus on: it will invert to the AC outlets and charge from shore when connected (~25 W idle draw).",
  2: "Inverter only: AC outlets powered from the batteries; shore power is ignored.",
  1: "Charger only: charges from shore; AC outlets are NOT powered from the batteries.",
  4: "Turns the MultiPlus off: AC outlets go dead and it will not charge from shore.",
};
export const MPPT_MODES = [[1, "On"], [4, "Off"]] as const;
export const LIM_MIN = 3, LIM_MAX = 50;
/** One − / + step of the shore limit (A), from what's shown; clamped to the MultiPlus' 3–50 A. */
export const nudgeLimit = (cur: number, dir: 1 | -1) => Math.min(LIM_MAX, Math.max(LIM_MIN, Math.round(cur) + dir));

/** A stat tile's text: "1.2 kW" → ["1.2", "kW"], "51°" → ["51°", ""]. */
export const splitUnit = (s: string): [string, string] => { const i = s.indexOf(" "); return i < 0 ? [s, ""] : [s.slice(0, i), s.slice(i + 1)]; };

// ── WebGL bank (bank3d.ts): layout, camera and animation maths, kept free of three so they test without WebGL ──
/** A charge reading eases in when it moves the level visibly (both scenes use these). */
export const LEVEL_MS = 900, LEVEL_MIN = 0.5;
/** Scene units: one EPOCH block (w × h × d), the gap between blocks, the + / − bus heights and depths. */
export const B3 = { w: 1, h: 1.3, d: 0.72, lid: 0.1, gap: 0.55, fillPad: 0.04, term: 0.28, busP: 1.9, busN: 1.74, zP: -0.12, zN: 0.12,
  platZ: 0.95, fov: 20, ty: 0.6, zoom: 1.1, dash: 0.24 } as const;
export const b3Width = (n: number) => n * B3.w + Math.max(0, n - 1) * B3.gap;
export const b3X = (i: number, n: number) => (i - (n - 1) / 2) * (B3.w + B3.gap);
/** Height of the fill at a charge of `pct` % (0 … the block's inner height). */
export const b3FillH = (pct: number | null) => (pct == null ? 0 : Math.max(0, Math.min(100, pct)) / 100 * (B3.h - B3.lid - 2 * B3.fillPad));

/** Camera distance at which a span `w` wide and `h` high fits the view (vertical fov in degrees). */
export function fitDistance(w: number, h: number, aspect: number, fovDeg: number): number {
  const v = fovDeg * Math.PI / 180, hz = 2 * Math.atan(Math.tan(v / 2) * aspect);
  return Math.max(w / 2 / Math.tan(hz / 2), h / 2 / Math.tan(v / 2));
}

/** Drag to orbit: azimuth / elevation (radians) around the bank, clamped so the camera never goes under or behind it;
 *  `idleMs` after the last touch it eases home with time constant `tau` (s). */
export const ORBIT = { az: 0.15, el: 0.26, azMax: 0.75, elMin: 0.05, elMax: 1.0, perPx: 0.008, idleMs: 3000, tau: 0.28 } as const;
export const clampOrbit = (az: number, el: number): [number, number] =>
  [Math.max(ORBIT.az - ORBIT.azMax, Math.min(ORBIT.az + ORBIT.azMax, az)), Math.max(ORBIT.elMin, Math.min(ORBIT.elMax, el))];
/** Frame-rate independent exponential approach of v to target over dt seconds. */
export const approach = (v: number, target: number, dt: number, tau: number) => target + (v - target) * Math.exp(-dt / tau);
/** Camera position for an orbit (target at the origin of `ty`). */
export const orbitPos = (az: number, el: number, dist: number, ty: number): [number, number, number] =>
  [dist * Math.sin(az) * Math.cos(el), ty + dist * Math.sin(el), dist * Math.cos(az) * Math.cos(el)];

/** Dash texture offset after dt s: one dash period per `durS` s; dashes run with the current (+1 charging, −1 discharging). */
export const flowAdvance = (offset: number, dir: 1 | -1, dtS: number, durS: number) => (((offset - dir * dtS / durS) % 1) + 1) % 1;
/** flowDur's "2.00s" as seconds. */
export const durSeconds = (s: string) => parseFloat(s) || 2;
