// The 3D battery-bank scene (app.js bankScene / updateBankScene), an SVG with CSS animations: glowing disc stacks
// on a platform, wired in parallel by + / − buses whose dashes flow with the bank current.
//
// The SVG is built once per battery set (a memoised component keyed by the names). Live values are written straight
// into it from a store subscription, coalesced to one DOM pass per frame; nothing re-renders. New over app.js:
// each battery's charge level eases to a new reading (the "liquid" rises / falls, the % counts) instead of jumping.
import { Fragment, memo, useLayoutEffect, useRef } from "react";
import { reducedMotion, tween } from "../../lib/tween";
import { store } from "../../state/store";
import { BS, discClip, discSide, flowDur, flowOf, hueOf, pctText, recordBank, sparkPaths, stackX, battPct,
  type BankHist, type Ble, type Hue } from "./bank";

// aethersdr.com tokens: --bg-2/--bg-3 navy, --cyan-deep #3aa7ff → --cyan #5de3ff → --aqua #8ef7e6 (their --grad);
// low SoC uses their line tokens --l-hpsdr #F2B441 (amber) and --l-native #EE7355 (coral).
// (toned down: the gradient's bright end is reserved for a thin highlight, not the whole body)
const HUES: Record<Hue, readonly [string, string, string, string]> = {
  blue: ["#0a1422", "#1d5f9e", "#3aa7ff", "#6fd0ea"], amber: ["#1f1606", "#8a5f14", "#c9942f", "#e9c77d"],
  red: ["#1f0d09", "#8a3a29", "#c9604a", "#e9a594"],
};
const H = { x1: "0", x2: "1", y1: "0", y2: "0" }, V = { x1: "0", x2: "0", y1: "0", y2: "1" };
type Stop = readonly [number, string, number?];
const Grad = ({ id, stops, dir = H }: { id: string; stops: readonly Stop[]; dir?: typeof H }) => (
  <linearGradient id={id} {...dir}>
    {stops.map(([o, c, a = 1], i) => <stop key={i} offset={o} stopColor={c} stopOpacity={a} />)}
  </linearGradient>
);

// ── Battery current history for the sparklines: recorded for as long as the app runs (app.js recorded it only
// while the Energy page was on screen), so the sparklines are already drawn when the page opens. ──
export const bankHist: BankHist = new Map();
store.subscribe("batteries.ble", () => recordBank(bankHist, store.get<Ble[]>("batteries.ble") || [], Date.now() / 1000));

