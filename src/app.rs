//! App state, key handling, and the two views.

use std::time::{Duration, Instant};

use anyhow::Result;

use crate::art::{Art, Rect};
use crate::mpd::{Conn, Song, State, Status};
use crate::nc::{self, Align, Input, Nc, Plane, key};

const ACCENT: u32 = 0x89b4fa;
const DIM: u32 = 0x6c7086;
const TEXT: u32 = 0xcdd6f4;
const SELECTED: u32 = 0x313244;

/// Idle subsystems that mean the queue changed / status may have changed.
const QUEUE_EVENTS: &[&str] = &["playlist"];
const STATUS_EVENTS: &[&str] = &["player", "mixer", "options", "playlist"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Now,
    Queue,
}

pub struct App<'nc> {
    nc: &'nc Nc,
    conn: Conn,
    status: Status,
    song: Option<Song>,
    queue: Vec<Song>,
    fetched_at: Instant,
    view: View,
    /// Queue cursor and the first visible row.
    sel: usize,
    scroll: usize,
    art: Art<'nc>,
}

impl<'nc> App<'nc> {
    pub fn new(nc: &'nc Nc, conn: Conn) -> Result<Self> {
        let mut app = Self {
            nc,
            conn,
            status: Status::default(),
            song: None,
            queue: Vec::new(),
            fetched_at: Instant::now(),
            view: View::Now,
            sel: 0,
            scroll: 0,
            art: Art::new(nc),
        };
        app.update(&["playlist", "player"])?;
        app.sel = app.status.song_pos.unwrap_or(0);
        Ok(app)
    }

    pub fn is_playing(&self) -> bool {
        self.status.state == State::Play
    }

    /// Refetches whatever the idle subsystems in `changed` affect.
    pub fn update<S: AsRef<str>>(&mut self, changed: &[S]) -> Result<()> {
        let any = |set: &[&str]| changed.iter().any(|c| set.contains(&c.as_ref()));
        if any(QUEUE_EVENTS) {
            self.queue = Song::parse_list(&self.conn.command("playlistinfo")?);
            self.sel = self.sel.min(self.queue.len().saturating_sub(1));
        }
        if any(STATUS_EVENTS) {
            self.status = Status::parse(&self.conn.command("status")?);
            self.song = Song::parse(&self.conn.command("currentsong")?);
            self.fetched_at = Instant::now();
            self.art.load(&mut self.conn, self.song.as_ref().map(|s| s.file.as_str()))?;
        }
        Ok(())
    }

    /// Elapsed time, advanced locally between fetches while playing.
    fn elapsed(&self) -> Duration {
        let e = match self.status.state {
            State::Play => self.status.elapsed + self.fetched_at.elapsed(),
            _ => self.status.elapsed,
        };
        if self.status.duration.is_zero() { e } else { e.min(self.status.duration) }
    }

    /// Returns false to quit.
    pub fn handle(&mut self, input: Input) -> Result<bool> {
        if input.is_char('q') || input.id == key::ESC {
            return Ok(false);
        }
        if input.is_char('1') {
            self.view = View::Now;
        } else if input.is_char('2') {
            self.view = View::Queue;
        } else if input.id == key::TAB {
            self.view = match self.view {
                View::Now => View::Queue,
                View::Queue => View::Now,
            };
        } else if let Some(cmd) = self.player_command(input) {
            // The idle connection reports the change, which triggers update().
            self.conn.command(&cmd)?;
        } else if self.view == View::Queue {
            self.handle_queue(input)?;
        }
        Ok(true)
    }

    /// Keys that work in every view.
    fn player_command(&self, input: Input) -> Option<String> {
        let s = &self.status;
        let c = |ch| input.is_char(ch);
        Some(match input.id {
            _ if c(' ') || c('p') => match s.state {
                State::Play => "pause 1".into(),
                _ => "play".into(),
            },
            _ if c('s') => "stop".into(),
            _ if c('n') || c('>') => "next".into(),
            _ if c('N') || c('<') => "previous".into(),
            key::RIGHT => "seekcur +5".into(),
            key::LEFT => "seekcur -5".into(),
            _ if c('+') || c('=') => format!("setvol {}", (s.volume? + 5).min(100)),
            _ if c('-') => format!("setvol {}", s.volume?.saturating_sub(5)),
            _ if c('r') => format!("repeat {}", u8::from(!s.repeat)),
            _ if c('z') => format!("random {}", u8::from(!s.random)),
            _ if c('y') => format!("single {}", u8::from(!s.single)),
            _ if c('c') => format!("consume {}", u8::from(!s.consume)),
            _ => return None,
        })
    }

