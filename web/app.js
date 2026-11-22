"use strict";
import { icon } from "./icons.js";
import { timeChart, ring, dial, dialGeom, esc } from "./charts.js";

const S = {};                       // live state mirror
const $ = s => document.querySelector(s);
const css = v => getComputedStyle(document.documentElement).getPropertyValue(v).trim();
const store = {
  get(k, d) { try { return localStorage.getItem(k) ?? d; } catch { return d; } },
  set(k, v) { try { localStorage.setItem(k, v); } catch { /* kiosk without storage */ } },
};

// ── Units ────────────────────────────────────────────────────────────────────
const UNIT = store.get("vp.temp", "F");                      // US van: °F by default
const tC = c => (c == null ? null : UNIT === "F" ? c * 9 / 5 + 32 : c);
const fmtT = c => (c == null ? "—" : `${Math.round(tC(c))}°`);
const fmtW = w => (w == null ? "—" : Math.abs(w) >= 1000 ? `${(w / 1000).toFixed(1)} kW` : `${Math.round(w)} W`);
const num = (v, d = 0) => (v == null || isNaN(v) ? "—" : (+v).toFixed(d));
const fmtTTG = s => { if (s == null || s <= 0) return "—"; const h = Math.floor(s / 3600); return h >= 48 ? `${Math.round(h / 24)} d` : `${h} h ${Math.round((s % 3600) / 60)} m`; };

// ── Navigation ───────────────────────────────────────────────────────────────
const VIEWS = [
  ["home", "Home", "home"], ["energy", "Energy", "bolt"], ["climate", "Climate", "flame"], ["weather", "Weather", "cloud"],
  ["switches", "Switches", "toggle"], ["network", "Network", "globe"], ["alerts", "Alerts", "alert"], ["systems", "Systems", "chip"],
  ["settings", "Settings", "gear"],
];
const FOOT = new Set(["settings"]);              // pinned to the bottom of the rail
let view = "home";
const navBtn = ([id, label, ic]) => `<button data-v="${id}">${icon(ic)}<span>${label}</span>${id === "alerts" ? '<i class="nav-badge" id="nav-badge" hidden></i>' : ""}</button>`;
$("#nav").innerHTML = VIEWS.filter(v => !FOOT.has(v[0])).map(navBtn).join("");
$("#nav-foot").innerHTML = VIEWS.filter(v => FOOT.has(v[0])).map(navBtn).join("");
function go(v) {
  view = v;
  document.querySelectorAll(".rail button[data-v]").forEach(b => b.classList.toggle("on", b.dataset.v === v));
  document.querySelectorAll(".view").forEach(s => s.classList.toggle("on", s.dataset.view === v));
  $("#page-title").textContent = VIEWS.find(x => x[0] === v)[1];
  history.replaceState(null, "", `#${v}`);
  render(true);
  loadHistory();
}
for (const n of ["#nav", "#nav-foot"]) $(n).addEventListener("click", e => { const b = e.target.closest("button"); if (b) go(b.dataset.v); });
document.addEventListener("click", e => { const g = e.target.closest("[data-goto]"); if (g) go(g.dataset.goto); });

// ── Top bar ──────────────────────────────────────────────────────────────────
function tick() {
  const d = new Date();
  $("#clock-t").textContent = d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  $("#clock-d").textContent = d.toLocaleDateString([], { weekday: "short", month: "short", day: "numeric" });
}
setInterval(tick, 1000); tick();

const night = on => { document.documentElement.classList.toggle("night", on); store.set("vp.night", on ? "1" : "0"); applyWxDark();
  $("#btn-night").innerHTML = icon(on ? "sun" : "moon"); render(true); };
$("#btn-night").addEventListener("click", () => night(!document.documentElement.classList.contains("night")));
document.documentElement.classList.toggle("night", new URLSearchParams(location.search).get("night") === "1" || store.get("vp.night", "0") === "1");
$("#btn-night").innerHTML = icon(document.documentElement.classList.contains("night") ? "sun" : "moon");

let toastT;
function toast(msg, kind = "") {
  const t = $("#toast"); t.className = `toast ${kind}`;
  t.innerHTML = `${icon(kind === "ok" ? "check" : kind === "bad" ? "alert" : kind === "busy" ? "flame" : "lock")}<span>${esc(msg)}</span>`; t.hidden = false;
  clearTimeout(toastT); toastT = setTimeout(() => (t.hidden = true), kind === "busy" ? 20000 : 4000);
}

// ── Energy flow diagram (built once, updated in place) ───────────────────────
// Topology: solar + alternator → battery → DC loads; shore → inverter → AC loads; battery ↔ inverter.
const FLOW = {
  W: 1000, H: 720, hub: { x: 500, y: 235, r: 125 },
  inv: { x: 500, y: 565, w: 236, h: 124 },
  nodes: [
    { id: "solar", label: "Solar", ic: "panel", key: "victron.pv.power", color: "--c-solar", x: 110, y: 105, to: "hub", side: "in" },
    { id: "alt", label: "Alternator", ic: "engine", key: "energy.alternator.power", color: "--c-alt", x: 110, y: 320, to: "hub", side: "in" },
    { id: "shore", label: "Shore", ic: "plug", key: "victron.ac.shore", color: "--c-shore", x: 110, y: 565, to: "inv", side: "in" },
    { id: "dc", label: "DC loads", ic: "dcload", key: "energy.dc.load", color: "--c-load", x: 890, y: 175, to: "hub", side: "out" },
    { id: "ac", label: "AC loads", ic: "outlet", key: "victron.ac.load", color: "--c-load", x: 890, y: 565, to: "inv", side: "out" },
  ],
};
function buildFlow() {
  const { W, H, hub, inv } = FLOW, R = 58;
  const curve = (x1, y1, x2, y2) => `M${x1},${y1} C${(x1 + x2) / 2},${y1} ${(x1 + x2) / 2},${y2} ${x2},${y2}`;
  let s = `<svg viewBox="0 0 ${W} ${H}" preserveAspectRatio="xMidYMid meet">`;
  for (const n of FLOW.nodes) {
    let d;
    if (n.to === "hub") d = n.side === "in" ? curve(n.x + R, n.y, hub.x - hub.r - 14, hub.y) : curve(hub.x + hub.r + 14, hub.y, n.x - R, n.y);
    else d = n.side === "in" ? curve(n.x + R, n.y, inv.x - inv.w / 2 - 8, inv.y) : curve(inv.x + inv.w / 2 + 8, inv.y, n.x - R, n.y);
    s += `<path class="f-edge-track" d="${d}"/><path class="f-edge" id="e-${n.id}" d="${d}" style="stroke:var(${n.color})"/>`;
  }
  // battery ↔ inverter link (drawn battery → inverter; reversed animation = charging)
  const dLink = `M${hub.x},${hub.y + hub.r + 16} L${inv.x},${inv.y - inv.h / 2 - 8}`;
  s += `<path class="f-edge-track" d="${dLink}"/><path class="f-edge" id="e-inv" d="${dLink}" style="stroke:var(--c-batt)"/>`;
  for (const n of FLOW.nodes) {
    s += `<g class="f-node idle" id="n-${n.id}">
      <circle class="bg" cx="${n.x}" cy="${n.y}" r="${R}" style="stroke:var(${n.color})"/>
      <g class="f-ic" style="color:var(${n.color})">${icon(n.ic).replace('<svg class="ic ', `<svg x="${n.x - 26}" y="${n.y - 26}" width="52" height="52" class="`)}</g>
      <text class="f-label" id="l-${n.id}" x="${n.x}" y="${n.y + R + 32}" text-anchor="middle">${n.label}</text>
      <text class="f-val" x="${n.x}" y="${n.y + R + 70}" text-anchor="middle" id="v-${n.id}">—</text>
    </g>`;
  }
  // wireframe inverter/charger
  const x0 = inv.x - inv.w / 2, y0 = inv.y - inv.h / 2, cy = inv.y + 6;
  s += `<g class="f-inv off" id="inv" role="button" aria-label="Inverter controls">
    <rect class="inv-body" x="${x0}" y="${y0}" width="${inv.w}" height="${inv.h}" rx="20"/>
    <path class="inv-line" d="M${x0 + 18},${y0 + 18} h44 M${x0 + 18},${y0 + 28} h44 M${x0 + 18},${y0 + 38} h44"/>
    <circle class="inv-led" id="inv-led" cx="${x0 + inv.w - 24}" cy="${y0 + 24}" r="7"/>
    <path class="inv-line strong" d="M${x0 + 30},${cy} h52"/>
    <path class="inv-line strong dash" d="M${x0 + 30},${cy + 16} h52"/>
    <path class="inv-line" d="M${inv.x - 14},${cy - 6} l14,14 l-14,14"/>
    <path class="inv-line strong" d="M${inv.x + 24},${cy + 8} c8,-22 15,-22 22,0 s14,22 22,0 s14,-22 22,0"/>
    <text class="inv-sym" x="${x0 + 56}" y="${y0 + inv.h - 14}" text-anchor="middle">DC</text>
    <text class="inv-sym" x="${inv.x + 57}" y="${y0 + inv.h - 14}" text-anchor="middle">AC</text>
    <text class="f-label" id="inv-label" x="${inv.x}" y="${y0 + inv.h + 34}" text-anchor="middle">Inverter</text>
    <text class="f-inv-val" id="inv-val" x="${inv.x}" y="${y0 + inv.h + 68}" text-anchor="middle"></text>
    <rect class="inv-hit" x="${x0 - 20}" y="${y0 - 20}" width="${inv.w + 40}" height="${inv.h + 100}" fill="transparent"/>
  </g>`;
  // hub: battery ring
  const rr = hub.r, c = 2 * Math.PI * rr;
  s += `<g id="hub">
    <circle cx="${hub.x}" cy="${hub.y}" r="${rr + 14}" fill="var(--surface-2)"/>
    <circle cx="${hub.x}" cy="${hub.y}" r="${rr}" fill="none" stroke="var(--track)" stroke-width="15"/>
    <circle id="hub-ring" class="ring-val" cx="${hub.x}" cy="${hub.y}" r="${rr}" fill="none" stroke="var(--c-batt)" stroke-width="15" stroke-linecap="round"
      stroke-dasharray="0 ${c}" transform="rotate(-90 ${hub.x} ${hub.y})"/>
    <text class="f-hub-soc" id="hub-soc" x="${hub.x}" y="${hub.y + 20}" text-anchor="middle">—</text>
    <text class="f-hub-sub" id="hub-sub" x="${hub.x}" y="${hub.y + 58}" text-anchor="middle"></text>
    <text class="f-hub-state" id="hub-state" x="${hub.x}" y="${hub.y - 58}" text-anchor="middle"></text>
  </g></svg>`;
  $("#flow").innerHTML = s;
  FLOW.circ = c;
}
buildFlow();

const speedBucket = w => (w < 3 ? 0 : w < 30 ? 4 : w < 100 ? 2.6 : w < 300 ? 1.7 : w < 800 ? 1.1 : 0.7);
function socColor(soc) {
  if (soc == null) return "var(--track)";
  if (soc < 15) return "var(--critical)";
  if (soc < 30) return "var(--warning)";
  return "var(--c-batt)";
}
function setEdge(id, active, sp, reverse = false) {
  const e = document.getElementById(id);
  e.classList.toggle("on", active);
  e.classList.toggle("rev", reverse);
  if (e.dataset.sp !== String(sp)) { e.style.animationDuration = `${sp || 1}s`; e.dataset.sp = sp; }
}
const VB_CHARGING = new Set([3, 4, 5, 6, 7]);   // bulk, absorption, float, storage, equalize
function updateFlow() {
  let inW = 0, outW = 0;
  const vbState = S["victron.vebus.state"], vbMode = S["victron.vebus.mode"];
  const inverting = vbState === 9 || vbState === 10;          // inverting / assisting from the battery
  const passthru = vbState === 8;
  const charging = VB_CHARGING.has(vbState);
  const acLive = inverting || passthru || (charging && vbMode === 3);
  for (const n of FLOW.nodes) {
    let w = S[n.key];
    if (n.id === "shore" && w == null) w = S["victron.vebus.ac_in_w"];
    const g = document.getElementById(`n-${n.id}`);
    const live = n.id === "ac" && acLive;
    const active = (w != null && w >= 3) || live;
    g.classList.toggle("idle", !active);
    document.getElementById(`v-${n.id}`).textContent = w == null ? "—" : fmtW(w);
    if (n.id === "alt") {                                     // WS500 charge stage, when the regulator is charging
      const st = S["energy.alternator.source"] === "n2k" && S["alternator.online"] ? S["alternator.state"] : null;
      document.getElementById("l-alt").textContent = st && st !== "not charging" ? `Alternator · ${st[0].toUpperCase() + st.slice(1)}` : "Alternator";
    }
    setEdge(`e-${n.id}`, active, (w ?? 0) < 3 && live ? 6 : speedBucket(w ?? 0));
    if (w > 0) (n.side === "in" ? (inW += w) : (outW += w));
  }
  // battery ↔ inverter
  const dcw = S["victron.vebus.dc_w"];                       // + = into the battery (charging), − = from it
  const linkW = Math.abs(dcw ?? 0);
  const linkActive = inverting || charging || linkW >= 3;
  setEdge("e-inv", linkActive, linkW < 3 ? 6 : speedBucket(linkW), charging && !inverting);
  const inv = document.getElementById("inv");
  const known = S["victron.vebus.instance"] != null;
  inv.classList.toggle("off", !known || vbMode === 4 || vbState === 0);
  inv.classList.toggle("charging", charging);
  inv.classList.toggle("inverting", inverting || passthru);
  const VBS = { 0: "Off", 1: "Low power", 2: "Fault", 3: "Bulk", 4: "Absorption", 5: "Float", 6: "Storage", 7: "Equalize",
    8: "Passthru", 9: "Inverting", 10: "Assisting", 11: "Power supply", 252: "External control" };
  document.getElementById("inv-label").textContent = known ? `${S["victron.vebus.name"] || "Inverter"} · ${VBS[vbState] ?? "—"}` : "Inverter";
  document.getElementById("inv-val").textContent = !known ? "" : dcw == null ? "" :
    charging ? `${fmtW(Math.abs(dcw))} → battery` : inverting ? `${fmtW(Math.abs(dcw))} from battery` : "";
  // battery hub
  const soc = S["victron.battery.soc"];
  const ringEl = document.getElementById("hub-ring");
  ringEl.setAttribute("stroke-dasharray", `${(FLOW.circ * (soc ?? 0) / 100).toFixed(1)} ${FLOW.circ}`);
  ringEl.setAttribute("stroke", socColor(soc));
  document.getElementById("hub-soc").textContent = soc == null ? "—" : `${Math.round(soc)}%`;
  document.getElementById("hub-sub").textContent = soc == null ? "Battery" : `${num(S["victron.battery.voltage"], 2)} V · ${fmtW(S["victron.battery.power"])}`;
  const bp = S["victron.battery.power"];
  const st = document.getElementById("hub-state");
  st.textContent = bp == null ? "" : bp > 5 ? "CHARGING" : bp < -5 ? "DISCHARGING" : "IDLE";
  st.style.fill = bp > 5 ? "var(--good)" : "var(--ink-2)";
  $("#flow-sub").textContent = S["victron.connected"] ? `in ${fmtW(inW)} · out ${fmtW(outW)}` : "Cerbo GX not connected";
}

// ── Home ─────────────────────────────────────────────────────────────────────
$("#ic-out").innerHTML = icon("thermo");
$("#ic-fridge").innerHTML = icon("fridge");
$("#ic-freezer").innerHTML = icon("snow");
// Refrigeration status: icon + label, never color alone.
function coldStatus(role) {
  const t = S[`climate.${role}.temp`], max = S[`ui.${role}_max_c`];
  if (t == null) return { cls: "", label: "no sensor" };
  if (S[`settings.${role}_off`]) return { cls: "", label: "off · alerts resume when cold" };
  if (max != null && t > max) return { cls: "warn", label: `above ${fmtT(max)}` };
  return { cls: "ok", label: "ok" };
}
// ── Home heat quick buttons (3×2): MCS7 sources + modes, Espar.
// One touch (owner's choice, no confirm): furnace / engine / electric / hot water / pre-heat, all through
// the MCS7 TxGate path (fresh read → send once → verify). Espar: not connected yet.
const HQ = [
  { k: "furnace", label: "Burner", ic: "flame", kind: "src" },
  { k: "engine", label: "Engine", ic: "engine", kind: "src" },
  { k: "electric", label: "Electric", ic: "plug", kind: "src" },
  { k: "hot_water", label: "Hot water", ic: "drop", kind: "mode" },
  { k: "preheat", label: "Pre-heat", ic: "thermo", kind: "mode" },
  { k: "espar", label: "Espar", ic: "fan", kind: "espar" },
];
const HQ_CTL = new Set(["furnace", "engine", "electric", "hot_water", "preheat"]);
const TANK_COLOR = { fresh: "--c-batt", grey: "--muted", black: "--c-alt", fuel: "--c-solar" };
// The Cerbo's default tank name is just "Tank"; show the fluid type instead unless it was renamed.
const TANK_LABEL = { fresh: "Water", grey: "Grey", black: "Black", fuel: "Fuel" };
const tankName = t => (!t.name || /^tank\s*\d*$/i.test(t.name.trim())) ? (TANK_LABEL[t.type] || t.name || "Tank") : t.name;
function renderHome() {
  renderQuick();
  updateFlow();
  renderHeatQuick();
  $("#coolant-therm").innerHTML = coolantThermo(k => S[`climate.rixens.${k}`]);
  { // setpoint ± on the Home climate card (same debounced path as the Climate page)
    const sp = spDisplay(), ok = canControl() && !ctlPending;
    $("#hsp-val").textContent = sp == null ? "—" : `${sp}°`;
    $("#hsp-note").textContent = spPending != null ? "sending…" : "set";
    $("#hsp-up").disabled = $("#hsp-down").disabled = !ok || sp == null;
  }
  $("#home-cabin").textContent = fmtT(S["climate.cabin.temp"]);
  $("#home-out").textContent = fmtT(S["climate.outside.temp"]);
  for (const r of ["fridge", "freezer"]) {
    const st = coldStatus(r);
    $(`#home-${r}`).textContent = fmtT(S[`climate.${r}.temp`]);
    $(`#home-${r}-l`).innerHTML = st.cls === "warn" ? `<span class="warn-t">${icon("alert")}${r} ${st.label}</span>` : r;
  }
  const heating = S["climate.rixens.state"] === "heating" || S["climate.espar.state"] === "heating";
  $("#home-heat-pill").innerHTML = S["climate.rixens.state"] == null && S["climate.espar.state"] == null
    ? `<span class="pill">not connected</span>`
    : `<span class="pill ${heating ? "heat" : ""}">${icon(heating ? "flame" : "snow")}${heating ? "Heating" : "Idle"}</span>`;
  const tanks = S["victron.tanks"] || [];
  $("#tanks").innerHTML = tanks.length ? tanks.map(t => `
    <div class="tank">
      <div class="tank-body"><div class="tank-fill" style="height:${Math.max(0, Math.min(100, t.level ?? 0))}%;background:color-mix(in srgb, var(${TANK_COLOR[t.type] || "--c-batt"}) 70%, transparent)"></div>
        <div class="tank-pct">${t.level == null ? "—" : Math.round(t.level) + "%"}</div></div>
      <div class="tank-name">${esc(tankName(t))}</div>
      <div class="tank-pct-out">${t.level == null ? "—" : Math.round(t.level) + "%"}</div>
    </div>`).join("") : `<div class="tank-empty">No tank sensors reported by the Cerbo</div>`;
  const ttg = S["victron.battery.ttg"];
  $("#soc-sub").textContent = ttg ? `${fmtTTG(ttg)} remaining at current draw` : "";
}

