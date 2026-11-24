// Inverter & solar card (app.js renderPowerControls + its click handler): MultiPlus mode, shore current limit, MPPT
// on / off. Safety as app.js: every mode change asks first; one verified command at a time (control() refuses
// another and every control here shows disabled while one is in flight); view-only screens can't send. The shore
// limit is nudged ± 1 A and sent 1.5 s after the last tap; the value being adjusted ($lim.pending) is shared with
// the Home inverter overlay.
import { memo } from "react";
import { Icon } from "../../components/Icon";
import { fmtW } from "../../lib/format";
import { canControlDev, control, ctlKeys } from "../../logic/control";
import { askConfirm } from "../../logic/ui";
import { useReady, useSelect } from "../../state/hooks";
import { K, store } from "../../state/store";
import { MODE_WARN, MODES, MPPT_MODES, MPPT_STATE, nudgeLimit, VEBUS_STATE } from "./bank";

const VB = (k: string) => `victron.vebus.${k}`, MP = (k: string) => `victron.mppt.${k}`;
const CTL = ctlKeys("victron");
/** May a victron command be sent now (app.js: canControlDev("victron") && !ctlPending)? */
const canSend = () => canControlDev("victron") && !store.get(K.pending);

// ── handlers (user gestures only) ──
function pickMode(m: number) {
  const label = MODES.find(x => x[0] === m)![1];
  askConfirm(`MultiPlus: ${label}?`, MODE_WARN[m]!, () => control("inverter_mode", m, `MultiPlus ${label}`, "victron"));
}
function pickMppt(m: number) {
  const on = m === 1;
  askConfirm(`Solar charger ${on ? "on" : "off"}?`, on ? "Re-enables solar charging." : "Stops ALL solar charging until turned back on.",
    () => control("mppt_mode", m, `Solar charger ${on ? "on" : "off"}`, "victron"));
}
let limTimer: ReturnType<typeof setTimeout> | undefined;
export const LIM_DEBOUNCE_MS = 1500;
function nudgeLim(dir: 1 | -1) {
  const cur = store.get<number | null>(K.limPending) ?? store.get<number | null>(VB("current_limit"));
  if (cur == null) return;
  const next = nudgeLimit(cur, dir);
  store.set(K.limPending, next);
  clearTimeout(limTimer);
  limTimer = setTimeout(() => {
    // sends what is shown when the timer fires; nothing if the adjustment was dropped meanwhile
    const v = store.get<number | null>(K.limPending);
    if (v != null) control("shore_limit", v, `Shore limit ${v} A`, "victron");
  }, LIM_DEBOUNCE_MS);
}

const MultiPlusBlk = memo(function MultiPlusBlk() {
  const v = useSelect([VB("name"), VB("state"), VB("ac_out_w"), VB("shore_connected"), VB("mode"), VB("mode_adjustable"), ...CTL], g => ({
    name: g<string>(VB("name")) || "MultiPlus",
    state: `${VEBUS_STATE[g<number>(VB("state")) ?? -1] ?? "—"}${g(VB("ac_out_w")) != null ? ` · ${fmtW(g<number>(VB("ac_out_w")))} AC out` : ""}${g(VB("shore_connected")) ? " · on shore" : ""}`,
    mode: g<number>(VB("mode")), adj: !!g(VB("mode_adjustable")), ok: canSend(),
  }));
  return (
    <div className="pwr-blk pwr-main">
      <div className="pwr-t"><Icon name="inverter" /><div><b>{v.name}</b><span>{v.state}</span></div></div>
      <div className="seg mode-seg">
        {MODES.map(([m, l]) => (
          <button key={m} data-mode={m} className={v.mode === m ? "on" : ""} disabled={!(v.ok && v.adj && v.mode !== m)}
            onClick={() => pickMode(m)}>{l}</button>
        ))}
      </div>
    </div>
  );
});

const ShoreBlk = memo(function ShoreBlk() {
  const v = useSelect([VB("limit_adjustable"), VB("current_limit"), K.limPending, ...CTL], g => ({
    adj: !!g(VB("limit_adjustable")), ok: canSend(),
    lim: g<number | null>(K.limPending) ?? g<number | null>(VB("current_limit")) ?? null,
  }));
  const dis = !(v.ok && v.adj);
  return (
    <div className="pwr-blk sm">
      <div className="pwr-t"><Icon name="plug" /><div><b>Shore limit</b><span>{v.adj ? "AC input current" : "not adjustable now"}</span></div></div>
      <div className="ctl-row lim">
        <button className="ctl-btn sm" id="lim-down" aria-label="Lower shore limit" disabled={dis} onClick={() => nudgeLim(-1)}>−</button>
        <div className="ctl-val sm"><b id="lim-val">{v.lim == null ? "—" : `${Math.round(v.lim)} A`}</b></div>
        <button className="ctl-btn sm" id="lim-up" aria-label="Raise shore limit" disabled={dis} onClick={() => nudgeLim(+1)}>+</button>
      </div>
    </div>
  );
});

const MpptBlk = memo(function MpptBlk() {
  const v = useSelect([MP("name"), MP("state"), MP("mode"), ...CTL], g => ({
    name: g<string>(MP("name")) || "Solar charger", state: MPPT_STATE[g<number>(MP("state")) ?? -1] ?? "—",
    mode: g<number>(MP("mode")), ok: canSend(),
  }));
  return (
    <div className="pwr-blk sm">
      <div className="pwr-t"><Icon name="panel" /><div><b>{v.name}</b><span>{v.state}</span></div></div>
      <div className="seg mode-seg">
        {MPPT_MODES.map(([m, l]) => (
          <button key={m} data-mppt={m} className={v.mode === m ? "on" : ""} disabled={!(v.ok && v.mode !== m)}
            onClick={() => pickMppt(m)}>{l}</button>
        ))}
      </div>
    </div>
  );
});

const HEAD_KEYS = [VB("instance"), MP("instance"), "victron.connected", K.control, "victron.controls_ok"];
export const PowerControls = memo(function PowerControls() {
  const ready = useReady();
  const h = useSelect(HEAD_KEYS, g => ({
    any: !!g(VB("instance")) || !!g(MP("instance")), connected: !!g("victron.connected"),
    why: g(K.control) !== true ? "view only" : !g("victron.controls_ok") ? "control off" : "",
  }));
  // until the first snapshot: the card's full layout as a skeleton, not "Cerbo GX not connected"
  if (ready && !h.any) {
    return (
      <div className="card pwr-ctl" id="pwr-ctl">
        <div className="card-h"><h2>Inverter &amp; solar</h2></div>
        <div className="muted">{h.connected ? "No MultiPlus or MPPT reported by the Cerbo." : "Cerbo GX not connected."}</div>
      </div>
    );
  }
  return (
    <div className="card pwr-ctl" id="pwr-ctl">
      <div className="card-h"><h2>Inverter &amp; solar</h2><span className="muted">{ready ? h.why : ""}</span></div>
      <div className="pwr-row"><MultiPlusBlk /><ShoreBlk /><MpptBlk /></div>
    </div>
  );
});
