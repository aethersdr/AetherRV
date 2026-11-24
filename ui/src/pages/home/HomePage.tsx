// Home: energy flow, climate card (setpoint ±, temperatures, coolant), tanks, heat buttons, lights & switches,
// battery 24 h. Same grid and contents as web/index.html; every card subscribes to its own keys.
import { memo, useState } from "react";
import { TimeChart } from "../../charts/TimeChart";
import { Icon } from "../../components/Icon";
import { fmtT, fmtTTG, tC } from "../../lib/format";
import { useTweenText } from "../../lib/tween";
import { canControlDev, ctlKeys } from "../../logic/control";
import { CAN_SP_KEYS, canSetpoint, HQ, HQ_KEYS, hqTap, hqView, nudgeSetpoint, SP_KEYS, spDisplay, type HQDef } from "../../logic/heat";
import { useOptimistic } from "../../logic/optimistic";
import { heldTile, holdStart, swOptId, toggleSwitch, type Sw } from "../../logic/switches";
import { useHistory } from "../../state/history";
import { useKey, useKeySelector, useSelect } from "../../state/hooks";
import { K } from "../../state/store";
import { FlowCard } from "./Flow";

// ── Climate card ──
const HeatPill = memo(function HeatPill() {
  const p = useSelect(["climate.rixens.state", "climate.espar.state"], g => {
    const r = g("climate.rixens.state"), e = g("climate.espar.state");
    return r == null && e == null ? null : r === "heating" || e === "heating";
  });
  return (
    <span id="home-heat-pill">
      {p == null ? <span className="pill">not connected</span>
        : <span className={`pill ${p ? "heat" : ""}`}><Icon name={p ? "flame" : "snow"} />{p ? "Heating" : "Idle"}</span>}
    </span>
  );
});

const HomeSetpoint = memo(function HomeSetpoint() {
  const sp = useSelect(SP_KEYS, spDisplay);
  const ok = useSelect(CAN_SP_KEYS, canSetpoint);
  const pending = useKey(K.spPending) != null;
  const dis = !ok || sp == null;
  // stopPropagation: a tap on ± never opens the Climate page (nor does a tap on them while disabled: .home-sp)
  return (
    <div className="home-sp" id="home-sp">
      <button className="ctl-btn hsp-btn" id="hsp-up" aria-label="Warmer" disabled={dis} onClick={e => { e.stopPropagation(); nudgeSetpoint(+1); }}>+</button>
      <div className="hsp-val"><b id="hsp-val">{sp == null ? "—" : `${sp}°`}</b><small id="hsp-note">{pending ? "sending…" : "set"}</small></div>
      <button className="ctl-btn hsp-btn" id="hsp-down" aria-label="Cooler" disabled={dis} onClick={e => { e.stopPropagation(); nudgeSetpoint(-1); }}>−</button>
    </div>
  );
});

const degF = (v: number) => `${Math.round(v)}°`;
const CabinTemp = memo(function CabinTemp() {
  const c = useKey<number | null>("climate.cabin.temp");
  const ref = useTweenText<HTMLSpanElement>(tC(c), degF);
  return <span id="home-cabin" ref={ref} />;
});
const OutsideTemp = memo(function OutsideTemp() {
  const c = useKey<number | null>("climate.outside.temp");
  return <span id="home-out">{fmtT(c)}</span>;
});

/** Refrigeration status: icon + label, never colour alone. */
function coldStatus(get: <T>(k: string) => T | undefined, role: string) {
  const t = get<number>(`climate.${role}.temp`), max = get<number>(`ui.${role}_max_c`);
  if (t == null) return { cls: "", label: "no sensor" };
  if (get(`settings.${role}_off`)) return { cls: "", label: "off" };
  if (max != null && t > max) return { cls: "warn", label: `above ${fmtT(max)}` };
  return { cls: "ok", label: "ok" };
}
const ColdRow = memo(function ColdRow({ role }: { role: "fridge" | "freezer" }) {
  const v = useSelect([`climate.${role}.temp`, `ui.${role}_max_c`, `settings.${role}_off`], g => ({
    t: g<number | null>(`climate.${role}.temp`) ?? null, st: coldStatus(g, role), off: !!g(`settings.${role}_off`),
  }));
  return (
    <div id={`home-${role}-row`}>
      <span className="ic-wrap"><Icon name={role === "fridge" ? "fridge" : "snow"} /></span>
      <span id={`home-${role}`}>{fmtT(v.t)}</span>
      <small id={`home-${role}-l`}>{v.st.cls === "warn" ? <span className="warn-t"><Icon name="alert" />{role} {v.st.label}</span> : v.off ? `${role} · off` : role}</small>
    </div>
  );
});

