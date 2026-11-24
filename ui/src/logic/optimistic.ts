// Optimistic toggles (owner ask: instant feedback). A control shows its new value the moment it's tapped, while
// the command is still being verified; the overlay ends when
//   • the result is refused / unverified (roll back: the control shows what the device reports; control()
//     already toasted the device's reason), or
//   • the reported state matches the new value, or
//   • 3 s after an "ok" result (the state delta normally lands within ~33 ms of it).
// Components read it with useOptimistic(id): { value, phase: "sending" | "ok" } | undefined.
import { useKey } from "../state/hooks";
import { store } from "../state/store";
import type { ControlResult } from "../state/ws";

export const OPT = "$opt.";
export interface Overlay<T = unknown> { value: T; phase: "sending" | "ok" }
export const optKey = (id: string) => OPT + id;

const live = new Map<string, { timer?: ReturnType<typeof setTimeout>; off?: () => void; seq: number }>();
let seq = 0;

export function clearOptimistic(id: string): void {
  const e = live.get(id);
  if (e) { clearTimeout(e.timer); e.off?.(); live.delete(id); }
  store.delete(optKey(id));
}

/**
 * Show `value` for `id` until the command settles. `matches()` says whether the real state already shows it;
 * `watch` are the keys that real state lives in.
 */
export function setOptimistic<T>(id: string, value: T, p: Promise<ControlResult>, matches: () => boolean,
                                 watch: readonly string[], graceMs = 3000): void {
  clearOptimistic(id);
  const me = ++seq;
  live.set(id, { seq: me });
  store.set(optKey(id), { value, phase: "sending" } satisfies Overlay<T>);
  void p.then(r => {
    const e = live.get(id);
    if (!e || e.seq !== me) return;               // superseded by a newer tap
    if (!r.ok || matches()) { clearOptimistic(id); return; }
    store.set(optKey(id), { value, phase: "ok" } satisfies Overlay<T>);
    e.off = store.subscribeKeys(watch, () => { if (matches()) clearOptimistic(id); });
    e.timer = setTimeout(() => clearOptimistic(id), graceMs);
  });
}

export const getOptimistic = <T,>(id: string) => store.get<Overlay<T>>(optKey(id));
export const useOptimistic = <T,>(id: string) => useKey<Overlay<T>>(optKey(id));
