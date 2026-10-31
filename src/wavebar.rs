//! Pixels for the waveform progress bar: the track's envelope and the
//! Omnium Cargo that rides it. Pure image code; `ui.rs` puts it on screen.

use crate::waveform::Waveform;

/// An RGBA image.
#[derive(Clone)]
pub struct Canvas {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>,
}

impl Canvas {
    pub fn new(w: u32, h: u32) -> Self {
        Self { w, h, px: vec![0; (w * h * 4) as usize] }
    }

    fn put(&mut self, x: i32, y: i32, c: [u8; 4]) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return;
        }
        let i = ((y as u32 * self.w + x as u32) * 4) as usize;
        self.px[i..i + 4].copy_from_slice(&c);
    }

    /// Columns [x0, x0 + w) of this canvas (transparent past its edges).
    pub fn slice(&self, x0: i32, w: u32) -> Canvas {
        let mut out = Canvas::new(w, self.h);
        for y in 0..self.h {
            for x in 0..w {
                let sx = x0 + x as i32;
                if sx >= 0 && (sx as u32) < self.w {
                    let i = ((y * self.w + sx as u32) * 4) as usize;
                    let o = ((y * w + x) * 4) as usize;
                    out.px[o..o + 4].copy_from_slice(&self.px[i..i + 4]);
                }
            }
        }
        out
    }

    /// Paints `sprite` scaled by `scale` with its top-left at (x, y).
    pub fn stamp(&mut self, sprite: &Canvas, scale: u32, x: i32, y: i32) {
        for sy in 0..sprite.h {
            for sx in 0..sprite.w {
                let i = ((sy * sprite.w + sx) * 4) as usize;
                let c: [u8; 4] = sprite.px[i..i + 4].try_into().unwrap();
                if c[3] == 0 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        self.put(x + (sx * scale + dx) as i32, y + (sy * scale + dy) as i32, c);
                    }
                }
            }
        }
    }
}

fn rgba(rgb: u32, a: u8) -> [u8; 4] {
    [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, a]
}

fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |s: u32| {
        let (x, y) = (((a >> s) & 0xff) as f32, ((b >> s) & 0xff) as f32);
        ((x + (y - x) * t).round() as u32) << s
    };
    ch(16) | ch(8) | ch(0)
}

pub struct Colors {
    pub played: u32,
    pub unplayed: u32,
    pub playhead: u32,
    /// Translucency (pixel graphics). Character blitters drop translucent
    /// pixels, so the fallback draws everything solid.
    pub translucent: bool,
}

impl Colors {
    /// Played in orange, nudged toward the cover's average colour.
    pub fn tinted(orange: u32, cover: Option<u32>, playhead: u32) -> Self {
        let played = cover.map_or(orange, |c| mix(orange, c, 0.3));
        Self { played, unplayed: 0x8a8a8a, playhead, translucent: true }
    }
}

/// The envelope, mirrored around the middle: the loudness (RMS) core
/// solid, peaks fainter around it. Played columns (`x < played`) take the
/// played colour; the rest is a translucent grey.
pub fn render_wave(wave: &Waveform, w: u32, h: u32, played: u32, colors: &Colors) -> Canvas {
    let mut c = Canvas::new(w, h);
    let mid = h as f32 / 2.0;
    let half = mid - 1.0;
    for x in 0..w {
        let (peak, rms) = wave.span(x as f32 / w as f32, (x + 1) as f32 / w as f32);
        // a gentle curve so quiet passages still read
        let (p, r) = (peak.powf(0.7) * half, (rms * 1.6).min(1.0).powf(0.7) * half);
        let done = x < played;
        // the played part brightens left to right
        let base = if done { mix(colors.played, 0xffffff, 0.15 * x as f32 / played.max(1) as f32) } else { colors.unplayed };
        let (outer, inner) = match (done, colors.translucent) {
            (true, true) => (rgba(base, 150), rgba(base, 255)),
            (false, true) => (rgba(base, 70), rgba(base, 140)),
            (true, false) => (rgba(mix(base, 0, 0.35), 255), rgba(base, 255)),
            (false, false) => (rgba(0x4a4a4a, 255), rgba(0x6e6e6e, 255)),
        };
        for y in 0..h {
            let d = (y as f32 + 0.5 - mid).abs();
            if d <= r.max(0.5) {
                c.put(x as i32, y as i32, inner);
            } else if d <= p {
                c.put(x as i32, y as i32, outer);
            }
        }
    }
    if played > 0 && played < w {
        for y in 0..h {
            c.put(played as i32, y as i32, rgba(colors.playhead, 255));
        }
    }
    c
}