// ── Energy ───────────────────────────────────────────────────────────────────
const VEBUS_STATE = { 0: "Off", 1: "Low power", 2: "Fault", 3: "Bulk", 4: "Absorption", 5: "Float", 6: "Storage", 7: "Equalize",
  8: "Passthru", 9: "Inverting", 10: "Power assist", 11: "Power supply", 244: "Sustain", 252: "External control" };
const MPPT_STATE = { 0: "Off", 2: "Fault", 3: "Bulk", 4: "Absorption", 5: "Float", 6: "Storage", 7: "Equalize", 252: "External control" };
const MODES = [[3, "On"], [1, "Charger only"], [2, "Inverter only"], [4, "Off"]];
function renderPowerControls() {
  const V = k => S[`victron.vebus.${k}`], M = k => S[`victron.mppt.${k}`];
  const el = $("#pwr-ctl");
  if (!V("instance") && !M("instance")) { el.innerHTML = `<div class="card-h"><h2>Inverter & solar</h2></div><div class="muted">${S["victron.connected"] ? "No MultiPlus or MPPT reported by the Cerbo." : "Cerbo GX not connected."}</div>`; return; }
  const ok = canControlDev("victron") && !ctlPending;
  const mode = V("mode"), lim = limPending ?? V("current_limit");
  const why = !CAN_CTL ? "view only" : !S["victron.controls_ok"] ? "control off" : "";
  el.innerHTML = `<div class="card-h"><h2>Inverter & solar</h2><span class="muted">${esc(why)}</span></div>
    <div class="pwr-row">
      <div class="pwr-blk pwr-main"><div class="pwr-t">${icon("inverter")}<div><b>${esc(V("name") || "MultiPlus")}</b>
        <span>${esc(VEBUS_STATE[V("state")] ?? "—")}${V("ac_out_w") != null ? ` · ${fmtW(V("ac_out_w"))} AC out` : ""}${V("shore_connected") ? " · on shore" : ""}</span></div></div>
        <div class="seg mode-seg">${MODES.map(([m, l]) => `<button data-mode="${m}" class="${mode === m ? "on" : ""}" ${ok && V("mode_adjustable") && mode !== m ? "" : "disabled"}>${l}</button>`).join("")}</div></div>
      <div class="pwr-blk sm"><div class="pwr-t">${icon("plug")}<div><b>Shore limit</b><span>${V("limit_adjustable") ? "AC input current" : "not adjustable now"}</span></div></div>
        <div class="ctl-row lim"><button class="ctl-btn sm" id="lim-down" ${ok && V("limit_adjustable") ? "" : "disabled"}>−</button>
          <div class="ctl-val sm"><b>${lim == null ? "—" : `${Math.round(lim)} A`}</b></div>
          <button class="ctl-btn sm" id="lim-up" ${ok && V("limit_adjustable") ? "" : "disabled"}>+</button></div></div>
      <div class="pwr-blk sm"><div class="pwr-t">${icon("panel")}<div><b>${esc(M("name") || "Solar charger")}</b><span>${esc(MPPT_STATE[M("state")] ?? "—")}</span></div></div>
        <div class="seg mode-seg">${[[1, "On"], [4, "Off"]].map(([m, l]) => `<button data-mppt="${m}" class="${M("mode") === m ? "on" : ""}" ${ok && M("mode") !== m ? "" : "disabled"}>${l}</button>`).join("")}</div></div>
    </div>`;
}
$("#pwr-ctl").addEventListener("click", e => {
  const b = e.target.closest("button"); if (!b || b.disabled) return;
  if (b.dataset.mode) {
    const m = +b.dataset.mode, label = MODES.find(x => x[0] === m)[1];
    const warn = { 3: "Turns the MultiPlus on: it will invert to the AC outlets and charge from shore when connected (~25 W idle draw).",
      2: "Inverter only: AC outlets powered from the batteries; shore power is ignored.", 1: "Charger only: charges from shore; AC outlets are NOT powered from the batteries.",
      4: "Turns the MultiPlus off: AC outlets go dead and it will not charge from shore." }[m];
    askConfirm(`MultiPlus: ${label}?`, warn, () => control("inverter_mode", m, `MultiPlus ${label}`, "victron"));
  } else if (b.dataset.mppt) {
    const on = b.dataset.mppt === "1";
    askConfirm(`Solar charger ${on ? "on" : "off"}?`, on ? "Re-enables solar charging." : "Stops ALL solar charging until turned back on.",
      () => control("mppt_mode", +b.dataset.mppt, `Solar charger ${on ? "on" : "off"}`, "victron"));
  } else if (b.id === "lim-down" || b.id === "lim-up") {
    const cur = limPending ?? S["victron.vebus.current_limit"]; if (cur == null) return;
    limPending = Math.min(50, Math.max(3, Math.round(cur) + (b.id === "lim-up" ? 1 : -1)));
    renderPowerControls();
    clearTimeout(limTimer);
    limTimer = setTimeout(() => control("shore_limit", limPending, `Shore limit ${limPending} A`, "victron"), 1500);
  }
});

// ── Battery bank card: Lynx shunt for the bank, EPOCH batteries over Bluetooth for each battery ──
const BATT_AH = 460;                         // EPOCH B12460A nominal capacity per battery
// Charge % from Ah: one decimal below 100 %, none at or above (a battery can read past its rated capacity).
const fmtPct = p => p == null ? "—" : p >= 99.95 ? `${Math.round(p)}` : p.toFixed(1);   // 99.96 → "100", not "100.0"
// 3D battery-bank scene: each battery is a stack of four glowing discs (one per 25 %) on a shared platform,
// wired in parallel by animated + / − buses. Built once per battery count, then updated in place every
// second (rebuilding would restart the animations).
const BS = { w: 800, h: 300, rx: 56, ry: 16, dh: 24, step: 31, yb0: 202 };
const bankHist = {};                         // battery name → [[t, amps]] for the sparklines (2 min, client-side)
const discSide = (cx, yt, yb) => `M${cx - BS.rx} ${yt} V${yb} A${BS.rx} ${BS.ry} 0 0 0 ${cx + BS.rx} ${yb} V${yt} Z`;
function bankScene(names) {
  const n = names.length, W = BS.w, cxs = names.map((_, i) => (i + .5) * W / n);
  const grad = (id, stops, attrs = 'x1="0" x2="1" y1="0" y2="0"') =>
    `<linearGradient id="${id}" ${attrs}>${stops.map(([o, c, a = 1]) => `<stop offset="${o}" stop-color="${c}" stop-opacity="${a}"/>`).join("")}</linearGradient>`;
  // aethersdr.com tokens: --bg-2/--bg-3 navy, --cyan-deep #3aa7ff → --cyan #5de3ff → --aqua #8ef7e6 (their --grad);
  // low SoC uses their line tokens --l-hpsdr #F2B441 (amber) and --l-native #EE7355 (coral).
  // (toned down: the gradient's bright end is reserved for a thin highlight, not the whole body)
  const hues = { blue: ["#0a1422", "#1d5f9e", "#3aa7ff", "#6fd0ea"], amber: ["#1f1606", "#8a5f14", "#c9942f", "#e9c77d"],
                 red: ["#1f0d09", "#8a3a29", "#c9604a", "#e9a594"] };
  let defs = "";
  for (const [k, [d0, d1, d2, d3]] of Object.entries(hues)) {
    defs += grad(`bg-side-${k}`, [[0, d0], [.18, d1], [.5, d2], [.58, d3], [.68, d2], [.86, d1], [1, d0]]);
    defs += `<radialGradient id="bg-top-${k}" cx=".45" cy=".35" r=".8"><stop offset="0" stop-color="${d2}"/><stop offset=".6" stop-color="${d1}"/><stop offset="1" stop-color="${d0}"/></radialGradient>`;
    defs += `<radialGradient id="bg-pool-${k}"><stop offset="0" stop-color="${d2}" stop-opacity=".5"/><stop offset="1" stop-color="${d2}" stop-opacity="0"/></radialGradient>`;
  }
  defs += grad("bg-scan", [[0, "#8ef7e6", 0], [.5, "#8ef7e6", .55], [1, "#8ef7e6", 0]], 'x1="0" x2="0" y1="0" y2="1"')
       + grad("bg-slab", [[0, "#0e1a2a"], [1, "#060b13"]], 'x1="0" x2="0" y1="0" y2="1"')
       + grad("bg-spark", [[0, "#5de3ff", .42], [1, "#3aa7ff", 0]], 'x1="0" x2="0" y1="0" y2="1"')
       + grad("bg-sparkline", [[0, "#3aa7ff"], [.42, "#5de3ff"], [1, "#8ef7e6"]])
       + `<radialGradient id="bg-slabtop" cx=".5" cy=".4" r=".7"><stop offset="0" stop-color="#12233a" stop-opacity=".95"/><stop offset="1" stop-color="#0a121e" stop-opacity=".95"/></radialGradient>
          <filter id="bg-glow" filterUnits="userSpaceOnUse" x="0" y="0" width="${BS.w}" height="${BS.h}"><feGaussianBlur stdDeviation="3.5" result="b"/><feMerge><feMergeNode in="b"/><feMergeNode in="SourceGraphic"/></feMerge></filter>`;
  const pY = BS.yb0 + 12, slab = `<ellipse cx="${W / 2}" cy="${pY + 12}" rx="${W / 2 - 8}" ry="36" fill="url(#bg-slab)"/>
    <ellipse cx="${W / 2}" cy="${pY}" rx="${W / 2 - 8}" ry="36" fill="url(#bg-slabtop)" class="bg-slab-top"/>
    <ellipse cx="${W / 2}" cy="${pY}" rx="${W / 2 - 22}" ry="30" class="bg-slab-line"/>
    <ellipse cx="${W / 2}" cy="${pY}" rx="${W / 2 - 8}" ry="36" class="bg-slab-edge bg-halo-line"/>
    <ellipse cx="${W / 2}" cy="${pY}" rx="${W / 2 - 8}" ry="36" class="bg-slab-edge"/>`;
  let clips = "", stacks = "", posts = "";
  const topY = BS.yb0 - 3 * BS.step - BS.dh;              // top of the top disc's body
  cxs.forEach((cx, i) => {
    clips += `<clipPath id="bg-stk${i}"><rect x="${cx - BS.rx}" y="${topY}" width="${2 * BS.rx}" height="${BS.yb0 - topY + BS.ry}"/></clipPath>`;   // a plain rect: cheap to repaint
    let discs = "";
    for (let k = 0; k < 4; k++) {
      const yb = BS.yb0 - k * BS.step, yt = yb - BS.dh;
      clips += `<clipPath id="bg-c${i}-${k}"><rect id="bg-cr${i}-${k}" x="${cx - BS.rx - 2}" y="${yb + BS.ry + 2}" width="${2 * BS.rx + 4}" height="0"/></clipPath>`;
      discs += `<g class="bg-disc">
        <path d="${discSide(cx, yt, yb)}" class="bg-dim"/><ellipse cx="${cx}" cy="${yt}" rx="${BS.rx}" ry="${BS.ry}" class="bg-dim-top"/>
        <g clip-path="url(#bg-c${i}-${k})" class="bg-lit"><path id="bg-ls${i}-${k}" d="${discSide(cx, yt, yb)}"/>
          <ellipse id="bg-lt${i}-${k}" cx="${cx}" cy="${yt}" rx="${BS.rx}" ry="${BS.ry}"/></g>
        <ellipse id="bg-sf${i}-${k}" cx="${cx}" cy="${yb}" rx="${BS.rx - 1}" ry="${BS.ry - 1}" class="bg-surface" opacity="0"/>
        <ellipse cx="${cx}" cy="${yt}" rx="${BS.rx}" ry="${BS.ry}" class="bg-rim"/></g>`;
    }
    stacks += `<g id="bg-s${i}" class="bg-stack">
      <ellipse id="bg-halo${i}" cx="${cx}" cy="${BS.yb0 - 1.6 * BS.step}" rx="${BS.rx + 34}" ry="${2.6 * BS.step}" fill="url(#bg-pool-blue)" class="bg-halo"/>
      <ellipse id="bg-pool${i}" cx="${cx}" cy="${BS.yb0 + 10}" rx="${BS.rx + 26}" ry="${BS.ry + 10}" fill="url(#bg-pool-blue)"/>
      <ellipse cx="${cx}" cy="${BS.yb0 + 10}" rx="${BS.rx + 12}" ry="${BS.ry + 6}" class="bg-ring" style="animation-delay:${i * .35}s"/>
      ${discs}
      <g clip-path="url(#bg-stk${i})"><rect class="bg-scan" x="${cx - BS.rx}" y="${BS.yb0 + BS.ry - 30}" width="${2 * BS.rx}" height="30" fill="url(#bg-scan)"/></g>
      <text id="bg-pct${i}" x="${cx}" y="${BS.yb0 - 1.5 * BS.step - BS.dh / 2 + 6}" class="bg-pct">—</text>
      <text x="${cx}" y="${pY + 26}" class="bg-name">${esc(names[i])}</text>
      <path id="bg-sa${i}" class="bg-spark-area" d=""/><path id="bg-sl${i}" class="bg-spark" d=""/>
      <line x1="${cx - 78}" x2="${cx + 34}" y1="${BS.h - 6}" y2="${BS.h - 6}" class="bg-zero"/>
      <text id="bg-amp${i}" x="${cx + 40}" y="${BS.h - 13}" class="bg-amp"></text></g>`;
    posts += `<line x1="${cx - 22}" y1="${topY - 4}" x2="${cx - 22}" y2="26" class="bg-post-p"/><line x1="${cx + 22}" y1="${topY - 4}" x2="${cx + 22}" y2="42" class="bg-post-n"/>
      <line x1="${cx - 22}" y1="${topY - 4}" x2="${cx - 22}" y2="26" class="bg-flow bg-flow-post"/><line x1="${cx + 22}" y1="${topY - 4}" x2="${cx + 22}" y2="42" class="bg-flow bg-flow-post"/>`;
  });
  const xl = cxs[0] - 22, xr = cxs[n - 1] + 22;
  const buses = `<path d="M${xl} 26 H${W - 14}" class="bg-bus-p bg-halo-line"/><path d="M${xr} 42 H14" class="bg-bus-n bg-halo-line"/>
    <path d="M${xl} 26 H${W - 14}" class="bg-bus-p"/><path d="M${xr} 42 H14" class="bg-bus-n"/>
    <path d="M${xl} 26 H${W - 14}" class="bg-flow bg-flow-bus"/><path d="M${xr} 42 H14" class="bg-flow bg-flow-bus"/>
    <text x="${W - 6}" y="31" class="bg-pol bg-pol-p">+</text><text x="6" y="47" class="bg-pol bg-pol-n">−</text>`;
  return `<svg class="bank-scene" id="bank-scene" data-n="${n}" viewBox="0 0 ${W} ${BS.h}" role="img" aria-label="${n} batteries in parallel">
    <defs>${defs}${clips}</defs>${slab}${posts}${buses}${stacks}</svg>`;
}
function updateBankScene(batts, bankAmps) {
  const svg = $("#bank-scene"); if (!svg) return;
  const now = Date.now() / 1000;
  const flow = bankAmps == null ? "" : bankAmps > 0.5 ? "chg" : bankAmps < -0.5 ? "dis" : "";
  svg.classList.toggle("chg", flow === "chg"); svg.classList.toggle("dis", flow === "dis");
  svg.style.setProperty("--flow-dur", `${Math.max(.5, Math.min(4, 4 - Math.abs(bankAmps || 0) / 12)).toFixed(2)}s`);
  batts.forEach(b => {
    const h = (bankHist[b.name] ||= []);
    if (b.ok && b.current != null) {
      const ts = Math.round(now - (b.age || 0));
      if (!h.length || h[h.length - 1][0] !== ts) h.push([ts, +b.current]);
    }
    while (h.length && h[0][0] < now - 120) h.shift();
  });
  batts.forEach((b, i) => {
    // Charge = available Ah ÷ capacity (finer than the BMS's own SoC, which sits at 100 % for a long time);
    // falls back to the BMS SoC if the battery doesn't report remaining Ah.
    const pct = !b.ok ? null : b.remaining_ah != null ? b.remaining_ah / BATT_AH * 100 : b.soc;
    const cx = (i + .5) * BS.w / batts.length, soc = pct == null ? null : Math.max(0, Math.min(100, pct));
    const hue = soc == null ? "blue" : soc < 15 ? "red" : soc < 30 ? "amber" : "blue";
    $(`#bg-s${i}`)?.classList.toggle("off", soc == null);
    $(`#bg-pool${i}`)?.setAttribute("fill", `url(#bg-pool-${hue})`);
    $(`#bg-pool${i}`)?.setAttribute("opacity", soc == null ? 0 : (.12 + .3 * soc / 100).toFixed(2));
    $(`#bg-halo${i}`)?.setAttribute("fill", `url(#bg-pool-${hue})`);
    $(`#bg-halo${i}`)?.setAttribute("opacity", soc == null ? 0 : (.04 + .1 * soc / 100).toFixed(2));
    for (let k = 0; k < 4; k++) {
      const f = soc == null ? 0 : Math.max(0, Math.min(1, (soc - 25 * k) / 25));
      const yb = BS.yb0 - k * BS.step, yt = yb - BS.dh;
      const top = f >= 1 ? yt - BS.ry - 2 : yb - f * BS.dh;
      const cr = $(`#bg-cr${i}-${k}`);
      cr.setAttribute("y", f > 0 ? top.toFixed(1) : yb + BS.ry + 2); cr.setAttribute("height", f > 0 ? (yb + BS.ry + 2 - top).toFixed(1) : 0);
      $(`#bg-ls${i}-${k}`).setAttribute("fill", `url(#bg-side-${hue})`);
      $(`#bg-lt${i}-${k}`).setAttribute("fill", `url(#bg-top-${hue})`);
      const sf = $(`#bg-sf${i}-${k}`);                     // liquid surface on the partly-filled quarter
      sf.setAttribute("cy", (yb - f * BS.dh).toFixed(1)); sf.setAttribute("fill", `url(#bg-top-${hue})`);
      sf.setAttribute("opacity", f > 0 && f < 1 ? .9 : 0);        // same 90 % alpha as the charged body
    }
    $(`#bg-pct${i}`).textContent = pct == null ? "—" : `${fmtPct(pct)}%`;
    $(`#bg-amp${i}`).textContent = b.ok && b.current != null ? `${(+b.current).toFixed(1)} A` : "";
    // Sparkline of this battery's current over 2 min, scaled to its own range (≥ 0.4 A) so small swings show.
    const h = bankHist[b.name] || [], x0 = cx - 78, x1 = cx + 34, yTop = BS.h - 32, yBot = BS.h - 6;
    if (h.length > 1) {
      let lo = Math.min(...h.map(p => p[1])), hi = Math.max(...h.map(p => p[1]));
      if (hi - lo < .4) { const m = (hi + lo) / 2; lo = m - .2; hi = m + .2; }
      const pts = h.map(([t, a]) => [x0 + (x1 - x0) * (1 - (now - t) / 120), yBot - (a - lo) / (hi - lo) * (yBot - yTop)]);
      const d = pts.map(([x, y], j) => `${j ? "L" : "M"}${x.toFixed(1)} ${y.toFixed(1)}`).join("");
      $(`#bg-sl${i}`).setAttribute("d", d);
      $(`#bg-sa${i}`).setAttribute("d", `${d}L${pts[pts.length - 1][0].toFixed(1)} ${yBot}L${pts[0][0].toFixed(1)} ${yBot}Z`);
    }
  });
}
function renderBattCard() {
  const soc = S["victron.battery.soc"], bp = S["victron.battery.power"];
  const st = bp == null ? ["", "", ""] : bp > 5 ? ["Charging", "bolt", "chg"] : bp < -5 ? ["Discharging", "battery", "dis"] : ["Standby", "battery", ""];
  const ble = S["batteries.ble"] || [], ok = ble.filter(b => b.ok);
  const cells = ok.flatMap(b => b.cells || []);
  const avg = cells.length ? cells.reduce((a, c) => a + c, 0) / cells.length : null;
  const lo = cells.length ? Math.min(...cells) : null, hi = cells.length ? Math.max(...cells) : null;
  const rem = ok.length ? ok.reduce((a, b) => a + (b.remaining_ah || 0), 0) : null;
  const withAh = ok.filter(b => b.remaining_ah != null);
  // Headline: available Ah ÷ capacity of the batteries reporting (same as the glyphs); shunt SoC as fallback.
  const bankPct = withAh.length ? withAh.reduce((a, b) => a + b.remaining_ah, 0) / (BATT_AH * withAh.length) * 100 : soc;
  const tmax = (i0, i1) => { const v = ok.flatMap(b => (b.temps || []).slice(i0, i1)); return v.length ? fmtT(Math.max(...v)) : "—"; };
  const v3 = v => v == null ? "—" : `${v.toFixed(3)} V`;
  const row = b => b.ok ? `<tr>
      <td><b>${esc(b.name)}</b></td><td class="soc-c" style="color:${socColor(b.soc)}">${b.soc ?? "—"}%</td>
      <td>${(+b.voltage).toFixed(2)} V</td><td>${b.current == null ? "—" : (+b.current).toFixed(1) + " A"}</td>
      <td>${b.current == null ? "—" : fmtW(b.voltage * b.current)}</td><td>${b.cycles ?? "—"}</td>
      <td>${b.remaining_ah == null ? "—" : (+b.remaining_ah).toFixed(1)}<small>/${BATT_AH}</small></td>
      <td title="${(b.temps || []).length >= 4 ? `T1 ${fmtT(b.temps[0])} · T2 ${fmtT(b.temps[1])} · ambient ${fmtT(b.temps[2])} · MOS ${fmtT(b.temps[3])}` : ""}">${(b.temps || []).length ? fmtT(Math.max(...b.temps.slice(0, 2))) : "—"}</td>
      <td class="mos"><i class="${b.charge_fet ? "on" : "off"}" title="Charge MOS ${b.charge_fet ? "on" : "off"}">C</i><i class="${b.discharge_fet ? "on" : "off"}" title="Discharge MOS ${b.discharge_fet ? "on" : "off"}">D</i></td></tr>`
    : `<tr class="na"><td><b>${esc(b.name)}</b></td><td colspan="8" class="muted">${esc(b.error ? "no data · reconnecting" : "connecting…")}</td></tr>`;
  const card = $("#batt-card"), names = ble.map(b => b.name).join("|");
  if (card.dataset.names !== names) {                    // (re)build the skeleton only when the battery set changes
    card.dataset.names = names;
    card.innerHTML = `<div class="bc-head" id="bc-head"></div>${ble.length ? bankScene(ble.map(b => b.name)) : ""}
      ${ble.length ? `<div class="bc-cells" id="bc-cells"></div>
      <table class="bc-table"><thead><tr><th>Battery</th><th>SoC</th><th>Volts</th><th>Amps</th><th>Power</th><th>Cycles</th><th>Ah</th><th>Temp</th><th>MOS</th></tr></thead>
        <tbody id="bc-rows"></tbody></table>` : `<div class="muted">No per-battery data (Bluetooth adapter off).</div>`}`;
  }
  $("#bc-head").innerHTML = `
      <div class="bc-soc" style="color:${bankPct == null ? "var(--muted)" : "var(--ink)"}" title="${withAh.length ? `${withAh.length} batteries: available Ah ÷ ${BATT_AH * withAh.length} Ah` : "Lynx shunt SoC"}">${fmtPct(bankPct)}<small>%</small></div>
      <div class="bc-sum"><span class="bc-state ${st[2]}">${st[1] ? icon(st[1]) : ""}${st[0]}</span>
        <span>${num(S["victron.battery.voltage"], 2)} V · ${num(S["victron.battery.current"], 1)} A · ${fmtTTG(S["victron.battery.ttg"])} to go</span>
        <span class="muted">${rem == null ? "" : `${Math.round(rem)} / ${BATT_AH * ble.length} Ah · `}${ok.length} of ${ble.length} batteries via Bluetooth</span></div>`;
  if (!ble.length) return;
  updateBankScene(ble, S["batteries.ble.current"] ?? S["victron.battery.current"]);
  $("#bc-cells").innerHTML = `
      <div><span>VAVG</span><b>${v3(avg)}</b></div><div><span>VDIFF</span><b class="${hi != null && hi - lo > .05 ? "warn" : ""}">${v3(hi == null ? null : hi - lo)}</b></div>
      <div><span>VLO</span><b>${v3(lo)}</b></div><div><span>VHI</span><b>${v3(hi)}</b></div>
      <div><span>Cells T1/T2</span><b>${tmax(0, 2)}</b></div><div><span>Ambient</span><b>${tmax(2, 3)}</b></div>
      <div><span>MOS</span><b>${tmax(3, 4)}</b></div><div><span>Battery (shunt)</span><b>${fmtT(S["victron.battery.temp"])}</b></div>`;
  $("#bc-rows").innerHTML = ble.map(row).join("");
}

