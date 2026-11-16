//! The Visualizer tab: a big picture of the music, drawn in pixels.
//!
//! - **Spectrum**: the header's bars, full size, with peak caps.
//! - **Cover**: the bars cut out of the album cover, over a dim copy of it.
//! - **Scope**: a goniometer (vectorscope). Left against right, turned so
//!   mono is a vertical line and wide stereo spreads sideways; the trace
//!   fades like a phosphor screen.
//! - **Waterfall**: a spectrogram scrolling right to left, bass at the
//!   bottom, loudness as colour.
//! - **Radial**: the spectrum bent round a circle, bass at the top, with
//!   the album cover in the middle pulsing to the bass.
//! - **Ridgeline**: the last few seconds of spectra as lines receding into
//!   the distance, each hiding the ones behind it.
//! - **Warp**: a feedback loop. Each frame is the last one zoomed in,
//!   turned a little and faded, with a ring-shaped waveform (and the cover,
//!   if there is one) drawn over the middle, so it all streams outward;
//!   the bass speeds the zoom.
//!
//! Colours come from the theme's `visualizer_colors`, or from the cover.
//! Each frame is drawn into an RGBA canvas; kitty gets it through shared
//! memory at a modest resolution and scales it on the GPU (`kitty.rs`),
//! other terminals get it through notcurses in character cells. Nothing
//! here runs, and its buffers are dropped, while the tab isn't showing.

use std::collections::VecDeque;
use std::f32::consts::{PI, SQRT_2, TAU};
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
    Cover,
    Scope,
    Waterfall,
    Ridgeline,
    Radial,
    Warp,
}

impl Mode {
    pub const ALL: [Mode; 7] =
        [Mode::Spectrum, Mode::Cover, Mode::Scope, Mode::Waterfall, Mode::Ridgeline, Mode::Radial, Mode::Warp];

