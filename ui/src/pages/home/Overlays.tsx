// Home overlays: battery bank (tap the battery ring) and inverter (tap the inverter glyph). Ported from app.js
// renderBattModal / renderInvModal. They subscribe only while open.
import { memo, useEffect, useRef, type ReactNode } from "react";
import { Icon } from "../../components/Icon";
import { battT, fmtT, fmtTTG, fmtW } from "../../lib/format";
import { canControlDev, control, ctlKeys } from "../../logic/control";
import { openModal } from "../../logic/ui";
import { useKey, usePrefix, useSelect } from "../../state/hooks";
import { K, store } from "../../state/store";

type Rec = Record<string, unknown>;
const n = (v: unknown) => v as number;
const a = (v: unknown) => v == null ? "—" : `${(+n(v)).toFixed(1)} A`;
const KV = ({ rows }: { rows: [string, string, string?][] }) => (
  <div className="battm-kv">{rows.map(([k, v, cls]) => <div key={k} className={cls || ""}><span>{k}</span><b>{v}</b></div>)}</div>
);

interface Ble {
  name: string; ok?: boolean; soc?: number | null; voltage?: number; current?: number | null; remaining_ah?: number | null;
  cycles?: number | null; cell_delta_mv?: number | null; temps?: number[]; cells?: number[]; error?: string | null;
  problem_code?: number | null; charge_fet?: boolean; discharge_fet?: boolean;
}

