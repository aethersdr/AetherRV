// The WebGL battery bank (owner Q-U6): one glass cell block per EPOCH battery on a platform, a lit fill level per
// battery coloured by charge (the SVG scene's thresholds), wired in parallel by + / − buses whose dashes run with the
// bank current. Drag to orbit; it eases back home a few seconds after the last touch.
//
// Built for the Pi 5:
//  • render on demand: a frame is drawn only while something moves (a level easing in, the flow while the bank is
//    charging or discharging, a drag or the ease home) or after a change; otherwise nothing is drawn;
//  • pixel ratio ≤ 1.5, low-poly geometry, one shared material per colour, no shadows (a glow decal under each block);
//  • a reading changes materials, scales and one texture offset, never the scene; nothing is allocated per frame;
//  • pause() (its page hidden) stops the loop and keeps everything, resume() draws again at the next frame
//    (lib/glkeep.ts); dispose() stops the loop and frees the renderer, its context, every geometry, material and
//    texture.
// Loaded only through Bank3D.tsx's dynamic import(), so three is its own chunk.
import * as THREE from "three";
import { easeOutCubic } from "../../lib/tween";
import { approach, B3, b3FillH, b3Width, b3X, clampOrbit, fitDistance, flowAdvance, hueOf, LEVEL_MIN, LEVEL_MS, ORBIT, orbitPos,
  pctText, type Hue } from "./bank";

// The SVG scene's palette (aethersdr.com tokens): [deep, body, bright, highlight] per charge colour.
const HUES: Record<Hue, readonly [number, number, number, number]> = {
  blue: [0x0a1422, 0x1d5f9e, 0x3aa7ff, 0x6fd0ea], amber: [0x1f1606, 0x8a5f14, 0xc9942f, 0xe9c77d], red: [0x1f0d09, 0x8a3a29, 0xc9604a, 0xe9a594],
};

/** Counters for tests and the benchmark, on window only with ?bench=1 (they outlive any one scene). */
export interface Bench { live: number; created: number; disposed: number; frames: number; res: number; lost: number;
  levels: (number | null)[]; hues: string[]; flow: string; calls: number; triangles: number; firstFrameMs: number | null; orbit: number[];
  ms: Record<string, number> }
const BENCH = typeof location !== "undefined" && new URLSearchParams(location.search).get("bench") === "1";
export const bench: Bench | null = BENCH
  ? ((window as unknown as { __bank3d?: Bench }).__bank3d ??= { live: 0, created: 0, disposed: 0, frames: 0, res: 0, lost: 0,
    levels: [], hues: [], flow: "", calls: 0, triangles: 0, firstFrameMs: null, orbit: [], ms: {} })
  : null;

export interface Labels { pct: HTMLElement[]; name: HTMLElement[]; foot: HTMLElement[]; plus: HTMLElement; minus: HTMLElement }

interface Batt {
  fill: THREE.Mesh; surf: THREE.Mesh; pool: THREE.Mesh; poolMat: THREE.MeshBasicMaterial; shell: THREE.Mesh; edges: THREE.LineSegments;
  x: number; shown: number | null; target: number | null | undefined; from: number; t0: number; hue: Hue | "off" | "";
}

const Y = new THREE.Vector3(0, 1, 0);

export class BankGL {
  readonly ok: boolean = false;
  private renderer!: THREE.WebGLRenderer;
  private readonly canvas: HTMLCanvasElement;
  private readonly scene = new THREE.Scene();
  private readonly camera = new THREE.PerspectiveCamera(B3.fov, 8 / 3, 0.1, 60);
  private readonly owned: { dispose(): void }[] = [];
  private readonly batts: Batt[] = [];
  private readonly fillMat = {} as Record<Hue, THREE.MeshPhongMaterial>;
  private readonly surfMat = {} as Record<Hue, THREE.MeshBasicMaterial>;
  private shellMat!: THREE.MeshPhongMaterial;
  private shellOffMat!: THREE.MeshPhongMaterial;
  private edgeMat!: THREE.LineBasicMaterial;
  private edgeOffMat!: THREE.LineDashedMaterial;
  private flowTex!: THREE.CanvasTexture;
  private readonly flowMeshes: THREE.Mesh[] = [];
  private flowDir: 0 | 1 | -1 = 0;
  private flowDur = 2;
  private readonly anchors: THREE.Vector3[] = [];         // pct, name, foot per battery; then + and − ends
  private readonly tmp = new THREE.Vector3();
  private readonly target = new THREE.Vector3(0, B3.ty, 0);
  private dist = 6;
  private az: number = ORBIT.az;
  private el: number = ORBIT.el;
  private camDirty = true;
  private size = { w: 0, h: 0 };
  private raf = 0;
  private last = 0;
  private dead = false;
  private paused = false;
  private dragging: { id: number; x: number; y: number } | null = null;
  private touchedAt = -1e9;
  private homeTimer: ReturnType<typeof setTimeout> | undefined;
  private ro: ResizeObserver | null = null;
  private readonly t0 = performance.now();
  private firstDone = false;
  private readonly reduced: boolean;

