// Systems page against a STUB gateway (page.routeWebSocket; no backend). Invented data only. The page has no device
// controls; the checks are its contents, skeletons, the inspector tabs, the Rust gateway's module health and
// per-key updates.
import { expect, test, type Page, type WebSocketRoute } from "@playwright/test";
import { STATE as BASE } from "./fixture";

const DEVS = [
  { source: 3, model: "Test Switcher", manufacturer: "Example Co", class: "Electrical Distribution", instance: 0, software: "9.9.1",
    serial: "000111", online: true, age: 1.5, heartbeat: true },
  { source: 17, model: "Test Shunt", manufacturer: "Example Co", class: "Electrical Generation", instance: 0, software: "2.0",
    serial: "000222", online: true, age: 42, heartbeat: true },
  { source: 99, model: "", manufacturer: "", class: "", instance: 0, software: "", serial: "", online: false, age: 900, heartbeat: false },
];
const PGNS = [
  { pgn: 11111, name: "Test PGN one", count: 10, sources: [3, 17], age: 0.4 },
  { pgn: 22222, name: "Test PGN two", count: 20, sources: [17], age: 2.5 },
];
const STATE: Record<string, unknown> = {
  ...BASE,
  "victron.portal": "testportal01",
  "bus.alpha.channel": "vcan7", "bus.alpha.connected": true, "bus.alpha.rate": 12.5, "bus.alpha.frames": 1000, "bus.alpha.ids": 9, "bus.alpha.errors": 0,
  "bus.beta.channel": "vcan8", "bus.beta.connected": true, "bus.beta.rate": 0, "bus.beta.frames": 0, "bus.beta.ids": 0, "bus.beta.errors": 0,
  "bus.gamma.channel": "vcan9", "bus.gamma.connected": false, "bus.gamma.rate": 0, "bus.gamma.frames": 5, "bus.gamma.ids": 1, "bus.gamma.errors": 2,
  "netmon.targets": [
    { name: "Router T", host: "10.9.9.1", tcp: 80, up: true, rtt_ms: 1.5, tcp_ok: true, reason: null, severity: "warning", hint: "", down_for: null },
    { name: "Box T", host: "10.9.9.2", tcp: 22, up: false, rtt_ms: null, tcp_ok: false, reason: "no reply", severity: "warning", hint: "check its power", down_for: 300 },
  ],
  "climate.espar.heard": true, "climate.espar.asleep": false, "climate.espar.state": "heating", "climate.espar.controller": true,
  "climate.espar.fault": null, "espar.ids": [{ id: "0x1", count: 3 }],
  "tx.enabled": { n2k: true, espar: false, victron: true }, "tx.blocked": { espar: 2, n2k: 1 },
  "n2k.devices": DEVS, "n2k.pgns": PGNS, "n2k.commands": [],
};
const SVC = { "svc.victron.up": true, "svc.victron.age_s": 0.4, "svc.n2k.up": true, "svc.n2k.age_s": 1.1, "svc.espar.up": true, "svc.espar.age_s": 0.9 };

async function stubGateway(page: Page, opts: { state?: Record<string, unknown>; hold?: boolean } = {}) {
  const sent: { type: string }[] = [];
  let sock: WebSocketRoute | null = null;
  await page.route("http://127.0.0.1:8091/**", r => r.abort());              // kiosk screen helper: never
  await page.route("**/api/**", r => r.fulfill({ json: {} }));
  const snapshot = () => sock?.send(JSON.stringify({ type: "snapshot", data: opts.state ?? STATE }));
  await page.routeWebSocket(/\/ws$/, ws => {
    sock = ws;
    ws.onMessage(raw => { sent.push(JSON.parse(String(raw)) as { type: string }); });
    ws.send(JSON.stringify({ type: "hello", control: true, who: "touchscreen" }));
    if (!opts.hold) snapshot();
  });
  return { sent, send: (o: object) => sock?.send(JSON.stringify(o)), release: snapshot };
}
const OUT = "../harness/out/ui-pages";
const card = (page: Page, id: string) => page.locator(`.sys[data-sys="${id}"]`);