const BattBody = memo(function BattBody() {
  usePrefix("victron.");                     // the bank card reads many victron.* keys (only while open)
  usePrefix("batteries.");
  const exp = useKey<number>("ui.bms_expected") || 0;
  const S = store.get;
  const bl = S<Rec[]>("victron.batteries") || [];
  const mon = bl.find(b => b.kind === "monitor"), bms = bl.find(b => b.kind === "bms");
  const ble = S<Ble[]>("batteries.ble") || [];
  const parts: ReactNode[] = [];
  if (mon) {
    parts.push(
      <section className="battm-card" key="mon">
        <h4><Icon name="battery" />{String(mon.CustomName || mon.ProductName || "Battery monitor")}<small>bank measurement</small></h4>
        <div className="battm-big">{mon.Soc == null ? "—" : Math.round(n(mon.Soc)) + "%"}</div>
        <KV rows={[
          ["Voltage", mon["Dc/0/Voltage"] == null ? "—" : `${(+n(mon["Dc/0/Voltage"])).toFixed(2)} V`],
          ["Current", a(mon["Dc/0/Current"])],
          ["Power", fmtW(n(mon["Dc/0/Power"]))],
          ["Consumed", mon.ConsumedAmphours == null ? "—" : `${Math.round(n(mon.ConsumedAmphours))} Ah`],
          ["Time to go", fmtTTG(n(mon.TimeToGo))],
          ["Temperature", mon["Dc/0/Temperature"] == null ? "—" : fmtT(n(mon["Dc/0/Temperature"]))],
        ]} />
      </section>);
  }
  if (bms) {
    // read over Bluetooth (owner 2026-11-22): the BMS-Can module count is information, not a fault
    const bleN = S<number>("batteries.ble.reporting"), on = bms["System/NrOfModulesOnline"] as number | null | undefined;
    const short = !!exp && on != null && on < exp && bleN == null;
    const vmin = bms["System/MinCellVoltage"] as number | null | undefined, vmax = bms["System/MaxCellVoltage"] as number | null | undefined;
    const spread = vmin != null && vmax != null ? Math.round((vmax - vmin) * 1000) : null;
    const alarms = Object.keys((bms.active_alarms as Rec | undefined) || {});
    const blocking = `${bms["System/NrOfModulesBlockingCharge"] ? "charge " : ""}${bms["System/NrOfModulesBlockingDischarge"] ? "discharge" : ""}` || "none";
    parts.push(
      <section className="battm-card" key="bms">
        <h4><Icon name="chip" />{String(bms.CustomName || bms.ProductName || "BMS")}<small>BMS (CAN-bus)</small></h4>
        <div className={`battm-mods ${short ? "warn" : "ok"}`}><Icon name={short ? "alert" : "check"} />
          {bleN != null
            ? <><b>{bleN}{exp ? ` of ${exp}` : ""}</b> batteries via Bluetooth · {on ?? "—"} on the BMS CAN bus</>
            : <><b>{on ?? "—"}{exp ? ` of ${exp}` : ""}</b> batteries reporting</>}</div>
        <KV rows={[
          ["BMS SoC / SoH", `${bms.Soc == null ? "—" : Math.round(n(bms.Soc)) + "%"} / ${bms.Soh == null ? "—" : Math.round(n(bms.Soh)) + "%"}`],
          ["BMS current", a(bms["Dc/0/Current"])],
          ["Cells", vmin == null ? "—" : `${(+vmin).toFixed(3)}–${(+n(vmax)).toFixed(3)} V`],
          ["Cell spread", spread == null ? "—" : `${spread} mV`, spread != null && spread > 50 ? "warn" : ""],
          ["Cell temp", bms["System/MinCellTemperature"] == null ? "—" : `${fmtT(n(bms["System/MinCellTemperature"]))}–${fmtT(n(bms["System/MaxCellTemperature"]))}`],
          ["Charge limit", `${a(bms["Info/MaxChargeCurrent"])} @ ${bms["Info/MaxChargeVoltage"] == null ? "—" : (+n(bms["Info/MaxChargeVoltage"])).toFixed(1) + " V"}`],
          ["Discharge limit", a(bms["Info/MaxDischargeCurrent"])],
          ["Low-voltage cutoff", bms["Info/BatteryLowVoltage"] == null ? "—" : `${(+n(bms["Info/BatteryLowVoltage"])).toFixed(1)} V`],
          ["Blocking", blocking],
        ]} />
        <div className="battm-alarms">{alarms.length ? alarms.map(k => <span key={k} className="src warn-src">{k}</span>)
          : <span className="muted"><Icon name="check" /> No BMS alarms</span>}</div>
        {short && <p className="battm-note">The BMS reports {on} module(s) but {exp} batteries are installed.{(S<number>("batteries.ble.reporting") || 0) >= exp
          ? " Bluetooth reads all of them directly (below), so it's only their CAN comms link:" : ""} Check the battery comms daisy-chain and DIP switches (and that each battery's breaker is closed).</p>}
      </section>);
  }
  if (ble.length) {
    const cur = S<number>("batteries.ble.current");
    parts.push(
      <section className="battm-card battm-wide" key="ble">
        <h4><Icon name="battery" />Individual batteries<small>{S<number>("batteries.ble.reporting") ?? 0} of {ble.length} via Bluetooth · total {cur == null ? "—" : (+cur).toFixed(1) + " A"}</small></h4>
        <div className="battm-cellgrid">{ble.map(b => {
          const t = battT(b.temps || []);
          return (
            <div key={b.name} className={`battm-cell ${b.ok ? "" : "na"} ${b.problem_code || b.charge_fet === false || b.discharge_fet === false ? "bad" : ""}`}>
              <div className="battm-cell-h"><b>{b.name}</b><span>{b.ok ? (b.soc == null ? "—" : b.soc + "%") : "no data"}</span></div>
              {b.ok ? <>
                <div className="battm-cell-kv"><span>{(+n(b.voltage)).toFixed(2)} V</span><span>{b.current == null ? "—" : (+b.current).toFixed(1) + " A"}</span>
                  <span>{b.remaining_ah == null ? "—" : Math.round(b.remaining_ah) + " Ah"}</span><span>{b.cycles ?? "—"} cycles</span>
                  <span className={(b.cell_delta_mv || 0) > 50 ? "warn" : ""}>Δ {b.cell_delta_mv ?? "—"} mV</span>
                  <span>{t ? <><b className={t.cold ? "t-cold" : ""}>{fmtT(t.c)}</b> {t.cold ? "min" : "max"}</> : "—"}</span></div>
                <div className="battm-cells">{(b.cells || []).map((c, i) => <i key={i} title={`${c.toFixed(3)} V`}>{c.toFixed(3)}</i>)}</div>
              </> : <div className="muted small">{b.error || "waiting for first read"}</div>}
            </div>);
        })}</div>
      </section>);
  }
  return <div className="battm-grid" id="battm-body">{parts.length ? parts : <div className="muted">No battery data from the Cerbo.</div>}</div>;
});

export const BattModal = memo(function BattModal() {
  const open = useKey(K.modal) === "batt";
  const connected = useKey("victron.connected");
  if (!open) return <div id="batt-modal" className="modal" hidden />;
  return (
    <div id="batt-modal" className="modal" onClick={e => { if ((e.target as Element).id === "batt-modal") openModal(null); }}>
      <div className="modal-box batt-box">
        <div className="inv-head"><div><h3>Battery bank</h3><p id="battm-sub" className="muted">{connected ? "Live from the Cerbo GX" : "Cerbo GX not connected"}</p></div>
          <button className="btn-soft" id="battm-close" onClick={() => openModal(null)}>Close</button></div>
        <BattBody />
      </div>
    </div>
  );
});

// ── Inverter overlay ──
const VEBUS_STATE: Record<number, string> = { 0: "Off", 1: "Low power", 2: "Fault", 3: "Bulk", 4: "Absorption", 5: "Float", 6: "Storage",
  7: "Equalize", 8: "Passthru", 9: "Inverting", 10: "Power assist", 11: "Power supply", 244: "Sustain", 252: "External control" };
const INVM_MODES = [[3, "On", "Inverts to the AC outlets and charges from shore"], [1, "Charger only", "Charges from shore; outlets not powered from the battery"],
  [2, "Inverter only", "Outlets powered from the battery; shore ignored"], [4, "Off", "Outlets dead, no charging"]] as const;
const VB = (k: string) => `victron.vebus.${k}`;
const INV_KEYS = [VB("product"), VB("name"), VB("state"), VB("ac_out_w"), VB("mode"), VB("mode_adjustable"), VB("limit_adjustable"),
  VB("current_limit"), K.limPending, ...ctlKeys("victron")];
let limTimer: ReturnType<typeof setTimeout> | undefined;

const InvBody = memo(function InvBody() {
  const v = useSelect(INV_KEYS, g => ({
    title: g<string>(VB("product")) || g<string>(VB("name")) || "MultiPlus",
    state: g<number>(VB("state")), outW: g<number>(VB("ac_out_w")), mode: g<number>(VB("mode")),
    modeAdj: !!g(VB("mode_adjustable")), adj: g(VB("limit_adjustable")) === true,
    lim: g<number | null>(K.limPending) ?? g<number | null>(VB("current_limit")) ?? null,
    limPending: g<number | null>(K.limPending) ?? null,
    ok: canControlDev("victron") && !g(K.pending),
    why: g(K.control) !== true ? " · view only" : !g("victron.controls_ok") ? " · control off" : g(K.pending) ? " · sending…" : "",
  }));
  const slider = useRef<HTMLInputElement>(null);
  const dragging = useRef(false);
  // the slider shows the device's limit on open, and follows it while nothing is being adjusted
  useEffect(() => {
    if (slider.current && !dragging.current && v.limPending == null) slider.current.value = String(Math.round(v.lim ?? 30));
  }, [v.lim, v.limPending]);
  const lim = v.lim;
  return <>
    <div className="inv-head"><div><h3 id="invm-title">{v.title}</h3>
      <p id="invm-state" className="muted">{`${(v.state != null ? VEBUS_STATE[v.state] : undefined) ?? "—"}${v.outW != null ? ` · ${fmtW(v.outW)} AC out` : ""}${v.why}`}</p></div>
      <button className="btn-soft" id="invm-close" aria-label="Close" onClick={() => openModal(null)}>Close</button></div>
    <div className="invm-modes" id="invm-modes">
      {INVM_MODES.map(([m, l, d]) => (
        <button key={m} className={`invm-mode ${v.mode === m ? "on" : ""}`} data-m={m} disabled={!(v.ok && v.modeAdj && v.mode !== m)}
          onClick={() => control("inverter_mode", m, `MultiPlus ${l}`, "victron")}>
          <b>{l}</b><span>{d}</span></button>))}
    </div>
    <div className="invm-lim">
      <div className="invm-lim-h"><b>Shore current limit</b><span id="invm-lim-val">{lim == null ? "—" : `${Math.round(lim)} A`}</span></div>
      <input type="range" id="invm-lim" min={3} max={50} step={1} ref={slider} disabled={!(v.ok && v.adj)}
        onPointerDown={() => { dragging.current = true; }}
        onPointerUp={() => { dragging.current = false; }} onPointerCancel={() => { dragging.current = false; }}
        onInput={e => store.set(K.limPending, +(e.target as HTMLInputElement).value)}
        onChange={e => {
          const x = +(e.target as HTMLInputElement).value;
          dragging.current = false;
          store.set(K.limPending, x);
          clearTimeout(limTimer);
          limTimer = setTimeout(() => control("shore_limit", x, `Shore limit ${x} A`, "victron"), 600);
        }} />
      <div className="invm-lim-scale"><span style={{ left: "calc(24px + 0.00% - 0.0px)" }}>3 A</span><span style={{ left: "calc(24px + 25.53% - 12.3px)" }}>15</span>
        <span style={{ left: "calc(24px + 36.17% - 17.4px)" }}>20</span><span style={{ left: "calc(24px + 57.45% - 27.6px)" }}>30</span>
        <span style={{ left: "calc(24px + 100.00% - 48.0px)" }}>50 A</span></div>
      <div className="muted" id="invm-lim-note">{v.adj ? "Match the shore pedestal / cord rating (e.g. 15 A or 30 A)."
        : "Adjustable while the MultiPlus is on (the Cerbo reports it not adjustable right now)."}</div>
    </div>
  </>;
});

export const InvModal = memo(function InvModal() {
  const open = useKey(K.modal) === "inv";
  if (!open) return <div id="inv-modal" className="modal" hidden />;
  return (
    <div id="inv-modal" className="modal" onClick={e => { if ((e.target as Element).id === "inv-modal") openModal(null); }}>
      <div className="modal-box inv-box"><InvBody /></div>
    </div>
  );
});
