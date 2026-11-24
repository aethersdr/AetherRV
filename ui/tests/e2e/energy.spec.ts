// Energy page against a STUB gateway (page.routeWebSocket; no backend). Invented data only. Checks the layout's
// contents, the confirmations before every MultiPlus / MPPT mode change, the single-flight rule, the debounced shore
// limit shared with the Home inverter overlay, view-only mode, skeletons, the chart and per-key updates; and the WebGL
// battery bank: render on demand, disposal when the page is hidden, orbit, and the SVG scene without WebGL2.
import { expect, test, type Page, type WebSocketRoute } from "@playwright/test";
import { STATE as BASE } from "./fixture";

// WebGL in headless Chrome through SwiftShader (as the weather / network specs)
test.use({ launchOptions: { args: ["--use-angle=swiftshader", "--enable-unsafe-swiftshader", "--ignore-gpu-blocklist"] } });

interface Sent { type: string; device?: string; cmd?: string; value?: unknown; id?: number }

const BLE = [
  { name: "A1", ok: true, soc: 91, voltage: 13.31, current: 2.4, remaining_ah: 410.2, cycles: 12, temps: [12.5, 13, 11, 15],
    cells: [3.327, 3.329, 3.326, 3.328], charge_fet: true, discharge_fet: true, age: 1.2, error: null },
  { name: "B2", ok: true, soc: 22, voltage: 13.1, current: 2.1, remaining_ah: 55.5, cycles: 30, temps: [12, 12.5, 11, 14],
    cells: [3.301, 3.305, 3.299, 3.302], charge_fet: true, discharge_fet: false, age: 0.8, error: null },
  { name: "C3", ok: false, error: "timeout", age: 40 },
];
const STATE: Record<string, unknown> = {
  ...BASE,
  "batteries.ble": BLE, "batteries.ble.current": 4.5, "batteries.ble.reporting": 2, "victron.battery.temp": 14,
  "victron.vebus.mode_adjustable": true, "victron.vebus.limit_adjustable": true, "victron.vebus.current_limit": 16,
  "victron.vebus.ac_out_w": 35, "victron.vebus.shore_connected": false,
  "victron.mppt.instance": 289, "victron.mppt.name": "TestSolar", "victron.mppt.state": 3, "victron.mppt.mode": 1,
};
const HIST = (() => {                       // invented 24 h of 5-minute buckets
  const t1 = 1_800_000_000, pts = (f: (i: number) => number) => Array.from({ length: 288 }, (_, i) => [t1 - (287 - i) * 300, f(i)]);
  return { "victron.pv.power": pts(i => Math.max(0, 300 * Math.sin((i - 70) / 288 * 2 * Math.PI))),
    "energy.dc.load": pts(i => 60 + (i % 12) * 4), "victron.ac.load": pts(() => 20) };
})();

/** Stub /ws (as smoke.spec.ts) + /api/history. `hold`: don't send the snapshot until release() (skeleton tests). */
async function stubGateway(page: Page, opts: { control?: boolean; state?: Record<string, unknown>; replyMs?: number;
  reply?: (m: Sent) => object | null; hold?: boolean } = {}) {
  const sent: Sent[] = [], hist: string[] = [];
  let sock: WebSocketRoute | null = null;
  await page.route("http://127.0.0.1:8091/**", r => r.abort());              // kiosk screen helper: never
  await page.route("**/api/**", r => {
    const u = new URL(r.request().url());
    if (u.pathname === "/api/history") { hist.push(u.search); return r.fulfill({ json: HIST }); }
    return r.fulfill({ json: {} });
  });
  const snapshot = () => sock?.send(JSON.stringify({ type: "snapshot", data: opts.state ?? STATE }));
  await page.routeWebSocket(/\/ws$/, ws => {
    sock = ws;
    ws.onMessage(raw => {
      const m = JSON.parse(String(raw)) as Sent;
      sent.push(m);
      if (m.type !== "control") return;
      const r = opts.reply ? opts.reply(m) : { ok: true, result: "ok", detail: "done (verified)" };
      if (r) setTimeout(() => ws.send(JSON.stringify({ type: "control_result", id: m.id, ...r })), opts.replyMs ?? 150);
    });
    ws.send(JSON.stringify({ type: "hello", control: opts.control ?? true, who: "touchscreen" }));
    if (!opts.hold) snapshot();
  });
  return { sent, hist, send: (o: object) => sock?.send(JSON.stringify(o)), release: snapshot };
}
const controls = (sent: Sent[]) => sent.filter(m => m.type === "control");
const OUT = "../harness/out/ui-pages";