  constructor(private readonly host: HTMLElement, private readonly n: number, private readonly labels: Labels,
              private readonly hooks: { onLost: () => void; onFirstFrame: () => void }) {
    this.reduced = typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
    this.canvas = document.createElement("canvas");
    this.canvas.className = "b3-canvas";
    try {
      this.renderer = new THREE.WebGLRenderer({ canvas: this.canvas, antialias: true, alpha: true, powerPreference: "low-power" });
    } catch {
      return;                                          // no WebGL2: ok stays false, the caller shows the SVG scene
    }
    const r = this.renderer;
    const tCtx = performance.now();
    r.setPixelRatio(Math.min(window.devicePixelRatio || 1, 1.5));
    r.setClearColor(0x000000, 0);
    host.appendChild(this.canvas);
    this.canvas.addEventListener("webglcontextlost", this.onLost);
    this.canvas.addEventListener("pointerdown", this.onDown);
    this.canvas.addEventListener("pointermove", this.onMove);
    this.canvas.addEventListener("pointerup", this.onUp);
    this.canvas.addEventListener("pointercancel", this.onUp);
    this.build();
    const tBuild = performance.now();
    this.ro = new ResizeObserver(() => this.resize());
    this.ro.observe(host);
    this.resize();
    (this as { ok: boolean }).ok = true;
    if (bench) {
      bench.live++; bench.created++; bench.levels = Array(this.n).fill(null); bench.hues = Array(this.n).fill(""); bench.firstFrameMs = null;
      bench.ms = { context: Math.round(tCtx - this.t0), build: Math.round(tBuild - tCtx) };
    }
    // compile the shaders without blocking (KHR_parallel_shader_compile where there is one), then the first frame
    if (r.extensions.has("KHR_parallel_shader_compile")) {
      void r.compileAsync(this.scene, this.camera).catch(() => undefined).then(() => { if (!this.dead) this.request(); });
    } else this.request();                             // no parallel compile: the first frame compiles
  }

  // ── building (once per battery set) ──
  private own<T extends { dispose(): void }>(x: T): T {
    this.owned.push(x);
    if (bench) bench.res++;
    return x;
  }