// Coolant thermometer (MCS7 coolant out). Black = system off & cold, blue = on & cold, yellow > 122 °F, red > 155 °F;
// the MCS7's own indicator when there's no temperature. Scale 32–190 °F, ticks at 122 and 155.
const FLUID_KEYS = ["climate.rixens.outlet_c", "climate.rixens.sources", "climate.rixens.system_heat", "climate.rixens.glycol"];
export function fluidClass(R: (k: string) => unknown): string {
  const t = R("outlet_c") as number | null | undefined;
  const on = ((R("sources") as string[] | undefined) || []).length > 0 || R("system_heat") === true;
  const tf = t == null ? null : t * 9 / 5 + 32;
  const ind = R("glycol");
  if (tf != null ? tf > 155 : ind === "hot") return "hot";
  if (tf != null ? tf > 122 : ind === "ready") return "warm";
  return on ? "cold-on" : "cold-off";
}
const CoolantThermo = memo(function CoolantThermo() {
  const v = useSelect(FLUID_KEYS, g => ({ t: g<number | null>("climate.rixens.outlet_c") ?? null, cls: fluidClass(k => g(`climate.rixens.${k}`)) }));
  const tf = v.t == null ? null : v.t * 9 / 5 + 32;
  const lo = 32, hi = 190, top = 10, bot = 196;
  const yOf = (f: number) => bot - (Math.max(lo, Math.min(hi, f)) - lo) / (hi - lo) * (bot - top);
  const fillY = tf == null ? bot : yOf(tf);
  const tick = (f: number) => <line key={f} x1={48} x2={60} y1={yOf(f)} y2={yOf(f)} className="th-tick" />;
  // tube + bulb as ONE outline, so the bulb's top arc between the tube walls isn't drawn
  const cy = bot + 30, Rb = 26, xl = 12, xr = 44, half = (xr - xl) / 2, yj = cy - Math.sqrt(Rb * Rb - half * half);
  return (
    <div className="coolant-therm" id="coolant-therm">
      <svg viewBox="0 0 66 260" className={`thermo ${v.cls}`} aria-label={`Coolant ${v.t == null ? "unknown" : fmtT(v.t)}`}>
        <path className="th-tube" d={`M${xl},${top + 8} A${half},${half} 0 0 1 ${xr},${top + 8} V${yj.toFixed(1)} A${Rb},${Rb} 0 1 1 ${xl},${yj.toFixed(1)} Z`} />
        <rect x={19} y={fillY} width={18} height={bot - fillY + 18} rx={9} className="th-fill" />
        <circle cx={28} cy={bot + 30} r={18} className="th-fill" />
        {tick(122)}{tick(155)}
      </svg>
      <div className="th-val"><b>{v.t == null ? "—" : fmtT(v.t)}</b><small>coolant</small></div>
    </div>
  );
});

const ClimateCard = memo(function ClimateCard() {
  return (
    <div className="card climate-card" data-goto="climate">
      <div className="card-h"><h2>Climate</h2><HeatPill /></div>
      <div className="climate-mini">
        <HomeSetpoint />
        <div className="temp-big"><CabinTemp /><small>cabin</small></div>
        <div className="temp-side">
          <div><span className="ic-wrap" id="ic-out"><Icon name="thermo" /></span><OutsideTemp /><small>outside</small></div>
          <ColdRow role="fridge" />
          <ColdRow role="freezer" />
        </div>
        <CoolantThermo />
      </div>
    </div>
  );
});

// ── Tanks ──
interface Tank { name?: string; type?: string; level?: number | null }
const TANK_COLOR: Record<string, string> = { fresh: "--c-batt", grey: "--muted", black: "--c-alt", fuel: "--c-solar" };
// The Cerbo's default tank name is just "Tank"; show the fluid type instead unless it was renamed.
const TANK_LABEL: Record<string, string> = { fresh: "Water", grey: "Grey", black: "Black", fuel: "Fuel" };
const tankName = (t: Tank) => (!t.name || /^tank\s*\d*$/i.test(t.name.trim())) ? (TANK_LABEL[t.type ?? ""] || t.name || "Tank") : t.name;
const TanksCard = memo(function TanksCard() {
  const tanks = useKey<Tank[]>("victron.tanks") ?? [];
  return (
    <div className="card tanks-card">
      <div id="tanks" className="tanks">
        {tanks.length ? tanks.map((t, i) => {
          const pct = t.level == null ? "—" : `${Math.round(t.level)}%`;
          return (
            <div className="tank" key={i}>
              <div className="tank-body">
                <div className="tank-fill" style={{ height: `${Math.max(0, Math.min(100, t.level ?? 0))}%`,
                  background: `color-mix(in srgb, var(${TANK_COLOR[t.type ?? ""] || "--c-batt"}) 70%, transparent)` }} />
                <div className="tank-pct">{pct}</div>
              </div>
              <div className="tank-name">{tankName(t)}</div>
              <div className="tank-pct-out">{pct}</div>
            </div>
          );
        }) : <div className="tank-empty">No tank sensors reported by the Cerbo</div>}
      </div>
    </div>
  );
});

