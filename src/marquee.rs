//! Scrolling text on highlighted rows: a field too long for its column
//! scrolls through instead of ending in `…`.
//!
//! One clock drives every scrolling field, and restarts whenever the set of
//! them changes (the cursor or highlight moved). Each pass rests at the
//! start, then scrolls every field round once, all starting together; the
//! longest sets the pace of the passes. Nothing moves while resting, so the
//! main loop doesn't redraw then.

use std::cell::{Cell, RefCell};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::iter;
use std::time::{Duration, Instant};

use crate::config::Style;
use crate::nc;

/// Styled pieces of one line.
pub type Segs = Vec<(String, Style)>;

/// Blank cells between the end of the text and its repeat.
const GAP: usize = 2;
/// How long the fields rest at their start before each pass.
const PAUSE: Duration = Duration::from_millis(250);

pub struct Marquee {
    /// Cells per second; 0 turns scrolling off.
    speed: u32,
    since: Cell<Instant>,
    /// The fields that scrolled in the last frame: (hash, cells per lap).
    shown: RefCell<Vec<(u64, usize)>>,
    /// The fields drawn so far this frame.
    seen: RefCell<Vec<(u64, usize)>>,
}

/// Where the clock is: which pass, and how many cells into it the fields
/// have scrolled (`None` while they rest).
#[derive(Clone, Copy, Debug, PartialEq)]
struct At {
    pass: u64,
    cell: Option<usize>,
    /// How long until `cell` next changes.
    until: Duration,
}

impl Marquee {
    pub fn new(speed: u32) -> Self {
        Self { speed, since: Cell::new(Instant::now()), shown: RefCell::default(), seen: RefCell::default() }
    }

    /// Starts a frame.
    pub fn begin(&self) {
        self.seen.borrow_mut().clear();
    }

    /// Ends a frame: different fields scrolling restart the clock.
    pub fn end(&self) {
        let seen = self.seen.take();
        let mut shown = self.shown.borrow_mut();
        if *shown != seen {
            self.since.set(Instant::now());
            *shown = seen;
        }
    }

    /// `segs` as they should appear in `cols` cells, when too long for
    /// them: a window onto the text at the current scroll offset, exactly
    /// `cols` wide. `None` when they fit or scrolling is off.
    pub fn window(&self, segs: &Segs, cols: usize) -> Option<Segs> {
        let w: usize = segs.iter().map(|(s, _)| nc::width(s)).sum();
        if self.speed == 0 || w <= cols || cols == 0 {
            return None;
        }
        let mut h = DefaultHasher::new();
        segs.iter().for_each(|(s, _)| s.hash(&mut h));
        let field = (h.finish(), w + GAP);
        self.seen.borrow_mut().push(field);
        // A field new this frame starts at the beginning (the clock restarts
        // when the frame ends); one done with its lap waits for the next pass.
        let skip = match self.at() {
            Some(At { cell: Some(c), .. }) if c < field.1 && self.shown.borrow().contains(&field) => c,
            _ => 0,
        };
        Some(window(segs, skip, cols, nc::width))
    }

    /// Changes whenever a scrolling field may have moved, `None` when none
    /// is scrolling: the main loop redraws when it changes.
    pub fn step(&self) -> Option<(u64, Option<usize>)> {
        self.at().map(|a| (a.pass, a.cell))
    }

    /// How long until `step` next changes, if anything is scrolling.
    pub fn next_step(&self) -> Option<Duration> {
        self.at().map(|a| a.until)
    }

    fn at(&self) -> Option<At> {
        let longest = self.shown.borrow().iter().map(|&(_, lap)| lap).max()?;
        (self.speed > 0).then(|| at(self.since.get().elapsed(), longest, self.speed))
    }
}

/// The clock `t` after it started, for passes `longest` cells long.
fn at(t: Duration, longest: usize, speed: u32) -> At {
    let cell = Duration::from_secs(1) / speed;
    let period = PAUSE + cell * longest as u32;
    let pass = (t.as_nanos() / period.as_nanos()) as u64;
    let into = t - period * pass as u32;
    match into.checked_sub(PAUSE) {
        None => At { pass, cell: None, until: PAUSE - into },
        Some(moving) => {
            let n = (moving.as_nanos() / cell.as_nanos()) as usize;
            At { pass, cell: Some(n), until: cell * (n as u32 + 1) - moving }
        }
    }
}

