// Climate page (web/index.html #climate + app.js renderClimate / renderCharts): the cabin dial with the fan slider,
// the MCS7 and Espar heater cards, refrigeration, and the temperature history. Same grid and contents as the
// reference; every part subscribes to its own keys (see DialCard.tsx and Heaters.tsx for the control rules).
import "./climate.css";
import { memo, useMemo, useState } from "react";
import { TimeChart, type Point } from "../../charts/TimeChart";
import { Icon } from "../../components/Icon";
import { fmtT, tC } from "../../lib/format";
import { useHistory } from "../../state/history";
import { useSelect } from "../../state/hooks";
import type { Getter } from "../../state/store";
import { DialCard } from "./DialCard";
import { Heaters } from "./Heaters";
import { deg, useNumText } from "./num";

// ── Refrigeration (Ruuvi tags): icon + label, never colour alone ──
export function coldStatus(gt: Getter, role: string): { cls: string; label: string } {
  const t = gt<number>(`climate.${role}.temp`), max = gt<number>(`ui.${role}_max_c`);
  if (t == null) return { cls: "", label: "no sensor" };
  if (gt(`settings.${role}_off`)) return { cls: "", label: "off" };
  if (max != null && t > max) return { cls: "warn", label: `above ${fmtT(max)}` };
  return { cls: "ok", label: "ok" };
}
const ColdItem = memo(function ColdItem({ role, name, ic }: { role: string; name: string; ic: string }) {
  const v = useSelect([`climate.${role}.temp`, `ui.${role}_max_c`, `settings.${role}_off`], gt => ({
    t: gt<number | null>(`climate.${role}.temp`) ?? null, max: gt<number | null>(`ui.${role}_max_c`) ?? null, st: coldStatus(gt, role),
  }));
  const ref = useNumText<HTMLElement>(tC(v.t), deg);
  return (
    <div className={`cold-item ${v.st.cls}`}>
      <Icon name={v.st.cls === "warn" ? "alert" : ic} />
      <div><b ref={ref} /><span>{`${name} · ${v.st.cls === "ok" ? `below ${fmtT(v.max)}` : v.st.label}`}</span></div>
    </div>
  );
});
const ColdCard = memo(function ColdCard() {
  return (
    <div className="card cold" id="cold">
      <div className="card-h"><h2>Refrigeration</h2><span className="muted">Ruuvi tags</span></div>
      <div className="cold-row"><ColdItem role="fridge" name="Fridge" ic="fridge" /><ColdItem role="freezer" name="Freezer" ic="snow" /></div>
    </div>
  );
});

// ── Temperature history (cabin + outside), 24 h / 7 d / 30 d; ?temp=<seconds> sets the first range (as app.js) ──
const TEMP_KEYS = ["climate.cabin.temp", "climate.outside.temp"];
const SPANS = [[86400, "24 h"], [604800, "7 d"], [2592000, "30 d"]] as const;
const firstSpan = typeof location === "undefined" ? 86400 : +(new URLSearchParams(location.search).get("temp") || 86400);
const conv = (pts: readonly Point[] | undefined): Point[] => (pts ?? []).map(p => [p[0], tC(p[1])!] as const);
export const tempFmt = (v: number) => `${Number.isInteger(+(+v).toFixed(1)) ? Math.round(v) : (+v).toFixed(1)}°`;
const TempCard = memo(function TempCard() {
  const [span, setSpan] = useState(firstSpan);
  const h = useHistory(TEMP_KEYS, span);
  const series = useMemo(() => [
    { name: "Cabin", color: "var(--c-cabin)", points: conv(h?.["climate.cabin.temp"]) },
    { name: "Outside", color: "var(--c-out)", points: conv(h?.["climate.outside.temp"]) },
  ], [h]);
  return (
    <div className="card wide">
      <div className="card-h"><h2>Temperature</h2>
        <div className="seg range" id="range-temp">
          {SPANS.map(([s, l]) => <button key={s} data-s={s} className={span === s ? "on" : undefined} onClick={() => setSpan(s)}>{l}</button>)}
        </div></div>
      <div className="legend" id="legend-temp">{series.map(s => <span key={s.name}><i style={{ background: s.color }} />{s.name}</span>)}</div>
      <TimeChart id="chart-temp" series={series} zero={false} fmt={tempFmt} />
    </div>
  );
});

export default function ClimatePage() {
  return (
    <div className="climate-grid">
      <DialCard />
      <Heaters />
      <ColdCard />
      <TempCard />
    </div>
  );
}
