// The Starlink card's sky: the 3D dome (sky3d.ts, three.js in its own chunk) when WebGL2 works, else the 2D
// obstruction map (app.js skyMap), drawn from the dish's obstruction grid (one byte per cell: 255 = no data, else
// quality 0–254). On mobile / roam plans the grid is in the dish's frame (bottom edge = the way the dish faces),
// so it is rotated by the dish azimuth to put north up; the rings and compass stay fixed.
import { memo, useEffect, useRef, useState } from "react";
import { disposeWhenDetached, keepGl } from "../../lib/glkeep";
import type { ObstructionMap, Sky3D } from "./sky3d";


/** Run f once the current frame has been painted (a frame, then a task). Returns cancel. (Same helper as
 *  weather/wxmap.ts; kept here so the Network chunk doesn't pull in Leaflet.) */
function afterPaint(f: () => void): () => void {
  let t: ReturnType<typeof setTimeout> | null = null;
  const r = requestAnimationFrame(() => { t = setTimeout(f, 0); });
  return () => { cancelAnimationFrame(r); if (t) clearTimeout(t); };
}

// ── 3D availability: probed once per session (module level) ──
type S3 = "unknown" | "ok" | "fail";
let s3: S3 = "unknown";
let mod: Promise<typeof import("./sky3d")> | null = null;
const s3Listeners = new Set<() => void>();
const setS3 = (v: S3) => { if (s3 !== v) { s3 = v; s3Listeners.forEach(l => l()); } };
export const loadSky3d = () => (mod ??= import("./sky3d"));

/** Does this browser give us WebGL2 (three.js r170 needs it)? The probe context is released at once. */
function probeWebGL2(): boolean {
  try {
    const c = document.createElement("canvas"), gl = c.getContext("webgl2");
    if (!gl) return false;
    gl.getExtension("WEBGL_lose_context")?.loseContext();
    return true;
  } catch { return false; }
}

/** "ok" once the 3D module is loaded and WebGL2 is there; until then (and for good without WebGL2) the 2D map. */
function useSky3dState(): S3 {
  const [v, setV] = useState(s3);
  useEffect(() => {
    const f = () => setV(s3);
    s3Listeners.add(f);
    f();
    let cancel = () => {};
    if (s3 === "unknown") {
      // the probe creates (and drops) a WebGL context: after the page's first frame, not in the tap's frame
      cancel = afterPaint(() => {
        if (s3 !== "unknown") return;
        if (new URLSearchParams(location.search).get("sky") === "2d" || !probeWebGL2()) setS3("fail");
        else loadSky3d().then(() => setS3("ok"), () => setS3("fail"));
      });
    }
    return () => { cancel(); s3Listeners.delete(f); };
  }, []);
  return v;
}

// ── 2D map ──
const imgCache = { data: "", url: "" };
function skyImage(m: ObstructionMap): string {
  if (imgCache.data === m.data) return imgCache.url;
  const raw = atob(m.data), n = m.rows, k = 4, size = n * k, c0 = n / 2;
  const cv = document.createElement("canvas"); cv.width = cv.height = size;
  const g = cv.getContext("2d");
  if (!g) return "";
  for (let r = 0; r < m.rows; r++) for (let c = 0; c < m.cols; c++) {
    const v = raw.charCodeAt(r * m.cols + c);
    if (v === 255 || Number.isNaN(v) || Math.hypot(c + 0.5 - c0, r + 0.5 - c0) > c0) continue;
    const q = v / 254;
    g.fillStyle = q >= 0.95 ? "rgba(96,165,250,.85)" : `rgba(248,72,72,${(0.45 + 0.55 * (1 - q)).toFixed(2)})`;
    g.fillRect(c * k + 0.4, r * k + 0.4, k - 0.8, k - 0.8);
  }
  imgCache.data = m.data; imgCache.url = cv.toDataURL();
  return imgCache.url;
}

