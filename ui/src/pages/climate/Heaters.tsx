// Climate: the Rixens MCS7 and Espar heater cards, ported from app.js renderClimate (heaters), mcsBadges, espBadges and
// the #heaters click handlers. Every tile, badge and button is its own component on its own keys.
//
// Safety rules kept exactly:
//  • MCS7 sources (Furnace / Electric / Engine) and destinations (Cabin heat / Hot water / Engine pre-heat): a confirm
//    before every change, on AND off; one verified command each.
//  • Espar: heat asks first and is refused outside its 10–30 °C range (heat.ts espStartHeat); fan only asks first;
//    off and the run time don't ask.
//  • Low-battery heat lockout (power.heat_lockout), three rules as app.js:
//      sources      usable iff (on || engine || !lockout)    — engine heat is exempt, turning off always works;
//      destinations Hot water / pre-heat locked when off      — Cabin heat (the thermostat) is not locked;
//      Espar        the heat badge is locked when not heating — fan only and the run time are not.
//    The handlers check the same rules again at tap time (a disabled button can't be tapped, but state may have moved).
import { memo, type ReactNode } from "react";
import { Icon } from "../../components/Icon";
import { cap, fmtT, num, tC } from "../../lib/format";
import { canControl, canControlDev, control, ctlKeys, ctlPending } from "../../logic/control";
import {
  cycleEspRun, espRun, espRunLabel, espSend, espStartFan, espStartHeat, heatLockout, HQ, HQ_KEYS, hqView, MCS_DEST,
} from "../../logic/heat";
import { getOptimistic, setOptimistic, useOptimistic } from "../../logic/optimistic";
import { askConfirm } from "../../logic/ui";
import { useKey, useSelect } from "../../state/hooks";
import { K, store, type Getter } from "../../state/store";
import { deg, useNumText } from "./num";

const R = (k: string) => `climate.rixens.${k}`;
const E = (k: string) => `climate.espar.${k}`;
const D = (k: string) => `climate.espar.diag.${k}`;
const C = (k: string) => `climate.espar.ctl.${k}`;
const g = store.get;

// ── Shared pieces ──
const LOCK_KEYS = ["power.heat_lockout", "power.shutdown"];
/** The low-battery banner in each heater card. */
const LockoutLine = memo(function LockoutLine() {
  const v = useSelect(LOCK_KEYS, gt => {
    if (gt("power.heat_lockout") !== true) return null;
    const soc = gt<{ soc?: number | null }>("power.shutdown")?.soc;
    return { soc: soc == null ? null : Math.round(soc) };
  });
  if (!v) return null;
  return (
    <div className="lockout-line" role="status"><Icon name="lock" /><b>Heat locked out</b>
      <span>{`House battery ${v.soc != null ? `${v.soc} % · ` : "low · "}charge to unlock`}</span></div>
  );
});

/** A burner tile. `val` returns a number (tweened with `fmt`) or a string; `on` false shows "—" and the idle label. */
interface TileDef {
  k: string; keys: readonly string[];
  val: (gt: Getter) => number | string | null | undefined;
  fmt?: (v: number) => string;
  sub: string | ((gt: Getter) => string);
  idle?: string;
}
const NumB = memo(function NumB({ v, fmt }: { v: number | null; fmt: (v: number) => string }) {
  return <b ref={useNumText<HTMLElement>(v, fmt)} />;
});
const Tile = memo(function Tile({ d, on }: { d: TileDef; on: boolean }) {
  const v = useSelect(d.keys, gt => {
    if (!on) return { val: null, sub: d.idle ?? (typeof d.sub === "string" ? d.sub : "") };
    const x = d.val(gt);
    return { val: x ?? null, sub: typeof d.sub === "function" ? d.sub(gt) : d.sub };
  });
  return (
    <div>
      <span>{d.k}</span>
      {typeof v.val === "string" ? <b>{v.val}</b> : <NumB v={v.val} fmt={d.fmt ?? deg} />}
      <small>{v.sub}</small>
    </div>
  );
});
const Burner = ({ tiles, on }: { tiles: readonly TileDef[]; on: boolean }) => (
  <div className={`burner${tiles.length > 6 ? " grid4" : ""}`}>{tiles.map(t => <Tile key={t.k} d={t} on={on} />)}</div>
);
const tempTile = (k: string, key: string, sub: string, pick?: (v: unknown) => number | null | undefined): TileDef => ({
  k, keys: [key], sub, val: gt => { const c = pick ? pick(gt(key)) : gt<number | null>(key); return c == null ? null : tC(c); },
});

