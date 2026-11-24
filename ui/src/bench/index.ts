// UI runtime benchmark instrumentation (harness/uibench, docs/UI-RUNTIME-BENCHMARK.md). Loaded only when the URL
// has ?bench=1 (a dynamic import in main.tsx, so it is its own chunk and the production page never runs it).
//
// Passive unless the runner asks: it never touches the DOM or the store on its own. The runner's long-poll commands
// ("raf", "presscost", "screen off|on", below) are the only exceptions, and none sends a device command. After each
// page change it tells the bench runner (on
// 127.0.0.1:<bench_port>, by navigator.sendBeacon: no CORS preflight) where the rail, the view area and a few
// press targets are, plus the page's boot marks, Event Timing and long-task entries where the engine has them, and
// the WebGL renderer. With &raf=1 it also reports requestAnimationFrame intervals; and the runner can ask for a
// rAF sample over a long poll (GET /cmd: "raf <ms>"), which is how a page's frame rate is read while nothing at all
// is captured. These are cross-checks and geometry only: every time that counts is measured from the compositor's
// frames.
//
// Press targets are chosen so that pressing them sends nothing: the runner presses, holds and slides off (no click),
// so only pointerdown side effects matter, and controls with any (dimmer tiles, dials, sliders, drag grips, React
// onPointerDown handlers) are skipped.
import { kioskWake, SCREEN_AVAIL, SHIELD, screenOff } from "../logic/kiosk";
import { heldTile } from "../logic/switches";
import { currentView } from "../state/route";
import { K, store } from "../state/store";

const qs = new URLSearchParams(location.search);
const PORT = qs.get("bench_port") || "18099";

// Press-feedback experiments (benchmark only), any of them comma-separated in &press=:
//   promote   every press target on its own compositing layer (will-change)
//   nofilter  the pressed state without its brightness filter (keeps the scale)
//   noscale   the pressed state without its scale (keeps the filter)
//   bg        neither: the pressed state is a background tint only (the cheapest repaint of the control)
//   rest      an identity transform at rest on every pressable control (zero specificity), so a press changes a
//             transform the element already has instead of giving it a new one (in WebKit a new transform makes
//             a new render layer, which needs layout)
//   restfx    the same with an identity filter too
const PRESSABLE = "button, [role=button], .pressable";
const PRESS_EXP: Record<string, string> = {
  promote: ".sw, .quick-btn, .chip, .btn-soft, .seg button, #nav button, #nav-foot button, .tabs button { will-change: transform; }",
  nofilter: ".pressed { filter: none !important; }",
  noscale: ".pressed { transform: none !important; }",
  bg: ".pressed { transform: none !important; filter: none !important; background-color: rgba(255, 255, 255, .14) !important; }",
  rest: `:where(${PRESSABLE}) { transform: scale(1); }`,
  restfx: `:where(${PRESSABLE}) { transform: scale(1); filter: brightness(1); }`,
};
for (const k of (qs.get("press") || "").split(",")) {
  if (!PRESS_EXP[k]) continue;
  const st = document.createElement("style");
  st.textContent = PRESS_EXP[k];
  document.head.appendChild(st);
}
const URL_ = `http://127.0.0.1:${PORT}/report`;
const RAF = qs.get("raf") === "1";

const send = (kind: string, body: Record<string, unknown>) => {
  try { navigator.sendBeacon(URL_, JSON.stringify({ kind, ui: "react", t_epoch: performance.timeOrigin + performance.now(), ...body })); }
  catch { /* runner not listening */ }
};

const rect = (el: Element | null) => {
  if (!el) return null;
  const r = el.getBoundingClientRect();
  return r.width > 0 && r.height > 0 ? [Math.round(r.left), Math.round(r.top), Math.round(r.width), Math.round(r.height)] : null;
};