    fn handle_queue(&mut self, input: Input) -> Result<()> {
        let last = self.queue.len().saturating_sub(1);
        let page = self.queue_rows().max(1);
        let c = |ch| input.is_char(ch);
        match input.id {
            key::DOWN => self.sel = (self.sel + 1).min(last),
            key::UP => self.sel = self.sel.saturating_sub(1),
            key::PGDOWN => self.sel = (self.sel + page).min(last),
            key::PGUP => self.sel = self.sel.saturating_sub(page),
            key::HOME => self.sel = 0,
            key::END => self.sel = last,
            key::ENTER | 0x0a | 0x0d => self.play_selected()?,
            key::DEL => self.delete_selected()?,
            _ if c('j') => self.sel = (self.sel + 1).min(last),
            _ if c('k') => self.sel = self.sel.saturating_sub(1),
            _ if c('g') => self.sel = 0,
            _ if c('G') => self.sel = last,
            _ if c('d') => self.delete_selected()?,
            _ if c('.') => self.sel = self.status.song_pos.unwrap_or(self.sel),
            _ => {}
        }
        Ok(())
    }

    fn selected_id(&self) -> Option<u32> {
        self.queue.get(self.sel).and_then(|s| s.id)
    }

    fn play_selected(&mut self) -> Result<()> {
        if let Some(id) = self.selected_id() {
            self.conn.command(&format!("playid {id}"))?;
        }
        Ok(())
    }

    fn delete_selected(&mut self) -> Result<()> {
        if let Some(id) = self.selected_id() {
            self.conn.command(&format!("deleteid {id}"))?;
        }
        Ok(())
    }

    /// Rows available for queue entries (below the column header).
    fn queue_rows(&self) -> usize {
        let (rows, _) = self.nc.stdplane().dim_yx();
        body_rect(rows, 0).rows.saturating_sub(1) as usize
    }

    pub fn draw(&mut self) -> Result<()> {
        let nc = self.nc;
        let mut p = nc.stdplane();
        p.erase();
        let (rows, cols) = p.dim_yx();
        if rows < 8 || cols < 40 {
            self.art.show(None)?;
            p.set_fg_default();
            p.putstr_yx(0, 0, "terminal too small");
            return Ok(());
        }

        self.draw_header(&mut p, cols);
        let body = body_rect(rows, cols);
        match self.view {
            View::Now => self.draw_now(&mut p, body)?,
            View::Queue => {
                self.art.show(None)?;
                self.draw_queue(&mut p, body);
            }
        }
        self.draw_progress(&mut p, rows - 2, cols);

        p.set_fg_rgb(DIM);
        let help = match self.view {
            View::Now => "space pause · n/N next/prev · ←/→ seek · +/- vol · r z y c modes · tab queue · q quit",
            View::Queue => "j/k move · enter play · d delete · . current · g/G top/bottom · tab now playing · q quit",
        };
        p.putstr_aligned(rows - 1, Align::Center, &nc::fit(help, cols as usize));
        p.set_fg_default();
        Ok(())
    }

    fn draw_header(&self, p: &mut Plane, cols: u32) {
        let mut x = 1;
        for (label, view) in [("notmpc", None), ("1 now playing", Some(View::Now)), ("2 queue", Some(View::Queue))] {
            let active = view.is_none_or(|v| v == self.view);
            p.set_styles(if active { nc::BOLD } else { 0 });
            p.set_fg_rgb(if active { ACCENT } else { DIM });
            p.putstr_yx(0, x, label);
            x += nc::width(label) as u32 + 3;
        }
        p.set_styles(0);

        let s = &self.status;
        let flag = |on: bool, c: char| if on { c } else { '-' };
        let vol = s.volume.map_or("n/a".into(), |v| format!("{v}%"));
        let right = format!(
            "vol {vol}  [{}{}{}{}] ",
            flag(s.repeat, 'r'),
            flag(s.random, 'z'),
            flag(s.single, 'y'),
            flag(s.consume, 'c'),
        );
        if x + nc::width(&right) as u32 <= cols {
            p.set_fg_rgb(DIM);
            p.putstr_aligned(0, Align::Right, &right);
        }
    }

    fn draw_now(&mut self, p: &mut Plane, body: Rect) -> Result<()> {
        // Cells are about twice as tall as wide, so 2 cols per row is square.
        let art = Rect {
            y: body.y,
            x: 2,
            rows: body.rows,
            cols: (body.rows * 2).min(body.cols / 2),
        };
        let (text_x, text_w) = if body.rows >= 6 && body.cols >= 50 {
            if self.art.has_image() {
                self.art.show(Some(art))?;
            } else {
                self.art.show(None)?;
                p.set_fg_rgb(DIM);
                put_centered(p, art.y + art.rows / 2, art.x, art.cols, "♪ no cover");
            }
            (art.x + art.cols + 2, body.cols - art.cols - 4)
        } else {
            self.art.show(None)?;
            (0, body.cols)
        };

        let mid = body.y + body.rows / 2;
        let Some(song) = &self.song else {
            p.set_fg_rgb(DIM);
            put_centered(p, mid, text_x, text_w, "nothing queued");
            return Ok(());
        };
        p.set_styles(nc::BOLD);
        p.set_fg_rgb(TEXT);
        put_centered(p, mid - 2, text_x, text_w, song.display_title());
        p.set_styles(0);
        p.set_fg_rgb(ACCENT);
        put_centered(p, mid - 1, text_x, text_w, song.artist.as_deref().unwrap_or(""));
        p.set_styles(nc::ITALIC);
        p.set_fg_rgb(DIM);
        put_centered(p, mid, text_x, text_w, song.album.as_deref().unwrap_or(""));
        p.set_styles(0);
        if let Some(pos) = self.status.song_pos {
            let n = format!("{} of {}", pos + 1, self.queue.len());
            put_centered(p, mid + 2, text_x, text_w, &n);
        }
        Ok(())
    }

