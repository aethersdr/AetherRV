// Systems page logic, ported from app.js renderSystems: status cards (Cerbo, CAN buses, network targets, the Espar
// bus, the transmit gate), the NMEA 2000 device table and the bus inspector. Pure functions over a getter.
type Get = <T = unknown>(k: string) => T | undefined;
export type Cls = "good" | "bad" | "idle";
/** A shown value; a key the gateway sends as null (its module is down) reads "—", not "null". */
const show = (v: unknown) => (v == null ? "—" : String(v));

// ── CAN bus rate sparklines: one sample per bus per second, 2 min (app.js rateHist) ──
export type RateHist = Map<string, number[]>;
export const RATE_N = 120;
/** Bus names in the order their keys arrived ("bus.<name>.<field>"). */
export const busNames = (keys: readonly string[]) => [...new Set(keys.filter(k => k.startsWith("bus.")).map(k => k.split(".")[1]!))];
export function sampleRates(hist: RateHist, names: readonly string[], get: Get): void {
  for (const n of names) {
    let h = hist.get(n);
    if (!h) hist.set(n, (h = []));
    h.push(get<number>(`bus.${n}.rate`) || 0);
    if (h.length > RATE_N) h.shift();
  }
}
export function sparkPath(vals: readonly number[]): string {
  if (vals.length < 2) return "";
  const max = Math.max(1, ...vals), w = 300, h = 44;
  return vals.map((v, i) => `${i ? "L" : "M"}${(i / (vals.length - 1) * w).toFixed(1)},${(h - 3 - v / max * (h - 6)).toFixed(1)}`).join("");
}

// ── Module health (Rust gateway: svc.<service>.up / svc.<service>.age_s). Absent with the Python backend. ──
export interface Mod { name: string; up: boolean; down: string }
/** Every module the gateway reports, down ones first; `down` is how long it's been silent ("" while up). */
export function modules(keys: readonly string[], get: Get): Mod[] {
  const out: Mod[] = [];
  for (const k of keys) {
    if (!k.startsWith("svc.") || !k.endsWith(".up")) continue;
    const name = k.slice(4, -3);
    const up = get(k) !== false;
    const age = get<number | null>(`svc.${name}.age_s`);
    out.push({ name, up, down: up ? "" : age == null ? "" : age < 60 ? "< 1 min" : `${Math.round(age / 60)} min` });
  }
  return out.sort((a, b) => Number(a.up) - Number(b.up) || a.name.localeCompare(b.name));
}
/** Is this module reported down (false only with the Rust gateway, and only when it really is)? */
export const svcDown = (get: Get, name: string) => get(`svc.${name}.up`) === false;

export interface Card { cls: Cls; status: string }

export function cerboCard(get: Get): Card {
  if (svcDown(get, "victron")) return { cls: "bad", status: "victron module down" };
  return get("victron.connected") ? { cls: "good", status: `connected · ${get("victron.portal") ?? ""}` } : { cls: "bad", status: "not connected" };
}

export interface BusView extends Card { channel: string; meta: string }
export function busCard(get: Get, n: string): BusView {
  const g = (k: string) => get(`bus.${n}.${k}`);
  const rate = g("rate") as number | null | undefined;
  return {
    channel: show(g("channel")),
    cls: !g("connected") ? "bad" : (rate ?? 0) > 0 ? "good" : "idle",
    status: !g("connected") ? "down" : (rate ?? 0) > 0 ? `${rate} frames/s` : "open · no traffic",
    meta: `${show(g("frames"))} frames · ${show(g("ids"))} IDs · ${show(g("errors"))} errors`,
  };
}

export interface Target { name?: string; host?: string; up?: boolean | null; reason?: string | null; down_for?: number | null;
  rtt_ms?: number | null; tcp?: number | null; hint?: string | null }
export interface TargetView extends Card { name: string; host: string; hint: string }
export function targetCard(t: Target): TargetView {
  const cls: Cls = t.up === false ? "bad" : t.up ? "good" : "idle";
  const status = t.up === false ? `${t.reason} · ${t.down_for != null ? Math.max(1, Math.round(t.down_for / 60)) + " min" : ""}`
    : t.up ? `online · ${t.rtt_ms ?? "—"} ms${t.tcp ? ` · port ${t.tcp} ok` : ""}` : "checking…";
  return { name: show(t.name), host: show(t.host), cls, status, hint: t.up === false && t.hint ? t.hint : "" };
}

