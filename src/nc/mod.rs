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
    pub const BACKSPACE: u32 = BASE + 8;
    pub const PGDOWN: u32 = BASE + 9;
    pub const PGUP: u32 = BASE + 10;
    pub const HOME: u32 = BASE + 11;
    pub const END: u32 = BASE + 12;
    pub const ENTER: u32 = BASE + 121;
    pub const BUTTON1: u32 = BASE + 201;
    pub const BUTTON11: u32 = BASE + 211;
    pub const ESC: u32 = super::sys::NCKEY_ESC;
    pub const TAB: u32 = super::sys::NCKEY_TAB;
    pub const SPACE: u32 = super::sys::NCKEY_SPACE;

    /// Whether `id` is a printable character rather than a synthesized key.
    pub fn is_printable(id: u32) -> bool {
        (0x20..BASE).contains(&id) && id != 0x7f
    }
}

/// A colour: truecolor, an index into the terminal's palette (so `yellow`
/// matches the terminal theme), or the terminal default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Default,
    Rgb(u32),
    Palette(u8),
}

pub use sys::{NCSTYLE_BOLD as BOLD, NCSTYLE_ITALIC as ITALIC};

/// What [`Nc::get`] delivers.
#[derive(Clone, Copy, Debug)]
pub enum Event {
    Key(Input),
    Mouse(Mouse),
}

/// A mouse button press at a cell. Buttons: 1 left, 2 middle, 3 right,
/// 4/5 wheel up/down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mouse {
    pub button: u32,
    pub y: u32,
    pub x: u32,
}

/// A key press (releases are filtered out by [`Nc::get`]).
///
/// Normalized so legacy and kitty-protocol terminals agree: Ctrl+letter is
/// lowercase with `ctrl`, Shift+letter is the uppercase letter, and
/// Enter/Backspace are always `key::ENTER`/`key::BACKSPACE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Input {
    pub id: u32,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Input {
    fn normalize(mut self) -> Self {
        match self.id {
            0x0a | 0x0d => self.id = key::ENTER,
            0x08 | 0x7f => self.id = key::BACKSPACE,
            _ => {}
        }
        if let Some(c) = char::from_u32(self.id).filter(char::is_ascii_alphabetic) {
            if self.ctrl {
                self.id = c.to_ascii_lowercase() as u32;
            } else if self.shift {
                self.id = c.to_ascii_uppercase() as u32;
            }
        }
        // Shift is already part of a printable character.
        if key::is_printable(self.id) && !self.ctrl {
            self.shift = false;
        }
        self
    }
}