interface Bench { live: number; created: number; disposed: number; frames: number; res: number; lost: number;
  levels: (number | null)[]; hues: string[]; flow: string; calls: number; triangles: number; firstFrameMs: number | null; orbit: number[] }
/** The WebGL bank's counters (only with ?bench=1). */
const bench = (page: Page) => page.evaluate(() => (window as unknown as { __bank3d?: Bench }).__bank3d ?? null);
/** This browser has no WebGL at all: the real detection path runs and must pick the SVG scene. */
const noWebGL = (page: Page) => page.addInitScript(() => {
  const get = HTMLCanvasElement.prototype.getContext;
  HTMLCanvasElement.prototype.getContext = function (this: HTMLCanvasElement, type: string, ...a: unknown[]) {
    return /webgl/.test(type) ? null : (get as (...x: unknown[]) => unknown).call(this, type, ...a);
  } as typeof get;
});
const in3D = async (page: Page) => {
  await expect(page.locator(".bank3d[data-ready] canvas")).toHaveCount(1);
  await expect(page.locator("#bank-scene")).toHaveCount(0);
};

test("the battery card, tiles, controls and chart show the state", async ({ page }) => {
  await stubGateway(page);
  await page.goto("/?bench=1#energy");
  const card = page.locator("#batt-card");
  // headline: available Ah of the batteries reporting it ÷ their capacity: (410.2 + 55.5) / 920
  await expect(card.locator(".bc-soc")).toHaveText("50.6%");
  await expect(card.locator(".bc-state")).toHaveText(/Discharging/i);
  await expect(card).toContainText("466 / 1380 Ah · 2 of 3 batteries via Bluetooth");
  await in3D(page);                                                           // the WebGL bank, one block per battery
  await expect(page.locator(".bank3d .b3-pct")).toHaveText(["89.2%", "12.1%", "—"]);
  await expect(page.locator(".bank3d .b3-name")).toHaveText(["A1", "B2", "C3"]);
  await expect(page.locator(".bank3d .b3-amp")).toHaveText(["2.4 A", "2.1 A", ""]);
  await expect(page.locator(".bank3d .b3-head")).toHaveText("50.6% bank +4.5 A");
  const b = (await bench(page))!;
  expect(b.hues).toEqual(["blue", "red", "off"]);                             // 12 %: red; no data: dimmed, dashed
  expect(b.levels).toEqual([89.2, 12.1, null]);
  expect(b.flow).toBe("chg");                                                 // bank current +4.5 A: buses flow
  await expect(page.locator("#bc-cells")).toContainText("VDIFF0.030 V");
  await expect(page.locator("#bc-rows tr")).toHaveCount(3);
  await expect(page.locator("#bc-rows tr").nth(2)).toContainText("no data · reconnecting");
  await expect(page.locator('#bc-rows tr').nth(1).locator(".mos i").nth(1)).toHaveClass("off");
  await expect(page.locator('.stat[data-k="victron.pv.power"] .stat-v')).toHaveText("120W");
  await expect(page.locator('.stat[data-k="energy.alternator.power"]')).toHaveClass(/\bidle\b/);
  await expect(page.locator("#pwr-ctl")).toContainText("TestInverter");
  await expect(page.locator("#pwr-ctl")).toContainText("Off · 35 W AC out");
  await expect(page.locator("#lim-val")).toHaveText("16 A");
  await expect(page.locator("#pwr-ctl")).toContainText("TestSolar");
  await expect(page.locator("#chart-power path[stroke]")).toHaveCount(2);     // solar + consumption lines
  await page.screenshot({ path: `${OUT}/react-energy-fixture.png` });
});