/** The static scene. Everything that changes is written by bindScene(). */
const SceneSvg = memo(function SceneSvg({ names }: { names: readonly string[] }) {
  const n = names.length, W = BS.w, cxs = names.map((_, i) => stackX(i, n));
  const pY = BS.yb0 + 12;
  const topY = BS.yb0 - 3 * BS.step - BS.dh;              // top of the top disc's body
  const xl = cxs[0]! - 22, xr = cxs[n - 1]! + 22;
  const pE = W - 30, nE = 30;                             // bus ends; the + / − sit 10 px past them, centred on the line
  return (
    <svg className="bank-scene" id="bank-scene" data-n={n} viewBox={`0 0 ${W} ${BS.h}`} role="img" aria-label={`${n} batteries in parallel`}>
      <defs>
        {(Object.entries(HUES) as [Hue, readonly [string, string, string, string]][]).map(([k, [d0, d1, d2, d3]]) => (
          <Fragment key={k}>
            <Grad id={`bg-side-${k}`} stops={[[0, d0], [.18, d1], [.5, d2], [.58, d3], [.68, d2], [.86, d1], [1, d0]]} />
            <radialGradient id={`bg-top-${k}`} cx=".45" cy=".35" r=".8">
              <stop offset="0" stopColor={d2} /><stop offset=".6" stopColor={d1} /><stop offset="1" stopColor={d0} />
            </radialGradient>
            <radialGradient id={`bg-pool-${k}`}>
              <stop offset="0" stopColor={d2} stopOpacity=".5" /><stop offset="1" stopColor={d2} stopOpacity="0" />
            </radialGradient>
          </Fragment>
        ))}
        <Grad id="bg-scan" stops={[[0, "#8ef7e6", 0], [.5, "#8ef7e6", .55], [1, "#8ef7e6", 0]]} dir={V} />
        <Grad id="bg-slab" stops={[[0, "#0e1a2a"], [1, "#060b13"]]} dir={V} />
        <Grad id="bg-spark" stops={[[0, "#5de3ff", .42], [1, "#3aa7ff", 0]]} dir={V} />
        <Grad id="bg-sparkline" stops={[[0, "#3aa7ff"], [.42, "#5de3ff"], [1, "#8ef7e6"]]} />
        <radialGradient id="bg-slabtop" cx=".5" cy=".4" r=".7">
          <stop offset="0" stopColor="#12233a" stopOpacity=".95" /><stop offset="1" stopColor="#0a121e" stopOpacity=".95" />
        </radialGradient>
        {cxs.map((cx, i) => (
          <Fragment key={i}>
            {/* a plain rect: cheap to repaint */}
            <clipPath id={`bg-stk${i}`}><rect x={cx - BS.rx} y={topY} width={2 * BS.rx} height={BS.yb0 - topY + BS.ry} /></clipPath>
            {[0, 1, 2, 3].map(k => (
              <clipPath key={k} id={`bg-c${i}-${k}`}>
                <rect id={`bg-cr${i}-${k}`} x={cx - BS.rx - 2} y={BS.yb0 - k * BS.step + BS.ry + 2} width={2 * BS.rx + 4} height="0" />
              </clipPath>
            ))}
          </Fragment>
        ))}
      </defs>
      {/* platform */}
      <ellipse cx={W / 2} cy={pY + 12} rx={W / 2 - 8} ry="36" fill="url(#bg-slab)" />
      <ellipse cx={W / 2} cy={pY} rx={W / 2 - 8} ry="36" fill="url(#bg-slabtop)" className="bg-slab-top" />
      <ellipse cx={W / 2} cy={pY} rx={W / 2 - 22} ry="30" className="bg-slab-line" />
      <ellipse cx={W / 2} cy={pY} rx={W / 2 - 8} ry="36" className="bg-slab-edge bg-halo-line" />
      <ellipse cx={W / 2} cy={pY} rx={W / 2 - 8} ry="36" className="bg-slab-edge" />
      {/* posts */}
      {cxs.map((cx, i) => (
        <Fragment key={i}>
          <line x1={cx - 22} y1={topY - 4} x2={cx - 22} y2="26" className="bg-post-p" />
          <line x1={cx + 22} y1={topY - 4} x2={cx + 22} y2="42" className="bg-post-n" />
          <line x1={cx - 22} y1={topY - 4} x2={cx - 22} y2="26" className="bg-flow bg-flow-post" />
          <line x1={cx + 22} y1={topY - 4} x2={cx + 22} y2="42" className="bg-flow bg-flow-post" />
        </Fragment>
      ))}
      {/* buses */}
      <path d={`M${xl} 26 H${pE}`} className="bg-bus-p bg-halo-line" />
      <path d={`M${xr} 42 H${nE}`} className="bg-bus-n bg-halo-line" />
      <path d={`M${xl} 26 H${pE}`} className="bg-bus-p" />
      <path d={`M${xr} 42 H${nE}`} className="bg-bus-n" />
      <path d={`M${xl} 26 H${pE}`} className="bg-flow bg-flow-bus" />
      <path d={`M${xr} 42 H${nE}`} className="bg-flow bg-flow-bus" />
      <text x={pE + 10} y="26" className="bg-pol bg-pol-p">+</text>
      <text x={nE - 10} y="42" className="bg-pol bg-pol-n">−</text>
      {/* stacks */}
      {cxs.map((cx, i) => (
        <g key={i} id={`bg-s${i}`} className="bg-stack off">
          <ellipse id={`bg-halo${i}`} cx={cx} cy={BS.yb0 - 1.6 * BS.step} rx={BS.rx + 34} ry={2.6 * BS.step} fill="url(#bg-pool-blue)" className="bg-halo" opacity="0" />
          <ellipse id={`bg-pool${i}`} cx={cx} cy={BS.yb0 + 10} rx={BS.rx + 26} ry={BS.ry + 10} fill="url(#bg-pool-blue)" opacity="0" />
          <ellipse cx={cx} cy={BS.yb0 + 10} rx={BS.rx + 12} ry={BS.ry + 6} className="bg-ring" style={{ animationDelay: `${i * .35}s` }} />
          {[0, 1, 2, 3].map(k => {
            const yb = BS.yb0 - k * BS.step, yt = yb - BS.dh;
            return (
              <g key={k} className="bg-disc">
                <path d={discSide(cx, yt, yb)} className="bg-dim" />
                <ellipse cx={cx} cy={yt} rx={BS.rx} ry={BS.ry} className="bg-dim-top" />
                <g clipPath={`url(#bg-c${i}-${k})`} className="bg-lit">
                  <path id={`bg-ls${i}-${k}`} d={discSide(cx, yt, yb)} fill="url(#bg-side-blue)" />
                  <ellipse id={`bg-lt${i}-${k}`} cx={cx} cy={yt} rx={BS.rx} ry={BS.ry} fill="url(#bg-top-blue)" />
                </g>
                <ellipse id={`bg-sf${i}-${k}`} cx={cx} cy={yb} rx={BS.rx - 1} ry={BS.ry - 1} className="bg-surface" opacity="0" fill="url(#bg-top-blue)" />
                <ellipse cx={cx} cy={yt} rx={BS.rx} ry={BS.ry} className={k === 3 ? "bg-rim" : "bg-rim bg-seam"} />
              </g>
            );
          })}
          <g clipPath={`url(#bg-stk${i})`}>
            <rect className="bg-scan" x={cx - BS.rx} y={BS.yb0 + BS.ry - 30} width={2 * BS.rx} height="30" fill="url(#bg-scan)" />
          </g>
          <text id={`bg-pct${i}`} x={cx} y={BS.yb0 - 1.5 * BS.step - BS.dh / 2 + 6} className="bg-pct">—</text>
          <text x={cx} y={pY + 26} className="bg-name">{names[i]}</text>
          <path id={`bg-sa${i}`} className="bg-spark-area" />
          <path id={`bg-sl${i}`} className="bg-spark" />
          <line x1={cx - 78} x2={cx + 34} y1={BS.h - 6} y2={BS.h - 6} className="bg-zero" />
          <text id={`bg-amp${i}`} x={cx + 40} y={BS.h - 13} className="bg-amp" />
        </g>
      ))}
    </svg>
  );
});

