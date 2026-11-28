// Smoke test against a STUB gateway (no backend at all): pages render, nav works, and taps produce the right
// pressed state and `control` messages, with the optimistic flip and its roll-back.
import { expect, test, type Page, type WebSocketRoute } from "@playwright/test";
import { STATE } from "./fixture";

interface Sent { type: string; device?: string; cmd?: string; value?: unknown; id?: number }

/** Stub /ws: hello + snapshot; records every message; answers control with `reply(msg)` (default: ok). */
async function stubGateway(page: Page, opts: { control?: boolean; reply?: (m: Sent) => object | null } = {}) {
  const sent: Sent[] = [];
  let sock: WebSocketRoute | null = null;
  // nothing leaves this machine: map tiles, radar, forecasts and anything else off loopback are aborted (registered
  // first, so the specific stubs below take precedence)
  await page.route(u => !/^(https?|wss?):\/\/(127\.0\.0\.1|localhost)(:\d+)?\//.test(u.href), r => r.abort());
  await page.route("http://127.0.0.1:8091/**", r => r.abort());              // kiosk screen helper: never
  await page.route("**/api/**", r => r.fulfill({ json: {} }));
  await page.routeWebSocket(/\/ws$/, ws => {
    sock = ws;
    ws.onMessage(raw => {
      const m = JSON.parse(String(raw)) as Sent;
      sent.push(m);
      if (m.type !== "control") return;
      const r = opts.reply ? opts.reply(m) : { ok: true, result: "ok", detail: "done (verified)" };
      if (r) setTimeout(() => ws.send(JSON.stringify({ type: "control_result", id: m.id, ...r })), 150);
    });
    ws.send(JSON.stringify({ type: "hello", control: opts.control ?? true, who: "touchscreen" }));
    ws.send(JSON.stringify({ type: "snapshot", data: STATE }));
  });
  return { sent, send: (o: object) => sock?.send(JSON.stringify(o)) };
}

const controls = (sent: Sent[]) => sent.filter(m => m.type === "control");

test("every page renders and the rail switches pages", async ({ page }) => {
  await stubGateway(page);
  await page.goto("/#home");
  await expect(page.locator("#page-title")).toHaveText("Home");
  await expect(page.locator("#chip-link")).toContainText("Live");
  await expect(page.locator("#hub-soc")).toHaveText("72%");
  await expect(page.locator(".quick-btn")).toHaveCount(4);
  await expect(page.locator("#chip-ro")).toHaveCount(0);                    // a screen that can control: no control chip
  await expect(page.locator(".quick-card .demo-tag")).toHaveCount(0);       // switches live: no "live" pill (owner, 2026-11-27)
  for (const [v, title] of [["energy", "Energy"], ["climate", "Climate"], ["weather", "Weather"], ["switches", "Switches"],
    ["network", "Network"], ["alerts", "Alerts"], ["systems", "Systems"], ["settings", "Settings"], ["home", "Home"]] as const) {
    await page.click(`.rail button[data-v=${v}]`);
    await expect(page.locator("#page-title")).toHaveText(title);
    await expect(page.locator(`.view.on[data-view=${v}]`)).toBeVisible();
    expect(new URL(page.url()).hash).toBe(`#${v}`);
  }
  await page.click(".rail button[data-v=switches]");
  await expect(page.locator(".sw-box")).toHaveCount(3);                    // Lights, Water, Vehicle inputs
  await page.click(".rail button[data-v=alerts]");
  await expect(page.locator(".alert-card")).toHaveCount(1);
  await expect(page.locator("#nav-badge")).toHaveText("1");
});

test("a tap on a Home switch: pressed at once, sends one control, flips optimistically", async ({ page }) => {
  const gw = await stubGateway(page);
  await page.goto("/#home");
  const btn = page.locator('.quick-btn[data-sw="7.1"]');
  await expect(btn).toBeVisible();
  await expect(btn).not.toHaveClass(/\bon\b/);
  const box = (await btn.boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await expect(btn).toHaveClass(/\bpressed\b/);                            // pressed before release
  await page.mouse.up();
  await expect(btn).toHaveClass(/\bon\b/);                                 // optimistic, before the result
  await expect.poll(() => controls(gw.sent)).toEqual([{ type: "control", device: "n2k", cmd: "set", value: { channel: 1, on: true }, id: 1 }]);
  await expect(page.locator("#toast")).toContainText("Alpha light on: done (verified)");
});

test("a refused command rolls the tile back and says why", async ({ page }) => {
  const gw = await stubGateway(page, { reply: () => ({ ok: false, result: "refused", detail: "tx gate closed" }) });
  await page.goto("/#switches");
  const tile = page.locator('.sw[data-sw="7.2"]');                         // Beta light is on
  await expect(tile).toHaveClass(/\bon\b/);
  await tile.click();
  await expect(page.locator("#toast")).toContainText("Beta light off failed: tx gate closed");
  await expect(tile).toHaveClass(/\bon\b/);                                // back to what the device reports
  expect(controls(gw.sent)).toHaveLength(1);
});

test("a dimmer is held: press sends hold on, release sends hold off", async ({ page }) => {
  const gw = await stubGateway(page);
  await page.goto("/#home");
  const btn = page.locator('.quick-btn[data-sw="7.3"]');
  const box = (await btn.boundingBox())!;
  await page.mouse.move(box.x + 20, box.y + 20);
  await page.mouse.down();
  await expect.poll(() => controls(gw.sent).map(m => m.value)).toEqual([{ channel: 3, on: true }]);
  await page.mouse.up();
  await expect.poll(() => controls(gw.sent).map(m => [m.cmd, m.value])).toEqual([["hold", { channel: 3, on: true }], ["hold", { channel: 3, on: false }]]);
});

test("view only: buttons explain and nothing is sent", async ({ page }) => {
  const gw = await stubGateway(page, { control: false });
  await page.goto("/#home");
  await expect(page.locator("#chip-ro")).toContainText("Read-only");
  await expect(page.locator(".quick-card .demo-tag")).toHaveText("view only");
  await expect(page.locator('.hq-btn[data-hq="furnace"]')).toBeDisabled();
  await expect(page.locator("#hsp-up")).toBeDisabled();
  await page.locator('.quick-btn[data-sw="7.1"]').click();
  await expect(page.locator("#toast")).toContainText("Switch control is off (view only)");
  expect(controls(gw.sent)).toHaveLength(0);
  expect(new URL(page.url()).hash).toBe("#home");                           // the disabled ± did not navigate
});

test("heat start asks first (Espar), and the confirm sends", async ({ page }) => {
  const gw = await stubGateway(page);
  await page.goto("/#home");
  await page.locator('.hq-btn[data-hq="espar"]').click();
  await expect(page.locator("#modal")).toBeVisible();
  await expect(page.locator("#modal-t")).toContainText("Start the Espar heating");
  expect(controls(gw.sent)).toHaveLength(0);
  await page.click("#modal-yes");
  await expect.poll(() => controls(gw.sent).map(m => [m.device, m.cmd, (m.value as { mode: string }).mode])).toEqual([["espar", "set", "heat"]]);
});

test("energy flow: the edges' dots run on the canvas while an edge carries power, and stop when none does", async ({ page }) => {
  const s = await stubGateway(page);
  await page.goto("/#home");
  await expect(page.locator("#hub-soc")).toHaveText("72%");
  await expect(page.locator("#e-solar")).toHaveClass(/\bon\b/);                 // the edges' state stays on the paths
  await expect(page.locator("#e-dc")).toHaveClass(/\bon\b/);
  await expect(page.locator("#e-shore")).not.toHaveClass(/\bon\b/);
  const snap = () => page.locator("#flow .f-canvas").evaluate(c => {
    const cv = c as HTMLCanvasElement, d = cv.getContext("2d")!.getImageData(0, 0, cv.width, cv.height).data;
    let h = 0;
    for (let i = 0; i < d.length; i += 4) h = (h * 31 + d[i]! + d[i + 1]! * 3 + d[i + 2]! * 7 + d[i + 3]! * 11) >>> 0;
    return h;
  });
  const a = await snap();
  await page.waitForTimeout(250);
  expect(await snap()).not.toBe(a);                                             // dots moving
  // back to Home: the running edges are drawn at once, not faded in again (the CSS version showed them at once)
  const lit = () => page.locator("#flow .f-canvas").evaluate(c => {
    const cv = c as HTMLCanvasElement, d = cv.getContext("2d")!.getImageData(0, 0, cv.width, cv.height).data;
    let n = 0;
    for (let i = 0; i < d.length; i += 4) if (d[i]! > 150 && d[i + 2]! < 80) n++;   // the solar edge's amber dots
    return n;
  });
  await page.waitForTimeout(700);                                               // the first snapshot's edges have faded in
  const full = await lit();
  expect(full).toBeGreaterThan(20);
  await page.click(".rail button[data-v=energy]");
  await page.click(".rail button[data-v=home]");
  await page.evaluate(() => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))));
  expect(await lit()).toBeGreaterThan(full * 0.8);
  s.send({ type: "delta", data: { "victron.pv.power": 0, "energy.dc.load": 0 } });
  await expect(page.locator("#e-solar")).not.toHaveClass(/\bon\b/);
  await page.waitForTimeout(800);                                               // the 0.5 s fade-out
  const b = await snap();
  await page.waitForTimeout(300);
  expect(await snap()).toBe(b);                                                 // nothing runs: nothing redrawn
});

test("Switches page: no banner while switching is live; it explains when control is off", async ({ page }) => {
  await stubGateway(page);
  await page.goto("/#switches");
  await expect(page.locator(".sw-tile, [data-sw]").first()).toBeVisible();
  await expect(page.locator("#sw-note")).toHaveCount(0);
  await page.screenshot({ path: "../harness/out/ui-pages/react-switches-live.png" });
  await page.goto("/#home");
  await page.locator(".quick-card").screenshot({ path: "../harness/out/ui-pages/react-home-switches-card.png" });
  const ro = await page.context().newPage();
  await stubGateway(ro, { control: false });
  await ro.goto("/#switches");
  await expect(ro.locator("#sw-note")).toContainText("Switch control is off");
});
