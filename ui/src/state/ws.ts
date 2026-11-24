// The gateway WebSocket (/ws): hello → snapshot → deltas in; control / ack / ack_all out (ARCHITECTURE §5).
//
// Commands are sent ONCE: never queued while disconnected, never resent after a reconnect. Every control()
// resolves (never rejects) with { ok, result, detail }:
//   result "ok"          the device did it and the service verified it
//   result "refused"     nothing was changed (view only, device not available, gate closed, out of range, not sent)
//   result "unverified"  it may or may not have happened (no confirmation, connection lost mid-command)
// `detail` is the user-facing text. `local` marks results made up here (timeout, disconnect, not sent) rather
// than by the device's service.
import { K, store as defaultStore, type Store } from "./store";

export type ResultKind = "ok" | "refused" | "unverified";
export interface ControlResult { ok: boolean; result: ResultKind; detail: string; local?: boolean }

/** The subset of WebSocket this client uses (a fake in tests). */
export interface SocketLike {
  readonly readyState: number;
  send(data: string): void;
  close(): void;
  onopen: ((ev: unknown) => void) | null;
  onclose: ((ev: unknown) => void) | null;
  onerror: ((ev: unknown) => void) | null;
  onmessage: ((ev: { data: unknown }) => void) | null;
}
export type SocketFactory = (url: string) => SocketLike;

export interface ClientOptions {
  url?: string | (() => string);
  factory?: SocketFactory;
  /** Reconnect delays: start, doubling to max (250 ms → 2 s). */
  backoffMin?: number;
  backoffMax?: number;
  /** Default command timeout (app.js: 60 s; 150 s for Starlink's install_update). */
  timeoutMs?: number;
}

const OPEN = 1;
const LONG_CMDS: Record<string, number> = { install_update: 150_000 };

export const NOT_SENT = "not sent — no connection";
export const NO_CONFIRM = "no confirmation received — check its status";
export const CONN_LOST = "connection lost — result unknown, check its status";

/** Normalise a control_result: the Python reference sends no `result`, the Rust gateway does. */
export function normalizeResult(m: { ok?: unknown; result?: unknown; detail?: unknown }): ControlResult {
  const r = m.result === "ok" || m.result === "refused" || m.result === "unverified" ? m.result : null;
  const ok = typeof m.ok === "boolean" ? m.ok : r === "ok";
  const result: ResultKind = r ?? (ok ? "ok" : "refused");
  return { ok: ok && result === "ok", result, detail: m.detail == null ? (ok ? "done" : "failed") : String(m.detail) };
}

interface Waiter { resolve: (r: ControlResult) => void; timer: ReturnType<typeof setTimeout> }

export class GatewayClient {
  private sock: SocketLike | null = null;
  private seq = 0;
  private waiters = new Map<number, Waiter>();
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private delay: number;
  private stopped = true;
  private readonly opt: Required<Omit<ClientOptions, "url" | "factory">> & Pick<ClientOptions, "url" | "factory">;

  constructor(private readonly store: Store = defaultStore, opt: ClientOptions = {}) {
    this.opt = { backoffMin: 250, backoffMax: 2000, timeoutMs: 60_000, ...opt };
    this.delay = this.opt.backoffMin;
  }

  private url(): string {
    const u = this.opt.url;
    if (typeof u === "function") return u();
    if (u) return u;
    return `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/ws`;
  }

  start(): void {
    if (!this.stopped) return;
    this.stopped = false;
    this.store.set(K.conn, "connecting");
    this.open();
  }

  stop(): void {
    this.stopped = true;
    if (this.retryTimer) clearTimeout(this.retryTimer);
    this.retryTimer = null;
    const s = this.sock;
    this.sock = null;
    if (s) { s.onclose = s.onmessage = s.onerror = s.onopen = null; try { s.close(); } catch { /* already closed */ } }
    this.failAll(CONN_LOST);
    this.store.set(K.conn, "offline");
  }