    fn draw_queue(&mut self, p: &mut Plane, body: Rect) {
        if self.queue.is_empty() {
            p.set_fg_rgb(DIM);
            put_centered(p, body.y + body.rows / 2, 0, body.cols, "queue is empty");
            return;
        }

        // Columns: marker, artist, title, album, time.
        let w = body.cols.saturating_sub(2) as usize;
        let rest = w.saturating_sub(2 + 6 + 3);
        let (artist_w, title_w) = (rest * 3 / 10, rest * 4 / 10);
        let album_w = rest - artist_w - title_w;
        let row = |marker: &str, artist: &str, title: &str, album: &str, time: &str| {
            format!(
                " {marker}{} {} {} {time:>6} ",
                pad(artist, artist_w),
                pad(title, title_w),
                pad(album, album_w),
            )
        };

        p.set_styles(nc::BOLD);
        p.set_fg_rgb(DIM);
        p.putstr_yx(body.y, 0, &row("  ", "Artist", "Title", "Album", "Time"));
        p.set_styles(0);

        let h = body.rows.saturating_sub(1) as usize;
        if self.sel < self.scroll {
            self.scroll = self.sel;
        } else if self.sel >= self.scroll + h {
            self.scroll = self.sel + 1 - h;
        }

        for (i, song) in self.queue.iter().enumerate().skip(self.scroll).take(h) {
            let y = body.y + 1 + (i - self.scroll) as u32;
            let current = self.status.song_pos == Some(i);
            if i == self.sel {
                p.set_bg_rgb(SELECTED);
            }
            p.set_styles(if current { nc::BOLD } else { 0 });
            p.set_fg_rgb(if current { ACCENT } else { TEXT });
            let line = row(
                if current { "▶ " } else { "  " },
                song.artist.as_deref().unwrap_or(""),
                song.display_title(),
                song.album.as_deref().unwrap_or(""),
                &fmt_time(song.duration),
            );
            p.putstr_yx(y, 0, &line);
            p.set_bg_default();
        }
        p.set_styles(0);
    }

    fn draw_progress(&self, p: &mut Plane, y: u32, cols: u32) {
        let s = &self.status;
        let state = match s.state {
            State::Play => "▶",
            State::Pause => "⏸",
            State::Stop => "⏹",
        };
        let left = format!(" {state} {}", fmt_time(self.elapsed()));
        let right = format!("{} ", fmt_time(s.duration));
        let left_w = nc::width(&left);
        let width = (cols as usize).saturating_sub(left_w + nc::width(&right) + 2);
        let frac = if s.duration.is_zero() {
            0.0
        } else {
            self.elapsed().as_secs_f64() / s.duration.as_secs_f64()
        };
        let filled = ((width as f64 * frac) as usize).min(width);
        p.set_fg_rgb(TEXT);
        p.putstr_yx(y, 0, &left);
        p.set_fg_rgb(ACCENT);
        p.putstr_yx(y, left_w as u32 + 1, &"━".repeat(filled));
        p.set_fg_rgb(DIM);
        p.putstr_yx(y, (left_w + 1 + filled) as u32, &"─".repeat(width - filled));
        p.set_fg_rgb(TEXT);
        p.putstr_aligned(y, Align::Right, &right);
    }
}

/// Everything between the header and the progress bar, with a blank row
/// on each side.
fn body_rect(rows: u32, cols: u32) -> Rect {
    Rect { y: 2, x: 0, rows: rows.saturating_sub(5), cols }
}

fn put_centered(p: &mut Plane, y: u32, x: u32, w: u32, s: &str) {
    let s = nc::fit(s, w as usize);
    let off = w.saturating_sub(nc::width(&s) as u32) / 2;
    p.putstr_yx(y, x + off, &s);
}

/// Fits `s` to exactly `cols` cells.
fn pad(s: &str, cols: usize) -> String {
    let s = nc::fit(s, cols);
    let w = nc::width(&s);
    s + &" ".repeat(cols.saturating_sub(w))
}

fn fmt_time(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}
