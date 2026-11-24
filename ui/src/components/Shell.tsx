// The app chrome, same layout as web/index.html: the left rail (nav, Settings pinned at the bottom) and the top bar
// (title, chips, night toggle, clock). Each chip subscribes to its own keys only.
import { memo, useEffect, useState } from "react";
import logo from "../assets/logo.png";
import { activeAlerts, topSeverity } from "../logic/alerts";
import { canControl, canControlDev, devOkKey } from "../logic/control";
import { SCREEN_AVAIL, screenOff } from "../logic/kiosk";
import { mbps } from "../lib/format";
import { prefs } from "../lib/prefs";
import { useKey, useSelect } from "../state/hooks";
import { FOOT, go, useView, VIEWS, viewTitle, type ViewId } from "../state/route";
import { K, store } from "../state/store";
import { Icon } from "./Icon";

// ── Rail ──
const NavBadge = memo(function NavBadge() {
  const b = useSelect(["alerts.active"], () => {
    const unacked = activeAlerts().filter(a => !a.acked);
    return { n: unacked.length, top: topSeverity(unacked) };
  });
  return <i className={`nav-badge sev-${b.top}`} id="nav-badge" hidden={!b.n}>{b.n}</i>;
});

const NavButton = memo(function NavButton({ id, label, ic, on }: { id: ViewId; label: string; ic: string; on: boolean }) {
  return (
    <button data-v={id} className={on ? "on" : undefined} onClick={() => go(id)}>
      <Icon name={ic} /><span>{label}</span>{id === "alerts" && <NavBadge />}
    </button>
  );
});

export const Rail = memo(function Rail() {
  const view = useView();
  const btn = ([id, label, ic]: (typeof VIEWS)[number]) => <NavButton key={id} id={id} label={label} ic={ic} on={view === id} />;
  return (
    <aside className="rail">
      <div className="brand"><img className="brand-mark" src={logo} alt="AetherRV" /></div>
      <nav id="nav">{VIEWS.filter(v => !FOOT.has(v[0])).map(btn)}</nav>
      <nav id="nav-foot">{VIEWS.filter(v => FOOT.has(v[0])).map(btn)}</nav>
    </aside>
  );
});

// ── Top bar chips ──
const DemoChip = memo(function DemoChip() {
  const demo = useKey("demo.active");
  return <span id="chip-demo" className="chip warn" hidden={!demo}>DEMO DATA</span>;
});

const AlertChip = memo(function AlertChip() {
  const c = useSelect(["alerts.active"], () => {
    const act = activeAlerts(), unacked = act.filter(a => !a.acked);
    return {
      n: act.length,
      cls: unacked.length ? `sev-${topSeverity(unacked)}` : "sev-acked",
      text: unacked.length === 1 ? unacked[0]!.title : unacked.length ? `${unacked.length} alerts` : `${act.length} acknowledged`,
    };
  });
  return (
    <button id="chip-alert" className={`chip alert ${c.cls}`} data-goto="alerts" hidden={!c.n}>
      {c.n > 0 && <><Icon name="alert" />{c.text}</>}
    </button>
  );
});

interface Wan { cellular?: { network?: string; carrier?: string } | null; connected?: boolean }
const NET_KEYS = ["peplink.reachable", "starlink.reachable", "peplink.active_type", "peplink.wans", "starlink.online",
  "starlink.down_mbps", "peplink.active"];
const NetChip = memo(function NetChip() {
  const c = useSelect(NET_KEYS, g => {
    if (g("peplink.reachable") == null && g("starlink.reachable") == null) return null;
    const act = g<string>("peplink.active_type"), wans = g<Wan[]>("peplink.wans") ?? [];
    const cell = wans.find(w => w.cellular && w.connected)?.cellular;
    if (act === "starlink" || (!act && g("starlink.online"))) return { ic: "dish", t: `Starlink · ${mbps(g<number>("starlink.down_mbps"))} Mbps` };
    if (act === "cellular" && cell) return { ic: "signal", t: `${cell.network || ""} · ${cell.carrier || ""}` };
    if (g("peplink.active")) return { ic: "router", t: String(g("peplink.active")) };
    return { ic: "alert", t: "No internet" };
  });
  return (
    <button id="chip-net" className="chip" data-goto="network" hidden={!c}>
      {c && <><Icon name={c.ic} />{c.t}</>}
    </button>
  );
});

const CtlChip = memo(function CtlChip() {
  const on = useSelect([K.control, devOkKey("rixens"), devOkKey("victron")], () =>
    [canControl() && "heat", canControlDev("victron") && "power"].filter(Boolean).join(" + "));
  return <span id="chip-ro" className="chip">{on ? <><Icon name="check" />Control: {on}</> : <><Icon name="lock" />Read-only</>}</span>;
});

const LinkChip = memo(function LinkChip() {
  const conn = useKey<string>(K.conn) ?? "connecting";
  const ready = useKey(K.ready) === true;
  const up = conn === "live";
  // before the first snapshot ever: "Connecting…" (app.js said "Offline"); after a drop: "Offline"
  return <span id="chip-link" className="chip"><span className={`dot ${up ? "good" : "bad"}`} />{up ? "Live" : ready ? "Offline" : "Connecting…"}</span>;
});

const ScreenBtn = memo(function ScreenBtn() {
  const avail = useKey(SCREEN_AVAIL) === true;
  return <button id="btn-screen" className="chip btn" aria-label="Screen off" hidden={!avail} onClick={() => screenOff()}><Icon name="screen_off" /></button>;
});

export function setNight(on: boolean): void {
  document.documentElement.classList.toggle("night", on);
  prefs.set("vp.night", on ? "1" : "0");
  store.set(K.night, on);
}
const NightBtn = memo(function NightBtn() {
  const night = useKey(K.night) === true;
  return <button id="btn-night" className="chip btn" aria-label="Night mode" onClick={() => setNight(!night)}><Icon name={night ? "sun" : "moon"} /></button>;
});

/** Tap the clock to toggle °F/°C (reloads, as app.js). Only this re-renders every second. */
const Clock = memo(function Clock() {
  const [d, setD] = useState(() => new Date());
  useEffect(() => { const t = setInterval(() => setD(new Date()), 1000); return () => clearInterval(t); }, []);
  const toggleUnit = () => { prefs.set("vp.temp", prefs.get("vp.temp", "F") === "C" ? "F" : "C"); location.reload(); };
  return (
    <div className="clock" onClick={toggleUnit}>
      <div id="clock-t">{d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}</div>
      <div id="clock-d">{d.toLocaleDateString([], { weekday: "short", month: "short", day: "numeric" })}</div>
    </div>
  );
});

export const TopBar = memo(function TopBar() {
  const view = useView();
  return (
    <header className="topbar">
      <div className="title"><h1 id="page-title">{viewTitle(view)}</h1><span id="page-sub"></span></div>
      <div className="chips">
        <DemoChip /><AlertChip /><NetChip /><CtlChip /><LinkChip /><ScreenBtn /><NightBtn />
      </div>
      <Clock />
    </header>
  );
});
