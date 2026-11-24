// RadarGL: the radar as one WebGL2 canvas in its own map pane, positioned and zoom-animated like Leaflet's Canvas
// renderer (ported from app.js). Each frame becomes one texture covering the view (tiles composited; recorded
// low-zoom tiles upscaled), and between frames A and B the shader moves both along the Pi's flow field d
// (A(p) ≈ B(p + d)):
//   I(t) = (1 − t)·A(p − t·d) + t·B(p + (1 − t)·d)        t = 0 is exactly A, t = 1 exactly B.
// No flow for a pair → the nearest recorded pair's (motion persists over tens of minutes); outside the flow block
// → d = 0.
//
// Changes from app.js (deliberate):
//  • every texture keeps its own atlas placement (aW0/aWs, bW0/bWs), and textures are keyed by frame URL: after a
//    pan / zoom or the 10-minute frame refresh the last image stays on screen while the new atlas builds, instead
//    of the radar blanking until every tile is re-fetched;
//  • the canvas fades in on its first image;
//  • remove() disposes everything (textures, flow fields, tile bitmaps, in-flight fetches, the context itself),
//    so a hidden Weather page holds no GPU memory.
import * as L from "leaflet";
import { nearestFlow, type Frame } from "./frames";

const VS = `#version 300 es
in vec2 a; out vec2 v; void main() { v = vec2(a.x * .5 + .5, .5 - a.y * .5); gl_Position = vec4(a, 0., 1.); }`;
const FS = `#version 300 es
precision highp float;
in vec2 v; out vec4 o;
uniform sampler2D A, B, F;
uniform vec2 cW0, cWs, aW0, aWs, bW0, bWs, fW0, fWs;
uniform float t, alpha, hasB, hasF, fScale;
vec4 samp(sampler2D s, vec2 w, vec2 W0, vec2 Ws) {
  vec2 u = (w - W0) / Ws;
  if (any(lessThan(u, vec2(0.))) || any(greaterThan(u, vec2(1.)))) return vec4(0.);
  return texture(s, u);
}
void main() {
  vec2 w = cW0 + v * cWs, d = vec2(0.);
  if (hasF > .5) {
    vec2 fu = (w - fW0) / fWs, e = smoothstep(0., .06, fu) * smoothstep(0., .06, 1. - fu);
    d = texture(F, clamp(fu, 0., 1.)).xy * fScale * e.x * e.y;
  }
  vec4 c = samp(A, w - d * t, aW0, aWs);
  if (hasB > .5) c = mix(c, samp(B, w + d * (1. - t), bW0, bWs), t);
  o = c * alpha;
}`;
const UNIFORMS = ["A", "B", "F", "cW0", "cWs", "aW0", "aWs", "bW0", "bWs", "fW0", "fWs", "t", "alpha", "hasB", "hasF", "fScale"] as const;

/** What the renderer reads from its owner (the Weather map controller). */
export interface RadarHost {
  frames(): readonly Frame[];
  flowTimes(): readonly number[];
  alpha(): number;
  pos(): number;
  playing(): boolean;
  /** The context was lost mid-session: the owner falls back to tile layers. */
  lost(why: string): void;
}

type V2 = [number, number];
interface Spec { za: number; tx0: number; ty0: number; nx: number; ny: number; key: string; W0: V2; Ws: V2 }
interface Tex { tex: WebGLTexture; key: string; W0: V2; Ws: V2 }
interface FlowTex { tex: WebGLTexture; W0: V2; Ws: V2; unit: number; dt: number }
interface FlowJson { z: number; x0: number; y0: number; w: number; n: number; dx: number[]; dy: number[]; dt?: number }

const TEX_BUDGET = 320e6;     // bytes of frame textures kept resident (a loop builds each frame once)
const BMP_LRU = 160;          // decoded tiles kept