test("status cards, devices and the inspector show the state", async ({ page }) => {
  const gw = await stubGateway(page);
  await page.goto("/#systems");
  await expect(card(page, "cerbo")).toContainText("connected · testportal01");
  await expect(card(page, "cerbo").locator(".st")).toHaveClass(/\bgood\b/);
  await expect(page.locator("#sys-cards .sys")).toHaveCount(1 + 3 + 2 + 1 + 1);   // Cerbo, 3 buses, 2 targets, Espar, gate
  await expect(card(page, "bus-alpha")).toContainText("alpha · vcan7");
  await expect(card(page, "bus-alpha")).toContainText("12.5 frames/s");
  await expect(card(page, "bus-alpha")).toContainText("1000 frames · 9 IDs · 0 errors");
  await expect(card(page, "bus-beta")).toContainText("open · no traffic");
  await expect(card(page, "bus-gamma").locator(".st")).toHaveText("down");
  await expect(card(page, "bus-gamma").locator(".st")).toHaveClass(/\bbad\b/);
  await expect(card(page, "net-0")).toContainText("online · 1.5 ms · port 80 ok");
  await expect(card(page, "net-1")).toContainText("no reply · 5 min");
  await expect(card(page, "net-1")).toContainText("check its power");
  await expect(card(page, "espar")).toContainText("awake · heater heating · EasyStart Pro on the bus");
  await expect(card(page, "espar")).toContainText("Airtronic S2 · 1 frame IDs");
  await expect(card(page, "tx")).toContainText("control enabled: n2k, victron · 3 blocked");
  await expect(card(page, "modules")).toHaveCount(0);                          // no svc.* keys (Python backend)
  await expect(page.locator("#dev-sub")).toHaveText("2 online · 1 offline · heard passively (no requests sent)");
  const rows = page.locator("#dev-table tr");
  await expect(rows).toHaveCount(4);
  await expect(rows.nth(1)).toContainText("Test Switcher · 000111");
  await expect(rows.nth(1)).toContainText("now");
  await expect(rows.nth(2)).toContainText("42 s ago");
  await expect(rows.nth(3)).toHaveClass("offline");
  await expect(rows.nth(3)).toContainText("OFFLINE");
  await expect(rows.nth(3)).toContainText("unidentified");
  await expect(rows.nth(3)).toContainText("15 min ago");
  await expect(page.locator("#inspect-table tr")).toHaveCount(3);
  await expect(page.locator("#inspect-table tr").first()).toHaveText(/pgn\s*name\s*count\s*sources\s*age/);
  await expect(page.locator("#inspect-table tr").nth(1)).toContainText("3,17");
  // the bus rate sparkline fills in (sampled once a second)
  await expect.poll(() => card(page, "bus-alpha").locator(".spark path").getAttribute("d"), { timeout: 5000 }).toMatch(/^M0\.0,.*L300\.0,/);
  await page.screenshot({ path: `${OUT}/react-systems-fixture.png` });
  expect(gw.sent.filter(m => m.type === "control")).toHaveLength(0);
});

test("inspector tabs switch, show an empty bus, and are remembered", async ({ page }) => {
  await stubGateway(page);
  await page.goto("/#systems");
  await page.click('#inspect-sel button[data-i="cmd"]');
  await expect(page.locator('#inspect-sel button[data-i="cmd"]')).toHaveClass("on");
  await expect(page.locator("#inspect-table")).toHaveText("No frames captured on this bus yet.");
  await page.click('#inspect-sel button[data-i="espar"]');
  await expect(page.locator("#inspect-table tr").first()).toHaveText(/id\s*count/);
  await page.reload();
  await expect(page.locator('#inspect-sel button[data-i="espar"]')).toHaveClass("on");
});

test("module health (Rust gateway): a Modules card, and 'module down' instead of 'not connected'", async ({ page }) => {
  const gw = await stubGateway(page, { state: { ...STATE, ...SVC } });
  await page.goto("/#systems");
  await expect(card(page, "modules")).toContainText("all 3 running");
  await expect(card(page, "modules").locator(".sys-mod")).toHaveCount(3);
  // the victron service dies: the gateway nulls its keys and marks it down
  gw.send({ type: "delta", data: { "svc.victron.up": false, "svc.victron.age_s": 130, "victron.connected": null, "victron.portal": null } });
  await expect(card(page, "cerbo")).toContainText("victron module down");
  await expect(card(page, "modules").locator(".st")).toHaveText("victron down");
  await expect(card(page, "modules").locator(".st")).toHaveClass(/\bbad\b/);
  await expect(card(page, "modules").locator(".sys-mod.down")).toHaveText("victron · 2 min");
  // the espar service dies: its card stays, and says why
  gw.send({ type: "delta", data: { "svc.espar.up": false, "svc.espar.age_s": 20, "climate.espar.heard": null, "climate.espar.asleep": null,
    "climate.espar.state": null, "climate.espar.controller": null, "espar.ids": null } });
  await expect(card(page, "espar")).toContainText("espar module down");
  await expect(card(page, "espar").locator(".st")).toHaveClass(/\bbad\b/);
  await expect(card(page, "modules").locator(".st")).toHaveText("espar, victron down");
  await page.screenshot({ path: `${OUT}/react-systems-modules.png` });
});