  private build(): void {
    const s = this.scene, n = this.n, W = b3Width(n);
    s.fog = new THREE.Fog(0x0b1424, 7, 16);
    s.add(new THREE.HemisphereLight(0xa8dcff, 0x0a1422, 1.6));
    const sun = new THREE.DirectionalLight(0xffffff, 1.5);
    sun.position.set(-3, 5, 4);
    s.add(sun);

    // shared resources
    const unitBox = this.own(new THREE.BoxGeometry(1, 1, 1));
    const unitPlane = this.own(new THREE.PlaneGeometry(1, 1)).rotateX(-Math.PI / 2);
    for (const h of Object.keys(HUES) as Hue[]) {
      const [, , d2, d3] = HUES[h];
      const side = this.own(sideTexture(HUES[h]));               // the SVG's side gradient, glowing
      this.fillMat[h] = this.own(new THREE.MeshPhongMaterial({ map: side, emissive: 0xffffff, emissiveMap: side, emissiveIntensity: .5,
        specular: d3, shininess: 60, transparent: true, opacity: .92 }));
      this.surfMat[h] = this.own(new THREE.MeshBasicMaterial({ color: d2, transparent: true, opacity: .85 }));
    }
    this.shellMat = this.own(new THREE.MeshPhongMaterial({ color: 0x1c3454, specular: 0x9fdcff, shininess: 90, transparent: true,
      opacity: .2, depthWrite: false }));
    this.shellOffMat = this.own(new THREE.MeshPhongMaterial({ color: 0x1c3454, transparent: true, opacity: .08, depthWrite: false }));
    this.edgeMat = this.own(new THREE.LineBasicMaterial({ color: 0x8ef7e6, transparent: true, opacity: .4 }));
    this.edgeOffMat = this.own(new THREE.LineDashedMaterial({ color: 0x78bee6, transparent: true, opacity: .3, dashSize: .05, gapSize: .05 }));
    const seamMat = this.own(new THREE.LineBasicMaterial({ color: 0x8ef7e6, transparent: true, opacity: .14 }));
    const lidMat = this.own(new THREE.MeshPhongMaterial({ color: 0x14243a, specular: 0x335577, shininess: 30 }));
    const termP = this.own(new THREE.MeshPhongMaterial({ color: 0xee7355, emissive: 0x5a1e12 }));
    const termN = this.own(new THREE.MeshPhongMaterial({ color: 0x5de3ff, emissive: 0x0d3a52 }));
    const busPMat = this.own(new THREE.MeshPhongMaterial({ color: 0xc9604a, emissive: 0x4a1a10 }));
    const busNMat = this.own(new THREE.MeshPhongMaterial({ color: 0x2a83c9, emissive: 0x0b2a48 }));
    const haloP = this.own(new THREE.MeshBasicMaterial({ color: 0xc9604a, transparent: true, opacity: .12, depthWrite: false }));
    const haloN = this.own(new THREE.MeshBasicMaterial({ color: 0x2a83c9, transparent: true, opacity: .12, depthWrite: false }));
    const glowTex = this.own(radialTexture());
    // flow dashes: 3 on, 15 off (the SVG's stroke-dasharray), scrolled along every bus and post
    this.flowTex = this.own(dashTexture());
    const flowMat = this.own(new THREE.MeshBasicMaterial({ map: this.flowTex, color: 0x8ef7e6, transparent: true, opacity: .55,
      blending: THREE.AdditiveBlending, depthWrite: false }));

    // platform: an elliptical slab with a glowing rim
    const rx = W / 2 + 0.8, rz = B3.platZ;
    const slab = new THREE.Mesh(this.own(new THREE.CylinderGeometry(1, 1, 0.14, 64, 1)),
      this.own(new THREE.MeshPhongMaterial({ color: 0x0e1a2a, emissive: 0x060c16, specular: 0x24466e, shininess: 40 })));
    slab.scale.set(rx, 1, rz);
    slab.position.y = -0.07;
    s.add(slab);
    const rimGeo = this.own(new THREE.TorusGeometry(1, 0.008, 4, 128)).rotateX(Math.PI / 2);
    const rim = new THREE.Mesh(rimGeo, this.own(new THREE.MeshBasicMaterial({ color: 0x5de3ff, transparent: true, opacity: .5 })));
    rim.scale.set(rx, 1, rz);
    rim.position.y = 0.002;
    s.add(rim);
    const inner = new THREE.Mesh(rimGeo, this.own(new THREE.MeshBasicMaterial({ color: 0x78bee6, transparent: true, opacity: .18 })));
    inner.scale.set(rx - 0.18, 1, rz - 0.14);
    inner.position.y = 0.002;
    s.add(inner);

    // one block per battery
    const shellGeo = this.own(new THREE.BoxGeometry(B3.w, B3.h, B3.d));
    const edgeGeo = this.own(new THREE.EdgesGeometry(shellGeo));
    const inH = B3.h - B3.lid - 2 * B3.fillPad;
    const seamGeo = this.own(seams(B3.w * 0.92, B3.d * 0.9, [0.25, 0.5, 0.75].map(f => B3.fillPad + f * inH)));
    const lidGeo = this.own(new THREE.BoxGeometry(B3.w * 0.98, B3.lid, B3.d * 0.98));
    const termGeo = this.own(new THREE.CylinderGeometry(0.065, 0.065, 0.1, 12));
    const postLen = B3.busP - (B3.h + 0.1), postNLen = B3.busN - (B3.h + 0.1);
    const postP = this.own(tubeGeo(0.03, postLen)), postN = this.own(tubeGeo(0.03, postNLen));
    const flowP = this.own(tubeGeo(0.038, postLen)), flowN = this.own(tubeGeo(0.038, postNLen));
    for (let i = 0; i < n; i++) {
      const x = b3X(i, n), g = new THREE.Group();
      g.position.x = x;
      const poolMat = this.own(new THREE.MeshBasicMaterial({ map: glowTex, color: HUES.blue[2], transparent: true, opacity: 0,
        blending: THREE.AdditiveBlending, depthWrite: false }));
      const pool = new THREE.Mesh(unitPlane, poolMat);
      pool.scale.set(B3.w * 2.2, 1, B3.d * 2.4);
      pool.position.y = 0.004;
      pool.renderOrder = -1;
      const fill = new THREE.Mesh(unitBox, this.fillMat.blue);
      const surf = new THREE.Mesh(unitPlane, this.surfMat.blue);
      fill.visible = surf.visible = false;
      const shell = new THREE.Mesh(shellGeo, this.shellMat);
      shell.position.y = B3.h / 2;
      shell.renderOrder = 2;
      const edges = new THREE.LineSegments(edgeGeo, this.edgeMat);
      edges.position.y = B3.h / 2;
      edges.computeLineDistances();
      const seam = new THREE.LineSegments(seamGeo, seamMat);
      const lid = new THREE.Mesh(lidGeo, lidMat);
      lid.position.y = B3.h - B3.lid / 2;
      const tp = new THREE.Mesh(termGeo, termP), tn = new THREE.Mesh(termGeo, termN);
      tp.position.set(-B3.term, B3.h + 0.05, B3.zP);
      tn.position.set(B3.term, B3.h + 0.05, B3.zN);
      // + post: bus down to the terminal; − post: terminal up to the bus (the direction charge current runs)
      const pp = place(new THREE.Mesh(postP, busPMat), [-B3.term, B3.busP, B3.zP], [-B3.term, B3.h + 0.1, B3.zP]);
      const pn = place(new THREE.Mesh(postN, busNMat), [B3.term, B3.h + 0.1, B3.zN], [B3.term, B3.busN, B3.zN]);
      const fp = place(new THREE.Mesh(flowP, flowMat), [-B3.term, B3.busP, B3.zP], [-B3.term, B3.h + 0.1, B3.zP]);
      const fn = place(new THREE.Mesh(flowN, flowMat), [B3.term, B3.h + 0.1, B3.zN], [B3.term, B3.busN, B3.zN]);
      this.flowMeshes.push(fp, fn);
      g.add(pool, fill, surf, shell, edges, seam, lid, tp, tn, pp, pn, fp, fn);
      s.add(g);
      this.batts.push({ fill, surf, pool, poolMat, shell, edges, x, shown: null, target: undefined, from: 0, t0: 0, hue: "" });
      this.anchors.push(new THREE.Vector3(x, B3.fillPad + inH * 0.5, B3.d / 2 + 0.02), new THREE.Vector3(x, 0, B3.d / 2 + 0.3),
        new THREE.Vector3(x, 0, rz));
    }
    // buses: + from its end (right) along to the first post, − from the last post to its end (left)
    const xl = b3X(0, n) - B3.term, xr = b3X(n - 1, n) + B3.term, pE = W / 2 + 0.5, nE = -W / 2 - 0.5;
    const bus = (r: number, mat: THREE.Material, a: number[], b: number[]) => {
      const m = place(new THREE.Mesh(this.own(tubeGeo(r, Math.hypot(b[0]! - a[0]!, b[1]! - a[1]!, b[2]! - a[2]!))), mat), a, b);
      s.add(m);
      return m;
    };
    const P0 = [pE, B3.busP, B3.zP], P1 = [xl - 0.03, B3.busP, B3.zP], N0 = [xr + 0.03, B3.busN, B3.zN], N1 = [nE, B3.busN, B3.zN];
    bus(0.035, busPMat, P0, P1); bus(0.035, busNMat, N0, N1);
    bus(0.09, haloP, P0, P1); bus(0.09, haloN, N0, N1);
    this.flowMeshes.push(bus(0.042, flowMat, P0, P1), bus(0.042, flowMat, N0, N1));
    for (const m of this.flowMeshes) m.visible = false;
    this.anchors.push(new THREE.Vector3(pE + 0.16, B3.busP, B3.zP), new THREE.Vector3(nE - 0.16, B3.busN, B3.zN));
  }

