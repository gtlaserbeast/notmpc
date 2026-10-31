mod app;
mod art;
mod config;
mod keys;
mod meta;
mod mpd;
mod nc;
mod ui;

use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use anyhow::Result;

use app::App;
use config::Config;
use mpd::{Conn, Target};
use nc::{Event, Nc, key};

/// How often to redraw while playing, so the progress bar moves.
const TICK: Duration = Duration::from_millis(250);
/// How often to try reaching MPD again after losing it.
const RETRY: Duration = Duration::from_secs(1);

enum MpdEvent {
    /// Idle subsystems that changed (`player`, `playlist`, ...).
    Changed(Vec<String>),
    /// MPD went away (stopped or restarted); we keep retrying.
    Lost(String),
    Reconnected,
}

/// Blocks on `idle` in a second connection and reports changes. Survives
/// MPD restarts: on failure it reports `Lost` and reconnects.
fn spawn_idle(target: Target) -> Result<Receiver<MpdEvent>> {
    let mut conn = Conn::connect(&target)?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        loop {
            let ev = match conn.command("idle player mixer options playlist update") {
                Ok(pairs) => MpdEvent::Changed(pairs.into_iter().map(|(_, v)| v).collect()),
                Err(e) => {
                    if tx.send(MpdEvent::Lost(format!("{e:#}"))).is_err() {
                        return;
                    }
                    conn = loop {
                        thread::sleep(RETRY);
                        if let Ok(c) = Conn::connect(&target) {
                            break c;
                        }
                    };
                    MpdEvent::Reconnected
                }
            };
            if tx.send(ev).is_err() {
                return;
            }
        }
    });
    Ok(rx)
}

fn main() -> Result<()> {
    // Load config and connect before taking over the terminal so errors
    // print normally.
    let cfg = Config::load()?;
    let target = Target::from_env();
    let conn = Conn::connect(&target)?;
    let events = spawn_idle(target)?;

    let nc = Nc::new()?;
    nc.enable_mouse();
    let mut app = App::new(&nc, cfg, conn)?;
    loop {
        app.draw()?;
        nc.render()?;

        let timeout = if app.is_playing() { TICK } else { Duration::from_secs(1) };
        match nc.get(timeout)? {
            Some(Event::Key(i)) if i.id == key::RESIZE => nc.refresh()?,
            // MPD trouble is shown in the footer; it never ends the session.
            Some(Event::Key(i)) => match app.handle(i) {
                Ok(false) => return Ok(()),
                Ok(true) => {}
                Err(e) => app.say(format!("MPD: {e:#}")),
            },
            Some(Event::Mouse(m)) => {
                if let Err(e) = app.click(m) {
                    app.say(format!("MPD: {e:#}"));
                }
            }
            None => {}
        }

        let mut changed = Vec::new();
        for ev in events.try_iter() {
            match ev {
                MpdEvent::Changed(c) => changed.extend(c),
                MpdEvent::Lost(e) => app.offline = Some(e),
                MpdEvent::Reconnected => {
                    app.offline = None;
                    changed.extend(app::ALL_EVENTS.iter().map(|s| s.to_string()));
                }
            }
        }
        if !changed.is_empty()
            && let Err(e) = app.update(&changed)
        {
            app.say(format!("MPD: {e:#}"));
        }
    }
}
