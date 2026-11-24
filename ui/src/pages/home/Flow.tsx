// Home energy-flow diagram. The SVG is rendered ONCE (a memoised component with no props, no dynamic children);
// live values are written straight into it by a store subscription: edges animate (CSS dash flow, speed by
// power), numbers tween, the battery ring eases (CSS transition on its dasharray). No re-render per update.
// Ported from app.js FLOW / buildFlow / updateFlow (same geometry, ids and classes).
//
// The edges (each track, then its running dots, in the original order) are drawn on a canvas under the SVG, which
// holds the nodes, the inverter and the hub. A frame clears and redraws six strokes and ~100 small dots. The dots
// were a CSS stroke-dashoffset animation, which re-painted the whole SVG, its text, icons and rings included, every
// frame (Home cost 50–85 % of a core on the Pi 5, docs/UI-RUNTIME-BENCHMARK.md). Same geometry, colours, dot size
// and spacing, speeds and fade; the .f-edge paths stay in the SVG (hidden) with their classes, as the edges' state.
import { memo, useEffect, useRef, type MouseEvent } from "react";
import { Icon } from "../../components/Icon";
import { fmtW, num, socColor } from "../../lib/format";
import { NumberText } from "../../lib/tween";
import { openModal, toast } from "../../logic/ui";
import { K, store } from "../../state/store";

const FLOW = {
  W: 1000, H: 720, hub: { x: 500, y: 235, r: 125 },
  inv: { x: 500, y: 565, w: 236, h: 124 },
  nodes: [
    { id: "solar", label: "Solar", ic: "panel", key: "victron.pv.power", color: "--c-solar", x: 110, y: 105, to: "hub", side: "in" },
    { id: "alt", label: "Alternator", ic: "engine", key: "energy.alternator.power", color: "--c-alt", x: 110, y: 320, to: "hub", side: "in" },
    { id: "shore", label: "Shore", ic: "plug", key: "victron.ac.shore", color: "--c-shore", x: 110, y: 565, to: "inv", side: "in" },
    { id: "dc", label: "DC loads", ic: "dcload", key: "energy.dc.load", color: "--c-load", x: 890, y: 175, to: "hub", side: "out" },
    { id: "ac", label: "AC loads", ic: "outlet", key: "victron.ac.load", color: "--c-load", x: 890, y: 565, to: "inv", side: "out" },
  ],
} as const;
const R = 58;
const CIRC = 2 * Math.PI * FLOW.hub.r;

export const FLOW_KEYS = [...FLOW.nodes.map(n => n.key), "victron.vebus.ac_in_w", "victron.vebus.state", "victron.vebus.mode",
  "victron.vebus.dc_w", "victron.vebus.instance", "victron.vebus.name", "energy.alternator.source", "alternator.online",
  "alternator.state", "victron.battery.soc", "victron.battery.voltage", "victron.battery.power", "victron.connected"];

const curve = (x1: number, y1: number, x2: number, y2: number) => `M${x1},${y1} C${(x1 + x2) / 2},${y1} ${(x1 + x2) / 2},${y2} ${x2},${y2}`;

/** Edge paths: node → hub / inverter, and battery → inverter (drawn battery → inverter; reversed = charging). */
const EDGES: readonly { id: string; d: string; color: string; pts: readonly (readonly [number, number])[] }[] = (() => {
  const { hub, inv } = FLOW;
  const c = (x1: number, y1: number, x2: number, y2: number) => ({ d: curve(x1, y1, x2, y2), pts: [[x1, y1], [(x1 + x2) / 2, y1], [(x1 + x2) / 2, y2], [x2, y2]] as const });
  const out = FLOW.nodes.map(n => ({
    id: n.id, color: n.color,
    ...(n.to === "hub"
      ? n.side === "in" ? c(n.x + R, n.y, hub.x - hub.r - 14, hub.y) : c(hub.x + hub.r + 14, hub.y, n.x - R, n.y)
      : n.side === "in" ? c(n.x + R, n.y, inv.x - inv.w / 2 - 8, inv.y) : c(inv.x + inv.w / 2 + 8, inv.y, n.x - R, n.y)),
  }));
  const a = [hub.x, hub.y + hub.r + 16] as const, b = [inv.x, inv.y - inv.h / 2 - 8] as const;
  return [...out, { id: "inv", color: "--c-batt", d: `M${a[0]},${a[1]} L${b[0]},${b[1]}`, pts: [a, a, b, b] }];
})();