function renderEnergy() {
  renderPowerControls();
  renderBattCard();
  const stats = [
    ["panel", "--c-solar", fmtW(S["victron.pv.power"]), "Solar", S["victron.pv.power"]],
    ["engine", "--c-alt", fmtW(S["energy.alternator.power"]), "Alternator", S["energy.alternator.power"]],
    ["plug", "--c-shore", fmtW(S["victron.ac.shore"]), "Shore", S["victron.ac.shore"]],
    ["dcload", "--c-load", fmtW(S["energy.dc.load"]), "DC loads", S["energy.dc.load"]],
    ["outlet", "--c-load", fmtW(S["victron.ac.load"]), "AC loads", S["victron.ac.load"]],
    ["thermo", "--ink-2", fmtT(S["victron.battery.temp"]), "Battery temp", S["victron.battery.temp"] == null ? 0 : 99],
  ];
  $("#energy-stats").innerHTML = stats.map(([ic, c, v, l, raw]) => {
    const [n, u] = v.split(" ");
    return `<div class="stat ${!raw || raw < 3 ? "idle" : ""}"><div class="stat-ic" style="color:var(${c})">${icon(ic)}</div>
      <div><div class="stat-v">${n}${u ? `<small>${u}</small>` : ""}</div><div class="stat-l">${l}</div></div></div>`;
  }).join("");
}

// ── Climate ──────────────────────────────────────────────────────────────────
function renderClimate() {
  const cabin = S["climate.cabin.temp"], sp = S["climate.cabin.setpoint"];
  const [lo, hi] = UNIT === "F" ? [40, 90] : [5, 32];
  $("#dial").innerHTML = dial(tC(cabin), tC(spPending ?? sp), { min: lo, max: hi, size: DIAL_SIZE, stroke: DIAL_STROKE,
    color: "var(--c-cabin)", active: spDragging });
  $("#dial").classList.toggle("draggable", canControl() && !ctlPending);
  $("#dial-temp").textContent = fmtT(cabin);
  const rh = S["climate.cabin.humidity"];
  const fr = S["climate.cabin.front_temp"], bk = S["climate.cabin.rear_temp"];
  $("#dial-label").innerHTML = esc(rh == null ? "cabin" : `cabin · ${Math.round(rh)}% RH`) +
    (S["climate.cabin.source"] === "average" ? `<span class="dial-split">average of front ${fmtT(fr)} · back ${fmtT(bk)}</span>` : "");
  renderClimateControls();
  const ids = d => (S[`${d}.ids`] || []).length;
  const R = k => S[`climate.rixens.${k}`];
  const rState = R("state");
  const live = R("live");
  const avail = R("available") || {};
  const srcs = R("sources") || [];
  const heaters = [
    { id: "rixens", name: "Rixens MCS7", sub: live ? `Hydronic · ${esc(R("heater_state") || "")}` : "Hydronic heat", state: rState,
      buttons: live ? ["furnace", "electric", "engine"].filter(k => k === "furnace" || avail[k]).map(k => ({ k, on: srcs.includes(k), st: R(k) })) : [],
      // Hot water / pre-heat show as unlabelled icon badges under the cabin-air-heat badge; status only here.
      modes: live ? [["Hot water", "hot_water", R("hot_water")], ["Preheat", "preheat", R("preheat")]] : [],
      chips: live && avail.floor ? [["Floor", R("floor") !== "off"]] : [],
      kv: rState == null ? [] : live ? [
        ["Coolant", fluidPill(R)],
        ["Battery", R("battery_v") == null ? "—" : `${num(R("battery_v"), 1)} V`],
        ["Runtime", R("runtime_h") == null ? "—" : `${Math.round(R("runtime_h")).toLocaleString()} h`],
        ["Fuel today", R("fuel_today_gal") == null ? "—" : `${(+R("fuel_today_gal")).toFixed(R("fuel_today_gal") < 0.1 ? 3 : 2)} gal`],
      ] : [["Coolant", R("coolant") == null ? "—" : fmtT(R("coolant"))], ["Fan speed", R("fan") ?? "—"]],
      burner: live ? [
        ["Fan", R("fan_rpm") == null ? "—" : `${R("fan_rpm").toLocaleString()} rpm`, "Burner fan"],
        ["Glow", R("glow_w") == null ? "—" : `${Math.round(R("glow_w"))} W`, "Glow plug"],
        ["Fuel", R("fuel_gph") == null ? "—" : `${(+R("fuel_gph")).toFixed(3)} gal/h`, "Fuel flow"],
        ["Inlet", R("inlet_c") == null ? "—" : fmtT(R("inlet_c")), "Coolant in"],
        ["Outlet", R("outlet_c") == null ? "—" : fmtT(R("outlet_c")), "Coolant out"],
        ["Flame", R("flame_c") == null ? "—" : fmtT(R("flame_c")), "Flame sensor"],
      ] : [],
      fault: R("fault_active") },
    { id: "espar", name: "Espar Airtronic S2", sub: "D2L forced-air · EasyStart Pro", state: S["climate.espar.state"], buttons: [], chips: [],
      kv: S["climate.espar.state"] == null ? [] : [["Status", S["climate.espar.state"] === "heating" ? "On" : "Off"], ["CAN IDs seen", ids("espar")]] },
  ];
  $("#cold").innerHTML = `<div class="card-h"><h2>Refrigeration</h2><span class="muted">Ruuvi tags</span></div><div class="cold-row">` +
    [["fridge", "Fridge", "fridge"], ["freezer", "Freezer", "snow"]].map(([r, name, ic]) => {
      const st = coldStatus(r);
      return `<div class="cold-item ${st.cls}">${icon(st.cls === "warn" ? "alert" : ic)}<div><b>${fmtT(S[`climate.${r}.temp`])}</b>
        <span>${name}${st.cls === "warn" ? ` · ${st.label}` : st.cls === "ok" ? ` · below ${fmtT(S[`ui.${r}_max_c`])}` : " · no sensor"}</span></div></div>`;
    }).join("") + `</div>`;
  $("#heaters").innerHTML = heaters.map(h => {
    const on = h.state === "heating";
    const status = h.state == null ? `Not connected · ${ids(h.id)} CAN IDs seen`
      : { heating: "Heating", off: "Off", standby: "Standby", idle: "Idle", error: "ERROR", "no data": "No data" }[h.state] || h.state;
    return `<div class="card heater ${on ? "on" : ""} ${h.kv.length || h.buttons.length ? "" : "compact"}">
      <div class="badge-col"><div class="badge" ${h.id === "rixens" ? 'title="Cabin air heat"' : ""}>${icon(h.state == null ? "can" : h.id === "rixens" ? "air_heat" : "flame")}</div>
        ${(h.modes || []).map(([k, ic, mon]) => `<div class="badge mode ${mon ? "on" : ""}" title="${k}${mon ? " on" : " off"}">${icon(ic)}</div>`).join("")}</div>
      <div><h3>${h.name}</h3><div class="sub">${h.sub} · ${status}</div>
        ${h.kv.length ? `<div class="kv">${h.kv.map(([k, v]) => `<div><b>${v}</b><span>${k}</span></div>`).join("")}</div>` : ""}
        ${h.burner && h.burner.length ? `<div class="burner">${h.burner.map(([k, v, sub]) => `<div><span>${k}</span><b>${v}</b><small>${sub}</small></div>`).join("")}</div>` : ""}
        ${h.buttons.length ? `<div class="src-btns">${h.buttons.map(b => `<button class="src-btn ${b.on ? "sel" : ""} st-${b.st || "off"}" data-src="${b.k}" ${canControl() && !ctlPending ? "" : "disabled"}>
            ${icon(b.k === "furnace" ? "flame" : b.k === "electric" ? "plug" : "engine")}<span>${b.k[0].toUpperCase() + b.k.slice(1)}</span><small>${b.on ? (b.st === "active" ? "active" : "on · " + (b.st || "")) : "off"}</small></button>`).join("")}</div>` : ""}
        ${h.chips.length ? `<div class="chips-row">${h.chips.map(([k, on]) => `<span class="src ${on ? "on" : ""}">${k}${on ? " on" : ""}</span>`).join("")}</div>` : ""}
        ${h.fault ? `<div class="warn-t fault-line">${icon("alert")}Fault code ${esc(h.fault)}</div>` : ""}</div>
      <span class="ro pill">${icon(h.buttons.length && canControl() ? "check" : "lock")}${h.buttons.length && canControl() ? "control" : "read-only"}</span></div>`;
  }).join("");
}

// Fluid pill: black = system off & cold, blue = system on & cold, yellow = above 122 °F, red = above 155 °F.
// Uses coolant outlet temperature; falls back to the MCS7's own indicator if no temperature.
function fluidClass(R) {
  const t = R("outlet_c"), on = (R("sources") || []).length > 0 || R("system_heat") === true;
  const tf = t == null ? null : t * 9 / 5 + 32;
  const ind = R("glycol");
  if (tf != null ? tf > 155 : ind === "hot") return "hot";
  if (tf != null ? tf > 122 : ind === "ready") return "warm";
  return on ? "cold-on" : "cold-off";
}
function fluidPill(R) {
  const t = R("outlet_c");
  return `<span class="fluid-pill ${fluidClass(R)}">${t != null ? fmtT(t) : "—"}</span>`;
}
// Home climate card: coolant thermometer (MCS7 coolant out), same colour rules as the Climate-page pill.
// Scale 32–190 °F, with ticks at the 122 °F "ready" and 155 °F "hot" thresholds.
function coolantThermo(R) {
  const t = R("outlet_c"), cls = fluidClass(R);
  const tf = t == null ? null : t * 9 / 5 + 32;
  const lo = 32, hi = 190, top = 10, bot = 196;                 // tube y-range (SVG units)
  const yOf = f => bot - (Math.max(lo, Math.min(hi, f)) - lo) / (hi - lo) * (bot - top);
  const fillY = tf == null ? bot : yOf(tf);
  const tick = (f, l) => `<line x1="48" x2="60" y1="${yOf(f)}" y2="${yOf(f)}" class="th-tick"/><text x="62" y="${yOf(f) + 4}" class="th-tl">${l}</text>`;
  return `<svg viewBox="0 0 66 260" class="thermo ${cls}" aria-label="Coolant ${t == null ? "unknown" : fmtT(t)}">
      ${(() => { // tube + bulb as ONE outline, so the bulb's top arc between the tube walls isn't drawn
        const cy = bot + 30, R = 26, xl = 12, xr = 44, half = (xr - xl) / 2, yj = cy - Math.sqrt(R * R - half * half);
        return `<path class="th-tube" d="M${xl},${top + 8} A${half},${half} 0 0 1 ${xr},${top + 8} V${yj.toFixed(1)} A${R},${R} 0 1 1 ${xl},${yj.toFixed(1)} Z"/>`;
      })()}
      <rect x="19" y="${fillY}" width="18" height="${bot - fillY + 18}" rx="9" class="th-fill"/>
      <circle cx="28" cy="${bot + 30}" r="18" class="th-fill"/>
      ${tick(122, "")}${tick(155, "")}
    </svg>
    <div class="th-val"><b>${t == null ? "—" : fmtT(t)}</b><small>coolant</small></div>`;
}

// ── MCS7 control (touchscreen only; every command is confirmed by re-reading the MCS7) ──
const IS_LOCAL = ["127.0.0.1", "localhost", "::1", "[::1]"].includes(location.hostname);
// Control rights come from the server per connection ("hello"): the touchscreen, or the owner's own Tailscale
// devices. CAN_CTL stays for kiosk-only things (screen off / auto-off / wake shield).
let CAN_CTL = IS_LOCAL, CTL_WHO = IS_LOCAL ? "touchscreen" : "viewer";
// The kiosk is touch-only: hide the mouse pointer there (remote browsers keep theirs).
if (IS_LOCAL) document.documentElement.classList.add("kiosk");
const canControl = () => CAN_CTL && S["climate.rixens.controls_ok"] === true;
let ctlPending = null, spPending = null, spTimer = null, ctlSeq = 0, limPending = null, limTimer = null;
const ctlWaiters = {};
function control(cmd, value, label, device = "rixens") {
  if (!canControlDev(device) || ctlPending) return;
  const id = ++ctlSeq;
  ctlPending = { id, cmd, label, device };
  toast(`${label}…`, "busy");
  ctlWaiters[id] = r => { ctlPending = null; spPending = cmd === "setpoint" ? null : spPending;
    if (cmd === "shore_limit") limPending = null;
    toast(r.ok ? `${label}: ${r.detail}` : `${label} failed: ${r.detail}`, r.ok ? "ok" : "bad"); render(); };
  sendWS({ type: "control", device, cmd, value, id });
  render();
}
const canControlDev = d => CAN_CTL && S[{ victron: "victron.controls_ok", starlink: "starlink.controls_ok", n2k: "n2k.controls_ok", peplink: "peplink.controls_ok" }[d] || "climate.rixens.controls_ok"] === true;
function askConfirm(title, detail, onYes) {
  $("#modal-t").textContent = title; $("#modal-d").textContent = detail; $("#modal").hidden = false;
  $("#modal-yes").onclick = () => { $("#modal").hidden = true; onYes(); };
  $("#modal-no").onclick = () => { $("#modal").hidden = true; };
}
const F2C = f => (f - 32) / 1.8;
function spDisplay() { const c = spPending ?? S["climate.cabin.setpoint"]; return c == null ? null : Math.round(tC(c)); }
function nudgeSetpoint(d) {
  if (!canControl() || ctlPending) return;
  const cur = spDisplay(); if (cur == null) return;
  let next = cur + d;
  next = UNIT === "F" ? Math.min(90, Math.max(35, next)) : Math.min(33, Math.max(1, next));
  spPending = UNIT === "F" ? F2C(next) : next;
  render();
  clearTimeout(spTimer);   // debounce taps; send one absolute value
  spTimer = setTimeout(() => control("setpoint", +spPending.toFixed(2), `Setpoint ${next}°`), 1500);
}
// Drag the setpoint knob around the cabin dial. Snaps to whole degrees (display unit), clamps to the
// dial's range, and sends ONE absolute setpoint when the finger lifts (same path as the ± buttons).
const DIAL_SIZE = 440, DIAL_STROKE = 24;
let spDragging = false;
function dialValueAt(ev) {
  const el = $("#dial"), rc = el.getBoundingClientRect(), k = rc.width / DIAL_SIZE, g = dialGeom(DIAL_SIZE, DIAL_STROKE);
  const dx = ev.clientX - (rc.left + rc.width / 2), dy = ev.clientY - (rc.top + rc.height / 2);
  let rel = ((Math.atan2(dy, dx) * 180 / Math.PI) - g.a0 + 720) % 360;
  if (rel > g.sweep) rel = rel > g.sweep + (360 - g.sweep) / 2 ? 0 : g.sweep;   // bottom gap → nearest end
  const [lo, hi] = UNIT === "F" ? [40, 90] : [5, 32];
  return { v: Math.round(lo + (hi - lo) * rel / g.sweep), dist: Math.hypot(dx, dy) / k, dx: dx / k, dy: dy / k };
}
$("#dial").addEventListener("pointerdown", ev => {
  if (!canControl() || ctlPending) return;
  const cur = spDisplay(); if (cur == null) return;
  const g = dialGeom(DIAL_SIZE, DIAL_STROKE), [lo, hi] = UNIT === "F" ? [40, 90] : [5, 32];
  const a = (g.a0 + g.sweep * (Math.max(lo, Math.min(hi, cur)) - lo) / (hi - lo)) * Math.PI / 180;
  const { dx, dy } = dialValueAt(ev);
  if (Math.hypot(dx - g.r * Math.cos(a), dy - g.r * Math.sin(a)) > 60) return;   // must grab the knob
  ev.preventDefault();
  $("#dial").setPointerCapture(ev.pointerId);
  spDragging = true; clearTimeout(spTimer);
  renderClimate();
});
$("#dial").addEventListener("pointermove", ev => {
  if (!spDragging) return;
  const { v } = dialValueAt(ev);
  const c = UNIT === "F" ? F2C(v) : v;
  if (spPending == null || Math.abs(c - spPending) > 1e-6) { spPending = c; renderClimate(); }
});
const endDrag = ev => {
  if (!spDragging) return;
  spDragging = false;
  const next = spDisplay(), cur = S["climate.cabin.setpoint"];
  if (spPending != null && (cur == null || Math.round(tC(cur)) !== next)) {
    control("setpoint", +spPending.toFixed(2), `Setpoint ${next}°`);
  } else { spPending = null; }
  renderClimate();
};
$("#dial").addEventListener("pointerup", endDrag);
$("#dial").addEventListener("pointercancel", ev => { if (spDragging) { spDragging = false; spPending = null; renderClimate(); } });

