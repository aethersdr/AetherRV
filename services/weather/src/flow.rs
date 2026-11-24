//! Motion fields between two recorded radar frames (Python `flow_pair`), in pure Rust (owner, 2026-11-23: no
//! OpenCV).
//!
//! **Input**, exactly as Python prepares it for OpenCV: the 3×3 z6 tiles are stitched, every visible pixel becomes
//! its NEXRAD palette rank (1-15, nearest colour), ×16, halved with an area average, truncated to 8 bits and
//! blurred with a Gaussian σ = 2 (OpenCV's 13 taps, reflect-101 borders, rounded to 8 bits).
//!
//! **Estimator**: Python uses OpenCV's DIS flow (medium preset) and then blurs the per-pixel field with σ = 12
//! half-res px, because a per-pixel field tears echoes. What survives that blur is a locally constant
//! translation, and the least-squares estimate of a locally constant translation is Lucas–Kanade. So this is
//! DIS's own skeleton (coarse-to-fine inverse search on a patch grid) with a Gaussian-weighted Lucas–Kanade patch
//! and without DIS's variational refinement (the σ = 12 blur does that job):
//! - a Gaussian pyramid (OpenCV `pyrDown`: 5-tap binomial, reflect-101) from 768 px down to 48 px, 5 levels, so
//!   motion of tens of z6 px per frame is still small at the coarsest level;
//! - on every level a patch every [`Params::stride`] px; each patch (Gaussian window σ = [`Params::win_sigma`] px of
//!   that level, cut at 2σ, so a larger area on the coarser levels) refines its own translation by inverse
//!   compositional Gauss–Newton: A's gradients and the 2×2 Hessian once, then B is sampled (bilinear) at the
//!   current shift each iteration. Patches are independent, so an error at the image edge can't spread;
//! - the gradients are centred on the patch mean, which fits a per-patch brightness offset: the counterpart of DIS's
//!   mean normalisation, so an echo that strengthens or weakens between frames isn't read as motion;
//! - a Tikhonov term ([`Params::lambda`]) and a step clamp ([`Params::max_step`]); a patch without texture (no
//!   echo) isn't fitted and keeps the coarser level's estimate. On the coarsest level those patches take the
//!   confidence-weighted mean of the textured ones around them ([`Params::fill_sigma`]), so motion carries into
//!   the empty area ahead of an echo, where the UI's shader samples it (DIS gets there by spatial propagation);
//! - after each level a 3×3 median over the patch grid drops single-patch outliers; the grid is then upsampled
//!   (bilinear, ×2) to seed the next level, and at the end to the full 768 px field.
//!
//! **Output**, exactly as Python: Gaussian σ = 12 (97 taps) on the full field, area-averaged to 32×32, ×2 (half-res px
//! → z6 px), rounded to 0.1. Units: z6 pixels (512 px tiles) over the pair's interval, A(p) ≈ B(p + d).

use anyhow::{Context, bail};
use std::path::Path;

pub const FLOW_Z: u32 = 6;
pub const FLOW_N: usize = 32;
/// Scheme-6 (NEXRAD) colours, weakest → strongest echo: radar tiles are palette images, the flow needs intensity.
pub const NEXRAD: [[u8; 3]; 15] = [
    [4, 233, 231],
    [1, 159, 244],
    [3, 0, 244],
    [2, 253, 2],
    [1, 197, 1],
    [0, 142, 0],
    [253, 248, 2],
    [229, 188, 0],
    [253, 149, 0],
    [253, 0, 0],
    [212, 0, 0],
    [188, 0, 0],
    [248, 0, 253],
    [152, 84, 198],
    [253, 253, 253],
];

