// The README's screenshots (docs/screenshots/*.png), from a sanitized state snapshot and a copy of the history
// database: harness/ui-reference/screenshots.sh sets SHOTS_STATE, SHOTS_HISTORY, SHOTS_TAKEN_AT and SHOTS_OUT.
// The gateway is a STUB: /ws sends hello + the snapshot and answers nothing; /api/history reads the copy with the
// gateway's own query. Every non-GET request and every host but the public map/radar/forecast services is refused,
// so nothing can reach a device. Times in the snapshot and the history are moved to "now", so nothing reads stale.
import { execFileSync } from "node:child_process";
import { mkdirSync, readFileSync } from "node:fs";
import { test } from "@playwright/test";

const env = (k: string) => {
  const v = process.env[k];
  if (!v) throw new Error(`${k} is not set (run harness/ui-reference/screenshots.sh)`);
  return v;
};
const STATE = JSON.parse(readFileSync(env("SHOTS_STATE"), "utf8")) as Record<string, unknown>;
const HISTORY = env("SHOTS_HISTORY");
const TAKEN_AT = Number(env("SHOTS_TAKEN_AT"));
const OUT = env("SHOTS_OUT");
const SHIFT = Date.now() / 1000 - TAKEN_AT;
// the public services the Weather page fetches itself (forecast, radar index and tiles, the base map)
const PUBLIC = /^https:\/\/([a-z0-9-]+\.)*(open-meteo\.com|librewxr\.net|openstreetmap\.org)\//;

/** Epoch seconds (and milliseconds) moved by SHIFT; everything else as it was. */
function shifted(v: unknown): unknown {
  if (typeof v === "number") {
    if (v > 1.7e9 && v < 1.9e9) return v + SHIFT;
    if (v > 1.7e12 && v < 1.9e12) return v + SHIFT * 1000;
    return v;
  }
  if (Array.isArray(v)) return v.map(shifted);
  if (v && typeof v === "object") return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, shifted(x)]));
  return v;
}

/** The gateway's history_get: buckets of max(30 s, since/points), [MAX(t), AVG(v)], read as of TAKEN_AT. */
function history(key: string, since: number, points: number): [number, number][] {
  if (!/^[a-z0-9_.]+$/.test(key) || /(^|\.)(lat|lon|location|gps)(\.|$)/.test(key)) return [];
  const start = TAKEN_AT - since, bucket = Math.max(30, since / Math.max(points, 1));
  const sql = `SELECT MAX(t) AS t, AVG(v) AS v FROM samples WHERE key = '${key}' AND t >= ${start} `
    + `GROUP BY CAST((t - ${start}) / ${bucket} AS INTEGER) ORDER BY 1`;
  const out = execFileSync("sqlite3", ["-readonly", "-json", HISTORY, sql], { encoding: "utf8" }).trim();
  return (out ? JSON.parse(out) as { t: number; v: number }[] : []).map(r => [r.t + SHIFT, Math.round(r.v * 100) / 100]);
}

const PAGES: [string, number][] = [["home", 5000], ["energy", 7000], ["climate", 5000], ["weather", 12000], ["network", 7000]];

for (const [view, settle] of PAGES) {
  test(`${view}.png`, async ({ page }) => {
    await page.route(() => true, r => {
      const req = r.request(), u = new URL(req.url());
      const local = u.hostname === "127.0.0.1" || u.hostname === "localhost";
      if (local && u.pathname === "/api/client-info") return r.fulfill({ json: {} });
      if (req.method() !== "GET") return r.abort();                         // no command can leave the page
      if (local && u.pathname === "/api/history") {
        const keys = (u.searchParams.get("keys") || "").split(",").filter(Boolean);
        const since = Math.min(Number(u.searchParams.get("since") || 86400), 31 * 86400);
        const points = Math.min(Math.max(Number(u.searchParams.get("points") || 240), 10), 2000);
        return r.fulfill({ json: Object.fromEntries(keys.map(k => [k, history(k, since, points)])) });
      }
      if (local && u.pathname === "/api/radar/recorded") return r.fulfill({ json: { frames: [], flows: [] } });
      if (local && u.pathname.startsWith("/api/")) return r.fulfill({ status: 404, body: "" });
      if (local || PUBLIC.test(req.url())) return r.continue();
      return r.abort();
    });
    await page.routeWebSocket(/\/ws$/, ws => {
      ws.onMessage(() => {});                                             // commands are dropped, never answered
      ws.send(JSON.stringify({ type: "hello", control: true, who: "touchscreen" }));
      ws.send(JSON.stringify({ type: "snapshot", data: shifted(STATE) }));
    });
    // the weather map dark, as in daylight it would follow Auto (light): the README shows it with the dark UI
    await page.addInitScript(() => { try { localStorage.setItem("vp.wxmap.mode", "dark"); } catch { /* none */ } });
    await page.goto(`/#${view}`);
    await page.locator(`.view.on[data-view=${view}]`).waitFor();
    await page.evaluate(() => document.fonts.ready);
    await page.waitForTimeout(settle);                                    // charts laid out, 3D scenes and tiles drawn
    mkdirSync(OUT, { recursive: true });
    await page.screenshot({ path: `${OUT}/${view}.png` });
  });
}
