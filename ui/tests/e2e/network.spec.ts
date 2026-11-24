// Network page against a STUB gateway (invented data; nothing reaches a backend or the internet: a catch-all route
// aborts and records any other request). Covers every command the page can send — Starlink install_update,
// Peplink set_wan_priority and set_wan_order — and that each is sent only after its confirmation, never on Cancel,
// never in view-only mode, and never while another command is pending.
import { expect, test, type Locator, type Page, type WebSocketRoute } from "@playwright/test";

// WebGL in headless Chrome (SwiftShader) for the 3D sky, set here rather than in the shared config
test.use({ launchOptions: { args: ["--use-angle=swiftshader", "--enable-unsafe-swiftshader", "--ignore-gpu-blocklist"] } });

interface Sent { type: string; device?: string; cmd?: string; value?: unknown; id?: number }

/** An invented obstruction map: a 61×61 grid, the south rows unseen (255), an obstructed arc in the east. */
function obstructionMap(n = 61) {
  let s = "";
  for (let r = 0; r < n; r++) for (let c = 0; c < n; c++) {
    const dx = c + .5 - n / 2, dy = r + .5 - n / 2, rr = Math.hypot(dx, dy), a = (Math.atan2(dx, -dy) * 180 / Math.PI + 360) % 360;
    s += String.fromCharCode(rr > n / 2 || dy > n / 4 ? 255 : a > 60 && a < 110 && rr > n / 3 ? 90 + Math.round(rr) : 254);
  }
  return { rows: n, cols: n, min_el: 10, max_theta: 80, frame: "dish", data: btoa(s) };
}
const WANS = [
  { id: 1, name: "Sat Link", message: "Connected", led: "green", enabled: true, priority: 1, uptime: 90000, connected: true, starlink: true },
  { id: 2, name: "LTE Modem", message: "Standby", led: "yellow", enabled: true, priority: 2, uptime: 0, connected: false, starlink: false,
    cellular: { carrier: "TestCarrier", network: "LTE", signal_level: 3, roaming: "home", ca: true, sim: 1, rat: "LTE", band: "Band 99", rsrp: -95, sinr: 8 } },
  { id: 3, name: "Wi-Fi WAN A", message: "Standby", led: "yellow", enabled: true, priority: 3, uptime: 0, connected: false, starlink: false,
    wifi: { ssid: "TestNet", level: 4, channel: 36, band: "5 GHz" } },
  { id: 4, name: "Wi-Fi WAN B", message: "Disabled", led: "gray", enabled: false, priority: null, uptime: 0, connected: false, starlink: false,
    wifi: { ssid: null, level: null, channel: null, band: null } },
  { id: 5, name: "Spare Port", message: "License Required", led: "gray", enabled: false, priority: null, uptime: 0, connected: false, starlink: false },
];
const STATE: Record<string, unknown> = {
  "starlink.reachable": true, "starlink.online": true, "starlink.state": "CONNECTED", "starlink.down_mbps": 123.4, "starlink.up_mbps": 12.3,
  "starlink.latency_ms": 41, "starlink.drop_rate": 0.004, "starlink.signal_quality": 0.92, "starlink.snr_db": 9.1, "starlink.snr_capped": false,
  "starlink.snr_persistently_low": false, "starlink.obstructed_pct": 1.4, "starlink.software": "test-sw-1.0", "starlink.azimuth": 170,
  "starlink.elevation": 65, "starlink.update_state": "REBOOT_REQUIRED", "starlink.alerts": [], "starlink.heating": "auto", "starlink.controls_ok": true,
  "starlink.obstruction_map": obstructionMap(),
  "peplink.reachable": true, "peplink.active": "Sat Link", "peplink.active_type": "starlink", "peplink.controls_ok": true, "peplink.wans": WANS,
  "peplink.allowance": [{ conn: 2, sim: null, usage_mb: 12000, limit_mb: 51200, percent: 23 }],
  "wan.down_mbps": 120, "wan.up_mbps": 11, "victron.controls_ok": true,
};

