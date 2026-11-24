// Weather page against a STUB gateway and stubbed tile / API hosts (invented data; nothing reaches a backend or the
// internet: a catch-all route aborts and records any other request, and every test asserts there were none).
// The Weather page sends no device commands at all; every test checks that too.
import { expect, test, type Page, type WebSocketRoute } from "@playwright/test";

// WebGL in headless Chrome (SwiftShader), set here rather than in the shared config
test.use({ launchOptions: { args: ["--use-angle=swiftshader", "--enable-unsafe-swiftshader", "--ignore-gpu-blocklist"] } });

interface Sent { type: string; device?: string; cmd?: string; value?: unknown; id?: number }
const B = (globalThis as unknown as { Buffer: { from(s: string, enc: "base64"): never } }).Buffer;
// 8×8 grey basemap tile, 16×16 radar blob (generated, not real map data)
const BASE_PNG = "iVBORw0KGgoAAAANSUhEUgAAAAgAAAAICAYAAADED76LAAAAGUlEQVR4nGNw84v6jw8zgAgdIyuceHgoAAD2+2rLADQcfwAAAABJRU5ErkJggg==";
const RADAR_PNG = "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAf8/9hAAAAPklEQVR4nGNgGJSAcf6XE7gwRZoJGoKsiOkv0x10jNcQQpoJGkKMZnRDRg3AYQDZsUBxOsBmCE2SM0HNpAIAbeMz8RyKfDsAAAAASUVORK5CYII=";
const png = (b64: string) => ({ body: B.from(b64, "base64"), contentType: "image/png" });

const LAT = 44.123, LON = -103.456;                     // invented position
const NOW = Date.now() / 1000, T0 = Math.floor(NOW / 600) * 600 - 600;
const WMAPS = {
  host: "https://tiles.librewxr.test", generated: Math.round(NOW),
  radar: {
    past: Array.from({ length: 13 }, (_, i) => ({ time: T0 - (12 - i) * 600, path: `/v2/radar/p${i}` })),
    nowcast: Array.from({ length: 6 }, (_, i) => ({ time: T0 + (i + 1) * 600, path: `/v2/radar/c${i}` })),
  },
  satellite: { infrared: Array.from({ length: 4 }, (_, i) => ({ time: T0 - (3 - i) * 3600, path: `/v2/sat/${i}` })) },
};
const REC_FRAMES = Array.from({ length: 6 }, (_, i) => T0 - 7200 - (6 - i) * 600);
const RECORDED = { frames: REC_FRAMES, flows: [...REC_FRAMES, ...WMAPS.radar.past.map(p => p.time)], zoom: [4, 7] };
const FLOW = { z: 6, x0: 12, y0: 21, w: 3, n: 32, dx: Array(1024).fill(3), dy: Array(1024).fill(-1), dt: 600 };
const day = (i: number) => new Date(Date.now() + i * 86400e3).toISOString().slice(0, 10);
const FORECAST = { daily: { time: Array.from({ length: 7 }, (_, i) => day(i)), weather_code: [0, 2, 3, 45, 61, 71, 95],
  temperature_2m_max: [50, 52, 48, 45, 40, 33, 60], temperature_2m_min: [30, 31, 29, 28, 25, 20, 41],
  precipitation_probability_max: [0, 5, 10, 20, 80, 60, 90], wind_speed_10m_max: [5, 7, 9, 11, 13, 15, 22] } };
const STATE: Record<string, unknown> = {
  "location.lat": LAT, "location.lon": LON, "location.alt_m": 1000, "location.fix": true, "location.speed": 0, "location.source": "test",
  "location.hdop": 0.9, "traffic.available": true, "victron.controls_ok": true,
};