/** One radar canvas on the map. Throws from addTo() when WebGL2 is unavailable. */
export class RadarGL extends L.Renderer {
  canvas!: HTMLCanvasElement;
  renderer = "";
  /** Frames drawn (diagnostics). */
  draws = 0;
  private gl!: WebGL2RenderingContext;
  private prog!: WebGLProgram;
  private buf: WebGLBuffer | null = null;
  private u = {} as Record<(typeof UNIFORMS)[number], WebGLUniformLocation | null>;
  private tex = new Map<string, Tex>();                       // frame URL → texture (with the atlas it was built for)
  private flows = new Map<number, FlowTex | "pending" | null>();
  private bmp = new Map<string, Promise<ImageBitmap | null>>();
  private building = new Set<string>();
  private queue: number[] = [];
  private spec: Spec | null = null;
  private cW0: V2 | null = null;
  private cWs: V2 = [1, 1];
  private dead = false;
  private shown = false;
  private abort = new AbortController();
  private onLost = (e: Event) => { e.preventDefault(); if (!this.dead) this.host.lost("context lost"); };

  constructor(private readonly host: RadarHost) {
    super({ padding: 0.06, pane: "radar" });
  }

  // ── Leaflet Renderer hooks ──
  _initContainer(): void {
    const c = this.canvas = L.DomUtil.create("canvas", "wx-radar-gl") as HTMLCanvasElement;
    (this as unknown as { _container: HTMLElement })._container = c;
    const gl = c.getContext("webgl2", { premultipliedAlpha: true, antialias: false, alpha: true });
    if (!gl) throw new Error("WebGL2 unavailable");
    this.gl = gl;
    const sh = (type: number, src: string) => {
      const s = gl.createShader(type)!;
      gl.shaderSource(s, src); gl.compileShader(s);
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s) ?? "shader");
      return s;
    };
    const p = this.prog = gl.createProgram()!;
    gl.attachShader(p, sh(gl.VERTEX_SHADER, VS)); gl.attachShader(p, sh(gl.FRAGMENT_SHADER, FS)); gl.linkProgram(p);
    if (!gl.getProgramParameter(p, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(p) ?? "link");
    gl.useProgram(p);
    this.buf = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buf);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
    gl.enableVertexAttribArray(0); gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
    for (const n of UNIFORMS) this.u[n] = gl.getUniformLocation(p, n);
    gl.uniform1i(this.u.A, 0); gl.uniform1i(this.u.B, 1); gl.uniform1i(this.u.F, 2);
    gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, true);
    const info = gl.getExtension("WEBGL_debug_renderer_info");
    try { this.renderer = String(info ? gl.getParameter(info.UNMASKED_RENDERER_WEBGL) : gl.getParameter(gl.RENDERER)); } catch { /* */ }
    c.addEventListener("webglcontextlost", this.onLost);
  }

  _destroyContainer(): void { this.dispose(); }

  /** Leaflet's Renderer.onAdd fires its first "update" before it listens for it: place the atlas now, or nothing
   *  would draw until the map next moves (it doesn't, when the page is shown again). */
  override onAdd(map: L.Map): this {
    (L.Renderer.prototype as unknown as { onAdd(m: L.Map): void }).onAdd.call(this, map);
    this._updatePaths();
    return this;
  }

  _update(): void {
    const self = this as unknown as { _map: L.Map & { _animatingZoom?: boolean }; _bounds?: L.Bounds };
    if (self._map._animatingZoom && self._bounds) return;
    (L.Renderer.prototype as unknown as { _update(): void })._update.call(this);
    const b = self._bounds!, s = b.getSize(), c = this.canvas;
    L.DomUtil.setPosition(c, b.min!);
    c.width = s.x; c.height = s.y; c.style.width = `${s.x}px`; c.style.height = `${s.y}px`;
    this.gl.viewport(0, 0, s.x, s.y);
    this.fire("update");
  }

  /** The view changed: new canvas placement, maybe a new atlas (old textures keep drawing until rebuilt). */
  _updatePaths(): void {
    const self = this as unknown as { _map: L.Map; _bounds: L.Bounds };
    const m = self._map, b = self._bounds, z = m.getZoom(), s = b.getSize();
    const w0 = m.project(m.layerPointToLatLng(b.min!), 0).divideBy(256);
    this.cW0 = [w0.x, w0.y]; this.cWs = [s.x / (256 * 2 ** z), s.y / (256 * 2 ** z)];
    const za = Math.max(1, Math.min(10, Math.round(z) - 1)), N = 2 ** za;
    const tx0 = Math.floor(w0.x * N), ty0 = Math.max(0, Math.floor(w0.y * N));
    const tx1 = Math.floor((w0.x + this.cWs[0]) * N), ty1 = Math.min(N - 1, Math.floor((w0.y + this.cWs[1]) * N));
    const sp = { za, tx0, ty0, nx: tx1 - tx0 + 1, ny: ty1 - ty0 + 1 }, key = JSON.stringify(sp);
    if (!this.spec || this.spec.key !== key) this.spec = { ...sp, key, W0: [tx0 / N, ty0 / N], Ws: [sp.nx / N, sp.ny / N] };
    const pos = this.host.pos();
    this.want(pos); this.draw(pos);
  }

  // ── frames ──
  /** A new frame list: textures for frames no longer in it are freed; the rest are kept (keyed by URL). */
  setFrames(frames: readonly Frame[]): void {
    if (this.dead) return;
    const urls = new Set(frames.map(f => f.url));
    for (const [u, t] of this.tex) if (!urls.has(u)) { this.gl.deleteTexture(t.tex); this.tex.delete(u); }
    const ft = new Set(this.host.flowTimes());
    for (const [t, f] of this.flows) if (!ft.has(t)) { if (f && f !== "pending") this.gl.deleteTexture(f.tex); this.flows.delete(t); }
    this.queue = [];
    if (!frames.length) { this.gl.clearColor(0, 0, 0, 0); this.gl.clear(this.gl.COLOR_BUFFER_BIT); }
  }

  /**
   * Build the frames around pos (more ahead while playing). Paused: the frame(s) shown plus the first frames of
   * the loop, so Play starts at once. Built textures stay resident up to TEX_BUDGET; past that the ones furthest
   * ahead in the loop order are freed first (stale-atlas ones before current ones).
   */
  want(pos: number): void {
    const fr = this.host.frames(), n = fr.length;
    if (this.dead || !n || !this.spec) return;
    const i = Math.floor(pos), playing = this.host.playing();
    const keep = [...new Set([...(playing ? [0, 1, 2, 3, 4, 5, 6, -1] : pos - i > 1e-3 ? [0, 1] : [0]).map(d => (i + d + n) % n),
      ...(playing ? [] : [0, 1, 2].filter(k => k < n))])];
    this.queue = keep;
    this.pump();
    for (const k of keep.slice(0, 2)) this.flowFor(k);
    const per = this.spec.nx * this.spec.ny * 512 * 512 * 4, max = Math.max(keep.length, Math.floor(TEX_BUDGET / per));
    const keepUrls = new Set(keep.map(k => fr[k]!.url)), idx = new Map(fr.map((f, k) => [f.url, k]));
    while (this.tex.size > max) {
      let worst: string | null = null, wd = -1;
      for (const [u, t] of this.tex) {
        if (keepUrls.has(u)) continue;                                 // never one we want
        const d = ((idx.get(u) ?? 0) - i + n) % n + (t.key === this.spec.key ? 0 : n);
        if (d > wd) { wd = d; worst = u; }
      }
      if (worst == null) break;
      this.gl.deleteTexture(this.tex.get(worst)!.tex); this.tex.delete(worst);
    }
  }

  /** At most two builds at once, most-needed first. */
  private pump(): void {
    const fr = this.host.frames();
    for (const k of this.queue) {
      if (this.dead || this.building.size >= 2 || !this.spec) break;
      const f = fr[k];
      if (!f) continue;
      const have = this.tex.get(f.url);
      if ((have && have.key === this.spec.key) || this.building.has(f.url)) continue;
      this.building.add(f.url);
      void this.build(f, this.spec).finally(() => { this.building.delete(f.url); if (!this.dead) this.pump(); });
    }
  }

  /** A tile, decoded once and kept in a small LRU (a pan or a rebuild reuses it). */
  private tileBitmap(url: string): Promise<ImageBitmap | null> {
    const hit = this.bmp.get(url);
    if (hit) { this.bmp.delete(url); this.bmp.set(url, hit); return hit; }
    const p = fetch(url, { signal: this.abort.signal }).then(r => (r.ok ? r.blob() : null))
      .then(b => (b && !this.dead ? createImageBitmap(b) : null)).catch(() => null);
    this.bmp.set(url, p);
    while (this.bmp.size > BMP_LRU) {
      const [k, v] = this.bmp.entries().next().value as [string, Promise<ImageBitmap | null>];
      this.bmp.delete(k);
      void v.then(b => b?.close());
    }
    return p;
  }

  /** One frame's texture for the atlas `sp`: its tiles composited into one canvas (recorded low-zoom tiles scaled
   *  up), uploaded once. (Measured against per-tile texSubImage2D uploads: no better, worse worst frame.) */
  private async build(f: Frame, sp: Spec): Promise<void> {
    // tile zoom for this frame: the atlas zoom, clamped to what exists (recorded: 4-7; source: ≤ 10)
    const zt = Math.max(f.minz, Math.min(f.maxz, sp.za)), dz = sp.za - zt;
    const cv = new OffscreenCanvas(sp.nx * 512, sp.ny * 512), cx = cv.getContext("2d")!;
    const jobs: Promise<unknown>[] = [];
    const tile = (z: number, x: number, y: number) => f.url.replace("{z}", String(z)).replace("{x}", String(x)).replace("{y}", String(y));
    for (let j = 0; j < sp.ny; j++) for (let i = 0; i < sp.nx; i++) {
      const N = 2 ** sp.za, ax = ((sp.tx0 + i) % N + N) % N, ay = sp.ty0 + j;
      if (dz >= 0) {                                  // one (parent) tile, its sub-square scaled up into the cell
        const s = 512 / 2 ** dz, px = ax >> dz, py = ay >> dz;
        jobs.push(this.tileBitmap(tile(zt, px, py)).then(b => b && cx.drawImage(b, (ax - (px << dz)) * s, (ay - (py << dz)) * s, s, s, i * 512, j * 512, 512, 512)));
      } else {                                        // finer tiles than the atlas: several scaled down into the cell
        const m = 2 ** -dz, s = 512 / m;
        for (let v = 0; v < m; v++) for (let u = 0; u < m; u++)
          jobs.push(this.tileBitmap(tile(zt, ax * m + u, ay * m + v)).then(b => b && cx.drawImage(b, i * 512 + u * s, j * 512 + v * s, s, s)));
      }
    }
    await Promise.all(jobs);
    if (this.dead || !this.host.frames().some(x => x.url === f.url)) return;       // gone, or no longer looped
    const cur = this.tex.get(f.url);
    if (cur && sp.key !== this.spec?.key) return;                                    // older atlas than the one shown
    const gl = this.gl, tex = gl.createTexture()!;
    gl.activeTexture(gl.TEXTURE3); gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, cv);
    this.texParams();
    if (cur) gl.deleteTexture(cur.tex);
    this.tex.set(f.url, { tex, key: sp.key, W0: sp.W0, Ws: sp.Ws });
    const fr = this.host.frames(), i = Math.floor(this.host.pos());
    if (fr[i]?.url === f.url || fr[i + 1]?.url === f.url) this.draw(this.host.pos());
  }

  private texParams(): void {
    const gl = this.gl;
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR); gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE); gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  }

  /** Flow for the pair starting at frame k: its own, else the nearest recorded pair's (scaled to this pair's gap). */
  private flowFor(k: number): (FlowTex & { scale: number }) | null {
    const fr = this.host.frames(), a = fr[k], b = fr[k + 1];
    if (!a || !b) return null;
    const ft = nearestFlow(this.host.flowTimes(), a.time);
    if (ft == null) return null;
    const fl = this.flows.get(ft);
    if (fl === undefined) {
      this.flows.set(ft, "pending");
      fetch(`/api/radar/flow/${ft}`, { signal: this.abort.signal }).then(r => (r.ok ? r.json() as Promise<FlowJson> : null)).then(j => {
        if (this.dead) return;
        if (!j || !j.n || !Array.isArray(j.dx)) { this.flows.set(ft, null); return; }
        const gl = this.gl, n = j.n, data = new Float32Array(n * n * 2);
        for (let q = 0; q < n * n; q++) { data[2 * q] = j.dx[q] ?? 0; data[2 * q + 1] = j.dy[q] ?? 0; }
        const tex = gl.createTexture()!;
        gl.activeTexture(gl.TEXTURE3); gl.bindTexture(gl.TEXTURE_2D, tex);
        gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, false);
        gl.texImage2D(gl.TEXTURE_2D, 0, gl.RG16F, n, n, 0, gl.RG, gl.FLOAT, data);
        gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, true);
        this.texParams();
        const Z = 2 ** j.z;                            // flow grid: z{j.z} tiles x0..x0+w, values in z{j.z} px per dt
        this.flows.set(ft, { tex, W0: [j.x0 / Z, j.y0 / Z], Ws: [j.w / Z, j.w / Z], unit: 1 / (512 * Z), dt: j.dt || 600 });
        this.draw(this.host.pos());
      }).catch(() => { if (!this.dead) this.flows.set(ft, null); });
      return null;
    }
    return fl && fl !== "pending" ? { ...fl, scale: fl.unit * (b.time - a.time) / fl.dt } : null;
  }

  /** Both textures of the pair at pos are in (a stale-atlas texture counts: it still shows the right weather). */
  ready(pos: number): boolean {
    const fr = this.host.frames(), i = Math.floor(pos), t = pos - i;
    const a = fr[i], b = fr[i + 1];
    return !!a && this.tex.has(a.url) && (t < 1e-3 || !b || this.tex.has(b.url));
  }

  draw(pos: number): boolean {
    const fr = this.host.frames(), n = fr.length;
    if (this.dead || !n || !this.spec || !this.cW0) return false;
    const i = Math.min(n - 1, Math.floor(pos)), t = pos - i;
    const A = this.tex.get(fr[i]!.url), B = t > 1e-3 && fr[i + 1] ? this.tex.get(fr[i + 1]!.url) : undefined;
    if (!A) return false;                             // keep the last image until this frame is in
    const gl = this.gl, u = this.u, fl = B ? this.flowFor(i) : null, Bt = B ?? A;
    gl.useProgram(this.prog);
    gl.activeTexture(gl.TEXTURE0); gl.bindTexture(gl.TEXTURE_2D, A.tex);
    gl.activeTexture(gl.TEXTURE1); gl.bindTexture(gl.TEXTURE_2D, Bt.tex);
    gl.activeTexture(gl.TEXTURE2); gl.bindTexture(gl.TEXTURE_2D, fl ? fl.tex : null);
    gl.uniform2fv(u.cW0, this.cW0); gl.uniform2fv(u.cWs, this.cWs);
    gl.uniform2fv(u.aW0, A.W0); gl.uniform2fv(u.aWs, A.Ws); gl.uniform2fv(u.bW0, Bt.W0); gl.uniform2fv(u.bWs, Bt.Ws);
    gl.uniform2fv(u.fW0, fl ? fl.W0 : [0, 0]); gl.uniform2fv(u.fWs, fl ? fl.Ws : [1, 1]);
    gl.uniform1f(u.t, B ? t : 0); gl.uniform1f(u.alpha, this.host.alpha()); gl.uniform1f(u.hasB, B ? 1 : 0);
    gl.uniform1f(u.hasF, fl ? 1 : 0); gl.uniform1f(u.fScale, fl ? fl.scale : 0);
    gl.clearColor(0, 0, 0, 0); gl.clear(gl.COLOR_BUFFER_BIT);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    this.draws++;
    if (!this.shown) { this.shown = true; this.canvas.classList.add("on"); }
    return true;
  }

  /** Free everything; the context too. Safe to call twice. */
  dispose(): void {
    if (this.dead) return;
    this.dead = true;
    this.abort.abort();
    const c = this.canvas, gl = this.gl;
    c?.removeEventListener("webglcontextlost", this.onLost);   // first: our own loseContext() must not trigger the fallback
    if (gl) {
      for (const t of this.tex.values()) gl.deleteTexture(t.tex);
      for (const f of this.flows.values()) if (f && f !== "pending") gl.deleteTexture(f.tex);
      if (this.buf) gl.deleteBuffer(this.buf);
      if (this.prog) gl.deleteProgram(this.prog);
      gl.getExtension("WEBGL_lose_context")?.loseContext();
    }
    for (const p of this.bmp.values()) void p.then(b => b?.close());
    this.tex.clear(); this.flows.clear(); this.bmp.clear(); this.building.clear(); this.queue = [];
    if (c) { c.width = c.height = 0; L.DomUtil.remove(c); }
  }

  /** Textures resident (tests / diagnostics). */
  get textures(): number { return this.tex.size; }
}
