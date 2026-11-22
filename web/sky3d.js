// 3D Starlink sky: the dish's obstruction map as a dome of light over a Sprinter whose nose points the way the
// dish faces. Drag to rotate, drag up/down to tilt, pinch (or wheel) to zoom; it auto-rotates when idle.
// three.js r160 (MIT) is vendored in ./vendor/three. Renders at ≤30 fps and only while visible.
import * as THREE from "./vendor/three/three.module.min.js";

const R = 10;                                   // dome radius (scene units ≈ metres for the van)
const C = { clear: 0x47d7ff, obst: 0xff6a4a, ring: 0x78bee6, beam: 0x8ef7e6, amber: 0xfbbf24,
            body: 0xe4e9ef, glass: 0x0b1524, tire: 0x0d0f12, trim: 0x1a1e24, light: 0xfff1c2 };
const rad = d => d * Math.PI / 180;
// azimuth (° clockwise from north) + elevation (°) → unit vector; north = −z, east = +x, up = +y
const dir = (az, el) => new THREE.Vector3(Math.cos(rad(el)) * Math.sin(rad(az)), Math.sin(rad(el)), -Math.cos(rad(el)) * Math.cos(rad(az)));

function canvasTex(w, h, draw) {
  const c = document.createElement("canvas"); c.width = w; c.height = h;
  draw(c.getContext("2d"), w, h);
  const t = new THREE.CanvasTexture(c); t.colorSpace = THREE.SRGBColorSpace; return t;
}
function label(text, color, px = 1.3) {
  const s = new THREE.Sprite(new THREE.SpriteMaterial({ depthWrite: false, transparent: true,
    map: canvasTex(128, 128, (g) => { g.font = '700 72px "Noto Sans", sans-serif'; g.textAlign = "center"; g.textBaseline = "middle";
      g.shadowColor = color; g.shadowBlur = 18; g.fillStyle = color; g.fillText(text, 64, 68); }) }));
  s.scale.set(px, px, 1); return s;
}
function line(pts, color, opacity, dashed = false) {
  const geo = new THREE.BufferGeometry().setFromPoints(pts);
  const mat = dashed ? new THREE.LineDashedMaterial({ color, transparent: true, opacity, dashSize: .3, gapSize: .3, depthWrite: false })
                     : new THREE.LineBasicMaterial({ color, transparent: true, opacity, depthWrite: false });
  const l = new THREE.Line(geo, mat); if (dashed) l.computeLineDistances(); return l;
}
const ringPts = (el, r = R) => { const p = []; for (let a = 0; a <= 360; a += 2) p.push(dir(a, el).multiplyScalar(r)); return p; };

// Sky cells: soft additive dots with per-point twinkle, obstruction pulse and a horizon→zenith scan sweep.
const skyMaterial = () => new THREE.ShaderMaterial({
  uniforms: { uTime: { value: 0 }, uScale: { value: 300 } },
  vertexShader: `
    attribute vec3 tint; attribute float size; attribute float phase; attribute float obst;
    uniform float uTime, uScale; varying vec3 vC;
    void main() {
      vec4 mv = modelViewMatrix * vec4(position, 1.0);
      float el = asin(clamp(position.y / ${R.toFixed(1)}, 0.0, 1.0));
      float sweepEl = fract(uTime / 7.0) * 1.9 - 0.15;                      // radians, horizon → zenith
      float s = exp(-pow((el - sweepEl) * 10.0, 2.0));
      float tw = 0.72 + 0.28 * sin(uTime * (1.2 + phase * 1.7) + phase * 6.2831);
      float pulse = 1.0 + obst * 0.45 * sin(uTime * 2.4 + phase * 6.2831);
      vec3 n = normalize((modelViewMatrix * vec4(position, 0.0)).xyz);
      float near = smoothstep(-0.1, 0.7, dot(n, normalize(-mv.xyz)));      // 1 on the side facing the camera
      vC = tint * (tw * pulse + s * (1.4 - obst)) * mix(1.0, mix(0.28, 1.0, obst), near);
      gl_PointSize = size * pulse * (1.0 + s * 0.7) * uScale / -mv.z;
      gl_Position = projectionMatrix * mv;
    }`,
  fragmentShader: `
    varying vec3 vC;
    void main() {
      float d = length(gl_PointCoord - 0.5);
      float a = smoothstep(0.5, 0.0, d); a *= a;
      gl_FragColor = vec4(vC * a, a);
      #include <colorspace_fragment>
    }`,
  transparent: true, depthWrite: false, blending: THREE.AdditiveBlending,
});

