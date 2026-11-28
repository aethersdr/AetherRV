// Settings tabs backed by the root admin helper (GET /api/settings, changes through settingsAction): Device & display,
// Network (wired + Wi-Fi), Tailscale, System updates. Ported from app.js renderSettings and its #set-body handlers.
//
// Confirmations kept exactly as app.js: Reboot (the only system action offered since 2026-11-27), Forget a Wi-Fi network, Install Tailscale,
// Tailscale sign-out, Install updates; plus Wi-Fi OFF (owner, Q-U5: remote access over Wi-Fi drops). No confirm (as
// app.js): rename, screen auto-off, Wi-Fi on, scan and join, Tailscale sign-in, update check. Every change is refused in view-only mode (buttons disabled, and settingsAction
// itself refuses). The units and night mode are this screen's own preferences and work in view-only mode too.
import { memo, useEffect, useLayoutEffect, useRef, useState } from "react";
import { Icon } from "../../components/Icon";
import { setNight } from "../../components/Shell";
import { UNIT } from "../../lib/format";
import { prefs } from "../../lib/prefs";
import { settingsAction } from "../../logic/control";
import { askConfirm } from "../../logic/ui";
import { useControl, useKey } from "../../state/hooks";
import { K } from "../../state/store";
import { kept, useFastPoll, useKept, type SettingsInfo, type Tailscale, type WifiNet } from "./data";
import { PiChart } from "./PiChart";
import { openKeyboard } from "./Keyboard";

const fmtBytes = (b: number | null | undefined) => (b == null ? "—" : b > 1e9 ? `${(b / 1e9).toFixed(1)} GB` : `${Math.round(b / 1e6)} MB`);
const fmtUp = (s: number | null | undefined) => (s == null ? "—"
  : s > 86400 ? `${Math.floor(s / 86400)} d ${Math.floor(s % 86400 / 3600)} h` : `${Math.floor(s / 3600)} h ${Math.floor(s % 3600 / 60)} min`);

// ── Device & display ──
const OFF_OPTS = [[5, "5 min"], [10, "10 min"], [15, "15 min"], [30, "30 min"], [60, "1 h"], [0, "Never"]] as const;
const HOST_RE = "[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?";

export async function renameDevice(current: string | undefined): Promise<void> {
  const name = await openKeyboard({ title: "Device name (letters, digits, -)", value: current || "", maxlen: 63, pattern: HOST_RE });
  if (name && name !== current) await settingsAction("set_hostname", { name }, "Rename");
}
// Only Reboot is offered (owner, 2026-11-27: "Restart display" and "Shut down" removed from the Device tab).
const SYS_ACTS = {
  reboot: ["Reboot AetherRV?", "The console is offline for about a minute. The heater, inverter and switches keep running on their own.", "Reboot"],
} as const;
export function systemAction(act: keyof typeof SYS_ACTS): void {
  const [t, d, label] = SYS_ACTS[act];
  askConfirm(t, d, () => void settingsAction(act, {}, label));
}

