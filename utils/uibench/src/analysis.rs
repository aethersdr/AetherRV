//! Detectors over recorded compositor frames. Times are CLOCK_MONOTONIC ns in, ms out.
//!
//! - fps: presented-frame intervals from the ticker, split into motion bursts (consecutive frames less than
//!   three refresh periods apart) with dropped frames counted inside bursts. A static page presents no frames,
//!   so the burst figures say whether motion is smooth; the raw rate says how busy the page keeps the compositor.
//! - press: the first patch copy after touch-down whose pixels inside the control differ from a pre-tap
//!   reference.
//! - page content: the first frame whose view region shows the new page at any opacity and up to a small
//!   vertical offset (the 180 ms `view-in` ease), by correlation of a patch mosaic against the settled page.
//! - settled: the first frame from which the mosaic stays equal to the settled page (live samples masked).

use crate::capture::{Patch, Rect, Update};
use serde::Serialize;
use serde_json::{Value, json};

pub fn pct(v: &[f64], p: f64) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let i = ((p / 100.0) * (s.len() - 1) as f64).round() as usize;
    Some(s[i.min(s.len() - 1)])
}
pub fn pct_u32(v: &[u32], p: f64) -> Option<u32> {
    pct(&v.iter().map(|&x| x as f64).collect::<Vec<_>>(), p).map(|x| x as u32)
}
pub fn r2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}
pub fn ms(a: u64, b: u64) -> f64 {
    r2((b as f64 - a as f64) / 1e6)
}
pub fn summary(v: &[f64]) -> Value {
    if v.is_empty() {
        return json!({"n": 0});
    }
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    json!({"n": v.len(), "median": pct(v, 50.0).map(r2), "p95": pct(v, 95.0).map(r2), "max": pct(v, 100.0).map(r2),
           "min": pct(v, 0.0).map(r2), "mean": r2(mean)})
}

#[derive(Serialize, Default, Clone)]
pub struct FpsStats {
    pub frames: usize,
    pub secs: f64,
    pub fps_raw: f64,
    pub interval_ms: Value,
    pub burst_frames: usize,
    pub burst_secs: f64,
    /// presented frames per second while something on screen moves (inside bursts)
    pub fps_motion: Option<f64>,
    /// refresh slots inside bursts that got no new frame
    pub dropped_in_motion: u64,
    pub longest_burst_ms: f64,
}

pub fn fps_stats(ticks: &[u64], refresh_mhz: i32) -> FpsStats {
    let mut t = ticks.to_vec();
    t.sort();
    t.dedup();
    let period_ms = if refresh_mhz > 0 { 1e6 / refresh_mhz as f64 } else { 1000.0 / 60.0 };
    if t.len() < 2 {
        return FpsStats { frames: t.len(), interval_ms: json!({"n": 0}), ..Default::default() };
    }
    let iv: Vec<f64> = t.windows(2).map(|w| (w[1] - w[0]) as f64 / 1e6).collect();
    let secs = (t[t.len() - 1] - t[0]) as f64 / 1e9;
    let (mut bf, mut bsecs, mut dropped, mut longest) = (0usize, 0f64, 0u64, 0f64);
    let mut i = 0;
    while i < iv.len() {
        let mut j = i;
        while j < iv.len() && iv[j] < 3.0 * period_ms {
            j += 1;
        }
        let n = j - i;
        if n >= 4 {
            let d: f64 = iv[i..j].iter().sum();
            bf += n + 1;
            bsecs += d / 1000.0;
            longest = longest.max(d);
            dropped += iv[i..j].iter().map(|&x| ((x / period_ms).round() as i64 - 1).max(0) as u64).sum::<u64>();
        }
        i = j + 1;
    }
    FpsStats {
        frames: t.len(),
        secs: r2(secs),
        fps_raw: r2((t.len() - 1) as f64 / secs),
        interval_ms: summary(&iv),
        burst_frames: bf,
        burst_secs: r2(bsecs),
        fps_motion: if bsecs > 0.0 { Some(r2((bf as f64 - 1.0).max(0.0) / bsecs)) } else { None },
        dropped_in_motion: dropped,
        longest_burst_ms: r2(longest),
    }
}

