// Heat control shared by Home and Climate, ported from app.js line for line (spDisplay, canSetpoint, nudgeSetpoint,
// sendSetpoint, espFollowSetpoint, espDirect, espStartHeat, espSend, heatLockout, the Home heat buttons).
//
// ONE cabin setpoint for both heaters: the MCS7's setpoint also drives a heating Espar, which accepts 10–30 °C.
// Nothing is clamped: starting the Espar outside its range is refused with a toast; a setpoint change while it
// heats always sets the MCS7; the Espar follows values inside its range, turns OFF below it (it runs its own
// cool-down) and keeps its current setpoint above it.
import { F2C, fmtT, tC, UNIT } from "../lib/format";
import { prefs } from "../lib/prefs";
import { K, store } from "../state/store";
import { canControl, canControlDev, control, controlDirect, ctlPending, devOkKey } from "./control";
import { getOptimistic, setOptimistic } from "./optimistic";
import { askConfirm, toast } from "./ui";

const g = store.get;

// ── Keys each derived value reads (subscribe to these) ──
export const SP_KEYS = [K.spPending, "climate.cabin.setpoint", "climate.espar.ctl.active", "climate.espar.ctl.mode",
  "climate.espar.ctl.setpoint_c"];
export const CAN_SP_KEYS = [...SP_KEYS, K.control, K.pending, devOkKey("rixens"), devOkKey("espar")];

/** Low-battery heat lockout (the power guard): heaters can't START heat; turning things OFF stays possible. */
export const heatLockout = () => g("power.heat_lockout") === true;

export const espHeating = () => !!(g("climate.espar.ctl.active") && g("climate.espar.ctl.mode") === "heat");

/** The setpoint shown (display unit, whole degrees) or null. */
export function spDisplay(): number | null {
  const c = g<number>(K.spPending) ?? g<number>("climate.cabin.setpoint") ?? (espHeating() ? g<number>("climate.espar.ctl.setpoint_c") : null);
  return c == null ? null : Math.round(tC(c)!);
}
/** May the setpoint be changed now (the MCS7 with no command in flight, or a heating Espar under our control)? */
export const canSetpoint = () => (canControl() && !ctlPending()) || (espHeating() && canControlDev("espar"));

// ── Espar ──
export const ESP_SP_RANGE: readonly [number, number] = [10, 30];
/** Run-time choices mirror the EasyStart Pro: … minutes, then unlimited (null). */
export const ESP_RUNS: readonly (number | null)[] = [null, 15, 30, 45, 60, 90, 120, 180, 240, 300, 360, 480, 600, 720];
store.set(K.espRun, (v => v === "inf" ? null : ESP_RUNS.includes(+v) ? +v : null)(prefs.get("vp.espar.run", "inf")));
export const espRun = () => g<number | null>(K.espRun) ?? null;
export const espRunLabel = (m: number | null) => m == null ? "∞" : m < 60 ? `${m} min` : `${(m / 60).toFixed(m % 60 ? 1 : 0)} h`;
/** The shared setpoint (°C, 0.1) for the Espar; its own last setpoint when the MCS7 isn't reporting. */
export const espSet = () => Math.round((g<number>(K.spPending) ?? g<number>("climate.cabin.setpoint") ?? g<number>("climate.espar.ctl.setpoint_c") ?? 21) * 10) / 10;
export const espInRange = (c: number) => c >= ESP_SP_RANGE[0] - 0.05 && c <= ESP_SP_RANGE[1] + 0.05;
const espRangeMsg = (c: number) => `${fmtT(c)} is outside the Espar's ${fmtT(ESP_SP_RANGE[0])}–${fmtT(ESP_SP_RANGE[1])} range`;

/** Cycle the run time (the Climate page's clock badge); a running heater gets the new time 1.5 s after the last tap. */
let espRunTimer: ReturnType<typeof setTimeout> | undefined;
export function cycleEspRun(): void {
  const next = ESP_RUNS[(ESP_RUNS.indexOf(espRun()) + 1) % ESP_RUNS.length] ?? null;
  store.set(K.espRun, next);
  prefs.set("vp.espar.run", next == null ? "inf" : String(next));
  const mode = g("climate.espar.ctl.active") ? g<string>("climate.espar.ctl.mode") : "off";
  if (mode === "heat" || mode === "fan") {
    clearTimeout(espRunTimer);
    espRunTimer = setTimeout(() => espSend(mode, `Espar run time ${espRunLabel(espRun())}`,
      g<number>("climate.espar.ctl.setpoint_c") ?? espSet()), 1500);   // a run-time change keeps the setpoint it's running at
  }
}

