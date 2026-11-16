//! The Visualizer tab: a big picture of the music, drawn in pixels.
//!
//! - **Spectrum**: the header's bars, full size, with peak caps.
//! - **Scope**: a goniometer (vectorscope). Left against right, turned so
//!   mono is a vertical line and wide stereo spreads sideways; the trace
//!   fades like a phosphor screen.
//! - **Waterfall**: a spectrogram scrolling right to left, bass at the
//!   bottom, loudness as colour.
//! - **Radial**: the spectrum bent round a circle, bass at the top, with
//!   the album cover in the middle pulsing to the bass.
//!
//! Colours come from the theme's `visualizer_colors`, or from the cover.
//! Each frame is drawn into an RGBA canvas; kitty gets it through shared
//! memory at a modest resolution and scales it on the GPU (`kitty.rs`),
//! other terminals get it through notcurses in character cells. Nothing
//! here runs, and its buffers are dropped, while the tab isn't showing.

use std::f32::consts::{SQRT_2, TAU};
use std::time::Instant;

use anyhow::Result;

use crate::art::Rect;
use crate::config::VisualizerMirror;
use crate::kitty::KittyImage;
use crate::nc::{Nc, OwnedPlane, Visual};
use crate::textart::Image;
use crate::ui::{gradient, scale};
use crate::viz::{Bars, Visualizer};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Spectrum,
    Scope,
    Waterfall,
    Radial,
}

impl Mode {
    pub const ALL: [Mode; 4] = [Mode::Spectrum, Mode::Scope, Mode::Waterfall, Mode::Radial];

    pub fn name(self) -> &'static str {
        match self {
            Mode::Spectrum => "Spectrum",
            Mode::Scope => "Scope",
            Mode::Waterfall => "Waterfall",
            Mode::Radial => "Radial",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name().eq_ignore_ascii_case(s))
    }

    /// The next (or previous) mode, wrapping.
    pub fn step(self, by: isize) -> Self {
        let i = Self::ALL.iter().position(|&m| m == self).unwrap_or(0) as isize;
        Self::ALL[(i + by).rem_euclid(Self::ALL.len() as isize) as usize]
    }
}

/// kitty image id, well away from the ones notcurses numbers from 1.
const KITTY_ID: u32 = 0x6e6d_7063;
/// Most pixels a kitty canvas has (kitty scales it up to the cells).
const KITTY_MAX_PX: f32 = 360_000.0;
/// Seconds of history the waterfall shows across its width.
const WATERFALL_SECS: f32 = 12.0;
/// Bands round the radial spectrum's half circle.
const RADIAL_BANDS: usize = 48;

struct Canvas {
    w: u32,
    h: u32,
    rgba: Vec<u8>,
    /// Scope: the trace's brightness per pixel.
    heat: Vec<f32>,
    /// Radial: each pixel's (band or `u16::MAX` between bars, radius).
    polar: Vec<(u16, f32)>,
}

impl Canvas {
    fn new(w: u32, h: u32) -> Self {
        Self { w, h, rgba: vec![0; (w * h * 4) as usize], heat: Vec::new(), polar: Vec::new() }
    }

    fn clear(&mut self) {
        self.rgba.fill(0);
    }

    /// Pixel `i` in `rgb` at opacity `a` (0..1).
    fn set(&mut self, i: usize, rgb: u32, a: f32) {
        let a = (a.clamp(0.0, 1.0) * 255.0) as u8;
        self.rgba[i * 4..i * 4 + 4].copy_from_slice(&[(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, a]);
    }
}

/// What the theme gives a scene to draw with.
pub struct Look<'a> {
    pub colors: &'a [u32],
    pub mirror: VisualizerMirror,
    pub peaks: bool,
    pub fps: u32,
}

pub struct Scene<'nc> {
    nc: &'nc Nc,
    pub mode: Mode,
    /// Colours from the cover instead of the theme.
    pub cover_colors: bool,
    canvas: Option<Canvas>,
    bars: Bars,
    /// Scope: the recent loudest sample, for automatic gain.
    gain: f32,
    heat_left: bool,
    cover: Option<Image>,
    /// The song `cover` belongs to (fetched or not).
    cover_key: Option<String>,
    palette: Vec<u32>,
    /// Some while kitty takes shared-memory frames.
    kitty: Option<KittyImage>,
    use_kitty: bool,
    plane: Option<OwnedPlane<'nc>>,
    /// Where the picture on screen is, if one is.
    shown: Option<Rect>,
    /// The canvas must be redrawn whole (new, cleared, recoloured).
    fresh: bool,
    last: Option<Instant>,
}