// Glass dome: fresnel rim glow, nearly clear where you look straight through it.
const domeMaterial = () => new THREE.ShaderMaterial({
  uniforms: { uColor: { value: new THREE.Color(C.ring) } },
  vertexShader: `varying vec3 vN, vV; varying float vY;
    void main() { vec4 mv = modelViewMatrix * vec4(position, 1.0); vN = normalize(normalMatrix * normal); vV = normalize(-mv.xyz);
      vY = position.y / ${R.toFixed(1)}; gl_Position = projectionMatrix * mv; }`,
  fragmentShader: `uniform vec3 uColor; varying vec3 vN, vV; varying float vY;
    void main() { float f = pow(1.0 - abs(dot(normalize(vN), normalize(vV))), 3.0);
      float a = (0.025 + f * 0.5) * smoothstep(-0.02, 0.06, vY);
      gl_FragColor = vec4(uColor * a, a);
      #include <colorspace_fragment>
    }`,
  transparent: true, depthWrite: false, blending: THREE.AdditiveBlending, side: THREE.DoubleSide,
});

// Beam: fades out with height.
const beamMaterial = () => new THREE.ShaderMaterial({
  uniforms: { uColor: { value: new THREE.Color(C.beam) }, uLen: { value: R * .92 } },
  vertexShader: `varying float vH; uniform float uLen; void main() { vH = position.y / uLen; gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0); }`,
  fragmentShader: `uniform vec3 uColor; varying float vH;
    void main() { float a = 0.32 * pow(1.0 - clamp(vH, 0.0, 1.0), 1.6); gl_FragColor = vec4(uColor * a, a);
      #include <colorspace_fragment>
    }`,
  transparent: true, depthWrite: false, blending: THREE.AdditiveBlending, side: THREE.DoubleSide,
});