// per page, in order of preference; anything else falls back to the page's first safe button
// Only these are ever pressed (UI-local controls: chart ranges, segmented toggles, tabs, map zoom; switch tiles on
// Switches, pressed without a click). A page where none matches gets no press measurement.
const PREFER: Record<string, string[]> = {
  home: [".quick-btn[data-sw]", ".quick-btn[data-i]", "#hub"],
  energy: [".seg button", ".range button"],
  climate: ["#range-temp button", ".seg button"],
  weather: [".wx-zoom button", ".wx-menu-btn", ".seg button"],
  switches: [".sw[data-sw]"],
  network: [".seg button", ".range button"],
  alerts: ["#btn-sound", "#btn-ackall", ".btn-soft[data-ack]"],
  systems: [".seg button", ".tabs button"],
  settings: [".tabs button", ".seg button"],
};
// heater controls are never pressed (not even without a click), nor anything with drag semantics
const UNSAFE = ".hq-btn, .home-sp, .hsp-btn, .ctl-btn, .ctl-pill, .src-btn, .dial-wrap, .vslider, #fan-slider, .wx-tl-track, "
  + ".grip, #osk-keys, input, select, textarea, [data-no-bench]";

function reactProps(el: Element): Record<string, unknown> | null {
  const k = Object.keys(el).find(x => x.startsWith("__reactProps$"));
  return k ? (el as unknown as Record<string, Record<string, unknown>>)[k] ?? null : null;
}
const POINTER = ["onPointerDown", "onPointerDownCapture", "onTouchStart", "onMouseDown"];
function safe(el: Element): boolean {
  if ((el as HTMLButtonElement).disabled || el.getAttribute("aria-disabled") === "true") return false;
  if (el.closest(UNSAFE)) return false;
  // a pointerdown handler on any ancestor inside the page (a dial, a drag area) would act on the press itself
  for (let a = el.parentElement; a && !a.matches("section.view"); a = a.parentElement) {
    const p = reactProps(a);
    if (p && POINTER.some(k => p[k])) return false;
  }
  const p = reactProps(el);
  if (p && POINTER.some(k => p[k])) {
    const id = (el as HTMLElement).dataset?.sw;
    if (!(id && !heldTile(id))) return false;           // a switch tile's pointerdown only acts for dimmers
  }
  const sw = (el as HTMLElement).dataset?.sw;
  if (sw && heldTile(sw)) return false;                  // dimmer: pressing holds the virtual switch
  const r = el.getBoundingClientRect();
  return r.width >= 24 && r.height >= 24 && r.top >= 0 && r.left >= 0 && r.bottom <= innerHeight && r.right <= innerWidth;
}

function targetEls(view: string) {
  const sec = document.querySelector(`section.view[data-view="${view}"]`);
  if (!sec) return [];
  const out: { el: Element; sel: string; rect: number[] }[] = [];
  const seen = new Set<Element>();
  for (const sel of PREFER[view] ?? []) {          // preferred controls only: no generic fallback
    for (const el of sec.querySelectorAll(sel)) {
      if (seen.has(el) || !safe(el)) continue;
      seen.add(el);
      const r = rect(el);
      if (r) out.push({ el, sel, rect: r });
      if (out.length >= 3) return out;
    }
  }
  return out;
}
function targets(view: string) {
  return targetEls(view).map(({ el, sel, rect }) => ({ name: (el.id || (el as HTMLElement).dataset?.sw || el.textContent || sel).trim().slice(0, 40), sel, rect }));
}

/** The page's animated scene, if any: its largest visible canvas, else its largest svg (energy flow, dials). */
function scene(view: string) {
  const sec = document.querySelector(`section.view[data-view="${view}"]`);
  if (!sec) return null;
  for (const sel of ["canvas", "svg"]) {
    let best: { sel: string; rect: number[] } | null = null;
    for (const el of sec.querySelectorAll(sel)) {
      const r = rect(el);
      const area = r ? r[2]! * r[3]! : 0;
      if (r && r[2]! >= 160 && r[3]! >= 120 && (!best || area > best.rect[2]! * best.rect[3]!)) best = { sel, rect: r };
    }
    if (best) return best;
  }
  return null;
}

/** A control the runner presses to start the page's scene (Weather: the radar loop plays only on demand). */
const STIM: Record<string, string> = { weather: "#wx-play" };
function stim(view: string) {
  const sel = STIM[view];
  const r = sel ? rect(document.querySelector(`section.view[data-view="${view}"] ${sel}`)) : null;
  return r ? { sel, rect: r } : null;
}

