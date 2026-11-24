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
const STATS: readonly (readonly [string, string, string, string, Kind])[] = [
  ["panel", "--c-solar", "victron.pv.power", "Solar", "w"],
  ["engine", "--c-alt", "energy.alternator.power", "Alternator", "w"],
  ["plug", "--c-shore", "victron.ac.shore", "Shore", "w"],
  ["dcload", "--c-load", "energy.dc.load", "DC loads", "w"],
  ["outlet", "--c-load", "victron.ac.load", "AC loads", "w"],
  ["thermo", "--ink-2", "victron.battery.temp", "Battery temp", "t"],
];

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

const StatTile = memo(function StatTile({ ic, color, k, label, kind }: { ic: string; color: string; k: string; label: string; kind: Kind }) {
  const idle = useSelect([k], g => statIdle(kind, g<number | null>(k)));
  const { num, unit } = useStatText(k, kind);
  return (
    <div className={`stat ${idle ? "idle" : ""}`} data-k={k}>
      <div className="stat-ic" style={{ color: `var(${color})` }}><Icon name={ic} /></div>
      <div><div className="stat-v"><span ref={num} /><small ref={unit} hidden /></div><div className="stat-l">{label}</div></div>
    </div>
  );
});

export const StatGrid = memo(function StatGrid() {
  return (
    <div className="card stat-grid" id="energy-stats">
      {STATS.map(([ic, c, k, l, kind]) => <StatTile key={k} ic={ic} color={c} k={k} label={l} kind={kind} />)}
    </div>
  );
});