/// Fraction of pixels (RGB triples) where any channel differs by more than `thr`.
pub fn diff_frac(a: &[u8], b: &[u8], thr: u8) -> f64 {
    let n = a.len().min(b.len()) / 3;
    if n == 0 {
        return 0.0;
    }
    let mut c = 0usize;
    for i in 0..n {
        let k = i * 3;
        if a[k].abs_diff(b[k]) > thr || a[k + 1].abs_diff(b[k + 1]) > thr || a[k + 2].abs_diff(b[k + 2]) > thr {
            c += 1;
        }
    }
    c as f64 / n as f64
}

pub fn mean_luma(a: &[u8]) -> f64 {
    let n = a.len() / 3;
    if n == 0 {
        return 0.0;
    }
    let mut s = 0f64;
    for i in 0..n {
        s += 0.2126 * a[i * 3] as f64 + 0.7152 * a[i * 3 + 1] as f64 + 0.0722 * a[i * 3 + 2] as f64;
    }
    s / n as f64
}

#[derive(Serialize, Clone)]
pub struct Press {
    pub latency_ms: Option<f64>,
    pub changed_frac: f64,
    pub copies_seen: usize,
}

/// The first copy of patch `id` in [t0, until] whose pixels differ from `base` in at least `need` of them.
pub fn press(updates: &[Update], id: u32, base: &[u8], t0: u64, until: u64, thr: u8, need: f64) -> Press {
    let mut best = 0f64;
    let mut seen = 0;
    let mut v: Vec<&Update> = updates.iter().filter(|u| u.id == id && u.t >= t0 && u.t <= until).collect();
    v.sort_by_key(|u| u.t);
    for u in v {
        seen += 1;
        let d = diff_frac(base, &u.px, thr);
        best = best.max(d);
        if d >= need {
            return Press { latency_ms: Some(ms(t0, u.t)), changed_frac: r2(d * 1000.0) / 1000.0, copies_seen: seen };
        }
    }
    Press { latency_ms: None, changed_frac: r2(best * 1000.0) / 1000.0, copies_seen: seen }
}

/// The mosaic used for page and boot detection: three horizontal bands (8 px tall at 25/50/75 % of the height) and
/// one vertical band (8 px wide at 50 % of the width) across `area`, every 2nd pixel. Calibration: four copies per
/// frame cost the compositor nothing measurable on the Pi 5, while a grid of 12 small patches (12 copies per frame)
/// already backs up the GPU (fence latency 134 ms, page at ~40 fps). Each band is one screencopy job.
pub fn bands(area: Rect, first_id: u32) -> Vec<Patch> {
    let mut v: Vec<Patch> = [0.25, 0.5, 0.75]
        .iter()
        .enumerate()
        .map(|(i, f)| Patch { id: first_id + i as u32, rect: Rect { x: area.x, y: area.y + (area.h as f64 * f) as i32 - 4, w: area.w, h: 8 }, step: 2 })
        .collect();
    v.push(Patch { id: first_id + 3, rect: Rect { x: area.x + area.w / 2 - 4, y: area.y, w: 8, h: area.h }, step: 2 });
    v
}

/// Times of the copies (of the patches in `ids`) whose pixels differ from that patch's previous copy: frames in
/// which something inside those regions actually changed. (wlroots tracks screencopy damage per client, not per
/// region, so a patch is copied on any damage anywhere on the output; the pixels say whether it changed.)
pub fn change_times(updates: &[Update], ids: &[u32], thr: u8) -> Vec<u64> {
    let mut v: Vec<&Update> = updates.iter().filter(|u| ids.contains(&u.id)).collect();
    v.sort_by_key(|u| u.t);
    let mut last: std::collections::HashMap<u32, &[u8]> = Default::default();
    let mut out = vec![];
    for u in v {
        if let Some(prev) = last.get(&u.id)
            && diff_frac(prev, &u.px, thr) > 0.0
        {
            out.push(u.t);
        }
        last.insert(u.id, &u.px);
    }
    out.dedup();
    out
}

