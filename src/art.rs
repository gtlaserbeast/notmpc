//! Album art: fetched from MPD, decoded by notcurses (ffmpeg), and drawn on
//! planes of its own so the per-frame redraw of the standard plane doesn't
//! touch it. Pixel graphics when the terminal has them, else character cells.

use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::Result;

use crate::mpd::Conn;
use crate::nc::{Nc, OwnedPlane, Visual};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub y: u32,
    pub x: u32,
    pub rows: u32,
    pub cols: u32,
}

pub struct Art<'nc> {
    nc: &'nc Nc,
    pixel: bool,
    // Fields drop in order, and the image plane is a child of the frame.
    image: Option<OwnedPlane<'nc>>,
    frame: Option<OwnedPlane<'nc>>,
    visual: Option<Visual>,
    /// The song `visual` belongs to.
    song: Option<String>,
    /// Where the art is on screen, if it is.
    shown: Option<Rect>,
}

impl<'nc> Art<'nc> {
    pub fn new(nc: &'nc Nc) -> Self {
        Self {
            nc,
            pixel: nc.can_pixel(),
            image: None,
            frame: None,
            visual: None,
            song: None,
            shown: None,
        }
    }

    /// Fetches the cover for `song` unless it's already loaded.
    pub fn load(&mut self, conn: &mut Conn, song: Option<&str>) -> Result<()> {
        if self.song.as_deref() == song {
            return Ok(());
        }
        self.hide();
        self.song = song.map(str::to_owned);
        self.visual = match song {
            // A cover notcurses can't decode is the same as no cover.
            Some(uri) => conn.picture(uri)?.and_then(|data| decode(&data).ok()),
            None => None,
        };
        Ok(())
    }

    pub fn has_image(&self) -> bool {
        self.visual.is_some()
    }

    /// Draws the cover scaled into `rect`, or hides it for None. Only
    /// re-blits when the rect or the cover changed.
    pub fn show(&mut self, rect: Option<Rect>) -> Result<()> {
        if rect == self.shown {
            return Ok(());
        }
        self.hide();
        if let (Some(r), Some(v)) = (rect, &self.visual) {
            let frame = self.nc.new_plane(r.y, r.x, r.rows, r.cols)?;
            self.image = Some(self.nc.blit_into(v, &frame, self.pixel)?);
            self.frame = Some(frame);
            self.shown = rect;
        }
        Ok(())
    }

    fn hide(&mut self) {
        self.image = None;
        self.frame = None;
        self.shown = None;
    }
}

/// notcurses 3.0.8 can only decode from a file, so stage the bytes in one.
fn decode(data: &[u8]) -> Result<Visual> {
    let dir = env::var_os("XDG_RUNTIME_DIR").map_or_else(env::temp_dir, PathBuf::from);
    let path = dir.join(format!("notmpc-{}.cover", std::process::id()));
    fs::write(&path, data)?;
    let v = Visual::from_file(&path);
    let _ = fs::remove_file(&path);
    v
}
