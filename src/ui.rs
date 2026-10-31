//! Drawing. Layout, top to bottom:
//!
//! ```text
//! [Playing]                  Kiska                    Volume: ▁▂▃▅▇ 100%
//! 2:51 / 5:25 (731 kbps, FLAC 16/44.1)  Artist - Album   Repeat / Random …
//! (──────────────󱢜                                                       )
//!                    Queue    Directories    Search
//! [ album art ] │ PACIFIC (2013) · Various ─────────── 8 tracks · 42:17
//!               │ ▶ 7 Kiska                Tatsuro Yamashita        5:25
//! message / prompt                       46 songs · 3:12:05 · 1:04:12 left
//! ```

use std::time::Duration;

use anyhow::Result;

use crate::app::{App, Prompt, Row, TABS, Tab};
use crate::art::Rect;
use crate::config::Style;
use crate::mpd::State;
use crate::nc::{self, BOLD, Color, ITALIC, Plane};

const HEADER_ROWS: u32 = 4;
const FOOTER_ROWS: u32 = 1;
/// Below this width the queue tab drops the art pane.
const MIN_ART_COLS: u32 = 80;

fn apply(p: &mut Plane, s: Style) {
    p.set_fg(s.fg);
    p.set_bg(s.bg);
    p.set_styles(if s.bold { BOLD } else { 0 } | if s.italic { ITALIC } else { 0 });
}

fn reset(p: &mut Plane) {
    p.set_fg(Color::Default);
    p.set_bg(Color::Default);
    p.set_styles(0);
}

fn italic_if(s: Style, on: bool) -> Style {
    Style { italic: s.italic || on, ..s }
}

/// Writes `s` at (y, x); returns the column after it.
fn put(p: &mut Plane, y: u32, x: u32, s: &str, style: Style) -> u32 {
    apply(p, style);
    p.putstr_yx(y, x, s);
    x + nc::width(s) as u32
}

/// Styled pieces of one line.
type Segs = Vec<(String, Style)>;

fn segs_width(segs: &Segs) -> u32 {
    segs.iter().map(|(s, _)| nc::width(s) as u32).sum()
}

/// Writes `segs` from x, truncating to `max` cells.
fn put_segs(p: &mut Plane, y: u32, x: u32, max: u32, segs: &Segs) -> u32 {
    let mut x0 = x;
    let mut left = max;
    for (s, st) in segs {
        if left == 0 {
            break;
        }
        let s = nc::fit(s, left as usize);
        let w = nc::width(&s) as u32;
        put(p, y, x0, &s, *st);
        x0 += w;
        left -= w;
    }
    x0
}

