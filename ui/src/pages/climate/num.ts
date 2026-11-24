// Tweened number text for the Climate page: like lib/tween useTweenText, plus `animate: false` (snap, e.g. while a
// control is dragged) and a custom text for "no number" (the fan's "Auto"). Writes textContent only: the element
// must have no React children.
import { useLayoutEffect, useRef } from "react";
import { NumberText } from "../../lib/tween";

export interface NumTextOpts { animate?: boolean; empty?: string; ms?: number }

export function useNumText<E extends HTMLElement>(value: number | null | undefined, fmt: (v: number) => string,
                                                  { animate = true, empty = "—", ms = 450 }: NumTextOpts = {}) {
  const ref = useRef<E>(null);
  const nt = useRef<NumberText | null>(null);
  const last = useRef<{ v: number | null | undefined; f: (v: number) => string; e: string } | null>(null);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    nt.current ??= new NumberText(el, fmt, ms);
    const prev = last.current;
    if (prev && Object.is(prev.v, value) && prev.f === fmt && prev.e === empty) return;   // let a running tween finish
    last.current = { v: value, f: fmt, e: empty };
    nt.current.setFormat(fmt);
    if (value == null || !isFinite(value)) {
      nt.current.set(null);                    // forget the shown number: the next one snaps in
      if (el.textContent !== empty) el.textContent = empty;
      return;
    }
    nt.current.set(value, animate);
  });
  useLayoutEffect(() => () => nt.current?.stop(), []);
  return ref;
}

/** Whole degrees in the display unit (fmtT's format for a value already converted with tC). */
export const deg = (v: number) => `${Math.round(v)}°`;