test("a MultiPlus mode change asks first; Cancel sends nothing, Confirm sends one command", async ({ page }) => {
  const gw = await stubGateway(page);
  await page.goto("/#energy");
  const on = page.locator('#pwr-ctl button[data-mode="3"]');
  await expect(on).toBeEnabled();
  await expect(page.locator('#pwr-ctl button[data-mode="4"]')).toBeDisabled();        // the current mode
  await on.click();
  await expect(page.locator("#modal")).toBeVisible();
  await expect(page.locator("#modal-t")).toHaveText("MultiPlus: On?");
  await expect(page.locator("#modal-d")).toContainText("invert to the AC outlets and charge from shore");
  await page.click("#modal-no");
  await expect(page.locator("#modal")).toBeHidden();
  await page.waitForTimeout(300);
  expect(controls(gw.sent)).toHaveLength(0);
  await on.click();
  await page.click("#modal-yes");
  await expect.poll(() => controls(gw.sent)).toEqual([{ type: "control", device: "victron", cmd: "inverter_mode", value: 3, id: 1 }]);
  await expect(page.locator("#toast")).toContainText("MultiPlus On: done (verified)");
});

test("the solar charger asks before turning off; one command at a time", async ({ page }) => {
  const gw = await stubGateway(page, { replyMs: 1200 });
  await page.goto("/#energy");
  await page.locator('#pwr-ctl button[data-mppt="4"]').click();
  await expect(page.locator("#modal-t")).toHaveText("Solar charger off?");
  await expect(page.locator("#modal-d")).toHaveText("Stops ALL solar charging until turned back on.");
  await page.click("#modal-yes");
  await expect.poll(() => controls(gw.sent).map(m => [m.cmd, m.value])).toEqual([["mppt_mode", 4]]);
  // while it is being confirmed every victron control is disabled (single flight, as app.js)
  await expect(page.locator('#pwr-ctl button[data-mode="3"]')).toBeDisabled();
  await expect(page.locator("#lim-up")).toBeDisabled();
  await expect(page.locator("#toast")).toContainText("Solar charger off: done (verified)");
  await expect(page.locator('#pwr-ctl button[data-mode="3"]')).toBeEnabled();
  expect(controls(gw.sent)).toHaveLength(1);
});

test("shore limit: ± shows at once, sends once 1.5 s after the last tap, shared with the inverter overlay", async ({ page }) => {
  const gw = await stubGateway(page);
  await page.goto("/#energy");
  await expect(page.locator("#lim-val")).toHaveText("16 A");
  for (let i = 0; i < 3; i++) await page.click("#lim-up");
  await page.click("#lim-down");
  await expect(page.locator("#lim-val")).toHaveText("18 A");                    // pending value, before anything is sent
  expect(controls(gw.sent)).toHaveLength(0);
  // the Home inverter overlay shows the same value being adjusted (within the 1.5 s, before it is sent)
  await page.click(".rail button[data-v=home]");
  await page.locator("#inv").click();
  await expect(page.locator("#invm-lim-val")).toHaveText("18 A");
  await page.click("#invm-close");
  await page.click(".rail button[data-v=energy]");
  await expect.poll(() => controls(gw.sent).map(m => [m.cmd, m.value]), { timeout: 4000 }).toEqual([["shore_limit", 18]]);
  await expect(page.locator("#toast")).toContainText("Shore limit 18 A: done (verified)");
  await expect(page.locator("#lim-val")).toHaveText("16 A");                    // back to what the device reports …
  gw.send({ type: "delta", data: { "victron.vebus.current_limit": 18 } });
  await expect(page.locator("#lim-val")).toHaveText("18 A");                    // … until it reports the new limit
  expect(controls(gw.sent)).toHaveLength(1);
});

test("shore limit stays within 3–50 A", async ({ page }) => {
  const gw = await stubGateway(page, { state: { ...STATE, "victron.vebus.current_limit": 49 } });
  await page.goto("/#energy");
  for (let i = 0; i < 3; i++) await page.click("#lim-up");
  await expect(page.locator("#lim-val")).toHaveText("50 A");
  await expect.poll(() => controls(gw.sent).map(m => m.value), { timeout: 4000 }).toEqual([50]);
});