// ── Heat buttons ──
const HQButton = memo(function HQButton({ b }: { b: HQDef }) {
  const v = useSelect(HQ_KEYS, () => hqView(b));
  const o = useOptimistic<boolean>(`hq.${b.k}`);
  const on = o ? o.value : v.on;
  const sending = o?.phase === "sending";
  return (
    <button className={`hq-btn${on ? " on" : ""}${v.active && on ? " active" : ""}${v.locked ? " locked" : ""}${sending ? " sending" : ""}`}
      data-hq={b.k} disabled={!(v.usable && !v.locked)} title={v.locked ? "Heat locked out: house battery low" : undefined}
      onClick={() => hqTap(b)}>
      <Icon name={b.ic} /><span className="hq-l">{b.label}</span>
      {v.locked && <span className="hq-lock"><Icon name="lock" /></span>}
    </button>
  );
});
const HeatQuickCard = memo(function HeatQuickCard() {
  return <div className="card heat-quick-card"><div className="hq-grid" id="heat-quick">{HQ.map(b => <HQButton key={b.k} b={b} />)}</div></div>;
});

// ── Lights & switches (the channels marked "home", up to 9; demo placeholders until there are any) ──
const QUICK_DEMO = [["Cabin", "light"], ["Galley", "light"], ["Porch", "light"], ["Reading", "light"], ["Water pump", "pump"],
  ["Vent fan", "fan"], ["Awning", "light"], ["Step", "light"], ["Accent", "light"]] as const;
const homeIds = (l: Sw[] | undefined) => (l ?? []).filter(s => s.home)
  .map((s, i) => [s.home_pos ?? 100 + i, s] as const).sort((a, b) => a[0] - b[0]).map(x => x[1].id).slice(0, 9);

const QuickBtn = memo(function QuickBtn({ id }: { id: string }) {
  const s = useKeySelector<Sw[], { name: string; icon?: string; on: boolean; stale: boolean } | null>("switches.list", l => {
    const x = l?.find(y => y.id === id);
    return x ? { name: x.name, icon: x.icon, on: !!x.on, stale: x.state === "stale" } : null;
  });
  const o = useOptimistic<boolean>(swOptId(id));
  if (!s) return null;
  const on = o ? o.value : s.on;
  return (
    <button className={`quick-btn${on ? " on" : ""}${s.stale ? " na" : ""}${o?.phase === "sending" ? " sending" : ""}`} data-sw={id}
      onPointerDown={e => holdStart(e.pointerId, id)}
      onClick={() => { if (heldTile(id)) return; toggleSwitch(id); }}>
      <Icon name={s.icon || "toggle"} /><span>{s.name}</span>
    </button>
  );
});

const QuickCard = memo(function QuickCard() {
  const ids = useKeySelector<Sw[], string[]>("switches.list", homeIds);
  const live = useSelect(ctlKeys("n2k"), () => canControlDev("n2k"));
  const [demoOn, setDemoOn] = useState<ReadonlySet<number>>(new Set());
  return (
    <div className="card quick-card">
      <div className="card-h"><h2>Lights &amp; switches</h2>
        <span className={`demo-tag${ids.length && live ? " live-tag" : ""}`} data-goto="switches">{ids.length ? (live ? "live" : "view only") : "demo · not connected"}</span></div>
      <div className="quick-grid" id="quick-sw">
        {ids.length ? ids.map(id => <QuickBtn key={id} id={id} />)
          // demo placeholders only: toggle on screen, send nothing
          : QUICK_DEMO.map(([n, ic], i) => (
            <button key={i} className={`quick-btn${demoOn.has(i) ? " on" : ""}`} data-i={i}
              onClick={() => setDemoOn(p => { const q = new Set(p); if (q.has(i)) q.delete(i); else q.add(i); return q; })}>
              <Icon name={ic} /><span>{n}</span>
            </button>))}
      </div>
    </div>
  );
});

// ── Battery, last 24 h ──
const SOC_SERIES_KEYS = ["victron.battery.soc"];
const SocSub = memo(function SocSub() {
  const ttg = useKey<number | null>("victron.battery.ttg");
  return <span className="muted" id="soc-sub">{ttg ? `${fmtTTG(ttg)} remaining at current draw` : ""}</span>;
});
const SocCard = memo(function SocCard() {
  const h = useHistory(SOC_SERIES_KEYS, 86400);
  return (
    <div className="card soc-card">
      <div className="card-h"><h2>Battery, last 24 h</h2><SocSub /></div>
      <TimeChart id="chart-soc" series={[{ name: "State of charge", color: "var(--c-batt)", points: h?.["victron.battery.soc"] }]}
        yMin={0} yMax={100} yTicks={SOC_TICKS} area fmt={pctFmt} />
    </div>
  );
});
const SOC_TICKS = [0, 25, 50, 75, 100];
const pctFmt = (v: number) => `${Math.round(v)}%`;

export default function HomePage() {
  return (
    <div className="home-grid">
      <FlowCard />
      <ClimateCard />
      <TanksCard />
      <HeatQuickCard />
      <QuickCard />
      <SocCard />
    </div>
  );
}
