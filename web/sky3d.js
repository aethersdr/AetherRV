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

// The owner's van: a lifted silver Mercedes Sprinter VS30 4x4 high roof (144" wb, ~5.9 m), nose toward −z before
// rotation. Long sloping hood + raked windshield, charcoal lower cladding, black steel bumper with light bar and
// winch, 33" all-terrains on black wheels, black roof rack with a light-bar fairing, solar panels, the Starlink
// panel, a driver-side rear ladder and a spare on the rear door. Van left (driver) = −x.
function sprinter() {
  const van = new THREE.Group();
  const M = (o) => new THREE.MeshStandardMaterial(o);
  const paint = M({ color: 0xd4dae1, metalness: .3, roughness: .32 });
  const clad = M({ color: 0x3b4048, metalness: .2, roughness: .75, side: THREE.DoubleSide });
  const black = M({ color: 0x15181c, metalness: .45, roughness: .55 });
  const glass = M({ color: 0x0a1320, metalness: .85, roughness: .08, emissive: 0x0b2a4a, emissiveIntensity: .45, side: THREE.DoubleSide });
  const tire = M({ color: 0x0c0d0f, roughness: .95 });
  const wheel = M({ color: 0x23272d, metalness: .6, roughness: .45 });
  const chrome = M({ color: 0xe8edf2, metalness: 1, roughness: .15 });
  const led = M({ color: 0xffffff, emissive: 0xeaf6ff, emissiveIntensity: 2.6 });
  const head = M({ color: C.light, emissive: C.light, emissiveIntensity: 3 });
  const tail = M({ color: 0xff2a20, emissive: 0xff2a20, emissiveIntensity: 2 });
  const solar = M({ color: 0x1b2a44, metalness: .6, roughness: .25, emissive: 0x10203a, emissiveIntensity: .4 });
  const add = (geo, m, x, y, z, rx = 0, ry = 0, rz = 0, parent = van) => { const o = new THREE.Mesh(geo, m); o.position.set(x, y, z); o.rotation.set(rx, ry, rz); parent.add(o); return o; };
  const box = (w, h, d, m, x, y, z) => add(new THREE.BoxGeometry(w, h, d), m, x, y, z);
  const L = 5.9, W = 2.02, hw = W / 2, sill = .62, roof = 2.92, zF = -L / 2;
  const Z = u => zF + u;                                  // u = metres back from the nose
  // one-piece side silhouette (u back, y up), extruded across the width with rounded edges
  const bv = .09, sil = new THREE.Shape();
  [[0, .72], [0, 1.02], [.12, 1.17], [1.02, 1.4], [1.86, 2.44], [2.02, 2.72], [2.3, roof], [L - .06, roof], [L, roof - .06], [L, sill], [.06, sill]]
    .forEach(([u, y], i) => i ? sil.lineTo(u, y) : sil.moveTo(u, y));
  sil.closePath();
  const body = new THREE.Mesh(new THREE.ExtrudeGeometry(sil, { depth: W - 2 * bv, bevelEnabled: true, bevelSize: bv, bevelThickness: bv, bevelSegments: 4 }), paint);
  body.rotation.y = -Math.PI / 2; body.position.set(hw - bv, 0, zF); van.add(body);   // shape u → +z, extrusion → −x
  // windshield on the rake
  const a = new THREE.Vector3(0, 1.42, Z(1.06)), b = new THREE.Vector3(0, 2.42, Z(1.84));
  const ws = add(new THREE.PlaneGeometry(W - .3, a.distanceTo(b) * .9), glass, 0, 0, 0);
  const rake = Math.atan2(b.z - a.z, b.y - a.y);                       // plane's +y runs up the rake
  ws.rotation.x = rake;
  ws.position.copy(a).add(b).multiplyScalar(.5)
    .add(new THREE.Vector3(0, Math.sin(rake), -Math.cos(rake)).multiplyScalar(bv + .015));   // outside the bevel
  // side glass + cladding + flares, both sides (flat shapes laid on the body side)
  const side = (pts, m, s, off = .012) => {
    const sh = new THREE.Shape(); pts.forEach(([u, y], i) => i ? sh.lineTo(u, y) : sh.moveTo(u, y)); sh.closePath();
    add(new THREE.ShapeGeometry(sh), m, s * (hw + off), 0, zF, 0, -Math.PI / 2);
  };
  for (const s of [-1, 1]) {
    side([[1.28, 1.66], [1.86, 2.34], [2.62, 2.34], [2.62, 1.66]], glass, s);                 // cab door
    side([[3.0, 1.7], [3.0, 2.36], [4.25, 2.36], [4.25, 1.7]], glass, s);                     // big quarter window
    side([[4.62, 2.0], [4.62, 2.36], [5.35, 2.36], [5.35, 2.0]], glass, s);                   // small rear window
    side([[.15, sill], [.15, 1.02], [L - .08, 1.02], [L - .08, sill]], clad, s, .008);       // two-tone lower cladding
    box(.22, .16, .06, black, s * (hw + .14), 1.78, Z(1.32));                                 // mirror head
    box(.18, .05, .05, black, s * (hw + .06), 1.72, Z(1.32));                                 // mirror arm
    box(.04, .05, .2, chrome, s * (hw + .02), 1.42, Z(2.25));                                 // door handle
  }
  // wheels: 33" all-terrains on black wheels, black arch flares
  for (const [s, u] of [[-1, .98], [1, .98], [-1, 4.62], [1, 4.62]]) {
    const x = s * (hw - .06), z = Z(u);
    add(new THREE.CylinderGeometry(.43, .43, .32, 32), tire, x, .43, z, 0, 0, Math.PI / 2);
    add(new THREE.TorusGeometry(.4, .045, 6, 32), tire, x + s * .16, .43, z, 0, Math.PI / 2, 0);      // sidewall bulge
    add(new THREE.CylinderGeometry(.27, .27, .33, 20), wheel, x, .43, z, 0, 0, Math.PI / 2);
    add(new THREE.TorusGeometry(.52, .075, 8, 28, Math.PI), black, s * (hw + .02), .45, z, 0, Math.PI / 2, 0);   // arch flare
  }
  // front: grille + star, swept headlights, black steel bumper with hoop, light bar and winch
  const nose = Z(0) - bv;                                               // front face, outside the bevel
  box(1.0, .3, .04, black, 0, .9, nose - .01);                          // grille
  box(.96, .025, .02, chrome, 0, .96, nose - .035);
  add(new THREE.TorusGeometry(.085, .012, 6, 24), chrome, 0, .9, nose - .04);   // star ring
  for (const s of [-1, 1]) add(new THREE.BoxGeometry(.34, .09, .05), head, s * .7, 1.08, nose + .02, 0, s * .45, s * -.1);  // swept headlights
  box(W + .12, .42, .32, black, 0, .66, Z(0) - .14);                                         // bumper
  for (const s of [-1, 1]) box(.07, .42, .07, black, s * .55, 1.02, Z(0) - .2);              // hoop uprights
  box(1.17, .07, .07, black, 0, 1.22, Z(0) - .2);                                            // hoop top
  box(.82, .07, .05, led, 0, 1.13, Z(0) - .24);                                              // bumper light bar
  box(.3, .14, .1, M({ color: 0x9aa1a9, metalness: .8, roughness: .35 }), 0, .62, Z(0) - .33);  // winch fairlead
  box(.08, .08, .04, M({ color: 0xd33, roughness: .5 }), 0, .62, Z(0) - .39);                // red hook
  for (const s of [-1, 1]) box(.09, .09, .03, led, s * .78, .72, Z(0) - .31);                // bumper pods
  // rear: bumper, tall tail lights, spare on the rear door
  box(W + .04, .3, .16, black, 0, .7, L / 2 + .06);
  for (const s of [-1, 1]) box(.1, .62, .04, tail, s * (hw - .06), 1.25, L / 2 + .02);
  add(new THREE.CylinderGeometry(.41, .41, .28, 28), tire, -.42, 1.45, L / 2 + .2, Math.PI / 2, 0, 0);
  add(new THREE.CylinderGeometry(.25, .25, .29, 18), wheel, -.42, 1.45, L / 2 + .2, Math.PI / 2, 0, 0);
  // roof rack: black deck over the roof, front fairing with a double-row light bar, solar panels
  const rackY = roof + .16;
  box(W + .06, .05, L - 2.1, black, 0, rackY, Z(2.0 + (L - 2.1) / 2));
  for (const s of [-1, 1]) box(.06, .14, L - 2.1, black, s * (hw + .03), rackY + .03, Z(2.0 + (L - 2.1) / 2));
  for (let u = 2.4; u < L - .1; u += .9) for (const s of [-1, 1]) box(.06, .16, .08, black, s * (hw - .2), roof + .08, Z(u));  // towers
  const fair = add(new THREE.BoxGeometry(W + .1, .34, .36), black, 0, rackY - .02, Z(1.86));
  fair.rotation.x = -.12;
  for (const y of [-.07, .07]) add(new THREE.BoxGeometry(W - .3, .07, .03), led, 0, rackY - .02 + y, Z(1.67));
  box(.03, .45, .03, black, -.5, rackY + .25, Z(2.1));                                       // antenna whip
  box(.05, .2, .05, black, .82, rackY + .12, Z(5.3));                                        // puck antenna
  box(W - .3, .04, 1.15, solar, 0, rackY + .05, Z(2.85));                                    // solar panel (front)
  box(W - .3, .04, 1.05, solar, 0, rackY + .05, Z(5.05));                                    // solar panel (rear)
  // driver-side rear ladder (rails + rungs)
  const lx = -(hw + .08);
  for (const u of [4.95, 5.35]) box(.05, roof - .55, .05, black, lx, (roof + .55) / 2 + .05, Z(u));
  for (let y = .85; y < roof; y += .3) box(.05, .04, .4, black, lx, y, Z(5.15));
  // Starlink panel on the rack, between the solar panels (pivots about its centre to face the boresight)
  const dish = new THREE.Group(); dish.position.set(0, rackY + .16, Z(3.95)); van.add(dish);
  dish.add(new THREE.Mesh(new THREE.BoxGeometry(.56, .035, .82),
    M({ color: 0xf6f8fb, roughness: .45, metalness: .1, emissive: 0x7fdcff, emissiveIntensity: .25 })));
  const mast = new THREE.Mesh(new THREE.CylinderGeometry(.04, .05, .14, 10), black); mast.position.y = -.08; dish.add(mast);
  // headlight + light-bar throw on the ground (canvas top = forward once the plane lies flat)
  const throwTex = canvasTex(256, 256, (g, w, h) => {
    const grd = g.createLinearGradient(0, h, 0, 0); grd.addColorStop(0, "rgba(255,240,200,.6)"); grd.addColorStop(1, "rgba(255,240,200,0)");
    g.fillStyle = grd; g.beginPath(); g.moveTo(w * .34, h); g.lineTo(w * .66, h); g.lineTo(w, 0); g.lineTo(0, 0); g.closePath(); g.fill();
  });
  add(new THREE.PlaneGeometry(4.6, 6), new THREE.MeshBasicMaterial({ map: throwTex, transparent: true, depthWrite: false, blending: THREE.AdditiveBlending }),
      0, .02, zF - 3.2, -Math.PI / 2);
  van.userData.dish = dish;
  return van;
}

export class Sky3D {
  constructor() {
    this.el = document.createElement("div");
    this.el.className = "sky3d";
    this.el.dataset.morphSkip = "1";              // the page morphs its card around this canvas
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
    const rimL = new THREE.DirectionalLight(0x5de3ff, 1.0); rimL.position.set(-10, 6, -12); s.add(rimL);
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
    this.points = new THREE.Points(new THREE.BufferGeometry(), this.skyMat); this.points.frustumCulled = false; this.points.visible = false; s.add(this.points);
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
      this.points.visible = pos.length > 0;
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