const FaultLine = ({ text }: { text: string | null }) =>
  text ? <div className="warn-t fault-line"><Icon name="alert" />{`Fault code ${text}`}</div> : null;

function HeaterCard({ on, compact, badges, children }: { on: boolean; compact: boolean; badges: ReactNode; children: ReactNode }) {
  return (
    <div className={`card heater ${on ? "on" : ""} ${compact ? "compact" : ""}`}>
      <div className="badge-col">{badges}</div>
      <div className="heater-main">{children}</div>
    </div>
  );
}

// ── Rixens MCS7 ──
const RIX_TILES: readonly TileDef[] = [
  { k: "Fan", keys: [R("fan_rpm")], val: gt => gt<number>(R("fan_rpm")), fmt: v => `${Math.round(v).toLocaleString()} rpm`, sub: "Burner fan" },
  { k: "Glow", keys: [R("glow_w")], val: gt => gt<number>(R("glow_w")), fmt: v => `${Math.round(v)} W`, sub: "Glow plug" },
  { k: "Fuel", keys: [R("fuel_gph")], val: gt => { const x = gt<number | null>(R("fuel_gph")); return x == null ? null : +x; },
    fmt: v => `${v.toFixed(3)} gal/h`, sub: "Fuel flow" },
  tempTile("Inlet", R("inlet_c"), "Coolant in"),
  tempTile("Outlet", R("outlet_c"), "Coolant out"),
  tempTile("Flame", R("flame_c"), "Flame sensor"),
];

/** The coolant pill beside the name: black = off & cold, blue = on & cold, yellow > 122 °F, red > 155 °F. */
export function fluidClass(gt: Getter): string {
  const t = gt<number | null>(R("outlet_c")), on = (gt<string[]>(R("sources")) ?? []).length > 0 || gt(R("system_heat")) === true;
  const tf = t == null ? null : t * 9 / 5 + 32;
  const ind = gt(R("glycol"));
  if (tf != null ? tf > 155 : ind === "hot") return "hot";
  if (tf != null ? tf > 122 : ind === "ready") return "warm";
  return on ? "cold-on" : "cold-off";
}
const FLUID_KEYS = [R("outlet_c"), R("sources"), R("system_heat"), R("glycol")];
const FluidHero = memo(function FluidHero() {
  const v = useSelect(FLUID_KEYS, gt => ({ cls: fluidClass(gt), t: gt<number | null>(R("outlet_c")) ?? null }));
  const ref = useNumText<HTMLSpanElement>(tC(v.t), deg);
  return <div className="heater-hero"><span className={`fluid-pill ${v.cls}`} ref={ref} /><small>coolant</small></div>;
});

/** Optimistic-overlay ids: the sources and hot water / pre-heat share Home's heat buttons' ids, so both pages agree. */
const optId = (k: string) => (k === "thermostat" ? "mcs.thermostat" : `hq.${k}`);
const hqDef = (k: string) => HQ.find(h => h.k === k)!;

const SRC_WHAT: Record<string, string> = { furnace: "the diesel furnace (Eberspächer burner)",
  electric: "the 1500 W electric element (shore power only!)", engine: "engine heat" };
const SRC_KEYS = [R("sources"), R("furnace"), R("electric"), R("engine"), "power.heat_lockout", ...ctlKeys("rixens")];
const srcOn = (k: string) => (g<string[]>(R("sources")) ?? []).includes(k);

