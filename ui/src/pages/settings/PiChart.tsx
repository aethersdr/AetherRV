// Settings → Device: the Pi's own load (owner, 2026-11-27): CPU, GPU and memory use, and its temperatures (the SoC,
// the RP1 I/O chip, the NVMe SSD), from history (`pi.*`, published by the settings service once per history interval
// as the mean over it). Two lanes on one time window: the three loads share a 0–100 % axis, the temperatures have
// their own scale (one chart with two y-axes would misread), side by side, each with its legend and live values. A
// figure the Pi doesn't provide (no GPU counters, no such sensor) is left out, legend included.
import { memo, useMemo, useState } from "react";
import { TimeChart, type Point, type Series } from "../../charts/TimeChart";
import { tC } from "../../lib/format";
import { useHistory } from "../../state/history";
import { useSelect } from "../../state/hooks";

type Line = readonly [key: string, name: string, color: string];
const LOAD: readonly Line[] = [["pi.cpu_pct", "CPU", "var(--c-cpu)"], ["pi.gpu_pct", "GPU", "var(--c-gpu)"], ["pi.mem_pct", "Memory", "var(--c-mem)"]];
const TEMPS: readonly Line[] = [["pi.soc_c", "SoC", "var(--c-soc)"], ["pi.rp1_c", "RP1 chip", "var(--c-rp1)"], ["pi.nvme_c", "NVMe SSD", "var(--c-nvme)"]];
const KEYS = [...LOAD, ...TEMPS].map(l => l[0]);
const SPANS = [[21600, "6 h"], [86400, "24 h"], [604800, "7 d"]] as const;
const pctFmt = (v: number) => `${Math.round(v)}%`;
const degFmt = (v: number) => `${Math.round(v)}°`;

/** Ticks every 10° (20° over a wide range) for the short temperature lane, not niceTicks' five. */
function tempAxis(vals: number[]) {
  if (!vals.length) return {};
  const lo0 = Math.min(...vals), hi0 = Math.max(...vals);
  const step = hi0 - lo0 > 30 ? 20 : 10;
  const lo = Math.floor(lo0 / step) * step, hi = Math.max(lo + step, Math.ceil(hi0 / step) * step);
  const yTicks: number[] = [];
  for (let v = lo; v <= hi; v += step) yTicks.push(v);
  return { yMin: lo, yMax: hi, yTicks };
}

const Legend = ({ id, lines, series, live, fmt }: { id: string; lines: readonly Line[]; series: Series[]; live: (number | null)[]; fmt: (v: number) => string }) => (
  <div className="legend" id={id}>
    {lines.map(([k, name, color], i) => ((series[i]!.points?.length ?? 0) > 0 || live[i] != null) &&
      <span key={k} data-k={k}><i style={{ background: color }} />{name}<b>{live[i] == null ? "—" : fmt(live[i]!)}</b></span>)}
  </div>
);

export const PiChart = memo(function PiChart() {
  const [span, setSpan] = useState(86400);
  const h = useHistory(KEYS, span, 360);
  const live = useSelect(KEYS, g => KEYS.map(k => g<number | null>(k) ?? null));
  const liveLoad = live.slice(0, LOAD.length), liveTemp = live.slice(LOAD.length).map(v => (v == null ? null : tC(v)!));
  const load: Series[] = useMemo(() => LOAD.map(([k, name, color]) => ({ name, color, points: h?.[k] })), [h]);
  const temps: Series[] = useMemo(() => TEMPS.map(([k, name, color]) => ({
    name, color, points: (h?.[k] ?? []).map(p => [p[0], tC(p[1])!] as Point),
  })), [h]);
  const has = (s: Series, v: number | null) => (s.points?.length ?? 0) > 0 || v != null;
  const shownLoad = load.filter((s, i) => has(s, liveLoad[i]!));
  const shownTemps = temps.filter((s, i) => has(s, liveTemp[i]!));
  const pts = [...load, ...temps].flatMap(s => s.points ?? []);
  const win = pts.length ? { t0: Math.min(...pts.map(p => p[0])), t1: Math.max(...pts.map(p => p[0])) } : {};
  const tAxis = tempAxis(temps.flatMap(s => (s.points ?? []).map(p => p[1])));
  return (
    <div className="pi-chart" id="pi-chart">
      <div className="set-h sub"><h2>Pi load</h2>
        <div className="seg range" id="range-pi">
          {SPANS.map(([s, t]) => <button key={s} data-s={s} className={s === span ? "on" : undefined} onClick={() => setSpan(s)}>{t}</button>)}</div></div>
      {/* side by side (owner, 2026-11-27: the page was too tall); stacked again at phone width */}
      <div className="pi-lanes">
        <div className="pi-lane">
          <Legend id="legend-pi" lines={LOAD} series={load} live={liveLoad} fmt={pctFmt} />
          <TimeChart id="chart-pi" series={shownLoad} yMin={0} yMax={100} yTicks={[0, 50, 100]} {...win} fmt={pctFmt} />
        </div>
        {shownTemps.length > 0 && <div className="pi-lane">
          <Legend id="legend-pi-temp" lines={TEMPS} series={temps} live={liveTemp} fmt={degFmt} />
          <TimeChart id="chart-pi-temp" series={shownTemps} zero={false} {...tAxis} {...win} fmt={degFmt} />
        </div>}
      </div>
    </div>
  );
});