/// The estimator's knobs (see the module docs).
#[derive(Clone, Debug)]
pub struct Params {
    /// Pyramid levels (768 px → 48 px).
    pub levels: usize,
    /// No level smaller than this.
    pub min_size: usize,
    /// Patch grid spacing, px of each level.
    pub stride: usize,
    /// Gauss–Newton iterations per patch and level (fewer once the step is below 0.01 px).
    pub iters: usize,
    /// Patch window: Gaussian σ, px of each level (cut at 2σ).
    pub win_sigma: f64,
    /// Tikhonov term on the 2×2 system: a patch without texture keeps its coarser estimate.
    pub lambda: f32,
    /// Largest step per iteration, px.
    pub max_step: f32,
    /// On the coarsest level, patches without texture are filled from the textured ones (σ = this × grid size);
    /// 0 = off.
    pub fill_sigma: f64,
}

impl Default for Params {
    fn default() -> Self {
        Params { levels: 5, min_size: 32, stride: 4, iters: 8, win_sigma: 3.0, lambda: 1.0, max_step: 1.0, fill_sigma: 0.25 }
    }
}

/// A motion field on the FLOW_N×FLOW_N grid covering the z6 block.
#[derive(Clone, Debug, PartialEq)]
pub struct Flow {
    pub z: u32,
    pub x0: i64,
    pub y0: i64,
    pub w: i64,
    pub n: usize,
    pub dx: Vec<f32>,
    pub dy: Vec<f32>,
}

impl Flow {
    /// `flow.json` as Python writes it: compact, keys z, x0, y0, w, n, dx, dy, dt; values `round(float(v), 1)`.
    pub fn to_json(&self, dt: i64) -> String {
        let r = |v: &[f32]| -> Vec<serde_json::Value> {
            v.iter().map(|x| aetherrv_core::num::jf(aetherrv_core::num::round(f64::from(*x), 1))).collect()
        };
        serde_json::json!({"z": self.z, "x0": self.x0, "y0": self.y0, "w": self.w, "n": self.n,
                           "dx": r(&self.dx), "dy": r(&self.dy), "dt": dt})
        .to_string()
    }
}

/// A single-channel f32 image, row-major.
#[derive(Clone, Debug, PartialEq)]
pub struct Img {
    pub w: usize,
    pub h: usize,
    pub d: Vec<f32>,
}

impl Img {
    pub fn new(w: usize, h: usize) -> Img {
        Img { w, h, d: vec![0.0; w * h] }
    }
    #[inline]
    pub fn at(&self, x: usize, y: usize) -> f32 {
        self.d[y * self.w + x]
    }
    fn scale(mut self, k: f32) -> Img {
        self.d.iter_mut().for_each(|v| *v *= k);
        self
    }
}

// ── the frame block on disk ─────────────────────────────────────────────────────────────────────────────────────

fn int_name(s: &str) -> anyhow::Result<i64> {
    s.parse().map_err(|_| anyhow::anyhow!("invalid literal for int() with base 10: '{s}'"))
}

/// The top-left tile of the frame's block at zoom `z` (smallest x directory, smallest y in it), or None.
pub fn origin(d: &Path, z: u32) -> anyhow::Result<Option<(i64, i64)>> {
    let zd = d.join(z.to_string());
    if !zd.is_dir() {
        return Ok(None);
    }
    let mut xs = Vec::new();
    for e in std::fs::read_dir(&zd)? {
        xs.push(int_name(&e?.file_name().to_string_lossy())?);
    }
    xs.sort_unstable();
    let Some(&x0) = xs.first() else { return Ok(None) };
    let mut ys = Vec::new();
    for e in std::fs::read_dir(zd.join(x0.to_string()))? {
        let name = e?.file_name().to_string_lossy().into_owned();
        let stem = match name.rfind('.') {
            Some(i) if i > 0 => name[..i].to_string(),
            _ => name,
        };
        ys.push(int_name(&stem)?);
    }
    ys.sort_unstable();
    Ok(ys.first().map(|&y0| (x0, y0)))
}

