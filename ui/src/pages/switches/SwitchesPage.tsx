// Switches: one board of outlined group boxes packed densely into a C-column grid (biggest first), so everything fits
// on the kiosk screen without scrolling. Ported from app.js renderSwitches. The board's structure re-renders only
// when the set of channels or groups changes; each tile subscribes to its own channel's fields.
import { memo, useLayoutEffect, useRef, useState } from "react";
import { Icon } from "../../components/Icon";
import { canControlDev, ctlKeys } from "../../logic/control";
import { useOptimistic } from "../../logic/optimistic";
import { heldTile, holdStart, swOptId, toggleSwitch, type Sw, type Trigger, type VInput } from "../../logic/switches";
import { useKey, useKeySelector, useSelect } from "../../state/hooks";

// A note only when something is off: no banks decoded, or switch control off. Live control needs none (owner,
// 2026-11-27: "Live: tap to switch. Keypad logic is off." removed).
const Note = memo(function Note() {
  const t = useSelect(["switches.source", ...ctlKeys("n2k")], g =>
    g("switches.source") !== "n2k" ? "No switch banks decoded yet."
      : canControlDev("n2k") ? ""
      : "Live from NMEA 2000. Switch control is off (n2k tx_enabled is not set), so tapping does nothing.");
  return t ? <div className="banner-note" id="sw-note"><Icon name="alert" /><span id="sw-note-t">{t}</span></div> : null;
});

/** The status line under a tile's name (app.js label()). */
function label(s: Sw, triggers: Trigger[], inputs: VInput[]): string {
  if (s.state === "error") return "Fault";
  if (s.state === "n/a") return "Not reporting";
  if (s.state === "stale") return "No data";
  if (triggers.some(t => t.channel === s.channel)) return s.on ? "Unlocked · timer" : "Locked · tap to open";
  const need = !s.on && s.requires_input != null && !inputs.find(i => i.input === s.requires_input)?.on;
  return (s.on ? (s.kind === "dimmer" && s.level != null && s.level < 100 ? `On · ${s.level}%` : "On") : "Off") + (s.blocked ? " · locked" : "")
    + (need ? ` · needs ${inputs.find(i => i.input === s.requires_input)?.name || "input"}` : "");
}

const TILE_KEYS = ["switches.list", "switching.triggers", "switching.inputs"];
const Tile = memo(function Tile({ id }: { id: string }) {
  const t = useSelect(TILE_KEYS, g => {
    const s = (g<Sw[]>("switches.list") ?? []).find(x => x.id === id);
    if (!s) return null;
    return { name: s.name, icon: s.icon, on: !!s.on, state: s.state, blocked: !!s.blocked, channel: s.channel,
      amps: s.amps ? `${(+s.amps).toFixed(1)} A${s.rating_a ? ` / ${s.rating_a}` : ""}` : "",
      label: label(s, g<Trigger[]>("switching.triggers") ?? [], g<VInput[]>("switching.inputs") ?? []) };
  });
  const o = useOptimistic<boolean>(swOptId(id));
  if (!t) return null;
  const on = o ? o.value : t.on;
  const st = o?.phase === "sending" ? (o.value ? "Switching on…" : "Switching off…") : o ? (o.value ? "On" : "Off") : t.label;
  return (
    <div className="sw-cell">
      <button className={`sw ${on ? "on" : ""} ${t.state === "error" ? "fault" : ""} ${t.state === "n/a" || t.state === "stale" ? "na" : ""} ${t.blocked ? "blocked" : ""}${o?.phase === "sending" ? " sending" : ""}`}
        data-sw={id} onPointerDown={e => holdStart(e.pointerId, id)} onClick={() => { if (heldTile(id)) return; toggleSwitch(id); }}>
        <Icon name={t.state === "error" ? "alert" : t.icon || "toggle"} />
        <div><div className="name">{t.name}</div>
          <div className="st"><i></i>{st}{t.amps && <span className="sw-amps">{t.amps}</span>}<span className="sw-id">ch {t.channel}</span></div></div>
      </button>
    </div>
  );
});

