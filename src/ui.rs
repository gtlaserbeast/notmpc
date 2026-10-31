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

use crate::app::{App, Hit, List, Mode, Prompt, TABS, Tab, Target};
use crate::queue::{Queue, Row};
use crate::scrobble::Health;
use crate::art::Rect;
use crate::browser::Entry;
use crate::config::{Style, Theme};
use crate::mpd::State;
use crate::nc::{self, BOLD, Color, ITALIC, Plane};
use crate::pixelbar;

const FOOTER_ROWS: u32 = 1;
/// Below this width the queue tab drops the art pane.
const MIN_ART_COLS: u32 = 80;

fn apply(p: &mut Plane, s: Style) {
    p.set_fg(s.fg);
    p.set_bg(s.bg);
    // Over the header's spectrum, unstyled text keeps the bars behind it.
    if s.bg == Color::Default {
        p.set_bg_transparent();
    }
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
        self.hits.borrow_mut().clear();
        let (rows, cols) = p.dim_yx();
        // header (2 rows), progress, tabs
        let header_rows = 4;
        if rows < header_rows + FOOTER_ROWS + 3 || cols < 40 {
            self.forget_images();
            self.header_plane = None;
            self.viz_plane = None;
            reset(&mut p);
            p.putstr_yx(0, 0, "terminal too small");
            return Ok(());
        }

        let pixel_bar = self.pixel_bar.is_some();
        self.layer_header(cols)?;
        self.draw_viz(cols);
        let mut hp = self.header_plane.take().expect("created above");
        hp.erase();
        self.draw_header(&mut hp, cols);
        if !pixel_bar {
            self.draw_progress(&mut hp, 2, cols);
        }
        reset(&mut hp);
        self.header_plane = Some(hp);
        if pixel_bar {
            self.draw_pixel_bar(2, cols)?;
        }

        self.draw_tabs(&mut p, header_rows - 1, cols);
        let body = Rect { y: header_rows, x: 0, rows: rows - header_rows - FOOTER_ROWS, cols };
        if self.tab != Tab::Directories {
            self.preview_art.show(None)?;
        }
        match self.tab {
            Tab::Queue => self.draw_queue_tab(&mut p, body)?,
            Tab::Directories => {
                self.art.show(None)?;
                self.draw_directories(&mut p, body)?;
            }
            Tab::Search => {
                self.art.show(None)?;
                self.draw_search(&mut p, body);
            }
        }
        self.draw_footer(&mut p, rows - 1, cols);
        self.draw_overlay(rows, cols)?;
        reset(&mut p);
        Ok(())
    }

    /// (Re)creates the header's planes for this width. Bottom to top: the
    /// spectrum; the header text on a transparent plane (and the symbol
    /// progress bar, row 2); the pixel bar on its own, since text shouldn't
    /// sit over pixel images. notcurses stacks a new plane on top, so they
    /// are made in that order.
    fn layer_header(&mut self, cols: u32) -> Result<()> {
        let hp_rows = if self.pixel_bar.is_some() { 2 } else { 3 };
        if self.header_plane.as_ref().is_some_and(|h| h.dim_yx() == (hp_rows, cols)) {
            return Ok(());
        }
        self.header_plane = None;
        self.viz_plane = None;
        self.viz_plane = Some(self.nc.new_plane(0, 0, self.cfg.visualizer_rows, cols)?);
        let mut h = self.nc.new_plane(0, 0, hp_rows, cols)?;
        h.set_base_transparent();
        h.move_top();
        self.header_plane = Some(h);
        // recreated on top at its next draw
        if let Some(bar) = self.pixel_bar.as_mut() {
            bar.hide();
        }
        Ok(())
    }

    /// One bar per column, rising from the bottom of the header in eighth
    /// blocks; full cells are drawn as background so text over them keeps
    /// the colour. Each bar is one colour: the palette's gradient across the
    /// spectrum, brighter the taller it is (no banding between rows).
    fn draw_viz(&mut self, cols: u32) {
        const PARTIAL: [&str; 8] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇"];
        let Some(mut vp) = self.viz_plane.take() else { return };
        vp.erase();
        let rows = vp.dim_yx().0;
        let stops = self.cfg.theme.visualizer_colors.clone();
        if let Some(viz) = self.viz.as_mut().filter(|_| self.offline.is_none() && self.viz_on) {
            let bars = viz.frame(cols as usize);
            for (x, &b) in bars.iter().enumerate() {
                let eighths = (b * (rows * 8) as f32).round() as u32;
                if eighths == 0 {
                    continue;
                }
                // Dim enough for the header text on top to stay readable.
                let c = Color::Rgb(scale(gradient(&stops, x as f32 / (cols.max(2) - 1) as f32), 0.30 + 0.40 * b));
                for r in 0..rows {
                    let filled = eighths.saturating_sub(r * 8).min(8);
                    if filled == 0 {
                        break;
                    }
                    let y = rows - 1 - r;
                    vp.set_fg(c);
                    if filled == 8 {
                        vp.set_bg(c);
                        vp.putstr_yx(y, x as u32, " ");
                    } else {
                        vp.set_bg(Color::Default);
                        vp.set_bg_transparent();
                        vp.putstr_yx(y, x as u32, PARTIAL[filled as usize]);
                    }
                }
            }
        }
        self.viz_plane = Some(vp);
    }

    fn hit(&self, y: u32, x: u32, w: u32, target: Target) {
        self.hits.borrow_mut().push(Hit { y, x, w, target });
    }

    fn draw_header(&self, p: &mut Plane, cols: u32) {
        let t = &self.cfg.theme;
        let s = &self.status;

        // row 0: [state]   title   volume
        let state = match s.state {
            _ if self.offline.is_some() => "[Offline]",
            State::Play => "[Playing]",
            State::Pause => "[Paused]",
            State::Stop => "[Stopped]",
        };
        let left0 = vec![(state.to_owned(), t.state)];

        let mut right0: Segs = Vec::new();
        if s.updating {
            right0.push(("Updating… ".into(), t.volume));
        }
        // scrobble badge
        let sc = &self.scrobble;
        let badge = match sc.health() {
            Health::Absent => t.dim,
            Health::Ok => t.text,
            Health::Warning => t.volume,
        };
        let badge_start = segs_width(&right0);
        right0.push(("last.fm".into(), badge));
        if sc.health() == Health::Warning {
            right0.push(("!".into(), t.volume));
        }
        if sc.submitted {
            right0.push((" ✓".into(), t.progress_thumb));
        }
        if sc.backlog > 0 {
            right0.push((format!(" ·{}", sc.backlog), t.date));
        }
        let badge_w = segs_width(&right0) - badge_start;
        right0.push(("   ".into(), t.text));
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
        let rw = segs_width(&right0);
        let x0 = cols.saturating_sub(rw);
        self.hit(0, x0 + badge_start, badge_w, Target::Scrobble);
        self.hit(0, x0 + badge_start + badge_w, rw - badge_start - badge_w, Target::Volume);

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

        let modes = [
            ("Repeat", s.repeat, Mode::Repeat),
            ("Random", s.random, Mode::Random),
            ("Consume", s.consume, Mode::Consume),
            ("Single", s.single, Mode::Single),
            ("Follow", self.follow, Mode::Follow),
        ];
        let mut right1: Segs = Vec::new();
        for (i, (name, on, _)) in modes.iter().enumerate() {
            if i > 0 {
                right1.push((" / ".into(), t.mode_on));
            }
            right1.push((name.to_string(), if *on { t.mode_on } else { t.mode_off }));
        }
        draw_three(p, 1, cols, &left1, &center1, &right1);
        let mut x = cols.saturating_sub(segs_width(&right1));
        for (name, _, mode) in modes {
            let w = nc::width(name) as u32;
            self.hit(1, x, w, Target::Mode(mode));
            x += w + 3;
        }
    }

    /// The pixel bar at row `y`; hidden under an overlay's backdrop.
    fn draw_pixel_bar(&mut self, y: u32, cols: u32) -> Result<()> {
        let t = &self.cfg.theme;
        let rgb = |s: crate::config::Style, or: u32| if let Color::Rgb(c) = s.fg { c } else { or };
        let colors = pixelbar::Colors {
            played: rgb(t.progress_elapsed, 0xdd6f00),
            track: rgb(t.dim, 0x5a5a5a),
            thumb: rgb(t.progress_thumb, 0xa7ff04),
        };
        let s = &self.status;
        let stopped = s.state == State::Stop || s.duration.is_zero();
        // whole seconds, so the image changes at most once a second
        let frac = if stopped { 0.0 } else { self.elapsed().as_secs() as f32 / s.duration.as_secs_f32() };
        let bar = self.pixel_bar.as_mut().expect("pixel bar");
        if self.overlay.is_some() {
            bar.hide();
            return Ok(());
        }
        bar.draw(y, cols, frac, !stopped, &colors)?;
        self.hit(y, 0, cols, Target::Progress);
        Ok(())
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

        self.hit(y, ws, inner, Target::Progress);
        let mut x = put(p, y, 0, start, t.progress_elapsed);
        let repeat = |sym: &str, cells: u32| {
            let w = nc::width(sym).max(1) as u32;
            sym.repeat((cells / w) as usize)
        };
        x = put(p, y, x, &repeat(elapsed, filled), t.progress_elapsed);
        if self.status.state != State::Stop {
            x = put(p, y, x, thumb, t.progress_thumb);
        }
        // A blank remaining-track symbol leaves the cells empty so the
        // spectrum behind shows through.
        if !track.trim().is_empty() {
            put(p, y, x, &repeat(track, (ws + inner).saturating_sub(x)), t.progress_track);
        }
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
            let end = put(p, y, x, label, st);
            self.hit(y, x, end - x, Target::Tab(*tab));
            x = end + GAP;
        }
        // the queue's fold-all button, at the far right
        let q = &self.queue;
        if self.tab == Tab::Queue && q.grouping && !q.groups.is_empty() {
            let all = q.groups.iter().all(|g| q.collapsed.contains(&g.key));
            let label = if all { "▾ expand all " } else { "▸ collapse all " };
            let w = nc::width(label) as u32;
            if x + w < cols {
                put(p, y, cols - w, label, t.dim);
                self.hit(y, cols - w, w, Target::FoldAll);
            }
        }
    }

    fn draw_queue_tab(&mut self, p: &mut Plane, body: Rect) -> Result<()> {
        let art_cols = if body.cols >= MIN_ART_COLS { body.cols * self.cfg.art_width / 100 } else { 0 };
        if art_cols > 0 {
            let art = Rect { y: body.y, x: 0, rows: body.rows, cols: art_cols };
            if self.art.has_image() && self.overlay.is_none() {
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
        self.draw_song_list(p, Rect { y: body.y, x, rows: body.rows, cols: body.cols - x }, List::Queue);
        Ok(())
    }

    /// A centred panel over a dimmed screen. Covers are hidden meanwhile:
    /// notcurses can't draw text over pixel graphics.
    fn draw_overlay(&mut self, rows: u32, cols: u32) -> Result<()> {
        let Some(o) = &self.overlay else {
            self.overlay_plane = None;
            return Ok(());
        };
        let t = self.cfg.theme.clone();
        if self.overlay_plane.as_ref().is_none_or(|pl| pl.dim_yx() != (rows, cols)) {
            self.overlay_plane = None;
            let mut pl = self.nc.new_plane(0, 0, rows, cols)?;
            pl.set_base_blend(0x000000);
            pl.move_top();
            self.overlay_plane = Some(pl);
        }
        let mut pl = self.overlay_plane.take().expect("created above");
        pl.erase();

        let label_w = o.lines.iter().filter(|l| !l.heading).map(|l| nc::width(&l.label)).max().unwrap_or(0).min(28) as u32;
        let content_w = o
            .lines
            .iter()
            .map(|l| if l.heading { nc::width(&l.label) } else { label_w as usize + 2 + nc::width(&l.value) })
            .max()
            .unwrap_or(20) as u32;
        let w = (content_w + 6).clamp(40, cols.saturating_sub(4).min(110));
        let body_h = (o.lines.len() as u32).min(rows.saturating_sub(8));
        let h = body_h + 4;
        let (y0, x0) = ((rows.saturating_sub(h)) / 2, (cols.saturating_sub(w)) / 2);
        self.hit(y0, x0, w, Target::OverlayPanel);
        for y in y0 + 1..y0 + h {
            self.hit(y, x0, w, Target::OverlayPanel);
        }

        // frame
        let inner = w - 2;
        let title = nc::fit(&format!(" {} ", o.title), inner.saturating_sub(4) as usize);
        let top = format!("╭─{title}{}╮", "─".repeat((inner - 1).saturating_sub(nc::width(&title) as u32) as usize));
        put(&mut pl, y0, x0, &top, t.panel_border);
        let total = o.lines.len();
        let more = if total > body_h as usize {
            format!(" {}–{} of {total} ", o.scroll + 1, (o.scroll + body_h as usize).min(total))
        } else {
            String::new()
        };
        let hint = if o.selectable() { " Enter toggle · Esc close " } else { " j/k scroll · Esc close " };
        let fill = (inner as usize).saturating_sub(nc::width(&more) + nc::width(hint) + 1);
        put(&mut pl, y0 + h - 1, x0, &format!("╰{more}{}{hint}─╯", "─".repeat(fill)), t.panel_border);
        for y in y0 + 1..y0 + h - 1 {
            put(&mut pl, y, x0, "│", t.panel_border);
            put(&mut pl, y, x0 + 1, &" ".repeat(inner as usize), t.panel);
            put(&mut pl, y, x0 + w - 1, "│", t.panel_border);
        }

        // lines
        let on = |s: Style| Style { bg: t.panel.bg, ..s };
        for (row, (i, line)) in o.lines.iter().enumerate().skip(o.scroll).take(body_h as usize).enumerate() {
            let y = y0 + 2 + row as u32;
            let x = x0 + 2;
            let room = inner.saturating_sub(3) as usize;
            if o.selectable() {
                self.hit(y, x0 + 1, inner, Target::OverlayLine(i));
            }
            let selected = o.selectable() && i == o.sel;
            if selected {
                put(&mut pl, y, x0 + 1, &" ".repeat(inner as usize), t.cursor);
            }
            let st = |s: Style| if selected { t.cursor } else { on(s) };
            if line.heading {
                put(&mut pl, y, x, &nc::fit(&line.label, room), st(t.group_header));
                continue;
            }
            put(&mut pl, y, x, &nc::fit(&line.label, label_w as usize), st(t.label));
            let vx = x + label_w + 2;
            put(&mut pl, y, vx, &nc::fit(&line.value, room.saturating_sub((label_w + 2) as usize)), st(t.text));
        }
        reset(&mut pl);
        self.overlay_plane = Some(pl);
        Ok(())
    }

    /// Query line, a summary, then results grouped by album.
    fn draw_search(&mut self, p: &mut Plane, body: Rect) {
        let t = self.cfg.theme.clone();
        let (y, cols) = (body.y, body.cols);
        self.hit(y, 0, cols, Target::SearchInput);

        let hint = "artist: album: albumartist: title: genre: date: file:  \"quoted words\" ";
        let hw = nc::width(hint) as u32;
        let show_hint = cols > hw + 40;
        let mut x = put(p, y, 1, "Search: ", t.label);
        let room = cols.saturating_sub(x + 2 + if show_hint { hw } else { 0 }) as usize;
        // Keep the end of a long query (where you type) in view.
        let q = &self.search.query;
        let visible: String = if nc::width(q) > room {
            let chars: Vec<char> = q.chars().collect();
            chars[chars.len().saturating_sub(room.saturating_sub(1))..].iter().collect()
        } else {
            q.clone()
        };
        x = put(p, y, x, &visible, t.title);
        if self.search.editing {
            put(p, y, x, "▏", t.progress_thumb);
        }
        if show_hint {
            put(p, y, cols - hw, hint, t.dim);
        }

        let r = &self.search.results;
        let summary = if self.search.is_empty_query() {
            "Type to search: words match any tag or the path; field:value narrows to one tag. Enter: results, i: back here".to_owned()
        } else if r.songs.is_empty() {
            "No matches".to_owned()
        } else {
            let n = r.songs.len();
            format!(
                "{n} song{} in {} album{}{}  ·  Enter play · a add · A add all · t play next · Space mark · h/l fold · i edit",
                if n == 1 { "" } else { "s" },
                r.groups.len(),
                if r.groups.len() == 1 { "" } else { "s" },
                if self.search.truncated { " (first 1000 per search)" } else { "" },
            )
        };
        put(p, y + 1, 1, &nc::fit(&summary, cols.saturating_sub(2) as usize), t.dim);

        let list = Rect { y: y + 2, x: 0, rows: body.rows.saturating_sub(2), cols };
        self.draw_song_list(p, list, List::Search);
    }

    /// The queue, or search results: album headers and songs, with the
    /// highlight (queue only) and the browse cursor.
    fn draw_song_list(&mut self, p: &mut Plane, area: Rect, kind: List) {
        let t = self.cfg.theme.clone();
        let h = area.rows as usize;
        self.list_height = h;
        if self.list(kind).is_empty() {
            if kind == List::Queue {
                put_centered(p, area.y + area.rows / 2, area.x, area.cols, "Queue is empty", t.dim);
            }
            return;
        }

        // Keep the cursor visible, along with its album header when there's
        // one directly above it.
        let q = self.list_mut(kind);
        let target = q.sel;
        // Follow the cursor only when it moved; the wheel scrolls freely.
        if q.cursor_moved() {
            let top = match target.checked_sub(1).map(|r| q.rows[r]) {
                Some(Row::Group(_)) => target - 1,
                _ => target,
            };
            if top < q.scroll {
                q.scroll = top;
            } else if target >= q.scroll + h {
                q.scroll = target + 1 - h;
            }
        }
        q.scroll = q.scroll.min(q.rows.len().saturating_sub(h));

        // Rows are drawn one cell in from each side, leaving room for the
        // browse cursor's dotted rails.
        let (x, w) = (area.x + 1, area.cols.saturating_sub(2));
        let q = self.list(kind);
        let scroll = q.scroll;
        for line in scroll..(scroll + h).min(q.rows.len()) {
            let row = q.rows[line];
            let y = area.y + (line - scroll) as u32;
            let browse = line == q.sel;
            let primary = kind == List::Queue && self.is_primary(row);
            let fill = if primary {
                Some(t.cursor)
            } else if browse {
                Some(t.browse)
            } else {
                None
            };
            if let Some(f) = fill {
                put(p, y, area.x, &" ".repeat(area.cols as usize), f);
            }
            // later hits win: the arrow over its row
            self.hit(y, area.x, area.cols, Target::Row(kind, row));
            if let Row::Group(g) = row {
                self.hit(y, x, 2, Target::Caret(kind, g));
            }
            let hl = Highlight { primary, fill };
            match row {
                Row::Group(g) => self.draw_group_header(p, y, x, w, (kind, g), hl, &t),
                Row::Song(i) => self.draw_song_row(p, y, x, w, (kind, i), hl, &t),
            }
            if browse {
                let rail = Style { bg: fill.map_or(Color::Default, |f| f.bg), ..t.browse_rail };
                put(p, y, area.x, "┆", rail);
                put(p, y, area.x + area.cols - 1, "┆", rail);
            }
        }
    }

    fn list(&self, kind: List) -> &Queue {
        match kind {
            List::Queue => &self.queue,
            List::Search => &self.search.results,
        }
    }

    fn list_mut(&mut self, kind: List) -> &mut Queue {
        match kind {
            List::Queue => &mut self.queue,
            List::Search => &mut self.search.results,
        }
    }

    /// Whether song `i` of the list is the one playing: by queue id in the
    /// queue, by file in search results (which aren't queue entries).
    fn is_playing_song(&self, kind: List, i: usize) -> bool {
        let s = &self.list(kind).songs[i];
        match kind {
            List::Queue => s.id.is_some() && s.id == self.status.song_id,
            List::Search => self.song.as_ref().is_some_and(|(cur, _)| cur.file == s.file),
        }
    }

    /// Whether `row` shows the highlight: the highlighted song, or the
    /// collapsed album containing it.
    fn is_primary(&self, row: Row) -> bool {
        let q = &self.queue;
        let Some(id) = self.primary else { return false };
        match row {
            Row::Song(i) => q.songs[i].id == Some(id),
            Row::Group(g) => {
                q.collapsed.contains(&q.groups[g].key) && q.pos_of_id(id).is_some_and(|i| q.group_of_song(i) == g)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_group_header(&self, p: &mut Plane, y: u32, x: u32, w: u32, (kind, g): (List, usize), hl: Highlight, t: &Theme) {
        let q = self.list(kind);
        let group = &q.groups[g];
        let m = &q.metas[q.order[group.start]];
        let collapsed = q.collapsed.contains(&group.key);
        let playing_here = q.group_songs(g).iter().any(|&i| self.is_playing_song(kind, i));
        let st = |s: Style| italic_if(hl.apply(s), m.inferred);

        // A collapsed album that's playing gets the ▶ on its header.
        let caret = match (collapsed, playing_here) {
            (true, true) => "▶ ",
            (true, false) => "▸ ",
            (false, _) => "▾ ",
        };
        let mut left: Segs = vec![
            (caret.into(), hl.apply(t.dim)),
            (if m.album.is_empty() { "(no album)".into() } else { m.album.clone() }, st(t.group_header)),
        ];
        if !m.year.is_empty() {
            left.push((format!(" ({})", m.year), st(t.date)));
        }
        let artist = if group.various { "Various Artists" } else { &m.album_artist };
        if !artist.is_empty() {
            left.push((" · ".into(), st(t.dim)));
            left.push((artist.to_owned(), st(t.album_artist)));
        }
        if group.ambiguous {
            let folder = m.dir.rsplit('/').next().unwrap_or(&m.dir);
            left.push((format!("  {folder}"), st(t.dim)));
        }
        let tracks = if group.len == 1 { "1 track".into() } else { format!("{} tracks", group.len) };
        let right = format!(" {tracks} · {} ", fmt_time(group.total));
        let rw = nc::width(&right) as u32;

        let lx = put_segs(p, y, x, w.saturating_sub(rw + 2), &left);
        let fill = (x + w).saturating_sub(lx + rw + 1);
        put(p, y, lx + 1, &"─".repeat(fill as usize), hl.apply(t.dim));
        put(p, y, x + w - rw, &right, hl.apply(t.dim));
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_song_row(&self, p: &mut Plane, y: u32, x: u32, w: u32, (kind, i): (List, usize), hl: Highlight, t: &Theme) {
        let q = self.list(kind);
        let song = &q.songs[i];
        let m = &q.metas[i];
        let current = self.is_playing_song(kind, i);
        let next = kind == List::Queue && !current && song.id.is_some() && song.id == self.status.next_id;
        let marked = song.id.is_some_and(|id| q.marked.contains(&id));

        // The highlight and current-song styles override the column styles.
        let st = |s: Style| italic_if(hl.apply(if current { t.current } else { s }), m.inferred);

        let (marker, marker_style) = if current {
            ("▶ ", st(t.text))
        } else if next {
            ("» ", st(t.progress_thumb))
        } else if marked {
            ("+ ", st(t.group_header))
        } else {
            ("  ", st(t.text))
        };
        let time = format!("{:>6} ", fmt_time(song.duration));
        let inner = w.saturating_sub(2 + 7) as usize;

        let cols: Vec<(String, usize, Style)> = if q.grouping {
            let various = q.groups[q.group_of_song(i)].various;
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

        let mut cx = put(p, y, x, marker, marker_style);
        for (text, width, style) in cols {
            let s = nc::fit(&text, width.saturating_sub(1));
            put(p, y, cx, &s, style);
            cx += width as u32;
        }
        put(p, y, x + w - nc::width(&time) as u32, &time, st(t.text));
    }

    fn draw_footer(&self, p: &mut Plane, y: u32, cols: u32) {
        let t = &self.cfg.theme;
        let q = &self.queue;
        let n = q.songs.len();
        let total: Duration = q.songs.iter().map(|s| s.duration).sum();
        let mut right = format!("{n} song{} · {}", if n == 1 { "" } else { "s" }, fmt_time(total));
        if let Some(pos) = self.current_pos().filter(|_| self.status.state != State::Stop) {
            let after: Duration = q.order[q.display_of[pos] + 1..].iter().map(|&i| q.songs[i].duration).sum();
            let left = after + self.status.duration.saturating_sub(self.elapsed());
            right += &format!(" · {} left", fmt_time(left));
        }
        right.push(' ');
        let rw = nc::width(&right) as u32;
        put(p, y, cols.saturating_sub(rw), &right, t.dim);

        let max = cols.saturating_sub(rw + 2);
        let next_song = || {
            let id = self.status.next_id?;
            let i = q.pos_of_id(id)?;
            let m = &q.metas[i];
            Some(m.title.clone() + if m.artist.is_empty() { "" } else { " — " } + &m.artist)
        };
        let segs: Segs = if let Some(why) = &self.offline {
            vec![(format!("MPD unreachable, retrying… ({why})"), t.current)]
        } else if let Some(Prompt::ClearQueue) = self.prompt {
            vec![("Clear the whole queue? (y/N)".to_owned(), t.current)]
        } else if !self.pending.is_empty() {
            vec![(format!("{}…", self.pending.iter().map(key_name).collect::<String>()), t.group_header)]
        } else if let Some(m) = self.current_message() {
            vec![(m.to_owned(), t.text)]
        } else if !q.marked.is_empty() {
            vec![(format!("{} selected", q.marked.len()), t.group_header)]
        } else if let Some(next) = next_song().filter(|_| self.status.state != State::Stop) {
            vec![("next: ".to_owned(), t.dim), (next, t.text)]
        } else {
            return;
        };
        put_segs(p, y, 1, max, &segs);
    }
}

impl App<'_> {
    /// rmpc-style Miller columns: parent (20%), current (38%), preview (42%).
    fn draw_directories(&mut self, p: &mut Plane, body: Rect) -> Result<()> {
        let t = self.cfg.theme.clone();
        if !self.browser.is_open() {
            return Ok(());
        }
        let crumbs = std::iter::once("Music")
            .chain(self.browser.current().uri.split('/').filter(|c| !c.is_empty()))
            .collect::<Vec<_>>()
            .join(" / ");
        put(p, body.y, 1, &nc::fit(&crumbs, body.cols.saturating_sub(2) as usize), t.dim);

        let (top, h) = (body.y + 1, body.rows.saturating_sub(1));
        let w0 = body.cols * 20 / 100;
        let w1 = body.cols * 38 / 100;
        let (x1, x2) = (w0 + 1, w0 + w1 + 2);
        let w2 = body.cols.saturating_sub(x2);
        for y in top..top + h {
            put(p, y, x1 - 1, "│", t.dim);
            put(p, y, x2 - 1, "│", t.dim);
        }
        self.list_height = h as usize;

        if let Some(parent) = self.browser.parent() {
            let (entries, sel) = (parent.entries.clone(), parent.sel);
            let scroll = sel.saturating_sub(h as usize / 2);
            self.draw_entries(p, Rect { y: top, x: 0, rows: h, cols: w0 }, &entries, Some(sel), scroll, 0, t.browse, &t);
        }

        let cur = self.browser.current();
        let (entries, sel, mut scroll) = (cur.entries.clone(), cur.sel, cur.scroll);
        if sel < scroll {
            scroll = sel;
        } else if sel >= scroll + h as usize {
            scroll = sel + 1 - h as usize;
        }
        self.browser.levels.last_mut().unwrap().scroll = scroll;
        if entries.is_empty() {
            put(p, top, x1 + 1, "(empty)", t.dim);
        }
        self.draw_entries(p, Rect { y: top, x: x1, rows: h, cols: w1 }, &entries, Some(sel), scroll, 1, t.cursor, &t);

        self.draw_preview(p, Rect { y: top, x: x2, rows: h, cols: w2 }, &t)
    }

    /// One column of entries. `sel_style` marks the cursor's row.
    #[allow(clippy::too_many_arguments)]
    fn draw_entries(
        &self,
        p: &mut Plane,
        area: Rect,
        entries: &[Entry],
        sel: Option<usize>,
        scroll: usize,
        col: u8,
        sel_style: Style,
        t: &Theme,
    ) {
        let playing = self.song.as_ref().map(|(s, _)| s.file.as_str());
        for (i, e) in entries.iter().enumerate().skip(scroll).take(area.rows as usize) {
            let y = area.y + (i - scroll) as u32;
            self.hit(y, area.x, area.cols, Target::Entry(col, i));
            let is_playing = playing.is_some_and(|f| match e {
                Entry::Dir(d) => f.starts_with(d.as_str()) && f[d.len()..].starts_with('/'),
                Entry::Song(s, _) => s.file == f,
            });
            let base = if is_playing { t.current } else { t.text };
            let selected = sel == Some(i);
            let st = if selected { Style { bg: sel_style.bg, ..if sel_style.fg == Color::Default { base } else { sel_style } } } else { base };
            if selected {
                put(p, y, area.x, &" ".repeat(area.cols as usize), st);
            }
            let marked = self.browser.marked.contains(e.uri());
            let (sym, label) = match e {
                Entry::Dir(_) => (t.dir_symbol.as_str(), e.name().to_owned()),
                Entry::Song(s, m) => {
                    let track = m.track.map(|n| format!("{n:02} ")).unwrap_or_default();
                    let name = if s.title.is_some() {
                        format!("{track}{} - {}", m.artist, m.title)
                    } else {
                        e.name().to_owned()
                    };
                    (t.song_symbol.as_str(), name)
                }
            };
            let lead = if marked { "+".to_owned() } else { sym.to_owned() };
            let mut x = put(p, y, area.x + 1, &lead, if marked && !selected { t.group_header } else { st });
            x = put(p, y, x, " ", st);
            put(p, y, x, &nc::fit(&label, (area.x + area.cols).saturating_sub(x + 1) as usize), st);
        }
    }

    /// The right column: cover, summary, and the folder's contents or the
    /// song's tags.
    fn draw_preview(&mut self, p: &mut Plane, area: Rect, t: &Theme) -> Result<()> {
        let selected_uri = self.browser.selected().map(|e| e.uri().to_owned());
        let Some(pv) = self.browser.preview.clone().filter(|pv| Some(&pv.uri) == selected_uri.as_ref()) else {
            self.preview_art.show(None)?;
            return Ok(());
        };
        let mut y = area.y;
        if self.preview_art.has_image() && area.cols >= 12 && self.overlay.is_none() {
            let rows = (area.rows / 2).min(area.cols / 2).max(1);
            let cols = (rows * 2).min(area.cols);
            let r = Rect { y, x: area.x + (area.cols - cols) / 2, rows, cols };
            self.preview_art.show(Some(r))?;
            y += rows + 1;
        } else {
            self.preview_art.show(None)?;
        }
        let w = area.cols.saturating_sub(2);
        let bottom = area.y + area.rows;

        let songs: Vec<(&crate::mpd::Song, &crate::meta::Meta)> = pv
            .entries
            .iter()
            .filter_map(|e| match e {
                Entry::Song(s, m) => Some((&**s, &**m)),
                Entry::Dir(_) => None,
            })
            .collect();
        let format = |s: &crate::mpd::Song| {
            let codec = s.file.rsplit_once('.').map(|(_, e)| e.to_ascii_uppercase()).unwrap_or_default();
            s.format.as_deref().map_or(codec.clone(), |f| format!("{codec} {}", fmt_audio(f)))
        };

        match self.browser.selected() {
            Some(Entry::Song(s, m)) => {
                let fields = [
                    ("Title", m.title.clone()),
                    ("Artist", m.artist.clone()),
                    ("Album", m.album.clone()),
                    ("Album artist", m.album_artist.clone()),
                    ("Year", m.year.clone()),
                    ("Track", m.track.map(|n| n.to_string()).unwrap_or_default()),
                    ("Disc", m.disc.map(|n| n.to_string()).unwrap_or_default()),
                    ("Length", fmt_time(s.duration)),
                    ("Format", format(s)),
                    ("File", s.file.clone()),
                ];
                for (label, value) in fields.iter().filter(|(_, v)| !v.is_empty()) {
                    if y >= bottom {
                        break;
                    }
                    let segs: Segs = vec![(format!("{label}: "), t.label), (value.clone(), italic_if(t.text, m.inferred))];
                    put_segs(p, y, area.x + 1, w, &segs);
                    y += 1;
                }
            }
            Some(Entry::Dir(_)) => {
                if let Some((first, m)) = songs.first() {
                    let total: Duration = songs.iter().map(|(s, _)| s.duration).sum();
                    let mut l1: Segs = vec![(m.album.clone(), italic_if(t.group_header, m.inferred))];
                    if !m.year.is_empty() {
                        l1.push((format!(" ({})", m.year), t.date));
                    }
                    if y < bottom {
                        put_segs(p, y, area.x + 1, w, &l1);
                        y += 1;
                    }
                    let n = songs.len();
                    let l2 = format!(
                        "{} · {n} track{} · {} · {}",
                        m.album_artist,
                        if n == 1 { "" } else { "s" },
                        fmt_time(total),
                        format(first)
                    );
                    if y < bottom {
                        put(p, y, area.x + 1, &nc::fit(&l2, w as usize), t.dim);
                        y += 2;
                    }
                }
                let rows = bottom.saturating_sub(y);
                if rows > 0 {
                    self.draw_entries(p, Rect { y, x: area.x, rows, cols: area.cols }, &pv.entries, None, 0, 2, t.cursor, t);
                }
            }
            None => {}
        }
        Ok(())
    }
}

/// The colour at `t` (0..1) along evenly spaced RGB stops.
fn gradient(stops: &[u32], t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0) * (stops.len() - 1) as f32;
    let i = (t as usize).min(stops.len().saturating_sub(2));
    let (a, b) = (stops[i], *stops.get(i + 1).unwrap_or(&stops[i]));
    let f = t - i as f32;
    let ch = |shift: u32| {
        let (x, y) = (((a >> shift) & 0xff) as f32, ((b >> shift) & 0xff) as f32);
        ((x + (y - x) * f).round() as u32) << shift
    };
    ch(16) | ch(8) | ch(0)
}

/// Scales an RGB colour's brightness.
fn scale(rgb: u32, k: f32) -> u32 {
    let ch = |shift: u32| ((((rgb >> shift) & 0xff) as f32 * k).round().min(255.0) as u32) << shift;
    ch(16) | ch(8) | ch(0)
}

/// How a queue row is highlighted: the highlight (`primary`) replaces the
/// row's colours; the browse cursor only tints the background.
#[derive(Clone, Copy)]
struct Highlight {
    primary: bool,
    fill: Option<Style>,
}

impl Highlight {
    fn apply(self, s: Style) -> Style {
        match self.fill {
            Some(f) if self.primary => f,
            Some(f) => Style { bg: f.bg, ..s },
            None => s,
        }
    }
}

/// How a typed key looks in the footer while a sequence is pending.
fn key_name(k: &crate::nc::Input) -> String {
    let c = char::from_u32(k.id).filter(|_| crate::nc::key::is_printable(k.id)).unwrap_or('?');
    if k.ctrl { format!("<C-{c}>") } else { c.to_string() }
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
