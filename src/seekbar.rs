//! The waveform progress bar on screen: the track's envelope on one plane,
//! the Omnium Cargo riding it on a smaller plane above.
//!
//! notcurses can't stack pixel images, so the rider's plane carries the
//! slice of waveform behind the bike as well. The waveform plane is only
//! re-sent when the playhead enters a new cell (the rider covers the
//! playhead in between); the rider's at ~12 fps while playing, and only
//! when it changed. Without pixel graphics the waveform falls back to
//! braille and there's no rider.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::nc::{Nc, OwnedPlane, Visual};
use crate::waveform::Loader;
use crate::wavebar::{Canvas, Colors, omnium, render_wave};

/// The rider's frame rate while playing.
pub const RIDER_FPS: u32 = 12;
/// Wheel turns per second.
const SPIN: f32 = 1.2;
const ORANGE: u32 = 0xdd6f00;

/// What the waveform plane last showed.
#[derive(PartialEq)]
struct Drawn {
    path: PathBuf,
    cols: u32,
    cell: u32,
    tint: Option<u32>,
    /// The rider's cell: notcurses blanks the cells under its plane, so the
    /// ones it leaves need the waveform re-sent.
    rider_cell: u32,
}

pub struct SeekBar<'nc> {
    nc: &'nc Nc,
    loader: Loader,
    music_dir: Option<PathBuf>,
    pixel: bool,
    // Fields drop in order: the rider first.
    rider: Option<OwnedPlane<'nc>>,
    wave: Option<OwnedPlane<'nc>>,
    drawn: Option<Drawn>,
    /// The rider's last image key: (x px, bob px, spoke step).
    rider_drawn: Option<(i32, i32, u32)>,
    tint: Option<(PathBuf, Option<u32>)>,
    phase: f32,
    bob: f32,
    last: Instant,
}

impl<'nc> SeekBar<'nc> {
    pub fn new(nc: &'nc Nc, music_dir: Option<PathBuf>) -> Self {
        Self {
            nc,
            loader: Loader::new(),
            music_dir,
            pixel: nc.can_pixel(),
            rider: None,
            wave: None,
            drawn: None,
            rider_drawn: None,
            tint: None,
            phase: 0.0,
            bob: 0.0,
            last: Instant::now(),
        }
    }

    /// Whether the rider wants frequent redraws.
    pub fn animating(&self, playing: bool) -> bool {
        playing && self.pixel && self.rider.is_some()
    }

    pub fn hide(&mut self) {
        self.rider = None;
        self.wave = None;
        self.drawn = None;
        self.rider_drawn = None;
    }