/** Start the Espar heating at the shared setpoint (after a confirm), or refuse with a toast when out of its range. */
export function espStartHeat(): void {
  const sp = espSet();
  if (!espInRange(sp)) { toast(`Espar not started: ${espRangeMsg(sp)}. Raise the setpoint first.`, "bad"); return; }
  const run = espRun();
  askConfirm(`Start the Espar heating to ${fmtT(sp)}?`,
    `Run time: ${run == null ? "until you turn it off" : espRunLabel(run)}. The heater runs its own 4-minute cool-down after heating.`,
    () => {
      // the battery can cross the lockout while the dialog is open: check again at the moment it is confirmed
      // (the Espar service refuses a start under lockout anyway; this keeps the message honest)
      if (heatLockout()) { toast("Espar not started: heat is locked out (battery low)", "bad"); return; }
      espSend("heat", `Espar heat ${fmtT(sp)}`, sp);
    });
}
/** Fan only (Climate page), after a confirm. */
export function espStartFan(): void {
  const run = espRun();
  askConfirm("Run the Espar fan only?",
    `Run time: ${run == null ? "until you turn it off" : espRunLabel(run)}. The heater runs its own 4-minute cool-down after heating.`,
    () => espSend("fan", "Espar fan"));
}
/** Espar command through the single-flight slot. */
export function espSend(mode: string, label: string, sp: number = espSet()) {
  return control("set", { mode, setpoint_c: sp, run_min: mode === "off" ? null : espRun() }, label, "espar");
}
/** A heating Espar follows the shared setpoint in parallel with the MCS7: its own command and result slot. */
function espDirect(mode: string, sp: number, label: string) {
  return controlDirect("espar", "set", { mode, setpoint_c: sp, run_min: mode === "off" ? null : espRun() }, label);
}
export function espFollowSetpoint(c: number): void {
  if (!espHeating() || !canControlDev("espar")) return;
  const sp = Math.round(c * 10) / 10;
  // owner: a setpoint below the Espar's range means "no Espar heat" → OFF (it runs its own cool-down)
  if (sp < ESP_SP_RANGE[0] - 0.05) { espDirect("off", sp, `Espar off (setpoint below ${fmtT(ESP_SP_RANGE[0])})`); return; }
  if (!espInRange(sp)) return;                                  // above its range: keeps its current setpoint
  if (Math.abs(sp - (g<number>("climate.espar.ctl.setpoint_c") ?? -99)) < 0.05) return;
  espDirect("heat", sp, `Espar set ${fmtT(sp)}`);
}

// ── Setpoint (Home ± and the Climate dial) ──
/** One setpoint change → the MCS7 (if it can be controlled) and a heating Espar, at the same time. */
export function sendSetpoint(c: number, next: number): void {
  espFollowSetpoint(c);
  if (canControl() && !ctlPending()) control("setpoint", +c.toFixed(2), `Setpoint ${next}°`);
  else store.set(K.spPending, null);
}
let spTimer: ReturnType<typeof setTimeout> | undefined;
/** ± one degree (display unit); taps are debounced and ONE absolute value is sent 1.5 s after the last. */
export function nudgeSetpoint(d: number): void {
  if (!canSetpoint()) return;
  const cur = spDisplay();
  if (cur == null) return;
  let next = cur + d;
  next = UNIT === "F" ? Math.min(90, Math.max(35, next)) : Math.min(33, Math.max(1, next));
  const c = UNIT === "F" ? F2C(next) : next;
  store.set(K.spPending, c);
  clearTimeout(spTimer);
  // app.js sent `spPending` as it stood when the timer fired; if a setpoint result had cleared it in between
  // (possible while the Espar heats, since canSetpoint() then ignores ctlPending) app.js sent null, which the
  // Espar path reads as 0 °C → "Espar off". Fall back to this tap's own value instead.
  spTimer = setTimeout(() => sendSetpoint(g<number>(K.spPending) ?? c, next), 1500);
}
/** The Climate dial: stop a pending ± send while the knob is dragged (the drag sends its own value). */
export const cancelSetpointTimer = () => clearTimeout(spTimer);

