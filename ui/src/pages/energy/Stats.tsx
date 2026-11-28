// The six reading tiles (app.js renderEnergy): solar, alternator, shore, DC loads, AC loads, battery temperature.
// A tile re-renders only when it turns idle / active; its number is written straight to the DOM and tweens to each
// new reading (the unit follows, so 980 W → 1.2 kW reads right on every frame).
import { memo, useLayoutEffect, useRef } from "react";
import { Icon } from "../../components/Icon";
import { fmtW, tC } from "../../lib/format";
import { reducedMotion, tween } from "../../lib/tween";
import { useSelect } from "../../state/hooks";
import { store } from "../../state/store";
import { splitUnit } from "./bank";

type Kind = "w" | "t";
/** Volts and amps under a tile (owner, 2026-11-27): solar on the panel side, shore in, DC loads, AC loads. */
type Sub = readonly [string, string] | null;
const STATS: readonly (readonly [string, string, string, string, Kind, Sub])[] = [
  ["panel", "--c-solar", "victron.pv.power", "Solar", "w", ["victron.pv.panel_v", "victron.pv.panel_a"]],
  ["engine", "--c-alt", "energy.alternator.power", "Alternator", "w", null],
  ["plug", "--c-shore", "victron.ac.shore", "Shore", "w", ["victron.ac.shore_v", "victron.ac.shore_a"]],
  ["dcload", "--c-load", "energy.dc.load", "DC loads", "w", ["victron.battery.voltage", "energy.dc.load_a"]],
  ["outlet", "--c-load", "victron.ac.load", "AC loads", "w", ["victron.ac.load_v", "victron.ac.load_a"]],
  ["thermo", "--ink-2", "victron.battery.temp", "Battery temp", "t", null],
];

const finite = (x: unknown): number | null => (typeof x === "number" && Number.isFinite(x) ? x : null);
/** "13.3 V · 9.8 A", "120 V · 1.8 A"; a missing one reads "—"; nothing when both are missing (no such device). */
export function subText(v: unknown, a: unknown): string {
  const V = finite(v), A = finite(a);
  if (V == null && A == null) return "";
  const vs = V == null ? "—" : Math.abs(V) >= 100 ? String(Math.round(V)) : V.toFixed(1);
  const am = A == null ? "—" : A.toFixed(1);
  return `${vs} V · ${am} A`;
}

const SubLine = memo(function SubLine({ keys }: { keys: readonly [string, string] }) {
  const t = useSelect(keys, g => subText(g(keys[0]), g(keys[1])));
  return t ? <span className="stat-sub">{t}</span> : null;
});

/** Display value (W, or the temperature in the display unit) and its text. */
const shown = (kind: Kind, raw: number | null | undefined) => (kind === "t" ? tC(raw) : raw ?? null);
const text = (kind: Kind, v: number): [string, string] => (kind === "t" ? [`${Math.round(v)}°`, ""] : splitUnit(fmtW(v)));
/** app.js: a tile is idle with no reading or under 3 W (the temperature tile only without a reading). */
export const statIdle = (kind: Kind, raw: number | null | undefined) => (kind === "t" ? raw == null : !raw || raw < 3);

const TWEEN_MS = 450;

/** Writes `key`'s live value into the number / unit elements, tweening between readings. */
function useStatText(key: string, kind: Kind) {
  const num = useRef<HTMLSpanElement>(null), unit = useRef<HTMLElement>(null);
  useLayoutEffect(() => {
    const n = num.current!, u = unit.current!;
    let cur: number | null = null, cancel: (() => void) | null = null;
    const write = (v: number | null) => {
      cur = v;
      const [a, b] = v == null ? ["—", ""] : text(kind, v);
      if (n.textContent !== a) n.textContent = a;
      if (u.textContent !== b) u.textContent = b;
      if (u.hidden !== !b) u.hidden = !b;
    };
    const set = (animate: boolean) => {
      const v = shown(kind, store.get<number | null>(key));
      cancel?.(); cancel = null;
      if (!animate || v == null || cur == null || reducedMotion()) write(v);
      else cancel = tween(cur, v, TWEEN_MS, write);
    };
    set(false);
    const off = store.subscribe(key, () => set(true));
    return () => { off(); cancel?.(); };
  }, [key, kind]);
  return { num, unit };
}

const StatTile = memo(function StatTile({ ic, color, k, label, kind, sub }: { ic: string; color: string; k: string; label: string; kind: Kind; sub: Sub }) {
  const idle = useSelect([k], g => statIdle(kind, g<number | null>(k)));
  const { num, unit } = useStatText(k, kind);
  return (
    <div className={`stat ${idle ? "idle" : ""}`} data-k={k}>
      <div className="stat-ic" style={{ color: `var(${color})` }}><Icon name={ic} /></div>
      <div><div className="stat-v"><span ref={num} /><small ref={unit} hidden /></div><div className="stat-l"><span>{label}</span>{sub && <SubLine keys={sub} />}</div></div>
    </div>
  );
});

export const StatGrid = memo(function StatGrid() {
  return (
    <div className="card stat-grid" id="energy-stats">
      {STATS.map(([ic, c, k, l, kind, sub]) => <StatTile key={k} ic={ic} color={c} k={k} label={l} kind={kind} sub={sub} />)}
    </div>
  );
});