    /// Draws the waveform for `file` at rows y..y+2 with `frac` played.
    /// Returns false when there's nothing to show yet (no local file, or
    /// still decoding): the caller then draws the plain bar.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        y: u32,
        cols: u32,
        file: Option<&str>,
        frac: f32,
        playing: bool,
        cover: Option<&Visual>,
        colors: (u32, u32),
    ) -> Result<bool> {
        let dt = self.last.elapsed().as_secs_f32().min(0.5);
        self.last = Instant::now();
        let (Some(dir), Some(file)) = (&self.music_dir, file) else {
            self.hide();
            return Ok(false);
        };
        let path = dir.join(file);
        let Some(wave) = self.loader.get(&path) else {
            self.hide();
            return Ok(false);
        };

        // pixel geometry of the strip
        // braille is 2x4 dots per cell
        let (ch, cw) = if self.pixel { self.nc.cell_px().unwrap_or((20, 10)) } else { (4, 2) };
        let (w, h) = (cols * cw, 2 * ch);
        let played = (frac.clamp(0.0, 1.0) * w as f32) as u32;

        let tint = self.cover_tint(&path, cover);
        let mut palette = Colors::tinted(ORANGE, tint, colors.1);
        palette.translucent = self.pixel;
        let canvas = render_wave(&wave, w, h, played, &palette);

        let rider_cell = self.rider_x(w, h, played).map_or(0, |x| x as u32 / cw);
        let key = Drawn { path: path.clone(), cols, cell: played / cw, tint, rider_cell };
        if self.drawn.as_ref() != Some(&key) || self.wave.is_none() {
            if self.wave.as_ref().is_none_or(|p| p.dim_yx() != (2, cols)) {
                self.rider = None;
                self.wave = Some(self.nc.new_plane(y, 0, 2, cols)?);
            }
            let v = Visual::from_rgba(&canvas.px, h, w)?;
            self.nc.blit_onto(&v, self.wave.as_mut().unwrap(), self.pixel)?;
            self.drawn = Some(key);
            self.rider_drawn = None;
        }

        if self.pixel && h >= 16 {
            self.draw_rider(&wave, &canvas, y, cw, frac, playing, dt, colors.0)?;
        }
        Ok(true)
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_rider(
        &mut self,
        wave: &crate::waveform::Waveform,
        canvas: &Canvas,
        y: u32,
        cw: u32,
        frac: f32,
        playing: bool,
        dt: f32,
        frame: u32,
    ) -> Result<()> {
        let scale = (canvas.h / 16).max(1);
        let (sw, sh) = (32 * scale, 16 * scale);
        if playing {
            self.phase = (self.phase + SPIN * dt).fract();
        }
        // rise with the music at the playhead, smoothly
        let (_, rms) = wave.span(frac, frac + 1.0 / 400.0);
        let target = (rms * 1.6).min(1.0) * canvas.h.saturating_sub(sh) as f32 * 0.9;
        self.bob += (target - self.bob) * (dt * 6.0).min(1.0);

        let played = (frac.clamp(0.0, 1.0) * canvas.w as f32) as u32;
        let x = self.rider_x(canvas.w, canvas.h, played).unwrap_or(0);
        let bob = self.bob.round() as i32;
        let step = (self.phase * 12.0) as u32 % 12;
        if self.rider_drawn == Some((x, bob, step)) {
            return Ok(());
        }

        let cells = sw.div_ceil(cw) + 1;
        let cell_x = x as u32 / cw;
        let mut img = canvas.slice((cell_x * cw) as i32, cells * cw);
        let bike = omnium(step as f32 / 12.0, frame, ORANGE);
        img.stamp(&bike, scale, x - (cell_x * cw) as i32, canvas.h as i32 - sh as i32 - bob);

        if self.rider.as_ref().is_none_or(|p| p.dim_yx() != (2, cells)) {
            let mut p = self.nc.new_plane(y, cell_x, 2, cells)?;
            p.move_top();
            self.rider = Some(p);
        }
        let rider = self.rider.as_mut().unwrap();
        rider.move_yx(y, cell_x);
        let v = Visual::from_rgba(&img.px, img.h, img.w)?;
        self.nc.blit_onto(&v, rider, true)?;
        self.rider_drawn = Some((x, bob, step));
        Ok(())
    }

    /// The rider's left edge in pixels, centred on the playhead; None when
    /// there's no rider (no pixel graphics, or too short).
    fn rider_x(&self, w: u32, h: u32, played: u32) -> Option<i32> {
        if !self.pixel || h < 16 {
            return None;
        }
        let sw = 32 * (h / 16).max(1);
        Some((played as i32 - sw as i32 / 2).clamp(0, w.saturating_sub(sw) as i32))
    }

    /// The cover's average colour, computed once per track.
    fn cover_tint(&mut self, path: &Path, cover: Option<&Visual>) -> Option<u32> {
        match &self.tint {
            Some((p, t)) if p == path => *t,
            _ => {
                let t = cover.and_then(Visual::average_rgb);
                self.tint = Some((path.to_owned(), t));
                t
            }
        }
    }
}

/// How often the main loop should wake for the rider.
pub fn rider_interval() -> Duration {
    Duration::from_secs(1) / RIDER_FPS
}
