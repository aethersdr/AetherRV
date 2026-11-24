// Energy page (web/index.html #energy + app.js renderEnergy): the battery bank card (3D scene, cells, per-battery
// table), the reading tiles, the inverter & solar controls and the power chart. Same grid and contents as the
// reference; every card subscribes to its own keys.
import "./energy.css";
import { BatteryCard } from "./BatteryCard";
import { PowerChart } from "./PowerChart";
import { PowerControls } from "./PowerControls";
import { StatGrid } from "./Stats";

export default function EnergyPage() {
  return (
    <div className="energy-grid">
      <BatteryCard />
      <StatGrid />
      <PowerControls />
      <PowerChart />
    </div>
  );
}
