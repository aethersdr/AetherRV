// Climate: the cabin dial (drag the setpoint knob around the ring) and the MCS7 fan slider beside it. Ported from
// app.js renderClimate / dial handlers / fan handlers / renderClimateControls.
//
// The dial's ticks and track are rendered once; the value arc and the knob are written straight into the SVG by two
// springs (no React render per frame). Drag rules kept exactly: grab the knob only (within 60 dial units), whole
// degrees in the display unit, ONE absolute setpoint sent on release (and only if it differs from the MCS7's),
// nothing sent on pointercancel. The send goes through heat.ts sendSetpoint, so a heating Espar follows the same
// rules as the Home ± buttons.
import { memo, useEffect, useRef, useState, type PointerEvent } from "react";
import { F2C, tC, UNIT } from "../../lib/format";
import { Spring } from "../../lib/tween";
import { canControl, control, ctlKeys, ctlPending } from "../../logic/control";
import { cancelSetpointTimer, CAN_SP_KEYS, canSetpoint, espHeating, SP_KEYS, sendSetpoint, spDisplay } from "../../logic/heat";
import { useKey, useSelect } from "../../state/hooks";
import { K, store } from "../../state/store";
import { A0, angOf, arc, DIAL_SIZE, DIAL_STROKE, dialRange, onKnob, pt, SWEEP, ticks, valueAt } from "./dial";
import { deg, useNumText } from "./num";

const [LO, HI] = dialRange(UNIT);
const TICKS = ticks(LO, HI);
const TRACK = arc(A0, A0 + SWEEP);
const clampD = (v: number) => Math.max(LO, Math.min(HI, v));
const R = (k: string) => `climate.rixens.${k}`;

/**
 * Where the knob sits (display unit, unrounded): the setpoint being adjusted, else the MCS7's, else a heating
 * Espar's. app.js drew it from the first two only, so with the MCS7 silent and the Espar heating the readout showed
 * a setpoint the ring had no knob for (and the knob could still be grabbed, invisibly).
 */
export function knobValue(): number | null {
  const g = store.get;
  const c = g<number>(K.spPending) ?? g<number>("climate.cabin.setpoint") ?? (espHeating() ? g<number>("climate.espar.ctl.setpoint_c") : null);
  return c == null ? null : tC(c);
}
const LIVE_KEYS = ["climate.cabin.temp", ...SP_KEYS];

/** The ring: static ticks + track; value arc and knob driven by springs from a store subscription. */
const DialSvg = memo(function DialSvg() {
  const valRef = useRef<SVGPathElement>(null), knobRef = useRef<SVGCircleElement>(null);
  const active = useKey(K.spDragging) === true;
  useEffect(() => {
    const vp = valRef.current!, kn = knobRef.current!;
    const vs = new Spring(LO, v => vp.setAttribute("d", arc(A0, angOf(v, LO, HI))));
    const ks = new Spring(LO, v => {
      const [x, y] = pt(angOf(v, LO, HI));
      kn.setAttribute("cx", String(x)); kn.setAttribute("cy", String(y));
    }, 400, 40);                                  // stiffer: the knob tracks a dragging finger closely
    let vOn = false, kOn = false;
    const update = () => {
      const c = tC(store.get<number | null>("climate.cabin.temp")), k = knobValue();
      if (c == null) vp.style.display = "none";
      else { vp.style.display = ""; if (vOn) vs.set(clampD(c)); else vs.snap(clampD(c)); }
      if (k == null) kn.style.display = "none";
      else { kn.style.display = ""; if (kOn) ks.set(clampD(k)); else ks.snap(clampD(k)); }
      vOn = c != null; kOn = k != null;
    };
    update();                                     // first paint (and when the page is shown again): no animation
    const off = store.subscribeKeys(LIVE_KEYS, update);
    return () => { off(); vs.stop(); ks.stop(); };
  }, []);
  const S = DIAL_STROKE;
  return (
    <svg viewBox={`0 0 ${DIAL_SIZE} ${DIAL_SIZE}`} className="dial">
      {TICKS.map((t, i) => <line key={i} x1={t.x1} y1={t.y1} x2={t.x2} y2={t.y2} className={t.major ? "tick-major" : "tick"} />)}
      <path d={TRACK} fill="none" stroke="var(--track)" strokeWidth={S} strokeLinecap="round" />
      <path ref={valRef} fill="none" stroke="var(--c-cabin)" strokeWidth={S} strokeLinecap="round" style={{ display: "none" }} />
      <circle ref={knobRef} className={`dial-knob${active ? " active" : ""}`} r={S / 2 + (active ? 11 : 5)} fill="var(--surface)"
        stroke={active ? "var(--c-cabin)" : "var(--ink)"} strokeWidth={active ? 4 : 3} style={{ display: "none" }} />
    </svg>
  );
});

