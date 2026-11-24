// Settings tabs read from the live state (no admin helper): the Starlink dish (with its controls), and the read-only
// details of the two heaters (their controls live on the Climate page). Ported from app.js renderSettings,
// slEventsHtml, drawSlPower and the Starlink handlers.
//
// Starlink commands go through control(…, "starlink") (one verified command at a time). Confirmations kept exactly
// as app.js: Reboot dish, Install update. No confirm (as app.js): dish heating, the sleep schedule, update check.
import { memo, useMemo, type ReactNode } from "react";
import { TimeChart } from "../../charts/TimeChart";
import { fmtT, num, UNIT } from "../../lib/format";
import { canControlDev, control, ctlKeys, ctlPending } from "../../logic/control";
import { askConfirm, toast } from "../../logic/ui";
import { useHistory } from "../../state/history";
import { useKey, useSelect } from "../../state/hooks";
import { K, type Getter } from "../../state/store";
import { kept, useKept } from "./data";

export const SetRow = ({ label, sub, val }: { label: ReactNode; sub?: ReactNode; val: ReactNode }) => (
  <div className="set-row"><div><b>{label}</b>{sub ? <small>{sub}</small> : null}</div><div className="set-v"><span>{val}</span></div></div>
);

// ── Starlink ──
interface Sleep { enabled?: boolean; start_utc_min?: number; duration_min?: number }
interface SlEvent { t: number; dur_s: number; what: string; outage?: boolean; sev?: string }
interface Edit { enabled: boolean; start: number; end: number }
export const hhmm = (m: number) => { const d = new Date(); d.setHours(0, m, 0, 0); return d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" }); };
const tzOff = () => new Date().getTimezoneOffset();             // minutes, UTC − local
export const utcToLocal = (m: number) => ((m - tzOff()) % 1440 + 1440) % 1440;
export const localToUtc = (m: number) => ((m + tzOff()) % 1440 + 1440) % 1440;
const TIMES = Array.from({ length: 48 }, (_, i) => i * 30);
/** The schedule as the dish has it (local time), or the 23:00–07:00 default. */
export const sleepEdit = (sl: Sleep | null | undefined): Edit => sl?.enabled
  ? { enabled: true, start: utcToLocal(sl.start_utc_min ?? 0), end: (utcToLocal(sl.start_utc_min ?? 0) + (sl.duration_min ?? 0)) % 1440 }
  : { enabled: false, start: 1380, end: 420 };

export function starlinkReboot(): void {
  askConfirm("Reboot the Starlink dish?", "The dish restarts and Starlink internet is down for a few minutes. If Starlink is the active connection, remote (Tailscale) access to this console drops too.",
    () => control("reboot", true, "Starlink reboot", "starlink"));
}
export function starlinkInstall(): void {
  askConfirm("Install the Starlink update?", "The dish restarts to install the downloaded update. Starlink internet drops for a few minutes; if Starlink is the active connection, remote (Tailscale) access to this console drops too. A firmware install can't be undone.",
    () => control("install_update", true, "Starlink update", "starlink"));
}

const SL = (k: string) => `starlink.${k}`;
const SLEEP_EDIT = kept<Edit | null>(null);
const SL_KEYS = [SL("heating"), SL("sleep"), SL("update_state"), SL("update_checked"), SL("software"), SL("reachable"), SL("controls_ok"),
  ...ctlKeys("starlink")];
const UPD_TXT: Record<string, string> = { REBOOT_REQUIRED: "Update downloaded — ready to install", IDLE: "Up to date" };

export const StarlinkTab = memo(function StarlinkTab() {
  const v = useSelect(SL_KEYS, gt => ({
    heat: gt<string>(SL("heating")) ?? null, sl: gt<Sleep>(SL("sleep")) ?? null, upd: gt<string>(SL("update_state")) ?? null,
    chk: gt<number>(SL("update_checked")) ?? null, sw: gt<string>(SL("software")) ?? null, reach: gt(SL("reachable")),
    ctlOk: !!gt(SL("controls_ok")), view: gt(K.control) !== true, ok: canControlDev("starlink") && !ctlPending(),
  }));
  // sleep-schedule edits are held here, so live updates don't reset the pickers mid-edit (app.js slSleepEdit)
  const [edit, setEdit] = useKept(SLEEP_EDIT);
  const sl = v.sl;
  const ed = edit ?? (sl ? sleepEdit(sl) : { enabled: false, start: 1380, end: 420 });
  const dur = (ed.end - ed.start + 1440) % 1440 || 1440;
  const updTxt = (v.upd != null ? UPD_TXT[v.upd] : undefined) ?? (v.upd ? String(v.upd).replace(/_/g, " ").toLowerCase() : "—");
  const dis = !v.ok;
  const pick = (k: "start" | "end", val: number) => (
    <select className="set-sel" data-slp={k} disabled={dis} value={val}
      onChange={e => { const m = +e.target.value; setEdit(p => ({ ...(p ?? { ...sleepEdit(sl), enabled: true }), [k]: m })); }}>
      {TIMES.map(m => <option key={m} value={m}>{hhmm(m)}</option>)}
    </select>);
  const sleepOff = () => { setEdit(null); if (sl?.enabled) control("set_sleep", { enabled: false }, "Sleep schedule off", "starlink"); };
  const sleepOn = () => setEdit({ ...sleepEdit(sl), enabled: true });
  const save = () => {
    if (!edit) return;
    const d = (edit.end - edit.start + 1440) % 1440;
    if (!d) { toast("Sleep start and end can't be the same time", "bad"); return; }
    setEdit(null);
    control("set_sleep", { enabled: true, start_utc_min: localToUtc(edit.start), duration_min: d }, `Sleep ${hhmm(edit.start)}–${hhmm(edit.end)}`, "starlink");
  };
  const nowWin = sl?.enabled ? ` · now ${hhmm(utcToLocal(sl.start_utc_min ?? 0))} – ${hhmm((utcToLocal(sl.start_utc_min ?? 0) + (sl.duration_min ?? 0)) % 1440)}` : "";
  return <>
    <div className="set-h"><h2>Starlink dish</h2>
      <span className="muted">{v.reach === false ? "dish unreachable" : !v.ctlOk ? "control off" : v.view ? "view only" : ""}</span>
      <button className="ctl-pill warn sl-reboot" data-sl="reboot" disabled={dis} onClick={starlinkReboot}>Reboot dish</button></div>
    <div className="set-rows">
      <div className="set-row"><div><b>Heating</b><small>Snow melt. Auto heats only when it detects snow or ice; On pre-heats all the time (more power)</small></div>
        <div className="seg">{([["auto", "Auto"], ["on", "On"], ["off", "Off"]] as const).map(([h, l]) => (
          <button key={h} className={v.heat === h ? "on" : ""} data-slheat={h} disabled={v.heat == null || dis}
            onClick={() => control("set_heating", h, `Dish heating ${h}`, "starlink")}>{l}</button>))}</div></div>
      <div className="set-row"><div><b>Sleep schedule</b><small>{`The dish powers down its radio daily in this window (local time) to save power${nowWin}`}</small></div>
        <div className="seg"><button className={ed.enabled ? "" : "on"} data-slpen="0" disabled={sl == null || dis} onClick={sleepOff}>Off</button>
          <button className={ed.enabled ? "on" : ""} data-slpen="1" disabled={sl == null || dis} onClick={sleepOn}>On</button></div></div>
      {ed.enabled && (
        <div className="set-row"><div><b>Sleep from</b><small>{`${Math.floor(dur / 60)} h ${dur % 60 ? `${dur % 60} m` : ""} of sleep`}</small></div>
          <div className="set-v">{pick("start", ed.start)}<span>to</span>{pick("end", ed.end)}</div></div>)}
      {edit && (
        <div className="set-row"><div><b>Unsaved schedule</b><small>Applies to the dish when you save</small></div>
          <div className="set-v"><button className="ctl-pill" data-slsleep="cancel" onClick={() => setEdit(null)}>Cancel</button>
            <button className="ctl-pill" data-slsleep="save" disabled={dis} onClick={save}>Save</button></div></div>)}
      <div className="set-row"><div><b>Software</b>
        <small>{`${v.sw || "—"}${v.chk ? ` · checked ${new Date(v.chk * 1000).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}` : ""}`}</small></div>
        <div className="set-v"><span>{updTxt}</span>
          <button className="ctl-pill" data-sl="check" disabled={dis} onClick={() => control("check_update", true, "Starlink update check", "starlink")}>Check for updates</button>
          {v.upd === "REBOOT_REQUIRED" && <button className="ctl-pill" data-sl="install" disabled={dis} onClick={starlinkInstall}>Install update</button>}</div></div>
    </div>
    <SlPower />
    <SlEvents />
  </>;
});

const POW_KEYS = ["starlink.power_w"];
const POW_SPANS = [[21600, "6 h"], [86400, "24 h"], [604800, "7 d"]] as const;
const wFmt = (v: number) => `${Math.round(v)} W`;
const POW_SPAN = kept(86400);
/** Dish power draw (recorded each minute from the dish's 1 Hz power history). */
const SlPower = memo(function SlPower() {
  const [span, setSpan] = useKept(POW_SPAN);
  const now = useKey<number | null>(SL("power_w"));
  const h = useHistory(POW_KEYS, span);
  const pts = h?.["starlink.power_w"];
  const avg = pts?.length ? pts.reduce((a, p) => a + p[1], 0) / pts.length : null;
  const series = useMemo(() => [{ name: "Dish power", color: "var(--c-solar)", points: pts }], [pts]);
  return <>
    <div className="set-h sub"><h2>Power usage</h2>
      <span className="muted">{`${now != null ? `now ${Math.round(now)} W` : "—"}${avg != null ? ` · average ${Math.round(avg)} W` : ""}`}</span>
      <div className="seg range" id="range-slpow">{POW_SPANS.map(([s, l]) => (
        <button key={s} data-slspan={s} className={span === s ? "on" : ""} onClick={() => setSpan(s)}>{l}</button>))}</div></div>
    <TimeChart id="chart-slpow" className="sl-pow" series={series} area zero fmt={wFmt} />
  </>;
});

const evDur = (s: number) => (s < 1 ? `${s.toFixed(1)} s` : s < 60 ? `${Math.round(s)} s` : s < 3600 ? `${Math.floor(s / 60)} m ${Math.round(s % 60)} s`
  : `${Math.floor(s / 3600)} h ${Math.round(s % 3600 / 60)} m`);
const SlEvents = memo(function SlEvents() {
  const v = useSelect([SL("events"), SL("events_span_s")], gt => ({ evs: gt<SlEvent[]>(SL("events")) ?? null, span: gt<number>(SL("events_span_s")) ?? null }));
  if (!v.evs) return <><div className="set-h sub"><h2>Events &amp; outages</h2></div><div className="muted">Waiting for the dish&apos;s history…</div></>;
  const outs = v.evs.filter(e => e.outage), down = outs.reduce((a, e) => a + e.dur_s, 0);
  const hrs = v.span ? Math.round(v.span / 3600) : null;
  const today = new Date().toDateString();
  const when = (t: number) => { const d = new Date(t * 1000);
    return (d.toDateString() === today ? "" : `${d.toLocaleDateString([], { month: "short", day: "numeric" })} `) + d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit", second: "2-digit" }); };
  return <>
    <div className="set-h sub"><h2>Events &amp; outages</h2>
      <span className="muted">{`${outs.length} outage${outs.length === 1 ? "" : "s"} · ${evDur(down)} down${hrs ? ` · last ${hrs} h kept by the dish` : ""}`}</span></div>
    <div className="sl-events">{v.evs.length ? v.evs.map((e, i) => (
      <div key={`${e.t}-${i}`} className={`sl-ev ${e.outage ? "out" : e.sev ?? ""}`}><i /><span className="t">{when(e.t)}</span>
        <b>{e.what}</b><span className="d">{e.dur_s > 0 ? evDur(e.dur_s) : ""}</span></div>)) : <div className="muted">No events.</div>}</div>
  </>;
});

// ── Rixens MCS7 (read-only details) ──
const R = (k: string) => `climate.rixens.${k}`;
const upDur = (s: number | null | undefined) => (s == null ? "—" : s >= 86400 ? `${Math.floor(s / 86400)} d ${Math.floor(s % 86400 / 3600)} h`
  : `${Math.floor(s / 3600)} h ${Math.floor(s % 3600 / 60)} m`);
const MCS_FIELDS = ["reachable", "live", "state", "heat_version", "version", "battery_v", "runtime_h", "uptime", "available", "thermostat",
  "setpoint_c", "fan_speed", "fan_auto_pct", "fuel_ml_per_pulse", "fuel_today_gal", "altitude_m", "controls_ok", "fault_active", "fault_history"];
const MCS_KEYS = MCS_FIELDS.map(R);
const pick = (gt: Getter, keys: readonly string[]) => Object.fromEntries(keys.map(k => [k, gt(k) ?? null])) as Record<string, unknown>;

export const Mcs7Tab = memo(function Mcs7Tab() {
  const s = useSelect(MCS_KEYS, gt => pick(gt, MCS_KEYS));
  const r = <T,>(k: string) => s[R(k)] as T;
  const srcs = Object.entries(r<Record<string, boolean> | null>("available") ?? {}).filter(([, on]) => on).map(([k]) => k[0]!.toUpperCase() + k.slice(1)).join(", ") || "—";
  const hist = r<unknown[] | null>("fault_history") ?? [];
  const fan = r<string | number | null>("fan_speed"), fa = r<number | null>("fan_auto_pct"), alt = r<number | null>("altitude_m");
  return <>
    <div className="set-h"><h2>Rixens MCS7</h2>
      <span className="muted">{r("reachable") === false ? "unreachable" : r("live") ? String(r("state") || "online") : "—"}</span></div>
    <div className="set-rows">
      <SetRow label="Firmware" sub={r("heat_version") ? `Heat controller ${r("heat_version")}` : ""} val={String(r("version") || "—")} />
      <SetRow label="Battery" sub="Supply voltage at the MCS7" val={r("battery_v") == null ? "—" : `${num(r("battery_v"), 1)} V`} />
      <SetRow label="Burner runtime" sub="Total hours on the heater" val={r("runtime_h") == null ? "—" : `${Math.round(r<number>("runtime_h")).toLocaleString()} h`} />
      <SetRow label="Controller uptime" val={upDur(r("uptime"))} />
      <SetRow label="Heat sources" sub="Fitted on this install" val={srcs} />
      <SetRow label="Thermostat" sub="The MCS7 regulates the cabin" val={r("thermostat") == null ? "—" : r("thermostat") ? `on · ${fmtT(r("setpoint_c"))}` : "off"} />
      <SetRow label="Fan" val={fan == null ? "—" : fan === "auto" ? `auto${fa != null ? ` · ${fa} %` : ""}` : String(fan)} />
      <SetRow label="Fuel today" sub={r("fuel_ml_per_pulse") ? `Fuel pump ${r("fuel_ml_per_pulse")} ml per stroke` : ""}
        val={r("fuel_today_gal") == null ? "—" : `${(+r<number>("fuel_today_gal")).toFixed(2)} gal`} />
      <SetRow label="Altitude" sub="Used by the burner's altitude compensation"
        val={alt == null ? "—" : UNIT === "F" ? `${Math.round(alt * 3.281).toLocaleString()} ft` : `${Math.round(alt)} m`} />
      <SetRow label="Control from AetherRV" val={r("controls_ok") ? "on" : "off"} />
    </div>
    <div className="set-h sub"><h2>Faults</h2>
      <span className="muted">{r("fault_active") != null ? `active: ${String(r("fault_active"))}` : "none active"}</span></div>
    {hist.length ? <div className="upks">{hist.map((f, i) => <div className="upk" key={i}><b>{String(f)}</b></div>)}</div>
      : <div className="set-p muted">No stored faults.</div>}
  </>;
});

// ── Espar Airtronic S2 (read-only details: controller, diagnostics, fault memory) ──
const E = (k: string) => `climate.espar.${k}`, D = (k: string) => `climate.espar.diag.${k}`, C = (k: string) => `climate.espar.ctl.${k}`;
const ESP_KEYS = [E("heard"), E("asleep"), E("state"), C("active"), C("room_c"), C("room_src"), "espar.controls_ok", D("hardware"), D("software"),
  D("voltage_v"), D("hours"), D("fuel"), D("air_hpa"), D("dtcs"), D("lockout"), D("enabled")];
const BANDS = [["p0_25", "0–25 % power"], ["p25_50", "25–50 %"], ["p50_75", "50–75 %"], ["p75_100", "75–100 %"], ["pause", "Control pause"]] as const;
const gal = (l: number | null | undefined) => (l == null ? "—" : `${(l / 3.785).toFixed(1)} gal`);
interface Dtc { code: string; text: string; active?: boolean }

export const EsparTab = memo(function EsparTab() {
  const s = useSelect(ESP_KEYS, gt => pick(gt, ESP_KEYS));
  const h = (s[D("hours")] as Record<string, number | null> | null) ?? {}, dtcs = (s[D("dtcs")] as Dtc[] | null) ?? [];
  const lo = s[D("lockout")] as { help?: string } | null;
  const roomC = s[C("room_c")] as number | null;
  return <>
    <div className="set-h"><h2>Espar Airtronic S2</h2>
      <span className="muted">{!s[E("heard")] ? "not heard on the bus" : s[E("asleep")] ? "bus asleep" : String(s[E("state")] || "")}</span></div>
    <div className="set-rows">
      <SetRow label="Controller" sub="AetherRV replaces the EasyStart Pro (keep it unplugged)"
        val={s[C("active")] ? "AetherRV" : s["espar.controls_ok"] ? "AetherRV · idle" : "off"} />
      <SetRow label="Room temperature" sub="What the heater regulates on: the cabin Ruuvi, else the heater's own intake sensor"
        val={roomC == null ? "—" : `${fmtT(roomC)} · ${String(s[C("room_src")] || "")}`} />
      <SetRow label="Software" sub={s[D("hardware")] ? `Hardware ${String(s[D("hardware")])}` : ""} val={String(s[D("software")] || "—")} />
      <SetRow label="Battery" sub="Supply voltage at the heater" val={s[D("voltage_v")] == null ? "—" : `${num(s[D("voltage_v")], 1)} V`} />
      <SetRow label="Operating hours" sub="Total time the heater has run" val={h.operating == null ? "—" : `${Math.round(h.operating).toLocaleString()} h`} />
      <SetRow label="Fuel burned" sub="Lifetime, from the heater's own counter" val={gal((s[D("fuel")] as { total_l?: number } | null)?.total_l)} />
      <SetRow label="Air pressure" sub="The heater adjusts fuel for altitude" val={s[D("air_hpa")] == null ? "—" : `${Math.round(s[D("air_hpa")] as number)} hPa`} />
    </div>
    {h.p0_25 != null && <><div className="set-h sub"><h2>Hours by power</h2></div>
      <div className="upks">{BANDS.map(([k, l]) => (
        <div className="upk" key={k}><b>{h[k] == null ? "—" : `${Math.round(h[k]!).toLocaleString()} h`}</b><span>{l}</span></div>))}</div></>}
    <div className="set-h sub"><h2>Fault memory</h2>
      <span className="muted">{lo ? "locked out" : dtcs.some(x => x.active) ? "fault active" : dtcs.length ? "stored only" : ""}</span></div>
    {lo && <div className="set-p" style={{ color: "#ff8a80" }}>{lo.help}</div>}
    {dtcs.length ? <div className="upks">{dtcs.map((x, i) => (
      <div className="upk" key={`${x.code}-${i}`}><b>{`${x.code}${x.active ? " · active" : ""}`}</b><span>{x.text}</span></div>))}</div>
      : <div className="set-p muted">{s[D("enabled")] ? "No stored faults." : "Diagnostics are off."}</div>}
  </>;
});