const VB = `0 0 ${FLOW.W} ${FLOW.H}`;

// ── the edges on a canvas ──
const PERIOD = 22.1;          // dash period of the old stroke-dasharray "0.1 22" (a round-capped 0.1 dash = one dot)
const DOT_R = 3;              // half the stroke width (6)
const FADE_S = 0.5;           // the old opacity transition

interface Edge { id: string; color: string; cum: Float32Array; xy: Float32Array; len: number;
  on: boolean; rev: boolean; sp: number; a: number; phase: number }

/** Cubic Bézier sampled into a polyline with cumulative lengths (for positions by distance along the path). */
function sample(p: readonly (readonly [number, number])[], n = 96): { xy: Float32Array; cum: Float32Array } {
  const xy = new Float32Array((n + 1) * 2), cum = new Float32Array(n + 1);
  for (let i = 0; i <= n; i++) {
    const t = i / n, u = 1 - t;
    const x = u * u * u * p[0]![0] + 3 * u * u * t * p[1]![0] + 3 * u * t * t * p[2]![0] + t * t * t * p[3]![0];
    const y = u * u * u * p[0]![1] + 3 * u * u * t * p[1]![1] + 3 * u * t * t * p[2]![1] + t * t * t * p[3]![1];
    xy[i * 2] = x; xy[i * 2 + 1] = y;
    if (i) cum[i] = cum[i - 1]! + Math.hypot(x - xy[i * 2 - 2]!, y - xy[i * 2 - 1]!);
  }
  return { xy, cum };
}

/** The tracks and the running dots. Draws only while something moves (an edge on, or fading); idle, one frame. */
class FlowCanvas {
  private readonly g: CanvasRenderingContext2D | null;
  private readonly edges: Edge[];
  private readonly ro: ResizeObserver;
  private raf = 0;
  private last = 0;
  private live = false;
  private w = 0; private h = 0; private dpr = 1;

  constructor(private readonly canvas: HTMLCanvasElement) {
    this.g = canvas.getContext("2d");
    this.edges = EDGES.map(e => ({ id: e.id, color: e.color, ...sample(e.pts), len: 0, on: false, rev: false, sp: 1, a: 0, phase: 0 }));
    for (const e of this.edges) e.len = e.cum[e.cum.length - 1]!;
    this.ro = new ResizeObserver(() => { this.size(); this.kick(); });
    this.ro.observe(canvas);
  }
  set(id: string, on: boolean, sp: number, rev: boolean): void {
    const e = this.edges.find(x => x.id === id);
    if (!e || (e.on === on && e.sp === sp && e.rev === rev)) return;
    e.on = on; e.sp = sp; e.rev = rev;
    if (!this.live) e.a = on ? 1 : 0;      // the state before the first frame (page shown): no fade, as the CSS had
    this.kick();
  }
  start(): void { this.live = true; this.last = 0; this.kick(); }
  stop(): void { this.live = false; cancelAnimationFrame(this.raf); this.raf = 0; }
  dispose(): void { this.stop(); this.ro.disconnect(); }

  private size(): void {
    const r = this.canvas.getBoundingClientRect();
    this.dpr = window.devicePixelRatio || 1;
    this.w = Math.round(r.width * this.dpr); this.h = Math.round(r.height * this.dpr);
    if (this.canvas.width !== this.w || this.canvas.height !== this.h) { this.canvas.width = this.w; this.canvas.height = this.h; }
  }
  private kick(): void { if (this.live && !this.raf) this.raf = requestAnimationFrame(this.frame); }

