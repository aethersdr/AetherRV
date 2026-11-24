// Time-series chart, ported from web/charts.js timeChart (same look: 2 px lines, ~12 % area wash, hairline grid, one
// y-axis, crosshair + tooltip on touch/hover). Renders when its data or size changes; the crosshair moves by
// writing attributes directly (no re-render while scrubbing). Colours may be CSS variables ("var(--c-batt)"), so
// night mode needs no re-render.
import { memo, useLayoutEffect, useMemo, useRef, useState, type PointerEvent, type ReactNode, type RefObject } from "react";

export type Point = readonly [number, number];
export interface Series { name: string; color: string; points?: readonly Point[] | null }
export interface TimeChartProps {
  series: readonly Series[];
  unit?: string; yMin?: number; yMax?: number; yTicks?: readonly number[];
  area?: boolean; zero?: boolean; t0?: number; t1?: number;
  fmt?: (v: number) => string;
  className?: string; id?: string;
}

export function niceTicks(min: number, max: number, count = 4): number[] {
  if (min === max) max = min + 1;
  const span = max - min, raw = span / count, mag = Math.pow(10, Math.floor(Math.log10(raw)));
  const step = [1, 2, 2.5, 5, 10].map(m => m * mag).find(s => span / s <= count) || 10 * mag;
  const lo = Math.floor(min / step) * step, hi = Math.ceil(max / step) * step;
  const ticks: number[] = [];
  for (let v = lo; v <= hi + step / 2; v += step) ticks.push(+v.toFixed(6));
  return ticks;
}
const hhmm = (t: number) => { const d = new Date(t * 1000); return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`; };
const PAD = { l: 52, r: 16, t: 12, b: 28 };

function useSize(ref: RefObject<HTMLElement | null>) {
  const [size, setSize] = useState<{ w: number; h: number } | null>(null);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const set = (w: number, h: number) => setSize(p => (p && p.w === w && p.h === h ? p : { w, h }));
    set(el.clientWidth, el.clientHeight);
    const ro = new ResizeObserver(es => { const r = es[0]!.contentRect; set(Math.round(r.width), Math.round(r.height)); });
    ro.observe(el);
    return () => ro.disconnect();
  }, [ref]);
  return size;
}

export const TimeChart = memo(function TimeChart(opt: TimeChartProps) {
  const el = useRef<HTMLDivElement>(null);
  const size = useSize(el);
  const W = size?.w || 600, H = size?.h || 220;
  const series = useMemo(() => opt.series.filter((s): s is Series & { points: readonly Point[] } => !!s.points && s.points.length > 0), [opt.series]);
  const fmt = opt.fmt || ((v: number) => `${Math.round(v)}${opt.unit || ""}`);
  const xh = useRef<SVGGElement>(null), tip = useRef<HTMLDivElement>(null);

  if (!series.length) {
    return <div className={`chart ${opt.className ?? ""}`} id={opt.id} ref={el}><div className="chart-empty">Collecting data…</div></div>;
  }
  const all = series.flatMap(s => s.points);
  const t0 = opt.t0 ?? Math.min(...all.map(p => p[0])), t1 = opt.t1 ?? Math.max(...all.map(p => p[0]));
  let vmin = opt.yMin ?? (opt.zero === false ? Math.min(...all.map(p => p[1])) : Math.min(0, ...all.map(p => p[1])));
  let vmax = opt.yMax ?? Math.max(...all.map(p => p[1]));
  const ticks = niceTicks(vmin, vmax, 4);
  vmin = opt.yMin ?? ticks[0]!; vmax = opt.yMax ?? ticks[ticks.length - 1]!;
  const x = (t: number) => PAD.l + (t - t0) / Math.max(1, t1 - t0) * (W - PAD.l - PAD.r);
  const y = (v: number) => PAD.t + (1 - (v - vmin) / Math.max(1e-9, vmax - vmin)) * (H - PAD.t - PAD.b);
  const span = t1 - t0;

  const xLabels: ReactNode[] = [];
  if (span > 2 * 86400) {
    const everyD = span > 14 * 86400 ? 7 : span > 7 * 86400 ? 2 : 1;
    const d = new Date(t0 * 1000); d.setHours(24, 0, 0, 0);
    for (let i = 0; d.getTime() / 1000 <= t1; i++, d.setDate(d.getDate() + 1)) {
      if (i % everyD) continue;
      const t = d.getTime() / 1000;
      xLabels.push(<line key={`g${t}`} className="grid" x1={x(t)} x2={x(t)} y1={PAD.t} y2={H - PAD.b} opacity=".5" />,
        <text key={`l${t}`} className="axis" x={x(t)} y={H - 6} textAnchor="middle">{d.toLocaleDateString([], { weekday: "short" })} {d.getDate()}</text>);
    }
  } else {
    const stepH = span > 12 * 3600 ? 3 : span > 4 * 3600 ? 1 : span > 1.5 * 3600 ? 0.5 : 0.25;
    const first = Math.ceil(t0 / (stepH * 3600)) * stepH * 3600;
    for (let t = first; t <= t1; t += stepH * 3600) xLabels.push(<text key={t} className="axis" x={x(t)} y={H - 6} textAnchor="middle">{hhmm(t)}</text>);
  }

  const move = (ev: PointerEvent<SVGRectElement>) => {
    const svg = ev.currentTarget.ownerSVGElement!, r = svg.getBoundingClientRect();
    const px = ev.clientX - r.left;
    const t = t0 + (px - PAD.l) / (W - PAD.l - PAD.r) * (t1 - t0);
    const g = xh.current!, dots = g.querySelectorAll("circle"), tp = tip.current!;
    let rows = "";
    series.forEach((s, i) => {
      const p = s.points.reduce((a, b) => Math.abs(b[0] - t) < Math.abs(a[0] - t) ? b : a);
      dots[i]?.setAttribute("cx", String(x(p[0]))); dots[i]?.setAttribute("cy", String(y(p[1])));
      rows += `<div><i style="background:${s.color}"></i>${esc(s.name)}<b>${esc(fmt(p[1]))}</b></div>`;
    });
    const cx = Math.min(Math.max(px, PAD.l), W - PAD.r);
    const line = g.querySelector("line")!;
    line.setAttribute("x1", String(cx)); line.setAttribute("x2", String(cx));
    g.style.display = "";
    const when = new Date(t * 1000);
    const label = span > 86400 ? when.toLocaleString([], { weekday: "short", hour: "2-digit", minute: "2-digit" })
      : when.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
    tp.innerHTML = `<div class="tip-t">${esc(label)}</div>${rows}`;
    tp.style.display = "";
    tp.style.left = `${Math.min(cx + 14, W - tp.offsetWidth - 4)}px`;
  };
  const hide = () => { if (xh.current) xh.current.style.display = "none"; if (tip.current) tip.current.style.display = "none"; };

  return (
    <div className={`chart ${opt.className ?? ""}`} id={opt.id} ref={el}>
      <svg viewBox={`0 0 ${W} ${H}`} width={W} height={H} className="tchart">
        {(opt.yTicks || ticks).filter(v => v >= vmin && v <= vmax).map(v => [
          <line key={`g${v}`} className={`grid${v === 0 ? " base" : ""}`} x1={PAD.l} x2={W - PAD.r} y1={y(v)} y2={y(v)} />,
          <text key={`t${v}`} className="axis" x={PAD.l - 10} y={y(v) + 5} textAnchor="end">{fmt(v)}</text>])}
        {xLabels}
        {series.map(s => {
          const d = s.points.map((p, i) => `${i ? "L" : "M"}${x(p[0]).toFixed(1)},${y(p[1]).toFixed(1)}`).join("");
          const last = s.points[s.points.length - 1]!, base = y(Math.max(vmin, 0));
          return <g key={s.name}>
            {opt.area && <path d={`${d}L${x(last[0])},${base}L${x(s.points[0]![0])},${base}Z`} fill={s.color} opacity=".12" />}
            <path d={d} fill="none" stroke={s.color} strokeWidth={2} strokeLinejoin="round" strokeLinecap="round" />
            <circle cx={x(last[0])} cy={y(last[1])} r={5} fill={s.color} stroke="var(--surface)" strokeWidth={2} />
          </g>;
        })}
        <g className="xhair" style={{ display: "none" }} ref={xh}>
          <line y1={PAD.t} y2={H - PAD.b} />
          {series.map(s => <circle key={s.name} r={5} fill={s.color} stroke="var(--surface)" strokeWidth={2} />)}
        </g>
        <rect className="hit" x={PAD.l} y={0} width={W - PAD.l - PAD.r} height={H} fill="transparent"
          onPointerMove={move} onPointerDown={move} onPointerLeave={hide}
          onPointerUp={ev => { if (ev.pointerType === "touch") setTimeout(hide, 2500); }} />
      </svg>
      <div className="tip" style={{ display: "none" }} ref={tip} />
    </div>
  );
});

const esc = (s: unknown) => String(s).replace(/[&<>"']/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]!));