  // ── live values ──
  /** New charge readings (%, null = no data). A visible change eases in; a small one is written at once. */
  setLevels(pcts: readonly (number | null)[]): void {
    const now = performance.now();
    let changed = false;
    pcts.forEach((p, i) => {
      const b = this.batts[i];
      if (!b || p === b.target) return;
      const firstTime = b.target === undefined;
      b.target = p;
      changed = true;
      if (firstTime || p == null || b.shown == null || Math.abs(p - b.shown) < LEVEL_MIN || this.reduced) { b.t0 = 0; this.draw(i, p); }
      else { b.from = b.shown; b.t0 = now; }
    });
    if (changed) this.request();
  }

  /** The bank current's direction and dash period (s): dashes run only while charging or discharging. */
  setFlow(flow: "" | "chg" | "dis", durS: number): void {
    const dir = flow === "chg" ? 1 : flow === "dis" ? -1 : 0;
    if (dir === this.flowDir && durS === this.flowDur) return;
    if ((dir === 0) !== (this.flowDir === 0)) for (const m of this.flowMeshes) m.visible = dir !== 0;
    this.flowDir = dir;
    this.flowDur = durS;
    if (bench) bench.flow = flow;
    this.request();
  }

  private draw(i: number, pct: number | null): void {
    const b = this.batts[i]!;
    b.shown = pct;
    const soc = pct == null ? null : Math.max(0, Math.min(100, pct));
    const hue: Hue | "off" = soc == null ? "off" : hueOf(soc);
    if (hue !== b.hue) {
      b.hue = hue;
      const off = hue === "off";
      b.shell.material = off ? this.shellOffMat : this.shellMat;
      b.edges.material = off ? this.edgeOffMat : this.edgeMat;
      if (!off) {
        b.fill.material = this.fillMat[hue];
        b.surf.material = this.surfMat[hue];
        b.poolMat.color.setHex(HUES[hue][2]);
      }
      if (bench) bench.hues[i] = hue;
    }
    const h = b3FillH(soc);
    b.fill.visible = b.surf.visible = h > 0.001;
    b.fill.scale.set(B3.w * 0.86, Math.max(h, 0.001), B3.d * 0.84);
    b.fill.position.y = B3.fillPad + h / 2;
    b.surf.scale.set(B3.w * 0.86, 1, B3.d * 0.84);
    b.surf.position.y = B3.fillPad + h + 0.002;
    b.poolMat.opacity = soc == null ? 0 : .14 + .34 * soc / 100;
    const t = pctText(pct), el = this.labels.pct[i];
    if (el && el.textContent !== t) el.textContent = t;
    if (bench) bench.levels[i] = pct == null ? null : Math.round(pct * 10) / 10;
  }

