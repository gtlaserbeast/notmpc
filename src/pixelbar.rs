//! The progress bar in pixels: a thin rounded track, the played part in
//! one colour, a round thumb at the playhead. One small image on its own
//! plane, re-sent only when the thumb moves a pixel (at most once per
//! second of playback, since the caller passes whole seconds).

use anyhow::Result;

use crate::nc::{Nc, OwnedPlane, Visual};

pub struct Colors {
    pub played: u32,
    pub track: u32,
    pub thumb: u32,
}

pub struct PixelBar<'nc> {
    nc: &'nc Nc,
    plane: Option<OwnedPlane<'nc>>,
    /// (y, cols, played px, thumb shown) of what's on screen.
    drawn: Option<(u32, u32, u32, bool)>,
}

impl<'nc> PixelBar<'nc> {
    /// None without pixel graphics: the caller draws the symbol bar.
    pub fn new(nc: &'nc Nc) -> Option<Self> {
        nc.can_pixel().then_some(Self { nc, plane: None, drawn: None })
    }

    pub fn hide(&mut self) {
        self.plane = None;
        self.drawn = None;
    }

    /// Draws the bar across row `y` with `frac` played.
    pub fn draw(&mut self, y: u32, cols: u32, frac: f32, thumb: bool, colors: &Colors) -> Result<()> {
        let (ch, cw) = self.nc.cell_px().unwrap_or((20, 10));
        let (w, h) = (cols * cw, ch);
        let played = (frac.clamp(0.0, 1.0) * w as f32) as u32;
        let key = (y, cols, played, thumb);
        if self.drawn == Some(key) {
            return Ok(());
        }
        if self.plane.as_ref().is_none_or(|p| p.dim_yx() != (1, cols)) || self.drawn.is_some_and(|d| d.0 != y) {
            let mut p = self.nc.new_plane(y, 0, 1, cols)?;
            p.move_top();
            self.plane = Some(p);
        }
        let px = render(w, h, played, thumb, colors);
        let v = Visual::from_rgba(&px, h, w)?;
        self.nc.blit_onto(&v, self.plane.as_mut().expect("created above"))?;
        self.drawn = Some(key);
        Ok(())
    }
}

/// RGBA pixels: transparent around a track a fifth of the cell tall, the
/// thumb a circle 60% of it.
fn render(w: u32, h: u32, played: u32, thumb: bool, c: &Colors) -> Vec<u8> {
    let mut px = vec![0u8; (w * h * 4) as usize];
    let mid = h as f32 / 2.0;
    let half = (h as f32 / 10.0).max(1.0);
    let r = h as f32 * 0.3;
    let tx = (played as f32).clamp(r, w as f32 - r);
    for y in 0..h {
        let dy = y as f32 + 0.5 - mid;
        for x in 0..w {
            let fx = x as f32 + 0.5;
            // rounded ends: distance to the track's centre line segment
            let ex = fx.clamp(half, w as f32 - half);
            let on_track = (fx - ex).hypot(dy) <= half;
            let on_thumb = thumb && (fx - tx).hypot(dy) <= r;
            let rgb = if on_thumb {
                c.thumb
            } else if on_track {
                if x < played { c.played } else { c.track }
            } else {
                continue;
            };
            let i = ((y * w + x) * 4) as usize;
            px[i..i + 4].copy_from_slice(&[(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 255]);
        }
    }
    px
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn played_track_and_thumb() {
        let c = Colors { played: 0xdd6f00, track: 0x444444, thumb: 0xa7ff04 };
        let (w, h) = (200, 20);
        let px = render(w, h, 100, true, &c);
        let at = |x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            u32::from_be_bytes([0, px[i], px[i + 1], px[i + 2]]) | if px[i + 3] == 0 { 0xff00_0000 } else { 0 }
        };
        assert_eq!(at(20, 10), 0xdd6f00, "played");
        assert_eq!(at(180, 10), 0x444444, "unplayed");
        assert_eq!(at(100, 10), 0xa7ff04, "thumb");
        assert_eq!(at(20, 1) >> 24, 0xff, "transparent above the track");
    }
}