export const DeviceTab = memo(function DeviceTab({ info }: { info: SettingsInfo }) {
  const ctl = useControl();
  const tz = useKey<string>("settings.timezone");
  const night = useKey(K.night) === true;
  const a = info.admin ?? undefined, p = info.prefs ?? {};
  const off = (p.screen_off_min as number | undefined) ?? 10;
  const ifs = (a?.interfaces ?? []).filter(i => i.ipv4?.length).map(i => `${i.name} ${i.ipv4!.join(", ")}`).join(" · ") || "—";
  return <>
    <div className="set-h"><h2>Device &amp; display</h2>{a && a.ok === false && <span className="pill bad-pill">{a.error || "helper unavailable"}</span>}
      <button className="ctl-pill warn dev-reboot" data-act="reboot" disabled={!ctl} onClick={() => systemAction("reboot")}>Reboot</button></div>
    <div className="set-rows">
      <div className="set-row"><div><b>Name</b><small>Shown on the network and in Tailscale</small></div>
        <div className="set-v"><span>{a?.hostname || "—"}</span>
          <button className="ctl-pill" data-act="rename" disabled={!ctl} onClick={() => void renameDevice(a?.hostname)}>Rename</button></div></div>
      <div className="set-row"><div><b>Time zone</b><small>Set automatically from the van&apos;s GPS position</small></div>
        <div className="set-v"><span>{(tz || a?.timezone || "—").replace(/_/g, " ")}</span><span className="pill ok-pill"><Icon name="pin" />GPS</span></div></div>
      <div className="set-row"><div><b>Screen auto-off</b><small>After this long without a touch; any touch wakes it</small></div>
        <div className="seg">{OFF_OPTS.map(([v, l]) => (
          <button key={v} className={off === v ? "on" : ""} data-off={v} disabled={!ctl}
            onClick={() => void settingsAction("pref", { key: "screen_off_min", value: v }, "Screen auto-off")}>{l}</button>))}</div></div>
      <div className="set-row"><div><b>Temperature</b><small>Units on every screen</small></div>
        <div className="seg">{(["F", "C"] as const).map(u => (
          <button key={u} className={UNIT === u ? "on" : ""} data-unit={u} onClick={() => { prefs.set("vp.temp", u); location.reload(); }}>°{u}</button>))}</div></div>
      <div className="set-row"><div><b>Night mode</b><small>Dim, warm colours; dark weather map</small></div>
        <div className="seg"><button className={night ? "" : "on"} data-night="0" onClick={() => setNight(false)}>Day</button>
          <button className={night ? "on" : ""} data-night="1" onClick={() => setNight(true)}>Night</button></div></div>
    </div>
    <div className="set-h sub"><h2>About</h2></div>
    <div className="set-about">
      <div><small>AetherRV</small><b>{info.version || "—"}</b></div>
      <div><small>Kernel</small><b>{a?.kernel || "—"}</b></div>
      <div><small>Up</small><b>{fmtUp(a?.uptime_s)}</b></div>
      <div><small>Disk free</small><b>{`${fmtBytes(a?.disk?.free)} of ${fmtBytes(a?.disk?.total)}`}</b></div>
      <div className="wide"><small>Addresses</small><b>{ifs}</b></div>
    </div>
    <PiChart />
  </>;
});

// ── Network ──
const Bars = ({ dbm }: { dbm: number | undefined }) => {
  const q = dbm == null ? 1 : dbm >= -55 ? 4 : dbm >= -65 ? 3 : dbm >= -75 ? 2 : 1;
  return <span className="wbars">{[1, 2, 3, 4].map(i => <i key={i} className={i <= q ? "on" : ""} style={{ height: `${i * 25}%` }} />)}</span>;
};
/** Wi-Fi off asks first (owner, Q-U5); turning it on stays one tap. */
export function wifiOff(): void {
  askConfirm("Turn Wi-Fi off?", "Remote access over Wi-Fi (Tailscale) will drop until Wi-Fi is turned back on from this screen.",
    () => void settingsAction("wifi_power", { on: false }, "Wi-Fi off"));
}
export function wifiForget(ssid: string): void {
  askConfirm(`Forget ${ssid}?`, "Its saved password is removed; the console won't join it again on its own.",
    () => void settingsAction("wifi_forget", { ssid }, "Forget"));
}
/** Join: a secured network not saved yet asks for its password on the on-screen keyboard first. */
export async function wifiJoin(n: WifiNet): Promise<void> {
  const args: { ssid: string; passphrase?: string } = { ssid: n.ssid };
  if (n.security === "psk" && !n.known) {
    const pw = await openKeyboard({ title: `Password for ${n.ssid}`, password: true, maxlen: 64, pattern: "[\\x20-\\x7e]{8,64}" });
    if (pw == null) return;
    args.passphrase = pw;                         // only into this one request body
  }
  await settingsAction("wifi_connect", args, `Joining ${n.ssid}`);
}
const netLabel = (n: WifiNet) => n.connected ? "Connected" : n.known ? "Saved" : n.security === "open" ? "Open"
  : n.security === "8021x" ? "Enterprise (not supported)" : "Secured";