// ── Home heat quick buttons (3×2): MCS7 sources + modes, Espar ──
// One touch, no confirm for the MCS7 ones (owner's choice in app.js); the Espar asks first (espStartHeat).
export interface HQDef { k: string; label: string; ic: string; kind: "src" | "mode" | "espar" }
export const HQ: readonly HQDef[] = [
  { k: "furnace", label: "Burner", ic: "flame", kind: "src" },
  { k: "engine", label: "Engine", ic: "engine", kind: "src" },
  { k: "electric", label: "Electric", ic: "plug", kind: "src" },
  { k: "hot_water", label: "Hot water", ic: "drop", kind: "mode" },
  { k: "preheat", label: "Pre-heat", ic: "thermo", kind: "mode" },
  { k: "espar", label: "Espar", ic: "fan", kind: "espar" },
];
const HQ_CTL = new Set(["furnace", "engine", "electric", "hot_water", "preheat"]);
export const HQ_KEYS = ["climate.rixens.live", "climate.rixens.sources", "climate.rixens.available", "climate.rixens.furnace",
  "climate.rixens.engine", "climate.rixens.electric", "climate.rixens.hot_water", "climate.rixens.preheat",
  "climate.espar.state", "power.heat_lockout", ...[K.control, K.pending, devOkKey("rixens"), devOkKey("espar")]];

export interface HQView { on: boolean; active: boolean; locked: boolean; usable: boolean; note: string }
export function hqView(b: HQDef): HQView {
  const R = (k: string) => g<unknown>(`climate.rixens.${k}`);
  const live = !!R("live"), srcs = (R("sources") as string[] | undefined) ?? [], avail = (R("available") as Record<string, boolean> | undefined) ?? {};
  let on = false, st = "", note = "", usable = false;
  if (b.kind === "src") {
    on = srcs.includes(b.k); st = (R(b.k) as string | undefined) || "off";
    const installed = b.k === "furnace" || avail[b.k] !== false;
    usable = live && installed && canControl() && !ctlPending();
    note = !live ? "offline" : !installed ? "n/a" : on ? (st === "active" ? "active" : "on") : "off";
  } else if (b.kind === "mode") {
    on = !!R(b.k);
    const installed = b.k !== "preheat" || avail.preheat !== false;
    usable = live && installed && canControl() && !ctlPending();
    note = !live ? "offline" : !installed ? "n/a" : on ? "on" : "off";
  } else {                                       // AetherRV drives the Espar when its control is enabled
    const es = g<string>("climate.espar.state");
    on = es === "heating" || es === "starting" || es === "fan"; st = es === "heating" ? "active" : "";
    note = es == null ? "not connected" : es;
    usable = canControlDev("espar") && !ctlPending();
  }
  // low-battery heat lockout: anything that would START heat is locked (turning off still works; engine is exempt)
  const locked = heatLockout() && !on && b.k !== "engine";
  return { on, active: st === "active", locked, usable, note };
}
/** A tap on a Home heat button (only reached when it isn't disabled). */
export function hqTap(b: HQDef): void {
  const v = hqView(b);
  if (!v.usable || v.locked) return;
  if (b.k === "espar") {                        // tap: heat at the last setpoint and run time, or off
    if (v.on) { espSend("off", "Espar off"); return; }
    espStartHeat();
    return;
  }
  if (!HQ_CTL.has(b.k)) return;
  // a verified overlay the state hasn't caught up with yet is the truth for the next tap (as toggleSwitch)
  const o = getOptimistic<boolean>(`hq.${b.k}`);
  const on = !(o?.phase === "ok" ? o.value : v.on);
  const p = control(b.k, on, `${b.label} ${on ? "on" : "off"}`);
  if (p) setOptimistic(`hq.${b.k}`, on, p, () => hqView(b).on === on, HQ_KEYS);   // lit at once; rolls back if not verified
}

// ── MCS7 destinations (Climate page badge stack: Heat / Hot water / Engine pre-heat; confirm, then one command) ──
export const MCS_DEST: Record<string, readonly [string, string, string, string]> = {
  thermostat: ["Cabin heat", "air_heat", "The MCS7 heats the cabin to the setpoint (thermostat).",
    "The MCS7 stops heating the cabin. Hot water and pre-heat are not affected."],
  hot_water: ["Hot water", "hot_water", "The MCS7 keeps the system fluid hot for hot water.",
    "The MCS7 stops keeping the fluid hot for hot water."],
  preheat: ["Engine pre-heat", "preheat", "The MCS7 sends heat to the engine.", "The MCS7 stops heating the engine."],
};