/// `cols` cells of `segs` + gap + `segs`, starting `skip` cells in. A wide
/// character cut by either edge becomes spaces, so the result is always
/// exactly `cols` wide. `width` measures one character (a parameter so
/// tests needn't set a locale).
fn window(segs: &Segs, skip: usize, cols: usize, width: impl Fn(&str) -> usize) -> Segs {
    let Some(&(_, last)) = segs.last() else { return Vec::new() };
    let gap = (" ".repeat(GAP), last);
    let mut out = Segs::new();
    let (mut col, mut used) = (0, 0);
    let mut buf = [0; 4];
    for (s, st) in segs.iter().chain(iter::once(&gap)).chain(segs) {
        let mut piece = String::new();
        for ch in s.chars() {
            if used == cols {
                break;
            }
            let cw = width(ch.encode_utf8(&mut buf));
            let start = col;
            col += cw;
            if col <= skip {
                continue;
            }
            // cut by the left edge, or by the right
            let cut = start < skip || used + cw > cols;
            let n = if cut { (col - start.max(skip)).min(cols - used) } else { cw };
            if cut {
                piece.extend(iter::repeat_n(' ', n));
            } else {
                piece.push(ch);
            }
            used += n;
        }
        if !piece.is_empty() {
            out.push((piece, *st));
        }
        if used == cols {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two cells for the CJK in these tests, one for anything else.
    fn cells(s: &str) -> usize {
        if s.is_ascii() { s.len() } else { 2 * s.chars().count() }
    }

    fn text(segs: &Segs) -> String {
        segs.iter().map(|(s, _)| s.as_str()).collect()
    }

    const PLAIN: Style = Style { fg: nc::Color::Default, bg: nc::Color::Default, bold: false, italic: false };

    fn one(s: &str) -> Segs {
        vec![(s.to_owned(), PLAIN)]
    }

    #[test]
    fn windows_wrap_round_through_the_gap() {
        let s = one("abcdefgh");
        assert_eq!(text(&window(&s, 0, 5, cells)), "abcde");
        assert_eq!(text(&window(&s, 6, 5, cells)), "gh  a");
        assert_eq!(text(&window(&s, 8, 5, cells)), "  abc");
    }

    #[test]
    fn windows_keep_styles_apart() {
        let bold = Style { bold: true, ..PLAIN };
        let segs = vec![("abc".to_owned(), PLAIN), ("def".to_owned(), bold)];
        let w = window(&segs, 1, 4, cells);
        assert_eq!(w.iter().map(|(s, st)| (s.as_str(), st.bold)).collect::<Vec<_>>(), [("bc", false), ("de", true)]);
    }

    #[test]
    fn wide_characters_cut_by_an_edge_become_spaces() {
        let s = one("日本語の歌");
        assert_eq!(text(&window(&s, 1, 4, cells)), " 本 ");
        assert_eq!(text(&window(&s, 0, 3, cells)), "日 ");
    }

    #[test]
    fn rests_then_scrolls_one_lap() {
        // 10 cells/s: 100 ms a cell; a 14-cell pass takes 0.25 s + 1.4 s
        let at = |ms| at(Duration::from_millis(ms), 14, 10);
        assert_eq!(at(0).cell, None);
        assert_eq!(at(150).until, Duration::from_millis(100));
        assert_eq!(at(250).cell, Some(0));
        assert_eq!(at(400), At { pass: 0, cell: Some(1), until: Duration::from_millis(50) });
        assert_eq!(at(250 + 1350).cell, Some(13));
        assert_eq!(at(1650), At { pass: 1, cell: None, until: PAUSE });
    }

    #[test]
    fn short_text_doesnt_scroll() {
        let m = Marquee::new(8);
        assert!(m.window(&one("short"), 10).is_none());
        assert!(Marquee::new(0).window(&one("much too long"), 4).is_none());
    }
}
