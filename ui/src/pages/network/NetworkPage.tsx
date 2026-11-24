// Network (web/index.html #network + app.js renderNetwork / renderCharts): Starlink (status, speeds, stats, the
// 3D sky or 2D obstruction map, Install update), Cellular and WWAN side by side, the Peplink WAN priority list
// (P# picker and drag to reorder / enable / disable), and the active link's throughput with Starlink's SNR lane.
//
// Device commands (each asks first, as app.js did, and goes through control() from the Confirm button):
//   starlink install_update · peplink set_wan_priority · peplink set_wan_order.
// Everything else here is read-only. Each card subscribes to its own keys.
import "./network.css";
import { flushSync } from "react-dom";
import { memo, useRef, useState, type PointerEvent as RPointerEvent } from "react";
import { TimeChart, type Series } from "../../charts/TimeChart";
import { Icon } from "../../components/Icon";
import { mbps, num } from "../../lib/format";
import { useTweenText } from "../../lib/tween";
import { canControlDev, control, ctlKeys, ctlPending } from "../../logic/control";
import { askConfirm, toast } from "../../logic/ui";
import { useHistory } from "../../state/history";
import { useKey, useReady, useSelect } from "../../state/hooks";
import { K } from "../../state/store";
import { SkyFigure } from "./Sky";
import type { ObstructionMap } from "./sky3d";
import { dragResult, dur, INSTALL_ASK, locked, orderConfirm, priorityConfirm, rsrpQ, sinrQ, wanIcon, wanLists,
  type Allowance, type Wan } from "./wan";

// ── small pieces ──
/** Ring gauge 0–100 (charts.js ring); the arc eases (.ring-val transition). */
function Ring({ value, size, stroke, color }: { value: number; size: number; stroke: number; color: string }) {
  const r = (size - stroke) / 2, c = 2 * Math.PI * r, v = Math.max(0, Math.min(100, value ?? 0));
  return (
    <svg viewBox={`0 0 ${size} ${size}`} className="ring">
      <circle cx={size / 2} cy={size / 2} r={r} fill="none" stroke="var(--track)" strokeWidth={stroke} />
      <circle cx={size / 2} cy={size / 2} r={r} fill="none" stroke={color} strokeWidth={stroke} strokeLinecap="round"
        strokeDasharray={`${(c * v / 100).toFixed(1)} ${c.toFixed(1)}`} transform={`rotate(-90 ${size / 2} ${size / 2})`} className="ring-val" />
    </svg>
  );
}

function Bars({ level, max = 5 }: { level: number; max?: number }) {
  return <span className="bars">{Array.from({ length: max }, (_, i) => <i key={i} className={i < level ? "on" : ""} style={{ height: `${30 + i * 17}%` }} />)}</span>;
}

const NotEnabled = ({ title, what }: { title: string; what: string }) => (
  <><div className="card-h"><h2>{title}</h2></div><div className="muted">{what}</div></>
);

const fmtMbps = (v: number) => String(mbps(v));

// ── Starlink ──
interface SlWan { led: string; message: string }
const SL_TITLE_KEYS = ["starlink.online", "starlink.reachable", "starlink.state", "starlink.alerts", "starlink.heating", "peplink.wans"];
const SlTitle = memo(function SlTitle() {
  const v = useSelect(SL_TITLE_KEYS, g => {
    const on = g("starlink.online"), reach = g("starlink.reachable");
    // the router's view of the Starlink WAN when the Peplink is monitored (green = carrying traffic, amber =
    // standby), so it matches the Cellular / WWAN dots; else the dish's own state. Unreachable dish = red.
    const w = (g<Wan[]>("peplink.wans") ?? []).find(x => x.starlink && x.enabled);
    const slWan: SlWan | null = w ? { led: w.led ?? "gray", message: w.message ?? "" } : null;
    const dishTxt = on ? "dish online" : reach ? `dish ${String(g("starlink.state") || "").replace(/_/g, " ").toLowerCase()}` : "dish unreachable";
    return {
      led: !reach ? "led-red" : slWan ? `led-${slWan.led}` : on ? "led-green" : "led-yellow",
      tip: slWan ? `${slWan.message} · ${dishTxt}` : dishTxt,
      heating: (g<{ id: string }[]>("starlink.alerts") ?? []).some(a => a.id === "alert_is_heating"),
      heatMode: String(g("starlink.heating") || "auto"),
    };
  });
  // dish alerts go to the alert center; on the card only "heating" shows, as a small icon by the title
  return (
    <h2 className="sl-title" title={v.tip}><span className={`led ${v.led}`} />Starlink
      {v.heating && <span className="sl-heat" title={`Heating: melting snow/ice (${v.heatMode})`}><Icon name="heat_waves" /></span>}</h2>
  );
});

