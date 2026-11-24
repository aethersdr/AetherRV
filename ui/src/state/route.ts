// Page switching by URL hash (#home, #energy, …), as app.js: replaceState, so pages don't pile up in history.
import { useSyncExternalStore } from "react";

/** [id, title, icon] in rail order; Settings is pinned to the bottom of the rail. */
export const VIEWS = [
  ["home", "Home", "home"], ["energy", "Energy", "bolt"], ["climate", "Climate", "flame"], ["weather", "Weather", "cloud"],
  ["switches", "Switches", "toggle"], ["network", "Network", "globe"], ["alerts", "Alerts", "alert"], ["systems", "Systems", "chip"],
  ["settings", "Settings", "gear"],
] as const;
export type ViewId = (typeof VIEWS)[number][0];
export const FOOT: ReadonlySet<ViewId> = new Set<ViewId>(["settings"]);
export const viewTitle = (v: ViewId) => VIEWS.find(x => x[0] === v)![1];

const parse = (h: string): ViewId => (VIEWS.some(v => v[0] === h.slice(1)) ? h.slice(1) as ViewId : "home");
let current: ViewId = typeof location === "undefined" ? "home" : parse(location.hash);
const listeners = new Set<() => void>();
const notify = () => listeners.forEach(l => l());

export function go(v: ViewId): void {
  if (typeof history !== "undefined") history.replaceState(null, "", `#${v}`);
  if (v === current) return;
  current = v;
  notify();
}
export const currentView = () => current;

if (typeof window !== "undefined") {
  window.addEventListener("hashchange", () => { const v = parse(location.hash); if (v !== current) { current = v; notify(); } });
}

const sub = (cb: () => void) => { listeners.add(cb); return () => { listeners.delete(cb); }; };
export const useView = (): ViewId => useSyncExternalStore(sub, currentView, currentView);
