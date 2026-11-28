// Climate page against a STUB gateway (page.routeWebSocket / page.route; nothing reaches a backend or a device):
// confirmations before every heat change, the Espar range refusal, the low-battery lockout locks, view-only, the
// dial drag and the shared setpoint rules for the two heaters, the fan slider. All values are invented.
import { expect, test, type Page, type WebSocketRoute } from "@playwright/test";

interface Sent { type: string; device?: string; cmd?: string; value?: unknown; id?: number }

/** Climate state (invented). °F display (the default unit): 20 °C = 68°. */
const BASE: Record<string, unknown> = {
  "victron.controls_ok": true,
  "climate.cabin.temp": 18.5, "climate.cabin.humidity": 52, "climate.cabin.setpoint": 20, "climate.cabin.source": "average",
  "climate.cabin.front_temp": 18.0, "climate.cabin.rear_temp": 19.0, "climate.outside.temp": 2.5,
  "climate.fridge.temp": 3.0, "climate.freezer.temp": -16, "ui.fridge_max_c": 5, "ui.freezer_max_c": -10,
  "climate.rixens.live": true, "climate.rixens.state": "idle", "climate.rixens.controls_ok": true,
  "climate.rixens.sources": ["furnace"], "climate.rixens.furnace": "ready", "climate.rixens.electric": "off", "climate.rixens.engine": "off",
  "climate.rixens.available": { electric: true, engine: true, preheat: true, floor: false },
  "climate.rixens.thermostat": false, "climate.rixens.hot_water": true, "climate.rixens.preheat": false,
  "climate.rixens.outlet_c": 40, "climate.rixens.inlet_c": 35, "climate.rixens.flame_c": 60, "climate.rixens.fan_rpm": 2400,
  "climate.rixens.glow_w": 0, "climate.rixens.fuel_gph": 0.125, "climate.rixens.fan_speed": "auto", "climate.rixens.fan_auto_pct": 35,
  "climate.rixens.fan_running": true, "climate.rixens.glycol": "cold", "climate.rixens.system_heat": true,
  "climate.espar.state": "off", "climate.espar.asleep": false, "climate.espar.diag.enabled": true,
  "climate.espar.ctl.active": false, "climate.espar.ctl.mode": "off", "climate.espar.ctl.setpoint_c": 20, "espar.controls_ok": true,
  "power.heat_lockout": false, "power.shutdown": { soc: 64 },
};
const LOCKOUT = { "power.heat_lockout": true, "power.shutdown": { soc: 9 } };
const ESPAR_HEATING = { "climate.espar.state": "heating", "climate.espar.ctl.active": true, "climate.espar.ctl.mode": "heat",
  "climate.espar.ctl.setpoint_c": 20 };

async function stubGateway(page: Page, opts: { control?: boolean; state?: Record<string, unknown>; reply?: (m: Sent) => object } = {}) {
  const sent: Sent[] = [];
  let sock: WebSocketRoute | null = null;
  await page.route("http://127.0.0.1:8091/**", r => r.abort());              // kiosk screen helper: never
  await page.route("**/api/**", r => r.fulfill({ json: {} }));
  await page.routeWebSocket(/\/ws$/, ws => {
    sock = ws;
    ws.onMessage(raw => {
      const m = JSON.parse(String(raw)) as Sent;
      sent.push(m);
      const r = opts.reply ? opts.reply(m) : { ok: true, result: "ok", detail: "done (verified)" };
      if (m.type === "control") setTimeout(() => ws.send(JSON.stringify({ type: "control_result", id: m.id, ...r })), 150);
    });
    ws.send(JSON.stringify({ type: "hello", control: opts.control ?? true, who: "touchscreen" }));
    ws.send(JSON.stringify({ type: "snapshot", data: { ...BASE, ...opts.state } }));
  });
  return { sent, delta: (data: object) => sock?.send(JSON.stringify({ type: "delta", data })) };
}
const controls = (sent: Sent[]) => sent.filter(m => m.type === "control");
const brief = (sent: Sent[]) => controls(sent).map(m => [m.device, m.cmd, m.value]);

test.use({ reducedMotion: "reduce" });                    // springs snap: the knob sits exactly where the value is

async function open(page: Page) {
  await page.goto("/#climate");
  await expect(page.locator("#sp-val")).toHaveText("68°");
}