/// A tile's pixels as (w, h, palette rank per pixel: 0 = transparent, 1-15 = NEXRAD colour).
///
/// Accepts what OpenCV's `IMREAD_UNCHANGED` turns into 4 channels (RGBA, grey+alpha, RGB or palette with a tRNS
/// chunk); anything else is "unreadable tile", as in Python.
pub fn tile_ranks(bytes: &[u8]) -> anyhow::Result<(usize, usize, Vec<u8>)> {
    use png::{BitDepth, ColorType};
    let unreadable = || anyhow::anyhow!("unreadable tile");
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND);
    let mut rd = dec.read_info().map_err(|_| unreadable())?;
    let (src, trns) = (rd.info().color_type, rd.info().trns.is_some());
    let four = match src {
        ColorType::Rgba | ColorType::GrayscaleAlpha => true,
        ColorType::Rgb | ColorType::Indexed => trns,
        ColorType::Grayscale => false,
    };
    if !four {
        bail!(unreadable());
    }
    let mut buf = vec![0; rd.output_buffer_size().ok_or_else(unreadable)?];
    let fi = rd.next_frame(&mut buf).map_err(|_| unreadable())?;
    let (ct, depth) = rd.output_color_type();
    let (w, h) = (fi.width as usize, fi.height as usize);
    let bps = if depth == BitDepth::Sixteen { 2 } else { 1 };
    let ch = match ct {
        ColorType::Rgba => 4,
        ColorType::GrayscaleAlpha => 2,
        _ => bail!(unreadable()),
    };
    let sample = |px: &[u8], c: usize| -> i64 {
        if bps == 2 { i64::from(u16::from_be_bytes([px[2 * c], px[2 * c + 1]])) } else { i64::from(px[c]) }
    };
    let mut out = vec![0u8; w * h];
    let mut last: Option<([i64; 3], u8)> = None;
    for (i, px) in buf[..fi.buffer_size()].chunks_exact(ch * bps).enumerate().take(w * h) {
        let (rgb, a) = if ch == 4 {
            ([sample(px, 0), sample(px, 1), sample(px, 2)], sample(px, 3))
        } else {
            let g = sample(px, 0);
            ([g, g, g], sample(px, 1))
        };
        if a <= 0 {
            continue;
        }
        out[i] = match last {
            Some((c, r)) if c == rgb => r,
            _ => {
                let r = nearest(rgb) + 1;
                last = Some((rgb, r));
                r
            }
        };
    }
    Ok((w, h, out))
}

/// Index of the nearest NEXRAD colour (squared distance; the first of equals, like `np.argmin`).
pub fn nearest(rgb: [i64; 3]) -> u8 {
    let mut best = (i64::MAX, 0u8);
    for (k, p) in NEXRAD.iter().enumerate() {
        let d: i64 = (0..3).map(|c| (rgb[c] - i64::from(p[c])).pow(2)).sum();
        if d < best.0 {
            best = (d, k as u8);
        }
    }
    best.1
}

/// The preprocessed intensity image of a frame's k×k block at `o` (Python `intensity()`).
pub fn intensity(d: &Path, o: (i64, i64), k: i64) -> anyhow::Result<Img> {
    let mut tiles = Vec::new();
    let mut size = None;
    for j in 0..k {
        for i in 0..k {
            let p = d.join(FLOW_Z.to_string()).join((o.0 + i).to_string()).join(format!("{}.png", o.1 + j));
            let bytes = std::fs::read(&p).map_err(|_| anyhow::anyhow!("unreadable tile"))?;
            let (w, h, r) = tile_ranks(&bytes)?;
            if *size.get_or_insert((w, h)) != (w, h) {
                bail!("tile sizes differ");
            }
            tiles.push(r);
        }
    }
    let (tw, th) = size.context("empty block")?;
    let k = k as usize;
    let (w, h) = (tw * k, th * k);
    let mut full = Img::new(w, h);
    for j in 0..k {
        for i in 0..k {
            let t = &tiles[j * k + i];
            for y in 0..th {
                let row = &mut full.d[(j * th + y) * w + i * tw..][..tw];
                for (dst, r) in row.iter_mut().zip(&t[y * tw..(y + 1) * tw]) {
                    *dst = f32::from(*r) * 16.0;
                }
            }
        }
    }
    let half = |n: usize| ((n as f64) * 0.5).round_ties_even() as usize;
    let mut small = resize_area(&full, half(w), half(h));
    small.d.iter_mut().for_each(|v| *v = v.clamp(0.0, 255.0).trunc()); // np.clip(…).astype(np.uint8)
    let mut g = gaussian_blur(&small, 2.0, ksize_for(2.0, true));
    g.d.iter_mut().for_each(|v| *v = (*v + 0.5).floor().clamp(0.0, 255.0)); // 8-bit result
    Ok(g)
}

