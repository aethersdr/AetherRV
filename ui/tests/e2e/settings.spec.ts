// Settings page against a STUB gateway and a stubbed /api/settings (page.route; nothing reaches a backend, the admin
// helper or a device): every tab renders, system actions and Wi-Fi forget ask first, a Wi-Fi password goes only into
// its one request, view-only changes nothing, Starlink commands go through the WebSocket. All values are invented.
import { expect, test, type Page, type Request, type WebSocketRoute } from "@playwright/test";

interface Sent { type: string; device?: string; cmd?: string; value?: unknown; id?: number }
interface Action { verb: string; args: Record<string, unknown> }

const NOW = Math.floor(Date.now() / 1000);
const STATE: Record<string, unknown> = {
  "victron.controls_ok": true, "climate.rixens.controls_ok": true, "settings.timezone": "Etc/Test_Zone", "settings.screen_off_min": 10,
  "starlink.controls_ok": true, "starlink.reachable": true, "starlink.heating": "auto", "starlink.update_state": "REBOOT_REQUIRED",
  "starlink.software": "test-sw-1.0", "starlink.sleep": { enabled: false, start_utc_min: 0, duration_min: 0 }, "starlink.power_w": 31,
  "starlink.events": [{ t: NOW - 120, dur_s: 2.5, what: "Test outage", outage: true }, { t: NOW - 600, dur_s: 0, what: "Test notice", sev: "warning" }],
  "starlink.events_span_s": 7200,
  "climate.rixens.live": true, "climate.rixens.state": "idle", "climate.rixens.version": "TEST-MCS-1.0", "climate.rixens.battery_v": 12.9,
  "climate.rixens.runtime_h": 1234.4, "climate.rixens.available": { electric: true, engine: false, preheat: true },
  "climate.rixens.thermostat": true, "climate.rixens.setpoint_c": 20, "climate.rixens.fan_speed": "auto", "climate.rixens.fan_auto_pct": 30,
  "climate.rixens.fault_history": ["T-01 test fault"],
  "climate.espar.heard": true, "climate.espar.state": "off", "climate.espar.asleep": false, "espar.controls_ok": true,
  "climate.espar.diag.enabled": true, "climate.espar.diag.software": "TEST-ESP-2", "climate.espar.diag.voltage_v": 12.6,
  "climate.espar.diag.hours": { operating: 321, p0_25: 10, p25_50: 20, p50_75: 30, p75_100: 40, pause: 5 },
  "climate.espar.diag.dtcs": [{ code: "T99", text: "Invented stored fault", active: false }],
};
const INFO = {
  version: "test-build-7", prefs: { screen_off_min: 10 },
  admin: { hostname: "test-console", timezone: "Etc/Test_Zone", kernel: "0.0.0-test", uptime_s: 93784,
    interfaces: [{ name: "end0", state: "UP", ipv4: ["10.99.0.5"] }, { name: "wlan0", state: "UP", ipv4: ["10.99.1.7"] }],
    disk: { total: 64e9, free: 21.5e9 } },
  wifi: { present: true, device: "wlan0", powered: true, scanning: false, connected: "TestNet-Home",
    networks: [{ ssid: "TestNet-Home", security: "psk", signal_dbm: -48, known: true, connected: true },
      { ssid: "TestNet-Open", security: "open", signal_dbm: -71, known: false, connected: false },
      { ssid: "TestNet-Locked", security: "psk", signal_dbm: -79, known: false, connected: false }],
    known: [{ ssid: "TestNet-Home", security: "psk" }, { ssid: "TestNet-Away", security: "psk" }] },
  tailscale: { installed: true, running: true, state: "Running", version: "0.0.1", name: "test-console", dns: "test-console.example.ts.net",
    ips: ["100.64.0.9"], online: true, tailnet: "example.test", user: "tester@example.test" },
  update: { state: { status: "done", restart: "suggested" }, log: ["test: line one", "test: line two"],
    check: { updates: [{ name: "pkg-alpha", old: "1.0", new: "1.1" }], held: [{ name: "linux-test", old: "6.0", new: "6.1" }], checked: NOW - 3600 },
    last: { finished: NOW - 7200, status: "done" } },
};

