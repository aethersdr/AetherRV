// Network page logic: WAN order and the confirmation texts (invented WANs).
import { describe, expect, it } from "vitest";
import { dragResult, dur, INSTALL_ASK, locked, orderConfirm, priorityConfirm, rsrpQ, sinrQ, wanIcon, wanLists, type Wan } from "./wan";

const W: Wan[] = [
  { id: 1, name: "Sat Link", enabled: true, priority: 1, connected: true, starlink: true, message: "Connected" },
  { id: 2, name: "LTE Modem", enabled: true, priority: 2, connected: false, cellular: { sim: 1 }, message: "Standby" },
  { id: 3, name: "Wi-Fi A", enabled: true, priority: 2, connected: true, wifi: { ssid: "x" }, message: "Connected" },
  { id: 4, name: "Wi-Fi B", enabled: false, priority: null, wifi: {}, message: "Disabled" },
  { id: 5, name: "Spare", enabled: false, priority: null, message: "License Required" },
];

describe("WAN lists", () => {
  it("enabled by priority, then the one in use, then router order; locked ones hidden", () => {
    const { on, off } = wanLists(W);
    expect(on.map(w => w.id)).toEqual([1, 3, 2]);
    expect(off.map(w => w.id)).toEqual([4]);
    expect(locked(W[4]!)).toBe(true);
    expect(locked({ id: 9, name: "x", message: "licence needed" })).toBe(true);
  });
  it("icons and quality words", () => {
    expect(W.map(wanIcon)).toEqual(["dish", "signal", "wifi", "wifi", "router"]);
    expect([rsrpQ(-75), rsrpQ(-85), rsrpQ(-95), rsrpQ(-105), rsrpQ(null)]).toEqual(["excellent", "good", "fair", "poor", null]);
    expect([sinrQ(25), sinrQ(15), sinrQ(5), sinrQ(-2)]).toEqual(["excellent", "good", "fair", "poor"]);
    expect([dur(90000), dur(3700), dur(null)]).toEqual(["1 d 1 h", "1 h 1 m", "—"]);
  });
});

describe("drag result", () => {
  const seq = [1, 3, 2, null, 4];
  it("reorder within the enabled list", () => {
    expect(dragResult(seq, 2, 0)).toEqual({ order: [2, 1, 3], disabled: [4] });
  });
  it("across the separator disables / enables", () => {
    expect(dragResult(seq, 0, 3)).toEqual({ order: [3, 2], disabled: [1, 4] });
    expect(dragResult(seq, 4, 3)).toEqual({ order: [1, 3, 2, 4], disabled: [] });
  });
});

describe("confirmations", () => {
  it("a reorder lists the new priorities (P4 at most) and the router's reaction", () => {
    const a = orderConfirm(W, [2, 1, 3], [4], "Sat Link");
    expect(a).toEqual({ title: "Change the WAN order?",
      detail: "Priorities: P1 LTE Modem, P2 Sat Link, P3 Wi-Fi A. The router re-evaluates which link carries traffic right away; if the active connection changes, internet drops for a few seconds and remote (Tailscale) access may reconnect." });
  });
  it("disabling the active link warns that traffic moves", () => {
    const a = orderConfirm(W, [3, 2], [1, 4], "Sat Link");
    expect(a).toMatchObject({ title: "Disable Sat Link?" });
    expect((a as { detail: string }).detail).toContain(" Disable: Sat Link. Sat Link is carrying your internet now; traffic moves to the next link and drops for a few seconds.");
  });
  it("enabling, and both at once", () => {
    expect(orderConfirm(W, [1, 3, 2, 4], [], "Sat Link")).toMatchObject({ title: "Enable Wi-Fi B?" });
    expect(orderConfirm(W, [4, 3, 2], [1], null)).toMatchObject({ title: "Change the WAN order?" });
  });
  it("nothing enabled is refused before any question", () => {
    expect(orderConfirm(W, [], [1, 2, 3, 4], "Sat Link")).toEqual({ error: "At least one WAN must stay enabled" });
  });
  it("priority and install texts (app.js, word for word)", () => {
    expect(priorityConfirm(W[1]!, 1).title).toBe("Set LTE Modem to priority 1?");
    expect(priorityConfirm(W[1]!, 1).detail).toContain("remote (Tailscale) access to this console may reconnect.");
    expect(INSTALL_ASK.title).toBe("Install the Starlink update?");
    expect(INSTALL_ASK.detail).toContain("A firmware install can't be undone.");
  });
});