test("view only: every control is disabled, says so, and sends nothing", async ({ page }) => {
  const gw = await stubGateway(page, { control: false });
  await page.goto("/#energy");
  await expect(page.locator("#pwr-ctl .card-h .muted")).toHaveText("view only");
  for (const sel of ['button[data-mode="3"]', 'button[data-mode="1"]', 'button[data-mppt="4"]', "#lim-up", "#lim-down"])
    await expect(page.locator(`#pwr-ctl ${sel}`)).toBeDisabled();
  await page.locator('#pwr-ctl button[data-mode="3"]').click({ force: true });
  await page.locator("#lim-up").click({ force: true });
  await page.waitForTimeout(1800);
  await expect(page.locator("#modal")).toBeHidden();
  expect(controls(gw.sent)).toHaveLength(0);
});

test("victron control off: says so and disables", async ({ page }) => {
  await stubGateway(page, { state: { ...STATE, "victron.controls_ok": false } });
  await page.goto("/#energy");
  await expect(page.locator("#pwr-ctl .card-h .muted")).toHaveText("control off");
  await expect(page.locator('#pwr-ctl button[data-mppt="4"]')).toBeDisabled();
});

test("a tap gives the pressed state before release", async ({ page }) => {
  await stubGateway(page);
  await page.goto("/#energy");
  const b = page.locator("#lim-up");
  const box = (await b.boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await expect(b).toHaveClass(/\bpressed\b/);
  await page.mouse.up();
});

test("skeletons until the first snapshot: no false 'not connected'", async ({ page }) => {
  const gw = await stubGateway(page, { hold: true, state: { ...BASE, "victron.connected": false, "victron.vebus.instance": null } });
  await page.goto("/#energy");
  await expect(page.locator("#pwr-ctl")).toBeVisible();
  await expect(page.locator("html")).toHaveClass(/\bbooting\b/);
  await page.screenshot({ path: `${OUT}/react-energy-skeleton.png` });
  await expect(page.locator("#pwr-ctl")).not.toContainText("not connected");
  await expect(page.locator("#batt-card")).not.toContainText("No per-battery data");
  gw.release();
  await expect(page.locator("#pwr-ctl")).toContainText("Cerbo GX not connected.");
  await expect(page.locator("#batt-card")).toContainText("No per-battery data (Bluetooth adapter off).");
});

test("the power chart loads its range on demand", async ({ page }) => {
  const gw = await stubGateway(page);
  await page.goto("/#energy");
  await expect(page.locator("#chart-power path[stroke]")).toHaveCount(2);
  expect(gw.hist.some(q => q.includes("since=86400"))).toBe(true);
  await page.click('#range-power button[data-s="604800"]');
  await expect(page.locator('#range-power button[data-s="604800"]')).toHaveClass("on");
  await expect.poll(() => gw.hist.some(q => q.includes("since=604800"))).toBe(true);
});

test("per-key updates: a solar reading touches only the Solar tile; a battery read only the battery card", async ({ page }) => {
  const gw = await stubGateway(page);
  await page.goto("/#energy");
  await in3D(page);
  await expect(page.locator('.stat[data-k="victron.pv.power"] .stat-v')).toHaveText("120W");
  await page.waitForTimeout(600);                                            // let the first values settle
  const watch = () => page.evaluate(() => {
    const w = window as unknown as { __muts: Set<string>; __mo?: MutationObserver };
    w.__mo?.disconnect();
    w.__muts = new Set();
    w.__mo = new MutationObserver(ms => ms.forEach(m => {
      const el = (m.target.nodeType === 1 ? m.target : m.target.parentElement) as Element;
      const tile = el.closest(".stat");
      const part = tile ? `stat:${tile.getAttribute("data-k")}` : el.closest("#bank-scene, .bank3d") ? "scene" : el.closest("#bc-rows") ? "rows"
        : el.closest(".bc-head") ? "head" : el.closest("#bc-cells") ? "cells" : el.closest(".card")?.id || el.tagName;
      w.__muts.add(part);
    }));
    w.__mo.observe(document.querySelector(".energy-grid")!, { subtree: true, childList: true, attributes: true, characterData: true });
  });
  const muts = () => page.evaluate(() => [...(window as unknown as { __muts: Set<string> }).__muts]);
  await watch();
  gw.send({ type: "delta", data: { "victron.pv.power": 640 } });
  await expect(page.locator('.stat[data-k="victron.pv.power"] .stat-v')).toHaveText("640W");
  expect(await muts()).toEqual(["stat:victron.pv.power"]);
  // a fresher read of the same values (only `age` changes) re-renders nothing: at most the scene's sparklines move
  await watch();
  gw.send({ type: "delta", data: { "batteries.ble": BLE.map(b => ({ ...b, age: (b.age ?? 0) + 3 })) } });
  await page.waitForTimeout(500);
  for (const m of await muts()) expect(m).toBe("scene");
  // a new current on one battery: that battery's row and its label / sparkline in the scene, nothing else
  await watch();
  gw.send({ type: "delta", data: { "batteries.ble": BLE.map((b, i) => (i === 0 ? { ...b, current: 3.1, age: 0.1 } : b)) } });
  await expect(page.locator(".bank3d .b3-amp").first()).toHaveText("3.1 A");
  await page.waitForTimeout(300);
  expect((await muts()).sort()).toEqual(["rows", "scene"]);
});

test("a battery's level eases to a new reading and changes colour across 30 %", async ({ page }) => {
  const gw = await stubGateway(page);
  await page.goto("/?bench=1#energy");
  await in3D(page);
  const pct0 = page.locator(".bank3d .b3-pct").first();
  await expect(pct0).toHaveText("89.2%");
  expect((await bench(page))!.hues[0]).toBe("blue");
  await page.evaluate(() => {                                                 // every % label shown on the way
    const w = window as unknown as { __pcts: string[] };
    w.__pcts = [];
    const el = document.querySelector(".bank3d .b3-pct")!;
    new MutationObserver(() => w.__pcts.push(el.textContent || "")).observe(el, { childList: true, characterData: true, subtree: true });
  });
  gw.send({ type: "delta", data: { "batteries.ble": BLE.map((b, i) => (i === 0 ? { ...b, remaining_ah: 120 } : b)) } });  // 26.1 %
  await expect(pct0).toHaveText("26.1%");
  await expect.poll(async () => (await bench(page))!.hues[0]).toBe("amber");
  expect((await bench(page))!.levels[0]).toBe(26.1);
  const seen = await page.evaluate(() => (window as unknown as { __pcts: string[] }).__pcts);
  expect(new Set(seen).size).toBeGreaterThan(3);                              // it counted down, not jumped
  expect(seen.map(s => parseFloat(s)).every((v, i, a) => i === 0 || v <= a[i - 1]!)).toBe(true);
  // a small step (< 0.5 %) is written at once
  gw.send({ type: "delta", data: { "batteries.ble": BLE.map((b, i) => (i === 0 ? { ...b, remaining_ah: 121 } : b)) } });
  await expect(pct0).toHaveText("26.3%");
});

test("without WebGL2 the SVG scene shows, in the same box, with the same readings", async ({ page, browser }) => {
  // the WebGL bank's box first
  await stubGateway(page);
  await page.goto("/#energy");
  await in3D(page);
  const box3d = await page.locator(".bank3d").boundingBox();
  // a browser without WebGL
  const ctx = await browser.newContext({ viewport: { width: 1920, height: 1200 } });
  const p2 = await ctx.newPage();
  await noWebGL(p2);
  const gw = await stubGateway(p2);
  await p2.goto("/#energy");
  await expect(p2.locator("#bank-scene .bg-stack")).toHaveCount(3);
  await expect(p2.locator(".bank3d")).toHaveCount(0);
  expect(await p2.locator("#bank-scene").boundingBox()).toEqual(box3d);
  await expect(p2.locator("#bg-pct0")).toHaveText("89.2%");
  await expect(p2.locator("#bg-pct2")).toHaveText("—");
  await expect(p2.locator("#bg-s2")).toHaveClass(/\boff\b/);                   // no data: dimmed, dashed
  await expect(p2.locator("#bg-ls1-0")).toHaveAttribute("fill", "url(#bg-side-red)");   // 12 %: red
  await expect(p2.locator("#bank-scene")).toHaveClass(/\bchg\b/);             // bank current +4.5 A: buses flow
  // its level still eases in
  gw.send({ type: "delta", data: { "batteries.ble": BLE.map((b, i) => (i === 0 ? { ...b, remaining_ah: 120 } : b)) } });
  await expect(p2.locator("#bg-pct0")).toHaveText("26.1%");
  await expect(p2.locator("#bg-ls0-0")).toHaveAttribute("fill", "url(#bg-side-amber)");
  expect(+(await p2.locator("#bg-cr0-1").getAttribute("height"))!).toBeGreaterThan(0);   // 1.1 % into the 2nd quarter
  await expect(p2.locator("#bg-cr0-2")).toHaveAttribute("height", "0");
  await p2.screenshot({ path: `${OUT}/react-energy-svg-fallback.png` });
  // and at phone width the two boxes agree too
  await page.setViewportSize({ width: 390, height: 844 });
  await p2.setViewportSize({ width: 390, height: 844 });
  await page.waitForTimeout(300);
  expect(await p2.locator("#bank-scene").boundingBox()).toEqual(await page.locator(".bank3d").boundingBox());
  await ctx.close();
});

test("render on demand: no frames while nothing moves; the flow runs only while charging or discharging", async ({ page }) => {
  const gw = await stubGateway(page, { state: { ...STATE, "batteries.ble.current": 0.2 } });
  await page.goto("/?bench=1#energy");
  await in3D(page);
  expect((await bench(page))!.flow).toBe("");
  await page.waitForTimeout(500);
  const f0 = (await bench(page))!.frames;
  await page.waitForTimeout(1000);
  expect((await bench(page))!.frames).toBe(f0);                                // idle: nothing drawn
  gw.send({ type: "delta", data: { "batteries.ble.current": -12 } });
  await expect.poll(async () => (await bench(page))!.flow).toBe("dis");
  const f1 = (await bench(page))!.frames;
  await page.waitForTimeout(1000);
  expect((await bench(page))!.frames - f1).toBeGreaterThan(20);               // discharging: the dashes run
  const b = (await bench(page))!;
  expect(b.calls).toBeLessThan(80);                                           // a small scene
  expect(b.triangles).toBeLessThan(6000);
  gw.send({ type: "delta", data: { "batteries.ble.current": 0 } });
  await expect.poll(async () => (await bench(page))!.flow).toBe("");
  await page.waitForTimeout(200);
  const f2 = (await bench(page))!.frames;
  await page.waitForTimeout(1000);
  expect((await bench(page))!.frames).toBe(f2);
});

test("reduced motion: the flow shows but doesn't run", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await stubGateway(page);
  await page.goto("/?bench=1#energy");
  await in3D(page);
  expect((await bench(page))!.flow).toBe("chg");
  await page.waitForTimeout(400);
  const f0 = (await bench(page))!.frames;
  await page.waitForTimeout(800);
  expect((await bench(page))!.frames).toBe(f0);
});

