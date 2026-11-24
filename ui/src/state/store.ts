// Live state mirror with per-key subscriptions.
//
// The gateway sends a flat key → value map (`snapshot`, then `delta`s). A component subscribes to exactly the keys
// it shows (hooks.ts), so a battery reading changing re-renders one number, never a page. Values are compared
// structurally on arrival: an object that arrives again with the same content keeps its old reference and
// notifies nobody (the gateway republishes whole arrays such as `switches.list` when one field changes).
//
// Keys that start with "$" are client-local (connection status, the pending command, optimistic overlays …). A
// snapshot never touches them; everything else is replaced by it.

export type Listener = () => void;
export type Getter = <T = unknown>(key: string) => T | undefined;

export const LOCAL_PREFIX = "$";
export const isLocal = (k: string) => k.startsWith(LOCAL_PREFIX);

/** Structural equality for JSON-like values (the only kind the gateway sends). */
export function deepEqual(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false;
  if (Array.isArray(a)) {
    if (!Array.isArray(b) || a.length !== b.length) return false;
    for (let i = 0; i < a.length; i++) if (!deepEqual(a[i], b[i])) return false;
    return true;
  }
  if (Array.isArray(b)) return false;
  const ka = Object.keys(a as object), kb = Object.keys(b as object);
  if (ka.length !== kb.length) return false;
  for (const k of ka) {
    if (!Object.prototype.hasOwnProperty.call(b, k)) return false;
    if (!deepEqual((a as Record<string, unknown>)[k], (b as Record<string, unknown>)[k])) return false;
  }
  return true;
}

export class Store {
  private values = new Map<string, unknown>();
  private keyListeners = new Map<string, Set<Listener>>();
  private prefixListeners = new Map<string, Set<Listener>>();
  private prefixVersions = new Map<string, number>();
  private batchDepth = 0;
  private dirty = new Set<string>();

  /** Read one key (undefined when absent). Bound, so it can be passed around as a getter. */
  get: Getter = <T,>(key: string) => this.values.get(key) as T | undefined;

  has(key: string): boolean { return this.values.has(key); }

  /** Every key, optionally only those under a prefix ("bus." → bus.n2k.rate, …). */
  keys(prefix = ""): string[] {
    const out: string[] = [];
    for (const k of this.values.keys()) if (k.startsWith(prefix)) out.push(k);
    return out;
  }

  /** Set one key (client-local keys, tests). Notifies only if the value really changed. */
  set(key: string, value: unknown): void {
    this.batch(() => this.write(key, value));
  }

  /** Remove one key. */
  delete(key: string): void {
    if (!this.values.has(key)) return;
    this.batch(() => { this.values.delete(key); this.dirty.add(key); });
  }

  /** `delta`: merge. A null value is stored as null (the gateway's "no reading"), not deleted. */
  applyDelta(data: Record<string, unknown>): void {
    this.batch(() => { for (const k in data) this.write(k, data[k]); });
  }

  /** `snapshot`: replace every non-local key; keys missing from it are deleted (and their listeners told). */
  applySnapshot(data: Record<string, unknown>): void {
    this.batch(() => {
      for (const k of [...this.values.keys()]) {
        if (!isLocal(k) && !Object.prototype.hasOwnProperty.call(data, k)) { this.values.delete(k); this.dirty.add(k); }
      }
      for (const k in data) this.write(k, data[k]);
    });
  }

  /** Group several writes so each listener runs once, after all of them. */
  batch(fn: () => void): void {
    this.batchDepth++;
    try { fn(); } finally {
      if (--this.batchDepth === 0 && this.dirty.size) this.flush();
    }
  }

  subscribe(key: string, fn: Listener): () => void {
    let s = this.keyListeners.get(key);
    if (!s) this.keyListeners.set(key, (s = new Set()));
    s.add(fn);
    return () => { s.delete(fn); if (!s.size) this.keyListeners.delete(key); };
  }

  subscribeKeys(keys: readonly string[], fn: Listener): () => void {
    const offs = keys.map(k => this.subscribe(k, fn));
    return () => offs.forEach(off => off());
  }

  /** Any key under `prefix` (use only where a component genuinely needs many keys). */
  subscribePrefix(prefix: string, fn: Listener): () => void {
    let s = this.prefixListeners.get(prefix);
    if (!s) this.prefixListeners.set(prefix, (s = new Set()));
    s.add(fn);
    return () => { s.delete(fn); if (!s.size) this.prefixListeners.delete(prefix); };
  }

  /** A number that changes whenever a key under `prefix` changes (stable snapshot for useSyncExternalStore). */
  prefixVersion(prefix: string): number { return this.prefixVersions.get(prefix) ?? 0; }

  private write(key: string, value: unknown): void {
    const had = this.values.has(key);
    if (had && deepEqual(this.values.get(key), value)) return;
    this.values.set(key, value);
    this.dirty.add(key);
  }

  private flush(): void {
    const changed = [...this.dirty];
    this.dirty.clear();
    const run = new Set<Listener>();
    for (const k of changed) {
      const s = this.keyListeners.get(k);
      if (s) for (const fn of s) run.add(fn);
    }
    for (const [p, s] of this.prefixListeners) {
      if (changed.some(k => k.startsWith(p))) {
        this.prefixVersions.set(p, (this.prefixVersions.get(p) ?? 0) + 1);
        for (const fn of s) run.add(fn);
      }
    }
    for (const fn of run) {
      try { fn(); } catch (e) { console.error("store listener", e); }
    }
  }
}

/** The app's one store. Tests make their own `new Store()`. */
export const store = new Store();

// ── Client-local keys (reserved "$" namespace). Page agents: add yours here, never as bare strings. ──
export const K = {
  conn: "$conn",                 // "connecting" | "live" | "offline"
  ready: "$ready",               // true once the first snapshot arrived (skeletons until then)
  control: "$control",           // hello.control: may this client send commands at all
  who: "$who",                   // hello.who: "touchscreen" | tailscale name | "viewer"
  pending: "$ctl.pending",       // the one in-flight verified command (app.js ctlPending) or null
  spPending: "$sp.pending",      // setpoint being adjusted (°C) before it's sent / confirmed
  spDragging: "$sp.dragging",    // the Climate dial knob is being dragged
  limPending: "$lim.pending",    // shore current limit being adjusted (A)
  fanPending: "$fan.pending",    // fan % being dragged / sent
  espRun: "$esp.run",            // Espar run time choice (min, null = unlimited)
  toast: "$toast",               // { msg, kind, seq }
  confirm: "$confirm",           // { title, detail, onYes } | null
  modal: "$modal",               // "batt" | "inv" | null
  night: "$night",               // night mode on
  // "$opt.<id>": optimistic overlays (logic/optimistic.ts); "$alerts.sound", "$screen.*": logic/alerts.ts, kiosk.ts
} as const;
