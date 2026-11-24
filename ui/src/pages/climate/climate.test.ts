// Climate page logic: the dial geometry (same numbers as web/charts.js) and the heater tap rules (confirm first,
// lockout, engine exempt), against a stubbed gateway: nothing is sent anywhere.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { K, store } from "../../state/store";
import { gateway, type ControlResult } from "../../state/ws";
import type { ConfirmState } from "../../logic/ui";
import { angOf, arc, DIAL_R, onKnob, pt, ticks, valueAt } from "./dial";
import { destTap, espFanTap, espHeatTap, fluidClass, srcTap } from "./Heaters";

describe("dial geometry (charts.js dial / app.js dialValueAt)", () => {
  it("has the reference's radius and sweep", () => {
    expect(DIAL_R).toBe(194);
    expect(angOf(40, 40, 90)).toBe(135);
    expect(angOf(90, 40, 90)).toBe(405);
    expect(angOf(20, 40, 90)).toBe(135);                     // clamped
    expect(ticks(40, 90)).toHaveLength(51);
    expect(ticks(40, 90).filter(t => t.major)).toHaveLength(11);
  });
  it("draws the same arc path as charts.js", () => {
    // charts.js: arc(135, 405) for the track on a 440 px dial with a 24 px stroke
    expect(arc(135, 405)).toBe("M82.8,357.2A194,194 0 1 1 357.2,357.2");
    expect(arc(135, 200)).toMatch(/ 0 0 1 /);                // short arcs use the small-arc flag
  });
  it("maps a pointer angle to a whole degree; the bottom gap snaps to the nearer end", () => {
    const at = (v: number) => { const [x, y] = pt(angOf(v, 40, 90)); return valueAt(x - 220, y - 220, 40, 90); };
    for (const v of [40, 55, 68, 72, 90]) expect(at(v)).toBe(v);
    expect(valueAt(-1, 100, 40, 90)).toBe(40);               // just left of straight down → low end
    expect(valueAt(1, 100, 40, 90)).toBe(90);                // just right → high end
  });
  it("grabs only within 60 units of the knob", () => {
    const [x, y] = pt(angOf(68, 40, 90));
    expect(onKnob(x - 220, y - 220, 68, 40, 90)).toBe(true);
    expect(onKnob(x - 220 + 59, y - 220, 68, 40, 90)).toBe(true);
    expect(onKnob(x - 220 + 61, y - 220, 68, 40, 90)).toBe(false);
    expect(onKnob(0, 0, 68, 40, 90)).toBe(false);            // the centre
  });
});

// ── heater taps ──
type Call = { device: string; cmd: string; value: unknown };
let calls: Call[];
const base = {
  "climate.rixens.controls_ok": true, "espar.controls_ok": true, "climate.rixens.live": true, "climate.cabin.setpoint": 20,
  "climate.rixens.sources": ["furnace"], "climate.rixens.available": { electric: true, engine: true, preheat: true },
  "climate.rixens.thermostat": false, "climate.rixens.hot_water": true, "climate.rixens.preheat": false,
  "climate.espar.ctl.active": false, "climate.espar.ctl.mode": "off", "power.heat_lockout": false,
};
function reset(state: Record<string, unknown>, ctl = true) {
  for (const k of store.keys("$")) if (k !== K.espRun) store.delete(k);
  store.applySnapshot(state);
  store.set(K.control, ctl);
}
const confirmShown = () => store.get<ConfirmState | null>(K.confirm) ?? null;
const yes = () => { const c = confirmShown()!; store.set(K.confirm, null); c.onYes(); };

beforeEach(() => {
  calls = [];
  vi.spyOn(gateway, "connected", "get").mockReturnValue(true);
  vi.spyOn(gateway, "control").mockImplementation((device: string, cmd: string, value: unknown) => {
    calls.push({ device, cmd, value });
    return new Promise<ControlResult>(() => {});            // never settles: the slot stays busy
  });
  reset(base);
});
afterEach(() => { store.set(K.confirm, null); });