function nudgeFan(d) {
  const f = S["climate.rixens.fan_speed"];
  const base = typeof f === "number" ? f : 50;
  const next = Math.min(100, Math.max(0, Math.round((base + d) / 10) * 10));
  control("fan", next, `Fan ${next}%`);
}
function renderClimateControls() {
  const ok = canControl() && !ctlPending;
  const live = S["climate.rixens.live"];
  const sp = spDisplay();
  $("#sp-val").textContent = sp == null ? "—" : `${sp}°`;
  $("#sp-note").textContent = !live ? "setpoint (MCS7 not connected)" : spDragging ? "setpoint · release to set" : spPending != null ? "setpoint · sending…" :
    !CAN_CTL ? "setpoint · view only" : !S["climate.rixens.controls_ok"] ? "setpoint · control off" : "setpoint";
  const f = S["climate.rixens.fan_speed"];
  const fa = S["climate.rixens.fan_auto_pct"], fanOn = S["climate.rixens.fan_running"];
  $("#fan-val").textContent = f == null ? "—" : f === "auto" ? (fa == null ? "Auto" : `${fanOn === false ? 0 : fa}%`)
    : f === "off" ? "Off" : `${f}%`;
  $("#fan-note").textContent = f === "auto" ? "fan · auto" : "fan";
  $("#fan-auto").classList.toggle("on", f === "auto");
  for (const id of ["#sp-down", "#sp-up", "#fan-auto", "#fan-down", "#fan-up"]) $(id).disabled = !ok;
}
$("#sp-down").addEventListener("click", () => nudgeSetpoint(-1));
$("#sp-up").addEventListener("click", () => nudgeSetpoint(+1));
$("#fan-auto").addEventListener("click", () => control("fan", "auto", "Fan auto"));
$("#fan-down").addEventListener("click", () => nudgeFan(-10));
$("#fan-up").addEventListener("click", () => nudgeFan(+10));
$("#heaters").addEventListener("click", e => {
  const b = e.target.closest("[data-src]"); if (!b || b.disabled) return;
  const k = b.dataset.src, on = !b.classList.contains("sel");
  const what = { furnace: "the diesel furnace (Eberspächer burner)", electric: "the 1500 W electric element (shore power only!)", engine: "engine heat" }[k];
  askConfirm(`${on ? "Turn on" : "Turn off"} ${k}?`, `This ${on ? "enables" : "disables"} ${what} on the MCS7. The MCS7 decides when to actually fire it.`,
    () => control(k, on, `${k[0].toUpperCase() + k.slice(1)} ${on ? "on" : "off"}`));
});

// ── Battery overlay (tap the battery ring on Home) ───────────────────────────
let battmOpen = false;
function openBattModal() { battmOpen = true; $("#batt-modal").hidden = false; renderBattModal(); }
function closeBattModal() { battmOpen = false; $("#batt-modal").hidden = true; }
function renderBattModal() {
  if (!battmOpen) return;
  const bl = S["victron.batteries"] || [];
  const mon = bl.find(b => b.kind === "monitor"), bms = bl.find(b => b.kind === "bms");
  const exp = S["ui.bms_expected"] || 0;
  const kv = rows => `<div class="battm-kv">${rows.map(([k, v, cls]) => `<div class="${cls || ""}"><span>${k}</span><b>${v}</b></div>`).join("")}</div>`;
  const a = v => v == null ? "—" : `${(+v).toFixed(1)} A`;
  let html = "";
  if (mon) {
    html += `<section class="battm-card"><h4>${icon("battery")}${esc(mon.CustomName || mon.ProductName || "Battery monitor")}<small>bank measurement</small></h4>
      <div class="battm-big">${mon.Soc == null ? "—" : Math.round(mon.Soc) + "%"}</div>` + kv([
        ["Voltage", mon["Dc/0/Voltage"] == null ? "—" : `${(+mon["Dc/0/Voltage"]).toFixed(2)} V`],
        ["Current", a(mon["Dc/0/Current"])],
        ["Power", fmtW(mon["Dc/0/Power"])],
        ["Consumed", mon.ConsumedAmphours == null ? "—" : `${Math.round(mon.ConsumedAmphours)} Ah`],
        ["Time to go", fmtTTG(mon.TimeToGo)],
        ["Temperature", mon["Dc/0/Temperature"] == null ? "—" : fmtT(mon["Dc/0/Temperature"])],
      ]) + `</section>`;
  }
  if (bms) {
    const on = bms["System/NrOfModulesOnline"], short = exp && on != null && on < exp;
    const vmin = bms["System/MinCellVoltage"], vmax = bms["System/MaxCellVoltage"];
    const spread = vmin != null && vmax != null ? Math.round((vmax - vmin) * 1000) : null;
    const alarms = Object.keys(bms.active_alarms || {});
    html += `<section class="battm-card"><h4>${icon("chip")}${esc(bms.CustomName || bms.ProductName || "BMS")}<small>BMS (CAN-bus)</small></h4>
      <div class="battm-mods ${short ? "warn" : "ok"}">${icon(short ? "alert" : "check")}<b>${on ?? "—"}${exp ? ` of ${exp}` : ""}</b> batteries reporting</div>` + kv([
        ["BMS SoC / SoH", `${bms.Soc == null ? "—" : Math.round(bms.Soc) + "%"} / ${bms.Soh == null ? "—" : Math.round(bms.Soh) + "%"}`],
        ["BMS current", a(bms["Dc/0/Current"])],
        ["Cells", vmin == null ? "—" : `${(+vmin).toFixed(3)}–${(+vmax).toFixed(3)} V`],
        ["Cell spread", spread == null ? "—" : `${spread} mV`, spread != null && spread > 50 ? "warn" : ""],
        ["Cell temp", bms["System/MinCellTemperature"] == null ? "—" : `${fmtT(bms["System/MinCellTemperature"])}–${fmtT(bms["System/MaxCellTemperature"])}`],
        ["Charge limit", `${a(bms["Info/MaxChargeCurrent"])} @ ${bms["Info/MaxChargeVoltage"] == null ? "—" : (+bms["Info/MaxChargeVoltage"]).toFixed(1) + " V"}`],
        ["Discharge limit", a(bms["Info/MaxDischargeCurrent"])],
        ["Low-voltage cutoff", bms["Info/BatteryLowVoltage"] == null ? "—" : `${(+bms["Info/BatteryLowVoltage"]).toFixed(1)} V`],
        ["Blocking", `${bms["System/NrOfModulesBlockingCharge"] ? "charge " : ""}${bms["System/NrOfModulesBlockingDischarge"] ? "discharge" : ""}` || "none"],
      ]) + `<div class="battm-alarms">${alarms.length ? alarms.map(k => `<span class="src warn-src">${esc(k)}</span>`).join("") : `<span class="muted">${icon("check")} No BMS alarms</span>`}</div>
      ${short ? `<p class="battm-note">The BMS reports ${on} module(s) but ${exp} batteries are installed.${(S["batteries.ble.reporting"] || 0) >= exp ? " Bluetooth reads all of them directly (below), so it's only their CAN comms link:" : ""} Check the battery comms daisy-chain and DIP switches (and that each battery's breaker is closed).</p>` : ""}
      </section>`;
  }
  const ble = S["batteries.ble"] || [];
  if (ble.length) {
    const tile = b => `<div class="battm-cell ${b.ok ? "" : "na"} ${b.problem_code || b.charge_fet === false || b.discharge_fet === false ? "bad" : ""}">
        <div class="battm-cell-h"><b>${esc(b.name)}</b><span>${b.ok ? (b.soc == null ? "—" : b.soc + "%") : "no data"}</span></div>
        ${b.ok ? `<div class="battm-cell-kv"><span>${(+b.voltage).toFixed(2)} V</span><span>${b.current == null ? "—" : (+b.current).toFixed(1) + " A"}</span>
          <span>${b.remaining_ah == null ? "—" : Math.round(b.remaining_ah) + " Ah"}</span><span>${b.cycles ?? "—"} cycles</span>
          <span class="${(b.cell_delta_mv || 0) > 50 ? "warn" : ""}">Δ ${b.cell_delta_mv ?? "—"} mV</span><span>${b.temps && b.temps.length ? fmtT(Math.max(...b.temps)) + " max" : "—"}</span></div>
          <div class="battm-cells">${(b.cells || []).map(c => `<i title="${c.toFixed(3)} V">${c.toFixed(3)}</i>`).join("")}</div>`
        : `<div class="muted small">${esc(b.error || "waiting for first read")}</div>`}</div>`;
    html += `<section class="battm-card battm-wide"><h4>${icon("battery")}Individual batteries<small>${S["batteries.ble.reporting"] ?? 0} of ${ble.length} via Bluetooth · total ${S["batteries.ble.current"] == null ? "—" : (+S["batteries.ble.current"]).toFixed(1) + " A"}</small></h4>
      <div class="battm-cellgrid">${ble.map(tile).join("")}</div></section>`;
  }
  $("#battm-body").innerHTML = html || `<div class="muted">No battery data from the Cerbo.</div>`;
  $("#battm-sub").textContent = S["victron.connected"] ? "Live from the Cerbo GX" : "Cerbo GX not connected";
}
$("#battm-close").addEventListener("click", closeBattModal);
$("#batt-modal").addEventListener("click", e => { if (e.target.id === "batt-modal") closeBattModal(); });

// ── Inverter overlay (tap the inverter glyph on Home) ─────────────────────────
let invmOpen = false, invmDragging = false, invmLimTimer = null;
const INVM_MODES = [[3, "On", "Inverts to the AC outlets and charges from shore"], [1, "Charger only", "Charges from shore; outlets not powered from the battery"],
  [2, "Inverter only", "Outlets powered from the battery; shore ignored"], [4, "Off", "Outlets dead, no charging"]];
function openInvModal() {
  if (S["victron.vebus.instance"] == null) { toast("No MultiPlus reported by the Cerbo", "bad"); return; }
  invmOpen = true; $("#inv-modal").hidden = false; renderInvModal(true);
}
function closeInvModal() { invmOpen = false; $("#inv-modal").hidden = true; }
function renderInvModal(force = false) {
  if (!invmOpen) return;
  const V = k => S[`victron.vebus.${k}`];
  const ok = canControlDev("victron") && !ctlPending;
  const mode = V("mode");
  $("#invm-title").textContent = V("product") || V("name") || "MultiPlus";
  const why = !CAN_CTL ? " · view only" : !S["victron.controls_ok"] ? " · control off" : ctlPending ? " · sending…" : "";
  $("#invm-state").textContent = `${VEBUS_STATE[V("state")] ?? "—"}${V("ac_out_w") != null ? ` · ${fmtW(V("ac_out_w"))} AC out` : ""}${why}`;
  $("#invm-modes").innerHTML = INVM_MODES.map(([m, l, d]) =>
    `<button class="invm-mode ${mode === m ? "on" : ""}" data-m="${m}" ${ok && V("mode_adjustable") && mode !== m ? "" : "disabled"}>
       <b>${l}</b><span>${d}</span></button>`).join("");
  const adj = V("limit_adjustable") === true;
  const sl = $("#invm-lim");
  sl.disabled = !(ok && adj);
  if (!invmDragging && (force || limPending == null)) sl.value = Math.round(limPending ?? V("current_limit") ?? 30);
  $("#invm-lim-val").textContent = (limPending ?? V("current_limit")) == null ? "—" : `${Math.round(limPending ?? V("current_limit"))} A`;
  $("#invm-lim-note").textContent = adj ? "Match the shore pedestal / cord rating (e.g. 15 A or 30 A)." :
    "Adjustable while the MultiPlus is on (the Cerbo reports it not adjustable right now).";
}
$("#flow").addEventListener("click", e => { if (e.target.closest("#inv")) openInvModal(); else if (e.target.closest("#hub")) openBattModal(); });
$("#invm-close").addEventListener("click", closeInvModal);
$("#inv-modal").addEventListener("click", e => { if (e.target.id === "inv-modal") closeInvModal(); });
$("#invm-modes").addEventListener("click", e => {
  const b = e.target.closest("[data-m]"); if (!b || b.disabled) return;
  const m = +b.dataset.m, label = INVM_MODES.find(x => x[0] === m)[1];
  control("inverter_mode", m, `MultiPlus ${label}`, "victron");
});
const invmSlider = $("#invm-lim");
invmSlider.addEventListener("pointerdown", () => { invmDragging = true; });
invmSlider.addEventListener("input", () => { limPending = +invmSlider.value; $("#invm-lim-val").textContent = `${limPending} A`; });
invmSlider.addEventListener("change", () => {
  invmDragging = false;
  limPending = +invmSlider.value;
  clearTimeout(invmLimTimer);
  invmLimTimer = setTimeout(() => control("shore_limit", limPending, `Shore limit ${limPending} A`, "victron"), 600);
});

// ── Switches ─────────────────────────────────────────────────────────────────
$("#sw-note").innerHTML = `${icon("alert")}<span id="sw-note-t"></span>`;
function renderSwitches() {
  const list = S["switches.list"] || [], live = canControlDev("n2k");
  $("#sw-note-t").textContent = S["switches.source"] !== "n2k" ? "No switch banks decoded yet."
    : live ? `Live: tap to switch. Keypad logic ${S["switching.automation"] ? "runs in AetherRV" : "is off"}.`
    : "Live from NMEA 2000. Switch control is off (n2k tx_enabled is not set), so tapping does nothing.";
  const label = s => s.state === "error" ? "Fault" : s.state === "n/a" ? "Not reporting" : s.state === "stale" ? "No data"
    : (S["switching.triggers"] || []).some(g => g.channel === s.channel) ? (s.on ? "Unlocked · timer" : "Locked · tap to open")
    : (s.on ? (s.kind === "dimmer" && s.level != null && s.level < 100 ? `On · ${s.level}%` : "On") : "Off") + (s.blocked ? " · locked" : "")
      + (!s.on && s.requires_input != null && !((S["switching.inputs"] || []).find(i => i.input === s.requires_input) || {}).on
         ? ` · needs ${((S["switching.inputs"] || []).find(i => i.input === s.requires_input) || {}).name || "input"}` : "");
  const tile = s => `<div class="sw-cell"><button class="sw ${s.on ? "on" : ""} ${s.state === "error" ? "fault" : ""} ${s.state === "n/a" || s.state === "stale" ? "na" : ""} ${s.blocked ? "blocked" : ""}" data-sw="${esc(s.id)}">
      ${s.state === "error" ? icon("alert") : icon(s.icon || "toggle")}
      <div><div class="name">${esc(s.name)}</div><div class="st"><i></i>${label(s)}${s.amps ? `<span class="sw-amps">${(+s.amps).toFixed(1)} A${s.rating_a ? ` / ${s.rating_a}` : ""}</span>` : ""}<span class="sw-id">ch ${s.channel}</span></div></div></button>
      ${s.kind === "dimmer" && live ? `<input class="sw-dim" type="range" min="1" max="100" value="${s.level ?? 100}" data-dim="${s.channel}" aria-label="${esc(s.name)} brightness">` : ""}</div>`;
  const inputs = S["switching.inputs"] || [];
  const groups = [...new Set(list.map(s => s.group || "Switches"))];
  $("#switches").innerHTML = (inputs.length ? `<div class="sw-group"><h3>Vehicle inputs</h3><div class="sw-inputs">${inputs.map(i =>
      `<span class="sw-in ${i.on ? "on" : ""}">${icon(i.icon || "toggle")}<b>${esc(i.name)}</b><span>${i.on == null ? "—" : i.on ? "On" : "Off"}${i.volts != null ? ` · ${(+i.volts).toFixed(1)} V` : ""}</span></span>`).join("")}</div></div>` : "") +
    (list.length ? groups.map(g => {
      const items = list.filter(s => (s.group || "Switches") === g);
      return `<div class="sw-group"><h3>${esc(g)} <span class="muted">· ${items.filter(s => s.on).length} on</span></h3>
        <div class="switch-grid">${items.map(tile).join("")}</div></div>`;
    }).join("") :
    `<div class="switch-grid"><div class="empty-state">${icon("toggle")}<div>No switch banks decoded yet.</div></div></div>`);
}
function toggleSwitch(id) {
  const s = (S["switches.list"] || []).find(x => x.id === id);
  if (!s) return;
  if (!canControlDev("n2k") || !s.channel) { toast("Switch control is off (view only)"); return; }
  if (s.state === "stale" || s.state === "n/a") { toast(`${s.name}: no live status`, "bad"); return; }
  const trig = (S["switching.triggers"] || []).find(g => g.channel === s.channel);
  if (trig) {                       // momentary: the load controller's own timer switches it off
    if (s.on) { toast(`${s.name}: active, the load controller switches it off`, "ok"); return; }
    control("trigger", { trigger: trig.id }, trig.name, "n2k"); return;
  }
  if (s.blocked && !s.on) { toast(`${s.name} is locked out (safety)`, "bad"); return; }
  if (!s.on && s.requires_input != null) {
    const inp = (S["switching.inputs"] || []).find(i => i.input === s.requires_input);
    if (!inp || inp.on !== true) { toast(`${s.name} needs ${inp ? inp.name : "input " + s.requires_input} on`, "bad"); return; }
  }
  if (s.state === "error") {   // tripped: OFF clears it
    askConfirm(`${s.name} tripped`, "The load controller reports a fault on this channel (it shut the output off). Switch it off to clear the fault? Check the load and wiring before switching it on again.",
      () => control("set", { channel: s.channel, on: false }, `${s.name} reset`, "n2k"));
    return;
  }
  control("set", { channel: s.channel, on: !s.on }, `${s.name} ${s.on ? "off" : "on"}`, "n2k");
}
$("#switches").addEventListener("click", e => { const b = e.target.closest(".sw"); if (b) toggleSwitch(b.dataset.sw); });
let dimTimer = null;
$("#switches").addEventListener("input", e => {
  const r = e.target.closest(".sw-dim"); if (!r) return;
  clearTimeout(dimTimer);
  dimTimer = setTimeout(() => control("dim", { channel: +r.dataset.dim, level: +r.value }, `Brightness ${r.value}%`, "n2k"), 400);
});

// ── Systems ──────────────────────────────────────────────────────────────────
const rateHist = {};
let inspect = store.get("vp.inspect", "n2k");
function busNames() { return [...new Set(Object.keys(S).filter(k => k.startsWith("bus.")).map(k => k.split(".")[1]))]; }
function spark(vals) {
  if (vals.length < 2) return "";
  const max = Math.max(1, ...vals), w = 300, h = 44;
  return `<svg viewBox="0 0 ${w} ${h}" preserveAspectRatio="none" class="spark"><path d="${vals.map((v, i) => `${i ? "L" : "M"}${(i / (vals.length - 1) * w).toFixed(1)},${(h - 3 - v / max * (h - 6)).toFixed(1)}`).join("")}"/></svg>`;
}
function renderSystems() {
  const card = (ic, title, cls, status, extra = "") => `<div class="card sys">${icon(ic)}<h3>${title}</h3>
    <div class="st ${cls}"><i></i>${status}</div>${extra}</div>`;
  let html = card("wifi", "Victron Cerbo GX", S["victron.connected"] ? "good" : "bad",
    S["victron.connected"] ? `connected · ${esc(S["victron.portal"] ?? "")}` : "not connected");
  for (const n of busNames()) {
    const g = k => S[`bus.${n}.${k}`];
    const cls = !g("connected") ? "bad" : g("rate") > 0 ? "good" : "idle";
    html += card("can", `${n} <span class="muted">· ${esc(g("channel"))}</span>`, cls,
      !g("connected") ? "down" : g("rate") > 0 ? `${g("rate")} frames/s` : "open · no traffic",
      `<div class="spark">${spark(rateHist[n] || [])}</div><div class="meta">${g("frames")} frames · ${g("ids")} IDs · ${g("errors")} errors</div>`);
  }
  for (const t of S["netmon.targets"] || []) {
    const cls = t.up === false ? "bad" : t.up ? "good" : "idle";
    const st = t.up === false ? `${t.reason} · ${t.down_for != null ? Math.max(1, Math.round(t.down_for / 60)) + " min" : ""}`
      : t.up ? `online · ${t.rtt_ms ?? "—"} ms${t.tcp ? ` · port ${t.tcp} ok` : ""}` : "checking…";
    html += card("router", `${esc(t.name)} <span class="muted">· ${esc(t.host)}</span>`, cls, st,
      t.up === false && t.hint ? `<div class="meta">${esc(t.hint)}</div>` : "");
  }
  const blocked = Object.values(S["tx.blocked"] || {}).reduce((a, b) => a + b, 0);
  const txOn = Object.entries(S["tx.enabled"] || {}).filter(([, v]) => v).map(([k]) => k);
  html += card("lock", "Transmit gate", txOn.length ? "good" : "idle",
    txOn.length ? `control enabled: ${esc(txOn.join(", "))} · ${blocked} blocked` : `all devices read-only · ${blocked} blocked`);
  $("#sys-cards").innerHTML = html;

  const devs = S["n2k.devices"] || [];
  const off = devs.filter(d => !d.online);
  $("#dev-sub").textContent = devs.length ? `${devs.length - off.length} online${off.length ? ` · ${off.length} offline` : ""} · heard passively (no requests sent)` : "";
  $("#dev-table").innerHTML = devs.length
    ? `<tr><th></th><th>Device</th><th>Maker</th><th>Class</th><th>Addr</th><th>Software</th><th>Last heard</th></tr>` +
      devs.map(d => `<tr class="${d.online ? "" : "offline"}">
        <td><span class="st-dot ${d.online ? "good" : "bad"}">${icon(d.online ? "check" : "alert")}${d.online ? "online" : "OFFLINE"}</span></td>
        <td><b>${esc(d.model || "unidentified")}</b>${d.serial ? `<small> · ${esc(d.serial)}</small>` : ""}</td>
        <td>${esc(d.manufacturer || "—")}</td><td>${esc(d.class || "—")}</td><td class="mono">${d.source}</td>
        <td class="mono">${esc(d.software || "—")}</td><td>${d.age < 5 ? "now" : d.age < 120 ? `${Math.round(d.age)} s ago` : `${Math.round(d.age / 60)} min ago`}</td></tr>`).join("")
    : `<tr><td class="empty">No NMEA 2000 devices heard yet.</td></tr>`;

  const tabs = [["n2k", "NMEA 2000", "n2k.pgns"], ["cmd", "N2K commands", "n2k.commands"], ["espar", "Espar", "espar.ids"], ["rixens", "Rixens", "rixens.ids"]];
  $("#inspect-sel").innerHTML = tabs.map(([id, l]) => `<button data-i="${id}" class="${id === inspect ? "on" : ""}">${l}</button>`).join("");
  const rows = S[(tabs.find(t => t[0] === inspect) || tabs[0])[2]] || [];
  const t = $("#inspect-table");
  if (!rows.length) { t.innerHTML = `<tr><td class="empty">No frames captured on this bus yet.</td></tr>`; return; }
  const cols = Object.keys(rows[0]);
  t.innerHTML = `<tr>${cols.map(c => `<th>${esc(c)}</th>`).join("")}</tr>` +
    rows.map(r => `<tr>${cols.map(c => `<td>${esc(Array.isArray(r[c]) ? r[c].join(",") : r[c])}</td>`).join("")}</tr>`).join("");
}
$("#inspect-sel").addEventListener("click", e => { const b = e.target.closest("button"); if (b) { inspect = b.dataset.i; store.set("vp.inspect", inspect); renderSystems(); } });
setInterval(() => { for (const n of busNames()) { (rateHist[n] ||= []).push(S[`bus.${n}.rate`] || 0); if (rateHist[n].length > 120) rateHist[n].shift(); } }, 1000);

// ── Alerts ───────────────────────────────────────────────────────────────────
const SEV = { warning: 1, serious: 2, critical: 3 };
const SEV_LABEL = { warning: "Warning", serious: "Serious", critical: "Critical" };
let soundOn = store.get("vp.sound", "1") === "1";
const ago = t => { const s = Date.now() / 1000 - t; return s < 60 ? "just now" : s < 3600 ? `${Math.round(s / 60)} min ago` : s < 86400 ? `${Math.round(s / 3600)} h ago` : `${Math.round(s / 86400)} d ago`; };
function sendWS(o) { if (ws && ws.readyState === 1) ws.send(JSON.stringify(o)); }

// Wake the (kiosk) screen when a critical alert fires: a critical id that wasn't critical+unacked on the last
// update (new, re-raised, or escalated). The kiosk block below sets kioskWake; elsewhere it stays null.
let kioskWake = null, critSeen = new Set();
const unackedCriticals = () => (S["alerts.active"] || []).filter(a => !a.acked && a.severity === "critical");
function renderAlertChrome() {
  const act = S["alerts.active"] || [];
  const unacked = act.filter(a => !a.acked);
  const crit = new Set(unackedCriticals().map(a => a.id));
  if ([...crit].some(id => !critSeen.has(id)) && kioskWake) kioskWake();
  critSeen = crit;
  const top = unacked.reduce((m, a) => (SEV[a.severity] > SEV[m] ? a.severity : m), "warning");
  const chip = $("#chip-alert");
  chip.hidden = !act.length;
  chip.className = `chip alert ${unacked.length ? "sev-" + top : "sev-acked"}`;
  if (act.length) chip.innerHTML = `${icon("alert")}${unacked.length === 1 ? esc(unacked[0].title) : unacked.length ? `${unacked.length} alerts` : `${act.length} acknowledged`}`;
  const badge = $("#nav-badge");
  badge.hidden = !unacked.length;
  badge.textContent = unacked.length;
  badge.className = `nav-badge sev-${top}`;
  document.body.classList.toggle("flash-critical", unacked.some(a => a.severity === "critical"));
  chime(unacked);
}

// Two-tone chime (WebAudio, no asset). New serious/critical always; warnings only in day mode.
// Unacknowledged criticals repeat every 60 s.
let audio, heard = new Set(), lastRepeat = 0;
function tone(f, t0, d, ctx) { const o = ctx.createOscillator(), g = ctx.createGain(); o.frequency.value = f; o.type = "sine";
  g.gain.setValueAtTime(0.0001, t0); g.gain.exponentialRampToValueAtTime(0.25, t0 + 0.02); g.gain.exponentialRampToValueAtTime(0.0001, t0 + d);
  o.connect(g).connect(ctx.destination); o.start(t0); o.stop(t0 + d + 0.05); }
function playChime(sev) {
  try { audio ||= new AudioContext(); const t = audio.currentTime + 0.05;
    if (sev === "critical") { tone(880, t, .25, audio); tone(660, t + .3, .25, audio); tone(880, t + .6, .4, audio); }
    else { tone(660, t, .25, audio); tone(880, t + .28, .35, audio); } } catch { /* audio unavailable */ }
}
function chime(unacked) {
  const night = document.documentElement.classList.contains("night");
  const fresh = unacked.filter(a => !heard.has(a.id + a.severity));
  unacked.forEach(a => heard.add(a.id + a.severity));
  if (!soundOn) return;
  const loud = fresh.filter(a => a.severity !== "warning" || !night);
  if (loud.length) { playChime(loud.some(a => a.severity === "critical") ? "critical" : "serious"); lastRepeat = Date.now(); return; }
  if (unacked.some(a => a.severity === "critical") && Date.now() - lastRepeat > 60000) { playChime("critical"); lastRepeat = Date.now(); }
}

const SRC_ICON = { victron: "battery", ruuvi: "fridge", n2k: "can", can: "can", rixens: "flame" };
function renderAlerts() {
  const act = S["alerts.active"] || [];
  $("#alerts-summary").textContent = act.length ? `${act.length} active · ${act.filter(a => !a.acked).length} unacknowledged` : "";
  $("#btn-sound").innerHTML = `${icon(soundOn ? "bell" : "bellOff")}${soundOn ? "Sound on" : "Sound off"}`;
  $("#btn-ackall").disabled = !act.some(a => !a.acked);
  $("#alerts-list").innerHTML = act.length ? act.map(a => `
    <div class="alert-card sev-${a.severity} ${a.acked ? "acked" : ""}">
      <div class="alert-ic">${icon(a.severity === "warning" ? "alert" : SRC_ICON[a.source] || "alert")}</div>
      <div class="alert-body"><div class="alert-sev">${icon("alert")}${SEV_LABEL[a.severity]}${a.acked ? " · acknowledged" : ""}</div>
        <h3>${esc(a.title)}</h3><p>${esc(a.detail)}</p><span class="muted">since ${ago(a.since)}</span></div>
      <div class="alert-btns">${/^cold\.(fridge|freezer)$/.test(a.id) && CAN_CTL ? `<button class="btn-soft" data-cold-off="${a.id.slice(5)}">${icon("toggle")}${a.id === "cold.fridge" ? "Fridge" : "Freezer"} is off</button>` : ""}
      ${/^switch\.trip\.\d+\.\d+$/.test(a.id) && canControlDev("n2k") ? `<button class="btn-soft" data-trip="${a.id.split(".").pop()}">${icon("toggle")}Reset breaker</button>` : ""}
      ${/^cerbo\.notif\.\d+$/.test(a.id) && canControlDev("victron") && !/^Acknowledged/.test(a.detail) ? `<button class="btn-soft" data-cerbo-ack="1">${icon("bell")}Acknowledge on Cerbo</button>` : ""}
      ${a.acked ? "" : `<button class="btn-soft" data-ack="${esc(a.id)}">Acknowledge</button>`}</div>
    </div>`).join("") :
    `<div class="all-clear">${icon("check")}<div><h3>All clear</h3><span class="muted">No active alerts.</span></div></div>`;
  const log = S["alerts.log"] || [];
  $("#alerts-log").innerHTML = log.length ? `<tr><th>When</th><th>Event</th><th>Severity</th><th>Alert</th></tr>` + log.map(e =>
    `<tr><td>${new Date(e.t * 1000).toLocaleString([], { weekday: "short", hour: "numeric", minute: "2-digit" })}</td><td>${esc(e.event)}</td>
     <td><span class="sev-t sev-${e.severity}">${esc(SEV_LABEL[e.severity] || e.severity)}</span></td><td>${esc(e.title)}</td></tr>`).join("")
    : `<tr><td class="empty">No alert history yet.</td></tr>`;
}
$("#alerts-list").addEventListener("click", e => {
  const b = e.target.closest("[data-ack]"); if (b) sendWS({ type: "ack", id: b.dataset.ack });
  const tr = e.target.closest("[data-trip]");
  if (tr) { const ch = +tr.dataset.trip, s = (S["switches.list"] || []).find(x => x.channel === ch);
    askConfirm(`Reset ${s ? s.name : "channel " + ch}?`, "This switches the channel off, which clears the tripped breaker in the load controller. It stays off: check the load and its current limit before switching it on again.",
      () => control("set", { channel: ch, on: false }, `${s ? s.name : "Channel " + ch} reset`, "n2k")); return; }
  if (e.target.closest("[data-cerbo-ack]")) { control("ack_alarms", 1, "Cerbo alarm acknowledged", "victron"); return; }
  const o = e.target.closest("[data-cold-off]");
  if (o) { const r = o.dataset.coldOff, L = r === "fridge" ? "Fridge" : "Freezer";
    askConfirm(`${L} switched off?`, `Warm alerts for the ${r} stop now and turn back on by themselves once it's cold again (below ${fmtT(S[`ui.${r}_max_c`])}).`,
      () => settingsAction("pref", { key: `${r}_off`, value: true }, `${L} off`)); }
});
$("#btn-ackall").addEventListener("click", () => sendWS({ type: "ack_all" }));
$("#btn-sound").addEventListener("click", () => { soundOn = !soundOn; store.set("vp.sound", soundOn ? "1" : "0"); if (soundOn) playChime("serious"); renderAlerts(); });