function history(url: string) {
  const u = new URL(url), keys = (u.searchParams.get("keys") || "").split(",").filter(Boolean), since = +(u.searchParams.get("since") || 86400);
  const now = Date.now() / 1000, out: Record<string, [number, number][]> = {};
  for (const k of keys) out[k] = Array.from({ length: 60 }, (_, i) => [now - since + i * since / 60, k.includes("snr") ? 8 : k.includes("down") ? 60 + 40 * Math.sin(i / 7) : 9]);
  return out;
}

interface Opts { state?: Record<string, unknown>; control?: boolean; reply?: (m: Sent) => object | null; query?: string }
async function stub(page: Page, o: Opts = {}) {
  const sent: Sent[] = [], outside: string[] = [], hist: string[] = [];
  let sock: WebSocketRoute | null = null;
  page.on("pageerror", e => outside.push(`pageerror: ${e.message}`));                // fails every test's outside check
  await page.route("**/*", r => {                                            // lowest precedence: everything else
    const u = r.request().url();
    if (/^https?:\/\/(127\.0\.0\.1|localhost)[:/]/.test(u)) return r.fallback();
    outside.push(u);
    return r.abort();
  });
  await page.route("http://127.0.0.1:8091/**", r => r.abort());
  await page.route("**/api/**", r => {
    const u = r.request().url();
    if (u.includes("/api/history")) { hist.push(u); return r.fulfill({ json: history(u) }); }
    return r.fulfill({ json: {} });
  });
  await page.routeWebSocket(/\/ws$/, ws => {
    sock = ws;
    ws.onMessage(raw => {
      const m = JSON.parse(String(raw)) as Sent;
      sent.push(m);
      if (m.type !== "control") return;
      const r = o.reply ? o.reply(m) : { ok: true, result: "ok", detail: "done (verified)" };
      if (r) setTimeout(() => ws.send(JSON.stringify({ type: "control_result", id: m.id, ...r })), 150);
    });
    ws.send(JSON.stringify({ type: "hello", control: o.control ?? true, who: "touchscreen" }));
    ws.send(JSON.stringify({ type: "snapshot", data: o.state ?? STATE }));
  });
  await page.goto(`/${o.query ?? ""}#network`);
  return { sent, outside, hist, send: (m: object) => sock?.send(JSON.stringify(m)) };
}
const controls = (sent: Sent[]) => sent.filter(m => m.type === "control");
const row = (page: Page, name: string) => page.locator("#wan-card .wan", { hasText: name });

/** Drag a WAN row by its handle so its centre lands `dy` px lower (negative: higher). */
async function dragRow(page: Page, r: Locator, dy: number) {
  const g = (await r.locator(".grip").boundingBox())!;
  await page.mouse.move(g.x + g.width / 2, g.y + g.height / 2);
  await page.mouse.down();
  await page.mouse.move(g.x + g.width / 2, g.y + g.height / 2 + dy, { steps: 8 });
  await page.mouse.up();
}