export interface EsparView extends Card { show: boolean; channel: string; meta: string; asleepSince: number | null }
/** The Espar heater bus (hat1): shown once the decoder has reported (or, with module health, while its module is down). */
export function esparCard(get: Get, now = Date.now()): EsparView {
  const E = (k: string) => get(`climate.espar.${k}`);
  const down = svcDown(get, "espar");
  const awake = E("asleep") === false;
  const last = E("last_seen") as number | null | undefined;
  const cls: Cls = down ? "bad" : E("state") === "error" ? "bad" : awake ? "good" : "idle";
  const status = down ? "espar module down"
    : !E("heard") ? "bus asleep · nothing heard since start"
    : awake ? `awake · heater ${E("state")}${E("controller") ? " · EasyStart Pro on the bus" : ""}`
    : `bus asleep (normal when idle) · last heard ${last == null ? "—" : agoAt(last, now)}`;
  return {
    show: E("heard") != null || down, channel: String(get("bus.hat1.channel") || "hat1"), cls, status,
    // app.js always said "listen-only" (written before AetherRV could control the Espar): follow the transmit gate
    meta: `Airtronic S2 · ${(get<unknown[]>("espar.ids") || []).length} frame IDs · ${get("tx.enabled.espar") === true ? "control enabled (transmit gate open)" : "listen-only, the Pi never transmits"}${E("fault") ? ` · fault ${E("fault")}` : ""}`,
    asleepSince: !down && E("heard") && !awake && last != null ? last : null,
  };
}
/** lib/format's ago() at a given time (so a test, or a 30 s tick, decides "now"). */
export function agoAt(t: number, nowMs: number): string {
  const s = nowMs / 1000 - t;
  return s < 60 ? "just now" : s < 3600 ? `${Math.round(s / 60)} min ago` : s < 86400 ? `${Math.round(s / 3600)} h ago` : `${Math.round(s / 86400)} d ago`;
}

export function txGateCard(get: Get): Card {
  const blocked = Object.values(get<Record<string, number>>("tx.blocked") || {}).reduce((a, b) => a + (+b || 0), 0);
  const txOn = Object.entries(get<Record<string, boolean>>("tx.enabled") || {}).filter(([, v]) => v).map(([k]) => k);
  return { cls: txOn.length ? "good" : "idle",
    status: txOn.length ? `control enabled: ${txOn.join(", ")} · ${blocked} blocked` : `all devices read-only · ${blocked} blocked` };
}

// ── NMEA 2000 devices ──
export interface Dev { source?: number; model?: string; serial?: string; manufacturer?: string; class?: string;
  software?: string; online?: boolean; age?: number }
export interface DevView { online: boolean; model: string; serial: string; maker: string; cls: string; addr: string; sw: string; heard: string }
export const heard = (age: number | undefined) => age == null ? "—" : age < 5 ? "now" : age < 120 ? `${Math.round(age)} s ago` : `${Math.round(age / 60)} min ago`;
export function devView(d: Dev | undefined): DevView | null {
  if (!d) return null;
  return { online: !!d.online, model: d.model || "unidentified", serial: d.serial || "", maker: d.manufacturer || "—",
    cls: d.class || "—", addr: show(d.source), sw: d.software || "—", heard: heard(d.age) };
}
export function devSub(devs: readonly Dev[] | undefined): string {
  if (!devs?.length) return "";
  const off = devs.filter(d => !d.online).length;
  return `${devs.length - off} online${off ? ` · ${off} offline` : ""} · heard passively (no requests sent)`;
}

// ── Bus inspector ──
export const INSPECT_TABS = [["n2k", "NMEA 2000", "n2k.pgns"], ["cmd", "N2K commands", "n2k.commands"], ["espar", "Espar", "espar.ids"],
  ["rixens", "Rixens", "rixens.ids"]] as const;
export const inspectKey = (sel: string) => (INSPECT_TABS.find(t => t[0] === sel) ?? INSPECT_TABS[0])[2];
/** A cell as app.js wrote it: arrays comma-joined, everything else as String() would. */
export const cellText = (v: unknown) => String(Array.isArray(v) ? v.join(",") : v);