test("renders the dial, both heaters, refrigeration and the fan", async ({ page }) => {
  await stubGateway(page);
  await open(page);
  await expect(page.locator("#dial-temp")).toHaveText("65°");
  await expect(page.locator("#dial-label")).toContainText("cabin · 52% RH");
  await expect(page.locator("#dial-label .dial-split")).toHaveText("average of front 64° · back 66°");
  await expect(page.locator("#sp-note")).toHaveText("setpoint · drag the ring");
  await expect(page.locator("#dial")).toHaveClass(/draggable/);
  await expect(page.locator(".heater")).toHaveCount(2);
  await expect(page.locator(".heater .fluid-pill")).toHaveText("104°");
  await expect(page.locator(".heater").first().locator(".burner > div")).toHaveCount(6);
  await expect(page.locator(".heater").first().locator(".burner")).toContainText("2,400 rpm");
  await expect(page.locator(".heater").nth(1).locator(".burner > div")).toHaveCount(8);
  await expect(page.locator(".src-btn")).toHaveCount(3);
  await expect(page.locator('.src-btn[data-src="furnace"]')).toHaveClass(/\bsel\b/);
  await expect(page.locator(".cold-item")).toHaveCount(2);
  await expect(page.locator(".cold-item").first()).toContainText("Fridge · below 41°");
  await expect(page.locator("#fan-val")).toHaveText("35%");
  await expect(page.locator("#fan-note")).toHaveText("fan · auto");
  await expect(page.locator("#fan-auto")).toHaveClass(/\bon\b/);
  await expect(page.locator(".lockout-line")).toHaveCount(0);
});

test("MCS7 sources: confirm before on AND off, nothing sent until Confirm, Cancel sends nothing", async ({ page }) => {
  const gw = await stubGateway(page);
  await open(page);
  await page.locator('.src-btn[data-src="electric"]').click();
  await expect(page.locator("#modal-t")).toHaveText("Turn on electric?");
  await expect(page.locator("#modal-d")).toContainText("1500 W electric element");
  await page.waitForTimeout(300);
  expect(controls(gw.sent)).toHaveLength(0);
  await page.click("#modal-no");
  await expect(page.locator("#modal")).toBeHidden();
  await page.waitForTimeout(300);
  expect(controls(gw.sent)).toHaveLength(0);

  await page.locator('.src-btn[data-src="furnace"]').click();               // on → asks before turning OFF too
  await expect(page.locator("#modal-t")).toHaveText("Turn off furnace?");
  expect(controls(gw.sent)).toHaveLength(0);
  await page.click("#modal-yes");
  await expect.poll(() => brief(gw.sent)).toEqual([["rixens", "furnace", false]]);
  await expect(page.locator("#toast")).toContainText("Furnace off: done (verified)");
});

test("a refused heat command: lit at once after Confirm, rolls back to the device's state with the reason", async ({ page }) => {
  const gw = await stubGateway(page, { reply: () => ({ ok: false, result: "refused", detail: "Heat is locked out: test" }) });
  await open(page);
  const b = page.locator('.src-btn[data-src="electric"]');
  await b.click();
  await page.click("#modal-yes");
  await expect(b).toHaveClass(/\bsel\b/);                                   // optimistic, while it's being verified
  await expect(page.locator("#toast")).toContainText("Electric on failed: Heat is locked out: test");
  await expect(b).not.toHaveClass(/\bsel\b/);                               // back to what the MCS7 reports
  expect(brief(gw.sent)).toEqual([["rixens", "electric", true]]);
  const hw = page.locator('[data-mcs="hot_water"]');                         // on → off, refused → on again
  await hw.click();
  await page.click("#modal-yes");
  await expect(page.locator("#toast")).toContainText("Hot water off failed");
  await expect(hw).toHaveClass(/\bon\b/);
});

test("MCS7 destination badges: confirm, then one command", async ({ page }) => {
  const gw = await stubGateway(page);
  await open(page);
  await page.locator('[data-mcs="preheat"]').click();
  await expect(page.locator("#modal-t")).toHaveText("Turn engine pre-heat on?");
  await expect(page.locator("#modal-d")).toHaveText("The MCS7 sends heat to the engine.");
  expect(controls(gw.sent)).toHaveLength(0);
  await page.click("#modal-yes");
  await expect.poll(() => brief(gw.sent)).toEqual([["rixens", "preheat", true]]);
});

