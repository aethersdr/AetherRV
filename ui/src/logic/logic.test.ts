// The safety rules ported from app.js, against a stubbed gateway (nothing is sent anywhere).
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { K, store } from "../state/store";
import { gateway, type ControlResult } from "../state/ws";
import { control } from "./control";
import { espFollowSetpoint, hqTap, hqView, HQ, nudgeSetpoint, sendSetpoint, spDisplay } from "./heat";
import { getOptimistic } from "./optimistic";
import { holdEnd, holdStart, toggleSwitch, swOptId } from "./switches";

type Call = { device: string; cmd: string; value: unknown };
let calls: Call[];
let settle: ((r: ControlResult) => void)[];

function reset(state: Record<string, unknown>, ctl = true) {
  for (const k of store.keys("$")) if (k !== K.espRun) store.delete(k);
  store.applySnapshot(state);
  store.set(K.control, ctl);
}
const toastMsg = () => (store.get<{ msg: string }>(K.toast) ?? { msg: "" }).msg;
const base = {
  "climate.rixens.controls_ok": true, "espar.controls_ok": true, "n2k.controls_ok": true, "victron.controls_ok": true,
  "climate.cabin.setpoint": 20, "climate.rixens.live": true, "climate.rixens.sources": [], "climate.rixens.available": { electric: true, engine: true, preheat: true },
};

beforeEach(() => {
  vi.useFakeTimers();
  calls = []; settle = [];
  vi.spyOn(gateway, "connected", "get").mockReturnValue(true);
  vi.spyOn(gateway, "control").mockImplementation((device: string, cmd: string, value: unknown) => {
    calls.push({ device, cmd, value });
    return new Promise<ControlResult>(r => settle.push(r));
  });
  reset(base);
});
afterEach(() => { vi.useRealTimers(); });

describe("control(): one verified command at a time", () => {
  it("sends, holds the slot, and frees it on the result", async () => {
    expect(control("furnace", true, "Burner on")).not.toBeNull();
    expect(store.get(K.pending)).toMatchObject({ cmd: "furnace", label: "Burner on" });
    expect(toastMsg()).toBe("Burner on…");
    expect(control("engine", true, "Engine on")).toBeNull();                 // refused while the first is in flight
    expect(calls).toHaveLength(1);
    settle[0]!({ ok: true, result: "ok", detail: "on (verified)" });
    await vi.runAllTicks(); await Promise.resolve();
    expect(store.get(K.pending)).toBeNull();
    expect(toastMsg()).toBe("Burner on: on (verified)");
  });
  it("view only: nothing is sent", () => {
    reset(base, false);
    expect(control("furnace", true, "Burner on")).toBeNull();
    expect(calls).toHaveLength(0);
  });
  it("a device whose control is off: nothing is sent", () => {
    reset({ ...base, "climate.rixens.controls_ok": false });
    expect(control("furnace", true, "Burner on")).toBeNull();
    expect(calls).toHaveLength(0);
  });
  it("no connection: not sent, said so", () => {
    vi.spyOn(gateway, "connected", "get").mockReturnValue(false);
    expect(control("furnace", true, "Burner on")).toBeNull();
    expect(toastMsg()).toBe("Burner on: not sent — no connection");
  });
  it("a setpoint that can't be sent doesn't stay 'sending…' (disconnected ± tap)", () => {
    vi.spyOn(gateway, "connected", "get").mockReturnValue(false);
    nudgeSetpoint(+1);
    expect(store.get(K.spPending)).not.toBeNull();
    vi.advanceTimersByTime(1500);
    expect(calls).toHaveLength(0);
    expect(store.get(K.spPending)).toBeNull();
    expect(spDisplay()).toBe(68);
    expect(toastMsg()).toBe("Setpoint 69°: not sent — no connection");
  });
  it("a shore limit refused because another command is in flight doesn't stay pending", () => {
    control("furnace", true, "Burner on");
    store.set(K.limPending, 16);
    expect(control("shore_limit", 16, "Shore limit 16 A", "victron")).toBeNull();
    expect(store.get(K.limPending)).toBeNull();
  });
});

describe("shared setpoint (MCS7 + Espar)", () => {
  const heating = { ...base, "climate.espar.ctl.active": true, "climate.espar.ctl.mode": "heat", "climate.espar.ctl.setpoint_c": 20 };
  it("a heating Espar follows a setpoint inside 10–30 °C", () => {
    reset(heating);
    espFollowSetpoint(22);
    expect(calls).toEqual([{ device: "espar", cmd: "set", value: { mode: "heat", setpoint_c: 22, run_min: null } }]);
  });
  it("below its range turns the Espar OFF; above it keeps its setpoint; same value sends nothing", () => {
    reset(heating);
    espFollowSetpoint(8);
    expect(calls.map(c => (c.value as { mode: string }).mode)).toEqual(["off"]);
    espFollowSetpoint(31);
    espFollowSetpoint(20.02);
    expect(calls).toHaveLength(1);
  });
  it("an idle Espar is never started by a setpoint change", () => {
    espFollowSetpoint(22);
    expect(calls).toHaveLength(0);
  });
  it("one change goes to the MCS7 and a heating Espar at the same time", () => {
    reset(heating);
    sendSetpoint(21, 70);
    expect(calls.map(c => `${c.device}:${c.cmd}`)).toEqual(["espar:set", "rixens:setpoint"]);
    expect(calls[1]!.value).toBe(21);
  });
  it("± is debounced: one absolute value, 1.5 s after the last tap, clamped to 35–90 °F", () => {
    expect(spDisplay()).toBe(68);
    nudgeSetpoint(+1); nudgeSetpoint(+1); nudgeSetpoint(+1);
    expect(spDisplay()).toBe(71);
    expect(calls).toHaveLength(0);
    vi.advanceTimersByTime(1500);
    expect(calls).toHaveLength(1);
    expect(calls[0]).toMatchObject({ device: "rixens", cmd: "setpoint" });
    expect(Math.round((calls[0]!.value as number) * 1.8 + 32)).toBe(71);
  });
  it("± can't change the setpoint while another MCS7 command is pending (unless the Espar heats)", () => {
    control("furnace", true, "Burner on");
    nudgeSetpoint(+1);
    expect(store.get(K.spPending)).toBeUndefined();
  });
});

