// Energy page logic: the numbers app.js showed, character for character (invented data).
import { describe, expect, it } from "vitest";
import { socColor } from "../../lib/format";
import { approach, B3, b3FillH, b3Width, b3X, bankPct, bankState, battPct, BS, cellTiles, chargePhase, clampOrbit, discClip, durSeconds, fitDistance, toGoText,
  flowAdvance, flowDur, flowOf, headView, hueOf, nudgeLimit, ORBIT, orbitPos, recordBank, rowView, sparkPaths, splitUnit, sumSeries,
  type BankHist, type Ble } from "./bank";

const A: Ble = { name: "A1", ok: true, soc: 91, voltage: 13.31, current: 2.4, remaining_ah: 410.2, cycles: 12,
  temps: [12.5, 13, 11, 15], cells: [3.327, 3.329, 3.326, 3.328], charge_fet: true, discharge_fet: true, age: 1 };
const B: Ble = { name: "B2", ok: true, soc: 22, voltage: 13.1, current: -2.1, remaining_ah: null, cycles: 30,
  temps: [2, 3, 1, 4], cells: [3.301, 3.305], charge_fet: true, discharge_fet: false, age: 0 };
const C: Ble = { name: "C3", ok: false, error: "timeout" };
const getter = (s: Record<string, unknown>) => <T,>(k: string) => s[k] as T | undefined;

describe("battery bank", () => {
  it("a battery's charge is available Ah ÷ 460 Ah, else its BMS SoC, else nothing", () => {
    expect(battPct(A)).toBeCloseTo(89.17, 2);
    expect(battPct(B)).toBe(22);
    expect(battPct(C)).toBeNull();
    expect(battPct(undefined)).toBeNull();
  });

  it("the headline uses only the batteries reporting Ah; the shunt SoC without any", () => {
    expect(bankPct([A, B, C], 70)).toBeCloseTo(89.17, 2);
    expect(bankPct([B, C], 70)).toBe(70);
    expect(bankPct([], null)).toBeNull();
  });

  it("charging / discharging / standby at ±5 W", () => {
    expect(bankState(6)[0]).toBe("Charging");
    expect(bankState(-6)[0]).toBe("Discharging");
    expect(bankState(5)[0]).toBe("Standby");
    expect(bankState(null)[0]).toBe("");
  });

  it("the header lines", () => {
    const v = headView(getter({ "batteries.ble": [A, B, C], "victron.battery.voltage": 13.218, "victron.battery.current": -4.66,
      "energy.battery.to_go_s": 3 * 86400, "energy.battery.to_go_dir": "empty", "victron.battery.power": -60 }));
    expect(v.line1).toBe("13.22 V · -4.7 A · 3 d to go");
    expect(v.line2).toBe("410 / 1380 Ah · 2 of 3 batteries via Bluetooth");
    expect(v.pctTitle).toBe("1 batteries: available Ah ÷ 460 Ah");
    expect(headView(getter({})).line2).toBe("0 of 0 batteries via Bluetooth");
  });

  it("cell tiles: spread, warning over 50 mV, the coldest temperature below 40 °F", () => {
    const t = cellTiles([A, B, C], 14);
    expect(t[1]).toEqual(["VDIFF", "0.028 V", "", ""]);
    expect(cellTiles([{ ...A, cells: [3.2, 3.3] }], null)[1]![2]).toBe("warn");
    expect(t[4]![0]).toBe("Cells T1/T2 · min");              // B2's 2 °C is below 40 °F: the coldest shows
    expect(t[4]![2]).toBe("t-cold");
    expect(cellTiles([], null).map(x => x[1])).toEqual(["—", "—", "—", "—", "—", "—", "—", "—"]);
  });

  it("table rows: what app.js wrote, and never the read's age", () => {
    const r = rowView(A, socColor)!;
    expect(r).toMatchObject({ ok: true, soc: "91%", volts: "13.31 V", amps: "2.4 A", power: "32 W", ah: "410.2", cycles: "12" });
    expect(rowView({ ...A, age: 99 }, socColor)).toEqual(r);
    expect(rowView(C, socColor)).toEqual({ ok: false, name: "C3", msg: "no data · reconnecting" });
    expect(rowView({ name: "D4", ok: false }, socColor)).toMatchObject({ msg: "connecting…" });
  });
});

