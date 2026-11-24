// Sending commands, ported from app.js (control / dropPending / canControlDev).
//
// Rules kept exactly:
//  • who may control comes from the gateway (hello.control) AND the device's own `<dev>.controls_ok` key;
//  • ONE verified command at a time (app.js ctlPending): while it is in flight, control() refuses another and
//    every control that depends on it shows disabled. espDirect (the Espar following the setpoint) and the
//    dimmer hold have their own slots, as in app.js;
//  • a command is sent once; a lost result (timeout, dropped socket) ends as "unverified", never as a retry.
import { K, store } from "../state/store";
import { gateway, type ControlResult } from "../state/ws";
import { toast } from "./ui";

export type Device = "rixens" | "victron" | "starlink" | "n2k" | "peplink" | "espar";
export interface Pending { id: number; cmd: string; label: string; device: string }

const DEV_OK: Record<string, string> = {
  victron: "victron.controls_ok", starlink: "starlink.controls_ok", n2k: "n2k.controls_ok",
  peplink: "peplink.controls_ok", espar: "espar.controls_ok",
};
/** The state key that says a device's control path is enabled (anything else: the MCS7's). */
export const devOkKey = (d: string) => DEV_OK[d] ?? "climate.rixens.controls_ok";
/** Keys to subscribe to for canControlDev(d). */
export const ctlKeys = (...devs: string[]) => [K.control, K.pending, ...devs.map(devOkKey)];

export const canControlDev = (d: string): boolean => store.get(K.control) === true && store.get(devOkKey(d)) === true;
/** The MCS7 (app.js canControl). */
export const canControl = (): boolean => canControlDev("rixens");
export const ctlPending = (): Pending | null => store.get<Pending | null>(K.pending) ?? null;

let seq = 0;

/** Toast text for a result, as app.js wrote it. */
export function resultText(label: string, r: ControlResult): string {
  if (r.local) return `${label}: ${r.detail}`;
  if (r.ok) return `${label}: ${r.detail}`;
  return r.result === "unverified" ? `${label} not confirmed: ${r.detail}` : `${label} failed: ${r.detail}`;
}

/**
 * Send one verified command (the single-flight slot). Returns null when it was not sent (no right, another
 * command in flight, no connection: each with the same feedback as app.js), else the result promise.
 */
/** The value a command was adjusting (setpoint, shore limit, fan) stops being "pending" once it ends or isn't sent. */
function dropAdjusting(cmd: string): void {
  store.batch(() => {
    if (cmd === "setpoint") store.set(K.spPending, null);
    if (cmd === "shore_limit") store.set(K.limPending, null);
    if (cmd === "fan") store.set(K.fanPending, null);
  });
}

export function control(cmd: string, value: unknown, label: string, device: string = "rixens"): Promise<ControlResult> | null {
  // not sent → nothing may stay shown as "sending…" (app.js dropPending did this for "no connection")
  if (!canControlDev(device)) { dropAdjusting(cmd); return null; }
  const busy = ctlPending();
  if (busy) { dropAdjusting(cmd); toast(`Wait: ${busy.label} is still being confirmed`, "bad"); return null; }
  if (!gateway.connected) { dropAdjusting(cmd); toast(`${label}: not sent — no connection`, "bad"); return null; }
  const p: Pending = { id: ++seq, cmd, label, device };
  store.set(K.pending, p);
  toast(`${label}…`, "busy");
  const res = gateway.control(device, cmd, value);
  void res.then(r => {
    store.batch(() => {
      if (ctlPending()?.id === p.id) store.set(K.pending, null);
      dropAdjusting(cmd);
    });
    toast(resultText(label, r), r.ok ? "ok" : "bad");
  });
  return res;
}

/**
 * A command outside the single-flight slot (app.js espDirect / holdSend): its own result, its own toasts.
 * quiet: no "…" toast and no success toast (the hold-to-dim press/release).
 */
export function controlDirect(device: string, cmd: string, value: unknown, label: string,
                              { quiet = false }: { quiet?: boolean } = {}): Promise<ControlResult> | null {
  if (!canControlDev(device)) return null;
  if (!gateway.connected) { toast(`${label}: not sent — no connection`, "bad"); return null; }
  if (!quiet) toast(`${label}…`, "busy");
  const res = gateway.control(device, cmd, value);
  void res.then(r => {
    if (quiet && r.ok) return;
    toast(quiet ? `${label}: ${r.detail}` : resultText(label, r), r.ok ? "ok" : "bad");
  });
  return res;
}

/** The /api/settings/action path (Settings, and the Alerts page's "Fridge is off"). */
export async function settingsAction(verb: string, args: Record<string, unknown> = {}, label = verb): Promise<unknown> {
  if (store.get(K.control) !== true) { toast("View only — control from the touchscreen or your Tailscale devices", "bad"); return null; }
  toast(`${label}…`, "busy");
  try {
    const r = await (await fetch("/api/settings/action", {
      method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ verb, args }),
    })).json() as { ok?: boolean; error?: string };
    toast(r.ok ? `${label}: done` : `${label} failed: ${r.error}`, r.ok ? "ok" : "bad");
    window.dispatchEvent(new Event("aetherrv:settings-changed"));
    return r;
  } catch (e) { toast(`${label} failed: ${String(e)}`, "bad"); return null; }
}
