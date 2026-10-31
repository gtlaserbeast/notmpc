mod app;
mod art;
mod mpd;
mod nc;

use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use anyhow::{Result, bail};

use app::App;
use mpd::{Conn, Target};
use nc::{Nc, key};

/// How often to redraw while playing, so the progress bar moves.
const TICK: Duration = Duration::from_millis(250);

enum Event {
    /// Idle subsystems that changed (`player`, `playlist`, ...).
    Changed(Vec<String>),
    Lost(anyhow::Error),
}

/// Blocks on `idle` in a second connection and reports changes.
fn spawn_idle(target: Target) -> Result<Receiver<Event>> {
    let mut conn = Conn::connect(&target)?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        loop {
            let ev = match conn.command("idle player mixer options playlist") {
                Ok(pairs) => Event::Changed(pairs.into_iter().map(|(_, v)| v).collect()),
                Err(e) => Event::Lost(e),
            };
            let lost = matches!(ev, Event::Lost(_));
            if tx.send(ev).is_err() || lost {
                return;
            }
        }
    });
    Ok(rx)
}

fn main() -> Result<()> {
    // Connect before taking over the terminal so errors print normally.
    let target = Target::from_env();
    let conn = Conn::connect(&target)?;
    let events = spawn_idle(target)?;

    let nc = Nc::new()?;
    let mut app = App::new(&nc, conn)?;
    loop {
        app.draw()?;
        nc.render()?;

        let timeout = if app.is_playing() { TICK } else { Duration::from_secs(1) };
        match nc.get(timeout)? {
            Some(i) if i.id == key::RESIZE => nc.refresh()?,
            Some(i) if !app.handle(i)? => return Ok(()),
            _ => {}
        }

        let mut changed = Vec::new();
        for ev in events.try_iter() {
            match ev {
                Event::Changed(c) => changed.extend(c),
                Event::Lost(e) => bail!("lost MPD idle connection: {e}"),
            }
        }
        if !changed.is_empty() {
            app.update(&changed)?;
        }
    }
}