/** The ring plus its pointer handling (app.js #dial pointerdown/move/up/cancel). */
const DialRing = memo(function DialRing() {
  const draggable = useSelect(CAN_SP_KEYS, canSetpoint);
  const at = (ev: PointerEvent<HTMLDivElement>) => {
    const rc = ev.currentTarget.getBoundingClientRect(), k = rc.width / DIAL_SIZE;
    const dx = ev.clientX - (rc.left + rc.width / 2), dy = ev.clientY - (rc.top + rc.height / 2);
    return { v: valueAt(dx, dy, LO, HI), dx: dx / k, dy: dy / k };
  };
  const dragging = () => store.get(K.spDragging) === true;
  const down = (ev: PointerEvent<HTMLDivElement>) => {
    if (!canSetpoint()) return;
    const cur = spDisplay();
    if (cur == null) return;
    const { dx, dy } = at(ev);
    if (!onKnob(dx, dy, cur, LO, HI)) return;     // must grab the knob
    ev.preventDefault();
    ev.currentTarget.setPointerCapture(ev.pointerId);
    cancelSetpointTimer();                        // a pending ± send is replaced by this drag's value
    store.set(K.spDragging, true);
  };
  const move = (ev: PointerEvent<HTMLDivElement>) => {
    if (!dragging()) return;
    const { v } = at(ev);
    const c = UNIT === "F" ? F2C(v) : v;
    const sp = store.get<number | null>(K.spPending);
    if (sp == null || Math.abs(c - sp) > 1e-6) store.set(K.spPending, c);
  };
  const up = () => {
    if (!dragging()) return;
    store.set(K.spDragging, false);
    // what the knob shows is what is sent: a result that cleared the pending value mid-drag (possible while the
    // Espar heats) leaves nothing to send, exactly as app.js
    const sp = store.get<number | null>(K.spPending) ?? null;
    const next = spDisplay(), cur = store.get<number | null>("climate.cabin.setpoint") ?? null;
    if (sp != null && next != null && (cur == null || Math.round(tC(cur)!) !== next)) sendSetpoint(sp, next);
    else store.set(K.spPending, null);
  };
  const cancel = () => {
    if (!dragging()) return;
    store.batch(() => { store.set(K.spDragging, false); store.set(K.spPending, null); });
  };
  return (
    <div className={`dial-wrap${draggable ? " draggable" : ""}`} id="dial"
      onPointerDown={down} onPointerMove={move} onPointerUp={up} onPointerCancel={cancel}>
      <DialSvg />
    </div>
  );
});

const CabinTemp = memo(function CabinTemp() {
  const c = useKey<number | null>("climate.cabin.temp");
  return <div className="dial-temp" id="dial-temp" ref={useNumText<HTMLDivElement>(tC(c), deg)} />;
});

const LABEL_KEYS = ["climate.cabin.humidity", "climate.cabin.source", "climate.cabin.front_temp", "climate.cabin.rear_temp"];
const CabinLabel = memo(function CabinLabel() {
  const v = useSelect(LABEL_KEYS, g => {
    const rh = g<number | null>("climate.cabin.humidity");
    return { rh: rh == null ? null : Math.round(rh), avg: g("climate.cabin.source") === "average",
      fr: g<number | null>("climate.cabin.front_temp") ?? null, bk: g<number | null>("climate.cabin.rear_temp") ?? null };
  });
  const fT = (c: number | null) => (c == null ? "—" : deg(tC(c)!));
  return (
    <div className="muted" id="dial-label">
      {v.rh == null ? "cabin" : `cabin · ${v.rh}% RH`}
      {v.avg && <span className="dial-split">{`average of front ${fT(v.fr)} · back ${fT(v.bk)}`}</span>}
    </div>
  );
});

const NOTE_KEYS = [R("live"), R("controls_ok"), K.spDragging, ...CAN_SP_KEYS];
const SetpointReadout = memo(function SetpointReadout() {
  const sp = useSelect(SP_KEYS, spDisplay);
  const dragging = useKey(K.spDragging) === true;
  const note = useSelect(NOTE_KEYS, g =>
    !g(R("live")) && !espHeating() ? "setpoint (MCS7 not connected)" : g(K.spDragging) === true ? "setpoint · release to set"
      : g(K.spPending) != null ? "setpoint · sending…" : g(K.control) !== true ? "setpoint · view only"
      : !g(R("controls_ok")) ? "setpoint · control off" : "setpoint · drag the ring");
  const ref = useNumText<HTMLElement>(sp, deg, { animate: !dragging, ms: 250 });
  return <div className="dial-ctl"><div className="ctl-val"><b id="sp-val" ref={ref} /><span id="sp-note">{note}</span></div></div>;
});