test("Starlink, Cellular, WWAN, WAN priority and the throughput chart render; nothing leaves the stubs", async ({ page }) => {
  const s = await stub(page);
  await expect(page.locator("#page-title")).toHaveText("Network");
  const sl = page.locator("#sl-card");
  await expect(sl.locator(".sl-title")).toContainText("Starlink");
  await expect(sl.locator(".sl-title .led")).toHaveClass(/led-green/);           // the router's view of the Starlink WAN
  await expect(page.locator("#sl-down")).toHaveText("123");
  await expect(page.locator("#sl-up")).toHaveText("12.3");
  for (const t of ["41 ms", "0.4%", "92%", "9.1 dB"]) await expect(sl.locator(".sl-kv")).toContainText(t);
  await expect(sl.locator(".sky3d canvas")).toHaveCount(1);                       // the 3D sky (WebGL)
  await expect(sl.locator(".sky-ov.tl")).toContainText("1.4%");
  await expect(sl.locator(".sky3d-hint")).toHaveText("Dish faces 170° · tilt 25°");
  const cell = page.locator("#cell-card");
  await expect(cell).toContainText("LTE");
  await expect(cell).toContainText("TestCarrier");
  await expect(cell).toContainText("RSRP · fair");
  await expect(cell).toContainText("SIM 1");
  await expect(cell.locator(".sl-obs")).toContainText("23%");
  await expect(cell.locator(".bars i.on")).toHaveCount(3);
  const ww = page.locator("#wwan-card");
  await expect(ww).toContainText("TestNet");
  await expect(ww).toContainText("channel · 5 GHz");
  await expect(ww).toContainText("P3");
  await expect(page.locator("#wan-card .wans > .wan")).toHaveCount(4);            // 3 enabled + 1 disabled; the licence-locked one hidden
  await expect(page.locator("#wan-card .wans > .wan").first()).toContainText("Sat Link");
  await expect(row(page, "Sat Link")).toHaveClass(/\bactive\b/);
  await expect(row(page, "Sat Link")).toContainText("up 1 d 1 h");
  await expect(row(page, "Wi-Fi WAN B")).toHaveClass(/\boff\b/);
  await expect(page.locator("#wan-card .wans")).toHaveClass(/\bcan-drag\b/);
  await expect(page.locator("#net-chart-title")).toHaveText("Sat Link throughput");
  await expect(page.locator("#legend-net span")).toHaveCount(3);
  await expect(page.locator("#chart-net svg.tchart")).toBeVisible();
  await expect(page.locator("#chart-net-snr svg.tchart")).toBeVisible();
  expect(s.hist.some(u => u.includes("keys=wan.down_mbps,wan.up_mbps,starlink.snr_db") && u.includes("since=86400"))).toBe(true);
  await page.click("#range-net button[data-s='604800']");
  await expect(page.locator("#range-net button[data-s='604800']")).toHaveClass(/\bon\b/);
  await expect.poll(() => s.hist.some(u => u.includes("since=604800"))).toBe(true);
  expect(s.outside).toEqual([]);
  expect(controls(s.sent)).toEqual([]);
});

test("Install update asks first: Cancel sends nothing, Confirm sends one install_update", async ({ page }) => {
  const s = await stub(page);
  const btn = page.locator("#sl-card [data-sl=install]");
  await btn.click();
  await expect(page.locator("#modal")).toBeVisible();
  await expect(page.locator("#modal-t")).toHaveText("Install the Starlink update?");
  await expect(page.locator("#modal-d")).toContainText("A firmware install can't be undone.");
  await page.click("#modal-no");
  await expect(page.locator("#modal")).toBeHidden();
  await page.waitForTimeout(300);
  expect(controls(s.sent)).toEqual([]);
  await btn.click();
  await page.click("#modal-yes");
  await expect.poll(() => controls(s.sent)).toEqual([{ type: "control", device: "starlink", cmd: "install_update", value: true, id: 1 }]);
  await expect(page.locator("#toast")).toContainText("Starlink update: done (verified)");
  expect(s.outside).toEqual([]);
});

test("WAN priority: P# opens the picker; a new priority asks first; the same one sends nothing", async ({ page }) => {
  const s = await stub(page);
  const lte = row(page, "LTE Modem");
  await lte.locator(".prio-btn").click();
  const pick = page.locator("#wan-card .wan-pick");
  await expect(pick).toContainText("Priority for LTE Modem");
  await expect(pick.locator("button.on")).toHaveText("P2");
  await pick.locator("button[data-prio='2']").click();                            // unchanged: no question, no command
  await expect(pick).toHaveCount(0);
  await expect(page.locator("#modal")).toBeHidden();
  await lte.locator(".prio-btn").click();
  await page.locator("#wan-card .wan-pick button[data-prio='1']").click();
  await expect(page.locator("#modal-t")).toHaveText("Set LTE Modem to priority 1?");
  await page.click("#modal-no");
  await page.waitForTimeout(300);
  expect(controls(s.sent)).toEqual([]);
  await lte.locator(".prio-btn").click();
  await page.locator("#wan-card .wan-pick button[data-prio='1']").click();
  await page.click("#modal-yes");
  await expect.poll(() => controls(s.sent)).toEqual([{ type: "control", device: "peplink", cmd: "set_wan_priority", value: { id: 2, priority: 1 }, id: 1 }]);
  expect(s.outside).toEqual([]);
});