// A stylised Mercedes Sprinter 170 high roof (~5.9 m × 2.0 m × 2.8 m), nose toward −z before rotation.
function sprinter() {
  const van = new THREE.Group();
  const paint = new THREE.MeshStandardMaterial({ color: C.body, metalness: .45, roughness: .32 });
  const glass = new THREE.MeshStandardMaterial({ color: C.glass, metalness: .8, roughness: .1, emissive: 0x0b2a4a, emissiveIntensity: .5, side: THREE.DoubleSide });
  const trim = new THREE.MeshStandardMaterial({ color: C.trim, metalness: .3, roughness: .7 });
  const tire = new THREE.MeshStandardMaterial({ color: C.tire, roughness: .95 });
  const rim = new THREE.MeshStandardMaterial({ color: 0x9aa4b0, metalness: .9, roughness: .3 });
  const add = (geo, m, x, y, z, rx = 0, ry = 0, rz = 0) => { const o = new THREE.Mesh(geo, m); o.position.set(x, y, z); o.rotation.set(rx, ry, rz); van.add(o); return o; };
  const box = (w, h, d, m, x, y, z) => add(new THREE.BoxGeometry(w, h, d), m, x, y, z);
  const L = 5.9, W = 2.0, base = .42, roof = 2.78, cabL = 1.45;
  const zF = -L / 2, zCab = zF + cabL;                                  // nose, cab/box seam
  // cargo box with chamfered roof edges (cross-section extruded along the van, +z = rearward)
  const sec = new THREE.Shape(); const hw = W / 2, ch = .14;
  sec.moveTo(-hw, base); sec.lineTo(hw, base); sec.lineTo(hw, roof - ch); sec.lineTo(hw - ch, roof); sec.lineTo(-hw + ch, roof); sec.lineTo(-hw, roof - ch); sec.closePath();
  const body = new THREE.Mesh(new THREE.ExtrudeGeometry(sec, { depth: L - cabL, bevelEnabled: false }), paint);
  body.position.z = zCab; van.add(body);
  // cab side profile (x = distance back from the nose, y = height), extruded across the width
  const prof = new THREE.Shape();
  prof.moveTo(0, base); prof.lineTo(cabL, base); prof.lineTo(cabL, roof - .02); prof.lineTo(cabL - .12, roof - .02);
  prof.lineTo(.78, 1.55); prof.lineTo(.08, 1.18); prof.lineTo(0, 1.0); prof.closePath();
  const cab = new THREE.Mesh(new THREE.ExtrudeGeometry(prof, { depth: W - .04, bevelEnabled: true, bevelSize: .04, bevelThickness: .04, bevelSegments: 2 }), paint);
  cab.rotation.y = -Math.PI / 2; cab.position.set(hw - .02, 0, zF); van.add(cab);    // profile x → +z, extrusion → −x
  // windscreen along the rake: from (0.78 back, 1.55 up) to (cabL-0.12 back, roof)
  const p0 = new THREE.Vector3(0, 1.55, zF + .78), p1 = new THREE.Vector3(0, roof - .02, zF + cabL - .12);
  const ws = add(new THREE.PlaneGeometry(W - .22, p0.distanceTo(p1) * .9), glass, 0, 0, 0);
  ws.position.copy(p0).add(p1).multiplyScalar(.5).add(new THREE.Vector3(0, .01, -.012));
  ws.rotation.x = -Math.atan2(p1.z - p0.z, p1.y - p0.y);
  for (const s of [-1, 1]) {
    box(.03, .62, .78, glass, s * (hw + .015), 1.85, zF + 1.0);             // cab door windows
    box(.03, .55, 2.9, glass, s * (hw + .015), 1.95, 1.0);                  // rear side windows
    box(.03, .1, L - .3, trim, s * (hw + .02), .62, .1);                    // rubbing strip
    box(.2, .14, .05, trim, s * (hw + .12), 1.6, zF + .78);                 // mirrors
  }
  box(W + .04, .32, .12, trim, 0, .58, zF - .02);                          // front bumper
  box(W + .04, .28, .12, trim, 0, .58, L / 2 + .04);                       // rear bumper
  box(1.1, .36, .04, trim, 0, .98, zF - .01);                              // grille
  box(.03, 1.9, .03, trim, 0, 1.45, L / 2 + .005);                         // rear door seam
  for (const [x, z] of [[-1, zF + .95], [1, zF + .95], [-1, 1.95], [1, 1.95]]) {
    add(new THREE.CylinderGeometry(.37, .37, .26, 28), tire, x * (hw - .08), .37, z, 0, 0, Math.PI / 2);
    add(new THREE.CylinderGeometry(.2, .2, .27, 16), rim, x * (hw - .08), .37, z, 0, 0, Math.PI / 2);
  }
  const hl = new THREE.MeshStandardMaterial({ color: C.light, emissive: C.light, emissiveIntensity: 3 });
  const tl = new THREE.MeshStandardMaterial({ color: 0xff2a20, emissive: 0xff2a20, emissiveIntensity: 2 });
  for (const s of [-1, 1]) { box(.4, .13, .04, hl, s * .74, 1.08, zF - .02); box(.1, .5, .04, tl, s * (hw - .07), 1.2, L / 2 + .02); }
  // headlight throw on the ground (canvas top = forward after laying the plane flat)
  const throwTex = canvasTex(256, 256, (g, w, h) => {
    const grd = g.createLinearGradient(0, h, 0, 0); grd.addColorStop(0, "rgba(255,236,180,.55)"); grd.addColorStop(1, "rgba(255,236,180,0)");
    g.fillStyle = grd; g.beginPath(); g.moveTo(w * .36, h); g.lineTo(w * .64, h); g.lineTo(w, 0); g.lineTo(0, 0); g.closePath(); g.fill();
  });
  add(new THREE.PlaneGeometry(4.2, 5.5), new THREE.MeshBasicMaterial({ map: throwTex, transparent: true, depthWrite: false, blending: THREE.AdditiveBlending }),
      0, .02, zF - 2.75, -Math.PI / 2);
  // Starlink panel on the roof (pivots about its centre to face the boresight)
  const dish = new THREE.Group(); dish.position.set(0, roof + .14, .7); van.add(dish);
  dish.add(new THREE.Mesh(new THREE.BoxGeometry(.56, .035, .82),
    new THREE.MeshStandardMaterial({ color: 0xf6f8fb, roughness: .45, metalness: .1, emissive: 0x7fdcff, emissiveIntensity: .25 })));
  const mast = new THREE.Mesh(new THREE.CylinderGeometry(.04, .05, .14, 10), trim); mast.position.y = -.08; dish.add(mast);
  van.userData.dish = dish;
  return van;
}

