// Energy page logic: the numbers app.js showed, character for character (invented data).
import { describe, expect, it } from "vitest";
import { socColor } from "../../lib/format";
import { bankPct, bankState, battPct, BS, cellTiles, discClip, flowDur, flowOf, headView, hueOf, nudgeLimit, recordBank, rowView,
  sparkPaths, splitUnit, sumSeries, type BankHist, type Ble } from "./bank";

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
      "victron.battery.ttg": 3 * 86400, "victron.battery.power": -60 }));
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