test("WAN drag: reorder asks first with the new priorities; Cancel sends nothing; Confirm sends the ids", async ({ page }) => {
  const s = await stub(page);
  const a = row(page, "Wi-Fi WAN A"), top = row(page, "Sat Link");
  const dy = (await top.boundingBox())!.y - (await a.boundingBox())!.y - 10;
  await dragRow(page, a, dy);
  await expect(page.locator("#modal-t")).toHaveText("Change the WAN order?");
  await expect(page.locator("#modal-d")).toContainText("Priorities: P1 Wi-Fi WAN A, P2 Sat Link, P3 LTE Modem.");
  await page.click("#modal-no");
  await page.waitForTimeout(300);
  expect(controls(s.sent)).toEqual([]);
  await expect(page.locator("#wan-card .wans > .wan").first()).toContainText("Sat Link");     // nothing moved
  await dragRow(page, a, dy);
  await page.click("#modal-yes");
  await expect.poll(() => controls(s.sent)).toEqual([{ type: "control", device: "peplink", cmd: "set_wan_order", value: { order: [3, 1, 2], disabled: [4] }, id: 1 }]);
  expect(s.outside).toEqual([]);
});

test("WAN drag across 'Disabled': disabling the active link says traffic moves; enabling one says so", async ({ page }) => {
  const s = await stub(page);
  const mid = async (l: Locator) => { const b = (await l.boundingBox())!; return b.y + b.height / 2; };
  const sep = page.locator("#wan-card .wan-sep"), sat = row(page, "Sat Link"), a = row(page, "Wi-Fi WAN A"), b = row(page, "Wi-Fi WAN B");
  await dragRow(page, sat, (await mid(sep) + await mid(b)) / 2 - await mid(sat));     // just below the separator
  await expect(page.locator("#modal-t")).toHaveText("Disable Sat Link?");
  await expect(page.locator("#modal-d")).toContainText("Sat Link is carrying your internet now");
  await page.click("#modal-yes");
  await expect.poll(() => controls(s.sent).map(m => m.value)).toEqual([{ order: [2, 3], disabled: [1, 4] }]);
  await expect(page.locator("#toast")).toContainText("WAN order: done (verified)");
  // enable: the disabled one dragged up above the separator
  await dragRow(page, b, (await mid(a) + await mid(sep)) / 2 - await mid(b));
  await expect(page.locator("#modal-t")).toHaveText("Enable Wi-Fi WAN B?");
  await expect(page.locator("#modal-d")).toContainText("Priorities: P1 Sat Link, P2 LTE Modem, P3 Wi-Fi WAN A, P4 Wi-Fi WAN B. Enable: Wi-Fi WAN B.");
  await page.click("#modal-no");
  expect(controls(s.sent)).toHaveLength(1);
  expect(s.outside).toEqual([]);
});

test("a pending command disables every control; a refused one rolls back with its reason", async ({ page }) => {
  let hold = true;
  const s = await stub(page, { reply: () => (hold ? null : { ok: false, result: "refused", detail: "router said no" }) });
  await row(page, "LTE Modem").locator(".prio-btn").click();
  await page.locator("#wan-card .wan-pick button[data-prio='1']").click();
  await page.click("#modal-yes");
  await expect.poll(() => controls(s.sent)).toHaveLength(1);
  await expect(page.locator("#wan-card .prio-btn").first()).toBeDisabled();
  await expect(page.locator("#sl-card [data-sl=install]")).toBeDisabled();
  await expect(page.locator("#wan-card .wans")).not.toHaveClass(/\bcan-drag\b/);
  await dragRow(page, row(page, "Wi-Fi WAN A"), -120);                          // no drag while pending
  await expect(page.locator("#modal")).toBeHidden();
  expect(controls(s.sent)).toHaveLength(1);
  // answer the pending one: refused → toast with the reason, controls back
  hold = false;
  s.send({ type: "control_result", id: 1, ok: false, result: "refused", detail: "router said no" });
  await expect(page.locator("#toast")).toContainText("WAN priority failed: router said no");
  await expect(page.locator("#wan-card .prio-btn").first()).toBeEnabled();
  expect(s.outside).toEqual([]);
});