const GroupCount = memo(function GroupCount({ group }: { group: string }) {
  const n = useKeySelector<Sw[], number>("switches.list", l => (l ?? []).filter(s => (s.group || "Switches") === group && s.on).length);
  return <span className="muted">· {n} on</span>;
});

const Inputs = memo(function Inputs() {
  const inputs = useKey<VInput[]>("switching.inputs") ?? [];
  return (
    <div className="sw-inputs" style={{ gridTemplateColumns: `repeat(${inputs.length},1fr)` }}>
      {inputs.map(i => (
        <span key={i.input} className={`sw-in ${i.on ? "on" : ""}`}><Icon name={i.icon || "toggle"} /><b>{i.name}</b>
          <span>{i.on == null ? "—" : i.on ? "On" : "Off"}{i.volts != null ? ` · ${(+i.volts).toFixed(1)} V` : ""}</span></span>))}
    </div>
  );
});

interface Shape { groups: { g: string; ids: string[] }[]; inputs: number }
const structure = (l: Sw[] | undefined, nInputs: number): Shape => {
  const groups: { g: string; ids: string[] }[] = [];
  for (const s of l ?? []) {
    const g = s.group || "Switches";
    let e = groups.find(x => x.g === g);
    if (!e) groups.push((e = { g, ids: [] }));
    e.ids.push(s.id);
  }
  return { groups, inputs: nInputs };
};

const Board = memo(function Board() {
  const shape = useSelect(["switches.list", "switching.inputs"], g => structure(g<Sw[]>("switches.list"), (g<VInput[]>("switching.inputs") ?? []).length));
  const ref = useRef<HTMLDivElement>(null);
  const [layout, setLayout] = useState<{ C: number; height: string }>({ C: 5, height: "" });
  // columns from the board's width; on kiosk-sized screens the board takes 80 % of the view's height below the
  // note (rows share it) so nothing scrolls. Measured on resize only, never per update.
  useLayoutEffect(() => {
    const board = ref.current;
    if (!board) return;
    const view = board.closest(".view") as HTMLElement | null;
    const measure = () => {
      const C = Math.max(2, Math.min(6, Math.floor((board.clientWidth || 1750) / 300)));
      const top = board.getBoundingClientRect().top;
      const avail = view ? view.getBoundingClientRect().bottom - parseFloat(getComputedStyle(view).paddingBottom) - top - view.scrollTop - 2 : 0;
      const height = innerWidth >= 1100 && top > 0 && avail > 300 ? `${Math.round(avail * 0.8)}px` : "";
      setLayout(p => (p.C === C && p.height === height ? p : { C, height }));
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(board);
    if (view) ro.observe(view);
    return () => ro.disconnect();
  }, [shape.groups.length > 0]);

  if (!shape.groups.length) {
    return <div id="switches" className="sw-board" ref={ref}><div className="empty-state"><Icon name="toggle" /><div>No switch banks decoded yet.</div></div></div>;
  }
  const { C } = layout;
  const dims = (n: number): [number, number] => { let h = n > 3 ? 2 : 1, w = Math.ceil(n / h); if (w > C) { w = C; h = Math.ceil(n / C); } return [w, h]; };
  const boxes = shape.groups.map(({ g, ids }) => {
    const [w, h] = dims(ids.length);
    return { key: g, w, h, body: <>
      <h3>{g} <GroupCount group={g} /></h3>
      <div className="sw-tiles" style={{ gridTemplateColumns: `repeat(${w},1fr)` }}>{ids.map(id => <Tile key={id} id={id} />)}</div>
    </> };
  });
  if (shape.inputs) boxes.push({ key: "\u0000inputs", w: Math.min(C, 2), h: 1, body: <><h3>Vehicle inputs</h3><Inputs /></> });
  boxes.sort((a, b) => b.w * b.h - a.w * a.h);
  return (
    <div id="switches" className="sw-board" ref={ref} style={{ gridTemplateColumns: `repeat(${C},1fr)`, height: layout.height || undefined }}>
      {boxes.map(b => <section key={b.key} className="sw-box" style={{ gridColumn: `span ${b.w}`, gridRow: `span ${b.h}` }}>{b.body}</section>)}
    </div>
  );
});

export default function SwitchesPage() {
  return <><Note /><Board /></>;
}