/// Patches laid out as a grid inside `area` (cols × rows patches of `size` px, evenly spaced).
pub fn grid_patches(area: Rect, cols: i32, rows: i32, size: i32, step: i32, first_id: u32) -> Vec<Patch> {
    let mut v = vec![];
    let mut id = first_id;
    for r in 0..rows {
        for c in 0..cols {
            let cx = area.x + (2 * c + 1) * area.w / (2 * cols);
            let cy = area.y + (2 * r + 1) * area.h / (2 * rows);
            v.push(Patch { id, rect: Rect { x: cx - size / 2, y: cy - size / 2, w: size, h: size }, step });
            id += 1;
        }
    }
    v
}

/// The state of a set of patches over time: at each copy time, the latest pixels of every patch, concatenated.
pub fn timeline(patches: &[Patch], initial: &[Update], updates: &[Update], t0: u64) -> Vec<(u64, Vec<u8>)> {
    let mut cur: Vec<Vec<u8>> = patches
        .iter()
        .map(|p| {
            let (w, h) = p.dims();
            initial.iter().find(|u| u.id == p.id).map(|u| u.px.to_vec()).unwrap_or_else(|| vec![0; w * h * 3])
        })
        .collect();
    let mut ups: Vec<&Update> = updates.iter().filter(|u| u.t >= t0 && patches.iter().any(|p| p.id == u.id)).collect();
    ups.sort_by_key(|u| u.t);
    let mut out: Vec<(u64, Vec<u8>)> = vec![];
    let mut i = 0;
    while i < ups.len() {
        let t = ups[i].t;
        while i < ups.len() && ups[i].t == t {
            if let Some(k) = patches.iter().position(|p| p.id == ups[i].id)
                && ups[i].px.len() == cur[k].len()
            {
                cur[k] = ups[i].px.to_vec();
            }
            i += 1;
        }
        out.push((t, cur.concat()));
    }
    out
}

/// A settled reference for a patch mosaic: per patch, the full-resolution pixels of the patch extended `margin`
/// rows upwards (for the offset search), plus the sampling grid the measured copies use.
pub struct MosaicRef {
    pub patches: Vec<Patch>,
    full: Vec<(Vec<u8>, usize, usize)>, // pixels, width, rows of margin actually present
    pub grid: Vec<u8>,
    pub bg: [f64; 3],
    pub live: Vec<bool>,
}

/// The extended rects to capture (step 1) for a MosaicRef.
pub fn ref_patches(patches: &[Patch], margin: i32, first_id: u32) -> Vec<Patch> {
    patches
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let y0 = (p.rect.y - margin).max(0);
            Patch { id: first_id + i as u32, rect: Rect { x: p.rect.x, y: y0, w: p.rect.w, h: p.rect.h + (p.rect.y - y0) }, step: 1 }
        })
        .collect()
}

impl MosaicRef {
    pub fn new(patches: &[Patch], refs: &[Patch], captured: &[Update]) -> Option<MosaicRef> {
        let mut full = vec![];
        let mut grid = vec![];
        for (p, r) in patches.iter().zip(refs) {
            let u = captured.iter().find(|u| u.id == r.id)?;
            let w = r.rect.w as usize;
            if u.px.len() < w * r.rect.h as usize * 3 {
                return None;
            }
            let m = (p.rect.y - r.rect.y) as usize;
            let s = p.step.max(1) as usize;
            let (gw, gh) = p.dims();
            for gy in 0..gh {
                for gx in 0..gw {
                    let k = ((gy * s + m) * w + gx * s) * 3;
                    grid.extend_from_slice(&u.px[k..k + 3]);
                }
            }
            full.push((u.px.to_vec(), w, m));
        }
        // background: the most common (quantised) colour of the settled samples
        let mut hist = std::collections::HashMap::<(u8, u8, u8), usize>::new();
        for c in grid.chunks_exact(3) {
            *hist.entry((c[0] >> 3, c[1] >> 3, c[2] >> 3)).or_default() += 1;
        }
        let q = hist.into_iter().max_by_key(|(_, n)| *n).map(|(q, _)| q).unwrap_or((0, 0, 0));
        let (mut acc, mut n) = ([0f64; 3], 0f64);
        for c in grid.chunks_exact(3) {
            if (c[0] >> 3, c[1] >> 3, c[2] >> 3) == q {
                for k in 0..3 {
                    acc[k] += c[k] as f64;
                }
                n += 1.0;
            }
        }
        let bg = [acc[0] / n.max(1.0), acc[1] / n.max(1.0), acc[2] / n.max(1.0)];
        let live = vec![false; grid.len() / 3];
        Some(MosaicRef { patches: patches.to_vec(), full, grid, bg, live })
    }

