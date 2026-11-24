// Smooth motion without re-rendering: numbers and dial angles are animated with requestAnimationFrame and written
// straight to the DOM (textContent / an attribute / a transform). One shared rAF loop drives every running
// animation; nothing reads layout. With prefers-reduced-motion, values snap.
import { useLayoutEffect, useRef } from "react";

type Job = (now: number) => boolean;       // return false when finished
const jobs = new Set<Job>();
let raf = 0;
function loop(now: number) {
  for (const j of [...jobs]) {
    let more = false;
    try { more = j(now); } catch (e) { console.error("tween", e); }
    if (!more) jobs.delete(j);
  }
  raf = jobs.size ? requestAnimationFrame(loop) : 0;
}
function schedule(j: Job): () => void {
  jobs.add(j);
  if (!raf) raf = requestAnimationFrame(loop);
  return () => { jobs.delete(j); };
}

export const reducedMotion = (): boolean =>
  typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;

export const easeOutCubic = (t: number) => 1 - (1 - t) ** 3;

/** Animate from → to over `ms`, calling onFrame(v) each frame (and with `to` at the end). Returns cancel. */
export function tween(from: number, to: number, ms: number, onFrame: (v: number) => void,
                      ease: (t: number) => number = easeOutCubic): () => void {
  if (ms <= 0 || reducedMotion() || from === to) { onFrame(to); return () => {}; }
  let t0 = -1;
  return schedule(now => {
    if (t0 < 0) t0 = now;
    const t = Math.min(1, (now - t0) / ms);
    onFrame(t >= 1 ? to : from + (to - from) * ease(t));
    return t < 1;
  });
}

/**
 * A critically-damped-ish spring toward a moving target (dials, knobs): call set(target) as often as you like;
 * onFrame gets the current value every frame until it settles. snap(v) jumps without animating.
 */
export class Spring {
  private v: number;
  private vel = 0;
  private target: number;
  private cancel: (() => void) | null = null;
  private last = -1;
  constructor(initial: number, private readonly onFrame: (v: number) => void,
              private readonly k = 170, private readonly c = 26) {
    this.v = this.target = initial;
  }
  get value() { return this.v; }
  snap(v: number) { this.cancel?.(); this.cancel = null; this.v = this.target = v; this.vel = 0; this.onFrame(v); }
  set(target: number) {
    this.target = target;
    if (reducedMotion()) { this.snap(target); return; }
    if (this.cancel) return;
    this.last = -1;
    this.cancel = schedule(now => {
      const dt = this.last < 0 ? 1 / 60 : Math.min(0.05, (now - this.last) / 1000);
      this.last = now;
      const a = -this.k * (this.v - this.target) - this.c * this.vel;
      this.vel += a * dt;
      this.v += this.vel * dt;
      if (Math.abs(this.v - this.target) < 0.01 && Math.abs(this.vel) < 0.01) {
        this.v = this.target; this.vel = 0; this.onFrame(this.v); this.cancel = null; return false;
      }
      this.onFrame(this.v);
      return true;
    });
  }
  stop() { this.cancel?.(); this.cancel = null; }
}

/**
 * Text that tweens between numbers: set(n) animates from what's shown to n; null shows "—" and the next number
 * snaps (no animation from nothing). Writes textContent only, so React never re-renders for the frames.
 */
export class NumberText {
  private shown: number | null = null;
  private cancel: (() => void) | null = null;
  constructor(private readonly el: { textContent: string | null }, private format: (v: number) => string,
              private readonly ms = 450, private readonly empty = "—") {}
  setFormat(f: (v: number) => string) { this.format = f; }
  set(v: number | null | undefined, animate = true) {
    this.cancel?.(); this.cancel = null;
    if (v == null || !isFinite(v)) { this.shown = null; this.write(this.empty); return; }
    const from = this.shown;
    if (from == null || !animate) { this.shown = v; this.write(this.format(v)); return; }
    this.cancel = tween(from, v, this.ms, x => { this.shown = x; this.write(this.format(x)); });
  }
  private write(s: string) { if (this.el.textContent !== s) this.el.textContent = s; }
  stop() { this.cancel?.(); this.cancel = null; }
}

/**
 * <span ref={useTweenText(value, format)} />: the span's text tweens to each new value. The element must have
 * no React children (React would otherwise overwrite the text).
 */
export function useTweenText<E extends HTMLElement | SVGElement>(value: number | null | undefined,
                                                                 format: (v: number) => string, ms = 450) {
  const ref = useRef<E>(null);
  const nt = useRef<NumberText | null>(null);
  const target = useRef<{ v: number | null | undefined; f: (v: number) => string } | null>(null);
  useLayoutEffect(() => {
    if (!ref.current) return;
    if (!nt.current) nt.current = new NumberText(ref.current, format, ms);
    const t = target.current;
    if (t && Object.is(t.v, value) && t.f === format) return;      // nothing new: let a running tween finish
    target.current = { v: value, f: format };
    nt.current.setFormat(format);
    nt.current.set(value);
  });
  useLayoutEffect(() => () => nt.current?.stop(), []);
  return ref;
}