/** Tap a source: live state at tap time (a verified overlay the state hasn't caught up with yet counts, as hqTap). */
export function srcTap(k: string): void {
  const o = getOptimistic<boolean>(optId(k));
  const cur = o?.phase === "ok" ? o.value : srcOn(k);
  if (!canControl() || ctlPending() || (!cur && k !== "engine" && heatLockout())) return;
  const on = !cur;
  askConfirm(`${on ? "Turn on" : "Turn off"} ${k}?`,
    `This ${on ? "enables" : "disables"} ${SRC_WHAT[k]} on the MCS7. The MCS7 decides when to actually fire it.`,
    () => {
      const p = control(k, on, `${cap(k)} ${on ? "on" : "off"}`);
      if (p) setOptimistic(optId(k), on, p, () => hqView(hqDef(k)).on === on, HQ_KEYS);   // lit at once; rolls back if not verified
    });
}
const SRC_IC: Record<string, string> = { furnace: "flame", electric: "plug", engine: "engine" };
const SrcBtn = memo(function SrcBtn({ k }: { k: string }) {
  const v = useSelect(SRC_KEYS, () => ({ on: srcOn(k), st: g<string>(R(k)) || "", lockout: heatLockout(), ok: canControl() && !ctlPending() }));
  const o = useOptimistic<boolean>(optId(k));
  const on = o ? o.value : v.on;
  const locked = !on && k !== "engine" && v.lockout;
  const small = on ? (o ? "on" : v.st === "active" ? "active" : `on · ${v.st}`) : "off";
  return (
    <button className={`src-btn ${on ? "sel" : ""} st-${v.st || "off"}${locked ? " locked" : ""}${o?.phase === "sending" ? " sending" : ""}`}
      data-src={k} disabled={!(v.ok && !locked)} title={locked ? "Heat locked out: house battery low" : undefined} onClick={() => srcTap(k)}>
      <Icon name={SRC_IC[k]!} /><span>{cap(k)}</span><small>{small}</small>
      {locked && <i className="src-lock"><Icon name="lock" /></i>}
    </button>
  );
});
const SrcButtons = memo(function SrcButtons() {
  const ks = useSelect([R("available")], gt => {
    const avail = gt<Record<string, boolean>>(R("available")) ?? {};
    return ["furnace", "electric", "engine"].filter(k => k === "furnace" || avail[k]);
  });
  return <div className="src-btns">{ks.map(k => <SrcBtn key={k} k={k} />)}</div>;
});

const DEST = Object.keys(MCS_DEST);
const destKeys = (k: string) => [R(k), R("available"), "power.heat_lockout", ...ctlKeys("rixens")];
/** Tap a destination badge: confirm, then one command. */
export function destTap(k: string): void {
  const o = getOptimistic<boolean>(optId(k));
  const cur = o?.phase === "ok" ? o.value : !!g(R(k));
  const na = (k === "preheat" && g<Record<string, boolean>>(R("available"))?.preheat === false) || (!cur && k !== "thermostat" && heatLockout());
  if (!canControl() || ctlPending() || na) return;
  const on = !cur, [label, , onTxt, offTxt] = MCS_DEST[k]!;
  askConfirm(`Turn ${label.toLowerCase()} ${on ? "on" : "off"}?`, on ? onTxt : offTxt, () => {
    const p = control(k, on, `${label} ${on ? "on" : "off"}`);
    if (p) setOptimistic(optId(k), on, p, () => !!g(R(k)) === on, [R(k)]);
  });
}
const DestBadge = memo(function DestBadge({ k, i }: { k: string; i: number }) {
  const v = useSelect(destKeys(k), gt => ({ on: !!gt(R(k)), preNa: k === "preheat" && gt<Record<string, boolean>>(R("available"))?.preheat === false,
    lockout: heatLockout(), ok: canControl() && !ctlPending() }));
  const o = useOptimistic<boolean>(optId(k));
  const on = o ? o.value : v.on;
  const locked = !on && k !== "thermostat" && v.lockout;
  const na = v.preNa || locked;
  const [label, ic] = MCS_DEST[k]!;
  return (
    <button className={`badge ${i ? "mode " : ""}tap-b ${on ? "on" : locked ? "locked" : ""}${o?.phase === "sending" ? " sending" : ""}`}
      data-mcs={k} disabled={!(v.ok && !na)}
      title={`${label}${locked ? " · locked out: battery low" : na ? " (not enabled on this MCS7)" : on ? " · on · tap to turn off" : " · off · tap to turn on"}`}
      onClick={() => destTap(k)}><Icon name={ic} /></button>
  );
});