function marks() {
  const m: Record<string, number> = {};
  for (const e of performance.getEntriesByType("mark")) if (e.name.startsWith("aetherrv:")) m[e.name] = performance.timeOrigin + e.startTime;
  for (const e of performance.getEntriesByType("paint")) m[e.name] = performance.timeOrigin + e.startTime;
  return m;
}

let gl: string | null = null;
function webgl(): string | null {
  if (gl !== null) return gl;
  try {
    const c = document.createElement("canvas");
    const ctx = (c.getContext("webgl2") || c.getContext("webgl")) as WebGLRenderingContext | null;
    if (!ctx) return (gl = "none");
    const ext = ctx.getExtension("WEBGL_debug_renderer_info");
    gl = `${ctx instanceof WebGL2RenderingContext ? "webgl2" : "webgl1"}: ${ext ? ctx.getParameter(ext.UNMASKED_RENDERER_WEBGL) : ctx.getParameter(ctx.RENDERER)}`;
    ctx.getExtension("WEBGL_lose_context")?.loseContext();
  } catch (e) { gl = `error: ${e}`; }
  return gl;
}

const events: unknown[] = [];
const longtasks: unknown[] = [];
const observe = (type: string, sink: unknown[], map: (e: PerformanceEntry) => unknown, opts: Record<string, unknown> = {}) => {
  try {
    if (!PerformanceObserver.supportedEntryTypes?.includes(type)) return false;
    new PerformanceObserver(l => { for (const e of l.getEntries()) if (sink.length < 500) sink.push(map(e)); }).observe({ type, buffered: true, ...opts } as PerformanceObserverInit);
    return true;
  } catch { return false; }
};
const evOk = observe("event", events, e => {
  const x = e as PerformanceEventTiming;
  return { name: x.name, start: performance.timeOrigin + x.startTime, dur: x.duration, proc: x.processingEnd - x.processingStart, delay: x.processingStart - x.startTime };
}, { durationThreshold: 16 });
const ltOk = observe("longtask", longtasks, e => ({ start: performance.timeOrigin + e.startTime, dur: e.duration }));

let rafIv: number[] = [];
if (RAF) {
  let last = performance.now();
  const tick = (t: number) => { rafIv.push(t - last); last = t; requestAnimationFrame(tick); };
  requestAnimationFrame(tick);
}

function report(why: string) {
  const view = currentView();
  const rail: Record<string, number[] | null> = {};
  for (const b of document.querySelectorAll<HTMLElement>(".rail button[data-v]")) rail[b.dataset.v!] = rect(b);
  const iv = rafIv; rafIv = [];
  send("page", {
    why, view, viewport: [innerWidth, innerHeight], dpr: devicePixelRatio, ua: navigator.userAgent, webgl: webgl(),
    conn: store.get(K.conn), ready: store.get(K.ready) === true,
    screen: { available: store.get(SCREEN_AVAIL) === true, shield: store.get(SHIELD) === true },
    rail, views: rect(document.getElementById("views")), topbar: rect(document.querySelector(".topbar")),
    targets: targets(view), scene: scene(view), stim: stim(view), marks: marks(),
    event_timing: evOk ? events.splice(0) : "unsupported", longtasks: ltOk ? longtasks.splice(0) : "unsupported",
    raf: RAF ? { n: iv.length, iv } : undefined,
  });
}

let last = "";
let timer: ReturnType<typeof setTimeout> | undefined;
setInterval(() => {
  const v = currentView();
  if (v !== last) { last = v; clearTimeout(timer); timer = setTimeout(() => report("view"), 800); }
}, 250);
store.subscribe(K.ready, () => { if (store.get(K.ready) === true) setTimeout(() => report("ready"), 800); });
if (RAF) setInterval(() => report("raf"), 5000);
window.addEventListener("hashchange", () => setTimeout(() => report("hash"), 300));

// every pointerdown: when the page's handler ran and the event's own timestamp, reported after the frame (the
// runner splits a press's latency into delivery, injection → handler, and render, handler → frame)
document.addEventListener("pointerdown", e => {
  const handler = performance.timeOrigin + performance.now(), event = performance.timeOrigin + e.timeStamp;
  setTimeout(() => send("input", { type: "pointerdown", handler_epoch: handler, event_epoch: event, ptype: e.pointerType }), 400);
}, { capture: true, passive: true });