test("hiding the page pauses the scene and keeps it; showing it draws at once, with no new context", async ({ page }) => {
  const warnings: string[] = [];
  page.on("console", m => { if (/too many active webgl contexts|context lost/i.test(m.text())) warnings.push(m.text()); });
  await stubGateway(page);
  await page.goto("/?bench=1#energy");
  await in3D(page);
  let b = (await bench(page))!;
  expect(b.live).toBe(1);
  const res = b.res;
  await page.click(".rail button[data-v=home]");
  await page.waitForTimeout(400);
  const f0 = (await bench(page))!.frames;
  await page.waitForTimeout(600);
  b = (await bench(page))!;
  expect(b.frames).toBe(f0);                                                  // paused: nothing drawn while hidden
  expect(b.live).toBe(1);                                                     // the context and the resources kept
  expect(b.res).toBe(res);
  await expect(page.locator(".bank3d canvas")).toHaveCount(1);
  await page.click(".rail button[data-v=energy]");
  await expect.poll(async () => (await bench(page))!.frames).toBeGreaterThan(f0);   // drawn again
  for (let i = 0; i < 20; i++) {                                              // 20 hide / show cycles: still one context
    await page.click(".rail button[data-v=home]");
    await page.click(".rail button[data-v=energy]");
    await expect(page.locator(".bank3d[data-ready] canvas")).toHaveCount(1);
  }
  b = (await bench(page))!;
  expect(b.live).toBe(1);
  expect(b.created).toBe(1);
  expect(b.lost).toBe(0);
  expect(warnings).toEqual([]);
});