const RIX_KEYS = [R("state"), R("live"), R("available")];
const RixensCard = memo(function RixensCard() {
  const v = useSelect(RIX_KEYS, gt => ({ state: gt<string>(R("state")) ?? null, live: !!gt(R("live")),
    floor: !!gt<Record<string, boolean>>(R("available"))?.floor }));
  const badges = v.live ? DEST.map((k, i) => <DestBadge key={k} k={k} i={i} />)
    : <div className="badge" title="Cabin air heat"><Icon name={v.state == null ? "can" : "air_heat"} /></div>;
  return (
    <HeaterCard on={v.state === "heating"} compact={!v.live && v.state == null} badges={badges}>
      <div className="heater-h"><h3>Rixens MCS7</h3>{v.live && <FluidHero />}</div>
      <LockoutLine />
      {v.state != null && !v.live && <RixKv />}
      {v.state != null && <Burner tiles={RIX_TILES} on={v.live} />}
      {v.live && <SrcButtons />}
      {v.live && v.floor && <FloorChip />}
      <RixFault />
    </HeaterCard>
  );
});
/** Offline but known: the last coolant reading and fan speed. */
const RixKv = memo(function RixKv() {
  const v = useSelect([R("coolant"), R("fan")], gt => ({ c: gt<number | null>(R("coolant")) ?? null, fan: gt<string | number | null>(R("fan")) ?? null }));
  return <div className="kv"><div><b>{v.c == null ? "—" : fmtT(v.c)}</b><span>Coolant</span></div><div><b>{v.fan ?? "—"}</b><span>Fan speed</span></div></div>;
});
const FloorChip = memo(function FloorChip() {
  const on = useSelect([R("floor")], gt => gt(R("floor")) !== "off");
  return <div className="chips-row"><span className={`src ${on ? "on" : ""}`}>Floor{on ? " on" : ""}</span></div>;
});
const RixFault = memo(function RixFault() {
  const f = useKey<unknown>(R("fault_active"));
  return <FaultLine text={f ? String(f) : null} />;
});