describe("heat buttons and the low-battery lockout", () => {
  const b = (k: string) => HQ.find(x => x.k === k)!;
  it("lockout locks anything that would START heat, except engine heat; OFF stays possible", () => {
    reset({ ...base, "power.heat_lockout": true, "climate.rixens.sources": ["electric"], "climate.espar.state": "off" });
    expect(hqView(b("furnace")).locked).toBe(true);
    expect(hqView(b("engine")).locked).toBe(false);
    expect(hqView(b("electric")).locked).toBe(false);                      // on: turning it off is allowed
    expect(hqView(b("espar")).locked).toBe(true);
    hqTap(b("furnace"));
    expect(calls).toHaveLength(0);
  });
  it("an MCS7 button flips at once and rolls back when not verified", async () => {
    hqTap(b("furnace"));
    expect(calls).toEqual([{ device: "rixens", cmd: "furnace", value: true }]);
    expect(getOptimistic("hq.furnace")).toEqual({ value: true, phase: "sending" });
    settle[0]!({ ok: false, result: "unverified", detail: "no answer from the MCS7" });
    await Promise.resolve(); await Promise.resolve();
    expect(getOptimistic("hq.furnace")).toBeUndefined();
    expect(toastMsg()).toBe("Burner on not confirmed: no answer from the MCS7");
  });
  it("the Espar button asks before heating", () => {
    reset({ ...base, "climate.espar.state": "off" });
    hqTap(b("espar"));
    expect(calls).toHaveLength(0);
    expect(store.get<{ title: string }>(K.confirm)?.title).toMatch(/^Start the Espar heating to/);
  });
});

describe("switches", () => {
  const sw = (o: Record<string, unknown>) => ({ id: "1.1", name: "Lamp", on: false, state: "off", channel: 1, kind: "switch", ...o });
  it("refuses to turn on a channel that needs an input that is off", () => {
    reset({ ...base, "switches.list": [sw({ requires_input: 2 })], "switching.inputs": [{ input: 2, name: "Ignition", on: false }] });
    toggleSwitch("1.1");
    expect(calls).toHaveLength(0);
    expect(toastMsg()).toBe("Lamp needs Ignition on");
  });
  it("refuses a locked-out channel and a channel without live status", () => {
    reset({ ...base, "switches.list": [sw({ blocked: true }), sw({ id: "1.2", state: "stale", channel: 2 })] });
    toggleSwitch("1.1");
    expect(toastMsg()).toBe("Lamp is locked out (safety)");
    toggleSwitch("1.2");
    expect(toastMsg()).toBe("Lamp: no live status");
    expect(calls).toHaveLength(0);
  });
  it("a tripped channel asks before the OFF that clears it", () => {
    reset({ ...base, "switches.list": [sw({ state: "error", on: false })] });
    toggleSwitch("1.1");
    expect(calls).toHaveLength(0);
    store.get<{ onYes: () => void }>(K.confirm)!.onYes();
    expect(calls).toEqual([{ device: "n2k", cmd: "set", value: { channel: 1, on: false } }]);
  });
  it("a trigger channel is momentary (no on/off, no optimistic flip)", () => {
    reset({ ...base, "switches.list": [sw({ channel: 9 })], "switching.triggers": [{ id: "locks", name: "Locks", channel: 9 }] });
    toggleSwitch("1.1");
    expect(calls).toEqual([{ device: "n2k", cmd: "trigger", value: { trigger: "locks" } }]);
    expect(getOptimistic(swOptId("1.1"))).toBeUndefined();
  });
  it("a dimmer is held from press to release, and only one hold at a time", () => {
    reset({ ...base, "switches.list": [sw({ kind: "dimmer" }), sw({ id: "1.2", kind: "dimmer", channel: 2 })] });
    expect(holdStart(7, "1.1")).toBe(true);
    expect(holdStart(8, "1.2")).toBe(true);                                // second finger: ignored
    holdEnd(8);                                                             // not the holding pointer
    holdEnd(7);
    expect(calls.map(c => c.value)).toEqual([{ channel: 1, on: true }, { channel: 1, on: false }]);
  });
  it("the release is sent even if control was revoked mid-press", () => {
    reset({ ...base, "switches.list": [sw({ kind: "dimmer" })] });
    holdStart(1, "1.1");
    store.set(K.control, false);
    holdEnd();
    expect(calls.map(c => (c.value as { on: boolean }).on)).toEqual([true, false]);
  });
});