test("skeletons until the first snapshot: no false 'not connected' / 'no devices'", async ({ page }) => {
  const gw = await stubGateway(page, { hold: true, state: { ...BASE, "victron.connected": false } });
  await page.goto("/#systems");
  await expect(card(page, "cerbo")).toBeVisible();
  await expect(page.locator("html")).toHaveClass(/\bbooting\b/);
  await expect(card(page, "cerbo")).not.toContainText("not connected");
  await expect(card(page, "tx")).not.toContainText("read-only");
  await expect(page.locator("#dev-table")).not.toContainText("No NMEA 2000 devices");
  await expect(page.locator("#inspect-table")).not.toContainText("No frames");
  await page.screenshot({ path: `${OUT}/react-systems-skeleton.png` });
  gw.release();
  await expect(card(page, "cerbo")).toContainText("not connected");
  await expect(page.locator("#dev-table")).toHaveText("No NMEA 2000 devices heard yet.");
  await expect(page.locator("#inspect-table")).toHaveText("No frames captured on this bus yet.");
});

test("per-key updates: one device heard again touches only its row; a bus rate only its card", async ({ page }) => {
  const gw = await stubGateway(page);
  await page.goto("/#systems");
  await expect(page.locator("#dev-table tr")).toHaveCount(4);
  await page.waitForTimeout(400);
  const watch = () => page.evaluate(() => {
    const w = window as unknown as { __muts: Set<string>; __mo?: MutationObserver };
    w.__mo?.disconnect();
    w.__muts = new Set();
    w.__mo = new MutationObserver(ms => ms.forEach(m => {
      const el = (m.target.nodeType === 1 ? m.target : m.target.parentElement) as Element;
      if (el.closest(".spark")) return;                                      // the 1 Hz sparkline sampler, not a render
      const row = el.closest("#dev-table tr");
      const rows = row ? [...row.parentElement!.children] : [];
      w.__muts.add(row ? `dev:${rows.indexOf(row)}` : (el.closest(".sys") as HTMLElement | null)?.dataset.sys || el.closest(".card")?.className || el.tagName);
    }));
    w.__mo.observe(document.querySelector(".view.on")!, { subtree: true, childList: true, attributes: true, characterData: true });
  });
  const muts = () => page.evaluate(() => [...(window as unknown as { __muts: Set<string> }).__muts]);
  await watch();
  gw.send({ type: "delta", data: { "n2k.devices": DEVS.map((d, i) => (i === 1 ? { ...d, age: 0.5 } : { ...d, age: d.age + 0.2 })) } });
  await expect(page.locator("#dev-table tr").nth(2)).toContainText("now");
  await page.waitForTimeout(200);
  expect(await muts()).toEqual(["dev:2"]);                                   // row 1 still "now", row 3 still "15 min ago"
  await watch();
  gw.send({ type: "delta", data: { "bus.beta.rate": 3.5, "bus.beta.frames": 7 } });
  await expect(card(page, "bus-beta")).toContainText("3.5 frames/s");
  await page.waitForTimeout(200);
  expect(await muts()).toEqual(["bus-beta"]);
});

test("phone width: one column, no sideways scroll", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await stubGateway(page, { state: { ...STATE, ...SVC } });
  await page.goto("/#systems");
  await expect(card(page, "modules")).toBeVisible();
  const over = await page.evaluate(() => { const v = document.querySelector(".view.on")!; return v.scrollWidth - v.clientWidth; });
  expect(over).toBeLessThanOrEqual(0);
  await page.screenshot({ path: `${OUT}/react-systems-390x844.png` });
});
