// Small SVG chart kit: time-series chart with crosshair, ring gauge, arc dial.
// Specs follow the dataviz method: 2px lines, ~10% area wash, hairline grid, one y-axis,
// legend for ≥2 series, crosshair + tooltip on hover/touch.

const NS = "http://www.w3.org/2000/svg";
const esc = s => String(s).replace(/[&<>"']/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

function niceTicks(min, max, count = 4) {
  if (min === max) { max = min + 1; }
  const span = max - min;
  const raw = span / count;
  const mag = Math.pow(10, Math.floor(Math.log10(raw)));
  const step = [1, 2, 2.5, 5, 10].map(m => m * mag).find(s => span / s <= count) || 10 * mag;
  const lo = Math.floor(min / step) * step, hi = Math.ceil(max / step) * step;
  const ticks = [];
  for (let v = lo; v <= hi + step / 2; v += step) ticks.push(+v.toFixed(6));
  return ticks;
}

const hhmm = t => { const d = new Date(t * 1000); return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`; };

/**
 * timeChart(el, { series: [{name, color, points:[[t,v]]}], unit, yMin, yMax, area, fmt })
 * Re-renders fully; cheap enough at ≤500 points/series.
 */
export function timeChart(el, opt) {
  const W = el.clientWidth || 600, H = el.clientHeight || 220;
  const pad = { l: 52, r: 16, t: 12, b: 28 };
  const series = opt.series.filter(s => s.points && s.points.length);
  el.innerHTML = "";
  if (!series.length) { el.innerHTML = `<div class="chart-empty">Collecting data…</div>`; return; }

  const all = series.flatMap(s => s.points);
  // opt.t0 / opt.t1 let stacked charts share one time window (e.g. throughput + SNR lanes)
  const t0 = opt.t0 ?? Math.min(...all.map(p => p[0])), t1 = opt.t1 ?? Math.max(...all.map(p => p[0]));
  let vmin = opt.yMin ?? (opt.zero === false ? Math.min(...all.map(p => p[1])) : Math.min(0, ...all.map(p => p[1])));
  let vmax = opt.yMax ?? Math.max(...all.map(p => p[1]));
  const ticks = niceTicks(vmin, vmax, 4);
  vmin = opt.yMin ?? ticks[0]; vmax = opt.yMax ?? ticks[ticks.length - 1];
  const x = t => pad.l + (t - t0) / Math.max(1, t1 - t0) * (W - pad.l - pad.r);
  const y = v => pad.t + (1 - (v - vmin) / Math.max(1e-9, vmax - vmin)) * (H - pad.t - pad.b);
  const fmt = opt.fmt || (v => `${Math.round(v)}${opt.unit || ""}`);

  let svg = `<svg viewBox="0 0 ${W} ${H}" width="${W}" height="${H}" class="tchart">`;
  // grid + y labels
  for (const v of (opt.yTicks || ticks)) {
    if (v < vmin || v > vmax) continue;
    svg += `<line class="grid${v === 0 ? " base" : ""}" x1="${pad.l}" x2="${W - pad.r}" y1="${y(v)}" y2="${y(v)}"/>`;
    svg += `<text class="axis" x="${pad.l - 10}" y="${y(v) + 5}" text-anchor="end">${esc(fmt(v))}</text>`;
  }
  // x labels: hours for ≤ 2 days, days (local midnight) beyond that
  const span = t1 - t0;
  if (span > 2 * 86400) {
    const everyD = span > 14 * 86400 ? 7 : span > 7 * 86400 ? 2 : 1;
    const d = new Date(t0 * 1000); d.setHours(24, 0, 0, 0);
    for (let i = 0; d.getTime() / 1000 <= t1; i++, d.setDate(d.getDate() + 1)) {
      if (i % everyD) continue;
      const t = d.getTime() / 1000;
      svg += `<line class="grid" x1="${x(t)}" x2="${x(t)}" y1="${pad.t}" y2="${H - pad.b}" opacity=".5"/>`;
      svg += `<text class="axis" x="${x(t)}" y="${H - 6}" text-anchor="middle">${d.toLocaleDateString([], { weekday: "short" })} ${d.getDate()}</text>`;
    }
  } else {
    const stepH = span > 12 * 3600 ? 3 : span > 4 * 3600 ? 1 : span > 1.5 * 3600 ? 0.5 : 0.25;
    const first = Math.ceil(t0 / (stepH * 3600)) * stepH * 3600;
    for (let t = first; t <= t1; t += stepH * 3600) {
      svg += `<text class="axis" x="${x(t)}" y="${H - 6}" text-anchor="middle">${hhmm(t)}</text>`;
    }
  }
  // series
  for (const s of series) {
    const d = s.points.map((p, i) => `${i ? "L" : "M"}${x(p[0]).toFixed(1)},${y(p[1]).toFixed(1)}`).join("");
    if (opt.area) {
      const base = y(Math.max(vmin, 0));
      svg += `<path d="${d}L${x(s.points.at(-1)[0])},${base}L${x(s.points[0][0])},${base}Z" fill="${s.color}" opacity=".12"/>`;
    }
    svg += `<path d="${d}" fill="none" stroke="${s.color}" stroke-width="2" stroke-linejoin="round" stroke-linecap="round"/>`;
    const last = s.points.at(-1);
    svg += `<circle cx="${x(last[0])}" cy="${y(last[1])}" r="5" fill="${s.color}" stroke="var(--surface)" stroke-width="2"/>`;
  }
  svg += `<g class="xhair" style="display:none"><line y1="${pad.t}" y2="${H - pad.b}"/>${series.map(s => `<circle r="5" fill="${s.color}" stroke="var(--surface)" stroke-width="2"/>`).join("")}</g>`;
  svg += `<rect class="hit" x="${pad.l}" y="0" width="${W - pad.l - pad.r}" height="${H}" fill="transparent"/></svg>`;
  el.innerHTML = svg + `<div class="tip" style="display:none"></div>`;

  // crosshair
  const root = el.querySelector("svg"), g = el.querySelector(".xhair"), tip = el.querySelector(".tip");
  const line = g.querySelector("line"), dots = g.querySelectorAll("circle");
  const move = ev => {
    const r = root.getBoundingClientRect();
    const px = (ev.touches ? ev.touches[0].clientX : ev.clientX) - r.left;
    const t = t0 + (px - pad.l) / (W - pad.l - pad.r) * (t1 - t0);
    let rows = "";
    series.forEach((s, i) => {
      const p = s.points.reduce((a, b) => Math.abs(b[0] - t) < Math.abs(a[0] - t) ? b : a);
      dots[i].setAttribute("cx", x(p[0])); dots[i].setAttribute("cy", y(p[1]));
      rows += `<div><i style="background:${s.color}"></i>${esc(s.name)}<b>${esc(fmt(p[1]))}</b></div>`;
    });
    const cx = Math.min(Math.max(px, pad.l), W - pad.r);
    line.setAttribute("x1", cx); line.setAttribute("x2", cx);
    g.style.display = "";
    const when = new Date(t * 1000);
    const label = span > 86400 ? when.toLocaleString([], { weekday: "short", hour: "2-digit", minute: "2-digit" }) : when.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
    tip.innerHTML = `<div class="tip-t">${label}</div>${rows}`;
    tip.style.display = "";
    tip.style.left = `${Math.min(cx + 14, W - tip.offsetWidth - 4)}px`;
  };
  const hide = () => { g.style.display = "none"; tip.style.display = "none"; };
  const hit = el.querySelector(".hit");
  hit.addEventListener("pointermove", move);
  hit.addEventListener("pointerdown", move);
  hit.addEventListener("pointerleave", hide);
  hit.addEventListener("pointerup", ev => { if (ev.pointerType === "touch") setTimeout(hide, 2500); });
}

/** Ring gauge (0..100). Returns SVG markup. */
export function ring(value, { size = 260, stroke = 18, color = "var(--s1)", track = "var(--track)" } = {}) {
  const r = (size - stroke) / 2, c = 2 * Math.PI * r;
  const v = Math.max(0, Math.min(100, value ?? 0));
  return `<svg viewBox="0 0 ${size} ${size}" class="ring">
    <circle cx="${size / 2}" cy="${size / 2}" r="${r}" fill="none" stroke="${track}" stroke-width="${stroke}"/>
    <circle cx="${size / 2}" cy="${size / 2}" r="${r}" fill="none" stroke="${color}" stroke-width="${stroke}"
      stroke-linecap="round" stroke-dasharray="${(c * v / 100).toFixed(1)} ${c.toFixed(1)}"
      transform="rotate(-90 ${size / 2} ${size / 2})" class="ring-val"/>
  </svg>`;
}

/** 270° dial for temperature: value + setpoint marker, range [min,max]. */
// Geometry shared with the drag handler: knob centre radius, start angle (deg, 0 = +x, clockwise) and sweep.
export const dialGeom = (size, stroke) => ({ r: (size - stroke) / 2 - 14, a0: 135, sweep: 270 });

export function dial(value, setpoint, { min = 5, max = 30, size = 420, stroke = 22, color = "var(--s2)", active = false } = {}) {
  const cx = size / 2, cy = size / 2, r = (size - stroke) / 2 - 14;
  const a0 = 135, sweep = 270;
  const ang = v => a0 + sweep * (Math.max(min, Math.min(max, v)) - min) / (max - min);
  const pt = (a, rr = r) => [cx + rr * Math.cos(a * Math.PI / 180), cy + rr * Math.sin(a * Math.PI / 180)];
  const arc = (from, to) => {
    const [x1, y1] = pt(from), [x2, y2] = pt(to);
    return `M${x1.toFixed(1)},${y1.toFixed(1)}A${r},${r} 0 ${to - from > 180 ? 1 : 0} 1 ${x2.toFixed(1)},${y2.toFixed(1)}`;
  };
  let ticks = "";
  for (let v = min; v <= max; v += 1) {
    const a = ang(v), major = v % 5 === 0;
    const [x1, y1] = pt(a, r + stroke / 2 + 6), [x2, y2] = pt(a, r + stroke / 2 + (major ? 18 : 11));
    ticks += `<line x1="${x1}" y1="${y1}" x2="${x2}" y2="${y2}" class="${major ? "tick-major" : "tick"}"/>`;
  }
  const sp = setpoint != null ? pt(ang(setpoint)) : null;
  return `<svg viewBox="0 0 ${size} ${size}" class="dial">
    ${ticks}
    <path d="${arc(a0, a0 + sweep)}" fill="none" stroke="var(--track)" stroke-width="${stroke}" stroke-linecap="round"/>
    ${value != null ? `<path d="${arc(a0, ang(value))}" fill="none" stroke="${color}" stroke-width="${stroke}" stroke-linecap="round"/>` : ""}
    ${sp ? `<circle class="dial-knob${active ? " active" : ""}" cx="${sp[0]}" cy="${sp[1]}" r="${stroke / 2 + (active ? 11 : 5)}" fill="var(--surface)" stroke="${active ? "var(--c-cabin)" : "var(--ink)"}" stroke-width="${active ? 4 : 3}"/>` : ""}
  </svg>`;
}

export { esc };