    pub fn name(self) -> &'static str {
        match self {
            Mode::Spectrum => "Spectrum",
            Mode::Cover => "Cover",
            Mode::Scope => "Scope",
            Mode::Waterfall => "Waterfall",
            Mode::Ridgeline => "Ridgeline",
            Mode::Radial => "Radial",
            Mode::Warp => "Warp",
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
/// Lines the ridgeline shows, one every `RIDGE_STEP` seconds.
const RIDGE_LINES: usize = 40;
const RIDGE_STEP: f32 = 0.07;
/// Points along each ridge.
const RIDGE_POINTS: usize = 96;
/// Most pixels the warp's canvas has: every pixel is resampled each frame,
/// and the picture is soft anyway.
const WARP_MAX_PX: f32 = 90_000.0;

struct Canvas {
    w: u32,
    h: u32,
    rgba: Vec<u8>,
    /// Scope: the trace's brightness per pixel.
    heat: Vec<f32>,
    /// Radial: each pixel's (band or `u16::MAX` between bars, radius).
    polar: Vec<(u16, f32)>,
    /// Cover: the cover scaled to fill the canvas.
    backdrop: Vec<u32>,
    /// Warp: the picture as light (0..1 per channel), and the next one.
    trail: Vec<[f32; 3]>,
    trail_next: Vec<[f32; 3]>,
}

impl Canvas {
    fn new(w: u32, h: u32) -> Self {
        Self {
            w,
            h,
            rgba: vec![0; (w * h * 4) as usize],
            heat: Vec::new(),
            polar: Vec::new(),
            backdrop: Vec::new(),
            trail: Vec::new(),
            trail_next: Vec::new(),
        }
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
    /// Scope, Warp: the recent loudest sample, for automatic gain.
    gain: f32,
    /// Scope, Warp: some trace is still fading.
    heat_left: bool,
    /// Ridgeline: the spectra shown, newest (the front one, still
    /// changing) first, and the time since the front one started.
    ridges: VecDeque<Vec<f32>>,
    ridge_age: f32,
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
            ridges: VecDeque::new(),
            ridge_age: 0.0,
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
            c.backdrop.clear();
            c.trail.clear();
            c.trail_next.clear();
        }
        self.bars = Bars::default();
        self.heat_left = false;
        self.ridges.clear();
        self.ridge_age = 0.0;
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
        if let Some(c) = self.canvas.as_mut() {
            c.backdrop.clear();
        }
        self.fresh = true;
    }

    /// Whether the next frame would differ from this one.
    pub fn moving(&self, live: bool) -> bool {
        live || match self.mode {
            Mode::Spectrum | Mode::Cover | Mode::Radial => self.bars.moving(),
            Mode::Scope | Mode::Warp => self.heat_left,
            Mode::Waterfall | Mode::Ridgeline => false,
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
        self.ridges.clear();
        self.ridge_age = 0.0;
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
            Mode::Spectrum | Mode::Cover => {
                let moving = live || self.bars.moving();
                if moving || fresh {
                    let bands = if look.mirror.horizontal() { bar_count(area).div_ceil(2) } else { bar_count(area) };
                    self.bars.update(&viz.levels(bands));
                    let fill = match (self.mode, self.cover.as_ref()) {
                        (Mode::Cover, Some(img)) => {
                            if c.backdrop.len() != (c.w * c.h) as usize {
                                c.backdrop = cover_fill(img, c.w, c.h);
                            }
                            Some(std::mem::take(&mut c.backdrop))
                        }
                        _ => None,
                    };
                    spectrum(&mut c, &self.bars, bar_count(area), &stops, look, fill.as_deref());
                    if let Some(f) = fill {
                        c.backdrop = f;
                    }
                }
                moving || fresh
            }
            Mode::Ridgeline => {
                if live {
                    self.bars.update(&viz.levels(RIDGE_POINTS));
                    self.ridge_age += dt;
                    if self.ridges.is_empty() || self.ridge_age >= RIDGE_STEP {
                        self.ridge_age = if self.ridges.is_empty() { 0.0 } else { self.ridge_age % RIDGE_STEP };
                        self.ridges.push_front(self.bars.bars.clone());
                        self.ridges.truncate(RIDGE_LINES + 1);
                    } else {
                        self.ridges[0].clone_from(&self.bars.bars);
                    }
                }
                if live || fresh {
                    ridgeline(&mut c, &self.ridges, self.ridge_age / RIDGE_STEP, &stops);
                }
                live || fresh
            }
            Mode::Warp => {
                let n = (c.w * c.h) as usize;
                if c.trail.len() != n {
                    c.trail = vec![[0.0; 3]; n];
                    c.trail_next = vec![[0.0; 3]; n];
                }
                let changed = live || self.heat_left || fresh;
                if changed {
                    let (samples, bass) = if live {
                        let lv = viz.levels(24);
                        (viz.recent(1024), lv[..3].iter().sum::<f32>() / 3.0)
                    } else {
                        (Vec::new(), 0.0)
                    };
                    self.heat_left = warp(&mut c, &samples, bass, dt, &mut self.gain, self.cover.as_ref(), &stops);
                }
                changed
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
    /// (the warp's to `WARP_MAX_PX`), keeping the screen's shape so circles
    /// stay round; otherwise
    /// about the dots character cells can show.
    fn canvas_size(&self, area: Rect) -> (u32, u32) {
        if self.use_kitty {
            let (ch, cw) = self.nc.cell_px().unwrap_or((20, 10));
            let (fw, fh) = ((area.cols * cw) as f32, (area.rows * ch) as f32);
            let most = if self.mode == Mode::Warp { WARP_MAX_PX } else { KITTY_MAX_PX };
            let k = (most / (fw * fh)).sqrt().min(1.0);
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

/// The bars, coloured from `stops`, or cut out of `fill` (a picture the
/// canvas's size, shown dim behind them).
fn spectrum(c: &mut Canvas, bars: &Bars, n: usize, stops: &[u32], look: &Look, fill: Option<&[u32]>) {
    match fill {
        Some(f) => {
            for (i, &rgb) in f.iter().enumerate() {
                c.set(i, rgb, 0.22);
            }
        }
        None => c.clear(),
    }
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
                let paint = |c: &mut Canvas, y: f32| {
                    let i = (y as u32 * c.w + x) as usize;
                    let (col, a) = match fill {
                        _ if on_cap => (scale(rgb, 1.25), 1.0),
                        Some(f) => (f[i], 1.0),
                        // brighter toward the bar's tip
                        None => (scale(rgb, 0.45 + 0.55 * d_f / span), 0.95),
                    };
                    c.set(i, col, a);
                };
                let up = base - 1.0 - d_f;
                if up >= 0.0 {
                    paint(c, up);
                }
                if look.mirror.vertical() {
                    let down = base + d_f;
                    if down < h {
                        paint(c, down);
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
    trace(w, h, a, b, amount, |i, v| heat[i] += v);
}

/// `line`, handing each pixel index and its share to `add`.
fn trace(w: u32, h: u32, a: (f32, f32), b: (f32, f32), amount: f32, mut add: impl FnMut(usize, f32)) {
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
                add((y as u32 * w + x as u32) as usize, each * k);
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

/// `img` scaled (smoothly) to cover a `w`×`h` canvas, cropping what
/// sticks out, as RGB.
fn cover_fill(img: &Image, w: u32, h: u32) -> Vec<u32> {
    let k = (img.w as f32 / w as f32).min(img.h as f32 / h as f32);
    let (ox, oy) = ((img.w as f32 - w as f32 * k) / 2.0, (img.h as f32 - h as f32 * k) / 2.0);
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let rgb = sample(img, ox + (x as f32 + 0.5) * k - 0.5, oy + (y as f32 + 0.5) * k - 0.5);
            out.push((u32::from(rgb[0]) << 16) | (u32::from(rgb[1]) << 8) | u32::from(rgb[2]));
        }
    }
    out
}

/// `img` at (`x`, `y`), between its pixels.
fn sample(img: &Image, x: f32, y: f32) -> [u8; 3] {
    let (x, y) = (x.clamp(0.0, (img.w - 1) as f32), y.clamp(0.0, (img.h - 1) as f32));
    let (x0, y0) = (x as u32, y as u32);
    let (x1, y1) = ((x0 + 1).min(img.w - 1), (y0 + 1).min(img.h - 1));
    let (fx, fy) = (x.fract(), y.fract());
    let px = |x: u32, y: u32| img.px[(y * img.w + x) as usize];
    let mut out = [0u8; 3];
    for (ch, o) in out.iter_mut().enumerate() {
        let top = f32::from(px(x0, y0)[ch]) * (1.0 - fx) + f32::from(px(x1, y0)[ch]) * fx;
        let bottom = f32::from(px(x0, y1)[ch]) * (1.0 - fx) + f32::from(px(x1, y1)[ch]) * fx;
        *o = (top * (1.0 - fy) + bottom * fy) as u8;
    }
    out
}

/// Draws `ridges` (newest first) from the back forward, each hiding what's
/// behind it. `scroll` (0..1) is how far the front one is toward the
/// next step back, so the lines glide rather than jump.
fn ridgeline(c: &mut Canvas, ridges: &VecDeque<Vec<f32>>, scroll: f32, stops: &[u32]) {
    c.clear();
    let (w, h) = (c.w as f32, c.h as f32);
    let (horizon, ground) = (h * 0.1, h * 0.96);
    // perspective: the front line is full size, the back one `far` of it
    let size = |depth: f32| 1.0 / (1.0 + 2.5 * depth);
    let far = size(1.0);
    let thick = (h / 220.0).max(1.0);
    for (k, levels) in ridges.iter().enumerate().rev() {
        let depth = (k as f32 + scroll) / RIDGE_LINES as f32;
        if depth > 1.0 || levels.len() < 2 {
            continue;
        }
        let p = size(depth);
        let base = horizon + (ground - horizon) * (p - far) / (1.0 - far);
        let amp = h * 0.42 * p;
        let half = w * 0.47 * (0.35 + 0.65 * p);
        let (x0, x1) = ((w / 2.0 - half).max(0.0) as u32, ((w / 2.0 + half) as u32).min(c.w));
        // dimmer into the distance, and fading in at the very back
        let light = (1.0 - depth).powf(0.6) * ((1.0 - depth) * 8.0).min(1.0);
        let mut prev: Option<f32> = None;
        for x in x0..x1 {
            let u = (x - x0) as f32 / (x1 - x0).max(1) as f32;
            // flat at the ends, the spectrum across the middle
            let edge = (u.min(1.0 - u) / 0.06).min(1.0);
            let f = (u - 0.06).clamp(0.0, 0.88) / 0.88 * (levels.len() - 1) as f32;
            let (i, t) = (f as usize, f.fract());
            let v = levels[i] * (1.0 - t) + levels[(i + 1).min(levels.len() - 1)] * t;
            let y = base - amp * v * edge * edge * (3.0 - 2.0 * edge);
            // hide what's behind, from the line down to its base
            for row in (y.max(0.0) as u32)..=(base as u32).min(c.h - 1) {
                c.rgba[((row * c.w + x) * 4 + 3) as usize] = 0;
            }
            let rgb = scale(gradient(stops, u), 0.55 + 0.6 * v);
            let (top, bottom) = match prev {
                Some(q) => (q.min(y), q.max(y) + thick),
                None => (y, y + thick),
            };
            for row in (top.max(0.0) as u32)..(bottom.ceil() as u32).min(c.h) {
                c.set((row * c.w + x) as usize, rgb, light);
            }
            prev = Some(y);
        }
    }
}

/// The warp's next frame: the last one zoomed in about the middle, turned
/// and faded, then the cover (if any) blended into the middle and the
/// waveform drawn round it as a ring. Returns whether anything is left.
fn warp(
    c: &mut Canvas,
    samples: &[[f32; 2]],
    bass: f32,
    dt: f32,
    gain: &mut f32,
    cover: Option<&Image>,
    stops: &[u32],
) -> bool {
    let (w, h) = (c.w as usize, c.h as usize);
    let (cx, cy) = (c.w as f32 / 2.0, c.h as f32 / 2.0);
    // per 20 fps frame, so the speed doesn't depend on the frame rate
    let k = dt * 20.0;
    let fade = 0.9f32.powf(k);
    let zoom = 1.0 + (0.012 + 0.06 * bass) * k;
    let (sin, cos) = (0.006 * k).sin_cos();
    let src = &c.trail;
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            // where this pixel was: nearer the middle, turned back
            let sx = (dx * cos + dy * sin) / zoom + cx - 0.5;
            let sy = (dy * cos - dx * sin) / zoom + cy - 0.5;
            let out = &mut c.trail_next[y * w + x];
            if sx < 0.0 || sy < 0.0 || sx > (w - 1) as f32 || sy > (h - 1) as f32 {
                *out = [0.0; 3];
                continue;
            }
            let (x0, y0) = (sx as usize, sy as usize);
            let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
            let (fx, fy) = (sx.fract(), sy.fract());
            let (a, b, cc, d) = (src[y0 * w + x0], src[y0 * w + x1], src[y1 * w + x0], src[y1 * w + x1]);
            for ch in 0..3 {
                let top = a[ch] + (b[ch] - a[ch]) * fx;
                let bottom = cc[ch] + (d[ch] - cc[ch]) * fx;
                out[ch] = (top + (bottom - top) * fy) * fade;
            }
        }
    }
    std::mem::swap(&mut c.trail, &mut c.trail_next);
    let radius = cx.min(cy);
    if let Some(img) = cover.filter(|_| !samples.is_empty()) {
        // the cover in a disc, more of it on a beat
        let rc = radius * 0.2 * (0.9 + 0.3 * bass);
        let mix = (0.2 + 0.6 * bass).min(0.8);
        let side = img.w.min(img.h) as f32;
        let (y0, y1) = ((cy - rc).max(0.0) as usize, ((cy + rc) as usize + 1).min(h));
        let (x0, x1) = ((cx - rc).max(0.0) as usize, ((cx + rc) as usize + 1).min(w));
        for y in y0..y1 {
            for x in x0..x1 {
                let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                let r = dx.hypot(dy);
                if r >= rc {
                    continue;
                }
                let a = mix * ((rc - r) / 2.0).min(1.0);
                let p =
                    sample(img, img.w as f32 / 2.0 + dx / rc * side / 2.0, img.h as f32 / 2.0 + dy / rc * side / 2.0);
                let t = &mut c.trail[y * w + x];
                for ch in 0..3 {
                    t[ch] += (f32::from(p[ch]) / 255.0 - t[ch]) * a;
                }
            }
        }
    }
    if samples.len() > 1 {
        let mono: Vec<f32> = samples.iter().map(|&[l, r]| (l + r) / 2.0).collect();
        let loudest = mono.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        *gain = (*gain * 0.98).max(loudest).max(0.02);
        let n = mono.len();
        // ease the end into the start so the ring closes
        let blend = n / 12;
        let at = |i: usize| {
            let v = mono[i];
            if i + blend >= n {
                let f = (i + blend + 1 - n) as f32 / (blend + 1) as f32;
                v * (1.0 - f) + mono[i + blend + 1 - n] * f
            } else {
                v
            }
        };
        let r0 = radius * (0.32 + 0.06 * bass);
        let point = |i: usize| {
            let a = i as f32 / n as f32 * TAU - PI / 2.0;
            let r = r0 * (1.0 + 0.45 * at(i) / *gain);
            (cx + r * a.cos(), cy + r * a.sin())
        };
        let mut prev = point(0);
        for i in 1..=n {
            let p = point(i % n);
            // bass to treble colours round from the top and back
            let u = i as f32 / n as f32;
            let rgb = gradient(stops, 1.0 - (1.0 - 2.0 * u).abs());
            let col = [(rgb >> 16) as f32 / 255.0, ((rgb >> 8) & 0xff) as f32 / 255.0, (rgb & 0xff) as f32 / 255.0];
            let trail = &mut c.trail;
            trace(c.w, c.h, prev, p, 0.6, |j, v| {
                let t = &mut trail[j];
                for ch in 0..3 {
                    t[ch] += col[ch] * v;
                }
                // brighter only up to a point, keeping the hue (not white)
                let most = t[0].max(t[1]).max(t[2]);
                if most > 1.2 {
                    t.iter_mut().for_each(|v| *v *= 1.2 / most);
                }
            });
            prev = p;
        }
    }
    let mut left = false;
    for (i, t) in c.trail.iter_mut().enumerate() {
        let most = t[0].max(t[1]).max(t[2]);
        if most < 0.004 {
            *t = [0.0; 3];
            c.rgba[i * 4 + 3] = 0;
            continue;
        }
        left = true;
        // the colour at full strength; how bright as opacity
        let ch = |v: f32| (v / most * 255.0) as u8;
        c.rgba[i * 4..i * 4 + 4].copy_from_slice(&[ch(t[0]), ch(t[1]), ch(t[2]), (most.min(1.0) * 255.0) as u8]);
    }
    left
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
        assert_eq!(Mode::Spectrum.step(-1), Mode::Warp);
        assert_eq!(Mode::Warp.step(1), Mode::Spectrum);
        assert_eq!(Mode::Spectrum.step(1), Mode::Cover);
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
    fn a_front_ridge_hides_the_ones_behind() {
        let line_rows = |ridges: [Vec<f32>; 2]| {
            let mut c = Canvas::new(200, 100);
            ridgeline(&mut c, &ridges.into_iter().collect(), 0.0, &[0xffffff]);
            (0..100u32).filter(|&y| c.rgba[((y * 200 + 100) * 4 + 3) as usize] > 0).collect::<Vec<_>>()
        };
        // newest (front) first: two flat lines show as two runs
        let flat = line_rows([vec![0.0; 8], vec![0.0; 8]]);
        assert!(flat.windows(2).any(|w| w[1] > w[0] + 1), "{flat:?}");
        // a tall front line rises over the flat one behind and hides it
        let tall = line_rows([vec![1.0; 8], vec![0.0; 8]]);
        assert!(tall.windows(2).all(|w| w[1] == w[0] + 1), "one run: {tall:?}");
        assert!(tall[0] < flat[0], "{tall:?} above {flat:?}");
    }

    #[test]
    fn warp_streams_outward_and_fades() {
        let mut c = Canvas::new(60, 60);
        c.trail = vec![[0.0; 3]; 3600];
        c.trail_next = c.trail.clone();
        c.trail[30 * 60 + 40] = [1.0; 3];
        let mut gain = 0.0;
        assert!(warp(&mut c, &[], 0.0, 1.0, &mut gain, None, &[0xffffff]));
        let (i, v) = c.trail.iter().enumerate().max_by(|a, b| a.1[0].total_cmp(&b.1[0])).unwrap();
        let x = i % 60;
        assert!(x >= 40 && v[0] < 0.95, "moved out from the middle at {x}, faded to {}", v[0]);
        for _ in 0..200 {
            warp(&mut c, &[], 0.0, 1.0, &mut gain, None, &[0xffffff]);
        }
        assert!(!warp(&mut c, &[], 0.0, 1.0, &mut gain, None, &[0xffffff]), "and goes out");
    }

    #[test]
    fn cover_bars_show_the_cover() {
        let look = Look { colors: &[0xffffff], mirror: VisualizerMirror::Off, peaks: false, fps: 20 };
        let mut c = Canvas::new(40, 20);
        let fill: Vec<u32> = (0..800).map(|i| if i % 40 < 20 { 0xff0000 } else { 0x0000ff }).collect();
        let mut bars = Bars::default();
        bars.update(&[1.0, 0.0]);
        spectrum(&mut c, &bars, 2, &[0xffffff], &look, Some(&fill));
        let px = |x: u32, y: u32| &c.rgba[((y * 40 + x) * 4) as usize..((y * 40 + x) * 4 + 4) as usize];
        assert_eq!(px(5, 10), [255, 0, 0, 255], "a bar is the cover");
        assert_eq!(&px(30, 10)[..3], [0, 0, 255], "dim cover between");
        assert!(px(30, 10)[3] < 80);
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
        spectrum(&mut c, &bars, 100, &colors, &look, None);
        save("spectrum", &c);
        let both = Look { mirror: VisualizerMirror::Both, ..look };
        let half = fallen(50);
        spectrum(&mut c, &half, 100, &colors, &both, None);
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

        let mut c = Canvas::new(w, h);
        let fill = cover_fill(&cover, w, h);
        spectrum(&mut c, &bars, 100, &palette(&cover), &look, Some(&fill));
        save("cover", &c);

        let mut c = Canvas::new(w, h);
        let mut ridges = VecDeque::new();
        for f in 0..RIDGE_LINES {
            let lv: Vec<f32> = levels(RIDGE_POINTS)
                .iter()
                .enumerate()
                .map(|(i, v)| (v * (0.6 + 0.4 * ((f as f32 * 0.7 + i as f32 * 0.3).sin()))).max(0.0))
                .collect();
            ridges.push_back(lv);
        }
        ridgeline(&mut c, &ridges, 0.4, &colors);
        save("ridgeline", &c);

        let (ww, wh) = (442, 203);
        let mut c = Canvas::new(ww, wh);
        c.trail = vec![[0.0; 3]; (ww * wh) as usize];
        c.trail_next = c.trail.clone();
        let mut gain = 0.0;
        for f in 0..60 {
            let chunk: Vec<[f32; 2]> = (0..1024)
                .map(|i| {
                    let t = (f * 1024 + i) as f32 / 44100.0;
                    let v = (t * 110.0 * TAU).sin() * 0.5 + (t * 660.0 * TAU).sin() * 0.25;
                    [v, v]
                })
                .collect();
            let bass = if f % 8 < 2 { 0.9 } else { 0.3 };
            warp(&mut c, &chunk, bass, 0.05, &mut gain, Some(&cover), &colors);
        }
        save("warp", &c);
    }
}