impl<'nc> Scene<'nc> {
    pub fn new(nc: &'nc Nc, mode: Mode, cover_colors: bool) -> Self {
        Self {
            nc,
            mode,
            cover_colors,
            canvas: None,
            bars: Bars::default(),
            gain: 0.0,
            heat_left: false,
            cover: None,
            cover_key: None,
            palette: Vec::new(),
            kitty: None,
            use_kitty: nc.is_local_kitty(),
            plane: None,
            shown: None,
            fresh: true,
            last: None,
        }
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.restart();
    }

    pub fn set_cover_colors(&mut self, on: bool) {
        self.cover_colors = on;
        self.fresh = true;
    }

    /// Whether the cover's colours are in use (there is a cover).
    pub fn has_cover_palette(&self) -> bool {
        !self.palette.is_empty()
    }

    /// Starts the picture over (another mode, size, or colours).
    fn restart(&mut self) {
        if let Some(c) = self.canvas.as_mut() {
            c.clear();
            c.heat.clear();
            c.polar.clear();
        }
        self.bars = Bars::default();
        self.heat_left = false;
        self.fresh = true;
    }

    pub fn cover_key(&self) -> Option<&str> {
        self.cover_key.as_deref()
    }

    /// The cover for song `key` (image bytes, if it has one).
    pub fn set_cover(&mut self, key: &str, data: Option<&[u8]>) {
        self.cover_key = Some(key.to_owned());
        self.cover = data.and_then(|d| crate::art::decode(d, 256)).and_then(|v| v.image());
        self.palette = self.cover.as_ref().map(palette).unwrap_or_default();
        self.fresh = true;
    }

    /// Whether the next frame would differ from this one.
    pub fn moving(&self, live: bool) -> bool {
        live || match self.mode {
            Mode::Spectrum | Mode::Radial => self.bars.moving(),
            Mode::Scope => self.heat_left,
            Mode::Waterfall => false,
        }
    }

    /// Drops the picture and everything kept for it.
    pub fn hide(&mut self) {
        if let Some(k) = self.kitty.as_mut() {
            k.hide();
        }
        self.plane = None;
        self.canvas = None;
        self.shown = None;
        self.bars = Bars::default();
        self.heat_left = false;
        self.fresh = true;
        self.last = None;
    }

    /// The terminal dropped its images (a screen clear): send it again.
    pub fn forget(&mut self) {
        self.shown = None;
    }

    /// Draws the next frame over `area`. Returns whether the screen changed.
    pub fn draw(&mut self, viz: &mut Visualizer, area: Rect, look: &Look) -> Result<bool> {
        let (w, h) = self.canvas_size(area);
        if self.canvas.as_ref().is_none_or(|c| (c.w, c.h) != (w, h)) {
            self.canvas = Some(Canvas::new(w, h));
            self.restart();
        }
        let dt = self.last.map_or(0.05, |t| t.elapsed().as_secs_f32().min(0.25));
        self.last = Some(Instant::now());
        let live = viz.live();
        let stops: Vec<u32> = match &self.palette {
            p if self.cover_colors && !p.is_empty() => p.clone(),
            _ => look.colors.to_vec(),
        };
        let fresh = std::mem::take(&mut self.fresh);
        let mut c = self.canvas.take().expect("made above");
        let changed = match self.mode {
            Mode::Spectrum => {
                let moving = live || self.bars.moving();
                if moving || fresh {
                    let bands = if look.mirror.horizontal() { bar_count(area).div_ceil(2) } else { bar_count(area) };
                    self.bars.update(&viz.levels(bands));
                    spectrum(&mut c, &self.bars, bar_count(area), &stops, look);
                }
                moving || fresh
            }
            Mode::Scope => {
                let decay = 0.8f32.powf(dt * 20.0);
                if c.heat.len() != c.rgba.len() / 4 {
                    c.heat = vec![0.0; c.rgba.len() / 4];
                }
                let changed = live || self.heat_left || fresh;
                if changed {
                    let samples = if live { viz.recent((44100.0 * dt * 1.5) as usize) } else { Vec::new() };
                    self.heat_left = scope(&mut c, &samples, decay, &mut self.gain, &stops);
                }
                changed
            }
            Mode::Waterfall => {
                if fresh {
                    c.clear();
                }
                if live {
                    let step = ((c.w as f32 / (look.fps as f32 * WATERFALL_SECS)).ceil() as u32).max(1);
                    let levels = viz.levels(c.h as usize);
                    waterfall(&mut c, &levels, step, &stops);
                }
                live || fresh
            }
            Mode::Radial => {
                let moving = live || self.bars.moving();
                if moving || fresh {
                    self.bars.update(&viz.levels(RADIAL_BANDS));
                    radial(&mut c, &self.bars, self.cover.as_ref(), &stops, look.peaks);
                }
                moving || fresh
            }
        };
        let resend = changed || self.shown != Some(area);
        if resend {
            self.present(&c, area)?;
        }
        self.canvas = Some(c);
        Ok(resend)
    }