// ── image operations (OpenCV semantics where Python relies on them) ─────────────────────────────────────────────

/// OpenCV `BORDER_REFLECT_101`: …cb|abcd|cb…
#[inline]
pub fn reflect101(i: isize, n: usize) -> usize {
    if n == 1 {
        return 0;
    }
    let n = n as isize;
    let period = 2 * (n - 1);
    let i = i.rem_euclid(period);
    (if i >= n { period - i } else { i }) as usize
}

/// OpenCV's automatic Gaussian kernel size for `ksize = (0, 0)`: `cvRound(σ·(8-bit ? 3 : 4)·2 + 1) | 1`.
pub fn ksize_for(sigma: f64, eight_bit: bool) -> usize {
    let f = if eight_bit { 3.0 } else { 4.0 };
    ((sigma * f * 2.0 + 1.0).round_ties_even() as usize) | 1
}

pub fn gaussian_kernel(sigma: f64, ksize: usize) -> Vec<f32> {
    let r = (ksize / 2) as f64;
    let k: Vec<f64> = (0..ksize).map(|i| (-(i as f64 - r).powi(2) / (2.0 * sigma * sigma)).exp()).collect();
    let s: f64 = k.iter().sum();
    k.iter().map(|v| (v / s) as f32).collect()
}

/// Separable convolution with a symmetric kernel, reflect-101 borders.
pub fn convolve(img: &Img, k: &[f32]) -> Img {
    let (w, h) = (img.w, img.h);
    let r = k.len() / 2;
    // horizontal
    let mut tmp = Img::new(w, h);
    let mut pad = vec![0f32; w + 2 * r];
    for y in 0..h {
        let src = &img.d[y * w..(y + 1) * w];
        for (i, p) in pad.iter_mut().enumerate() {
            *p = src[reflect101(i as isize - r as isize, w)];
        }
        let dst = &mut tmp.d[y * w..(y + 1) * w];
        for (x, o) in dst.iter_mut().enumerate() {
            let win = &pad[x..x + k.len()];
            *o = win.iter().zip(k).map(|(a, b)| a * b).sum();
        }
    }
    // vertical: whole rows at a time (vectorises)
    let mut out = Img::new(w, h);
    for y in 0..h {
        let dst = &mut out.d[y * w..(y + 1) * w];
        for (i, kv) in k.iter().enumerate() {
            let sy = reflect101(y as isize + i as isize - r as isize, h);
            let src = &tmp.d[sy * w..(sy + 1) * w];
            for (o, s) in dst.iter_mut().zip(src) {
                *o += kv * s;
            }
        }
    }
    out
}

pub fn gaussian_blur(img: &Img, sigma: f64, ksize: usize) -> Img {
    convolve(img, &gaussian_kernel(sigma, ksize))
}

/// Area-average weights from `n` source pixels to `dn` (OpenCV `INTER_AREA` for a downscale): row o lists
/// (source index, weight).
fn area_weights(n: usize, dn: usize) -> Vec<Vec<(usize, f32)>> {
    let s = n as f64 / dn as f64;
    (0..dn)
        .map(|o| {
            let (a, b) = (o as f64 * s, (o as f64 + 1.0) * s);
            let mut v = Vec::new();
            let mut i = a.floor() as usize;
            while (i as f64) < b && i < n {
                let ov = (b.min(i as f64 + 1.0) - a.max(i as f64)).max(0.0);
                if ov > 1e-9 {
                    v.push((i, (ov / s) as f32));
                }
                i += 1;
            }
            v
        })
        .collect()
}

