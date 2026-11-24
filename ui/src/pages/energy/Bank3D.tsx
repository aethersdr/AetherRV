// The battery bank on the Energy page: the WebGL scene (bank3d.ts, owner Q-U6) where WebGL2 works, else the SVG scene
// (BankScene.tsx) in the same box. three loads only here, through import(), as its own chunk.
//
// Lifecycle (pages stay mounted under <Activity>): the effect creates the scene and its canvas; its cleanup (the page
// hidden, the battery set changed) stops the loop and disposes the renderer, its context, geometries, materials and
// textures, and removes the canvas, so a re-shown page builds a fresh one. React owns only the box and the labels.
import { memo, useCallback, useEffect, useRef, useState } from "react";
import { store } from "../../state/store";
import { BS, battPct, durSeconds, flowDur, flowOf, headView, pctText, sparkPaths, type Ble } from "./bank";
import type { BankGL, Labels } from "./bank3d";
import { BankScene, bankHist } from "./BankScene";

let mod: Promise<typeof import("./bank3d")> | null = null;
const load = () => (mod ??= import("./bank3d"));
// fetch three at idle once the Energy page's code is here, so the first visit doesn't wait for it
if (typeof window !== "undefined" && import.meta.env.MODE !== "test") {
  const ric = (window as { requestIdleCallback?: (f: () => void) => void }).requestIdleCallback ?? ((f: () => void) => setTimeout(f, 1500));
  ric(() => { void load().catch(() => undefined); });
}
// Detection is the real thing: making the renderer. (No separate probe: a WebGL context costs a synchronous trip to
// the GPU process.) Once it has failed, or a context was lost, the SVG scene is used for the rest of the session.
let glFailed = false;

const KEYS = ["batteries.ble", "batteries.ble.current", "victron.battery.current", "victron.battery.soc"];
// sparkline coordinates as the SVG scene draws them, for a battery centred at x = 78 (0 … 112 wide)
const SPARK_CX = 78, SPARK_VB = `0 ${BS.h - 34} 112 34`;

const Gl = memo(function Gl({ names, onFail }: { names: readonly string[]; onFail: () => void }) {
  const host = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const box = host.current!, n = names.length;
    const q = <E extends Element>(sel: string) => [...box.querySelectorAll<E>(sel)];
    const labels: Labels = { pct: q<HTMLElement>(".b3-pct"), name: q<HTMLElement>(".b3-name"), foot: q<HTMLElement>(".b3-foot"),
      plus: box.querySelector<HTMLElement>(".b3-plus")!, minus: box.querySelector<HTMLElement>(".b3-minus")! };
    const amps = q<HTMLElement>(".b3-amp"), sl = q<SVGPathElement>(".b3-sl"), sa = q<SVGPathElement>(".b3-sa");
    const head = box.querySelector<HTMLElement>(".b3-head b")!, headA = box.querySelector<HTMLElement>(".b3-head span")!;
    let dead = false, gl: BankGL | null = null, off = () => {}, raf = 0, defer = 0, deferT: ReturnType<typeof setTimeout> | undefined;
    const set = (el: Element | undefined, a: string, v: string) => { if (el && el.getAttribute(a) !== v) el.setAttribute(a, v); };
    const text = (el: Element | undefined, s: string) => { if (el && el.textContent !== s) el.textContent = s; };
    const update = () => {
      if (!gl) return;
      const batts = store.get<Ble[]>("batteries.ble") || [];
      const a = store.get<number | null>("batteries.ble.current") ?? store.get<number | null>("victron.battery.current");
      gl.setLevels(Array.from({ length: n }, (_, i) => battPct(batts[i])));
      gl.setFlow(flowOf(a), durSeconds(flowDur(a)));
      const now = Date.now() / 1000;
      for (let i = 0; i < n; i++) {
        const b = batts[i];
        text(amps[i], b?.ok && b.current != null ? `${(+b.current).toFixed(1)} A` : "");
        const sp = b ? sparkPaths(bankHist.get(b.name) || [], SPARK_CX, now) : null;
        if (sp) { set(sl[i], "d", sp.line); set(sa[i], "d", sp.area); }
      }
      text(head, pctText(headView(store.get).pct));
      text(headA, a == null ? "" : `${a > 0 ? "+" : ""}${(+a).toFixed(1)} A`);
    };
    const fail = () => { if (dead) return; dead = true; off(); cancelAnimationFrame(raf); gl?.dispose(); gl = null; glFailed = true; onFail(); };
    // build after the page's first frame is on screen: a page switch must not wait for a WebGL context and shaders
    // (React runs a tap's effects before that frame is painted)
    const start = () => load().then(m => {
      if (dead) return;
      try {
        gl = new m.BankGL(box.querySelector<HTMLElement>(".b3-gl")!, n, labels, {
          onLost: fail, onFirstFrame: () => { box.dataset.ready = "1"; },
        });
      } catch (e) { console.error("bank3d", e); fail(); return; }
      if (!gl.ok) { fail(); return; }
      update();
      // coalesce bursts of deltas into one pass per frame
      off = store.subscribeKeys(KEYS, () => { if (!raf) raf = requestAnimationFrame(() => { raf = 0; update(); }); });
    }, fail);
    defer = requestAnimationFrame(() => { deferT = setTimeout(() => { if (!dead) void start(); }, 0); });
    return () => {
      dead = true; off(); cancelAnimationFrame(raf); cancelAnimationFrame(defer); clearTimeout(deferT);
      gl?.dispose(); gl = null; delete box.dataset.ready;
    };
  }, [names, onFail]);
  return (
    <div className="bank3d" ref={host} data-n={names.length} role="img" aria-label={`${names.length} batteries in parallel (3D, drag to turn)`}>
      <div className="b3-gl" />
      <div className="b3-ov">
        <svg className="b3-defs" aria-hidden="true"><defs>
          <linearGradient id="b3-spark" x1="0" x2="0" y1="0" y2="1"><stop offset="0" stopColor="#5de3ff" stopOpacity=".42" /><stop offset="1" stopColor="#3aa7ff" stopOpacity="0" /></linearGradient>
          <linearGradient id="b3-sparkline" x1="0" x2="1" y1="0" y2="0"><stop offset="0" stopColor="#3aa7ff" /><stop offset=".42" stopColor="#5de3ff" /><stop offset="1" stopColor="#8ef7e6" /></linearGradient>
        </defs></svg>
        <div className="b3-head"><b>—</b> bank <span /></div>
        <div className="b3-pol b3-plus">+</div><div className="b3-pol b3-minus">−</div>
        {names.map((nm, i) => (
          <div key={i} className="b3-batt">
            <div className="b3-pct">—</div>
            <div className="b3-name">{nm}</div>
            <div className="b3-foot">
              <svg viewBox={SPARK_VB} preserveAspectRatio="none"><path className="b3-sa" /><path className="b3-sl" />
                <line x1="0" x2="112" y1={BS.h - 6} y2={BS.h - 6} className="b3-zero" /></svg>
              <span className="b3-amp" />
            </div>
          </div>
        ))}
      </div>
    </div>
  );
});

/** The bank: WebGL where it works, the SVG scene otherwise (no WebGL2, or the context is lost). */
export const Bank = memo(function Bank({ names }: { names: readonly string[] }) {
  const [svg, setSvg] = useState(() => glFailed);
  const fail = useCallback(() => setSvg(true), []);
  return svg ? <BankScene names={names} /> : <Gl names={names} onFail={fail} />;
});