/// The rider: an Omnium Cargo front-loader, side view, facing right, on a
/// 32x16 grid — big rear wheel, small front wheel under a cargo platform
/// with a crate, frame in `frame` colour. `phase` (0..1) turns the spokes.
pub fn omnium(phase: f32, frame: u32, cargo: u32) -> Canvas {
    let mut c = Canvas::new(32, 16);
    let tire = rgba(0xd8d8d8, 255);
    let spoke = rgba(0x9a9a9a, 255);
    let fr = rgba(frame, 255);
    let ca = rgba(cargo, 255);

    let wheel = |c: &mut Canvas, (cx, cy): (f32, f32), r: f32| {
        for k in 0..3 {
            let a = (phase + k as f32 / 3.0) * std::f32::consts::TAU;
            let (dx, dy) = (a.cos() * (r - 0.8), a.sin() * (r - 0.8));
            line(c, (cx - dx, cy - dy), (cx + dx, cy + dy), spoke);
        }
        for y in 0..16 {
            for x in 0..32 {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                if (d - r).abs() < 0.6 {
                    c.put(x, y, tire);
                }
            }
        }
    };

    let rear = (6.5, 10.5);
    let front = (26.0, 12.5);
    wheel(&mut c, rear, 5.0);
    wheel(&mut c, front, 3.0);

    let bb = (13.0, 12.0);
    let seat = (10.0, 5.0);
    let head_top = (19.0, 5.0);
    let head_bottom = (20.0, 8.0);
    line(&mut c, bb, seat, fr); // seat tube
    line(&mut c, seat, head_top, fr); // top tube
    line(&mut c, bb, head_bottom, fr); // down tube
    line(&mut c, bb, rear, fr); // chainstay
    line(&mut c, seat, rear, fr); // seatstay
    line(&mut c, head_top, head_bottom, fr); // head tube
    line(&mut c, head_bottom, front, fr); // fork
    line(&mut c, (8.0, 4.0), (12.0, 4.0), fr); // saddle
    line(&mut c, head_top, (18.0, 2.0), fr); // stem
    line(&mut c, (16.0, 2.0), (19.5, 2.0), fr); // bars

    // cargo platform over the front wheel, braced to the frame, and a crate
    line(&mut c, (20.5, 8.5), (31.0, 8.5), ca);
    line(&mut c, (30.5, 8.5), (28.0, 11.5), ca);
    line(&mut c, (22.0, 4.0), (29.0, 4.0), ca);
    line(&mut c, (22.0, 4.0), (22.0, 8.0), ca);
    line(&mut c, (29.0, 4.0), (29.0, 8.0), ca);
    line(&mut c, (22.0, 6.0), (29.0, 6.0), ca);
    c
}

fn line(c: &mut Canvas, (x0, y0): (f32, f32), (x1, y1): (f32, f32), col: [u8; 4]) {
    let n = ((x1 - x0).abs().max((y1 - y0).abs()) * 2.0).ceil().max(1.0) as i32;
    for i in 0..=n {
        let t = i as f32 / n as f32;
        c.put((x0 + (x1 - x0) * t).round() as i32, (y0 + (y1 - y0) * t).round() as i32, col);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wave(levels: &[f32]) -> Waveform {
        let n = crate::waveform::BUCKETS;
        let f = |i: usize| levels[i * levels.len() / n];
        Waveform { peak: (0..n).map(f).collect(), rms: (0..n).map(|i| f(i) * 0.5).collect() }
    }

    fn alpha(c: &Canvas, x: u32, y: u32) -> u8 {
        c.px[((y * c.w + x) * 4 + 3) as usize]
    }

    #[test]
    fn louder_is_taller_and_played_is_opaque() {
        let w = wave(&[0.1, 1.0]);
        let c = render_wave(&w, 100, 40, 50, &Colors::tinted(0xdd6f00, None, 0xa7ff04));
        let height = |x: u32| (0..40).filter(|&y| alpha(&c, x, y) > 0).count();
        assert!(height(80) > height(20) * 2, "loud half is taller");
        assert_eq!(alpha(&c, 20, 20), 255, "played core is solid");
        assert!(alpha(&c, 80, 20) < 255, "unplayed core is translucent");
    }

    #[test]
    fn omnium_fits_and_turns() {
        let a = omnium(0.0, 0xa7ff04, 0xdd6f00);
        let b = omnium(0.25, 0xa7ff04, 0xdd6f00);
        assert_eq!((a.w, a.h), (32, 16));
        assert_ne!(a.px, b.px, "spokes move");
        assert!(a.px.chunks(4).filter(|p| p[3] > 0).count() > 80);
    }
}

