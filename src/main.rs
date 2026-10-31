mod mpd;
mod nc;

use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use mpd::{Conn, Song, State, Status, Target};
use nc::{Align, Input, Nc, Plane, key};

/// How often to redraw while playing, so the progress bar moves.
const TICK: Duration = Duration::from_millis(250);

const ACCENT: u32 = 0x89b4fa;
const DIM: u32 = 0x6c7086;
const TEXT: u32 = 0xcdd6f4;

enum Event {
    Changed,
    Lost(anyhow::Error),
}

/// Blocks on `idle` in a second connection and reports changes.
fn spawn_idle(target: Target) -> Result<Receiver<Event>> {
    let mut conn = Conn::connect(&target)?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        loop {
            match conn.command("idle player mixer options playlist") {
                Ok(_) => {
                    if tx.send(Event::Changed).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Event::Lost(e));
                    return;
                }
            }
        }
    });
    Ok(rx)
}

struct App {
    conn: Conn,
    status: Status,
    song: Option<Song>,
    fetched_at: Instant,
}

impl App {
    fn fetch(&mut self) -> Result<()> {
        self.status = Status::parse(&self.conn.command("status")?);
        self.song = Song::parse(&self.conn.command("currentsong")?);
        self.fetched_at = Instant::now();
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
    fn handle(&mut self, input: Input) -> Result<bool> {
        let s = &self.status;
        let cmd = match input.id {
            _ if input.is_char('q') || input.id == key::ESC => return Ok(false),
            _ if input.is_char(' ') || input.is_char('p') => match s.state {
                State::Play => "pause 1".into(),
                _ => "play".into(),
            },
            _ if input.is_char('s') => "stop".into(),
            _ if input.is_char('n') || input.is_char('>') => "next".into(),
            _ if input.is_char('N') || input.is_char('<') => "previous".into(),
            key::RIGHT => "seekcur +5".into(),
            key::LEFT => "seekcur -5".into(),
            _ if input.is_char('+') || input.is_char('=') => match s.volume {
                Some(v) => format!("setvol {}", (v + 5).min(100)),
                None => return Ok(true),
            },
            _ if input.is_char('-') => match s.volume {
                Some(v) => format!("setvol {}", v.saturating_sub(5)),
                None => return Ok(true),
            },
            _ if input.is_char('r') => format!("repeat {}", u8::from(!s.repeat)),
            _ if input.is_char('z') => format!("random {}", u8::from(!s.random)),
            _ if input.is_char('y') => format!("single {}", u8::from(!s.single)),
            _ if input.is_char('c') => format!("consume {}", u8::from(!s.consume)),
            _ => return Ok(true),
        };
        // The idle connection reports the change, which triggers a fetch.
        self.conn.command(&cmd)?;
        Ok(true)
    }

    fn draw(&self, p: &mut Plane) {
        p.erase();
        let (rows, cols) = p.dim_yx();
        if rows < 6 || cols < 20 {
            p.putstr_yx(0, 0, "too small");
            return;
        }

        // header
        p.set_styles(nc::BOLD);
        p.set_fg_rgb(ACCENT);
        p.putstr_yx(0, 1, "notmpc");
        p.set_styles(0);
        p.set_fg_rgb(DIM);
        let s = &self.status;
        let flag = |on: bool, c: char| if on { c } else { '-' };
        let vol = s.volume.map_or("n/a".into(), |v| format!("{v}%"));
        p.putstr_aligned(
            0,
            Align::Right,
            &format!(
                "vol {vol}  [{}{}{}{}] ",
                flag(s.repeat, 'r'),
                flag(s.random, 'z'),
                flag(s.single, 'y'),
                flag(s.consume, 'c'),
            ),
        );

        // now playing, centred vertically
        let mid = rows / 2;
        match &self.song {
            Some(song) => {
                p.set_styles(nc::BOLD);
                p.set_fg_rgb(TEXT);
                p.putstr_aligned(mid - 2, Align::Center, song.display_title());
                p.set_styles(0);
                p.set_fg_rgb(ACCENT);
                p.putstr_aligned(mid - 1, Align::Center, song.artist.as_deref().unwrap_or(""));
                p.set_styles(nc::ITALIC);
                p.set_fg_rgb(DIM);
                p.putstr_aligned(mid, Align::Center, song.album.as_deref().unwrap_or(""));
                p.set_styles(0);
            }
            None => {
                p.set_fg_rgb(DIM);
                p.putstr_aligned(mid - 1, Align::Center, "nothing queued");
            }
        }

        // progress bar
        let state = match s.state {
            State::Play => "▶",
            State::Pause => "⏸",
            State::Stop => "⏹",
        };
        let left = format!(" {state} {}", fmt_time(self.elapsed()));
        let right = format!("{} ", fmt_time(s.duration));
        let width = (cols as usize).saturating_sub(left.chars().count() + right.chars().count() + 2);
        let frac = if s.duration.is_zero() {
            0.0
        } else {
            self.elapsed().as_secs_f64() / s.duration.as_secs_f64()
        };
        let filled = ((width as f64 * frac) as usize).min(width);
        let y = mid + 2;
        p.set_fg_rgb(TEXT);
        p.putstr_yx(y, 0, &left);
        p.set_fg_rgb(ACCENT);
        p.putstr_yx(y, left.chars().count() as u32 + 1, &"━".repeat(filled));
        p.set_fg_rgb(DIM);
        p.putstr_yx(y, (left.chars().count() + 1 + filled) as u32, &"─".repeat(width - filled));
        p.set_fg_rgb(TEXT);
        p.putstr_aligned(y, Align::Right, &right);

        // key help
        p.set_fg_rgb(DIM);
        p.putstr_aligned(
            rows - 1,
            Align::Center,
            "space pause · n/N next/prev · ←/→ seek · +/- vol · r z y c modes · q quit",
        );
        p.set_fg_default();
    }
}

fn fmt_time(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

fn main() -> Result<()> {
    // Connect before taking over the terminal so errors print normally.
    let target = Target::from_env();
    let mut app = App {
        conn: Conn::connect(&target)?,
        status: Status::default(),
        song: None,
        fetched_at: Instant::now(),
    };
    app.fetch()?;
    let events = spawn_idle(target)?;

    let mut nc = Nc::new()?;
    loop {
        app.draw(&mut nc.stdplane());
        nc.render()?;

        let timeout = if app.status.state == State::Play { TICK } else { Duration::from_secs(1) };
        match nc.get(timeout)? {
            Some(i) if i.id == key::RESIZE => nc.refresh()?,
            Some(i) if !app.handle(i)? => return Ok(()),
            _ => {}
        }

        let mut changed = false;
        for ev in events.try_iter() {
            match ev {
                Event::Changed => changed = true,
                Event::Lost(e) => bail!("lost MPD idle connection: {e}"),
            }
        }
        if changed {
            app.fetch()?;
        }
    }
}