export class Sky3D {
  constructor() {
    this.el = document.createElement("div");
    this.el.className = "sky3d";
    this.ok = false;
    try {
      this.renderer = new THREE.WebGLRenderer({ antialias: true, alpha: true, powerPreference: "low-power" });
    } catch (e) { return; }                     // no WebGL: caller keeps the 2D map
    this.ok = true;
    const r = this.renderer;
    r.setPixelRatio(Math.min(window.devicePixelRatio || 1, 1.5));
    r.setClearColor(0x000000, 0);
    r.toneMapping = THREE.ACESFilmicToneMapping; r.toneMappingExposure = 1.15;
    this.el.appendChild(r.domElement);
    this.hint = document.createElement("div"); this.hint.className = "sky3d-hint"; this.el.appendChild(this.hint);
    this.scene = new THREE.Scene();
    this.camera = new THREE.PerspectiveCamera(40, 1, .1, 300);
    this.yaw = rad(30); this.pitch = rad(24); this.dist = 26; this.target = new THREE.Vector3(0, 3.2, 0);
    this.idleSince = -1e9; this.running = false; this.lastT = 0; this.clock = 0;
    this.origin = new THREE.Vector3(0, 2.9, 0);
    this._build(); this._controls();
    new ResizeObserver(() => this._resize()).observe(this.el);
  }

  _build() {
    const s = this.scene;
    s.add(new THREE.HemisphereLight(0xcfeeff, 0x08101c, 1.1));
    const key = new THREE.DirectionalLight(0xffffff, 2.2); key.position.set(8, 14, 10); s.add(key);
    const rimL = new THREE.DirectionalLight(0x5de3ff, 1.6); rimL.position.set(-10, 6, -12); s.add(rimL);
    // ground glow + polar grid
    const groundTex = canvasTex(512, 512, (g, w) => {
      const grd = g.createRadialGradient(w / 2, w / 2, 0, w / 2, w / 2, w / 2);
      grd.addColorStop(0, "rgba(40,92,140,.55)"); grd.addColorStop(.55, "rgba(18,44,72,.4)"); grd.addColorStop(.8, "rgba(10,24,40,.25)"); grd.addColorStop(1, "rgba(6,11,19,0)");
      g.fillStyle = grd; g.fillRect(0, 0, w, w);
    });
    const ground = new THREE.Mesh(new THREE.CircleGeometry(R * 1.6, 96), new THREE.MeshBasicMaterial({ map: groundTex, transparent: true, depthWrite: false }));
    ground.rotation.x = -Math.PI / 2; s.add(ground);
    const grid = new THREE.PolarGridHelper(R, 12, 6, 128, 0x2a5a86, 0x173250);
    grid.material.transparent = true; grid.material.opacity = .45; grid.material.depthWrite = false; grid.position.y = .01; s.add(grid);
    // dome furniture: glass shell, horizon + 30°/60° rings, meridians, compass labels
    s.add(new THREE.Mesh(new THREE.SphereGeometry(R, 64, 32, 0, Math.PI * 2, 0, Math.PI / 2), domeMaterial()));
    s.add(line(ringPts(0), C.clear, .7));
    s.add(line(ringPts(0, R * 1.012), C.clear, .25));
    s.add(line(ringPts(30), C.ring, .28, true));
    s.add(line(ringPts(60), C.ring, .28, true));
    for (let a = 0; a < 360; a += 30) {
      const p = []; for (let e = 0; e <= 90; e += 2) p.push(dir(a, e).multiplyScalar(R));
      s.add(line(p, C.ring, a % 90 ? .06 : .16));
    }
    for (const [t, a] of [["N", 0], ["E", 90], ["S", 180], ["W", 270]]) {
      const l = label(t, a === 0 ? "#5de3ff" : "#9fb3cc", a === 0 ? 1.6 : 1.25); l.position.copy(dir(a, 0).multiplyScalar(R + 1.1)).setY(.55); s.add(l);
    }
    for (const e of [30, 60]) { const l = label(`${e}°`, "#6f87a5", .8); l.position.copy(dir(225, e).multiplyScalar(R + .35)); s.add(l); }
    // starfield
    const sp = []; let seed = 7; const rnd = () => (seed = (seed * 16807) % 2147483647) / 2147483647;
    for (let i = 0; i < 900; i++) { const v = dir(rnd() * 360, Math.asin(rnd()) * 180 / Math.PI).multiplyScalar(90 + rnd() * 30); sp.push(v.x, v.y - 4, v.z); }
    const stars = new THREE.BufferGeometry(); stars.setAttribute("position", new THREE.Float32BufferAttribute(sp, 3));
    s.add(new THREE.Points(stars, new THREE.PointsMaterial({ color: 0xbfd8ff, size: 1.6, sizeAttenuation: false, transparent: true, opacity: .55, depthWrite: false })));
    // sky cells (filled by setMap)
    this.skyMat = skyMaterial();
    this.points = new THREE.Points(new THREE.BufferGeometry(), this.skyMat); this.points.frustumCulled = false; s.add(this.points);
    // van, heading line + chevron on the horizon, beam, pulse ring
    this.van = sprinter(); this.van.scale.setScalar(.82); s.add(this.van);
    this.heading = new THREE.Group(); s.add(this.heading);
    this.heading.add(line([new THREE.Vector3(0, .03, -3), new THREE.Vector3(0, .03, -R)], C.amber, .7, true));
    const chev = new THREE.Shape(); chev.moveTo(0, .7); chev.lineTo(.42, 0); chev.lineTo(.18, 0); chev.lineTo(0, .38); chev.lineTo(-.18, 0); chev.lineTo(-.42, 0); chev.closePath();
    const cm = new THREE.Mesh(new THREE.ShapeGeometry(chev), new THREE.MeshBasicMaterial({ color: C.amber, transparent: true, opacity: .95, side: THREE.DoubleSide, depthWrite: false }));
    cm.rotation.x = -Math.PI / 2; cm.position.set(0, .04, -R - .25); this.heading.add(cm);   // shape +y → −z (outward)
    const beamGeo = new THREE.CylinderGeometry(.03, .7, R * .92, 32, 1, true); beamGeo.translate(0, R * .46, 0);
    this.beam = new THREE.Mesh(beamGeo, beamMaterial()); s.add(this.beam);
    this.pulse = new THREE.Mesh(new THREE.TorusGeometry(.4, .025, 8, 48), new THREE.MeshBasicMaterial({ color: C.beam, transparent: true, blending: THREE.AdditiveBlending, depthWrite: false }));
    s.add(this.pulse);
  }

