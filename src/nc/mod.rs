//! A thin safe layer over the notcurses calls notmpc uses.
//!
//! Only what the app needs lives here; reach into `sys` for anything else
//! and wrap it once it's used in more than one place.
//!
//! [`Nc`] methods take `&self` so planes can borrow the context: the borrow
//! checker then guarantees every [`OwnedPlane`] is destroyed before
//! `notcurses_stop`. notcurses isn't thread-safe, and the raw pointers keep
//! these types `!Send`/`!Sync`.

pub mod sys;

use std::ffi::CString;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::ptr;
use std::time::Duration;

use anyhow::{Result, bail};

/// Synthesized key codes. notcurses defines these with the function-like
/// `preterunicode()` macro, which bindgen can't evaluate.
#[allow(dead_code)] // the rest are for the library view
pub mod key {
    const BASE: u32 = 1_115_000;
    pub const RESIZE: u32 = BASE + 1;
    pub const UP: u32 = BASE + 2;
    pub const RIGHT: u32 = BASE + 3;
    pub const DOWN: u32 = BASE + 4;
    pub const LEFT: u32 = BASE + 5;
    pub const DEL: u32 = BASE + 7;
    pub const PGDOWN: u32 = BASE + 9;
    pub const PGUP: u32 = BASE + 10;
    pub const HOME: u32 = BASE + 11;
    pub const END: u32 = BASE + 12;
    pub const ENTER: u32 = BASE + 121;
    pub const ESC: u32 = super::sys::NCKEY_ESC;
    pub const TAB: u32 = super::sys::NCKEY_TAB;
}

pub use sys::{NCSTYLE_BOLD as BOLD, NCSTYLE_ITALIC as ITALIC};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Center,
    Right,
}

impl Align {
    fn raw(self) -> sys::ncalign_e {
        match self {
            Align::Center => sys::ncalign_e_NCALIGN_CENTER,
            Align::Right => sys::ncalign_e_NCALIGN_RIGHT,
        }
    }
}

/// A key press (releases and repeats are filtered out by [`Nc::get`]).
#[derive(Clone, Copy, Debug)]
pub struct Input {
    pub id: u32,
    pub ctrl: bool,
}

impl Input {
    pub fn is_char(&self, c: char) -> bool {
        !self.ctrl && self.id == c as u32
    }
}

pub struct Nc {
    raw: *mut sys::notcurses,
}

impl Nc {
    pub fn new() -> Result<Self> {
        let opts = sys::notcurses_options {
            flags: sys::NCOPTION_SUPPRESS_BANNERS as u64,
            ..Default::default()
        };
        // SAFETY: opts is valid for the call; a null FILE* means stdout.
        let raw = unsafe { sys::notcurses_init(&opts, ptr::null_mut()) };
        if raw.is_null() {
            bail!("notcurses_init failed");
        }
        Ok(Self { raw })
    }

