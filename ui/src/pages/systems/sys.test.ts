// Systems page logic: the status lines app.js wrote (invented data), plus module health from the Rust gateway.
import { describe, expect, it } from "vitest";
import { agoAt, busCard, busNames, cellText, cerboCard, devSub, devView, esparCard, heard, inspectKey, modules, sampleRates, sparkPath,
  targetCard, txGateCard, type RateHist } from "./sys";

const getter = (s: Record<string, unknown>) => <T,>(k: string) => s[k] as T | undefined;

describe("status cards", () => {
  it("Cerbo: connected / not connected / its module down", () => {
    expect(cerboCard(getter({ "victron.connected": true, "victron.portal": "p1" }))).toEqual({ cls: "good", status: "connected · p1" });
    expect(cerboCard(getter({ "victron.connected": false }))).toEqual({ cls: "bad", status: "not connected" });
    expect(cerboCard(getter({ "victron.connected": null, "svc.victron.up": false }))).toEqual({ cls: "bad", status: "victron module down" });
  });

  it("CAN bus: frames/s, open with no traffic, down; nulls read as —", () => {
    const g = getter({ "bus.x.channel": "vcan1", "bus.x.connected": true, "bus.x.rate": 4.2, "bus.x.frames": 10, "bus.x.ids": 2, "bus.x.errors": 0 });
    expect(busCard(g, "x")).toEqual({ channel: "vcan1", cls: "good", status: "4.2 frames/s", meta: "10 frames · 2 IDs · 0 errors" });
    expect(busCard(getter({ "bus.x.connected": true, "bus.x.rate": 0 }), "x").status).toBe("open · no traffic");
    expect(busCard(getter({ "bus.x.connected": null, "bus.x.frames": null }), "x")).toMatchObject({ cls: "bad", status: "down", channel: "—",
      meta: "— frames · — IDs · — errors" });
  });

  it("bus names in arrival order, rate history capped at 2 minutes, sparkline from 2 samples", () => {
    expect(busNames(["bus.b.rate", "x.y", "bus.a.rate", "bus.b.ids"])).toEqual(["b", "a"]);
    const h: RateHist = new Map();
    sampleRates(h, ["a"], getter({ "bus.a.rate": 5 }));
    expect(sparkPath(h.get("a")!)).toBe("");
    for (let i = 0; i < 130; i++) sampleRates(h, ["a"], getter({ "bus.a.rate": i }));
    expect(h.get("a")).toHaveLength(120);
    expect(sparkPath([0, 10])).toBe("M0.0,41.0L300.0,3.0");
  });

  it("network targets", () => {
    expect(targetCard({ name: "R", host: "h", up: true, rtt_ms: 2, tcp: 80 })).toMatchObject({ cls: "good", status: "online · 2 ms · port 80 ok" });
    expect(targetCard({ name: "R", host: "h", up: false, reason: "no reply", down_for: 20, hint: "check it" }))
      .toMatchObject({ cls: "bad", status: "no reply · 1 min", hint: "check it" });
    expect(targetCard({ name: "R", host: "h" })).toMatchObject({ cls: "idle", status: "checking…", hint: "" });
  });

  it("Espar bus: awake / asleep / never heard / its module down", () => {
    const now = 2_000_000_000_000;
    const awake = esparCard(getter({ "climate.espar.heard": true, "climate.espar.asleep": false, "climate.espar.state": "heating",
      "climate.espar.controller": true, "espar.ids": [1, 2] }), now);
    expect(awake).toMatchObject({ show: true, cls: "good", status: "awake · heater heating · EasyStart Pro on the bus", asleepSince: null });
    expect(awake.meta).toBe("Airtronic S2 · 2 frame IDs · listen-only, the Pi never transmits");
    const ctl = esparCard(getter({ "climate.espar.heard": true, "climate.espar.asleep": false, "espar.ids": [1, 2], "tx.enabled.espar": true }), now);
    expect(ctl.meta).toBe("Airtronic S2 · 2 frame IDs · control enabled (transmit gate open)");
    const asleep = esparCard(getter({ "climate.espar.heard": true, "climate.espar.asleep": true, "climate.espar.last_seen": now / 1000 - 600 }), now);
    expect(asleep).toMatchObject({ cls: "idle", status: "bus asleep (normal when idle) · last heard 10 min ago" });
    expect(esparCard(getter({ "climate.espar.heard": false }), now).status).toBe("bus asleep · nothing heard since start");
    expect(esparCard(getter({}), now).show).toBe(false);
    expect(esparCard(getter({ "svc.espar.up": false }), now)).toMatchObject({ show: true, cls: "bad", status: "espar module down" });
    expect(esparCard(getter({ "climate.espar.heard": true, "climate.espar.state": "error", "climate.espar.fault": "E10" }), now))
      .toMatchObject({ cls: "bad" });
    expect(agoAt(100, 100_000 + 30_000)).toBe("just now");
  });

  it("transmit gate", () => {
    expect(txGateCard(getter({ "tx.enabled": { n2k: true, espar: false, victron: true }, "tx.blocked": { a: 2, b: 1 } })))
      .toEqual({ cls: "good", status: "control enabled: n2k, victron · 3 blocked" });
    expect(txGateCard(getter({}))).toEqual({ cls: "idle", status: "all devices read-only · 0 blocked" });
  });

  it("modules: down ones first with how long, absent without svc keys", () => {
    const s = { "svc.n2k.up": true, "svc.n2k.age_s": 0.5, "svc.victron.up": false, "svc.victron.age_s": 125, "svc.espar.up": false,
      "svc.espar.age_s": 10, "svc.x.age_s": 1 };
    expect(modules(Object.keys(s), getter(s))).toEqual([
      { name: "espar", up: false, down: "< 1 min" }, { name: "victron", up: false, down: "2 min" }, { name: "n2k", up: true, down: "" }]);
    expect(modules(["victron.connected"], getter({}))).toEqual([]);
  });
});

describe("NMEA 2000 devices and the inspector", () => {
  it("rows and the summary", () => {
    expect(devView({ source: 3, model: "M", serial: "1", manufacturer: "Mk", class: "C", software: "1.0", online: true, age: 3 }))
      .toEqual({ online: true, model: "M", serial: "1", maker: "Mk", cls: "C", addr: "3", sw: "1.0", heard: "now" });
    expect(devView({ online: false, age: 200 })).toMatchObject({ model: "unidentified", maker: "—", heard: "3 min ago" });
    expect([heard(4.9), heard(5), heard(119), heard(120)]).toEqual(["now", "5 s ago", "119 s ago", "2 min ago"]);
    expect(devSub([{ online: true }, { online: false }])).toBe("1 online · 1 offline · heard passively (no requests sent)");
    expect(devSub([])).toBe("");
  });

  it("inspector tabs and cells", () => {
    expect(inspectKey("espar")).toBe("espar.ids");
    expect(inspectKey("bogus")).toBe("n2k.pgns");
    expect([cellText([1, 2]), cellText(null), cellText(""), cellText(7)]).toEqual(["1,2", "null", "", "7"]);
  });
});