const SlSpeeds = memo(function SlSpeeds() {
  const down = useKey<number | null>("starlink.down_mbps"), up = useKey<number | null>("starlink.up_mbps");
  const dRef = useTweenText<HTMLElement>(down, fmtMbps), uRef = useTweenText<HTMLElement>(up, fmtMbps);
  return (
    <div className="sl-speeds">
      <div><span className="arrow">↓</span><b ref={dRef} id="sl-down" /><small>Mbps down</small></div>
      <div><span className="arrow up">↑</span><b ref={uRef} id="sl-up" /><small>Mbps up</small></div>
    </div>
  );
});

const SlKv = memo(function SlKv() {
  const v = useSelect(["starlink.latency_ms", "starlink.drop_rate", "starlink.signal_quality", "starlink.snr_db", "starlink.snr_capped",
    "starlink.snr_persistently_low"], g => {
    const dr = g<number | null>("starlink.drop_rate"), sq = g<number | null>("starlink.signal_quality"), snr = g<number | null>("starlink.snr_db");
    return {
      lat: `${num(g("starlink.latency_ms"))} ms`,
      drop: dr == null ? "—" : `${(dr * 100).toFixed(1)}%`,
      sq: sq == null ? "—" : `${Math.round(sq * 100)}%`,
      snr: snr == null ? "—" : `${g("starlink.snr_capped") ? "≥ 10" : (+snr).toFixed(1)} dB`,
      low: !!g("starlink.snr_persistently_low"),
    };
  });
  const w = v.low ? "warn-text" : undefined;
  return (
    <div className="kv-row sl-kv">
      <div><b>{v.lat}</b><span>latency</span></div>
      <div><b>{v.drop}</b><span>ping drop</span></div>
      <div><b className={w}>{v.sq}</b><span>signal quality</span></div>
      <div><b className={w}>{v.snr}</b><span>SNR</span></div>
    </div>
  );
});

/** Install update: only when the dish has one downloaded (REBOOT_REQUIRED) and this screen may control the dish. */
const SlInstall = memo(function SlInstall() {
  const v = useSelect(["starlink.online", "starlink.update_state", ...ctlKeys("starlink")], g => ({
    ready: g("starlink.online") != null && g("starlink.update_state") === "REBOOT_REQUIRED",
    can: canControlDev("starlink"), busy: g(K.pending) != null,
  }));
  if (!v.ready) return null;
  const install = () => {
    if (!canControlDev("starlink") || ctlPending()) return;
    askConfirm(INSTALL_ASK.title, INSTALL_ASK.detail, () => control("install_update", true, "Starlink update", "starlink"));
  };
  return (
    <div className="chips-row">
      {v.can && <button className="btn-soft sl-install" data-sl="install" disabled={v.busy} onClick={install}><Icon name="download" />Install update</button>}
    </div>
  );
});