export const NetworkTab = memo(function NetworkTab({ info }: { info: SettingsInfo }) {
  const ctl = useControl();
  const w = info.wifi ?? {}, ifs = info.admin?.interfaces ?? [];
  useFastPoll(!!w.scanning, 2500);
  const ip = (n: string | undefined) => (ifs.find(i => i.name === n)?.ipv4 ?? []).join(", ");
  const wired = ifs.filter(i => /^(end|eth|en)/.test(i.name));
  const nets = (w.networks ?? []).filter(n => n.ssid);
  const savedOnly = (w.known ?? []).filter(k => !nets.some(n => n.ssid === k.ssid));
  const wifiSub = !w.present ? (w.error || "No Wi-Fi device") : !w.powered ? "Off"
    : w.connected ? `Connected to ${w.connected}${ip(w.device) ? ` · ${ip(w.device)}` : ""}` : "Not connected";
  return <>
    <div className="set-h"><h2>Network</h2></div>
    <div className="set-rows">
      {wired.map(i => (
        <div className="set-row" key={i.name}><div><b>{`Wired · ${i.name}`}</b><small>Ethernet (Peplink router / van LAN)</small></div>
          <div className="set-v"><span>{ip(i.name) || "no address"}</span>
            <span className={`pill ${i.state === "UP" ? "ok-pill" : "bad-pill"}`}>{i.state === "UP" ? "Connected" : "Down"}</span></div></div>))}
      <div className="set-row"><div><b>Wi-Fi</b><small>{wifiSub}</small></div>
        <div className="set-v">{w.present && <>
          <button className="ctl-pill" data-wscan="" disabled={!(w.powered && ctl)} onClick={() => void settingsAction("wifi_scan", {}, "Wi-Fi scan")}>
            {w.scanning ? "Scanning…" : "Scan"}</button>
          <div className="seg">
            <button className={w.powered ? "on" : ""} data-wpow="1" disabled={!ctl} onClick={() => void settingsAction("wifi_power", { on: true }, "Wi-Fi on")}>On</button>
            <button className={w.powered ? "" : "on"} data-wpow="0" disabled={!ctl} onClick={wifiOff}>Off</button>
          </div></>}</div></div>
    </div>
    {w.present && w.powered && (
      <div className="wlist">
        {nets.length ? nets.map(n => (
          <div className={`wnet${n.connected ? " conn" : ""}`} key={n.ssid}><Bars dbm={n.signal_dbm} />
            <div className="wname"><b>{n.ssid}</b><small>{netLabel(n)}</small></div>
            {n.security !== "open" && <Icon name="lock" />}
            {n.known && <button className="ctl-pill" data-wforget={n.ssid} disabled={!ctl} onClick={() => wifiForget(n.ssid)}>Forget</button>}
            {!n.connected && n.security !== "8021x" && (
              <button className="ctl-pill" data-wjoin={n.ssid} data-sec={n.security} data-known={n.known ? 1 : 0} disabled={!ctl}
                onClick={() => void wifiJoin(n)}>{n.known ? "Connect" : "Join"}</button>)}
          </div>)) : <div className="muted">{w.scanning ? "Scanning…" : "No networks found. Tap Scan."}</div>}
        {savedOnly.map(k => (
          <div className="wnet out" key={`saved-${k.ssid}`}><span className="wbars" />
            <div className="wname"><b>{k.ssid}</b><small>Saved · not in range</small></div>
            <button className="ctl-pill" data-wforget={k.ssid} disabled={!ctl} onClick={() => wifiForget(k.ssid)}>Forget</button></div>))}
      </div>)}
  </>;
});