  // ── the loop: runs only while something moves ──
  private request(): void {
    if (!this.raf && !this.dead && !this.paused) this.raf = requestAnimationFrame(this.frame);
  }

  /** The page is hidden: no frames (a change while hidden is drawn on resume). Everything stays allocated. */
  pause(): void {
    this.paused = true;
    cancelAnimationFrame(this.raf); this.raf = 0; this.last = 0;
    clearTimeout(this.homeTimer);
    this.dragging = null;
  }
  /** The page is shown again: one frame now (and the loop while something moves). */
  resume(): void {
    if (!this.paused || this.dead) return;
    this.paused = false;
    this.request();
  }

  private readonly frame = (now: number): void => {
    this.raf = 0;
    if (this.dead) return;
    const dt = this.last ? Math.min(0.1, (now - this.last) / 1000) : 1 / 60;
    let more = false;
    const t = performance.now();
    for (let i = 0; i < this.batts.length; i++) {             // levels easing in
      const b = this.batts[i]!;
      if (!b.t0 || b.target == null) continue;
      const k = Math.min(1, (t - b.t0) / LEVEL_MS);
      this.draw(i, k >= 1 ? b.target : b.from + (b.target - b.from) * easeOutCubic(k));
      if (k >= 1) b.t0 = 0; else more = true;
    }
    if (this.flowDir && !this.reduced) {                         // dashes running with the current
      this.flowTex.offset.y = flowAdvance(this.flowTex.offset.y, this.flowDir, dt, this.flowDur);
      more = true;
    }
    if (!this.dragging && t - this.touchedAt >= ORBIT.idleMs && (this.az !== ORBIT.az || this.el !== ORBIT.el)) {   // ease home
      const k = this.reduced ? 0 : ORBIT.tau;
      this.az = k ? approach(this.az, ORBIT.az, dt, k) : ORBIT.az;
      this.el = k ? approach(this.el, ORBIT.el, dt, k) : ORBIT.el;
      if (Math.abs(this.az - ORBIT.az) < 1e-3 && Math.abs(this.el - ORBIT.el) < 1e-3) { this.az = ORBIT.az; this.el = ORBIT.el; }
      else more = true;
      this.camDirty = true;
    }
    if (this.camDirty) this.placeCamera();
    const tr = this.firstDone ? 0 : performance.now();
    this.renderer.render(this.scene, this.camera);
    if (!this.firstDone && bench) bench.ms.firstRender = Math.round(performance.now() - tr);
    if (bench) {
      bench.frames++;
      bench.calls = this.renderer.info.render.calls;
      bench.triangles = this.renderer.info.render.triangles;
    }
    if (!this.firstDone) {
      this.firstDone = true;
      if (bench) bench.firstFrameMs = Math.round(performance.now() - this.t0);
      this.hooks.onFirstFrame();
    }
    this.last = more ? now : 0;
    if (more) this.request();
  };