test("?keepgl=0: hiding the page disposes the renderer and everything it holds; showing it builds a fresh one", async ({ page }) => {
  const warnings: string[] = [];
  page.on("console", m => { if (/too many active webgl contexts|context lost/i.test(m.text())) warnings.push(m.text()); });
  await stubGateway(page);
  await page.goto("/?bench=1&keepgl=0#energy");
  await in3D(page);
  let b = (await bench(page))!;
  expect(b.live).toBe(1);
  expect(b.res).toBeGreaterThan(0);
  await page.click(".rail button[data-v=home]");
  await expect.poll(async () => (await bench(page))!.live).toBe(0);
  b = (await bench(page))!;
  expect(b.res).toBe(0);                                                      // every geometry, material, texture freed
  expect(b.disposed).toBe(b.created);
  await expect(page.locator(".bank3d canvas")).toHaveCount(0);
  await page.click(".rail button[data-v=energy]");
  await in3D(page);
  const f = (await bench(page))!.frames;
  await expect.poll(async () => (await bench(page))!.frames).toBeGreaterThan(f + 10);   // a live context, drawing again
  expect((await bench(page))!.firstFrameMs).not.toBeNull();
  // 20 hide / show cycles: no context left behind
  for (let i = 0; i < 20; i++) {
    await page.click(".rail button[data-v=home]");
    await expect.poll(async () => (await bench(page))!.live).toBe(0);
    await page.click(".rail button[data-v=energy]");
    await expect(page.locator(".bank3d[data-ready] canvas")).toHaveCount(1);
  }
  b = (await bench(page))!;
  expect(b.live).toBe(1);
  expect(b.lost).toBe(0);
  expect(b.created - b.disposed).toBe(1);
  expect(warnings).toEqual([]);
});