/// Downscale by area averaging (exact box average for integer factors).
pub fn resize_area(img: &Img, dw: usize, dh: usize) -> Img {
    let (wx, wy) = (area_weights(img.w, dw), area_weights(img.h, dh));
    let mut tmp = Img::new(dw, img.h);
    for y in 0..img.h {
        let src = &img.d[y * img.w..(y + 1) * img.w];
        for (x, ws) in wx.iter().enumerate() {
            tmp.d[y * dw + x] = ws.iter().map(|(i, k)| src[*i] * k).sum();
        }
    }
    let mut out = Img::new(dw, dh);
    for (y, ws) in wy.iter().enumerate() {
        let dst = &mut out.d[y * dw..(y + 1) * dw];
        for (sy, k) in ws {
            for (o, s) in dst.iter_mut().zip(&tmp.d[sy * dw..(sy + 1) * dw]) {
                *o += k * s;
            }
        }
    }
    out
}

/// OpenCV `pyrDown`: 5-tap binomial blur, every second pixel, size ⌈n/2⌉.
pub fn pyr_down(img: &Img) -> Img {
    let b = convolve(img, &[1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0]);
    let (w, h) = (img.w.div_ceil(2), img.h.div_ceil(2));
    let mut out = Img::new(w, h);
    for y in 0..h {
        for x in 0..w {
            out.d[y * w + x] = b.at(2 * x, 2 * y);
        }
    }
    out
}