test("view only: no install button, no P# buttons, no drag; nothing is sent", async ({ page }) => {
  const s = await stub(page, { control: false });
  await expect(page.locator("#sl-card .sl-kv")).toContainText("41 ms");
  await expect(page.locator("#sl-card [data-sl=install]")).toHaveCount(0);
  await expect(page.locator("#wan-card .prio-btn")).toHaveCount(0);
  await expect(row(page, "LTE Modem").locator(".prio")).toHaveText("P2");
  await expect(page.locator("#wan-card .wans")).not.toHaveClass(/\bcan-drag\b/);
  await row(page, "LTE Modem").locator(".prio").click();
  await dragRow(page, row(page, "Wi-Fi WAN A"), -120);
  await expect(page.locator("#modal")).toBeHidden();
  await expect(page.locator("#wan-card .wan-pick")).toHaveCount(0);
  expect(controls(s.sent)).toEqual([]);
  expect(s.outside).toEqual([]);
});

test("a device's own control path off (controls_ok false) hides that device's controls only", async ({ page }) => {
  const s = await stub(page, { state: { ...STATE, "starlink.controls_ok": false } });
  await expect(page.locator("#sl-card [data-sl=install]")).toHaveCount(0);
  await expect(page.locator("#wan-card .prio-btn")).toHaveCount(3);
  expect(controls(s.sent)).toEqual([]);
});

test("not enabled: Starlink and Peplink absent say so; no update ready: no install button", async ({ page }) => {
  const s = await stub(page, { state: { "victron.controls_ok": true } });
  await expect(page.locator("#sl-card")).toContainText("Not enabled.");
  await expect(page.locator("#cell-card")).toContainText("Not enabled.");
  await expect(page.locator("#wwan-card")).toContainText("Not enabled.");
  await expect(page.locator("#wan-card")).toContainText("Peplink not enabled.");
  await expect(page.locator("#legend-net span")).toHaveCount(2);                   // no Starlink: no SNR lane
  await expect(page.locator("#chart-net-snr")).toHaveCount(0);
  s.send({ type: "delta", data: { ...STATE, "starlink.update_state": "IDLE" } });
  await expect(page.locator("#sl-card .sl-kv")).toContainText("41 ms");
  await expect(page.locator("#sl-card [data-sl=install]")).toHaveCount(0);
  expect(s.outside).toEqual([]);
});

test("hidden page: the 3D sky's WebGL context is freed, and rebuilt on return", async ({ page }) => {
  const s = await stub(page);
  await expect(page.locator("#sl-card .sky3d canvas")).toHaveCount(1);
  const lost = await page.locator("#sl-card .sky3d canvas").evaluate(c => {
    const w = window as unknown as { __lost?: boolean };
    c.addEventListener("webglcontextlost", () => { w.__lost = true; });
    return true;
  });
  expect(lost).toBe(true);
  await page.click(".rail button[data-v=home]");
  await expect(page.locator("#sl-card .sky3d canvas")).toHaveCount(0);
  expect(await page.evaluate(() => (window as unknown as { __lost?: boolean }).__lost)).toBe(true);
  await page.click(".rail button[data-v=network]");
  await expect(page.locator("#sl-card .sky3d canvas")).toHaveCount(1);
  expect(s.outside).toEqual([]);
});

test("?sky=2d (or no WebGL): the 2D obstruction map with its caption", async ({ page }) => {
  const s = await stub(page, { query: "?sky=2d" });
  await expect(page.locator("#sl-card .sky3d")).toHaveCount(0);
  await expect(page.locator("#sl-card .sky .sky-disc img")).toHaveCount(1);
  await expect(page.locator("#sl-card figcaption")).toContainText("1.4% obstructed");
  await expect(page.locator("#sl-card .sky text")).toHaveCount(4);                 // N E S W
  await expect(page.locator("#sl-card .sky .facing")).toHaveAttribute("transform", /rotate\(170\.0 /);
  expect(s.outside).toEqual([]);
});

test("before the first snapshot: skeleton cards, never 'Not enabled.'", async ({ page }) => {
  await page.route("**/*", r => (/^https?:\/\/(127\.0\.0\.1|localhost)[:/]/.test(r.request().url()) ? r.fallback() : r.abort()));
  await page.route("**/api/**", r => r.fulfill({ json: {} }));
  await page.routeWebSocket(/\/ws$/, () => { /* never answers */ });
  await page.goto("/#network");
  await expect(page.locator("html")).toHaveClass(/\bbooting\b/);
  await expect(page.locator("#sl-card .sl-kv")).toBeVisible();
  await expect(page.locator(".view.on")).not.toContainText("Not enabled");
});