  private readonly frame = (now: number): void => {
    this.raf = 0;
    const g = this.g;
    if (!g || !this.live) return;
    if (!this.w) this.size();
    const dt = this.last ? Math.min(0.1, (now - this.last) / 1000) : 0;
    let moving = false;
    for (const e of this.edges) {
      if (e.on) e.phase = (e.phase + dt / (e.sp || 1)) % 1;
      const to = e.on ? 1 : 0;
      if (e.a !== to) e.a = to > e.a ? Math.min(1, e.a + dt / FADE_S) : Math.max(0, e.a - dt / FADE_S);
      if (e.on || e.a > 0) moving = true;
    }
    // the SVG's box: viewBox 1000 × 720, xMidYMid meet
    const s = Math.min(this.w / FLOW.W, this.h / FLOW.H), ox = (this.w - FLOW.W * s) / 2, oy = (this.h - FLOW.H * s) / 2;
    const cs = getComputedStyle(this.canvas);
    g.setTransform(1, 0, 0, 1, 0, 0);
    g.clearRect(0, 0, this.w, this.h);
    g.setTransform(s, 0, 0, s, ox, oy);
    g.lineCap = "round"; g.lineWidth = 2 * DOT_R;
    const track = cs.getPropertyValue("--track");
    for (const e of this.edges) {
      g.strokeStyle = track;
      g.beginPath();
      for (let i = 0; i < e.cum.length; i++) (i ? g.lineTo : g.moveTo).call(g, e.xy[i * 2]!, e.xy[i * 2 + 1]!);
      g.stroke();
      if (e.a <= 0) continue;
      g.globalAlpha = e.a;
      g.fillStyle = cs.getPropertyValue(e.color);
      g.beginPath();
      // dots where the old dash pattern had its dashes: k·PERIOD + offset, the offset running forward (or back)
      const off = (e.rev ? 1 - e.phase : e.phase) * PERIOD;
      let j = 0;
      for (let d = off; d <= e.len + 0.05; d += PERIOD) {
        while (j < e.cum.length - 2 && e.cum[j + 1]! < d) j++;
        const seg = e.cum[j + 1]! - e.cum[j]!, f = seg > 0 ? Math.min(1, Math.max(0, (d - e.cum[j]!) / seg)) : 0;
        const x = e.xy[j * 2]! + (e.xy[j * 2 + 2]! - e.xy[j * 2]!) * f, y = e.xy[j * 2 + 1]! + (e.xy[j * 2 + 3]! - e.xy[j * 2 + 1]!) * f;
        g.moveTo(x + DOT_R, y);
        g.arc(x, y, DOT_R, 0, 2 * Math.PI);
      }
      g.fill();
      g.globalAlpha = 1;
    }
    this.last = moving ? now : 0;
    if (moving) this.raf = requestAnimationFrame(this.frame);
  };
}

/** The static diagram. Every text that changes is EMPTY here: FlowCard's updater owns its content. */
const FlowSvg = memo(function FlowSvg() {
  const { hub, inv } = FLOW;
  const x0 = inv.x - inv.w / 2, y0 = inv.y - inv.h / 2, cy = inv.y + 6;
  return (
    <svg viewBox={VB} preserveAspectRatio="xMidYMid meet">
      {/* the edges' state (classes, speed): drawn by FlowCanvas, not here */}
      {EDGES.map(e => <path key={e.id} className="f-edge" id={`e-${e.id}`} d={e.d} />)}
      {FLOW.nodes.map(n => (
        <g key={n.id} className="f-node idle" id={`n-${n.id}`}>
          <circle className="bg" cx={n.x} cy={n.y} r={R} style={{ stroke: `var(${n.color})` }} />
          <g className="f-ic" style={{ color: `var(${n.color})` }}>
            <Icon name={n.ic} bare x={n.x - 26} y={n.y - 26} width={52} height={52} />
          </g>
          <text className="f-label" id={`l-${n.id}`} x={n.x} y={n.y + R + 32} textAnchor="middle" />
          <text className="f-val" x={n.x} y={n.y + R + 70} textAnchor="middle" id={`v-${n.id}`} />
        </g>
      ))}
      {/* wireframe inverter/charger */}
      <g className="f-inv off" id="inv" role="button" aria-label="Inverter controls">
        <rect className="inv-body" x={x0} y={y0} width={inv.w} height={inv.h} rx={20} />
        <path className="inv-line" d={`M${x0 + 18},${y0 + 18} h44 M${x0 + 18},${y0 + 28} h44 M${x0 + 18},${y0 + 38} h44`} />
        <circle className="inv-led" id="inv-led" cx={x0 + inv.w - 24} cy={y0 + 24} r={7} />
        <path className="inv-line strong" d={`M${x0 + 30},${cy} h52`} />
        <path className="inv-line strong dash" d={`M${x0 + 30},${cy + 16} h52`} />
        <path className="inv-line" d={`M${inv.x - 14},${cy - 6} l14,14 l-14,14`} />
        <path className="inv-line strong" d={`M${inv.x + 24},${cy + 8} c8,-22 15,-22 22,0 s14,22 22,0 s14,-22 22,0`} />
        <text className="inv-sym" x={x0 + 56} y={y0 + inv.h - 14} textAnchor="middle">DC</text>
        <text className="inv-sym" x={inv.x + 57} y={y0 + inv.h - 14} textAnchor="middle">AC</text>
        <text className="f-label" id="inv-label" x={inv.x} y={y0 + inv.h + 34} textAnchor="middle" />
        <text className="f-inv-val" id="inv-val" x={inv.x} y={y0 + inv.h + 68} textAnchor="middle" />
        <rect className="inv-hit" x={x0 - 20} y={y0 - 20} width={inv.w + 40} height={inv.h + 100} fill="transparent" />
      </g>
      {/* hub: battery ring */}
      <g id="hub" role="button" aria-label="Battery details">
        <circle cx={hub.x} cy={hub.y} r={hub.r + 14} fill="var(--surface-2)" />
        <circle cx={hub.x} cy={hub.y} r={hub.r} fill="none" stroke="var(--track)" strokeWidth={15} />
        <circle id="hub-ring" className="ring-val" cx={hub.x} cy={hub.y} r={hub.r} fill="none" stroke="var(--c-batt)" strokeWidth={15}
          strokeLinecap="round" strokeDasharray={`0 ${CIRC}`} transform={`rotate(-90 ${hub.x} ${hub.y})`} />
        <text className="f-hub-soc" id="hub-soc" x={hub.x} y={hub.y + 20} textAnchor="middle" />
        <text className="f-hub-sub" id="hub-sub" x={hub.x} y={hub.y + 58} textAnchor="middle" />
        <text className="f-hub-state" id="hub-state" x={hub.x} y={hub.y - 58} textAnchor="middle" />
      </g>
    </svg>
  );
});