describe("3D bank scene", () => {
  it("hue by charge: red < 15 %, amber < 30 %", () => {
    expect([hueOf(null), hueOf(10), hueOf(20), hueOf(30)]).toEqual(["blue", "red", "amber", "blue"]);
  });

  it("each quarter disc fills in turn; the surface shows only on the partly-filled one", () => {
    const full = discClip(100, 3), empty = discClip(0, 0), half = discClip(37.5, 1);
    expect(full.f).toBe(1);
    expect(empty).toMatchObject({ f: 0, height: "0", y: String(BS.yb0 + BS.ry + 2) });
    expect(half.f).toBe(.5);
    expect(+half.surfaceY).toBe(BS.yb0 - BS.step - BS.dh / 2);
  });

  it("bus flow from the bank current", () => {
    expect([flowOf(1), flowOf(-1), flowOf(0.4), flowOf(null)]).toEqual(["chg", "dis", "", ""]);
    expect(flowDur(0)).toBe("4.00s");
    expect(flowDur(60)).toBe("0.50s");
    expect(flowDur(24)).toBe("2.00s");
    expect(flowDur(25.4)).toBe("2.00s");                      // 0.25 s steps: small current changes don't restyle the scene
  });

  it("current history: one point per read (stamped by its age), 2 minutes kept, a sparkline from 2 points", () => {
    const h: BankHist = new Map();
    recordBank(h, [A], 1000);
    recordBank(h, [{ ...A, age: 1 }], 1000);                      // same read again: no new point
    expect(h.get("A1")).toEqual([[999, 2.4]]);
    expect(sparkPaths(h.get("A1")!, 100, 1000)).toBeNull();
    recordBank(h, [{ ...A, current: 3, age: 0 }], 1010);
    const sp = sparkPaths(h.get("A1")!, 100, 1010)!;
    expect(sp.line).toMatch(/^M\d/);
    expect(sp.area.endsWith("Z")).toBe(true);
    recordBank(h, [A], 1200);
    expect(h.get("A1")!.every(p => p[0] >= 1080)).toBe(true);
  });
});

describe("power", () => {
  it("consumption = DC + AC loads, matched on the nearest bucket", () => {
    expect(sumSeries([[0, 10], [60, 20], [120, 30]], [[5, 1], [118, 3]])).toEqual([[0, 11], [60, 21], [120, 33]]);
    expect(sumSeries([[0, 10]], [])).toEqual([[0, 10]]);
    expect(sumSeries(undefined, undefined)).toEqual([]);
  });

  it("shore limit steps by 1 A within 3–50 A", () => {
    expect(nudgeLimit(16, 1)).toBe(17);
    expect(nudgeLimit(15.6, -1)).toBe(15);
    expect(nudgeLimit(50, 1)).toBe(50);
    expect(nudgeLimit(3, -1)).toBe(3);
  });

  it("tile text splits the unit off", () => {
    expect(splitUnit("1.2 kW")).toEqual(["1.2", "kW"]);
    expect(splitUnit("51°")).toEqual(["51°", ""]);
    expect(splitUnit("—")).toEqual(["—", ""]);
  });
});