// ── Espar Airtronic S2 ──
const fmtDur = (sec: number) => (sec < 3600 ? `${Math.floor(sec / 60)} min` : `${Math.floor(sec / 3600)} h ${Math.floor(sec % 3600 / 60)} min`);
const mmss = (s: number) => `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
interface Temps { intake_c?: number | null; outlet_c?: number | null; flame_c?: number | null }
const ESP_TILES: readonly TileDef[] = [
  { k: "Fan", keys: [D("blower")], val: gt => { const b = gt<number | null>(D("blower")); return b == null ? null : b / 45.59; },
    fmt: v => `${Math.round(v)} %`, sub: "Blower drive" },
  { k: "Glow", keys: [D("glow_w")], val: gt => gt<number>(D("glow_w")), fmt: v => `${Math.round(v)} W`, sub: "Glow plug" },
  { k: "Fuel", keys: [D("fuel"), D("pump_hz")],
    val: gt => { const r = gt<{ rate_ul_s?: number | null }>(D("fuel"))?.rate_ul_s; return r == null ? null : r * 3600 / 1e6 / 3.785; },
    fmt: v => `${v.toFixed(3)} gal/h`,
    sub: gt => { const hz = gt<number | null>(D("pump_hz")); return hz == null ? "Fuel pump" : `pump ${num(hz, 1)} Hz`; }, idle: "Fuel pump" },
  { k: "Power", keys: [D("power_pct")], val: gt => gt<number>(D("power_pct")), fmt: v => `${Math.round(v)} %`, sub: "Demand" },
  tempTile("Inlet", D("temps"), "Air in", t => (t as Temps | null)?.intake_c),
  tempTile("Outlet", D("temps"), "Air out", t => (t as Temps | null)?.outlet_c),
  { ...tempTile("Flame", D("temps"), "Flame sensor", t => (t as Temps | null)?.flame_c), keys: [D("temps"), D("flame")],
    sub: gt => (gt<{ flame?: boolean }>(D("flame"))?.flame ? "Flame on" : "Flame sensor"), idle: "Flame sensor" },
  // what's left of the run (∞ when unlimited) or of the heater's own cool-down
  { k: "Time left", keys: [E("cooldown_left_s"), C("left_s"), C("active"), C("mode")],
    val: gt => {
      const cd = gt<number | null>(E("cooldown_left_s")), left = gt<number | null>(C("left_s"));
      const running = gt(C("active")) && gt(C("mode")) && gt(C("mode")) !== "off";
      return cd != null ? mmss(cd) : running ? (left != null ? fmtDur(left) : "∞") : "—";
    },
    sub: gt => {
      const cd = gt<number | null>(E("cooldown_left_s")), left = gt<number | null>(C("left_s"));
      const running = gt(C("active")) && gt(C("mode")) && gt(C("mode")) !== "off";
      return cd != null ? "Cool-down" : running ? (left != null ? "Run timer" : "No time limit") : "Run timer";
    }, idle: "Run timer" },
];

const espMode = () => (g(C("active")) ? g<string>(C("mode")) ?? null : "off");
const ESP_BADGE_KEYS = [C("active"), C("mode"), "power.heat_lockout", K.espRun, ...ctlKeys("espar")];
const espOk = () => canControlDev("espar") && !ctlPending();
/** Espar badge taps, from live state at tap time (app.js read the badge's data-esp from the last render). */
export function espHeatTap(): void {
  if (!espOk()) return;
  if (espMode() === "heat") { espSend("off", "Espar off"); return; }
  if (heatLockout()) return;                              // heat start locked out (low battery)
  espStartHeat();                                         // confirm, or refused out of range
}
export function espFanTap(): void {
  if (!espOk()) return;
  if (espMode() === "fan") { espSend("off", "Espar off"); return; }
  espStartFan();                                          // confirm
}
const EspBadges = memo(function EspBadges() {
  const v = useSelect(ESP_BADGE_KEYS, () => ({ mode: espMode(), lockout: heatLockout(), ok: espOk(), run: espRun() }));
  const heat = v.mode === "heat", fan = v.mode === "fan";
  return <>
    <button className={`badge tap-b ${heat ? "on" : v.lockout ? "locked" : ""}`} data-esp={heat ? "off" : "heat"}
      disabled={!heat && v.lockout ? true : !v.ok} title={heat ? "Heating · tap to turn off" : "Heat"} onClick={espHeatTap}><Icon name="flame" /></button>
    <button className={`badge mode tap-b ${fan ? "on" : ""}`} data-esp={fan ? "off" : "fan"} disabled={!v.ok}
      title={fan ? "Fan on · tap to turn off" : "Fan only"} onClick={espFanTap}><Icon name="fan" /></button>
    <button className="badge mode tap-b esp-timer" data-esp="run" disabled={!v.ok} title="Run time (like the EasyStart Pro)"
      onClick={() => { if (espOk()) cycleEspRun(); }}><Icon name="clock" /><small>{espRunLabel(v.run)}</small></button>
  </>;
});

const EspFault = memo(function EspFault() {
  const t = useSelect([D("lockout"), C("problem"), E("issue"), E("fault"), E("fault_text")], gt => {
    const lo = gt<{ code?: string; why?: string; help?: string }>(D("lockout"));
    return lo ? `${lo.code} · locked out: ${lo.why}. ${lo.help}` : gt(C("problem")) ? `Control: ${gt(C("problem"))}`
      : gt(E("issue")) ? `Not heating: ${gt(E("issue"))}` : gt(E("fault")) ? `${gt(E("fault"))} · ${gt(E("fault_text"))}` : null;
  });
  return <FaultLine text={t} />;
});

const ESP_KEYS = [E("state"), D("enabled"), E("asleep")];
const EsparCard = memo(function EsparCard() {
  const v = useSelect(ESP_KEYS, gt => ({ state: gt<string>(E("state")) ?? null, on: !!gt(D("enabled")) && !gt(E("asleep")) }));
  const badges = v.state == null ? <div className="badge"><Icon name="can" /></div> : <EspBadges />;
  return (
    <HeaterCard on={v.state === "heating"} compact={v.state == null} badges={badges}>
      <div className="heater-h"><h3>Espar Airtronic S2</h3></div>
      <LockoutLine />
      {v.state != null && <Burner tiles={ESP_TILES} on={v.on} />}
      <EspFault />
    </HeaterCard>
  );
});

export const Heaters = memo(function Heaters() {
  return <div className="heaters"><div id="heaters" className="heater-list"><RixensCard /><EsparCard /></div></div>;
});