test("heat lockout: banners, the right buttons locked, turning off still works, engine exempt", async ({ page }) => {
  const gw = await stubGateway(page, { state: LOCKOUT });
  await open(page);
  await expect(page.locator(".lockout-line")).toHaveCount(2);
  await expect(page.locator(".lockout-line").first()).toContainText("Heat locked out");
  await expect(page.locator(".lockout-line").first()).toContainText("House battery 9 % · charge to unlock");
  // sources: an off source that would START heat is locked; engine heat is exempt; an ON source can be turned off
  await expect(page.locator('.src-btn[data-src="electric"]')).toBeDisabled();
  await expect(page.locator('.src-btn[data-src="electric"]')).toHaveClass(/\blocked\b/);
  await expect(page.locator('.src-btn[data-src="engine"]')).toBeEnabled();
  await expect(page.locator('.src-btn[data-src="furnace"]')).toBeEnabled();
  // destinations: hot water (on) can be turned off, pre-heat (off) is locked, cabin heat (the thermostat) is not
  await expect(page.locator('[data-mcs="hot_water"]')).toBeEnabled();
  await expect(page.locator('[data-mcs="preheat"]')).toBeDisabled();
  await expect(page.locator('[data-mcs="preheat"]')).toHaveClass(/\blocked\b/);
  await expect(page.locator('[data-mcs="thermostat"]')).toBeEnabled();
  // Espar: heat locked, fan only and the run time are not
  await expect(page.locator('[data-esp="heat"]')).toBeDisabled();
  await expect(page.locator('[data-esp="heat"]')).toHaveClass(/\blocked\b/);
  await expect(page.locator('[data-esp="fan"]')).toBeEnabled();
  await expect(page.locator('[data-esp="run"]')).toBeEnabled();
  // a locked button does nothing even when forced
  await page.locator('.src-btn[data-src="electric"]').click({ force: true });
  await page.locator('[data-esp="heat"]').click({ force: true });
  await page.locator('[data-mcs="preheat"]').click({ force: true });
  await expect(page.locator("#modal")).toBeHidden();
  // turning off still works (after its confirm)
  await page.locator('.src-btn[data-src="furnace"]').click();
  await expect(page.locator("#modal-t")).toHaveText("Turn off furnace?");
  await page.click("#modal-yes");
  await expect.poll(() => brief(gw.sent)).toEqual([["rixens", "furnace", false]]);
});

test("heat lockout while the Espar heats: its heat badge stays usable and turns it off, no confirm", async ({ page }) => {
  const gw = await stubGateway(page, { state: { ...LOCKOUT, ...ESPAR_HEATING } });
  await open(page);
  const b = page.locator('[data-esp="off"]').first();
  await expect(b).toBeEnabled();
  await expect(b).toHaveClass(/\bon\b/);
  await b.click();
  await expect.poll(() => controls(gw.sent).map(m => [m.device, m.cmd, (m.value as { mode: string }).mode])).toEqual([["espar", "set", "off"]]);
  await expect(page.locator("#modal")).toBeHidden();
});

test("Espar heat asks first and sends the shared setpoint; fan only asks first", async ({ page }) => {
  const gw = await stubGateway(page);
  await open(page);
  await page.locator('[data-esp="heat"]').click();
  await expect(page.locator("#modal-t")).toHaveText("Start the Espar heating to 68°?");
  await expect(page.locator("#modal-d")).toContainText("Run time: until you turn it off.");
  await page.waitForTimeout(300);
  expect(controls(gw.sent)).toHaveLength(0);
  await page.click("#modal-yes");
  await expect.poll(() => brief(gw.sent)).toEqual([["espar", "set", { mode: "heat", setpoint_c: 20, run_min: null }]]);
  await expect(page.locator("#toast")).toContainText("Espar heat 68°");

  await page.waitForTimeout(400);                                          // the first result has come back
  await page.locator('[data-esp="fan"]').click();
  await expect(page.locator("#modal-t")).toHaveText("Run the Espar fan only?");
  expect(controls(gw.sent)).toHaveLength(1);
  await page.click("#modal-yes");
  await expect.poll(() => controls(gw.sent).length).toBe(2);
  expect(controls(gw.sent)[1]).toMatchObject({ device: "espar", cmd: "set", value: { mode: "fan" } });
});

