//! Moving pictures for kitty, sent around notcurses: each frame goes in a
//! POSIX shared-memory object whose name is all that crosses the terminal
//! (kitty reads the pixels from it and unlinks it), placed over a block of
//! cells that kitty scales it to on the GPU. notcurses would encode every
//! frame as base64 at full screen resolution instead.
//!
//! Only for a local kitty: shared memory doesn't cross ssh. If kitty
//! leaves the objects unread, `show` reports it and the caller falls back
//! to notcurses.

use std::collections::VecDeque;
use std::ffi::CString;
use std::io;

use anyhow::{Result, bail};

/// Frames whose objects may still be waiting for kitty to read them.
const IN_FLIGHT: usize = 4;
/// Old frames found unread, in a row, before giving up on kitty.
const UNREAD_LIMIT: u32 = 3;

pub struct KittyImage {
    id: u32,
    seq: u64,
    /// Objects sent and maybe not yet read, oldest first.
    sent: VecDeque<CString>,
    unread: u32,
    shown: bool,
}

impl KittyImage {
    pub fn new(id: u32) -> Self {
        Self { id, seq: 0, sent: VecDeque::new(), unread: 0, shown: false }
    }

    /// Shows `rgba` (`w`×`h` pixels) scaled over `rows`×`cols` cells at
    /// (`y`, `x`), below the text, replacing the previous frame.
    #[allow(clippy::too_many_arguments)]
    pub fn show(&mut self, rgba: &[u8], w: u32, h: u32, y: u32, x: u32, rows: u32, cols: u32) -> Result<()> {
        let name = CString::new(format!("/notmpc-{}-{}-{}", std::process::id(), self.id, self.seq))?;
        self.seq += 1;
        write_shm(&name, rgba)?;
        let cmd = format!(
            // save the cursor, go to the corner, place without moving it, restore
            "\x1b7\x1b[{};{}H\x1b_Ga=T,q=2,f=32,t=s,s={w},v={h},S={},i={},p=1,c={cols},r={rows},C=1,z=-1;{}\x1b\\\x1b8",
            y + 1,
            x + 1,
            rgba.len(),
            self.id,
            base64(name.as_bytes()),
        );
        write_tty(cmd.as_bytes())?;
        self.shown = true;
        self.sent.push_back(name);
        while self.sent.len() > IN_FLIGHT {
            let old = self.sent.pop_front().expect("non-empty");
            // kitty unlinks what it read; one still here was never read
            // SAFETY: old is NUL-terminated.
            if unsafe { libc::shm_unlink(old.as_ptr()) } == 0 {
                self.unread += 1;
                if self.unread >= UNREAD_LIMIT {
                    bail!("kitty isn't reading shared-memory images");
                }
            } else {
                self.unread = 0;
            }
        }
        Ok(())
    }

    /// Removes the picture (if shown) and any frames kitty hasn't read.
    pub fn hide(&mut self) {
        if self.shown {
            let _ = write_tty(format!("\x1b_Ga=d,d=I,i={},q=2\x1b\\", self.id).as_bytes());
            self.shown = false;
        }
        for name in self.sent.drain(..) {
            // SAFETY: name is NUL-terminated; a read one is already gone.
            unsafe { libc::shm_unlink(name.as_ptr()) };
        }
    }
}

impl Drop for KittyImage {
    fn drop(&mut self) {
        self.hide();
    }
}

fn write_shm(name: &CString, data: &[u8]) -> Result<()> {
    // SAFETY: name is NUL-terminated; the fd is closed below on every path.
    let fd = unsafe { libc::shm_open(name.as_ptr(), libc::O_CREAT | libc::O_EXCL | libc::O_RDWR, 0o600) };
    if fd < 0 {
        return Err(io::Error::last_os_error().into());
    }
    let done = (|| {
        // SAFETY: fd is open.
        if unsafe { libc::ftruncate(fd, data.len() as libc::off_t) } != 0 {
            return Err(io::Error::last_os_error());
        }
        write_all(fd, data)
    })();
    // SAFETY: fd is open and closed once.
    unsafe { libc::close(fd) };
    if let Err(e) = done {
        // SAFETY: name is NUL-terminated.
        unsafe { libc::shm_unlink(name.as_ptr()) };
        return Err(e.into());
    }
    Ok(())
}

/// Straight to the terminal (stdout), after whatever notcurses flushed.
fn write_tty(data: &[u8]) -> io::Result<()> {
    write_all(libc::STDOUT_FILENO, data)
}

fn write_all(fd: libc::c_int, mut data: &[u8]) -> io::Result<()> {
    while !data.is_empty() {
        // SAFETY: data is valid for data.len() bytes.
        let n = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        data = &data[n as usize..];
    }
    Ok(())
}

fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_pads() {
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"/notmpc-1"), "L25vdG1wYy0x");
    }
}