    /// Mark samples that change between consecutive states in `settle` (animations, clocks, live values), plus
    /// their neighbours within the same patch.
    pub fn mask_live(&mut self, settle: &[&[u8]], thr: u8) {
        let n = self.live.len();
        let mut m = vec![false; n];
        let mut prev: &[u8] = &self.grid;
        for s in settle {
            for (i, mi) in m.iter_mut().enumerate() {
                let k = i * 3;
                if k + 2 < s.len() && (s[k].abs_diff(prev[k]) > thr || s[k + 1].abs_diff(prev[k + 1]) > thr || s[k + 2].abs_diff(prev[k + 2]) > thr) {
                    *mi = true;
                }
            }
            prev = s;
        }
        let mut d = m.clone();
        let mut off = 0usize;
        for p in &self.patches {
            let (gw, gh) = p.dims();
            for gy in 0..gh {
                for gx in 0..gw {
                    if m[off + gy * gw + gx] {
                        for dy in -2i64..=2 {
                            for dx in -2i64..=2 {
                                let (x, y) = (gx as i64 + dx, gy as i64 + dy);
                                if x >= 0 && y >= 0 && (x as usize) < gw && (y as usize) < gh {
                                    d[off + y as usize * gw + x as usize] = true;
                                }
                            }
                        }
                    }
                }
            }
            off += gw * gh;
        }
        self.live = d;
    }

    pub fn live_frac(&self) -> f64 {
        self.live.iter().filter(|&&b| b).count() as f64 / self.live.len().max(1) as f64
    }

    /// Fraction of (non-live) samples that differ strongly from the settled reference.
    pub fn settled_diff(&self, g: &[u8], thr: u8) -> f64 {
        let (mut c, mut n) = (0usize, 0usize);
        for (i, &l) in self.live.iter().enumerate() {
            let k = i * 3;
            if l || k + 2 >= g.len() {
                continue;
            }
            n += 1;
            if g[k].abs_diff(self.grid[k]) > thr || g[k + 1].abs_diff(self.grid[k + 1]) > thr || g[k + 2].abs_diff(self.grid[k + 2]) > thr {
                c += 1;
            }
        }
        c as f64 / n.max(1) as f64
    }

    /// Content mismatch 1 − ρ² between (state − bg) and (settled − bg) shifted down by 0..=max_shift px, with the
    /// opacity estimate α and the best shift. ρ ≈ 1 means the new page is drawn, at whatever opacity and offset.
    pub fn content(&self, g: &[u8], max_shift: usize) -> (f64, f64, usize) {
        let mut best = (1.0, 0.0, 0);
        for sh in 0..=max_shift {
            let (mut ff, mut fr, mut rr) = (0f64, 0f64, 0f64);
            let mut off = 0usize;
            for (p, (full, w, m)) in self.patches.iter().zip(&self.full) {
                let s = p.step.max(1) as usize;
                let (gw, gh) = p.dims();
                for gy in 0..gh {
                    let yy = gy * s + m;
                    if yy < sh {
                        continue;
                    }
                    for gx in 0..gw {
                        let i = off + gy * gw + gx;
                        let k = i * 3;
                        if self.live[i] || k + 2 >= g.len() {
                            continue;
                        }
                        let kf = ((yy - sh) * w + gx * s) * 3;
                        for c in 0..3 {
                            let a = g[k + c] as f64 - self.bg[c];
                            let b = full[kf + c] as f64 - self.bg[c];
                            ff += a * a;
                            fr += a * b;
                            rr += b * b;
                        }
                    }
                }
                off += gw * gh;
            }
            if ff <= 0.0 || rr <= 0.0 {
                continue;
            }
            let mism = 1.0 - fr * fr / (ff * rr);
            if mism < best.0 {
                best = (mism, fr / rr, sh);
            }
        }
        best
    }
}