    pub fn stdplane(&self) -> Plane<'_> {
        // SAFETY: raw is a live notcurses context; the plane is owned by it.
        let raw = unsafe { sys::notcurses_stdplane(self.raw) };
        Plane { raw, _nc: PhantomData }
    }

    /// Creates a plane at (y, x) relative to the standard plane.
    pub fn new_plane(&self, y: u32, x: u32, rows: u32, cols: u32) -> Result<OwnedPlane<'_>> {
        let opts = sys::ncplane_options {
            y: y as i32,
            x: x as i32,
            rows,
            cols,
            ..Default::default()
        };
        // SAFETY: the standard plane is live; opts is valid for the call.
        let raw = unsafe { sys::ncplane_create(self.stdplane().raw, &opts) };
        if raw.is_null() {
            bail!("ncplane_create failed");
        }
        Ok(OwnedPlane(Plane { raw, _nc: PhantomData }))
    }

    /// Whether the terminal supports pixel graphics (kitty, sixel, ...).
    pub fn can_pixel(&self) -> bool {
        // SAFETY: raw is a live notcurses context.
        unsafe { sys::notcurses_check_pixel_support(self.raw) != sys::ncpixelimpl_e_NCPIXEL_NONE }
    }

    /// Scales `v` to fit `parent`, centred, in a new child plane.
    pub fn blit_into<'a>(&'a self, v: &Visual, parent: &Plane<'a>, pixel: bool) -> Result<OwnedPlane<'a>> {
        let opts = sys::ncvisual_options {
            n: parent.raw,
            scaling: sys::ncscale_e_NCSCALE_SCALE,
            y: sys::ncalign_e_NCALIGN_CENTER as i32,
            x: sys::ncalign_e_NCALIGN_CENTER as i32,
            blitter: if pixel { sys::ncblitter_e_NCBLIT_PIXEL } else { sys::ncblitter_e_NCBLIT_DEFAULT },
            flags: (sys::NCVISUAL_OPTION_CHILDPLANE
                | sys::NCVISUAL_OPTION_HORALIGNED
                | sys::NCVISUAL_OPTION_VERALIGNED) as u64,
            ..Default::default()
        };
        // SAFETY: raw, v and parent are live; opts is valid for the call.
        let raw = unsafe { sys::ncvisual_blit(self.raw, v.raw, &opts) };
        if raw.is_null() {
            bail!("ncvisual_blit failed");
        }
        Ok(OwnedPlane(Plane { raw, _nc: PhantomData }))
    }

    pub fn render(&self) -> Result<()> {
        // SAFETY: raw is a live notcurses context.
        if unsafe { sys::notcurses_render(self.raw) } != 0 {
            bail!("notcurses_render failed");
        }
        Ok(())
    }

    /// Picks up a new terminal size after `key::RESIZE`.
    pub fn refresh(&self) -> Result<()> {
        // SAFETY: raw is live; null out-params are allowed.
        if unsafe { sys::notcurses_refresh(self.raw, ptr::null_mut(), ptr::null_mut()) } != 0 {
            bail!("notcurses_refresh failed");
        }
        Ok(())
    }

    /// Waits up to `timeout` for a key press.
    pub fn get(&self, timeout: Duration) -> Result<Option<Input>> {
        let ts = sys::timespec {
            tv_sec: timeout.as_secs() as _,
            tv_nsec: timeout.subsec_nanos() as _,
        };
        let mut ni = sys::ncinput::default();
        // SAFETY: raw is live; ts and ni outlive the call.
        let id = unsafe { sys::notcurses_get(self.raw, &ts, &mut ni) };
        match id {
            0 => Ok(None),
            u32::MAX => bail!("notcurses_get failed"),
            // Terminals speaking the kitty keyboard protocol report releases too.
            _ if ni.evtype == sys::ncintype_e_NCTYPE_RELEASE => Ok(None),
            _ => Ok(Some(Input {
                id,
                ctrl: ni.modifiers & sys::NCKEY_MOD_CTRL != 0,
            })),
        }
    }
}

impl Drop for Nc {
    fn drop(&mut self) {
        // SAFETY: raw came from notcurses_init and is stopped exactly once.
        unsafe { sys::notcurses_stop(self.raw) };
    }
}

/// A plane borrowed from its notcurses context.
pub struct Plane<'a> {
    raw: *mut sys::ncplane,
    _nc: PhantomData<&'a Nc>,
}

