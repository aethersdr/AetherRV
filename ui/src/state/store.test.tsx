import { act, cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useKey, useKeys, useKeySelector, usePrefix, useSelect, StoreContext } from "./hooks";
import { deepEqual, Store } from "./store";

describe("Store", () => {
  it("notifies only the listeners of keys that changed", () => {
    const s = new Store();
    const a = vi.fn(), b = vi.fn();
    s.subscribe("victron.battery.soc", a);
    s.subscribe("climate.cabin.temp", b);
    s.applyDelta({ "victron.battery.soc": 81 });
    expect(a).toHaveBeenCalledTimes(1);
    expect(b).not.toHaveBeenCalled();
  });

  it("does not notify when a value arrives again unchanged (structurally)", () => {
    const s = new Store();
    s.applyDelta({ "switches.list": [{ id: "1.1", on: false }] });
    const before = s.get("switches.list");
    const fn = vi.fn();
    s.subscribe("switches.list", fn);
    s.applyDelta({ "switches.list": [{ id: "1.1", on: false }] });
    expect(fn).not.toHaveBeenCalled();
    expect(s.get("switches.list")).toBe(before);          // reference kept: no re-render downstream
    s.applyDelta({ "switches.list": [{ id: "1.1", on: true }] });
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it("calls a multi-key listener once per batch", () => {
    const s = new Store();
    const fn = vi.fn();
    s.subscribeKeys(["a", "b", "c"], fn);
    s.applyDelta({ a: 1, b: 2, c: 3 });
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it("snapshot replaces non-local keys, deletes missing ones, and keeps local ones", () => {
    const s = new Store();
    s.applyDelta({ a: 1, b: 2 });
    s.set("$conn", "live");
    const fb = vi.fn(), fl = vi.fn();
    s.subscribe("b", fb);
    s.subscribe("$conn", fl);
    s.applySnapshot({ a: 1, c: 3 });
    expect(s.has("b")).toBe(false);
    expect(fb).toHaveBeenCalledTimes(1);                   // a deleted key notifies its listeners
    expect(s.get("c")).toBe(3);
    expect(s.get("$conn")).toBe("live");
    expect(fl).not.toHaveBeenCalled();
  });

  it("stores null as a value (a sensor with no reading), not as a deletion", () => {
    const s = new Store();
    s.applyDelta({ "climate.outside.temp": null });
    expect(s.has("climate.outside.temp")).toBe(true);
    expect(s.get("climate.outside.temp")).toBeNull();
  });

  it("prefix subscriptions fire for any key under the prefix and bump its version", () => {
    const s = new Store();
    const fn = vi.fn();
    s.subscribePrefix("bus.", fn);
    expect(s.prefixVersion("bus.")).toBe(0);
    s.applyDelta({ "bus.n2k.rate": 10, "bus.hat1.rate": 2, other: 1 });
    expect(fn).toHaveBeenCalledTimes(1);
    expect(s.prefixVersion("bus.")).toBe(1);
    s.applyDelta({ other: 2 });
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it("deepEqual handles nested JSON", () => {
    expect(deepEqual({ a: [1, { b: null }] }, { a: [1, { b: null }] })).toBe(true);
    expect(deepEqual({ a: [1, { b: null }] }, { a: [1, { b: 0 }] })).toBe(false);
    expect(deepEqual([1], { 0: 1 })).toBe(false);
    expect(deepEqual({ a: 1 }, { a: 1, b: undefined })).toBe(false);
  });
});

describe("hooks: a component re-renders only for its keys", () => {
  afterEach(cleanup);
  function setup() {
    const s = new Store();
    s.applySnapshot({ "victron.battery.soc": 80, "climate.cabin.temp": 20, "switches.list": [
      { id: "1.1", name: "Cabin", on: false, amps: 0.1 }, { id: "1.2", name: "Pump", on: false, amps: 0 }] });
    const renders = { soc: 0, cabin: 0, pair: 0, sw: 0, derived: 0, prefix: 0 };
    function Soc() { renders.soc++; return <b data-testid="soc">{String(useKey("victron.battery.soc"))}</b>; }
    function Cabin() { renders.cabin++; return <b>{String(useKey("climate.cabin.temp"))}</b>; }
    function Pair() { renders.pair++; const [a, b] = useKeys(["victron.battery.soc", "climate.cabin.temp"]); return <i>{String(a)}{String(b)}</i>; }
    function Pump() {   // one field of one entry of an array key
      renders.sw++;
      const on = useKeySelector<{ id: string; on: boolean }[], boolean>("switches.list", l => !!l?.find(x => x.id === "1.2")?.on);
      return <i data-testid="pump">{on ? "on" : "off"}</i>;
    }
    function Derived() { renders.derived++; const warm = useSelect(["climate.cabin.temp"], g => (g<number>("climate.cabin.temp") ?? 0) > 25); return <i>{String(warm)}</i>; }
    function Prefix() { renders.prefix++; usePrefix("victron."); return null; }
    const r = render(<StoreContext.Provider value={s}><Soc /><Cabin /><Pair /><Pump /><Derived /><Prefix /></StoreContext.Provider>);
    return { s, renders, r };
  }

  it("a battery reading re-renders the battery number, not its neighbours", () => {
    const { s, renders, r } = setup();
    const base = { ...renders };
    act(() => s.applyDelta({ "victron.battery.soc": 81 }));
    expect(r.getByTestId("soc").textContent).toBe("81");
    expect(renders.soc).toBe(base.soc + 1);
    expect(renders.pair).toBe(base.pair + 1);
    expect(renders.prefix).toBe(base.prefix + 1);
    expect(renders.cabin).toBe(base.cabin);
    expect(renders.sw).toBe(base.sw);
    expect(renders.derived).toBe(base.derived);
  });

  it("an array key changing elsewhere does not re-render a tile whose own field is unchanged", () => {
    const { s, renders, r } = setup();
    const base = { ...renders };
    act(() => s.applyDelta({ "switches.list": [{ id: "1.1", name: "Cabin", on: true, amps: 2.5 }, { id: "1.2", name: "Pump", on: false, amps: 0 }] }));
    expect(renders.sw).toBe(base.sw);
    act(() => s.applyDelta({ "switches.list": [{ id: "1.1", name: "Cabin", on: true, amps: 2.5 }, { id: "1.2", name: "Pump", on: true, amps: 3 }] }));
    expect(renders.sw).toBe(base.sw + 1);
    expect(r.getByTestId("pump").textContent).toBe("on");
  });

  it("a derived value re-renders only when the derived result changes", () => {
    const { s, renders } = setup();
    const base = renders.derived;
    act(() => s.applyDelta({ "climate.cabin.temp": 21 }));
    act(() => s.applyDelta({ "climate.cabin.temp": 22 }));
    expect(renders.derived).toBe(base);
    act(() => s.applyDelta({ "climate.cabin.temp": 26 }));
    expect(renders.derived).toBe(base + 1);
  });

  it("an unchanged delta re-renders nothing", () => {
    const { s, renders } = setup();
    const base = { ...renders };
    act(() => s.applyDelta({ "victron.battery.soc": 80, "climate.cabin.temp": 20 }));
    expect(renders).toEqual(base);
  });
});