    /// The canvas for `area`: kitty's scaled to at most `KITTY_MAX_PX`
    /// (keeping the screen's shape, so circles stay round); otherwise
    /// about the dots character cells can show.
    fn canvas_size(&self, area: Rect) -> (u32, u32) {
        if self.use_kitty {
            let (ch, cw) = self.nc.cell_px().unwrap_or((20, 10));
            let (fw, fh) = ((area.cols * cw) as f32, (area.rows * ch) as f32);
            let k = (KITTY_MAX_PX / (fw * fh)).sqrt().min(1.0);
            (((fw * k) as u32).max(8), ((fh * k) as u32).max(8))
        } else {
            (area.cols * 2, area.rows * 4)
        }
    }

    fn present(&mut self, c: &Canvas, area: Rect) -> Result<()> {
        if self.use_kitty {
            let k = self.kitty.get_or_insert_with(|| KittyImage::new(KITTY_ID));
            if let Err(e) = k.show(&c.rgba, c.w, c.h, area.y, area.x, area.rows, area.cols) {
                // character cells from now on
                k.hide();
                self.kitty = None;
                self.use_kitty = false;
                self.canvas = None;
                self.shown = None;
                return Err(e);
            }
        } else {
            if self.plane.as_ref().is_none_or(|p| p.dim_yx() != (area.rows, area.cols)) || self.shown != Some(area) {
                self.plane = None;
                self.plane = Some(self.nc.new_plane(area.y, area.x, area.rows, area.cols)?);
            }
            let v = Visual::from_rgba(&c.rgba, c.h, c.w)?;
            let plane = self.plane.as_mut().expect("made above");
            plane.erase();
            self.nc.blit_stretched(&v, plane)?;
        }
        self.shown = Some(area);
        Ok(())
    }
}

/// Bars across the spectrum: about one per cell.
fn bar_count(area: Rect) -> usize {
    area.cols.clamp(8, 160) as usize
}