  get connected(): boolean { return !!this.sock && this.sock.readyState === OPEN; }

  private open(): void {
    let s: SocketLike;
    try {
      s = (this.opt.factory ?? ((u: string) => new WebSocket(u) as unknown as SocketLike))(this.url());
    } catch {
      this.scheduleRetry();
      return;
    }
    this.sock = s;
    s.onopen = () => {
      this.delay = this.opt.backoffMin;
      this.store.set(K.conn, "live");
    };
    s.onmessage = ev => this.onMessage(ev.data);
    s.onerror = () => { /* onclose follows */ };
    s.onclose = () => {
      if (this.sock !== s) return;
      this.sock = null;
      this.failAll(CONN_LOST);
      this.store.set(K.conn, "offline");
      this.scheduleRetry();
    };
  }

  private scheduleRetry(): void {
    if (this.stopped || this.retryTimer) return;
    const d = this.delay;
    this.delay = Math.min(this.opt.backoffMax, this.delay * 2);
    this.retryTimer = setTimeout(() => { this.retryTimer = null; if (!this.stopped) this.open(); }, d);
  }

  private onMessage(raw: unknown): void {
    let m: Record<string, unknown>;
    try { m = JSON.parse(String(raw)) as Record<string, unknown>; } catch { return; }
    switch (m.type) {
      case "hello":
        this.store.batch(() => {
          this.store.set(K.control, m.control === true);
          this.store.set(K.who, typeof m.who === "string" ? m.who : "viewer");
        });
        break;
      case "snapshot":
        this.store.batch(() => {
          this.store.applySnapshot((m.data ?? {}) as Record<string, unknown>);
          this.store.set(K.ready, true);
          this.store.set(K.conn, "live");
        });
        break;
      case "delta":
        this.store.applyDelta((m.data ?? {}) as Record<string, unknown>);
        break;
      case "control_result": {
        const id = typeof m.id === "number" ? m.id : Number(m.id);
        const w = this.waiters.get(id);
        if (!w) return;
        this.waiters.delete(id);
        clearTimeout(w.timer);
        w.resolve(normalizeResult(m));
        break;
      }
      default:
        break;                      // "error" (unknown request type) and anything newer: ignore
    }
  }

  /** Send a raw message now; false if the socket isn't open (nothing is queued). */
  send(o: object): boolean {
    const s = this.sock;
    if (!s || s.readyState !== OPEN) return false;
    s.send(JSON.stringify(o));
    return true;
  }

  /** One command, sent once. Never rejects. */
  control(device: string, cmd: string, value: unknown, opts: { timeoutMs?: number } = {}): Promise<ControlResult> {
    const id = ++this.seq;
    return new Promise<ControlResult>(resolve => {
      if (!this.send({ type: "control", device, cmd, value, id })) {
        resolve({ ok: false, result: "refused", detail: NOT_SENT, local: true });
        return;
      }
      const ms = opts.timeoutMs ?? LONG_CMDS[cmd] ?? this.opt.timeoutMs;
      const timer = setTimeout(() => {
        this.waiters.delete(id);
        resolve({ ok: false, result: "unverified", detail: NO_CONFIRM, local: true });
      }, ms);
      this.waiters.set(id, { resolve, timer });
    });
  }

  /** Acknowledge one alert (AetherRV's own UI state, never a device). */
  ack(id: string): boolean { return this.send({ type: "ack", id }); }
  ackAll(): boolean { return this.send({ type: "ack_all" }); }

  /** Commands still waiting for their result. */
  get inFlight(): number { return this.waiters.size; }

  private failAll(detail: string): void {
    const ws = [...this.waiters.values()];
    this.waiters.clear();
    for (const w of ws) {
      clearTimeout(w.timer);
      w.resolve({ ok: false, result: "unverified", detail, local: true });
    }
  }
}

/** The app's one connection, started from main.tsx. */
export const gateway = new GatewayClient();
