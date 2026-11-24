// Home energy-flow diagram. The SVG is rendered ONCE (a memoised component with no props, no dynamic children);
// live values are written straight into it by a store subscription: edges animate (CSS dash flow, speed by
// power), numbers tween, the battery ring eases (CSS transition on its dasharray). No re-render per update.
// Ported from app.js FLOW / buildFlow / updateFlow (same geometry, ids and classes).
import { memo, useEffect, useRef, type MouseEvent } from "react";
import { Icon } from "../../components/Icon";
import { fmtW, num, socColor } from "../../lib/format";
import { NumberText } from "../../lib/tween";
import { openModal, toast } from "../../logic/ui";
import { store } from "../../state/store";

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

/** The static diagram. Every text that changes is EMPTY here: FlowCard's updater owns its content. */
const FlowSvg = memo(function FlowSvg() {
  const { W, H, hub, inv } = FLOW;
  const x0 = inv.x - inv.w / 2, y0 = inv.y - inv.h / 2, cy = inv.y + 6;
  const dLink = `M${hub.x},${hub.y + hub.r + 16} L${inv.x},${inv.y - inv.h / 2 - 8}`;
  return (
    <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="xMidYMid meet">
      {FLOW.nodes.map(n => {
        const d = n.to === "hub"
          ? n.side === "in" ? curve(n.x + R, n.y, hub.x - hub.r - 14, hub.y) : curve(hub.x + hub.r + 14, hub.y, n.x - R, n.y)
          : n.side === "in" ? curve(n.x + R, n.y, inv.x - inv.w / 2 - 8, inv.y) : curve(inv.x + inv.w / 2 + 8, inv.y, n.x - R, n.y);
        return [<path key={`t${n.id}`} className="f-edge-track" d={d} />,
          <path key={`e${n.id}`} className="f-edge" id={`e-${n.id}`} d={d} style={{ stroke: `var(${n.color})` }} />];
      })}
      {/* battery ↔ inverter link (drawn battery → inverter; reversed animation = charging) */}
      <path className="f-edge-track" d={dLink} />
      <path className="f-edge" id="e-inv" d={dLink} style={{ stroke: "var(--c-batt)" }} />
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
function bindFlow(root: HTMLElement, sub: HTMLElement) {
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
    if (e.dataset.sp !== String(sp)) { e.style.animationDuration = `${sp || 1}s`; e.dataset.sp = String(sp); }
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
  useEffect(() => {
    const flow = bindFlow(root.current!, sub.current!);
    flow.update();
    let raf = 0;                                     // coalesce bursts of deltas into one DOM pass per frame
    const off = store.subscribeKeys(FLOW_KEYS, () => { if (!raf) raf = requestAnimationFrame(() => { raf = 0; flow.update(); }); });
    return () => { off(); cancelAnimationFrame(raf); flow.stop(); };
  }, []);
  return (
    <div className="card flow-card">
      <div className="card-h"><h2>Energy flow</h2><span className="muted" id="flow-sub" ref={sub} /></div>
      <div id="flow" ref={root} onClick={onFlowClick}><FlowSvg /></div>
    </div>
  );
});