fn spectrum(c: &mut Canvas, bars: &Bars, n: usize, stops: &[u32], look: &Look) {
    c.clear();
    let (w, h) = (c.w as f32, c.h as f32);
    let horizontal = look.mirror.horizontal();
    let bands = bars.bars.len();
    if bands == 0 {
        return;
    }
    let band = |i: usize| {
        if !horizontal {
            i
        } else if i >= n / 2 {
            i - n / 2
        } else {
            (n - 1) / 2 - i
        }
    };
    let bw = w / n as f32;
    let gap = if bw >= 3.0 { (bw * 0.25).max(1.0) } else { 0.0 };
    let cap_h = (h / 120.0).max(1.0);
    let (base, span) = if look.mirror.vertical() { (h / 2.0, h / 2.0) } else { (h, h) };
    for i in 0..n {
        let b = band(i).min(bands - 1);
        let (bar, peak) = (bars.bars[b], bars.peaks[b]);
        let t = if horizontal { b as f32 / (bands.max(2) - 1) as f32 } else { i as f32 / (n.max(2) - 1) as f32 };
        let rgb = gradient(stops, t);
        let (x0, x1) = ((i as f32 * bw) as u32, (((i + 1) as f32 * bw - gap) as u32).max((i as f32 * bw) as u32 + 1));
        let len = bar * span;
        let cap = peak * span;
        for x in x0..x1.min(c.w) {
            for d in 0..span as u32 {
                let d_f = d as f32;
                let on_bar = d_f < len;
                let on_cap = look.peaks && d_f >= cap && d_f < cap + cap_h && cap > len + 1.0;
                if !on_bar && !on_cap {
                    continue;
                }
                let (col, a) = if on_cap {
                    (scale(rgb, 1.25), 1.0)
                } else {
                    // brighter toward the bar's tip
                    (scale(rgb, 0.45 + 0.55 * d_f / span), 0.95)
                };
                let up = base - 1.0 - d_f;
                if up >= 0.0 {
                    c.set((up as u32 * c.w + x) as usize, col, a);
                }
                if look.mirror.vertical() {
                    let down = base + d_f;
                    if down < h {
                        c.set((down as u32 * c.w + x) as usize, col, a);
                    }
                }
            }
        }
    }
}

/// Fades the trace, draws `samples` into it, and colours the canvas.
/// Returns whether any trace is left to fade.
fn scope(c: &mut Canvas, samples: &[[f32; 2]], decay: f32, gain: &mut f32, stops: &[u32]) -> bool {
    for v in c.heat.iter_mut() {
        *v *= decay;
    }
    let (w, h) = (c.w as f32, c.h as f32);
    let (cx, cy) = (w / 2.0, h / 2.0);
    // mid (L+R) up, side (R−L) across: mono is a vertical line
    let points: Vec<(f32, f32)> = samples.iter().map(|&[l, r]| ((r - l) / SQRT_2, (l + r) / SQRT_2)).collect();
    let loudest = points.iter().fold(0.0f32, |m, &(x, y)| m.max(x.abs()).max(y.abs()));
    *gain = (*gain * 0.98).max(loudest).max(0.02);
    let k = 0.92 * cx.min(cy) / *gain;
    let mut prev: Option<(f32, f32)> = None;
    for &(sx, sy) in &points {
        let p = (cx + sx * k, cy - sy * k);
        if let Some(q) = prev {
            line(&mut c.heat, c.w, c.h, q, p, 0.9);
        }
        prev = Some(p);
    }
    let mut left = false;
    for i in 0..c.heat.len() {
        let v = c.heat[i];
        if v < 0.004 {
            c.heat[i] = 0.0;
            c.rgba[i * 4 + 3] = 0;
            continue;
        }
        left = true;
        let t = (v / 1.5).min(1.0);
        c.set(i, scale(gradient(stops, t), 0.6 + 0.6 * t), v * 2.0);
    }
    left
}

/// Adds `amount` along the line from `a` to `b`, spread thinner the longer
/// it is (a fast-moving trace is fainter, like a real scope's beam).
fn line(heat: &mut [f32], w: u32, h: u32, a: (f32, f32), b: (f32, f32), amount: f32) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = dx.abs().max(dy.abs()).max(1.0);
    let each = amount / len.sqrt();
    let steps = len.ceil() as u32;
    for s in 0..=steps {
        let f = s as f32 / steps as f32;
        let (x, y) = (a.0 + dx * f, a.1 + dy * f);
        // the beam, and a soft edge a pixel either side
        for (ox, oy, k) in [(0.0, 0.0, 1.0), (1.0, 0.0, 0.35), (-1.0, 0.0, 0.35), (0.0, 1.0, 0.35), (0.0, -1.0, 0.35)] {
            let (x, y) = (x + ox, y + oy);
            if x >= 0.0 && y >= 0.0 && (x as u32) < w && (y as u32) < h {
                heat[(y as u32 * w + x as u32) as usize] += each * k;
            }
        }
    }
}