const SlSky = memo(function SlSky() {
  const v = useSelect(["starlink.obstruction_map", "starlink.azimuth", "starlink.elevation", "starlink.obstructed_pct", "starlink.software"], g => ({
    map: g<ObstructionMap | null>("starlink.obstruction_map") ?? null, az: g<number | null>("starlink.azimuth") ?? null,
    el: g<number | null>("starlink.elevation") ?? null, obs: g<number | null>("starlink.obstructed_pct") ?? null,
    sw: String(g("starlink.software") ?? ""),
  }), (a, b) => a.map === b.map && a.az === b.az && a.el === b.el && a.obs === b.obs && a.sw === b.sw);
  if (v.map?.data && v.map.rows) return <SkyFigure map={v.map} az={v.az} el={v.el} obs={v.obs} software={v.sw} />;
  const obs = v.obs ?? 0;
  return (
    <div className="sl-obs">
      <Ring value={Math.min(100, obs)} size={150} stroke={14} color={obs > 5 ? "var(--warning)" : "var(--c-shore)"} />
      <div className="center"><b>{`${num(v.obs, 1)}%`}</b><small>obstructed</small></div>
    </div>
  );
});

const StarlinkCard = memo(function StarlinkCard() {
  const ready = useReady();
  const reach = useKey("starlink.reachable");
  return (
    <div className="card sl-card" id="sl-card">
      {ready && reach == null ? <NotEnabled title="Starlink" what="Not enabled." /> : (
        <div className="sl-body">
          <div className="sl-left">
            <SlTitle />
            <SlSpeeds />
            <SlKv />
            <SlInstall />
          </div>
          <SlSky />
        </div>
      )}
    </div>
  );
});

// ── Cellular / WWAN ──
const PEP_KEYS = ["peplink.wans", "peplink.reachable", "peplink.allowance"];

const CellCard = memo(function CellCard() {
  const ready = useReady();
  const v = useSelect(PEP_KEYS, g => {
    const wans = g<Wan[]>("peplink.wans") ?? [], cw = wans.find(w => w.cellular), c = cw?.cellular ?? null;
    const allow = (g<Allowance[]>("peplink.allowance") ?? []).find(a => cw && a.conn === cw.id && (a.sim == null || a.sim === c?.sim)) ?? null;
    return { enabled: g("peplink.reachable") != null, led: cw?.led ?? "", msg: cw?.message ?? "", c, allow };
  });
  const c = v.c;
  if (!c) {
    return <div className="card cell-card" id="cell-card">
      {ready ? <NotEnabled title="Cellular" what={v.enabled ? "No cellular WAN." : "Not enabled."} /> : <div className="card-h"><h2>Cellular</h2></div>}</div>;
  }
  const a = v.allow;
  return (
    <div className="card cell-card" id="cell-card">
      <div className="card-h"><h2 className="sl-title" title={v.msg}><span className={`led led-${v.led}`} />Cellular</h2></div>
      <div className="cell-main">
        <div className="cell-sig"><Bars level={c.signal_level ?? 0} />
          <div><b>{c.network || ""}</b><span>{`${c.carrier || ""}${c.roaming && c.roaming !== "home" ? ` · ${c.roaming}` : ""}`}</span></div></div>
        {a && <div className="sl-obs">
          <Ring value={a.percent} size={150} stroke={14} color={a.percent >= 95 ? "var(--critical)" : a.percent >= 80 ? "var(--warning)" : "var(--c-batt)"} />
          <div className="center"><b>{`${Math.round(a.percent)}%`}</b><small>{`${(a.usage_mb / 1024).toFixed(1)} / ${(a.limit_mb / 1024).toFixed(0)} GB`}</small></div></div>}
      </div>
      <div className="kv-row">
        <div><b>{c.rsrp ?? "—"} <small>dBm</small></b><span>{`RSRP · ${rsrpQ(c.rsrp) ?? "—"}`}</span></div>
        <div><b>{c.sinr ?? "—"} <small>dB</small></b><span>{`SINR · ${sinrQ(c.sinr) ?? "—"}`}</span></div>
        <div><b>{c.band || "—"}</b><span>{`${c.rat || "band"}${c.ca ? " · CA" : ""}`}</span></div>
        <div><b>{`SIM ${c.sim ?? "—"}`}</b><span>active</span></div>
      </div>
    </div>
  );
});