fn fmt_time(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// `44100:16:2` → `16/44.1`; `dsd64:2` → `DSD64`.
fn fmt_audio(audio: &str) -> String {
    let mut parts = audio.split(':');
    let rate = parts.next().unwrap_or("");
    let bits = parts.next().unwrap_or("");
    match rate.parse::<u32>() {
        Ok(hz) => {
            let khz = format!("{:.1}", hz as f64 / 1000.0);
            let khz = khz.strip_suffix(".0").unwrap_or(&khz);
            format!("{bits}/{khz}")
        }
        Err(_) => rate.to_ascii_uppercase(),
    }
}

impl App<'_> {
    pub fn draw(&mut self) -> Result<()> {
        let nc = self.nc;
        let mut p = nc.stdplane();
        p.erase();
        let (rows, cols) = p.dim_yx();
        if rows < HEADER_ROWS + FOOTER_ROWS + 3 || cols < 40 {
            self.art.show(None)?;
            reset(&mut p);
            p.putstr_yx(0, 0, "terminal too small");
            return Ok(());
        }

        self.draw_header(&mut p, cols);
        self.draw_progress(&mut p, 2, cols);
        self.draw_tabs(&mut p, 3, cols);
        let body = Rect { y: HEADER_ROWS, x: 0, rows: rows - HEADER_ROWS - FOOTER_ROWS, cols };
        match self.tab {
            Tab::Queue => self.draw_queue_tab(&mut p, body)?,
            Tab::Directories | Tab::Search => {
                self.art.show(None)?;
                let t = self.cfg.theme.dim;
                put_centered(&mut p, body.y + body.rows / 2, 0, cols, "not built yet", t);
            }
        }
        self.draw_footer(&mut p, rows - 1, cols);
        reset(&mut p);
        Ok(())
    }

    fn draw_header(&self, p: &mut Plane, cols: u32) {
        let t = &self.cfg.theme;
        let s = &self.status;

        // row 0: [state]   title   volume
        let state = match s.state {
            State::Play => "[Playing]",
            State::Pause => "[Paused]",
            State::Stop => "[Stopped]",
        };
        let left0 = vec![(state.to_owned(), t.state)];

        let mut right0: Segs = Vec::new();
        if s.updating {
            right0.push(("Updating… ".into(), t.volume));
        }
        right0.push(("Volume: ".into(), t.volume));
        match s.volume {
            Some(v) => {
                const RAMP: [&str; 5] = ["▁", "▂", "▃", "▅", "▇"];
                let lit = (usize::from(v) * RAMP.len()).div_ceil(100);
                for (i, bar) in RAMP.iter().enumerate() {
                    right0.push((bar.to_string(), if i < lit { t.volume } else { t.dim }));
                }
                right0.push((format!(" {v}%"), t.volume));
            }
            None => right0.push(("n/a".into(), t.dim)),
        }

        let center0 = match &self.song {
            Some((_, m)) => vec![(m.title.clone(), italic_if(t.title, m.inferred))],
            None => vec![("No Song".into(), t.title)],
        };
        draw_three(p, 0, cols, &left0, &center0, &right0);

        // row 1: time (bitrate, format)   artist - album   modes
        let mut info = format!("{} / {}", fmt_time(self.elapsed()), fmt_time(s.duration));
        let mut extra = Vec::new();
        if let Some(b) = s.bitrate {
            extra.push(format!("{b} kbps"));
        }
        if let Some(a) = &s.audio {
            let codec = self
                .song
                .as_ref()
                .and_then(|(song, _)| song.file.rsplit_once('.'))
                .map(|(_, ext)| ext.to_ascii_uppercase());
            extra.push(match codec {
                Some(c) => format!("{c} {}", fmt_audio(a)),
                None => fmt_audio(a),
            });
        }
        if !extra.is_empty() {
            info += &format!(" ({})", extra.join(", "));
        }
        let left1 = vec![(info, t.text)];

        let center1 = match &self.song {
            Some((_, m)) => vec![
                (m.artist.clone(), italic_if(t.artist, m.inferred)),
                (" - ".into(), t.text),
                (m.album.clone(), italic_if(t.text, m.inferred)),
            ],
            None => Vec::new(),
        };

        let sep = (" / ".to_owned(), t.mode_on);
        let mode = |name: &str, on: bool| (name.to_owned(), if on { t.mode_on } else { t.mode_off });
        let right1 = vec![
            mode("Repeat", s.repeat),
            sep.clone(),
            mode("Random", s.random),
            sep.clone(),
            mode("Consume", s.consume),
            sep,
            mode("Single", s.single),
        ];
        draw_three(p, 1, cols, &left1, &center1, &right1);
    }

    /// `[start][elapsed…][thumb][remaining…][end]` across the full width.
    fn draw_progress(&self, p: &mut Plane, y: u32, cols: u32) {
        let t = &self.cfg.theme;
        let [start, elapsed, thumb, track, end] = &t.progress_symbols;
        let (ws, we, wt) = (nc::width(start) as u32, nc::width(end) as u32, nc::width(thumb) as u32);
        let inner = cols.saturating_sub(ws + we);
        let d = self.status.duration.as_secs_f64();
        let frac = if d > 0.0 && self.status.state != State::Stop {
            (self.elapsed().as_secs_f64() / d).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let filled = ((inner.saturating_sub(wt)) as f64 * frac).round() as u32;

        let mut x = put(p, y, 0, start, t.progress_elapsed);
        let repeat = |sym: &str, cells: u32| {
            let w = nc::width(sym).max(1) as u32;
            sym.repeat((cells / w) as usize)
        };
        x = put(p, y, x, &repeat(elapsed, filled), t.progress_elapsed);
        if self.status.state != State::Stop {
            x = put(p, y, x, thumb, t.progress_thumb);
        }
        put(p, y, x, &repeat(track, (ws + inner).saturating_sub(x)), t.progress_track);
        put(p, y, ws + inner, end, t.progress_track);
    }

    fn draw_tabs(&self, p: &mut Plane, y: u32, cols: u32) {
        let t = &self.cfg.theme;
        const PAD: &str = "   ";
        const GAP: u32 = 2;
        let labels: Vec<String> = TABS.iter().map(|(_, l)| format!("{PAD}{l}{PAD}")).collect();
        let total: u32 = labels.iter().map(|l| nc::width(l) as u32).sum::<u32>() + GAP * (labels.len() as u32 - 1);
        let mut x = cols.saturating_sub(total) / 2;
        for ((tab, _), label) in TABS.iter().zip(&labels) {
            let st = if *tab == self.tab { t.tab_active } else { t.tab_inactive };
            x = put(p, y, x, label, st) + GAP;
        }
    }

    fn draw_queue_tab(&mut self, p: &mut Plane, body: Rect) -> Result<()> {
        let art_cols = if body.cols >= MIN_ART_COLS { body.cols * self.cfg.art_width / 100 } else { 0 };
        if art_cols > 0 {
            let art = Rect { y: body.y, x: 0, rows: body.rows, cols: art_cols };
            if self.art.has_image() {
                self.art.show(Some(art))?;
            } else {
                self.art.show(None)?;
                let dim = self.cfg.theme.dim;
                put_centered(p, art.y + art.rows / 2, art.x, art.cols, "♪ no cover", dim);
            }
        } else {
            self.art.show(None)?;
        }
        let x = if art_cols > 0 { art_cols + 1 } else { 0 };
        self.draw_queue_list(p, Rect { y: body.y, x, rows: body.rows, cols: body.cols - x });
        Ok(())
    }

    fn draw_queue_list(&mut self, p: &mut Plane, area: Rect) {
        let t = self.cfg.theme.clone();
        let h = area.rows as usize;
        self.list_height = h;
        if self.queue.is_empty() {
            put_centered(p, area.y + area.rows / 2, area.x, area.cols, "Queue is empty", t.dim);
            return;
        }

        // Keep the cursor visible, along with its album header when there's
        // one directly above it.
        let target = self.song_row[self.sel];
        let top = match target.checked_sub(1).map(|r| self.rows[r]) {
            Some(Row::Group(_)) => target - 1,
            _ => target,
        };
        if top < self.scroll {
            self.scroll = top;
        } else if target >= self.scroll + h {
            self.scroll = target + 1 - h;
        }
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(h));

        let w = area.cols;
        for (line, row) in self.rows.iter().enumerate().skip(self.scroll).take(h) {
            let y = area.y + (line - self.scroll) as u32;
            match *row {
                Row::Group(g) => self.draw_group_header(p, y, area.x, w, g, &t),
                Row::Song(i) => self.draw_song_row(p, y, area.x, w, i, &t),
            }
        }
    }

    fn draw_group_header(&self, p: &mut Plane, y: u32, x: u32, w: u32, g: usize, t: &crate::config::Theme) {
        let group = &self.groups[g];
        let m = &self.metas[group.first];
        let mut left: Segs = vec![(
            if m.album.is_empty() { "(no album)".into() } else { m.album.clone() },
            italic_if(t.group_header, m.inferred),
        )];
        if !m.year.is_empty() {
            left.push((format!(" ({})", m.year), italic_if(t.date, m.inferred)));
        }
        let artist = if group.various { "Various Artists" } else { &m.album_artist };
        if !artist.is_empty() {
            left.push((format!(" · {artist}"), italic_if(t.text, m.inferred)));
        }
        let tracks = if group.len == 1 { "1 track".into() } else { format!("{} tracks", group.len) };
        let right = format!(" {tracks} · {} ", fmt_time(group.total));
        let rw = nc::width(&right) as u32;

        let lx = put_segs(p, y, x, w.saturating_sub(rw + 2), &left);
        let fill = (x + w).saturating_sub(lx + rw + 1);
        put(p, y, lx + 1, &"─".repeat(fill as usize), t.dim);
        put(p, y, x + w - rw, &right, t.dim);
    }

    fn draw_song_row(&self, p: &mut Plane, y: u32, x: u32, w: u32, i: usize, t: &crate::config::Theme) {
        let song = &self.queue[i];
        let m = &self.metas[i];
        let cursor = i == self.sel;
        let current = self.status.song_pos == Some(i);
        let marked = song.id.is_some_and(|id| self.marked.contains(&id));

        // The cursor and current-song styles override the column styles.
        let over = if cursor {
            Some(t.cursor)
        } else if current {
            Some(t.current)
        } else {
            None
        };
        let st = |s: Style| italic_if(over.unwrap_or(s), m.inferred);
        if cursor {
            put(p, y, x, &" ".repeat(w as usize), t.cursor);
        }

        let marker = if current { "▶ " } else if marked { "+ " } else { "  " };
        let time = format!("{:>6} ", fmt_time(song.duration));
        let inner = w.saturating_sub(2 + 7) as usize;

        let cols: Vec<(String, usize, Style)> = if self.cfg.group_by_album {
            let various = self.groups.iter().rev().find(|g| g.first <= i).is_some_and(|g| g.various);
            let track = m.track.map_or(String::new(), |n| format!("{n:>2}"));
            let rest = inner.saturating_sub(4);
            if various {
                let artist_w = rest * 2 / 5;
                vec![
                    (track, 4, st(t.dim)),
                    (m.title.clone(), rest - artist_w, st(t.text)),
                    (m.artist.clone(), artist_w, st(t.text)),
                ]
            } else {
                vec![(track, 4, st(t.dim)), (m.title.clone(), rest, st(t.text))]
            }
        } else {
            // rmpc's song table: Artist 15%, Title 35%, Album 30%, Date 10%
            let (aw, tw, alw) = (inner * 15 / 90, inner * 35 / 90, inner * 30 / 90);
            vec![
                (m.artist.clone(), aw, st(t.text)),
                (m.title.clone(), tw, st(t.text)),
                (m.album.clone(), alw, st(t.album)),
                (m.year.clone(), inner - aw - tw - alw, st(t.date)),
            ]
        };

        let mx = put(p, y, x, marker, if marked && !current { st(t.group_header) } else { st(t.text) });
        let mut cx = mx;
        for (text, width, style) in cols {
            let s = nc::fit(&text, width.saturating_sub(1));
            put(p, y, cx, &s, style);
            cx += width as u32;
        }
        put(p, y, x + w - nc::width(&time) as u32, &time, st(t.text));
    }

    fn draw_footer(&self, p: &mut Plane, y: u32, cols: u32) {
        let t = &self.cfg.theme;
        let n = self.queue.len();
        let total: Duration = self.queue.iter().map(|s| s.duration).sum();
        let mut right = format!("{n} song{} · {}", if n == 1 { "" } else { "s" }, fmt_time(total));
        if let Some(pos) = self.status.song_pos.filter(|_| self.status.state != State::Stop) {
            let after: Duration = self.queue.iter().skip(pos + 1).map(|s| s.duration).sum();
            let left = after + self.status.duration.saturating_sub(self.elapsed());
            right += &format!(" · {} left", fmt_time(left));
        }
        right.push(' ');
        let rw = nc::width(&right) as u32;
        put(p, y, cols.saturating_sub(rw), &right, t.dim);

        let max = cols.saturating_sub(rw + 2) as usize;
        let (msg, style) = if let Some(Prompt::ClearQueue) = self.prompt {
            ("Clear the whole queue? (y/N)".to_owned(), t.current)
        } else if let Some(m) = self.current_message() {
            (m.to_owned(), t.text)
        } else if !self.marked.is_empty() {
            (format!("{} selected", self.marked.len()), t.group_header)
        } else {
            return;
        };
        put(p, y, 1, &nc::fit(&msg, max), style);
    }
}

/// Left- and right-aligned segments with a centred one between, the centre
/// truncated so it never overlaps the sides.
fn draw_three(p: &mut Plane, y: u32, cols: u32, left: &Segs, center: &Segs, right: &Segs) {
    let (lw, rw) = (segs_width(left), segs_width(right));
    put_segs(p, y, 0, cols, left);
    put_segs(p, y, cols.saturating_sub(rw), rw, right);
    let side = lw.max(rw) + 2;
    let avail = cols.saturating_sub(side * 2);
    let cw = segs_width(center).min(avail);
    put_segs(p, y, (cols - cw) / 2, avail, center);
}

fn put_centered(p: &mut Plane, y: u32, x: u32, w: u32, s: &str, style: Style) {
    let s = nc::fit(s, w as usize);
    let off = w.saturating_sub(nc::width(&s) as u32) / 2;
    put(p, y, x + off, &s, style);
}
