// Screen off (kiosk only), ported from app.js. The kiosk session runs a helper on 127.0.0.1:8091 (aetherrv-screen,
// same HTTP API as screen_helper.py) that powers the panel off/on. While off, a black shield swallows the first
// touch so waking the screen can't also press whatever is underneath. Auto-off after `settings.screen_off_min`
// (default 10) without a touch; a NEW critical alert wakes it.
import { IS_LOCAL } from "../lib/prefs";
import { store } from "../state/store";
import { toast } from "./ui";

const SCREEN = "http://127.0.0.1:8091/screen";
export const SCREEN_AVAIL = "$screen.available";
export const SHIELD = "$screen.shield";

let wakeImpl: (() => void) | null = null;
/** Wake the screen (a new critical alert). No-op off the kiosk. */
export const kioskWake = () => wakeImpl?.();
let offImpl: ((quiet?: boolean) => void) | null = null;
export const screenOff = () => offImpl?.();
let wakeTouch: ((e: Event) => void) | null = null;
/** pointerdown on the shield. */
export const shieldDown = (e: Event) => wakeTouch?.(e);

async function screenPower(on: boolean): Promise<boolean | null> {
  try { const r = await fetch(`${SCREEN}/${on ? "on" : "off"}`, { method: "POST" }); return ((await r.json()) as { on: boolean }).on; }
  catch { return null; }
}

let installed = false;
export function installKiosk(): void {
  if (installed || !IS_LOCAL) return;
  installed = true;
  document.documentElement.classList.add("kiosk");          // touch-only: no mouse pointer on the kiosk
  fetch(SCREEN).then(r => r.json()).then(() => store.set(SCREEN_AVAIL, true)).catch(() => {});
  const shield = (on: boolean) => store.set(SHIELD, on);
  const shieldUp = () => store.get(SHIELD) === true;
  let lastTouch = Date.now();
  let shieldPoll: ReturnType<typeof setInterval> | undefined;
  // While off, sway drops touches, so the helper wakes the screen itself; the page watches for it coming back.
  const dropShieldWhenOn = () => {
    clearInterval(shieldPoll);
    shieldPoll = setInterval(async () => {
      try {
        if (((await (await fetch(SCREEN)).json()) as { on: boolean }).on) {
          clearInterval(shieldPoll); lastTouch = Date.now();
          setTimeout(() => shield(false), 700);           // swallow the rest of the waking touch
        }
      } catch { /* helper restarting: keep polling */ }
    }, 700);
  };
  offImpl = async (quiet = false) => {
    shield(true);                                          // up first, so the turn-off tap's tail is absorbed too
    if (await screenPower(false) === null) { shield(false); if (!quiet) toast("Screen control unavailable", "bad"); return; }
    setTimeout(dropShieldWhenOn, 1500);                    // don't mistake the turn-off moment for a wake
  };
  const idleOffMs = () => { const m = store.get<number>("settings.screen_off_min"); return m == null ? 10 * 60e3 : m > 0 ? m * 60e3 : Infinity; };
  for (const ev of ["pointerdown", "keydown", "wheel"]) document.addEventListener(ev, () => { lastTouch = Date.now(); }, true);
  setInterval(() => {   // applies even with unacknowledged alerts (owner's choice); a NEW critical still wakes it
    if (!shieldUp() && Date.now() - lastTouch > idleOffMs() && store.get(SCREEN_AVAIL) === true) offImpl?.(true);
  }, 15e3);
  wakeImpl = async () => {
    lastTouch = Date.now();
    if (shieldUp()) { await screenPower(true); dropShieldWhenOn(); }
  };
  wakeTouch = async e => {
    e.preventDefault(); e.stopPropagation();
    await screenPower(true);
    lastTouch = Date.now();
    setTimeout(() => shield(false), 400);                  // keep swallowing the rest of this touch
  };
}
