// Switching (NMEA 2000 load controller), ported from app.js toggleSwitch / holdable / holdStart / holdEnd.
//
// Kept exactly: view-only refusal, no-live-status refusal, triggers are momentary (drawer locks), locked-out
// channels and channels that need a vehicle input refuse to turn ON, a tripped channel asks before the OFF that
// clears it, dimmers are held (press = hold the virtual switch, release = let go), one command at a time.
// New: a plain on/off tap flips the tile at once (optimistic) and rolls back with the device's reason when the
// result is refused or unverified.
import { store } from "../state/store";
import { gateway } from "../state/ws";
import { canControlDev, control } from "./control";
import { getOptimistic, setOptimistic } from "./optimistic";
import { askConfirm, toast } from "./ui";

export interface Sw {
  id: string; name: string; icon?: string; group?: string; on: boolean; state?: string; channel?: number;
  kind?: string; blocked?: boolean; home?: boolean; home_pos?: number | null; requires_input?: number | null;
  rating_a?: number | null; level?: number | null; amps?: number | null;
}
export interface Trigger { id: string; name: string; channel: number }
export interface VInput { input: number; name: string; icon?: string; on?: boolean | null; volts?: number | null }

const g = store.get;
export const swList = () => g<Sw[]>("switches.list") ?? [];
const triggers = () => g<Trigger[]>("switching.triggers") ?? [];
const inputs = () => g<VInput[]>("switching.inputs") ?? [];
/** The optimistic-overlay id of a switch tile. */
export const swOptId = (id: string) => `sw.${id}`;
export const isTrigger = (s: Sw) => triggers().some(t => t.channel === s.channel);

/** Dimmers are held, not toggled (tap = on/off, hold = ramp, in the load controller's own logic). */
export const holdable = (s: Sw | undefined): s is Sw => !!s && s.kind === "dimmer" && !!s.channel && s.state !== "error"
  && s.state !== "stale" && s.state !== "n/a" && !isTrigger(s);

export function toggleSwitch(id: string): void {
  const s = swList().find(x => x.id === id);
  if (!s) return;
  if (!canControlDev("n2k") || !s.channel) { toast("Switch control is off (view only)"); return; }
  if (s.state === "stale" || s.state === "n/a") { toast(`${s.name}: no live status`, "bad"); return; }
  const trig = triggers().find(t => t.channel === s.channel);
  if (trig) {                                   // momentary: the load controller's own timer switches it off
    if (s.on) { toast(`${s.name}: active, the load controller switches it off`, "ok"); return; }
    control("trigger", { trigger: trig.id }, trig.name, "n2k");
    return;
  }
  if (s.blocked && !s.on) { toast(`${s.name} is locked out (safety)`, "bad"); return; }
  if (!s.on && s.requires_input != null) {
    const inp = inputs().find(i => i.input === s.requires_input);
    if (!inp || inp.on !== true) { toast(`${s.name} needs ${inp ? inp.name : "input " + s.requires_input} on`, "bad"); return; }
  }
  if (s.state === "error") {                    // tripped: OFF clears it
    askConfirm(`${s.name} tripped`, "The load controller reports a fault on this channel (it shut the output off). Switch it off to clear the fault? Check the load and wiring before switching it on again.",
      () => control("set", { channel: s.channel, on: false }, `${s.name} reset`, "n2k"));
    return;
  }
  // a verified overlay that the state hasn't caught up with yet is the truth for the next tap
  const o = getOptimistic<boolean>(swOptId(s.id));
  const want = !(o?.phase === "ok" ? o.value : s.on);
  const p = control("set", { channel: s.channel, on: want }, `${s.name} ${want ? "on" : "off"}`, "n2k");
  if (!p) return;                               // not sent: nothing to show
  setOptimistic(swOptId(s.id), want, p, () => swList().find(x => x.id === s.id)?.on === want, ["switches.list"]);
}

// ── Hold to dim ──
// The load controller's own counter sees the press for as long as the tile is held. AetherRV holds the
// channel's virtual switch ON from pointerdown to pointerup; the backend makes a quick tap a full-length press
// and releases a hold that never ends after 8 s. Release listeners are on the document/window (installed once),
// not on the tile: the tile may re-render or the finger may slide off while it's down.
let swHold: { pointerId: number; channel: number; name: string } | null = null;

function holdSend(channel: number, on: boolean, name: string): void {
  // straight to the gateway (no rights re-check): a release must go out even if rights changed mid-press
  void gateway.control("n2k", "hold", { channel, on }).then(r => {
    if (!r.ok) toast(`${name}: ${r.detail}`, "bad");
  });
}
/** pointerdown on a switch tile. Returns true when the press belongs to a dimmer (the click must not toggle). */
export function holdStart(pointerId: number, id: string): boolean {
  const s = swList().find(x => x.id === id);
  if (!holdable(s)) return false;               // everything else is a click (toggleSwitch)
  if (swHold) return true;
  if (!canControlDev("n2k")) { toast("Switch control is off (view only)"); return true; }
  swHold = { pointerId, channel: s.channel!, name: s.name };
  holdSend(s.channel!, true, s.name);
  return true;
}
export function holdEnd(pointerId?: number): void {
  if (!swHold || (pointerId != null && pointerId !== swHold.pointerId)) return;
  const h = swHold;
  swHold = null;
  holdSend(h.channel, false, h.name);
}
/** A click on a tile that is handled as a hold (dimmer): ignore it. */
export const heldTile = (id: string) => holdable(swList().find(x => x.id === id));
export const holding = () => swHold != null;

let installed = false;
/** Once, at start: release a held dimmer on any pointerup/cancel, window blur or the page being hidden. */
export function installSwitchLogic(): void {
  if (installed) return;
  installed = true;
  for (const ev of ["pointerup", "pointercancel"] as const) document.addEventListener(ev, e => holdEnd(e.pointerId));
  window.addEventListener("blur", () => holdEnd());
  document.addEventListener("visibilitychange", () => { if (document.hidden) holdEnd(); });
}