  private placeCamera(): void {
    this.camDirty = false;
    const [x, y, z] = orbitPos(this.az, this.el, this.dist, this.target.y);
    this.camera.position.set(x, y, z);
    this.camera.lookAt(this.target);
    this.camera.updateMatrixWorld();
    if (bench) bench.orbit = [Math.round(this.az * 1000) / 1000, Math.round(this.el * 1000) / 1000];
    // the HTML labels follow their anchors (crisp text, no font textures)
    const { w, h } = this.size, L = this.labels, n = this.n;
    const put = (el: HTMLElement | undefined, v: THREE.Vector3, fixY?: number) => {
      if (!el) return;
      this.tmp.copy(v).project(this.camera);
      const sx = (this.tmp.x + 1) / 2 * w, sy = fixY ?? (1 - this.tmp.y) / 2 * h;
      el.style.transform = `translate(${sx.toFixed(1)}px, ${sy.toFixed(1)}px)`;
    };
    for (let i = 0; i < n; i++) {
      put(L.pct[i], this.anchors[3 * i]!);
      put(L.name[i], this.anchors[3 * i + 1]!);
      put(L.foot[i], this.anchors[3 * i + 2]!, h);
    }
    put(L.plus, this.anchors[3 * n]!);
    put(L.minus, this.anchors[3 * n + 1]!);
  }

  private resize(): void {
    const w = this.host.clientWidth, h = this.host.clientHeight;
    if (!w || !h || (w === this.size.w && h === this.size.h)) return;
    this.size = { w, h };
    this.renderer.setSize(w, h, false);
    this.camera.aspect = w / h;
    // fit the platform's width and the bank's height (with room for the labels under it); aim a little low so
    // the buses sit near the top and the readings under the platform stay clear
    this.dist = fitDistance(b3Width(this.n) + 2.1, 2.9, w / h, B3.fov) * B3.zoom;
    this.camera.near = Math.max(0.1, this.dist - 6);
    this.camera.far = this.dist + 8;
    this.camera.updateProjectionMatrix();
    (this.scene.fog as THREE.Fog).near = this.dist;
    (this.scene.fog as THREE.Fog).far = this.dist + 7;
    this.camDirty = true;
    this.request();
  }

  // ── drag to orbit ──
  private readonly onDown = (e: PointerEvent) => {
    if (this.dragging || (e.pointerType === "mouse" && e.button !== 0)) return;
    this.dragging = { id: e.pointerId, x: e.clientX, y: e.clientY };
    try { this.canvas.setPointerCapture(e.pointerId); } catch { /* already released */ }
    this.touchedAt = performance.now();
  };
  private readonly onMove = (e: PointerEvent) => {
    const d = this.dragging;
    if (!d || d.id !== e.pointerId) return;
    [this.az, this.el] = clampOrbit(this.az - (e.clientX - d.x) * ORBIT.perPx, this.el + (e.clientY - d.y) * ORBIT.perPx * 0.8);
    d.x = e.clientX; d.y = e.clientY;
    this.touchedAt = performance.now();
    this.camDirty = true;
    this.request();
  };
  private readonly onUp = (e: PointerEvent) => {
    if (!this.dragging || this.dragging.id !== e.pointerId) return;
    this.dragging = null;
    this.touchedAt = performance.now();
    clearTimeout(this.homeTimer);
    this.homeTimer = setTimeout(() => this.request(), ORBIT.idleMs + 20);       // wake the loop to ease home
  };

