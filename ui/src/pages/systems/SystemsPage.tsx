// Systems page (web/index.html #systems + app.js renderSystems): status cards, the NMEA 2000 device table and the
// bus inspector. Every card / row subscribes to its own keys and selects only the strings it shows, so a bus's
// frame counter ticking re-renders that card, and a device's `age` changing re-renders that row.
//
// New over app.js (Rust gateway only; nothing changes with the Python backend): a "Modules" card from the gateway's
// svc.<service>.up / age_s keys, and the Cerbo / Espar cards say "module down" instead of "not connected" when
// it's the service, not the device, that is gone.
import "./systems.css";
import { memo, useCallback, useEffect, useRef, useState, useSyncExternalStore, type ReactNode } from "react";
import { Icon } from "../../components/Icon";
import { prefs } from "../../lib/prefs";
import { useKeySelector, useReady, useSelect } from "../../state/hooks";
import { deepEqual, store, type Getter } from "../../state/store";
import { busCard, busNames, cellText, cerboCard, devSub, devView, esparCard, INSPECT_TABS, inspectKey, modules, sampleRates,
  sparkPath, targetCard, txGateCard, type Card, type Dev, type DevView, type Mod, type RateHist, type Target } from "./sys";

// ── CAN bus rate history: sampled once a second for as long as the app runs (as app.js), so a sparkline is
// already drawn when the page opens. ──
export const rateHist: RateHist = new Map();
const rateListeners = new Set<() => void>();
if (import.meta.env.MODE !== "test") {
  setInterval(() => {
    sampleRates(rateHist, busNames(store.keys("bus.")), store.get);
    rateListeners.forEach(f => f());
  }, 1000);
}

/** A value derived from every key under a prefix, re-rendering only when the derived value changes. */
function usePrefixSelect<T>(prefix: string, select: (keys: string[], get: Getter) => T): T {
  const cache = useRef<{ v: T } | null>(null);
  const sel = useRef(select);
  sel.current = select;
  const sub = useCallback((cb: () => void) => store.subscribePrefix(prefix, cb), [prefix]);
  const get = useCallback(() => {
    const v = sel.current(store.keys(prefix), store.get);
    if (cache.current && deepEqual(cache.current.v, v)) return cache.current.v;
    cache.current = { v };
    return v;
  }, [prefix]);
  return useSyncExternalStore(sub, get, get);
}

/** One status card (app.js card()): icon, title, a status line with a coloured dot, optional extras. */
function SysCard({ ic, title, sub, c, children, id }: { ic: string; title: string; sub?: string; c: Card; children?: ReactNode; id?: string }) {
  return (
    <div className="card sys" data-sys={id}>
      <Icon name={ic} />
      <h3>{title}{sub != null && <> <span className="muted">· {sub}</span></>}</h3>
      <div className={`st ${c.cls}`}><i />{c.status}</div>
      {children}
    </div>
  );
}

const READY_IDLE: Card = { cls: "idle", status: "—" };       // before the first snapshot: no false "not connected"

const CerboCard = memo(function CerboCard() {
  const ready = useReady();
  const c = useSelect(["victron.connected", "victron.portal", "svc.victron.up"], cerboCard);
  return <SysCard id="cerbo" ic="wifi" title="Victron Cerbo GX" c={ready ? c : READY_IDLE} />;
});

const BusCard = memo(function BusCard({ name }: { name: string }) {
  const keys = ["channel", "connected", "rate", "frames", "ids", "errors"].map(k => `bus.${name}.${k}`);
  const v = useSelect(keys, g => busCard(g, name));
  const path = useRef<SVGPathElement>(null);
  useEffect(() => {                                  // the sparkline is drawn straight into the SVG, once a second
    const draw = () => { const d = sparkPath(rateHist.get(name) ?? []); if (path.current && path.current.getAttribute("d") !== d) path.current.setAttribute("d", d); };
    draw();
    rateListeners.add(draw);
    return () => { rateListeners.delete(draw); };
  }, [name]);
  return (
    <SysCard id={`bus-${name}`} ic="can" title={name} sub={v.channel} c={v}>
      <div className="spark"><svg viewBox="0 0 300 44" preserveAspectRatio="none" className="spark"><path ref={path} /></svg></div>
      <div className="meta">{v.meta}</div>
    </SysCard>
  );
});

const TargetCard = memo(function TargetCard({ i }: { i: number }) {
  const v = useKeySelector<Target[] | null, ReturnType<typeof targetCard> | null>("netmon.targets", l => (l?.[i] ? targetCard(l[i]!) : null));
  if (!v) return null;
  return <SysCard id={`net-${i}`} ic="router" title={v.name} sub={v.host} c={v}>{v.hint && <div className="meta">{v.hint}</div>}</SysCard>;
});

const ESPAR_KEYS = ["heard", "asleep", "state", "controller", "last_seen", "fault"].map(k => `climate.espar.${k}`)
  .concat(["bus.hat1.channel", "espar.ids", "svc.espar.up", "tx.enabled.espar"]);
const EsparCard = memo(function EsparCard() {
  const [now, setNow] = useState(() => Date.now());
  const v = useSelect(ESPAR_KEYS, g => esparCard(g, now));
  // "last heard 5 min ago" keeps counting while the bus sleeps (no key changes then)
  const asleep = v.asleepSince != null;
  useEffect(() => {
    if (!asleep) return;
    setNow(Date.now());                              // fresh when the page is re-shown or the bus has just gone to sleep
    const t = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(t);
  }, [asleep]);
  if (!v.show) return null;
  return <SysCard id="espar" ic="flame" title="Espar" sub={v.channel} c={v}><div className="meta">{v.meta}</div></SysCard>;
});