// ── Tailscale ──
/** The sign-in link as a QR code (qrcode-generator, loaded only when a code is shown). */
const QR = memo(function QR({ url }: { url: string }) {
  const [svg, setSvg] = useState("");
  useEffect(() => {
    let dead = false;
    void import("qrcode-generator").then(({ default: qrcode }) => {
      const q = qrcode(0, "M"); q.addData(url); q.make();
      if (!dead) setSvg(q.createSvgTag({ cellSize: 8, margin: 2, scalable: true }));
    });
    return () => { dead = true; };
  }, [url]);
  return <div className="ts-qr" id="ts-qr" dangerouslySetInnerHTML={{ __html: svg }} />;
});
export function tailscaleInstall(): void {
  askConfirm("Install Tailscale?", "Downloads and installs Tailscale (about 30 MB) and starts its service. This takes a minute or two.",
    () => void settingsAction("tailscale_install", {}, "Installing Tailscale"));
}
export function tailscaleLogout(): void {
  askConfirm("Sign out of Tailscale?", "The console leaves your tailnet until you sign in again.",
    () => void settingsAction("tailscale_logout", {}, "Tailscale sign-out"));
}

const LOGIN_URL = kept<string | null>(null);
export const TailscaleTab = memo(function TailscaleTab({ info }: { info: SettingsInfo }) {
  const ctl = useControl();
  const [loginUrl, setLoginUrl] = useKept(LOGIN_URL);              // from this screen's own sign-in request only
  const t: Tailscale = info.tailscale ?? {};
  const running = t.state === "Running";
  const url = running ? null : t.auth_url || loginUrl;
  useFastPoll(!!t.installed && !running, 3000);
  useLayoutEffect(() => { if (running && loginUrl) setLoginUrl(null); }, [running, loginUrl]);
  const login = async () => {
    const r = await settingsAction("tailscale_login", {}, "Tailscale sign-in") as { auth_url?: string } | null;
    if (r?.auth_url) setLoginUrl(r.auth_url);
  };
  let inner;
  if (t.ok === false) inner = <div className="muted">{t.error || "Unavailable"}</div>;
  else if (!t.installed) inner = <>
    <p className="set-p">Tailscale gives you secure access to AetherRV from your phone or laptop anywhere,
      without opening ports on the Peplink. It isn&apos;t installed yet.</p>
    <button className="ctl-pill big" data-ts="install" disabled={!ctl} onClick={tailscaleInstall}>Install Tailscale</button></>;
  else if (running) inner = <>
    <div className="set-rows">
      <div className="set-row"><div><b>Connected</b><small>{`${t.tailnet || ""}${t.user ? ` · ${t.user}` : ""}`}</small></div>
        <div className="set-v"><span className={`pill ${t.online ? "ok-pill" : "bad-pill"}`}>{t.online ? "Online" : "Offline"}</span></div></div>
      <div className="set-row"><div><b>Name</b><small>Reach AetherRV at http://&lt;name&gt;:8080 from your tailnet</small></div>
        <div className="set-v"><span>{t.dns || t.name || "—"}</span></div></div>
      <div className="set-row"><div><b>Addresses</b><small>Tailscale IPs</small></div><div className="set-v"><span>{(t.ips ?? []).join("  ·  ") || "—"}</span></div></div>
    </div>
    <p className="set-p muted">Over Tailscale the screens are view-only, like any other remote browser; controls and settings stay on the touchscreen.</p>
    <div className="set-actions"><button className="ctl-pill warn" data-ts="logout" disabled={!ctl} onClick={tailscaleLogout}>Sign out</button></div></>;
  else if (url) inner = (
    <div className="ts-login"><QR url={url} />
      <div><b>Scan to sign in</b><p className="set-p">Open the camera on your phone, scan the code, and sign in to Tailscale.
        The console joins your tailnet as soon as you approve it.</p><code className="ts-url">{url}</code>
        <p className="muted">Waiting for sign-in…</p></div></div>);
  else inner = <>
    <p className="set-p">{`Tailscale is installed${t.running ? "" : " but its service isn't running"}. Sign in to add this console to your tailnet.`}</p>
    <button className="ctl-pill big" data-ts={t.running ? "login" : "install"} disabled={!ctl}
      onClick={t.running ? () => void login() : tailscaleInstall}>{t.running ? "Sign in" : "Start Tailscale"}</button></>;
  return <><div className="set-h"><h2>Tailscale</h2>{t.version && <span className="muted">{`v${t.version}`}</span>}</div>{inner}</>;
});