test("Espar heat outside its 10–30 °C range is refused with a toast, no confirm, nothing sent", async ({ page }) => {
  const gw = await stubGateway(page, { state: { "climate.cabin.setpoint": 7 } });   // 45 °F
  await page.goto("/#climate");
  await expect(page.locator("#sp-val")).toHaveText("45°");
  await page.locator('[data-esp="heat"]').click();
  await expect(page.locator("#toast")).toContainText("Espar not started: 45° is outside the Espar's 50°–86° range. Raise the setpoint first.");
  await expect(page.locator("#modal")).toBeHidden();
  await page.waitForTimeout(300);
  expect(controls(gw.sent)).toHaveLength(0);
});

test("the run-time badge cycles without asking and without sending while the Espar is off", async ({ page }) => {
  const gw = await stubGateway(page);
  await open(page);
  const run = page.locator('[data-esp="run"]');
  await expect(run).toContainText("∞");
  await run.click();
  await expect(run).toContainText("15 min");
  await page.waitForTimeout(1800);
  expect(controls(gw.sent)).toHaveLength(0);
});

// ── Dial drag ──
/** Screen point of a value (°F) on the knob's circle. */
async function dialPoint(page: Page, f: number) {
  const box = (await page.locator("#dial").boundingBox())!;
  const k = box.width / 440, a = (135 + 270 * (f - 40) / 50) * Math.PI / 180;
  return { x: box.x + box.width / 2 + 194 * k * Math.cos(a), y: box.y + box.height / 2 + 194 * k * Math.sin(a) };
}
async function drag(page: Page, from: number, to: number) {
  const p0 = await dialPoint(page, from), p1 = await dialPoint(page, to);
  await page.mouse.move(p0.x, p0.y);
  await page.mouse.down();
  await page.mouse.move((p0.x + p1.x) / 2, (p0.y + p1.y) / 2 - 40, { steps: 6 });
  await page.mouse.move(p1.x, p1.y, { steps: 6 });
}

test("dial: drag the knob, the readout follows, ONE setpoint is sent on release", async ({ page }) => {
  const gw = await stubGateway(page);
  await open(page);
  await drag(page, 68, 72);
  await expect(page.locator("#sp-val")).toHaveText("72°");
  await expect(page.locator("#sp-note")).toHaveText("setpoint · release to set");
  await expect(page.locator(".dial-knob")).toHaveClass(/active/);
  expect(controls(gw.sent)).toHaveLength(0);                                  // nothing while dragging
  await page.mouse.up();
  await expect.poll(() => brief(gw.sent)).toEqual([["rixens", "setpoint", 22.22]]);
  await expect(page.locator("#toast")).toContainText("Setpoint 72°");
});

test("dial: only the knob can be grabbed, and a drag back to the same value sends nothing", async ({ page }) => {
  const gw = await stubGateway(page);
  await open(page);
  const off = await dialPoint(page, 80);                                     // on the ring, far from the knob
  await page.mouse.move(off.x, off.y);
  await page.mouse.down();
  await page.mouse.move(off.x + 30, off.y + 30, { steps: 4 });
  await page.mouse.up();
  await expect(page.locator("#sp-val")).toHaveText("68°");
  await drag(page, 68, 75);
  await expect(page.locator("#sp-val")).toHaveText("75°");
  const back = await dialPoint(page, 68);
  await page.mouse.move(back.x, back.y, { steps: 6 });
  await page.mouse.up();
  await expect(page.locator("#sp-val")).toHaveText("68°");
  await expect(page.locator("#sp-note")).toHaveText("setpoint · drag the ring");
  await page.waitForTimeout(400);
  expect(controls(gw.sent)).toHaveLength(0);
});

test("dial: a cancelled drag (pointercancel) drops the value and sends nothing", async ({ page }) => {
  const gw = await stubGateway(page);
  await open(page);
  await drag(page, 68, 74);
  await expect(page.locator("#sp-val")).toHaveText("74°");
  await page.locator("#dial").dispatchEvent("pointercancel", { pointerId: 1 });
  await expect(page.locator("#sp-val")).toHaveText("68°");
  await page.mouse.up();
  await page.waitForTimeout(400);
  expect(controls(gw.sent)).toHaveLength(0);
});

