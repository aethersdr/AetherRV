// Instant touch feedback: the pressed state is set by a capturing pointerdown listener in the same frame as the
// touch (CSS :active alone is late or missing on some touch browsers), and held for at least 120 ms so even a
// quick tap visibly registers. Purely a class toggle: no React render, no layout read.
const SEL = "button, [role=button], .pressable";
const MIN_MS = 120;

export function installPressFeedback(): void {
  const down = new Map<number, { el: Element; t: number }>();
  document.addEventListener("pointerdown", e => {
    const el = (e.target as Element | null)?.closest?.(SEL);
    if (!el || (el as HTMLButtonElement).disabled || el.getAttribute("aria-disabled") === "true") return;
    el.classList.add("pressed");
    down.set(e.pointerId, { el, t: performance.now() });
  }, { capture: true, passive: true });
  const up = (e: PointerEvent) => {
    const d = down.get(e.pointerId);
    if (!d) return;
    down.delete(e.pointerId);
    const left = MIN_MS - (performance.now() - d.t);
    if (left > 0) setTimeout(() => d.el.classList.remove("pressed"), left);
    else d.el.classList.remove("pressed");
  };
  document.addEventListener("pointerup", up, true);
  document.addEventListener("pointercancel", up, true);
  window.addEventListener("blur", () => { for (const d of down.values()) d.el.classList.remove("pressed"); down.clear(); });
}