describe("MCS7 sources and destinations", () => {
  it("ask first, on and off; the command goes only on Confirm", () => {
    srcTap("electric");
    expect(confirmShown()?.title).toBe("Turn on electric?");
    expect(calls).toHaveLength(0);
    yes();
    expect(calls).toEqual([{ device: "rixens", cmd: "electric", value: true }]);
    reset(base);
    srcTap("furnace");
    expect(confirmShown()?.title).toBe("Turn off furnace?");
    yes();
    expect(calls.at(-1)).toEqual({ device: "rixens", cmd: "furnace", value: false });
  });
  it("lockout: an off heat source can't start; engine is exempt; an on one can be turned off", () => {
    reset({ ...base, "power.heat_lockout": true });
    srcTap("electric");
    expect(confirmShown()).toBeNull();
    srcTap("engine");
    expect(confirmShown()?.title).toBe("Turn on engine?");
    store.set(K.confirm, null);
    srcTap("furnace");
    expect(confirmShown()?.title).toBe("Turn off furnace?");
  });
  it("lockout: hot water / pre-heat locked when off, cabin heat (thermostat) not", () => {
    reset({ ...base, "power.heat_lockout": true });
    destTap("preheat");
    expect(confirmShown()).toBeNull();
    destTap("thermostat");
    expect(confirmShown()?.title).toBe("Turn cabin heat on?");
    store.set(K.confirm, null);
    destTap("hot_water");                                     // on → may be turned off
    expect(confirmShown()?.title).toBe("Turn hot water off?");
  });
  it("pre-heat not enabled on this MCS7: nothing", () => {
    reset({ ...base, "climate.rixens.available": { preheat: false } });
    destTap("preheat");
    expect(confirmShown()).toBeNull();
  });
  it("view only / another command in flight: nothing", () => {
    reset(base, false);
    srcTap("electric"); destTap("thermostat"); espHeatTap(); espFanTap();
    expect(confirmShown()).toBeNull();
    reset(base);
    store.set(K.pending, { id: 1, cmd: "fan", label: "Fan 50%", device: "rixens" });
    srcTap("electric"); destTap("thermostat"); espHeatTap();
    expect(confirmShown()).toBeNull();
    expect(calls).toHaveLength(0);
  });
});

describe("Espar badges", () => {
  it("heat asks first; under lockout it does nothing; while heating it turns off without asking", () => {
    espHeatTap();
    expect(confirmShown()?.title).toMatch(/^Start the Espar heating to/);
    expect(calls).toHaveLength(0);
    store.set(K.confirm, null);
    reset({ ...base, "power.heat_lockout": true });
    espHeatTap();
    expect(confirmShown()).toBeNull();
    reset({ ...base, "power.heat_lockout": true, "climate.espar.ctl.active": true, "climate.espar.ctl.mode": "heat" });
    espHeatTap();
    expect(confirmShown()).toBeNull();
    expect(calls).toEqual([{ device: "espar", cmd: "set", value: { mode: "off", setpoint_c: 20, run_min: null } }]);
  });
  it("fan only is not locked out, and asks first", () => {
    reset({ ...base, "power.heat_lockout": true });
    espFanTap();
    expect(confirmShown()?.title).toBe("Run the Espar fan only?");
  });
});

describe("coolant pill colour", () => {
  const cls = (s: Record<string, unknown>) => fluidClass(<T,>(k: string) => s[k] as T);
  it("follows the outlet temperature, else the MCS7's own indicator", () => {
    expect(cls({ "climate.rixens.outlet_c": 70 })).toBe("hot");          // 158 °F
    expect(cls({ "climate.rixens.outlet_c": 55 })).toBe("warm");         // 131 °F
    expect(cls({ "climate.rixens.outlet_c": 20, "climate.rixens.sources": ["furnace"] })).toBe("cold-on");
    expect(cls({ "climate.rixens.outlet_c": 20 })).toBe("cold-off");
    expect(cls({ "climate.rixens.glycol": "ready" })).toBe("warm");
  });
});