#[derive(Serialize, Clone)]
pub struct Switch {
    /// first copy after the tap whose pixels differ from before it
    pub first_change_ms: Option<f64>,
    /// first frame showing the new page's content (correlation, any opacity / offset)
    pub content_ms: Option<f64>,
    /// first frame from which the mosaic stays equal to the settled page
    pub settled_ms: Option<f64>,
    pub mismatch_before: f64,
    pub mismatch_at_content: Option<f64>,
    pub alpha_at_content: Option<f64>,
    pub shift_at_content: Option<usize>,
    pub live_frac: f64,
    pub states: usize,
    /// presented frames (ticker) from the tap to the settled frame
    pub transition: Value,
}

pub struct SwitchParams {
    pub tau_content: f64,
    pub min_alpha: f64,
    pub tau_settled: f64,
    pub thr: u8,
}

pub fn switch(tl: &[(u64, Vec<u8>)], base: &[u8], r: &MosaicRef, t0: u64, ticks: &[u64], p: &SwitchParams, refresh_mhz: i32) -> Switch {
    let first_change = tl.iter().find(|(_, g)| diff_frac(base, g, p.thr) > 0.002).map(|(t, _)| ms(t0, *t));
    let mut content = None;
    let mut at = (None, None, None);
    for (t, g) in tl {
        let (m, a, s) = r.content(g, 10);
        if m <= p.tau_content && a >= p.min_alpha {
            content = Some(ms(t0, *t));
            at = (Some(r2(m * 1000.0) / 1000.0), Some(r2(a)), Some(s));
            break;
        }
    }
    let mut settled = None;
    for (t, g) in tl.iter().rev() {
        if r.settled_diff(g, p.thr) <= p.tau_settled {
            settled = Some(ms(t0, *t));
        } else {
            break;
        }
    }
    // a settled frame shows the whole page by definition (on a page that is mostly background the correlation is
    // dominated by small differences and can fail where the strict comparison already passes)
    if let Some(st) = settled
        && content.is_none_or(|c| st < c)
    {
        content = Some(st);
        at = (None, None, None);
    }
    let end = settled.map(|s| t0 + (s * 1e6) as u64).unwrap_or(u64::MAX);
    let tr: Vec<u64> = ticks.iter().copied().filter(|&t| t >= t0 && t <= end).collect();
    let ts = fps_stats(&tr, refresh_mhz);
    // the ease-in itself: states between "content" and "settled" whose pixels changed (the page moving)
    let c0 = content.map(|c| t0 + (c * 1e6) as u64).unwrap_or(u64::MAX);
    let mut prev: Option<&[u8]> = None;
    let mut moving = vec![];
    for (t, g) in tl.iter().filter(|(t, _)| *t >= c0 && *t <= end) {
        if prev.is_none_or(|p| diff_frac(p, g, 4) > 0.0) {
            moving.push(*t);
        }
        prev = Some(g);
    }
    let mv: Vec<f64> = moving.windows(2).map(|w| (w[1] - w[0]) as f64 / 1e6).collect();
    Switch {
        first_change_ms: first_change,
        content_ms: content,
        settled_ms: settled,
        mismatch_before: r2(r.content(base, 10).0 * 1000.0) / 1000.0,
        mismatch_at_content: at.0,
        alpha_at_content: at.1,
        shift_at_content: at.2,
        live_frac: r2(r.live_frac() * 1000.0) / 1000.0,
        states: tl.len(),
        transition: json!({"frames": ts.frames, "interval_ms": ts.interval_ms, "dropped": ts.dropped_in_motion,
                           "ease_in_frames": moving.len(), "ease_in_interval_ms": summary(&mv)}),
    }
}
