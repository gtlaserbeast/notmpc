//! A thin safe layer over the notcurses calls notmpc uses.
//!
//! Only what the app needs lives here; reach into `sys` for anything else
//! and wrap it once it's used in more than one place.

pub mod sys;

use std::ffi::CString;
use std::marker::PhantomData;
use std::ptr;
use std::time::Duration;

use anyhow::{Result, bail};

/// Synthesized key codes. notcurses defines these with the function-like
/// `preterunicode()` macro, which bindgen can't evaluate.
#[allow(dead_code)] // the rest are for the queue/library views
pub mod key {
    const BASE: u32 = 1_115_000;
    pub const RESIZE: u32 = BASE + 1;
    pub const UP: u32 = BASE + 2;
    pub const RIGHT: u32 = BASE + 3;
    pub const DOWN: u32 = BASE + 4;
    pub const LEFT: u32 = BASE + 5;
    pub const ENTER: u32 = BASE + 121;
    pub const ESC: u32 = super::sys::NCKEY_ESC;
}

pub use sys::{NCSTYLE_BOLD as BOLD, NCSTYLE_ITALIC as ITALIC};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Align {
    Left,
    Center,
    Right,
}

impl Align {
    fn raw(self) -> sys::ncalign_e {
        match self {
            Align::Left => sys::ncalign_e_NCALIGN_LEFT,
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

    pub fn stdplane(&mut self) -> Plane<'_> {
        // SAFETY: raw is a live notcurses context; the plane is owned by it.
        let raw = unsafe { sys::notcurses_stdplane(self.raw) };
        Plane { raw, _nc: PhantomData }
    }

    pub fn render(&mut self) -> Result<()> {
        // SAFETY: raw is a live notcurses context.
        if unsafe { sys::notcurses_render(self.raw) } != 0 {
            bail!("notcurses_render failed");
        }
        Ok(())
    }

    /// Picks up a new terminal size after `key::RESIZE`.
    pub fn refresh(&mut self) -> Result<()> {
        // SAFETY: raw is live; null out-params are allowed.
        if unsafe { sys::notcurses_refresh(self.raw, ptr::null_mut(), ptr::null_mut()) } != 0 {
            bail!("notcurses_refresh failed");
        }
        Ok(())
    }

    /// Waits up to `timeout` for a key press.
    pub fn get(&mut self, timeout: Duration) -> Result<Option<Input>> {
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
    _nc: PhantomData<&'a mut Nc>,
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

    pub fn set_fg_default(&mut self) {
        // SAFETY: raw is a live plane.
        unsafe { sys::ncplane_set_fg_default(self.raw) };
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
