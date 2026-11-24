// Alerts: types, the chime and the chrome side effects (app.js renderAlertChrome / chime / playChime).
import { prefs } from "../lib/prefs";
import { K, store } from "../state/store";
import { kioskWake } from "./kiosk";

export type Severity = "warning" | "serious" | "critical";
export interface Alert { id: string; severity: Severity; title: string; detail: string; since: number; acked?: boolean; source?: string }
export interface AlertLog { t: number; event: string; id: string; severity: Severity | string; title: string }

export const SEV: Record<string, number> = { warning: 1, serious: 2, critical: 3 };
export const SEV_LABEL: Record<string, string> = { warning: "Warning", serious: "Serious", critical: "Critical" };
export const SRC_ICON: Record<string, string> = { victron: "battery", ruuvi: "fridge", n2k: "can", can: "can", rixens: "flame" };

export const SOUND_KEY = "$alerts.sound";
store.set(SOUND_KEY, prefs.get("vp.sound", "1") === "1");
export const soundOn = () => store.get(SOUND_KEY) === true;
export function toggleSound(): void {
  const on = !soundOn();
  store.set(SOUND_KEY, on);
  prefs.set("vp.sound", on ? "1" : "0");
  if (on) playChime("serious");
}

export const activeAlerts = () => store.get<Alert[]>("alerts.active") ?? [];
export const topSeverity = (unacked: Alert[]) =>
  unacked.reduce<string>((m, a) => ((SEV[a.severity] ?? 0) > (SEV[m] ?? 0) ? a.severity : m), "warning");

// ── Two-tone chime (WebAudio, no asset). New serious/critical always; warnings only in day mode.
// Unacknowledged criticals repeat every 60 s. ──
let audio: AudioContext | undefined;
const heard = new Set<string>();
let lastRepeat = 0;
function tone(f: number, t0: number, d: number, ctx: AudioContext) {
  const o = ctx.createOscillator(), gn = ctx.createGain();
  o.frequency.value = f; o.type = "sine";
  gn.gain.setValueAtTime(0.0001, t0);
  gn.gain.exponentialRampToValueAtTime(0.25, t0 + 0.02);
  gn.gain.exponentialRampToValueAtTime(0.0001, t0 + d);
  o.connect(gn).connect(ctx.destination); o.start(t0); o.stop(t0 + d + 0.05);
}
export function playChime(sev: string): void {
  try {
    audio ||= new AudioContext();
    const t = audio.currentTime + 0.05;
    if (sev === "critical") { tone(880, t, .25, audio); tone(660, t + .3, .25, audio); tone(880, t + .6, .4, audio); }
    else { tone(660, t, .25, audio); tone(880, t + .28, .35, audio); }
  } catch { /* audio unavailable */ }
}
function chime(unacked: Alert[]): void {
  const night = document.documentElement.classList.contains("night");
  const fresh = unacked.filter(a => !heard.has(a.id + a.severity));
  unacked.forEach(a => heard.add(a.id + a.severity));
  if (!soundOn()) return;
  const loud = fresh.filter(a => a.severity !== "warning" || !night);
  if (loud.length) { playChime(loud.some(a => a.severity === "critical") ? "critical" : "serious"); lastRepeat = Date.now(); return; }
  if (unacked.some(a => a.severity === "critical") && Date.now() - lastRepeat > 60000) { playChime("critical"); lastRepeat = Date.now(); }
}

let critSeen = new Set<string>();
function onAlerts(): void {
  const act = activeAlerts();
  const unacked = act.filter(a => !a.acked);
  // wake the kiosk screen when a critical alert fires (new, re-raised or escalated)
  const crit = new Set(unacked.filter(a => a.severity === "critical").map(a => a.id));
  if ([...crit].some(id => !critSeen.has(id))) kioskWake();
  critSeen = crit;
  document.body.classList.toggle("flash-critical", unacked.some(a => a.severity === "critical"));
  chime(unacked);
}

let installed = false;
/** Once, at start: chime, flash and wake follow `alerts.active` on every page (app.js did this per render). */
export function installAlertLogic(): void {
  if (installed) return;
  installed = true;
  store.subscribe("alerts.active", onAlerts);
  store.subscribe(K.ready, onAlerts);
  // app.js re-checked the 60 s critical repeat on every render (several per second); here a timer does it
  setInterval(() => { if (store.get(K.ready)) chime(activeAlerts().filter(a => !a.acked)); }, 5000);
}