// ── Network (Starlink + Peplink) ──────────────────────────────────────────────
const rsrpQ = v => v == null ? null : v >= -80 ? "excellent" : v >= -90 ? "good" : v >= -100 ? "fair" : "poor";
const sinrQ = v => v == null ? null : v >= 20 ? "excellent" : v >= 13 ? "good" : v >= 0 ? "fair" : "poor";
const mbps = v => v == null ? "—" : v >= 100 ? Math.round(v) : (+v).toFixed(1);
const dur = s => s == null ? "—" : s >= 86400 ? `${Math.floor(s / 86400)} d ${Math.floor(s % 86400 / 3600)} h` : `${Math.floor(s / 3600)} h ${Math.floor(s % 3600 / 60)} m`;
function bars(level, max = 5) {
  return `<span class="bars">${Array.from({ length: max }, (_, i) => `<i class="${i < level ? "on" : ""}" style="height:${30 + i * 17}%"></i>`).join("")}</span>`;
}
function renderNetChip() {
  const chip = $("#chip-net");
  if (S["peplink.reachable"] == null && S["starlink.reachable"] == null) { chip.hidden = true; return; }
  chip.hidden = false;
  const act = S["peplink.active_type"], wans = S["peplink.wans"] || [];
  const cell = wans.find(w => w.cellular && w.connected)?.cellular;
  if (act === "starlink" || (!act && S["starlink.online"])) chip.innerHTML = `${icon("dish")}Starlink · ${mbps(S["starlink.down_mbps"])} Mbps`;
  else if (act === "cellular" && cell) chip.innerHTML = `${icon("signal")}${esc(cell.network || "")} · ${esc(cell.carrier || "")}`;
  else if (S["peplink.active"]) chip.innerHTML = `${icon("router")}${esc(S["peplink.active"])}`;
  else chip.innerHTML = `${icon("alert")}No internet`;
}
// Sunrise/sunset (NOAA simplified algorithm, local computation — no internet needed). Returns epoch ms or null.
function sunTimes(lat, lon, date = new Date()) {
  const rad = Math.PI / 180, day = Date.UTC(date.getFullYear(), date.getMonth(), date.getDate());
  const n = Math.round((day - Date.UTC(2000, 0, 1, 12)) / 86400000);
  const Jstar = n - lon / 360, M = (357.5291 + 0.98560028 * Jstar) % 360;
  const C = 1.9148 * Math.sin(M * rad) + 0.02 * Math.sin(2 * M * rad) + 0.0003 * Math.sin(3 * M * rad);
  const L = (M + C + 180 + 102.9372) % 360;
  const Jt = 2451545 + Jstar + 0.0053 * Math.sin(M * rad) - 0.0069 * Math.sin(2 * L * rad);
  const dec = Math.asin(Math.sin(L * rad) * Math.sin(23.44 * rad));
  const cosw = (Math.sin(-0.833 * rad) - Math.sin(lat * rad) * Math.sin(dec)) / (Math.cos(lat * rad) * Math.cos(dec));
  if (cosw < -1 || cosw > 1) return null;                     // polar day/night
  const w = Math.acos(cosw) / rad / 360, toMs = j => (j - 2440587.5) * 86400000;
  return { rise: toMs(Jt - w), set: toMs(Jt + w), noon: toMs(Jt) };
}

// ── Weather: Leaflet map (vendored) with an OpenStreetMap basemap + LibreWXR radar, centred on the van's GPS.
// Tapping the map pins a spot and loads that spot's 7-day forecast; Re-centre returns map + forecast to the van.
// The view only auto-recentres when the van moves > 15 km, so panning/zooming isn't reset by GPS updates.
let wxCenter = null, wxMap = null, wxVan = null, wxPin = null, fcPick = null;
let rvFrames = [], rvHost = "", rvLayers = [], rvIdx = -1, rvTimer = null, rvAt = 0;
const kmBetween = (a, b) => { const R = 6371, r = Math.PI / 180, dLat = (b[0] - a[0]) * r, dLon = (b[1] - a[1]) * r;
  const h = Math.sin(dLat / 2) ** 2 + Math.cos(a[0] * r) * Math.cos(b[0] * r) * Math.sin(dLon / 2) ** 2; return 2 * R * Math.asin(Math.sqrt(h)); };