/// Bilinear sample, coordinates clamped to the image (replicated border).
#[inline]
fn sample(img: &Img, x: f32, y: f32) -> f32 {
    let x = x.clamp(0.0, (img.w - 1) as f32);
    let y = y.clamp(0.0, (img.h - 1) as f32);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(img.w - 1), (y0 + 1).min(img.h - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let top = img.at(x0, y0) + (img.at(x1, y0) - img.at(x0, y0)) * fx;
    let bot = img.at(x0, y1) + (img.at(x1, y1) - img.at(x0, y1)) * fx;
    top + (bot - top) * fy
}

/// Resample to (w, h) bilinearly (pixel centres aligned), multiplying the values by `k`.
fn resize_linear(img: &Img, w: usize, h: usize, k: f32) -> Img {
    let (sx, sy) = (img.w as f32 / w as f32, img.h as f32 / h as f32);
    let mut out = Img::new(w, h);
    for y in 0..h {
        for x in 0..w {
            out.d[y * w + x] = k * sample(img, (x as f32 + 0.5) * sx - 0.5, (y as f32 + 0.5) * sy - 0.5);
        }
    }
    out
}

/// Central-difference gradients (one-sided at the border).
fn gradients(a: &Img) -> (Img, Img) {
    let (w, h) = (a.w, a.h);
    let (mut gx, mut gy) = (Img::new(w, h), Img::new(w, h));
    for y in 0..h {
        let (ym, yp) = (y.saturating_sub(1), (y + 1).min(h - 1));
        for x in 0..w {
            let (xm, xp) = (x.saturating_sub(1), (x + 1).min(w - 1));
            gx.d[y * w + x] = (a.at(xp, y) - a.at(xm, y)) / (xp - xm).max(1) as f32;
            gy.d[y * w + x] = (a.at(x, yp) - a.at(x, ym)) / (yp - ym).max(1) as f32;
        }
    }
    (gx, gy)
}

/// Component-wise 3×3 median of a grid field (drops single-patch outliers before the next level).
fn median3(g: &Img) -> Img {
    let mut out = g.clone();
    let mut win = [0f32; 9];
    for y in 0..g.h {
        for x in 0..g.w {
            let mut n = 0;
            for dy in -1isize..=1 {
                for dx in -1isize..=1 {
                    let (sx, sy) = (x as isize + dx, y as isize + dy);
                    if sx >= 0 && sy >= 0 && (sx as usize) < g.w && (sy as usize) < g.h {
                        win[n] = g.at(sx as usize, sy as usize);
                        n += 1;
                    }
                }
            }
            let w = &mut win[..n];
            w.sort_unstable_by(f32::total_cmp);
            out.d[y * g.w + x] = if n % 2 == 1 { w[n / 2] } else { 0.5 * (w[n / 2 - 1] + w[n / 2]) };
        }
    }
    out
}

/// One pyramid level of the inverse search: every patch of the grid refines its own translation (Gauss–Newton,
/// inverse compositional, so A's gradients and Hessian are computed once per patch). Returns which patches had
/// texture (1) and which kept their initial value (0).
fn search_level(a: &Img, b: &Img, gu: &mut Img, gv: &mut Img, p: &Params) -> Img {
    let mut conf = Img::new(gu.w, gu.h);
    let (gx, gy) = gradients(a);
    let r = (2.0 * p.win_sigma).ceil() as isize;
    let side = (2 * r + 1) as usize;
    let mut wts = Vec::with_capacity(side * side);
    for dy in -r..=r {
        for dx in -r..=r {
            wts.push((-((dx * dx + dy * dy) as f64) / (2.0 * p.win_sigma * p.win_sigma)).exp() as f32);
        }
    }
    let (gw, gh) = (gu.w, gu.h);
    // per patch: sample offsets, weights (normalised over the in-image part), A, centred gradients
    let mut q: Vec<(f32, f32, f32, f32, f32, f32)> = Vec::with_capacity(side * side); // x, y, w, A, ĝx, ĝy
    for j in 0..gh {
        let cy = (((j as f32 + 0.5) * a.h as f32 / gh as f32) - 0.5).round() as isize;
        for i in 0..gw {
            let cx = (((i as f32 + 0.5) * a.w as f32 / gw as f32) - 0.5).round() as isize;
            q.clear();
            let (mut sw, mut mx, mut my) = (0f32, 0f32, 0f32);
            let mut k = 0;
            for dy in -r..=r {
                for dx in -r..=r {
                    let (x, y) = (cx + dx, cy + dy);
                    let w = wts[k];
                    k += 1;
                    if x < 0 || y < 0 || x as usize >= a.w || y as usize >= a.h {
                        continue;
                    }
                    let (x, y) = (x as usize, y as usize);
                    let (ex, ey) = (gx.at(x, y), gy.at(x, y));
                    q.push((x as f32, y as f32, w, a.at(x, y), ex, ey));
                    sw += w;
                    mx += w * ex;
                    my += w * ey;
                }
            }
            if sw <= 0.0 {
                continue;
            }
            let (mx, my) = (mx / sw, my / sw);
            let (mut hxx, mut hxy, mut hyy) = (0f32, 0f32, 0f32);
            for e in q.iter_mut() {
                e.2 /= sw;
                e.4 -= mx; // centred: the patch's brightness offset drops out of the fit
                e.5 -= my;
                hxx += e.2 * e.4 * e.4;
                hxy += e.2 * e.4 * e.5;
                hyy += e.2 * e.5 * e.5;
            }
            let c = j * gw + i;
            if hxx + hyy <= p.lambda {
                continue; // no texture (no echo): nothing to fit
            }
            conf.d[c] = 1.0;
            let (hxx, hyy) = (hxx + p.lambda, hyy + p.lambda);
            let det = hxx * hyy - hxy * hxy;
            if det.is_nan() || det <= 1e-6 {
                continue;
            }
            let (mut u, mut v) = (gu.d[c], gv.d[c]);
            for _ in 0..p.iters {
                let (mut bx, mut by) = (0f32, 0f32);
                for &(x, y, w, av, ex, ey) in &q {
                    let res = sample(b, x + u, y + v) - av;
                    bx += w * ex * res;
                    by += w * ey * res;
                }
                let du = ((hyy * bx - hxy * by) / det).clamp(-p.max_step, p.max_step);
                let dv = ((hxx * by - hxy * bx) / det).clamp(-p.max_step, p.max_step);
                if !(du.is_finite() && dv.is_finite()) {
                    break;
                }
                u -= du;
                v -= dv;
                if du.abs() < 0.01 && dv.abs() < 0.01 {
                    break;
                }
            }
            gu.d[c] = u;
            gv.d[c] = v;
        }
    }
    conf
}

/// Patches without texture take the confidence-weighted mean of the textured ones around them (normalised
/// convolution, Gaussian σ in grid cells), so motion carries into the empty area ahead of an echo.
fn fill(gu: &mut Img, gv: &mut Img, conf: &Img, sigma: f64) {
    let k = gaussian_kernel(sigma, 2 * (3.0 * sigma).ceil() as usize + 1);
    let den = convolve(conf, &k);
    let weighted = |g: &Img| Img { w: g.w, h: g.h, d: g.d.iter().zip(&conf.d).map(|(a, c)| a * c).collect() };
    let (nu, nv) = (convolve(&weighted(gu), &k), convolve(&weighted(gv), &k));
    for i in 0..gu.d.len() {
        if conf.d[i] == 0.0 && den.d[i] > 1e-4 {
            gu.d[i] = nu.d[i] / den.d[i];
            gv.d[i] = nv.d[i] / den.d[i];
        }
    }
}

/// Coarse-to-fine inverse search on a patch grid: the dense field (u, v) with A(p) ≈ B(p + (u, v)), in pixels of
/// A.
pub fn dense_flow(a: &Img, b: &Img, p: &Params) -> (Img, Img) {
    let mut pa = vec![a.clone()];
    let mut pb = vec![b.clone()];
    while pa.len() < p.levels {
        let l = pa.last().unwrap();
        if l.w.div_ceil(2) < p.min_size || l.h.div_ceil(2) < p.min_size {
            break;
        }
        let (na, nb) = (pyr_down(l), pyr_down(pb.last().unwrap()));
        pa.push(na);
        pb.push(nb);
    }
    let grid = |img: &Img| (img.w.div_ceil(p.stride).max(1), img.h.div_ceil(p.stride).max(1));
    let (tw, th) = grid(pa.last().unwrap());
    let (mut gu, mut gv) = (Img::new(tw, th), Img::new(tw, th));
    let mut prev = (pa.last().unwrap().w, pa.last().unwrap().h);
    for lvl in (0..pa.len()).rev() {
        let (ia, ib) = (&pa[lvl], &pb[lvl]);
        let (w, h) = grid(ia);
        if (w, h) != (gu.w, gu.h) {
            let (kx, ky) = (ia.w as f32 / prev.0 as f32, ia.h as f32 / prev.1 as f32);
            gu = resize_linear(&gu, w, h, kx);
            gv = resize_linear(&gv, w, h, ky);
        }
        let conf = search_level(ia, ib, &mut gu, &mut gv, p);
        if lvl == pa.len() - 1 && p.fill_sigma > 0.0 {
            let sigma = p.fill_sigma * gu.w.max(gu.h) as f64;
            fill(&mut gu, &mut gv, &conf, sigma);
        }
        gu = median3(&gu);
        gv = median3(&gv);
        prev = (ia.w, ia.h);
    }
    (resize_linear(&gu, a.w, a.h, 1.0), resize_linear(&gv, a.w, a.h, 1.0))
}

/// Python `flow_pair`: the coarse, smooth motion field from frame A to frame B (blocking; run it off the async
/// runtime). None when the two frames don't cover the same block (the van moved between them).
pub fn flow_pair(da: &Path, db: &Path) -> anyhow::Result<Option<Flow>> {
    flow_pair_with(da, db, &Params::default())
}

pub fn flow_pair_with(da: &Path, db: &Path, p: &Params) -> anyhow::Result<Option<Flow>> {
    let (oa, ob) = (origin(da, FLOW_Z)?, origin(db, FLOW_Z)?);
    let Some(o) = oa else { return Ok(None) };
    if ob != Some(o) {
        return Ok(None);
    }
    let k = crate::radar::block_size(FLOW_Z).unwrap_or(3);
    let (ga, gb) = (intensity(da, o, k)?, intensity(db, o, k)?);
    if ga.w != gb.w || ga.h != gb.h {
        bail!("frame sizes differ");
    }
    let (u, v) = dense_flow(&ga, &gb, p);
    Ok(Some(field_to_grid(&u, &v, o, k)))
}

/// Python's post-processing: Gaussian σ = 12 on the full field (coarse and smooth: per-pixel flow tears echoes),
/// area-averaged to FLOW_N×FLOW_N, ×2 (half-res px → z6 px).
pub fn field_to_grid(u: &Img, v: &Img, o: (i64, i64), k: i64) -> Flow {
    let ks = ksize_for(12.0, false);
    let g = |f: &Img| resize_area(&gaussian_blur(f, 12.0, ks), FLOW_N, FLOW_N).scale(2.0).d;
    Flow { z: FLOW_Z, x0: o.0, y0: o.1, w: k, n: FLOW_N, dx: g(u), dy: g(v) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opencv_details() {
        assert_eq!(ksize_for(2.0, true), 13);
        assert_eq!(ksize_for(12.0, false), 97);
        assert_eq!(ksize_for(6.0, false), 49);
        let r: Vec<usize> = (-3..8).map(|i| reflect101(i, 5)).collect();
        assert_eq!(r, vec![3, 2, 1, 0, 1, 2, 3, 4, 3, 2, 1]);
        assert_eq!(reflect101(-1, 1), 0);
        let k = gaussian_kernel(2.0, 13);
        assert!((k.iter().sum::<f32>() - 1.0).abs() < 1e-6 && k[6] > k[5] && k[0] == k[12]);
    }

    #[test]
    fn area_resize_is_a_box_average() {
        let img = Img { w: 4, h: 2, d: vec![0.0, 2.0, 4.0, 6.0, 8.0, 10.0, 12.0, 14.0] };
        assert_eq!(resize_area(&img, 2, 1).d, vec![5.0, 9.0]);
        assert_eq!(resize_area(&img, 1, 1).d, vec![7.0]);
        let r = resize_area(&Img { w: 3, h: 1, d: vec![0.0, 3.0, 6.0] }, 2, 1).d;
        assert!((r[0] - 1.0).abs() < 1e-6 && (r[1] - 5.0).abs() < 1e-6, "{r:?}");
    }

    #[test]
    fn palette_rank() {
        assert_eq!(nearest([4, 233, 231]), 0);
        assert_eq!(nearest([253, 253, 253]), 14);
        assert_eq!(nearest([250, 1, 1]), 9);
    }

    #[test]
    fn lk_finds_a_subpixel_shift() {
        // smooth texture, B(p) = A(p - (2.5, -1.25))
        let f = |x: f32, y: f32| 100.0 + 60.0 * (x / 9.0).sin() * (y / 7.0).cos() + 40.0 * ((x + 2.0 * y) / 13.0).sin();
        let (w, h) = (128, 128);
        let mut a = Img::new(w, h);
        let mut b = Img::new(w, h);
        for y in 0..h {
            for x in 0..w {
                a.d[y * w + x] = f(x as f32, y as f32);
                b.d[y * w + x] = f(x as f32 - 2.5, y as f32 + 1.25);
            }
        }
        let (u, v) = dense_flow(&a, &b, &Params::default());
        let c = (h / 2) * w + w / 2;
        assert!((u.d[c] - 2.5).abs() < 0.1 && (v.d[c] + 1.25).abs() < 0.1, "{} {}", u.d[c], v.d[c]);
    }
}