  /** map: state starlink.obstruction_map; az/el: dish boresight (direction_azimuth / direction_elevation). */
  setMap(map, az, el) {
    if (!this.ok) return;
    const key = map?.data ? map.data.length + map.data.slice(0, 64) + map.data.slice(-256) + "|" + Math.round(az ?? 0) : "";
    if (key && key !== this._key) {
      this._key = key;
      const raw = atob(map.data), c0 = map.rows / 2, maxT = map.max_theta || 80;
      const rot = map.frame === "dish" ? (az ?? 180) - 180 : 0;       // same orientation as the verified 2D map
      const pos = [], tint = [], size = [], phase = [], obst = [];
      const clear = new THREE.Color(C.clear), hot = new THREE.Color(C.obst), ember = new THREE.Color(0xffc06a), tmp = new THREE.Color();
      for (let r = 0; r < map.rows; r++) for (let c = 0; c < map.cols; c++) {
        const v = raw.charCodeAt(r * map.cols + c);
        if (v === 255) continue;
        const dx = c + .5 - c0, dy = r + .5 - c0, rr = Math.hypot(dx, dy) / c0;
        if (rr > 1) continue;
        const azc = (Math.atan2(dx, -dy) * 180 / Math.PI + rot + 360) % 360, elc = 90 - rr * maxT;
        const p = dir(azc, elc).multiplyScalar(R); pos.push(p.x, p.y, p.z);
        const q = v / 254, o = q < .95 ? 1 : 0;                       // < 95 % quality reads as obstructed
        if (o) { const sev = 1 - q, k = .9 + 1.6 * sev;                 // severity drives size + heat
                 tmp.copy(hot).lerp(ember, q * .6); tint.push(tmp.r * k, tmp.g * k, tmp.b * k); size.push(.26 + .42 * sev); }
        else { tint.push(clear.r * .42, clear.g * .42, clear.b * .42); size.push(.2); }
        phase.push(Math.random()); obst.push(o);
      }
      const g = this.points.geometry;
      g.setAttribute("position", new THREE.Float32BufferAttribute(pos, 3));
      g.setAttribute("tint", new THREE.Float32BufferAttribute(tint, 3));
      g.setAttribute("size", new THREE.Float32BufferAttribute(size, 1));
      g.setAttribute("phase", new THREE.Float32BufferAttribute(phase, 1));
      g.setAttribute("obst", new THREE.Float32BufferAttribute(obst, 1));
    }
    if (az != null && (az !== this._az || el !== this._el)) {
      const first = this._az == null;
      this._az = az; this._el = el;
      const e = el ?? 90, d = dir(az, e);
      this.van.rotation.y = -rad(az);                                  // nose points where the dish faces
      this.heading.rotation.y = -rad(az);
      const dish = this.van.userData.dish; dish.rotation.x = -rad(90 - e);   // panel normal → boresight
      this.van.updateMatrixWorld(true);
      this.origin = dish.getWorldPosition(new THREE.Vector3());
      this.beam.quaternion.setFromUnitVectors(new THREE.Vector3(0, 1, 0), d);
      this.beam.position.copy(this.origin);
      this.pulse.quaternion.setFromUnitVectors(new THREE.Vector3(0, 0, 1), d);
      this.dirVec = d;
      this.hint.innerHTML = `Dish faces <b>${Math.round(az)}°</b> · tilt <b>${Math.round(90 - e)}°</b>`;
      if (first) this.yaw = rad(180 - az - 35);                       // open on the van's front quarter
    }
  }