/// Scrolls the spectrogram `step` pixels left and paints `levels` (one per
/// row, bass first) into the new columns at the right.
fn waterfall(c: &mut Canvas, levels: &[f32], step: u32, stops: &[u32]) {
    let (w, h) = (c.w as usize, c.h as usize);
    let step = (step as usize).min(w);
    for y in 0..h {
        let row = y * w * 4;
        c.rgba.copy_within(row + step * 4..row + w * 4, row);
        let v = levels[h - 1 - y].powf(1.3);
        let rgb = scale(gradient(stops, v), 0.25 + 0.85 * v);
        for x in w - step..w {
            c.set(y * w + x, rgb, v * 1.6);
        }
    }
}

fn radial(c: &mut Canvas, bars: &Bars, cover: Option<&Image>, stops: &[u32], peaks: bool) {
    let (w, h) = (c.w, c.h);
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    if c.polar.len() != (w * h) as usize {
        c.polar = polar_map(w, h, RADIAL_BANDS);
    }
    c.clear();
    let bands = bars.bars.len();
    if bands == 0 {
        return;
    }
    let outer = cx.min(cy) * 0.96;
    let r0 = outer * 0.42;
    let len = outer - r0 - 2.0;
    let bass = bars.bars[..3.min(bands)].iter().sum::<f32>() / 3.0;
    let rc = r0 * (0.86 + 0.12 * bass) - 2.0;
    let side = cover.map_or(0.0, |img| img.w.min(img.h) as f32);
    for i in 0..(w * h) as usize {
        let (band, r) = c.polar[i];
        if r < rc {
            let edge = (rc - r).min(1.0);
            match cover {
                Some(img) => {
                    let (x, y) = ((i as u32 % w) as f32 - cx, (i as u32 / w) as f32 - cy);
                    let u = (img.w as f32 / 2.0 + x / rc * side / 2.0).clamp(0.0, img.w as f32 - 1.0) as u32;
                    let v = (img.h as f32 / 2.0 + y / rc * side / 2.0).clamp(0.0, img.h as f32 - 1.0) as u32;
                    let p = img.px[(v * img.w + u) as usize];
                    let rgb = (u32::from(p[0]) << 16) | (u32::from(p[1]) << 8) | u32::from(p[2]);
                    c.set(i, rgb, edge * f32::from(p[3]) / 255.0);
                }
                None => c.set(i, stops[0], edge * (0.15 + 0.25 * bass)),
            }
            continue;
        }
        if band == u16::MAX || r < r0 {
            continue;
        }
        let b = usize::from(band).min(bands - 1);
        let d = r - r0;
        let t = b as f32 / (bands.max(2) - 1) as f32;
        let tip = bars.bars[b] * len;
        let cap = bars.peaks[b] * len + 2.0;
        if d <= tip {
            c.set(i, scale(gradient(stops, t), 0.5 + 0.6 * d / len), 0.95);
        } else if peaks && (d - cap).abs() < 1.2 && cap > tip + 2.0 {
            c.set(i, scale(gradient(stops, t), 1.25), 1.0);
        }
    }
}

/// Each pixel's band round the circle and distance from the middle. The
/// bands run from the top down both sides (bass at the top, treble at the
/// bottom), each a sector with a gap either side.
fn polar_map(w: u32, h: u32, bands: usize) -> Vec<(u16, f32)> {
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let sectors = bands * 2;
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let r = dx.hypot(dy);
            // 0 at the top, clockwise
            let a = dx.atan2(-dy).rem_euclid(TAU);
            let s = a / TAU * sectors as f32;
            let (sector, frac) = ((s as usize).min(sectors - 1), s.fract());
            let band = if sector < bands { sector } else { sectors - 1 - sector };
            let lit = (0.18..0.82).contains(&frac);
            out.push((if lit { band as u16 } else { u16::MAX }, r));
        }
    }
    out
}