async function stub(page: Page, opts: { control?: boolean; info?: object; delayInfo?: number; action?: (a: Action) => object } = {}) {
  const sent: Sent[] = [], actions: Action[] = [];
  let gets = 0;
  let sock: WebSocketRoute | null = null;
  await page.route("http://127.0.0.1:8091/**", r => r.abort());
  await page.route("**/api/**", r => r.fulfill({ json: {} }));               // anything else (history): empty
  await page.route("**/api/settings", async r => {
    gets++;
    if (opts.delayInfo) await new Promise(res => setTimeout(res, opts.delayInfo));
    await r.fulfill({ json: opts.info ?? INFO });
  });
  await page.route("**/api/settings/action", r => {
    const a = JSON.parse((r.request() as Request).postData() || "{}") as Action;
    actions.push(a);
    return r.fulfill({ json: opts.action ? opts.action(a) : { ok: true } });
  });
  await page.routeWebSocket(/\/ws$/, ws => {
    sock = ws;
    ws.onMessage(raw => {
      const m = JSON.parse(String(raw)) as Sent;
      sent.push(m);
      if (m.type === "control") setTimeout(() => ws.send(JSON.stringify({ type: "control_result", id: m.id, ok: true, result: "ok", detail: "done (verified)" })), 150);
    });
    ws.send(JSON.stringify({ type: "hello", control: opts.control ?? true, who: "touchscreen" }));
    ws.send(JSON.stringify({ type: "snapshot", data: STATE }));
  });
  return { sent, actions, gets: () => gets, delta: (d: object) => sock?.send(JSON.stringify({ type: "delta", data: d })) };
}
const controls = (sent: Sent[]) => sent.filter(m => m.type === "control");
const tab = (page: Page, t: string) => page.click(`#set-tabs [data-t=${t}]`);

test("device tab: helper status, preferences, and system actions only after a confirm", async ({ page }) => {
  const s = await stub(page);
  await page.goto("/#settings");
  await expect(page.locator("#set-body h2").first()).toHaveText("Device & display");
  await expect(page.locator("#set-body")).toContainText("test-console");
  await expect(page.locator("#set-body")).toContainText("Etc/Test Zone");
  await expect(page.locator(".set-about")).toContainText("test-build-7");
  await expect(page.locator(".set-about")).toContainText("1 d 2 h");
  await expect(page.locator(".set-about")).toContainText("21.5 GB of 64.0 GB");
  await expect(page.locator(".set-about")).toContainText("end0 10.99.0.5 · wlan0 10.99.1.7");
  await expect(page.locator('[data-off="10"]')).toHaveClass(/\bon\b/);
  for (const [act, title, verb] of [["reboot", "Reboot AetherRV?", "reboot"], ["poweroff", "Shut down AetherRV?", "poweroff"],
    ["restart_kiosk", "Restart the display?", "restart_kiosk"]] as const) {
    await page.click(`.set-actions [data-act=${act}]`);
    await expect(page.locator("#modal-t")).toHaveText(title);
    await page.waitForTimeout(150);
    expect(s.actions).toHaveLength(0);
    await page.click("#modal-no");
    await expect(page.locator("#modal")).toBeHidden();
    expect(s.actions).toHaveLength(0);
    await page.click(`.set-actions [data-act=${act}]`);
    await page.click("#modal-yes");
    await expect.poll(() => s.actions.map(a => a.verb)).toEqual([verb]);
    s.actions.length = 0;
  }
  await page.click('[data-off="30"]');                                         // no confirm (as app.js)
  await expect.poll(() => s.actions).toEqual([{ verb: "pref", args: { key: "screen_off_min", value: 30 } }]);
});

test("an action reloads /api/settings (aetherrv:settings-changed)", async ({ page }) => {
  const s = await stub(page);
  await page.goto("/#settings");
  await expect(page.locator("#set-body")).toContainText("test-console");
  const before = s.gets();
  await page.click('[data-off="5"]');
  await expect.poll(() => s.gets()).toBeGreaterThan(before);
});

