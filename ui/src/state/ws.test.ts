import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { Store } from "./store";
import { CONN_LOST, GatewayClient, NO_CONFIRM, normalizeResult, NOT_SENT, type SocketLike } from "./ws";

class FakeSocket implements SocketLike {
  static all: FakeSocket[] = [];
  readyState = 0;
  sent: Record<string, unknown>[] = [];
  onopen: ((ev: unknown) => void) | null = null;
  onclose: ((ev: unknown) => void) | null = null;
  onerror: ((ev: unknown) => void) | null = null;
  onmessage: ((ev: { data: unknown }) => void) | null = null;
  constructor(public url: string) { FakeSocket.all.push(this); }
  send(d: string) { this.sent.push(JSON.parse(d) as Record<string, unknown>); }
  close() { this.readyState = 3; }
  // test helpers
  open() { this.readyState = 1; this.onopen?.({}); }
  drop() { this.readyState = 3; this.onclose?.({}); }
  recv(o: object) { this.onmessage?.({ data: JSON.stringify(o) }); }
}
const last = () => FakeSocket.all[FakeSocket.all.length - 1]!;

describe("GatewayClient", () => {
  let s: Store, c: GatewayClient;
  beforeEach(() => {
    vi.useFakeTimers();
    FakeSocket.all = [];
    s = new Store();
    c = new GatewayClient(s, { url: "ws://test/ws", factory: u => new FakeSocket(u) });
    c.start();
  });
  afterEach(() => { c.stop(); vi.useRealTimers(); });

  it("hello, snapshot and delta fill the store; status goes connecting → live", () => {
    expect(s.get("$conn")).toBe("connecting");
    last().open();
    expect(s.get("$conn")).toBe("live");
    last().recv({ type: "hello", control: true, who: "touchscreen" });
    expect(s.get("$control")).toBe(true);
    expect(s.get("$who")).toBe("touchscreen");
    expect(s.get("$ready")).toBeUndefined();
    last().recv({ type: "snapshot", data: { "victron.battery.soc": 55, "a.b": 1 } });
    expect(s.get("$ready")).toBe(true);
    last().recv({ type: "delta", data: { "victron.battery.soc": 56 } });
    expect(s.get("victron.battery.soc")).toBe(56);
    last().recv({ type: "snapshot", data: { "victron.battery.soc": 57 } });   // resync: a.b is gone
    expect(s.has("a.b")).toBe(false);
  });

  it("sends control with app.js's message shape and resolves on control_result", async () => {
    last().open();
    const p = c.control("n2k", "set", { channel: 3, on: true });
    const m = last().sent[0]!;
    expect(m).toEqual({ type: "control", device: "n2k", cmd: "set", value: { channel: 3, on: true }, id: 1 });
    last().recv({ type: "control_result", id: 1, ok: true, result: "ok", detail: "on (verified)" });
    await expect(p).resolves.toEqual({ ok: true, result: "ok", detail: "on (verified)" });
    expect(c.inFlight).toBe(0);
  });

  it("ids increase per command and results are matched by id", async () => {
    last().open();
    const p1 = c.control("rixens", "furnace", true);
    const p2 = c.control("espar", "set", { mode: "off" });
    expect(last().sent.map(m => m.id)).toEqual([1, 2]);
    last().recv({ type: "control_result", id: 2, ok: false, result: "refused", detail: "gate closed" });
    last().recv({ type: "control_result", id: 1, ok: false, result: "unverified", detail: "no echo" });
    await expect(p2).resolves.toMatchObject({ ok: false, result: "refused", detail: "gate closed" });
    await expect(p1).resolves.toMatchObject({ ok: false, result: "unverified", detail: "no echo" });
  });

  it("is never queued: not connected → refused 'not sent', nothing goes out later", async () => {
    const p = c.control("n2k", "set", { channel: 1, on: true });
    await expect(p).resolves.toEqual({ ok: false, result: "refused", detail: NOT_SENT, local: true });
    last().open();
    expect(last().sent).toEqual([]);
  });

  it("times out as unverified (60 s; 150 s for install_update)", async () => {
    last().open();
    const p = c.control("victron", "inverter_mode", 3);
    const q = c.control("starlink", "install_update", null);
    vi.advanceTimersByTime(60_000);
    await expect(p).resolves.toEqual({ ok: false, result: "unverified", detail: NO_CONFIRM, local: true });
    expect(c.inFlight).toBe(1);
    vi.advanceTimersByTime(90_000);
    await expect(q).resolves.toMatchObject({ result: "unverified" });
    // a late result for a timed-out id is ignored
    last().recv({ type: "control_result", id: 1, ok: true, detail: "late" });
  });

  it("a dropped connection settles every pending command as unverified and reconnects with backoff", async () => {
    last().open();
    const p = c.control("n2k", "set", { channel: 1, on: false });
    const first = last();
    first.drop();
    await expect(p).resolves.toEqual({ ok: false, result: "unverified", detail: CONN_LOST, local: true });
    expect(s.get("$conn")).toBe("offline");
    // 250 ms → 500 → 1000 → 2000 → 2000 (capped)
    const waits = [250, 500, 1000, 2000, 2000];
    for (const w of waits) {
      const n = FakeSocket.all.length;
      vi.advanceTimersByTime(w - 1);
      expect(FakeSocket.all.length).toBe(n);
      vi.advanceTimersByTime(1);
      expect(FakeSocket.all.length).toBe(n + 1);
      last().drop();
    }
    // a successful open resets the backoff to 250 ms
    vi.advanceTimersByTime(2000);
    last().open();
    last().drop();
    const n = FakeSocket.all.length;
    vi.advanceTimersByTime(250);
    expect(FakeSocket.all.length).toBe(n + 1);
    expect(first.sent).toHaveLength(1);                     // the command was sent once, never again
  });

  it("ack and ack_all use app.js's shapes", () => {
    last().open();
    expect(c.ack("cold.fridge")).toBe(true);
    expect(c.ackAll()).toBe(true);
    expect(last().sent).toEqual([{ type: "ack", id: "cold.fridge" }, { type: "ack_all" }]);
  });

  it("ignores garbage and unknown message types", () => {
    last().open();
    last().onmessage?.({ data: "not json" });
    last().recv({ type: "error", error: "control is disabled" });
    expect(s.get("$conn")).toBe("live");
  });
});

describe("normalizeResult", () => {
  it("derives result from ok when the Python reference omits it", () => {
    expect(normalizeResult({ ok: true, detail: "done" })).toEqual({ ok: true, result: "ok", detail: "done" });
    expect(normalizeResult({ ok: false, detail: "device not available" }))
      .toEqual({ ok: false, result: "refused", detail: "device not available" });
  });
  it("keeps the Rust gateway's result", () => {
    expect(normalizeResult({ ok: false, result: "unverified", detail: "x" }).result).toBe("unverified");
    expect(normalizeResult({ result: "ok", detail: "y" }).ok).toBe(true);
  });
});
