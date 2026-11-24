// Peplink WAN logic for the Network page, ported from app.js renderNetwork / the #wan-card handlers. Pure, so the
// confirmation texts and the order a drag produces are unit-tested.

export interface Cell {
  carrier?: string | null; network?: string | null; signal_level?: number | null; roaming?: string | null; ca?: boolean | null;
  sim?: number | null; rat?: string | null; band?: string | null; rsrp?: number | null; sinr?: number | null;
}
export interface WifiInfo { ssid?: string | null; level?: number | null; channel?: number | null; band?: string | null }
export interface Wan {
  id: number; name: string; message?: string | null; led?: string | null; enabled?: boolean; priority?: number | null;
  uptime?: number | null; connected?: boolean; starlink?: boolean; cellular?: Cell | null; wifi?: WifiInfo | null;
}
export interface Allowance { conn: number; sim?: number | null; usage_mb: number; limit_mb: number; percent: number }

export const rsrpQ = (v: number | null | undefined) => v == null ? null : v >= -80 ? "excellent" : v >= -90 ? "good" : v >= -100 ? "fair" : "poor";
export const sinrQ = (v: number | null | undefined) => v == null ? null : v >= 20 ? "excellent" : v >= 13 ? "good" : v >= 0 ? "fair" : "poor";
export const dur = (s: number | null | undefined) => s == null ? "—"
  : s >= 86400 ? `${Math.floor(s / 86400)} d ${Math.floor(s % 86400 / 3600)} h` : `${Math.floor(s / 3600)} h ${Math.floor(s % 3600 / 60)} m`;

/** "License Required" WANs can't be enabled: hidden from the disabled list, no drag handle. */
export const locked = (w: Wan) => /licen[cs]e/i.test(w.message || "");

/** Enabled WANs in priority order (P1 first; same priority: the one in use, then router order), then the disabled
 *  ones that could be enabled. */
export function wanLists(wans: readonly Wan[]): { on: Wan[]; off: Wan[] } {
  const on = wans.filter(w => w.enabled)
    .sort((a, b) => (a.priority ?? 99) - (b.priority ?? 99) || (+!!b.connected - +!!a.connected) || a.id - b.id);
  return { on, off: wans.filter(w => !w.enabled && !locked(w)) };
}

/** A drag moved row `from` to slot `to` in [...on, SEPARATOR, ...off] (null = the separator). */
export function dragResult(seq: readonly (number | null)[], from: number, to: number): { order: number[]; disabled: number[] } {
  const s = seq.slice();
  s.splice(to, 0, ...s.splice(from, 1));
  const sep = s.indexOf(null);
  return { order: s.slice(0, sep) as number[], disabled: s.slice(sep + 1) as number[] };
}

export interface Ask { title: string; detail: string }

/** The confirmation for a new WAN order (app.js endWanDrag), or an error when nothing would stay enabled. */
export function orderConfirm(wans: readonly Wan[], order: readonly number[], disabled: readonly number[], active: string | null | undefined): Ask | { error: string } {
  if (!order.length) return { error: "At least one WAN must stay enabled" };
  const W = (id: number) => wans.find(w => w.id === id) ?? ({ id, name: String(id) } as Wan);
  const off = disabled.filter(id => W(id).enabled), on = order.filter(id => W(id).enabled === false);
  const act = off.find(id => W(id).name === active);
  const names = (ids: number[]) => ids.map(id => W(id).name).join(", ");
  const title = off.length && !on.length ? `Disable ${names(off)}?` : on.length && !off.length ? `Enable ${names(on)}?` : "Change the WAN order?";
  const detail = `Priorities: ${order.map((id, i) => `P${Math.min(i + 1, 4)} ${W(id).name}`).join(", ")}.` +
    (on.length ? ` Enable: ${names(on)}.` : "") + (off.length ? ` Disable: ${names(off)}.` : "") +
    (act != null ? ` ${W(act).name} is carrying your internet now; traffic moves to the next link and drops for a few seconds.` :
      " The router re-evaluates which link carries traffic right away; if the active connection changes, internet drops for a few seconds and remote (Tailscale) access may reconnect.");
  return { title, detail };
}

/** The confirmation for one WAN's priority (the P# picker). */
export function priorityConfirm(w: Wan, p: number): Ask {
  return { title: `Set ${w.name} to priority ${p}?`,
    detail: "The router re-evaluates which link carries traffic right away. If the active connection changes, internet drops for a few seconds, and remote (Tailscale) access to this console may reconnect." };
}

export const INSTALL_ASK: Ask = {
  title: "Install the Starlink update?",
  detail: "The dish restarts to install the downloaded update. Starlink internet drops for a few minutes; if Starlink is the active connection, remote (Tailscale) access to this console drops too. A firmware install can't be undone.",
};

/** The WAN card's icon for a WAN. */
export const wanIcon = (w: Wan) => (w.starlink ? "dish" : w.cellular ? "signal" : w.wifi ? "wifi" : "router");