pub struct Nc {
    raw: *mut sys::notcurses,
    /// The terminal's default foreground. Glyphs in the "default" colour
    /// are see-through when composited over another plane, so planes draw
    /// default-coloured text in this instead.
    default_fg: Color,
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
        let mut rgb = 0;
        // SAFETY: raw is live; rgb is a valid out-param.
        let default_fg = if unsafe { sys::notcurses_default_foreground(raw, &mut rgb) } == 0 {
            Color::Rgb(rgb)
        } else {
            Color::Palette(7)
        };
        Ok(Self { raw, default_fg })
    }

    pub fn stdplane(&self) -> Plane<'_> {
        // SAFETY: raw is a live notcurses context; the plane is owned by it.
        let raw = unsafe { sys::notcurses_stdplane(self.raw) };
        Plane { raw, default_fg: self.default_fg, _nc: PhantomData }
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
        Ok(OwnedPlane(Plane { raw, default_fg: self.default_fg, _nc: PhantomData }))
    }

    /// Reports button presses and the wheel as [`Event::Mouse`].
    pub fn enable_mouse(&self) {
        // SAFETY: raw is a live notcurses context. Failure just means no mouse.
        unsafe { sys::notcurses_mice_enable(self.raw, sys::NCMICE_BUTTON_EVENT) };
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
        Ok(OwnedPlane(Plane { raw, default_fg: self.default_fg, _nc: PhantomData }))
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

    /// Waits up to `timeout` for a key press or mouse click.
    pub fn get(&self, timeout: Duration) -> Result<Option<Event>> {
        // notcurses wants an absolute CLOCK_MONOTONIC deadline, not a duration.
        let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: now is a valid out-param.
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
        let nsec = now.tv_nsec as u64 + u64::from(timeout.subsec_nanos());
        let ts = sys::timespec {
            tv_sec: (now.tv_sec as u64 + timeout.as_secs() + nsec / 1_000_000_000) as _,
            tv_nsec: (nsec % 1_000_000_000) as _,
        };
        let mut ni = sys::ncinput::default();
        // SAFETY: raw is live; ts and ni outlive the call.
        let id = unsafe { sys::notcurses_get(self.raw, &ts, &mut ni) };
        match id {
            0 => Ok(None),
            u32::MAX => bail!("notcurses_get failed"),
            // Terminals speaking the kitty keyboard protocol report releases too.
            _ if ni.evtype == sys::ncintype_e_NCTYPE_RELEASE => Ok(None),
            key::BUTTON1..=key::BUTTON11 => Ok((ni.y >= 0 && ni.x >= 0).then_some(Event::Mouse(Mouse {
                button: id - key::BUTTON1 + 1,
                y: ni.y as u32,
                x: ni.x as u32,
            }))),
            _ => Ok(Some(Event::Key(
                Input {
                    id,
                    ctrl: ni.modifiers & sys::NCKEY_MOD_CTRL != 0,
                    alt: ni.modifiers & sys::NCKEY_MOD_ALT != 0,
                    shift: ni.modifiers & sys::NCKEY_MOD_SHIFT != 0,
                }
                .normalize(),
            ))),
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
    default_fg: Color,
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

    /// Sets an opaque foreground; `Default` means the terminal's actual
    /// default colour (see `Nc::default_fg`). Setting a colour alone keeps
    /// the alpha, and `erase` resets it to the base cell's.
    pub fn set_fg(&mut self, c: Color) {
        // SAFETY: raw is a live plane.
        unsafe {
            sys::ncplane_set_fg_alpha(self.raw, sys::NCALPHA_OPAQUE as i32);
            let c = if c == Color::Default { self.default_fg } else { c };
            match c {
                Color::Default => sys::ncplane_set_fg_default(self.raw),
                Color::Rgb(rgb) => _ = sys::ncplane_set_fg_rgb(self.raw, rgb),
                Color::Palette(i) => _ = sys::ncplane_set_fg_palindex(self.raw, i.into()),
            }
        }
    }

    /// Sets an opaque background (see `set_fg`).
    pub fn set_bg(&mut self, c: Color) {
        // SAFETY: raw is a live plane.
        unsafe {
            sys::ncplane_set_bg_alpha(self.raw, sys::NCALPHA_OPAQUE as i32);
            match c {
                Color::Default => sys::ncplane_set_bg_default(self.raw),
                Color::Rgb(rgb) => _ = sys::ncplane_set_bg_rgb(self.raw, rgb),
                Color::Palette(i) => _ = sys::ncplane_set_bg_palindex(self.raw, i.into()),
            }
        }
    }

    /// Raises the plane above every other.
    pub fn move_top(&mut self) {
        // SAFETY: raw is a live plane.
        unsafe { sys::ncplane_move_top(self.raw) };
    }

    /// Makes the plane see-through wherever nothing is drawn, so planes
    /// below show (glyph and background).
    pub fn set_base_transparent(&mut self) {
        const T: u64 = sys::NCALPHA_TRANSPARENT as u64;
        // SAFETY: raw is a live plane; the EGC is a valid empty C string.
        unsafe { sys::ncplane_set_base(self.raw, c"".as_ptr(), 0, (T << 32) | T) };
    }

    /// Text drawn next keeps whatever background the planes below have.
    pub fn set_bg_transparent(&mut self) {
        // SAFETY: raw is a live plane.
        unsafe { sys::ncplane_set_bg_alpha(self.raw, sys::NCALPHA_TRANSPARENT as i32) };
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
    if s.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        return s.len();
    }
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