const TxGateCard = memo(function TxGateCard() {
  const ready = useReady();
  const c = useSelect(["tx.enabled", "tx.blocked"], txGateCard);
  return <SysCard id="tx" ic="lock" title="Transmit gate" c={ready ? c : READY_IDLE} />;
});

const ModulesCard = memo(function ModulesCard() {
  const mods = usePrefixSelect<Mod[]>("svc.", modules);
  if (!mods.length) return null;
  const down = mods.filter(m => !m.up);
  const c: Card = down.length ? { cls: "bad", status: `${down.map(m => m.name).join(", ")} down` } : { cls: "good", status: `all ${mods.length} running` };
  return (
    <SysCard id="modules" ic="chip" title="Modules" c={c}>
      <div className="meta sys-mods">
        {mods.map(m => <span key={m.name} className={`sys-mod ${m.up ? "up" : "down"}`}><i />{m.name}{m.down && ` · ${m.down}`}</span>)}
      </div>
    </SysCard>
  );
});

const SysCards = memo(function SysCards() {
  const buses = usePrefixSelect("bus.", keys => busNames(keys));
  const targets = useKeySelector<Target[] | null, number>("netmon.targets", l => l?.length ?? 0);
  return (
    <div className="sys-grid" id="sys-cards">
      <CerboCard />
      {buses.map(n => <BusCard key={n} name={n} />)}
      {Array.from({ length: targets }, (_, i) => <TargetCard key={i} i={i} />)}
      <EsparCard />
      <TxGateCard />
      <ModulesCard />
    </div>
  );
});

// ── NMEA 2000 devices ──
const DevRow = memo(function DevRow({ i }: { i: number }) {
  const d = useKeySelector<Dev[] | null, DevView | null>("n2k.devices", l => devView(l?.[i]));
  if (!d) return null;
  return (
    <tr className={d.online ? "" : "offline"}>
      <td><span className={`st-dot ${d.online ? "good" : "bad"}`}><Icon name={d.online ? "check" : "alert"} />{d.online ? "online" : "OFFLINE"}</span></td>
      <td><b>{d.model}</b>{d.serial && <small> · {d.serial}</small>}</td>
      <td>{d.maker}</td><td>{d.cls}</td><td className="mono">{d.addr}</td>
      <td className="mono">{d.sw}</td><td>{d.heard}</td>
    </tr>
  );
});

const DevicesCard = memo(function DevicesCard() {
  const ready = useReady();
  const n = useKeySelector<Dev[] | null, number>("n2k.devices", l => l?.length ?? 0);
  const sub = useKeySelector<Dev[] | null, string>("n2k.devices", l => devSub(l ?? undefined));
  return (
    <div className="card devices-card">
      <div className="card-h"><h2>NMEA 2000 devices</h2><span className="muted" id="dev-sub">{sub}</span></div>
      <div className="table-wrap short"><table className="devs" id="dev-table"><tbody>
        {n ? <>
          <tr><th></th><th>Device</th><th>Maker</th><th>Class</th><th>Addr</th><th>Software</th><th>Last heard</th></tr>
          {Array.from({ length: n }, (_, i) => <DevRow key={i} i={i} />)}
        </> : ready ? <tr><td className="empty">No NMEA 2000 devices heard yet.</td></tr> : null}
      </tbody></table></div>
    </div>
  );
});

// ── Bus inspector ──
type Row = Record<string, unknown>;
const InspRow = memo(function InspRow({ k, i, cols }: { k: string; i: number; cols: readonly string[] }) {
  const cells = useKeySelector<Row[] | null, string[]>(k, l => (l?.[i] ? cols.map(c => cellText(l[i]![c])) : []));
  return <tr>{cells.map((c, j) => <td key={j}>{c}</td>)}</tr>;
});

const InspectorCard = memo(function InspectorCard() {
  const ready = useReady();
  const [sel, setSel] = useState(() => prefs.get("vp.inspect", "n2k"));
  const k = inspectKey(sel);
  const shape = useKeySelector<Row[] | null, { n: number; cols: string[] }>(k, l => ({ n: l?.length ?? 0, cols: l?.length ? Object.keys(l[0]!) : [] }));
  const pick = (id: string) => { setSel(id); prefs.set("vp.inspect", id); };
  return (
    <div className="card">
      <div className="card-h"><h2>Bus inspector</h2>
        <div className="seg" id="inspect-sel">
          {INSPECT_TABS.map(([id, l]) => <button key={id} data-i={id} className={id === sel ? "on" : ""} onClick={() => pick(id)}>{l}</button>)}
        </div></div>
      <div className="table-wrap"><table className="ids" id="inspect-table"><tbody>
        {shape.n ? <>
          <tr>{shape.cols.map(c => <th key={c}>{c}</th>)}</tr>
          {Array.from({ length: shape.n }, (_, i) => <InspRow key={i} k={k} i={i} cols={shape.cols} />)}
        </> : ready ? <tr><td className="empty">No frames captured on this bus yet.</td></tr> : null}
      </tbody></table></div>
    </div>
  );
});

export default function SystemsPage() {
  return (
    <>
      <SysCards />
      <DevicesCard />
      <InspectorCard />
    </>
  );
}
