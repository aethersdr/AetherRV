// Battery bank card (app.js renderBattCard): the Lynx shunt for the bank, the EPOCH batteries over Bluetooth for each
// battery. Header, the 3D bank (WebGL, or the SVG scene without it), cell / temperature tiles and the per-battery
// table, each subscribed on its own: the batteries' `age` changes on every read, so every part selects only the strings
// it shows.
import { memo } from "react";
import { Icon } from "../../components/Icon";
import { fmtPct, socColor } from "../../lib/format";
import { useTweenText } from "../../lib/tween";
import { useKeySelector, useReady, useSelect } from "../../state/hooks";
import { BATT_AH, cellTiles, headView, rowView, type Ble, type RowView } from "./bank";
import { Bank } from "./Bank3D";

const HEAD_KEYS = ["batteries.ble", "victron.battery.soc", "victron.battery.power", "victron.battery.voltage",
  "victron.battery.current", "energy.battery.to_go_s", "energy.battery.to_go_dir", "victron.vebus.state", "victron.mppt.state"];
const pctFmt = (p: number) => fmtPct(p);

const BankHead = memo(function BankHead() {
  const v = useSelect(HEAD_KEYS, headView);
  const soc = useTweenText<HTMLSpanElement>(v.pct, pctFmt);       // the headline counts to a new value
  return (
    <div className="bc-head" id="bc-head">
      <div className="bc-soc" style={{ color: v.pct == null ? "var(--muted)" : "var(--ink)" }} title={v.pctTitle}><span ref={soc} /><small>%</small></div>
      <div className="bc-sum">
        <span className={`bc-state ${v.st[2]}`}>{v.st[1] ? <Icon name={v.st[1]} /> : null}{v.st[0]}</span>
        <span>{v.line1}</span>
        <span className="muted">{v.line2}</span>
      </div>
    </div>
  );
});

const CELL_KEYS = ["batteries.ble", "victron.battery.temp"];
const BankCells = memo(function BankCells() {
  const tiles = useSelect(CELL_KEYS, g => cellTiles(g<Ble[]>("batteries.ble") || [], g<number | null>("victron.battery.temp")));
  return (
    <div className="bc-cells" id="bc-cells">
      {tiles.map(([l, v, cls, title]) => <div key={l} title={title || undefined}><span>{l}</span><b className={cls || undefined}>{v}</b></div>)}
    </div>
  );
});

const BattRow = memo(function BattRow({ i }: { i: number }) {
  const r = useKeySelector<Ble[], RowView | null>("batteries.ble", l => rowView(l?.[i], socColor));
  if (!r) return null;
  if (!r.ok) return <tr className="na"><td><b>{r.name}</b></td><td colSpan={8} className="muted">{r.msg}</td></tr>;
  return (
    <tr>
      <td><b>{r.name}</b></td><td className="soc-c" style={{ color: r.socColor }}>{r.soc}</td>
      <td>{r.volts}</td><td>{r.amps}</td><td>{r.power}</td><td>{r.cycles}</td>
      <td>{r.ah}<small>/{BATT_AH}</small></td>
      <td title={r.tempTitle || undefined}>{r.temp === "—" ? "—" : <span className={r.cold ? "t-cold" : ""}>{r.temp}</span>}</td>
      <td className="mos"><i className={r.cfet ? "on" : "off"} title={`Charge MOS ${r.cfet ? "on" : "off"}`}>C</i>
        <i className={r.dfet ? "on" : "off"} title={`Discharge MOS ${r.dfet ? "on" : "off"}`}>D</i></td>
    </tr>
  );
});

const namesOf = (l: Ble[] | undefined) => (l ?? []).map(b => b.name);

export const BatteryCard = memo(function BatteryCard() {
  const names = useKeySelector<Ble[], string[]>("batteries.ble", namesOf);
  const ready = useReady();
  return (
    <div className="card batt-card" id="batt-card">
      <BankHead />
      {names.length ? <>
        <Bank key={names.join("|")} names={names} />
        <BankCells />
        <table className="bc-table">
          <thead><tr><th>Battery</th><th>SoC</th><th>Volts</th><th>Amps</th><th>Power</th><th>Cycles</th><th>Ah</th><th>Temp</th><th>MOS</th></tr></thead>
          <tbody id="bc-rows">{names.map((n, i) => <BattRow key={`${i}:${n}`} i={i} />)}</tbody>
        </table>
      </> : ready ? <div className="muted">No per-battery data (Bluetooth adapter off).</div> : null}
    </div>
  );
});