  _controls() {
    const el = this.renderer.domElement, ptr = new Map();
    let pinch0 = 0, dist0 = 0;
    el.style.touchAction = "none";
    const poke = () => { this.idleSince = performance.now(); };
    el.addEventListener("pointerdown", e => { el.setPointerCapture(e.pointerId); ptr.set(e.pointerId, [e.clientX, e.clientY]); poke(); });
    el.addEventListener("pointermove", e => {
      if (!ptr.has(e.pointerId)) return;
      const [px, py] = ptr.get(e.pointerId); ptr.set(e.pointerId, [e.clientX, e.clientY]);
      if (ptr.size === 1) {
        this.yaw -= (e.clientX - px) * .008;
        this.pitch = Math.min(rad(88), Math.max(rad(3), this.pitch + (e.clientY - py) * .006));
      } else if (ptr.size === 2) {
        const [a, b] = [...ptr.values()], d = Math.hypot(a[0] - b[0], a[1] - b[1]);
        if (!pinch0) { pinch0 = d; dist0 = this.dist; }
        this.dist = Math.min(48, Math.max(9, dist0 * pinch0 / Math.max(1, d)));
      }
      poke();
    });
    const up = e => { ptr.delete(e.pointerId); if (ptr.size < 2) pinch0 = 0; poke(); };
    el.addEventListener("pointerup", up); el.addEventListener("pointercancel", up);
    el.addEventListener("wheel", e => { e.preventDefault(); this.dist = Math.min(48, Math.max(9, this.dist * (1 + e.deltaY * .001))); poke(); }, { passive: false });
    el.addEventListener("dblclick", () => { this.pitch = rad(24); this.dist = 26; if (this._az != null) this.yaw = rad(180 - this._az - 35); poke(); });
  }

  _resize() {
    const w = this.el.clientWidth, h = this.el.clientHeight;
    if (!w || !h) return;
    this.renderer.setSize(w, h, false);
    this.camera.aspect = w / h; this.camera.updateProjectionMatrix();
    this.skyMat.uniforms.uScale.value = h * this.renderer.getPixelRatio() / (2 * Math.tan(rad(this.camera.fov / 2)));
  }

  start() {
    if (!this.ok || this.running) return;
    this.running = true; this.lastT = 0;
    const loop = t => {
      if (!this.running) return;
      if (document.hidden || !this.el.isConnected || this.el.offsetParent === null) { this.running = false; return; }
      this.raf = requestAnimationFrame(loop);
      if (t - this.lastT < 31) return;                                 // ≈30 fps cap
      const dt = this.lastT ? Math.min(.1, (t - this.lastT) / 1000) : 0; this.lastT = t; this.clock += dt;
      if (t - this.idleSince > 6000) this.yaw += dt * rad(4);          // gentle auto-rotate when idle
      const cp = Math.cos(this.pitch);
      this.camera.position.set(this.target.x + this.dist * cp * Math.sin(this.yaw), this.target.y + this.dist * Math.sin(this.pitch),
                               this.target.z + this.dist * cp * Math.cos(this.yaw));
      this.camera.lookAt(this.target);
      this.skyMat.uniforms.uTime.value = this.clock;
      if (this.dirVec) {                                               // a ring of light travelling up the beam
        const f = (this.clock / 1.8) % 1;
        this.pulse.position.copy(this.dirVec).multiplyScalar(R * .9 * f).add(this.origin);
        this.pulse.scale.setScalar(.3 + f * 1.6);
        this.pulse.material.opacity = .85 * (1 - f);
      }
      this.renderer.render(this.scene, this.camera);
    };
    this.raf = requestAnimationFrame(loop);
  }

  stop() { this.running = false; if (this.raf) cancelAnimationFrame(this.raf); }
}