test("skeleton until /api/settings answers; the live-state tabs don't wait for it", async ({ page }) => {
  await stub(page, { delayInfo: 2500 });
  await page.goto("/#settings");
  await expect(page.locator(".set-skel")).toBeVisible();
  await tab(page, "mcs7");
  await expect(page.locator("#set-body")).toContainText("TEST-MCS-1.0");
  await tab(page, "device");
  await expect(page.locator(".set-skel")).toBeVisible();
  await expect(page.locator("#set-body")).toContainText("test-console", { timeout: 5000 });
  await expect(page.locator(".set-skel")).toHaveCount(0);
});

test("network: forget asks first; a known network joins without a password", async ({ page }) => {
  const s = await stub(page);
  await page.goto("/#settings");
  await tab(page, "network");
  await expect(page.locator(".set-row").first()).toContainText("Wired · end0");
  await expect(page.locator(".wnet")).toHaveCount(4);                           // 3 in range + 1 saved out of range
  await expect(page.locator(".wnet.conn")).toContainText("TestNet-Home");
  await page.click('[data-wforget="TestNet-Away"]');
  await expect(page.locator("#modal-t")).toHaveText("Forget TestNet-Away?");
  expect(s.actions).toHaveLength(0);
  await page.click("#modal-yes");
  await expect.poll(() => s.actions).toEqual([{ verb: "wifi_forget", args: { ssid: "TestNet-Away" } }]);
  await page.click('[data-wjoin="TestNet-Open"]');
  await expect.poll(() => s.actions.length).toBe(2);
  expect(s.actions[1]).toEqual({ verb: "wifi_connect", args: { ssid: "TestNet-Open" } });
  await expect(page.locator("#osk")).toHaveCount(0);
});

test("network: Wi-Fi off asks first (Cancel sends nothing, Confirm sends one request); on is one tap", async ({ page }) => {
  const s = await stub(page);
  await page.goto("/#settings");
  await tab(page, "network");
  await page.click('[data-wpow="0"]');
  await expect(page.locator("#modal-t")).toHaveText("Turn Wi-Fi off?");
  await expect(page.locator("#modal-d")).toHaveText("Remote access over Wi-Fi (Tailscale) will drop until Wi-Fi is turned back on from this screen.");
  await page.waitForTimeout(200);
  expect(s.actions).toHaveLength(0);
  await page.click("#modal-no");
  await expect(page.locator("#modal")).toBeHidden();
  await page.waitForTimeout(300);
  expect(s.actions).toHaveLength(0);
  await page.click('[data-wpow="0"]');
  await page.click("#modal-yes");
  await expect.poll(() => s.actions).toEqual([{ verb: "wifi_power", args: { on: false } }]);
  await page.waitForTimeout(300);
  expect(s.actions).toHaveLength(1);
  await page.click('[data-wpow="1"]');                                         // Wi-Fi on: one tap
  await expect(page.locator("#modal")).toBeHidden();
  await expect.poll(() => s.actions.length).toBe(2);
  expect(s.actions[1]).toEqual({ verb: "wifi_power", args: { on: true } });
});

test("network: a new secured network asks for its password; it goes ONLY into that one request", async ({ page }) => {
  const s = await stub(page);
  const logs: string[] = [];
  page.on("console", m => logs.push(m.text()));
  await page.goto("/#settings");
  await tab(page, "network");
  await page.click('[data-wjoin="TestNet-Locked"]');
  await expect(page.locator("#osk")).toBeVisible();
  await expect(page.locator("#osk-title")).toHaveText("Password for TestNet-Locked");
  await expect(page.locator('#osk-keys [data-a="done"]')).toBeDisabled();       // 8 characters minimum
  const secret = "zq7xw3kv";
  for (const ch of secret) await page.locator(`#osk-keys [data-k="${ch}"]`).dispatchEvent("pointerdown");
  await expect(page.locator("#osk-text")).toHaveText("•".repeat(secret.length));  // shown as dots
  expect(await page.evaluate(s2 => document.body.innerText.includes(s2), secret)).toBe(false);
  expect(s.actions).toHaveLength(0);
  await page.click('#osk-keys [data-a="done"]');
  await expect(page.locator("#osk")).toHaveCount(0);
  await expect.poll(() => s.actions).toEqual([{ verb: "wifi_connect", args: { ssid: "TestNet-Locked", passphrase: secret } }]);
  await expect(page.locator("#toast")).toContainText("Joining TestNet-Locked");
  const leaked = await page.evaluate(s2 => {
    const inStorage = (st: Storage) => Object.keys(st).some(k => (st.getItem(k) ?? "").includes(s2));
    return document.documentElement.outerHTML.includes(s2) || inStorage(localStorage) || inStorage(sessionStorage);
  }, secret);
  expect(leaked).toBe(false);
  expect(logs.some(l => l.includes(secret))).toBe(false);
});