interface Opts { state?: Record<string, unknown>; control?: boolean; wxIndex?: "ok" | "fail"; forecast?: "ok" | "fail"; query?: string }
/** Stubs: WebSocket (hello + snapshot; records what's sent), tile hosts, LibreWXR, Open-Meteo, /api/*. */
async function stub(page: Page, o: Opts = {}) {
  const sent: Sent[] = [], outside: string[] = [], seen: string[] = [], errors: string[] = [];
  let sock: WebSocketRoute | null = null;
  page.on("pageerror", e => errors.push(e.message));
  page.on("console", m => { if (/WebGL|context/i.test(m.text()) && m.type() !== "debug") errors.push(m.text()); });
  // first = lowest precedence: anything not stubbed below is aborted and recorded
  await page.route("**/*", r => {
    const u = r.request().url();
    if (/^https?:\/\/(127\.0\.0\.1|localhost)[:/]/.test(u)) return r.fallback();
    outside.push(u);
    return r.abort();
  });
  await page.route("http://127.0.0.1:8091/**", r => r.abort());                       // kiosk screen helper: never
  await page.route(/^https:\/\/(tile\.openstreetmap\.org|basemap\.nationalmap\.gov|[abc]\.tile\.opentopomap\.org)\//, r => { seen.push(r.request().url()); return r.fulfill(png(BASE_PNG)); });
  await page.route("https://gis.blm.gov/**", r => { seen.push(r.request().url()); return r.fulfill(png(RADAR_PNG)); });
  await page.route("https://api.librewxr.net/public/weather-maps.json", r => o.wxIndex === "fail" ? r.fulfill({ status: 503, body: "down" }) : r.fulfill({ json: WMAPS }));
  await page.route("https://tiles.librewxr.test/**", r => { seen.push(r.request().url()); return r.fulfill(png(RADAR_PNG)); });
  await page.route(/^https:\/\/api\.open-meteo\.com\/v1\/forecast/, r => { seen.push(r.request().url()); return o.forecast === "fail" ? r.fulfill({ status: 500, body: "x" }) : r.fulfill({ json: FORECAST }); });
  await page.route("**/api/**", r => {
    const u = r.request().url();
    seen.push(u);
    if (u.includes("/api/radar/recorded")) return r.fulfill({ json: RECORDED });
    if (u.includes("/api/radar/flow/")) return r.fulfill({ json: FLOW });
    if (/\/api\/(radar\/tile|traffic)\//.test(u)) return r.fulfill(png(RADAR_PNG));
    return r.fulfill({ json: {} });
  });
  await page.routeWebSocket(/\/ws$/, ws => {
    sock = ws;
    ws.onMessage(raw => sent.push(JSON.parse(String(raw)) as Sent));
    ws.send(JSON.stringify({ type: "hello", control: o.control ?? true, who: "touchscreen" }));
    ws.send(JSON.stringify({ type: "snapshot", data: o.state ?? STATE }));
  });
  await page.goto(`/${o.query ?? ""}#weather`);
  return { sent, outside, seen, errors, send: (m: object) => sock?.send(JSON.stringify(m)) };
}
const controls = (sent: Sent[]) => sent.filter(m => m.type === "control");
async function noLeaks(s: { sent: Sent[]; outside: string[]; errors: string[] }) {
  expect(s.errors).toEqual([]);
  expect(s.outside).toEqual([]);
  expect(controls(s.sent)).toEqual([]);
}

test("map, radar loop, sun line and forecast render from stubs; nothing leaves the stubs", async ({ page }) => {
  const s = await stub(page);
  await expect(page.locator("#page-title")).toHaveText("Weather");
  await expect(page.locator("#wx-sub")).toContainText("Van 44.123° N, 103.456° W");
  await expect(page.locator("#wx-sub small")).toHaveText("3,281 ft · parked");
  await expect(page.locator("#wx-loc")).toContainText("sunrise");
  await expect(page.locator("#wx-loc")).toContainText("daylight");
  await expect(page.locator(".wx-van")).toHaveCount(1);
  await expect(page.locator("#wx-fc .fc-row:not(.fc-skel)")).toHaveCount(7);
  await expect(page.locator("#wx-fc .fc-row").first()).toContainText("Today");
  await expect(page.locator("#wx-fc .fc-row").first()).toContainText("Clear");
  await expect(page.locator("#wx-fc .fc-row").nth(6)).toContainText("90%");
  // 2 h loop: the 13 observed frames + 6 nowcast; "now" on the newest observed one
  await expect(page.locator("#wx-tl-ticks i")).toHaveCount(19);
  await expect(page.locator("#wx-tl-ticks i.now")).toHaveCount(1);
  await expect(page.locator("#wx-time")).toContainText("min ago");
  await expect(page.locator(".wx-radar-gl.on")).toHaveCount(1);            // WebGL radar drew its first image
  await expect(page.locator(".leaflet-control-attribution")).toContainText("LibreWXR");
  expect(s.seen.some(u => u.startsWith("https://api.open-meteo.com/v1/forecast?latitude=44.123&longitude=-103.456"))).toBe(true);
  expect(s.seen.some(u => u.includes("/api/radar/recorded"))).toBe(true);
  await expect.poll(() => s.seen.some(u => u.includes("/api/radar/flow/"))).toBe(true);
  await noLeaks(s);
});

test("play / pause, loop length and speed: cycled and remembered per screen", async ({ page }) => {
  const s = await stub(page);
  await expect(page.locator("#wx-tl-ticks i")).toHaveCount(19);
  const thumb = page.locator("#wx-tl-thumb");
  await page.click("#wx-play");
  await expect(page.locator("#wx-tl")).toHaveClass(/\bplaying\b/);
  await expect(page.locator("#wx-play")).toHaveAttribute("aria-label", "Pause radar loop");
  const a = await thumb.evaluate(e => parseFloat((e as HTMLElement).style.left));
  await expect.poll(async () => thumb.evaluate(e => parseFloat((e as HTMLElement).style.left))).not.toBe(a);   // it moves
  await page.click("#wx-play");
  await expect(page.locator("#wx-tl")).not.toHaveClass(/\bplaying\b/);
  // paused on a real frame
  const n = Number(await page.locator("#wx-tl-track").getAttribute("aria-valuenow"));
  expect(Math.abs(parseFloat(await thumb.evaluate(e => (e as HTMLElement).style.left)) - n / 18 * 100)).toBeLessThan(0.01);
  await expect(page.locator("#wx-span")).toHaveText("2h");
  await page.click("#wx-span");
  await expect(page.locator("#wx-span")).toHaveText("6h");
  await expect(page.locator("#wx-tl-ticks i")).toHaveCount(25);            // + the 6 frames the Pi recorded
  await expect(page.locator("#wx-tl-ticks i.rec")).toHaveCount(6);
  await expect(page.locator("#wx-speed")).toHaveText("1×");
  await page.click("#wx-speed");
  await expect(page.locator("#wx-speed")).toHaveText("2×");
  await page.click("#wx-speed"); await page.click("#wx-speed");
  await expect(page.locator("#wx-speed")).toHaveText("1×");
  expect(await page.evaluate(() => [localStorage.getItem("vp.radar.span"), localStorage.getItem("vp.radar.speed")])).toEqual(["6", "1"]);
  await page.reload();
  await expect(page.locator("#wx-span")).toHaveText("6h");
  await noLeaks(s);
});

test("dragging the timeline scrubs (continuously with WebGL); the forecast part is marked", async ({ page }) => {
  const s = await stub(page);
  await expect(page.locator("#wx-tl-ticks i")).toHaveCount(19);
  const r = (await page.locator("#wx-tl-track .wx-tl-rail").boundingBox())!;
  await page.mouse.move(r.x + 2, r.y + r.height / 2);
  await page.mouse.down();
  await expect(page.locator("#wx-tl-track")).toHaveClass(/\bdrag\b/);
  await expect(page.locator("#wx-tl-track")).toHaveAttribute("aria-valuenow", "0");
  await page.mouse.move(r.x + r.width * 0.37, r.y + r.height / 2, { steps: 5 });
  expect(parseFloat(await page.locator("#wx-tl-thumb").evaluate(e => (e as HTMLElement).style.left))).toBeCloseTo(37, 0);
  await page.mouse.move(r.x + r.width - 1, r.y + r.height / 2, { steps: 5 });
  await page.mouse.up();
  await expect(page.locator("#wx-tl")).toHaveClass(/\bcast\b/);
  await expect(page.locator("#wx-time")).toContainText("forecast +");
  await noLeaks(s);
});

test("layers menu: basemap, weather layer (off hides the loop controls), overlays; a tap outside closes it", async ({ page }) => {
  const s = await stub(page);
  await expect(page.locator("#wx-menu-pop")).toBeHidden();
  await page.click("#wx-menu");
  await expect(page.locator("#wx-menu-pop")).toBeVisible();
  await expect(page.locator("#wx-menu")).toHaveAttribute("aria-expanded", "true");
  await expect(page.locator('.wx-seg[data-group=base] button[data-v=streets]')).toHaveClass(/\bon\b/);
  await page.click('.wx-seg[data-group=base] button[data-v=sat]');
  await expect(page.locator('.wx-seg[data-group=base] button[data-v=sat]')).toHaveClass(/\bon\b/);
  await expect.poll(() => s.seen.some(u => u.startsWith("https://basemap.nationalmap.gov/"))).toBe(true);
  await page.click('.wx-seg[data-group=wx] button[data-v=off]');
  for (const id of ["#wx-tl", "#wx-speed", "#wx-span", "#wx-opac"]) await expect(page.locator(id)).toBeHidden();
  await page.click('.wx-seg[data-group=wx] button[data-v=ir]');
  await expect(page.locator("#wx-tl")).toBeVisible();
  await expect(page.locator("#wx-tl-ticks i")).toHaveCount(3);              // hourly IR within the 2 h loop
  await expect(page.locator("#wx-time")).toContainText("IR ·");
  await page.click('.wx-seg[data-group=wx] button[data-v=radar]');
  await expect(page.locator("#wx-tl-ticks i")).toHaveCount(19);
  await page.click("#wx-lands");
  await expect(page.locator("#wx-lands")).toHaveClass(/\bon\b/);
  await expect.poll(() => s.seen.some(u => u.startsWith("https://gis.blm.gov/"))).toBe(true);
  await expect(page.locator("#wx-traffic")).toBeVisible();                  // traffic.available
  await expect(page.locator("#wx-traffic")).toHaveClass(/\bon\b/);
  await expect(page.locator(".leaflet-layer.wx-traffic")).toHaveCount(2);   // slow roads + incidents
  await page.click("#wx-traffic");
  await expect(page.locator(".leaflet-layer.wx-traffic")).toHaveCount(0);
  expect(await page.evaluate(() => [localStorage.getItem("vp.wx.base"), localStorage.getItem("vp.wx.layer"), localStorage.getItem("vp.wx.lands"),
    localStorage.getItem("vp.traffic")])).toEqual(["sat", "radar", "on", "off"]);
  await page.click("#page-title");                                          // outside
  await expect(page.locator("#wx-menu-pop")).toBeHidden();
  await noLeaks(s);
});

test("a tap on the map pins that spot and shows its forecast; re-centre returns to the van", async ({ page }) => {
  const s = await stub(page);
  await expect(page.locator("#wx-fc .fc-row:not(.fc-skel)")).toHaveCount(7);
  await expect(page.locator("#wx-hint")).toBeVisible();
  const m = (await page.locator("#wx-map").boundingBox())!;
  await page.mouse.click(m.x + m.width * 0.25, m.y + m.height * 0.3);
  await expect(page.locator(".wx-pin")).toHaveCount(1);
  await expect(page.locator("#wx-hint")).toBeHidden();
  await expect.poll(() => s.seen.filter(u => u.includes("open-meteo") && !u.includes("latitude=44.123")).length).toBe(1);
  await expect(page.locator("#wx-fc .fc-row:not(.fc-skel)")).toHaveCount(7);
  const before = s.seen.filter(u => u.includes("latitude=44.123")).length;
  await page.click("#wx-recenter");
  await expect(page.locator(".wx-pin")).toHaveCount(0);
  await expect(page.locator("#wx-hint")).toBeVisible();
  await expect.poll(() => s.seen.filter(u => u.includes("latitude=44.123")).length).toBe(before + 1);
  await noLeaks(s);
});

test("map mode cycles Auto → Dark → Light; Auto without a position follows night mode", async ({ page }) => {
  const s = await stub(page, { state: { "victron.controls_ok": true }, query: "?night=1" });
  await expect(page.locator("#wx-sub")).toHaveText("No GPS position yet");
  await expect(page.locator("#wx-empty")).toBeVisible();
  await expect(page.locator("#wx-loc")).toBeEmpty();
  await expect(page.locator("#wx-dark")).toHaveText("Map: Auto");
  await expect(page.locator(".wx-wrap")).toHaveClass(/\bdark-map\b/);       // night, no fix
  await page.click("#btn-night");                                           // day
  await expect(page.locator(".wx-wrap")).not.toHaveClass(/\bdark-map\b/);
  await page.click("#wx-dark");
  await expect(page.locator("#wx-dark")).toHaveText("Map: Dark");
  await expect(page.locator(".wx-wrap")).toHaveClass(/\bdark-map\b/);
  await page.click("#wx-dark");
  await expect(page.locator("#wx-dark")).toHaveText("Map: Light");
  await expect(page.locator(".wx-wrap")).not.toHaveClass(/\bdark-map\b/);
  await page.click("#wx-dark");
  await expect(page.locator("#wx-dark")).toHaveText("Map: Auto");
  expect(await page.evaluate(() => localStorage.getItem("vp.wxmap.mode"))).toBe("auto");
  await expect(page.locator("#wx-fc")).toContainText("waiting for a position");
  await noLeaks(s);
});

test("radar opacity: the slider sets it in 5 % steps and remembers it", async ({ page }) => {
  const s = await stub(page);
  await expect(page.locator("#wx-opac")).toHaveAttribute("aria-valuenow", "85");
  const t = (await page.locator("#wx-opac-track").boundingBox())!;
  await page.mouse.move(t.x + t.width / 2, t.y + t.height * 0.5);
  await page.mouse.down();
  await expect(page.locator("#wx-opac")).toHaveClass(/\bdrag\b/);
  await page.mouse.move(t.x + t.width / 2, t.y + t.height - 1, { steps: 4 });
  await page.mouse.up();
  await expect(page.locator("#wx-opac")).toHaveAttribute("aria-valuenow", "10");
  expect(await page.evaluate(() => localStorage.getItem("vp.radar.opacity"))).toBe("0.1");
  await noLeaks(s);
});

test("hidden page: the radar keeps its WebGL context, paused, and shows at once on return; the map and its pin stay", async ({ page }) => {
  const s = await stub(page);
  await expect(page.locator(".wx-radar-gl.on")).toHaveCount(1);
  await page.locator(".wx-radar-gl").evaluate(c => {
    const w = window as unknown as { __wxLost?: boolean };
    c.addEventListener("webglcontextlost", () => { w.__wxLost = true; });
    (c as HTMLCanvasElement).dataset.mark = "kept";
  });
  const m = (await page.locator("#wx-map").boundingBox())!;
  await page.mouse.click(m.x + m.width * 0.6, m.y + m.height * 0.6);
  await expect(page.locator(".wx-pin")).toHaveCount(1);
  await page.click("#wx-play");
  await expect(page.locator("#wx-tl")).toHaveClass(/\bplaying\b/);
  await page.click(".rail button[data-v=network]");
  await expect(page.locator("#wx-tl")).not.toHaveClass(/\bplaying\b/);      // the loop stopped
  await expect(page.locator(".wx-radar-gl")).toHaveCount(1);                 // the canvas (and its context) kept
  await page.click(".rail button[data-v=weather]");
  await expect(page.locator(".wx-radar-gl.on[data-mark=kept]")).toHaveCount(1);   // the same canvas, not a new one
  await expect(page.locator(".wx-pin")).toHaveCount(1);
  await expect(page.locator("#wx-tl-ticks i")).toHaveCount(19);
  expect(await page.evaluate(() => (window as unknown as { __wxLost?: boolean }).__wxLost)).toBeUndefined();
  await noLeaks(s);
});

test("?keepgl=0: hiding the page frees the radar's WebGL context and showing it rebuilds it; the map and its pin stay", async ({ page }) => {
  const s = await stub(page, { query: "?keepgl=0" });
  await expect(page.locator(".wx-radar-gl.on")).toHaveCount(1);
  const m = (await page.locator("#wx-map").boundingBox())!;
  await page.mouse.click(m.x + m.width * 0.6, m.y + m.height * 0.6);
  await expect(page.locator(".wx-pin")).toHaveCount(1);
  await page.click("#wx-play");
  await page.click(".rail button[data-v=network]");
  await expect(page.locator(".wx-radar-gl")).toHaveCount(0);               // canvas (and its context) gone
  await expect(page.locator("#wx-tl")).not.toHaveClass(/\bplaying\b/);      // the loop stopped
  await page.click(".rail button[data-v=weather]");
  await expect(page.locator(".wx-radar-gl.on")).toHaveCount(1);
  await expect(page.locator(".wx-pin")).toHaveCount(1);
  await expect(page.locator("#wx-tl-ticks i")).toHaveCount(19);
  await noLeaks(s);
});

test("?keep=0 (a real unmount): leaving removes the map; coming back rebuilds it with the same pin", async ({ page }) => {
  const s = await stub(page, { query: "?keep=0" });
  await expect(page.locator(".wx-radar-gl.on")).toHaveCount(1);
  const m = (await page.locator("#wx-map").boundingBox())!;
  await page.mouse.click(m.x + m.width * 0.3, m.y + m.height * 0.7);
  await expect(page.locator(".wx-pin")).toHaveCount(1);
  await page.click(".rail button[data-v=home]");
  await expect(page.locator("#wx-map")).toHaveCount(0);
  await expect(page.locator(".leaflet-container")).toHaveCount(0);
  await page.click(".rail button[data-v=weather]");
  await expect(page.locator(".wx-radar-gl.on")).toHaveCount(1);
  await expect(page.locator(".wx-pin")).toHaveCount(1);
  await expect(page.locator("#wx-hint")).toBeHidden();
  await noLeaks(s);
});

const SKY = { "starlink.reachable": true, "starlink.online": true, "starlink.azimuth": 90, "starlink.elevation": 70,
  "starlink.obstruction_map": { rows: 3, cols: 3, max_theta: 80, frame: "dish", data: btoa("\xfe".repeat(9)) } };

test("many page switches don't pile up WebGL contexts (one per scene, kept)", async ({ page }) => {
  const s = await stub(page, { state: { ...STATE, ...SKY } });
  for (let i = 0; i < 12; i++) {
    await expect(page.locator(".wx-radar-gl.on")).toHaveCount(1);
    await page.click(".rail button[data-v=network]");
    await expect(page.locator("#sl-card .sky3d canvas")).toHaveCount(1);
    await page.click(".rail button[data-v=weather]");
  }
  await expect(page.locator(".wx-radar-gl.on")).toHaveCount(1);
  await expect(page.locator("canvas:not(.f-canvas)")).toHaveCount(2);    // WebGL: the radar's and the sky's, nothing more
  await noLeaks(s);
});

test("?keepgl=0: many page switches don't pile up WebGL contexts (each hide frees its context)", async ({ page }) => {
  const s = await stub(page, { state: { ...STATE, ...SKY }, query: "?keepgl=0" });
  for (let i = 0; i < 12; i++) {
    await expect(page.locator(".wx-radar-gl.on")).toHaveCount(1);
    await page.click(".rail button[data-v=network]");
    await expect(page.locator("#sl-card .sky3d canvas")).toHaveCount(1);
    await page.click(".rail button[data-v=weather]");
  }
  await expect(page.locator(".wx-radar-gl.on")).toHaveCount(1);
  await expect(page.locator("canvas:not(.f-canvas)")).toHaveCount(1);    // WebGL: only the radar's (the sky's was disposed)
  await noLeaks(s);
});

test("failures: radar index down → 'radar unavailable'; forecast down → says so", async ({ page }) => {
  const s = await stub(page, { wxIndex: "fail", forecast: "fail" });
  await expect(page.locator("#wx-time")).toHaveText("radar unavailable");
  await expect(page.locator("#wx-fc")).toContainText("Forecast unavailable");
  await expect(page.locator("#wx-fc")).toContainText("HTTP 500");
  await noLeaks(s);
});

test("without WebGL (?radar=tiles) the loop steps through Leaflet tile layers", async ({ page }) => {
  const s = await stub(page, { query: "?radar=tiles" });
  await expect(page.locator("#wx-tl-ticks i")).toHaveCount(19);
  await expect(page.locator(".wx-radar-gl")).toHaveCount(0);
  await expect(page.locator(".leaflet-layer.wx-radar").first()).toBeAttached();
  await page.click("#wx-play");
  await expect(page.locator("#wx-tl")).toHaveClass(/\bplaying\b/);
  await expect.poll(async () => Number(await page.locator("#wx-tl-track").getAttribute("aria-valuenow")), { timeout: 5000 }).toBeGreaterThan(1);
  await page.click("#wx-play");
  await noLeaks(s);
});

test("before the first snapshot: skeletons, no 'No GPS' message", async ({ page }) => {
  await page.route("**/*", r => (/^https?:\/\/(127\.0\.0\.1|localhost)[:/]/.test(r.request().url()) ? r.fallback() : r.abort()));
  await page.route("**/api/**", r => r.fulfill({ json: {} }));
  await page.routeWebSocket(/\/ws$/, () => { /* never answers */ });
  await page.goto("/#weather");
  await expect(page.locator("html")).toHaveClass(/\bbooting\b/);
  await expect(page.locator("#wx-fc .fc-skel")).toHaveCount(7);
  await expect(page.locator("#wx-sub")).toBeEmpty();
  await expect(page.locator("#wx-empty")).toBeHidden();
});
