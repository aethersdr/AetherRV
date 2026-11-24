// Shell overlays: the toast, the confirmation dialog and the kiosk's wake shield.
import { memo, useEffect, useState } from "react";
import { SHIELD, shieldDown } from "../logic/kiosk";
import { closeConfirm, type ConfirmState, type ToastState } from "../logic/ui";
import { useKey } from "../state/hooks";
import { K } from "../state/store";
import { Icon } from "./Icon";

export const Toast = memo(function Toast() {
  const t = useKey<ToastState>(K.toast);
  const [hidden, setHidden] = useState(true);
  useEffect(() => {
    if (!t) return;
    setHidden(false);
    const h = setTimeout(() => setHidden(true), t.kind === "busy" ? 20000 : 4000);
    return () => clearTimeout(h);
  }, [t]);
  if (!t) return <div id="toast" className="toast" hidden />;
  const ic = t.kind === "ok" ? "check" : t.kind === "bad" ? "alert" : t.kind === "busy" ? "flame" : "lock";
  return (
    <div id="toast" className={`toast ${t.kind}`} hidden={hidden} role="status" aria-live="polite">
      <Icon name={ic} /><span>{t.msg}</span>
    </div>
  );
});

/** Cancel / Confirm before heat, fan and critical actions (app.js askConfirm). */
export const ConfirmModal = memo(function ConfirmModal() {
  const c = useKey<ConfirmState | null>(K.confirm);
  if (!c) return <div id="modal" className="modal" hidden />;
  return (
    <div id="modal" className="modal" role="dialog" aria-modal="true" aria-labelledby="modal-t">
      <div className="modal-box">
        <h3 id="modal-t">{c.title}</h3>
        <p id="modal-d">{c.detail}</p>
        <div className="modal-btns">
          <button className="btn-soft" id="modal-no" onClick={closeConfirm}>Cancel</button>
          <button className="btn-soft primary" id="modal-yes" onClick={() => { closeConfirm(); c.onYes(); }}>Confirm</button>
        </div>
      </div>
    </div>
  );
});

export const WakeShield = memo(function WakeShield() {
  const up = useKey(SHIELD) === true;
  return (
    <div id="wake-shield" hidden={!up}
      onPointerDown={e => shieldDown(e.nativeEvent)}
      onClickCapture={e => { e.preventDefault(); e.stopPropagation(); }} />
  );
});