  private readonly onLost = (e: Event) => {
    e.preventDefault();
    if (bench) bench.lost++;
    this.hooks.onLost();
  };

  dispose(): void {
    if (this.dead) return;
    this.dead = true;
    cancelAnimationFrame(this.raf);
    clearTimeout(this.homeTimer);
    this.ro?.disconnect();
    const c = this.canvas;
    c.removeEventListener("webglcontextlost", this.onLost);           // our own context loss below is not a failure
    c.removeEventListener("pointerdown", this.onDown);
    c.removeEventListener("pointermove", this.onMove);
    c.removeEventListener("pointerup", this.onUp);
    c.removeEventListener("pointercancel", this.onUp);
    for (const o of this.owned) o.dispose();
    if (bench) bench.res -= this.owned.length;
    this.owned.length = 0;
    if (this.ok) {
      this.renderer.dispose();
      this.renderer.forceContextLoss();
      if (bench) { bench.live--; bench.disposed++; }
    }
    c.remove();
  }
}

/** A cylinder of length `len` along +y, open-ended, whose v coordinate counts dash periods from its start. */
function tubeGeo(r: number, len: number): THREE.CylinderGeometry {
  const g = new THREE.CylinderGeometry(r, r, len, 8, 1, true);
  const pos = g.attributes.position!, uv = g.attributes.uv!;
  for (let i = 0; i < pos.count; i++) uv.setY(i, (pos.getY(i) + len / 2) / B3.dash);
  return g;
}
/** Put a tube made by tubeGeo() from a to b (scene / group coordinates). */
function place(m: THREE.Mesh, a: readonly number[], b: readonly number[]): THREE.Mesh {
  const from = new THREE.Vector3(a[0], a[1], a[2]), to = new THREE.Vector3(b[0], b[1], b[2]);
  m.position.copy(from).add(to).multiplyScalar(0.5);
  m.quaternion.setFromUnitVectors(Y, to.sub(from).normalize());
  return m;
}
/** Rectangles (w × d) at the given heights: the faint quarter seams inside each block. */
function seams(w: number, d: number, ys: number[]): THREE.BufferGeometry {
  const v: number[] = [], x = w / 2, z = d / 2;
  for (const y of ys) v.push(-x, y, z, x, y, z, x, y, z, x, y, -z, x, y, -z, -x, y, -z, -x, y, -z, -x, y, z);
  return new THREE.BufferGeometry().setAttribute("position", new THREE.Float32BufferAttribute(v, 3));
}
function dashTexture(): THREE.CanvasTexture {
  const c = document.createElement("canvas");
  c.width = 4; c.height = 36;
  const g = c.getContext("2d")!;
  g.fillStyle = "#fff";
  g.fillRect(0, 0, 4, 6);                                           // 1 : 5 on / off
  const t = new THREE.CanvasTexture(c);
  t.wrapS = t.wrapT = THREE.RepeatWrapping;
  return t;
}
/** A block's side: deep at the edges, bright with a thin highlight just right of the middle (the SVG's bg-side-*). */
function sideTexture([d0, d1, d2, d3]: readonly [number, number, number, number]): THREE.CanvasTexture {
  const c = document.createElement("canvas");
  c.width = 64; c.height = 2;
  const g = c.getContext("2d")!, gr = g.createLinearGradient(0, 0, 64, 0), hex = (v: number) => `#${v.toString(16).padStart(6, "0")}`;
  for (const [o, v] of [[0, d0], [.18, d1], [.5, d2], [.58, d3], [.68, d2], [.86, d1], [1, d0]] as const) gr.addColorStop(o, hex(v));
  g.fillStyle = gr;
  g.fillRect(0, 0, 64, 2);
  const t = new THREE.CanvasTexture(c);
  t.colorSpace = THREE.SRGBColorSpace;
  return t;
}
function radialTexture(): THREE.CanvasTexture {
  const c = document.createElement("canvas");
  c.width = c.height = 64;
  const g = c.getContext("2d")!, gr = g.createRadialGradient(32, 32, 0, 32, 32, 32);
  gr.addColorStop(0, "rgba(255,255,255,1)");
  gr.addColorStop(1, "rgba(255,255,255,0)");
  g.fillStyle = gr;
  g.fillRect(0, 0, 64, 64);
  return new THREE.CanvasTexture(c);
}
