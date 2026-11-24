// Alerts: active alerts (acknowledge, the per-alert actions), sound toggle, acknowledge all, recent events.
// Ported from app.js renderAlerts and its click handler.
import { memo, useEffect, useState } from "react";
import { Icon } from "../../components/Icon";
import { ago, fmtT } from "../../lib/format";
import { activeAlerts, SEV_LABEL, SOUND_KEY, SRC_ICON, toggleSound, type Alert, type AlertLog } from "../../logic/alerts";
import { canControlDev, control, ctlKeys, settingsAction } from "../../logic/control";
import { clearOptimistic, setOptimistic, useOptimistic } from "../../logic/optimistic";
import { askConfirm, toast } from "../../logic/ui";
import { useKey, useSelect } from "../../state/hooks";
import { K, store } from "../../state/store";
import { gateway } from "../../state/ws";
import type { Sw } from "../../logic/switches";

const ACKED = { ok: true, result: "ok", detail: "" } as const;
const ackMatches = (id: string) => () => !!activeAlerts().find(a => a.id === id)?.acked || !activeAlerts().some(a => a.id === id);
/** Acknowledge (AetherRV's own UI state, never a device): shown as acknowledged at once. */
function ack(ids: string[], all = false): void {
  const sent = all ? gateway.ackAll() : gateway.ack(ids[0]!);
  if (!sent) { toast("Acknowledge: not sent — no connection", "bad"); return; }
  for (const id of ids) setOptimistic(`alert.${id}`, true, Promise.resolve(ACKED), ackMatches(id), ["alerts.active"]);
}

/** Re-render every `ms` (the "since N min ago" labels). */
function useTick(ms: number) {
  const [, set] = useState(0);
  useEffect(() => { const t = setInterval(() => set(x => x + 1), ms); return () => clearInterval(t); }, [ms]);
}
const Since = ({ t }: { t: number }) => { useTick(30_000); return <span className="muted">since {ago(t)}</span>; };

const AlertCard = memo(function AlertCard({ a }: { a: Alert }) {
  const o = useOptimistic<boolean>(`alert.${a.id}`);
  const acked = !!a.acked || !!o?.value;
  const r = useSelect([K.control, ...ctlKeys("n2k", "victron")], g => ({
    ctl: g(K.control) === true, n2k: canControlDev("n2k"), victron: canControlDev("victron"),
  }));
  const cold = /^cold\.(fridge|freezer)$/.test(a.id) ? a.id.slice(5) as "fridge" | "freezer" : null;
  const trip = /^switch\.trip\.\d+\.\d+$/.test(a.id) ? +a.id.split(".").pop()! : null;
  const cerbo = /^cerbo\.notif\.\d+$/.test(a.id) && !/^Acknowledged/.test(a.detail);
  const onCold = () => {
    if (!cold) return;
    const L = cold === "fridge" ? "Fridge" : "Freezer";
    askConfirm(`${L} switched off?`, `Warm alerts for the ${cold} stop now and turn back on by themselves once it's cold again (below ${fmtT(store.get<number>(`ui.${cold}_max_c`))}).`,
      () => void settingsAction("pref", { key: `${cold}_off`, value: true }, `${L} off`));
  };
  const onTrip = () => {
    if (trip == null) return;
    const s = (store.get<Sw[]>("switches.list") ?? []).find(x => x.channel === trip);
    askConfirm(`Reset ${s ? s.name : "channel " + trip}?`, "This switches the channel off, which clears the tripped breaker in the load controller. It stays off: check the load and its current limit before switching it on again.",
      () => control("set", { channel: trip, on: false }, `${s ? s.name : "Channel " + trip} reset`, "n2k"));
  };
  return (
    <div className={`alert-card sev-${a.severity} ${acked ? "acked" : ""}`}>
      <div className="alert-ic"><Icon name={a.severity === "warning" ? "alert" : SRC_ICON[a.source ?? ""] || "alert"} /></div>
      <div className="alert-body"><div className="alert-sev"><Icon name="alert" />{SEV_LABEL[a.severity]}{acked ? " · acknowledged" : ""}</div>
        <h3>{a.title}</h3><p>{a.detail}</p><Since t={a.since} /></div>
      <div className="alert-btns">
        {cold && r.ctl && <button className="btn-soft" data-cold-off={cold} onClick={onCold}><Icon name="toggle" />{cold === "fridge" ? "Fridge" : "Freezer"} is off</button>}
        {trip != null && r.n2k && <button className="btn-soft" data-trip={trip} onClick={onTrip}><Icon name="toggle" />Reset breaker</button>}
        {cerbo && r.victron && <button className="btn-soft" data-cerbo-ack="1" onClick={() => control("ack_alarms", 1, "Cerbo alarm acknowledged", "victron")}><Icon name="bell" />Acknowledge on Cerbo</button>}
        {!acked && <button className="btn-soft" data-ack={a.id} onClick={() => ack([a.id])}>Acknowledge</button>}
      </div>
    </div>
  );
});

const Head = memo(function Head() {
  const s = useSelect(["alerts.active"], () => {
    const act = activeAlerts();
    return { n: act.length, unacked: act.filter(a => !a.acked).map(a => a.id) };
  });
  const sound = useKey(SOUND_KEY) === true;
  return (
    <div className="alerts-head">
      <div id="alerts-summary" className="muted">{s.n ? `${s.n} active · ${s.unacked.length} unacknowledged` : ""}</div>
      <div className="alerts-actions">
        <button className="btn-soft" id="btn-sound" onClick={toggleSound}><Icon name={sound ? "bell" : "bellOff"} />{sound ? "Sound on" : "Sound off"}</button>
        <button className="btn-soft" id="btn-ackall" disabled={!s.unacked.length} onClick={() => ack(s.unacked, true)}>Acknowledge all</button>
      </div>
    </div>
  );
});

const List = memo(function List() {
  const act = useKey<Alert[]>("alerts.active") ?? [];
  // a cleared alert's overlay goes with it
  useEffect(() => { for (const k of store.keys("$opt.alert.")) if (!act.some(a => `$opt.alert.${a.id}` === k)) clearOptimistic(k.slice(5)); }, [act]);
  return (
    <div id="alerts-list" className="alerts-list">
      {act.length ? act.map(a => <AlertCard key={a.id} a={a} />)
        : <div className="all-clear"><Icon name="check" /><div><h3>All clear</h3><span className="muted">No active alerts.</span></div></div>}
    </div>
  );
});

const fmtWhen = (t: number) => new Date(t * 1000).toLocaleString([], { weekday: "short", hour: "numeric", minute: "2-digit" });
const Log = memo(function Log() {
  const log = useKey<AlertLog[]>("alerts.log") ?? [];
  return (
    <div className="card">
      <div className="card-h"><h2>Recent events</h2></div>
      <div className="table-wrap short"><table className="devs" id="alerts-log"><tbody>
        {log.length ? <>
          <tr><th>When</th><th>Event</th><th>Severity</th><th>Alert</th></tr>
          {log.map((e, i) => (
            <tr key={`${e.t}-${e.id}-${e.event}-${i}`}><td>{fmtWhen(e.t)}</td><td>{e.event}</td>
              <td><span className={`sev-t sev-${e.severity}`}>{SEV_LABEL[e.severity] || e.severity}</span></td><td>{e.title}</td></tr>))}
        </> : <tr><td className="empty">No alert history yet.</td></tr>}
      </tbody></table></div>
    </div>
  );
});

export default function AlertsPage() {
  return <><Head /><List /><Log /></>;
}
