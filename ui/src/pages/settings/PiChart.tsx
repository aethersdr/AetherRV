// Settings → Device: the Pi's own load (owner, 2026-11-27): CPU, GPU and memory use and the SoC temperature, from
// history (`pi.*`, published by the settings service once per history interval as the mean over it). Two lanes on one
// time window: the three loads share a 0–100 % axis, the temperature has its own scale (one chart with two y-axes
// would misread). A figure the Pi doesn't provide (no GPU counters, no SoC sensor) is left out, legend included.
import { memo, useMemo, useState } from "react";
import { TimeChart, type Point, type Series } from "../../charts/TimeChart";
import { tC } from "../../lib/format";
import { useHistory } from "../../state/history";
import { useSelect } from "../../state/hooks";

const LOAD = [["pi.cpu_pct", "CPU", "var(--c-cpu)"], ["pi.gpu_pct", "GPU", "var(--c-gpu)"], ["pi.mem_pct", "Memory", "var(--c-mem)"]] as const;
const TEMP = "pi.soc_c";
const KEYS = [...LOAD.map(l => l[0]), TEMP];
const SPANS = [[21600, "6 h"], [86400, "24 h"], [604800, "7 d"]] as const;
const pctFmt = (v: number) => `${Math.round(v)}%`;
const degFmt = (v: number) => `${Math.round(v)}°`;

export const PiChart = memo(function PiChart() {
  const [span, setSpan] = useState(86400);
  const h = useHistory(KEYS, span, 360);
  const live = useSelect(KEYS, g => KEYS.map(k => g<number | null>(k) ?? null));
  const load: Series[] = useMemo(() => LOAD.map(([k, name, color]) => ({ name, color, points: h?.[k] })), [h]);
  const temp: Series = useMemo(() => ({
    name: "SoC", color: "var(--c-soc)", points: (h?.[TEMP] ?? []).map(p => [p[0], tC(p[1])!] as Point),
  }), [h]);
  const has = (s: Series, i: number) => (s.points?.length ?? 0) > 0 || live[i] != null;
  const shown = load.filter((s, i) => has(s, i));
  const hasTemp = has(temp, 3);
  const pts = [...load, temp].flatMap(s => s.points ?? []);
  const win = pts.length ? { t0: Math.min(...pts.map(p => p[0])), t1: Math.max(...pts.map(p => p[0])) } : {};
  // the temperature lane is short: ticks every 10° (20° over a wide range), not niceTicks' five
  const tv = (temp.points ?? []).map(p => p[1]);
  const tAxis = tv.length ? (() => {
    const step = Math.max(...tv) - Math.min(...tv) > 30 ? 20 : 10;
    const lo = Math.floor(Math.min(...tv) / step) * step, hi = Math.max(lo + step, Math.ceil(Math.max(...tv) / step) * step);
    const ticks: number[] = [];
    for (let v = lo; v <= hi; v += step) ticks.push(v);
    return { yMin: lo, yMax: hi, yTicks: ticks };
  })() : {};
  const now = (i: number, f: (v: number) => string) => (live[i] == null ? "—" : f(i === 3 ? tC(live[i])! : live[i]!));
  return (
    <div className="pi-chart" id="pi-chart">
      <div className="set-h sub"><h2>Pi load</h2>
        <div className="seg range" id="range-pi">
          {SPANS.map(([s, t]) => <button key={s} data-s={s} className={s === span ? "on" : undefined} onClick={() => setSpan(s)}>{t}</button>)}</div></div>
      <div className="legend" id="legend-pi">
        {LOAD.map(([k, name, color], i) => has(load[i]!, i) &&
          <span key={k} data-k={k}><i style={{ background: color }} />{name}<b>{now(i, pctFmt)}</b></span>)}
        {hasTemp && <span data-k={TEMP}><i style={{ background: "var(--c-soc)" }} />SoC temperature<b>{now(3, degFmt)}</b></span>}
      </div>
      <TimeChart id="chart-pi" series={shown} yMin={0} yMax={100} yTicks={[0, 50, 100]} {...win} fmt={pctFmt} />
      {hasTemp && <TimeChart id="chart-pi-temp" className="pi-temp" series={[temp]} zero={false} {...tAxis} {...win} fmt={degFmt} />}
    </div>
  );
});