test("network: Cancel on the keyboard sends nothing", async ({ page }) => {
  const s = await stub(page);
  await page.goto("/#settings");
  await tab(page, "network");
  await page.click('[data-wjoin="TestNet-Locked"]');
  for (const ch of "abcdefgh") await page.locator(`#osk-keys [data-k="${ch}"]`).dispatchEvent("pointerdown");
  await page.click("#osk-cancel");
  await expect(page.locator("#osk")).toHaveCount(0);
  await page.waitForTimeout(300);
  expect(s.actions).toHaveLength(0);
});

test("device: rename on the keyboard (pattern-checked), sent without a confirm", async ({ page }) => {
  const s = await stub(page);
  await page.goto("/#settings");
  await page.click('[data-act="rename"]');
  await expect(page.locator("#osk-text")).toHaveText("test-console");           // not a password: shown
  await page.locator('#osk-keys [data-a="bs"]').dispatchEvent("pointerdown");
  await page.locator('#osk-keys [data-k="2"]').dispatchEvent("pointerdown");
  await expect(page.locator("#osk-text")).toHaveText("test-consol2");
  await page.click('#osk-keys [data-a="done"]');
  await expect.poll(() => s.actions).toEqual([{ verb: "set_hostname", args: { name: "test-consol2" } }]);
});

test("tailscale: sign-out asks first", async ({ page }) => {
  const s = await stub(page);
  await page.goto("/#settings");
  await tab(page, "tailscale");
  await expect(page.locator("#set-body")).toContainText("test-console.example.ts.net");
  await page.click('[data-ts="logout"]');
  await expect(page.locator("#modal-t")).toHaveText("Sign out of Tailscale?");
  expect(s.actions).toHaveLength(0);
  await page.click("#modal-yes");
  await expect.poll(() => s.actions.map(a => a.verb)).toEqual(["tailscale_logout"]);
});

test("tailscale: sign-in shows the QR code for the returned link", async ({ page }) => {
  const s = await stub(page, {
    info: { ...INFO, tailscale: { installed: true, running: true, state: "NeedsLogin", version: "0.0.1" } },
    action: a => (a.verb === "tailscale_login" ? { ok: true, state: "NeedsLogin", auth_url: "https://login.example.test/a/xyz" } : { ok: true }),
  });
  await page.goto("/#settings");
  await tab(page, "tailscale");
  await page.click('[data-ts="login"]');
  await expect.poll(() => s.actions.map(a => a.verb)).toEqual(["tailscale_login"]);
  await expect(page.locator("#ts-qr svg")).toBeVisible();
  await expect(page.locator(".ts-url")).toHaveText("https://login.example.test/a/xyz");
});

test("updates: install asks first, check doesn't", async ({ page }) => {
  const s = await stub(page);
  await page.goto("/#settings");
  await tab(page, "updates");
  await expect(page.locator(".upks").first()).toContainText("pkg-alpha");
  await expect(page.locator(".upks.held")).toContainText("linux-test");
  await expect(page.locator("#uplog")).toContainText("test: line two");
  await page.click('[data-up="start"]');
  await expect(page.locator("#modal-t")).toHaveText("Install system updates?");
  expect(s.actions).toHaveLength(0);
  await page.click("#modal-yes");
  await expect.poll(() => s.actions.map(a => a.verb)).toEqual(["update_start"]);
  await page.click('[data-up="check"]');
  await expect.poll(() => s.actions.map(a => a.verb)).toEqual(["update_start", "update_check"]);
  await page.click('#set-body [data-act="reboot"]');                           // "Reboot now" (restart suggested)
  await expect(page.locator("#modal-t")).toHaveText("Reboot AetherRV?");
  await page.click("#modal-no");
});