const speedBucket = (w: number) => (w < 3 ? 0 : w < 30 ? 4 : w < 100 ? 2.6 : w < 300 ? 1.7 : w < 800 ? 1.1 : 0.7);
const VB_CHARGING = new Set([3, 4, 5, 6, 7]);   // bulk, absorption, float, storage, equalize
const VBS: Record<number, string> = { 0: "Off", 1: "Low power", 2: "Fault", 3: "Bulk", 4: "Absorption", 5: "Float", 6: "Storage",
  7: "Equalize", 8: "Passthru", 9: "Inverting", 10: "Assisting", 11: "Power supply", 252: "External control" };
const setText = (el: Element | null, s: string) => { if (el && el.textContent !== s) el.textContent = s; };

/** Binds the live values to the diagram's elements: update() writes them, stop() ends running tweens. */
function bindFlow(root: HTMLElement, sub: HTMLElement, dots: FlowCanvas) {
  const $ = (id: string) => root.querySelector(`#${id}`);
  const nodeVals = new Map(FLOW.nodes.map(n => [n.id, new NumberText($(`v-${n.id}`)!, v => fmtW(v))]));
  const soc = new NumberText($("hub-soc")!, v => `${Math.round(v)}%`);
  for (const n of FLOW.nodes) setText($(`l-${n.id}`), n.label);
  setText($("inv-label"), "Inverter");
  const setEdge = (id: string, active: boolean, sp: number, reverse = false) => {
    const e = $(id) as SVGPathElement | null;
    if (!e) return;
    e.classList.toggle("on", active);
    e.classList.toggle("rev", reverse);
    if (e.dataset.sp !== String(sp)) e.dataset.sp = String(sp);
    dots.set(id.slice(2), active, sp, reverse);
  };
  let first = true;
  const stop = () => { nodeVals.forEach(t => t.stop()); soc.stop(); };
  return { stop, update() {
    const S = store.get;
    let inW = 0, outW = 0;
    const vbState = S<number>("victron.vebus.state"), vbMode = S<number>("victron.vebus.mode");
    const inverting = vbState === 9 || vbState === 10;          // inverting / assisting from the battery
    const passthru = vbState === 8;
    const charging = vbState != null && VB_CHARGING.has(vbState);
    const acLive = inverting || passthru || (charging && vbMode === 3);
    for (const n of FLOW.nodes) {
      let w = S<number | null>(n.key);
      if (n.id === "shore" && w == null) w = S<number>("victron.vebus.ac_in_w");
      const live = n.id === "ac" && acLive;
      const active = (w != null && w >= 3) || live;
      $(`n-${n.id}`)?.classList.toggle("idle", !active);
      nodeVals.get(n.id)!.set(w, !first);
      if (n.id === "alt") {                                     // WS500 charge stage, when the regulator is charging
        const st = S("energy.alternator.source") === "n2k" && S("alternator.online") ? S<string>("alternator.state") : null;
        setText($("l-alt"), st && st !== "not charging" ? `Alternator · ${st[0]!.toUpperCase() + st.slice(1)}` : "Alternator");
      }
      setEdge(`e-${n.id}`, active, (w ?? 0) < 3 && live ? 6 : speedBucket(w ?? 0));
      if (w != null && w > 0) (n.side === "in" ? (inW += w) : (outW += w));
    }
    // battery ↔ inverter
    const dcw = S<number>("victron.vebus.dc_w");               // + = into the battery (charging), − = from it
    const linkW = Math.abs(dcw ?? 0);
    const linkActive = inverting || charging || linkW >= 3;
    setEdge("e-inv", linkActive, linkW < 3 ? 6 : speedBucket(linkW), charging && !inverting);
    const inv = $("inv");
    const known = S("victron.vebus.instance") != null;
    inv?.classList.toggle("off", !known || vbMode === 4 || vbState === 0);
    inv?.classList.toggle("charging", charging);
    inv?.classList.toggle("inverting", inverting || passthru);
    setText($("inv-label"), known ? `${S<string>("victron.vebus.name") || "Inverter"} · ${(vbState != null ? VBS[vbState] : undefined) ?? "—"}` : "Inverter");
    setText($("inv-val"), !known || dcw == null ? "" : charging ? `${fmtW(Math.abs(dcw))} → battery` : inverting ? `${fmtW(Math.abs(dcw))} from battery` : "");
    // battery hub
    const s = S<number | null>("victron.battery.soc");
    const ring = $("hub-ring");
    ring?.setAttribute("stroke-dasharray", `${(CIRC * (s ?? 0) / 100).toFixed(1)} ${CIRC}`);
    ring?.setAttribute("stroke", socColor(s));
    soc.set(s, !first);
    setText($("hub-sub"), s == null ? "Battery" : `${num(S("victron.battery.voltage"), 2)} V · ${fmtW(S<number>("victron.battery.power"))}`);
    const bp = S<number | null>("victron.battery.power");
    const st = $("hub-state") as SVGTextElement | null;
    setText(st, bp == null ? "" : bp > 5 ? "CHARGING" : bp < -5 ? "DISCHARGING" : "IDLE");
    if (st) st.style.fill = bp != null && bp > 5 ? "var(--good)" : "var(--ink-2)";
    setText(sub, S("victron.connected") ? `in ${fmtW(inW)} · out ${fmtW(outW)}` : "Cerbo GX not connected");
    first = false;
  } };
}