/** Wi-Fi as WAN: the connected profile, else the first enabled one. */
const WwanCard = memo(function WwanCard() {
  const ready = useReady();
  const v = useSelect(PEP_KEYS, g => {
    const wifis = (g<Wan[]>("peplink.wans") ?? []).filter(w => w.wifi && w.enabled);
    const ww = wifis.find(w => w.connected) ?? wifis[0] ?? null;
    return { enabled: g("peplink.reachable") != null, ww };
  });
  const ww = v.ww, wf = ww?.wifi;
  if (!ww || !wf) {
    return <div className="card cell-card wwan-card" id="wwan-card">
      {ready ? <NotEnabled title="WWAN" what={v.enabled ? "No Wi-Fi WAN enabled." : "Not enabled."} /> : <div className="card-h"><h2>WWAN</h2></div>}</div>;
  }
  return (
    <div className="card cell-card wwan-card" id="wwan-card">
      <div className="card-h"><h2 className="sl-title" title={ww.message ?? ""}><span className={`led led-${ww.led ?? ""}`} />WWAN</h2></div>
      <div className="cell-main">
        <div className="cell-sig"><Bars level={ww.connected ? (wf.level ?? 0) : 0} /><div><b>{wf.ssid || "—"}</b><span>{ww.name}</span></div></div>
      </div>
      <div className="kv-row">
        <div><b>{wf.level != null && ww.connected ? <>{wf.level}<small>/5</small></> : "—"}</b><span>signal</span></div>
        <div><b>{wf.channel ?? "—"}</b><span>{`channel${wf.band ? ` · ${wf.band}` : ""}`}</span></div>
        <div><b>{ww.connected && ww.uptime != null ? dur(ww.uptime) : "—"}</b><span>uptime</span></div>
        <div><b>{ww.priority != null ? `P${ww.priority}` : "—"}</b><span>priority</span></div>
      </div>
    </div>
  );
});

// ── WAN priority ──
const GRIP = (
  <svg viewBox="0 0 10 16"><circle cx="2.5" cy="3" r="1.4" /><circle cx="7.5" cy="3" r="1.4" /><circle cx="2.5" cy="8" r="1.4" />
    <circle cx="7.5" cy="8" r="1.4" /><circle cx="2.5" cy="13" r="1.4" /><circle cx="7.5" cy="13" r="1.4" /></svg>
);

interface Drag {
  id: number; grip: Element; items: HTMLElement[]; row: HTMLElement; from: number; to: number;
  y0: number; mid0: number; mids: number[]; shift: number; seq: (number | null)[];
}

/**
 * Priority order (P1 first). Tap P# → pick P1–P4 → confirm → one command. Drag a row by its handle to reorder;
 * dragging across the "Disabled" separator switches a WAN off / on; on drop the enabled list becomes P1, P2, P3…
 * (one confirm, one router change). While a row is dragged the list is frozen: a state update can't move rows
 * under the finger, and the command is built from WAN ids, never DOM positions.
 */