test("starlink: reboot and install ask first; heating and the sleep schedule go straight to the dish", async ({ page }) => {
  const s = await stub(page);
  await page.goto("/#settings");
  await tab(page, "starlink");
  await expect(page.locator("#set-body")).toContainText("test-sw-1.0");
  await expect(page.locator(".sl-ev")).toHaveCount(2);
  await page.click('[data-sl="reboot"]');
  await expect(page.locator("#modal-t")).toHaveText("Reboot the Starlink dish?");
  await page.waitForTimeout(200);
  expect(controls(s.sent)).toHaveLength(0);
  await page.click("#modal-no");
  await page.click('[data-sl="install"]');
  await expect(page.locator("#modal-t")).toHaveText("Install the Starlink update?");
  await page.click("#modal-yes");
  await expect.poll(() => controls(s.sent).map(m => [m.device, m.cmd, m.value])).toEqual([["starlink", "install_update", true]]);
  await page.waitForTimeout(400);
  await page.click('[data-slheat="on"]');
  await expect.poll(() => controls(s.sent).length).toBe(2);
  expect(controls(s.sent)[1]).toMatchObject({ device: "starlink", cmd: "set_heating", value: "on" });
  await page.waitForTimeout(400);
  await page.click('[data-slpen="1"]');
  await expect(page.locator('[data-slp="start"]')).toBeVisible();
  await expect(page.locator('[data-slsleep="save"]')).toBeVisible();
  expect(controls(s.sent)).toHaveLength(2);                                    // nothing until Save
  await page.click('[data-slsleep="save"]');
  await expect.poll(() => controls(s.sent).length).toBe(3);
  const v = controls(s.sent)[2]!.value as { enabled: boolean; duration_min: number; start_utc_min: number };
  expect(v.enabled).toBe(true);
  expect(v.duration_min).toBe(480);                                            // 23:00 → 07:00
  expect(v.start_utc_min).toBe(((1380 + new Date().getTimezoneOffset()) % 1440 + 1440) % 1440);
});

test("starlink: an unsaved sleep schedule survives switching tabs (as app.js)", async ({ page }) => {
  const s = await stub(page);
  await page.goto("/#settings");
  await tab(page, "starlink");
  await page.click('[data-slpen="1"]');
  await page.selectOption('[data-slp="end"]', "360");
  await tab(page, "device");
  await tab(page, "starlink");
  await expect(page.locator('[data-slsleep="save"]')).toBeVisible();
  await expect(page.locator('[data-slp="end"]')).toHaveValue("360");
  await page.click('[data-slsleep="cancel"]');
  await expect(page.locator('[data-slsleep="save"]')).toHaveCount(0);
  expect(controls(s.sent)).toHaveLength(0);
});

test("heater tabs show the read-only details", async ({ page }) => {
  await stub(page);
  await page.goto("/#settings");
  await tab(page, "mcs7");
  await expect(page.locator("#set-body")).toContainText("TEST-MCS-1.0");
  await expect(page.locator("#set-body")).toContainText("1,234 h");
  await expect(page.locator("#set-body")).toContainText("Electric, Preheat");
  await expect(page.locator("#set-body")).toContainText("on · 68°");
  await expect(page.locator(".upk")).toContainText("T-01 test fault");
  await tab(page, "espar");
  await expect(page.locator("#set-body")).toContainText("TEST-ESP-2");
  await expect(page.locator("#set-body")).toContainText("321 h");
  await expect(page.locator("#set-body")).toContainText("Invented stored fault");
  await expect(page.locator("#set-body")).toContainText("stored only");
});