// ── System updates ──
const when = (t: number | null | undefined) => (t ? new Date(t * 1000).toLocaleString([], { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" }) : "never");
export function updateInstall(): void {
  askConfirm("Install system updates?", "Downloads and installs the listed packages (kernel and boot files stay as they are). The heater, inverter and switches keep running; the console stays usable but may be slow for a few minutes.",
    () => void settingsAction("update_start", {}, "Starting update"));
}
const Pkgs = ({ l }: { l: { name: string; old: string; new: string }[] }) =>
  <>{l.map(x => <div className="upk" key={x.name}><b>{x.name}</b><span>{`${x.old} → ${x.new}`}</span></div>)}</>;

export const UpdatesTab = memo(function UpdatesTab({ info }: { info: SettingsInfo }) {
  const ctl = useControl();
  const u = info.update ?? {}, st = u.state ?? {}, chk = u.check;
  const running = st.status === "running";
  useFastPoll(running, 2000);
  const logRef = useRef<HTMLPreElement>(null);
  useLayoutEffect(() => { const l = logRef.current; if (l) l.scrollTop = l.scrollHeight; }, [u.log]);
  return <>
    <div className="set-h"><h2>System updates</h2>{running && <span className="pill">Updating…</span>}</div>
    <div className="set-rows">
      <div className="set-row"><div><b>Last update</b><small>{st.status === "failed" ? "The last update didn't finish — see the log" : "Arch Linux ARM packages (pacman)"}</small></div>
        <div className="set-v"><span>{`${when(+(u.last?.finished ?? 0))}${u.last?.status ? ` · ${u.last.status}` : ""}`}</span></div></div>
      <div className="set-row"><div><b>Available</b><small>{chk ? `Checked ${when(chk.checked)}` : "Not checked yet"}</small></div>
        <div className="set-v"><span>{chk ? `${chk.updates.length} update${chk.updates.length === 1 ? "" : "s"}` : "—"}</span>
          <button className="ctl-pill" data-up="check" disabled={running || !ctl} onClick={() => void settingsAction("update_check", {}, "Checking for updates")}>Check now</button>
          <button className="ctl-pill" data-up="start" disabled={running || (!!chk && !chk.updates.length) || !ctl} onClick={updateInstall}>Install updates</button></div></div>
    </div>
    {chk && chk.updates.length > 0 && <><div className="set-h sub"><h2>Will be updated</h2></div><div className="upks"><Pkgs l={chk.updates} /></div></>}
    {chk && chk.held.length > 0 && <><div className="set-h sub"><h2>Held</h2><span className="muted">kernel &amp; boot — custom kernel, update by hand</span></div>
      <div className="upks held"><Pkgs l={chk.held} /></div></>}
    {!!u.log?.length && <><div className="set-h sub"><h2>{running ? "Progress" : "Last update log"}</h2></div>
      <pre className="uplog" id="uplog" ref={logRef}>{u.log.join("\n")}</pre></>}
    {!running && st.status === "done" && st.restart === "suggested" && (
      <div className="set-actions"><span className="set-p">Updated. A reboot is recommended so everything uses the new versions.</span>
        <button className="ctl-pill warn" data-act="reboot" disabled={!ctl} onClick={() => systemAction("reboot")}>Reboot now</button></div>)}
  </>;
});
