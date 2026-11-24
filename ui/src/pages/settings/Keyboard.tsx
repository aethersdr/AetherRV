// On-screen keyboard (the kiosk has no keyboard), ported from app.js openKeyboard / renderOsk.
//   const text = await openKeyboard({ title, value, password, maxlen, pattern })   → string, or null on Cancel
// The typed text lives only in this component's state while it is open and goes back only to the caller (e.g.
// straight into the Wi-Fi join request): never into the store, a toast, a log or the page. A password shows as dots
// unless Show is tapped (as app.js).
//
// Keys act on pointerdown (fast typing, as app.js). Done and Cancel act on click, so the tap that closes the
// keyboard ends inside it and can't land on a Settings button underneath (app.js closed on pointerdown).
import { memo, useEffect, useState, useSyncExternalStore, type PointerEvent } from "react";
import { createPortal } from "react-dom";

export interface KeyboardOpts { title?: string; value?: string; password?: boolean; maxlen?: number; pattern?: string }
interface Req { opt: KeyboardOpts; resolve: (v: string | null) => void; seq: number }

let req: Req | null = null;
let seq = 0;
const subs = new Set<() => void>();
const notify = () => subs.forEach(f => f());

export function openKeyboard(opt: KeyboardOpts): Promise<string | null> {
  req?.resolve(null);                             // only one at a time: a second request cancels the first
  return new Promise(resolve => { req = { opt, resolve, seq: ++seq }; notify(); });
}
function close(v: string | null): void {
  const r = req;
  req = null;
  notify();
  r?.resolve(v);
}
export const keyboardOpen = () => req != null;

const ROWS = {
  abc: ["1234567890", "qwertyuiop", "asdfghjkl", "zxcvbnm"],
  sym: ["1234567890", "-_@#$%&*+=", "!?.,:;'\"()~", "/\\|<>[]{}^`"],
} as const;

const Pad = memo(function Pad({ r }: { r: Req }) {
  const [text, setText] = useState(r.opt.value || "");
  const [shift, setShift] = useState(false);
  const [sym, setSym] = useState(false);
  const [show, setShow] = useState(!r.opt.password);
  const max = r.opt.maxlen || 63;
  const ok = !r.opt.pattern || new RegExp(`^(?:${r.opt.pattern})$`).test(text);
  const key = (e: PointerEvent<HTMLDivElement>) => {
    const b = (e.target as Element).closest<HTMLButtonElement>(".osk-k");
    if (!b || b.disabled) return;
    const k = b.dataset.k, a = b.dataset.a;
    if (k == null && a !== "bs" && a !== "shift" && a !== "sym") return;       // Done: on click
    e.preventDefault();
    if (k != null) { setText(t => (t.length < max ? t + k : t)); if (shift) setShift(false); }
    else if (a === "bs") setText(t => t.slice(0, -1));
    else if (a === "shift") setShift(s => !s);
    else setSym(s => !s);
  };
  const rows = ROWS[sym ? "sym" : "abc"].map(row => [...row].map(ch => {
    const c = !sym && shift ? ch.toUpperCase() : ch;
    return <button key={ch} className="osk-k" data-k={c}>{c}</button>;
  }));
  rows[3] = [<button key="shift" className={`osk-k wide${shift ? " on" : ""}`} data-a="shift">⇧</button>, ...rows[3]!,
    <button key="bs" className="osk-k wide" data-a="bs">⌫</button>];
  rows.push([<button key="sym" className="osk-k wide" data-a="sym">{sym ? "abc" : "123#"}</button>,
    <button key="sp" className="osk-k space" data-k=" ">space</button>,
    <button key="done" className="osk-k wide done" data-a="done" disabled={!(text.length && ok)} onClick={() => close(text)}>Done</button>]);
  return (
    <div className="osk-modal" id="osk" role="dialog" aria-modal="true" aria-labelledby="osk-title">
      <div className="osk-box">
        <div className="osk-head"><span id="osk-title">{r.opt.title || ""}</span>
          <button className="ctl-pill" id="osk-cancel" onClick={() => close(null)}>Cancel</button></div>
        <div className="osk-field">
          <span id="osk-text">{show ? text : "•".repeat(text.length)}</span><i className="osk-caret" />
          {r.opt.password && <button className="ctl-pill" id="osk-show" onClick={() => setShow(s => !s)}>{show ? "Hide" : "Show"}</button>}
        </div>
        <div className="osk-keys" id="osk-keys" onPointerDown={key}>
          {rows.map((r2, i) => <div key={i} className="osk-row">{r2}</div>)}
        </div>
      </div>
    </div>
  );
});

const sub = (f: () => void) => { subs.add(f); return () => { subs.delete(f); }; };
const get = () => req;
/** Mount once (the Settings page); renders into <body> so it covers the whole screen. */
export const Keyboard = memo(function Keyboard() {
  const r = useSyncExternalStore(sub, get, get);
  useEffect(() => () => { if (req) close(null); }, []);           // the page hidden while open: cancel, never hang
  if (!r || typeof document === "undefined") return null;
  return createPortal(<Pad key={r.seq} r={r} />, document.body);          // a fresh pad (and empty text) per request
});