// ── Fan speed: a vertical slider (5 % steps, sent once on release); in Auto it follows the MCS7's live output ──
const FAN_KEYS = [R("fan_speed"), R("fan_auto_pct"), R("fan_running"), K.fanPending, ...ctlKeys("rixens")];
const pct = (v: number) => `${Math.round(v)}%`;
const FanCol = memo(function FanCol() {
  const [drag, setDrag] = useState(false);
  const dragRef = useRef(false);
  const track = useRef<HTMLDivElement>(null);
  const v = useSelect(FAN_KEYS, g => {
    const f = g<number | string | null>(R("fan_speed")) ?? null, fa = g<number | null>(R("fan_auto_pct")) ?? null;
    const pend = g<number | null>(K.fanPending) ?? null;
    const auto = f === "auto" && pend == null;
    const pos = pend != null ? pend : f === "auto" ? (g(R("fan_running")) === false ? 0 : fa) : f === "off" ? 0 : typeof f === "number" ? f : null;
    return { f, pend, auto, pos, ok: canControl() && !ctlPending() };
  });
  const shown = v.pend != null ? v.pend : v.f == null ? null : v.pos;
  const valRef = useNumText<HTMLElement>(shown, pct, { animate: !drag, empty: v.f != null && v.auto ? "Auto" : "—", ms: 300 });
  const note = drag ? "fan · release to set" : v.pend != null ? "fan · sending…" : v.auto ? "fan · auto" : v.f === "off" ? "fan · off" : "fan";
  const at = (ev: PointerEvent) => {
    const r = track.current!.getBoundingClientRect();
    return Math.max(0, Math.min(100, Math.round((1 - (ev.clientY - r.top) / r.height) * 20) * 5));
  };
  const down = (ev: PointerEvent<HTMLDivElement>) => {
    if (!canControl() || ctlPending()) return;
    ev.preventDefault();
    ev.currentTarget.setPointerCapture(ev.pointerId);
    dragRef.current = true; setDrag(true);
    store.set(K.fanPending, at(ev));
  };
  const move = (ev: PointerEvent<HTMLDivElement>) => {
    if (!dragRef.current) return;
    const p = at(ev);
    if (p !== store.get(K.fanPending)) store.set(K.fanPending, p);
  };
  const up = () => {
    if (!dragRef.current) return;
    dragRef.current = false; setDrag(false);
    const f = store.get(R("fan_speed")), p = store.get<number | null>(K.fanPending) ?? null;
    if (p != null && f !== p) control("fan", p, `Fan ${p}%`);        // also leaves Auto
    else store.set(K.fanPending, null);
  };
  const cancel = () => { dragRef.current = false; setDrag(false); store.set(K.fanPending, null); };
  const pos = v.pos ?? 0;
  return (
    <div className="fan-col">
      <div className="ctl-val sm fan-read"><b id="fan-val" ref={valRef} /><span id="fan-note">{note}</span></div>
      <div className={`vslider${v.auto ? " auto" : ""}${!v.ok && !drag ? " disabled" : ""}${drag ? " dragging" : ""}`} id="fan-slider"
        role="slider" aria-label="Fan speed" aria-valuemin={0} aria-valuemax={100} aria-valuenow={pos}
        onPointerDown={down} onPointerMove={move} onPointerUp={up} onPointerCancel={cancel}>
        <div className="vs-track" ref={track}>
          <div className="vs-fill" id="fan-fill" style={{ height: `${pos}%` }} />
          <div className="vs-thumb" id="fan-thumb" style={{ bottom: `${pos}%` }} />
        </div>
      </div>
      <button className={`ctl-pill${v.f === "auto" ? " on" : ""}`} id="fan-auto" disabled={!v.ok}
        onClick={() => control("fan", "auto", "Fan auto")}>Auto</button>
    </div>
  );
});

export const DialCard = memo(function DialCard() {
  return (
    <div className="card dial-card">
      <div className="dial-main">
        <div className="dial-area">
          <DialRing />
          <div className="dial-center"><CabinTemp /><CabinLabel /></div>
          <SetpointReadout />
        </div>
      </div>
      <FanCol />
    </div>
  );
});