test("dial: a page hidden and shown again draws the current setpoint (knob position)", async ({ page }) => {
  const gw = await stubGateway(page);
  await open(page);
  await page.click(".rail button[data-v=home]");
  gw.delta({ "climate.cabin.setpoint": 25 });                                // 77 °F while Climate is hidden
  await page.click(".rail button[data-v=climate]");
  await expect(page.locator("#sp-val")).toHaveText("77°");
  const expected = await page.evaluate(() => {
    const a = (135 + 270 * (77 - 40) / 50) * Math.PI / 180;
    return [220 + 194 * Math.cos(a), 220 + 194 * Math.sin(a)];
  });
  const knob = page.locator(".dial-knob");
  await expect.poll(async () => Math.abs(+(await knob.getAttribute("cx"))! - expected[0]!) + Math.abs(+(await knob.getAttribute("cy"))! - expected[1]!)).toBeLessThan(0.5);
});

test("dial with the Espar heating: below its range → Espar OFF + MCS7 setpoint", async ({ page }) => {
  const gw = await stubGateway(page, { state: ESPAR_HEATING });
  await open(page);
  await drag(page, 68, 40);
  await page.mouse.up();
  await expect.poll(() => controls(gw.sent).length).toBe(2);
  expect(brief(gw.sent)).toEqual([["espar", "set", { mode: "off", setpoint_c: 4.4, run_min: null }], ["rixens", "setpoint", 4.44]]);
});

test("dial with the Espar heating: inside its range → both heaters get the new setpoint", async ({ page }) => {
  const gw = await stubGateway(page, { state: ESPAR_HEATING });
  await open(page);
  await drag(page, 68, 72);
  await page.mouse.up();
  await expect.poll(() => controls(gw.sent).length).toBe(2);
  expect(brief(gw.sent)).toEqual([["espar", "set", { mode: "heat", setpoint_c: 22.2, run_min: null }], ["rixens", "setpoint", 22.22]]);
});

test("dial with the Espar heating: above its range → MCS7 only (the Espar keeps its setpoint)", async ({ page }) => {
  const gw = await stubGateway(page, { state: ESPAR_HEATING });
  await open(page);
  await drag(page, 68, 90);
  await page.mouse.up();
  await expect.poll(() => controls(gw.sent).length).toBe(1);
  await page.waitForTimeout(400);
  expect(brief(gw.sent)).toEqual([["rixens", "setpoint", 32.22]]);
});

test("dial with the MCS7 silent and the Espar heating: the knob shows the Espar's setpoint and drives the Espar alone", async ({ page }) => {
  const gw = await stubGateway(page, { state: { ...ESPAR_HEATING, "climate.cabin.setpoint": null, "climate.rixens.live": false,
    "climate.rixens.controls_ok": false, "climate.espar.ctl.setpoint_c": 21 } });
  await page.goto("/#climate");
  await expect(page.locator("#sp-val")).toHaveText("70°");
  await expect(page.locator(".dial-knob")).toBeVisible();
  await drag(page, 70, 74);
  await page.mouse.up();
  await expect.poll(() => brief(gw.sent)).toEqual([["espar", "set", { mode: "heat", setpoint_c: 23.3, run_min: null }]]);
});

// ── Fan ──
test("fan slider: drag sends ONE fan speed on release; Auto sends auto", async ({ page }) => {
  const gw = await stubGateway(page);
  await open(page);
  const tr = (await page.locator("#fan-slider .vs-track").boundingBox())!;
  const yOf = (p: number) => tr.y + tr.height * (1 - p / 100);
  await page.mouse.move(tr.x + tr.width / 2, yOf(35));
  await page.mouse.down();
  await page.mouse.move(tr.x + tr.width / 2, yOf(60), { steps: 6 });
  await expect(page.locator("#fan-val")).toHaveText("60%");
  await expect(page.locator("#fan-note")).toHaveText("fan · release to set");
  expect(controls(gw.sent)).toHaveLength(0);
  await page.mouse.up();
  await expect.poll(() => brief(gw.sent)).toEqual([["rixens", "fan", 60]]);
  await page.waitForTimeout(400);
  await page.click("#fan-auto");
  await expect.poll(() => brief(gw.sent)).toEqual([["rixens", "fan", 60], ["rixens", "fan", "auto"]]);
});