test("view only: the note shows, every change is disabled and nothing is sent or posted", async ({ page }) => {
  const s = await stub(page, { control: false });
  await page.goto("/#settings");
  await expect(page.locator(".set-ro")).toContainText("View only");
  for (const sel of ['[data-act="rename"]', '[data-act="reboot"]', '[data-act="poweroff"]', '[data-act="restart_kiosk"]', '[data-off="5"]'])
    await expect(page.locator(sel)).toBeDisabled();
  await expect(page.locator('[data-unit="C"]')).toBeEnabled();                 // this screen's own preferences
  await expect(page.locator('[data-night="1"]')).toBeEnabled();
  await page.locator('[data-act="reboot"]').click({ force: true });
  await tab(page, "network");
  for (const sel of ['[data-wforget="TestNet-Home"]', '[data-wjoin="TestNet-Locked"]', '[data-wpow="0"]', "[data-wscan]"])
    await expect(page.locator(sel)).toBeDisabled();
  await page.locator('[data-wjoin="TestNet-Locked"]').click({ force: true });
  await tab(page, "starlink");
  await expect(page.locator("#set-body .set-h .muted").first()).toHaveText("view only");
  for (const sel of ['[data-sl="reboot"]', '[data-sl="install"]', '[data-slheat="off"]', '[data-slpen="1"]']) await expect(page.locator(sel)).toBeDisabled();
  await tab(page, "updates");
  await expect(page.locator('[data-up="start"]')).toBeDisabled();
  await expect(page.locator("#modal")).toBeHidden();
  await expect(page.locator("#osk")).toHaveCount(0);
  await page.waitForTimeout(300);
  expect(s.actions).toHaveLength(0);
  expect(controls(s.sent)).toHaveLength(0);
});

test("night mode from Settings switches the whole UI", async ({ page }) => {
  await stub(page);
  await page.goto("/#settings");
  await page.click('[data-night="1"]');
  await expect(page.locator("html")).toHaveClass(/\bnight\b/);
  await expect(page.locator('[data-night="1"]')).toHaveClass(/\bon\b/);
  await page.click('[data-night="0"]');
  await expect(page.locator("html")).not.toHaveClass(/\bnight\b/);
});

test("device tab: the Pi load chart (CPU, GPU, memory; SoC temperature in its own lane), live values in the legend", async ({ page }) => {
  const gw = await stub(page);
  const now = Math.floor(Date.now() / 1000);
  const ts = Array.from({ length: 360 }, (_, i) => now - 86400 + (i + 1) * 240);
  const wave = (base: number, amp: number) => ts.map((t, i) => [t, +(base + amp * Math.sin(i / 20)).toFixed(1)]);
  const hist: string[] = [];
  await page.route("**/api/history**", r => {
    hist.push(r.request().url());
    return r.fulfill({ json: { "pi.cpu_pct": wave(18, 10), "pi.gpu_pct": wave(4, 3), "pi.mem_pct": wave(7, 0.5), "pi.soc_c": wave(48, 5) } });
  });
  await page.goto("/#settings");
  await tab(page, "device");
  await expect(page.locator("#chart-pi path[stroke]")).toHaveCount(3);       // CPU, GPU, memory on one % axis
  await expect(page.locator("#chart-pi-temp path[stroke]")).toHaveCount(1);  // the SoC temperature on its own scale
  expect(hist.some(u => u.includes("pi.cpu_pct") && u.includes("pi.soc_c"))).toBe(true);
  gw.delta({ "pi.cpu_pct": 12.3, "pi.gpu_pct": 0.4, "pi.mem_pct": 6.8, "pi.soc_c": 40 });
  await expect(page.locator("#legend-pi")).toHaveText(/CPU12%.*GPU0%.*Memory7%.*SoC temperature104°/);
  await page.locator("#pi-chart").scrollIntoViewIfNeeded();
  await page.locator("#pi-chart").screenshot({ path: "../harness/out/ui-pages/react-settings-pi-chart.png" });
  // a Pi without GPU counters: the GPU series and its legend entry are left out
  gw.delta({ "pi.gpu_pct": null });
  await page.unroute("**/api/history**");
  await page.route("**/api/history**", r => r.fulfill({ json: { "pi.cpu_pct": wave(18, 10), "pi.mem_pct": wave(7, 0.5), "pi.soc_c": wave(48, 5) } }));
  await page.click("#range-pi [data-s='21600']");
  await expect(page.locator("#chart-pi path[stroke]")).toHaveCount(2);
  await expect(page.locator('#legend-pi [data-k="pi.gpu_pct"]')).toHaveCount(0);
});