const LEVEL_MS = 900;                // a new charge reading eases in over this long …
const LEVEL_MIN = 0.5;               // … when it moves the level visibly (≥ 0.5 %); smaller steps are just written
const setAttr = (el: Element | null | undefined, a: string, v: string) => { if (el && el.getAttribute(a) !== v) el.setAttribute(a, v); };
const setText = (el: Element | null | undefined, s: string) => { if (el && el.textContent !== s) el.textContent = s; };

/** Binds the live values to one built scene: update() writes them (levels ease), stop() ends running tweens. */
export function bindScene(svg: SVGSVGElement, n: number) {
  const q = (id: string) => svg.querySelector(`#${id}`);
  const st = Array.from({ length: n }, (_, i) => ({
    stack: q(`bg-s${i}`), pool: q(`bg-pool${i}`), halo: q(`bg-halo${i}`), pct: q(`bg-pct${i}`), amp: q(`bg-amp${i}`),
    sl: q(`bg-sl${i}`), sa: q(`bg-sa${i}`),
    discs: [0, 1, 2, 3].map(k => ({ cr: q(`bg-cr${i}-${k}`), ls: q(`bg-ls${i}-${k}`), lt: q(`bg-lt${i}-${k}`), sf: q(`bg-sf${i}-${k}`) })),
    shown: null as number | null, target: undefined as number | null | undefined, hue: "" as Hue | "", cancel: null as (() => void) | null,
  }));

  /** Draw battery i at a charge of `pct` % (the % label is unclamped: a battery can read past its rating). */
  const draw = (i: number, pct: number | null) => {
    const s = st[i]!;
    s.shown = pct;
    const soc = pct == null ? null : Math.max(0, Math.min(100, pct));
    const hue = hueOf(soc);
    s.stack?.classList.toggle("off", soc == null);
    setAttr(s.pool, "opacity", soc == null ? "0" : (.12 + .3 * soc / 100).toFixed(2));
    setAttr(s.halo, "opacity", soc == null ? "0" : (.04 + .1 * soc / 100).toFixed(2));
    if (hue !== s.hue) {
      s.hue = hue;
      setAttr(s.pool, "fill", `url(#bg-pool-${hue})`);
      setAttr(s.halo, "fill", `url(#bg-pool-${hue})`);
      for (const d of s.discs) {
        setAttr(d.ls, "fill", `url(#bg-side-${hue})`);
        setAttr(d.lt, "fill", `url(#bg-top-${hue})`);
        setAttr(d.sf, "fill", `url(#bg-top-${hue})`);
      }
    }
    s.discs.forEach((d, k) => {
      const c = discClip(soc, k);
      setAttr(d.cr, "y", c.y);
      setAttr(d.cr, "height", c.height);
      setAttr(d.sf, "cy", c.surfaceY);                       // liquid surface on the partly-filled quarter
      setAttr(d.sf, "opacity", c.f > 0 && c.f < 1 ? "0.9" : "0");     // same 90 % alpha as the charged body
    });
    setText(s.pct, pctText(pct));
  };

  let first = true;
  const stop = () => { for (const s of st) { s.cancel?.(); s.cancel = null; } };
  const update = () => {
    const batts = store.get<Ble[]>("batteries.ble") || [];
    const amps = store.get<number | null>("batteries.ble.current") ?? store.get<number | null>("victron.battery.current");
    const flow = flowOf(amps);
    svg.classList.toggle("chg", flow === "chg");
    svg.classList.toggle("dis", flow === "dis");
    const dur = flowDur(amps);
    if (svg.style.getPropertyValue("--flow-dur") !== dur) svg.style.setProperty("--flow-dur", dur);
    const now = Date.now() / 1000;
    for (let i = 0; i < n; i++) {
      const b = batts[i], s = st[i]!;
      const pct = battPct(b);
      if (pct !== s.target) {
        s.target = pct;
        s.cancel?.(); s.cancel = null;
        if (first || pct == null || s.shown == null || Math.abs(pct - s.shown) < LEVEL_MIN || reducedMotion()) draw(i, pct);
        else s.cancel = tween(s.shown, pct, LEVEL_MS, v => draw(i, v));
      }
      setText(s.amp, b?.ok && b.current != null ? `${(+b.current).toFixed(1)} A` : "");
      const sp = b ? sparkPaths(bankHist.get(b.name) || [], stackX(i, n), now) : null;
      if (sp) { setAttr(s.sl, "d", sp.line); setAttr(s.sa, "d", sp.area); }
    }
    first = false;
  };
  return { update, stop };
}

const SCENE_KEYS = ["batteries.ble", "batteries.ble.current", "victron.battery.current"];

export const BankScene = memo(function BankScene({ names }: { names: readonly string[] }) {
  const root = useRef<HTMLDivElement>(null);
  // layout effect: the first values are in place before the scene is first painted (no empty-stack flash)
  useLayoutEffect(() => {
    const svg = root.current!.querySelector("svg")!;
    const scene = bindScene(svg, names.length);
    scene.update();
    let raf = 0;                                     // coalesce bursts of deltas into one DOM pass per frame
    const off = store.subscribeKeys(SCENE_KEYS, () => { if (!raf) raf = requestAnimationFrame(() => { raf = 0; scene.update(); }); });
    return () => { off(); cancelAnimationFrame(raf); scene.stop(); };
  }, [names]);
  // display: contents keeps the SVG a direct flex item of the card, exactly as app.js laid it out
  return <div ref={root} style={{ display: "contents" }}><SceneSvg names={names} /></div>;
});
