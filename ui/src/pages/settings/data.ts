// Settings data: GET /api/settings (the admin helper's status, Wi-Fi, Tailscale and updates, plus the screen prefs).
// Held here, outside the live-state store: it is a polled HTTP read, and it can carry a Tailscale sign-in link meant
// only for the screen it was fetched on. Ported from app.js loadSettings / setPoll: fetched when the page is shown,
// every 10 s while it is shown, and at once after every settings action (logic/control.ts settingsAction fires
// `aetherrv:settings-changed`).
import { useCallback, useEffect, useState, useSyncExternalStore } from "react";

export interface Iface { name: string; state?: string; ipv4?: string[] }
export interface Admin {
  ok?: boolean; error?: string; hostname?: string; timezone?: string; kernel?: string; uptime_s?: number | null;
  interfaces?: Iface[]; disk?: { total?: number; free?: number };
}
export interface WifiNet { ssid: string; security?: string; signal_dbm?: number; known?: boolean; connected?: boolean }
export interface Wifi {
  ok?: boolean; error?: string; present?: boolean; device?: string; powered?: boolean; scanning?: boolean; connected?: string | null;
  networks?: WifiNet[]; known?: { ssid: string; security?: string }[];
}
export interface Tailscale {
  ok?: boolean; error?: string; installed?: boolean; version?: string | null; running?: boolean; state?: string | null;
  auth_url?: string | null; name?: string | null; dns?: string | null; ips?: string[]; online?: boolean; tailnet?: string | null; user?: string | null;
}
export interface Pkg { name: string; old: string; new: string }
export interface Update {
  ok?: boolean; error?: string;
  state?: { status?: string; restart?: string };
  log?: string[]; check?: { updates: Pkg[]; held: Pkg[]; checked: number } | null;
  last?: { finished?: string | number; status?: string };
}
export interface SettingsInfo {
  prefs?: Record<string, unknown>; admin?: Admin | null; version?: string | null;
  wifi?: Wifi; tailscale?: Tailscale; update?: Update; error?: string;
}

let info: SettingsInfo | null = null;
let at = 0, busy = false, again = false;
const subs = new Set<() => void>();
const notify = () => subs.forEach(f => f());

/** Fetch now (force) or when the last fetch is older than 10 s. A forced load during a fetch runs right after it. */
export async function loadSettings(force = false): Promise<void> {
  if (busy) { if (force) again = true; return; }
  if (!force && Date.now() - at < 10e3) return;
  busy = true;
  try { info = await (await fetch("/api/settings")).json() as SettingsInfo; } catch (e) { info = { error: String(e) }; }
  at = Date.now(); busy = false;
  notify();
  if (again) { again = false; void loadSettings(true); }
}

const subscribe = (f: () => void) => { subs.add(f); return () => { subs.delete(f); }; };
const snap = () => info;
export const useSettingsInfo = (): SettingsInfo | null => useSyncExternalStore(subscribe, snap, snap);

/** While the Settings page is shown: load, poll every 10 s, reload after every settings action. */
export function useSettingsPolling(): void {
  useEffect(() => {
    void loadSettings(false);
    const t = setInterval(() => void loadSettings(false), 10e3);
    const changed = () => void loadSettings(true);
    window.addEventListener("aetherrv:settings-changed", changed);
    return () => { clearInterval(t); window.removeEventListener("aetherrv:settings-changed", changed); };
  }, []);
}

/** A faster poll while something is in progress (a Wi-Fi scan, a Tailscale sign-in, a system update). */
export function useFastPoll(on: boolean, ms: number): void {
  const cur = useSettingsInfo();                  // re-armed by each new result, not by unrelated re-renders
  useEffect(() => {
    if (!on) return;
    const t = setTimeout(() => void loadSettings(true), ms);
    return () => clearTimeout(t);
  }, [on, ms, cur]);
}

/** Tests: forget what was loaded. */
export function resetSettingsData(): void { info = null; at = 0; busy = false; again = false; notify(); }

/**
 * State that outlives a tab switch (app.js kept these at module level: the unsaved Starlink sleep schedule, this
 * screen's Tailscale sign-in link, the dish power chart's range). Lives in this module, never in the store.
 */
export interface Kept<T> { v: T }
export const kept = <T,>(v: T): Kept<T> => ({ v });
export function useKept<T>(box: Kept<T>): readonly [T, (n: T | ((p: T) => T)) => void] {
  const [v, set] = useState(box.v);
  const put = useCallback((n: T | ((p: T) => T)) => {
    box.v = typeof n === "function" ? (n as (p: T) => T)(box.v) : n;
    set(box.v);
  }, [box]);
  return [v, put] as const;
}