const fmtLL = (lat, lon) => `${Math.abs(lat).toFixed(3)}° ${lat >= 0 ? "N" : "S"}, ${Math.abs(lon).toFixed(3)}° ${lon >= 0 ? "E" : "W"}`;
function initWxMap() {
  if (wxMap || !window.L) return;
  wxMap = L.map("wx-map", { zoomControl: true, attributionControl: true, zoomSnap: 0.5, tap: true }).setView([45, -110], 7);
  // OpenStreetMap standard tiles (no key; light use per the OSM tile policy), shown as-is (AetherMap's default look).
  L.tileLayer("https://tile.openstreetmap.org/{z}/{x}/{y}.png", { maxZoom: 12, className: "wx-base",
    attribution: "© OpenStreetMap contributors · Radar © LibreWXR" }).addTo(wxMap);
  wxMap.on("click", e => {
    fcPick = [e.latlng.lat, e.latlng.lng];
    const pinIcon = L.divIcon({ className: "wx-pin", html: "<i></i>", iconSize: [26, 26], iconAnchor: [13, 13] });
    if (wxPin) wxPin.setLatLng(e.latlng); else wxPin = L.marker(e.latlng, { icon: pinIcon, interactive: false }).addTo(wxMap);
    fc = null; fcAt = 0; fcErr = null;
    renderWeather();
  });
  loadRadar();
}
async function loadRadar() {
  if (Date.now() - rvAt < 10 * 60e3 && rvFrames.length) return;
  rvAt = Date.now();
  try {
    // LibreWXR: RainViewer-compatible API (same JSON + tile paths), crisper and deeper zoom — as used by AetherMap.
    const j = await (await fetch("https://api.librewxr.net/public/weather-maps.json")).json();
    rvHost = j.host; rvFrames = j.radar.past || [];
    rvLayers.forEach(l => wxMap.removeLayer(l));
    // 512 px tiles (map zoom N → tile z N-1), colour scheme 6 (NWS-style), no smoothing — AetherMap's settings.
    rvLayers = rvFrames.map(f => L.tileLayer(`${rvHost}${f.path}/512/{z}/{x}/{y}/6/0_0.png`,
      { opacity: 0, tileSize: 512, zoomOffset: -1, maxNativeZoom: 11, maxZoom: 12, zIndex: 5, className: "wx-radar" }).addTo(wxMap));
    showFrame(rvFrames.length - 1);
  } catch (e) { $("#wx-time").textContent = "radar unavailable"; }
}
function showFrame(i) {
  if (!rvLayers.length) return;
  if (rvIdx >= 0 && rvLayers[rvIdx]) rvLayers[rvIdx].setOpacity(0);
  rvIdx = i; rvLayers[i].setOpacity(0.85);
  const t = new Date(rvFrames[i].time * 1000), ago = Math.round((Date.now() - t) / 60e3);
  $("#wx-time").textContent = `${t.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })} · ${ago} min ago`;
}
$("#wx-play").addEventListener("click", () => {
  if (rvTimer) { clearInterval(rvTimer); rvTimer = null; $("#wx-play").textContent = "Play"; showFrame(rvLayers.length - 1); return; }
  $("#wx-play").textContent = "Pause";
  rvTimer = setInterval(() => showFrame((rvIdx + 1) % rvLayers.length), 700);
});
// Sun + position strip at the top of the Weather card (moved from the Network page's Location card).
function renderWxLoc() {
  // One line in the card header: sunrise ▬ sunset, with the daylight length floating above the lit span.
  const lat = S["location.lat"], lon = S["location.lon"];
  const el = $("#wx-loc");
  el.title = [S["location.hdop"] != null ? "HDOP " + S["location.hdop"] : null,
    S["location.source"] ? S["location.source"] + " GPS" : null].filter(Boolean).join(" · ");
  const st = lat != null ? sunTimes(+lat, +lon) : null;
  if (!st) { el.innerHTML = ""; return; }
  const hm = t => new Date(t).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  const d0 = new Date(); d0.setHours(0, 0, 0, 0);
  const pct = t => Math.max(0, Math.min(100, (t - d0) / 864000));
  const len = Math.round((st.set - st.rise) / 60000), a = pct(st.rise), b = pct(st.set);
  el.innerHTML = `<b>${hm(st.rise)}<small>sunrise</small></b>
    <div class="daybar"><i style="left:${a}%;width:${b - a}%"></i><b style="left:${pct(Date.now())}%"></b>
      <em style="left:${(a + b) / 2}%">${Math.floor(len / 60)} h ${len % 60} m daylight</em></div>
    <b>${hm(st.set)}<small>sunset</small></b>`;
}

function renderWeather() {
  renderWxLoc();
  const lat = S["location.lat"], lon = S["location.lon"], fix = S["location.fix"];
  const posx = [S["location.alt_m"] != null ? Math.round(S["location.alt_m"] * 3.281).toLocaleString() + " ft" : null,
    lat == null ? null : S["location.speed"] ? "moving" : "parked"].filter(Boolean).join(" · ");
  $("#wx-sub").innerHTML = lat == null ? "No GPS position yet"
    : `<span>Van ${fmtLL(lat, lon)}${fix ? "" : " · last known"}</span>${posx ? `<small>${posx}</small>` : ""}`;
  $("#wx-empty").hidden = lat != null;
  initWxMap();
  if (wxMap) {
    wxMap.invalidateSize();
    loadRadar();
    if (lat != null) {
      const vanIcon = L.divIcon({ className: "wx-van", html: "<i></i>", iconSize: [22, 22], iconAnchor: [11, 11] });
      if (wxVan) wxVan.setLatLng([lat, lon]); else wxVan = L.marker([lat, lon], { icon: vanIcon, interactive: false, zIndexOffset: 500 }).addTo(wxMap);
      if (!wxCenter || kmBetween(wxCenter, [lat, lon]) > 15) { wxCenter = [lat, lon]; wxMap.setView([lat, lon], 7); }
    }
  }
  const tgt = fcPick || (lat != null ? [lat, lon] : null);
  $("#wx-hint").hidden = !!fcPick;
  if (tgt) loadForecast(tgt[0], tgt[1]);
  renderForecast();
}
// Dark map (AetherMap-style luminance ramp, basemap only). Separate remembered choices for day and night
// mode: day defaults to off, night defaults to ON, so switching to night mode darkens the map automatically.
const isNight = () => document.documentElement.classList.contains("night");
const wxDarkKey = () => isNight() ? "vp.wxdark.night" : "vp.wxdark";
function applyWxDark() {
  const on = store.get(wxDarkKey(), isNight() ? "1" : "0") === "1";
  $(".wx-wrap").classList.toggle("dark-map", on); $("#wx-dark").classList.toggle("on", on);
}
$("#wx-dark").addEventListener("click", () => {
  store.set(wxDarkKey(), $(".wx-wrap").classList.contains("dark-map") ? "0" : "1"); applyWxDark();
});
$("#wx-recenter").addEventListener("click", () => {
  wxCenter = null; fcPick = null; fc = null; fcAt = 0; fcErr = null;
  if (wxPin) { wxMap.removeLayer(wxPin); wxPin = null; }
  renderWeather();
});

// 7-day forecast from Open-Meteo (free, no key): refetched every 30 min or after a > 15 km move.
let fc = null, fcAt = 0, fcAtPos = null, fcBusy = false, fcErr = null;
async function loadForecast(lat, lon) {
  if (fcBusy || (fc && Date.now() - fcAt < 30 * 60e3 && fcAtPos && kmBetween(fcAtPos, [lat, lon]) <= 15)) return;
  if (fcErr && Date.now() - fcAt < 2 * 60e3) return;                      // back off after a failure
  fcBusy = true;
  const q = new URLSearchParams({ latitude: lat.toFixed(3), longitude: lon.toFixed(3), forecast_days: "7", timezone: "auto",
    daily: "weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max,wind_speed_10m_max",
    temperature_unit: UNIT === "F" ? "fahrenheit" : "celsius", wind_speed_unit: "mph" });
  try {
    const r = await fetch(`https://api.open-meteo.com/v1/forecast?${q}`);
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    fc = (await r.json()).daily; fcErr = null; fcAtPos = [lat, lon];
  } catch (e) { fcErr = String(e.message || e); }
  fcAt = Date.now(); fcBusy = false;
  if (view === "weather") renderForecast();
}
const WMO = c => c === 0 ? ["sun", "Clear"] : c <= 2 ? ["wx_partly", "Partly cloudy"] : c === 3 ? ["wx_cloud", "Overcast"]
  : c <= 48 ? ["wx_fog", "Fog"] : c <= 57 ? ["wx_rain", "Drizzle"] : c <= 67 ? ["wx_rain", "Rain"] : c <= 77 ? ["wx_snow", "Snow"]
  : c <= 82 ? ["wx_rain", "Showers"] : c <= 86 ? ["wx_snow", "Snow showers"] : ["wx_storm", "Thunderstorm"];
function renderForecast() {
  const el = $("#wx-fc");
  if (!fc) { el.innerHTML = `<div class="wx-fc-msg">${fcErr ? `Forecast unavailable<br><small>${esc(fcErr)}</small>` : "Loading forecast…"}</div>`; return; }
  el.innerHTML = fc.time.map((d, i) => {
    const dt = new Date(`${d}T12:00`), [ic, label] = WMO(fc.weather_code[i]);
    const day = i === 0 ? "Today" : dt.toLocaleDateString([], { weekday: "short" });
    const pp = fc.precipitation_probability_max[i];
    return `<div class="fc-row" title="${esc(label)}">
      <div class="fc-day"><b>${day}</b><small>${esc(label)}</small></div>
      <div class="fc-ic">${icon(ic)}</div>
      <div class="fc-t"><b>${Math.round(fc.temperature_2m_max[i])}°</b><span>${Math.round(fc.temperature_2m_min[i])}°</span></div>
      <div class="fc-x">${pp != null && pp >= 10 ? `<span class="fc-pp">${icon("drop")}${pp}%</span>` : ""}<span>${Math.round(fc.wind_speed_10m_max[i])} mph</span></div>
    </div>`;
  }).join("");
}

// ── On-screen keyboard (the kiosk has no keyboard; Maestro ships its own for the same reason) ─────────────
// openKeyboard({title, value, password, maxlen, pattern}) → Promise<string|null>. The text never leaves the page
// except through the caller (e.g. straight to the admin helper for a Wi-Fi passphrase — never into State/logs).
const OSK_ROWS = {
  abc: ["1234567890", "qwertyuiop", "asdfghjkl", "zxcvbnm"],
  sym: ["1234567890", "-_@#$%&*+=", "!?.,:;'\"()~", "/\\|<>[]{}^`"],
};
let osk = null;
function openKeyboard(opt) {
  return new Promise(resolve => {
    osk = { text: opt.value || "", shift: false, sym: false, show: !opt.password, opt, resolve };
    $("#osk-title").textContent = opt.title || "";
    $("#osk-show").hidden = !opt.password;
    $("#osk").hidden = false;
    renderOsk();
  });
}
function closeOsk(val) { const r = osk?.resolve; osk = null; $("#osk").hidden = true; r && r(val); }
function renderOsk() {
  if (!osk) return;
  const t = osk.text, ok = !osk.opt.pattern || new RegExp(`^(?:${osk.opt.pattern})$`).test(t);
  $("#osk-text").textContent = osk.show ? t : "•".repeat(t.length);
  $("#osk-show").textContent = osk.show ? "Hide" : "Show";
  const rows = OSK_ROWS[osk.sym ? "sym" : "abc"].map(r => [...r].map(ch => {
    const c = !osk.sym && osk.shift ? ch.toUpperCase() : ch;
    return `<button class="osk-k" data-k="${esc(c)}">${esc(c)}</button>`;
  }).join(""));
  rows[3] = `<button class="osk-k wide${osk.shift ? " on" : ""}" data-a="shift">⇧</button>` + rows[3] + `<button class="osk-k wide" data-a="bs">⌫</button>`;
  rows.push(`<button class="osk-k wide" data-a="sym">${osk.sym ? "abc" : "123#"}</button><button class="osk-k space" data-k=" ">space</button>` +
    `<button class="osk-k wide done" data-a="done" ${t.length && ok ? "" : "disabled"}>Done</button>`);
  $("#osk-keys").innerHTML = rows.map(r => `<div class="osk-row">${r}</div>`).join("");
}
$("#osk-keys").addEventListener("pointerdown", e => {
  const b = e.target.closest(".osk-k"); if (!b || b.disabled || !osk) return;
  e.preventDefault();
  const a = b.dataset.a, max = osk.opt.maxlen || 63;
  if (b.dataset.k != null) { if (osk.text.length < max) osk.text += b.dataset.k; if (osk.shift) osk.shift = false; }
  else if (a === "bs") osk.text = osk.text.slice(0, -1);
  else if (a === "shift") osk.shift = !osk.shift;
  else if (a === "sym") osk.sym = !osk.sym;
  else if (a === "done") return closeOsk(osk.text);
  renderOsk();
});
$("#osk-cancel").addEventListener("click", () => closeOsk(null));
$("#osk-show").addEventListener("click", () => { if (osk) { osk.show = !osk.show; renderOsk(); } });