/// A cover's main colours, dark to light: its most common distinct
/// colours, favouring vivid ones, lifted so none is too dark to see.
fn palette(img: &Image) -> Vec<u32> {
    let mut count = vec![0f32; 1 << 15];
    let stride = (img.px.len() / 8192).max(1);
    for p in img.px.iter().step_by(stride) {
        if p[3] < 128 {
            continue;
        }
        let (r, g, b) = (f32::from(p[0]), f32::from(p[1]), f32::from(p[2]));
        let (hi, lo) = (r.max(g).max(b), r.min(g).min(b));
        if hi < 40.0 {
            continue;
        }
        let sat = (hi - lo) / hi;
        let key = ((p[0] as usize >> 3) << 10) | ((p[1] as usize >> 3) << 5) | (p[2] as usize >> 3);
        count[key] += 0.25 + sat;
    }
    let mut keys: Vec<usize> = (0..count.len()).filter(|&k| count[k] > 0.0).collect();
    keys.sort_by(|&a, &b| count[b].total_cmp(&count[a]));
    let rgb = |k: usize| {
        [((k >> 10) & 31) as f32 * 8.0 + 4.0, ((k >> 5) & 31) as f32 * 8.0 + 4.0, (k & 31) as f32 * 8.0 + 4.0]
    };
    let mut picked: Vec<[f32; 3]> = Vec::new();
    for k in keys {
        let c = rgb(k);
        if picked.iter().all(|p| p.iter().zip(&c).map(|(a, b)| (a - b).powi(2)).sum::<f32>() > 70.0 * 70.0) {
            picked.push(c);
            if picked.len() == 3 {
                break;
            }
        }
    }
    let lum = |c: &[f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    picked.sort_by(|a, b| lum(a).total_cmp(&lum(b)));
    picked
        .iter()
        .map(|c| {
            let k = (110.0 / lum(c).max(1.0)).max(1.0);
            let ch = |v: f32| ((v * k).min(255.0)) as u32;
            (ch(c[0]) << 16) | (ch(c[1]) << 8) | ch(c[2])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_cycle_and_parse() {
        assert_eq!(Mode::Spectrum.step(-1), Mode::Radial);
        assert_eq!(Mode::Radial.step(1), Mode::Spectrum);
        assert_eq!(Mode::from_name("waterfall"), Some(Mode::Waterfall));
    }

    #[test]
    fn mono_draws_a_vertical_line() {
        let mut c = Canvas::new(41, 41);
        c.heat = vec![0.0; 41 * 41];
        let samples: Vec<[f32; 2]> = (0..200).map(|i| [((i as f32) / 10.0).sin(); 2]).collect();
        let mut gain = 0.0;
        assert!(scope(&mut c, &samples, 1.0, &mut gain, &[0xffffff]));
        let lit: Vec<(u32, u32)> =
            (0..41 * 41).filter(|&i| c.heat[i] > 0.0).map(|i| (i as u32 % 41, i as u32 / 41)).collect();
        assert!(lit.iter().all(|&(x, _)| (19..=21).contains(&x)), "the beam and its soft edge: {lit:?}");
        assert!(lit.iter().any(|&(_, y)| y < 10) && lit.iter().any(|&(_, y)| y > 30));
    }

    #[test]
    fn waterfall_scrolls_left_with_bass_at_the_bottom() {
        let mut c = Canvas::new(10, 4);
        waterfall(&mut c, &[1.0, 0.0, 0.0, 0.0], 2, &[0xffffff]);
        let alpha = |c: &Canvas, x: u32, y: u32| c.rgba[((y * 10 + x) * 4 + 3) as usize];
        assert_eq!(alpha(&c, 9, 3), 255, "bass, bottom right");
        assert_eq!(alpha(&c, 9, 0), 0, "silent treble");
        waterfall(&mut c, &[0.0; 4], 2, &[0xffffff]);
        assert_eq!(alpha(&c, 7, 3), 255, "moved left");
        assert_eq!(alpha(&c, 9, 3), 0);
    }

    #[test]
    fn radial_bands_run_down_both_sides() {
        let p = polar_map(101, 101, 4);
        let at = |x: u32, y: u32| p[(y * 101 + x) as usize].0;
        // just right of the top and just left of it: both bass
        assert_eq!(at(58, 10), 0);
        assert_eq!(at(42, 10), 0);
        // the bottom: treble
        assert_eq!(at(58, 90), 3);
    }

    #[test]
    fn a_cover_palette_is_its_colours() {
        let px = (0..64 * 64).map(|i| if i % 2 == 0 { [200, 30, 30, 255] } else { [30, 30, 200, 255] }).collect();
        let pal = palette(&Image { w: 64, h: 64, px });
        assert_eq!(pal.len(), 2, "{pal:06x?}");
        assert!(pal.iter().any(|c| c >> 16 > (c & 0xff)), "a red");
        assert!(pal.iter().any(|c| c & 0xff > c >> 16), "a blue");
    }
}

/// `NOTMPC_SCENE_DUMP=dir cargo test dump_scenes -- --ignored` writes each
/// mode's canvas for synthetic music to `dir/<mode>.rgba` (with its size
/// in the name), to look at without a terminal.
#[cfg(test)]
mod dump {
    use super::*;

    #[test]
    #[ignore]
    fn dump_scenes() {
        let Some(dir) = std::env::var_os("NOTMPC_SCENE_DUMP") else { return };
        let dir = std::path::PathBuf::from(dir);
        let (w, h) = (884, 406);
        let colors = [0x5e81ac, 0xb48ead, 0xebcb8b];
        let look = Look { colors: &colors, mirror: VisualizerMirror::Off, peaks: true, fps: 20 };
        // a falling spectrum with a bass bump, and peaks above it
        let levels = |n: usize| -> Vec<f32> {
            (0..n)
                .map(|i| {
                    let t = i as f32 / n as f32;
                    (0.9 - 0.6 * t + 0.25 * (t * 40.0).sin() * (1.0 - t)).clamp(0.0, 1.0)
                })
                .collect()
        };
        // bars fallen to 70% under their peaks
        let fallen = |n: usize| {
            let mut b = Bars::default();
            b.update(&levels(n));
            b.bars.iter_mut().for_each(|v| *v *= 0.7);
            b
        };
        let bars = fallen(100);
        let save = |name: &str, c: &Canvas| {
            std::fs::write(dir.join(format!("{name}-{}x{}.rgba", c.w, c.h)), &c.rgba).unwrap();
        };

        let mut c = Canvas::new(w, h);
        spectrum(&mut c, &bars, 100, &colors, &look);
        save("spectrum", &c);
        let both = Look { mirror: VisualizerMirror::Both, ..look };
        let half = fallen(50);
        spectrum(&mut c, &half, 100, &colors, &both);
        save("spectrum-mirrored", &c);

        let mut c = Canvas::new(w, h);
        c.heat = vec![0.0; (w * h) as usize];
        let mut gain = 0.0;
        // two detuned tones, partly correlated: a wide, rotating figure
        let samples: Vec<[f32; 2]> = (0..3300)
            .map(|i| {
                let t = i as f32 / 44100.0;
                let a = (t * 220.0 * TAU).sin() * 0.5 + (t * 331.0 * TAU).sin() * 0.2;
                let b = (t * 220.0 * TAU + 0.9).sin() * 0.45 + (t * 443.0 * TAU).sin() * 0.15;
                [a, b]
            })
            .collect();
        for chunk in samples.chunks(1100) {
            scope(&mut c, chunk, 0.8, &mut gain, &colors);
        }
        save("scope", &c);

        let mut c = Canvas::new(w, h);
        for f in 0..220 {
            let lv: Vec<f32> = (0..h as usize)
                .map(|y| {
                    let t = y as f32 / h as f32;
                    let beat = if f % 10 < 2 { 0.4 * (1.0 - t * 3.0).max(0.0) } else { 0.0 };
                    let melody = (-(t - 0.4 - 0.1 * (f as f32 / 15.0).sin()).powi(2) * 400.0).exp() * 0.8;
                    (0.35 * (1.0 - t) + beat + melody).min(1.0)
                })
                .collect();
            waterfall(&mut c, &lv, 4, &colors);
        }
        save("waterfall", &c);

        let mut c = Canvas::new(w, h);
        let rb = fallen(RADIAL_BANDS);
        let cover = Image {
            w: 64,
            h: 64,
            px: (0..64 * 64)
                .map(|i| {
                    let (x, y) = (i % 64, i / 64);
                    if (x / 8 + y / 8) % 2 == 0 { [220, 120, 40, 255] } else { [40, 60, 120, 255] }
                })
                .collect(),
        };
        radial(&mut c, &rb, Some(&cover), &palette(&cover), true);
        save("radial", &c);
    }
}
