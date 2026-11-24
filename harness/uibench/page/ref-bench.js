// Benchmark instrumentation for the Python reference UI (web/), injected by harness/uibench/serve_ref.py when the
// URL has ?bench=1. Same reports as ui/src/bench/ (the React UI's), so the runner treats both alike: layout of the
// rail, the view area and safe press targets after every page change, the boot marks (first snapshot over the
// WebSocket, and the frame after it), Event Timing / long tasks, the WebGL renderer. It never touches the page.
(() => {
  const qs = new URLSearchParams(location.search);
  const URL_ = `http://127.0.0.1:${qs.get("bench_port") || "18099"}/report`;
  const send = (kind, body) => { try { navigator.sendBeacon(URL_, JSON.stringify({ kind, ui: "python", t_epoch: performance.timeOrigin + performance.now(), ...body })); } catch {} };

  // the first snapshot from the WebSocket = data arrived (as main.tsx marks it in the React UI)
  let ready = false, sw = [];
  const WS = window.WebSocket;
  window.WebSocket = function (...a) {
    const ws = new WS(...a);
    ws.addEventListener("message", ev => {
      if (ready) return;
      try {
        const m = JSON.parse(ev.data);
        if (m.type === "snapshot") {
          ready = true;
          sw = m.data?.["switches.list"] || [];
          performance.mark("aetherrv:snapshot");
          requestAnimationFrame(() => setTimeout(() => performance.mark("aetherrv:data-frame"), 0));
          setTimeout(() => report("ready"), 800);
        }
      } catch {}
    });
    return ws;
  };
  window.WebSocket.prototype = WS.prototype;
  Object.assign(window.WebSocket, { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 });

  const rect = el => {
    if (!el) return null;
    const r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0 ? [Math.round(r.left), Math.round(r.top), Math.round(r.width), Math.round(r.height)] : null;
  };
  const PREFER = {   // the same choices as ui/src/bench/ (UI-local controls first)
    home: [".quick-btn[data-sw]", ".quick-btn[data-i]", "#hub"], energy: [".seg button", ".range button"],
    climate: ["#range-temp button", ".seg button"], weather: [".wx-zoom button", ".wx-menu-btn", ".seg button"],
    switches: [".sw[data-sw]"], network: [".seg button", ".range button"], alerts: ["#btn-sound", "#btn-ackall", ".btn-soft[data-ack]"],
    systems: [".seg button", ".tabs button"], settings: [".tabs button", ".seg button"],
  };
  const UNSAFE = ".hq-btn, .home-sp, .hsp-btn, .ctl-btn, .ctl-pill, .src-btn, .dial-wrap, .vslider, #fan-slider, .wx-tl-track, .grip, #osk-keys, input, select, textarea";
  const dimmer = id => {   // app.js holdable(): dimmers are held from pointerdown, so they are never pressed
    try { return sw.some(s => s.id === id && s.kind === "dimmer"); } catch { return true; }
  };
  const safe = el => {
    if (el.disabled || el.getAttribute("aria-disabled") === "true" || el.closest(UNSAFE)) return false;
    if (el.dataset?.sw && (el.classList.contains("sw") || el.classList.contains("quick-btn")) && dimmer(el.dataset.sw)) return false;
    const r = el.getBoundingClientRect();
    return r.width >= 24 && r.height >= 24 && r.top >= 0 && r.left >= 0 && r.bottom <= innerHeight && r.right <= innerWidth;
  };
  const view = () => document.querySelector(".view.on")?.dataset.view || "home";
  const targets = v => {
    const sec = document.querySelector(`.view[data-view="${v}"]`);
    if (!sec) return [];
    const out = [], seen = new Set();
    for (const sel of PREFER[v] || []) {          // preferred controls only: no generic fallback
      for (const el of sec.querySelectorAll(sel)) {
        if (seen.has(el) || !safe(el)) continue;
        seen.add(el);
        const r = rect(el);
        if (r) out.push({ name: (el.id || el.dataset?.sw || el.textContent || sel).trim().slice(0, 40), sel, rect: r });
        if (out.length >= 3) return out;
      }
    }
    return out;
  };
  const scene = v => {   // the page's largest visible canvas, else its largest svg
    const sec = document.querySelector(`.view[data-view="${v}"]`);
    if (!sec) return null;
    for (const sel of ["canvas", "svg"]) {
      let best = null;
      for (const el of sec.querySelectorAll(sel)) {
        const r = rect(el);
        if (r && r[2] >= 160 && r[3] >= 120 && (!best || r[2] * r[3] > best.rect[2] * best.rect[3])) best = { sel, rect: r };
      }
      if (best) return best;
    }
    return null;
  };
  const marks = () => {
    const m = {};
    for (const e of performance.getEntriesByType("mark")) if (e.name.startsWith("aetherrv:")) m[e.name] = performance.timeOrigin + e.startTime;
    for (const e of performance.getEntriesByType("paint")) m[e.name] = performance.timeOrigin + e.startTime;
    return m;
  };
  let gl = null;
  const webgl = () => {
    if (gl !== null) return gl;
    try {
      const c = document.createElement("canvas"), ctx = c.getContext("webgl2") || c.getContext("webgl");
      if (!ctx) return (gl = "none");
      const ext = ctx.getExtension("WEBGL_debug_renderer_info");
      gl = `${typeof WebGL2RenderingContext !== "undefined" && ctx instanceof WebGL2RenderingContext ? "webgl2" : "webgl1"}: ${ext ? ctx.getParameter(ext.UNMASKED_RENDERER_WEBGL) : ctx.getParameter(ctx.RENDERER)}`;
      ctx.getExtension("WEBGL_lose_context")?.loseContext();
    } catch (e) { gl = `error: ${e}`; }
    return gl;
  };
  const events = [], longtasks = [];
  const observe = (type, sink, map, opts = {}) => {
    try {
      if (!PerformanceObserver.supportedEntryTypes?.includes(type)) return false;
      new PerformanceObserver(l => { for (const e of l.getEntries()) if (sink.length < 500) sink.push(map(e)); }).observe({ type, buffered: true, ...opts });
      return true;
    } catch { return false; }
  };
  const evOk = observe("event", events, e => ({ name: e.name, start: performance.timeOrigin + e.startTime, dur: e.duration, proc: e.processingEnd - e.processingStart, delay: e.processingStart - e.startTime }), { durationThreshold: 16 });
  const ltOk = observe("longtask", longtasks, e => ({ start: performance.timeOrigin + e.startTime, dur: e.duration }));

  function report(why) {
    const v = view(), rail = {};
    for (const b of document.querySelectorAll(".rail button[data-v]")) rail[b.dataset.v] = rect(b);
    const link = document.getElementById("chip-link")?.textContent || "";
    send("page", {
      why, view: v, viewport: [innerWidth, innerHeight], dpr: devicePixelRatio, ua: navigator.userAgent, webgl: webgl(),
      conn: /live/i.test(link) ? "live" : link.trim().toLowerCase() || null, ready,
      rail, views: rect(document.getElementById("views")), topbar: rect(document.querySelector(".topbar")),
      targets: targets(v), scene: scene(v), marks: marks(),
      event_timing: evOk ? events.splice(0) : "unsupported", longtasks: ltOk ? longtasks.splice(0) : "unsupported",
    });
  }
  let rz;
  window.addEventListener("resize", () => { clearTimeout(rz); rz = setTimeout(() => ready && report("resize"), 500); });
  let last = "", timer;
  setInterval(() => {
    const v = view();
    if (ready && v !== last) { last = v; clearTimeout(timer); timer = setTimeout(() => report("view"), 800); }
  }, 250);
})();