test("a lost WebGL context falls back to the SVG scene", async ({ page }) => {
  await stubGateway(page);
  await page.goto("/?bench=1#energy");
  await in3D(page);
  await page.evaluate(() => {
    const c = document.querySelector<HTMLCanvasElement>(".bank3d canvas")!;
    (c.getContext("webgl2") as WebGL2RenderingContext).getExtension("WEBGL_lose_context")!.loseContext();
  });
  await expect(page.locator("#bank-scene .bg-stack")).toHaveCount(3);
  await expect(page.locator("#bg-pct0")).toHaveText("89.2%");
  const b = (await bench(page))!;
  expect(b.live).toBe(0);
  expect(b.res).toBe(0);
});

test("drag to orbit, then it eases back home", async ({ page }) => {
  await stubGateway(page, { state: { ...STATE, "batteries.ble.current": 0 } });
  await page.goto("/?bench=1#energy");
  await in3D(page);
  const home = (await bench(page))!.orbit;
  const box = (await page.locator(".bank3d canvas").boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 2 - 150, box.y + box.height / 2 + 40, { steps: 8 });
  await page.mouse.up();
  const turned = (await bench(page))!.orbit;
  expect(turned[0]).toBeGreaterThan(home[0]! + 0.5);                          // turned round the bank
  expect(turned[1]).toBeGreaterThan(home[1]!);
  await page.screenshot({ path: `${OUT}/react-energy-3d-orbit.png` });
  await expect.poll(async () => (await bench(page))!.orbit, { timeout: 8000 }).toEqual(home);
});

test("phone width: one column, no sideways scroll", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await stubGateway(page);
  await page.goto("/#energy");
  await in3D(page);
  const over = await page.evaluate(() => { const v = document.querySelector(".view.on")!; return v.scrollWidth - v.clientWidth; });
  expect(over).toBeLessThanOrEqual(0);
  await page.screenshot({ path: `${OUT}/react-energy-390x844.png`, fullPage: true });
});
