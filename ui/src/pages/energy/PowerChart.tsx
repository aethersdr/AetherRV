// Energy → Power chart (app.js renderCharts "energy"): solar and consumption (DC + AC loads summed per bucket) over
// 6 h / 24 h / 7 d / 30 d. Only this card re-renders when its history or range changes. ?power=604800 sets the
// initial range, as app.js.
import { memo, useMemo, useState } from "react";
import { TimeChart, type Series } from "../../charts/TimeChart";
import { fmtW } from "../../lib/format";
import { useHistory } from "../../state/history";
import { sumSeries } from "./bank";

const KEYS = ["victron.pv.power", "energy.dc.load", "victron.ac.load"];
const RANGES = [[21600, "6 h"], [86400, "24 h"], [604800, "7 d"], [2592000, "30 d"]] as const;
const initialSpan = () => (typeof location === "undefined" ? 86400 : +(new URLSearchParams(location.search).get("power") || 86400));
const LEGEND = [["Solar", "var(--c-solar)"], ["Consumption", "var(--c-load)"]] as const;

export const PowerChart = memo(function PowerChart() {
  const [span, setSpan] = useState(initialSpan);
  const h = useHistory(KEYS, span);
  const series: Series[] = useMemo(() => [
    { name: "Solar", color: "var(--c-solar)", points: h?.["victron.pv.power"] },
    { name: "Consumption", color: "var(--c-load)", points: h ? sumSeries(h["energy.dc.load"], h["victron.ac.load"]) : undefined },
  ], [h]);
  return (
    <div className="card wide pwr-chart">
      <div className="card-h"><h2>Power</h2>
        <div className="legend" id="legend-power">{LEGEND.map(([n, c]) => <span key={n}><i style={{ background: c }} />{n}</span>)}</div>
        <div className="seg range" id="range-power">
          {RANGES.map(([s, l]) => <button key={s} data-s={s} className={s === span ? "on" : ""} onClick={() => setSpan(s)}>{l}</button>)}
        </div>
      </div>
      <TimeChart id="chart-power" className="tall" series={series} area fmt={fmtW} />
    </div>
  );
});
