// WebGL scenes on hidden pages (the Network sky, the Weather radar, the Energy battery bank).
//
// Kept by default: hiding a page (React <Activity>) only pauses its scene's loop; the renderer, its context, the
// compiled shaders and the textures stay, so showing the page again draws at once. Disposing on hide and rebuilding
// on show made every switch to Network take ~400 ms on all three runtimes on the Pi 5 (a new context, the shaders,
// the first frame), and Weather / Energy 150–370 ms (docs/UI-RUNTIME-BENCHMARK.md). A real unmount still disposes.
//
// Except the radar (owner, 2026-11-24): its loop's frames hold about 800 MB of GPU memory while kept, for a switch
// back to Weather of ~110 ms instead of 140-150 ms, so a hidden Weather page frees its radar and showing it rebuilds
// it. The sky and the bank hold a few MB and stay.
//
// ?keepgl=1 keeps all three; ?keepgl=0 frees all three on hide (the earlier rule: a hidden page holds no GPU memory);
// ?keepgl=sky,bank keeps only the scenes named (sky, radar, bank), for the memory comparison in the benchmark.
export type GlScene = "sky" | "radar" | "bank";
const KEPT: readonly GlScene[] = ["sky", "bank"];
const KEEP_Q = typeof location === "undefined" ? null : new URLSearchParams(location.search).get("keepgl");
export const keepGl = (scene: GlScene): boolean =>
  KEEP_Q == null ? KEPT.includes(scene) : KEEP_Q === "1" ? true : KEEP_Q === "0" ? false : KEEP_Q.split(",").includes(scene);

/** Call `dispose` once `el` has left the document: an <Activity> hide keeps the DOM connected (the scene is kept,
 *  paused), a real unmount removes it (the scene is freed). Checked after the commit, when the DOM is final. */
export function disposeWhenDetached(el: Element, dispose: () => void): void {
  setTimeout(() => { if (!el.isConnected) dispose(); }, 0);
}