// ── View only ──
test("view only: every control is disabled and nothing is sent", async ({ page }) => {
  const gw = await stubGateway(page, { control: false });
  await open(page);
  await expect(page.locator("#sp-note")).toHaveText("setpoint · view only");
  await expect(page.locator("#dial")).not.toHaveClass(/draggable/);
  for (const sel of ['.src-btn[data-src="furnace"]', '.src-btn[data-src="electric"]', '[data-mcs="thermostat"]', '[data-mcs="hot_water"]',
    '[data-esp="heat"]', '[data-esp="fan"]', '[data-esp="run"]', "#fan-auto"]) await expect(page.locator(sel)).toBeDisabled();
  await expect(page.locator("#fan-slider")).toHaveClass(/disabled/);
  await drag(page, 68, 75);
  await page.mouse.up();
  const tr = (await page.locator("#fan-slider .vs-track").boundingBox())!;
  await page.mouse.move(tr.x + tr.width / 2, tr.y + 10);
  await page.mouse.down();
  await page.mouse.move(tr.x + tr.width / 2, tr.y + tr.height - 10, { steps: 4 });
  await page.mouse.up();
  for (const sel of ['.src-btn[data-src="electric"]', '[data-esp="heat"]', '[data-mcs="preheat"]']) await page.locator(sel).click({ force: true });
  await expect(page.locator("#modal")).toBeHidden();
  await expect(page.locator("#sp-val")).toHaveText("68°");
  await page.waitForTimeout(400);
  expect(controls(gw.sent)).toHaveLength(0);
});

test("live values update in place without a reload", async ({ page }) => {
  const gw = await stubGateway(page);
  await open(page);
  gw.delta({ "climate.cabin.temp": 21, "climate.rixens.fan_rpm": 3100, "climate.cabin.setpoint": 21.5 });
  await expect(page.locator("#dial-temp")).toHaveText("70°");
  await expect(page.locator("#sp-val")).toHaveText("71°");
  await expect(page.locator(".heater").first().locator(".burner")).toContainText("3,100 rpm");
  gw.delta({ "power.heat_lockout": true });
  await expect(page.locator(".lockout-line")).toHaveCount(2);
  await expect(page.locator('.src-btn[data-src="electric"]')).toBeDisabled();
});

// The kiosk runs with motion on: the springs and tweens, not the reduced-motion snaps the tests above use.
test.describe("with motion (springs and tweens running)", () => {
  test.use({ reducedMotion: "no-preference" });
  test("drag and send", async ({ page }) => {
    const gw = await stubGateway(page);
    await open(page);
    await page.waitForTimeout(600);                                            // let the knob settle
    await drag(page, 68, 72);
    await expect(page.locator("#sp-val")).toHaveText("72°");
    await page.mouse.up();
    await expect.poll(() => brief(gw.sent)).toEqual([["rixens", "setpoint", 22.22]]);
  });
  test("the knob springs to a new setpoint and settles exactly on it", async ({ page }) => {
    const gw = await stubGateway(page);
    await open(page);
    await page.click(".rail button[data-v=home]");
    gw.delta({ "climate.cabin.setpoint": 25 });
    await page.click(".rail button[data-v=climate]");
    gw.delta({ "climate.cabin.setpoint": 22 });                              // 72 °F: animates from 77
    await expect(page.locator("#sp-val")).toHaveText("72°");
    const f = 22 * 9 / 5 + 32;                                                 // 71.6: the knob sits at the unrounded value
    const a = (135 + 270 * (f - 40) / 50) * Math.PI / 180, ex = 220 + 194 * Math.cos(a), ey = 220 + 194 * Math.sin(a);
    const knob = page.locator(".dial-knob");
    await expect.poll(async () => Math.abs(+(await knob.getAttribute("cx"))! - ex) + Math.abs(+(await knob.getAttribute("cy"))! - ey)).toBeLessThan(0.05);
  });
});

test("cabin average with the rear Ruuvi: front · rear · MCS7, and only the sensors in use", async ({ page }) => {
  const s = await stubGateway(page, { state: { "climate.cabin.rear_ruuvi_temp": 20.0 } });
  await open(page);
  await expect(page.locator("#dial-label .dial-split")).toHaveText("average of front 64° · rear 68° · MCS7 66°");
  s.delta({ "climate.cabin.rear_temp": null });                                   // the MCS7 left out (stale)
  await expect(page.locator("#dial-label .dial-split")).toHaveText("average of front 64° · rear 68°");
  s.delta({ "climate.cabin.rear_ruuvi_temp": null, "climate.cabin.rear_temp": 19.0 }); // the rear Ruuvi left out
  await expect(page.locator("#dial-label .dial-split")).toHaveText("average of front 64° · back 66°");
});