// ── Settings (Maestro-style). Reads come from /api/settings; every change goes to /api/settings/action,
// which the backend accepts only from the van's own touchscreen and forwards to the root admin helper.
let setTab = "device", setInfo = null, setAt = 0, setBusy = false;
let tsLoginUrl = null, setPollT = null;
// One poll timer for the Settings view (renderSettings runs on every state delta; never stack timers).
const setPoll = ms => { clearTimeout(setPollT); setPollT = setTimeout(() => { if (view === "settings") loadSettings(true); }, ms); };
const SET_TABS = [["network", "Network", "wifi"], ["tailscale", "Tailscale", "ts"], ["updates", "Updates", "download"], ["device", "Device", "info"]];
async function loadSettings(force) {
  if (setBusy || (!force && Date.now() - setAt < 10e3)) return;
  setBusy = true;
  try { setInfo = await (await fetch("/api/settings")).json(); } catch (e) { setInfo = { error: String(e) }; }
  setAt = Date.now(); setBusy = false;
  if (view === "settings") renderSettings();
}
async function settingsAction(verb, args = {}, label = verb) {
  if (!CAN_CTL) { toast("View only — control from the touchscreen or your Tailscale devices", "bad"); return null; }
  toast(`${label}…`, "busy");
  try {
    const r = await (await fetch("/api/settings/action", { method: "POST", headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ verb, args }) })).json();
    toast(r.ok ? `${label}: done` : `${label} failed: ${r.error}`, r.ok ? "ok" : "bad");
    loadSettings(true);
    return r;
  } catch (e) { toast(`${label} failed: ${e}`, "bad"); return null; }
}
const fmtBytes = b => b == null ? "—" : b > 1e9 ? `${(b / 1e9).toFixed(1)} GB` : `${Math.round(b / 1e6)} MB`;
const fmtUp = s => s == null ? "—" : s > 86400 ? `${Math.floor(s / 86400)} d ${Math.floor(s % 86400 / 3600)} h` : `${Math.floor(s / 3600)} h ${Math.floor(s % 3600 / 60)} min`;
let setSig = "";
function renderSettings(force = false) {
  loadSettings(false);
  // renderSettings is also called on every live-state delta; only rebuild when something shown changed.
  const sig = JSON.stringify([setTab, setInfo, S["settings.timezone"], S["settings.screen_off_min"], UNIT, isNight(), tsLoginUrl]);
  if (!force && sig === setSig) return;
  setSig = sig;
  $("#set-tabs").innerHTML = SET_TABS.map(([id, label, ic]) =>
    `<button class="set-tab${setTab === id ? " on" : ""}" data-t="${id}">${icon(ic)}<span>${label}</span></button>`).join("") +
    (CAN_CTL ? "" : `<div class="set-ro">${icon("lock")}View only — change settings from the touchscreen or your Tailscale devices</div>`);
  const a = setInfo?.admin, p = setInfo?.prefs || {};
  const body = $("#set-body");
  if (!setInfo) { body.innerHTML = `<div class="muted">Loading…</div>`; return; }
  if (setTab === "device") {
    const ifs = (a?.interfaces || []).filter(i => i.ipv4?.length).map(i => `${esc(i.name)} ${esc(i.ipv4.join(", "))}`).join(" · ") || "—";
    const off = p.screen_off_min ?? 10;
    const offOpts = [[5, "5 min"], [10, "10 min"], [15, "15 min"], [30, "30 min"], [60, "1 h"], [0, "Never"]];
    body.innerHTML = `
      <div class="set-h"><h2>Device &amp; display</h2>${a && a.ok === false ? `<span class="pill bad-pill">${esc(a.error || "helper unavailable")}</span>` : ""}</div>
      <div class="set-rows">
        <div class="set-row"><div><b>Name</b><small>Shown on the network and in Tailscale</small></div>
          <div class="set-v"><span>${esc(a?.hostname || "—")}</span><button class="ctl-pill" data-act="rename" ${CAN_CTL ? "" : "disabled"}>Rename</button></div></div>
        <div class="set-row"><div><b>Time zone</b><small>Set automatically from the van's GPS position</small></div>
          <div class="set-v"><span>${esc((S["settings.timezone"] || a?.timezone || "—").replace(/_/g, " "))}</span><span class="pill ok-pill">${icon("pin")}GPS</span></div></div>
        <div class="set-row"><div><b>Screen auto-off</b><small>After this long without a touch; any touch wakes it</small></div>
          <div class="seg">${offOpts.map(([v, l]) => `<button class="${off === v ? "on" : ""}" data-off="${v}" ${CAN_CTL ? "" : "disabled"}>${l}</button>`).join("")}</div></div>
        <div class="set-row"><div><b>Temperature</b><small>Units on every screen</small></div>
          <div class="seg"><button class="${UNIT === "F" ? "on" : ""}" data-unit="F">°F</button><button class="${UNIT === "C" ? "on" : ""}" data-unit="C">°C</button></div></div>
        <div class="set-row"><div><b>Night mode</b><small>Dim, warm colours; dark weather map</small></div>
          <div class="seg"><button class="${isNight() ? "" : "on"}" data-night="0">Day</button><button class="${isNight() ? "on" : ""}" data-night="1">Night</button></div></div>
      </div>
      <div class="set-h sub"><h2>About</h2></div>
      <div class="set-about">
        <div><small>AetherRV</small><b>${esc(setInfo.version || "—")}</b></div>
        <div><small>Kernel</small><b>${esc(a?.kernel || "—")}</b></div>
        <div><small>Up</small><b>${fmtUp(a?.uptime_s)}</b></div>
        <div><small>Disk free</small><b>${fmtBytes(a?.disk?.free)} of ${fmtBytes(a?.disk?.total)}</b></div>
        <div class="wide"><small>Addresses</small><b>${ifs}</b></div>
      </div>
      <div class="set-actions">
        <button class="ctl-pill" data-act="restart_kiosk" ${CAN_CTL ? "" : "disabled"}>Restart display</button>
        <button class="ctl-pill warn" data-act="reboot" ${CAN_CTL ? "" : "disabled"}>Reboot</button>
        <button class="ctl-pill warn" data-act="poweroff" ${CAN_CTL ? "" : "disabled"}>Shut down</button>
      </div>`;
  } else if (setTab === "network") {
    const w = setInfo.wifi || {}, ifs = a?.interfaces || [];
    const ip = n => (ifs.find(i => i.name === n)?.ipv4 || []).join(", ");
    const wired = ifs.filter(i => /^(end|eth|en)/.test(i.name));
    const bars = dbm => { const q = dbm >= -55 ? 4 : dbm >= -65 ? 3 : dbm >= -75 ? 2 : 1;
      return `<span class="wbars">${[1, 2, 3, 4].map(i => `<i class="${i <= q ? "on" : ""}" style="height:${i * 25}%"></i>`).join("")}</span>`; };
    const nets = (w.networks || []).filter(n => n.ssid);
    const savedOnly = (w.known || []).filter(k => !nets.some(n => n.ssid === k.ssid));
    const dis = CAN_CTL ? "" : "disabled";
    body.innerHTML = `
      <div class="set-h"><h2>Network</h2></div>
      <div class="set-rows">
        ${wired.map(i => `<div class="set-row"><div><b>Wired · ${esc(i.name)}</b><small>Ethernet (Peplink router / van LAN)</small></div>
          <div class="set-v"><span>${esc(ip(i.name) || "no address")}</span><span class="pill ${i.state === "UP" ? "ok-pill" : "bad-pill"}">${i.state === "UP" ? "Connected" : "Down"}</span></div></div>`).join("")}
        <div class="set-row"><div><b>Wi-Fi</b><small>${!w.present ? esc(w.error || "No Wi-Fi device") : !w.powered ? "Off"
          : w.connected ? `Connected to ${esc(w.connected)}${ip(w.device) ? " · " + esc(ip(w.device)) : ""}` : "Not connected"}</small></div>
          <div class="set-v">${w.present ? `<button class="ctl-pill" data-wscan ${w.powered ? dis : "disabled"}>${w.scanning ? "Scanning…" : "Scan"}</button>
            <div class="seg"><button class="${w.powered ? "on" : ""}" data-wpow="1" ${dis}>On</button><button class="${w.powered ? "" : "on"}" data-wpow="0" ${dis}>Off</button></div>` : ""}</div></div>
      </div>
      ${w.present && w.powered ? `<div class="wlist">${nets.length ? nets.map(n => `
        <div class="wnet${n.connected ? " conn" : ""}">${bars(n.signal_dbm)}
          <div class="wname"><b>${esc(n.ssid)}</b><small>${n.connected ? "Connected" : n.known ? "Saved" : n.security === "open" ? "Open" : n.security === "8021x" ? "Enterprise (not supported)" : "Secured"}</small></div>
          ${n.security !== "open" ? icon("lock") : ""}
          ${n.known ? `<button class="ctl-pill" data-wforget="${esc(n.ssid)}" ${dis}>Forget</button>` : ""}
          ${!n.connected && n.security !== "8021x" ? `<button class="ctl-pill" data-wjoin="${esc(n.ssid)}" data-sec="${esc(n.security)}" data-known="${n.known ? 1 : 0}" ${dis}>${n.known ? "Connect" : "Join"}</button>` : ""}
        </div>`).join("") : `<div class="muted">${w.scanning ? "Scanning…" : "No networks found. Tap Scan."}</div>`}
        ${savedOnly.map(k => `<div class="wnet out"><span class="wbars"></span><div class="wname"><b>${esc(k.ssid)}</b><small>Saved · not in range</small></div>
          <button class="ctl-pill" data-wforget="${esc(k.ssid)}" ${dis}>Forget</button></div>`).join("")}
      </div>` : ""}`;
    if (w.scanning) setPoll(2500);
  } else if (setTab === "tailscale") {
    const t = setInfo.tailscale || {}, dis = CAN_CTL ? "" : "disabled";
    const url = t.auth_url || tsLoginUrl;
    let inner;
    if (t.ok === false) inner = `<div class="muted">${esc(t.error || "Unavailable")}</div>`;
    else if (!t.installed) inner = `
      <p class="set-p">Tailscale gives you secure access to AetherRV from your phone or laptop anywhere,
      without opening ports on the Peplink. It isn't installed yet.</p>
      <button class="ctl-pill big" data-ts="install" ${dis}>Install Tailscale</button>`;
    else if (t.state === "Running") inner = `
      <div class="set-rows">
        <div class="set-row"><div><b>Connected</b><small>${esc(t.tailnet || "")}${t.user ? " · " + esc(t.user) : ""}</small></div>
          <div class="set-v"><span class="pill ${t.online ? "ok-pill" : "bad-pill"}">${t.online ? "Online" : "Offline"}</span></div></div>
        <div class="set-row"><div><b>Name</b><small>Reach AetherRV at http://&lt;name&gt;:8080 from your tailnet</small></div>
          <div class="set-v"><span>${esc(t.dns || t.name || "—")}</span></div></div>
        <div class="set-row"><div><b>Addresses</b><small>Tailscale IPs</small></div><div class="set-v"><span>${esc((t.ips || []).join("  ·  ") || "—")}</span></div></div>
      </div>
      <p class="set-p muted">Over Tailscale the screens are view-only, like any other remote browser; controls and settings stay on the touchscreen.</p>
      <div class="set-actions"><button class="ctl-pill warn" data-ts="logout" ${dis}>Sign out</button></div>`;
    else if (url) inner = `
      <div class="ts-login"><div class="ts-qr" id="ts-qr"></div>
        <div><b>Scan to sign in</b><p class="set-p">Open the camera on your phone, scan the code, and sign in to Tailscale.
          The console joins your tailnet as soon as you approve it.</p><code class="ts-url">${esc(url)}</code>
          <p class="muted">Waiting for sign-in…</p></div></div>`;
    else inner = `
      <p class="set-p">Tailscale is installed${t.running ? "" : " but its service isn't running"}. Sign in to add this console to your tailnet.</p>
      <button class="ctl-pill big" data-ts="${t.running ? "login" : "install"}" ${dis}>${t.running ? "Sign in" : "Start Tailscale"}</button>`;
    body.innerHTML = `<div class="set-h"><h2>Tailscale</h2>${t.version ? `<span class="muted">v${esc(t.version)}</span>` : ""}</div>${inner}`;
    if (url && $("#ts-qr") && window.qrcode) {
      const q = qrcode(0, "M"); q.addData(url); q.make();
      $("#ts-qr").innerHTML = q.createSvgTag({ cellSize: 8, margin: 2, scalable: true });
    }
    if (t.installed && t.state !== "Running") setPoll(3000);
    if (t.state === "Running") tsLoginUrl = null;
  } else if (setTab === "updates") {
    const u = setInfo.update || {}, st = u.state || {}, chk = u.check, dis = CAN_CTL ? "" : "disabled";
    const running = st.status === "running";
    const when = t => t ? new Date(t * 1000).toLocaleString([], { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" }) : "never";
    const pk = l => l.map(x => `<div class="upk"><b>${esc(x.name)}</b><span>${esc(x.old)} → ${esc(x.new)}</span></div>`).join("");
    body.innerHTML = `
      <div class="set-h"><h2>System updates</h2>${running ? `<span class="pill">Updating…</span>` : ""}</div>
      <div class="set-rows">
        <div class="set-row"><div><b>Last update</b><small>${st.status === "failed" ? "The last update didn't finish — see the log" : "Arch Linux ARM packages (pacman)"}</small></div>
          <div class="set-v"><span>${when(+u.last?.finished)}${u.last?.status ? ` · ${esc(u.last.status)}` : ""}</span></div></div>
        <div class="set-row"><div><b>Available</b><small>${chk ? `Checked ${when(chk.checked)}` : "Not checked yet"}</small></div>
          <div class="set-v"><span>${chk ? `${chk.updates.length} update${chk.updates.length === 1 ? "" : "s"}` : "—"}</span>
            <button class="ctl-pill" data-up="check" ${running ? "disabled" : dis}>Check now</button>
            <button class="ctl-pill" data-up="start" ${running || (chk && !chk.updates.length) ? "disabled" : dis}>Install updates</button></div></div>
      </div>
      ${chk && chk.updates.length ? `<div class="set-h sub"><h2>Will be updated</h2></div><div class="upks">${pk(chk.updates)}</div>` : ""}
      ${chk && chk.held.length ? `<div class="set-h sub"><h2>Held</h2><span class="muted">kernel &amp; boot — custom kernel, update by hand</span></div><div class="upks held">${pk(chk.held)}</div>` : ""}
      ${u.log?.length ? `<div class="set-h sub"><h2>${running ? "Progress" : "Last update log"}</h2></div><pre class="uplog" id="uplog">${esc(u.log.join("\n"))}</pre>` : ""}
      ${!running && st.status === "done" && st.restart === "suggested" ? `<div class="set-actions"><span class="set-p">Updated. A reboot is recommended so everything uses the new versions.</span>
        <button class="ctl-pill warn" data-act="reboot" ${dis}>Reboot now</button></div>` : ""}
      ${st.venv === "broken" ? `<div class="set-p" style="color:#ff8a80">AetherRV's Python environment couldn't be rebuilt after the update — reinstall with deploy/push.sh.</div>` : ""}`;
    const lg = $("#uplog"); if (lg) lg.scrollTop = lg.scrollHeight;
    if (running) setPoll(2000);
  } else {
    const what = {
      updates: "Check for and install system updates" }[setTab];
    body.innerHTML = `<div class="set-h"><h2>${SET_TABS.find(t => t[0] === setTab)[1]}</h2></div><div class="muted set-soon">${what} — next.</div>`;
  }
}
$("#set-tabs").addEventListener("click", e => { const b = e.target.closest("[data-t]"); if (b) { setTab = b.dataset.t; renderSettings(true); } });
$("#set-body").addEventListener("click", async e => {
  const b = e.target.closest("button"); if (!b || b.disabled) return;
  if (b.dataset.off != null) { await settingsAction("pref", { key: "screen_off_min", value: +b.dataset.off }, "Screen auto-off"); return; }
  if (b.dataset.unit) { store.set("vp.temp", b.dataset.unit); location.reload(); return; }
  if (b.dataset.night != null) { night(b.dataset.night === "1"); renderSettings(); return; }
  if (b.dataset.up) {
    if (b.dataset.up === "check") settingsAction("update_check", {}, "Checking for updates");
    else askConfirm("Install system updates?", "Downloads and installs the listed packages (kernel and boot files stay as they are). The heater, inverter and switches keep running; the console stays usable but may be slow for a few minutes.",
      () => settingsAction("update_start", {}, "Starting update"));
    return;
  }
  if (b.dataset.ts) {
    const k = b.dataset.ts;
    if (k === "install") askConfirm("Install Tailscale?", "Downloads and installs Tailscale (about 30 MB) and starts its service. This takes a minute or two.",
      () => settingsAction("tailscale_install", {}, "Installing Tailscale"));
    else if (k === "login") { const r = await settingsAction("tailscale_login", {}, "Tailscale sign-in"); if (r?.auth_url) { tsLoginUrl = r.auth_url; renderSettings(); } }
    else if (k === "logout") askConfirm("Sign out of Tailscale?", "The console leaves your tailnet until you sign in again.",
      () => settingsAction("tailscale_logout", {}, "Tailscale sign-out"));
    return;
  }
  if (b.dataset.wscan != null) { settingsAction("wifi_scan", {}, "Wi-Fi scan"); return; }
  if (b.dataset.wpow != null) { settingsAction("wifi_power", { on: b.dataset.wpow === "1" }, b.dataset.wpow === "1" ? "Wi-Fi on" : "Wi-Fi off"); return; }
  if (b.dataset.wforget != null) {
    const ssid = b.dataset.wforget;
    askConfirm(`Forget ${ssid}?`, "Its saved password is removed; the console won't join it again on its own.", () => settingsAction("wifi_forget", { ssid }, "Forget"));
    return;
  }
  if (b.dataset.wjoin != null) {
    const ssid = b.dataset.wjoin, args = { ssid };
    if (b.dataset.sec === "psk" && b.dataset.known !== "1") {
      const pw = await openKeyboard({ title: `Password for ${ssid}`, password: true, maxlen: 64, pattern: "[\\x20-\\x7e]{8,64}" });
      if (pw == null) return;
      args.passphrase = pw;
    }
    settingsAction("wifi_connect", args, `Joining ${ssid}`);
    return;
  }
  const act = b.dataset.act;
  if (act === "rename") {
    const name = await openKeyboard({ title: "Device name (letters, digits, -)", value: setInfo?.admin?.hostname || "", maxlen: 63,
      pattern: "[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?" });
    if (name && name !== setInfo?.admin?.hostname) settingsAction("set_hostname", { name }, "Rename");
  } else if (act === "restart_kiosk") {
    askConfirm("Restart the display?", "The touchscreen reloads in a few seconds. Nothing in the van changes.", () => settingsAction(act, {}, "Restart display"));
  } else if (act === "reboot") {
    askConfirm("Reboot AetherRV?", "The console is offline for about a minute. The heater, inverter and switches keep running on their own.", () => settingsAction(act, {}, "Reboot"));
  } else if (act === "poweroff") {
    askConfirm("Shut down AetherRV?", "The console stays off until its power is cycled. The heater, inverter and switches keep running on their own.", () => settingsAction(act, {}, "Shut down"));
  }
});

// Starlink sky map: the dish's obstruction grid (one byte per cell, see adapters/starlink.py) drawn once per new
// map into a cached image. On mobile/roam plans the grid is in the dish's frame (bottom edge = the way the dish
// faces), so the image is rotated by the dish azimuth to put north up; the rings and compass stay fixed.
const skyCache = { data: null, url: "" };
function skyImage(m) {
  if (skyCache.data === m.data) return skyCache.url;
  const raw = atob(m.data), n = m.rows, k = 4, size = n * k, c0 = n / 2;
  const cv = document.createElement("canvas"); cv.width = cv.height = size;
  const g = cv.getContext("2d");
  for (let r = 0; r < m.rows; r++) for (let c = 0; c < m.cols; c++) {
    const v = raw.charCodeAt(r * m.cols + c);
    if (v === 255 || Math.hypot(c + 0.5 - c0, r + 0.5 - c0) > c0) continue;
    const q = v / 254;
    g.fillStyle = q >= 0.95 ? "rgba(96,165,250,.85)" : `rgba(248,72,72,${(0.45 + 0.55 * (1 - q)).toFixed(2)})`;
    g.fillRect(c * k + 0.4, r * k + 0.4, k - 0.8, k - 0.8);
  }
  skyCache.data = m.data; skyCache.url = cv.toDataURL();
  return skyCache.url;
}
function skyMap(m, az, size = 300) {
  if (!m?.data || !m.rows) return "";
  const maxT = m.max_theta || 80, R = size / 2 - 18, cx = size / 2;
  const rot = m.frame === "dish" ? (az ?? 180) - 180 : 0;
  const rr = el => (90 - el) / maxT * R;                       // elevation ring radius
  const compass = m.frame === "unknown" ? "" : [["N", 0], ["E", 90], ["S", 180], ["W", 270]].map(([l, a]) => {
    const x = cx + Math.sin(a * Math.PI / 180) * (R + 10), y = cx - Math.cos(a * Math.PI / 180) * (R + 10);
    return `<text x="${x.toFixed(1)}" y="${y.toFixed(1)}">${l}</text>`; }).join("");
  const facing = az == null || m.frame === "unknown" ? "" :
    `<path class="facing" transform="rotate(${az.toFixed(1)} ${cx} ${cx})" d="M${cx} ${cx - R + 9}l-6 -10h12z"/>`;
  return `<div class="sky" style="width:${size}px;height:${size}px">
    <div class="sky-disc" style="inset:${cx - R}px"><img alt="" src="${skyImage(m)}" style="transform:rotate(${rot.toFixed(1)}deg)"></div>
    <svg viewBox="0 0 ${size} ${size}" aria-hidden="true">
      <circle cx="${cx}" cy="${cx}" r="${R}" class="edge"/>
      ${[60, 30].map(el => `<circle cx="${cx}" cy="${cx}" r="${rr(el).toFixed(1)}" class="ring"/>`).join("")}
      <circle cx="${cx}" cy="${cx}" r="2.5" class="zen"/>${compass}${facing}
    </svg></div>`;
}

// WAN priority (Peplink, read-write API client): tap P# → pick P1–P4 → confirm → fresh read, one send, verify.
let wanEdit = null;
$("#wan-card").addEventListener("click", e => {
  const b = e.target.closest("button[data-wan]");
  if (!b || !canControlDev("peplink") || ctlPending) return;
  const id = +b.dataset.wan, w = (S["peplink.wans"] || []).find(x => x.id === id);
  if (!w) return;
  if (!b.dataset.prio) { wanEdit = wanEdit === id ? null : id; renderNetwork(); return; }
  const p = +b.dataset.prio;
  wanEdit = null; renderNetwork();
  if (p === w.priority) return;
  askConfirm(`Set ${w.name} to priority ${p}?`,
    "The router re-evaluates which link carries traffic right away. If the active connection changes, internet drops for a few seconds, and remote (Tailscale) access to this console may reconnect.",
    () => control("set_wan_priority", { id, priority: p }, "WAN priority", "peplink"));
});

// 3D sky (three.js, loaded only when the Network view has a map). The card's HTML is rebuilt on every
// update, so one persistent element is re-attached into a slot each time; it falls back to the 2D map
// until the module loads, or for good if WebGL isn't available.
let sky3d = null, sky3dLoading = false;
function sky3dSlot(map) {
  if (sky3d === false) return null;
  if (!sky3d) {
    if (!sky3dLoading) {
      sky3dLoading = true;
      import("./sky3d.js").then(m => { const s = new m.Sky3D(); sky3d = s.ok ? s : false; renderNetwork(); })
        .catch(() => { sky3d = false; });
    }
    return null;
  }
  return `<div class="sky3d-slot"></div>`;
}
function mountSky3d() {
  const slot = sky3d && $("#sl-card .sky3d-slot");
  if (!slot) return;
  slot.replaceWith(sky3d.el);
  sky3d.setMap(S["starlink.obstruction_map"], S["starlink.azimuth"], S["starlink.elevation"]);
  sky3d.start();
}

$("#sl-card").addEventListener("click", e => {
  if (!e.target.closest("[data-sl=install]") || !canControlDev("starlink") || ctlPending) return;
  askConfirm("Install the Starlink update?",
    "The dish restarts to install the downloaded update. Starlink internet drops for a few minutes; if Starlink is the active connection, remote (Tailscale) access to this console drops too. A firmware install can't be undone.",
    () => control("install_update", true, "Starlink update", "starlink"));
});

// Network history chart: the Peplink's active WAN when the router API is on, else the Starlink dish.
const netKeys = () => S["peplink.reachable"] != null ? ["wan.down_mbps", "wan.up_mbps"] : ["starlink.down_mbps", "starlink.up_mbps"];
const netTitle = () => S["peplink.reachable"] == null ? "Starlink throughput"
  : `${S["peplink.active"] || "Internet"} throughput`;

let slot3d = null;
function renderNetwork() {
  $("#net-chart-title").textContent = netTitle();       // follows WAN failover live
  if (netHistKeys().join(",") !== netSig) loadHistory();   // state arrived after the first fetch (or WAN changed)
  const on = S["starlink.online"], reach = S["starlink.reachable"];
  const slAlerts = (S["starlink.alerts"] || []);
  const obs = S["starlink.obstructed_pct"], map = S["starlink.obstruction_map"];
  const updReady = on != null && S["starlink.update_state"] === "REBOOT_REQUIRED";
  $("#sl-card").innerHTML = reach == null ? `<div class="card-h"><h2>Starlink</h2></div><div class="muted">Not enabled.</div>` : `
    <div class="card-h"><h2>Starlink</h2><span class="pill ${on ? "ok-pill" : "bad-pill"}">${icon(on ? "check" : "alert")}${on ? "Online" : reach ? esc(String(S["starlink.state"] || "").replace(/_/g, " ").toLowerCase()) : "Unreachable"}</span></div>
    <div class="sl-body">
      <div class="sl-left">
        <div class="sl-speeds">
          <div><span class="arrow">↓</span><b>${mbps(S["starlink.down_mbps"])}</b><small>Mbps down</small></div>
          <div><span class="arrow up">↑</span><b>${mbps(S["starlink.up_mbps"])}</b><small>Mbps up</small></div>
        </div>
        <div class="kv-row sl-kv">
          <div><b>${num(S["starlink.latency_ms"])} ms</b><span>latency</span></div>
          <div><b>${S["starlink.drop_rate"] == null ? "—" : (S["starlink.drop_rate"] * 100).toFixed(1) + "%"}</b><span>ping drop</span></div>
          <div><b>${dur(S["starlink.uptime"])}</b><span>uptime</span></div>
          <div><b>${S["starlink.gps_sats"] ?? "—"}</b><span>GPS sats</span></div>
          <div><b class="${S["starlink.snr_persistently_low"] ? "warn-text" : ""}">${S["starlink.signal_quality"] == null ? "—" : Math.round(S["starlink.signal_quality"] * 100) + "%"}</b><span>signal quality</span></div>
          <div><b class="${S["starlink.snr_persistently_low"] ? "warn-text" : ""}">${S["starlink.snr_db"] == null ? "—" : (S["starlink.snr_capped"] ? "≥ 10" : (+S["starlink.snr_db"]).toFixed(1)) + " dB"}</b><span>SNR</span></div>
        </div>
        ${slAlerts.length || updReady ? `<div class="chips-row">${slAlerts.map(a => `<span class="src ${a.severity ? "warn-src" : ""}">${esc(a.label)}</span>`).join("")}
          ${updReady && canControlDev("starlink") ? `<button class="btn-soft sl-install" data-sl="install" ${ctlPending ? "disabled" : ""}>${icon("download")}Install update</button>` : ""}</div>` : ""}
      </div>
      ${map?.data ? ((slot3d = sky3dSlot(map)) ? `<figure class="sl-sky has3d">${slot3d}
          <div class="sky-sw">${esc(S["starlink.software"] || "")}</div>
          <div class="sky-ov tl"><b class="${(obs ?? 0) > 5 ? "warn-text" : ""}">${num(obs, 1)}%</b><span>obstructed</span></div>
          <div class="sky-ov br sky-key"><i class="clear"></i>Clear <i class="blocked"></i>Obstructed</div></figure>`
        : `<figure class="sl-sky">${skyMap(map, S["starlink.azimuth"], 360)}
        <figcaption><b class="${(obs ?? 0) > 5 ? "warn-text" : ""}">${num(obs, 1)}%</b> obstructed
          <span class="sky-key"><i class="clear"></i>Clear <i class="blocked"></i>Obstructed</span></figcaption></figure>`)
      : `<div class="sl-obs">${ring(Math.min(100, obs ?? 0), { size: 150, stroke: 14, color: (obs ?? 0) > 5 ? "var(--warning)" : "var(--c-shore)" })}
        <div class="center"><b>${num(obs, 1)}%</b><small>obstructed</small></div></div>`}
    </div>`;

  mountSky3d();

  const wans = S["peplink.wans"] || [];
  const cw = wans.find(w => w.cellular);
  const c = cw?.cellular;
  const allow = (S["peplink.allowance"] || []).find(a => cw && a.conn === cw.id && (a.sim == null || a.sim === c?.sim));
  $("#cell-card").innerHTML = !c ? `<div class="card-h"><h2>Cellular</h2></div><div class="muted">${S["peplink.reachable"] == null ? "Not enabled." : "No cellular WAN."}</div>` : `
    <div class="card-h"><h2>Cellular</h2><span class="pill ${cw.connected ? "ok-pill" : "bad-pill"}">${icon(cw.connected ? "check" : "alert")}${esc(cw.message || "")}</span></div>
    <div class="cell-main">
      <div class="cell-sig">${bars(c.signal_level ?? 0)}<div><b>${esc(c.network || "")}</b><span>${esc(c.carrier || "")}${c.roaming && c.roaming !== "home" ? " · " + esc(c.roaming) : ""}</span></div></div>
      ${allow ? `<div class="sl-obs">${ring(allow.percent, { size: 150, stroke: 14, color: allow.percent >= 95 ? "var(--critical)" : allow.percent >= 80 ? "var(--warning)" : "var(--c-batt)" })}
        <div class="center"><b>${Math.round(allow.percent)}%</b><small>${(allow.usage_mb / 1024).toFixed(1)} / ${(allow.limit_mb / 1024).toFixed(0)} GB</small></div></div>` : ""}
    </div>
    <div class="kv-row">
      <div><b>${c.rsrp ?? "—"} <small>dBm</small></b><span>RSRP · ${rsrpQ(c.rsrp) ?? "—"}</span></div>
      <div><b>${c.sinr ?? "—"} <small>dB</small></b><span>SINR · ${sinrQ(c.sinr) ?? "—"}</span></div>
      <div><b>${esc(c.band || "—")}</b><span>${esc(c.rat || "band")}${c.ca ? " · CA" : ""}</span></div>
      <div><b>SIM ${c.sim ?? "—"}</b><span>active</span></div>
    </div>`;

  // Priority order (P1 first; same priority: the one in use, then router order); disabled ("off") WANs hidden.
  const wanList = wans.filter(w => w.enabled)
    .sort((a, b) => (a.priority ?? 99) - (b.priority ?? 99) || (b.connected - a.connected) || a.id - b.id);
  $("#wan-card").innerHTML = `<div class="card-h"><h2>WAN priority</h2><span class="muted">${S["peplink.reachable"] === false ? "router unreachable" : ""}</span></div>` +
    (wanList.length ? `<div class="wans">${wanList.map(w => `<div class="wan ${w.name === S["peplink.active"] ? "active" : ""}">
       <span class="led led-${esc(w.led)}"></span>${icon(w.starlink ? "dish" : w.cellular ? "signal" : w.wifi ? "wifi" : "router")}
       <div><b>${esc(w.name)}</b><span>${esc(w.message)}${w.uptime ? " · up " + dur(w.uptime) : ""}</span></div>
       ${canControlDev("peplink") ? `<button class="prio prio-btn ${wanEdit === w.id ? "on" : ""}" data-wan="${w.id}" ${ctlPending ? "disabled" : ""}>${w.priority != null ? "P" + w.priority : "—"}</button>`
         : `<span class="prio">${w.priority != null ? "P" + w.priority : ""}</span>`}
       ${w.name === S["peplink.active"] ? `<span class="pill ok-pill">in use</span>` : ""}</div>
       ${wanEdit === w.id && canControlDev("peplink") ? `<div class="wan-pick"><span>Priority for ${esc(w.name)}</span>
         <div class="seg">${[1, 2, 3, 4].map(p => `<button data-wan="${w.id}" data-prio="${p}" class="${w.priority === p ? "on" : ""}">P${p}</button>`).join("")}</div></div>` : ""}`).join("")}</div>`
      : `<div class="muted">${S["peplink.reachable"] == null ? "Peplink not enabled." : "No WAN data."}</div>`);

}

// ── History charts ───────────────────────────────────────────────────────────
// Each chart fetches only its own keys for its own span; the backend averages into buckets.
const qs = new URLSearchParams(location.search);
const span = { power: +(qs.get("power") || 86400), temp: +(qs.get("temp") || 86400), net: 86400 };   // ?power=604800 sets the initial range
for (const [id, k] of [["#range-power", "power"], ["#range-temp", "temp"]])
  $(id).querySelectorAll("button").forEach(b => b.classList.toggle("on", +b.dataset.s === span[k]));
const hist = {};
async function fetchHist(name, keys, since, points = 360) {
  try {
    const r = await fetch(`/api/history?keys=${keys.join(",")}&since=${since}&points=${points}`);
    hist[name] = await r.json();
  } catch { /* backend restarting; keep the last data */ }
}
async function loadHistory() {
  if (view === "home") await fetchHist("home", ["victron.battery.soc"], 86400);
  if (view === "energy") await fetchHist("power", ["victron.pv.power", "energy.dc.load", "victron.ac.load"], span.power);
  if (view === "climate") await fetchHist("temp", ["climate.cabin.temp", "climate.outside.temp"], span.temp);
  if (view === "network") { netSig = netHistKeys().join(","); await fetchHist("net", netHistKeys(), span.net); }
  renderCharts();
}
// The network chart's keys depend on live state (Peplink active WAN, Starlink present). The first history
// fetch can run before that state arrives, so refetch whenever the key set changes.
const netHistKeys = () => [...netKeys(), ...(S["starlink.reachable"] != null ? ["starlink.snr_db"] : [])];
let netSig = "";
setInterval(loadHistory, 60000);

// Sum two bucketed series point-by-point, matching on nearest timestamp.
function sumSeries(a = [], b = []) {
  if (!b.length) return a;
  let j = 0;
  return a.map(([t, v]) => {
    while (j + 1 < b.length && Math.abs(b[j + 1][0] - t) <= Math.abs(b[j][0] - t)) j++;
    return [t, v + (b[j] ? b[j][1] : 0)];
  });
}
function renderCharts() {
  if (view === "home") timeChart($("#chart-soc"), {
    series: [{ name: "State of charge", color: css("--c-batt"), points: hist.home?.["victron.battery.soc"] }],
    yMin: 0, yMax: 100, yTicks: [0, 25, 50, 75, 100], area: true, fmt: v => `${Math.round(v)}%`,
  });
  if (view === "energy") {
    const h = hist.power || {};
    const series = [
      { name: "Solar", color: css("--c-solar"), points: h["victron.pv.power"] },
      { name: "Consumption", color: css("--c-load"), points: sumSeries(h["energy.dc.load"], h["victron.ac.load"]) },
    ];
    $("#legend-power").innerHTML = series.map(s => `<span><i style="background:${s.color}"></i>${s.name}</span>`).join("");
    timeChart($("#chart-power"), { series, area: true, fmt: v => fmtW(v) });
  }
  if (view === "network") {
    const h = hist.net || {}, [kd, ku] = netKeys();
    const series = [
      { name: "Download", color: css("--c-out"), points: h[kd] },
      { name: "Upload", color: css("--c-cabin"), points: h[ku] },
    ];
    // Starlink SNR as its own lane under the throughput plot: shared time window, its own 0–10 dB scale
    // (one chart with two y-axes would misread).
    const snr = { name: "Starlink SNR", color: css("--c-alt"), points: h["starlink.snr_db"] };
    const showSnr = S["starlink.reachable"] != null;
    const pts = [...series, ...(showSnr ? [snr] : [])].flatMap(s => s.points || []);
    const win = pts.length ? { t0: Math.min(...pts.map(p => p[0])), t1: Math.max(...pts.map(p => p[0])) } : {};
    $("#net-chart-title").textContent = netTitle();
    $("#legend-net").innerHTML = [...series, ...(showSnr ? [snr] : [])].map(s => `<span><i style="background:${s.color}"></i>${s.name}</span>`).join("");
    timeChart($("#chart-net"), { series, area: true, ...win,
      fmt: v => `${+v.toFixed(Math.abs(v) >= 10 ? 0 : Math.abs(v) >= 1 ? 1 : 2)} Mbps` });   // small scales keep decimals (not "0, 0, 0")
    $("#chart-net-snr").hidden = !showSnr;
    if (showSnr) timeChart($("#chart-net-snr"), { series: [snr], yMin: 0, yMax: 10, yTicks: [0, 10], ...win,
      fmt: v => `${+v.toFixed(1)} dB` });
  }
  if (view === "climate") {
    const h = hist.temp || {};
    const conv = pts => (pts || []).map(p => [p[0], tC(p[1])]);
    const series = [
      { name: "Cabin", color: css("--c-cabin"), points: conv(h["climate.cabin.temp"]) },
      { name: "Outside", color: css("--c-out"), points: conv(h["climate.outside.temp"]) },
    ];
    $("#legend-temp").innerHTML = series.map(s => `<span><i style="background:${s.color}"></i>${s.name}</span>`).join("");
    timeChart($("#chart-temp"), { series, zero: false, fmt: v => `${Number.isInteger(+(+v).toFixed(1)) ? Math.round(v) : (+v).toFixed(1)}°` });
  }
}
document.querySelectorAll(".seg.range").forEach(seg => seg.addEventListener("click", e => {
  const b = e.target.closest("button"); if (!b) return;
  span[{ "range-power": "power", "range-temp": "temp", "range-net": "net" }[seg.id]] = +b.dataset.s;
  seg.querySelectorAll("button").forEach(x => x.classList.toggle("on", x === b));
  loadHistory();
}));
addEventListener("resize", () => renderCharts());

// ── Render loop ──────────────────────────────────────────────────────────────
let pending = false;
function render(full = false) {
  pending = false;
  $("#chip-demo").hidden = !S["demo.active"];
  renderAlertChrome();
  renderNetChip();
  renderInvModal();
  renderBattModal();
  const ctlOn = [canControl() && "heat", canControlDev("victron") && "power"].filter(Boolean);
  $("#chip-ro").innerHTML = ctlOn.length ? `${icon("check")}Control: ${ctlOn.join(" + ")}` : `${icon("lock")}Read-only`;
  const up = ws && ws.readyState === 1;
  $("#chip-link").innerHTML = `<span class="dot ${up ? "good" : "bad"}"></span>${up ? "Live" : "Offline"}`;
  ({ home: renderHome, energy: renderEnergy, climate: renderClimate, switches: renderSwitches, network: renderNetwork, alerts: renderAlerts, systems: renderSystems, weather: renderWeather, settings: renderSettings })[view]();
  if (full) renderCharts();
}
const schedule = () => { if (!pending) { pending = true; requestAnimationFrame(() => render()); } };

let ws;
function connect() {
  ws = new WebSocket(`${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/ws`);
  ws.onopen = () => { schedule(); loadHistory(); };
  ws.onclose = () => { schedule(); setTimeout(connect, 2000); };
  ws.onmessage = ev => {
    const m = JSON.parse(ev.data);
    if (m.type === "hello") { CAN_CTL = !!m.control; CTL_WHO = m.who || "viewer"; render(true); }
    if (m.type === "snapshot") for (const k in S) delete S[k];
    if (m.type === "snapshot" || m.type === "delta") { Object.assign(S, m.data); schedule(); }
    if (m.type === "control_result" && ctlWaiters[m.id]) { ctlWaiters[m.id](m); delete ctlWaiters[m.id]; }
  };
}
// Tap the clock to toggle °F/°C.
$(".clock").addEventListener("click", () => { store.set("vp.temp", UNIT === "F" ? "C" : "F"); location.reload(); });

connect();
if (new URLSearchParams(location.search).get("inv") === "1") setTimeout(openInvModal, 2500);   // screenshot/debug aid
if (new URLSearchParams(location.search).get("batt") === "1") setTimeout(openBattModal, 2500);
go(VIEWS.some(v => v[0] === location.hash.slice(1)) ? location.hash.slice(1) : "home");


// ── Home quick switches: DEMO placeholders only. Toggle on screen, send nothing (no bus, no backend).
// Replace with mapped Carling channels once the switch mapping + control approval are done.
const QUICK_DEMO = [
  ["Cabin", "light"], ["Galley", "light"], ["Porch", "light"],
  ["Reading", "light"], ["Water pump", "pump"], ["Vent fan", "fan"],
  ["Awning", "light"], ["Step", "light"], ["Accent", "light"],
];
const quickOn = new Set();
function renderQuick() {
  const real = (S["switches.list"] || []).filter(s => s.home).slice(0, 9);
  const tag = document.querySelector(".quick-card .demo-tag");
  if (real.length) {
    if (tag) { tag.textContent = canControlDev("n2k") ? "live" : "view only"; tag.classList.toggle("live-tag", canControlDev("n2k")); }
    $("#quick-sw").innerHTML = real.map(s =>
      `<button class="quick-btn${s.on ? " on" : ""}${s.state === "stale" ? " na" : ""}" data-sw="${esc(s.id)}">${icon(s.icon || "toggle")}<span>${esc(s.name)}</span></button>`).join("");
    return;
  }
  $("#quick-sw").innerHTML = QUICK_DEMO.map(([n, ic], i) =>
    `<button class="quick-btn${quickOn.has(i) ? " on" : ""}" data-i="${i}">${icon(ic)}<span>${esc(n)}</span></button>`).join("");
}
$("#quick-sw").addEventListener("click", ev => {
  const b = ev.target.closest(".quick-btn"); if (!b) return;
  if (b.dataset.sw) { toggleSwitch(b.dataset.sw); return; }
  const i = +b.dataset.i; quickOn.has(i) ? quickOn.delete(i) : quickOn.add(i);
  renderQuick();
});
renderQuick();


function renderHeatQuick() {
  const R = k => S[`climate.rixens.${k}`];
  const live = R("live"), srcs = R("sources") || [], avail = R("available") || {};
  $("#heat-quick").innerHTML = HQ.map(b => {
    let on = false, st = "", note = "", usable = false;
    if (b.kind === "src") {
      on = srcs.includes(b.k); st = R(b.k) || "off";
      const installed = b.k === "furnace" || avail[b.k] !== false;
      usable = live && installed && canControl() && !ctlPending;
      note = !live ? "offline" : !installed ? "n/a" : on ? (st === "active" ? "active" : "on") : "off";
    } else if (b.kind === "mode") {
      on = !!R(b.k);
      const installed = b.k !== "preheat" || avail.preheat !== false;
      usable = live && installed && canControl() && !ctlPending;
      note = !live ? "offline" : !installed ? "n/a" : on ? "on" : "off";
    } else { note = "not connected"; }
    return `<button class="hq-btn${on ? " on" : ""}${st === "active" ? " active" : ""}" data-hq="${b.k}" ${usable ? "" : "disabled"}>
      ${icon(b.ic)}<span class="hq-l">${b.label}</span></button>`;
  }).join("");
}
$("#heat-quick").addEventListener("click", e => {
  const b = e.target.closest("[data-hq]"); if (!b || b.disabled) return;
  const k = b.dataset.hq; if (!HQ_CTL.has(k)) return;
  const on = !b.classList.contains("on");
  control(k, on, `${HQ.find(x => x.k === k).label} ${on ? "on" : "off"}`);
});

for (const [id, d] of [["#hsp-up", +1], ["#hsp-down", -1]]) {
  $(id).addEventListener("click", e => { e.stopPropagation(); nudgeSetpoint(d); });   // don't open the Climate page
}

applyWxDark();

// ── Screen off (kiosk only). The sway kiosk session runs a helper on 127.0.0.1:8091 that powers the HDMI
// output off/on (the Waveshare panel has no software backlight). While off, a black shield swallows the
// first touch so waking the screen can't also press whatever button is underneath.
const SCREEN = "http://127.0.0.1:8091/screen";
async function screenPower(on) {
  try { const r = await fetch(`${SCREEN}/${on ? "on" : "off"}`, { method: "POST" }); return (await r.json()).on; }
  catch (e) { return null; }
}
if (IS_LOCAL) {
  fetch(SCREEN).then(r => r.json()).then(() => { $("#btn-screen").hidden = false; }).catch(() => {});
  $("#btn-screen").innerHTML = icon("screen_off");
  // While off, sway drops touches, so the helper wakes the screen itself (reads the touch panel directly).
  // The page just watches for the screen coming back and drops the shield a moment later.
  let shieldPoll = null;
  const dropShieldWhenOn = () => {
    clearInterval(shieldPoll);
    shieldPoll = setInterval(async () => {
      try {
        if ((await (await fetch(SCREEN)).json()).on) {
          clearInterval(shieldPoll); lastTouch = Date.now();
          setTimeout(() => { $("#wake-shield").hidden = true; }, 700);   // swallow the rest of the waking touch
        }
      } catch (e) { /* helper restarting: keep polling */ }
    }, 700);
  };
  const screenOff = async (quiet = false) => {
    $("#wake-shield").hidden = false;                       // up first, so the turn-off tap's tail is absorbed too
    if (await screenPower(false) === null) { $("#wake-shield").hidden = true; if (!quiet) toast("Screen control unavailable", "bad"); return; }
    setTimeout(dropShieldWhenOn, 1500);                     // don't mistake the turn-off moment for a wake
  };
  $("#btn-screen").addEventListener("click", () => screenOff());
  // Auto-off after 10 min without a touch (owner's choice). Any touch resets the timer.
  const idleOffMs = () => { const m = S["settings.screen_off_min"]; return m == null ? 10 * 60e3 : m > 0 ? m * 60e3 : Infinity; };
  let lastTouch = Date.now();
  for (const ev of ["pointerdown", "keydown", "wheel"]) document.addEventListener(ev, () => { lastTouch = Date.now(); }, true);
  setInterval(() => {   // applies even with unacknowledged alerts (owner's choice); a NEW critical still wakes it
    if ($("#wake-shield").hidden && Date.now() - lastTouch > idleOffMs() && !$("#btn-screen").hidden) screenOff(true);
  }, 15e3);
  kioskWake = async () => {
    lastTouch = Date.now();
    if (!$("#wake-shield").hidden) { await screenPower(true); dropShieldWhenOn(); }
  };
  const wake = async e => {
    e.preventDefault(); e.stopPropagation();
    await screenPower(true);
    lastTouch = Date.now();
    setTimeout(() => { $("#wake-shield").hidden = true; }, 400);   // keep swallowing the rest of this touch
  };
  $("#wake-shield").addEventListener("pointerdown", wake);
  $("#wake-shield").addEventListener("click", e => { e.preventDefault(); e.stopPropagation(); });
}