describe("WebGL bank maths (three-free)", () => {
  it("blocks are centred on the bank, one block + one gap apart", () => {
    expect([0, 1, 2, 3].map(i => b3X(i, 4))).toEqual([-2.325, -0.775, 0.775, 2.325]);
    expect(b3X(0, 1)).toBe(0);
    expect(b3Width(4)).toBeCloseTo(5.65);
    expect(b3Width(1)).toBe(1);
  });

  it("fill height follows the charge, clamped to the block", () => {
    const inner = B3.h - B3.lid - 2 * B3.fillPad;
    expect(b3FillH(null)).toBe(0);
    expect(b3FillH(50)).toBeCloseTo(inner / 2);
    expect(b3FillH(104)).toBeCloseTo(inner);                  // a battery can read past its rating
    expect(b3FillH(-3)).toBe(0);
  });

  it("the camera distance fits the wider of the two spans", () => {
    const wide = fitDistance(8, 1, 8 / 3, 20), tall = fitDistance(1, 8, 8 / 3, 20);
    expect(tall).toBeGreaterThan(wide);
    // the span just fits: half-width / distance = tan(half the horizontal fov)
    const hz = 2 * Math.atan(Math.tan(10 * Math.PI / 180) * 8 / 3);
    expect(4 / wide).toBeCloseTo(Math.tan(hz / 2), 6);
  });

  it("orbit stays in front of and above the bank, and eases home", () => {
    expect(clampOrbit(ORBIT.az + 5, -1)).toEqual([ORBIT.az + ORBIT.azMax, ORBIT.elMin]);
    expect(clampOrbit(ORBIT.az - 5, 3)).toEqual([ORBIT.az - ORBIT.azMax, ORBIT.elMax]);
    let az = 1;
    for (let i = 0; i < 120; i++) az = approach(az, 0.15, 1 / 60, ORBIT.tau);   // 2 s at 60 fps
    expect(Math.abs(az - 0.15)).toBeLessThan(1e-3);
    // frame-rate independent: one 0.1 s step = six 1/60 s steps
    let a = 1, b = 1;
    a = approach(a, 0, 0.1, ORBIT.tau);
    for (let i = 0; i < 6; i++) b = approach(b, 0, 0.1 / 6, ORBIT.tau);
    expect(a).toBeCloseTo(b, 10);
    const [x, y, z] = orbitPos(0, 0, 5, 1);
    expect([x, y, z]).toEqual([0, 1, 5]);
  });

  it("flow dashes run with the current and wrap", () => {
    expect(flowAdvance(0.5, 1, 0.5, 2)).toBeCloseTo(0.25);      // charging: offset falls (dashes run source → battery)
    expect(flowAdvance(0.5, -1, 0.5, 2)).toBeCloseTo(0.75);     // discharging: the other way
    expect(flowAdvance(0.1, 1, 0.5, 2)).toBeCloseTo(0.85);      // wraps into [0, 1)
    expect(durSeconds(flowDur(24))).toBe(2);
  });
});

describe("time to go, both ways (owner, 2026-11-27)", () => {
  const tg = (o: Record<string, unknown>) => toGoText(getter(o));
  it("to full while charging, to go while discharging, nothing at rest or when the label disagrees", () => {
    expect(tg({ "energy.battery.to_go_s": 9720, "energy.battery.to_go_dir": "full", "victron.battery.power": 878 })).toBe(" · 2 h 42 m to full at this rate");
    // the charger's phase: the MultiPlus's, else the solar charger's (a MultiPlus inverting is no charge phase)
    expect(tg({ "energy.battery.to_go_s": 9720, "energy.battery.to_go_dir": "full", "victron.battery.power": 878, "victron.vebus.state": 3 })).toBe(" · 2 h 42 m to full at this rate · Bulk");
    expect(tg({ "energy.battery.to_go_s": 600, "energy.battery.to_go_dir": "full", "victron.battery.power": 90, "victron.vebus.state": 9, "victron.mppt.state": 4 })).toBe(" · 0 h 10 m to full at this rate · Absorption");
    expect(chargePhase(5, null)).toBe("Float");
    expect(chargePhase(9, 0)).toBe("");
    expect(tg({ "energy.battery.to_go_s": 292260, "energy.battery.to_go_dir": "empty", "victron.battery.power": -260 })).toBe(" · 3 d to go");
    expect(tg({ "energy.battery.to_go_s": null, "energy.battery.to_go_dir": null, "victron.battery.power": 2 })).toBe("");
    // the average still says charging while the bank has just started discharging: say nothing rather than "to full"
    expect(tg({ "energy.battery.to_go_s": 9720, "energy.battery.to_go_dir": "full", "victron.battery.power": -200 })).toBe("");
    expect(headView(getter({ "victron.battery.voltage": 13.49, "victron.battery.current": 65.1, "victron.battery.power": 878,
      "energy.battery.to_go_s": 9720, "energy.battery.to_go_dir": "full" })).line1).toBe("13.49 V · 65.1 A · 2 h 42 m to full at this rate");
  });
});