impl Plane<'_> {
    /// Returns (rows, cols).
    pub fn dim_yx(&self) -> (u32, u32) {
        let (mut y, mut x) = (0, 0);
        // SAFETY: raw is a live plane; out-params are valid.
        unsafe { sys::ncplane_dim_yx(self.raw, &mut y, &mut x) };
        (y, x)
    }

    pub fn erase(&mut self) {
        // SAFETY: raw is a live plane.
        unsafe { sys::ncplane_erase(self.raw) };
    }

    pub fn set_fg_rgb(&mut self, rgb: u32) {
        // SAFETY: raw is a live plane.
        unsafe { sys::ncplane_set_fg_rgb(self.raw, rgb) };
    }

    pub fn set_bg_rgb(&mut self, rgb: u32) {
        // SAFETY: raw is a live plane.
        unsafe { sys::ncplane_set_bg_rgb(self.raw, rgb) };
    }

    pub fn set_fg_default(&mut self) {
        // SAFETY: raw is a live plane.
        unsafe { sys::ncplane_set_fg_default(self.raw) };
    }

    pub fn set_bg_default(&mut self) {
        // SAFETY: raw is a live plane.
        unsafe { sys::ncplane_set_bg_default(self.raw) };
    }

    pub fn set_styles(&mut self, styles: u32) {
        // SAFETY: raw is a live plane.
        unsafe { sys::ncplane_set_styles(self.raw, styles) };
    }

    /// Writes `s` at (y, x). Text past the right edge is dropped.
    pub fn putstr_yx(&mut self, y: u32, x: u32, s: &str) {
        let Ok(s) = CString::new(s) else { return };
        // SAFETY: raw is a live plane; s is NUL-terminated and outlives the call.
        unsafe { sys::ncplane_putstr_yx(self.raw, y as i32, x as i32, s.as_ptr()) };
    }

    pub fn putstr_aligned(&mut self, y: u32, align: Align, s: &str) {
        let Ok(s) = CString::new(s) else { return };
        // SAFETY: as in putstr_yx.
        unsafe { sys::ncplane_putstr_aligned(self.raw, y as i32, align.raw(), s.as_ptr()) };
    }
}

/// A plane notmpc created, destroyed on drop.
///
/// notcurses 3.0.8 has no `ncplane_family_destroy`, so drop children before
/// their parent (declare them first in a struct).
pub struct OwnedPlane<'a>(Plane<'a>);

impl<'a> Deref for OwnedPlane<'a> {
    type Target = Plane<'a>;
    fn deref(&self) -> &Plane<'a> {
        &self.0
    }
}

impl DerefMut for OwnedPlane<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for OwnedPlane<'_> {
    fn drop(&mut self) {
        // SAFETY: the plane is live (its context outlives 'a) and destroyed once.
        unsafe { sys::ncplane_destroy(self.0.raw) };
    }
}

/// A decoded image. Independent of any notcurses context.
pub struct Visual {
    raw: *mut sys::ncvisual,
}

impl Visual {
    /// Decodes any format notcurses' ffmpeg backend understands.
    pub fn from_file(path: &Path) -> Result<Self> {
        let Some(p) = path.to_str().and_then(|p| CString::new(p).ok()) else {
            bail!("unusable path {}", path.display());
        };
        // SAFETY: p is NUL-terminated and outlives the call.
        let raw = unsafe { sys::ncvisual_from_file(p.as_ptr()) };
        if raw.is_null() {
            bail!("couldn't decode {}", path.display());
        }
        Ok(Self { raw })
    }
}

impl Drop for Visual {
    fn drop(&mut self) {
        // SAFETY: raw came from ncvisual_from_file and is destroyed once.
        unsafe { sys::ncvisual_destroy(self.raw) };
    }
}

/// Display width of `s` in cells, as notcurses will lay it out.
pub fn width(s: &str) -> usize {
    let Ok(c) = CString::new(s) else { return 0 };
    // SAFETY: c is NUL-terminated; the out-params may be null.
    let w = unsafe { sys::ncstrwidth(c.as_ptr(), ptr::null_mut(), ptr::null_mut()) };
    w.max(0) as usize
}

/// Truncates `s` to at most `cols` cells, marking the cut with `…`.
pub fn fit(s: &str, cols: usize) -> String {
    if width(s) <= cols {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    let mut buf = [0; 4];
    for ch in s.chars() {
        let w = width(ch.encode_utf8(&mut buf));
        if used + w + 1 > cols {
            break;
        }
        out.push(ch);
        used += w;
    }
    if cols > 0 {
        out.push('…');
    }
    out
}