const WanCard = memo(function WanCard() {
  const v = useSelect(["peplink.wans", "peplink.active", "peplink.reachable", ...ctlKeys("peplink")], g => ({
    wans: g<Wan[]>("peplink.wans") ?? [], active: g<string | null>("peplink.active") ?? null, reach: g("peplink.reachable"),
    can: canControlDev("peplink"), busy: g(K.pending) != null,
  }));
  const ready = useReady();
  const [edit, setEdit] = useState<number | null>(null);
  const [frozen, setFrozen] = useState<{ wans: Wan[]; active: string | null } | null>(null);
  const drag = useRef<Drag | null>(null);
  const list = useRef<HTMLDivElement>(null);
  const wans = frozen?.wans ?? v.wans, active = frozen ? frozen.active : v.active;
  const { on, off } = wanLists(wans);
  const canDrag = v.can && !v.busy && on.length + off.length > 1;

  const down = (e: RPointerEvent<HTMLSpanElement>) => {
    if (!canDrag || drag.current || ctlPending()) return;
    e.preventDefault();
    const grip = e.currentTarget, wid = Number(grip.closest<HTMLElement>(".wan")?.dataset.wid);
    if (edit != null) flushSync(() => setEdit(null));            // the picker row closes first, then rows are measured
    const items = [...list.current!.querySelectorAll<HTMLElement>(":scope > .wan, :scope > .wan-sep")];
    const row = items.find(r => Number(r.dataset.wid) === wid);
    if (!row) return;
    const from = items.indexOf(row), rect = row.getBoundingClientRect();
    const mids = items.map(r => { const b = r.getBoundingClientRect(); return b.top + b.height / 2; });
    const gap = items.length > 1 ? items[1]!.getBoundingClientRect().top - items[0]!.getBoundingClientRect().bottom : 6;
    const seq = items.map(r => (r.classList.contains("wan-sep") ? null : Number(r.dataset.wid)));
    drag.current = { id: e.pointerId, grip, items, row, from, to: from, y0: e.clientY, mid0: rect.top + rect.height / 2, mids, shift: rect.height + gap, seq };
    try { grip.setPointerCapture(e.pointerId); } catch { /* synthetic event */ }
    row.classList.add("dragging");
    setFrozen({ wans: v.wans, active: v.active });
  };
  const move = (e: RPointerEvent) => {
    const d = drag.current;
    if (!d || e.pointerId !== d.id) return;
    const dy = e.clientY - d.y0, y = d.mid0 + dy;
    // target slot = how many of the OTHER items' (drag-start) midpoints are above the dragged row's centre
    d.to = d.mids.filter((m, i) => i !== d.from && y > m).length;
    d.row.style.transform = `translateY(${dy}px)`;
    d.items.forEach((r, i) => {
      if (r === d.row) return;
      const s = d.to > d.from && i > d.from && i <= d.to ? -d.shift : d.to < d.from && i < d.from && i >= d.to ? d.shift : 0;
      r.style.transform = s ? `translateY(${s}px)` : "";
    });
  };
  const end = (e: RPointerEvent) => {
    const d = drag.current;
    if (!d || e.pointerId !== d.id) return;
    drag.current = null;
    d.items.forEach(r => { r.style.transform = ""; r.classList.remove("dragging"); });
    const snap = frozen ?? { wans: v.wans, active: v.active };
    setFrozen(null);
    if (e.type === "pointercancel" || d.to === d.from) return;
    const { order, disabled } = dragResult(d.seq, d.from, d.to);
    const ask = orderConfirm(snap.wans, order, disabled, snap.active);
    if ("error" in ask) { toast(ask.error, "bad"); return; }
    askConfirm(ask.title, ask.detail, () => control("set_wan_order", { order, disabled }, "WAN order", "peplink"));
  };

  const tapPrio = (w: Wan) => {
    if (!canControlDev("peplink") || ctlPending()) return;
    setEdit(edit === w.id ? null : w.id);
  };
  const pickPrio = (w: Wan, p: number) => {
    if (!canControlDev("peplink") || ctlPending()) return;
    setEdit(null);
    if (p === w.priority) return;
    const ask = priorityConfirm(w, p);
    askConfirm(ask.title, ask.detail, () => control("set_wan_priority", { id: w.id, priority: p }, "WAN priority", "peplink"));
  };

  const row = (w: Wan) => {
    const lk = locked(w);
    return [
      <div key={w.id} className={`wan${w.name === active ? " active" : ""}${w.enabled ? "" : " off"}${lk ? " locked" : ""}`} data-wid={w.id}>
        {lk ? <span className="grip none" /> : <span className="grip" aria-hidden="true" onPointerDown={down} onPointerMove={move} onPointerUp={end} onPointerCancel={end}>{GRIP}</span>}
        <span className={`led led-${w.enabled ? w.led ?? "" : "gray"}`} /><Icon name={wanIcon(w)} />
        <div><b>{w.name}</b><span>{`${w.message ?? ""}${w.enabled && w.uptime ? ` · up ${dur(w.uptime)}` : ""}`}</span></div>
        {!w.enabled ? <span className="prio">off</span>
          : v.can ? <button className={`prio prio-btn${edit === w.id ? " on" : ""}`} data-wan={w.id} disabled={v.busy} onClick={() => tapPrio(w)}>{w.priority != null ? `P${w.priority}` : "—"}</button>
          : <span className="prio">{w.priority != null ? `P${w.priority}` : ""}</span>}
      </div>,
      w.enabled && edit === w.id && v.can ? (
        <div key={`pick${w.id}`} className="wan-pick"><span>{`Priority for ${w.name}`}</span>
          <div className="seg">{[1, 2, 3, 4].map(p => <button key={p} data-wan={w.id} data-prio={p} className={w.priority === p ? "on" : undefined} onClick={() => pickPrio(w, p)}>{`P${p}`}</button>)}</div></div>
      ) : null,
    ];
  };

  return (
    <div className="card" id="wan-card">
      <div className="card-h"><h2>WAN priority</h2><span className="muted">{v.reach === false ? "router unreachable" : ""}</span></div>
      {on.length + off.length ? (
        <div className={`wans${canDrag ? " can-drag" : ""}`} ref={list}>
          {on.map(row)}
          <div className="wan-sep"><span>Disabled</span></div>
          {off.map(row)}
        </div>
      ) : <div className="muted">{!ready ? "" : v.reach == null ? "Peplink not enabled." : "No WAN data."}</div>}
    </div>
  );
});

