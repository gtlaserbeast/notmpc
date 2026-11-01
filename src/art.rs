//! Album art: fetched from MPD, decoded by notcurses (ffmpeg), and drawn on
//! planes of its own so the per-frame redraw of the standard plane doesn't
//! touch it. Pixel graphics when the terminal has them, else character
//! cells; or, by choice, half blocks or braille (`art_style`).

use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::Result;

use crate::config::ArtStyle;
use crate::mpd::Conn;
use crate::nc::{Blitter, Color, Nc, OwnedPlane, Visual};
use crate::textart::{self, Image};

/// Braille needs far fewer pixels than the screen shows: a few dots per
/// cell. Decoding smaller keeps reading the pixels out cheap.
const BRAILLE_MAX_PX: u32 = 400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub y: u32,
    pub x: u32,
    pub rows: u32,
    pub cols: u32,
}

pub struct Art<'nc> {
    nc: &'nc Nc,
    style: ArtStyle,
    /// How notcurses draws it, unless it's braille (which we draw).
    blitter: Blitter,
    /// Largest side a cover is decoded at.
    max_px: u32,
    // Fields drop in order, and the image plane is a child of the frame.
    image: Option<OwnedPlane<'nc>>,
    frame: Option<OwnedPlane<'nc>>,
    visual: Option<Visual>,
    /// The cover's pixels, for braille (instead of `visual`).
    pixels: Option<Image>,
    /// What `visual` belongs to (a song URI, or any caller-chosen key).
    song: Option<String>,
    /// Where the art is on screen, if it is.
    shown: Option<Rect>,
}

impl<'nc> Art<'nc> {
    /// Covers are decoded at most `max_px` pixels a side.
    pub fn new(nc: &'nc Nc, max_px: u32, style: ArtStyle) -> Self {
        let blitter = match style {
            ArtStyle::Pixels if nc.can_pixel() => Blitter::Pixel,
            ArtStyle::Blocks => Blitter::HalfBlocks,
            _ => Blitter::Cells,
        };
        let max_px = if style == ArtStyle::Braille { max_px.min(BRAILLE_MAX_PX) } else { max_px };
        Self {
            nc,
            style,
            blitter,
            max_px,
            image: None,
            frame: None,
            visual: None,
            pixels: None,
            song: None,
            shown: None,
        }
    }

    /// Keeps a decoded cover in the form `show` draws from.
    fn keep(&mut self, v: Option<Visual>) {
        if self.style == ArtStyle::Braille {
            self.pixels = v.and_then(|v| v.image());
            self.visual = None;
        } else {
            self.visual = v;
            self.pixels = None;
        }
    }

    /// Fetches the cover for `song` unless it's already loaded. If that
    /// fails the song shows no cover (not the last one's), and isn't
    /// retried until it changes.
    pub fn load(&mut self, conn: &mut Conn, song: Option<&str>) -> Result<()> {
        if self.song.as_deref() == song {
            return Ok(());
        }
        self.hide();
        self.song = song.map(str::to_owned);
        self.keep(None);
        if let Some(uri) = song {
            let v = conn.picture(uri)?.and_then(|data| decode(&data, self.max_px));
            self.keep(v);
        }
        Ok(())
    }

    /// Shows a cover from already-fetched image bytes, identified by `key`;
    /// decoded only when the key changes.
    pub fn set(&mut self, key: &str, data: Option<&[u8]>) {
        if self.song.as_deref() == Some(key) {
            return;
        }
        self.hide();
        self.song = Some(key.to_owned());
        let v = data.and_then(|d| decode(d, self.max_px));
        self.keep(v);
    }

    pub fn has_image(&self) -> bool {
        self.visual.is_some() || self.pixels.is_some()
    }

    /// Draws the cover scaled into `rect`, or hides it for None. Only
    /// re-blits when the rect or the cover changed.
    pub fn show(&mut self, rect: Option<Rect>) -> Result<()> {
        if rect == self.shown {
            return Ok(());
        }
        self.hide();
        let Some(r) = rect.filter(|_| self.has_image()) else { return Ok(()) };
        let mut frame = self.nc.new_plane(r.y, r.x, r.rows, r.cols)?;
        if let Some(img) = &self.pixels {
            let cell_px = self.nc.cell_px().unwrap_or((20, 10));
            for c in textart::braille(img, r.cols, r.rows, cell_px) {
                frame.set_fg(Color::Rgb(c.rgb));
                frame.putstr_yx(c.row, c.col, c.ch.encode_utf8(&mut [0; 4]));
            }
        } else if let Some(v) = &self.visual {
            self.image = Some(self.nc.blit_into(v, &frame, self.blitter)?);
        }
        self.frame = Some(frame);
        self.shown = rect;
        Ok(())
    }

    /// Drops the image so the next `show` re-sends it (after the terminal
    /// cleared the screen, say).
    pub fn hide(&mut self) {
        self.image = None;
        self.frame = None;
        self.shown = None;
    }
}

/// Largest side kept for the queue's cover (enough for a big pane on a 4K
/// screen) and for the Directories preview (a smaller pane).
pub const MAX_PX: u32 = 1200;
pub const PREVIEW_MAX_PX: u32 = 640;

/// Decodes image bytes, shrunk to at most `max` pixels a side. A cover
/// notcurses can't decode is the same as no cover.
fn decode(data: &[u8], max: u32) -> Option<Visual> {
    let mut v = decode_file(data).ok()?;
    v.downscale(max);
    Some(v)
}

/// notcurses 3.0.8 can only decode from a file, so stage the bytes in one.
fn decode_file(data: &[u8]) -> Result<Visual> {
    let dir = env::var_os("XDG_RUNTIME_DIR").map_or_else(env::temp_dir, PathBuf::from);
    let path = dir.join(format!("notmpc-{}.cover", std::process::id()));
    fs::write(&path, data)?;
    let v = Visual::from_file(&path);
    let _ = fs::remove_file(&path);
    v
}
