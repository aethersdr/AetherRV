// UI runtime benchmark instrumentation (harness/uibench, docs/UI-RUNTIME-BENCHMARK.md). Loaded only when the URL
// has ?bench=1 (a dynamic import in main.tsx, so it is its own chunk and the production page never runs it).
//
// Passive: it never touches the DOM or the store. After each page change it tells the bench runner (on
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
import { heldTile } from "../logic/switches";
import { currentView } from "../state/route";
import { K, store } from "../state/store";

const qs = new URLSearchParams(location.search);
const PORT = qs.get("bench_port") || "18099";

// Press-feedback experiments (benchmark only): &press=promote gives every press target its own compositing layer;
// &press=nofilter drops the brightness filter from the pressed state (keeps the scale).
const PRESS_EXP: Record<string, string> = {
  promote: ".sw, .quick-btn, .chip, .btn-soft, .seg button, #nav button, #nav-foot button, .tabs button { will-change: transform; }",
  nofilter: ".pressed { filter: none !important; }",
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

function targets(view: string) {
  const sec = document.querySelector(`section.view[data-view="${view}"]`);
  if (!sec) return [];
  const out: { name: string; sel: string; rect: number[] }[] = [];
  const seen = new Set<Element>();
  for (const sel of PREFER[view] ?? []) {          // preferred controls only: no generic fallback
    for (const el of sec.querySelectorAll(sel)) {
      if (seen.has(el) || !safe(el)) continue;
      seen.add(el);
      const r = rect(el);
      if (r) out.push({ name: (el.id || (el as HTMLElement).dataset?.sw || el.textContent || sel).trim().slice(0, 40), sel, rect: r });
      if (out.length >= 3) return out;
    }
  }
  return out;
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
void (async () => {
  for (;;) {
    try {
      const c = await (await fetch(`http://127.0.0.1:${PORT}/cmd`, { cache: "no-store" })).text();
      if (c.startsWith("raf ")) sampleRaf(Number(c.slice(4)) || 5000);
    } catch { await new Promise(r => setTimeout(r, 3000)); }   // runner not there
  }
})();
let rz: ReturnType<typeof setTimeout> | undefined;   // the window going fullscreen after the first report
window.addEventListener("resize", () => { clearTimeout(rz); rz = setTimeout(() => report("resize"), 500); });
