// Settings page (web/index.html #settings + app.js renderSettings): the tab list and the open tab. Same tabs, rows,
// controls and confirmations as the reference; see SystemTabs.tsx (admin-helper tabs) and DeviceTabs.tsx (Starlink
// and the heaters' details). Reads: /api/settings (data.ts) and the live state; changes: settingsAction (the admin
// helper) or control() (the Starlink dish). The on-screen keyboard (Keyboard.tsx) is mounted here.
import "./settings.css";
import { memo, useState } from "react";
import { Icon } from "../../components/Icon";
import { useControl } from "../../state/hooks";
import { useSettingsInfo, useSettingsPolling, type SettingsInfo } from "./data";
import { EsparTab, Mcs7Tab, StarlinkTab } from "./DeviceTabs";
import { Keyboard } from "./Keyboard";
import { DeviceTab, NetworkTab, TailscaleTab, UpdatesTab } from "./SystemTabs";

export const SET_TABS = [["network", "Network", "wifi"], ["tailscale", "Tailscale", "ts"], ["starlink", "Starlink", "dish"], ["mcs7", "MCS7", "air_heat"],
  ["espar", "Espar", "flame"], ["updates", "Updates", "download"], ["device", "Device", "info"]] as const;
export type TabId = (typeof SET_TABS)[number][0];

/** Placeholder rows until /api/settings answers (app.js showed "Loading…"). */
const Skeleton = () => (
  <div className="set-skel" aria-busy="true" aria-label="Loading">
    <div className="set-h"><span className="sk sk-h" /></div>
    <div className="set-rows">{[0, 1, 2, 3, 4].map(i => (
      <div className="set-row" key={i}><div><span className="sk sk-b" /><span className="sk sk-s" /></div><span className="sk sk-v" /></div>))}</div>
  </div>
);

const NEEDS_HELPER: ReadonlySet<TabId> = new Set<TabId>(["device", "network", "tailscale", "updates"]);
const Body = memo(function Body({ tab, info }: { tab: TabId; info: SettingsInfo | null }) {
  // the heater and Starlink tabs read the live state only: they don't wait for the admin helper
  if (!info && NEEDS_HELPER.has(tab)) return <Skeleton />;
  switch (tab) {
    case "device": return <DeviceTab info={info!} />;
    case "network": return <NetworkTab info={info!} />;
    case "tailscale": return <TailscaleTab info={info!} />;
    case "updates": return <UpdatesTab info={info!} />;
    case "starlink": return <StarlinkTab />;
    case "mcs7": return <Mcs7Tab />;
    case "espar": return <EsparTab />;
  }
});

const Tabs = memo(function Tabs({ tab, setTab }: { tab: TabId; setTab: (t: TabId) => void }) {
  const ctl = useControl();
  return (
    <div className="card set-tabs" id="set-tabs">
      {SET_TABS.map(([id, label, ic]) => (
        <button key={id} className={`set-tab${tab === id ? " on" : ""}`} data-t={id} onClick={() => setTab(id)}><Icon name={ic} /><span>{label}</span></button>))}
      {!ctl && <div className="set-ro"><Icon name="lock" />View only — change settings from the touchscreen or your Tailscale devices</div>}
    </div>
  );
});

export default function SettingsPage() {
  useSettingsPolling();
  const info = useSettingsInfo();
  const [tab, setTab] = useState<TabId>("device");
  return (
    <div className="set-grid">
      <Tabs tab={tab} setTab={setTab} />
      <div className="card set-body" id="set-body"><Body tab={tab} info={info} /></div>
      <Keyboard />
    </div>
  );
}
