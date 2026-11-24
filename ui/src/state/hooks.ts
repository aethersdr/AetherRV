// React bindings for the store. Every hook subscribes to exactly the keys it names, through
// useSyncExternalStore, and returns a cached value so a component re-renders only when what it reads changed.
import { createContext, useCallback, useContext, useRef, useSyncExternalStore } from "react";
import { deepEqual, K, store as defaultStore, type Getter, type Store } from "./store";

/** Tests can render with their own Store; the app uses the singleton. */
export const StoreContext = createContext<Store>(defaultStore);
export const useStore = () => useContext(StoreContext);

/** One key. Re-renders only when that key's value changes (structurally). */
export function useKey<T = unknown>(key: string): T | undefined {
  const s = useStore();
  const sub = useCallback((cb: () => void) => s.subscribe(key, cb), [s, key]);
  const get = useCallback(() => s.get<T>(key), [s, key]);
  return useSyncExternalStore(sub, get, get);
}

/** Several keys, as a tuple in the same order. The tuple is cached until one of them changes. */
export function useKeys(keys: readonly string[]): readonly unknown[] {
  const s = useStore();
  const sig = keys.join("\u0000");
  const cache = useRef<unknown[] | null>(null);
  const sub = useCallback((cb: () => void) => s.subscribeKeys(keys, cb), [s, sig]);
  const get = useCallback(() => {
    const prev = cache.current;
    if (prev && prev.length === keys.length && keys.every((k, i) => Object.is(s.get(k), prev[i]))) return prev;
    return (cache.current = keys.map(k => s.get(k)));
  }, [s, sig]);
  return useSyncExternalStore(sub, get, get);
}

/**
 * A value derived from some keys: re-renders only when the derived value changes (deepEqual by default).
 * `select` may close over props; it's read at call time, so it needn't be stable.
 *   const live = useSelect(["$control", "n2k.controls_ok"], g => g("$control") === true && g("n2k.controls_ok") === true);
 */
export function useSelect<T>(keys: readonly string[], select: (get: Getter) => T,
                             isEqual: (a: T, b: T) => boolean = deepEqual): T {
  const s = useStore();
  const sig = keys.join("\u0000");
  const selRef = useRef(select);
  selRef.current = select;
  const eqRef = useRef(isEqual);
  eqRef.current = isEqual;
  const cache = useRef<{ v: T } | null>(null);
  const sub = useCallback((cb: () => void) => s.subscribeKeys(keys, cb), [s, sig]);
  const get = useCallback(() => {
    const next = selRef.current(s.get);
    const prev = cache.current;
    if (prev && eqRef.current(prev.v, next)) return prev.v;
    cache.current = { v: next };
    return next;
  }, [s]);
  return useSyncExternalStore(sub, get, get);
}

/** One field (or any projection) of one key: e.g. a single switch out of `switches.list`. */
export function useKeySelector<V, T>(key: string, select: (v: V | undefined) => T,
                                     isEqual: (a: T, b: T) => boolean = deepEqual): T {
  return useSelect([key], g => select(g<V>(key)), isEqual);
}

/**
 * Every key under a prefix: re-renders when any of them changes and returns the prefix's version (read the
 * values with store.get inside render). Use only where a component genuinely needs many keys.
 */
export function usePrefix(prefix: string): number {
  const s = useStore();
  const sub = useCallback((cb: () => void) => s.subscribePrefix(prefix, cb), [s, prefix]);
  const get = useCallback(() => s.prefixVersion(prefix), [s, prefix]);
  return useSyncExternalStore(sub, get, get);
}

/** Can this client send commands at all (the gateway's hello)? Per-device rights: logic/control.ts. */
export function useControl(): boolean {
  return useKey<boolean>(K.control) === true;
}

/** Connection status for the Live chip. */
export function useConn(): "connecting" | "live" | "offline" {
  return useKey<"connecting" | "live" | "offline">(K.conn) ?? "connecting";
}

/** True once the first snapshot arrived (before that, render skeletons). */
export function useReady(): boolean {
  return useKey<boolean>(K.ready) === true;
}