// ── throughput chart ──
const SPANS: [number, string][] = [[21600, "6 h"], [86400, "24 h"], [604800, "7 d"]];
const netFmt = (v: number) => `${+v.toFixed(Math.abs(v) >= 10 ? 0 : Math.abs(v) >= 1 ? 1 : 2)} Mbps`;   // small scales keep decimals
const snrFmt = (v: number) => `${+v.toFixed(1)} dB`;

/** The Peplink's active WAN when the router API is on, else the Starlink dish; Starlink's SNR as its own lane under
 *  the throughput (shared time window, its own 0–10 dB scale: one chart with two y-axes would misread). */
const NetChart = memo(function NetChart() {
  const v = useSelect(["peplink.reachable", "starlink.reachable", "peplink.active"], g => ({
    pep: g("peplink.reachable") != null, sl: g("starlink.reachable") != null, active: g<string>("peplink.active") ?? null,
  }));
  const [span, setSpan] = useState(86400);
  const [kd, ku] = v.pep ? ["wan.down_mbps", "wan.up_mbps"] : ["starlink.down_mbps", "starlink.up_mbps"];
  const h = useHistory(v.sl ? [kd, ku, "starlink.snr_db"] : [kd, ku], span, 360);
  const series: Series[] = [
    { name: "Download", color: "var(--c-out)", points: h?.[kd] },
    { name: "Upload", color: "var(--c-cabin)", points: h?.[ku] },
  ];
  const snr: Series = { name: "Starlink SNR", color: "var(--c-alt)", points: h?.["starlink.snr_db"] };
  const all = [...series, ...(v.sl ? [snr] : [])];
  const pts = all.flatMap(s => s.points ?? []);
  const win = pts.length ? { t0: Math.min(...pts.map(p => p[0])), t1: Math.max(...pts.map(p => p[0])) } : {};
  return (
    <div className="card wide pwr-chart">
      <div className="card-h"><h2 id="net-chart-title">{!v.pep ? "Starlink throughput" : `${v.active || "Internet"} throughput`}</h2>
        <div className="legend" id="legend-net">{all.map(s => <span key={s.name}><i style={{ background: s.color }} />{s.name}</span>)}</div>
        <div className="seg range" id="range-net">
          {SPANS.map(([s, t]) => <button key={s} data-s={s} className={s === span ? "on" : undefined} onClick={() => setSpan(s)}>{t}</button>)}</div></div>
      <TimeChart id="chart-net" series={series} area {...win} fmt={netFmt} />
      {v.sl && <TimeChart id="chart-net-snr" className="snr-lane" series={[snr]} yMin={0} yMax={10} yTicks={[0, 10]} {...win} fmt={snrFmt} />}
    </div>
  );
});

export default function NetworkPage() {
  return (
    <div className="net-grid">
      <StarlinkCard />
      <div className="net-right">
        <div className="net-pair">
          <CellCard />
          <WwanCard />
        </div>
        <WanCard />
      </div>
      <NetChart />
    </div>
  );
}