function onFlowClick(e: MouseEvent) {
  const t = e.target as Element;
  if (t.closest("#inv")) {
    if (store.get("victron.vebus.instance") == null) { toast("No MultiPlus reported by the Cerbo", "bad"); return; }
    openModal("inv");
  } else if (t.closest("#hub")) openModal("batt");
}

export const FlowCard = memo(function FlowCard() {
  const root = useRef<HTMLDivElement>(null);
  const sub = useRef<HTMLSpanElement>(null);
  const cv = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const dots = new FlowCanvas(cv.current!);
    const flow = bindFlow(root.current!, sub.current!, dots);
    flow.update();
    dots.start();                                    // runs while the page is shown (an <Activity> hide stops it)
    let raf = 0;                                     // coalesce bursts of deltas into one DOM pass per frame
    const off = store.subscribeKeys(FLOW_KEYS, () => { if (!raf) raf = requestAnimationFrame(() => { raf = 0; flow.update(); }); });
    const night = store.subscribe(K.night, () => dots.start());   // colours changed: redraw
    return () => { off(); night(); cancelAnimationFrame(raf); flow.stop(); dots.dispose(); };
  }, []);
  return (
    <div className="card flow-card">
      <div className="card-h"><h2>Energy flow</h2><span className="muted" id="flow-sub" ref={sub} /></div>
      <div id="flow" ref={root} onClick={onFlowClick}><canvas className="f-canvas" ref={cv} aria-hidden="true" /><FlowSvg /></div>
    </div>
  );
});