export const SkyMap2D = memo(function SkyMap2D({ map, az, size = 300 }: { map: ObstructionMap; az: number | null; size?: number }) {
  const maxT = map.max_theta || 80, R = size / 2 - 18, cx = size / 2;
  const rot = map.frame === "dish" ? (az ?? 180) - 180 : 0;
  const rr = (el: number) => (90 - el) / maxT * R;                     // elevation ring radius
  const src = skyImage(map);
  return (
    <div className="sky" style={{ width: size, height: size }}>
      <div className="sky-disc" style={{ inset: cx - R }}>{src && <img alt="" src={src} style={{ transform: `rotate(${rot.toFixed(1)}deg)` }} />}</div>
      <svg viewBox={`0 0 ${size} ${size}`} aria-hidden="true">
        <circle cx={cx} cy={cx} r={R} className="edge" />
        {[60, 30].map(el => <circle key={el} cx={cx} cy={cx} r={rr(el).toFixed(1)} className="ring" />)}
        <circle cx={cx} cy={cx} r={2.5} className="zen" />
        {map.frame !== "unknown" && ([["N", 0], ["E", 90], ["S", 180], ["W", 270]] as const).map(([l, a]) => (
          <text key={l} x={(cx + Math.sin(a * Math.PI / 180) * (R + 10)).toFixed(1)} y={(cx - Math.cos(a * Math.PI / 180) * (R + 10)).toFixed(1)}>{l}</text>))}
        {az != null && map.frame !== "unknown" &&
          <path className="facing" transform={`rotate(${az.toFixed(1)} ${cx} ${cx})`} d={`M${cx} ${cx - R + 9}l-6 -10h12z`} />}
      </svg>
    </div>
  );
});

// ── 3D view ──
/** The scene is built once, after the page's first frame is on screen. Hiding the page (<Activity>) pauses it and
 *  keeps its context, so showing the page again draws the sky at the next frame (lib/glkeep.ts); a real unmount, or
 *  any hide with ?keepgl=0, disposes the renderer, its textures and its context. */
const Sky3DView = memo(function Sky3DView({ map, az, el, onFail }: { map: ObstructionMap; az: number | null; el: number | null; onFail: () => void }) {
  const host = useRef<HTMLDivElement>(null);
  const sky = useRef<Sky3D | null>(null);
  const latest = useRef({ map, az, el });
  latest.current = { map, az, el };
  useEffect(() => {
    const box = host.current!;
    let dead = false, cancel = () => {};
    if (sky.current) {                                              // shown again: the kept scene, resumed after the
      const s = sky.current;                                        // page's first frame (its canvas still shows the
      cancel = afterPaint(() => {                                   // last image), so the switch frame carries no GL
        if (dead) return;
        const v = latest.current;
        s.setMap(v.map, v.az, v.el);
        s.resume();
      });
    } else {
      // built after the page's first frame is on screen: a tap's effects run before the browser paints, and the
      // context, the scene and its shaders would otherwise hold the page switch
      void loadSky3d().then(m => {
        cancel = afterPaint(() => {
          if (dead || !host.current) return;
          const s = new m.Sky3D(host.current);
          if (!s.ok) { s.dispose(); onFail(); return; }
          sky.current = s;
          const v = latest.current;
          s.setMap(v.map, v.az, v.el);
          s.start();
        });
      }, onFail);
    }
    return () => {
      dead = true; cancel();
      const s = sky.current;
      if (!s) return;
      const free = () => { s.dispose(); if (sky.current === s) sky.current = null; };
      if (!keepGl("sky")) { free(); return; }
      s.pause();
      disposeWhenDetached(box, free);
    };
  }, [onFail]);
  useEffect(() => { sky.current?.setMap(map, az, el); }, [map, az, el]);
  return <div className="sky3d" ref={host} />;
});

const fail = () => setS3("fail");

/** The figure: 3D with overlays, or 2D with its caption. */
export const SkyFigure = memo(function SkyFigure({ map, az, el, obs, software }: {
  map: ObstructionMap; az: number | null; el: number | null; obs: number | null; software: string;
}) {
  const st = useSky3dState();
  const warn = (obs ?? 0) > 5 ? "warn-text" : undefined;
  const pct = obs == null ? "—" : obs.toFixed(1);
  if (st === "ok") {
    return (
      <figure className="sl-sky has3d">
        <Sky3DView map={map} az={az} el={el} onFail={fail} />
        <div className="sky-sw">{software}</div>
        <div className="sky-ov tl"><b className={warn}>{`${pct}%`}</b><span>obstructed</span></div>
        <div className="sky-ov br sky-key"><i className="clear" />Clear <i className="blocked" />Obstructed</div>
      </figure>
    );
  }
  return (
    <figure className="sl-sky">
      <SkyMap2D map={map} az={az} size={360} />
      <figcaption><b className={warn}>{`${pct}%`}</b> obstructed
        <span className="sky-key"><i className="clear" />Clear <i className="blocked" />Obstructed</span></figcaption>
    </figure>
  );
});