// Weather's loop state, so the runner knows its Play tap started the radar (it can't with no frames loaded)
const wxObs = new MutationObserver(() => {
  const tl = document.getElementById("wx-tl");
  if (tl) send("stim", { view: "weather", playing: tl.classList.contains("playing"), frames: document.querySelectorAll("#wx-tl-ticks i").length });
});
const wxWatch = setInterval(() => {
  const tl = document.getElementById("wx-tl");
  if (tl) { wxObs.observe(tl, { attributes: true, attributeFilter: ["class"] }); clearInterval(wxWatch); }
}, 1000);

// the runner's commands (long poll): "raf <ms>" samples requestAnimationFrame for that long and reports the intervals
function sampleRaf(ms: number) {
  const iv: number[] = [];
  let last = 0;
  const t0 = performance.now();
  const tick = (t: number) => {
    if (last) iv.push(Math.round((t - last) * 100) / 100);
    last = t;
    if (t - t0 < ms) requestAnimationFrame(tick);
    else send("raf", { view: currentView(), ms, n: iv.length, iv });
  };
  requestAnimationFrame(tick);
}
// "presscost <n>": what toggling .pressed on the page's first press target costs the engine, measured in the page
// (no input, nothing sent: the same class a press sets). Per trial: the style and the layout of adding the class,
// each forced synchronously (getComputedStyle, offsetHeight), timed over 10 toggles for the clock's 1 ms grain;
// and the rAF interval after adding the class inside a rAF callback, against an interval with no change (a
// rendering update that overruns the frame shows as a doubled interval).
const raf = () => new Promise<number>(r => requestAnimationFrame(r));
async function pressCost(n: number) {
  const view = currentView();
  const tg = targetEls(view)[0];
  if (!tg) { send("presscost", { view, target: null }); return; }
  const el = tg.el as HTMLElement;
  const root = document.documentElement;
  const style: number[] = [], layout: number[] = [], frame: number[] = [], ctrl: number[] = [];
  for (let i = 0; i < n; i++) {
    await raf();
    let ts = 0, tl = 0;
    for (let k = 0; k < 10; k++) {
      const t0 = performance.now();
      el.classList.add("pressed"); void getComputedStyle(el).transform;
      const t1 = performance.now();
      void root.offsetHeight; void el.getBoundingClientRect();
      const t2 = performance.now();
      el.classList.remove("pressed"); void getComputedStyle(el).transform; void root.offsetHeight;
      ts += t1 - t0; tl += t2 - t1;
    }
    style.push(+(ts / 10).toFixed(2)); layout.push(+(tl / 10).toFixed(2));
    await raf(); await raf();
    const a = await raf(); el.classList.add("pressed"); const b = await raf(); frame.push(+(b - a).toFixed(1));
    await raf(); el.classList.remove("pressed"); await raf(); await raf();
    const c = await raf(); const d = await raf(); ctrl.push(+(d - c).toFixed(1));
    await new Promise(r => setTimeout(r, 30));
  }
  send("presscost", { view, sel: tg.sel, rect: tg.rect, n, style_ms: style, layout_ms: layout, frame_ms: frame, control_ms: ctrl,
    press: qs.get("press") || "" });
}
void (async () => {
  for (;;) {
    try {
      const c = await (await fetch(`http://127.0.0.1:${PORT}/cmd`, { cache: "no-store" })).text();
      if (c.startsWith("raf ")) sampleRaf(Number(c.slice(4)) || 5000);
      else if (c.startsWith("presscost")) void pressCost(Number(c.slice(10)) || 12);
      // the screen-off control's own path (logic/kiosk.ts): POST /screen/off on the helper, then the wake an alert
      // uses (POST /screen/on); the page reports what it sees afterwards
      else if (c === "screen off") { screenOff(); setTimeout(() => report("screen-off"), 2500); }
      else if (c === "screen on") { kioskWake(); setTimeout(() => report("screen-on"), 2500); }
      else if (c === "report") report("asked");
    } catch { await new Promise(r => setTimeout(r, 3000)); }   // runner not there
  }
})();
let rz: ReturnType<typeof setTimeout> | undefined;   // the window going fullscreen after the first report
window.addEventListener("resize", () => { clearTimeout(rz); rz = setTimeout(() => report("resize"), 500); });
